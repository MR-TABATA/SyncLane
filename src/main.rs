use anyhow::Result;
use clap::{Parser, Subcommand};
use synclane::sync::{api::UreqTransport, device_flow, engine, ops, secrets::KeyringStore, store::{self, Mode}};
use synclane::{db, importer};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "synclane", version, about = "SyncLane local journal store")]
struct Cli {
    /// SQLite database path. Defaults to ~/Library/Application Support/SyncLane/synclane.db.
    #[arg(long, global = true)]
    db: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create the database and apply schema migrations.
    Init,
    /// Show database schema and record counts.
    Status,
    /// Show recent imported events in reverse chronological order.
    Timeline {
        /// Show only events whose timestamp starts with YYYY-MM-DD.
        #[arg(long)]
        date: Option<String>,
        /// Maximum number of events to show.
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Run an MCP server over stdio so AI clients can search saved conversations (read-only).
    Mcp,
    /// Import logs from a supported source.
    Import {
        #[command(subcommand)]
        source: ImportSource,
    },
    /// Connect to a SyncLane server (optional; nothing leaves this computer unless you connect).
    Server {
        #[command(subcommand)]
        action: ServerAction,
    },
    /// Send the conversations you picked to the connected server.
    Sync {
        #[command(subcommand)]
        action: Option<SyncAction>,
        /// Only count what would be sent.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum ServerAction {
    /// Register this computer with a server (approve in your browser).
    Connect {
        /// Server address, e.g. https://sync.example.com
        url: String,
        /// How this computer is shown on the server. Defaults to the computer name.
        #[arg(long)]
        name: Option<String>,
    },
    /// Show the connection.
    Status,
    /// Disconnect and forget what was sent (the server keeps what it already has).
    Disconnect,
}

#[derive(Subcommand)]
enum SyncAction {
    /// Show what is selected, sent and waiting.
    Status,
    /// Show or change the mode: manual (only what you pick) or auto (everything except what you exclude).
    Mode {
        mode: Option<String>,
    },
    /// List conversations and whether they are sent.
    List {
        #[arg(long, default_value_t = 30)]
        limit: usize,
    },
    /// Pick conversations to send (by session id), or --all.
    Include {
        ids: Vec<i64>,
        #[arg(long, conflicts_with = "ids")]
        all: bool,
        /// private (only you) or org (everyone on the server). Defaults to the server's setting.
        #[arg(long)]
        visibility: Option<String>,
    },
    /// Never send these conversations.
    Exclude {
        #[arg(required = true)]
        ids: Vec<i64>,
    },
    /// Remove your choice for these conversations (they follow the mode again).
    Reset {
        #[arg(required = true)]
        ids: Vec<i64>,
    },
}

#[derive(Subcommand)]
enum ImportSource {
    /// Import Claude Code JSONL sessions.
    Claude {
        /// Claude Code projects directory. Defaults to ~/.claude/projects.
        #[arg(long)]
        root: Option<PathBuf>,
        /// Import only events whose timestamp starts with YYYY-MM-DD.
        #[arg(long)]
        date: Option<String>,
        /// Import all events in the Claude Code projects directory.
        #[arg(long, conflicts_with = "date")]
        all: bool,
    },
    /// Import Gemini CLI JSONL sessions.
    Gemini {
        /// Gemini CLI temporary session directory. Defaults to ~/.gemini/tmp.
        #[arg(long)]
        root: Option<PathBuf>,
        /// Import only events whose timestamp starts with YYYY-MM-DD.
        #[arg(long)]
        date: Option<String>,
        /// Import all events in the Gemini CLI session directory.
        #[arg(long, conflicts_with = "date")]
        all: bool,
    },
    /// Import Codex (ChatGPT Codex app / Codex CLI) sessions.
    Codex {
        /// Codex sessions directory. Defaults to ~/.codex/sessions.
        #[arg(long)]
        root: Option<PathBuf>,
        /// Import only events whose timestamp starts with YYYY-MM-DD.
        #[arg(long)]
        date: Option<String>,
        /// Import all events in the Codex sessions directory.
        #[arg(long, conflicts_with = "date")]
        all: bool,
    },
}

fn main() {
    if let Err(error) = run() {
        eprintln!("synclane: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let path = cli.db.unwrap_or(db::default_db_path()?);
    if matches!(cli.command, Command::Mcp) {
        // 標準出力は JSON-RPC 専用。読み取り専用で開き、マイグレーションはしない
        let conn = db::open_readonly(&path)?;
        return synclane::mcp::serve(&conn, std::io::stdin().lock(), std::io::stdout().lock());
    }
    let mut conn = db::open(&path)?;

    match cli.command {
        Command::Mcp => unreachable!("handled above"),
        Command::Init => {
            println!("initialized {} (schema v{})", path.display(), db::schema_version(&conn)?);
        }
        Command::Status => {
            let version = db::schema_version(&conn)?;
            let counts = db::counts(&conn)?;
            println!("database: {}", path.display());
            println!("schema: v{version}");
            println!("sessions: {}", counts.sessions);
            println!("events: {}", counts.events);
            println!("journals: {}", counts.journals);
            println!("analyses: {}", counts.analyses);
        }
        Command::Timeline { date, limit } => {
            let events = db::timeline(&conn, date.as_deref(), None, limit)?;
            if events.is_empty() {
                println!("no events");
            } else {
                for event in events {
                    let content: String = event.content.chars().take(240).collect();
                    let suffix = if event.content.chars().count() > 240 { "…" } else { "" };
                    println!("{} [{}] {}", event.occurred_at, event.role, event.project_path);
                    println!("  {}{}", content.replace('\n', "\\n"), suffix);
                }
            }
        }
        Command::Server { action } => server_command(&conn, action)?,
        Command::Sync { action, dry_run } => sync_command(&conn, action, dry_run)?,
        Command::Import { source } => match source {
            ImportSource::Claude { root, date, all } => {
            let root = root.unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                    .join(".claude")
                    .join("projects")
            });
            // --all is explicit documentation; omitting both flags keeps the original
            // behavior and also imports all events.
            let date_filter = if all { None } else { date.as_deref() };
            let report = importer::import_claude(&root, date_filter, &mut conn)?;
            println!("root: {}", root.display());
            println!("scope: {}", if date_filter.is_some() { date_filter.unwrap() } else { "all" });
            println!("files: {}", report.files);
            println!("sessions: {}", report.sessions);
            println!("events added: {}", report.events);
            println!("skipped lines: {}", report.skipped_lines);
            }
            ImportSource::Gemini { root, date, all } => {
                let root = root.unwrap_or_else(|| {
                    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                        .join(".gemini")
                        .join("tmp")
                });
                let date_filter = if all { None } else { date.as_deref() };
                let report = importer::import_gemini(&root, date_filter, &mut conn)?;
                println!("root: {}", root.display());
                println!("scope: {}", if date_filter.is_some() { date_filter.unwrap() } else { "all" });
                println!("files: {}", report.files);
                println!("sessions: {}", report.sessions);
                println!("events added: {}", report.events);
                println!("skipped lines: {}", report.skipped_lines);
            }
            ImportSource::Codex { root, date, all } => {
                let root = root.unwrap_or_else(|| {
                    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                        .join(".codex")
                        .join("sessions")
                });
                let date_filter = if all { None } else { date.as_deref() };
                let report = importer::import_codex(&root, date_filter, &mut conn)?;
                println!("root: {}", root.display());
                println!("scope: {}", date_filter.unwrap_or("all"));
                println!("files: {}", report.files);
                println!("sessions: {}", report.sessions);
                println!("events added: {}", report.events);
                println!("skipped lines: {}", report.skipped_lines);
            }
        },
    }
    Ok(())
}

fn open_in_browser(url: &str) {
    let result = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).status()
    } else if cfg!(target_os = "windows") {
        std::process::Command::new("cmd").args(["/C", "start", "", url]).status()
    } else {
        std::process::Command::new("xdg-open").arg(url).status()
    };
    if result.map(|s| !s.success()).unwrap_or(true) {
        println!("(ブラウザを開けませんでした。上のアドレスを自分で開いてください)");
    }
}

