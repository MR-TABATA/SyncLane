//! サーバーへの送信(任意)の、アプリ側の操作。画面の「サーバー」タブから呼ばれる。
//!
//! 通信は、画面を止めないよう別のスレッドで行う。DB のロックは、通信のあいだ持たない
//! (送信のスレッドは、自分用の接続で開く)。トークンは OS のキーチェーンに置く。

use serde::Serialize;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use synclane::db;
use synclane::sync::api::UreqTransport;
use synclane::sync::device_flow::{self, ConnectError, Pending};
use synclane::sync::engine::{self, Options, Stop};
use synclane::sync::ops::{self, SessionRow};
use synclane::sync::secrets::KeyringStore;
use synclane::sync::store::{self, Mode};
use tauri::{AppHandle, Emitter, State};

use crate::AppState;

const TICK: Duration = Duration::from_secs(60);
const MAX_BACKOFF: Duration = Duration::from_secs(600);

struct Inner {
    db_path: PathBuf,
    /// 承認待ちの登録(あれば)
    connecting: Mutex<Option<Pending>>,
    cancel: AtomicBool,
    running: AtomicBool,
    /// 利用者の操作が要るため、自動の送信を止めている理由(再接続・アプリの更新など)
    blocked: Mutex<Option<String>>,
}

#[derive(Clone)]
pub struct SyncState(Arc<Inner>);

impl SyncState {
    pub fn new(db_path: PathBuf) -> Self {
        SyncState(Arc::new(Inner {
            db_path,
            connecting: Mutex::new(None),
            cancel: AtomicBool::new(false),
            running: AtomicBool::new(false),
            blocked: Mutex::new(None),
        }))
    }

    fn open(&self) -> Result<rusqlite::Connection, String> {
        db::open(&self.0.db_path).map_err(|e| format!("{e:#}"))
    }
}

fn message(e: impl std::fmt::Display) -> String {
    format!("{e:#}")
}

#[derive(Serialize)]
pub struct ConnectInfo {
    server: String,
    user_code: String,
    url: String,
    opened: bool,
}

/// 1 回ぶんの送信。すでに動いていれば何もしない。結果を画面に知らせる。
fn run_once(state: &SyncState, handle: &AppHandle) -> Option<Stop> {
    if state.0.running.swap(true, Ordering::SeqCst) {
        return None;
    }
    let result = (|| -> anyhow::Result<engine::Report> {
        let conn = db::open(&state.0.db_path)?;
        engine::run(&conn, &UreqTransport::new(), &KeyringStore, &thread::sleep, &Options::default())
    })();
    state.0.running.store(false, Ordering::SeqCst);

    let stop = match result {
        Ok(report) => {
            if report.batches > 0 || report.stop.is_some() {
                let _ = handle.emit("sync-updated", json!({"sent": report.events_sent}));
            }
            report.stop
        }
        Err(error) => Some(Stop::Error(message(error))),
    };
    match &stop {
        Some(Stop::NeedsAuth(m) | Stop::Forbidden(m) | Stop::UpdateRequired(m)) => {
            *state.0.blocked.lock().unwrap() = Some(m.clone());
        }
        Some(_) => {}
        None => *state.0.blocked.lock().unwrap() = None,
    }
    let _ = handle.emit("sync-updated", json!({}));
    stop
}

/// 起動時に 1 度呼ぶ。1 分おきに、選んだ会話の新しい発言を送る(接続していなければ、何もしない)。
pub fn spawn_loop(state: SyncState, handle: AppHandle) {
    thread::spawn(move || {
        let mut backoff = TICK;
        loop {
            thread::sleep(backoff);
            let connected = state.open().ok().is_some_and(|c| store::load_connection(&c).ok().flatten().is_some());
            if !connected || state.0.blocked.lock().unwrap().is_some() {
                backoff = TICK;
                continue;
            }
            backoff = match run_once(&state, &handle) {
                None => TICK,
                Some(Stop::RateLimited(seconds)) => Duration::from_secs(seconds.clamp(10, 3600)),
                Some(Stop::Unavailable(_)) => (backoff * 2).min(MAX_BACKOFF), // つながらないあいだは、間隔をあける
                Some(_) => TICK,
            };
        }
    });
}

#[tauri::command]
pub fn sync_overview(app: State<'_, AppState>, sync: State<'_, SyncState>) -> Result<Value, String> {
    let conn = app.0.lock().map_err(message)?;
    let overview = ops::overview(&conn).map_err(message)?;
    let connecting = sync.0.connecting.lock().unwrap().as_ref().map(|p| {
        json!({"server": p.server, "user_code": p.start.user_code, "url": p.start.verification_uri})
    });
    let mut value = serde_json::to_value(overview).map_err(message)?;
    value["blocked"] = json!(*sync.0.blocked.lock().unwrap());
    value["running"] = json!(sync.0.running.load(Ordering::SeqCst));
    value["connecting"] = json!(connecting);
    Ok(value)
}

