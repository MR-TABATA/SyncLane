//! 接続の設定と、何を送ったかの記録(SQLite)。

use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::now_iso;

pub fn get(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| row.get(0)).optional()?)
}

pub fn set(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO settings(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

pub fn remove(conn: &Connection, key: &str) -> Result<()> {
    conn.execute("DELETE FROM settings WHERE key = ?1", [key])?;
    Ok(())
}

/// 送信のモード。manual は、本人が選んだ会話だけ。auto は、除外した会話以外の全部。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Manual,
    Auto,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Manual => "manual",
            Mode::Auto => "auto",
        }
    }

    pub fn parse(value: &str) -> Result<Mode> {
        match value {
            "manual" => Ok(Mode::Manual),
            "auto" => Ok(Mode::Auto),
            other => bail!("モードは manual か auto です(指定: {other})"),
        }
    }
}

/// 接続中のサーバー。トークンはここには置かない(キーチェーン)。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerConnection {
    pub server: String,
    pub device_id: String,
    pub user_name: String,
    pub user_email: String,
}

const K_SERVER: &str = "sync.server";
const K_DEVICE: &str = "sync.device_id";
const K_USER: &str = "sync.user";
const K_MODE: &str = "sync.mode";

pub fn load_connection(conn: &Connection) -> Result<Option<ServerConnection>> {
    let Some(server) = get(conn, K_SERVER)? else { return Ok(None) };
    let user: serde_json::Value = get(conn, K_USER)?.and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default();
    Ok(Some(ServerConnection {
        server,
        device_id: get(conn, K_DEVICE)?.unwrap_or_default(),
        user_name: user["name"].as_str().unwrap_or("").to_string(),
        user_email: user["email"].as_str().unwrap_or("").to_string(),
    }))
}

pub fn save_connection(conn: &Connection, c: &ServerConnection) -> Result<()> {
    set(conn, K_SERVER, &c.server)?;
    set(conn, K_DEVICE, &c.device_id)?;
    set(conn, K_USER, &serde_json::json!({"name": c.user_name, "email": c.user_email}).to_string())?;
    Ok(())
}

pub fn mode(conn: &Connection) -> Result<Mode> {
    match get(conn, K_MODE)? {
        Some(value) => Mode::parse(&value),
        None => Ok(Mode::Manual), // 既定は、本人が選んだものだけ
    }
}

pub fn set_mode(conn: &Connection, mode: Mode) -> Result<()> {
    set(conn, K_MODE, mode.as_str())
}

/// トークンの発行と期限(更新の時期の判断に使う)。トークン本体は入れない。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenMeta {
    pub issued_at: String,
    pub expires_at: String,
}

fn token_key(scope: &str) -> String {
    format!("sync.token.{scope}")
}

pub fn token_meta(conn: &Connection, scope: &str) -> Result<Option<TokenMeta>> {
    Ok(get(conn, &token_key(scope))?.and_then(|v| serde_json::from_str(&v).ok()))
}

pub fn set_token_meta(conn: &Connection, scope: &str, meta: &TokenMeta) -> Result<()> {
    set(conn, &token_key(scope), &serde_json::to_string(meta)?)
}

/// 接続を解除する。そのサーバーに送った記録も消す(つなぎ直すと、全部を送り直す。サーバー側は重複を省く)。
pub fn clear_connection(conn: &Connection) -> Result<()> {
    if let Some(server) = get(conn, K_SERVER)? {
        for table in ["sync_sessions", "sync_events", "sync_outbox"] {
            conn.execute(&format!("DELETE FROM {table} WHERE server_id = ?1"), [&server])?;
        }
    }
    for key in [K_SERVER, K_DEVICE, K_USER, K_MODE, K_RULES, "sync.last_result"] {
        remove(conn, key)?;
    }
    for scope in ["ingest", "search"] {
        remove(conn, &token_key(scope))?;
    }
    Ok(())
}

// ---- 除外のルール ----

/// 自動のモードで、送らない会話を決めるルール。会話ごとの指定(`sync_sessions.state`)は、これより優先する(例外の指定)。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rules {
    /// 送らないプロジェクト(`project_path` の完全一致)
    #[serde(default)]
    pub exclude_projects: Vec<String>,
    /// 送らないツール(`source_tool` の完全一致。例: gemini_web)
    #[serde(default)]
    pub exclude_tools: Vec<String>,
    /// この日(YYYY-MM-DD)より前に始まった会話は送らない
    #[serde(default)]
    pub since: Option<String>,
}

const K_RULES: &str = "sync.rules";

