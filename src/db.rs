use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

pub const SCHEMA_VERSION: i64 = 4;

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

// サーバーへの送信(任意)の記録。本文や題名が書き換わった時刻(updated_at)は、トリガーで入れる
// (Web の会話は、回答の途中で取り込んだ本文が、あとで完全な本文に更新されるため。送り直しの判定に使う)。
// 接続先(server_id)ごとに持つので、送信先を変えても、もう片方の記録は壊れない。
const MIGRATION_4: &str = r#"
ALTER TABLE events ADD COLUMN updated_at TEXT;
ALTER TABLE sessions ADD COLUMN updated_at TEXT;

CREATE TRIGGER IF NOT EXISTS events_touch AFTER UPDATE OF content ON events
BEGIN
    UPDATE events SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = NEW.id;
END;

CREATE TRIGGER IF NOT EXISTS sessions_touch AFTER UPDATE OF title ON sessions
BEGIN
    UPDATE sessions SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = NEW.id;
END;

-- 会話ごとの送信の指定。行が無ければ、送信のモード(manual / auto)の既定に従う
CREATE TABLE IF NOT EXISTS sync_sessions (
    server_id  TEXT    NOT NULL,
    session_id INTEGER NOT NULL REFERENCES sessions(id),
    state      TEXT    NOT NULL CHECK (state IN ('send', 'excluded')),
    visibility TEXT,
    sent_at    TEXT,
    PRIMARY KEY (server_id, session_id)
);

-- 発言ごとの送信済みの記録
CREATE TABLE IF NOT EXISTS sync_events (
    server_id TEXT    NOT NULL,
    event_id  INTEGER NOT NULL REFERENCES events(id),
    sent_at   TEXT    NOT NULL,
    PRIMARY KEY (server_id, event_id)
);

-- 送信の途中状態。通信が切れたら、同じ batch_id と同じ本文で送り直す(サーバー側が冪等に受ける)
CREATE TABLE IF NOT EXISTS sync_outbox (
    server_id  TEXT NOT NULL,
    batch_id   TEXT NOT NULL,
    body       TEXT NOT NULL,
    event_ids  TEXT NOT NULL,
    session_ids TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (server_id, batch_id)
);
"#;

pub fn default_db_path() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("SYNCLANE_DB") {
        return Ok(PathBuf::from(path));
    }
    let dir = crate::paths::app_data_dir().context("the home folder is not set (HOME, or USERPROFILE on Windows)")?;
    Ok(dir.join("synclane.db"))
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

/// 読み取り専用で開く(MCP など、アプリとは別のプロセスから読む用)。
/// マイグレーションはしない。アプリが書き込み中でも待てるよう、待ち時間を設ける。
pub fn open_readonly(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("failed to open database {} (start the SyncLane app once to create it)", path.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    let version = schema_version(&conn).unwrap_or(0);
    if version < SCHEMA_VERSION {
        bail!("database schema v{version} is older than this SyncLane (v{SCHEMA_VERSION}); start the SyncLane app once to update it");
    }
    Ok(conn)
}

/// 1つのセッションの会話を、時刻順に返す。
/// `around`(イベントID)があれば、その前後 `window` 件ずつ。無ければ先頭から `window * 2 + 1` 件。
/// ログの並び順の番号(sequence)は増分の収集で重なるため、時刻と ID で並べる。
pub fn session_events(conn: &Connection, session_id: i64, around: Option<i64>, window: usize) -> Result<Vec<TimelineEvent>> {
    let mut stmt = conn.prepare(
        "SELECT e.session_id, e.occurred_at, e.source_tool, s.project_path, e.role, e.content, s.title, e.id, e.source_event_id
         FROM events e JOIN sessions s ON s.id = e.session_id
         WHERE e.session_id = ?1 ORDER BY e.occurred_at ASC, e.id ASC",
    )?;
    let all = stmt
        .query_map([session_id], row_to_timeline_event)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let (start, end) = match around.and_then(|id| all.iter().position(|e| e.event_id == id)) {
        Some(index) => (index.saturating_sub(window), (index + window + 1).min(all.len())),
        None => (0, (window * 2 + 1).min(all.len())),
    };
    let mut shown = all[start..end].to_vec();
    crate::sections::attach(conn, &mut shown)?;
    Ok(shown)
}

/// メモリ上の DB(テスト用。src-tauri のテストからも使う)。
pub fn open_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    configure(&conn)?;
    Ok(conn)
}

