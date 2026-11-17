//! 「続きから」の一覧。プロジェクトごとに、ツールごとの最新の会話を 1 件ずつ返す(画面の左のサイドバー用)。
//!
//! - 「最新の会話」は、最後の発言の時刻で決める(印は付けない。保存してある記録から、そのまま分かる)。
//! - プロジェクトは、最後の発言が新しい順。同じ名前で場所が違うプロジェクトは、別のものとして数える。
//!   表示の名前は場所の末尾の名前で、重なったときだけ、親のフォルダを足す。
//! - 読み取りだけ。会話を開くことも、実行することもしない(再開は、`resume` が、コマンドを作って渡す)。

use anyhow::Result;
use rusqlite::{Connection, params};
use serde::Serialize;
use std::collections::HashMap;

/// 題名が無い会話の、仮の題名にする最初の質問の長さ(文字数)。
const FALLBACK_TITLE_CHARS: usize = 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LatestSession {
    pub session_id: i64,
    pub source_tool: String,
    /// 会話の題名。無ければ、最初の質問の先頭(それも無ければ空)
    pub title: String,
    /// この会話の、最後の発言。再開のコマンドを作るときの、指し先
    pub last_event_id: i64,
    pub last_at: String,
    /// 元のツールで再開するコマンドを作れるか(Web 版の Gemini・ChatGPT は、作れない)
    pub resumable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectRow {
    /// 絞り込みの鍵(`sessions.project_path`)
    pub project_path: String,
    /// 表示の名前
    pub name: String,
    pub last_at: String,
    /// このプロジェクトの会話の数(発言が 1 つ以上ある会話)
    pub session_count: usize,
    /// ツールごとの最新の会話。新しい順
    pub sessions: Vec<LatestSession>,
}

/// プロジェクトごと・ツールごとの、最新の会話。プロジェクトは、最後の発言が新しい順。
pub fn overview(conn: &Connection) -> Result<Vec<ProjectRow>> {
    // 会話ごとの、最後の発言の時刻
    let mut stmt = conn.prepare(
        "SELECT s.id, s.project_path, s.source_tool, s.title, MAX(e.occurred_at)
         FROM sessions s JOIN events e ON e.session_id = s.id
         GROUP BY s.id",
    )?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, Option<String>>(3)?, r.get::<_, String>(4)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    // (プロジェクト, ツール) ごとに、最後の発言がいちばん新しい会話を残す(同じ時刻なら、新しく取り込んだほう)
    let mut latest: HashMap<(String, String), (i64, Option<String>, String)> = HashMap::new();
    let mut totals: HashMap<String, usize> = HashMap::new();
    for (id, project, tool, title, last_at) in rows {
        *totals.entry(project.clone()).or_default() += 1;
        let entry = latest.entry((project, tool)).or_insert((id, title.clone(), last_at.clone()));
        if (last_at.as_str(), id) > (entry.2.as_str(), entry.0) {
            *entry = (id, title, last_at);
        }
    }

    let mut by_project: HashMap<String, Vec<LatestSession>> = HashMap::new();
    for ((project, tool), (session_id, title, last_at)) in latest {
        let last_event_id: i64 = conn.query_row(
            "SELECT id FROM events WHERE session_id = ?1 ORDER BY occurred_at DESC, sequence DESC LIMIT 1",
            params![session_id],
            |r| r.get(0),
        )?;
        let title = title_or_first_question(conn, session_id, title)?;
        let resumable = crate::resume::supports(&tool);
        by_project.entry(project).or_default().push(LatestSession { session_id, source_tool: tool, title, last_event_id, last_at, resumable });
    }

    let names = display_names(&by_project.keys().map(String::as_str).collect::<Vec<_>>());
    let mut projects: Vec<ProjectRow> = by_project
        .into_iter()
        .map(|(project_path, mut sessions)| {
            // 同じ時刻なら、ツール名で並べて、順番を安定させる
            sessions.sort_by(|a, b| b.last_at.cmp(&a.last_at).then_with(|| a.source_tool.cmp(&b.source_tool)));
            let last_at = sessions.first().map(|s| s.last_at.clone()).unwrap_or_default();
            let name = names.get(&project_path).cloned().unwrap_or_else(|| project_path.clone());
            let session_count = totals.get(&project_path).copied().unwrap_or(sessions.len());
            ProjectRow { project_path, name, last_at, session_count, sessions }
        })
        .collect();
    projects.sort_by(|a, b| b.last_at.cmp(&a.last_at).then_with(|| a.project_path.cmp(&b.project_path)));
    Ok(projects)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolSession {
    pub session_id: i64,
    pub project_path: String,
    pub project_name: String,
    pub title: String,
    pub last_event_id: i64,
    pub last_at: String,
    pub resumable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolGroup {
    pub source_tool: String,
    pub display_name: String,
    pub sessions: Vec<ToolSession>,
}

fn tool_display_name(tool: &str) -> &'static str {
    match tool {
        "claude_code" => "Claude Code",
        "codex" => "Codex",
        "gemini_cli" => "Gemini CLI",
        "copilot_cli" => "Copilot CLI",
        "claude_web" | "claude_ai" => "Claude (Web)",
        "chatgpt_web" => "ChatGPT (Web)",
        "gemini_web" => "Gemini (Web)",
        _ => "AI",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DaySummary {
    pub day: String,
    pub count: i64,
}

/// タイムライン用: 会話が存在する全日付の一覧(新しい順)。
/// プロジェクト指定なしの場合は events だけで直接集計して無駄な JOIN を省く。
pub fn timeline_days(conn: &Connection, project: Option<&str>) -> Result<Vec<DaySummary>> {
    let mapper = |r: &rusqlite::Row| Ok(DaySummary { day: r.get(0)?, count: r.get(1)? });
    let rows = if let Some(p) = project {
        let mut stmt = conn.prepare(
            "SELECT substr(e.occurred_at, 1, 10) as day, count(*)
             FROM events e
             JOIN sessions s ON s.id = e.session_id
             WHERE s.project_path = ?1
             GROUP BY day
             ORDER BY day DESC",
        )?;
        stmt.query_map([p], mapper)?.collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        let mut stmt = conn.prepare(
            "SELECT substr(occurred_at, 1, 10) as day, count(*)
             FROM events
             GROUP BY day
             ORDER BY day DESC",
        )?;
        stmt.query_map([], mapper)?.collect::<rusqlite::Result<Vec<_>>>()?
    };
    Ok(rows)
}

/// AIツールごとにグルーピングした会話一覧(最新順)。サイドバー用。
/// 各会話の最後の発言(last_event_id, last_at)と最初の質問を一括取得し、N+1クエリを排除。
pub fn sessions_by_tool(conn: &Connection) -> Result<Vec<ToolGroup>> {
    let mut stmt = conn.prepare(
        "SELECT s.id, s.project_path, s.source_tool, s.title, e.id AS last_event_id, e.occurred_at AS last_at
         FROM sessions s
         JOIN (
             SELECT id, session_id, occurred_at, sequence,
                    ROW_NUMBER() OVER (PARTITION BY session_id ORDER BY occurred_at DESC, sequence DESC) as rn
             FROM events
         ) e ON e.session_id = s.id AND e.rn = 1
         ORDER BY e.occurred_at DESC",
    )?;

    struct RawRow {
        id: i64,
        project_path: String,
        source_tool: String,
        title: Option<String>,
        last_event_id: i64,
        last_at: String,
    }

    let rows: Vec<RawRow> = stmt
        .query_map([], |r| {
            Ok(RawRow {
                id: r.get(0)?,
                project_path: r.get(1)?,
                source_tool: r.get(2)?,
                title: r.get(3)?,
                last_event_id: r.get(4)?,
                last_at: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let all_projects: Vec<&str> = rows.iter().map(|r| r.project_path.as_str()).collect();
    let names = display_names(&all_projects);

    let need_title_ids: Vec<i64> = rows
        .iter()
        .filter(|r| r.title.as_ref().map(|t| t.trim().is_empty()).unwrap_or(true))
        .map(|r| r.id)
        .collect();

    let mut first_questions: HashMap<i64, String> = HashMap::new();
    if !need_title_ids.is_empty() {
        let placeholders = need_title_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT session_id, content FROM (
                 SELECT session_id, content,
                        ROW_NUMBER() OVER (PARTITION BY session_id ORDER BY sequence ASC) as rn
                 FROM events
                 WHERE role = 'user' AND session_id IN ({placeholders})
             ) WHERE rn = 1"
        );
        let mut q_stmt = conn.prepare(&sql)?;
        let q_rows = q_stmt.query_map(rusqlite::params_from_iter(need_title_ids), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?;
        for q in q_rows {
            if let Ok((sid, content)) = q {
                let clean = content
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .take(FALLBACK_TITLE_CHARS)
                    .collect();
                first_questions.insert(sid, clean);
            }
        }
    }

    let mut by_tool: HashMap<String, Vec<ToolSession>> = HashMap::new();
    for row in rows {
        let title = match row.title.filter(|t| !t.trim().is_empty()) {
            Some(t) => t,
            None => first_questions.get(&row.id).cloned().unwrap_or_default(),
        };
        let resumable = crate::resume::supports(&row.source_tool);
        let project_name = names.get(&row.project_path).cloned().unwrap_or_else(|| row.project_path.clone());
        by_tool.entry(row.source_tool.clone()).or_default().push(ToolSession {
            session_id: row.id,
            project_path: row.project_path,
            project_name,
            title,
            last_event_id: row.last_event_id,
            last_at: row.last_at,
            resumable,
        });
    }

    let mut groups: Vec<ToolGroup> = by_tool
        .into_iter()
        .map(|(tool, mut sessions)| {
            sessions.sort_by(|a, b| b.last_at.cmp(&a.last_at));
            ToolGroup {
                display_name: tool_display_name(&tool).to_string(),
                source_tool: tool,
                sessions,
            }
        })
        .collect();

    // 最新の会話があるツールを上に
    groups.sort_by(|a, b| {
        let a_last = a.sessions.first().map(|s| s.last_at.as_str()).unwrap_or("");
        let b_last = b.sessions.first().map(|s| s.last_at.as_str()).unwrap_or("");
        b_last.cmp(a_last)
    });

    Ok(groups)
}

/// プロジェクトの指定(名前または場所)を、プロジェクトに引き当てた結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    One(ProjectRow),
    /// 複数に当たった(推測で 1 つに決めない。呼び出し側が、選び直させる)
    Many(Vec<ProjectRow>),
    None,
}

/// 名前または場所で、プロジェクトを探す。優先順: ①場所がそのまま一致 ②名前(表示の名前、または場所の末尾の名前)が、大文字小文字を無視して一致
/// ③名前または場所の一部が、大文字小文字を無視して一致。各段で、ちょうど 1 つなら決まり、複数なら `Many`。当たらなければ次の段へ。
pub fn find(conn: &Connection, input: &str) -> Result<Found> {
    let input = input.trim();
    if input.is_empty() {
        return Ok(Found::None);
    }
    let projects = overview(conn)?;
    let lower = input.to_lowercase();
    let stages: [Box<dyn Fn(&ProjectRow) -> bool>; 3] = [
        Box::new(|p| p.project_path == input),
        Box::new(|p| p.name.to_lowercase() == lower || last_name(&p.project_path).to_lowercase() == lower),
        Box::new(|p| p.name.to_lowercase().contains(&lower) || p.project_path.to_lowercase().contains(&lower)),
    ];
    for matches in stages {
        let mut hit: Vec<ProjectRow> = projects.iter().filter(|p| matches(p)).cloned().collect();
        match hit.len() {
            0 => continue,
            1 => return Ok(Found::One(hit.remove(0))),
            _ => return Ok(Found::Many(hit)),
        }
    }
    Ok(Found::None)
}

/// あるプロジェクトの、最近の会話 1 件の、最後の質問と最後の回答(切り詰める前の本文)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionContext {
    pub session_id: i64,
    pub source_tool: String,
    pub title: String,
    pub last_at: String,
    pub last_question: Option<String>,
    pub last_answer: Option<String>,
}

/// そのプロジェクトの、最近の会話(最後の発言が新しい順)。ツールは問わない。
pub fn recent_sessions(conn: &Connection, project_path: &str, limit: usize) -> Result<Vec<SessionContext>> {
    let mut stmt = conn.prepare(
        "SELECT s.id, s.source_tool, s.title, MAX(e.occurred_at) AS last_at
         FROM sessions s JOIN events e ON e.session_id = s.id
         WHERE s.project_path = ?1
         GROUP BY s.id
         ORDER BY last_at DESC, s.id DESC
         LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![project_path, limit as i64], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, String>(3)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let last_of = |session_id: i64, role: &str| -> Option<String> {
        conn.query_row(
            "SELECT content FROM events WHERE session_id = ?1 AND role = ?2 ORDER BY occurred_at DESC, sequence DESC LIMIT 1",
            params![session_id, role],
            |r| r.get(0),
        )
        .ok()
    };
    rows.into_iter()
        .map(|(session_id, source_tool, title, last_at)| {
            Ok(SessionContext {
                session_id,
                source_tool,
                title: title_or_first_question(conn, session_id, title)?,
                last_at,
                last_question: last_of(session_id, "user"),
                last_answer: last_of(session_id, "assistant"),
            })
        })
        .collect()
}

/// 会話の題名。無ければ、最初の質問の先頭(それも無ければ空)。
fn title_or_first_question(conn: &Connection, session_id: i64, title: Option<String>) -> Result<String> {
    match title.filter(|t| !t.trim().is_empty()) {
        Some(title) => Ok(title),
        None => first_question(conn, session_id),
    }
}

fn first_question(conn: &Connection, session_id: i64) -> Result<String> {
    let content: Option<String> = conn
        .query_row(
            "SELECT content FROM events WHERE session_id = ?1 AND role = 'user' ORDER BY sequence LIMIT 1",
            params![session_id],
            |r| r.get(0),
        )
        .ok();
    Ok(content.map(|c| c.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(FALLBACK_TITLE_CHARS).collect()).unwrap_or_default())
}

/// 場所の末尾の名前。区切りは `/` と `\`(Windows の記録も、そのまま読める)。末尾が空なら、場所そのもの。
pub(crate) fn last_name(path: &str) -> &str {
    path.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().filter(|n| !n.is_empty()).unwrap_or(path)
}

/// 表示の名前。末尾の名前が、別の場所のプロジェクトと重なったときだけ、親のフォルダを足す(`親/名前`)。
pub(crate) fn display_names(paths: &[&str]) -> HashMap<String, String> {
    let mut count: HashMap<&str, usize> = HashMap::new();
    for path in paths {
        *count.entry(last_name(path)).or_default() += 1;
    }
    paths
        .iter()
        .map(|path| {
            let name = last_name(path);
            if count[name] < 2 {
                return ((*path).to_owned(), name.to_owned());
            }
            let parent = path.trim_end_matches(['/', '\\']).rsplit_once(['/', '\\']).map(|(head, _)| last_name(head)).filter(|p| !p.is_empty());
            ((*path).to_owned(), parent.map_or_else(|| name.to_owned(), |p| format!("{p}/{name}")))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn session(conn: &Connection, tool: &str, project: &str, title: Option<&str>, file: &str) -> i64 {
        conn.execute(
            "INSERT INTO sessions(source_tool, project_path, source_file, source_hash, title) VALUES (?1, ?2, ?3, ?3, ?4)",
            params![tool, project, file, title],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn event(conn: &Connection, session: i64, at: &str, role: &str, content: &str, sequence: i64) -> i64 {
        conn.execute(
            "INSERT INTO events(session_id, source_tool, source_event_id, occurred_at, role, content, sequence) VALUES (?1, 'x', ?2, ?3, ?4, ?5, ?6)",
            params![session, format!("e{session}-{sequence}-{at}"), at, role, content, sequence],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    #[test]
    fn keeps_the_newest_conversation_per_project_and_tool_and_orders_projects_by_last_message() -> Result<()> {
        let conn = db::open_memory()?;
        // SyncLane: Claude Code は 2 本(新しいほうが残る)、Codex は 1 本
        let old = session(&conn, "claude_code", "/work/SyncLane", Some("古い会話"), "a");
        event(&conn, old, "2026-10-01T01:00:00.000Z", "user", "古い質問", 0);
        let new = session(&conn, "claude_code", "/work/SyncLane", Some("新しい会話"), "b");
        event(&conn, new, "2026-10-10T01:00:00.000Z", "user", "新しい質問", 0);
        let last = event(&conn, new, "2026-10-10T01:05:00.000Z", "assistant", "新しい答え", 1);
        let codex = session(&conn, "codex", "/work/SyncLane", Some("Codex の会話"), "c");
        event(&conn, codex, "2026-10-05T01:00:00.000Z", "user", "Codex の質問", 0);
        // MrPosts: いちばん新しい
        let posts = session(&conn, "claude_code", "/work/MrPosts", Some("投稿の下書き"), "d");
        event(&conn, posts, "2026-10-20T01:00:00.000Z", "user", "下書きして", 0);

        let projects = overview(&conn)?;
        assert_eq!(projects.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["MrPosts", "SyncLane"]);
        let sync_lane = &projects[1];
        assert_eq!(sync_lane.project_path, "/work/SyncLane");
        assert_eq!(sync_lane.last_at, "2026-10-10T01:05:00.000Z");
        // 新しい順に、ツールごとの最新が 1 件ずつ
        assert_eq!(sync_lane.sessions.iter().map(|s| (s.source_tool.as_str(), s.title.as_str())).collect::<Vec<_>>(), [("claude_code", "新しい会話"), ("codex", "Codex の会話")]);
        // 再開の指し先は、その会話の最後の発言
        assert_eq!(sync_lane.sessions[0].last_event_id, last);
        assert!(sync_lane.sessions.iter().all(|s| s.resumable));
        Ok(())
    }

    #[test]
    fn falls_back_to_the_first_question_and_marks_web_conversations_as_not_resumable() -> Result<()> {
        let conn = db::open_memory()?;
        let chat = session(&conn, "chatgpt", "(unknown)", None, "w");
        event(&conn, chat, "2026-10-02T01:00:00.000Z", "user", "京都で\n3日間の旅行を計画したい", 0);
        event(&conn, chat, "2026-10-02T01:01:00.000Z", "assistant", "どうぞ", 1);

        let projects = overview(&conn)?;
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].sessions[0].title, "京都で 3日間の旅行を計画したい");
        assert!(!projects[0].sessions[0].resumable);
        Ok(())
    }

    #[test]
    fn a_conversation_without_messages_is_not_listed() -> Result<()> {
        let conn = db::open_memory()?;
        session(&conn, "claude_code", "/work/empty", Some("空"), "e");
        assert!(overview(&conn)?.is_empty());
        Ok(())
    }

    fn two_apps_and_sync_lane(conn: &Connection) {
        let a = session(conn, "claude_code", "/a/app", Some("ログイン"), "a");
        event(conn, a, "2026-10-12T01:00:00.000Z", "user", "ログインを直して", 0);
        event(conn, a, "2026-10-12T01:05:00.000Z", "assistant", "直しました", 1);
        let b = session(conn, "claude_code", "/b/app", Some("ビルド"), "b");
        event(conn, b, "2026-10-01T01:00:00.000Z", "user", "ビルドが遅い", 0);
        let s1 = session(conn, "claude_code", "/work/SyncLane", Some("古い会話"), "c");
        event(conn, s1, "2026-09-01T01:00:00.000Z", "user", "古い質問", 0);
        let s2 = session(conn, "codex", "/work/SyncLane", None, "d");
        event(conn, s2, "2026-10-10T01:00:00.000Z", "user", "新しい質問\nです", 0);
    }

    #[test]
    fn counts_every_conversation_even_when_only_the_newest_per_tool_is_kept() -> Result<()> {
        let conn = db::open_memory()?;
        two_apps_and_sync_lane(&conn);
        let projects = overview(&conn)?;
        let sync_lane = projects.iter().find(|p| p.project_path == "/work/SyncLane").unwrap();
        assert_eq!(sync_lane.session_count, 2);
        assert_eq!(sync_lane.sessions.len(), 2); // Claude Code と Codex に 1 本ずつ
        Ok(())
    }

    #[test]
    fn find_prefers_the_exact_place_then_the_exact_name_and_never_guesses_between_several() -> Result<()> {
        let conn = db::open_memory()?;
        two_apps_and_sync_lane(&conn);
        let one = |input: &str| match find(&conn, input).unwrap() {
            Found::One(p) => p.project_path,
            other => panic!("{input}: {other:?}"),
        };
        // 場所がそのまま
        assert_eq!(one("/b/app"), "/b/app");
        // 親つきの表示名・大文字小文字の違い・名前の一部
        assert_eq!(one("b/app"), "/b/app");
        assert_eq!(one("SYNCLANE"), "/work/SyncLane");
        assert_eq!(one("Sync"), "/work/SyncLane");
        // 末尾の名前が重なるときは、決めない
        match find(&conn, "app")? {
            Found::Many(candidates) => assert_eq!(candidates.iter().map(|p| p.project_path.as_str()).collect::<Vec<_>>(), ["/a/app", "/b/app"]),
            other => panic!("{other:?}"),
        }
        assert_eq!(find(&conn, "nothing")?, Found::None);
        assert_eq!(find(&conn, "  ")?, Found::None);
        Ok(())
    }

    #[test]
    fn recent_sessions_are_newest_first_with_the_last_question_and_answer() -> Result<()> {
        let conn = db::open_memory()?;
        two_apps_and_sync_lane(&conn);
        let recent = recent_sessions(&conn, "/work/SyncLane", 10)?;
        assert_eq!(recent.iter().map(|s| (s.source_tool.as_str(), s.title.as_str())).collect::<Vec<_>>(), [("codex", "新しい質問 です"), ("claude_code", "古い会話")]);
        assert_eq!(recent[0].last_question.as_deref(), Some("新しい質問\nです"));
        assert_eq!(recent[0].last_answer, None);
        // 件数の上限
        assert_eq!(recent_sessions(&conn, "/work/SyncLane", 1)?.len(), 1);
        // 最後の質問と、最後の回答
        let login = recent_sessions(&conn, "/a/app", 5)?;
        assert_eq!((login[0].last_question.as_deref(), login[0].last_answer.as_deref()), (Some("ログインを直して"), Some("直しました")));
        assert!(recent_sessions(&conn, "/nothing", 5)?.is_empty());
        Ok(())
    }

    #[test]
    fn names_are_the_last_folder_and_get_the_parent_only_when_they_collide() {
        let names = display_names(&["/work/SyncLane", "/a/app", "/b/app", "/c/solo/", r"C:\Users\x\proj"]);
        assert_eq!(names["/work/SyncLane"], "SyncLane");
        assert_eq!(names["/a/app"], "a/app");
        assert_eq!(names["/b/app"], "b/app");
        assert_eq!(names["/c/solo/"], "solo");
        assert_eq!(names[r"C:\Users\x\proj"], "proj");
        // 場所が分からない会話は、そのまま
        assert_eq!(display_names(&["(unknown)"])["(unknown)"], "(unknown)");
    }

    #[test]
    fn sessions_by_tool_groups_and_sorts_correctly() -> Result<()> {
        let conn = db::open_memory()?;
        // claude_code の会話 (最終 10:00)
        let c_sid = conn.query_row(
            "INSERT INTO sessions (source_tool, project_path, started_at) VALUES ('claude_code', '/work/A', '2026-11-17 09:00:00') RETURNING id",
            [],
            |r| r.get::<_, i64>(0),
        )?;
        conn.execute(
            "INSERT INTO events (session_id, source_tool, source_event_id, occurred_at, role, content, sequence)
             VALUES (?1, 'claude_code', 'ev1', '2026-11-17 10:00:00', 'user', 'Claudeの質問', 1)",
            params![c_sid],
        )?;

        // codex の会話 (最終 12:00 -> codex の方が新しいので先頭になる)
        let cx_sid = conn.query_row(
            "INSERT INTO sessions (source_tool, project_path, started_at) VALUES ('codex', '/work/B', '2026-11-17 11:00:00') RETURNING id",
            [],
            |r| r.get::<_, i64>(0),
        )?;
        conn.execute(
            "INSERT INTO events (session_id, source_tool, source_event_id, occurred_at, role, content, sequence)
             VALUES (?1, 'codex', 'ev2', '2026-11-17 12:00:00', 'user', 'Codexの質問', 1)",
            params![cx_sid],
        )?;

        let groups = sessions_by_tool(&conn)?;
        assert_eq!(groups.len(), 2);
        // 最新会話のある codex が先頭
        assert_eq!(groups[0].source_tool, "codex");
        assert_eq!(groups[0].sessions[0].title, "Codexの質問");
        assert_eq!(groups[1].source_tool, "claude_code");
        assert_eq!(groups[1].sessions[0].title, "Claudeの質問");
        Ok(())
    }

    #[test]
    fn last_event_id_picks_higher_sequence_when_occurred_at_is_identical() -> Result<()> {
        let conn = db::open_memory()?;
        let sid = conn.query_row(
            "INSERT INTO sessions (source_tool, project_path, started_at) VALUES ('codex', '/work/test', '2026-11-17 10:00:00') RETURNING id",
            [],
            |r| r.get::<_, i64>(0),
        )?;
        // 同一時刻で sequence 1 と 2 の発言
        conn.execute(
            "INSERT INTO events (session_id, source_tool, source_event_id, occurred_at, role, content, sequence)
             VALUES (?1, 'codex', 'ev_seq1', '2026-11-17 10:00:00', 'user', '質問1', 1)",
            params![sid],
        )?;
        let ev2_id = conn.query_row(
            "INSERT INTO events (session_id, source_tool, source_event_id, occurred_at, role, content, sequence)
             VALUES (?1, 'codex', 'ev_seq2', '2026-11-17 10:00:00', 'assistant', '回答2', 2) RETURNING id",
            params![sid],
            |r| r.get::<_, i64>(0),
        )?;

        let groups = sessions_by_tool(&conn)?;
        assert_eq!(groups[0].sessions[0].last_event_id, ev2_id);
        Ok(())
    }

    #[test]
    fn timeline_days_filters_by_project_and_orders_descending() -> Result<()> {
        let conn = db::open_memory()?;
        // プロジェクトA: 11/17 (2件), 11/15 (1件)
        let s_a = conn.query_row(
            "INSERT INTO sessions (source_tool, project_path) VALUES ('codex', '/work/ProjA') RETURNING id",
            [],
            |r| r.get::<_, i64>(0),
        )?;
        conn.execute(
            "INSERT INTO events (session_id, source_tool, source_event_id, occurred_at, role, content, sequence)
             VALUES (?1, 'codex', 'a1', '2026-11-17 10:00:00', 'user', 'msg', 1)",
            params![s_a],
        )?;
        conn.execute(
            "INSERT INTO events (session_id, source_tool, source_event_id, occurred_at, role, content, sequence)
             VALUES (?1, 'codex', 'a2', '2026-11-17 11:00:00', 'assistant', 'msg', 2)",
            params![s_a],
        )?;
        conn.execute(
            "INSERT INTO events (session_id, source_tool, source_event_id, occurred_at, role, content, sequence)
             VALUES (?1, 'codex', 'a3', '2026-11-15 09:00:00', 'user', 'msg', 3)",
            params![s_a],
        )?;

        // プロジェクトB: 11/16 (1件)
        let s_b = conn.query_row(
            "INSERT INTO sessions (source_tool, project_path) VALUES ('codex', '/work/ProjB') RETURNING id",
            [],
            |r| r.get::<_, i64>(0),
        )?;
        conn.execute(
            "INSERT INTO events (session_id, source_tool, source_event_id, occurred_at, role, content, sequence)
             VALUES (?1, 'codex', 'b1', '2026-11-16 14:00:00', 'user', 'msg', 1)",
            params![s_b],
        )?;

        // プロジェクトA指定: 11/17 (2件) と 11/15 (1件)
        let days_a = timeline_days(&conn, Some("/work/ProjA"))?;
        assert_eq!(days_a.len(), 2);
        assert_eq!(days_a[0].day, "2026-11-17");
        assert_eq!(days_a[0].count, 2);
        assert_eq!(days_a[1].day, "2026-11-15");
        assert_eq!(days_a[1].count, 1);

        // 指定なし(全体): 11/17 (2件), 11/16 (1件), 11/15 (1件) が降順で返る
        let days_all = timeline_days(&conn, None)?;
        assert_eq!(days_all.len(), 3);
        assert_eq!(days_all[0].day, "2026-11-17");
        assert_eq!(days_all[0].count, 2);
        assert_eq!(days_all[1].day, "2026-11-16");
        assert_eq!(days_all[1].count, 1);
        assert_eq!(days_all[2].day, "2026-11-15");
        assert_eq!(days_all[2].count, 1);

        Ok(())
    }
}
