mod ingest_server;
mod resident;
mod sync_app;

use synclane::{db, importer};
use notify::Watcher;
use resident::{
    detect_tool_from_path, is_relevant_event_kind, should_hide_on_close, wait_with_debounce, ToolKind, WaitResult,
};
use rusqlite::Connection;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::TrayIconBuilder;
use tauri::{Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_autostart::ManagerExt;

static IS_QUITTING: AtomicBool = AtomicBool::new(false);

pub struct AppState(pub Arc<Mutex<Connection>>);

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

/// 一覧の検索。完全一致が 1 件も無いときは、あいまい一致で探し直す(`fuzzy` が true のときに、その旨を画面に出す)。
#[tauri::command]
fn timeline(
    date_from: Option<String>,
    date_to: Option<String>,
    query: Option<String>,
    limit: usize,
    offset: Option<usize>,
    sort: Option<String>,
    project: Option<String>,
    session_id: Option<i64>,
    state: tauri::State<'_, AppState>,
) -> Result<db::SearchHits, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let order = if sort.as_deref() == Some("relevance") { db::SearchOrder::Relevance } else { db::SearchOrder::Newest };
    db::timeline_search_page(
        &conn,
        date_from.as_deref(),
        date_to.as_deref(),
        project.as_deref(),
        session_id,
        query.as_deref(),
        limit,
        offset.unwrap_or(0),
        order,
        true,
    ).map_err(|e| e.to_string())
}

/// トークン数の集計(期間・単位別)。読み取りだけ。`group_by` は `day` / `project` / `model` / `tool`。`utc_offset_minutes` は、日別を端末の時刻で切るための時差。
#[tauri::command]
fn usage_summary(
    date_from: Option<String>,
    date_to: Option<String>,
    group_by: String,
    utc_offset_minutes: i32,
    limit: Option<usize>,
    state: tauri::State<'_, AppState>,
) -> Result<synclane::usage::Summary, String> {
    let group = synclane::usage::GroupBy::parse(&group_by).ok_or_else(|| format!("知らない集計の単位です: {group_by}"))?;
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    synclane::usage::summarize(&conn, date_from.as_deref(), date_to.as_deref(), group, utc_offset_minutes, limit.unwrap_or(100).clamp(1, 1000)).map_err(|e| format!("{e:#}"))
}

/// 「続きから」の一覧(サイドバー)。プロジェクトごとに、ツールごとの最新の会話を返す。読み取りだけ。
#[tauri::command]
fn project_overview(state: tauri::State<'_, AppState>) -> Result<Vec<synclane::projects::ProjectRow>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    synclane::projects::overview(&conn).map_err(|e| format!("{e:#}"))
}

/// AIツールごとにグルーピングした会話一覧(サイドバー用)。
#[tauri::command]
fn sessions_by_tool(state: tauri::State<'_, AppState>) -> Result<Vec<synclane::projects::ToolGroup>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    synclane::projects::sessions_by_tool(&conn).map_err(|e| format!("{e:#}"))
}

/// タイムライン用: 会話が存在する全日付の一覧(新しい順)
#[tauri::command]
fn timeline_days(project: Option<String>, state: tauri::State<'_, AppState>) -> Result<Vec<synclane::projects::DaySummary>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    synclane::projects::timeline_days(&conn, project.as_deref()).map_err(|e| format!("{e:#}"))
}

/// 題名の提案(手元の規則)。指した発言が属する区切り(話題のまとまり)の、最近の発言から出す。外には何も送らない。
#[tauri::command]
fn section_title_suggest(event_id: i64, state: tauri::State<'_, AppState>) -> Result<String, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let section = synclane::sections::load(&conn, event_id).map_err(|e| format!("{e:#}"))?;
    synclane::titles::suggest_local(&section.messages).ok_or_else(|| "提案できる発言がありません".to_string())
}

/// 画面の指定から、書き出しの設定を作る。`prompt` は、先頭に付ける別の AI への依頼文(`summarize` / `continue` / `review`。空・`none` は付けない。知らない名前は、断る)。
/// `include_calls` は、ツールの呼び出しも書き出すか(省略したら、書き出す)。
fn export_options(keep_secrets: bool, lang: &str, utc_offset_minutes: i32, prompt: Option<&str>, include_calls: Option<bool>) -> Result<synclane::export::Options, String> {
    let prompt = match prompt {
        None => synclane::export::Prompt::None,
        Some(name) => synclane::export::Prompt::parse(name).ok_or_else(|| format!("知らない依頼文です: {name}"))?,
    };
    Ok(synclane::export::Options {
        prompt,
        include_calls: include_calls.unwrap_or(true),
        secrets: if keep_secrets { synclane::export::Secrets::Keep } else { synclane::export::Secrets::Mask },
        lang: if lang == "ja" { synclane::export::Lang::Ja } else { synclane::export::Lang::En },
        utc_offset_minutes,
    })
}

