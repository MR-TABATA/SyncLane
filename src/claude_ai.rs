//! claude.ai の公式エクスポート(Settings → Privacy → Export data)を取り込む。
//!
//! 渡せるもの: `conversations.json` / それを含む `.zip`(`conversations-000.zip` など) / それらを含むフォルダ。
//! 取り込みの本体は、Chrome 拡張と同じ `ingest::ingest`(冪等。同じ会話を取り込み直しても重複しない。伸びた会話は、足した分だけ入る)。
//! 保存するのは、人間の入力と Claude のテキスト応答だけ(ツールの結果・思考・添付は保存しない)。
//!
//! ファイルは、ディスクに展開しない。1 ファイルずつ、会話を 1 件ずつストリームで読んで取り込み、すぐ手放す
//! (大きなアカウントの、複数の zip でも、メモリに載るのは最大でも 1 会話分)。ネットワークには何も出さない。
//!
//! 失敗は、黙って落とさず、報告に積む(会話の失敗は `skipped`、ファイルの失敗は `failed_files`)。
//! 1 つのファイルが壊れていても、ほかのファイルは続けて取り込む。壊れたファイルは、読めたところまでが保存される。

use crate::ingest::{self, CLAUDE_AI_TOOL, Message, Payload};
use anyhow::{Context, Result, bail};
use rusqlite::Connection;
use serde::Deserializer as _;
use serde::de::{SeqAccess, Visitor};
use serde_json::Value;
use std::fs;
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};

/// 読みすぎを防ぐ上限。
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// 1 つの JSON を読む大きさ(展開後)。壊れた・悪意のある zip で、時間とメモリを使い切らないため。
    pub max_json_bytes: u64,
    /// フォルダをたどる深さ。
    pub max_depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { max_json_bytes: 1 << 30, max_depth: 6 }
    }
}

/// 取り込めなかった会話。黙って落とさず、理由つきで報告する。
#[derive(Debug, PartialEq, Eq)]
pub struct Skipped {
    pub id: String,
    pub title: String,
    pub reason: String,
}

