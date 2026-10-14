# MrVoie

AIとの会話や作業ログを収集し、日々の歩みとして保存・検索・表示するローカルファーストアプリ。

現在はSQLite保存層の最小実装段階。

## Tauri画面

保存済みのイベントを日付ごとのタイムラインで確認できる最小画面を用意している。

```sh
cargo run --manifest-path src-tauri/Cargo.toml --offline
```

画面のデータは既定のSQLite（`~/Library/Application Support/MrVoie/mrvoie.db`）から読み込む。

## 開発

```sh
cargo test --offline
```

ネットワークに接続できる環境では、通常の `cargo test` も使用できる。

## CLI

データベースの既定の保存先は次のとおり。

```text
~/Library/Application Support/MrVoie/mrvoie.db
```

開発中は `--db` で保存先を変更できる。

```sh
cargo run -- init
cargo run -- status
cargo run -- --db /tmp/mrvoie.db init
cargo run -- --db /tmp/mrvoie.db status
cargo run -- --db /tmp/mrvoie.db timeline --limit 20
cargo run -- --db /tmp/mrvoie.db timeline --date 2026-10-13 --limit 20
cargo run -- import claude --date 2026-10-13
cargo run -- --db /tmp/mrvoie.db import claude --root ~/.claude/projects --date 2026-10-13
```

`MRVOIE_DB` 環境変数でも保存先を指定できる。

```sh
MRVOIE_DB=/tmp/mrvoie.db cargo run -- status
```

## Claude Codeログの取り込み

`import claude` は `~/.claude/projects/**/*.jsonl` を読み、ユーザー入力とAIのテキスト応答を `sessions` / `events` に追記する。ツール実行結果やthinkingブロックは、現段階では保存しない。

```sh
cargo run -- import claude --date YYYY-MM-DD
cargo run -- import claude --all
```

## Gemini CLIログの取り込み

`import gemini` は `~/.gemini/tmp/**/*.jsonl` を読み、ユーザー入力とGeminiのテキスト応答を追記する。Gemini CLIの更新ログは同じメッセージ配列を繰り返すことがあるため、メッセージIDで重複を除外する。

```sh
cargo run -- import gemini --date YYYY-MM-DD
cargo run -- import gemini --all
```

`--date` を省略しても全イベントが対象になるが、明示的に取り込む場合は `--all` を使う。`--date` と `--all` は同時に指定できない。取り込みは冪等で、同じログを再実行してもイベントは重複しない。

## タイムラインの確認

`timeline` は保存済みイベントを新しい順に表示する。Tauri画面の表示モデルを固めるための初期版。

```sh
cargo run -- --db /tmp/mrvoie.db timeline --date YYYY-MM-DD --limit 20
```

## 現在のスキーマ

SQLiteの設計は [notes/sqlite-schema.md](notes/sqlite-schema.md) を参照。

主なテーブルは次のとおり。

- `sessions`: AIツールのセッション
- `events`: ユーザー入力・AI応答・ツール実行などの原イベント
- `journals`: AIが生成した日記
- `analyses`: 期間ごとの傾向分析
- `tags` / `event_tags`: タグ
- `settings`: アプリ設定

イベントは追記型で、`source_tool` と `source_event_id` の組み合わせで重複登録を防ぐ。
