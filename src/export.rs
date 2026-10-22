//! 会話の書き出し(Markdown)。別の AI に貼って続きを頼む・記録として残す、といった用途。
//!
//! 「集める部分」(`section_group`)と「Markdown にする部分」(`render`)を分けてある。形式を増やすときは、後者だけを足す。
//! - 書き出すのは、発言(あなたの入力・AI のテキストの応答)と、ツールの呼び出し(実行したコマンド・触ったファイル。`tools.rs`)。
//!   **ツールの実行結果と AI の思考は、保存していないので含まれない。**
//! - 秘密に見えるもの(API キー・トークン・パスワードなど)は、既定で伏せる(サーバーに送るときと同じ処理。`sync/mask.rs`)。
//!   `Secrets::Keep` なら、そのまま書き出す。伏せた件数は、返り値に入る。
//! - 先頭に、別の AI への依頼文を付けられる(`Prompt`)。貼り付けるだけで頼めるようにするため。SyncLane は AI を呼ばない(コピーするだけ)。

use anyhow::Result;
use rusqlite::Connection;

use crate::sections;
use crate::sync::mask::{self, Lists};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Secrets {
    /// 秘密に見えるものを伏せる(既定)
    Mask,
    /// そのまま書き出す
    Keep,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Ja,
    En,
}

/// 書き出しの先頭に付ける、別の AI への依頼文。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prompt {
    /// 付けない(記録として残す・そのまま貼る)
    None,
    /// 要点を、決まったこと・決まっていないこと・次にやることに分けて、まとめてもらう
    Summarize,
    /// この続きから、作業してもらう
    Continue,
    /// 別の視点から、評価してもらう(セカンドオピニオン)
    Review,
}

impl Prompt {
    /// 画面から渡される名前から。空・`none` は、付けない。知らない名前は None(呼び出し側が、断る)。
    pub fn parse(name: &str) -> Option<Prompt> {
        match name.trim() {
            "" | "none" => Some(Prompt::None),
            "summarize" => Some(Prompt::Summarize),
            "continue" => Some(Prompt::Continue),
            "review" => Some(Prompt::Review),
            _ => None,
        }
    }