pub fn rules(conn: &Connection) -> Result<Rules> {
    Ok(get(conn, K_RULES)?.and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default())
}

pub fn set_rules(conn: &Connection, rules: &Rules) -> Result<()> {
    if let Some(since) = rules.since.as_deref() {
        let ok = since.len() == 10
            && since.chars().enumerate().all(|(i, c)| if i == 4 || i == 7 { c == '-' } else { c.is_ascii_digit() });
        if !ok {
            bail!("since は YYYY-MM-DD の形式で指定してください(指定: {since})");
        }
    }
    set(conn, K_RULES, &serde_json::to_string(rules)?)
}

impl Rules {
    /// ルールに照らして、この会話を送ってよいか(SQL の `rules_sql` と同じ判定)。
    pub fn allows(&self, project_path: &str, source_tool: &str, started_at: Option<&str>) -> bool {
        if self.exclude_projects.iter().any(|p| p == project_path) || self.exclude_tools.iter().any(|t| t == source_tool) {
            return false;
        }
        match (self.since.as_deref(), started_at) {
            (Some(since), Some(started)) => started >= since,
            _ => true,
        }
    }

    /// 会話の表 `s`(sessions)の列に対する、「ルールで送ってよい」条件の SQL。値は文字列として埋め込む(? の番号を増やさないため)。
    pub fn sql(&self) -> String {
        fn literal(value: &str) -> String {
            format!("'{}'", value.replace('\'', "''"))
        }
        let mut parts = vec!["1".to_string()];
        if !self.exclude_projects.is_empty() {
            let list: Vec<_> = self.exclude_projects.iter().map(|v| literal(v)).collect();
            parts.push(format!("s.project_path NOT IN ({})", list.join(",")));
        }
        if !self.exclude_tools.is_empty() {
            let list: Vec<_> = self.exclude_tools.iter().map(|v| literal(v)).collect();
            parts.push(format!("s.source_tool NOT IN ({})", list.join(",")));
        }
        if let Some(since) = &self.since {
            parts.push(format!("(s.started_at IS NULL OR s.started_at >= {})", literal(since)));
        }
        format!("({})", parts.join(" AND "))
    }
}

// ---- 会話ごとの指定 ----

/// 会話の送信の指定を変える。`state` が None なら、指定を外して既定(モード)に戻す。
pub fn set_session_state(conn: &Connection, server: &str, session_id: i64, state: Option<&str>, visibility: Option<&str>) -> Result<()> {
    match state {
        Some(state @ ("send" | "excluded")) => {
            conn.execute(
                "INSERT INTO sync_sessions(server_id, session_id, state, visibility) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(server_id, session_id) DO UPDATE SET state = excluded.state,
                   visibility = COALESCE(excluded.visibility, sync_sessions.visibility)",
                params![server, session_id, state, visibility],
            )?;
        }
        None => {
            conn.execute("DELETE FROM sync_sessions WHERE server_id = ?1 AND session_id = ?2", params![server, session_id])?;
        }
        Some(other) => bail!("state は send か excluded です(指定: {other})"),
    }
    Ok(())
}

/// すべての会話を「送る」にする(manual のときの一括)。
pub fn include_all(conn: &Connection, server: &str, visibility: Option<&str>) -> Result<usize> {
    Ok(conn.execute(
        "INSERT INTO sync_sessions(server_id, session_id, state, visibility)
         SELECT ?1, id, 'send', ?2 FROM sessions WHERE true
         ON CONFLICT(server_id, session_id) DO UPDATE SET state = 'send',
           visibility = COALESCE(excluded.visibility, sync_sessions.visibility)
         WHERE sync_sessions.state <> 'excluded'",
        params![server, visibility],
    )?)
}

/// 送る対象の会話の条件(SQL)。`ss` は sync_sessions、`s` は sessions の別名。?1 が server_id、?2 が auto かどうか(1/0)。
/// 会話ごとの指定(send)が先。指定がなく、auto のときは、除外のルールに合わない会話を送る。
pub fn eligible(rules: &Rules) -> String {
    format!("(ss.state = 'send' OR (ss.state IS NULL AND ?2 = 1 AND {}))", rules.sql())
}

pub struct PendingEvent {
    pub id: i64,
    pub session_id: i64,
    pub source_tool: String,
    pub source_event_id: String,
    pub occurred_at: String,
    pub role: String,
    pub content: String,
}

