//! 送信の本体。選んだ会話の発言を、まとめてサーバーへ送る。
//!
//! 守ること:
//! - 同じ内容を何度送ってもよい(サーバーが冪等に受ける)。通信が切れたら、同じ batch_id と本文で送り直す。
//! - 送れなくても、収集と手元の検索は止めない。止まるのは、この送信だけ。
//! - 送った印は、バッチを組んだ時刻で付ける(送っている間に本文が更新されたら、次回に送り直せる)。

use anyhow::{Result, anyhow};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::time::Duration;

use super::api::{Response, Transport};
use super::device_flow::platform;
use super::now_iso;
use super::parse_time;
use super::secrets::TokenStore;
use super::store::{self, Mode, PendingEvent, TokenMeta, load_connection};

/// 1 バッチの発言の数と、本文の大きさの目安(サーバーの上限は 1,000 件・展開後 8 MB)
pub const BATCH_EVENTS: usize = 500;
pub const BATCH_BYTES: usize = 4 * 1024 * 1024;
/// 1 件の本文の上限(文字数)。超えたら切って、切ったことを伝える
pub const CONTENT_CHARS: usize = 262_144;
const RETRIES: u32 = 3;
const MAX_REJECTED_SAMPLES: usize = 20;

/// 送信を止めた理由。止まっても、収集と手元の検索は続く。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stop {
    /// トークンが使えない。つなぎ直し(再認証)が要る
    NeedsAuth(String),
    /// 権限がない、または端末が失効している
    Forbidden(String),
    /// アプリかサーバーの更新が要る
    UpdateRequired(String),
    /// 送りすぎ。指定の秒数あとに、もう一度
    RateLimited(u64),
    /// サーバーに届かない、またはサーバーの障害。あとで自動でやり直す
    Unavailable(String),
    /// そのほかの誤り
    Error(String),
}

impl Stop {
    pub fn message(&self) -> String {
        match self {
            Stop::NeedsAuth(m) | Stop::Forbidden(m) | Stop::UpdateRequired(m) | Stop::Unavailable(m) | Stop::Error(m) => m.clone(),
            Stop::RateLimited(s) => format!("送りすぎです。{s} 秒あとにやり直します"),
        }
    }
}

#[derive(Debug, Default)]
pub struct Report {
    pub batches: usize,
    pub events_sent: usize,
    pub created: u64,
    pub updated: u64,
    pub duplicate: u64,
    pub rejected: u64,
    pub rejected_samples: Vec<String>,
    pub warnings: u64,
    /// まだ送っていない発言の数(実行のあと)
    pub pending: i64,
    pub stop: Option<Stop>,
}

#[derive(Debug, Default, Clone)]
pub struct Options {
    /// 送らずに、送る対象の数だけを調べる
    pub dry_run: bool,
    /// このバッチ数で止める(None なら、送り終えるまで)
    pub max_batches: Option<usize>,
}

struct Batch {
    batch_id: String,
    body: String,
    event_ids: Vec<i64>,
    session_ids: Vec<i64>,
    built_at: String,
}

pub fn sha256_hex(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn truncate_chars(text: &str, max: usize) -> (String, bool) {
    if text.chars().count() <= max {
        return (text.to_string(), false);
    }
    (text.chars().take(max).collect(), true)
}

/// 会話の情報(サーバーの `sessions`)。
fn session_meta(conn: &Connection, server: &str, session_id: i64) -> Result<Option<(String, Value)>> {
    let row = conn
        .query_row(
            "SELECT s.source_tool, COALESCE(s.source_hash, 'file:' || COALESCE(s.source_file, '')), s.project_path, s.title,
                    COALESCE(s.started_at, (SELECT MIN(occurred_at) FROM events WHERE session_id = s.id)),
                    COALESCE(s.ended_at, (SELECT MAX(occurred_at) FROM events WHERE session_id = s.id)),
                    (SELECT visibility FROM sync_sessions WHERE server_id = ?2 AND session_id = s.id)
             FROM sessions s WHERE s.id = ?1",
            params![session_id, server],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            },
        )
        .optional()?;
    let Some((tool, source_session_id, path, title, started, ended, visibility)) = row else { return Ok(None) };
    let Some(started) = started else { return Ok(None) }; // 発言が 1 件も無い会話は送らない
    let mut meta = json!({
        "source_tool": tool,
        "source_session_id": source_session_id,
        "project_path": path,
        "title": title,
        "started_at": started,
        "ended_at": ended,
    });
    if let Some(visibility) = visibility {
        meta["visibility"] = json!(visibility);
    }
    Ok(Some((source_session_id, meta)))
}

