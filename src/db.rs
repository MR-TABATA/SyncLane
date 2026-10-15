use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

pub const SCHEMA_VERSION: i64 = 3;

const MIGRATION_1: &str = r#"
CREATE TABLE IF NOT EXISTS schema_versions (
    version INTEGER PRIMARY KEY,
    applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS sessions (
    id INTEGER PRIMARY KEY,
    source_tool TEXT NOT NULL,
    project_path TEXT NOT NULL,
    started_at TEXT,
    ended_at TEXT,
    source_file TEXT,
    source_hash TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS events (
    id INTEGER PRIMARY KEY,
    session_id INTEGER NOT NULL REFERENCES sessions(id),
    source_tool TEXT NOT NULL,
    source_event_id TEXT NOT NULL,
    occurred_at TEXT NOT NULL,
    role TEXT NOT NULL,
    content TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(source_tool, source_event_id)
);

CREATE INDEX IF NOT EXISTS idx_events_session_sequence
    ON events(session_id, sequence);
CREATE INDEX IF NOT EXISTS idx_events_occurred_at
    ON events(occurred_at);

CREATE TABLE IF NOT EXISTS journals (
    id INTEGER PRIMARY KEY,
    journal_date TEXT NOT NULL,
    project_name TEXT NOT NULL,
    content_markdown TEXT NOT NULL,
    model TEXT,
    prompt_version TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_journals_date
    ON journals(journal_date);

CREATE TABLE IF NOT EXISTS analyses (
    id INTEGER PRIMARY KEY,
    period_start TEXT NOT NULL,
    period_end TEXT NOT NULL,
    analysis_type TEXT NOT NULL,
    result_json TEXT NOT NULL,
    model TEXT,
    prompt_version TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS tags (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE
);

CREATE TABLE IF NOT EXISTS event_tags (
    event_id INTEGER NOT NULL REFERENCES events(id),
    tag_id INTEGER NOT NULL REFERENCES tags(id),
    PRIMARY KEY(event_id, tag_id)
);

CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

const MIGRATION_2: &str = r#"
CREATE TABLE IF NOT EXISTS collector_files (
    source_tool TEXT NOT NULL,
    source_file TEXT NOT NULL,
    byte_offset INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY(source_tool, source_file)
);
"#;

// セッションの題名(Claude Code が付ける ai-title)。無いツールは NULL
const MIGRATION_3: &str = "ALTER TABLE sessions ADD COLUMN title TEXT;";

pub fn default_db_path() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("SYNCLANE_DB") {
        return Ok(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("SyncLane")
        .join("synclane.db"))
}

pub fn open(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let conn = Connection::open(path)
        .with_context(|| format!("failed to open database {}", path.display()))?;
    configure(&conn)?;
    Ok(conn)
}

#[cfg(test)]
pub fn open_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    configure(&conn)?;
    Ok(conn)
}

fn configure(conn: &Connection) -> Result<()> {
    conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")?;
    migrate(conn)
}

fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_versions (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );",
    )?;
    let mut current: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_versions",
        [],
        |row| row.get(0),
    )?;
    if current > SCHEMA_VERSION {
        bail!("database schema version {current} is newer than this SyncLane ({SCHEMA_VERSION})");
    }
    if current < 1 {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(MIGRATION_1)?;
        tx.execute(
            "INSERT INTO schema_versions(version) VALUES (?1)",
            params![1],
        )?;
        tx.commit()?;
        current = 1;
    }
    if current < 2 {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(MIGRATION_2)?;
        tx.execute(
            "INSERT INTO schema_versions(version) VALUES (?1)",
            params![2],
        )?;
        tx.commit()?;
        current = 2;
    }
    if current < 3 {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(MIGRATION_3)?;
        tx.execute(
            "INSERT INTO schema_versions(version) VALUES (?1)",
            params![3],
        )?;
        tx.commit()?;
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub struct Counts {
    pub sessions: i64,
    pub events: i64,
    pub journals: i64,
    pub analyses: i64,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct TimelineEvent {
    pub session_id: i64,
    pub occurred_at: String,
    pub source_tool: String,
    pub project_path: String,
    pub title: Option<String>,
    pub role: String,
    pub content: String,
}

pub fn timeline(
    conn: &Connection,
    date: Option<&str>,
    query: Option<&str>,
    limit: usize,
) -> Result<Vec<TimelineEvent>> {
    timeline_range(conn, date, None, query, limit)
}

/// 日付(YYYY-MM-DD)は、その日の0時(UTC)として扱う。時刻つき(…T…Z)ならそのまま使う。
/// 画面はローカルの日付で絞るので、ローカルの0時をUTCにした時刻が渡ってくる。
fn day_bound(value: &str) -> String {
    if value.contains('T') {
        value.to_owned()
    } else {
        format!("{value}T00:00:00")
    }
}

pub fn timeline_range(
    conn: &Connection,
    date_from: Option<&str>,
    date_to: Option<&str>,
    query: Option<&str>,
    limit: usize,
) -> Result<Vec<TimelineEvent>> {
    let mut sql = String::from(
        "SELECT e.session_id, e.occurred_at, e.source_tool, s.project_path, e.role, e.content, s.title
         FROM events e
         JOIN sessions s ON s.id = e.session_id",
    );
    let terms: Vec<&str> = query
        .unwrap_or_default()
        .split_whitespace()
        .filter(|term| !term.is_empty())
        .collect();
    let mut conditions: Vec<String> = Vec::new();
    if date_from.is_some() {
        conditions.push("e.occurred_at >= ?".to_owned());
    }
    if date_to.is_some() {
        conditions.push("e.occurred_at < ?".to_owned());
    }
    for _ in &terms {
        conditions.push("(INSTR(LOWER(e.content), LOWER(?)) > 0 OR INSTR(LOWER(s.project_path), LOWER(?)) > 0 OR INSTR(LOWER(COALESCE(s.title, '')), LOWER(?)) > 0)".to_owned());
    }
    if !conditions.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&conditions.join(" AND "));
    }
    sql.push_str(" ORDER BY e.occurred_at DESC, e.sequence DESC LIMIT ?");

    let mut stmt = conn.prepare(&sql)?;
    let mut values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(date) = date_from {
        values.push(Box::new(day_bound(date)));
    }
    if let Some(date) = date_to {
        values.push(Box::new(day_bound(date)));
    }
    for term in &terms {
        for _ in 0..3 {
            values.push(Box::new((*term).to_owned()));
        }
    }
    values.push(Box::new(limit as i64));
    let rows = stmt.query_map(rusqlite::params_from_iter(values.iter()), row_to_timeline_event)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn row_to_timeline_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<TimelineEvent> {
    Ok(TimelineEvent {
        session_id: row.get(0)?,
        occurred_at: row.get(1)?,
        source_tool: row.get(2)?,
        project_path: row.get(3)?,
        role: row.get(4)?,
        content: row.get(5)?,
        title: row.get(6)?,
    })
}

pub fn counts(conn: &Connection) -> Result<Counts> {
    Ok(Counts {
        sessions: count(conn, "sessions")?,
        events: count(conn, "events")?,
        journals: count(conn, "journals")?,
        analyses: count(conn, "analyses")?,
    })
}

fn count(conn: &Connection, table: &str) -> Result<i64> {
    Ok(conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
        row.get(0)
    })?)
}