/// 読めなかった(または、途中で読めなくなった)ファイル。
#[derive(Debug, PartialEq, Eq)]
pub struct FailedFile {
    pub origin: String,
    pub reason: String,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ClaudeAiReport {
    /// 読もうとした JSON ファイル(zip の中のものを含む)の数。
    pub files: usize,
    /// エクスポートに入っていた会話の数。
    pub conversations: usize,
    /// 保存できる発言が 1 つも無かった会話(空の会話・添付だけなど)。取り込まない。
    pub empty: usize,
    /// 新しく足した発言の数。
    pub inserted: usize,
    /// 本文が変わって更新した発言の数。
    pub updated: usize,
    pub skipped: Vec<Skipped>,
    pub failed_files: Vec<FailedFile>,
}

impl ClaudeAiReport {
    /// 取り込めなかった会話・ファイルがあるか(スクリプトから呼ぶとき、成功扱いにしないための印)。
    pub fn has_problems(&self) -> bool {
        !self.skipped.is_empty() || !self.failed_files.is_empty()
    }
}

/// 読む対象 1 つ(中身は、読む直前まで開かない)。
#[derive(Debug)]
enum Source {
    Json(PathBuf),
    ZipEntry { zip: PathBuf, name: String },
}

impl Source {
    fn origin(&self) -> String {
        match self {
            Source::Json(path) => path.display().to_string(),
            Source::ZipEntry { zip, name } => format!("{}!{}", zip.display(), name),
        }
    }
}

/// `path`(JSON・zip・フォルダ)から会話を取り込む。
pub fn import_claude_ai(path: &Path, conn: &mut Connection) -> Result<ClaudeAiReport> {
    import_with_limits(path, conn, &Limits::default())
}

pub fn import_with_limits(path: &Path, conn: &mut Connection, limits: &Limits) -> Result<ClaudeAiReport> {
    let mut report = ClaudeAiReport::default();
    let sources = list_sources(path, limits, &mut report)?;
    if sources.is_empty() && report.failed_files.is_empty() {
        bail!("conversations.json が見つかりません: {}(claude.ai のエクスポートの zip か、展開したフォルダを渡してください)", path.display());
    }
    let captured_at = now_iso()?;
    for source in sources {
        report.files += 1;
        if let Err(error) = import_source(&source, limits, conn, &captured_at, &mut report) {
            report.failed_files.push(FailedFile { origin: source.origin(), reason: format!("{error:#}") });
        }
    }
    Ok(report)
}

/// 1 つのファイルを、会話を 1 件ずつ読みながら取り込む。
fn import_source(source: &Source, limits: &Limits, conn: &mut Connection, captured_at: &str, report: &mut ClaudeAiReport) -> Result<()> {
    let origin = source.origin();
    let mut each = |conversation: Value| import_conversation(&conversation, conn, captured_at, report);
    match source {
        Source::Json(path) => {
            let file = fs::File::open(path).with_context(|| format!("開けません: {origin}"))?;
            let size = file.metadata()?.len();
            if size > limits.max_json_bytes {
                bail!("大きすぎます({size} バイト): {origin}");
            }
            stream_conversations(file, limits, &origin, &mut each)
        }
        Source::ZipEntry { zip, name } => {
            let file = fs::File::open(zip).with_context(|| format!("開けません: {}", zip.display()))?;
            let mut archive = zip::ZipArchive::new(file).with_context(|| format!("zip として読めません: {}", zip.display()))?;
            let entry = archive.by_name(name).with_context(|| format!("zip の中を開けません: {origin}"))?;
            if entry.size() > limits.max_json_bytes {
                bail!("大きすぎます({} バイト): {origin}", entry.size());
            }
            stream_conversations(entry, limits, &origin, &mut each)
        }
    }
}

/// 読む量に上限を付けた読み取り。申告された大きさを信用せず、実際に読んだ量を数える。
struct LimitReader<R> {
    inner: R,
    left: u64,
}

impl<R: Read> Read for LimitReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.left == 0 {
            // 上限ちょうどで終わるファイルは正常。まだ続きがあれば、上限超え
            let mut probe = [0u8; 1];
            return match self.inner.read(&mut probe)? {
                0 => Ok(0),
                _ => Err(io::Error::new(io::ErrorKind::InvalidData, "展開した大きさが上限を超えました")),
            };
        }
        let room = usize::try_from(self.left).unwrap_or(usize::MAX).min(buf.len());
        let n = self.inner.read(&mut buf[..room])?;
        self.left -= n as u64;
        Ok(n)
    }
}

/// JSON の配列を、要素(会話)ごとに `each` へ渡す。配列全体を Value の木にして持たない。
struct EachElement<'a, F: FnMut(Value)>(&'a mut F);

impl<'de, F: FnMut(Value)> Visitor<'de> for EachElement<'_, F> {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a list of conversations")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<(), A::Error> {
        while let Some(value) = seq.next_element::<Value>()? {
            (self.0)(value);
        }
        Ok(())
    }
}

fn stream_conversations<R: Read>(reader: R, limits: &Limits, origin: &str, each: &mut impl FnMut(Value)) -> Result<()> {
    let limited = LimitReader { inner: BufReader::new(reader), left: limits.max_json_bytes };
    let mut de = serde_json::Deserializer::from_reader(limited);
    let result = de.deserialize_seq(EachElement(each)).and_then(|()| de.end());
    result.map_err(|error| {
        if error.is_io() {
            anyhow::anyhow!("読めません({error}): {origin}")
        } else if error.is_data() {
            anyhow::anyhow!("会話の一覧(配列)ではありません: {origin}(claude.ai の conversations.json ではないようです)")
        } else {
            anyhow::anyhow!("JSON として読めません: {origin}({error})")
        }
    })
}

