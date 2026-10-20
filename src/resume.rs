//! 元のツールで、その会話を再開するコマンド(コピー用)。SyncLane が会話を開くのではなく、コマンドを渡すだけ。
//!
//! - Claude Code: `claude --resume <セッション ID>`。会話は、プロジェクトの場所ごとに保存されるので、その場所で実行する(`cd` を付ける)。
//!   セッション ID は、記録ファイルの名前(`<セッション ID>.jsonl`)。
//! - Codex: `codex resume <セッション ID>`。ID は、記録の先頭の `session_meta` の ID(記録ファイルの名前の末尾と同じ)。
//! - Gemini CLI: `gemini --resume <セッション ID>`(ヘルプには、番号と `latest` しか書かれていないが、UUID も使える。
//!   Gemini CLI 0.64.0 のエラー文: 「--resume {number}, --resume {uuid}, or --resume latest」)。ID は、記録の 1 行目の `sessionId`。
//!   Gemini CLI は、プロジェクトを短い名前(`~/.gemini/tmp/<名前>/chats`)で記録するので、SyncLane が持っているのは名前。
//!   本物の場所は、`~/.gemini/projects.json`(場所 → 名前)から逆引きする。引けなければ、`cd` は付けない。
//! - Web 版の Gemini・ChatGPT には、再開するコマンドが無い。

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;

