use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;

/// ブラウザ拡張から届く、Web 版の Gemini の会話。
pub const GEMINI_WEB_TOOL: &str = "gemini_web";
const SOURCES: &[&str] = &[GEMINI_WEB_TOOL];

const MAX_MESSAGES: usize = 5000;
const MAX_TEXT_BYTES: usize = 1_000_000;

/// 拡張は、会話の全体を毎回送る。同じ内容なら何も起きず(冪等)、回答が伸びていれば内容を更新する。
#[derive(Debug, Deserialize)]
pub struct Payload {
    pub source: String,
    pub conversation_id: String,
    pub title: Option<String>,
    /// 拡張が会話を見た時刻(ISO 8601)。画面には発言の時刻が無いので、初めて取り込んだときの時刻を発言の時刻にする。
    pub captured_at: String,
    pub messages: Vec<Message>,
}

#[derive(Debug, Deserialize)]
pub struct Message {
    pub index: i64,
    pub role: String,
    pub text: String,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct IngestReport {
    pub inserted: usize,
    pub updated: usize,
}

fn validate(payload: &Payload) -> Result<()> {
    if !SOURCES.contains(&payload.source.as_str()) {
        bail!("unknown source: {}", payload.source);
    }
    let id = &payload.conversation_id;
    if id.is_empty() || id.len() > 200 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        bail!("invalid conversation_id");
    }
    let at = &payload.captured_at;
    if at.len() < 10 || at.len() > 40 || !at.chars().all(|c| c.is_ascii_digit() || "T:-.Z+".contains(c)) {
        bail!("invalid captured_at");
    }
    if payload.messages.len() > MAX_MESSAGES {
        bail!("too many messages");
    }
    for message in &payload.messages {
        if message.role != "user" && message.role != "assistant" {
            bail!("invalid role: {}", message.role);
        }
        if message.text.len() > MAX_TEXT_BYTES {
            bail!("message too large");
        }
    }
    Ok(())
}

pub fn ingest(conn: &mut Connection, payload: &Payload) -> Result<IngestReport> {
    validate(payload)?;
    let tool = payload.source.as_str();
    let source_file = format!("gemini.google.com/{}", payload.conversation_id);

    let tx = conn.transaction()?;
    let session_id = match tx
        .query_row(
            "SELECT id FROM sessions WHERE source_tool = ?1 AND source_file = ?2",
            params![tool, source_file],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
    {
        Some(id) => id,
        None => {
            tx.execute(
                "INSERT INTO sessions(source_tool, project_path, source_file, source_hash, started_at)
                 VALUES (?1, 'gemini.google.com', ?2, ?3, ?4)",
                params![tool, source_file, payload.conversation_id, payload.captured_at],
            )?;
            tx.last_insert_rowid()
        }
    };

    let mut report = IngestReport::default();
    for message in &payload.messages {
        let text = message.text.trim();
        if text.is_empty() {
            continue;
        }
        let event_id = format!("{}:{}:{}", payload.conversation_id, message.index, message.role);
        let existing: Option<String> = tx
            .query_row(
                "SELECT content FROM events WHERE source_tool = ?1 AND source_event_id = ?2",
                params![tool, event_id],
                |row| row.get(0),
            )
            .optional()?;
        match existing {
            None => {
                tx.execute(
                    "INSERT INTO events (session_id, source_tool, source_event_id, occurred_at, role, content, sequence)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![session_id, tool, event_id, payload.captured_at, message.role, text, message.index],
                )?;
                report.inserted += 1;
            }
            Some(old) if old != text => {
                tx.execute(
                    "UPDATE events SET content = ?3 WHERE source_tool = ?1 AND source_event_id = ?2",
                    params![tool, event_id, text],
                )?;
                report.updated += 1;
            }
            Some(_) => {}
        }
    }
    if let Some(title) = payload.title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        tx.execute(
            "UPDATE sessions SET title = ?2 WHERE id = ?1 AND COALESCE(title, '') <> ?2",
            params![session_id, title],
        )?;
    }
    tx.commit()?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn payload(messages: &[(i64, &str, &str)]) -> Payload {
        Payload {
            source: GEMINI_WEB_TOOL.into(),
            conversation_id: "abc123".into(),
            title: Some("フリーWi-Fi".into()),
            captured_at: "2026-10-16T01:00:00.000Z".into(),
            messages: messages
                .iter()
                .map(|(index, role, text)| Message { index: *index, role: (*role).into(), text: (*text).into() })
                .collect(),
        }
    }

    #[test]
    fn inserts_once_and_is_idempotent() -> Result<()> {
        let mut conn = db::open_memory()?;
        let p = payload(&[(0, "user", "質問"), (1, "assistant", "回答")]);
        assert_eq!(ingest(&mut conn, &p)?, IngestReport { inserted: 2, updated: 0 });
        assert_eq!(ingest(&mut conn, &p)?, IngestReport::default());
        let counts = db::counts(&conn)?;
        assert_eq!((counts.sessions, counts.events), (1, 2));
        Ok(())
    }

    #[test]
    fn updates_a_growing_answer_and_adds_new_turns() -> Result<()> {
        let mut conn = db::open_memory()?;
        ingest(&mut conn, &payload(&[(0, "user", "質問"), (1, "assistant", "回")]))?;
        let report = ingest(&mut conn, &payload(&[(0, "user", "質問"), (1, "assistant", "回答です"), (2, "user", "続き")]))?;
        assert_eq!(report, IngestReport { inserted: 1, updated: 1 });
        let content: String = conn.query_row("SELECT content FROM events WHERE sequence = 1", [], |r| r.get(0))?;
        assert_eq!(content, "回答です");
        assert_eq!(db::counts(&conn)?.events, 3);
        Ok(())
    }

    #[test]
    fn saves_title_and_is_searchable_as_gemini_web() -> Result<()> {
        let mut conn = db::open_memory()?;
        ingest(&mut conn, &payload(&[(0, "user", "FreeWifiでAIチャットをすると？")]))?;
        let events = db::timeline(&conn, None, Some("FreeWifi"), 10)?;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].source_tool, "gemini_web");
        assert_eq!(events[0].title.as_deref(), Some("フリーWi-Fi"));
        Ok(())
    }

    #[test]
    fn rejects_bad_payloads_without_writing() -> Result<()> {
        let mut conn = db::open_memory()?;
        let mut p = payload(&[(0, "user", "x")]);
        p.source = "evil".into();
        assert!(ingest(&mut conn, &p).is_err());
        let mut p = payload(&[(0, "system", "x")]);
        p.conversation_id = "abc123".into();
        assert!(ingest(&mut conn, &p).is_err());
        let mut p = payload(&[(0, "user", "x")]);
        p.conversation_id = "../etc".into();
        assert!(ingest(&mut conn, &p).is_err());
        let mut p = payload(&[(0, "user", "x")]);
        p.captured_at = "now'; DROP TABLE events;--".into();
        assert!(ingest(&mut conn, &p).is_err());
        assert_eq!(db::counts(&conn)?.sessions, 0);
        Ok(())
    }
}