fn event_json(event: &PendingEvent, source_session_id: &str) -> Value {
    let (content, truncated) = truncate_chars(&event.content, CONTENT_CHARS);
    let mut value = json!({
        "source_tool": event.source_tool,
        "source_session_id": source_session_id,
        "source_event_id": event.source_event_id,
        "occurred_at": event.occurred_at,
        "kind": "message", // ローカルには、会話(user / assistant)だけを保存している
        "role": event.role,
        "content_hash": format!("sha256:{}", sha256_hex(&content)),
        "content": content,
    });
    if truncated {
        value["truncated"] = json!(true);
    }
    value
}

/// 次に送るバッチを組む。送るものが無ければ None。組んだら、送る前に outbox へ保存する。
fn build_batch(conn: &Connection, server: &str, mode: Mode, max_events: usize) -> Result<Option<Batch>> {
    let built_at = now_iso();
    let mut events = store::pending_events(conn, server, mode, max_events)?;

    // 本文の大きさで切る(最低でも 1 件は入れる)
    let mut bytes = 0usize;
    let mut keep = 0usize;
    for event in &events {
        bytes += event.content.len();
        keep += 1;
        if bytes >= BATCH_BYTES {
            break;
        }
    }
    events.truncate(keep.max(events.len().min(1)));

    let mut session_ids: Vec<i64> = Vec::new();
    for event in &events {
        if !session_ids.contains(&event.session_id) {
            session_ids.push(event.session_id);
        }
    }
    if events.is_empty() {
        // 発言は無いが、題名などが変わった会話があれば、会話の情報だけを送り直す
        session_ids = store::sessions_needing_meta(conn, server, mode)?;
        if session_ids.is_empty() {
            return Ok(None);
        }
    }

    let mut sessions = Vec::new();
    let mut source_ids: HashMap<i64, String> = HashMap::new();
    let mut included: Vec<i64> = Vec::new();
    for id in &session_ids {
        if let Some((source_session_id, meta)) = session_meta(conn, server, *id)? {
            source_ids.insert(*id, source_session_id);
            sessions.push(meta);
            included.push(*id);
        }
    }
    let events_json: Vec<Value> = events
        .iter()
        .filter_map(|e| source_ids.get(&e.session_id).map(|sid| event_json(e, sid)))
        .collect();
    let event_ids: Vec<i64> = events.iter().filter(|e| source_ids.contains_key(&e.session_id)).map(|e| e.id).collect();
    if sessions.is_empty() && events_json.is_empty() {
        return Ok(None);
    }

    let batch_id = uuid::Uuid::new_v4().to_string();
    let body = json!({
        "batch_id": batch_id,
        "client": {"app_version": env!("CARGO_PKG_VERSION"), "platform": platform()},
        "sessions": sessions,
        "events": events_json,
    })
    .to_string();

    conn.execute(
        "INSERT INTO sync_outbox(server_id, batch_id, body, event_ids, session_ids, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![server, batch_id, body, serde_json::to_string(&event_ids)?, serde_json::to_string(&included)?, built_at],
    )?;
    Ok(Some(Batch { batch_id, body, event_ids, session_ids: included, built_at }))
}

fn load_outbox(conn: &Connection, server: &str) -> Result<Option<Batch>> {
    let row = conn
        .query_row(
            "SELECT batch_id, body, event_ids, session_ids, created_at FROM sync_outbox WHERE server_id = ?1 ORDER BY created_at LIMIT 1",
            [server],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?, row.get::<_, String>(4)?)),
        )
        .optional()?;
    let Some((batch_id, body, event_ids, session_ids, built_at)) = row else { return Ok(None) };
    Ok(Some(Batch {
        batch_id,
        body,
        event_ids: serde_json::from_str(&event_ids)?,
        session_ids: serde_json::from_str(&session_ids)?,
        built_at,
    }))
}

