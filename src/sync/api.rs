//! HTTP(JSON)の薄い層。テストでは `Transport` を差し替えて、サーバーを使わずに確かめる。

use anyhow::{Context, Result, bail};
use flate2::{Compression, write::GzEncoder};
use serde_json::Value;
use std::io::{Read, Write};
use std::time::Duration;

pub struct Response {
    pub status: u16,
    pub body: Value,
    pub retry_after: Option<u64>,
}

impl Response {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// サーバーの誤り応答(`{"error": …, "error_description": …}`)の名前。
    pub fn error(&self) -> &str {
        self.body.get("error").and_then(Value::as_str).unwrap_or("")
    }

    pub fn error_description(&self) -> &str {
        self.body.get("error_description").and_then(Value::as_str).unwrap_or("")
    }
}

pub trait Transport {
    /// `body` があれば JSON として送る。`gzip` なら圧縮する。通信できなければ Err(HTTP の誤りは Response で返す)。
    fn request(&self, method: &str, url: &str, bearer: Option<&str>, body: Option<&[u8]>, gzip: bool) -> Result<Response>;
}

pub struct UreqTransport {
    agent: ureq::Agent,
}

impl UreqTransport {
    pub fn new() -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .user_agent(concat!("SyncLane/", env!("CARGO_PKG_VERSION")))
            .build();
        Self { agent }
    }
}

impl Default for UreqTransport {
    fn default() -> Self {
        Self::new()
    }
}

fn read_response(resp: ureq::Response) -> Result<Response> {
    let status = resp.status();
    let retry_after = resp.header("Retry-After").and_then(|v| v.trim().parse().ok());
    let mut text = String::new();
    resp.into_reader().take(32 * 1024 * 1024).read_to_string(&mut text).context("failed to read the response")?;
    let body = if text.trim().is_empty() { Value::Null } else { serde_json::from_str(&text).unwrap_or(Value::Null) };
    Ok(Response { status, body, retry_after })
}

impl Transport for UreqTransport {
    fn request(&self, method: &str, url: &str, bearer: Option<&str>, body: Option<&[u8]>, gzip: bool) -> Result<Response> {
        let mut request = self.agent.request(method, url).set("Accept", "application/json");
        if let Some(token) = bearer {
            request = request.set("Authorization", &format!("Bearer {token}"));
        }
        let result = match body {
            Some(bytes) => {
                request = request.set("Content-Type", "application/json");
                if gzip {
                    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
                    encoder.write_all(bytes)?;
                    request = request.set("Content-Encoding", "gzip");
                    request.send_bytes(&encoder.finish()?)
                } else {
                    request.send_bytes(bytes)
                }
            }
            None => request.call(),
        };
        match result {
            Ok(resp) => read_response(resp),
            Err(ureq::Error::Status(_, resp)) => read_response(resp),
            Err(ureq::Error::Transport(error)) => bail!("{error}"),
        }
    }
}

/// 入力されたサーバーの URL を整える。https のみ(開発用に localhost だけ http を許す)。パスや末尾の `/` は除く。
pub fn normalize_server_url(input: &str) -> Result<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        bail!("サーバーの URL が空です");
    }
    let with_scheme = if trimmed.contains("://") { trimmed.to_string() } else { format!("https://{trimmed}") };
    let (scheme, rest) = with_scheme.split_once("://").expect("scheme");
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "https" && scheme != "http" {
        bail!("URL は https:// で始めてください");
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') || authority.contains(' ') {
        bail!("サーバーの URL が正しくありません");
    }
    let host = authority.trim_start_matches('[').split([']', ':']).next().unwrap_or("").to_ascii_lowercase();
    let is_local = matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1");
    if scheme == "http" && !is_local {
        bail!("https:// の URL だけ使えます(開発用に localhost だけ http:// を使えます)");
    }
    Ok(format!("{scheme}://{}", authority.to_ascii_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_server_urls() {
        assert_eq!(normalize_server_url("Sync.Example.com").unwrap(), "https://sync.example.com");
        assert_eq!(normalize_server_url(" https://sync.example.com/ ").unwrap(), "https://sync.example.com");
        assert_eq!(normalize_server_url("https://sync.example.com:8443/path?x=1").unwrap(), "https://sync.example.com:8443");
        assert_eq!(normalize_server_url("http://localhost:8000").unwrap(), "http://localhost:8000");
        assert_eq!(normalize_server_url("http://127.0.0.1:8000/").unwrap(), "http://127.0.0.1:8000");
    }

    #[test]
    fn rejects_unsafe_or_broken_urls() {
        for bad in ["", "   ", "http://sync.example.com", "ftp://sync.example.com", "https://user@sync.example.com", "https://"] {
            assert!(normalize_server_url(bad).is_err(), "{bad}");
        }
    }
}
