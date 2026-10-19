use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use std::collections::HashMap;

/// ブラウザ拡張から届く、Web 版の会話。
pub const GEMINI_WEB_TOOL: &str = "gemini_web";
pub const CHATGPT_WEB_TOOL: &str = "chatgpt_web";
const SOURCES: &[&str] = &[GEMINI_WEB_TOOL, CHATGPT_WEB_TOOL];

/// 会話の置き場所(画面では、プロジェクトの欄に出る)。
fn host(source: &str) -> &'static str {
    if source == CHATGPT_WEB_TOOL { "chatgpt.com" } else { "gemini.google.com" }
}

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
    /// 画面が発言ごとに持つ ID(ChatGPT の data-message-id)。あれば、それを ID に使う。
    #[serde(default)]
    pub id: Option<String>,
    /// 画面が持つ、その発言の時刻(ChatGPT の <time datetime>)。あれば、取り込んだ時刻の代わりに使う。
    #[serde(default)]
    pub at: Option<String>,
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
    if parse_iso_ms(&payload.captured_at).is_none() {
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

/// FNV-1a(64bit)。版が変わっても同じ値になる(DefaultHasher は保証がないので使わない)。
fn fnv(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

struct Item<'a> {
    id: String,
    role: &'a str,
    text: &'a str,
    at: Option<i64>,
}

/// 画面の並びの番号には頼らず、発言の中身から ID を作る。
/// Gemini は長い会話の古い発言を、あとから足すことがある。番号だと、そのときずれる。
///   あなたの発言: 文面のハッシュ + 同じ文面の何回目か
///   Gemini の回答: 直前のあなたの発言に紐づける(+ その発言への何番目の回答か)
fn items<'a>(conversation_id: &str, messages: &'a [Message], captured_ms: i64) -> Vec<Item<'a>> {
    // 画面の時刻は、読み取れて、取り込んだ時刻より1日以上先でないものだけ使う
    let at_of = |message: &Message| message.at.as_deref().and_then(parse_iso_ms).filter(|ms| *ms <= captured_ms + 86_400_000);
    let own_id = |message: &Message| -> Option<String> {
        let id = message.id.as_deref()?;
        let ok = !id.is_empty() && id.len() <= 100 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        ok.then(|| format!("{conversation_id}:m:{id}"))
    };
    let mut out = Vec::new();
    let mut seen: HashMap<u64, usize> = HashMap::new();
    let mut turn: Option<(u64, usize)> = None;
    let mut answers = 0;
    for message in messages {
        let text = message.text.trim();
        if text.is_empty() {
            continue;
        }
        let id = if message.role == "user" {
            let hash = fnv(text);
            let nth = seen.entry(hash).or_insert(0);
            let id = own_id(message).unwrap_or_else(|| format!("{conversation_id}:u:{hash:016x}:{nth}"));
            turn = Some((hash, *nth));
            *nth += 1;
            answers = 0;
            id
        } else {
            let id = own_id(message).unwrap_or_else(|| match turn {
                Some((hash, nth)) => format!("{conversation_id}:a:{hash:016x}:{nth}:{answers}"),
                None => format!("{conversation_id}:a:start:{answers}"),
            });
            answers += 1;
            id
        };
        out.push(Item { id, role: if message.role == "user" { "user" } else { "assistant" }, text, at: at_of(message) });
    }
    out
}