fn configure(conn: &Connection) -> Result<()> {
    conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")?;
    migrate(conn)?;
    ensure_search_index(conn)
}

/// 発言の全文検索の索引(FTS5、3 文字単位の `trigram`)。発言の本文は、`events` に置いたまま(外部の内容)で、索引だけを持つ。
/// 日本語も、部分一致で探せる。3 文字未満の語は、索引では探せないので、`LIKE` ではなく `INSTR` で探す。
///
/// **スキーマの版は変えない**(版を上げると、古いアプリで開けなくなる)。無ければ作って、いまある発言を取り込む。
/// 索引は、トリガで `events` に追従する。古いアプリが発言を足しても、トリガが効くので、ずれない。
const SEARCH_INDEX: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS events_fts USING fts5(content, content='events', content_rowid='id', tokenize='trigram');
CREATE TRIGGER IF NOT EXISTS events_fts_ai AFTER INSERT ON events BEGIN
    INSERT INTO events_fts(rowid, content) VALUES (new.id, new.content);
END;
CREATE TRIGGER IF NOT EXISTS events_fts_ad AFTER DELETE ON events BEGIN
    INSERT INTO events_fts(events_fts, rowid, content) VALUES ('delete', old.id, old.content);
END;
CREATE TRIGGER IF NOT EXISTS events_fts_au AFTER UPDATE OF content ON events BEGIN
    INSERT INTO events_fts(events_fts, rowid, content) VALUES ('delete', old.id, old.content);
    INSERT INTO events_fts(rowid, content) VALUES (new.id, new.content);
END;
"#;

/// 全文検索の索引が、このデータベースにあるか。
pub fn has_search_index(conn: &Connection) -> bool {
    conn.query_row("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'events_fts'", [], |_| Ok(())).is_ok()
}

