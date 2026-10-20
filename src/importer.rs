use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::tools::{self, ToolCall};

const CLAUDE_TOOL: &str = "claude_code";
const GEMINI_TOOL: &str = "gemini_cli";
const CODEX_TOOL: &str = "codex";

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub files: usize,
    pub sessions: usize,
    pub events: usize,
    pub skipped_lines: usize,
}

/// Claude CodeのJSONLを、前回の読み取り位置からSQLiteへ追記する。
/// 初回はファイル全体を読み、その後は追記された完全な行だけを読む。
pub fn collect_claude_incremental(root: &Path, conn: &mut Connection) -> Result<ImportReport> {
    let mut files = Vec::new();
    collect_jsonl(root, &mut files)?;
    files.sort();

    let tx = conn.transaction()?;
    let mut report = ImportReport::default();
    for path in files {
        collect_claude_file(&tx, &path, &mut report)?;
    }
    tx.commit()?;
    Ok(report)
}

fn collect_claude_file(
    tx: &rusqlite::Transaction<'_>,
    path: &Path,
    report: &mut ImportReport,
) -> Result<()> {
    let source_file = path.to_string_lossy().into_owned();
    let file_size = std::fs::metadata(path)?.len();
    let saved_offset: u64 = tx
        .query_row(
            "SELECT byte_offset FROM collector_files WHERE source_tool = ?1 AND source_file = ?2",
            params![CLAUDE_TOOL, source_file],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .unwrap_or(0)
        .max(0) as u64;
    let offset = if saved_offset > file_size { 0 } else { saved_offset };

    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let complete_len = bytes.iter().rposition(|byte| *byte == b'\n').map(|index| index + 1).unwrap_or(0);
    let next_offset = offset + complete_len as u64;
    let text = String::from_utf8_lossy(&bytes[..complete_len]);

    let mut project_path = String::new();
    let mut session_key = path.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown").to_string();
    let mut rows = Vec::new();
    let mut title: Option<String> = None;
    let mut breaks: Vec<String> = Vec::new();
    let mut calls: Vec<ToolCall> = Vec::new();
    for (line_index, line) in text.lines().enumerate() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            report.skipped_lines += 1;
            continue;
        };
        if let Some(found) = ai_title(&value) { title = Some(found); }
        if let Some(at) = compact_boundary(&value) { breaks.push(at); }
        calls.extend(tools::claude_calls(&value));
        if let Some(cwd) = value.get("cwd").and_then(Value::as_str) {
            if project_path.is_empty() { project_path = cwd.to_string(); }
        }
        if let Some(id) = value.get("sessionId").and_then(Value::as_str) {
            session_key = id.to_string();
        }
        let Some((occurred_at, role, content)) = event_content(&value) else { continue; };
        let event_id = value.get("uuid").and_then(Value::as_str).map(str::to_owned)
            .unwrap_or_else(|| format!("{session_key}:{offset}:{}", line_index));
        rows.push((event_id, occurred_at, role, content, line_index as i64));
    }

    if !rows.is_empty() {
        if project_path.is_empty() { project_path = "(unknown)".to_string(); }
        let (session_id, created) = find_or_create_session(tx, &source_file, &project_path, &session_key)?;
        if created { report.sessions += 1; }
        for (event_id, occurred_at, role, content, sequence) in rows {
            let inserted = tx.execute(
                "INSERT OR IGNORE INTO events (session_id, source_tool, source_event_id, occurred_at, role, content, sequence)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![session_id, CLAUDE_TOOL, event_id, occurred_at, role, content, sequence],
            )?;
            report.events += inserted;
        }
    }
    store_calls(tx, CLAUDE_TOOL, &source_file, &calls)?;
    if let Some(title) = title { set_title(tx, &source_file, &title)?; }
    record_breaks(tx, &source_file, &breaks)?;
    if next_offset != saved_offset {
        tx.execute(
            "INSERT INTO collector_files(source_tool, source_file, byte_offset, updated_at) VALUES (?1, ?2, ?3, CURRENT_TIMESTAMP)
             ON CONFLICT(source_tool, source_file) DO UPDATE SET byte_offset = excluded.byte_offset, updated_at = CURRENT_TIMESTAMP",
            params![CLAUDE_TOOL, source_file, next_offset as i64],
        )?;
        if next_offset > saved_offset { report.files += 1; }
    }
    Ok(())
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

// ---- Codex(ChatGPT の Codex アプリ・Codex CLI) ----
//
// `~/.codex/sessions/<年>/<月>/<日>/rollout-<日時>-<ID>.jsonl`。1 行が `{timestamp, ordinal, type, payload}`。
//   - `session_meta`: `payload.id`(セッション)、`payload.cwd`(プロジェクトのフォルダ)
//   - `event_msg` の `item_completed`: `payload.item.type` が `UserMessage`(あなたの発言)・`AgentMessage`(回答)のものだけ取る。
//     `Reasoning`・コマンドの実行・ファイルの変更などは取らない(Claude Code の取り込みと同じ方針)。
// ファイルは数百 MB になる。1 行ずつ読み、読み終えた位置を覚えて、追記分だけを読む。

