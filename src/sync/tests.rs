//! 送信と登録の試験。サーバーは、メモリ上の見せかけ(FakeServer)で置き換える。

use anyhow::{Result, anyhow};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;

use super::api::{Response, Transport};
use super::device_flow::{self, ConnectError, DeviceStart, Pending};
use super::engine::{self, Options, Stop};
use super::secrets::{MemoryStore, TokenStore};
use super::store::{self, Mode, ServerConnection, TokenMeta};
use crate::db;

const SERVER: &str = "https://sync.example.com";

#[derive(Clone, Debug)]
struct Call {
    method: String,
    url: String,
    bearer: Option<String>,
    body: Value,
}

type Scripted = Result<Response>;

fn reply(status: u16, body: Value) -> Scripted {
    Ok(Response { status, body, retry_after: None })
}

/// サーバーの見せかけ。取り込みの受け方は本物(SyncLane-Server)に合わせた最小の写し。
#[derive(Default)]
struct FakeServer {
    calls: RefCell<Vec<Call>>,
    /// 取り込みの要求に、先に返す応答(使い切ったら、普通に受ける)
    script: RefCell<VecDeque<Scripted>>,
    valid_tokens: RefCell<HashSet<String>>,
    /// 更新で返すトークン(None なら、更新は 401)
    renew_to: RefCell<Option<String>>,
    /// event_key → (本文, 時刻)
    events: RefCell<HashMap<(String, String), (String, String)>>,
    /// 承認の問い合わせ(device/token)に返す応答
    device_replies: RefCell<VecDeque<Scripted>>,
}

impl FakeServer {
    fn new() -> Self {
        let server = FakeServer::default();
        server.valid_tokens.borrow_mut().insert("ingest-token".into());
        server
    }

    fn ingest_calls(&self) -> Vec<Call> {
        self.calls.borrow().iter().filter(|c| c.url.ends_with("/ingest/batches")).cloned().collect()
    }

    fn accept(&self, body: &Value) -> Value {
        let (mut created, mut updated, mut duplicate) = (0, 0, 0);
        for event in body["events"].as_array().unwrap() {
            let key = (event["source_tool"].as_str().unwrap().to_string(), event["source_event_id"].as_str().unwrap().to_string());
            let value = (event["content"].as_str().unwrap().to_string(), event["occurred_at"].as_str().unwrap().to_string());
            match self.events.borrow_mut().insert(key, value.clone()) {
                None => created += 1,
                Some(old) if old.0 != value.0 => updated += 1,
                Some(_) => duplicate += 1,
            }
        }
        json!({"batch_id": body["batch_id"], "status": "ok",
               "events": {"created": created, "updated": updated, "duplicate": duplicate, "rejected": 0},
               "rejected": [], "warnings": []})
    }
}

impl Transport for FakeServer {
    fn request(&self, method: &str, url: &str, bearer: Option<&str>, body: Option<&[u8]>, _gzip: bool) -> Result<Response> {
        let body: Value = body.map(|b| serde_json::from_slice(b).unwrap()).unwrap_or(Value::Null);
        self.calls.borrow_mut().push(Call { method: method.into(), url: url.into(), bearer: bearer.map(str::to_string), body: body.clone() });

        if url.ends_with("/auth/renew") {
            return match self.renew_to.borrow().clone() {
                Some(token) => {
                    self.valid_tokens.borrow_mut().insert(token.clone());
                    reply(200, json!({"scope": "ingest", "token": token, "expires_at": "2027-06-01T00:00:00+00:00"}))
                }
                None => reply(401, json!({"error": "invalid_token"})),
            };
        }
        if url.ends_with("/auth/device/token") {
            return self.device_replies.borrow_mut().pop_front().unwrap_or_else(|| reply(400, json!({"error": "authorization_pending"})));
        }
        if url.ends_with("/ingest/batches") {
            if let Some(scripted) = self.script.borrow_mut().pop_front() {
                return scripted;
            }
            if !bearer.is_some_and(|t| self.valid_tokens.borrow().contains(t)) {
                return reply(401, json!({"error": "invalid_token"}));
            }
            return reply(200, self.accept(&body));
        }
        Err(anyhow!("想定外の呼び出し: {method} {url}"))
    }
}

fn no_sleep(_: Duration) {}

struct Env {
    conn: Connection,
    store: MemoryStore,
    server: FakeServer,
}

fn env() -> Env {
    let conn = db::open_memory().unwrap();
    let store = MemoryStore::default();
    store.set(SERVER, "ingest", "ingest-token").unwrap();
    store
        .set(SERVER, "search", "search-token").unwrap();
    store::save_connection(&conn, &ServerConnection {
        server: SERVER.into(), device_id: "dev1".into(), user_name: "田中".into(), user_email: "tanaka@example.com".into(),
    }).unwrap();
    Env { conn, store, server: FakeServer::new() }
}

/// 会話を 1 つ作る。発言は (source_event_id, 本文) の並びで、時刻は 1 分ずつ進む。
fn add_session(conn: &Connection, tool: &str, key: &str, events: &[(&str, &str)]) -> i64 {
    conn.execute(
        "INSERT INTO sessions(source_tool, project_path, source_file, source_hash) VALUES (?1, '/work/app', ?2, ?3)",
        rusqlite::params![tool, format!("/logs/{key}.jsonl"), key],
    ).unwrap();
    let session_id = conn.last_insert_rowid();
    for (i, (event_id, content)) in events.iter().enumerate() {
        conn.execute(
            "INSERT INTO events(session_id, source_tool, source_event_id, occurred_at, role, content, sequence)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![session_id, tool, event_id, format!("2026-10-16T12:{:02}:00.000Z", i), if i % 2 == 0 { "user" } else { "assistant" }, content, i as i64],
        ).unwrap();
    }
    session_id
}

