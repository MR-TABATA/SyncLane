mod ingest_server;
mod sync_app;

use synclane::{db, importer};
use synclane::db::TimelineEvent;
use rusqlite::Connection;
use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

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

#[tauri::command]
fn timeline(
    date_from: Option<String>,
    date_to: Option<String>,
    query: Option<String>,
    limit: usize,
    sort: Option<String>,
    project: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<TimelineEvent>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let order = if sort.as_deref() == Some("relevance") { db::SearchOrder::Relevance } else { db::SearchOrder::Newest };
    db::timeline_in_project(&conn, date_from.as_deref(), date_to.as_deref(), project.as_deref(), query.as_deref(), limit, order).map_err(|e| e.to_string())
}

/// 「続きから」の一覧(サイドバー)。プロジェクトごとに、ツールごとの最新の会話を返す。読み取りだけ。
#[tauri::command]
fn project_overview(state: tauri::State<'_, AppState>) -> Result<Vec<synclane::projects::ProjectRow>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    synclane::projects::overview(&conn).map_err(|e| format!("{e:#}"))
}

/// 題名の提案(手元の規則)。指した発言が属する区切り(話題のまとまり)の、最近の発言から出す。外には何も送らない。
#[tauri::command]
fn section_title_suggest(event_id: i64, state: tauri::State<'_, AppState>) -> Result<String, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let section = synclane::sections::load(&conn, event_id).map_err(|e| format!("{e:#}"))?;
    synclane::titles::suggest_local(&section.messages).ok_or_else(|| "提案できる発言がありません".to_string())
}

/// 指した発言が属する区切りを、Markdown にする(コピーする前の見本にも、そのまま使う)。外には何も送らない。
/// `keep_secrets` が true なら、秘密に見えるものも、そのまま書き出す(既定は、伏せる)。`utc_offset_minutes` は、画面の端末の時差。
/// `prompt` は、先頭に付ける別の AI への依頼文(`summarize` / `continue` / `review`。空・`none` は付けない)。知らない名前は、断る。
#[tauri::command]
fn export_section(
    event_id: i64,
    keep_secrets: bool,
    lang: String,
    utc_offset_minutes: i32,
    prompt: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<synclane::export::Exported, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let prompt = match prompt.as_deref() {
        None => synclane::export::Prompt::None,
        Some(name) => synclane::export::Prompt::parse(name).ok_or_else(|| format!("知らない依頼文です: {name}"))?,
    };
    let options = synclane::export::Options {
        prompt,
        secrets: if keep_secrets { synclane::export::Secrets::Keep } else { synclane::export::Secrets::Mask },
        lang: if lang == "ja" { synclane::export::Lang::Ja } else { synclane::export::Lang::En },
        utc_offset_minutes,
    };
    synclane::export::section_markdown(&conn, event_id, &options).map_err(|e| format!("{e:#}"))
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

/// メニューの「設定…」(⌘,)。選ぶと、画面に `open-settings` を知らせる。
const OPEN_SETTINGS: &str = "open-settings";
/// メニューの「検索…」(⌘F)。選ぶと、画面に `open-search` を知らせる。
const OPEN_SEARCH: &str = "open-search";

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
            &PredefinedMenuItem::quit(app, t("SyncLane を終了", "Quit SyncLane"))?,
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
    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .on_menu_event(|app, event| {
            let id = event.id();
            if id == OPEN_SETTINGS || id == OPEN_SEARCH {
                let _ = app.emit(id.as_ref(), ());
            }
        })
        .manage(AppState(shared_conn))
        .manage(sync_state.clone())
        .setup(move |app| {
            app.set_menu(build_menu(app.handle())?)?;
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
                    // Codex のログは数百 MB になる。読んで解析する間は DB のロックを持たず、書くときだけ持つ
                    // (初回の取り込みで、画面が固まらないように)
                    collect_codex(&collector_conn, &codex_root, &handle);
                    // Gemini CLI の記録は小さい。大きさが変わったファイルだけ、読み直す
                    if let Ok(mut conn) = collector_conn.lock() {
                        match importer::collect_gemini_incremental(&gemini_root, &mut conn) {
                            Ok(report) if report.events > 0 => {
                                let _ = handle.emit("events-updated", report.events);
                            }
                            Ok(_) => {}
                            Err(error) => eprintln!("synclane gemini: {error:#}"),
                        }
                    }
                    // Antigravity CLI の会話。書き換わったものだけ、読み直す
                    if let Ok(mut conn) = collector_conn.lock() {
                        match importer::collect_antigravity_incremental(&antigravity_root, &mut conn) {
                            Ok(report) if report.events > 0 => {
                                let _ = handle.emit("events-updated", report.events);
                            }
                            Ok(_) => {}
                            Err(error) => eprintln!("synclane antigravity: {error:#}"),
                        }
                    }
                    thread::sleep(Duration::from_secs(30));
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            status, timeline, project_overview, mcp_executable, open_search_window, close_search_window,
            section_title_suggest, section_title_set, export_section, resume_command, resume_open,
            sync_app::sync_overview, sync_app::sync_sessions, sync_app::sync_set_mode, sync_app::sync_select,
            sync_app::sync_select_all, sync_app::sync_rules, sync_app::sync_set_rules, sync_app::sync_connect, sync_app::sync_connect_cancel,
            sync_app::sync_disconnect, sync_app::sync_now, sync_app::mask_lists_get, sync_app::mask_lists_set
        ])
        .run(tauri::generate_context!())
        .expect("error while running SyncLane");
}