#[derive(Debug, Default)]
pub struct CodexParsed {
    pub project_path: Option<String>,
    pub session_key: Option<String>,
    /// (event_id, occurred_at, role, content, sequence)
    pub rows: Vec<(String, String, &'static str, String, i64)>,
    /// ツールの呼び出し(実行したコマンド・変更したファイル・MCP の呼び出し)
    pub calls: Vec<ToolCall>,
    pub next_offset: u64,
    pub skipped_lines: usize,
}

/// `root` 以下の rollout-*.jsonl を、名前順に返す。
pub fn codex_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    if root.exists() {
        collect_jsonl(root, &mut files)?;
    }
    files.retain(|path| path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("rollout-")));
    files.sort();
    Ok(files)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

/// DB には触らずに、`offset` から先を読んで解析する(長くかかるので、DB のロックの外で呼ぶ)。
/// 改行で終わっていない最後の行は、まだ書き込み中かもしれないので読まない。
pub fn parse_codex_file(path: &Path, offset: u64, date: Option<&str>) -> Result<CodexParsed> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let size = file.metadata()?.len();
    let offset = if offset > size { 0 } else { offset };
    let mut reader = BufReader::with_capacity(1 << 20, file);
    reader.seek(SeekFrom::Start(offset))?;

    let mut parsed = CodexParsed { next_offset: offset, ..Default::default() };
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line)?;
        if read == 0 || line.last() != Some(&b'\n') {
            break;
        }
        parsed.next_offset += read as u64;
        // 必要な行(session_meta と item_completed)だけを、JSON として読む(数百 MB を全部は解析しない)
        let wanted_meta = contains(&line, b"\"session_meta\"");
        if !wanted_meta && !contains(&line, b"\"item_completed\"") {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&line) else {
            parsed.skipped_lines += 1;
            continue;
        };
        let payload = value.get("payload");
        match value.get("type").and_then(Value::as_str) {
            Some("session_meta") => {
                if let Some(cwd) = payload.and_then(|p| p.get("cwd")).and_then(Value::as_str) {
                    parsed.project_path = Some(cwd.to_string());
                }
                if let Some(id) = payload.and_then(|p| p.get("id")).and_then(Value::as_str) {
                    parsed.session_key = Some(id.to_string());
                }
            }
            Some("event_msg") => {
                let Some(payload) = payload else { continue };
                if payload.get("type").and_then(Value::as_str) != Some("item_completed") {
                    continue;
                }
                if let Some(call) = tools::codex_call(&value) {
                    if date.is_none_or(|wanted| call.occurred_at.starts_with(wanted)) {
                        parsed.calls.push(call);
                    }
                    continue;
                }
                let Some(item) = payload.get("item") else { continue };
                let role = match item.get("type").and_then(Value::as_str) {
                    Some("UserMessage") => "user",
                    Some("AgentMessage") => "assistant",
                    _ => continue,
                };
                let Some(occurred_at) = value.get("timestamp").and_then(Value::as_str) else { continue };
                if date.is_some_and(|wanted| !occurred_at.starts_with(wanted)) {
                    continue;
                }
                let text = item
                    .get("content")
                    .and_then(Value::as_array)
                    .map(|blocks| blocks.iter().filter_map(|b| b.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n"))
                    .unwrap_or_default();
                let text = text.trim();
                if text.is_empty() {
                    continue;
                }
                let ordinal = value.get("ordinal").and_then(Value::as_i64).unwrap_or(0);
                let event_id = item
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("{}:{ordinal}", path.file_stem().and_then(|s| s.to_str()).unwrap_or("codex")));
                parsed.rows.push((event_id, occurred_at.to_string(), role, text.to_string(), ordinal));
            }
            _ => {}
        }
    }
    Ok(parsed)
}

/// 前回までに読み終えた位置(無ければ 0)。
pub fn codex_saved_offset(conn: &Connection, path: &Path) -> Result<u64> {
    let offset: Option<i64> = conn
        .query_row(
            "SELECT byte_offset FROM collector_files WHERE source_tool = ?1 AND source_file = ?2",
            params![CODEX_TOOL, path.to_string_lossy()],
            |row| row.get(0),
        )
        .optional()?;
    Ok(offset.unwrap_or(0).max(0) as u64)
}

