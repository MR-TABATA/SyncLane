//! サーバーへ送る前に、会話の中の秘密(API キー・トークン・パスワードなど)を伏せる。
//!
//! 守ること:
//! - 伏せるのは「サーバーへ送る分」だけ。手元の SQLite と、手元の検索は、元のまま。
//! - 迷ったら伏せる(見逃すより、伏せすぎるほうがまし)。
//! - 何を伏せたかは、種類と件数だけを返す。伏せた中身は、ログにも画面にも出さない。
//!
//! 単語の境目は ASCII だけで判定する(`(?-u:\b)`)。普通の `\b` は日本語も単語と数えるので、
//! 「トークンはghp_…です」のように、日本語に挟まれた鍵を見逃す。
//!
//! ルールは、gitleaks などの定番のパターンから、よく出る形を選んだもの。
//! 先に、形が決まっている鍵(誤検出が少ない)を伏せ、最後に「password = …」のような、名前で当てる規則を当てる。

use regex::{Captures, Regex};
use rusqlite::Connection;
use std::collections::HashSet;
use std::sync::OnceLock;

/// ルールを変えたら上げる。これより古い規則で組んだ、未送信のバッチは、送らずに捨てて組み直す。
pub const MASK_VERSION: u32 = 1;

/// 伏せた箇所の置き換え。種類が分かる(中身は分からない)。
const MARK: &str = "[MASKED:";

/// 許可する値(ユーザーが決める)。ここにある文字列と**完全に一致する値**は、伏せずに送る。
///
/// データベースの隣の `mask-allow.txt` に、1 行に 1 つ書く(空の行と、`#` で始まる行は無視)。
/// 名前やパターンでは、許可できない。許可した値は、そのまま外に出る。
#[derive(Debug, Default, Clone)]
pub struct Allow(HashSet<String>);

impl Allow {
    pub const FILE_NAME: &'static str = "mask-allow.txt";

    pub fn from_text(text: &str) -> Allow {
        Allow(text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(str::to_string).collect())
    }

    fn contains(&self, value: &str) -> bool {
        self.0.contains(value)
    }
}

/// 必ず伏せる語(ユーザーが決める)。社内だけの名前(ホスト名・顧客名・案件のコード名など)は、既定のルールでは分からない。
///
/// データベースの隣の `mask-block.txt` に、1 行に 1 つ書く(空の行と、`#` で始まる行は無視)。
/// 普通の行は、その文字列そのもの(大文字小文字を区別する)。`re:` で始まる行は、正規表現。
/// 壊れた正規表現の行は無視して、数だけ数える。許可リストと同じ値があるときは、こちらが勝つ。
#[derive(Debug, Default, Clone)]
pub struct Block {
    literals: Vec<String>,
    patterns: Vec<Regex>,
    /// 正規表現として読めず、無視した行の数
    pub ignored: usize,
}

impl Block {
    pub const FILE_NAME: &'static str = "mask-block.txt";

    pub fn from_text(text: &str) -> Block {
        let mut block = Block::default();
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            match line.strip_prefix("re:") {
                Some(pattern) => match Regex::new(pattern.trim()) {
                    Ok(re) => block.patterns.push(re),
                    Err(_) => block.ignored += 1,
                },
                None => block.literals.push(line.to_string()),
            }
        }
        block
    }
}

/// 許可と、必ず伏せる語。どちらも、データベースの隣のファイルから読む。
#[derive(Debug, Default, Clone)]
pub struct Lists {
    pub allow: Allow,
    pub block: Block,
}

impl Lists {
    /// データベースの隣のファイルを読む。無い・読めないときは、空(許可なし・追加の語なし)。
    pub fn load_beside(conn: &Connection) -> Lists {
        let Some(db_path) = conn.path().filter(|p| !p.is_empty()) else { return Lists::default() };
        let Some(dir) = std::path::Path::new(db_path).parent() else { return Lists::default() };
        let read = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap_or_default();
        Lists { allow: Allow::from_text(&read(Allow::FILE_NAME)), block: Block::from_text(&read(Block::FILE_NAME)) }
    }
}

