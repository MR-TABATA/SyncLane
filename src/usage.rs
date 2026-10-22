//! トークン数(AI が使った量)の保存と集計。
//!
//! - 保存するのは数だけ(入力・出力・キャッシュの作成と読み出し・モデル名・時刻)。**会話の本文には触れない**。
//! - 費用(ドル)の目安と価格表は、作らない(契約の形で意味が変わり、価格表の保守だけが残るため)。
//! - 元の記録にトークン数があるのは、Claude Code(AI の返答すべての `usage`)と Codex(`token_count`)。
//!   Gemini CLI・Antigravity・Web 版の Gemini と ChatGPT は、記録に無い(含まれない)。
//! - データベースの版は変えない(表は、無ければ作る。`db.rs`)。

use anyhow::Result;
use rusqlite::{Connection, params};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;

/// 1 回の呼び出し(Claude Code は AI の返答 1 つ、Codex は `token_count` 1 つ)で使ったトークン数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenUsage {
    /// 元のツールでの ID(重複して数えないための鍵。`source_tool` と組)。Claude Code は返答の ID、Codex は「記録ファイル名:累計」
    pub source_usage_id: String,
    pub occurred_at: String,
    /// モデル名。分からなければ空
    pub model: String,
    /// キャッシュに当たらなかった入力
    pub input: i64,
    pub output: i64,
    /// キャッシュの作成に使った入力(Claude Code だけ)
    pub cache_creation: i64,
    /// キャッシュから読んだ入力
    pub cache_read: i64,
    /// 推論に使った出力(Codex だけ。`output` の内訳で、足し込まない)
    pub reasoning: i64,
}

fn number(value: Option<&Value>) -> i64 {
    value.and_then(Value::as_i64).unwrap_or(0).max(0)
}

/// Claude Code の、AI の返答の行から。`message.usage` が無い・すべて 0・合成された返答(`<synthetic>`)は、None。
/// 同じ返答が、内容のブロックごとに複数の行に出るので、保存のとき、同じ ID を 1 件にまとめる(`store`)。
pub fn claude_usage(value: &Value) -> Option<TokenUsage> {
    if value.get("type").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let message = value.get("message")?;
    let usage = message.get("usage")?;
    let model = message.get("model").and_then(Value::as_str).unwrap_or_default();
    if model == "<synthetic>" {
        return None;
    }
    let id = message.get("id").and_then(Value::as_str).or_else(|| value.get("uuid").and_then(Value::as_str))?;
    let found = TokenUsage {
        source_usage_id: id.to_string(),
        occurred_at: value.get("timestamp").and_then(Value::as_str)?.to_string(),
        model: model.to_string(),
        input: number(usage.get("input_tokens")),
        output: number(usage.get("output_tokens")),
        cache_creation: number(usage.get("cache_creation_input_tokens")),
        cache_read: number(usage.get("cache_read_input_tokens")),
        reasoning: 0,
    };
    (found.input + found.output + found.cache_creation + found.cache_read > 0).then_some(found)
}

/// Codex の `token_count` の行から。`payload.info.last_token_usage`(直前の 1 回の呼び出し)を使う。`info` が無い行(利用枠の情報だけ)は、None。
/// Codex の `input_tokens` は、キャッシュに当たった分を含むので、`cached_input_tokens` を引いて、`input` と `cache_read` に分ける。
/// 同じ累計の行が続けて出ることがあるので、鍵は「記録ファイル名:累計」にして、1 件にまとめる。`model` は、直前の `turn_context` の値(`codex_model`)。
pub fn codex_usage(value: &Value, file_stem: &str, model: &str) -> Option<TokenUsage> {
    if value.get("type").and_then(Value::as_str) != Some("event_msg") {
        return None;
    }
    let payload = value.get("payload")?;
    if payload.get("type").and_then(Value::as_str) != Some("token_count") {
        return None;
    }
    let info = payload.get("info").filter(|info| info.is_object())?;
    let last = info.get("last_token_usage")?;
    let total = info.get("total_token_usage").and_then(|t| t.get("total_tokens")).and_then(Value::as_i64)?;
    let input_all = number(last.get("input_tokens"));
    let cached = number(last.get("cached_input_tokens")).min(input_all);
    let found = TokenUsage {
        source_usage_id: format!("{file_stem}:{total}"),
        occurred_at: value.get("timestamp").and_then(Value::as_str)?.to_string(),
        model: model.to_string(),
        input: input_all - cached,
        output: number(last.get("output_tokens")),
        cache_creation: 0,
        cache_read: cached,
        reasoning: number(last.get("reasoning_output_tokens")),
    };
    (found.input + found.output + found.cache_read > 0).then_some(found)
}