/// ある発言の会話を、再開するコマンド。作れない(対応していない)ときは、None。
pub fn command(conn: &Connection, event_id: i64) -> Result<Option<String>> {
    let (tool, project, file, key): (String, String, Option<String>, Option<String>) = conn
        .query_row(
            "SELECT s.source_tool, s.project_path, s.source_file, s.source_hash FROM events e JOIN sessions s ON s.id = e.session_id WHERE e.id = ?1",
            [event_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .context("その発言が見つかりません")?;
    if tool == "gemini_cli" {
        let place = crate::paths::home_dir().and_then(|home| gemini_place_in(&home, &project));
        return Ok(key.as_deref().and_then(|id| build_gemini(id, place.as_deref())));
    }
    Ok(build(&tool, &project, file.as_deref(), key.as_deref()))
}

/// ターミナルで実行するスクリプトの中身。`command` は、`command()` が作った(場所は引用符つき・ID は検証済みの)コマンド。
/// - **`-il`(対話的なログインシェル)**で動かす。`-l` だけだと `.zshrc` を読まないので、`PATH` が、ふだんのターミナルと変わり、
///   別の場所の `claude`(古いインストール)や、別の `node` が使われて、落ちることがある(実際にあった)。
/// - 終わったとき、失敗していれば、理由の見当を表示する(場所や会話が無くなっている場合など)。変数名は、zsh で読み取り専用の `status` を避ける。
pub fn launch_script(command: &str) -> String {
    format!(
        "#!/bin/zsh -il\n{command}\nrc=$?\nif [ $rc -ne 0 ]; then\n  echo\n  echo \"SyncLane: 再開できませんでした(終了コード $rc)。プロジェクトの場所や、会話が、なくなっている可能性があります。\"\nfi\n"
    )
}

/// スクリプトを `dir` に書く(実行できる権限つき)。1 日より古い、前のスクリプトは消す。書いたファイルの場所を返す。
pub fn write_launch_file(dir: &Path, command: &str) -> Result<std::path::PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    // 前のスクリプトを片付ける(`resume-*.command` だけ)
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let old = entry.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > std::time::Duration::from_secs(24 * 3600));
            if name.starts_with("resume-") && name.ends_with(".command") && old {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let path = dir.join(format!("resume-{stamp}.command"));
    std::fs::write(&path, launch_script(command))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(path)
}

/// その会話を、新しいターミナルの窓で再開する(macOS)。`.command` ファイルを、ターミナルに開かせるだけ(オートメーションの許可は、要らない)。
/// 実行したコマンドを返す。コマンドを作れない会話は、エラー。
pub fn open_in_terminal(conn: &Connection, event_id: i64) -> Result<String> {
    let command = command(conn, event_id)?.context("この会話には、再開するコマンドがありません")?;
    #[cfg(target_os = "macos")]
    {
        let dir = crate::paths::app_data_dir().context("保存先が分かりません")?.join("launch");
        let path = write_launch_file(&dir, &command)?;
        let status = std::process::Command::new("open").arg(&path).status().context("open を実行できませんでした")?;
        anyhow::ensure!(status.success(), "ターミナルを開けませんでした");
        Ok(command)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = command;
        anyhow::bail!("ターミナルで開く機能は、macOS だけです。コマンドをコピーして使ってください")
    }
}

/// Gemini CLI のプロジェクトの名前から、本物の場所を引く(`<home>/.gemini/projects.json` は `{"projects": {場所: 名前}}`)。
pub fn gemini_place_in(home: &Path, name: &str) -> Option<String> {
    let text = std::fs::read_to_string(home.join(".gemini").join("projects.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value.get("projects")?.as_object()?.iter().find(|(_, n)| n.as_str() == Some(name)).map(|(place, _)| place.clone())
}

/// Gemini CLI の再開するコマンド。`place` が分かれば、そこへ移ってから実行する。
pub fn build_gemini(session_id: &str, place: Option<&str>) -> Option<String> {
    if !valid_id(session_id) {
        return None;
    }
    let program = format!("gemini --resume {session_id}");
    match place.map(str::trim).filter(|p| !p.is_empty() && !p.starts_with('(')) {
        Some(place) => Some(format!("cd {} && {program}", shell_quote(place))),
        None => Some(program),
    }
}

/// セッション ID として使える文字だけか(コマンドに差し込むので、記号や空白があるものは使わない)。
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 80 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// 記録ファイルの名前(拡張子を除く)。
fn file_stem(source_file: Option<&str>) -> Option<String> {
    source_file.and_then(|f| Path::new(f).file_stem()).and_then(|s| s.to_str()).map(str::to_owned)
}

/// 記録ファイルの名前の末尾にある UUID(`rollout-…-<UUID>.jsonl`)。
fn trailing_uuid(source_file: Option<&str>) -> Option<String> {
    let stem = file_stem(source_file)?;
    let tail = stem.get(stem.len().checked_sub(36)?..)?;
    let shape = [8, 13, 18, 23];
    let looks_like_uuid = tail.chars().enumerate().all(|(i, c)| if shape.contains(&i) { c == '-' } else { c.is_ascii_hexdigit() });
    looks_like_uuid.then(|| tail.to_string())
}

/// シェルに渡せる形にする(安全な文字だけなら、そのまま。それ以外は、一重引用符で囲む)。
fn shell_quote(text: &str) -> String {
    if !text.is_empty() && text.chars().all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c)) {
        text.to_string()
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}

/// 取り込み元・プロジェクトの場所・記録ファイル・保存してある ID から、再開するコマンドを作る。
pub fn build(source_tool: &str, project_path: &str, source_file: Option<&str>, key: Option<&str>) -> Option<String> {
    let (program, id) = match source_tool {
        "claude_code" => ("claude --resume", file_stem(source_file).filter(|id| valid_id(id)).or_else(|| key.map(str::to_owned).filter(|id| valid_id(id)))?),
        "codex" => ("codex resume", key.map(str::to_owned).filter(|id| valid_id(id)).or_else(|| trailing_uuid(source_file))?),
        _ => return None,
    };
    // 場所が分からない会話(`(unknown)`)は、`cd` を付けない
    let place = project_path.trim();
    if place.is_empty() || place.starts_with('(') {
        Some(format!("{program} {id}"))
    } else {
        Some(format!("cd {} && {program} {id}", shell_quote(place)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use rusqlite::params;

    const CLAUDE_ID: &str = "86dfdb3b-75dc-4155-a89c-1af3f2a62689";
    const CODEX_ID: &str = "019a1c2e-7b3d-7d51-8c2a-4f6e9a0b1c2d";

    #[test]
    fn claude_code_resumes_by_the_file_name_after_moving_to_the_project() {
        let file = format!("/Users/x/.claude/projects/-work-app/{CLAUDE_ID}.jsonl");
        assert_eq!(
            build("claude_code", "/work/app", Some(&file), Some("別の値")),
            Some(format!("cd /work/app && claude --resume {CLAUDE_ID}"))
        );
        // ファイル名が使えなければ、保存してある ID
        assert_eq!(build("claude_code", "/work/app", None, Some(CLAUDE_ID)), Some(format!("cd /work/app && claude --resume {CLAUDE_ID}")));
        // どちらも使えなければ、作らない
        assert_eq!(build("claude_code", "/work/app", Some("/a/b c;d.jsonl"), Some("x y")), None);
        assert_eq!(build("claude_code", "/work/app", None, None), None);
    }

    #[test]
    fn codex_resumes_by_the_session_id_or_the_end_of_the_file_name() {
        let file = format!("/Users/x/.codex/sessions/2026/10/20/rollout-2026-10-20T20-00-00-{CODEX_ID}.jsonl");
        assert_eq!(build("codex", "/work/app", Some(&file), Some(CODEX_ID)), Some(format!("cd /work/app && codex resume {CODEX_ID}")));
        assert_eq!(build("codex", "/work/app", Some(&file), None), Some(format!("cd /work/app && codex resume {CODEX_ID}")));
        assert_eq!(build("codex", "/work/app", Some("/a/rollout-x.jsonl"), None), None);
    }

    #[test]
    fn the_project_place_is_quoted_and_unknown_places_are_left_out() {
        let file = format!("/p/{CLAUDE_ID}.jsonl");
        assert_eq!(build("claude_code", "/Users/x/My Project", Some(&file), None), Some(format!("cd '/Users/x/My Project' && claude --resume {CLAUDE_ID}")));
        assert_eq!(build("claude_code", "/Users/x/it's", Some(&file), None), Some(format!("cd '/Users/x/it'\\''s' && claude --resume {CLAUDE_ID}")));
        // シェルが特別に扱う文字も、引用符の中に入る
        assert_eq!(build("claude_code", "/a;rm -rf $HOME", Some(&file), None), Some(format!("cd '/a;rm -rf $HOME' && claude --resume {CLAUDE_ID}")));
        assert_eq!(build("claude_code", "(unknown)", Some(&file), None), Some(format!("claude --resume {CLAUDE_ID}")));
        assert_eq!(build("claude_code", "", Some(&file), None), Some(format!("claude --resume {CLAUDE_ID}")));
    }

    #[test]
    fn tools_without_a_resume_command_give_none() {
        // Gemini CLI は、`command` が別に作る(`build` は扱わない)
        for tool in ["gemini_cli", "gemini_web", "chatgpt_web", "unknown"] {
            assert_eq!(build(tool, "/work/app", Some("/a/b.jsonl"), Some("abc")), None, "{tool}");
        }
    }

    #[test]
    fn the_launch_script_runs_the_command_and_explains_a_failure() {
        let script = launch_script("cd '/Users/x/My Project' && claude --resume abc-123");
        // `-il`: .zshrc も読む(ふだんのターミナルと同じ PATH)。`status` は zsh で読み取り専用なので、使わない
        assert!(script.starts_with("#!/bin/zsh -il\ncd '/Users/x/My Project' && claude --resume abc-123\nrc=$?\n"), "{script}");
        assert!(script.contains("終了コード $rc") && script.trim_end().ends_with("fi") && !script.contains("status"));
    }

    /// 実際に zsh で動かして、失敗したときの説明が出る(`status` が読み取り専用で、落ちた不具合の再発を防ぐ)。zsh が無い環境では、飛ばす。
    #[test]
    fn the_launch_script_really_runs_under_zsh_and_explains_a_failure() {
        if !Path::new("/bin/zsh").exists() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("synclane-zsh-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = write_launch_file(&dir, "sh -c 'exit 3'").unwrap();
        // `-f`: .zshrc などを読まない(試験を、利用者の設定に左右されないようにする)。スクリプトの中身だけを確かめる
        let output = std::process::Command::new("/bin/zsh").arg("-f").arg(&path).output().unwrap();
        let text = String::from_utf8_lossy(&output.stdout).to_string() + &String::from_utf8_lossy(&output.stderr);
        assert!(text.contains("再開できませんでした(終了コード 3)"), "{text}");
        assert!(!text.contains("read-only"), "{text}");
        // 成功したときは、何も出ない
        let ok = write_launch_file(&dir, "true").unwrap();
        let output = std::process::Command::new("/bin/zsh").arg("-f").arg(&ok).output().unwrap();
        assert!(output.stdout.is_empty() && output.stderr.is_empty(), "{}", String::from_utf8_lossy(&output.stderr));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_launch_file_is_executable_and_old_ones_are_cleaned_up() {
        let dir = std::env::temp_dir().join(format!("synclane-launch-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // 1 日より古い前のスクリプトと、関係のないファイル
        std::fs::create_dir_all(&dir).unwrap();
        let old = dir.join("resume-1.command");
        let other = dir.join("memo.txt");
        std::fs::write(&old, "古い").unwrap();
        std::fs::write(&other, "関係ない").unwrap();
        let two_days_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(48 * 3600);
        std::fs::File::options().write(true).open(&old).unwrap().set_modified(two_days_ago).unwrap();
        std::fs::File::options().write(true).open(&other).unwrap().set_modified(two_days_ago).unwrap();

        let path = write_launch_file(&dir, "claude --resume abc").unwrap();
        assert!(path.starts_with(&dir) && path.extension().is_some_and(|e| e == "command"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), launch_script("claude --resume abc"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o755);
        }
        assert!(!old.exists(), "古いスクリプトが残っている");
        assert!(other.exists(), "関係のないファイルは消さない");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn opening_in_a_terminal_needs_a_resume_command() {
        let conn = db::open_memory().unwrap();
        conn.execute("INSERT INTO sessions(source_tool, project_path, source_file, source_hash) VALUES ('gemini_web', '/w', 'f', 'k')", []).unwrap();
        let session = conn.last_insert_rowid();
        conn.execute("INSERT INTO events(session_id, source_tool, source_event_id, occurred_at, role, content, sequence) VALUES (?1, 'gemini_web', 'e', '2026-10-20T01:00:00.000Z', 'user', 'こんにちは', 0)", [session]).unwrap();
        let event = conn.last_insert_rowid();
        // Web 版には、コマンドが無い。ファイルも作らず、ターミナルも開かずに、エラーを返す
        assert!(open_in_terminal(&conn, event).is_err());
        assert!(open_in_terminal(&conn, 9999).is_err());
    }

    #[test]
    fn gemini_cli_resumes_by_uuid_in_the_project_found_from_its_name() {
        let id = "f7b30d28-48ba-4fa6-b8dc-f61ae83ea89d";
        assert_eq!(build_gemini(id, Some("/Users/x/My Project")), Some(format!("cd '/Users/x/My Project' && gemini --resume {id}")));
        // 場所が引けなければ、cd は付けない
        assert_eq!(build_gemini(id, None), Some(format!("gemini --resume {id}")));
        assert_eq!(build_gemini(id, Some("(unknown)")), Some(format!("gemini --resume {id}")));
        assert_eq!(build_gemini("x y;ls", Some("/a")), None);
        assert_eq!(build_gemini("", Some("/a")), None);

        // projects.json(場所 → 名前)から、名前で逆引きする
        let home = std::env::temp_dir().join(format!("synclane-resume-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".gemini")).unwrap();
        std::fs::write(home.join(".gemini").join("projects.json"), r#"{"projects": {"/Users/x/mdcrud": "mdcrud", "/Users/x/My Project": "my-project"}}"#).unwrap();
        assert_eq!(gemini_place_in(&home, "my-project"), Some("/Users/x/My Project".to_string()));
        assert_eq!(gemini_place_in(&home, "mdcrud"), Some("/Users/x/mdcrud".to_string()));
        assert_eq!(gemini_place_in(&home, "unknown-name"), None);
        // ファイルが無い・壊れている
        std::fs::write(home.join(".gemini").join("projects.json"), "壊れている").unwrap();
        assert_eq!(gemini_place_in(&home, "mdcrud"), None);
        std::fs::remove_dir_all(&home).unwrap();
        assert_eq!(gemini_place_in(Path::new("/no/such/home"), "mdcrud"), None);
    }

    #[test]
    fn the_command_is_built_from_a_saved_conversation() {
        let conn = db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO sessions(source_tool, project_path, source_file, source_hash) VALUES ('claude_code', '/work/app', ?1, ?2)",
            params![format!("/p/{CLAUDE_ID}.jsonl"), CLAUDE_ID],
        ).unwrap();
        let session = conn.last_insert_rowid();
        conn.execute("INSERT INTO events(session_id, source_tool, source_event_id, occurred_at, role, content, sequence) VALUES (?1, 'claude_code', 'e', '2026-10-20T01:00:00.000Z', 'user', 'こんにちは', 0)", [session]).unwrap();
        let event = conn.last_insert_rowid();
        assert_eq!(command(&conn, event).unwrap(), Some(format!("cd /work/app && claude --resume {CLAUDE_ID}")));
        assert!(command(&conn, 9999).is_err());
        // Web 版の会話には、作らない
        conn.execute("UPDATE sessions SET source_tool = 'gemini_web'", []).unwrap();
        assert_eq!(command(&conn, event).unwrap(), None);
        // Gemini CLI: 保存してある ID で作る(場所は、この端末の projects.json 次第なので、末尾だけを確かめる)
        conn.execute("UPDATE sessions SET source_tool = 'gemini_cli', source_hash = 'f7b30d28-48ba-4fa6-b8dc-f61ae83ea89d'", []).unwrap();
        let gemini = command(&conn, event).unwrap().unwrap();
        assert!(gemini.ends_with("gemini --resume f7b30d28-48ba-4fa6-b8dc-f61ae83ea89d"), "{gemini}");
        conn.execute("UPDATE sessions SET source_hash = NULL", []).unwrap();
        assert_eq!(command(&conn, event).unwrap(), None);
    }
}
