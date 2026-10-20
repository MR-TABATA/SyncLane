//! ツールの呼び出し(AI が実行したコマンド・触ったファイル・開いた URL)の保存。
//!
//! - 保存するのは「何を呼んだか」だけ。**実行結果と、ファイルの中身(編集の本文)は、保存しない**(大きく、秘密も入りやすい)。
//! - 全文検索の索引・MCP・サーバーへの送信の対象にしない。書き出し(Markdown)のときだけ使う(コマンドに秘密が入りうるため)。
//! - データベースの版は変えない(表は、無ければ作る。`db.rs`)。

use anyhow::Result;
use rusqlite::{Connection, params};
use serde_json::Value;

/// 1 件の内容(コマンド・パスなど)の長さの上限(文字数)。超えた分は切って、末尾に「…」を付ける。
pub const DETAIL_MAX_CHARS: usize = 1000;

/// 入力の中から、内容として取る項目(前にあるほうを優先)。Edit・Write の本文(`old_string`・`content` など)は、含めない。
const DETAIL_KEYS: [&str; 8] = ["command", "file_path", "notebook_path", "url", "query", "pattern", "path", "description"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    /// 元のツールでの呼び出しの ID(重複して保存しないための鍵。`source_tool` と組)
    pub source_call_id: String,
    pub occurred_at: String,
    /// ツール名(`Bash`・`Read`・`Edit`・`shell`・`mcp__…` など)
    pub name: String,
    /// 実行したコマンド・触ったファイル・URL・検索語。無いときは空
    pub detail: String,
    /// 終了コード(記録にあるときだけ。Codex のコマンド)
    pub exit_code: Option<i64>,
}