/// Codex の `turn_context` の行から、モデル名。
pub fn codex_model(value: &Value) -> Option<String> {
    if value.get("type").and_then(Value::as_str) != Some("turn_context") {
        return None;
    }
    value.get("payload")?.get("model").and_then(Value::as_str).filter(|m| !m.is_empty()).map(str::to_owned)
}

/// 同じ ID の行を、1 つにまとめる(各項目の大きいほう。順番は、最初に出た順)。書き込みの前にまとめて、無駄な書き込みを減らす。
fn merged(usages: &[TokenUsage]) -> Vec<TokenUsage> {
    let mut index: HashMap<&str, usize> = HashMap::new();
    let mut out: Vec<TokenUsage> = Vec::new();
    for usage in usages {
        match index.get(usage.source_usage_id.as_str()) {
            Some(&i) => {
                let one = &mut out[i];
                one.input = one.input.max(usage.input);
                one.output = one.output.max(usage.output);
                one.cache_creation = one.cache_creation.max(usage.cache_creation);
                one.cache_read = one.cache_read.max(usage.cache_read);
                one.reasoning = one.reasoning.max(usage.reasoning);
                if one.model.is_empty() {
                    one.model = usage.model.clone();
                }
            }
            None => {
                index.insert(usage.source_usage_id.as_str(), out.len());
                out.push(usage.clone());
            }
        }
    }
    out
}

/// 保存する。同じ ID(同じ返答の、別の行・別の記録ファイル)は、各項目の大きいほうを残して、1 件にまとめる
/// (返答の途中の行は、出力が途中の数になっていることがあるため)。モデルが空のときは、その会話の、直前に分かっているモデルで補う。入れた(変わった)件数を返す。
pub fn store(conn: &Connection, session_id: i64, source_tool: &str, usages: &[TokenUsage]) -> Result<usize> {
    let mut changed = 0;
    for usage in &merged(usages) {
        let model = if usage.model.is_empty() {
            conn.query_row(
                "SELECT model FROM token_usage WHERE session_id = ?1 AND model != '' ORDER BY occurred_at DESC LIMIT 1",
                params![session_id],
                |r| r.get::<_, String>(0),
            )
            .unwrap_or_default()
        } else {
            usage.model.clone()
        };
        changed += conn.execute(
            "INSERT INTO token_usage(session_id, source_tool, source_usage_id, occurred_at, model, input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens, reasoning_tokens)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(source_tool, source_usage_id) DO UPDATE SET
                 input_tokens = MAX(input_tokens, excluded.input_tokens),
                 output_tokens = MAX(output_tokens, excluded.output_tokens),
                 cache_creation_tokens = MAX(cache_creation_tokens, excluded.cache_creation_tokens),
                 cache_read_tokens = MAX(cache_read_tokens, excluded.cache_read_tokens),
                 reasoning_tokens = MAX(reasoning_tokens, excluded.reasoning_tokens),
                 model = CASE WHEN token_usage.model = '' THEN excluded.model ELSE token_usage.model END
             WHERE input_tokens < excluded.input_tokens OR output_tokens < excluded.output_tokens
                OR cache_creation_tokens < excluded.cache_creation_tokens OR cache_read_tokens < excluded.cache_read_tokens
                OR reasoning_tokens < excluded.reasoning_tokens OR (token_usage.model = '' AND excluded.model != '')",
            params![session_id, source_tool, usage.source_usage_id, usage.occurred_at, model, usage.input, usage.output, usage.cache_creation, usage.cache_read, usage.reasoning],
        )?;
    }
    Ok(changed)
}