pub fn ingest(conn: &mut Connection, payload: &Payload) -> Result<IngestReport> {
    validate(payload)?;
    let tool = payload.source.as_str();
    let source_file = format!("{}/{}", host(tool), payload.conversation_id);
    let captured_ms = parse_iso_ms(&payload.captured_at).expect("validated");

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
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![tool, host(tool), source_file, payload.conversation_id, payload.captured_at],
            )?;
            tx.last_insert_rowid()
        }
    };

    // 最初の試作で使った、番号の ID(`…:0:user` の形)の行は消して、入れ直す
    tx.execute(
        "DELETE FROM events WHERE session_id = ?1 AND (source_event_id LIKE '%:user' OR source_event_id LIKE '%:assistant')",
        params![session_id],
    )?;

    struct Known {
        occurred_at: String,
        sequence: i64,
        content: String,
    }
    let mut known: HashMap<String, Known> = HashMap::new();
    {
        let mut stmt = tx.prepare("SELECT source_event_id, occurred_at, sequence, content FROM events WHERE session_id = ?1")?;
        let rows = stmt.query_map(params![session_id], |row| {
            Ok((row.get::<_, String>(0)?, Known { occurred_at: row.get(1)?, sequence: row.get(2)?, content: row.get(3)? }))
        })?;
        for row in rows {
            let (id, value) = row?;
            known.insert(id, value);
        }
    }
    let max_sequence = known.values().map(|k| k.sequence).max();

    let items = items(&payload.conversation_id, &payload.messages, captured_ms);
    let first_known = items.iter().position(|item| known.contains_key(&item.id));
    let last_known = items.iter().rposition(|item| known.contains_key(&item.id));

    // 画面の並びの番号(sequence)は、すでにある発言を基準にそろえる。基準が無ければ、末尾に足す
    let offset = match first_known {
        Some(f) => known[&items[f].id].sequence - f as i64,
        None => max_sequence.map_or(0, |max| max + 1),
    };

    // 時刻: すでにある発言は変えない。画面が時刻を持つ新しい発言は、その時刻にする(その直後の回答は +1ms)。
    // あとから読み込まれた古い発言は、基準より前に、1ms ずつずらして置く
    // (画面は時刻順に並べるので、こうすると会話の順番が保たれる)
    let known_ms = |id: &str| parse_iso_ms(&known[id].occurred_at).unwrap_or(captured_ms);
    let n = items.len();
    let is_known = |i: usize| known.contains_key(&items[i].id);
    let from_at: Vec<bool> = (0..n).map(|i| !is_known(i) && items[i].at.is_some()).collect();
    let mut times = vec![0i64; n];
    for i in 0..n {
        if from_at[i] {
            times[i] = items[i].at.expect("from_at");
        }
    }
    match first_known {
        Some(f) => {
            times[f] = known_ms(&items[f].id);
            for i in (0..f).rev() {
                if !from_at[i] {
                    times[i] = times[i + 1] - 1;
                }
            }
            for i in f + 1..n {
                if is_known(i) {
                    times[i] = known_ms(&items[i].id);
                } else if from_at[i] {
                } else if from_at[i - 1] || last_known.is_some_and(|l| i < l) {
                    times[i] = times[i - 1] + 1;
                } else {
                    times[i] = captured_ms.max(times[i - 1] + 1);
                }
            }
        }
        None => {
            for i in 0..n {
                if !from_at[i] {
                    times[i] = if i > 0 && from_at[i - 1] { times[i - 1] + 1 } else { captured_ms + i as i64 };
                }
            }
        }
    }

    let mut report = IngestReport::default();
    for (i, item) in items.iter().enumerate() {
        let sequence = i as i64 + offset;
        match known.get(&item.id) {
            None => {
                tx.execute(
                    "INSERT INTO events (session_id, source_tool, source_event_id, occurred_at, role, content, sequence)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![session_id, tool, item.id, format_iso_ms(times[i]), item.role, item.text, sequence],
                )?;
                report.inserted += 1;
            }
            Some(old) => {
                if old.content != item.text {
                    tx.execute(
                        "UPDATE events SET content = ?3 WHERE source_tool = ?1 AND source_event_id = ?2",
                        params![tool, item.id, item.text],
                    )?;
                    report.updated += 1;
                }
                if old.sequence != sequence {
                    tx.execute(
                        "UPDATE events SET sequence = ?3 WHERE source_tool = ?1 AND source_event_id = ?2",
                        params![tool, item.id, sequence],
                    )?;
                }
            }
        }
    }
    if let Some(title) = payload.title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        tx.execute(
            "UPDATE sessions SET title = ?2 WHERE id = ?1 AND COALESCE(title, '') <> ?2
               AND NOT EXISTS (SELECT 1 FROM settings WHERE key = 'title_manual:' || ?1)",
            params![session_id, title],
        )?;
    }
    tx.commit()?;
    Ok(report)
}

