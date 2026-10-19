//! 長い会話を、話題のまとまり(区切り)に分けて、区切りごとに題名を付ける。
//!
//! 区切りは、次のどちらかの位置から始まる:
//! - 前の発言から `GAP_MINUTES` 分以上、空いている
//! - 「続きから再開」された位置: Claude Code が、長くなった会話を要約して続けた位置(取り込みが残した `section_break:`)、
//!   または、その要約の文(「This session is being continued…」)が、発言として入っている位置
//!
//! 区切りの題名は、`settings` の `section_title:<会話の ID>:<区切りの先頭の発言の ID>` に入れる(スキーマは変えない)。
//! 区切りの題名が無ければ、会話全体の題名を使う。

use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use std::collections::HashMap;

use crate::db::TimelineEvent;

/// 発言の間隔が、これ以上あいたら、新しい区切り。実際の長い会話(約 11 時間・574 件)で、3 時間だと 0 回、30 分だと 5 回区切れた
pub const GAP_MINUTES: i64 = 30;
pub const TITLE_PREFIX: &str = "section_title:";
/// 要約して続けた位置(Claude Code の `compact_boundary`)。`section_break:<会話の ID>:<時刻>`。取り込みが残す
pub const BREAK_PREFIX: &str = "section_break:";
const CONTINUATION_PREFIXES: [&str; 1] = ["This session is being continued"];

/// 区切りの先頭の発言。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Boundary {
    pub occurred_at: String,
    pub event_id: i64,
}

/// ある会話の区切りの先頭を、時刻順に並べて返す(先頭の発言は、必ず入る)。
pub fn boundaries(conn: &Connection, session_id: i64) -> Result<Vec<Boundary>> {
    let mut stmt = conn.prepare("SELECT id, occurred_at, substr(content, 1, 80) FROM events WHERE session_id = ?1 ORDER BY occurred_at ASC, id ASC")?;
    let rows = stmt.query_map([session_id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?;
    let breaks = break_times(conn, session_id)?;
    let mut next_break = 0usize;
    let mut out: Vec<Boundary> = Vec::new();
    let mut prev = None;
    for row in rows {
        let (event_id, occurred_at, head) = row?;
        let at = crate::sync::parse_time(&occurred_at);
        let gap = matches!((prev, at), (Some(p), Some(t)) if t - p >= time::Duration::minutes(GAP_MINUTES));
        // 要約して続けた時刻以降の、最初の発言が、新しい区切りの先頭
        let mut summarised = false;
        while next_break < breaks.len() && breaks[next_break].as_str() <= occurred_at.as_str() {
            summarised = true;
            next_break += 1;
        }
        let continued = summarised || CONTINUATION_PREFIXES.iter().any(|p| head.trim_start().starts_with(p));
        if out.is_empty() || gap || continued {
            out.push(Boundary { occurred_at, event_id });
        }
        if at.is_some() {
            prev = at;
        }
    }
    Ok(out)
}

/// ある会話の、要約して続けた時刻(時刻順)。
fn break_times(conn: &Connection, session_id: i64) -> Result<Vec<String>> {
    let prefix = format!("{BREAK_PREFIX}{session_id}:");
    let mut stmt = conn.prepare("SELECT key FROM settings WHERE substr(key, 1, ?2) = ?1")?;
    let rows = stmt.query_map(params![prefix, prefix.chars().count() as i64], |r| r.get::<_, String>(0))?;
    let mut times: Vec<String> = rows.map(|k| k.map(|k| k[prefix.len()..].to_string())).collect::<rusqlite::Result<_>>()?;
    times.sort();
    Ok(times)
}

/// 発言(時刻と ID)が属する区切りの、先頭の位置。
fn section_of<'a>(boundaries: &'a [Boundary], occurred_at: &str, event_id: i64) -> Option<&'a Boundary> {
    boundaries.iter().rev().find(|b| (b.occurred_at.as_str(), b.event_id) <= (occurred_at, event_id)).or_else(|| boundaries.first())
}

fn key(session_id: i64, start_event_id: i64) -> String {
    format!("{TITLE_PREFIX}{session_id}:{start_event_id}")
}