fn text(value: &Value, key: &str) -> String {
    value.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

/// 会話 1 つを取り込み、結果を報告に足す。1 つの会話の失敗で、ほかの会話を止めない。
fn import_conversation(conversation: &Value, conn: &mut Connection, captured_at: &str, report: &mut ClaudeAiReport) {
    report.conversations += 1;
    let payload = match to_payload(conversation, captured_at) {
        Ok(Some(payload)) => payload,
        Ok(None) => {
            report.empty += 1;
            return;
        }
        Err(reason) => {
            report.skipped.push(Skipped { id: text(conversation, "uuid"), title: text(conversation, "name"), reason });
            return;
        }
    };
    match ingest::ingest(conn, &payload) {
        Ok(done) => {
            report.inserted += done.inserted;
            report.updated += done.updated;
        }
        Err(error) => report.skipped.push(Skipped { id: payload.conversation_id, title: payload.title.unwrap_or_default(), reason: format!("{error:#}") }),
    }
}

/// 会話 1 つを取り込みの形にする。保存できる発言が無ければ `Ok(None)`。形が足りなければ理由を返す。
fn to_payload(conversation: &Value, captured_at: &str) -> std::result::Result<Option<Payload>, String> {
    let id = conversation.get("uuid").and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or("uuid がありません")?;
    let raw = conversation.get("chat_messages").and_then(Value::as_array).ok_or("chat_messages がありません")?;
    let mut messages = Vec::with_capacity(raw.len());
    let mut any = false;
    for (index, message) in raw.iter().enumerate() {
        // human → user。ほかの送り手は、保存しない(位置の番号は、元の並びのまま)
        let role = match message.get("sender").and_then(Value::as_str) {
            Some("human") => "user",
            Some("assistant") => "assistant",
            _ => continue,
        };
        let body = message_text(message);
        any |= !body.trim().is_empty();
        messages.push(Message {
            index: index as i64,
            role: role.into(),
            text: body,
            id: message.get("uuid").and_then(Value::as_str).map(str::to_string),
            at: message.get("created_at").and_then(Value::as_str).map(normalize_time),
        });
    }
    if !any {
        return Ok(None);
    }
    let title = conversation.get("name").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    Ok(Some(Payload { source: CLAUDE_AI_TOOL.into(), conversation_id: id.into(), title, captured_at: captured_at.into(), messages }))
}

/// 発言の本文。`content` の `text` ブロックを連ねる(ツールの結果・思考は含めない)。無ければ `text`。
fn message_text(message: &Value) -> String {
    let blocks = message.get("content").and_then(Value::as_array);
    let joined = blocks
        .map(|blocks| {
            blocks
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    let joined = joined.trim();
    if !joined.is_empty() {
        return joined.to_string();
    }
    message.get("text").and_then(Value::as_str).unwrap_or_default().trim().to_string()
}

/// `+00:00` の形を `Z` に直す(取り込み側は `Z` で終わる形だけを読む)。読めない形は、そのまま(取り込み側が時刻なしとして扱う)。
fn normalize_time(raw: &str) -> String {
    match raw.strip_suffix("+00:00") {
        Some(head) => format!("{head}Z"),
        None => raw.to_string(),
    }
}

fn now_iso() -> Result<String> {
    use time::format_description::well_known::Rfc3339;
    time::OffsetDateTime::now_utc().format(&Rfc3339).context("現在時刻を書けません")
}

/// 渡されたパスから、読む対象を集める(中身は開かない。zip は、中の名前を見るだけ)。
/// 読めなかった zip は、`report.failed_files` に積む。
fn list_sources(path: &Path, limits: &Limits, report: &mut ClaudeAiReport) -> Result<Vec<Source>> {
    let meta = fs::symlink_metadata(path).with_context(|| format!("読めません: {}", path.display()))?;
    if meta.file_type().is_symlink() {
        bail!("シンボリックリンクは読みません: {}", path.display());
    }
    let mut out = Vec::new();
    if meta.is_dir() {
        let mut files = Vec::new();
        collect_files(path, 0, limits.max_depth, &mut files)?;
        files.sort();
        for file in files {
            list_file(&file, &mut out, report)?;
        }
    } else {
        list_file(path, &mut out, report)?;
    }
    Ok(out)
}

fn collect_files(dir: &Path, depth: usize, max_depth: usize, out: &mut Vec<PathBuf>) -> Result<()> {
    if depth > max_depth {
        return Ok(());
    }
    for entry in fs::read_dir(dir).with_context(|| format!("読めません: {}", dir.display()))? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            continue; // リンクの先は読まない(フォルダの外を読ませないため)
        }
        if kind.is_dir() {
            collect_files(&entry.path(), depth + 1, max_depth, out)?;
        } else if kind.is_file() && (is_conversations_json(&entry.path()) || is_conversations_zip(&entry.path())) {
            out.push(entry.path());
        }
    }
    Ok(())
}

fn file_name(path: &Path) -> &str {
    path.file_name().and_then(|n| n.to_str()).unwrap_or_default()
}

fn is_conversations_json(path: &Path) -> bool {
    file_name(path) == "conversations.json"
}

/// `conversations-000.zip` のように、名前が `conversations` で始まる zip だけ(関係のない zip は読まない)。
fn is_conversations_zip(path: &Path) -> bool {
    let name = file_name(path);
    name.starts_with("conversations") && name.ends_with(".zip")
}

fn list_file(path: &Path, out: &mut Vec<Source>, report: &mut ClaudeAiReport) -> Result<()> {
    let name = file_name(path);
    if name.ends_with(".zip") {
        let listed = fs::File::open(path)
            .with_context(|| format!("開けません: {}", path.display()))
            .and_then(|file| zip::ZipArchive::new(file).with_context(|| format!("zip として読めません: {}", path.display())))
            .map(|mut archive| {
                let mut names = Vec::new();
                for i in 0..archive.len() {
                    let Ok(entry) = archive.by_index(i) else { continue };
                    // 中のパスは使わない(ディスクに書かない)。名前の末尾だけ見る
                    let entry_name = entry.name().map(|n| n.into_owned()).unwrap_or_else(|_| String::from_utf8_lossy(entry.name_raw()).into_owned());
                    if !entry.is_dir() && entry_name.rsplit('/').next() == Some("conversations.json") {
                        names.push(entry_name);
                    }
                }
                names
            });
        match listed {
            Ok(names) => out.extend(names.into_iter().map(|name| Source::ZipEntry { zip: path.to_path_buf(), name })),
            Err(error) => report.failed_files.push(FailedFile { origin: path.display().to_string(), reason: format!("{error:#}") }),
        }
    } else if name.ends_with(".json") {
        out.push(Source::Json(path.to_path_buf()));
    } else {
        bail!("対応していない形です(JSON か zip を渡してください): {}", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use serde_json::json;
    use std::io::Write;

    /// 試験用の置き場。/tmp は使わず、プロジェクトの target/ の中に作って、終わったら消す。
    struct Scratch(PathBuf);
    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join(format!("test-claude-ai-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn message(uuid: &str, sender: &str, at: &str, body: &str) -> Value {
        json!({ "uuid": uuid, "sender": sender, "created_at": at, "text": body, "content": [{ "type": "text", "text": body }] })
    }

    /// 架空の会話 2 つ。1 つ目は、ツールの結果・空の発言・知らない送り手を含む。
    fn export() -> Value {
        json!([
            {
                "uuid": "c1111111-1111-1111-1111-111111111111", "name": "認証の方式", "created_at": "2026-10-01T01:00:00.000000Z",
                "chat_messages": [
                    message("m1", "human", "2026-10-01T01:00:00.123456Z", "ログインの認証をどうする?"),
                    { "uuid": "m2", "sender": "assistant", "created_at": "2026-10-01T01:00:05.000000+00:00",
                      "text": "ツールの結果は含めない",
                      "content": [{ "type": "thinking", "thinking": "考え中" }, { "type": "text", "text": "セッション方式にしましょう。" }, { "type": "tool_use", "name": "search" }] },
                    message("m3", "human", "2026-10-01T01:01:00.000000Z", "   "),
                    message("m4", "system", "2026-10-01T01:01:30.000000Z", "知らない送り手"),
                ]
            },
            {
                "uuid": "c2222222-2222-2222-2222-222222222222", "name": "", "created_at": "2026-10-02T01:00:00.000000Z",
                "chat_messages": [message("n1", "human", "2026-10-02T01:00:00.000000Z", "Rust のライフタイムを教えて")]
            }
        ])
    }

    fn rows(conn: &Connection) -> Vec<(String, String, String)> {
        let mut stmt = conn.prepare("SELECT role, content, occurred_at FROM events ORDER BY occurred_at, id").unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().map(|r| r.unwrap()).collect()
    }

    #[test]
    fn a_json_file_is_imported_with_roles_times_title_and_the_claude_ai_place() {
        let s = Scratch::new("json");
        fs::write(s.path("conversations.json"), export().to_string()).unwrap();
        let mut conn = db::open_memory().unwrap();
        let report = import_claude_ai(&s.path("conversations.json"), &mut conn).unwrap();
        assert_eq!((report.files, report.conversations, report.empty, report.inserted, report.updated), (1, 2, 0, 3, 0));
        assert!(report.skipped.is_empty());
        let got = rows(&conn);
        assert_eq!(got.len(), 3);
        assert_eq!(got[0], ("user".into(), "ログインの認証をどうする?".into(), "2026-10-01T01:00:00.123Z".into()));
        // 思考・ツールは保存せず、text ブロックだけ。`+00:00` の時刻も読める
        assert_eq!(got[1], ("assistant".into(), "セッション方式にしましょう。".into(), "2026-10-01T01:00:05.000Z".into()));
        let (tool, place, title): (String, String, Option<String>) = conn
            .query_row("SELECT source_tool, project_path, title FROM sessions WHERE source_file LIKE '%c1111111%'", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap();
        assert_eq!((tool.as_str(), place.as_str(), title.as_deref()), ("claude_ai", "claude.ai", Some("認証の方式")));
        // 題名が空の会話は、題名なし
        let none: Option<String> = conn.query_row("SELECT title FROM sessions WHERE source_file LIKE '%c2222222%'", [], |r| r.get(0)).unwrap();
        assert_eq!(none, None);
    }

    #[test]
    fn importing_the_same_export_again_adds_nothing_and_a_longer_conversation_adds_only_the_new_messages() {
        let s = Scratch::new("again");
        let file = s.path("conversations.json");
        fs::write(&file, export().to_string()).unwrap();
        let mut conn = db::open_memory().unwrap();
        import_claude_ai(&file, &mut conn).unwrap();
        let again = import_claude_ai(&file, &mut conn).unwrap();
        assert_eq!((again.inserted, again.updated), (0, 0));
        // 会話が伸びた新しいエクスポート(1 つ目に発言が 1 つ増え、既存の発言の本文が 1 つ変わる)
        let mut longer = export();
        let messages = longer[0]["chat_messages"].as_array_mut().unwrap();
        messages.push(message("m5", "human", "2026-10-01T02:00:00.000000Z", "ありがとう"));
        messages[0] = message("m1", "human", "2026-10-01T01:00:00.123456Z", "ログインの認証をどうする? (追記)");
        fs::write(&file, longer.to_string()).unwrap();
        let third = import_claude_ai(&file, &mut conn).unwrap();
        assert_eq!((third.inserted, third.updated), (1, 1));
        let count: i64 = conn.query_row("SELECT count(*) FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 4);
    }

    #[test]
    fn a_zip_and_a_folder_with_a_zip_work_and_only_conversations_json_is_read() {
        let s = Scratch::new("zip");
        let zip_path = s.path("conversations-000.zip");
        {
            let mut z = zip::ZipWriter::new(fs::File::create(&zip_path).unwrap());
            let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            z.start_file("conversations.json", options).unwrap();
            z.write_all(export().to_string().as_bytes()).unwrap();
            // 個人情報を含む別のファイルが入っていても、読まない
            z.start_file("users.json", options).unwrap();
            z.write_all(b"not json at all").unwrap();
            z.finish().unwrap();
        }
        let mut conn = db::open_memory().unwrap();
        let report = import_claude_ai(&zip_path, &mut conn).unwrap();
        assert_eq!((report.files, report.inserted), (1, 3));
        // 名前が conversations で始まらない zip は、フォルダから渡しても読まない
        let folder = s.path("folder");
        fs::create_dir_all(folder.join("deep/er")).unwrap();
        fs::copy(&zip_path, folder.join("deep/er/conversations-000.zip")).unwrap();
        fs::write(folder.join("projects-000.zip"), b"PK-not-read").unwrap();
        let mut conn = db::open_memory().unwrap();
        let report = import_claude_ai(&folder, &mut conn).unwrap();
        assert_eq!((report.files, report.inserted), (1, 3));
    }

    #[test]
    fn empty_and_too_long_conversations_are_reported_and_the_others_are_still_imported() {
        let s = Scratch::new("limits");
        let mut value = export();
        let list = value.as_array_mut().unwrap();
        list.push(json!({ "uuid": "empty-1", "name": "空", "chat_messages": [] }));
        list.push(json!({ "uuid": "noattach-1", "name": "添付だけ", "chat_messages": [{ "uuid": "x", "sender": "human", "text": "", "content": [{ "type": "tool_use" }] }] }));
        let long: Vec<Value> = (0..5001).map(|i| message(&format!("l{i}"), "human", "2026-10-03T01:00:00.000000Z", &format!("発言{i}"))).collect();
        list.push(json!({ "uuid": "long-1", "name": "長すぎる会話", "chat_messages": long }));
        list.push(json!({ "name": "uuid なし", "chat_messages": [] }));
        fs::write(s.path("conversations.json"), value.to_string()).unwrap();
        let mut conn = db::open_memory().unwrap();
        let report = import_claude_ai(&s.path("conversations.json"), &mut conn).unwrap();
        assert_eq!((report.conversations, report.empty, report.inserted), (6, 2, 3));
        assert_eq!(report.skipped.len(), 2, "{:?}", report.skipped);
        let long = report.skipped.iter().find(|k| k.id == "long-1").unwrap();
        assert_eq!(long.title, "長すぎる会話");
        assert!(long.reason.contains("too many messages"), "{}", long.reason);
        assert!(report.skipped.iter().any(|k| k.title == "uuid なし" && k.reason.contains("uuid")));
        // 長すぎる会話は、途中まで保存されない(全体を巻き戻す)
        let stray: i64 = conn.query_row("SELECT count(*) FROM sessions WHERE source_file LIKE '%long-1'", [], |r| r.get(0)).unwrap();
        assert_eq!(stray, 0);
    }

    #[test]
    fn wrong_inputs_are_reported_with_a_reason_and_nothing_is_saved() {
        let s = Scratch::new("bad");
        let mut conn = db::open_memory().unwrap();
        fs::write(s.path("conversations.json"), b"{\"not\": \"a list\"}").unwrap();
        let report = import_claude_ai(&s.path("conversations.json"), &mut conn).unwrap();
        assert_eq!(report.failed_files.len(), 1);
        assert!(report.failed_files[0].reason.contains("配列"), "{:?}", report.failed_files);
        assert!(report.has_problems());
        fs::write(s.path("broken.json"), b"[1, 2").unwrap();
        let report = import_claude_ai(&s.path("broken.json"), &mut conn).unwrap();
        assert!(report.failed_files[0].reason.contains("JSON として読めません"), "{:?}", report.failed_files);
        // 壊れた zip も、理由つきの報告になる(エラー終了ではない)
        fs::write(s.path("conversations-bad.zip"), b"not a zip").unwrap();
        let report = import_claude_ai(&s.path("conversations-bad.zip"), &mut conn).unwrap();
        assert_eq!((report.files, report.failed_files.len()), (0, 1));
        assert!(report.failed_files[0].reason.contains("zip として読めません"), "{:?}", report.failed_files);
        // 見つからない・読めない・対応していない形は、エラー
        let empty_dir = s.path("empty");
        fs::create_dir_all(&empty_dir).unwrap();
        assert!(import_claude_ai(&empty_dir, &mut conn).unwrap_err().to_string().contains("見つかりません"));
        assert!(import_claude_ai(&s.path("missing.json"), &mut conn).is_err());
        fs::write(s.path("notes.txt"), b"x").unwrap();
        assert!(import_claude_ai(&s.path("notes.txt"), &mut conn).unwrap_err().to_string().contains("対応していない形"));
        let count: i64 = conn.query_row("SELECT count(*) FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0);
    }

    fn zip_with(path: &Path, body: &[u8]) {
        let mut z = zip::ZipWriter::new(fs::File::create(path).unwrap());
        let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        z.start_file("conversations.json", options).unwrap();
        z.write_all(body).unwrap();
        z.finish().unwrap();
    }

    #[test]
    fn a_broken_second_file_does_not_hide_what_the_first_one_saved_and_the_run_is_flagged() {
        let s = Scratch::new("partial");
        let folder = s.path("export");
        fs::create_dir_all(&folder).unwrap();
        zip_with(&folder.join("conversations-000.zip"), export().to_string().as_bytes());
        zip_with(&folder.join("conversations-001.zip"), b"[{\"uuid\": \"x\", \"chat_messages\": [");
        let mut conn = db::open_memory().unwrap();
        let report = import_claude_ai(&folder, &mut conn).unwrap();
        // 000 は保存され、001 の失敗は報告に残る(エラー終了で、000 の結果が消えない)
        assert_eq!((report.files, report.conversations, report.inserted), (2, 2, 3));
        assert_eq!(report.failed_files.len(), 1);
        assert!(report.failed_files[0].origin.contains("conversations-001.zip!conversations.json"), "{:?}", report.failed_files);
        assert!(report.has_problems());
        // 問題が無ければ、印は立たない
        let mut conn = db::open_memory().unwrap();
        fs::remove_file(folder.join("conversations-001.zip")).unwrap();
        assert!(!import_claude_ai(&folder, &mut conn).unwrap().has_problems());
    }

    #[test]
    fn the_size_limit_refuses_a_big_file_and_a_big_zip_entry_but_not_one_just_under_it() {
        let s = Scratch::new("size");
        let body = export().to_string();
        let size = body.len() as u64;
        fs::write(s.path("conversations.json"), &body).unwrap();
        zip_with(&s.path("conversations-000.zip"), body.as_bytes());
        for target in ["conversations.json", "conversations-000.zip"] {
            let mut conn = db::open_memory().unwrap();
            let small = Limits { max_json_bytes: size - 1, ..Limits::default() };
            let report = import_with_limits(&s.path(target), &mut conn, &small).unwrap();
            assert_eq!(report.failed_files.len(), 1, "{target}");
            assert!(report.failed_files[0].reason.contains("大きすぎます"), "{:?}", report.failed_files);
            assert_eq!(report.inserted, 0);
            // ちょうどの大きさは、読める
            let exact = Limits { max_json_bytes: size, ..Limits::default() };
            let report = import_with_limits(&s.path(target), &mut conn, &exact).unwrap();
            assert!(report.failed_files.is_empty() && report.inserted == 3, "{target}: {report:?}");
        }
    }

    #[test]
    fn the_reader_stops_at_the_limit_even_when_the_declared_size_was_a_lie() {
        let data = vec![b'x'; 100];
        let mut ok = String::new();
        LimitReader { inner: io::Cursor::new(&data), left: 100 }.read_to_string(&mut ok).unwrap();
        assert_eq!(ok.len(), 100); // 上限ちょうどは、正常に終わる
        let mut buf = Vec::new();
        let error = LimitReader { inner: io::Cursor::new(&data), left: 99 }.read_to_end(&mut buf).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("上限"), "{error}");
        assert!(buf.len() <= 99);
        // ストリームの途中で超えたときも、取り込みはエラーとして報告される(要素の途中で止まる)
        let big = format!("[{{\"uuid\": \"u\", \"name\": \"{}\"}}]", "あ".repeat(1000));
        let mut seen = 0;
        let error = stream_conversations(io::Cursor::new(big.into_bytes()), &Limits { max_json_bytes: 200, max_depth: 1 }, "x", &mut |_| seen += 1).unwrap_err();
        assert!(error.to_string().contains("読めません"), "{error}");
        assert_eq!(seen, 0);
    }

    #[test]
    fn folders_are_read_only_down_to_the_depth_limit() {
        let s = Scratch::new("depth");
        let shallow = s.path("a/b");
        let deep = s.path("a/b/c/d/e");
        fs::create_dir_all(&deep).unwrap();
        fs::write(shallow.join("conversations.json"), export().to_string()).unwrap();
        fs::write(deep.join("conversations.json"), export().to_string()).unwrap();
        let mut conn = db::open_memory().unwrap();
        // 深さ 2 までなら、浅いほう(a/b = 深さ 1)だけ
        let report = import_with_limits(&s.path("a"), &mut conn, &Limits { max_depth: 2, ..Limits::default() }).unwrap();
        assert_eq!(report.files, 1);
        // 十分な深さなら、2 つとも
        let report = import_with_limits(&s.path("a"), &mut conn, &Limits { max_depth: 6, ..Limits::default() }).unwrap();
        assert_eq!(report.files, 2);
        // 深すぎるものしか無ければ、見つからないエラー
        let only_deep = Scratch::new("depth-only");
        let deep = only_deep.path("1/2/3/4");
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("conversations.json"), export().to_string()).unwrap();
        let error = import_with_limits(&only_deep.0, &mut db::open_memory().unwrap(), &Limits { max_depth: 2, ..Limits::default() }).unwrap_err();
        assert!(error.to_string().contains("見つかりません"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_refused_as_the_input_and_never_followed_inside_a_folder() {
        use std::os::unix::fs::symlink;
        let s = Scratch::new("symlink");
        let outside = s.path("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("conversations.json"), export().to_string()).unwrap();
        // 入力そのものがリンク
        symlink(outside.join("conversations.json"), s.path("link.json")).unwrap();
        let mut conn = db::open_memory().unwrap();
        let error = import_claude_ai(&s.path("link.json"), &mut conn).unwrap_err();
        assert!(error.to_string().contains("シンボリックリンク"), "{error}");
        // フォルダの中のリンク(ファイルも、フォルダも)は、たどらない
        let folder = s.path("folder");
        fs::create_dir_all(&folder).unwrap();
        symlink(outside.join("conversations.json"), folder.join("conversations.json")).unwrap();
        symlink(&outside, folder.join("dir-link")).unwrap();
        let error = import_claude_ai(&folder, &mut conn).unwrap_err();
        assert!(error.to_string().contains("見つかりません"), "{error}");
        let count: i64 = conn.query_row("SELECT count(*) FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn imported_conversations_are_found_by_search_including_a_typo() {
        let s = Scratch::new("search");
        fs::write(s.path("conversations.json"), export().to_string()).unwrap();
        let mut conn = db::open_memory().unwrap();
        import_claude_ai(&s.path("conversations.json"), &mut conn).unwrap();
        // 検索は、本文のほかに題名も探す。題名が「認証の方式」の会話は、本文に「認証」が無い発言(2 つ目)も当たる
        let exact = db::timeline_search(&conn, None, None, None, Some("認証"), 30, db::SearchOrder::Newest, true).unwrap();
        assert!(!exact.fuzzy);
        assert_eq!(exact.events.len(), 2);
        assert!(exact.events.iter().all(|e| e.project_path == "claude.ai"));
        // 本文だけにある語(題名には無い)も、探せる
        let body = db::timeline_search(&conn, None, None, None, Some("ライフタイム"), 30, db::SearchOrder::Newest, true).unwrap();
        assert!(!body.fuzzy);
        assert_eq!(body.events.len(), 1);
        // 誤字は、完全一致が無いときだけ、あいまい一致で探し直す
        let typo = db::timeline_search(&conn, None, None, None, Some("認承"), 30, db::SearchOrder::Newest, true).unwrap();
        assert!(typo.fuzzy && typo.events.len() == 2, "{typo:?}");
        let typo_body = db::timeline_search(&conn, None, None, None, Some("ライフタイブ"), 30, db::SearchOrder::Newest, true).unwrap();
        assert!(typo_body.fuzzy && typo_body.events.len() == 1, "{typo_body:?}");
    }
}