/// 集計の単位。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupBy {
    Day,
    Project,
    Model,
    Tool,
}

impl GroupBy {
    pub fn parse(name: &str) -> Option<GroupBy> {
        match name.trim() {
            "day" => Some(GroupBy::Day),
            "project" => Some(GroupBy::Project),
            "model" => Some(GroupBy::Model),
            "tool" => Some(GroupBy::Tool),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Totals {
    pub input: i64,
    pub output: i64,
    pub cache_creation: i64,
    pub cache_read: i64,
    /// 推論に使った出力(Codex)。`output` の内訳なので、合計には足さない
    pub reasoning: i64,
    /// 呼び出しの数(Claude Code は返答の数、Codex は `token_count` の数)
    pub calls: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UsageRow {
    /// 日別は日付(端末の時刻)、プロジェクト別は場所、モデル別はモデル名(不明は空)、ツール別はツール名
    pub key: String,
    /// 表示用の名前(プロジェクトは、場所の末尾の名前。同名は、親のフォルダを足す)
    pub label: String,
    #[serde(flatten)]
    pub totals: Totals,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub total: Totals,
    pub rows: Vec<UsageRow>,
    /// 件数の上限のために、行を省いた
    pub truncated: bool,
}

/// 表の列から、集計を読む。
fn totals_of(row: &rusqlite::Row<'_>, at: usize) -> rusqlite::Result<Totals> {
    Ok(Totals {
        input: row.get(at)?,
        output: row.get(at + 1)?,
        cache_creation: row.get(at + 2)?,
        cache_read: row.get(at + 3)?,
        reasoning: row.get(at + 4)?,
        calls: row.get(at + 5)?,
    })
}

const SUMS: &str = "COALESCE(SUM(u.input_tokens), 0), COALESCE(SUM(u.output_tokens), 0), COALESCE(SUM(u.cache_creation_tokens), 0), COALESCE(SUM(u.cache_read_tokens), 0), COALESCE(SUM(u.reasoning_tokens), 0), COUNT(*)";

/// トークン数を、期間と単位で集計する。`date_from` と `date_to` は、`timeline` と同じ形(UTC の時刻。`date_to` は含まない)。
/// `utc_offset_minutes` は、日別の日付を、端末の時刻で切るための、UTC からのずれ。行は、日別は新しい日が先、ほかは、新しいトークン(入力 + 出力 + キャッシュ作成)が多い順。
pub fn summarize(conn: &Connection, date_from: Option<&str>, date_to: Option<&str>, group: GroupBy, utc_offset_minutes: i32, limit: usize) -> Result<Summary> {
    if !crate::db::has_token_usage(conn) {
        return Ok(Summary { total: Totals::default(), rows: Vec::new(), truncated: false });
    }
    let mut conditions: Vec<&str> = Vec::new();
    let mut values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(from) = date_from {
        conditions.push("u.occurred_at >= ?");
        values.push(Box::new(from.to_owned()));
    }
    if let Some(to) = date_to {
        conditions.push("u.occurred_at < ?");
        values.push(Box::new(to.to_owned()));
    }
    let filter = if conditions.is_empty() { String::new() } else { format!(" WHERE {}", conditions.join(" AND ")) };

    let total = conn.query_row(&format!("SELECT {SUMS} FROM token_usage u{filter}"), rusqlite::params_from_iter(values.iter()), |r| totals_of(r, 0))?;

    // 日別は、パラメータで時差を渡す(`date(…, '+540 minutes')`)。値の並びは、SELECT 側が先、WHERE 側があと
    let (key_expr, join) = match group {
        GroupBy::Day => ("date(substr(u.occurred_at, 1, 19), printf('%+d minutes', ?))", ""),
        GroupBy::Project => ("s.project_path", " JOIN sessions s ON s.id = u.session_id"),
        GroupBy::Model => ("u.model", ""),
        GroupBy::Tool => ("u.source_tool", ""),
    };
    let order = if group == GroupBy::Day { "key DESC" } else { "(SUM(u.input_tokens) + SUM(u.output_tokens) + SUM(u.cache_creation_tokens)) DESC, key" };
    let sql = format!("SELECT {key_expr} AS key, {SUMS} FROM token_usage u{join}{filter} GROUP BY key ORDER BY {order} LIMIT ?");
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if group == GroupBy::Day {
        params.push(Box::new(i64::from(utc_offset_minutes)));
    }
    params.extend(values);
    params.push(Box::new(limit as i64 + 1));
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt
        .query_map(rusqlite::params_from_iter(params.iter()), |r| Ok((r.get::<_, String>(0)?, totals_of(r, 1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let truncated = rows.len() > limit;
    rows.truncate(limit);

    let names: HashMap<String, String> = if group == GroupBy::Project {
        crate::projects::display_names(&rows.iter().map(|(key, _)| key.as_str()).collect::<Vec<_>>())
    } else {
        HashMap::new()
    };
    let rows = rows
        .into_iter()
        .map(|(key, totals)| {
            let label = names.get(&key).cloned().unwrap_or_else(|| key.clone());
            UsageRow { key, label, totals }
        })
        .collect();
    Ok(Summary { total, rows, truncated })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use serde_json::json;

    fn assistant(id: &str, at: &str, model: &str, usage: Value) -> Value {
        json!({ "type": "assistant", "timestamp": at, "uuid": "u", "message": { "id": id, "model": model, "role": "assistant", "content": [], "usage": usage } })
    }

    #[test]
    fn claude_usage_reads_the_numbers_and_skips_synthetic_and_empty_replies() {
        let usage = json!({ "input_tokens": 10, "output_tokens": 20, "cache_creation_input_tokens": 300, "cache_read_input_tokens": 4000, "service_tier": "standard" });
        let found = claude_usage(&assistant("msg_1", "2026-10-20T01:00:00.000Z", "claude-sonnet-5", usage.clone())).unwrap();
        assert_eq!(
            (found.source_usage_id.as_str(), found.model.as_str(), found.input, found.output, found.cache_creation, found.cache_read, found.reasoning),
            ("msg_1", "claude-sonnet-5", 10, 20, 300, 4000, 0)
        );
        // 合成された返答・すべて 0・usage が無い・返答でない行は、数えない
        assert!(claude_usage(&assistant("msg_2", "2026-10-20T01:00:00.000Z", "<synthetic>", usage)).is_none());
        assert!(claude_usage(&assistant("msg_3", "2026-10-20T01:00:00.000Z", "claude-sonnet-5", json!({ "input_tokens": 0, "output_tokens": 0 }))).is_none());
        assert!(claude_usage(&json!({ "type": "assistant", "timestamp": "2026-10-20T01:00:00Z", "message": { "id": "x" } })).is_none());
        assert!(claude_usage(&json!({ "type": "user", "timestamp": "2026-10-20T01:00:00Z", "message": { "usage": { "input_tokens": 5 } } })).is_none());
        // 返答の ID が無ければ、行の uuid を鍵にする
        let no_id = json!({ "type": "assistant", "uuid": "line-7", "timestamp": "2026-10-20T01:00:00Z", "message": { "model": "m", "usage": { "output_tokens": 3 } } });
        assert_eq!(claude_usage(&no_id).unwrap().source_usage_id, "line-7");
    }

    fn token_count(at: &str, total: i64, last: Value) -> Value {
        json!({ "timestamp": at, "type": "event_msg", "payload": { "type": "token_count", "info": { "total_token_usage": { "total_tokens": total }, "last_token_usage": last } } })
    }

    #[test]
    fn codex_usage_splits_the_cached_input_and_ignores_rows_without_info() {
        let row = token_count("2026-10-20T02:00:00.000Z", 5000, json!({ "input_tokens": 1000, "cached_input_tokens": 800, "output_tokens": 50, "reasoning_output_tokens": 20, "total_tokens": 1050 }));
        let found = codex_usage(&row, "rollout-x", "gpt-5").unwrap();
        assert_eq!((found.source_usage_id.as_str(), found.model.as_str()), ("rollout-x:5000", "gpt-5"));
        // 入力は、キャッシュに当たった分を引く。推論は、出力の内訳
        assert_eq!((found.input, found.cache_read, found.output, found.reasoning, found.cache_creation), (200, 800, 50, 20, 0));
        // 利用枠の情報だけの行(info が null)
        let limits_only = json!({ "timestamp": "2026-10-20T02:00:00Z", "type": "event_msg", "payload": { "type": "token_count", "info": null, "rate_limits": {} } });
        assert!(codex_usage(&limits_only, "rollout-x", "").is_none());
        assert!(codex_usage(&json!({ "type": "event_msg", "payload": { "type": "agent_message" } }), "rollout-x", "").is_none());
        assert_eq!(codex_model(&json!({ "type": "turn_context", "payload": { "model": "gpt-5" } })).as_deref(), Some("gpt-5"));
        assert_eq!(codex_model(&json!({ "type": "turn_context", "payload": {} })), None);
    }

    fn two_sessions() -> Connection {
        let conn = db::open_memory().unwrap();
        for (i, (tool, project)) in [("claude_code", "/w/SyncLane"), ("codex", "/w/SyncLane"), ("claude_code", "/a/app"), ("claude_code", "/b/app")].iter().enumerate() {
            conn.execute("INSERT INTO sessions(source_tool, project_path, source_file, source_hash) VALUES (?1, ?2, ?3, ?3)", params![tool, project, format!("f{i}")]).unwrap();
        }
        conn
    }

    fn usage(id: &str, at: &str, model: &str, input: i64, output: i64) -> TokenUsage {
        TokenUsage { source_usage_id: id.into(), occurred_at: at.into(), model: model.into(), input, output, cache_creation: 0, cache_read: 0, reasoning: 0 }
    }

    #[test]
    fn the_same_reply_is_counted_once_with_the_larger_numbers_and_a_missing_model_is_filled() {
        let conn = two_sessions();
        // 返答の途中の行(出力が途中の数)→ 最後の行。順番が逆でも、大きいほうが残る
        assert_eq!(store(&conn, 1, "claude_code", &[usage("m1", "2026-10-20T01:00:00Z", "claude-sonnet-5", 100, 1)]).unwrap(), 1);
        assert_eq!(store(&conn, 1, "claude_code", &[usage("m1", "2026-10-20T01:00:01Z", "claude-sonnet-5", 100, 250)]).unwrap(), 1);
        assert_eq!(store(&conn, 1, "claude_code", &[usage("m1", "2026-10-20T01:00:02Z", "claude-sonnet-5", 100, 5)]).unwrap(), 0, "小さい数では、変わらない");
        let (calls, output): (i64, i64) = conn.query_row("SELECT COUNT(*), SUM(output_tokens) FROM token_usage", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((calls, output), (1, 250));
        // モデルが空の呼び出しは、その会話の、分かっているモデルで補う(Codex の追記だけを読んだとき)
        store(&conn, 1, "claude_code", &[usage("m2", "2026-10-20T02:00:00Z", "", 10, 10)]).unwrap();
        let model: String = conn.query_row("SELECT model FROM token_usage WHERE source_usage_id = 'm2'", [], |r| r.get(0)).unwrap();
        assert_eq!(model, "claude-sonnet-5");
    }

    #[test]
    fn totals_are_grouped_by_day_in_the_local_time_and_by_project_model_and_tool() {
        let conn = two_sessions();
        store(&conn, 1, "claude_code", &[usage("a", "2026-10-20T20:00:00Z", "sonnet", 100, 10), usage("b", "2026-10-21T01:00:00Z", "opus", 200, 20)]).unwrap();
        store(&conn, 2, "codex", &[TokenUsage { cache_read: 800, reasoning: 5, ..usage("c", "2026-10-21T02:00:00Z", "gpt-5", 50, 5) }]).unwrap();
        store(&conn, 3, "claude_code", &[usage("d", "2026-10-21T03:00:00Z", "sonnet", 1000, 100)]).unwrap();
        store(&conn, 4, "claude_code", &[usage("e", "2026-10-21T04:00:00Z", "sonnet", 7, 3)]).unwrap();

        let all = summarize(&conn, None, None, GroupBy::Day, 0, 50).unwrap();
        assert_eq!((all.total.input, all.total.output, all.total.cache_read, all.total.reasoning, all.total.calls), (1357, 138, 800, 5, 5));
        // UTC では 2 日に分かれ、新しい日が先
        assert_eq!(all.rows.iter().map(|r| (r.key.as_str(), r.totals.calls)).collect::<Vec<_>>(), [("2026-10-21", 4), ("2026-10-20", 1)]);
        // 日本時間(+9 時間)では、10/20 20:00Z は 10/21 05:00 なので、全部が 10/21
        let jst = summarize(&conn, None, None, GroupBy::Day, 540, 50).unwrap();
        assert_eq!(jst.rows.iter().map(|r| (r.key.as_str(), r.totals.calls)).collect::<Vec<_>>(), [("2026-10-21", 5)]);

        // プロジェクト別: 新しいトークン(入力 + 出力 + キャッシュ作成)の多い順。同名(app)は、親のフォルダつきの名前
        let by_project = summarize(&conn, None, None, GroupBy::Project, 0, 50).unwrap();
        assert_eq!(by_project.rows.iter().map(|r| (r.label.as_str(), r.totals.input)).collect::<Vec<_>>(), [("a/app", 1000), ("SyncLane", 350), ("b/app", 7)]);
        let by_model = summarize(&conn, None, None, GroupBy::Model, 0, 50).unwrap();
        assert_eq!(by_model.rows[0].key, "sonnet");
        let by_tool = summarize(&conn, None, None, GroupBy::Tool, 0, 50).unwrap();
        assert_eq!(by_tool.rows.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(), ["claude_code", "codex"]);
    }

    #[test]
    fn the_period_and_the_row_limit_are_applied_and_the_total_covers_the_whole_period() {
        let conn = two_sessions();
        store(&conn, 1, "claude_code", &[usage("a", "2026-10-20T01:00:00Z", "m", 1, 1), usage("b", "2026-10-21T01:00:00Z", "m", 2, 2), usage("c", "2026-10-22T01:00:00Z", "m", 4, 4)]).unwrap();
        // 期間: from を含み、to を含まない
        let period = summarize(&conn, Some("2026-10-21T00:00:00"), Some("2026-10-22T00:00:00"), GroupBy::Day, 0, 50).unwrap();
        assert_eq!((period.total.input, period.total.calls, period.rows.len()), (2, 1, 1));
        // 行の上限: 省いたことが分かる。合計は、省いた行も含む
        let limited = summarize(&conn, None, None, GroupBy::Day, 0, 2).unwrap();
        assert!(limited.truncated);
        assert_eq!((limited.rows.len(), limited.total.calls), (2, 3));
        assert_eq!(limited.rows[0].key, "2026-10-22");
        assert_eq!(GroupBy::parse("project"), Some(GroupBy::Project));
        assert_eq!(GroupBy::parse("week"), None);
    }
}