/// 指した発言が属する区切りを、Markdown にする(コピーする前の見本にも、そのまま使う)。外には何も送らない。
/// `keep_secrets` が true なら、秘密に見えるものも、そのまま書き出す(既定は、伏せる)。`utc_offset_minutes` は、画面の端末の時差。
#[tauri::command]
fn export_section(
    event_id: i64,
    keep_secrets: bool,
    lang: String,
    utc_offset_minutes: i32,
    prompt: Option<String>,
    include_calls: Option<bool>,
    state: tauri::State<'_, AppState>,
) -> Result<synclane::export::Exported, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let options = export_options(keep_secrets, &lang, utc_offset_minutes, prompt.as_deref(), include_calls)?;
    synclane::export::section_markdown(&conn, event_id, &options).map_err(|e| format!("{e:#}"))
}

/// 画面に出ている範囲(日付・プロジェクト・検索語)の話題を、1 つの Markdown にする(新しい 50 件まで)。外には何も送らない。
/// `label` は、書き出しの冒頭に書く範囲の説明(画面が作る)。
#[tauri::command]
#[allow(clippy::too_many_arguments)]
fn export_range(
    date_from: Option<String>,
    date_to: Option<String>,
    project: Option<String>,
    session_id: Option<i64>,
    query: Option<String>,
    label: Option<String>,
    keep_secrets: bool,
    lang: String,
    utc_offset_minutes: i32,
    prompt: Option<String>,
    include_calls: Option<bool>,
    state: tauri::State<'_, AppState>,
) -> Result<synclane::export::Exported, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let options = export_options(keep_secrets, &lang, utc_offset_minutes, prompt.as_deref(), include_calls)?;
    let range = synclane::export::Range {
        date_from: date_from.as_deref(),
        date_to: date_to.as_deref(),
        project: project.as_deref(),
        session_id,
        query: query.as_deref(),
        label: label.as_deref(),
    };
    synclane::export::range_markdown(&conn, &range, &options).map_err(|e| format!("{e:#}"))
}

/// その発言の会話を、元のツール(Claude Code・Codex)で再開するコマンド。対応していない会話は None。コマンドを返すだけで、実行はしない。
#[tauri::command]
fn resume_command(event_id: i64, state: tauri::State<'_, AppState>) -> Result<Option<String>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    synclane::resume::command(&conn, event_id).map_err(|e| format!("{e:#}"))
}

/// その発言の会話を、新しいターミナルの窓で再開する(macOS)。実行したコマンドを返す。
#[tauri::command]
fn resume_open(event_id: i64, state: tauri::State<'_, AppState>) -> Result<String, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    synclane::resume::open_in_terminal(&conn, event_id).map_err(|e| format!("{e:#}"))
}