fn run(env: &Env) -> engine::Report {
    engine::run(&env.conn, &env.server, &env.store, &no_sleep, &Options::default()).unwrap()
}

fn include(env: &Env, session_id: i64) {
    store::set_session_state(&env.conn, SERVER, session_id, Some("send"), None).unwrap();
}

// ---- 何を送るか ----

#[test]
fn manual_mode_sends_only_the_sessions_you_picked() {
    let env = env();
    let a = add_session(&env.conn, "claude-code", "a", &[("a1", "質問"), ("a2", "答え")]);
    add_session(&env.conn, "claude-code", "b", &[("b1", "別の会話")]);
    include(&env, a);

    let report = run(&env);
    assert_eq!(report.events_sent, 2);
    assert_eq!(report.created, 2);
    assert_eq!(report.pending, 0);
    let sent = env.server.events.borrow();
    assert!(sent.contains_key(&("claude-code".to_string(), "a1".to_string())));
    assert!(!sent.contains_key(&("claude-code".to_string(), "b1".to_string())));
}

#[test]
fn nothing_is_sent_by_default() {
    let env = env();
    add_session(&env.conn, "claude-code", "a", &[("a1", "質問")]);
    let report = run(&env);
    assert_eq!(report.batches, 0);
    assert!(env.server.ingest_calls().is_empty());
}

#[test]
fn auto_mode_sends_everything_except_excluded() {
    let env = env();
    add_session(&env.conn, "claude-code", "a", &[("a1", "一つ目")]);
    let secret = add_session(&env.conn, "claude-code", "s", &[("s1", "送りたくない")]);
    add_session(&env.conn, "gemini_web", "g", &[("g1", "三つ目")]);
    store::set_mode(&env.conn, Mode::Auto).unwrap();
    store::set_session_state(&env.conn, SERVER, secret, Some("excluded"), None).unwrap();

    let report = run(&env);
    assert_eq!(report.created, 2);
    assert!(!env.server.events.borrow().contains_key(&("claude-code".to_string(), "s1".to_string())));
}

#[test]
fn include_all_does_not_override_exclusions() {
    let env = env();
    let a = add_session(&env.conn, "claude-code", "a", &[("a1", "x")]);
    let b = add_session(&env.conn, "claude-code", "b", &[("b1", "y")]);
    store::set_session_state(&env.conn, SERVER, b, Some("excluded"), None).unwrap();
    store::include_all(&env.conn, SERVER, Some("org")).unwrap();
    let state = |id| env.conn.query_row("SELECT state FROM sync_sessions WHERE session_id = ?1", [id], |r| r.get::<_, String>(0)).unwrap();
    assert_eq!(state(a), "send");
    assert_eq!(state(b), "excluded");
}

// ---- バッチの中身 ----

#[test]
fn batch_describes_session_and_events_the_way_the_server_expects() {
    let env = env();
    let id = add_session(&env.conn, "claude-code", "sess-1", &[("e1", "こんにちは"), ("e2", "はい")]);
    env.conn.execute("UPDATE sessions SET title = 'MCP の設定' WHERE id = ?1", [id]).unwrap();
    store::set_session_state(&env.conn, SERVER, id, Some("send"), Some("org")).unwrap();
    run(&env);

    let call = env.server.ingest_calls().remove(0);
    assert_eq!(call.method, "POST");
    assert_eq!(call.bearer.as_deref(), Some("ingest-token"));
    let session = &call.body["sessions"][0];
    assert_eq!(session["source_tool"], "claude-code");
    assert_eq!(session["source_session_id"], "sess-1");
    assert_eq!(session["project_path"], "/work/app");
    assert_eq!(session["title"], "MCP の設定");
    assert_eq!(session["started_at"], "2026-10-16T12:00:00.000Z"); // 発言の最初の時刻
    assert_eq!(session["ended_at"], "2026-10-16T12:01:00.000Z"); // 発言の最後の時刻
    assert_eq!(session["visibility"], "org");

    let event = &call.body["events"][0];
    assert_eq!(event["source_session_id"], "sess-1");
    assert_eq!(event["source_event_id"], "e1");
    assert_eq!(event["kind"], "message");
    assert_eq!(event["role"], "user");
    assert_eq!(event["content"], "こんにちは");
    assert_eq!(event["content_hash"], format!("sha256:{}", engine::sha256_hex("こんにちは")));
    assert!(call.body["batch_id"].as_str().unwrap().len() == 36);
}

#[test]
fn visibility_is_left_to_the_server_unless_chosen() {
    let env = env();
    let id = add_session(&env.conn, "claude-code", "a", &[("a1", "x")]);
    include(&env, id);
    run(&env);
    assert!(env.server.ingest_calls()[0].body["sessions"][0].get("visibility").is_none());
}

#[test]
fn session_without_source_hash_still_gets_a_stable_id() {
    let env = env();
    env.conn.execute("INSERT INTO sessions(source_tool, project_path, source_file, source_hash) VALUES ('codex', '/p', '/logs/x.jsonl', NULL)", []).unwrap();
    let id = env.conn.last_insert_rowid();
    env.conn.execute("INSERT INTO events(session_id, source_tool, source_event_id, occurred_at, role, content, sequence) VALUES (?1, 'codex', 'c1', '2026-10-16T01:00:00Z', 'user', 'x', 0)", [id]).unwrap();
    include(&env, id);
    run(&env);
    assert_eq!(env.server.ingest_calls()[0].body["sessions"][0]["source_session_id"], "file:/logs/x.jsonl");
}

#[test]
fn very_long_content_is_cut_and_flagged() {
    let env = env();
    let long = "あ".repeat(engine::CONTENT_CHARS + 10);
    let id = add_session(&env.conn, "claude-code", "a", &[("a1", &long)]);
    include(&env, id);
    run(&env);
    let event = &env.server.ingest_calls()[0].body["events"][0];
    let content = event["content"].as_str().unwrap();
    assert_eq!(content.chars().count(), engine::CONTENT_CHARS);
    assert_eq!(event["truncated"], true);
    assert_eq!(event["content_hash"], format!("sha256:{}", engine::sha256_hex(content)));
}