/// 付けてある区切りの題名: (会話の ID, 区切りの先頭の発言の ID) → 題名。
fn titles(conn: &Connection) -> Result<HashMap<(i64, i64), String>> {
    let mut stmt = conn.prepare("SELECT key, value FROM settings WHERE key LIKE 'section_title:%'")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut map = HashMap::new();
    for row in rows {
        let (k, v) = row?;
        let mut parts = k[TITLE_PREFIX.len()..].split(':');
        if let (Some(Ok(session)), Some(Ok(start))) = (parts.next().map(str::parse::<i64>), parts.next().map(str::parse::<i64>)) {
            map.insert((session, start), v);
        }
    }
    Ok(map)
}

/// 画面に出す発言に、区切りの先頭(`section_start`)と、区切りの題名(あれば `title` を置き換える)を付ける。
pub fn attach(conn: &Connection, events: &mut [TimelineEvent]) -> Result<()> {
    if events.is_empty() {
        return Ok(());
    }
    let titles = titles(conn)?;
    let mut cache: HashMap<i64, Vec<Boundary>> = HashMap::new();
    for event in events.iter_mut() {
        if !cache.contains_key(&event.session_id) {
            cache.insert(event.session_id, boundaries(conn, event.session_id)?);
        }
        let bounds = &cache[&event.session_id];
        if let Some(start) = section_of(bounds, &event.occurred_at, event.event_id) {
            event.section_start = Some(start.event_id);
            if let Some(title) = titles.get(&(event.session_id, start.event_id)) {
                event.title = Some(title.clone());
            }
        }
    }
    Ok(())
}

/// ある発言が属する区切りの、中身。
pub struct Section {
    pub session_id: i64,
    pub start_event_id: i64,
    /// (役割, 本文)。時刻順
    pub messages: Vec<(String, String)>,
}

