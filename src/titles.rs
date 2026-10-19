//! 会話の題名の編集と、題名の提案(手元の規則)。
//!
//! - 編集した題名には「手動」の印を付ける(`settings` の `title_manual:<会話の ID>`)。取り込みは、印のある会話の題名を上書きしない。
//!   データベースの版(スキーマ)は変えない。
//! - 提案は、最近の発言から、手元の規則で出す。外には何も送らない。

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, params};

pub const MANUAL_PREFIX: &str = "title_manual:";
pub const TITLE_MAX_CHARS: usize = 120;
/// 題名を、手動で付ける。取り込みで上書きされないよう、印を付ける。付けた題名を返す。
pub fn set_manual(conn: &Connection, session_id: i64, title: &str) -> Result<String> {
    let title = clean_title(title).context("題名を入れてください")?;
    let changed = conn.execute("UPDATE sessions SET title = ?2 WHERE id = ?1", params![session_id, title])?;
    ensure!(changed == 1, "その会話が見つかりません");
    conn.execute(
        "INSERT INTO settings(key, value) VALUES (?1, '1') ON CONFLICT(key) DO UPDATE SET value = '1'",
        [format!("{MANUAL_PREFIX}{session_id}")],
    )?;
    Ok(title)
}

/// 題名の整形: 最後の空でない行を取り、前後の引用符や「題名:」を外して、長さを切る。空なら None。
pub fn clean_title(raw: &str) -> Option<String> {
    let line = raw.lines().map(str::trim).rev().find(|l| !l.is_empty())?;
    let line = line.trim_start_matches("題名:").trim_start_matches("題名：").trim();
    let line = line.trim_matches(|c| matches!(c, '「' | '」' | '"' | '\'' | '`' | '『' | '』')).trim();
    let title: String = line.chars().take(TITLE_MAX_CHARS).collect();
    (!title.is_empty()).then_some(title)
}

fn truncate(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    flat.chars().take(max).collect()
}

/// 手元の規則による提案: 区切りの発言のうち、最近のものから、中身のあるもの(短い返事や、スラッシュで始まる命令を除く)の先頭の 1 文。
/// `messages` は、時刻順の (役割, 本文)。
pub fn suggest_local(messages: &[(String, String)]) -> Option<String> {
    for (_, content) in messages.iter().rev().filter(|(role, _)| role == "user").take(30) {
        let first = content.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
        if first.chars().count() < 12 || first.starts_with('/') || first.starts_with('<') {
            continue;
        }
        let sentence = first.split(['。', '？', '?', '！', '!']).next().unwrap_or(first);
        return clean_title(&truncate(sentence, 40));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn session(conn: &Connection, msgs: &[(&str, &str)]) -> i64 {
        conn.execute("INSERT INTO sessions(source_tool, project_path, source_file, source_hash, title) VALUES ('claude-code', '/work', '/logs/a.jsonl', 'a', '古い題名')", []).unwrap();
        let id = conn.last_insert_rowid();
        for (i, (role, content)) in msgs.iter().enumerate() {
            conn.execute(
                "INSERT INTO events(session_id, source_tool, source_event_id, occurred_at, role, content, sequence) VALUES (?1, 'claude-code', ?2, ?3, ?4, ?5, ?6)",
                params![id, format!("s{id}-e{i}"), format!("2026-10-19T10:{i:02}:00.000Z"), role, content, i as i64],
            ).unwrap();
        }
        id
    }

    #[test]
    fn titles_are_cleaned() {
        assert_eq!(clean_title("前置きです。\n\n「SyncLane の題名編集」\n"), Some("SyncLane の題名編集".into()));
        assert_eq!(clean_title("題名: ストア申請"), Some("ストア申請".into()));
        assert_eq!(clean_title("  \n \n"), None);
        assert_eq!(clean_title(&"あ".repeat(500)).unwrap().chars().count(), TITLE_MAX_CHARS);
    }

    #[test]
    fn a_manual_title_is_saved_marked_and_survives_an_import() {
        let conn = db::open_memory().unwrap();
        let id = session(&conn, &[("user", "こんにちは")]);
        assert_eq!(set_manual(&conn, id, "  新しい題名  ").unwrap(), "新しい題名");
        let title: String = conn.query_row("SELECT title FROM sessions WHERE id = ?1", [id], |r| r.get(0)).unwrap();
        assert_eq!(title, "新しい題名");
        assert_eq!(crate::sync::store::get(&conn, &format!("{MANUAL_PREFIX}{id}")).unwrap().as_deref(), Some("1"));
        assert!(set_manual(&conn, id, "   ").is_err());
        assert!(set_manual(&conn, 9999, "x").is_err());
    }

    fn msgs(items: &[(&str, &str)]) -> Vec<(String, String)> {
        items.iter().map(|(r, c)| (r.to_string(), c.to_string())).collect()
    }

    #[test]
    fn the_local_suggestion_skips_short_replies_and_commands() {
        let messages = msgs(&[
            ("user", "SyncLane のストア申請の画面を確認したい。手順も知りたい"),
            ("assistant", "はい"),
            ("user", "いいよ"),
            ("user", "/compact"),
            ("user", "push して"),
        ]);
        assert_eq!(suggest_local(&messages), Some("SyncLane のストア申請の画面を確認したい".into()));
        assert_eq!(suggest_local(&msgs(&[("user", "ok")])), None);
        assert_eq!(suggest_local(&[]), None);
    }
}