fn drop_outbox(conn: &Connection, server: &str, batch_id: &str) -> Result<()> {
    conn.execute("DELETE FROM sync_outbox WHERE server_id = ?1 AND batch_id = ?2", params![server, batch_id])?;
    Ok(())
}

// ---- トークンの更新 ----

fn needs_renewal(meta: &TokenMeta) -> bool {
    let (Some(issued), Some(expires)) = (parse_time(&meta.issued_at), parse_time(&meta.expires_at)) else { return false };
    let lifetime = expires - issued;
    let remaining = expires - time::OffsetDateTime::now_utc();
    remaining <= lifetime / 4 // 残りが 4 分の 1 を切ったら更新する(認証の設計メモの 4.3)
}

/// トークンを更新する。成功したら、新しいトークンを保存して返す。
pub fn renew_token(conn: &Connection, t: &dyn Transport, store: &dyn TokenStore, server: &str, scope: &str) -> Result<String> {
    let current = store.get(server, scope)?.ok_or_else(|| anyhow!("{scope} のトークンがありません"))?;
    let response = t.request("POST", &format!("{server}/api/v1/auth/renew"), Some(&current), Some(b"{}"), false)?;
    if !response.ok() {
        return Err(anyhow!("トークンを更新できません({} {})", response.status, response.error()));
    }
    let token = response.body["token"].as_str().ok_or_else(|| anyhow!("更新の応答にトークンがありません"))?.to_string();
    let expires_at = response.body["expires_at"].as_str().unwrap_or("").to_string();
    store.set(server, scope, &token)?;
    store::set_token_meta(conn, scope, &TokenMeta { issued_at: now_iso(), expires_at })?;
    Ok(token)
}

/// 期限が近いトークンを更新する(できる範囲で。失敗しても、まだ使えるうちは送信を続ける)。
pub fn maintain_tokens(conn: &Connection, t: &dyn Transport, store: &dyn TokenStore, server: &str) {
    for scope in ["ingest", "search"] {
        let due = store::token_meta(conn, scope).ok().flatten().is_some_and(|m| needs_renewal(&m));
        if due {
            let _ = renew_token(conn, t, store, server, scope);
        }
    }
}

// ---- 送信 ----

fn send_once(t: &dyn Transport, server: &str, token: &str, batch: &Batch, sleep: &dyn Fn(Duration)) -> Result<Response> {
    let url = format!("{server}/api/v1/ingest/batches");
    let mut delay = 1;
    let mut last = None;
    for attempt in 0..RETRIES {
        match t.request("POST", &url, Some(token), Some(batch.body.as_bytes()), true) {
            Ok(response) if response.status < 500 => return Ok(response),
            Ok(response) => last = Some(anyhow!("サーバーの障害です({})", response.status)),
            Err(error) => last = Some(error),
        }
        if attempt + 1 < RETRIES {
            sleep(Duration::from_secs(delay));
            delay *= 2;
        }
    }
    Err(last.unwrap_or_else(|| anyhow!("送信に失敗しました")))
}

fn add_counts(report: &mut Report, body: &Value) {
    let events = &body["events"];
    report.created += events["created"].as_u64().unwrap_or(0);
    report.updated += events["updated"].as_u64().unwrap_or(0);
    report.duplicate += events["duplicate"].as_u64().unwrap_or(0);
    report.rejected += events["rejected"].as_u64().unwrap_or(0);
    report.warnings += body["warnings"].as_array().map_or(0, |w| w.len() as u64);
    if let Some(list) = body["rejected"].as_array() {
        for item in list {
            if report.rejected_samples.len() < MAX_REJECTED_SAMPLES {
                let id = item["source_event_id"].as_str().or(item["source_session_id"].as_str()).unwrap_or("?");
                report.rejected_samples.push(format!("{id}: {}", item["reason"].as_str().unwrap_or("?")));
            }
        }
    }
}