/// 設定の画面から編集する、2 つのファイル(`mask-allow.txt`・`mask-block.txt`)の中身を読む。無ければ空。
pub fn load_texts(dir: &std::path::Path) -> (String, String) {
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap_or_default();
    (read(Allow::FILE_NAME), read(Block::FILE_NAME))
}

/// 1 つのファイルの大きさの上限(これを超える入力は、受け付けない)
pub const LIST_MAX_BYTES: usize = 256 * 1024;

/// 設定の画面で入力した中身を、2 つのファイルに書く。正規表現として読めず、無視される行の数を返す。
/// どちらも空で、ファイルがまだ無いときは、何も作らない。書くときは、いったん別の名前で書いてから置き換える。
pub fn save_texts(dir: &std::path::Path, allow: &str, block: &str) -> anyhow::Result<usize> {
    for (name, text) in [(Allow::FILE_NAME, allow), (Block::FILE_NAME, block)] {
        anyhow::ensure!(text.len() <= LIST_MAX_BYTES, "{name} が大きすぎます({} KB まで)", LIST_MAX_BYTES / 1024);
    }
    for (name, text) in [(Allow::FILE_NAME, allow), (Block::FILE_NAME, block)] {
        let path = dir.join(name);
        if text.trim().is_empty() && !path.exists() {
            continue;
        }
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!("{name}.tmp"));
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &path)?;
    }
    Ok(Block::from_text(block).ignored)
}

/// 伏せた結果。`hits` は (種類, 件数)。`allowed` は、許可リストにあって、伏せずに通した数。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Masked {
    pub text: String,
    pub hits: Vec<(&'static str, usize)>,
    pub allowed: usize,
}

impl Masked {
    pub fn count(&self) -> usize {
        self.hits.iter().map(|(_, n)| n).sum()
    }
}

enum Scope {
    /// 一致した全体を伏せる
    Whole,
    /// 一致のうち、この番号のグループだけを伏せる(前後は残す)
    Group(usize),
}

struct Rule {
    kind: &'static str,
    re: Regex,
    scope: Scope,
}

fn rule(kind: &'static str, pattern: &str, scope: Scope) -> Rule {
    Rule { kind, re: Regex::new(pattern).unwrap_or_else(|e| panic!("mask rule {kind}: {e}")), scope }
}

