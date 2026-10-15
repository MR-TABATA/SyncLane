use anyhow::Result;
use clap::{Parser, Subcommand};
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
        },
    }
    Ok(())
}
