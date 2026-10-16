//! ブラウザ拡張から会話を受け取る、localhost だけの小さな HTTP サーバー。
//!
//! - `127.0.0.1` にだけ待ち受ける(同じ Mac の外からは届かない)。
//! - 他のウェブページから叩かれないよう、`Origin` が `chrome-extension://` のものだけを受ける。
//!   本文は `Content-Type: application/json` に限る(別のページからだと事前確認で止まる)。

use rusqlite::Connection;
use std::io::Read;
use std::sync::{Arc, Mutex};
use synclane::ingest::{self, Payload};
use tiny_http::{Header, Method, Request, Response, Server};

pub const ADDRESS: &str = "127.0.0.1:47821";
const MAX_BODY: u64 = 8 * 1024 * 1024;

/// 受け口を別スレッドで動かす。会話が増えたら `on_changed` を呼ぶ。
pub fn spawn(address: &str, conn: Arc<Mutex<Connection>>, on_changed: impl Fn(usize) + Send + 'static) {
    let server = match Server::http(address) {
        Ok(server) => server,
        Err(error) => {
            // 二重起動などでポートが使えないときは、拡張からの取り込みだけ止める(アプリは動かす)
            eprintln!("synclane ingest: cannot listen on {address}: {error}");
            return;
        }
    };
    std::thread::spawn(move || {
        for request in server.incoming_requests() {
            handle(request, &conn, &on_changed);
        }
    });
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("valid header")
}

fn request_header(request: &Request, name: &str) -> Option<String> {
    request.headers().iter().find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name)).map(|h| h.value.as_str().to_string())
}

fn respond(request: Request, status: u16, body: &str, origin: Option<&str>) {
    let mut response = Response::from_string(body)
        .with_status_code(status)
        .with_header(header("Content-Type", "application/json"));
    if let Some(origin) = origin {
        response.add_header(header("Access-Control-Allow-Origin", origin));
        response.add_header(header("Access-Control-Allow-Methods", "POST, GET, OPTIONS"));
        response.add_header(header("Access-Control-Allow-Headers", "Content-Type"));
        response.add_header(header("Access-Control-Allow-Private-Network", "true"));
        response.add_header(header("Vary", "Origin"));
    }
    let _ = request.respond(response);
}