#[tauri::command]
pub fn sync_sessions(limit: usize, offset: usize, app: State<'_, AppState>) -> Result<Vec<SessionRow>, String> {
    let conn = app.0.lock().map_err(message)?;
    let Some(c) = store::load_connection(&conn).map_err(message)? else { return Ok(vec![]) };
    let mode = store::mode(&conn).map_err(message)?;
    ops::list_sessions(&conn, &c.server, mode, limit.min(500), offset).map_err(message)
}

#[tauri::command]
pub fn sync_set_mode(mode: String, app: State<'_, AppState>) -> Result<(), String> {
    let conn = app.0.lock().map_err(message)?;
    store::set_mode(&conn, Mode::parse(&mode).map_err(message)?).map_err(message)
}

/// 会話の指定を変える。state は "send" / "excluded" / null(指定を外す)。
#[tauri::command]
pub fn sync_select(ids: Vec<i64>, state: Option<String>, visibility: Option<String>, app: State<'_, AppState>) -> Result<(), String> {
    if let Some(v) = visibility.as_deref() {
        if !matches!(v, "private" | "org") {
            return Err("visibility は private か org です".into());
        }
    }
    let conn = app.0.lock().map_err(message)?;
    let c = store::load_connection(&conn).map_err(message)?.ok_or("サーバーに接続していません")?;
    for id in ids {
        store::set_session_state(&conn, &c.server, id, state.as_deref(), visibility.as_deref()).map_err(message)?;
    }
    Ok(())
}

#[tauri::command]
pub fn sync_select_all(visibility: Option<String>, app: State<'_, AppState>) -> Result<usize, String> {
    let conn = app.0.lock().map_err(message)?;
    let c = store::load_connection(&conn).map_err(message)?.ok_or("サーバーに接続していません")?;
    store::include_all(&conn, &c.server, visibility.as_deref()).map_err(message)
}

/// 登録を始める。コードを返し、ブラウザを開く。承認の待ちは別のスレッドで行い、終わったら "sync-connected" を送る。
#[tauri::command]
pub async fn sync_connect(url: String, sync: State<'_, SyncState>, handle: AppHandle) -> Result<ConnectInfo, String> {
    let state = sync.inner().clone();
    if state.0.connecting.lock().unwrap().is_some() {
        return Err("登録の途中です。承認するか、中止してください".into());
    }
    let begin_state = state.clone();
    let pending = tauri::async_runtime::spawn_blocking(move || -> Result<Pending, String> {
        let conn = begin_state.open()?;
        if store::load_connection(&conn).map_err(message)?.is_some() {
            return Err("すでにサーバーに接続しています。先に接続を解除してください".into());
        }
        device_flow::begin(&UreqTransport::new(), &url, &device_flow::default_device_name(), env!("CARGO_PKG_VERSION"))
            .map_err(message)
    })
    .await
    .map_err(message)??;

    let info = ConnectInfo {
        server: pending.server.clone(),
        user_code: pending.start.user_code.clone(),
        url: pending.start.verification_uri.clone(),
        opened: device_flow::open_in_browser(&pending.start.verification_uri),
    };
    state.0.cancel.store(false, Ordering::SeqCst);
    *state.0.connecting.lock().unwrap() = Some(pending.clone());

    thread::spawn(move || {
        let outcome = state.open().and_then(|conn| {
            device_flow::finish(&conn, &UreqTransport::new(), &KeyringStore, &pending, &thread::sleep, &|| {
                state.0.cancel.load(Ordering::SeqCst)
            })
            .map_err(|e: ConnectError| e.to_string())
        });
        *state.0.connecting.lock().unwrap() = None;
        let payload = match outcome {
            Ok(connection) => {
                *state.0.blocked.lock().unwrap() = None;
                json!({"ok": true, "server": connection.server})
            }
            Err(error) => json!({"ok": false, "message": error}),
        };
        let _ = handle.emit("sync-connected", payload);
    });
    Ok(info)
}

#[tauri::command]
pub fn sync_connect_cancel(sync: State<'_, SyncState>) {
    sync.0.cancel.store(true, Ordering::SeqCst);
    *sync.0.connecting.lock().unwrap() = None;
}

#[tauri::command]
pub fn sync_disconnect(app: State<'_, AppState>, sync: State<'_, SyncState>) -> Result<(), String> {
    let conn = app.0.lock().map_err(message)?;
    ops::disconnect(&conn, &KeyringStore).map_err(message)?;
    *sync.0.blocked.lock().unwrap() = None;
    Ok(())
}

/// 今すぐ送る。結果は "sync-updated" で知らせる。
#[tauri::command]
pub fn sync_now(sync: State<'_, SyncState>, handle: AppHandle) {
    let state = sync.inner().clone();
    *state.0.blocked.lock().unwrap() = None;
    thread::spawn(move || {
        run_once(&state, &handle);
    });
}
