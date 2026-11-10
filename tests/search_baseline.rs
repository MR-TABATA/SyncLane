//! 検索の基準値(RAG 推進プラン §9 の段階 1)。
//!
//! 今の検索(FTS5 trigram の完全一致 + 完全一致が無いときのあいまい一致)が、
//! どんな質問を当て、どんな質問を外すかを、**架空の会話**で測る。本物の会話は使わない。
//! 意味検索(ベクトル)を足したときの、比較の基準になる。
//!
//! 実行: `cargo test --test search_baseline -- --nocapture`
//!
//! 注意: 質問と文書は、この作者が作った少数の例。件数が少ないので、数字は「傾向の確認」であり、
//! 一般的な精度ではない。外した質問の種類を見るためのもの。

use synclane::db::{self, SearchOrder};

/// (キー, 本文)。キーは `source_event_id` に入れて、結果と突き合わせる。
const DOCS: &[(&str, &str)] = &[
    ("d1", "ログイン画面で認証エラーが出る。トークンの有効期限が切れていたのが原因だった。"),
    ("d2", "アプリが突然クラッシュする。Segmentation fault が出ていて、unsafe ブロックの境界外アクセスだった。"),
    ("d3", "データベースのスキーマ移行で、古い版のアプリが開けなくなる問題を避けるため、別ファイルにした。"),
    ("d4", "ビルドが遅いので、キャッシュを有効にして CI の時間を半分にした。"),
    ("d5", "メモリ使用量が増え続ける。リークの原因は、イベントリスナーを解除していなかったこと。"),
    ("d6", "Use a connection pool because opening a new database connection per request was too slow."),
    ("d7", "依存ライブラリを v2 に上げたら API が変わっていたので、元に戻した。"),
    ("d8", "CORS のエラーで API が呼べない。サーバー側で Access-Control-Allow-Origin を設定した。"),
    ("d9", "ファンが回ってバッテリーが減る。バックグラウンドの取り込み間隔を伸ばして、負荷を下げた。"),
    ("d10", "UI のレイアウト崩れ。Safari だけで flex の gap が効かなかった。"),
    ("d11", "release note: the installer was notarized but stapling failed on the dmg."),
    ("d12", "AB テストの結果、青いボタンのほうがクリック率が高かった。"),
];

/// (種類, 質問, 正解のキー)。質問は MCP と同じく、空白区切りで AND。
/// 言い換えは、文書に無い語で書いた(文字は一致しない)。
const QUERIES: &[(&str, &str, &str)] = &[
    ("完全一致", "認証エラー トークン", "d1"),
    ("完全一致", "Segmentation fault", "d2"),
    ("完全一致", "connection pool", "d6"),
    ("完全一致", "CORS", "d8"),
    ("完全一致", "flex gap Safari", "d10"),
    ("完全一致", "API 元に戻した", "d7"),
    ("言い換え", "落ちる 原因", "d2"),
    ("言い換え", "メモリ 肥大化", "d5"),
    ("言い換え", "発熱 電池", "d9"),
    ("言い換え", "高速化 遅い", "d4"),
    ("言い換え", "データ移行 互換性", "d3"),
    ("言い換え", "ログインできない 理由", "d1"),
    ("誤字", "認承エラー", "d1"),
    ("誤字", "Segmentaton fault", "d2"),
    ("誤字", "conection pool", "d6"),
    ("誤字", "notarised dmg", "d11"),
    ("2文字語", "遅い", "d4"),
    ("2文字語", "移行", "d3"),
    ("2文字語", "崩れ", "d10"),
];

const K: usize = 5;

fn build() -> rusqlite::Connection {
    let conn = db::open_memory().expect("open_memory");
    conn.execute(
        "INSERT INTO sessions(source_tool, project_path, source_file, source_hash, title) VALUES ('test', '/work/fiction', 'f', 'f', '架空の会話')",
        [],
    )
    .unwrap();
    for (i, (key, content)) in DOCS.iter().enumerate() {
        conn.execute(
            "INSERT INTO events(session_id, source_tool, source_event_id, occurred_at, role, content, sequence)
             VALUES (1, 'test', ?1, ?2, 'user', ?3, ?4)",
            rusqlite::params![key, format!("2026-10-01T01:{:02}:00.000Z", i), content, i as i64],
        )
        .unwrap();
    }
    conn
}

/// 正解の順位(1 始まり)。結果に無ければ None。
fn rank_of(conn: &rusqlite::Connection, query: &str, answer: &str) -> Option<usize> {
    let hits = db::timeline_search(conn, None, None, None, Some(query), 10, SearchOrder::Relevance, true).expect("search");
    hits.events.iter().position(|e| e.source_event_id == answer).map(|p| p + 1)
}

#[test]
fn baseline_of_current_search() {
    let conn = build();
    assert!(db::has_search_index(&conn), "索引が無い: 評価の前提が崩れている");

    let mut categories: Vec<&str> = Vec::new();
    for (c, _, _) in QUERIES {
        if !categories.contains(c) {
            categories.push(c);
        }
    }

    println!("\n=== 今の検索の基準値(架空の会話 {} 件 / 質問 {} 件 / recall@{K}) ===", DOCS.len(), QUERIES.len());
    let mut exact_all_found = true;
    for cat in &categories {
        let mut found = 0usize;
        let mut total = 0usize;
        let mut rr = 0.0f64;
        for (c, q, a) in QUERIES.iter().filter(|(c, _, _)| c == cat) {
            let rank = rank_of(&conn, q, a);
            total += 1;
            if let Some(r) = rank.filter(|r| *r <= K) {
                found += 1;
                rr += 1.0 / r as f64;
            } else if *c == "完全一致" {
                exact_all_found = false;
            }
            println!("  [{c}] {q:<24} -> {}", rank.map(|r| format!("{r} 位")).unwrap_or_else(|| "外れ".into()));
        }
        println!("  ==> {cat}: recall@{K} = {found}/{total}, MRR = {:.2}\n", rr / total as f64);
    }

    // 完全一致の質問まで外れるなら、評価の仕組みか文書の作りが壊れている(精度の問題ではない)
    assert!(exact_all_found, "完全一致の質問が外れた: 評価データを見直す");
}