fn handle(mut request: Request, conn: &Arc<Mutex<Connection>>, on_changed: &impl Fn(usize)) {
    let origin = request_header(&request, "Origin").filter(|o| o.starts_with("chrome-extension://"));
    if origin.is_none() {
        return respond(request, 403, r#"{"ok":false,"error":"forbidden"}"#, None);
    }
    let path = request.url().split('?').next().unwrap_or("").to_string();
    match (request.method(), path.as_str()) {
        (Method::Options, _) => respond(request, 204, "", origin.as_deref()),
        (Method::Get, "/ping") => respond(request, 200, r#"{"ok":true,"app":"SyncLane"}"#, origin.as_deref()),
        (Method::Post, "/ingest") => {
            let is_json = request_header(&request, "Content-Type")
                .is_some_and(|v| v.to_ascii_lowercase().starts_with("application/json"));
            if !is_json {
                return respond(request, 415, r#"{"ok":false,"error":"content-type"}"#, origin.as_deref());
            }
            let mut body = String::new();
            if request.as_reader().take(MAX_BODY + 1).read_to_string(&mut body).is_err() || body.len() as u64 > MAX_BODY {
                return respond(request, 413, r#"{"ok":false,"error":"body"}"#, origin.as_deref());
            }
            let payload: Payload = match serde_json::from_str(&body) {
                Ok(payload) => payload,
                Err(_) => return respond(request, 400, r#"{"ok":false,"error":"json"}"#, origin.as_deref()),
            };
            let result = match conn.lock() {
                Ok(mut conn) => ingest::ingest(&mut conn, &payload),
                Err(_) => return respond(request, 500, r#"{"ok":false,"error":"busy"}"#, origin.as_deref()),
            };
            match result {
                Ok(report) => {
                    let changed = report.inserted + report.updated;
                    if changed > 0 {
                        on_changed(changed);
                    }
                    let body = format!(r#"{{"ok":true,"inserted":{},"updated":{}}}"#, report.inserted, report.updated);
                    respond(request, 200, &body, origin.as_deref())
                }
                Err(error) => {
                    eprintln!("synclane ingest: {error:#}");
                    respond(request, 400, r#"{"ok":false,"error":"invalid"}"#, origin.as_deref())
                }
            }
        }
        _ => respond(request, 404, r#"{"ok":false,"error":"not found"}"#, origin.as_deref()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpStream;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn send(address: &str, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> (u16, String) {
        let mut stream = TcpStream::connect(address).unwrap();
        let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Length: {}\r\n", body.len());
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str("\r\n");
        req.push_str(body);
        stream.write_all(req.as_bytes()).unwrap();
        let mut out = String::new();
        stream.read_to_string(&mut out).unwrap();
        let status = out.split_whitespace().nth(1).unwrap().parse().unwrap();
        (status, out.split("\r\n\r\n").nth(1).unwrap_or("").to_string())
    }

    #[test]
    fn accepts_only_the_extension_and_stores_conversations() {
        let address = "127.0.0.1:47899";
        let conn = Arc::new(Mutex::new(synclane::db::open_memory().unwrap()));
        let changed = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&changed);
        spawn(address, Arc::clone(&conn), move |n| { counter.fetch_add(n, Ordering::SeqCst); });

        let body = r#"{"source":"gemini_web","conversation_id":"c1","title":"題","captured_at":"2026-10-16T01:00:00.000Z","messages":[{"index":0,"role":"user","text":"質問"},{"index":1,"role":"assistant","text":"回答"}]}"#;
        let ext = [("Origin", "chrome-extension://abcdef"), ("Content-Type", "application/json")];

        // 拡張ではないページ(Origin が https)・Origin なしは拒否
        assert_eq!(send(address, "POST", "/ingest", &[("Origin", "https://evil.example"), ("Content-Type", "application/json")], body).0, 403);
        assert_eq!(send(address, "POST", "/ingest", &[("Content-Type", "application/json")], body).0, 403);
        // JSON 以外は拒否
        assert_eq!(send(address, "POST", "/ingest", &[("Origin", "chrome-extension://abcdef"), ("Content-Type", "text/plain")], body).0, 415);
        // 壊れた JSON・不正な中身は拒否
        assert_eq!(send(address, "POST", "/ingest", &ext, "{").0, 400);
        assert_eq!(send(address, "POST", "/ingest", &ext, &body.replace("gemini_web", "evil")).0, 400);
        assert_eq!(conn.lock().unwrap().query_row("SELECT COUNT(*) FROM events", [], |r| r.get::<_, i64>(0)).unwrap(), 0);

        // 正しい送信は保存され、同じものをもう一度送っても増えない
        let (status, reply) = send(address, "POST", "/ingest", &ext, body);
        assert_eq!((status, reply.as_str()), (200, r#"{"ok":true,"inserted":2,"updated":0}"#));
        let (_, reply) = send(address, "POST", "/ingest", &ext, body);
        assert_eq!(reply, r#"{"ok":true,"inserted":0,"updated":0}"#);
        assert_eq!(conn.lock().unwrap().query_row("SELECT COUNT(*) FROM events", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
        assert_eq!(changed.load(Ordering::SeqCst), 2);

        // 事前確認と疎通確認
        assert_eq!(send(address, "OPTIONS", "/ingest", &[("Origin", "chrome-extension://abcdef")], "").0, 204);
        assert_eq!(send(address, "GET", "/ping", &[("Origin", "chrome-extension://abcdef")], "").0, 200);
    }
}