#[test]
fn large_histories_are_sent_in_batches_of_500() {
    let env = env();
    let events: Vec<(String, String)> = (0..1200).map(|i| (format!("e{i}"), format!("発言 {i}"))).collect();
    let refs: Vec<(&str, &str)> = events.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let id = add_session(&env.conn, "claude-code", "big", &refs);
    include(&env, id);
    let report = run(&env);
    let sizes: Vec<usize> = env.server.ingest_calls().iter().map(|c| c.body["events"].as_array().unwrap().len()).collect();
    assert_eq!(sizes, vec![500, 500, 200]);
    assert_eq!(report.batches, 3);
    assert_eq!(report.pending, 0);
}

// ---- 送った記録 ----

#[test]
fn already_sent_events_are_not_sent_again() {
    let env = env();
    let id = add_session(&env.conn, "claude-code", "a", &[("a1", "x")]);
    include(&env, id);
    run(&env);
    let second = run(&env);
    assert_eq!(second.batches, 0);
    assert_eq!(env.server.ingest_calls().len(), 1);
}

#[test]
fn new_events_in_a_sent_session_are_sent_alone() {
    let env = env();
    let id = add_session(&env.conn, "claude-code", "a", &[("a1", "x")]);
    include(&env, id);
    run(&env);
    env.conn.execute("INSERT INTO events(session_id, source_tool, source_event_id, occurred_at, role, content, sequence) VALUES (?1, 'claude-code', 'a2', '2026-10-16T13:00:00Z', 'assistant', 'y', 1)", [id]).unwrap();
    run(&env);
    let calls = env.server.ingest_calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1].body["events"].as_array().unwrap().len(), 1);
    assert_eq!(calls[1].body["events"][0]["source_event_id"], "a2");
}

#[test]
fn an_event_whose_content_changed_is_sent_again() {
    // Web の会話は、回答の途中で取り込んだ本文が、あとで完全な本文に書き換わる
    let env = env();
    let id = add_session(&env.conn, "gemini_web", "g", &[("g1", "途中までの回")]);
    include(&env, id);
    run(&env);
    std::thread::sleep(Duration::from_millis(5));
    env.conn.execute("UPDATE events SET content = '完全な回答' WHERE source_event_id = 'g1'", []).unwrap();
    let report = run(&env);
    assert_eq!(report.updated, 1);
    assert_eq!(env.server.events.borrow()[&("gemini_web".to_string(), "g1".to_string())].0, "完全な回答");
    assert_eq!(run(&env).batches, 0); // 送り直したあとは、もう送らない
}

#[test]
fn a_changed_title_resends_only_the_session_info() {
    let env = env();
    let id = add_session(&env.conn, "claude-code", "a", &[("a1", "x")]);
    include(&env, id);
    run(&env);
    std::thread::sleep(Duration::from_millis(5));
    env.conn.execute("UPDATE sessions SET title = '新しい題名' WHERE id = ?1", [id]).unwrap();
    run(&env);
    let calls = env.server.ingest_calls();
    assert_eq!(calls.len(), 2);
    assert!(calls[1].body["events"].as_array().unwrap().is_empty());
    assert_eq!(calls[1].body["sessions"][0]["title"], "新しい題名");
    assert_eq!(run(&env).batches, 0);
}

#[test]
fn counts_report_progress() {
    let env = env();
    let a = add_session(&env.conn, "claude-code", "a", &[("a1", "x"), ("a2", "y")]);
    add_session(&env.conn, "claude-code", "b", &[("b1", "z")]);
    include(&env, a);
    let before = store::counts(&env.conn, SERVER, Mode::Manual).unwrap();
    assert_eq!((before.sessions_total, before.sessions_selected, before.events_pending, before.events_sent), (2, 1, 2, 0));
    run(&env);
    let after = store::counts(&env.conn, SERVER, Mode::Manual).unwrap();
    assert_eq!((after.events_pending, after.events_sent), (0, 2));
    assert!(after.last_sent_at.is_some());
}

#[test]
fn dry_run_sends_nothing() {
    let env = env();
    let id = add_session(&env.conn, "claude-code", "a", &[("a1", "x")]);
    include(&env, id);
    let report = engine::run(&env.conn, &env.server, &env.store, &no_sleep, &Options { dry_run: true, max_batches: None }).unwrap();
    assert_eq!(report.pending, 1);
    assert!(env.server.calls.borrow().is_empty());
}

#[test]
fn disconnecting_forgets_what_was_sent() {
    let env = env();
    let id = add_session(&env.conn, "claude-code", "a", &[("a1", "x")]);
    include(&env, id);
    run(&env);
    store::clear_connection(&env.conn).unwrap();
    assert!(store::load_connection(&env.conn).unwrap().is_none());
    let rows: i64 = env.conn.query_row("SELECT (SELECT COUNT(*) FROM sync_events) + (SELECT COUNT(*) FROM sync_sessions) + (SELECT COUNT(*) FROM sync_outbox)", [], |r| r.get(0)).unwrap();
    assert_eq!(rows, 0);
}

#[test]
fn running_without_a_connection_is_an_error() {
    let conn = db::open_memory().unwrap();
    let error = engine::run(&conn, &FakeServer::new(), &MemoryStore::default(), &no_sleep, &Options::default()).unwrap_err();
    assert!(error.to_string().contains("接続していません"));
}

// ---- 失敗したとき ----

