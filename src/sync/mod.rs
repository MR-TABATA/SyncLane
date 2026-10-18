//! サーバーへの会話の送信(任意の機能)。
//!
//! 接続の設定をしなければ、何も外に出ない。接続すると、選んだ会話(または、自動のモードなら全部)を
//! 会社のサーバーへ HTTPS で送る。手元の SQLite が正本のまま。送れなくても、収集と手元の検索は止まらない。
//!
//! 構成:
//! - `store`       設定と、何を送ったかの記録(SQLite)
//! - `secrets`     端末トークンの保存先(OS のキーチェーン)
//! - `api`         HTTP(JSON)の薄い層。テストでは差し替える
//! - `device_flow` 端末の登録(ブラウザで承認する方式)
//! - `engine`      送信の本体(まとめて送る・再送・トークンの更新)
//! - `remote`      サーバーの検索 API を呼ぶ(MCP の `team` / `all`)

pub mod api;
pub mod device_flow;
pub mod engine;
pub mod ops;
pub mod remote;
pub mod secrets;
pub mod store;

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// SQLite のトリガー(`strftime('%Y-%m-%dT%H:%M:%fZ')`)と同じ形の現在時刻。文字列のまま比べられる。
pub fn now_iso() -> String {
    iso(OffsetDateTime::now_utc())
}

pub fn iso(t: OffsetDateTime) -> String {
    let t = t.to_offset(time::UtcOffset::UTC);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second(),
        t.millisecond()
    )
}

pub fn parse_time(value: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(value, &Rfc3339).ok()
}

#[cfg(test)]
mod tests;