fn rules() -> &'static [Rule] {
    static RULES: OnceLock<Vec<Rule>> = OnceLock::new();
    RULES.get_or_init(|| {
        use Scope::{Group, Whole};
        vec![
            // 形が決まっている鍵
            rule("private-key", r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----.*?-----END [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----", Whole),
            // 末尾が切れている(終わりの行が無い)鍵
            rule("private-key", r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----[A-Za-z0-9+/=\s]{40,}", Whole),
            rule("aws-access-key", r"(?-u:\b)(?:AKIA|ASIA|AGPA|AIDA|AROA|ANPA)[A-Z0-9]{16}(?-u:\b)", Whole),
            rule("github-token", r"(?-u:\b)gh[pousr]_[A-Za-z0-9]{36,}(?-u:\b)", Whole),
            rule("github-token", r"(?-u:\b)github_pat_[A-Za-z0-9_]{22,}", Whole),
            rule("anthropic-key", r"(?-u:\b)sk-ant-[A-Za-z0-9_-]{20,}", Whole),
            rule("openai-key", r"(?-u:\b)sk-(?:proj-|svcacct-|admin-)?[A-Za-z0-9_-]{32,}", Whole),
            rule("google-api-key", r"(?-u:\b)AIza[0-9A-Za-z_-]{35}(?-u:\b)", Whole),
            rule("slack-token", r"(?-u:\b)xox[abprso]-[A-Za-z0-9-]{10,}", Whole),
            rule("stripe-key", r"(?-u:\b)(?:[sr]k|whsec)_(?:live_|test_)?[A-Za-z0-9]{16,}", Whole),
            rule("npm-token", r"(?-u:\b)npm_[A-Za-z0-9]{36}(?-u:\b)", Whole),
            rule("jwt", r"(?-u:\b)eyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}", Whole),
            // 「Authorization: Bearer …」
            rule("bearer-token", r"(?i)((?-u:\b)Bearer\s+)([A-Za-z0-9._~+/=-]{20,})", Group(2)),
            // https://user:password@host
            rule("url-password", r"((?-u:\b)[A-Za-z][A-Za-z0-9+.-]*://[^\s/:@]+:)([^\s/@]+)(@)", Group(2)),
            // 名前で当てる: password = …, api_key: "…", AWS_SECRET_ACCESS_KEY=… など。伏せすぎる側に倒す
            rule(
                "secret-assignment",
                r#"(?i)((?-u:\b)[\w.-]*(?:password|passwd|pwd|secret|token|api[_-]?key|access[_-]?key|private[_-]?key|credential)[\w.-]*["']?\s*[:=]\s*["']?)([^\s"',;)}\]]{6,})"#,
                Group(2),
            ),
            rule("secret-assignment", r"((?:パスワード|暗証番号|秘密鍵|アクセストークン|API ?キー)\s*(?:は|：|:|=)\s*)([^\s、。,」』)]{6,})", Group(2)),
        ]
    })
}

fn mask_blocklist(text: &mut String, block: &Block) -> usize {
    let mark = format!("{MARK}blocklist]");
    let mut count = 0usize;
    for literal in &block.literals {
        let found = text.matches(literal.as_str()).count();
        if found > 0 {
            *text = text.replace(literal.as_str(), &mark);
            count += found;
        }
    }
    for re in &block.patterns {
        let replaced = re
            .replace_all(text, |caps: &Captures| {
                let m = caps.get(0).expect("group 0");
                if m.as_str().is_empty() || m.as_str().starts_with(MARK) {
                    return m.as_str().to_string(); // 空の一致や、伏せ済みの印は、触らない
                }
                count += 1;
                mark.clone()
            })
            .into_owned();
        *text = replaced;
    }
    count
}

/// 本文から、秘密に見えるものを伏せる(許可リストなし)。
pub fn mask(text: &str) -> Masked {
    mask_with(text, &Lists::default())
}

/// 本文から、秘密に見えるものを伏せる。許可リストにある値は、そのまま通す。
pub fn mask_with(text: &str, lists: &Lists) -> Masked {
    let allow = &lists.allow;
    let mut current = text.to_string();
    let mut hits: Vec<(&'static str, usize)> = Vec::new();
    let mut allowed = 0usize;
    // 先に、ユーザーが決めた語を伏せる(許可リストより強い)
    let blocked = mask_blocklist(&mut current, &lists.block);
    if blocked > 0 {
        hits.push(("blocklist", blocked));
    }
    for rule in rules() {
        let mut found = 0usize;
        let replaced = rule
            .re
            .replace_all(&current, |caps: &Captures| {
                let whole = caps.get(0).expect("group 0");
                match rule.scope {
                    Scope::Whole => {
                        if allow.contains(whole.as_str()) {
                            allowed += 1;
                            return whole.as_str().to_string();
                        }
                        found += 1;
                        format!("{MARK}{}]", rule.kind)
                    }
                    Scope::Group(n) => {
                        let Some(g) = caps.get(n) else { return whole.as_str().to_string() };
                        if g.as_str().starts_with(MARK) {
                            return whole.as_str().to_string(); // もう伏せてある
                        }
                        if allow.contains(g.as_str()) {
                            allowed += 1;
                            return whole.as_str().to_string();
                        }
                        found += 1;
                        let s = whole.as_str();
                        let (from, to) = (g.start() - whole.start(), g.end() - whole.start());
                        format!("{}{MARK}{}]{}", &s[..from], rule.kind, &s[to..])
                    }
                }
            })
            .into_owned();
        if found > 0 {
            current = replaced;
            match hits.iter_mut().find(|(k, _)| *k == rule.kind) {
                Some((_, n)) => *n += found,
                None => hits.push((rule.kind, found)),
            }
        }
    }
    Masked { text: current, hits, allowed }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn masked(text: &str) -> String {
        mask(text).text
    }

    #[test]
    fn plain_text_is_left_alone() {
        let text = "こんにちは。今日は Rust の所有権について話しましょう。token の数は 3 つです。";
        let m = mask(text);
        assert_eq!(m.text, text);
        assert_eq!(m.count(), 0);
    }

    #[test]
    fn well_known_keys_are_masked() {
        let aws = "AKIAIOSFODNN7EXAMPLE";
        let github = format!("ghp_{}", "a1B2c3D4e5F6g7H8i9J0k1L2m3N4o5P6q7R8");
        let anthropic = format!("sk-ant-api03-{}", "abcdefghij0123456789_-ABCDEFGHIJ");
        let openai = format!("sk-proj-{}", "abcdefghijklmnopqrstuvwxyz012345");
        let google = format!("AIza{}", "SyA-1234567890abcdefghijklmnopqrstu");
        let slack = "xoxb-123456789012-abcdefghijkl";
        let stripe = format!("sk_live_{}", "abcdefghijklmnop1234");
        for secret in [aws.to_string(), github, anthropic, openai, google, slack.to_string(), stripe] {
            let m = mask(&format!("キーは {secret} です"));
            assert!(!m.text.contains(&secret), "残っている: {}", m.text);
            assert!(m.text.starts_with("キーは [MASKED:") && m.text.ends_with("] です"), "{}", m.text);
            assert_eq!(m.count(), 1);
        }
    }

    #[test]
    fn secrets_glued_to_japanese_text_are_masked() {
        let token = format!("ghp_{}", "a1B2c3D4e5F6g7H8i9J0k1L2m3N4o5P6q7R8");
        assert_eq!(masked(&format!("トークンは{token}です")), "トークンは[MASKED:github-token]です");
        assert_eq!(masked("キーはAKIAIOSFODNN7EXAMPLEです"), "キーは[MASKED:aws-access-key]です");
        assert_eq!(masked("設定はDB_PASSWORD=hunter2hunter2です").contains("hunter2"), false);
    }

    #[test]
    fn private_key_blocks_are_masked_whole() {
        let key = "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEAx\nabcdefg\n-----END RSA PRIVATE KEY-----";
        let m = mask(&format!("貼ります:\n{key}\n以上"));
        assert_eq!(m.text, "貼ります:\n[MASKED:private-key]\n以上");
        // 末尾が切れていても、残さない
        let cut = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW";
        assert!(!masked(cut).contains("b3BlbnNz"));
    }

    #[test]
    fn jwt_and_bearer_tokens_are_masked() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abcdefghijklmnop";
        assert_eq!(masked(&format!("token {jwt}")), "token [MASKED:jwt]");
        let m = mask("curl -H 'Authorization: Bearer abcdefghijklmnopqrstuvwxyz123456'");
        assert_eq!(m.text, "curl -H 'Authorization: Bearer [MASKED:bearer-token]'");
    }

    #[test]
    fn passwords_in_urls_are_masked_but_the_host_stays() {
        assert_eq!(
            masked("postgres://app:s3cr3tpass@db.example.com:5432/main"),
            "postgres://app:[MASKED:url-password]@db.example.com:5432/main"
        );
    }

    #[test]
    fn named_assignments_are_masked_and_keep_their_name() {
        assert_eq!(masked("DB_PASSWORD=hunter2hunter2"), "DB_PASSWORD=[MASKED:secret-assignment]");
        assert_eq!(masked(r#"api_key: "abcd1234efgh""#), r#"api_key: "[MASKED:secret-assignment]""#);
        assert_eq!(masked("AWS_SECRET_ACCESS_KEY = wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"), "AWS_SECRET_ACCESS_KEY = [MASKED:secret-assignment]");
        assert_eq!(masked("パスワードは abc12345 にしました"), "パスワードは [MASKED:secret-assignment] にしました");
    }

    #[test]
    fn short_or_ordinary_values_are_not_touched() {
        for text in ["password: true", "token = 12", "secret: 無し", "max_tokens: 4096"] {
            assert_eq!(masked(text), text, "{text}");
        }
    }

    #[test]
    fn a_secret_masked_by_a_specific_rule_is_not_counted_twice() {
        let m = mask(&format!("GITHUB_TOKEN=ghp_{}", "a1B2c3D4e5F6g7H8i9J0k1L2m3N4o5P6q7R8"));
        assert_eq!(m.text, "GITHUB_TOKEN=[MASKED:github-token]");
        assert_eq!(m.hits, vec![("github-token", 1)]);
    }

    #[test]
    fn several_secrets_are_all_masked_and_counted_by_kind() {
        let m = mask(&format!("a AKIAIOSFODNN7EXAMPLE b AKIAIOSFODNN7EXAMPLF c ghp_{}", "a1B2c3D4e5F6g7H8i9J0k1L2m3N4o5P6q7R8"));
        assert_eq!(m.count(), 3);
        assert_eq!(m.hits, vec![("aws-access-key", 2), ("github-token", 1)]);
    }

    #[test]
    fn masking_twice_changes_nothing() {
        let once = masked("DB_PASSWORD=hunter2hunter2 and AKIAIOSFODNN7EXAMPLE");
        assert_eq!(masked(&once), once);
    }

    #[test]
    fn allowed_values_pass_through_but_only_on_an_exact_match() {
        let allow = Lists { allow: Allow::from_text("# デモ用\nhunter2hunter2\n\n  demo-pass-123  \n"), ..Lists::default() };
        let m = mask_with("DB_PASSWORD=hunter2hunter2 と PASSWORD=demo-pass-123", &allow);
        assert_eq!(m.text, "DB_PASSWORD=hunter2hunter2 と PASSWORD=demo-pass-123");
        assert_eq!((m.count(), m.allowed), (0, 2));
        // 一部が同じだけでは通さない
        let m = mask_with("DB_PASSWORD=hunter2hunter2-extra", &allow);
        assert_eq!(m.text, "DB_PASSWORD=[MASKED:secret-assignment]");
        assert_eq!((m.count(), m.allowed), (1, 0));
        // コメントの行は、許可にならない
        assert_eq!(mask_with("DB_PASSWORD=デモ用xxxxxxxx", &Lists { allow: Allow::from_text("# デモ用xxxxxxxx"), ..Lists::default() }).count(), 1);
    }

    #[test]
    fn a_whole_key_can_be_allowed_and_other_keys_are_still_masked() {
        let allow = Lists { allow: Allow::from_text("AKIAIOSFODNN7EXAMPLE"), ..Lists::default() };
        let m = mask_with("a AKIAIOSFODNN7EXAMPLE b AKIAIOSFODNN7EXAMPLF", &allow);
        assert_eq!(m.text, "a AKIAIOSFODNN7EXAMPLE b [MASKED:aws-access-key]");
        assert_eq!((m.count(), m.allowed), (1, 1));
    }

    #[test]
    fn a_missing_allow_file_allows_nothing() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let lists = Lists::load_beside(&conn);
        assert!(!lists.allow.contains("anything"));
        assert_eq!(mask_with("社内ホスト corp-db-01", &lists).count(), 0);
    }

    fn block(text: &str) -> Lists {
        Lists { block: Block::from_text(text), ..Lists::default() }
    }

    #[test]
    fn blocklisted_words_are_always_masked() {
        let lists = block("# 社内\ncorp-db-01\n  Project Aurora  \n");
        let m = mask_with("corp-db-01 に接続。Project Aurora の件。corp-db-01 は本番。", &lists);
        assert_eq!(m.text, "[MASKED:blocklist] に接続。[MASKED:blocklist] の件。[MASKED:blocklist] は本番。");
        assert_eq!(m.hits, vec![("blocklist", 3)]);
        // 大文字小文字は区別する
        assert_eq!(mask_with("PROJECT AURORA", &lists).count(), 0);
    }

    #[test]
    fn re_lines_are_regular_expressions_and_broken_ones_are_ignored_and_counted() {
        let lists = block("re: (?i)acme-[a-z]+-\\d{3}\nre:([\nre:\nre:(?:)\n");
        assert_eq!(lists.block.ignored, 1); // `([` だけが壊れている
        let m = mask_with("見てください ACME-tokyo-042 と acme-osaka-007 を", &lists);
        assert_eq!(m.text, "見てください [MASKED:blocklist] と [MASKED:blocklist] を");
        assert_eq!(m.hits, vec![("blocklist", 2)]);
        // 空にしか一致しない規則でも、本文は壊れない
        assert_eq!(mask_with("ふつう", &block("re:(?:)")).text, "ふつう");
    }

    #[test]
    fn a_rule_that_matches_everything_masks_everything_which_is_the_safe_side() {
        let m = mask_with("社外秘ではない文", &block("re:.+"));
        assert_eq!(m.text, "[MASKED:blocklist]");
    }

    #[test]
    fn the_blocklist_wins_over_the_allow_list() {
        let lists = Lists { allow: Allow::from_text("demo-pass-123"), block: Block::from_text("demo-pass-123") };
        let m = mask_with("PASSWORD=demo-pass-123", &lists);
        assert_eq!(m.text, "PASSWORD=[MASKED:blocklist]");
        assert_eq!((m.count(), m.allowed), (1, 0));
    }

    fn test_dir(name: &str) -> std::path::PathBuf {
        // /tmp は使わない。プロジェクトの target/ の中に作って、終わったら消す
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join(format!("test-mask-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn texts_are_saved_and_read_back_and_broken_regex_lines_are_counted() {
        let dir = test_dir("save");
        assert_eq!(load_texts(&dir), (String::new(), String::new())); // まだ何も無い
        let ignored = save_texts(&dir, "demo-pass-123\n", "corp-db-01\nre:([\nre:acme-\\d+\n").unwrap();
        assert_eq!(ignored, 1);
        let (allow, block) = load_texts(&dir);
        assert_eq!(allow, "demo-pass-123\n");
        assert_eq!(block, "corp-db-01\nre:([\nre:acme-\\d+\n");
        // 保存した中身は、そのまま伏せ方に効く
        let lists = Lists { allow: Allow::from_text(&allow), block: Block::from_text(&block) };
        assert_eq!(mask_with("corp-db-01 と acme-42", &lists).text, "[MASKED:blocklist] と [MASKED:blocklist]");
        assert!(!dir.join("mask-allow.txt.tmp").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn saving_two_empty_texts_creates_no_files_but_clearing_an_existing_one_empties_it() {
        let dir = test_dir("empty");
        assert_eq!(save_texts(&dir, "  \n", "").unwrap(), 0);
        assert!(!dir.exists());
        save_texts(&dir, "x-value-1", "").unwrap();
        save_texts(&dir, "", "").unwrap();
        assert_eq!(load_texts(&dir).0, "");
        assert!(dir.join("mask-allow.txt").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn oversized_input_is_refused_and_writes_nothing() {
        let dir = test_dir("big");
        let big = "a".repeat(LIST_MAX_BYTES + 1);
        assert!(save_texts(&dir, "ok-value-1", &big).is_err());
        assert!(!dir.exists());
    }
}
