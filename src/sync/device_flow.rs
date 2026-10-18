//! 端末の登録(デバイスフロー)。アプリに出したコードを、ブラウザで承認してもらい、端末トークンを受け取る。
//!
//! 流れ: begin(コードを得る)→ 利用者がブラウザで承認 → finish(承認を待って、トークンを保存する)。
//! アプリの中にパスワードの入力欄は作らない。

use anyhow::{Context, Result, anyhow};
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{Value, json};
use std::fmt;
use std::time::Duration;

use super::api::{Transport, normalize_server_url};
use super::now_iso;
use super::secrets::TokenStore;
use super::store::{ServerConnection, TokenMeta, save_connection, set_token_meta};

#[derive(Clone, Debug, Deserialize)]
pub struct DeviceStart {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Clone, Debug)]
pub struct Pending {
    pub server: String,
    pub start: DeviceStart,
}

/// 登録できなかった理由。画面に出す文を持つ。
#[derive(Debug)]
pub enum ConnectError {
    Expired,
    Denied,
    Cancelled,
    Other(anyhow::Error),
}

impl fmt::Display for ConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnectError::Expired => write!(f, "コードの期限が切れました。もう一度、最初からやり直してください"),
            ConnectError::Denied => write!(f, "ブラウザで登録が拒否されました"),
            ConnectError::Cancelled => write!(f, "登録を中止しました"),
            ConnectError::Other(error) => write!(f, "{error:#}"),
        }
    }
}

impl std::error::Error for ConnectError {}

impl From<anyhow::Error> for ConnectError {
    fn from(error: anyhow::Error) -> Self {
        ConnectError::Other(error)
    }
}

pub fn platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    }
}

/// サーバーで「この端末」として表示する名前の既定(コンピューター名)。
pub fn default_device_name() -> String {
    let from_command = |program: &str, args: &[&str]| {
        std::process::Command::new(program)
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let name = if cfg!(target_os = "macos") { from_command("scutil", &["--get", "ComputerName"]) } else { None };
    name.or_else(|| from_command("hostname", &[])).unwrap_or_else(|| "SyncLane".to_string())
}

/// 既定のブラウザで URL を開く。開けなければ false(その場合は、画面に URL を出して、自分で開いてもらう)。
pub fn open_in_browser(url: &str) -> bool {
    let status = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).status()
    } else if cfg!(target_os = "windows") {
        std::process::Command::new("cmd").args(["/C", "start", "", url]).status()
    } else {
        std::process::Command::new("xdg-open").arg(url).status()
    };
    status.map(|s| s.success()).unwrap_or(false)
}

/// サーバーに接続できるかを確かめ、登録のコードを受け取る。
pub fn begin(t: &dyn Transport, input_url: &str, device_name: &str, app_version: &str) -> Result<Pending> {
    let server = normalize_server_url(input_url)?;

    let info = t
        .request("GET", &format!("{server}/.well-known/synclane"), None, None, false)
        .with_context(|| format!("サーバーに接続できません: {server}"))?;
    if !info.ok() || info.body["name"] != "synclane-server" {
        return Err(anyhow!("このアドレスは SyncLane のサーバーではないようです: {server}"));
    }
    let supports_v1 = info.body["api_versions"].as_array().is_some_and(|v| v.iter().any(|x| x == "v1"));
    if !supports_v1 {
        return Err(anyhow!("このサーバーは、この版のアプリに対応していません(アプリかサーバーを更新してください)"));
    }

    let body = json!({"device_name": device_name, "platform": platform(), "app_version": app_version});
    let response = t
        .request("POST", &format!("{server}/api/v1/auth/device/start"), None, Some(body.to_string().as_bytes()), false)
        .context("登録の開始に失敗しました")?;
    if response.status == 429 {
        return Err(anyhow!("登録の開始が多すぎます。しばらく待ってからやり直してください"));
    }
    if !response.ok() {
        return Err(anyhow!("登録を始められません({} {})", response.status, response.error_description()));
    }
    let start: DeviceStart = serde_json::from_value(response.body).context("サーバーの応答が読めません")?;
    Ok(Pending { server, start })
}

/// 承認を待ち、端末トークンを保存して、接続の設定を書く。
/// `cancelled` が真を返したら中止する(画面の「中止」ボタン用)。
pub fn finish(
    conn: &Connection,
    t: &dyn Transport,
    store: &dyn TokenStore,
    pending: &Pending,
    sleep: &dyn Fn(Duration),
    cancelled: &dyn Fn() -> bool,
) -> Result<ServerConnection, ConnectError> {
    let url = format!("{}/api/v1/auth/device/token", pending.server);
    let body = json!({"device_code": pending.start.device_code}).to_string();
    let mut interval = pending.start.interval.max(1);

    loop {
        sleep(Duration::from_secs(interval));
        if cancelled() {
            return Err(ConnectError::Cancelled);
        }
        let response = match t.request("POST", &url, None, Some(body.as_bytes()), false) {
            Ok(r) => r,
            Err(_) => continue, // 一時的な通信の失敗は、待って続ける(期限が来れば expired_token になる)
        };
        if response.ok() {
            return save_tokens(conn, store, &pending.server, &response.body).map_err(ConnectError::Other);
        }
        match response.error() {
            "authorization_pending" => {}
            "slow_down" => interval = (interval + 5).min(30),
            "expired_token" => return Err(ConnectError::Expired),
            "access_denied" => return Err(ConnectError::Denied),
            _ if response.status == 429 => interval = (interval + 5).min(30),
            other => {
                return Err(ConnectError::Other(anyhow!(
                    "登録に失敗しました({} {} {})",
                    response.status,
                    other,
                    response.error_description()
                )));
            }
        }
    }
}

fn save_tokens(conn: &Connection, store: &dyn TokenStore, server: &str, body: &Value) -> Result<ServerConnection> {
    let tokens = body["tokens"]
        .as_array()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| anyhow!("サーバーの応答にトークンがありません"))?;
    let issued_at = now_iso();
    for token in tokens {
        let scope = token["scope"].as_str().ok_or_else(|| anyhow!("トークンの権限が読めません"))?;
        let value = token["token"].as_str().ok_or_else(|| anyhow!("トークンが読めません"))?;
        let expires_at = token["expires_at"].as_str().unwrap_or("").to_string();
        store.set(server, scope, value)?;
        set_token_meta(conn, scope, &TokenMeta { issued_at: issued_at.clone(), expires_at })?;
    }
    let connection = ServerConnection {
        server: server.to_string(),
        device_id: body["device_id"].as_str().unwrap_or("").to_string(),
        user_name: body["user"]["display_name"].as_str().unwrap_or("").to_string(),
        user_email: body["user"]["email"].as_str().unwrap_or("").to_string(),
    };
    save_connection(conn, &connection)?;
    // 接続したら、既定は「除外した以外の全部」。接続すること自体が、送ることへの同意(絞るなら、除外のルールで)
    if super::store::get(conn, "sync.mode")?.is_none() {
        super::store::set_mode(conn, super::store::Mode::Auto)?;
    }
    Ok(connection)
}