/// 解析した結果を DB に書く。`save_offset` が true なら、読み終えた位置も覚える。
pub fn store_codex(conn: &mut Connection, path: &Path, parsed: &CodexParsed, save_offset: bool, report: &mut ImportReport) -> Result<()> {
    let source_file = path.to_string_lossy().into_owned();
    let tx = conn.transaction()?;
    if !parsed.rows.is_empty() {
        let known_project: Option<String> = tx
            .query_row(
                "SELECT project_path FROM sessions WHERE source_tool = ?1 AND source_file = ?2",
                params![CODEX_TOOL, source_file],
                |row| row.get(0),
            )
            .optional()?;
        // 追記分だけを読むときは session_meta(先頭の行)が含まれないので、すでにある行のプロジェクトを使う
        let project = parsed.project_path.clone().or(known_project).unwrap_or_else(|| "(unknown)".to_string());
        let key = parsed.session_key.clone().unwrap_or_else(|| path.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown").to_string());
        let (session_id, created) = find_or_create_session_for_tool(&tx, CODEX_TOOL, &source_file, &project, &key)?;
        if created {
            report.sessions += 1;
        }
        for (event_id, occurred_at, role, content, sequence) in &parsed.rows {
            report.events += tx.execute(
                "INSERT OR IGNORE INTO events (session_id, source_tool, source_event_id, occurred_at, role, content, sequence)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![session_id, CODEX_TOOL, event_id, occurred_at, role, content, sequence],
            )?;
        }
        // 題名は、ログに無いので、最初のあなたの発言から付ける(すでにあれば変えない)
        if let Some((_, _, _, first, _)) = parsed.rows.iter().find(|r| r.2 == "user") {
            let title: String = first.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(60).collect();
            tx.execute("UPDATE sessions SET title = ?2 WHERE id = ?1 AND title IS NULL", params![session_id, title])?;
        }
    }
    store_calls(&tx, CODEX_TOOL, &source_file, &parsed.calls)?;
    report.skipped_lines += parsed.skipped_lines;
    if save_offset {
        let saved: i64 = tx
            .query_row(
                "SELECT byte_offset FROM collector_files WHERE source_tool = ?1 AND source_file = ?2",
                params![CODEX_TOOL, source_file],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0);
        if parsed.next_offset as i64 != saved {
            tx.execute(
                "INSERT INTO collector_files(source_tool, source_file, byte_offset, updated_at) VALUES (?1, ?2, ?3, CURRENT_TIMESTAMP)
                 ON CONFLICT(source_tool, source_file) DO UPDATE SET byte_offset = excluded.byte_offset, updated_at = CURRENT_TIMESTAMP",
                params![CODEX_TOOL, source_file, parsed.next_offset as i64],
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Codex のセッションを、前回の読み取り位置からSQLiteへ追記する(ロックを細かく扱いたいアプリは、上の3つを直接使う)。
pub fn collect_codex_incremental(root: &Path, conn: &mut Connection) -> Result<ImportReport> {
    let mut report = ImportReport::default();
    for path in codex_files(root)? {
        let parsed = parse_codex_file(&path, codex_saved_offset(conn, &path)?, None)?;
        if !parsed.rows.is_empty() {
            report.files += 1;
        }
        store_codex(conn, &path, &parsed, true, &mut report)?;
    }
    Ok(report)
}

/// Codex のセッションをすべて(または `date` の日だけ)取り込む。読み取り位置は覚えない(CLI 用)。
pub fn import_codex(root: &Path, date: Option<&str>, conn: &mut Connection) -> Result<ImportReport> {
    let mut report = ImportReport::default();
    for path in codex_files(root)? {
        let parsed = parse_codex_file(&path, 0, date)?;
        if !parsed.rows.is_empty() {
            report.files += 1;
        }
        store_codex(conn, &path, &parsed, false, &mut report)?;
    }
    Ok(report)
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
    let mut title: Option<String> = None;
    let mut breaks: Vec<String> = Vec::new();
    let mut calls: Vec<ToolCall> = Vec::new();

    for (line_number, line) in reader.lines().enumerate() {
        let line = line?;
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            report.skipped_lines += 1;
            continue;
        };
        if let Some(found) = ai_title(&value) { title = Some(found); }
        if let Some(at) = compact_boundary(&value) { breaks.push(at); }
        calls.extend(tools::claude_calls(&value).into_iter().filter(|call| date.is_none_or(|wanted| call.occurred_at.starts_with(wanted))));
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
    store_calls(tx, CLAUDE_TOOL, &source_file, &calls)?;
    if let Some(title) = title { set_title(tx, &source_file, &title)?; }
    record_breaks(tx, &source_file, &breaks)?;
    Ok(true)
}

/// この記録ファイルの会話(すでにあるとき)に、ツールの呼び出しを保存する。会話がまだ無ければ、何もしない。入れた件数を返す。
fn store_calls(conn: &Connection, source_tool: &str, source_file: &str, calls: &[ToolCall]) -> Result<usize> {
    if calls.is_empty() {
        return Ok(0);
    }
    let session_id: Option<i64> = conn
        .query_row("SELECT id FROM sessions WHERE source_tool = ?1 AND source_file = ?2", params![source_tool, source_file], |row| row.get(0))
        .optional()?;
    match session_id {
        Some(id) => tools::store(conn, id, source_tool, calls),
        None => Ok(0),
    }
}

/// Claude Code がセッションに付ける題名(`{"type":"ai-title","aiTitle":"..."}`)。
fn ai_title(value: &Value) -> Option<String> {
    if value.get("type")?.as_str()? != "ai-title" { return None; }
    let title = value.get("aiTitle")?.as_str()?.trim();
    (!title.is_empty()).then(|| title.to_string())
}

/// Claude Code が、長くなった会話を要約して続けるときに書く印(`{"type":"system","subtype":"compact_boundary","timestamp":"…"}`)。その時刻。
fn compact_boundary(value: &Value) -> Option<String> {
    if value.get("type")?.as_str()? != "system" || value.get("subtype")?.as_str()? != "compact_boundary" { return None; }
    value.get("timestamp")?.as_str().map(str::to_owned)
}

/// 要約して続けた位置を、会話の区切りの手がかりとして、`settings` に残す(`section_break:<会話の ID>:<時刻>`)。
/// スキーマは変えない。区切りの決め方は `sections.rs`。
fn record_breaks(tx: &rusqlite::Transaction<'_>, source_file: &str, breaks: &[String]) -> Result<()> {
    if breaks.is_empty() { return Ok(()); }
    let session_id: Option<i64> = tx
        .query_row("SELECT id FROM sessions WHERE source_tool = ?1 AND source_file = ?2", params![CLAUDE_TOOL, source_file], |r| r.get(0))
        .optional()?;
    let Some(session_id) = session_id else { return Ok(()); };
    for at in breaks {
        tx.execute(
            "INSERT OR IGNORE INTO settings(key, value) VALUES (?1, '1')",
            [format!("{}{session_id}:{at}", crate::sections::BREAK_PREFIX)],
        )?;
    }
    Ok(())
}

const CLAUDE_CALLS_DONE: &str = "tool_calls_backfilled_claude";
const CODEX_CALLS_DONE: &str = "tool_calls_backfilled_codex";

/// すでに取り込んだ Claude Code の会話に、記録ファイルから、ツールの呼び出しをさかのぼって保存する。**1 度だけ**走る
/// (増分の収集は、読み済みの位置から先しか読まないため)。返り値は、入れた件数。記録ファイルが消えていれば、補えない。
pub fn backfill_tool_calls_claude(root: &Path, conn: &mut Connection) -> Result<usize> {
    if crate::sync::store::get(conn, CLAUDE_CALLS_DONE)?.as_deref() == Some("1") { return Ok(0); }
    let mut files = Vec::new();
    collect_jsonl(root, &mut files)?;
    let tx = conn.transaction()?;
    let mut added = 0;
    for path in files {
        let Ok(file) = File::open(&path) else { continue; };
        let mut calls: Vec<ToolCall> = Vec::new();
        for line in BufReader::new(file).lines() {
            let Ok(line) = line else { break; };
            if !line.contains("\"tool_use\"") { continue; }
            if let Ok(value) = serde_json::from_str::<Value>(&line) { calls.extend(tools::claude_calls(&value)); }
        }
        added += store_calls(&tx, CLAUDE_TOOL, &path.to_string_lossy(), &calls)?;
    }
    tx.execute("INSERT INTO settings(key, value) VALUES (?1, '1') ON CONFLICT(key) DO UPDATE SET value = '1'", [CLAUDE_CALLS_DONE])?;
    tx.commit()?;
    Ok(added)
}

/// Codex の記録ファイル全体から、ツールの呼び出しだけを読む(DB には触らない。数百 MB になるので、DB のロックの外で呼ぶ)。
pub fn parse_codex_calls(path: &Path) -> Result<Vec<ToolCall>> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut reader = BufReader::with_capacity(1 << 20, file);
    let mut calls = Vec::new();
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 { break; }
        if line.last() != Some(&b'\n') { break; }
        if !contains(&line, b"\"item_completed\"") { continue; }
        if !(contains(&line, b"\"CommandExecution\"") || contains(&line, b"\"FileChange\"") || contains(&line, b"\"McpToolCall\"")) { continue; }
        if let Ok(value) = serde_json::from_slice::<Value>(&line) {
            calls.extend(tools::codex_call(&value));
        }
    }
    Ok(calls)
}

/// すでに取り込んだ Codex の会話に、ツールの呼び出しをさかのぼって保存する。**1 度だけ**走る。
/// 記録ファイルは大きいので、読む間は DB のロックを持たず、書くときだけ持つ。返り値は、入れた件数。
pub fn backfill_tool_calls_codex(conn: &std::sync::Mutex<Connection>, root: &Path) -> Result<usize> {
    let done = |conn: &Connection| -> Result<bool> { Ok(crate::sync::store::get(conn, CODEX_CALLS_DONE)?.as_deref() == Some("1")) };
    {
        let conn = conn.lock().map_err(|_| anyhow::anyhow!("database lock poisoned"))?;
        if done(&conn)? { return Ok(0); }
    }
    let mut added = 0;
    for path in codex_files(root)? {
        let calls = match parse_codex_calls(&path) {
            Ok(calls) => calls,
            Err(error) => { eprintln!("synclane tool calls: {}: {error:#}", path.display()); continue; }
        };
        let conn = conn.lock().map_err(|_| anyhow::anyhow!("database lock poisoned"))?;
        added += store_calls(&conn, CODEX_TOOL, &path.to_string_lossy(), &calls)?;
    }
    let conn = conn.lock().map_err(|_| anyhow::anyhow!("database lock poisoned"))?;
    crate::sync::store::set(&conn, CODEX_CALLS_DONE, "1")?;
    Ok(added)
}

/// すでに取り込んだ会話について、記録ファイルから、要約して続けた位置をさかのぼって残す。**1 度だけ**走る
/// (増分の収集は、読み済みの位置から先しか読まないため、別に走らせる)。返り値は、残した位置の数。
pub fn backfill_claude_breaks(root: &Path, conn: &mut Connection) -> Result<usize> {
    const DONE: &str = "section_breaks_backfilled";
    if crate::sync::store::get(conn, DONE)?.as_deref() == Some("1") { return Ok(0); }
    let mut files = Vec::new();
    collect_jsonl(root, &mut files)?;
    let tx = conn.transaction()?;
    let mut found = 0;
    for path in files {
        let Ok(file) = File::open(&path) else { continue; };
        let mut breaks: Vec<String> = Vec::new();
        for line in BufReader::new(file).lines() {
            let Ok(line) = line else { break; };
            if !line.contains("\"compact_boundary\"") { continue; }
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                if let Some(at) = compact_boundary(&value) { breaks.push(at); }
            }
        }
        found += breaks.len();
        record_breaks(&tx, &path.to_string_lossy(), &breaks)?;
    }
    tx.execute("INSERT INTO settings(key, value) VALUES (?1, '1') ON CONFLICT(key) DO UPDATE SET value = '1'", [DONE])?;
    tx.commit()?;
    Ok(found)
}

fn set_title(tx: &rusqlite::Transaction<'_>, source_file: &str, title: &str) -> Result<()> {
    tx.execute(
        "UPDATE sessions SET title = ?3 WHERE source_tool = ?1 AND source_file = ?2 AND COALESCE(title, '') <> ?3
           AND NOT EXISTS (SELECT 1 FROM settings WHERE key = 'title_manual:' || sessions.id)",
        params![CLAUDE_TOOL, source_file, title],
    )?;
    Ok(())
}

/// 題名の無いセッションに、ログから題名を入れる(題名の対応より前に取り込んだ分の補完)。
/// 増分の収集は読み済みの位置から先しか読まないため、別に走らせる。返り値は題名を入れた件数。
pub fn backfill_claude_titles(root: &Path, conn: &mut Connection) -> Result<usize> {
    let mut files = Vec::new();
    collect_jsonl(root, &mut files)?;
    let tx = conn.transaction()?;
    let mut updated = 0;
    for path in files {
        let source_file = path.to_string_lossy().into_owned();
        let needs: bool = tx
            .query_row(
                "SELECT 1 FROM sessions WHERE source_tool = ?1 AND source_file = ?2 AND title IS NULL",
                params![CLAUDE_TOOL, source_file],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if !needs { continue; }
        let Ok(file) = File::open(&path) else { continue; };
        let mut title: Option<String> = None;
        for line in BufReader::new(file).lines() {
            let Ok(line) = line else { break; };
            if !line.contains("\"ai-title\"") { continue; }
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                if let Some(found) = ai_title(&value) { title = Some(found); }
            }
        }
        if let Some(title) = title {
            set_title(&tx, &source_file, &title)?;
            updated += 1;
        }
    }
    tx.commit()?;
    Ok(updated)
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
    fn saves_session_title_and_backfills_and_searches_by_it() -> Result<()> {
        let temp = tempfile_dir();
        let path = temp.join("session.jsonl");
        let mut file = File::create(&path)?;
        writeln!(file, r#"{{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"2026-10-13T01:00:00Z","cwd":"/tmp/project","message":{{"content":"調べる"}}}}"#)?;
        writeln!(file, r#"{{"type":"ai-title","aiTitle":"Rename the app","sessionId":"s1"}}"#)?;
        writeln!(file, r#"{{"type":"ai-title","aiTitle":"Rename app to SyncLane","sessionId":"s1"}}"#)?;

        // 増分の収集: 最後の題名が入る
        let mut conn = db::open_memory()?;
        collect_claude_incremental(&temp, &mut conn)?;
        let title = |conn: &Connection| -> Result<Option<String>> {
            Ok(conn.query_row("SELECT title FROM sessions", [], |row| row.get(0))?)
        };
        assert_eq!(title(&conn)?.as_deref(), Some("Rename app to SyncLane"));
        let found = db::timeline(&conn, None, Some("synclane"), 10)?;
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].title.as_deref(), Some("Rename app to SyncLane"));

        // 題名の対応より前に取り込んだ想定: 題名を消してから補完する
        conn.execute("UPDATE sessions SET title = NULL", [])?;
        assert_eq!(backfill_claude_titles(&temp, &mut conn)?, 1);
        assert_eq!(title(&conn)?.as_deref(), Some("Rename app to SyncLane"));
        assert_eq!(backfill_claude_titles(&temp, &mut conn)?, 0);

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

    #[test]
    fn imports_codex_messages_incrementally_and_skips_tool_noise() -> Result<()> {
        let temp = tempfile_dir();
        let day = temp.join("2026").join("10").join("15");
        std::fs::create_dir_all(&day)?;
        let path = day.join("rollout-2026-10-15T20-00-00-abc.jsonl");
        let meta = r#"{"timestamp":"2026-10-15T11:00:00.000Z","ordinal":0,"type":"session_meta","payload":{"id":"sess-1","cwd":"/Users/x/MrDiff","originator":"Codex Desktop"}}"#;
        let user = r#"{"timestamp":"2026-10-15T11:00:01.000Z","ordinal":1,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","id":"u1","content":[{"type":"text","text":"同期できない理由を調べる"}]}}}"#;
        let reasoning = r#"{"timestamp":"2026-10-15T11:00:02.000Z","ordinal":2,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"Reasoning","id":"r1","summary_text":["考え中"]}}}"#;
        let command = r#"{"timestamp":"2026-10-15T11:00:03.000Z","ordinal":3,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"CommandExecution","id":"c1"}}}"#;
        let agent = r#"{"timestamp":"2026-10-15T11:00:04.000Z","ordinal":4,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","id":"a1","phase":"final_answer","content":[{"type":"Text","text":"原因は別々の保存先です"}]}}}"#;
        let developer = r#"{"timestamp":"2026-10-15T11:00:05.000Z","ordinal":5,"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"指示文"}]}}"#;
        std::fs::write(&path, format!("{meta}\n{user}\n{reasoning}\n{command}\n{agent}\n{developer}\n"))?;

        let mut conn = db::open_memory()?;
        let report = collect_codex_incremental(&temp, &mut conn)?;
        assert_eq!((report.sessions, report.events), (1, 2));
        let (project, title, tool): (String, String, String) = conn.query_row(
            "SELECT project_path, title, source_tool FROM sessions",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        assert_eq!((project.as_str(), title.as_str(), tool.as_str()), ("/Users/x/MrDiff", "同期できない理由を調べる", "codex"));

        // もう一度読んでも増えない
        assert_eq!(collect_codex_incremental(&temp, &mut conn)?.events, 0);

        // 追記: 完全な行だけ読む。書き込み途中(改行なし)の行は、次回に回す
        let more = r#"{"timestamp":"2026-10-15T11:05:00.000Z","ordinal":6,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","id":"u2","content":[{"type":"text","text":"続き"}]}}}"#;
        let partial = r#"{"timestamp":"2026-10-15T11:06:00.000Z","ordinal":7,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","id":"a2","content":[{"type":"text","text":"書きかけ"#;
        let mut file = std::fs::OpenOptions::new().append(true).open(&path)?;
        writeln!(file, "{more}")?;
        write!(file, "{partial}")?;
        assert_eq!(collect_codex_incremental(&temp, &mut conn)?.events, 1);
        // 書き終わったら(改行が付いたら)、残りが入る
        writeln!(file, r#""}}]}}}}}}"#)?;
        assert_eq!(collect_codex_incremental(&temp, &mut conn)?.events, 1);
        assert_eq!(db::counts(&conn)?.events, 4);
        // 追記分だけを読んでも、プロジェクトは最初の行のまま
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM sessions WHERE project_path = '/Users/x/MrDiff'", [], |r| r.get(0))?;
        assert_eq!(n, 1);

        // CLI の取り込み(日付で絞る)は、位置を覚えない別の経路
        let mut other = db::open_memory()?;
        assert_eq!(import_codex(&temp, Some("2026-10-15"), &mut other)?.events, 4);
        assert_eq!(import_codex(&temp, Some("2026-10-14"), &mut other)?.events, 0);

        std::fs::remove_dir_all(temp)?;
        Ok(())
    }

    fn calls_of(conn: &Connection) -> Vec<(String, String, String)> {
        let mut stmt = conn.prepare("SELECT source_tool, name, detail FROM tool_calls ORDER BY occurred_at, id").unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap()
    }

    fn claude_session_lines() -> [&'static str; 5] {
        [
            r#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"2026-10-20T01:00:00Z","cwd":"/work/app","message":{"content":"ビルドが通らない"}}"#,
            // テキストの無い、ツール呼び出しだけの行(発言としては取り込まれない)
            r#"{"type":"assistant","uuid":"a1","sessionId":"s1","timestamp":"2026-10-20T01:00:05Z","cwd":"/work/app","message":{"content":[{"type":"tool_use","id":"tu1","name":"Bash","input":{"command":"npm run build"}}]}}"#,
            r#"{"type":"user","uuid":"r1","sessionId":"s1","timestamp":"2026-10-20T01:00:09Z","cwd":"/work/app","message":{"content":[{"type":"tool_result","tool_use_id":"tu1","content":"出力の本文: SECRET=abc123"}]}}"#,
            r#"{"type":"assistant","uuid":"a2","sessionId":"s1","timestamp":"2026-10-20T01:01:00Z","cwd":"/work/app","message":{"content":[{"type":"text","text":"設定を直します"},{"type":"tool_use","id":"tu2","name":"Edit","input":{"file_path":"/work/app/tsconfig.json","old_string":"古い本文","new_string":"新しい本文"}}]}}"#,
            r#"{"type":"assistant","uuid":"a3","sessionId":"s1","timestamp":"2026-10-21T02:00:00Z","cwd":"/work/app","message":{"content":[{"type":"tool_use","id":"tu3","name":"Read","input":{"file_path":"/work/app/README.md"}}]}}"#,
        ]
    }

    #[test]
    fn claude_tool_calls_are_saved_without_results_or_bodies_and_are_idempotent() -> Result<()> {
        let temp = tempfile_dir();
        let path = temp.join("session.jsonl");
        std::fs::write(&path, claude_session_lines().join("\n") + "\n")?;
        let mut conn = db::open_memory()?;
        let report = collect_claude_incremental(&temp, &mut conn)?;
        assert_eq!(report.events, 2); // 発言は、ユーザーとテキストのある AI だけ(これまでどおり)
        assert_eq!(
            calls_of(&conn),
            [
                ("claude_code".into(), "Bash".into(), "npm run build".into()),
                ("claude_code".into(), "Edit".into(), "/work/app/tsconfig.json".into()),
                ("claude_code".into(), "Read".into(), "/work/app/README.md".into()),
            ]
        );
        // 実行結果・編集の本文は、どこにも入らない
        let dump: String = calls_of(&conn).into_iter().map(|(a, b, c)| format!("{a}{b}{c}")).collect();
        assert!(!dump.contains("SECRET") && !dump.contains("古い本文") && !dump.contains("新しい本文"));
        // もう一度読んでも増えない。追記した分だけが増える
        collect_claude_incremental(&temp, &mut conn)?;
        assert_eq!(calls_of(&conn).len(), 3);
        let mut file = std::fs::OpenOptions::new().append(true).open(&path)?;
        writeln!(file, r#"{{"type":"assistant","uuid":"a4","sessionId":"s1","timestamp":"2026-10-21T02:05:00Z","cwd":"/work/app","message":{{"content":[{{"type":"tool_use","id":"tu4","name":"Bash","input":{{"command":"git status"}}}}]}}}}"#)?;
        collect_claude_incremental(&temp, &mut conn)?;
        assert_eq!(calls_of(&conn).len(), 4);
        // 全件の取り込み(CLI)でも、重複しない。日付を指定したら、その日の分だけ
        import_claude(&temp, None, &mut conn)?;
        assert_eq!(calls_of(&conn).len(), 4);
        let mut other = db::open_memory()?;
        import_claude(&temp, Some("2026-10-20"), &mut other)?;
        assert_eq!(calls_of(&other).len(), 2);
        std::fs::remove_dir_all(temp)?;
        Ok(())
    }

    #[test]
    fn claude_tool_calls_are_backfilled_once_for_conversations_already_imported() -> Result<()> {
        let temp = tempfile_dir();
        std::fs::write(temp.join("session.jsonl"), claude_session_lines().join("\n") + "\n")?;
        let mut conn = db::open_memory()?;
        collect_claude_incremental(&temp, &mut conn)?;
        // この版より前に取り込んだ状態を作る(読み済みの位置はそのまま、呼び出しだけが無い)
        conn.execute("DELETE FROM tool_calls", [])?;
        assert!(calls_of(&conn).is_empty());
        assert_eq!(collect_claude_incremental(&temp, &mut conn)?.events, 0); // 増分では、もう読まれない
        assert!(calls_of(&conn).is_empty());
        assert_eq!(backfill_tool_calls_claude(&temp, &mut conn)?, 3);
        assert_eq!(calls_of(&conn).len(), 3);
        // 1 度だけ(消しても、もう一度は走らない)
        conn.execute("DELETE FROM tool_calls", [])?;
        assert_eq!(backfill_tool_calls_claude(&temp, &mut conn)?, 0);
        assert!(calls_of(&conn).is_empty());
        std::fs::remove_dir_all(temp)?;
        Ok(())
    }

    fn codex_session_lines() -> Vec<String> {
        vec![
            r#"{"timestamp":"2026-10-20T11:00:00.000Z","ordinal":0,"type":"session_meta","payload":{"id":"sess-9","cwd":"/work/app"}}"#.into(),
            r#"{"timestamp":"2026-10-20T11:00:01.000Z","ordinal":1,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","id":"u1","content":[{"type":"text","text":"テストを回して"}]}}}"#.into(),
            r#"{"timestamp":"2026-10-20T11:00:02.000Z","ordinal":2,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"CommandExecution","id":"c1","command":["/bin/zsh","-lc","cargo test"],"cwd":"/work/app","exit_code":0,"aggregated_output":"出力の本文: TOKEN=xyz"}}}"#.into(),
            r#"{"timestamp":"2026-10-20T11:00:03.000Z","ordinal":3,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"FileChange","id":"f1","changes":{"/work/app/src/a.rs":{"type":"update","unified_diff":"-古い本文\n+新しい本文","move_path":null}}}}}"#.into(),
            r#"{"timestamp":"2026-10-20T11:00:04.000Z","ordinal":4,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"McpToolCall","id":"m1","server":"synclane","tool":"search_conversations","arguments":{"query":"認証"}}}}"#.into(),
            r#"{"timestamp":"2026-10-20T11:00:05.000Z","ordinal":5,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","id":"a1","content":[{"type":"text","text":"通りました"}]}}}"#.into(),
        ]
    }

    #[test]
    fn codex_tool_calls_are_saved_and_backfilled_once() -> Result<()> {
        let temp = tempfile_dir();
        let day = temp.join("2026").join("10").join("20");
        std::fs::create_dir_all(&day)?;
        std::fs::write(day.join("rollout-2026-10-20T20-00-00-xyz.jsonl"), codex_session_lines().join("\n") + "\n")?;
        let mut conn = db::open_memory()?;
        let report = collect_codex_incremental(&temp, &mut conn)?;
        assert_eq!(report.events, 2);
        assert_eq!(
            calls_of(&conn),
            [
                ("codex".into(), "shell".into(), "cargo test".into()),
                ("codex".into(), "FileChange".into(), "/work/app/src/a.rs (update)".into()),
                ("codex".into(), "mcp__synclane__search_conversations".into(), "認証".into()),
            ]
        );
        let dump: String = calls_of(&conn).into_iter().map(|(a, b, c)| format!("{a}{b}{c}")).collect();
        assert!(!dump.contains("TOKEN") && !dump.contains("古い本文"));
        let exit: Option<i64> = conn.query_row("SELECT exit_code FROM tool_calls WHERE name = 'shell'", [], |r| r.get(0))?;
        assert_eq!(exit, Some(0));
        // 増えない
        collect_codex_incremental(&temp, &mut conn)?;
        assert_eq!(calls_of(&conn).len(), 3);
        // さかのぼり(1 度だけ)
        conn.execute("DELETE FROM tool_calls", [])?;
        let shared = std::sync::Mutex::new(conn);
        assert_eq!(backfill_tool_calls_codex(&shared, &temp)?, 3);
        assert_eq!(backfill_tool_calls_codex(&shared, &temp)?, 0);
        assert_eq!(calls_of(&shared.lock().unwrap()).len(), 3);
        // 日付の指定(CLI): その日だけ
        let parsed = parse_codex_file(&day.join("rollout-2026-10-20T20-00-00-xyz.jsonl"), 0, Some("2026-10-19"))?;
        assert!(parsed.calls.is_empty());
        std::fs::remove_dir_all(temp)?;
        Ok(())
    }

    fn tempfile_dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "synclane-test-{}-{}",
            std::process::id(),
            TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn a_manual_title_survives_a_later_ai_title_in_the_log() -> Result<()> {
        let temp = tempfile_dir();
        let path = temp.join("session.jsonl");
        let mut file = File::create(&path)?;
        writeln!(file, r#"{{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"2026-10-13T01:00:00Z","cwd":"/tmp/project","message":{{"content":"調べる"}}}}"#)?;
        writeln!(file, r#"{{"type":"ai-title","sessionId":"s1","aiTitle":"最初の題名"}}"#)?;
        let mut conn = db::open_memory()?;
        import_claude(&temp, None, &mut conn)?;
        let id: i64 = conn.query_row("SELECT id FROM sessions", [], |r| r.get(0))?;
        let title = |conn: &Connection| -> Result<String> { Ok(conn.query_row("SELECT title FROM sessions", [], |r| r.get(0))?) };
        assert_eq!(title(&conn)?, "最初の題名");

        crate::titles::set_manual(&conn, id, "自分の題名")?;
        let mut file = std::fs::OpenOptions::new().append(true).open(&path)?;
        writeln!(file, r#"{{"type":"user","uuid":"u2","sessionId":"s1","timestamp":"2026-10-13T01:05:00Z","cwd":"/tmp/project","message":{{"content":"続き"}}}}"#)?;
        writeln!(file, r#"{{"type":"ai-title","sessionId":"s1","aiTitle":"Claude Code が付け直した題名"}}"#)?;
        import_claude(&temp, None, &mut conn)?;
        assert_eq!(title(&conn)?, "自分の題名");
        Ok(())
    }

    fn nonstop_log() -> String {
        [
            r#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"2026-10-19T01:00:00.000Z","cwd":"/tmp/p","message":{"content":"話題 A"}}"#,
            r#"{"type":"assistant","uuid":"a1","sessionId":"s1","timestamp":"2026-10-19T01:05:00.000Z","cwd":"/tmp/p","message":{"content":[{"type":"text","text":"答え A"}]}}"#,
            r#"{"type":"system","subtype":"compact_boundary","sessionId":"s1","timestamp":"2026-10-19T01:09:30.000Z","content":"Conversation compacted"}"#,
            r#"{"type":"user","uuid":"u2","sessionId":"s1","timestamp":"2026-10-19T01:10:00.000Z","cwd":"/tmp/p","message":{"content":"話題 B"}}"#,
        ]
        .join("\n")
            + "\n"
    }

    fn breaks(conn: &Connection) -> Vec<String> {
        let mut stmt = conn.prepare("SELECT key FROM settings WHERE key LIKE 'section_break:%' ORDER BY key").unwrap();
        stmt.query_map([], |r| r.get::<_, String>(0)).unwrap().map(|r| r.unwrap()).collect()
    }

    #[test]
    fn a_compaction_marker_in_the_log_is_recorded_by_both_import_paths() -> Result<()> {
        let temp = tempfile_dir();
        std::fs::write(temp.join("session.jsonl"), nonstop_log())?;
        // 増分の収集
        let mut conn = db::open_memory()?;
        collect_claude_incremental(&temp, &mut conn)?;
        let id: i64 = conn.query_row("SELECT id FROM sessions", [], |r| r.get(0))?;
        assert_eq!(breaks(&conn), vec![format!("section_break:{id}:2026-10-19T01:09:30.000Z")]);
        let starts = crate::sections::boundaries(&conn, id)?;
        assert_eq!(starts.len(), 2); // 休みなく続いていても、要約して続けた位置で、区切れる
        // 全体の取り込み
        let mut other = db::open_memory()?;
        import_claude(&temp, None, &mut other)?;
        assert_eq!(breaks(&other).len(), 1);
        // 同じ行を、もう一度取り込んでも、増えない
        import_claude(&temp, None, &mut other)?;
        assert_eq!(breaks(&other).len(), 1);
        Ok(())
    }

    #[test]
    fn the_backfill_records_markers_for_logs_imported_before_and_runs_only_once() -> Result<()> {
        let temp = tempfile_dir();
        std::fs::write(temp.join("session.jsonl"), nonstop_log())?;
        let mut conn = db::open_memory()?;
        collect_claude_incremental(&temp, &mut conn)?;
        // 印を取り込む前に、取り込まれた会話を、真似る
        conn.execute("DELETE FROM settings WHERE key LIKE 'section_break:%'", [])?;
        assert_eq!(backfill_claude_breaks(&temp, &mut conn)?, 1);
        assert_eq!(breaks(&conn).len(), 1);
        // 2 回目は、何もしない
        conn.execute("DELETE FROM settings WHERE key LIKE 'section_break:%'", [])?;
        assert_eq!(backfill_claude_breaks(&temp, &mut conn)?, 0);
        assert_eq!(breaks(&conn).len(), 0);
        Ok(())
    }
}