#[test]
fn a_dropped_connection_keeps_the_batch_and_resends_it_unchanged() {
    let env = env();
    let id = add_session(&env.conn, "claude-code", "a", &[("a1", "x")]);
    include(&env, id);
    for _ in 0..3 {
        env.server.script.borrow_mut().push_back(Err(anyhow!("接続できません")));
    }
    let first = run(&env);
    assert!(matches!(first.stop, Some(Stop::Unavailable(_))));
    assert_eq!(env.server.ingest_calls().len(), 3); // 3 回まで試す
    assert_eq!(store::counts(&env.conn, SERVER, Mode::Manual).unwrap().events_pending, 1);

    // その間に新しい発言が増えても、送り直しは、組んだときのままのバッチ(同じ batch_id と本文)
    env.conn.execute("INSERT INTO events(session_id, source_tool, source_event_id, occurred_at, role, content, sequence) VALUES (?1, 'claude-code', 'a2', '2026-10-16T14:00:00Z', 'user', 'あとから', 1)", [id]).unwrap();
    let second = run(&env);
    assert!(second.stop.is_none());
    let calls = env.server.ingest_calls();
    assert_eq!(calls[0].body, calls[3].body);
    assert_eq!(second.pending, 0);
    assert_eq!(env.server.events.borrow().len(), 2);
}

#[test]
fn server_errors_are_retried_a_few_times() {
    let env = env();
    let id = add_session(&env.conn, "claude-code", "a", &[("a1", "x")]);
    include(&env, id);
    env.server.script.borrow_mut().push_back(reply(503, json!({})));
    env.server.script.borrow_mut().push_back(reply(502, json!({})));
    let report = run(&env);
    assert!(report.stop.is_none());
    assert_eq!(report.created, 1);
    assert_eq!(env.server.ingest_calls().len(), 3);
}

#[test]
fn an_expired_token_is_renewed_and_the_batch_retried() {
    let env = env();
    let id = add_session(&env.conn, "claude-code", "a", &[("a1", "x")]);
    include(&env, id);
    env.server.valid_tokens.borrow_mut().clear();
    *env.server.renew_to.borrow_mut() = Some("renewed-token".into());
    let report = run(&env);
    assert!(report.stop.is_none());
    assert_eq!(report.created, 1);
    assert_eq!(env.store.get(SERVER, "ingest").unwrap().as_deref(), Some("renewed-token"));
}

#[test]
fn when_renewing_fails_the_user_must_reconnect_and_nothing_is_lost() {
    let env = env();
    let id = add_session(&env.conn, "claude-code", "a", &[("a1", "x")]);
    include(&env, id);
    env.server.valid_tokens.borrow_mut().clear();
    let report = run(&env);
    assert!(matches!(report.stop, Some(Stop::NeedsAuth(_))));
    assert_eq!(report.pending, 1);
}

#[test]
fn a_missing_token_asks_to_reconnect() {
    let env = env();
    env.store.delete(SERVER, "ingest").unwrap();
    assert!(matches!(run(&env).stop, Some(Stop::NeedsAuth(_))));
}

#[test]
fn rate_limit_stops_with_the_wait_time() {
    let env = env();
    let id = add_session(&env.conn, "claude-code", "a", &[("a1", "x")]);
    include(&env, id);
    env.server.script.borrow_mut().push_back(Ok(Response { status: 429, body: json!({}), retry_after: Some(120) }));
    assert_eq!(run(&env).stop, Some(Stop::RateLimited(120)));
}

#[test]
fn forbidden_and_old_client_stop_the_run() {
    let env = env();
    let id = add_session(&env.conn, "claude-code", "a", &[("a1", "x")]);
    include(&env, id);
    env.server.script.borrow_mut().push_back(reply(403, json!({"error": "insufficient_scope"})));
    assert!(matches!(run(&env).stop, Some(Stop::Forbidden(_))));
    env.server.script.borrow_mut().push_back(reply(426, json!({})));
    assert!(matches!(run(&env).stop, Some(Stop::UpdateRequired(_))));
}

#[test]
fn too_large_batches_are_halved() {
    let env = env();
    let events: Vec<(String, String)> = (0..8).map(|i| (format!("e{i}"), format!("発言 {i}"))).collect();
    let refs: Vec<(&str, &str)> = events.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let id = add_session(&env.conn, "claude-code", "a", &refs);
    include(&env, id);
    env.server.script.borrow_mut().push_back(reply(413, json!({"error": "payload_too_large"})));
    let report = run(&env);
    let sizes: Vec<usize> = env.server.ingest_calls().iter().map(|c| c.body["events"].as_array().unwrap().len()).collect();
    assert_eq!(sizes, vec![8, 4, 4]);
    assert_eq!(report.created, 8);
}

#[test]
fn a_single_event_that_is_too_large_is_skipped() {
    let env = env();
    let id = add_session(&env.conn, "claude-code", "a", &[("a1", "大きすぎる")]);
    include(&env, id);
    env.server.script.borrow_mut().push_back(reply(413, json!({})));
    let report = run(&env);
    assert_eq!(report.rejected, 1);
    assert_eq!(report.pending, 0); // 印を付けて先に進む(永遠に再送しない)
}

#[test]
fn a_batch_id_conflict_is_rebuilt_once() {
    let env = env();
    let id = add_session(&env.conn, "claude-code", "a", &[("a1", "x")]);
    include(&env, id);
    env.server.script.borrow_mut().push_back(reply(409, json!({"error": "batch_conflict"})));
    let report = run(&env);
    assert!(report.stop.is_none());
    let calls = env.server.ingest_calls();
    assert_ne!(calls[0].body["batch_id"], calls[1].body["batch_id"]);
}

#[test]
fn events_rejected_by_the_server_are_not_sent_forever() {
    let env = env();
    let id = add_session(&env.conn, "claude-code", "a", &[("a1", "x")]);
    include(&env, id);
    env.server.script.borrow_mut().push_back(reply(200, json!({
        "status": "partial", "events": {"created": 0, "updated": 0, "duplicate": 0, "rejected": 1},
        "rejected": [{"source_event_id": "a1", "reason": "occurred_at_out_of_range"}], "warnings": [],
    })));
    let report = run(&env);
    assert_eq!(report.rejected, 1);
    assert_eq!(report.rejected_samples, vec!["a1: occurred_at_out_of_range".to_string()]);
    assert_eq!(run(&env).batches, 0);
}

