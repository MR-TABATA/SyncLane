# SyncLane

[日本語](README.md) | English

An app (macOS) that saves your conversations with AI tools such as Claude Code on your own computer, so you can search them later. They are stored only in a local SQLite file and are never sent to a server.

- Search past conversations by keyword or date
- Reread a day's conversations with AI in a day-by-day timeline
- Works as an MCP server, so your AI (Claude Code, the Claude app) can search your past conversations (read-only)
- Free and open source (MIT)

## Install

Download the dmg from [mrktools.app/en/synclane/download/](https://mrktools.app/en/synclane/download/) and put `SyncLane.app` in `/Applications`. It is signed and notarized, and the app tells you when a new version is out.

While the app is running, it imports new Claude Code conversations (`~/.claude/projects`) and new Codex conversations (`~/.codex/sessions`) every 30 seconds.

## Importing Gemini and ChatGPT conversations (Chrome extension)

Conversations on the web versions of Gemini (gemini.google.com) and ChatGPT (chatgpt.com) have no local files, so a Chrome extension imports them.

1. Open `chrome://extensions` in Chrome and turn on "Developer mode" (top right).
2. Click "Load unpacked" and choose the `extension/` folder of this repository.
3. Keep SyncLane running and open a conversation on Gemini or ChatGPT (reload tabs that were already open before you installed the extension). It is imported automatically once the answer is finished.

- It sends data only to the SyncLane app on this Mac (`127.0.0.1`), never to an external server. If the app is not running, it retries later.
- Only **conversations opened in Chrome** are imported. A conversation held only in the Gemini Mac app or on your phone is not imported, even though it syncs, until you open it once in Chrome.
- The page shows no time for each message, so the time recorded is when it was imported. Text is plain: formatting such as headings and tables is dropped.
- If a site changes its page layout, importing may stop working. When a conversation page yields nothing, the extension icon shows "!" ("×" when the SyncLane app is not running). See [`extension/README.md`](extension/README.md) (in Japanese) for how to fix it.

## Ask your AI (MCP)

Register it with Claude Code:

```sh
claude mcp add --scope user synclane -- /Applications/SyncLane.app/Contents/MacOS/synclane-app --mcp
```

For the Claude app (desktop), add this to `claude_desktop_config.json` (you can also copy it from the “MCP” tab in Settings):

```json
{ "mcpServers": { "synclane": { "command": "/Applications/SyncLane.app/Contents/MacOS/synclane-app", "args": ["--mcp"] } } }
```

Once registered, that AI can read all of your saved conversations. Check first that they contain no passwords or sensitive code.

## Development

### Tauri app

A minimal window shows saved events in a day-by-day timeline.

```sh
cargo run --manifest-path src-tauri/Cargo.toml --offline
```

The window reads from the default SQLite file (`~/Library/Application Support/SyncLane/synclane.db`).

### Tests

```sh
cargo test --offline
```

Plain `cargo test` also works when you are online.

### CLI

The default database location is:

```text
~/Library/Application Support/SyncLane/synclane.db
```

During development you can change it with `--db`.

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

You can also set the location with the `SYNCLANE_DB` environment variable.

```sh
SYNCLANE_DB=/tmp/synclane.db cargo run -- status
```

### Importing Claude Code logs

`import claude` reads `~/.claude/projects/**/*.jsonl` and appends your input and the AI's text replies to `sessions` / `events`. Tool results and thinking blocks are not saved at this stage.

```sh
cargo run -- import claude --date YYYY-MM-DD
cargo run -- import claude --all
```

### Importing Gemini CLI logs

`import gemini` reads `~/.gemini/tmp/**/*.jsonl` and appends your input and Gemini's text replies. Gemini CLI's update log can repeat the same message array, so duplicates are dropped by message ID.

```sh
cargo run -- import gemini --date YYYY-MM-DD
cargo run -- import gemini --all
```

Omitting `--date` also targets every event, but use `--all` when you mean to import everything. `--date` and `--all` cannot be combined. Importing is idempotent: running it again on the same log does not duplicate events.

### Importing Codex logs

`import codex` reads `~/.codex/sessions/**/rollout-*.jsonl` (logs of the ChatGPT Codex app and the Codex CLI) and appends your messages (`UserMessage`) and Codex's replies (`AgentMessage`). Reasoning, command runs and file changes are not saved. The project comes from the working folder in the log (`cwd`), and the title from the first message.

```sh
cargo run -- import codex --date YYYY-MM-DD
cargo run -- import codex --all
```

- Codex logs can reach hundreds of MB. The app remembers how far it has read and reads only what was appended.
- Branching or duplicating a conversation copies the original history into a new file. A message is saved once and stays in the original session (the copy holds only the messages after the branch).
- Work run on "This computer" in the app does not appear on the ChatGPT website. Import it from the Codex logs (this folder).

### Checking the timeline

`timeline` prints saved events, newest first. It is an early version meant to settle the display model for the Tauri window.

```sh
cargo run -- --db /tmp/synclane.db timeline --date YYYY-MM-DD --limit 20
```

### Current schema

See [docs/sqlite-schema.md](docs/sqlite-schema.md) for the SQLite design (in Japanese).

The main tables are:

- `sessions`: sessions of AI tools
- `events`: raw events such as your input, AI replies and tool runs
- `journals`: diaries generated by AI
- `analyses`: trend analyses per period
- `tags` / `event_tags`: tags
- `settings`: app settings

Events are append-only; the pair `source_tool` and `source_event_id` prevents duplicates.

## License

MIT ([LICENSE](LICENSE))
