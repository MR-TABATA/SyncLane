# SyncLane SQLite設計書

作成日: 2026-10-13

## 1. 基本方針

SyncLaneは、SQLiteの**追記型イベントストア**として設計する。

- 元のログイベントは原則として更新・削除しない
- 同じログを再読込しても重複しないようにする
- 日記やAI分析は上書きせず、生成ごとにバージョンとして追記する
- セッションの終了時刻や取り込み状態などのメタデータは更新してよい
- Key-Valueは設定や可変メタデータに限定する

## 2. テーブル一覧

| テーブル | 役割 | 更新方針 |
|---|---|---|
| `sessions` | 1回のCLI・AIツール利用セッション | 基本は作成。終了時刻・状態は更新可 |
| `events` | ユーザー入力、AI応答、ツール実行などの原イベント | 追記のみ |
| `journals` | AIが生成した日記本文 | 生成ごとに追記。過去版は保持 |
| `analyses` | 期間ごとの傾向・テーマなどのAI分析結果 | 分析ごとに追記。過去版は保持 |
| `tags` | イベントや日記に付けるタグ | 作成・名称変更可 |
| `event_tags` | イベントとタグの関連 | 追加・削除可 |
| `settings` | 保存先、取り込み設定などのアプリ設定 | Key-Valueとして更新可 |

## 3. `sessions`

| カラム | 型 | 内容 |
|---|---|---|
| `id` | INTEGER | 主キー |
| `source_tool` | TEXT | Claude Code、Gemini CLIなど |
| `project_path` | TEXT | 対象プロジェクトのパス |
| `started_at` | TEXT | セッション開始日時（ISO 8601） |
| `ended_at` | TEXT | セッション終了日時（ISO 8601、未確定ならNULL） |
| `source_file` | TEXT | 元ログファイルのパス |
| `source_hash` | TEXT | 取り込み元の識別子。現状はログ側のセッションID |
| `title` | TEXT | セッションの題名(Claude Code のログの `ai-title`。無いものはNULL。スキーマ版3で追加) |
| `created_at` | TEXT | 登録日時 |

## 4. `events`

| カラム | 型 | 内容 |
|---|---|---|
| `id` | INTEGER | 主キー |
| `session_id` | INTEGER | `sessions.id`への外部キー |
| `source_tool` | TEXT | 元ログのツール名 |
| `source_event_id` | TEXT | 元ログ側のイベントIDまたは安定した識別子 |
| `occurred_at` | TEXT | 発生日時（ISO 8601） |
| `role` | TEXT | `user`、`assistant`、`tool`など |
| `content` | TEXT | 保存する本文。必要に応じてマスク済み |
| `sequence` | INTEGER | セッション内の順序 |
| `created_at` | TEXT | 取り込み日時 |

`events` には、次の一意制約を設定する。

```sql
UNIQUE(source_tool, source_event_id)
```

これにより、ログを何度再走査しても同じイベントが二重登録されない。

## 5. `journals`

| カラム | 型 | 内容 |
|---|---|---|
| `id` | INTEGER | 主キー |
| `journal_date` | TEXT | 日記の日付 |
| `project_name` | TEXT | プロジェクト名 |
| `content_markdown` | TEXT | 生成されたMarkdown本文 |
| `model` | TEXT | 使用したモデル |
| `prompt_version` | TEXT | 要約プロンプトのバージョン |
| `created_at` | TEXT | 生成日時 |

同じ日付・プロジェクトの日記を再生成しても、過去の生成結果は削除せず、新しい行を追加する。最新版本は `created_at` または別途 `is_current` で判定する。

## 6. `analyses`

| カラム | 型 | 内容 |
|---|---|---|
| `id` | INTEGER | 主キー |
| `period_start` | TEXT | 分析対象期間の開始 |
| `period_end` | TEXT | 分析対象期間の終了 |
| `analysis_type` | TEXT | `trend`、`topic`、`difficulty`など |
| `result_json` | TEXT | AI分析結果のJSON |
| `model` | TEXT | 使用したモデル |
| `prompt_version` | TEXT | 分析プロンプトのバージョン |
| `created_at` | TEXT | 分析日時 |

## 7. `settings`

| カラム | 型 | 内容 |
|---|---|---|
| `key` | TEXT | 設定名。主キー |
| `value` | TEXT | 設定値。JSON文字列も可 |

Key-Value形式は、`settings` のような設定情報には使うが、セッション・イベント・日記・分析結果の正本には使わない。