/// まだ送っていない(または送ったあとで本文が変わった)発言。古い ID から。
pub fn pending_events(conn: &Connection, server: &str, mode: Mode, limit: usize) -> Result<Vec<PendingEvent>> {
    let eligible = eligible(&rules(conn)?);
    let sql = format!(
        "SELECT e.id, e.session_id, e.source_tool, e.source_event_id, e.occurred_at, e.role, e.content
         FROM events e
         JOIN sessions s ON s.id = e.session_id
         LEFT JOIN sync_sessions ss ON ss.server_id = ?1 AND ss.session_id = e.session_id
         LEFT JOIN sync_events se ON se.server_id = ?1 AND se.event_id = e.id
         WHERE {eligible} AND (se.event_id IS NULL OR e.updated_at > se.sent_at)
         ORDER BY e.id LIMIT ?3"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![server, (mode == Mode::Auto) as i64, limit as i64], |row| {
        Ok(PendingEvent {
            id: row.get(0)?,
            session_id: row.get(1)?,
            source_tool: row.get(2)?,
            source_event_id: row.get(3)?,
            occurred_at: row.get(4)?,
            role: row.get(5)?,
            content: row.get(6)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// 送ったあとで、題名などが変わった会話(発言は送らず、会話の情報だけを送り直す)。
pub fn sessions_needing_meta(conn: &Connection, server: &str, mode: Mode) -> Result<Vec<i64>> {
    let eligible = eligible(&rules(conn)?);
    let sql = format!(
        "SELECT s.id FROM sessions s
         JOIN sync_sessions ss ON ss.server_id = ?1 AND ss.session_id = s.id
         WHERE {eligible} AND ss.sent_at IS NOT NULL AND s.updated_at > ss.sent_at
         ORDER BY s.id LIMIT 200"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![server, (mode == Mode::Auto) as i64], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub sessions_total: i64,
    pub sessions_selected: i64,
    pub sessions_excluded: i64,
    pub events_pending: i64,
    pub events_sent: i64,
    pub last_sent_at: Option<String>,
}

pub fn counts(conn: &Connection, server: &str, mode: Mode) -> Result<Counts> {
    let auto = (mode == Mode::Auto) as i64;
    let eligible = eligible(&rules(conn)?);
    let sessions_total: i64 = conn.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))?;
    let excluded: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sync_sessions WHERE server_id = ?1 AND state = 'excluded'", [server], |r| r.get(0))?;
    let selected: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM sessions s LEFT JOIN sync_sessions ss ON ss.server_id = ?1 AND ss.session_id = s.id WHERE {eligible}"),
        params![server, auto], |r| r.get(0))?;
    let pending: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM events e
             JOIN sessions s ON s.id = e.session_id
             LEFT JOIN sync_sessions ss ON ss.server_id = ?1 AND ss.session_id = e.session_id
             LEFT JOIN sync_events se ON se.server_id = ?1 AND se.event_id = e.id
             WHERE {eligible} AND (se.event_id IS NULL OR e.updated_at > se.sent_at)"),
        params![server, auto], |r| r.get(0))?;
    let (sent, last): (i64, Option<String>) = conn.query_row(
        "SELECT COUNT(*), MAX(sent_at) FROM sync_events WHERE server_id = ?1", [server], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(Counts {
        sessions_total,
        sessions_selected: selected,
        sessions_excluded: excluded,
        events_pending: pending,
        events_sent: sent,
        last_sent_at: last,
    })
}

/// 送った印を付ける。`built_at` は、バッチを組んだ時刻(送っている間に本文が更新されたら、次回に送り直せるように)。
pub fn mark_sent(conn: &Connection, server: &str, batch_id: &str, event_ids: &[i64], session_ids: &[i64], built_at: &str) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    for id in event_ids {
        tx.execute(
            "INSERT INTO sync_events(server_id, event_id, sent_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(server_id, event_id) DO UPDATE SET sent_at = excluded.sent_at",
            params![server, id, built_at],
        )?;
    }
    for id in session_ids {
        tx.execute(
            "INSERT INTO sync_sessions(server_id, session_id, state, sent_at) VALUES (?1, ?2, 'send', ?3)
             ON CONFLICT(server_id, session_id) DO UPDATE SET sent_at = excluded.sent_at",
            params![server, id, built_at],
        )?;
    }
    tx.execute("DELETE FROM sync_outbox WHERE server_id = ?1 AND batch_id = ?2", params![server, batch_id])?;
    tx.commit()?;
    Ok(())
}

pub fn record_result(conn: &Connection, summary: &serde_json::Value) -> Result<()> {
    set(conn, "sync.last_result", &serde_json::json!({"at": now_iso(), "result": summary}).to_string())
}