// ---- 時刻(ISO 8601、`2026-10-16T01:00:00.000Z` の形)をミリ秒で足し引きする ----

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn parse_iso_ms(text: &str) -> Option<i64> {
    let text = text.strip_suffix('Z')?;
    let (date_time, fraction) = match text.split_once('.') {
        Some((head, tail)) => (head, Some(tail)),
        None => (text, None),
    };
    let (date, time) = date_time.split_once('T')?;
    let mut d = date.split('-');
    let (y, m, day): (i64, i64, i64) = (d.next()?.parse().ok()?, d.next()?.parse().ok()?, d.next()?.parse().ok()?);
    let mut t = time.split(':');
    let (h, mi, sec): (i64, i64, i64) = (t.next()?.parse().ok()?, t.next()?.parse().ok()?, t.next()?.parse().ok()?);
    if d.next().is_some() || t.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&day) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let ms = match fraction {
        None => 0,
        Some(f) if !f.is_empty() && f.len() <= 9 && f.bytes().all(|b| b.is_ascii_digit()) => {
            format!("{:0<3}", &f[..f.len().min(3)]).parse::<i64>().ok()?
        }
        _ => return None,
    };
    Some(((days_from_civil(y, m, day) * 24 + h) * 60 + mi) * 60_000 + sec * 1000 + ms)
}