/// 指した発言が属する区切りに、題名を付ける。空なら、区切りの題名を外す(会話全体の題名に戻る)。
/// 付けた題名を返す(外したときは、空の文字列)。
#[tauri::command]
fn section_title_set(event_id: i64, title: String, state: tauri::State<'_, AppState>) -> Result<String, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    synclane::sections::set_title(&conn, event_id, &title).map(Option::unwrap_or_default).map_err(|e| format!("{e:#}"))
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
        .and_then(|conn| {
            // サーバーに接続していれば、team / all の検索も使える(していなければ、手元だけ)
            let transport = synclane::sync::api::UreqTransport::new();
            let remote = synclane::sync::remote::Remote::from_settings(&conn, &synclane::sync::secrets::KeyringStore, &transport);
            synclane::mcp::serve_with(&conn, remote.as_ref(), std::io::stdin().lock(), std::io::stdout().lock())
        });
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

#[tauri::command]
fn autostart_is_enabled(app: tauri::AppHandle) -> Result<bool, String> {
    app.autolaunch().is_enabled().map_err(|e| e.to_string())
}

#[tauri::command]
fn autostart_set_enabled(app: tauri::AppHandle, enabled: bool) -> Result<(), String> {
    if enabled {
        app.autolaunch().enable().map_err(|e| format!("自動起動の登録に失敗しました: {e}"))
    } else {
        app.autolaunch().disable().map_err(|e| format!("自動起動の解除に失敗しました: {e}"))
    }
}

/// メニューの「設定…」(⌘,)。選ぶと、画面に `open-settings` を知らせる。
const OPEN_SETTINGS: &str = "open-settings";
/// メニューの「検索…」(⌘F)。選ぶと、画面に `open-search` を知らせる。
const OPEN_SEARCH: &str = "open-search";
/// メニューの「SyncLane を終了」(⌘Q)。
const APP_QUIT: &str = "app-quit";

/// トレイメニューの各項目 ID
const TRAY_SHOW: &str = "tray-show";
const TRAY_SEARCH: &str = "tray-search";
const TRAY_QUIT: &str = "tray-quit";

/// メニューバーのトレイアイコンを組む。
fn setup_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    let ja = sys_locale::get_locale().map(|l| l.to_lowercase().starts_with("ja")).unwrap_or(false);
    let show_item = MenuItem::with_id(app, TRAY_SHOW, if ja { "SyncLane を表示" } else { "Show SyncLane" }, true, None::<&str>)?;
    let search_item = MenuItem::with_id(app, TRAY_SEARCH, if ja { "検索…" } else { "Find…" }, true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit_item = MenuItem::with_id(app, TRAY_QUIT, if ja { "SyncLane を終了" } else { "Quit SyncLane" }, true, None::<&str>)?;
    let tray_menu = Menu::with_items(app, &[&show_item, &search_item, &separator, &quit_item])?;

    let icon = app.default_window_icon().cloned().or_else(|| {
        Image::from_bytes(include_bytes!("../icons/32x32.png")).ok()
    });

    let mut builder = TrayIconBuilder::new()
        .menu(&tray_menu)
        .show_menu_on_left_click(true)
        .tooltip("SyncLane")
        .on_menu_event(|app, event| {
            let id = event.id();
            if id == TRAY_SHOW {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.unminimize();
                    let _ = w.set_focus();
                }
            } else if id == TRAY_SEARCH {
                let _ = open_search_window(app.clone());
            } else if id == TRAY_QUIT {
                IS_QUITTING.store(true, Ordering::SeqCst);
                app.exit(0);
            }
        });

    if let Some(icon) = icon {
        builder = builder.icon(icon);
    }

    let _tray = builder.build(app)?;

    Ok(())
}

/// 画面上部のメニューバー。既定のメニューは英語固定なので、システムの言語に合わせて自前で組む。
fn build_menu(app: &tauri::AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let ja = sys_locale::get_locale().map(|l| l.to_lowercase().starts_with("ja")).unwrap_or(false);
    let t = |ja_text: &'static str, en_text: &'static str| Some(if ja { ja_text } else { en_text });
    let app_menu = Submenu::with_items(
        app,
        "SyncLane",
        true,
        &[
            &PredefinedMenuItem::about(app, t("SyncLane について", "About SyncLane"), None)?,
            &PredefinedMenuItem::separator(app)?,
            // ⌘, は、メニューの項目として持つ(画面の中でキーを拾う方式は、画面が入力の受け手でないと届かない)
            &MenuItem::with_id(app, OPEN_SETTINGS, if ja { "設定…" } else { "Settings…" }, true, Some("CmdOrCtrl+,"))?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::services(app, t("サービス", "Services"))?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::hide(app, t("SyncLane を隠す", "Hide SyncLane"))?,
            &PredefinedMenuItem::hide_others(app, t("ほかを隠す", "Hide Others"))?,
            &PredefinedMenuItem::show_all(app, t("すべてを表示", "Show All"))?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, APP_QUIT, if ja { "SyncLane を終了" } else { "Quit SyncLane" }, true, Some("CmdOrCtrl+Q"))?,
        ],
    )?;
    let edit_menu = Submenu::with_items(
        app,
        if ja { "編集" } else { "Edit" },
        true,
        &[
            &PredefinedMenuItem::undo(app, t("取り消す", "Undo"))?,
            &PredefinedMenuItem::redo(app, t("やり直す", "Redo"))?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::cut(app, t("切り取り", "Cut"))?,
            &PredefinedMenuItem::copy(app, t("コピー", "Copy"))?,
            &PredefinedMenuItem::paste(app, t("ペースト", "Paste"))?,
            &PredefinedMenuItem::select_all(app, t("すべてを選択", "Select All"))?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, OPEN_SEARCH, if ja { "検索…" } else { "Find…" }, true, Some("CmdOrCtrl+F"))?,
        ],
    )?;
    let view_menu = Submenu::with_items(
        app,
        if ja { "表示" } else { "View" },
        true,
        &[&PredefinedMenuItem::fullscreen(app, t("フルスクリーンにする", "Enter Full Screen"))?],
    )?;
    let window_menu = Submenu::with_items(
        app,
        if ja { "ウインドウ" } else { "Window" },
        true,
        &[
            &PredefinedMenuItem::minimize(app, t("しまう", "Minimize"))?,
            &PredefinedMenuItem::maximize(app, t("拡大／縮小", "Zoom"))?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::close_window(app, t("ウインドウを閉じる", "Close Window"))?,
        ],
    )?;
    Menu::with_items(app, &[&app_menu, &edit_menu, &view_menu, &window_menu])
}