pub fn schema_version(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_versions",
        [],
        |row| row.get(0),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_schema_and_is_idempotent() -> Result<()> {
        let conn = open_memory()?;
        assert_eq!(schema_version(&conn)?, SCHEMA_VERSION);
        assert_eq!(counts(&conn)?, Counts { sessions: 0, events: 0, journals: 0, analyses: 0 });

        migrate(&conn)?;
        assert_eq!(schema_version(&conn)?, SCHEMA_VERSION);
        Ok(())
    }

    #[test]
    fn enforces_event_deduplication_key() -> Result<()> {
        let conn = open_memory()?;
        conn.execute(
            "INSERT INTO sessions(source_tool, project_path) VALUES ('test', '/tmp/project')",
            [],
        )?;
        conn.execute(
            "INSERT INTO events(session_id, source_tool, source_event_id, occurred_at, role, content, sequence)
             VALUES (1, 'test', 'event-1', '2026-10-13T00:00:00Z', 'user', 'hello', 0)",
            [],
        )?;
        let duplicate = conn.execute(
            "INSERT INTO events(session_id, source_tool, source_event_id, occurred_at, role, content, sequence)
             VALUES (1, 'test', 'event-1', '2026-10-13T00:00:01Z', 'assistant', 'world', 1)",
            [],
        );
        assert!(duplicate.is_err());
        Ok(())
    }

    #[test]
    fn timeline_search_requires_all_space_separated_terms() -> Result<()> {
        let conn = open_memory()?;
        conn.execute(
            "INSERT INTO sessions(source_tool, project_path) VALUES ('test', '/projects/other')",
            [],
        )?;
        for (id, content) in [("one", "SyncLane SQLite search"), ("two", "SyncLane timeline"), ("three", "SQLite only")] {
            conn.execute(
                "INSERT INTO events(session_id, source_tool, source_event_id, occurred_at, role, content, sequence)
                 VALUES (1, 'test', ?1, '2026-10-13T00:00:00Z', 'user', ?2, 0)",
                params![id, content],
            )?;
        }

        let results = timeline(&conn, None, Some("SyncLane SQLite"), 20)?;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "SyncLane SQLite search");
        Ok(())
    }
}
