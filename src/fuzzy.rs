//! 誤字を許すあいまい一致。完全一致が 1 件も無いときだけ、探し直しに使う(ふだんの検索は変えない)。
//!
//! 方式: 部分文字列の編集距離(Levenshtein 距離。Sellers 法)。語の長さ m・本文の長さ n として、
//! 時間は m×n に比例する。行のほとんどは、その前の絞り込みで落ちるので、実用上は速い。
//! - 絞り込み(k+1 分割の補題): k 個以内の誤りで当たるなら、語を k+1 個に分けた断片のどれかが、
//!   本文にそのまま含まれる(1 つの誤りは、高々 1 つの断片しか壊さないため)。速い完全一致で確かめる
//! - 距離: 残った行だけ、動的計画法で正確な距離を測る(点数付けに要る)
//!
//! 許す誤りの数は、語の長さで決める。
//! - 1 文字以下・65 文字以上: あいまいに探さない(完全一致だけ)。短すぎる語の誤字は、当たりすぎるため
//! - 2〜7 文字: 1 つまで(「認証」→「認承」、「Session」→「Sesison」など)
//! - 8〜64 文字: 2 つまで(「authentication」→「authnetication」のような入れ替わりは、距離 2)

/// 語(文字数)に許す誤りの数。あいまいに探さない語は None(完全一致だけ)。
pub fn max_errors(chars: usize) -> Option<usize> {
    match chars {
        0..=1 => None,
        2..=7 => Some(1),
        8..=64 => Some(2),
        _ => None,
    }
}

/// 照合の前の小文字化。日本語は変わらない。SQLite の `LOWER`(ASCII だけ)より、少し広い。
pub fn lowercase(text: &str) -> String {
    text.chars().flat_map(|c| c.to_lowercase()).collect()
}

/// 検索語 1 つ。小文字化と、絞り込みの断片を持つ。
pub struct QueryTerm {
    lowered: String,
    chars: Vec<char>,
    errors: Option<usize>,
    pieces: Vec<String>,
}

impl QueryTerm {
    pub fn new(term: &str) -> Self {
        let lowered = lowercase(term);
        let chars: Vec<char> = lowered.chars().collect();
        let errors = max_errors(chars.len());
        let pieces = errors.map(|k| split_pieces(&chars, k + 1)).unwrap_or_default();
        QueryTerm { lowered, chars, errors, pieces }
    }

    /// あいまいに探す語か(完全一致だけの語は false)。
    pub fn is_fuzzy(&self) -> bool {
        self.errors.is_some()
    }

    /// 本文(小文字化済み)の中の、語に最も近い位置の編集距離。当たらないときは None。
    pub fn distance(&self, text: &str) -> Option<usize> {
        match self.errors {
            None if text.contains(&self.lowered) => Some(0),
            None => None,
            Some(k) => {
                if !self.pieces.iter().any(|piece| text.contains(piece)) {
                    return None;
                }
                let best = substring_distance(&self.chars, text);
                (best <= k).then_some(best)
            }
        }
    }

    /// 本文(小文字化済み)との近さ(0〜1000)。語に当たらない(誤りが多すぎる)ときは None。
    pub fn points(&self, text: &str) -> Option<i64> {
        self.distance(text).map(|d| (self.chars.len() - d) as i64 * 1000 / self.chars.len().max(1) as i64)
    }
}

/// 語を n 個の空でない断片に分ける(n は 1 以上、語の長さは n 以上)。
fn split_pieces(chars: &[char], n: usize) -> Vec<String> {
    assert!(n > 0 && chars.len() >= n);
    let base = chars.len() / n;
    let extra = chars.len() % n; // 先頭の extra 個が、1 文字長い
    let mut pieces = Vec::with_capacity(n);
    let mut start = 0;
    for i in 0..n {
        let len = base + usize::from(i < extra);
        pieces.push(chars[start..start + len].iter().collect());
        start += len;
    }
    pieces
}