fn collect_codex(conn: &Arc<Mutex<Connection>>, root: &std::path::Path, handle: &tauri::AppHandle) {
    let files = match importer::codex_files(root) {
        Ok(files) => files,
        Err(error) => return eprintln!("synclane codex: {error:#}"),
    };
    let mut added = 0;
    for path in files {
        let offset = match conn.lock() {
            Ok(conn) => importer::codex_saved_offset(&conn, &path),
            Err(_) => return,
        };
        let parsed = match offset.and_then(|offset| importer::parse_codex_file(&path, offset, None)) {
            Ok(parsed) => parsed,
            Err(error) => {
                eprintln!("synclane codex: {}: {error:#}", path.display());
                continue;
            }
        };
        let mut report = importer::ImportReport::default();
        if let Ok(mut conn) = conn.lock() {
            match importer::store_codex(&mut conn, &path, &parsed, true, &mut report) {
                Ok(()) => added += report.events,
                Err(error) => eprintln!("synclane codex: {}: {error:#}", path.display()),
            }
        }
    }
    if added > 0 {
        let _ = handle.emit("events-updated", added);
    }
}

/// 会話ログディレクトリの変更を macOS FSEvents で監視する Watcher を生成する。
/// イベント発生時は対象ツールを判定してチャンネルへ送信する。
fn setup_watcher(
    tx: std::sync::mpsc::Sender<Option<ToolKind>>,
    dirs: &[std::path::PathBuf],
) -> Option<notify::RecommendedWatcher> {
    let watcher_res = notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
        Ok(event) => {
            if is_relevant_event_kind(&event.kind) {
                let mut found_any = false;
                for path in &event.paths {
                    if let Some(tool) = detect_tool_from_path(path) {
                        let _ = tx.send(Some(tool));
                        found_any = true;
                    }
                }
                // 特定できなかったが監視ディレクトリ内の変更だった場合
                if !found_any && !event.paths.is_empty() {
                    let _ = tx.send(None);
                }
            }
        }
        Err(error) => {
            eprintln!("synclane watcher event error: {error:#}");
            // エラー時も念のため全体収集をトリガー
            let _ = tx.send(None);
        }
    });

    let mut watcher = match watcher_res {
        Ok(w) => w,
        Err(error) => {
            eprintln!("synclane watcher init failed: {error:#}");
            return None;
        }
    };

    for dir in dirs {
        if dir.exists() {
            if let Err(error) = watcher.watch(dir, notify::RecursiveMode::Recursive) {
                eprintln!("synclane watcher watch failed for {}: {error:#}", dir.display());
            }
        } else {
            eprintln!("synclane watcher: directory does not exist yet (will retry in periodic loop): {}", dir.display());
        }
    }

    Some(watcher)
}