fn format_iso_ms(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let rest = ms.rem_euclid(86_400_000);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3_600_000,
        rest / 60_000 % 60,
        rest / 1000 % 60,
        rest % 1000
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn payload_at(at: &str, messages: &[(&str, &str)]) -> Payload {
        Payload {
            source: GEMINI_WEB_TOOL.into(),
            conversation_id: "abc123".into(),
            title: Some("フリーWi-Fi".into()),
            captured_at: at.into(),
            messages: messages
                .iter()
                .enumerate()
                .map(|(index, (role, text))| Message { index: index as i64, role: (*role).into(), text: (*text).into(), id: None, at: None })
                .collect(),
        }
    }

    fn payload(messages: &[(&str, &str)]) -> Payload {
        payload_at("2026-10-16T01:00:00.000Z", messages)
    }

    /// 画面と同じ並び(時刻、同じなら ID)で、中身を取り出す。
    fn ordered(conn: &Connection) -> Vec<String> {
        let mut stmt = conn.prepare("SELECT content FROM events ORDER BY occurred_at ASC, id ASC").unwrap();
        stmt.query_map([], |r| r.get::<_, String>(0)).unwrap().map(|r| r.unwrap()).collect()
    }

    #[test]
    fn inserts_once_and_is_idempotent() -> Result<()> {
        let mut conn = db::open_memory()?;
        let p = payload(&[("user", "質問"), ("assistant", "回答")]);
        assert_eq!(ingest(&mut conn, &p)?, IngestReport { inserted: 2, updated: 0 });
        assert_eq!(ingest(&mut conn, &p)?, IngestReport::default());
        let counts = db::counts(&conn)?;
        assert_eq!((counts.sessions, counts.events), (1, 2));
        Ok(())
    }

    #[test]
    fn updates_a_growing_answer_and_adds_new_turns() -> Result<()> {
        let mut conn = db::open_memory()?;
        ingest(&mut conn, &payload(&[("user", "質問"), ("assistant", "回")]))?;
        let report = ingest(&mut conn, &payload_at("2026-10-16T01:00:05.000Z", &[("user", "質問"), ("assistant", "回答です"), ("user", "続き")]))?;
        assert_eq!(report, IngestReport { inserted: 1, updated: 1 });
        assert_eq!(ordered(&conn), vec!["質問", "回答です", "続き"]);
        Ok(())
    }

    #[test]
    fn older_turns_loaded_later_keep_the_conversation_order() -> Result<()> {
        let mut conn = db::open_memory()?;
        // 先に、新しい 2 往復だけが見えていた
        ingest(&mut conn, &payload(&[("user", "Q3"), ("assistant", "A3"), ("user", "Q4"), ("assistant", "A4")]))?;
        // スクロールで、古い 2 往復が足された
        let report = ingest(
            &mut conn,
            &payload_at(
                "2026-10-16T01:05:00.000Z",
                &[("user", "Q1"), ("assistant", "A1"), ("user", "Q2"), ("assistant", "A2"), ("user", "Q3"), ("assistant", "A3"), ("user", "Q4"), ("assistant", "A4")],
            ),
        )?;
        assert_eq!(report, IngestReport { inserted: 4, updated: 0 });
        assert_eq!(ordered(&conn), vec!["Q1", "A1", "Q2", "A2", "Q3", "A3", "Q4", "A4"]);
        // 開き直して、また新しい分だけが見えても、増えず、順番も崩れない
        let report = ingest(&mut conn, &payload_at("2026-10-16T02:00:00.000Z", &[("user", "Q4"), ("assistant", "A4"), ("user", "Q5")]))?;
        assert_eq!(report, IngestReport { inserted: 1, updated: 0 });
        assert_eq!(ordered(&conn), vec!["Q1", "A1", "Q2", "A2", "Q3", "A3", "Q4", "A4", "Q5"]);
        assert_eq!(db::counts(&conn)?.events, 9);
        Ok(())
    }

    #[test]
    fn identical_messages_stay_separate() -> Result<()> {
        let mut conn = db::open_memory()?;
        let p = payload(&[("user", "はい"), ("assistant", "了解"), ("user", "はい"), ("assistant", "了解")]);
        assert_eq!(ingest(&mut conn, &p)?.inserted, 4);
        assert_eq!(ingest(&mut conn, &p)?, IngestReport::default());
        Ok(())
    }

    #[test]
    fn removes_rows_from_the_old_numbered_format() -> Result<()> {
        let mut conn = db::open_memory()?;
        ingest(&mut conn, &payload(&[("user", "質問")]))?;
        let session: i64 = conn.query_row("SELECT id FROM sessions", [], |r| r.get(0))?;
        conn.execute(
            "INSERT INTO events (session_id, source_tool, source_event_id, occurred_at, role, content, sequence) VALUES (?1, 'gemini_web', 'abc123:0:user', '2026-10-16T00:00:00.000Z', 'user', '質問', 0)",
            [session],
        )?;
        ingest(&mut conn, &payload(&[("user", "質問")]))?;
        assert_eq!(db::counts(&conn)?.events, 1);
        Ok(())
    }

    #[test]
    fn saves_title_and_is_searchable_as_gemini_web() -> Result<()> {
        let mut conn = db::open_memory()?;
        ingest(&mut conn, &payload(&[("user", "FreeWifiでAIチャットをすると？")]))?;
        let events = db::timeline(&conn, None, Some("FreeWifi"), 10)?;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].source_tool, "gemini_web");
        assert_eq!(events[0].title.as_deref(), Some("フリーWi-Fi"));
        Ok(())
    }

    #[test]
    fn rejects_bad_payloads_without_writing() -> Result<()> {
        let mut conn = db::open_memory()?;
        let mut p = payload(&[("user", "x")]);
        p.source = "evil".into();
        assert!(ingest(&mut conn, &p).is_err());
        let p = payload(&[("system", "x")]);
        assert!(ingest(&mut conn, &p).is_err());
        let mut p = payload(&[("user", "x")]);
        p.conversation_id = "../etc".into();
        assert!(ingest(&mut conn, &p).is_err());
        let mut p = payload(&[("user", "x")]);
        p.captured_at = "now'; DROP TABLE events;--".into();
        assert!(ingest(&mut conn, &p).is_err());
        assert_eq!(db::counts(&conn)?.sessions, 0);
        Ok(())
    }

    #[test]
    fn iso_time_round_trips_and_shifts_across_days() {
        let ms = parse_iso_ms("2026-10-16T00:00:00.000Z").unwrap();
        assert_eq!(format_iso_ms(ms), "2026-10-16T00:00:00.000Z");
        assert_eq!(format_iso_ms(ms - 1), "2026-10-15T23:59:59.999Z");
        assert_eq!(format_iso_ms(parse_iso_ms("2024-02-29T12:34:56Z").unwrap()), "2024-02-29T12:34:56.000Z");
        assert_eq!(parse_iso_ms("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(parse_iso_ms("2026-13-01T00:00:00Z"), None);
        assert_eq!(parse_iso_ms("2026-10-16 00:00:00"), None);
    }

    #[test]
    fn uses_the_screens_own_message_ids_when_given() -> Result<()> {
        let mut conn = db::open_memory()?;
        let with_ids = |at: &str, rows: &[(&str, &str, &str)]| Payload {
            source: CHATGPT_WEB_TOOL.into(),
            conversation_id: "11111111-2222-3333-4444-555555555555".into(),
            title: Some("題".into()),
            captured_at: at.into(),
            messages: rows
                .iter()
                .enumerate()
                .map(|(i, (id, role, text))| Message { index: i as i64, role: (*role).into(), text: (*text).into(), id: Some((*id).into()), at: None })
                .collect(),
        };
        ingest(&mut conn, &with_ids("2026-10-16T01:00:00.000Z", &[("m3", "user", "同じ"), ("m4", "assistant", "答え")]))?;
        // 古い発言があとから足され、同じ文面の発言もあるが、ID が違うので別々に残る
        let report = ingest(
            &mut conn,
            &with_ids("2026-10-16T01:05:00.000Z", &[("m1", "user", "同じ"), ("m2", "assistant", "古い答え"), ("m3", "user", "同じ"), ("m4", "assistant", "答え")]),
        )?;
        assert_eq!(report, IngestReport { inserted: 2, updated: 0 });
        assert_eq!(ordered(&conn), vec!["同じ", "古い答え", "同じ", "答え"]);
        let (tool, project): (String, String) =
            conn.query_row("SELECT source_tool, project_path FROM sessions", [], |r| Ok((r.get(0)?, r.get(1)?)))?;
        assert_eq!((tool.as_str(), project.as_str()), ("chatgpt_web", "chatgpt.com"));
        Ok(())
    }

    #[test]
    fn uses_the_times_the_screen_shows() -> Result<()> {
        let mut conn = db::open_memory()?;
        let msg = |index: i64, role: &str, text: &str, id: &str, at: Option<&str>| Message {
            index,
            role: role.into(),
            text: text.into(),
            id: Some(id.into()),
            at: at.map(Into::into),
        };
        let p = Payload {
            source: CHATGPT_WEB_TOOL.into(),
            conversation_id: "conv-1111".into(),
            title: None,
            captured_at: "2026-10-16T01:00:00.000Z".into(),
            messages: vec![
                msg(0, "user", "5月の質問", "u1", Some("2026-05-12T01:29:15.635Z")),
                msg(1, "assistant", "5月の答え", "a1", None),
                msg(2, "user", "10月の質問", "u2", Some("2026-10-15T09:00:00.000Z")),
                msg(3, "assistant", "10月の答え", "a2", None),
                msg(4, "user", "未来の時刻", "u3", Some("2099-01-01T00:00:00.000Z")), // 取り込んだ時刻より先のものは使わない
            ],
        };
        ingest(&mut conn, &p)?;
        let rows: Vec<(String, String)> = conn
            .prepare("SELECT content, occurred_at FROM events ORDER BY occurred_at ASC, id ASC")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        assert_eq!(rows[0], ("5月の質問".into(), "2026-05-12T01:29:15.635Z".into()));
        assert_eq!(rows[1], ("5月の答え".into(), "2026-05-12T01:29:15.636Z".into())); // 直後の回答は +1ms
        assert_eq!(rows[2], ("10月の質問".into(), "2026-10-15T09:00:00.000Z".into()));
        assert_eq!(rows[3], ("10月の答え".into(), "2026-10-15T09:00:00.001Z".into()));
        assert_eq!(rows[4].0, "未来の時刻");
        assert!(rows[4].1.starts_with("2026-10-16T01:00:00")); // 取り込んだ時刻(回答の直後ではなく、末尾)
        Ok(())
    }

    #[test]
    fn a_manual_title_is_not_overwritten_when_the_page_title_changes() -> Result<()> {
        let mut conn = db::open_memory()?;
        ingest(&mut conn, &payload(&[("user", "質問")]))?;
        let id: i64 = conn.query_row("SELECT id FROM sessions", [], |r| r.get(0))?;
        crate::titles::set_manual(&conn, id, "自分で付けた題名")?;
        // 画面側の題名が変わって、同じ会話を送り直しても、手動の題名は残る
        let mut again = payload(&[("user", "質問"), ("assistant", "回答")]);
        again.title = Some("ページ側の新しい題名".into());
        ingest(&mut conn, &again)?;
        let title: String = conn.query_row("SELECT title FROM sessions WHERE id = ?1", [id], |r| r.get(0))?;
        assert_eq!(title, "自分で付けた題名");
        Ok(())
    }
}