// ---- トークンの更新 ----

#[test]
fn tokens_are_renewed_when_a_quarter_of_their_life_is_left() {
    let env = env();
    let now = time::OffsetDateTime::now_utc();
    let meta = |issued_days_ago: i64, days_left: i64| TokenMeta {
        issued_at: super::iso(now - time::Duration::days(issued_days_ago)),
        expires_at: super::iso(now + time::Duration::days(days_left)),
    };
    *env.server.renew_to.borrow_mut() = Some("fresh".into());

    store::set_token_meta(&env.conn, "ingest", &meta(10, 80)).unwrap(); // 90 日のうち 80 日が残っている
    engine::maintain_tokens(&env.conn, &env.server, &env.store, SERVER);
    assert_eq!(env.store.get(SERVER, "ingest").unwrap().as_deref(), Some("ingest-token"));

    store::set_token_meta(&env.conn, "ingest", &meta(75, 15)).unwrap(); // 残りが 4 分の 1 を切った
    engine::maintain_tokens(&env.conn, &env.server, &env.store, SERVER);
    assert_eq!(env.store.get(SERVER, "ingest").unwrap().as_deref(), Some("fresh"));
}

// ---- 登録(デバイスフロー) ----

fn well_known(name: &str, versions: Value) -> Scripted {
    reply(200, json!({"name": name, "api_versions": versions, "login_methods": ["email_link"]}))
}