fn require_connection(conn: &rusqlite::Connection) -> Result<store::ServerConnection> {
    store::load_connection(conn)?.ok_or_else(|| anyhow::anyhow!("サーバーに接続していません。`synclane server connect <URL>` で接続してください"))
}

fn server_command(conn: &rusqlite::Connection, action: ServerAction) -> Result<()> {
    let transport = UreqTransport::new();
    let secrets = KeyringStore;
    match action {
        ServerAction::Connect { url, name } => {
            if let Some(existing) = store::load_connection(conn)? {
                anyhow::bail!("すでに {} に接続しています。先に `synclane server disconnect` で解除してください", existing.server);
            }
            let name = name.unwrap_or_else(device_flow::default_device_name);
            let pending = device_flow::begin(&transport, &url, &name, env!("CARGO_PKG_VERSION"))?;
            println!("ブラウザで次のコードを承認してください。");
            println!("  コード: {}", pending.start.user_code);
            println!("  アドレス: {}", pending.start.verification_uri_complete);
            println!("  端末の名前: {name}");
            open_in_browser(&pending.start.verification_uri_complete);
            println!("承認を待っています…(Ctrl+C で中止)");
            let connection = device_flow::finish(conn, &transport, &secrets, &pending, &std::thread::sleep, &|| false)?;
            println!("接続しました: {} ({} / {})", connection.server, connection.user_name, connection.user_email);
            println!("今は何も送りません。送る会話を選ぶか(`synclane sync include …`)、`synclane sync mode auto` にしてください。");
        }
        ServerAction::Status => {
            let overview = ops::overview(conn)?;
            match overview.connection {
                None => println!("接続していません(外には何も送りません)"),
                Some(c) => {
                    println!("サーバー: {}", c.server);
                    println!("利用者: {} ({})", c.user_name, c.user_email);
                    println!("端末: {}", c.device_id);
                    println!("モード: {}", overview.mode);
                }
            }
        }
        ServerAction::Disconnect => {
            if store::load_connection(conn)?.is_none() {
                println!("接続していません");
            } else {
                ops::disconnect(conn, &secrets)?;
                println!("接続を解除しました(サーバーに送り済みの会話は、サーバーに残ります)");
            }
        }
    }
    Ok(())
}