/// 部分文字列の編集距離。本文の各位置で終わる当たりのうち、最小の距離(Sellers 法)。
/// 列の最小値が許す誤りを超えても、後で下がることがあるので、途中で打ち切らない。
fn substring_distance(pattern: &[char], text: &str) -> usize {
    let mut prev: Vec<usize> = (0..=pattern.len()).collect();
    let mut cur: Vec<usize> = vec![0; pattern.len() + 1];
    let mut best = pattern.len();
    if best == 0 {
        return 0;
    }
    for c in text.chars() {
        cur[0] = 0; // 空の語は、どこでも距離 0(部分文字列なので)
        for (j, &p) in pattern.iter().enumerate() {
            let cost = usize::from(p != c);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        best = best.min(cur[pattern.len()]);
        if best == 0 {
            return 0; // これ以上は良くならない
        }
        std::mem::swap(&mut prev, &mut cur); // 配列は 2 本を使い回す(文字ごとに確保しない)
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_and_very_long_terms_are_exact_only() {
        assert_eq!(max_errors(0), None);
        assert_eq!(max_errors(1), None);
        assert_eq!(max_errors(2), Some(1));
        assert_eq!(max_errors(7), Some(1));
        assert_eq!(max_errors(8), Some(2));
        assert_eq!(max_errors(64), Some(2));
        assert_eq!(max_errors(65), None);
        assert!(!QueryTerm::new("あ").is_fuzzy());
        assert!(!QueryTerm::new(&"あ".repeat(65)).is_fuzzy());
        assert!(QueryTerm::new("認証").is_fuzzy());
    }

    #[test]
    fn the_pieces_are_not_empty_and_join_back_to_the_term() {
        for (term, n) in [("ab", 2), ("abcdefg", 2), ("abcdefgh", 3), ("あ".repeat(64).as_str(), 3)] {
            let chars: Vec<char> = term.chars().collect();
            let pieces = split_pieces(&chars, n);
            assert_eq!(pieces.len(), n);
            assert!(pieces.iter().all(|p| !p.is_empty()));
            assert_eq!(pieces.join(""), term);
        }
    }

    #[test]
    fn english_typos_are_found_with_their_distance() {
        let text = &lowercase("how to configure authentication in this app");
        // 入れ替わりは、Levenshtein 距離では 2(2 文字の置き換え)
        assert_eq!(QueryTerm::new("authnetication").distance(text), Some(2));
        assert_eq!(QueryTerm::new("authentiction").distance(text), Some(1)); // 1 文字欠け('a' が無い)
        assert_eq!(QueryTerm::new("authenttication").distance(text), Some(1)); // 1 文字増え
        assert_eq!(QueryTerm::new("sesion").distance(&lowercase("session handling")), Some(1)); // 1 文字欠け
        // 入れ替わりは距離 2。7 文字の語の許容(1)を超えるので、当たらない
        assert_eq!(QueryTerm::new("sessoin").distance(&lowercase("session handling")), None);
        // 誤りが多すぎる語は、当たらない
        assert_eq!(QueryTerm::new("authxyzation").distance(text), None);
        assert_eq!(QueryTerm::new("xyz").distance(text), None);
    }

    #[test]
    fn japanese_one_character_differences_are_found() {
        let text = "認証の方式を認可サーバーで確認する";
        assert_eq!(QueryTerm::new("認承").distance(text), Some(1)); // 「認証」の 1 文字違い
        assert_eq!(QueryTerm::new("認証").distance(text), Some(0));
        assert_eq!(QueryTerm::new("認可").distance(text), Some(0));
        assert_eq!(QueryTerm::new("承服").distance(text), None); // 2 文字とも本文に無い
    }

    #[test]
    fn hand_checked_distances_cover_insertions_deletions_and_moves() {
        assert_eq!(QueryTerm::new("abc").distance("aXbc"), Some(1)); // 差し込み 1 つ
        assert_eq!(QueryTerm::new("abcdefgh").distance("aXbcYdefgh"), Some(2)); // 差し込み 2 つ(8 文字の語は 2 つまで許す)
        assert_eq!(QueryTerm::new("abc").distance("axc"), Some(1)); // 置き換え
        assert_eq!(QueryTerm::new("abc").distance("ab"), Some(1)); // 語の末尾の欠け
        assert_eq!(QueryTerm::new("abc").distance("bc"), Some(1)); // 語の先頭の欠け
        assert_eq!(QueryTerm::new("abc").distance("xxabcxx"), Some(0));
        assert_eq!(QueryTerm::new("abc").distance("xxx"), None);
        assert_eq!(QueryTerm::new("abc").distance(""), None); // 空の本文は、語の全削除(距離 3)
    }

    #[test]
    fn case_does_not_matter() {
        assert_eq!(QueryTerm::new("Session").distance(&lowercase("SESSION handling")), Some(0));
        assert_eq!(QueryTerm::new("SESSION").distance(&lowercase("session handling")), Some(0));
    }

    #[test]
    fn exact_only_terms_need_a_substring() {
        assert_eq!(QueryTerm::new("あ").points("あいうえお"), Some(1000));
        assert_eq!(QueryTerm::new("あ").points("かきくけこ"), None);
        let long = "あ".repeat(65);
        assert_eq!(QueryTerm::new(&long).points(&format!("xx{long}xx")), Some(1000));
        assert_eq!(QueryTerm::new(&long).points("短い本文"), None);
    }

    #[test]
    fn closer_spellings_score_higher() {
        let exact = QueryTerm::new("authentication");
        let near = QueryTerm::new("authnetication"); // 距離 2
        assert!(exact.points("authentication").unwrap() > near.points("authentication").unwrap());
        assert!(near.points("authentication").unwrap() > 0);
    }

    #[test]
    fn the_piece_prefilter_never_drops_a_long_term_match() {
        // 8〜20 文字(2 文字の誤りを許す、3 分割の語)。本文は語の長さ+10 程度にして、当たりが十分に出るようにする。
        // 語の長さの帯ごとに、当たりの数を数えて、どの帯でも試験になっていることを確かめる
        let alphabet = ['a', 'b', 'あ', 'い'];
        let mut seed: u64 = 0x0fed_cba9_8765_4321;
        let mut next = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            alphabet[(seed >> 33) as usize % alphabet.len()]
        };
        let mut matches = [0usize; 3]; // 8〜11 / 12〜15 / 16〜20 文字
        for round in 0..6000 {
            let pattern_len = 8 + round % 13;
            let text_len = pattern_len + 10 - (round * 7) % 5;
            let pattern: String = (0..pattern_len).map(|_| next()).collect();
            let mut text: String = (0..text_len).map(|_| next()).collect();
            if round % 3 == 0 {
                // 語の途中を 1〜2 文字壊したものを本文に埋める(当たりを確実に作る)
                let broken: String = pattern.chars().enumerate().filter(|(i, _)| *i != pattern_len / 2 && *i != pattern_len / 3).map(|(_, c)| c).collect();
                text.insert_str(text.char_indices().nth(3).map(|(i, _)| i).unwrap_or(0), &broken);
            }
            let term = QueryTerm::new(&pattern);
            let errors = max_errors(pattern_len).unwrap();
            let expected = substring_distance(&pattern.chars().collect::<Vec<_>>(), &text);
            assert_eq!(term.distance(&text), (expected <= errors).then_some(expected), "語={pattern} 本文={text}");
            if expected <= errors {
                matches[match pattern_len { 8..=11 => 0, 12..=15 => 1, _ => 2 }] += 1;
            }
        }
        assert!(matches.iter().all(|&n| n >= 200), "どの長さの帯でも、当たりが十分に出ない: {matches:?}");
    }

    #[test]
    fn the_piece_prefilter_never_drops_a_match() {
        // 決まった種の乱数で、ASCII と日本語の混ざった語と本文を大量に作り、
        // 絞り込みつきの当たりが、絞り込みなしの正確な距離と一致することを確かめる
        let alphabet = ['a', 'b', 'c', 'あ', 'い', '認', '証'];
        let mut seed: u64 = 0x1234_5678_9a_bc_de_f0;
        let mut next = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            alphabet[(seed >> 33) as usize % alphabet.len()]
        };
        let mut matches = 0;
        for round in 0..5000 {
            let pattern_len = 2 + round % 63; // 2〜64 文字
            let text_len = (round * 37) % 41; // 0〜40 文字
            let pattern: String = (0..pattern_len).map(|_| next()).collect();
            let text: String = (0..text_len).map(|_| next()).collect();
            let term = QueryTerm::new(&pattern);
            let errors = max_errors(pattern_len).unwrap();
            let expected = substring_distance(&pattern.chars().collect::<Vec<_>>(), &text);
            assert_eq!(term.distance(&text), (expected <= errors).then_some(expected), "語={pattern} 本文={text}");
            matches += usize::from(expected <= errors);
        }
        assert!(matches > 100, "当たりが少なすぎて、試験にならない: {matches}");
    }
}