/// 長さの上限で切る。
fn clip(text: &str) -> String {
    if text.chars().count() <= DETAIL_MAX_CHARS {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(DETAIL_MAX_CHARS).collect();
    cut.push('…');
    cut
}

/// 入力(JSON のオブジェクト)から、内容を取る。
fn detail_of(input: Option<&Value>) -> String {
    let Some(object) = input.and_then(Value::as_object) else { return String::new() };
    for key in DETAIL_KEYS {
        if let Some(text) = object.get(key).and_then(Value::as_str) {
            let text = text.trim();
            if !text.is_empty() {
                return clip(text);
            }
        }
    }
    String::new()
}

/// Claude Code の記録 1 行から、ツールの呼び出し(`assistant` の `tool_use`)を取る。
pub fn claude_calls(value: &Value) -> Vec<ToolCall> {
    if value.get("type").and_then(Value::as_str) != Some("assistant") {
        return Vec::new();
    }
    let Some(at) = value.get("timestamp").and_then(Value::as_str) else { return Vec::new() };
    let Some(blocks) = value.pointer("/message/content").and_then(Value::as_array) else { return Vec::new() };
    blocks
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
        .filter_map(|block| {
            Some(ToolCall {
                source_call_id: block.get("id")?.as_str()?.to_string(),
                occurred_at: at.to_string(),
                name: block.get("name")?.as_str()?.to_string(),
                detail: detail_of(block.get("input")),
                exit_code: None,
            })
        })
        .collect()
}

/// `["/bin/zsh", "-lc", "git status"]` のような、シェルを呼ぶ形から、中のコマンドを取り出す。
fn shell_command(args: &[&str]) -> String {
    if args.len() >= 3 && matches!(args[1], "-lc" | "-c") {
        args[2..].join(" ")
    } else {
        args.join(" ")
    }
}

/// Codex の記録 1 行から、ツールの呼び出し(`item_completed` の `CommandExecution`・`FileChange`・`McpToolCall`)を取る。
pub fn codex_call(value: &Value) -> Option<ToolCall> {
    if value.get("type")?.as_str()? != "event_msg" {
        return None;
    }
    let payload = value.get("payload")?;
    if payload.get("type")?.as_str()? != "item_completed" {
        return None;
    }
    let item = payload.get("item")?;
    let at = value.get("timestamp")?.as_str()?.to_string();
    let id = item.get("id")?.as_str()?.to_string();
    let call = |name: String, detail: String, exit_code: Option<i64>| ToolCall { source_call_id: id.clone(), occurred_at: at.clone(), name, detail, exit_code };
    match item.get("type")?.as_str()? {
        "CommandExecution" => {
            let args: Vec<&str> = item.get("command")?.as_array()?.iter().filter_map(Value::as_str).collect();
            Some(call("shell".into(), clip(shell_command(&args).trim()), item.get("exit_code").and_then(Value::as_i64)))
        }
        // 変更したファイルのパスだけ(`changes` は、パス → {type, unified_diff, move_path})。差分の本文は保存しない
        "FileChange" => {
            let changes = item.get("changes")?.as_object()?;
            let detail = changes
                .iter()
                .map(|(path, change)| match change.get("type").and_then(Value::as_str) {
                    Some(kind) => format!("{path} ({kind})"),
                    None => path.clone(),
                })
                .collect::<Vec<_>>()
                .join(", ");
            Some(call("FileChange".into(), clip(&detail), None))
        }
        "McpToolCall" => {
            let name = format!("mcp__{}__{}", item.get("server")?.as_str()?, item.get("tool")?.as_str()?);
            Some(call(name, detail_of(item.get("arguments")), None))
        }
        _ => None,
    }
}

/// 呼び出しを保存する(同じ呼び出しは、重複して入れない)。入れた件数を返す。
pub fn store(conn: &Connection, session_id: i64, source_tool: &str, calls: &[ToolCall]) -> Result<usize> {
    let mut added = 0;
    for call in calls {
        added += conn.execute(
            "INSERT OR IGNORE INTO tool_calls(session_id, source_tool, source_call_id, occurred_at, name, detail, exit_code)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![session_id, source_tool, call.source_call_id, call.occurred_at, call.name, call.detail, call.exit_code],
        )?;
    }
    Ok(added)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn assistant(blocks: Value) -> Value {
        json!({ "type": "assistant", "timestamp": "2026-10-20T01:00:00Z", "message": { "content": blocks } })
    }

    #[test]
    fn claude_calls_keep_what_was_called_and_not_the_contents() {
        let line = assistant(json!([
            { "type": "text", "text": "調べます" },
            { "type": "tool_use", "id": "t1", "name": "Bash", "input": { "command": "npm run build", "description": "ビルド", "timeout": 60000 } },
            { "type": "tool_use", "id": "t2", "name": "Read", "input": { "file_path": "/work/src/a.ts", "limit": 20 } },
            { "type": "tool_use", "id": "t3", "name": "Edit", "input": { "file_path": "/work/src/a.ts", "old_string": "秘密の古い本文", "new_string": "秘密の新しい本文" } },
            { "type": "tool_use", "id": "t4", "name": "Write", "input": { "file_path": "/work/b.md", "content": "ファイルの中身" } },
            { "type": "tool_use", "id": "t5", "name": "WebFetch", "input": { "url": "https://example.com/", "prompt": "要約して" } },
            { "type": "tool_use", "id": "t6", "name": "WebSearch", "input": { "query": "rust fts5", "mode": "standard" } },
            { "type": "tool_use", "id": "t7", "name": "Grep", "input": { "pattern": "TODO", "path": "src" } },
            { "type": "tool_use", "id": "t8", "name": "mcp__synclane__get_session", "input": { "session_id": 3, "window": 5 } },
        ]));
        let calls = claude_calls(&line);
        let pairs: Vec<(&str, &str)> = calls.iter().map(|c| (c.name.as_str(), c.detail.as_str())).collect();
        assert_eq!(
            pairs,
            [
                ("Bash", "npm run build"),
                ("Read", "/work/src/a.ts"),
                ("Edit", "/work/src/a.ts"),
                ("Write", "/work/b.md"),
                ("WebFetch", "https://example.com/"),
                ("WebSearch", "rust fts5"),
                ("Grep", "TODO"),
                ("mcp__synclane__get_session", ""),
            ]
        );
        // 編集の本文や、ファイルの中身は、どこにも入らない
        let all: String = calls.iter().map(|c| format!("{}{}", c.name, c.detail)).collect();
        assert!(!all.contains("秘密") && !all.contains("ファイルの中身"));
        assert_eq!(calls[0].source_call_id, "t1");
        assert_eq!(calls[0].occurred_at, "2026-10-20T01:00:00Z");
        assert!(calls.iter().all(|c| c.exit_code.is_none()));
    }

    #[test]
    fn claude_calls_ignore_other_lines() {
        assert!(claude_calls(&json!({ "type": "user", "timestamp": "2026-10-20T01:00:00Z", "message": { "content": "こんにちは" } })).is_empty());
        assert!(claude_calls(&assistant(json!([{ "type": "text", "text": "答え" }]))).is_empty());
        assert!(claude_calls(&json!({ "type": "assistant", "message": { "content": [{ "type": "tool_use", "id": "x", "name": "Bash", "input": {} }] } })).is_empty()); // 時刻が無い
        assert!(claude_calls(&json!({ "type": "assistant", "timestamp": "t", "message": { "content": "文字列" } })).is_empty());
    }

    #[test]
    fn a_long_command_is_cut_with_a_mark() {
        let long = "あ".repeat(DETAIL_MAX_CHARS + 500);
        let calls = claude_calls(&assistant(json!([{ "type": "tool_use", "id": "t", "name": "Bash", "input": { "command": long } }])));
        assert_eq!(calls[0].detail.chars().count(), DETAIL_MAX_CHARS + 1);
        assert!(calls[0].detail.ends_with('…'));
        let exact = "い".repeat(DETAIL_MAX_CHARS);
        let calls = claude_calls(&assistant(json!([{ "type": "tool_use", "id": "t", "name": "Bash", "input": { "command": exact } }])));
        assert!(!calls[0].detail.ends_with('…'));
    }

    fn codex(item: Value) -> Value {
        json!({ "timestamp": "2026-10-20T02:00:00.000Z", "ordinal": 3, "type": "event_msg", "payload": { "type": "item_completed", "item": item } })
    }

    #[test]
    fn codex_commands_files_and_mcp_calls_are_kept_without_output_or_diffs() {
        let command = codex_call(&codex(json!({ "type": "CommandExecution", "id": "c1", "command": ["/bin/zsh", "-lc", "git status --short"], "cwd": "/work", "exit_code": 1, "aggregated_output": "出力の本文", "stdout": "出力" }))).unwrap();
        assert_eq!((command.name.as_str(), command.detail.as_str(), command.exit_code), ("shell", "git status --short", Some(1)));
        assert_eq!(command.source_call_id, "c1");
        assert_eq!(command.occurred_at, "2026-10-20T02:00:00.000Z");
        // シェルを通さない形は、そのまま並べる
        let plain = codex_call(&codex(json!({ "type": "CommandExecution", "id": "c2", "command": ["ls", "-la"], "exit_code": 0 }))).unwrap();
        assert_eq!(plain.detail, "ls -la");

        let change = codex_call(&codex(json!({ "type": "FileChange", "id": "f1", "changes": { "/work/a.rs": { "type": "update", "unified_diff": "-秘密\n+秘密2", "move_path": null }, "/work/b.rs": { "type": "add" } } }))).unwrap();
        assert_eq!(change.name, "FileChange");
        assert!(change.detail.contains("/work/a.rs (update)") && change.detail.contains("/work/b.rs (add)"));
        assert!(!change.detail.contains("秘密"));

        let mcp = codex_call(&codex(json!({ "type": "McpToolCall", "id": "m1", "server": "synclane", "tool": "search_conversations", "arguments": { "query": "認証", "limit": 5 } }))).unwrap();
        assert_eq!((mcp.name.as_str(), mcp.detail.as_str()), ("mcp__synclane__search_conversations", "認証"));
    }

    #[test]
    fn codex_lines_that_are_not_calls_are_ignored() {
        assert!(codex_call(&codex(json!({ "type": "AgentMessage", "id": "a", "content": [] }))).is_none());
        assert!(codex_call(&codex(json!({ "type": "Reasoning", "id": "r" }))).is_none());
        assert!(codex_call(&json!({ "type": "response_item", "timestamp": "t", "payload": {} })).is_none());
        assert!(codex_call(&codex(json!({ "type": "CommandExecution", "command": ["ls"] }))).is_none()); // ID が無い
    }
}
