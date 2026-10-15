use synclane::{db, importer};
use synclane::db::TimelineEvent;
use rusqlite::Connection;
use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tauri::{Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

struct AppState(Arc<Mutex<Connection>>);

#[derive(Serialize)]
struct AppInfo {
    schema: i64,
    sessions: i64,
    events: i64,
    user_events: i64,
    assistant_events: i64,
    journals: i64,
    analyses: i64,
}

#[tauri::command]
fn status(state: tauri::State<'_, AppState>) -> Result<AppInfo, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let counts = db::counts(&conn).map_err(|e| e.to_string())?;
    let roles = db::role_counts(&conn).map_err(|e| e.to_string())?;
    Ok(AppInfo {
        schema: db::schema_version(&conn).map_err(|e| e.to_string())?,
        sessions: counts.sessions,
        events: roles.total,
        user_events: roles.user,
        assistant_events: roles.assistant,
        journals: counts.journals,
        analyses: counts.analyses,
    })
}

#[tauri::command]
fn timeline(
    date_from: Option<String>,
    date_to: Option<String>,
    query: Option<String>,
    limit: usize,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<TimelineEvent>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    db::timeline_range(&conn, date_from.as_deref(), date_to.as_deref(), query.as_deref(), limit).map_err(|e| e.to_string())
}

#[tauri::command]
fn open_search_window(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("search") {
        window.unminimize().map_err(|e| e.to_string())?;
        window.show().map_err(|e| e.to_string())?;
        window.set_focus().map_err(|e| e.to_string())?;
        return Ok(());
    }
    let window = WebviewWindowBuilder::new(&app, "search", WebviewUrl::App("index.html#search".into()))
        .title("SyncLane 検索")
        .inner_size(720.0, 250.0)
        .min_inner_size(600.0, 210.0)
        .resizable(true)
        .build()
        .map_err(|e| e.to_string())?;
    window.set_focus().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn close_search_window(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("search") {
        window.close().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// `--mcp` で起動されたときは、画面を出さず、MCP サーバーとして標準入出力で動く(読み取り専用)。
/// Claude Code などには `<このアプリの実行ファイル> --mcp` を登録する。
fn run_mcp() -> ! {
    let result = db::default_db_path()
        .and_then(|path| db::open_readonly(&path))
        .and_then(|conn| synclane::mcp::serve(&conn, std::io::stdin().lock(), std::io::stdout().lock()));
    match result {
        Ok(()) => std::process::exit(0),
        Err(error) => {
            eprintln!("synclane mcp: {error:#}");
            std::process::exit(1);
        }
    }
}

/// 設定画面に出す、MCP 登録用の実行ファイルのパス。
#[tauri::command]
fn mcp_executable() -> Result<String, String> {
    std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).map_err(|e| e.to_string())
}

fn main() {
    if std::env::args().any(|arg| arg == "--mcp") {
        run_mcp();
    }
    let path = db::default_db_path().expect("failed to resolve SyncLane database path");
    let conn = db::open(&path).expect("failed to open SyncLane database");

    let shared_conn = Arc::new(Mutex::new(conn));
    let collector_conn = Arc::clone(&shared_conn);
    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(AppState(shared_conn))
        .setup(move |app| {
            let handle = app.handle().clone();
            thread::spawn(move || {
                let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default();
                let root = home.join(".claude").join("projects");
                // 題名の対応より前に取り込んだ会話に、題名を入れる(起動のたびに1回。入っているものは飛ばす)
                if let Ok(mut conn) = collector_conn.lock() {
                    if let Err(error) = importer::backfill_claude_titles(&root, &mut conn) {
                        eprintln!("synclane title backfill: {error:#}");
                    }
                }
                loop {
                    if let Ok(mut conn) = collector_conn.lock() {
                        match importer::collect_claude_incremental(&root, &mut conn) {
                            // 新しいイベントが入ったことだけ画面に知らせる(一覧は動かさない)
                            Ok(report) if report.events > 0 => {
                                let _ = handle.emit("events-updated", report.events);
                            }
                            Ok(_) => {}
                            Err(error) => eprintln!("synclane collector: {error:#}"),
                        }
                    }
                    thread::sleep(Duration::from_secs(30));
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![status, timeline, mcp_executable, open_search_window, close_search_window])
        .run(tauri::generate_context!())
        .expect("error while running SyncLane");
}
