# SyncLane

日本語 | [English](README.en.md)

Claude Code などのAIとの会話を、自分のPCに保存して、あとから検索できるアプリ(macOS)。保存先はPCの中のSQLiteだけで、外のサーバーには送りません。

- キーワードや日付で、過去の会話を探せる
- 日付ごとのタイムラインで、その日のAIとの会話を読み返せる
- MCPサーバーとして、AI(Claude Code・Claude アプリ)から、過去の会話を検索できる(読み取り専用)
- 無料・オープンソース(MIT)

## インストール

[mrktools.app/ja/synclane/download/](https://mrktools.app/ja/synclane/download/) から、dmg をダウンロードして、`SyncLane.app` を `/Applications` に入れます。署名・公証済みで、新しい版は、アプリが自動で知らせます。

アプリを起動しておくと、30秒ごとに、Claude Code の新しい会話(`~/.claude/projects`)と、Codex の新しい会話(`~/.codex/sessions`)を取り込みます。

## Gemini・ChatGPT の会話を取り込む(Chrome 拡張)

Web 版の Gemini(gemini.google.com)と ChatGPT(chatgpt.com)の会話は、ローカルにファイルが無いので、Chrome 拡張で取り込みます。

1. Chrome で `chrome://extensions` を開き、右上の「デベロッパー モード」をオンにします。
2. 「パッケージ化されていない拡張機能を読み込む」で、このリポジトリの `extension/` フォルダを選びます。
3. SyncLane を起動したまま、Gemini か ChatGPT で会話を開きます(拡張を入れる前から開いていたタブは、再読み込みしてください)。回答が書き終わると、自動で取り込まれます。

- 送り先は、この Mac の SyncLane アプリ(`127.0.0.1`)だけです。外部のサーバーには送りません。アプリが起動していなければ、あとで送り直します。
- 取り込まれるのは、**Chrome で開いた会話**です。Gemini アプリ(Mac)やスマホだけで交わした会話は、同期されていても、Chrome で一度開くまで取り込まれません。
- 画面に発言の時刻が無いので、時刻は「取り込んだ時刻」になります。文章は、書式(見出し・表など)を外した文字だけです。
- 各サイトが画面の作りを変えると、取り込めなくなることがあります。会話の画面なのに何も読み取れないときは、拡張のアイコンに「!」が出ます(アプリが起動していないときは「×」)。直し方は [`extension/README.md`](extension/README.md) を見てください。

## AIに聞く(MCP)

Claude Code に登録します。

```sh
claude mcp add --scope user synclane -- /Applications/SyncLane.app/Contents/MacOS/synclane-app --mcp
```

Claude アプリ(デスクトップ)には、`claude_desktop_config.json` に次を足します(設定画面の「MCP」タブでもコピーできます)。

```json
{ "mcpServers": { "synclane": { "command": "/Applications/SyncLane.app/Contents/MacOS/synclane-app", "args": ["--mcp"] } } }
```

登録すると、そのAIは、保存した会話をすべて読めます。パスワードや機密のコードが入っていないか、先に確認してください。

## 開発

### Tauri画面

保存済みのイベントを日付ごとのタイムラインで確認できる最小画面を用意している。

```sh
cargo run --manifest-path src-tauri/Cargo.toml --offline
```

画面のデータは既定のSQLite（`~/Library/Application Support/SyncLane/synclane.db`）から読み込む。

### テスト

```sh
cargo test --offline
```

ネットワークに接続できる環境では、通常の `cargo test` も使用できる。

### CLI

データベースの既定の保存先は次のとおり。

```text
~/Library/Application Support/SyncLane/synclane.db
```

開発中は `--db` で保存先を変更できる。

```sh
cargo run -- init
cargo run -- status
cargo run -- --db /tmp/synclane.db init
cargo run -- --db /tmp/synclane.db status
cargo run -- --db /tmp/synclane.db timeline --limit 20
cargo run -- --db /tmp/synclane.db timeline --date 2026-10-13 --limit 20
cargo run -- import claude --date 2026-10-13
cargo run -- --db /tmp/synclane.db import claude --root ~/.claude/projects --date 2026-10-13
```

`SYNCLANE_DB` 環境変数でも保存先を指定できる。

```sh
SYNCLANE_DB=/tmp/synclane.db cargo run -- status
```

### Claude Codeログの取り込み

`import claude` は `~/.claude/projects/**/*.jsonl` を読み、ユーザー入力とAIのテキスト応答を `sessions` / `events` に追記する。ツール実行結果やthinkingブロックは、現段階では保存しない。

```sh
cargo run -- import claude --date YYYY-MM-DD
cargo run -- import claude --all
```

### Gemini CLIログの取り込み

`import gemini` は `~/.gemini/tmp/**/*.jsonl` を読み、ユーザー入力とGeminiのテキスト応答を追記する。Gemini CLIの更新ログは同じメッセージ配列を繰り返すことがあるため、メッセージIDで重複を除外する。

```sh
cargo run -- import gemini --date YYYY-MM-DD
cargo run -- import gemini --all
```

`--date` を省略しても全イベントが対象になるが、明示的に取り込む場合は `--all` を使う。`--date` と `--all` は同時に指定できない。取り込みは冪等で、同じログを再実行してもイベントは重複しない。

### Codexログの取り込み

`import codex` は `~/.codex/sessions/**/rollout-*.jsonl`(ChatGPT の Codex アプリと Codex CLI のログ)を読み、ユーザーの発言(`UserMessage`)と Codex の回答(`AgentMessage`)を追記する。考え中の表示・コマンドの実行・ファイルの変更などは保存しない。プロジェクトは、ログの作業フォルダ(`cwd`)から付け、題名は最初の発言から付ける。

```sh
cargo run -- import codex --date YYYY-MM-DD
cargo run -- import codex --all
```

- Codex のログは数百MBになる。アプリは、読み終えた位置を覚えて、追記された分だけを読む。
- 会話を分岐・複製すると、元の履歴が新しいファイルにコピーされる。同じ発言は1件だけ保存し、元のセッションに残す(複製側には、その後の発言が入る)。
- ChatGPT の Web 版には、アプリの「このコンピューター」で動かした作業は出ない。Codex のログ(このフォルダ)から取り込む。

### タイムラインの確認

`timeline` は保存済みイベントを新しい順に表示する。Tauri画面の表示モデルを固めるための初期版。

```sh
cargo run -- --db /tmp/synclane.db timeline --date YYYY-MM-DD --limit 20
```

### 現在のスキーマ

SQLiteの設計は [docs/sqlite-schema.md](docs/sqlite-schema.md) を参照。

主なテーブルは次のとおり。

- `sessions`: AIツールのセッション
- `events`: ユーザー入力・AI応答・ツール実行などの原イベント
- `journals`: AIが生成した日記
- `analyses`: 期間ごとの傾向分析
- `tags` / `event_tags`: タグ
- `settings`: アプリ設定

イベントは追記型で、`source_tool` と `source_event_id` の組み合わせで重複登録を防ぐ。


## ライセンス

MIT([LICENSE](LICENSE))