/// URL の末尾ごとに応答を返す、手書きのサーバー
struct Routes(RefCell<HashMap<&'static str, VecDeque<Scripted>>>);

impl Routes {
    fn new(routes: Vec<(&'static str, Vec<Scripted>)>) -> Self {
        Routes(RefCell::new(routes.into_iter().map(|(k, v)| (k, v.into())).collect()))
    }
}

impl Transport for Routes {
    fn request(&self, _m: &str, url: &str, _b: Option<&str>, _body: Option<&[u8]>, _g: bool) -> Result<Response> {
        let mut routes = self.0.borrow_mut();
        for (suffix, queue) in routes.iter_mut() {
            if url.ends_with(suffix) {
                return if queue.len() > 1 { queue.pop_front().unwrap() } else { match queue.front() {
                    Some(Ok(r)) => Ok(Response { status: r.status, body: r.body.clone(), retry_after: r.retry_after }),
                    Some(Err(e)) => Err(anyhow!("{e}")),
                    None => Err(anyhow!("応答がありません: {url}")),
                } };
            }
        }
        Err(anyhow!("接続できません: {url}"))
    }
}

fn start_reply() -> Scripted {
    reply(200, json!({"device_code": "dc_x", "user_code": "WXYZ-1234", "verification_uri": "https://sync.example.com/device",
        "verification_uri_complete": "https://sync.example.com/device?code=WXYZ-1234", "expires_in": 600, "interval": 5}))
}

fn pending() -> Pending {
    Pending {
        server: SERVER.into(),
        start: DeviceStart {
            device_code: "dc_x".into(), user_code: "WXYZ-1234".into(), verification_uri: "u".into(),
            verification_uri_complete: "u?code=WXYZ-1234".into(), expires_in: 600, interval: 5,
        },
    }
}

fn token_reply() -> Scripted {
    reply(200, json!({"device_id": "dev-9", "user": {"id": "u1", "display_name": "田中", "email": "tanaka@example.com"},
        "tokens": [{"scope": "ingest", "token": "sl_ingest", "expires_at": "2027-01-15T04:12:33.123456+00:00"},
                   {"scope": "search", "token": "sl_search", "expires_at": "2026-11-16T04:12:33+00:00"}]}))
}

#[test]
fn begin_gives_the_code_to_show() {
    let t = Routes::new(vec![
        ("/.well-known/synclane", vec![well_known("synclane-server", json!(["v1"]))]),
        ("/auth/device/start", vec![start_reply()]),
    ]);
    let pending = device_flow::begin(&t, "Sync.Example.com/", "田中の MacBook", "0.3.0").unwrap();
    assert_eq!(pending.server, SERVER);
    assert_eq!(pending.start.user_code, "WXYZ-1234");
    assert_eq!(pending.start.verification_uri_complete, "https://sync.example.com/device?code=WXYZ-1234");
}

#[test]
fn begin_explains_why_it_cannot_connect() {
    let message = |routes: Vec<(&'static str, Vec<Scripted>)>, url: &str| device_flow::begin(&Routes::new(routes), url, "n", "v").unwrap_err().to_string();
    assert!(message(vec![], SERVER).contains("接続できません"));
    assert!(message(vec![("/.well-known/synclane", vec![reply(200, json!({"name": "other"}))])], SERVER).contains("SyncLane のサーバーではない"));
    assert!(message(vec![("/.well-known/synclane", vec![well_known("synclane-server", json!(["v2"]))])], SERVER).contains("対応していません"));
    assert!(message(vec![], "http://sync.example.com").contains("https://"));
    let limited = message(vec![("/.well-known/synclane", vec![well_known("synclane-server", json!(["v1"]))]), ("/auth/device/start", vec![reply(429, json!({}))])], SERVER);
    assert!(limited.contains("多すぎます"));
}

#[test]
fn finish_waits_for_approval_then_stores_tokens_safely() {
    let conn = db::open_memory().unwrap();
    let store = MemoryStore::default();
    let t = Routes::new(vec![("/auth/device/token", vec![
        reply(400, json!({"error": "authorization_pending"})),
        reply(400, json!({"error": "authorization_pending"})),
        token_reply(),
    ])]);
    let waits = RefCell::new(Vec::new());
    let connection = device_flow::finish(&conn, &t, &store, &pending(), &|d| waits.borrow_mut().push(d.as_secs()), &|| false).unwrap();

    assert_eq!(*waits.borrow(), vec![5, 5, 5]);
    assert_eq!(connection, ServerConnection { server: SERVER.into(), device_id: "dev-9".into(), user_name: "田中".into(), user_email: "tanaka@example.com".into() });
    assert_eq!(store::load_connection(&conn).unwrap(), Some(connection));
    assert_eq!(store.get(SERVER, "ingest").unwrap().as_deref(), Some("sl_ingest"));
    assert_eq!(store.get(SERVER, "search").unwrap().as_deref(), Some("sl_search"));
    // トークン本体は、DB に入れない(期限だけ)
    let dump: String = conn.query_row("SELECT group_concat(key || '=' || value) FROM settings", [], |r| r.get(0)).unwrap();
    assert!(!dump.contains("sl_ingest") && !dump.contains("sl_search"));
    assert!(store::token_meta(&conn, "ingest").unwrap().unwrap().expires_at.starts_with("2027-01-15"));
}

#[test]
fn finish_slows_down_when_told_to() {
    let conn = db::open_memory().unwrap();
    let t = Routes::new(vec![("/auth/device/token", vec![reply(400, json!({"error": "slow_down"})), token_reply()])]);
    let waits = RefCell::new(Vec::new());
    device_flow::finish(&conn, &t, &MemoryStore::default(), &pending(), &|d| waits.borrow_mut().push(d.as_secs()), &|| false).unwrap();
    assert_eq!(*waits.borrow(), vec![5, 10]);
}

#[test]
fn finish_reports_denial_expiry_and_cancel() {
    let run = |reply_: Scripted, cancelled: bool| {
        let conn = db::open_memory().unwrap();
        let t = Routes::new(vec![("/auth/device/token", vec![reply_])]);
        device_flow::finish(&conn, &t, &MemoryStore::default(), &pending(), &no_sleep, &|| cancelled).unwrap_err()
    };
    assert!(matches!(run(reply(400, json!({"error": "access_denied"})), false), ConnectError::Denied));
    assert!(matches!(run(reply(400, json!({"error": "expired_token"})), false), ConnectError::Expired));
    assert!(matches!(run(reply(400, json!({"error": "authorization_pending"})), true), ConnectError::Cancelled));
    assert!(matches!(run(reply(500, json!({"error": "oops"})), false), ConnectError::Other(_)));
}

#[test]
fn finish_survives_a_brief_network_failure() {
    let conn = db::open_memory().unwrap();
    let t = Routes::new(vec![("/auth/device/token", vec![Err(anyhow!("一瞬つながらない")), token_reply()])]);
    assert!(device_flow::finish(&conn, &t, &MemoryStore::default(), &pending(), &no_sleep, &|| false).is_ok());
}

// ---- 保存先 ----

#[test]
fn schema_v4_adds_tracking_without_touching_existing_data() {
    let conn = db::open_memory().unwrap();
    let id = add_session(&conn, "claude-code", "a", &[("a1", "元の本文")]);
    let (updated_event, updated_session): (Option<String>, Option<String>) = conn
        .query_row("SELECT (SELECT updated_at FROM events WHERE session_id = ?1), (SELECT updated_at FROM sessions WHERE id = ?1)", [id], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert_eq!((updated_event, updated_session), (None, None)); // 新しい発言は、まだ更新されていない
    conn.execute("UPDATE events SET content = '書き換え' WHERE session_id = ?1", [id]).unwrap();
    let touched: Option<String> = conn.query_row("SELECT updated_at FROM events WHERE session_id = ?1", [id], |r| r.get(0)).unwrap();
    assert!(touched.unwrap().ends_with('Z'));
    assert_eq!(db::schema_version(&conn).unwrap(), 4);
}

#[test]
fn mode_defaults_to_manual_and_rejects_nonsense() {
    let conn = db::open_memory().unwrap();
    assert_eq!(store::mode(&conn).unwrap(), Mode::Manual);
    store::set_mode(&conn, Mode::Auto).unwrap();
    assert_eq!(store::mode(&conn).unwrap(), Mode::Auto);
    assert!(Mode::parse("sometimes").is_err());
}

#[test]
fn time_helpers_agree_with_sqlite() {
    let conn = db::open_memory().unwrap();
    let from_sqlite: String = conn.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ', 'now')", [], |r| r.get(0)).unwrap();
    let ours = super::now_iso();
    assert_eq!(from_sqlite.len(), ours.len());
    assert!(super::parse_time("2027-01-15T04:12:33.123456+00:00").is_some());
}

// ---- 除外のルール ----

fn set_session(conn: &Connection, id: i64, project: &str, started: Option<&str>) {
    conn.execute("UPDATE sessions SET project_path = ?2, started_at = ?3 WHERE id = ?1", rusqlite::params![id, project, started]).unwrap();
}

fn sent(env: &Env, tool: &str, event: &str) -> bool {
    env.server.events.borrow().contains_key(&(tool.to_string(), event.to_string()))
}

#[test]
fn rules_exclude_by_project_tool_and_date_in_auto_mode() {
    let env = env();
    let keep = add_session(&env.conn, "claude-code", "k", &[("k1", "送る")]);
    let personal = add_session(&env.conn, "claude-code", "p", &[("p1", "個人")]);
    let web = add_session(&env.conn, "gemini_web", "w", &[("w1", "Web")]);
    let old = add_session(&env.conn, "claude-code", "o", &[("o1", "古い")]);
    set_session(&env.conn, keep, "/work/app", Some("2026-10-10T00:00:00.000Z"));
    set_session(&env.conn, personal, "/home/me/private", Some("2026-10-10T00:00:00.000Z"));
    set_session(&env.conn, web, "", Some("2026-10-10T00:00:00.000Z"));
    set_session(&env.conn, old, "/work/app", Some("2026-09-01T00:00:00.000Z"));
    store::set_mode(&env.conn, Mode::Auto).unwrap();
    store::set_rules(&env.conn, &store::Rules {
        exclude_projects: vec!["/home/me/private".into()],
        exclude_tools: vec!["gemini_web".into()],
        since: Some("2026-10-01".into()),
    }).unwrap();

    assert_eq!(store::counts(&env.conn, SERVER, Mode::Auto).unwrap().events_pending, 1);
    run(&env);
    assert!(sent(&env, "claude-code", "k1"));
    assert!(!sent(&env, "claude-code", "p1") && !sent(&env, "gemini_web", "w1") && !sent(&env, "claude-code", "o1"));
}

#[test]
fn explicit_send_beats_rules_and_explicit_exclusion_beats_no_rule() {
    let env = env();
    let forced = add_session(&env.conn, "gemini_web", "f", &[("f1", "例外で送る")]);
    let dropped = add_session(&env.conn, "claude-code", "d", &[("d1", "個別に除外")]);
    store::set_mode(&env.conn, Mode::Auto).unwrap();
    store::set_rules(&env.conn, &store::Rules { exclude_tools: vec!["gemini_web".into()], ..Default::default() }).unwrap();
    include(&env, forced);
    store::set_session_state(&env.conn, SERVER, dropped, Some("excluded"), None).unwrap();
    run(&env);
    assert!(sent(&env, "gemini_web", "f1"));
    assert!(!sent(&env, "claude-code", "d1"));
}

#[test]
fn rules_do_not_apply_in_manual_mode_and_list_shows_rule_excluded() {
    let env = env();
    let id = add_session(&env.conn, "gemini_web", "w", &[("w1", "x")]);
    store::set_rules(&env.conn, &store::Rules { exclude_tools: vec!["gemini_web".into()], ..Default::default() }).unwrap();
    let state = |mode| super::ops::list_sessions(&env.conn, SERVER, mode, 10, 0).unwrap().into_iter().find(|r| r.id == id).unwrap().state;
    assert_eq!(state(Mode::Manual), "not_selected");
    assert_eq!(state(Mode::Auto), "rule_excluded");
    include(&env, id);
    assert_eq!(state(Mode::Auto), "send");
}

#[test]
fn rules_quote_odd_project_names_and_validate_the_date() {
    let env = env();
    let id = add_session(&env.conn, "claude-code", "q", &[("q1", "x")]);
    set_session(&env.conn, id, "/work/it's \"mine\"", None);
    store::set_mode(&env.conn, Mode::Auto).unwrap();
    store::set_rules(&env.conn, &store::Rules { exclude_projects: vec!["/work/it's \"mine\"".into()], ..Default::default() }).unwrap();
    assert_eq!(store::counts(&env.conn, SERVER, Mode::Auto).unwrap().events_pending, 0);
    assert!(store::set_rules(&env.conn, &store::Rules { since: Some("10/01".into()), ..Default::default() }).is_err());
    assert!(store::set_rules(&env.conn, &store::Rules { since: Some("2026-10-01'; --".into()), ..Default::default() }).is_err());
}

#[test]
fn connecting_defaults_to_auto_but_keeps_an_earlier_choice() {
    let connect = |conn: &Connection| {
        let t = Routes::new(vec![("/auth/device/token", vec![token_reply()])]);
        device_flow::finish(conn, &t, &MemoryStore::default(), &pending(), &no_sleep, &|| false).unwrap();
    };
    let conn = db::open_memory().unwrap();
    connect(&conn);
    assert_eq!(store::mode(&conn).unwrap(), Mode::Auto);

    let conn = db::open_memory().unwrap();
    store::set_mode(&conn, Mode::Manual).unwrap();
    connect(&conn);
    assert_eq!(store::mode(&conn).unwrap(), Mode::Manual);
}

// ---- 秘密を伏せる ----

const GITHUB_TOKEN: &str = "ghp_a1B2c3D4e5F6g7H8i9J0k1L2m3N4o5P6q7R8";

#[test]
fn secrets_are_masked_in_what_is_sent_but_not_in_the_local_record() {
    let env = env();
    let text = format!("このトークンで push してください: {GITHUB_TOKEN}\nDB_PASSWORD=hunter2hunter2");
    let id = add_session(&env.conn, "claude-code", "a", &[("a1", &text), ("a2", "はい")]);
    env.conn.execute("UPDATE sessions SET title = ?1 WHERE id = ?2", rusqlite::params![format!("token {GITHUB_TOKEN} を試す"), id]).unwrap();
    include(&env, id);
    let report = run(&env);

    let call = env.server.ingest_calls().remove(0);
    let sent = call.body.to_string();
    assert!(!sent.contains(GITHUB_TOKEN) && !sent.contains("hunter2hunter2"), "送った本文に秘密が残っている: {sent}");
    let event = &call.body["events"][0];
    assert_eq!(event["content"], "このトークンで push してください: [MASKED:github-token]\nDB_PASSWORD=[MASKED:secret-assignment]");
    assert_eq!(call.body["sessions"][0]["title"], "token [MASKED:github-token] を試す");
    // ハッシュは、送った(伏せたあとの)本文のもの
    let content = event["content"].as_str().unwrap();
    assert_eq!(event["content_hash"], format!("sha256:{}", engine::sha256_hex(content)));
    assert_eq!(report.masked, 3);

    // 手元の記録は、元のまま
    let local: String = env.conn.query_row("SELECT content FROM events WHERE source_event_id = 'a1'", [], |r| r.get(0)).unwrap();
    assert!(local.contains(GITHUB_TOKEN) && local.contains("hunter2hunter2"));
}

#[test]
fn the_secret_is_masked_before_the_cut_so_no_half_of_it_is_sent() {
    let env = env();
    // 切れ目(CONTENT_CHARS)をまたぐ位置に、鍵を置く
    let head = "あ".repeat(engine::CONTENT_CHARS - 10);
    let id = add_session(&env.conn, "claude-code", "a", &[("a1", &format!("{head}{GITHUB_TOKEN} 以降"))]);
    include(&env, id);
    run(&env);
    let sent = env.server.ingest_calls()[0].body.to_string();
    assert!(!sent.contains("ghp_"), "鍵の前半が残っている");
}

#[test]
fn a_batch_built_before_masking_existed_is_dropped_and_rebuilt_masked() {
    let env = env();
    let id = add_session(&env.conn, "claude-code", "a", &[("a1", &format!("token {GITHUB_TOKEN}"))]);
    include(&env, id);
    // マスクの導入前に、生の本文で組まれた、未送信のバッチ
    let raw = json!({"batch_id": "old", "client": {}, "sessions": [], "events": [{"content": format!("token {GITHUB_TOKEN}")}]}).to_string();
    env.conn.execute(
        "INSERT INTO sync_outbox(server_id, batch_id, body, event_ids, session_ids, created_at) VALUES (?1, 'old', ?2, '[]', '[]', '2026-10-16T00:00:00.000Z')",
        rusqlite::params![SERVER, raw],
    ).unwrap();
    run(&env);
    let calls = env.server.ingest_calls();
    assert!(calls.iter().all(|c| c.body["batch_id"] != "old"), "古いバッチを送ってしまった");
    assert!(calls.iter().all(|c| !c.body.to_string().contains(GITHUB_TOKEN)));
    assert_eq!(env.server.events.borrow().len(), 1); // 伏せたうえで、組み直して送られた
}

#[test]
fn text_without_secrets_is_sent_unchanged_and_reports_zero() {
    let env = env();
    let id = add_session(&env.conn, "claude-code", "a", &[("a1", "ふつうの文章です")]);
    include(&env, id);
    let report = run(&env);
    assert_eq!(env.server.ingest_calls()[0].body["events"][0]["content"], "ふつうの文章です");
    assert_eq!(report.masked, 0);
}

#[test]
fn the_allow_and_block_files_beside_the_database_are_applied() {
    // /tmp は使わない。プロジェクトの target/ の中に作って、終わったら消す
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join(format!("test-mask-allow-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("mask-allow.txt"), "# デモ用\ndemo-pass-123\n").unwrap();
    std::fs::write(dir.join("mask-block.txt"), "# 社内\ncorp-db-01\nre:(?i)project-[a-z]+\nre:([\n").unwrap();

    let conn = db::open(&dir.join("synclane.db")).unwrap();
    let store = MemoryStore::default();
    store.set(SERVER, "ingest", "ingest-token").unwrap();
    store.set(SERVER, "search", "search-token").unwrap();
    store::save_connection(&conn, &ServerConnection {
        server: SERVER.into(), device_id: "dev1".into(), user_name: "田中".into(), user_email: "tanaka@example.com".into(),
    }).unwrap();
    let server = FakeServer::new();
    let text = format!("PASSWORD=demo-pass-123 と PASSWORD=real-pass-456 と {GITHUB_TOKEN} と corp-db-01 と Project-Aurora");
    let id = add_session(&conn, "claude-code", "a", &[("a1", &text)]);
    store::set_session_state(&conn, SERVER, id, Some("send"), None).unwrap();
    let report = engine::run(&conn, &server, &store, &no_sleep, &Options::default()).unwrap();
    drop(conn);
    std::fs::remove_dir_all(&dir).unwrap();

    let content = server.ingest_calls()[0].body["events"][0]["content"].as_str().unwrap().to_string();
    assert_eq!(content, "PASSWORD=demo-pass-123 と PASSWORD=[MASKED:secret-assignment] と [MASKED:github-token] と [MASKED:blocklist] と [MASKED:blocklist]");
    assert_eq!((report.masked, report.allowed, report.block_lines_ignored), (4, 1, 1));
}

#[test]
fn auto_mode_respects_later_added_exclusion_rules_for_already_sent_sessions() {
    let env = env();
    let id = add_session(&env.conn, "claude-code", "s1", &[("e1", "最初の発言")]);
    set_session(&env.conn, id, "/work/private-app", Some("2026-10-20T00:00:00.000Z"));
    store::set_mode(&env.conn, Mode::Auto).unwrap();

    // 1回目の送信: auto モードで private-app の会話が送信される
    let first = run(&env);
    assert_eq!(first.events_sent, 1);
    assert!(sent(&env, "claude-code", "e1"));

    // 送信後も sync_sessions に state='send' が勝手に設定されていないことを確認
    let explicit_state: Option<String> = env.conn.query_row(
        "SELECT state FROM sync_sessions WHERE server_id = ?1 AND session_id = ?2",
        rusqlite::params![SERVER, id],
        |r| r.get(0),
    ).optional().unwrap();
    assert_eq!(explicit_state, None);

    // 後から private-app を除外ルールに追加
    store::set_rules(&env.conn, &store::Rules {
        exclude_projects: vec!["/work/private-app".into()],
        ..Default::default()
    }).unwrap();

    // 同じセッションに新しい発言を追加
    env.conn.execute(
        "INSERT INTO events(session_id, source_tool, source_event_id, occurred_at, role, content, sequence)
         VALUES (?1, 'claude-code', 'e2', '2026-10-20T01:00:00.000Z', 'user', '秘密の発言', 2)",
        [id],
    ).unwrap();

    // 除外ルールが正しく効いて、未送信件数が 0 件になっていることを確認
    assert_eq!(store::counts(&env.conn, SERVER, Mode::Auto).unwrap().events_pending, 0);

    // 送信を実行しても何も送られないことを確認
    let second = run(&env);
    assert_eq!(second.batches, 0);
    assert_eq!(second.events_sent, 0);
    assert!(!sent(&env, "claude-code", "e2"));
}
