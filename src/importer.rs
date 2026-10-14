use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

const CLAUDE_TOOL: &str = "claude_code";
const GEMINI_TOOL: &str = "gemini_cli";

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub files: usize,
    pub sessions: usize,
    pub events: usize,
    pub skipped_lines: usize,
}

/// Claude CodeのJSONLセッションをSQLiteへ追記する。
/// `date` が指定された場合は、その日付のイベントだけを取り込む。
pub fn import_claude(root: &Path, date: Option<&str>, conn: &mut Connection) -> Result<ImportReport> {
    let mut files = Vec::new();
    collect_jsonl(root, &mut files)?;
    files.sort();

    let tx = conn.transaction()?;
    let mut report = ImportReport::default();
    for path in files {
        let imported = import_file(&tx, &path, date, &mut report)?;
        if imported {
            report.files += 1;
        }
    }
    tx.commit()?;
    Ok(report)
}

/// Gemini CLIのセッションJSONLをSQLiteへ追記する。
/// Gemini CLIは同じmessages配列を`$set`で何度も書くため、message idで冪等化する。
pub fn import_gemini(root: &Path, date: Option<&str>, conn: &mut Connection) -> Result<ImportReport> {
    let mut files = Vec::new();
    collect_jsonl(root, &mut files)?;
    files.sort();

    let tx = conn.transaction()?;
    let mut report = ImportReport::default();
    for path in files {
        if import_gemini_file(&tx, &path, date, &mut report)? {
            report.files += 1;
        }
    }
    tx.commit()?;
    Ok(report)
}

fn import_gemini_file(
    tx: &rusqlite::Transaction<'_>,
    path: &Path,
    date: Option<&str>,
    report: &mut ImportReport,
) -> Result<bool> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let source_file = path.to_string_lossy().into_owned();
    let mut session_key = path.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown").to_string();
    let project_path = path.parent().and_then(Path::parent).and_then(Path::file_name)
        .and_then(|s| s.to_str()).unwrap_or("(unknown)").to_string();
    let mut rows = Vec::new();
    let mut seen_event_ids = HashSet::new();

    for (line_number, line) in reader.lines().enumerate() {
        let line = line?;
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            report.skipped_lines += 1;
            continue;
        };
        if let Some(id) = value.get("sessionId").and_then(Value::as_str) {
            session_key = id.to_string();
        }
        if let Some(messages) = value.get("$set").and_then(|set| set.get("messages")).and_then(Value::as_array) {
            for (message_index, message) in messages.iter().enumerate() {
                let Some((occurred_at, role, content)) = gemini_message_content(message) else { continue };
                if date.is_some_and(|wanted| !occurred_at.starts_with(wanted)) { continue; }
                let event_id = message.get("id").and_then(Value::as_str).map(str::to_owned)
                    .unwrap_or_else(|| format!("{session_key}:{line_number}:{message_index}"));
                if seen_event_ids.insert(event_id.clone()) {
                    rows.push((event_id, occurred_at, role, content, message_index as i64));
                }
            }
        } else if let Some((occurred_at, role, content)) = gemini_message_content(&value) {
            if date.is_some_and(|wanted| !occurred_at.starts_with(wanted)) { continue; }
            let event_id = value.get("id").or_else(|| value.get("uuid")).and_then(Value::as_str)
                .map(str::to_owned).unwrap_or_else(|| format!("{session_key}:{line_number}"));
            if seen_event_ids.insert(event_id.clone()) {
                rows.push((event_id, occurred_at, role, content, line_number as i64));
            }
        }
    }
    if rows.is_empty() { return Ok(false); }
    let (session_id, created) = find_or_create_session_for_tool(tx, GEMINI_TOOL, &source_file, &project_path, &session_key)?;
    if created { report.sessions += 1; }
    for (event_id, occurred_at, role, content, sequence) in rows {
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO events (session_id, source_tool, source_event_id, occurred_at, role, content, sequence)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![session_id, GEMINI_TOOL, event_id, occurred_at, role, content, sequence],
        )?;
        report.events += inserted;
    }
    Ok(true)
}

fn gemini_message_content(value: &Value) -> Option<(String, &'static str, String)> {
    let occurred_at = value.get("timestamp")?.as_str()?.to_string();
    let role = match value.get("type")?.as_str()? {
        "user" => "user",
        "gemini" | "assistant" | "model" => "assistant",
        _ => return None,
    };
    let content = value.get("content")?;
    let text = if let Some(text) = content.as_str() {
        text.to_string()
    } else {
        content.as_array()?.iter().filter_map(|block| block.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n")
    };
    let text = text.trim();
    (!text.is_empty()).then(|| (occurred_at, role, text.to_string()))
}

fn collect_jsonl(root: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if !root.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(root)
        .with_context(|| format!("failed to read {}", root.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl(&path, out)?;
        } else if path.extension().is_some_and(|ext| ext == "jsonl") {
            out.push(path);
        }
    }
    Ok(())
}

fn import_file(
    tx: &rusqlite::Transaction<'_>,
    path: &Path,
    date: Option<&str>,
    report: &mut ImportReport,
) -> Result<bool> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let source_file = path.to_string_lossy().into_owned();
    let mut project_path = String::new();
    let mut session_key = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let mut rows = Vec::new();

    for (line_number, line) in reader.lines().enumerate() {
        let line = line?;
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            report.skipped_lines += 1;
            continue;
        };
        if let Some(cwd) = value.get("cwd").and_then(Value::as_str) {
            if project_path.is_empty() {
                project_path = cwd.to_string();
            }
        }
        if let Some(id) = value.get("sessionId").and_then(Value::as_str) {
            session_key = id.to_string();
        }
        let Some((occurred_at, role, content)) = event_content(&value) else {
            continue;
        };
        if date.is_some_and(|wanted| !occurred_at.starts_with(wanted)) {
            continue;
        }
        if project_path.is_empty() {
            project_path = "(unknown)".to_string();
        }
        let event_id = value
            .get("uuid")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{session_key}:{line_number}"));
        rows.push((event_id, occurred_at, role, content, line_number as i64));
    }

    if rows.is_empty() {
        return Ok(false);
    }
    if project_path.is_empty() {
        project_path = "(unknown)".to_string();
    }

    let (session_id, created) = find_or_create_session(tx, &source_file, &project_path, &session_key)?;
    if created {
        report.sessions += 1;
    }
    for (event_id, occurred_at, role, content, sequence) in rows {
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO events
             (session_id, source_tool, source_event_id, occurred_at, role, content, sequence)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![session_id, CLAUDE_TOOL, event_id, occurred_at, role, content, sequence],
        )?;
        report.events += inserted;
    }
    Ok(true)
}

