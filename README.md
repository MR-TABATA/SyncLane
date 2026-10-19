# SyncLane

日本語 | [English](README.en.md)

Claude Code・Codex・Gemini・ChatGPT などのAIとの会話を、自分のPCに保存して、あとから検索できるアプリ(macOS)。保存先はPCの中のSQLiteです。**初期状態では、外のサーバーには何も送りません。** 会社などが用意した SyncLane サーバーに自分で接続した場合だけ、選んだ会話を送ります(任意。下の「サーバーに送る」)。

- キーワードや日付で、過去の会話を探せる
- 日付ごとのタイムラインで、その日のAIとの会話を読み返せる
- MCPサーバーとして、AI(Claude Code・Claude アプリ)から、過去の会話を検索できる(読み取り専用)
- 無料・オープンソース(MIT)

## 取り込めるもの

| 取り込み元 | 方法 |
|---|---|
| Claude Code | アプリ起動中、30秒ごとに自動(`~/.claude/projects`) |
| Codex(Codex アプリ・Codex CLI) | アプリ起動中、30秒ごとに自動(`~/.codex/sessions`) |
| Gemini(ウェブ版)・ChatGPT(ウェブ版) | Chrome 拡張([手順](#geminichatgpt-の会話を取り込むchrome-拡張)) |
| Gemini CLI | コマンド(`import gemini`。ソースからビルド) |

コマンド(CLI)は、**配布版(dmg)には入っていません**。ソースをビルドして使います(下の「開発」)。配布版のアプリは、画面と、AI から検索するための MCP(`--mcp`)だけです。

保存するのは、あなたの入力と、AIのテキストの応答です。ツールの実行結果と、AIの思考の過程は保存しません。

## インストール

[mrktools.app/ja/synclane/download/](https://mrktools.app/ja/synclane/download/) から、dmg をダウンロードして、`SyncLane.app` を `/Applications` に入れます。署名・公証済みで、新しい版は、アプリが自動で知らせます。

アプリを起動しておくと、30秒ごとに、Claude Code の新しい会話(`~/.claude/projects`)と、Codex の新しい会話(`~/.codex/sessions`)を取り込みます。

## Gemini・ChatGPT の会話を取り込む(Chrome 拡張)

Web 版の Gemini(gemini.google.com)と ChatGPT(chatgpt.com)の会話は、ローカルにファイルが無いので、Chrome 拡張で取り込みます。

1. 拡張をダウンロードして、展開します。`~/Downloads/SyncLane-extension` フォルダができます(このフォルダは、消さないでください。消すと、拡張が動かなくなります)。

   ```
   https://mrktools.app/downloads/synclane/SyncLane-extension.zip
   ```

2. Chrome で `chrome://extensions` を開き、右上の「デベロッパー モード」をオンにします。
3. 「パッケージ化されていない拡張機能を読み込む」で、`~/Downloads/SyncLane-extension` を選びます。
4. SyncLane を起動したまま、Gemini か ChatGPT で会話を開きます(拡張を入れる前から開いていたタブは、再読み込みしてください)。回答が書き終わると、自動で取り込まれます。

- 送り先は、この Mac の SyncLane アプリ(`127.0.0.1`)だけです。外部のサーバーには送りません。アプリが起動していなければ、あとで送り直します。
- 取り込まれるのは、**Chrome で開いた会話**です。Gemini アプリ(Mac)やスマホだけで交わした会話は、同期されていても、Chrome で一度開くまで取り込まれません。
- 発言の時刻は、ChatGPT は画面にある時刻を使います。Gemini は画面に時刻が無いので、「取り込んだ時刻」になります。文章は、書式(見出し・表など)を外した文字だけです。
- ソースから使う場合は、このリポジトリの `extension/` フォルダを選びます。zip は、`sh scripts/pack-extension.sh` で作ります。
- 各サイトが画面の作りを変えると、取り込めなくなることがあります。会話の画面なのに何も読み取れないときは、拡張のアイコンに「!」が出ます(アプリが起動していないときは「×」)。直し方は [`extension/README.md`](extension/README.md) を見てください。

## サーバーに送る(任意)

会社などが用意した SyncLane サーバーに接続すると、選んだ会話を、そのサーバーへ HTTPS で送れます。チームで過去の経緯を探せるようになります。サーバーは別に提供されるもので、このリポジトリには含まれません。

- **接続しなければ、何も送りません。** 接続しても、**既定では何も送りません。** 送る会話を自分で選ぶか、自動のモードにします。
- 接続は、ブラウザで承認する方式です(アプリにパスワードを入力しません)。端末のトークンは、macOS のキーチェーンに保存します。
- 送るのは、会話の本文(あなたの発言と AI の応答)、題名、プロジェクトのパス、時刻です。送らない会話は、いつでも除外できます。
- **秘密に見えるものは、伏せて送ります。** API キー・トークン・秘密鍵・URL の中のパスワード・`password=…` のような代入を、`[MASKED:種類]` に置き換えます。伏せるのは送る分だけで、手元の記録と検索は元のままです。迷ったら伏せる側に倒しているので、秘密でないものを伏せることもあります。
- 伏せなくてよい値は、データベースの隣の `mask-allow.txt` に、1 行に 1 つ書くと、そのまま送ります(`#` で始まる行はコメント)。値が**完全に一致**したときだけ通ります。書いた値は、そのまま外に出ます。
- 社内だけの名前(ホスト名・顧客名・案件のコード名など)は、既定のルールでは分かりません。必ず伏せたい語は、同じ場所の `mask-block.txt` に、1 行に 1 つ書きます。普通の行はその文字列そのもの(大文字小文字を区別)、`re:` で始まる行は正規表現です。読めない正規表現の行は無視します。`mask-allow.txt` と同じ値があるときは、こちらが優先です。
- 手元の SQLite が正本のままです。送れなくても、取り込みと手元の検索は止まりません。接続を解除すると、トークンを消します(サーバーに送り済みの会話は、サーバーに残ります)。

```sh
synclane server connect https://sync.example.com   # ブラウザで承認する
synclane sync list                                 # 会話と、送るかどうかの一覧
synclane sync include 12 14 --visibility org       # 会話を選んで送る(org は、サーバーの全員に見える)
synclane sync exclude 13                           # この会話は送らない
synclane sync mode auto                            # 除外した会話以外の全部を送る
synclane sync                                      # 送る(アプリは自動で送ります)
synclane server disconnect
```

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
