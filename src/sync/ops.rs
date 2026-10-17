//! 画面(アプリ)とコマンド(CLI)が共有する、送信まわりの操作。

use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;
use serde_json::Value;

use super::secrets::TokenStore;
use super::store::{self, Counts, Mode, ServerConnection};

#[derive(Debug, Serialize)]
pub struct Overview {
    pub connection: Option<ServerConnection>,
    pub mode: &'static str,
    pub counts: Option<Counts>,
    /// 直近の送信の結果(`engine::run` が書く)
    pub last_result: Option<Value>,
}

pub fn overview(conn: &Connection) -> Result<Overview> {
    let connection = store::load_connection(conn)?;
    let mode = store::mode(conn)?;
    let counts = match &connection {
        Some(c) => Some(store::counts(conn, &c.server, mode)?),
        None => None,
    };
    let last_result = store::get(conn, "sync.last_result")?.and_then(|v| serde_json::from_str(&v).ok());
    Ok(Overview { connection, mode: mode.as_str(), counts, last_result })
}

/// 接続を解除する。トークンをキーチェーンから消し、送った記録も消す。
pub fn disconnect(conn: &Connection, secrets: &dyn TokenStore) -> Result<()> {
    if let Some(c) = store::load_connection(conn)? {
        for scope in ["ingest", "search"] {
            secrets.delete(&c.server, scope)?;
        }
    }
    store::clear_connection(conn)
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct SessionRow {
    pub id: i64,
    pub source_tool: String,
    pub project_path: String,
    pub title: Option<String>,
    pub started_at: Option<String>,
    pub events: i64,
    /// 実際に送る対象か(モードと指定から決まる): send / excluded / not_selected
    pub state: &'static str,
    pub unsent_events: i64,
}

/// 会話の一覧(新しい順)と、それぞれを送るかどうか。
pub fn list_sessions(conn: &Connection, server: &str, mode: Mode, limit: usize, offset: usize) -> Result<Vec<SessionRow>> {
    let mut stmt = conn.prepare(
        "SELECT s.id, s.source_tool, s.project_path, s.title,
                COALESCE(s.started_at, (SELECT MIN(occurred_at) FROM events WHERE session_id = s.id)) AS started,
                (SELECT COUNT(*) FROM events WHERE session_id = s.id),
                ss.state,
                (SELECT COUNT(*) FROM events e LEFT JOIN sync_events se ON se.server_id = ?1 AND se.event_id = e.id
                  WHERE e.session_id = s.id AND (se.event_id IS NULL OR e.updated_at > se.sent_at))
         FROM sessions s LEFT JOIN sync_sessions ss ON ss.server_id = ?1 AND ss.session_id = s.id
         ORDER BY started DESC, s.id DESC LIMIT ?2 OFFSET ?3",
    )?;
    let rows = stmt.query_map(rusqlite::params![server, limit as i64, offset as i64], |row| {
        let explicit: Option<String> = row.get(6)?;
        let state = match (explicit.as_deref(), mode) {
            (Some("send"), _) | (None, Mode::Auto) => "send",
            (Some("excluded"), _) => "excluded",
            _ => "not_selected",
        };
        Ok(SessionRow {
            id: row.get(0)?,
            source_tool: row.get(1)?,
            project_path: row.get(2)?,
            title: row.get(3)?,
            started_at: row.get(4)?,
            events: row.get(5)?,
            state,
            unsent_events: row.get(7)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}