fn sync_command(conn: &rusqlite::Connection, action: Option<SyncAction>, dry_run: bool) -> Result<()> {
    match action {
        None => {
            let transport = UreqTransport::new();
            let options = engine::Options { dry_run, max_batches: None };
            let report = engine::run(conn, &transport, &KeyringStore, &std::thread::sleep, &options)?;
            if dry_run {
                println!("送る対象の発言: {} 件(送っていません)", report.pending);
                return Ok(());
            }
            println!(
                "送信: {} バッチ、{} 件(新規 {}、更新 {}、重複 {}、拒否 {})。残り {} 件",
                report.batches, report.events_sent, report.created, report.updated, report.duplicate, report.rejected, report.pending
            );
            for sample in &report.rejected_samples {
                println!("  拒否: {sample}");
            }
            if let Some(stop) = report.stop {
                anyhow::bail!("{}", stop.message());
            }
        }
        Some(SyncAction::Status) => {
            let overview = ops::overview(conn)?;
            let Some(c) = overview.connection else {
                println!("接続していません");
                return Ok(());
            };
            let counts = overview.counts.unwrap_or_default();
            println!("サーバー: {}  モード: {}", c.server, overview.mode);
            println!("会話: 全部で {} 件、送る対象 {} 件、除外 {} 件", counts.sessions_total, counts.sessions_selected, counts.sessions_excluded);
            println!("発言: 送り済み {} 件、待ち {} 件", counts.events_sent, counts.events_pending);
            println!("最後に送った時刻: {}", counts.last_sent_at.as_deref().unwrap_or("まだ送っていません"));
        }
        Some(SyncAction::Mode { mode }) => match mode {
            None => println!("{}", store::mode(conn)?.as_str()),
            Some(value) => {
                let mode = Mode::parse(&value)?;
                store::set_mode(conn, mode)?;
                match mode {
                    Mode::Manual => println!("manual: 選んだ会話だけを送ります"),
                    Mode::Auto => println!("auto: 除外した会話以外の全部を送ります(これから取り込む会話も)"),
                }
            }
        },
        Some(SyncAction::List { limit }) => {
            let c = require_connection(conn)?;
            for row in ops::list_sessions(conn, &c.server, store::mode(conn)?, limit, 0)? {
                let mark = match row.state {
                    "send" => "送る  ",
                    "excluded" => "除外  ",
                    _ => "未選択",
                };
                println!(
                    "{:>6} {mark} {} {:<12} {:>5}件 (未送信 {}) {}",
                    row.id,
                    row.started_at.as_deref().unwrap_or("-").chars().take(10).collect::<String>(),
                    row.source_tool,
                    row.events,
                    row.unsent_events,
                    row.title.as_deref().unwrap_or(&row.project_path)
                );
            }
        }
        Some(SyncAction::Include { ids, all, visibility }) => {
            let c = require_connection(conn)?;
            if let Some(v) = visibility.as_deref() {
                anyhow::ensure!(matches!(v, "private" | "org"), "visibility は private か org です");
            }
            if all {
                let n = store::include_all(conn, &c.server, visibility.as_deref())?;
                println!("{n} 件の会話を送る対象にしました(除外した会話は、そのまま)");
            } else {
                anyhow::ensure!(!ids.is_empty(), "会話の ID か --all を指定してください(`synclane sync list` で確認できます)");
                for id in &ids {
                    store::set_session_state(conn, &c.server, *id, Some("send"), visibility.as_deref())?;
                }
                println!("{} 件の会話を送る対象にしました", ids.len());
            }
        }
        Some(SyncAction::Exclude { ids }) => {
            let c = require_connection(conn)?;
            for id in &ids {
                store::set_session_state(conn, &c.server, *id, Some("excluded"), None)?;
            }
            println!("{} 件の会話を、送らない対象にしました", ids.len());
        }
        Some(SyncAction::Reset { ids }) => {
            let c = require_connection(conn)?;
            for id in &ids {
                store::set_session_state(conn, &c.server, *id, None, None)?;
            }
            println!("{} 件の会話の指定を外しました(モードに従います)", ids.len());
        }
    }
    Ok(())
}