fn main() {
    if std::env::args().any(|arg| arg == "--mcp") {
        run_mcp();
    }
    let path = db::default_db_path().expect("failed to resolve SyncLane database path");
    let conn = db::open(&path).expect("failed to open SyncLane database");

    let shared_conn = Arc::new(Mutex::new(conn));
    let sync_state = sync_app::SyncState::new(path.clone());
    let collector_conn = Arc::clone(&shared_conn);
    let ingest_conn = Arc::clone(&shared_conn);
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--minimized"]),
        ))
        .on_menu_event(|app, event| {
            let id = event.id();
            if id == OPEN_SETTINGS || id == OPEN_SEARCH {
                let _ = app.emit(id.as_ref(), ());
            } else if id == APP_QUIT {
                IS_QUITTING.store(true, Ordering::SeqCst);
                app.exit(0);
            }
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if should_hide_on_close(window.label(), IS_QUITTING.load(Ordering::SeqCst)) {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .manage(AppState(shared_conn))
        .manage(sync_state.clone())
        .setup(move |app| {
            app.set_menu(build_menu(app.handle())?)?;
            setup_tray(app.handle())?;
            if std::env::args().any(|arg| arg == "--minimized") {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.hide();
                }
            }
            // サーバーへの送信(接続したときだけ動く。接続していなければ、何もしない)
            sync_app::spawn_loop(sync_state, app.handle().clone());
            // ブラウザ拡張(Web 版の Gemini)からの取り込みを受ける
            let ingest_handle = app.handle().clone();
            ingest_server::spawn(ingest_server::ADDRESS, Arc::clone(&ingest_conn), move |changed| {
                let _ = ingest_handle.emit("events-updated", changed);
            });
            let handle = app.handle().clone();
            thread::spawn(move || {
                let root = synclane::paths::claude_projects_dir();
                // 題名の対応より前に取り込んだ会話に、題名を入れる(起動のたびに1回。入っているものは飛ばす)
                if let Ok(mut conn) = collector_conn.lock() {
                    if let Err(error) = importer::backfill_claude_titles(&root, &mut conn) {
                        eprintln!("synclane title backfill: {error:#}");
                    }
                }
                // 要約して続けた位置を、すでに取り込んだ会話にも、さかのぼって残す(1 度だけ)
                if let Ok(mut conn) = collector_conn.lock() {
                    if let Err(error) = importer::backfill_claude_breaks(&root, &mut conn) {
                        eprintln!("synclane section breaks: {error:#}");
                    }
                }
                let codex_root = synclane::paths::codex_sessions_dir();
                let copilot_root = synclane::paths::copilot_sessions_dir();
                let gemini_root = synclane::paths::gemini_tmp_dir();
                let antigravity_root = synclane::paths::antigravity_cli_dir();
                // ツールの呼び出し(実行したコマンド・触ったファイル)を、すでに取り込んだ会話にも、さかのぼって保存する(1 度だけ)
                if let Ok(mut conn) = collector_conn.lock() {
                    if let Err(error) = importer::backfill_tool_calls_claude(&root, &mut conn) {
                        eprintln!("synclane tool calls (claude): {error:#}");
                    }
                }
                if let Err(error) = importer::backfill_tool_calls_codex(&collector_conn, &codex_root) {
                    eprintln!("synclane tool calls (codex): {error:#}");
                }
                // トークン数を、すでに取り込んだ会話にも、さかのぼって保存する(1 度だけ)
                if let Ok(mut conn) = collector_conn.lock() {
                    if let Err(error) = importer::backfill_token_usage_claude(&root, &mut conn) {
                        eprintln!("synclane token usage (claude): {error:#}");
                    }
                }
                if let Err(error) = importer::backfill_token_usage_codex(&collector_conn, &codex_root) {
                    eprintln!("synclane token usage (codex): {error:#}");
                }
                // Gemini CLI は、覚えている大きさを 1 度だけ消して、次の収集で読み直させる
                if let Ok(conn) = collector_conn.lock() {
                    if let Err(error) = importer::backfill_token_usage_gemini(&conn) {
                        eprintln!("synclane token usage (gemini): {error:#}");
                    }
                }
                // 会話ログの変更検知(macOS FSEvents)を設定する
                let (notify_tx, notify_rx) = channel::<Option<ToolKind>>();
                let watch_dirs = vec![
                    root.clone(),
                    antigravity_root.join("conversations"),
                    codex_root.clone(),
                    copilot_root.clone(),
                    gemini_root.clone(),
                ];
                let mut watcher = setup_watcher(notify_tx, &watch_dirs);

                // ツール別の増分取り込みヘルパー(None のときは全ツールを実行)
                let collect_tools = |tools_to_run: Option<&std::collections::HashSet<ToolKind>>| {
                    let should_run = |tool: ToolKind| tools_to_run.map(|s| s.contains(&tool)).unwrap_or(true);

                    if should_run(ToolKind::Claude) {
                        if let Ok(mut conn) = collector_conn.lock() {
                            match importer::collect_claude_incremental(&root, &mut conn) {
                                Ok(report) if report.events > 0 => { let _ = handle.emit("events-updated", report.events); }
                                Ok(_) => {}
                                Err(error) => eprintln!("synclane collector: {error:#}"),
                            }
                        }
                    }
                    if should_run(ToolKind::Codex) {
                        collect_codex(&collector_conn, &codex_root, &handle);
                    }
                    if should_run(ToolKind::Copilot) {
                        if let Ok(mut conn) = collector_conn.lock() {
                            match importer::collect_copilot_incremental(&copilot_root, &mut conn) {
                                Ok(report) if report.events > 0 => { let _ = handle.emit("events-updated", report.events); }
                                Ok(_) => {}
                                Err(error) => eprintln!("synclane copilot: {error:#}"),
                            }
                        }
                    }
                    if should_run(ToolKind::Gemini) {
                        if let Ok(mut conn) = collector_conn.lock() {
                            match importer::collect_gemini_incremental(&gemini_root, &mut conn) {
                                Ok(report) if report.events > 0 => { let _ = handle.emit("events-updated", report.events); }
                                Ok(_) => {}
                                Err(error) => eprintln!("synclane gemini: {error:#}"),
                            }
                        }
                    }
                    if should_run(ToolKind::Antigravity) {
                        if let Ok(mut conn) = collector_conn.lock() {
                            match importer::collect_antigravity_incremental(&antigravity_root, &mut conn) {
                                Ok(report) if report.events > 0 => { let _ = handle.emit("events-updated", report.events); }
                                Ok(_) => {}
                                Err(error) => eprintln!("synclane antigravity: {error:#}"),
                            }
                        }
                    }
                };

                // 起動直後の初回収集(全ツール)
                collect_tools(None);

                loop {
                    match wait_with_debounce(&notify_rx, Duration::from_secs(60), Duration::from_millis(500)) {
                        WaitResult::Event(items) => {
                            // イベント検知: 変更されたツールだけをピンポイントで収集
                            let mut specific_tools = std::collections::HashSet::new();
                            let mut run_all = false;
                            for item in items {
                                match item {
                                    Some(tool) => { specific_tools.insert(tool); }
                                    None => { run_all = true; break; }
                                }
                            }
                            if run_all || specific_tools.is_empty() {
                                collect_tools(None);
                            } else {
                                collect_tools(Some(&specific_tools));
                            }
                        }
                        WaitResult::Timeout => {
                            // 60秒定期バックアップ: 全ツールを収集
                            collect_tools(None);
                            // 起動時に存在しなかったディレクトリがあれば監視を追加登録
                            if let Some(ref mut w) = watcher {
                                for dir in &watch_dirs {
                                    if dir.exists() {
                                        let _ = w.watch(dir, notify::RecursiveMode::Recursive);
                                    }
                                }
                            }
                        }
                        WaitResult::Disconnected => {
                            // 受信チャンネル切断時もループを抜けず、定期収集を継続
                            eprintln!("synclane watcher: channel disconnected, continuing periodic loop");
                            collect_tools(None);
                            thread::sleep(Duration::from_secs(60));
                        }
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            status, timeline, timeline_days, project_overview, sessions_by_tool, usage_summary, mcp_executable, open_search_window, close_search_window,
            autostart_is_enabled, autostart_set_enabled,
            section_title_suggest, section_title_set, export_section, export_range, resume_command, resume_open,
            sync_app::sync_overview, sync_app::sync_sessions, sync_app::sync_set_mode, sync_app::sync_select,
            sync_app::sync_select_all, sync_app::sync_rules, sync_app::sync_set_rules, sync_app::sync_connect, sync_app::sync_connect_cancel,
            sync_app::sync_disconnect, sync_app::sync_now, sync_app::mask_lists_get, sync_app::mask_lists_set
        ])
        .build(tauri::generate_context!())
        .expect("error while building SyncLane");

    app.run(|app_handle, event| {
        match event {
            tauri::RunEvent::Reopen { .. } => {
                if let Some(window) = app_handle.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.unminimize();
                    let _ = window.set_focus();
                }
            }
            tauri::RunEvent::ExitRequested { .. } => {
                if !IS_QUITTING.load(Ordering::SeqCst) {
                    IS_QUITTING.store(true, Ordering::SeqCst);
                }
            }
            _ => {}
        }
    });
}