/// 選んだ会話の、まだ送っていない発言を、送り終えるまで(または止まるまで)送る。
pub fn run(conn: &Connection, t: &dyn Transport, store: &dyn TokenStore, sleep: &dyn Fn(Duration), opts: &Options) -> Result<Report> {
    let connection = load_connection(conn)?.ok_or_else(|| anyhow!("サーバーに接続していません(synclane server connect)"))?;
    let server = connection.server.as_str();
    let mode = store::mode(conn)?;
    let mut report = Report::default();

    if opts.dry_run {
        report.pending = store::counts(conn, server, mode)?.events_pending;
        return Ok(report);
    }

    maintain_tokens(conn, t, store, server);
    let Some(mut token) = store.get(server, "ingest")? else {
        report.stop = Some(Stop::NeedsAuth("送信のトークンがありません。つなぎ直してください".into()));
        return Ok(report);
    };

    let mut max_events = BATCH_EVENTS;
    let mut renewed = false;
    let mut conflicts = 0;

    loop {
        if opts.max_batches.is_some_and(|n| report.batches >= n) {
            break;
        }
        let batch = match load_outbox(conn, server)? {
            Some(batch) => batch,
            None => match build_batch(conn, server, mode, max_events)? {
                Some(batch) => batch,
                None => break,
            },
        };

        let response = match send_once(t, server, &token, &batch, sleep) {
            Ok(response) => response,
            Err(error) => {
                report.stop = Some(Stop::Unavailable(format!("サーバーに送れませんでした({error:#})。あとでやり直します")));
                break;
            }
        };

        match response.status {
            200..=299 => {
                add_counts(&mut report, &response.body);
                store::mark_sent(conn, server, &batch.batch_id, &batch.event_ids, &batch.session_ids, &batch.built_at)?;
                report.batches += 1;
                report.events_sent += batch.event_ids.len();
            }
            401 => {
                if !renewed {
                    renewed = true;
                    if let Ok(new_token) = renew_token(conn, t, store, server, "ingest") {
                        token = new_token;
                        continue;
                    }
                }
                report.stop = Some(Stop::NeedsAuth("トークンの期限が切れたか、失効しました。つなぎ直してください".into()));
                break;
            }
            403 => {
                report.stop = Some(Stop::Forbidden(format!("送信が許可されていません: {}", response.error_description())));
                break;
            }
            409 => {
                // 同じ batch_id で内容が違う(アプリの不具合)。新しい batch_id で組み直す。繰り返すなら止める
                drop_outbox(conn, server, &batch.batch_id)?;
                conflicts += 1;
                if conflicts > 1 {
                    report.stop = Some(Stop::Error("batch_id が衝突し続けています".into()));
                    break;
                }
            }
            413 => {
                drop_outbox(conn, server, &batch.batch_id)?;
                if batch.event_ids.len() > 1 {
                    max_events = (batch.event_ids.len() / 2).max(1); // 半分にして送り直す
                } else {
                    // 1 件でも大きすぎる。送れないものとして印を付けて、先に進む
                    store::mark_sent(conn, server, &batch.batch_id, &batch.event_ids, &batch.session_ids, &batch.built_at)?;
                    report.rejected += batch.event_ids.len() as u64;
                    report.rejected_samples.push("1 件が大きすぎて送れませんでした".into());
                }
            }
            426 => {
                report.stop = Some(Stop::UpdateRequired("アプリが古いため、サーバーが受け付けません。アプリを更新してください".into()));
                break;
            }
            429 => {
                report.stop = Some(Stop::RateLimited(response.retry_after.unwrap_or(60)));
                break;
            }
            500.. => unreachable!("send_once が 5xx を扱う"),
            status => {
                report.stop = Some(Stop::Error(format!("サーバーが送信を受け付けません({status} {} {})", response.error(), response.error_description())));
                break;
            }
        }
    }

    report.pending = store::counts(conn, server, mode)?.events_pending;
    store::record_result(
        conn,
        &json!({
            "batches": report.batches, "events_sent": report.events_sent, "created": report.created, "updated": report.updated,
            "duplicate": report.duplicate, "rejected": report.rejected, "pending": report.pending,
            "stop": report.stop.as_ref().map(Stop::message),
        }),
    )?;
    Ok(report)
}
