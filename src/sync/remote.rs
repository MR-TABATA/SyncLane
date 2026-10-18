//! サーバーの検索 API(`/search`、`/sessions/{id}`)を呼ぶ。MCP の `team` と `all` が使う。
//!
//! 読み取り専用の MCP から呼ぶので、DB には書かない(トークンの更新は、アプリの送信側が行う)。

use rusqlite::Connection;
use std::cell::OnceCell;
use serde_json::{Value, json};

use super::api::{Response, Transport};
use super::secrets::TokenStore;
use super::store;

/// サーバーに届かなかった・断られた理由。MCP が、そのまま利用者(AI)に見せる文に直す。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteError {
    /// トークンが無効・期限切れ(401)。つなぎ直しが要る
    Auth,
    /// 検索の権限がない(403)
    Forbidden,
    /// 会話が見つからない、または見る権限がない(404)
    NotFound,
    /// 検索が多すぎる(429)。指定の秒数あとに
    RateLimited(u64),
    /// 要求が不正(400)や、検索の時間切れ(503)。サーバーの説明をそのまま見せる
    Rejected(String),
    /// サーバーに届かない、または障害
    Unavailable(String),
}

impl RemoteError {
    pub fn message(&self) -> String {
        match self {
            RemoteError::Auth => "サーバーの認証が切れています。SyncLane のアプリを開いて、サーバーにつなぎ直してください".into(),
            RemoteError::Forbidden => "この端末には、サーバーを検索する権限がありません。サーバーの管理者に確認してください".into(),
            RemoteError::NotFound => "会話が見つかりません".into(),
            RemoteError::RateLimited(seconds) => format!("サーバーへの検索が多すぎます。{seconds} 秒ほど待ってください"),
            RemoteError::Rejected(m) | RemoteError::Unavailable(m) => m.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteHit {
    pub occurred_at: String,
    pub source_tool: String,
    pub source_event_id: String,
    pub session_id: i64,
    pub event_id: i64,
    pub role: String,
    pub project_path: String,
    pub title: Option<String>,
    pub author: String,
    pub is_me: bool,
    pub text: String,
    pub is_anchor: bool,
}

pub struct Remote<'a> {
    pub server: String,
    /// 検索のトークン。キーチェーンは、サーバーを実際に使うときまで読まない(確認の画面で、手元の検索まで止まらないように)
    token: OnceCell<Option<String>>,
    secrets: Option<&'a dyn TokenStore>,
    transport: &'a dyn Transport,
}

impl<'a> Remote<'a> {
    pub fn new(server: &str, token: &str, transport: &'a dyn Transport) -> Self {
        Remote { server: server.to_string(), token: OnceCell::from(Some(token.to_string())), secrets: None, transport }
    }

    /// 接続中なら作る。接続していなければ None(手元だけで動く)。トークンは、最初に使うときに、キーチェーンから読む。
    pub fn from_settings(conn: &Connection, secrets: &'a dyn TokenStore, transport: &'a dyn Transport) -> Option<Self> {
        let connection = store::load_connection(conn).ok().flatten()?;
        Some(Remote { server: connection.server, token: OnceCell::new(), secrets: Some(secrets), transport })
    }

    fn token(&self) -> Option<&str> {
        self.token
            .get_or_init(|| self.secrets.and_then(|s| s.get(&self.server, "search").ok().flatten()))
            .as_deref()
    }

    fn call(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value, RemoteError> {
        let token = self.token().ok_or(RemoteError::Auth)?;
        let url = format!("{}/api/v1{path}", self.server);
        let bytes = body.map(|b| b.to_string());
        let response = self
            .transport
            .request(method, &url, Some(token), bytes.as_deref().map(str::as_bytes), false)
            .map_err(|e| RemoteError::Unavailable(format!("サーバーに届きません: {e}")))?;
        check(response)
    }

    pub fn search(&self, query: Option<&str>, date_from: Option<&str>, date_to: Option<&str>, limit: usize) -> Result<Vec<RemoteHit>, RemoteError> {
        let mut body = json!({"limit": limit, "author": "all", "content": "excerpt"});
        if let Some(q) = query {
            body["query"] = json!(q);
        }
        if let Some(d) = date_from {
            body["date_from"] = json!(d);
        }
        if let Some(d) = date_to {
            body["date_to"] = json!(d);
        }
        let value = self.call("POST", "/search", Some(body))?;
        Ok(value["items"].as_array().map(|items| items.iter().map(|i| hit(i, "excerpt")).collect()).unwrap_or_default())
    }

    /// 1 つの会話を時刻順に読む。`window` は、`around` の前後それぞれの件数。
    pub fn session(&self, session_id: i64, around: Option<i64>, window: usize) -> Result<Vec<RemoteHit>, RemoteError> {
        let mut path = format!("/sessions/{session_id}?window={window}");
        if let Some(id) = around {
            path.push_str(&format!("&around_event_id={id}"));
        }
        let value = self.call("GET", &path, None)?;
        let session = &value["session"];
        let (project, title) = (
            session["project_path"].as_str().unwrap_or("").to_string(),
            session["title"].as_str().map(str::to_string),
        );
        Ok(value["items"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .map(|i| {
                        let mut h = hit(i, "content");
                        h.session_id = session_id;
                        h.project_path = project.clone();
                        h.title = title.clone();
                        h
                    })
                    .collect()
            })
            .unwrap_or_default())
    }
}

fn check(response: Response) -> Result<Value, RemoteError> {
    if response.ok() {
        return Ok(response.body);
    }
    let described = || {
        let text = response.error_description();
        if text.is_empty() { format!("サーバーが断りました({})", response.status) } else { text.to_string() }
    };
    Err(match response.status {
        401 => RemoteError::Auth,
        403 => RemoteError::Forbidden,
        404 => RemoteError::NotFound,
        429 => RemoteError::RateLimited(response.retry_after.unwrap_or(10)),
        400 | 503 => RemoteError::Rejected(described()),
        status if status >= 500 => RemoteError::Unavailable(format!("サーバーの障害です({status})")),
        _ => RemoteError::Rejected(described()),
    })
}

fn hit(item: &Value, text_key: &str) -> RemoteHit {
    let s = |key: &str| item[key].as_str().unwrap_or("").to_string();
    RemoteHit {
        occurred_at: s("occurred_at"),
        source_tool: s("source_tool"),
        source_event_id: s("source_event_id"),
        session_id: item["session_id"].as_i64().unwrap_or(0),
        event_id: item["event_id"].as_i64().unwrap_or(0),
        role: s("role"),
        project_path: s("project_path"),
        title: item["title"].as_str().map(str::to_string),
        author: item["author"]["name"].as_str().unwrap_or("").to_string(),
        is_me: item["author"]["is_me"].as_bool().unwrap_or(false),
        text: s(text_key),
        is_anchor: item["is_anchor"].as_bool().unwrap_or(false),
    }
}