pub fn load(conn: &Connection, event_id: i64) -> Result<Section> {
    let (session_id, occurred_at): (i64, String) = conn
        .query_row("SELECT session_id, occurred_at FROM events WHERE id = ?1", [event_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .context("その発言が見つかりません")?;
    let bounds = boundaries(conn, session_id)?;
    let start = section_of(&bounds, &occurred_at, event_id).context("区切りが見つかりません")?.clone();
    let next = bounds.iter().find(|b| (b.occurred_at.as_str(), b.event_id) > (start.occurred_at.as_str(), start.event_id));
    let mut stmt = conn.prepare(
        "SELECT role, content FROM events
         WHERE session_id = ?1
           AND (occurred_at > ?2 OR (occurred_at = ?2 AND id >= ?3))
           AND (?4 IS NULL OR occurred_at < ?4 OR (occurred_at = ?4 AND id < ?5))
         ORDER BY occurred_at ASC, id ASC",
    )?;
    let messages = stmt
        .query_map(
            params![session_id, start.occurred_at, start.event_id, next.map(|b| b.occurred_at.clone()), next.map(|b| b.event_id)],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Section { session_id, start_event_id: start.event_id, messages })
}

/// 発言が属する区切りに、題名を付ける。空なら、区切りの題名を外す(会話全体の題名に戻る)。付けた題名を返す。
pub fn set_title(conn: &Connection, event_id: i64, title: &str) -> Result<Option<String>> {
    let (session_id, occurred_at): (i64, String) = conn
        .query_row("SELECT session_id, occurred_at FROM events WHERE id = ?1", [event_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .context("その発言が見つかりません")?;
    let bounds = boundaries(conn, session_id)?;
    let start = section_of(&bounds, &occurred_at, event_id).context("区切りが見つかりません")?;
    let key = key(session_id, start.event_id);
    match crate::titles::clean_title(title) {
        Some(title) => {
            conn.execute("INSERT INTO settings(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value", params![key, title])?;
            Ok(Some(title))
        }
        None => {
            conn.execute("DELETE FROM settings WHERE key = ?1", [key])?;
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    /// 会話を 1 つ作る。発言は (時刻, 役割, 本文)。
    fn session(conn: &Connection, key: &str, msgs: &[(&str, &str, &str)]) -> i64 {
        conn.execute("INSERT INTO sessions(source_tool, project_path, source_file, source_hash, title) VALUES ('claude-code', '/w', ?1, ?1, '会話全体の題名')", [key]).unwrap();
        let id = conn.last_insert_rowid();
        for (i, (at, role, content)) in msgs.iter().enumerate() {
            conn.execute(
                "INSERT INTO events(session_id, source_tool, source_event_id, occurred_at, role, content, sequence) VALUES (?1, 'claude-code', ?2, ?3, ?4, ?5, ?6)",
                params![id, format!("{key}-{i}"), at, role, content, i as i64],
            ).unwrap();
        }
        id
    }

    fn long_session(conn: &Connection) -> i64 {
        session(conn, "long", &[
            ("2026-10-19T01:00:00.000Z", "user", "京都の旅行を計画したい"),
            ("2026-10-19T01:05:00.000Z", "assistant", "伏見稲荷がおすすめです"),
            ("2026-10-19T01:30:00.000Z", "user", "清水寺は?"),
            // 30 分以上あく → 新しい区切り
            ("2026-10-19T05:00:00.000Z", "user", "家計簿アプリを作りたい"),
            ("2026-10-19T05:10:00.000Z", "assistant", "csv を読みましょう"),
            // 続きから再開 → 新しい区切り(時間は空いていない)
            ("2026-10-19T05:20:00.000Z", "user", "This session is being continued from a previous conversation that ran out of context. ..."),
            ("2026-10-19T05:21:00.000Z", "assistant", "続きです"),
        ])
    }

    fn event_ids(conn: &Connection, session_id: i64) -> Vec<i64> {
        let mut stmt = conn.prepare("SELECT id FROM events WHERE session_id = ?1 ORDER BY occurred_at, id").unwrap();
        stmt.query_map([session_id], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect()
    }

    #[test]
    fn a_long_gap_and_a_continuation_start_new_sections() {
        let conn = db::open_memory().unwrap();
        let id = long_session(&conn);
        let ids = event_ids(&conn, id);
        let starts: Vec<i64> = boundaries(&conn, id).unwrap().iter().map(|b| b.event_id).collect();
        assert_eq!(starts, vec![ids[0], ids[3], ids[5]]);
    }

    #[test]
    fn short_gaps_do_not_split_and_a_single_event_is_one_section() {
        let conn = db::open_memory().unwrap();
        let id = session(&conn, "short", &[
            ("2026-10-19T01:00:00.000Z", "user", "a"),
            ("2026-10-19T01:29:00.000Z", "assistant", "b"), // 29 分
            ("2026-10-19T01:59:00.000Z", "user", "c"),      // 30 分
        ]);
        assert_eq!(boundaries(&conn, id).unwrap().len(), 2); // 29 分は区切らない。ちょうど 30 分で区切る
        let lone = session(&conn, "lone", &[("2026-10-19T01:00:00.000Z", "user", "x")]);
        assert_eq!(boundaries(&conn, lone).unwrap().len(), 1);
    }

    #[test]
    fn setting_a_title_changes_only_that_section() {
        let conn = db::open_memory().unwrap();
        let id = long_session(&conn);
        let ids = event_ids(&conn, id);
        // 2 つ目の区切りの、真ん中の発言(ids[4])を指して、題名を付ける
        assert_eq!(set_title(&conn, ids[4], "  家計簿アプリの設計  ").unwrap(), Some("家計簿アプリの設計".into()));

        let mut events = db::session_events(&conn, id, None, 20).unwrap();
        let by_id = |events: &[TimelineEvent], id: i64| events.iter().find(|e| e.event_id == id).unwrap().title.clone();
        assert_eq!(by_id(&events, ids[3]).as_deref(), Some("家計簿アプリの設計")); // 同じ区切り
        assert_eq!(by_id(&events, ids[4]).as_deref(), Some("家計簿アプリの設計"));
        assert_eq!(by_id(&events, ids[0]).as_deref(), Some("会話全体の題名")); // 別の区切りは、そのまま
        assert_eq!(by_id(&events, ids[6]).as_deref(), Some("会話全体の題名"));
        // 会話全体の題名(sessions.title)は、変わらない
        let session_title: String = conn.query_row("SELECT title FROM sessions WHERE id = ?1", [id], |r| r.get(0)).unwrap();
        assert_eq!(session_title, "会話全体の題名");
        assert_eq!(events.iter().find(|e| e.event_id == ids[1]).unwrap().section_start, Some(ids[0]));
        events.clear();
    }

    #[test]
    fn an_empty_title_goes_back_to_the_session_title() {
        let conn = db::open_memory().unwrap();
        let id = long_session(&conn);
        let ids = event_ids(&conn, id);
        set_title(&conn, ids[3], "家計簿").unwrap();
        assert_eq!(set_title(&conn, ids[4], "   ").unwrap(), None);
        let events = db::session_events(&conn, id, None, 20).unwrap();
        assert_eq!(events.iter().find(|e| e.event_id == ids[3]).unwrap().title.as_deref(), Some("会話全体の題名"));
    }

    #[test]
    fn a_section_title_survives_new_messages_and_title_updates_from_imports() {
        let conn = db::open_memory().unwrap();
        let id = long_session(&conn);
        let ids = event_ids(&conn, id);
        set_title(&conn, ids[3], "家計簿").unwrap();
        // 取り込みが、会話全体の題名を書き換えても、区切りの題名は残る
        conn.execute("UPDATE sessions SET title = '付け直された題名' WHERE id = ?1", [id]).unwrap();
        // 同じ区切りに、発言が増えても、区切りは動かない
        conn.execute("INSERT INTO events(session_id, source_tool, source_event_id, occurred_at, role, content, sequence) VALUES (?1, 'claude-code', 'late', '2026-10-19T05:22:00.000Z', 'user', 'あと', 9)", [id]).unwrap();
        let events = db::session_events(&conn, id, None, 30).unwrap();
        assert_eq!(events.iter().find(|e| e.event_id == ids[3]).unwrap().title.as_deref(), Some("家計簿"));
        assert_eq!(events.iter().find(|e| e.event_id == ids[0]).unwrap().title.as_deref(), Some("付け直された題名"));
    }

    #[test]
    fn loading_a_section_returns_only_its_messages() {
        let conn = db::open_memory().unwrap();
        let id = long_session(&conn);
        let ids = event_ids(&conn, id);
        let middle = load(&conn, ids[4]).unwrap();
        assert_eq!(middle.start_event_id, ids[3]);
        assert_eq!(middle.messages.len(), 2);
        assert!(middle.messages[0].1.contains("家計簿"));
        let first = load(&conn, ids[1]).unwrap();
        assert_eq!(first.messages.len(), 3);
        let last = load(&conn, ids[6]).unwrap();
        assert_eq!(last.messages.len(), 2);
        assert!(load(&conn, 99999).is_err());
    }

    #[test]
    fn a_recorded_compaction_starts_a_new_section_at_the_next_event() {
        let conn = db::open_memory().unwrap();
        // 休みなく続いた会話(間隔は、すべて 30 分未満)。途中で、要約して続けた
        let id = session(&conn, "nonstop", &[
            ("2026-10-19T01:00:00.000Z", "user", "話題 A"),
            ("2026-10-19T01:05:00.000Z", "assistant", "答え A"),
            ("2026-10-19T01:20:00.000Z", "user", "話題 B"),
            ("2026-10-19T01:25:00.000Z", "assistant", "答え B"),
            ("2026-10-19T01:40:00.000Z", "user", "話題 C"),
        ]);
        let ids = event_ids(&conn, id);
        assert_eq!(boundaries(&conn, id).unwrap().len(), 1); // 印が無ければ、1 つのまま
        // 01:20:00 の直前に、要約して続けた(印は、次の発言より前の時刻)
        conn.execute("INSERT INTO settings(key, value) VALUES (?1, '1')", [format!("{BREAK_PREFIX}{id}:2026-10-19T01:19:30.000Z")]).unwrap();
        conn.execute("INSERT INTO settings(key, value) VALUES (?1, '1')", [format!("{BREAK_PREFIX}{id}:2026-10-19T01:30:00.000Z")]).unwrap();
        // 別の会話の印は、影響しない
        conn.execute("INSERT INTO settings(key, value) VALUES (?1, '1')", [format!("{BREAK_PREFIX}{}:2026-10-19T01:30:00.000Z", id + 100)]).unwrap();
        let starts: Vec<i64> = boundaries(&conn, id).unwrap().iter().map(|b| b.event_id).collect();
        assert_eq!(starts, vec![ids[0], ids[2], ids[4]]);
    }

    #[test]
    fn two_compactions_before_the_same_event_make_one_boundary() {
        let conn = db::open_memory().unwrap();
        let id = session(&conn, "twice", &[
            ("2026-10-19T01:00:00.000Z", "user", "a"),
            ("2026-10-19T01:20:00.000Z", "user", "b"),
        ]);
        for at in ["2026-10-19T01:10:00.000Z", "2026-10-19T01:15:00.000Z"] {
            conn.execute("INSERT INTO settings(key, value) VALUES (?1, '1')", [format!("{BREAK_PREFIX}{id}:{at}")]).unwrap();
        }
        assert_eq!(boundaries(&conn, id).unwrap().len(), 2);
    }
}