fn find_or_create_session(
    tx: &rusqlite::Transaction<'_>,
    source_file: &str,
    project_path: &str,
    session_key: &str,
) -> Result<(i64, bool)> {
    let existing = tx
        .query_row(
            "SELECT id FROM sessions WHERE source_tool = ?1 AND source_file = ?2",
            params![CLAUDE_TOOL, source_file],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(id) = existing {
        return Ok((id, false));
    }
    tx.execute(
        "INSERT INTO sessions(source_tool, project_path, source_file, source_hash)
         VALUES (?1, ?2, ?3, ?4)",
        params![CLAUDE_TOOL, project_path, source_file, session_key],
    )?;
    Ok((tx.last_insert_rowid(), true))
}

fn find_or_create_session_for_tool(
    tx: &rusqlite::Transaction<'_>,
    source_tool: &str,
    source_file: &str,
    project_path: &str,
    session_key: &str,
) -> Result<(i64, bool)> {
    let existing = tx.query_row(
        "SELECT id FROM sessions WHERE source_tool = ?1 AND source_file = ?2",
        params![source_tool, source_file],
        |row| row.get(0),
    ).optional()?;
    if let Some(id) = existing { return Ok((id, false)); }
    tx.execute(
        "INSERT INTO sessions(source_tool, project_path, source_file, source_hash) VALUES (?1, ?2, ?3, ?4)",
        params![source_tool, project_path, source_file, session_key],
    )?;
    Ok((tx.last_insert_rowid(), true))
}

fn event_content(value: &Value) -> Option<(String, &'static str, String)> {
    let kind = value.get("type")?.as_str()?;
    let occurred_at = value.get("timestamp")?.as_str()?.to_string();
    let message = value.get("message")?;
    match kind {
        "user" => {
            if value.get("isMeta").and_then(Value::as_bool) == Some(true) {
                return None;
            }
            let content = message.get("content")?.as_str()?.trim();
            (!content.is_empty()).then(|| (occurred_at, "user", content.to_string()))
        }
        "assistant" => {
            let blocks = message.get("content")?.as_array()?;
            let text = blocks
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            (!text.is_empty()).then(|| (occurred_at, "assistant", text))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEST_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

    #[test]
    fn imports_user_and_assistant_and_is_idempotent() -> Result<()> {
        let temp = tempfile_dir();
        let path = temp.join("session.jsonl");
        let mut file = File::create(&path)?;
        writeln!(file, r#"{{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"2026-10-13T01:00:00Z","cwd":"/tmp/project","message":{{"content":"調べる"}}}}"#)?;
        writeln!(file, r#"{{"type":"assistant","uuid":"a1","sessionId":"s1","timestamp":"2026-10-13T01:01:00Z","cwd":"/tmp/project","message":{{"content":[{{"type":"text","text":"調査しました"}}]}}}}"#)?;
        writeln!(file, r#"{{"type":"tool_result","uuid":"t1","sessionId":"s1","timestamp":"2026-10-13T01:02:00Z","cwd":"/tmp/project"}}"#)?;

        let mut conn = db::open_memory()?;
        let first = import_claude(&temp, Some("2026-10-13"), &mut conn)?;
        assert_eq!(first.files, 1);
        assert_eq!(first.events, 2);
        assert_eq!(db::counts(&conn)?.events, 2);

        let second = import_claude(&temp, Some("2026-10-13"), &mut conn)?;
        assert_eq!(second.events, 0);
        assert_eq!(db::counts(&conn)?.sessions, 1);

        std::fs::remove_dir_all(temp)?;
        Ok(())
    }

    #[test]
    fn imports_gemini_set_messages_and_deduplicates_updates() -> Result<()> {
        let temp = tempfile_dir();
        let chats = temp.join("project").join("chats");
        std::fs::create_dir_all(&chats)?;
        let path = chats.join("session.jsonl");
        let mut file = File::create(&path)?;
        let update = r#"{"$set":{"messages":[{"id":"g1","timestamp":"2026-10-13T02:00:00Z","type":"user","content":[{"text":"質問"}]},{"id":"g2","timestamp":"2026-10-13T02:01:00Z","type":"gemini","content":[{"text":"回答"}]}]}}"#;
        writeln!(file, "{update}")?;
        writeln!(file, "{update}")?;

        let mut conn = db::open_memory()?;
        let report = import_gemini(&temp, Some("2026-10-13"), &mut conn)?;
        assert_eq!(report.files, 1);
        assert_eq!(report.events, 2);
        assert_eq!(db::counts(&conn)?.events, 2);

        std::fs::remove_dir_all(temp)?;
        Ok(())
    }

    fn tempfile_dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "mrvoie-test-{}-{}",
            std::process::id(),
            TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }
}