    fn text(self, lang: Lang) -> Option<&'static str> {
        match (self, lang) {
            (Prompt::None, _) => None,
            (Prompt::Summarize, Lang::Ja) => Some("以下は、AI との会話の記録です。要点を、「決まったこと」「まだ決まっていないこと」「次にやること」に分けて、簡潔にまとめてください。記録にないことは、書き足さないでください。"),
            (Prompt::Continue, Lang::Ja) => Some("以下は、これまでの作業の記録です。この続きから作業してください。最初に、いまの状況と、残っていることを短く確認してから、始めてください。"),
            (Prompt::Review, Lang::Ja) => Some("以下は、別の AI との会話の記録です。別の視点から評価してください。見落としている点、前提が弱い点、ほかの選び方があればそれも、理由つきで挙げてください。"),
            (Prompt::Summarize, Lang::En) => Some("Below is a record of a conversation with an AI. Summarize it briefly under three headings: \"Decided\", \"Still open\" and \"Next steps\". Do not add anything that is not in the record."),
            (Prompt::Continue, Lang::En) => Some("Below is a record of the work so far. Continue the work from here. First, briefly confirm the current state and what remains, then start."),
            (Prompt::Review, Lang::En) => Some("Below is a record of a conversation with another AI. Evaluate it from a different point of view. Point out anything overlooked, weak assumptions, and other options worth considering, with reasons."),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Options {
    pub secrets: Secrets,
    pub lang: Lang,
    /// 先頭に付ける依頼文
    pub prompt: Prompt,
    /// 時刻を表示するときの、UTC からのずれ(分)。画面は、使っている端末のずれを渡す
    pub utc_offset_minutes: i32,
}

impl Default for Options {
    fn default() -> Self {
        Options { secrets: Secrets::Mask, lang: Lang::Ja, prompt: Prompt::None, utc_offset_minutes: 0 }
    }
}

/// 書き出す 1 つの区切りの中身(時刻順)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Message { at: String, role: String, content: String },
    Call { at: String, name: String, detail: String, exit_code: Option<i64> },
}

impl Item {
    fn at(&self) -> &str {
        match self {
            Item::Message { at, .. } | Item::Call { at, .. } => at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub title: String,
    pub source_tool: String,
    pub project_path: String,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Exported {
    pub markdown: String,
    /// 伏せた件数(`Secrets::Keep` のときは 0)
    pub masked: usize,
    pub messages: usize,
    pub calls: usize,
}

/// ある発言が属する区切りを、書き出す形に集める。
pub fn section_group(conn: &Connection, event_id: i64) -> Result<Group> {
    let span = sections::span(conn, event_id)?;
    let (source_tool, project_path, session_title): (String, String, Option<String>) = conn.query_row(
        "SELECT source_tool, project_path, title FROM sessions WHERE id = ?1",
        [span.session_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let title = sections::title_of(conn, span.session_id, span.start.event_id)?
        .or(session_title)
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| project_path.clone());

    // 区切りの範囲に入る発言
    let mut stmt = conn.prepare(
        "SELECT occurred_at, role, content FROM events
         WHERE session_id = ?1
           AND (occurred_at > ?2 OR (occurred_at = ?2 AND id >= ?3))
           AND (?4 IS NULL OR occurred_at < ?4 OR (occurred_at = ?4 AND id < ?5))
         ORDER BY occurred_at ASC, id ASC",
    )?;
    let messages = stmt
        .query_map(
            rusqlite::params![span.session_id, span.start.occurred_at, span.start.event_id, span.next.as_ref().map(|b| b.occurred_at.clone()), span.next.as_ref().map(|b| b.event_id)],
            |r| Ok(Item::Message { at: r.get(0)?, role: r.get(1)?, content: r.get(2)? }),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    // 区切りの範囲に入るツールの呼び出し。時刻の書き方が記録によって違う(`…:05Z` と `…:05.000Z`)ので、文字列ではなく、時刻として比べる
    let start_time = crate::sync::parse_time(&span.start.occurred_at);
    let next_time = span.next.as_ref().and_then(|b| crate::sync::parse_time(&b.occurred_at));
    let mut items = messages;
    if crate::db::has_tool_calls(conn) {
        let mut stmt = conn.prepare("SELECT occurred_at, name, detail, exit_code FROM tool_calls WHERE session_id = ?1 ORDER BY occurred_at ASC, id ASC")?;
        let calls = stmt
            .query_map([span.session_id], |r| Ok(Item::Call { at: r.get(0)?, name: r.get(1)?, detail: r.get(2)?, exit_code: r.get(3)? }))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for call in calls {
            let Some(at) = crate::sync::parse_time(call.at()) else { continue };
            if start_time.is_some_and(|start| at < start) || next_time.is_some_and(|next| at >= next) {
                continue;
            }
            items.push(call);
        }
    }
    // 時刻順に並べる(同じ時刻なら、発言を先に)。並べ替えは安定なので、同じ時刻の中は、取り出した順のまま
    items.sort_by_key(|item| (crate::sync::parse_time(item.at()).map(|t| t.unix_timestamp_nanos()).unwrap_or(0), matches!(item, Item::Call { .. })));
    Ok(Group { title, source_tool, project_path, items })
}

/// 区切りを Markdown にする(秘密の扱い・言語・時刻のずれは、`options`)。
pub fn section_markdown(conn: &Connection, event_id: i64, options: &Options) -> Result<Exported> {
    let group = section_group(conn, event_id)?;
    Ok(render(&group, options, &Lists::load_beside(conn)))
}

struct Labels {
    when: &'static str,
    source: &'static str,
    project: &'static str,
    counts: &'static str,
    you: &'static str,
    ai: &'static str,
    ran: &'static str,
    exit_code: &'static str,
    not_included: &'static str,
    masked_note: fn(usize) -> String,
}

fn labels(lang: Lang) -> Labels {
    match lang {
        Lang::Ja => Labels {
            when: "日時",
            source: "取り込み元",
            project: "プロジェクト",
            counts: "件数",
            you: "あなた",
            ai: "AI",
            ran: "実行したこと",
            exit_code: "終了コード",
            not_included: "※ ツールの実行結果と、AI の思考は、保存していないので含まれません。",
            masked_note: |n| format!("※ 秘密に見えるものを {n} 件、伏せました。"),
        },
        Lang::En => Labels {
            when: "When",
            source: "Source",
            project: "Project",
            counts: "Count",
            you: "You",
            ai: "AI",
            ran: "Ran",
            exit_code: "exit code",
            not_included: "* Tool output and the AI's reasoning are not saved, so they are not included.",
            masked_note: |n| format!("* {n} things that look like secrets were masked."),
        },
    }
}

fn tool_label(source: &str) -> &str {
    match source {
        "claude_code" => "Claude Code",
        "gemini_cli" => "Gemini CLI",
        "gemini_web" => "Gemini",
        "antigravity_cli" => "Antigravity",
        "chatgpt_web" => "ChatGPT",
        "codex" => "Codex",
        other => other,
    }
}

/// `2026-10-20 10:00` の形。時刻として読めなければ、そのまま。
fn format_time(at: &str, offset_minutes: i32) -> String {
    let Some(time) = crate::sync::parse_time(at) else { return at.to_string() };
    let offset = time::UtcOffset::from_whole_seconds(offset_minutes * 60).unwrap_or(time::UtcOffset::UTC);
    let Ok(format) = time::format_description::parse_borrowed::<2>("[year]-[month]-[day] [hour]:[minute]") else { return at.to_string() };
    time.to_offset(offset).format(&format).unwrap_or_else(|_| at.to_string())
}

fn offset_label(offset_minutes: i32) -> String {
    if offset_minutes == 0 {
        return "UTC".to_string();
    }
    let sign = if offset_minutes < 0 { '-' } else { '+' };
    format!("UTC{sign}{:02}:{:02}", offset_minutes.abs() / 60, offset_minutes.abs() % 60)
}

/// 本文の中の、連続するバッククォートの最大の長さ。
fn longest_backtick_run(text: &str) -> usize {
    text.split(|c| c != '`').map(str::len).max().unwrap_or(0)
}

/// 1 行のコードの書き方(バッククォートを含んでいても崩れない)。
fn code_span(text: &str) -> String {
    let fence = "`".repeat(longest_backtick_run(text) + 1);
    if text.starts_with('`') || text.ends_with('`') {
        format!("{fence} {text} {fence}")
    } else {
        format!("{fence}{text}{fence}")
    }
}

/// 複数行のコードの書き方(囲みの記号は、本文の中のものより長くする)。各行に `indent` を付ける。
fn code_block(text: &str, indent: &str) -> String {
    let fence = "`".repeat((longest_backtick_run(text) + 1).max(3));
    let mut out = format!("{indent}{fence}\n");
    for line in text.lines() {
        out.push_str(indent);
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(&format!("{indent}{fence}\n"));
    out
}

/// 集めた区切りを、Markdown にする。
pub fn render(group: &Group, options: &Options, lists: &Lists) -> Exported {
    let l = labels(options.lang);
    let mut masked = 0usize;
    let mut hide = |text: &str| -> String {
        match options.secrets {
            Secrets::Keep => text.to_string(),
            Secrets::Mask => {
                let result = mask::mask_with(text, lists);
                masked += result.count();
                result.text
            }
        }
    };

    let messages = group.items.iter().filter(|i| matches!(i, Item::Message { .. })).count();
    let calls = group.items.len() - messages;
    let first = group.items.first().map(Item::at).unwrap_or_default();
    let last = group.items.last().map(Item::at).unwrap_or_default();
    let tz = offset_label(options.utc_offset_minutes);

    // 本文を先に作る(伏せた件数を、冒頭に書くため)
    let mut body = String::new();
    let mut index = 0;
    while index < group.items.len() {
        match &group.items[index] {
            Item::Message { at, role, content } => {
                let who = if role.eq_ignore_ascii_case("user") { l.you } else { l.ai };
                body.push_str(&format!("## {who} ({})\n\n{}\n\n", format_time(at, options.utc_offset_minutes), hide(content).trim_end()));
                index += 1;
            }
            Item::Call { .. } => {
                // 続く呼び出しを、1 つの一覧にまとめる
                body.push_str(&format!("**{}**\n\n", l.ran));
                while let Some(Item::Call { name, detail, exit_code, .. }) = group.items.get(index) {
                    let detail = hide(detail);
                    let exit = exit_code.filter(|code| *code != 0).map(|code| format!(" ({} {code})", l.exit_code)).unwrap_or_default();
                    if detail.contains('\n') {
                        body.push_str(&format!("- {}{exit}\n{}", code_span(name), code_block(&detail, "  ")));
                    } else if detail.is_empty() {
                        body.push_str(&format!("- {}{exit}\n", code_span(name)));
                    } else {
                        body.push_str(&format!("- {}: {}{exit}\n", code_span(name), code_span(&detail)));
                    }
                    index += 1;
                }
                body.push('\n');
            }
        }
    }

    let title = hide(&group.title);
    let project = hide(&group.project_path);
    let mut head = format!("# {}\n\n", title.replace('\n', " ").trim());
    head.push_str(&format!("- {}: {} 〜 {} ({tz})\n", l.when, format_time(first, options.utc_offset_minutes), format_time(last, options.utc_offset_minutes)));
    head.push_str(&format!("- {}: {}\n", l.source, tool_label(&group.source_tool)));
    head.push_str(&format!("- {}: {}\n", l.project, code_span(&project)));
    head.push_str(&match options.lang {
        Lang::Ja => format!("- {}: 発言 {messages} 件 / 実行 {calls} 件\n", l.counts),
        Lang::En => format!("- {}: {messages} messages / {calls} tool calls\n", l.counts),
    });
    head.push_str(&format!("\n{}\n", l.not_included));
    if masked > 0 {
        head.push_str(&format!("{}\n", (l.masked_note)(masked)));
    }
    head.push_str("\n---\n\n");

    // 依頼文は、先頭に置く(貼った先の AI が、先に何を頼まれているかを読むため)。区切り線で、記録と分ける
    let request = options.prompt.text(options.lang).map(|text| format!("{text}\n\n---\n\n")).unwrap_or_default();
    Exported { markdown: format!("{request}{head}{}", body.trim_end().to_owned() + "\n"), masked, messages, calls }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use rusqlite::params;

    /// 会話を 1 つ作る。発言は (時刻, 役割, 本文)。
    fn session(conn: &Connection, msgs: &[(&str, &str, &str)]) -> (i64, Vec<i64>) {
        conn.execute("INSERT INTO sessions(source_tool, project_path, source_file, source_hash, title) VALUES ('claude_code', '/work/app', 'f', 'f', '会話全体の題名')", []).unwrap();
        let id = conn.last_insert_rowid();
        let mut ids = Vec::new();
        for (i, (at, role, content)) in msgs.iter().enumerate() {
            conn.execute(
                "INSERT INTO events(session_id, source_tool, source_event_id, occurred_at, role, content, sequence) VALUES (?1, 'claude_code', ?2, ?3, ?4, ?5, ?6)",
                params![id, format!("e{i}"), at, role, content, i as i64],
            ).unwrap();
            ids.push(conn.last_insert_rowid());
        }
        (id, ids)
    }

    fn call(conn: &Connection, session: i64, id: &str, at: &str, name: &str, detail: &str, exit: Option<i64>) {
        crate::tools::store(conn, session, "claude_code", &[crate::tools::ToolCall { source_call_id: id.into(), occurred_at: at.into(), name: name.into(), detail: detail.into(), exit_code: exit }]).unwrap();
    }

    /// 2 つの区切り(30 分以上あく)。前半: 発言 2 つと呼び出し 3 つ。後半: 発言 1 つと呼び出し 1 つ。
    fn two_sections() -> (Connection, Vec<i64>, i64) {
        let conn = db::open_memory().unwrap();
        let (sid, ids) = session(&conn, &[
            ("2026-10-20T01:00:00.000Z", "user", "ビルドが通らない。直して"),
            ("2026-10-20T01:02:00.000Z", "assistant", "設定を直しました"),
            ("2026-10-20T03:00:00.000Z", "user", "次の話題です"),
        ]);
        call(&conn, sid, "c1", "2026-10-20T01:00:05Z", "Bash", "npm run build", Some(1));
        call(&conn, sid, "c2", "2026-10-20T01:01:00Z", "Read", "/work/app/tsconfig.json", None);
        call(&conn, sid, "c3", "2026-10-20T01:01:30Z", "Edit", "/work/app/tsconfig.json", None);
        call(&conn, sid, "c4", "2026-10-20T03:00:10Z", "Bash", "git status", Some(0));
        (conn, ids, sid)
    }

    #[test]
    fn a_section_is_written_as_markdown_with_its_messages_and_calls_only() {
        let (conn, ids, _) = two_sections();
        let out = section_markdown(&conn, ids[0], &Options::default()).unwrap();
        let md = &out.markdown;
        assert!(md.starts_with("# 会話全体の題名\n"), "{md}");
        assert!(md.contains("- 日時: 2026-10-20 01:00 〜 2026-10-20 01:02 (UTC)"), "{md}");
        assert!(md.contains("- 取り込み元: Claude Code") && md.contains("- プロジェクト: `/work/app`"), "{md}");
        assert!(md.contains("発言 2 件 / 実行 3 件"), "{md}");
        assert!(md.contains("ツールの実行結果と、AI の思考は、保存していないので含まれません"), "{md}");
        // 前半だけ(後半の発言と呼び出しは入らない)
        assert!(md.contains("ビルドが通らない。直して") && md.contains("設定を直しました"));
        assert!(!md.contains("次の話題") && !md.contains("git status"), "{md}");
        // 順番: あなた → 実行(Bash) → 実行(Read・Edit) → AI
        let order = ["## あなた (2026-10-20 01:00)", "npm run build", "`Read`: `/work/app/tsconfig.json`", "## AI (2026-10-20 01:02)", "設定を直しました"];
        let positions: Vec<usize> = order.iter().map(|s| md.find(s).unwrap_or_else(|| panic!("無い: {s}\n{md}"))).collect();
        assert!(positions.windows(2).all(|w| w[0] < w[1]), "並びが違う: {positions:?}\n{md}");
        // 終了コードは、0 以外のときだけ
        assert!(md.contains("`Bash`: `npm run build` (終了コード 1)"), "{md}");
        assert_eq!((out.messages, out.calls, out.masked), (2, 3, 0));
        // 後半の区切り
        let later = section_markdown(&conn, ids[2], &Options::default()).unwrap().markdown;
        assert!(later.contains("次の話題です") && later.contains("`Bash`: `git status`") && !later.contains("設定を直しました"));
        assert!(!later.contains("終了コード")); // 終了コード 0 は書かない
    }

    #[test]
    fn a_section_title_beats_the_session_title_and_the_project_is_the_last_resort() {
        let (conn, ids, _) = two_sections();
        sections::set_title(&conn, ids[0], "ビルドエラーの調査").unwrap();
        assert!(section_markdown(&conn, ids[0], &Options::default()).unwrap().markdown.starts_with("# ビルドエラーの調査\n"));
        assert!(section_markdown(&conn, ids[2], &Options::default()).unwrap().markdown.starts_with("# 会話全体の題名\n"));
        conn.execute("UPDATE sessions SET title = NULL", []).unwrap();
        assert!(section_markdown(&conn, ids[2], &Options::default()).unwrap().markdown.starts_with("# /work/app\n"));
    }

    #[test]
    fn secrets_are_masked_by_default_in_messages_titles_and_commands_and_can_be_kept() {
        let conn = db::open_memory().unwrap();
        let (sid, ids) = session(&conn, &[
            ("2026-10-20T01:00:00.000Z", "user", "キーは AKIAIOSFODNN7EXAMPLE です。DB_PASSWORD=hunter2hunter2"),
            ("2026-10-20T01:01:00.000Z", "assistant", "了解"),
        ]);
        call(&conn, sid, "c1", "2026-10-20T01:00:30Z", "Bash", "curl -H 'Authorization: Bearer abcdefghijklmnopqrstuvwxyz0123456789' https://api.example.com", None);
        let masked = section_markdown(&conn, ids[0], &Options::default()).unwrap();
        assert!(!masked.markdown.contains("AKIAIOSFODNN7EXAMPLE") && !masked.markdown.contains("hunter2hunter2") && !masked.markdown.contains("abcdefghijklmnopqrstuvwxyz0123456789"), "{}", masked.markdown);
        assert!(masked.masked >= 3, "伏せた件数: {}", masked.masked);
        assert!(masked.markdown.contains(&format!("{} 件、伏せました", masked.masked)), "{}", masked.markdown);
        let kept = section_markdown(&conn, ids[0], &Options { secrets: Secrets::Keep, ..Options::default() }).unwrap();
        assert!(kept.markdown.contains("AKIAIOSFODNN7EXAMPLE") && kept.markdown.contains("hunter2hunter2") && kept.markdown.contains("abcdefghijklmnopqrstuvwxyz0123456789"));
        assert_eq!(kept.masked, 0);
        assert!(!kept.markdown.contains("伏せました"));
    }

    #[test]
    fn multiline_commands_and_backticks_do_not_break_the_markdown() {
        let conn = db::open_memory().unwrap();
        let (sid, ids) = session(&conn, &[("2026-10-20T01:00:00.000Z", "user", "実行して")]);
        call(&conn, sid, "c1", "2026-10-20T01:00:10Z", "Bash", "cat > a.md <<'E'\n```sh\nls\n```\nE", None);
        call(&conn, sid, "c2", "2026-10-20T01:00:20Z", "Bash", "echo `date`", None);
        call(&conn, sid, "c3", "2026-10-20T01:00:30Z", "AskUserQuestion", "", None);
        let md = section_markdown(&conn, ids[0], &Options::default()).unwrap().markdown;
        // 中に ``` があるので、囲みは、それより長い(```` )
        assert!(md.contains("  ````\n  cat > a.md <<'E'\n  ```sh\n  ls\n  ```\n  E\n  ````\n"), "{md}");
        assert!(md.contains("`Bash`: `` echo `date` ``"), "{md}"); // バッククォートで終わるときは、前後に空白(Markdown の決まり)
        assert!(md.contains("- `AskUserQuestion`\n"), "{md}"); // 内容が無いときは、名前だけ
    }

    #[test]
    fn english_labels_and_the_utc_offset_are_applied() {
        let (conn, ids, _) = two_sections();
        let out = section_markdown(&conn, ids[0], &Options { secrets: Secrets::Mask, lang: Lang::En, prompt: Prompt::None, utc_offset_minutes: 540 }).unwrap();
        let md = &out.markdown;
        assert!(md.contains("- When: 2026-10-20 10:00 〜 2026-10-20 10:02 (UTC+09:00)"), "{md}");
        assert!(md.contains("## You (2026-10-20 10:00)") && md.contains("## AI (2026-10-20 10:02)") && md.contains("**Ran**"), "{md}");
        assert!(md.contains("2 messages / 3 tool calls") && md.contains("(exit code 1)") && md.contains("not saved"), "{md}");
        assert_eq!(offset_label(-330), "UTC-05:30");
    }

    #[test]
    fn a_request_is_put_first_and_separated_from_the_record() {
        let (conn, ids, _) = two_sections();
        let plain = section_markdown(&conn, ids[0], &Options::default()).unwrap();
        assert!(plain.markdown.starts_with("# "), "依頼文なしは、いままでと同じ");
        for (prompt, needle) in [(Prompt::Summarize, "要点を"), (Prompt::Continue, "この続きから作業してください"), (Prompt::Review, "別の視点から評価してください")] {
            let out = section_markdown(&conn, ids[0], &Options { prompt, ..Options::default() }).unwrap();
            let (request, record) = out.markdown.split_once("\n\n---\n\n").unwrap();
            assert!(request.contains(needle), "{request}");
            assert!(!request.contains('\n'), "依頼文は 1 段落: {request}");
            // 記録の部分は、依頼文なしと同じ(伏せた件数・発言の数も変わらない)
            assert!(record.starts_with("# ") && out.markdown.ends_with(plain.markdown.rsplit_once("\n\n---\n\n").unwrap().1), "{}", out.markdown);
            assert_eq!((out.masked, out.messages, out.calls), (plain.masked, plain.messages, plain.calls));
        }
    }

    #[test]
    fn requests_follow_the_language_and_the_names_are_parsed_strictly() {
        let (conn, ids, _) = two_sections();
        let en = section_markdown(&conn, ids[0], &Options { lang: Lang::En, prompt: Prompt::Review, ..Options::default() }).unwrap();
        assert!(en.markdown.starts_with("Below is a record of a conversation with another AI. Evaluate it"), "{}", en.markdown);
        assert_eq!(Prompt::parse(""), Some(Prompt::None));
        assert_eq!(Prompt::parse("none"), Some(Prompt::None));
        assert_eq!(Prompt::parse(" summarize "), Some(Prompt::Summarize));
        assert_eq!(Prompt::parse("continue"), Some(Prompt::Continue));
        assert_eq!(Prompt::parse("review"), Some(Prompt::Review));
        assert_eq!(Prompt::parse("Summarize"), None);
        assert_eq!(Prompt::parse("やって"), None);
    }

    #[test]
    fn without_tool_calls_the_section_still_exports_and_a_missing_event_is_an_error() {
        let conn = db::open_memory().unwrap();
        let (_, ids) = session(&conn, &[("2026-10-20T01:00:00.000Z", "user", "こんにちは")]);
        let out = section_markdown(&conn, ids[0], &Options::default()).unwrap();
        assert_eq!((out.messages, out.calls), (1, 0));
        assert!(!out.markdown.contains("実行したこと"));
        assert!(section_markdown(&conn, 9999, &Options::default()).is_err());
    }
}