/// 索引が無ければ作って、いまある発言を取り込む(1 度だけ。あれば、何もしない)。
fn ensure_search_index(conn: &Connection) -> Result<()> {
    if has_search_index(conn) {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(SEARCH_INDEX)?;
    tx.execute("INSERT INTO events_fts(events_fts) VALUES ('rebuild')", [])?;
    tx.commit()?;
    Ok(())
}

/// 索引を、`events` から作り直す(ずれた疑いがあるとき用)。
pub fn rebuild_search_index(conn: &Connection) -> Result<()> {
    ensure_search_index(conn)?;
    conn.execute("INSERT INTO events_fts(events_fts) VALUES ('rebuild')", [])?;
    Ok(())
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
        current = 3;
    }
    if current < 4 {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(MIGRATION_4)?;
        tx.execute(
            "INSERT INTO schema_versions(version) VALUES (?1)",
            params![4],
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

#[derive(Debug, PartialEq, Eq)]
pub struct RoleCounts {
    pub total: i64,
    pub user: i64,
    pub assistant: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TimelineEvent {
    pub session_id: i64,
    pub occurred_at: String,
    pub source_tool: String,
    pub project_path: String,
    pub title: Option<String>,
    pub event_id: i64,
    pub role: String,
    pub content: String,
    /// この発言が属する区切り(話題のまとまり)の、先頭の発言の ID。`title` は、区切りの題名があれば、それに置き換わっている
    pub section_start: Option<i64>,
    /// 元のツールでの発言の ID(サーバーの結果と突き合わせて、重複を省くための鍵。`source_tool` と組)
    pub source_event_id: String,
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
    search_events(conn, date_from, date_to, query, limit, has_search_index(conn))
}

/// 語が、索引で探せるか(3 文字以上)。短い語は、本文を直接探す。
fn indexable(term: &str) -> bool {
    term.chars().count() >= 3
}

/// 索引に渡す問い合わせ(語を、そのまま 1 つの句として扱う。引用符は、重ねて逃がす)。
fn fts_phrase(term: &str) -> String {
    format!("\"{}\"", term.replace('"', "\"\""))
}

/// 検索の本体。`indexed` が true なら、3 文字以上の語を、全文検索の索引で探す。false なら、すべて本文を直接探す(索引が無いとき・比較の試験用)。
fn search_events(
    conn: &Connection,
    date_from: Option<&str>,
    date_to: Option<&str>,
    query: Option<&str>,
    limit: usize,
    indexed: bool,
) -> Result<Vec<TimelineEvent>> {
    let mut sql = String::from(
        "SELECT e.session_id, e.occurred_at, e.source_tool, s.project_path, e.role, e.content, s.title, e.id, e.source_event_id
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
    // 索引で探せる語を先に評価する(AND は、前から評価されるので、重い本文の直接検索は、絞られた結果にだけ掛かる)
    let mut ordered: Vec<&str> = terms.clone();
    ordered.sort_by_key(|term| !(indexed && indexable(term)));
    for term in &ordered {
        let body = if indexed && indexable(term) {
            "e.id IN (SELECT rowid FROM events_fts WHERE events_fts MATCH ?)"
        } else {
            "INSTR(LOWER(e.content), LOWER(?)) > 0"
        };
        conditions.push(format!("({body} OR INSTR(LOWER(s.project_path), LOWER(?)) > 0 OR INSTR(LOWER(COALESCE(s.title, '')), LOWER(?)) > 0)"));
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
    for term in &ordered {
        values.push(Box::new(if indexed && indexable(term) { fts_phrase(term) } else { (*term).to_owned() }));
        values.push(Box::new((*term).to_owned()));
        values.push(Box::new((*term).to_owned()));
    }
    values.push(Box::new(limit as i64));
    let rows = stmt.query_map(rusqlite::params_from_iter(values.iter()), row_to_timeline_event)?;
    let mut events = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    crate::sections::attach(conn, &mut events)?;
    Ok(events)
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
        event_id: row.get(7)?,
        section_start: None,
        source_event_id: row.get(8)?,
    })
}

/// イベントの件数を、全体・ユーザー・AIに分けて返す。
pub fn role_counts(conn: &Connection) -> Result<RoleCounts> {
    let count_of = |role: &str| -> Result<i64> {
        Ok(conn.query_row("SELECT COUNT(*) FROM events WHERE LOWER(role) = ?1", [role], |row| row.get(0))?)
    };
    Ok(RoleCounts { total: count(conn, "events")?, user: count_of("user")?, assistant: count_of("assistant")? })
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

    // ---- 全文検索の索引 ----

    fn corpus(conn: &Connection) {
        conn.execute("INSERT INTO sessions(source_tool, project_path, source_file, source_hash, title) VALUES ('claude-code', '/work/SyncLane', 'a', 'a', '京都旅行の計画')", []).unwrap();
        conn.execute("INSERT INTO sessions(source_tool, project_path, source_file, source_hash, title) VALUES ('codex', '/work/other', 'b', 'b', NULL)", []).unwrap();
        let rows = [
            (1, "2026-10-01T01:00:00.000Z", "user", "京都で3日間の旅行を計画したい。のんびり回れる行程を知りたい"),
            (1, "2026-10-01T01:01:00.000Z", "assistant", "1日目は伏見稲荷、2日目は嵐山、3日目は清水寺がおすすめです。"),
            (1, "2026-10-02T02:00:00.000Z", "user", "SyncLane の SQLite を Rust で検索したい。FTS5 は使える?"),
            (1, "2026-10-02T02:01:00.000Z", "assistant", "FTS5 の trigram なら、日本語も部分一致で探せます。syncLANE でも sqlite でも同じです。"),
            (2, "2026-10-03T03:00:00.000Z", "user", "She said \"hello world\" and left. NEAR AND OR NOT * - (x)"),
            (2, "2026-10-03T03:01:00.000Z", "assistant", "AB の二文字と、C#、a+b、100%、under_score、back\\slash"),
            (2, "2026-10-04T04:00:00.000Z", "user", "ＦＵＬＬ ＷＩＤＴＨ と ｈａｌｆ カナ ﾊﾝｶｸ"),
            (2, "2026-10-04T04:01:00.000Z", "assistant", "tab\tseparated and 改行\nも含む"),
        ];
        for (i, (session, at, role, content)) in rows.iter().enumerate() {
            conn.execute(
                "INSERT INTO events(session_id, source_tool, source_event_id, occurred_at, role, content, sequence) VALUES (?1, 'x', ?2, ?3, ?4, ?5, ?6)",
                params![session, format!("e{i}"), at, role, content, i as i64],
            ).unwrap();
        }
    }

    fn ids(events: Vec<TimelineEvent>) -> Vec<i64> {
        events.into_iter().map(|e| e.event_id).collect()
    }

    #[test]
    fn an_indexed_search_returns_the_same_events_as_the_direct_scan() -> Result<()> {
        let conn = open_memory()?;
        corpus(&conn);
        assert!(has_search_index(&conn));
        let queries = [
            "京都", "京都旅行", "伏見稲荷", "日本語も部分一致", "sqlite", "SQLITE", "SyncLane", "synclane", "fts5", "FTS5 trigram",
            "SyncLane sqlite", "trigram 日本語", "京都 清水寺", "AB", "C#", "a+b", "100%", "under_score", "back\\slash",
            "\"hello", "world\"", "NEAR", "AND", "NOT", "*", "-", "(x)", "hello world", "She said",
            "ＦＵＬＬ", "ｈａｌｆ", "ﾊﾝｶｸ", "separated", "改行", "存在しない語", "京都 存在しない語", "work", "/work/SyncLane", "旅行の計画", "",
        ];
        for query in queries {
            let with = ids(search_events(&conn, None, None, Some(query), 50, true)?);
            let without = ids(search_events(&conn, None, None, Some(query), 50, false)?);
            assert_eq!(with, without, "検索語 {query:?} で、索引ありと総当たりの結果が違う");
        }
        // 日付で絞っても、同じ
        let with = ids(search_events(&conn, Some("2026-10-02"), Some("2026-10-04"), Some("sqlite"), 50, true)?);
        let without = ids(search_events(&conn, Some("2026-10-02"), Some("2026-10-04"), Some("sqlite"), 50, false)?);
        assert_eq!(with, without);
        // 実際に、見つかっている(空の一致を、一致と取り違えていない)
        assert_eq!(ids(search_events(&conn, None, None, Some("京都旅行"), 50, true)?).len(), 4); // 題名にも当たる会話の全発言
        assert_eq!(ids(search_events(&conn, None, None, Some("伏見稲荷"), 50, true)?).len(), 1);
        assert!(search_events(&conn, None, None, Some("存在しない語"), 50, true)?.is_empty());
        Ok(())
    }

    #[test]
    fn the_index_follows_inserts_updates_and_deletes() -> Result<()> {
        let conn = open_memory()?;
        corpus(&conn);
        let found = |q: &str| -> Result<usize> { Ok(search_events(&conn, None, None, Some(q), 50, true)?.len()) };
        conn.execute("INSERT INTO events(session_id, source_tool, source_event_id, occurred_at, role, content, sequence) VALUES (2, 'x', 'new', '2026-10-05T00:00:00.000Z', 'user', 'ユニークな新しい発言ABCDEF', 99)", [])?;
        assert_eq!(found("ABCDEF")?, 1);
        conn.execute("UPDATE events SET content = '書き換えた本文XYZXYZ' WHERE source_event_id = 'new'", [])?;
        assert_eq!(found("ABCDEF")?, 0);
        assert_eq!(found("XYZXYZ")?, 1);
        conn.execute("DELETE FROM events WHERE source_event_id = 'new'", [])?;
        assert_eq!(found("XYZXYZ")?, 0);
        // 索引は、作り直しても、同じ結果
        rebuild_search_index(&conn)?;
        assert_eq!(found("伏見稲荷")?, 1);
        Ok(())
    }

    fn file_db(name: &str) -> PathBuf {
        // /tmp は使わない。プロジェクトの target/ の中に作って、終わったら消す
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join(format!("test-fts-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.join("synclane.db")
    }

    fn drop_index(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch("DROP TRIGGER events_fts_ai; DROP TRIGGER events_fts_ad; DROP TRIGGER events_fts_au; DROP TABLE events_fts;").unwrap();
    }

    #[test]
    fn a_database_made_before_the_index_gets_it_when_opened_and_keeps_its_schema_version() -> Result<()> {
        let path = file_db("upgrade");
        {
            let conn = open(&path)?;
            corpus(&conn);
        }
        drop_index(&path); // 索引ができる前のデータベースを、真似る
        {
            let raw = Connection::open(&path)?;
            assert!(!has_search_index(&raw));
        }
        let conn = open(&path)?;
        assert!(has_search_index(&conn));
        assert_eq!(schema_version(&conn)?, SCHEMA_VERSION); // 版は、上がらない
        assert_eq!(ids(search_events(&conn, None, None, Some("伏見稲荷"), 50, true)?).len(), 1); // 既にあった発言も、探せる
        drop(conn);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
        Ok(())
    }

    #[test]
    fn a_read_only_connection_falls_back_to_the_direct_scan_when_there_is_no_index() -> Result<()> {
        let path = file_db("readonly");
        {
            let conn = open(&path)?;
            corpus(&conn);
        }
        drop_index(&path);
        let conn = open_readonly(&path)?;
        assert!(!has_search_index(&conn));
        assert_eq!(timeline(&conn, None, Some("伏見稲荷"), 10)?.len(), 1); // 索引が無くても、探せる
        drop(conn);
        // 索引があれば、読み取り専用でも、それを使う
        {
            let conn = open(&path)?;
            assert!(has_search_index(&conn));
        }
        let conn = open_readonly(&path)?;
        assert_eq!(timeline(&conn, None, Some("伏見稲荷"), 10)?.len(), 1);
        drop(conn);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
        Ok(())
    }

    /// 速さの比較(`cargo test --lib bench -- --ignored --nocapture`)。実行のたびに、時間を表示するだけで、合否は決めない。
    #[test]
    #[ignore]
    fn bench_indexed_search_against_the_direct_scan() -> Result<()> {
        let path = file_db("bench");
        let conn = open(&path)?;
        for session in 0..40 {
            conn.execute("INSERT INTO sessions(source_tool, project_path, source_file, source_hash) VALUES ('claude-code', '/w', ?1, ?1)", [format!("s{session}")])?;
        }
        let words = ["京都", "旅行", "家計簿", "検索", "設計", "実装", "確認", "公開", "リリース", "署名", "公証", "題名", "区切り", "会話", "取り込み"];
        let tx = conn.unchecked_transaction()?;
        for i in 0..20_000 {
            let mut body = String::new();
            for k in 0..120 {
                body.push_str(words[(i * 7 + k * 3) % words.len()]);
                body.push_str(if k % 9 == 0 { "。\n" } else { "、" });
            }
            body.push_str(&format!("固有の語{i:05}"));
            tx.execute(
                "INSERT INTO events(session_id, source_tool, source_event_id, occurred_at, role, content, sequence) VALUES (?5, 'x', ?1, ?2, 'user', ?3, ?4)",
                params![format!("b{i}"), format!("2026-10-{:02}T{:02}:{:02}:00.000Z", 1 + (i / 1440) % 28, (i / 60) % 24, i % 60), body, i as i64, 1 + (i % 40) as i64],
            )?;
        }
        tx.commit()?;
        let size: i64 = conn.query_row("SELECT SUM(LENGTH(content)) FROM events", [], |r| r.get(0))?;
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        let with_index = fs::metadata(&path)?.len();
        println!("発言 20,000 件(会話 40 個)、本文の合計 {} MB", size / 1_000_000);
        for query in ["固有の語12345", "リリース 取り込み", "リリース 題名 区切り", "公証 署名", "検索"] {
            let t = std::time::Instant::now();
            let a = search_events(&conn, None, None, Some(query), 30, false)?.len();
            let scan = t.elapsed();
            let t = std::time::Instant::now();
            let b = search_events(&conn, None, None, Some(query), 30, true)?.len();
            let indexed = t.elapsed();
            println!("{query:<14} 総当たり {scan:>10.1?} / 索引 {indexed:>10.1?} ({a} 件・{b} 件)");
            assert_eq!(a, b);
        }
        // 索引を外して、大きさを比べる
        conn.execute_batch("DROP TRIGGER events_fts_ai; DROP TRIGGER events_fts_ad; DROP TRIGGER events_fts_au; DROP TABLE events_fts; VACUUM;")?;
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        let without_index = fs::metadata(&path)?.len();
        println!("データベースの大きさ: 索引なし {:.1} MB / 索引あり {:.1} MB (索引で +{:.1} MB)", without_index as f64 / 1e6, with_index as f64 / 1e6, (with_index as f64 - without_index as f64) / 1e6);
        drop(conn);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
        Ok(())
    }
}
