# SyncLane

[日本語](README.md) | English

**Keep every conversation you have with AI, and find it again.**

SyncLane is a free, open-source Mac app that keeps your conversations with Claude Code, Codex, Gemini and ChatGPT in one place on your own computer. Search them by keyword or date, or let your AI search them for you.

![A day-by-day timeline. Your input on the left, the AI's reply on the right](art/readme/en/timeline.png)

*The screen shows made-up conversations.*

- **One place for your AI conversations**: Claude Code, Codex, Gemini CLI and Antigravity CLI are collected automatically; Gemini and ChatGPT (web) come in through a Chrome extension.
- **Ask your AI about your own past**: it works as an MCP server, so you can ask “How did we solve this before?” (read-only).
- **Your record stays on your Mac**: it is stored in a local SQLite file, and stays even if a tool stops showing its history. **By default nothing is sent anywhere.**
- **Filter by project, copy as Markdown, and see token usage**: see “What you can do in the window”.
- **Japanese works too**: full-text search, with substring matching for Japanese.
- Free and open source (MIT). Installs from a signed and notarized dmg ([download](https://mrktools.app/en/synclane/download/)).

Only if you connect to a SyncLane server (for example one your company runs) are the conversations you choose sent there (optional; see "Send to a server" below).

## What it imports

| Source | How |
|---|---|
| Claude Code | Automatic every 30 seconds while the app runs (`~/.claude/projects`) |
| Codex (Codex app, Codex CLI) | Automatic every 30 seconds while the app runs (`~/.codex/sessions`) |
| Gemini (web), ChatGPT (web) | Chrome extension ([how](#importing-gemini-and-chatgpt-conversations-chrome-extension)) |
| Gemini CLI | Automatic every 30 seconds while the app runs (`~/.gemini/tmp`) |
| Antigravity CLI (`agy`) | Automatic every 30 seconds while the app runs (`~/.gemini/antigravity-cli`) |

The command (CLI) is **not included in the download (dmg)**. Build it from the source (see "Development" below). Use it, for example, to import past conversations again for a given date. The downloaded app only has the window and the MCP server (`--mcp`) your AI uses to search.

It saves your input and the AI's text replies. Tool output and the AI's reasoning are not saved.

## Install

Download the dmg from [mrktools.app/en/synclane/download/](https://mrktools.app/en/synclane/download/) and put `SyncLane.app` in `/Applications`. It is signed and notarized, and the app tells you when a new version is out.

While the app is running, it imports new Claude Code conversations (`~/.claude/projects`), new Codex conversations (`~/.codex/sessions`), new Gemini CLI conversations (`~/.gemini/tmp`) and new Antigravity CLI conversations (`~/.gemini/antigravity-cli`) every 30 seconds.

## Importing Gemini and ChatGPT conversations (Chrome extension)

Conversations on the web versions of Gemini (gemini.google.com) and ChatGPT (chatgpt.com) have no local files, so a Chrome extension imports them.

1. Open [“SyncLane for AI chats” on the Chrome Web Store](https://chromewebstore.google.com/detail/synclane-for-ai-chats/jibhikabeebhfbhablfedojafekcmmog) and click "Add to Chrome".
2. Keep SyncLane running and open a conversation on Gemini or ChatGPT (reload tabs that were already open before you installed the extension). It is imported automatically once the answer is finished.

You can also install it by hand without the store (for developers):

1. Download and unzip the extension. This makes a `~/Downloads/SyncLane-extension` folder (do not delete it, or the extension stops working).

   ```
   https://mrktools.app/downloads/synclane/SyncLane-extension.zip
   ```

2. Open `chrome://extensions` in Chrome and turn on "Developer mode" (top right).
3. Click "Load unpacked" and choose `~/Downloads/SyncLane-extension`.

- It sends data only to the SyncLane app on this Mac (`127.0.0.1`), never to an external server. If the app is not running, it retries later.
- Only **conversations opened in Chrome** are imported. A conversation held only in the Gemini Mac app or on your phone is not imported, even though it syncs, until you open it once in Chrome.
- For message times, ChatGPT uses the time shown on the page. Gemini shows none, so the time recorded is when it was imported. Text is plain: formatting such as headings and tables is dropped.
- To use it from source, choose the `extension/` folder of this repository. Build the zip with `sh scripts/pack-extension.sh`.
- If nothing is imported, first check that **the extension is turned on** (the switch for “SyncLane for AI chats” at `chrome://extensions`), that the SyncLane app is running, and that you reloaded the tab. If the extension icon shows “×”, the conversation did not reach the app.
- If a site changes its page layout, importing may stop working. When a conversation page yields nothing, the extension icon shows "!" ("×" when the SyncLane app is not running). See [`extension/README.md`](extension/README.md) (in Japanese) for how to fix it.

## What you can do in the window

- **Filter by project**: the sidebar on the left lists your projects (name, last activity). Click one to show only that project; it combines with the date and search filters. Click it again to clear. It is hidden when the window is narrow (900px or less).
- **Copy as Markdown**: copy one topic, or everything the list currently shows (date, project, search words), as Markdown. You can put a request for another AI at the top (summarize / continue the work / review from another angle). Copying a range takes at most 50 topics, newest first. Anything that looks like a secret is masked by default.
- **See token usage**: “Token usage” in the header totals tokens for a period (today, 7 days, 30 days, all) by day, project, model or tool. **Only Claude Code, Codex and Gemini CLI are counted.** Antigravity and Gemini / ChatGPT on the web are not included. **Cost (money) is not calculated.** Only the counts are stored, not the conversation text.

![The token usage window (by project)](art/readme/en/usage.png)

*The screen shows made-up conversations.*

## How search works

It has a full-text index, and Japanese works as a substring match. Terms of 3 or more characters use the index; shorter terms scan the text directly. The index makes the database about 40% larger.

## Editing conversation titles

Use the ✎ next to a title in the list to rewrite it. Claude Code gives a conversation a title once, at the start, and never updates it when the topic changes.

- A title applies to **one stretch of the conversation (a "section")**, not the whole thing. In a long conversation, changing the title of one topic does not affect the others. Sections start automatically after a gap of **30 minutes or more**, and where the conversation is continued from a summary (where Claude Code summarised a long conversation and carried on). Save a title empty to remove it and go back to the title of the whole conversation.
- A title you edit is not overwritten by later imports.
- "Suggest from recent messages": proposes a title from your recent messages with local rules. Nothing leaves your Mac.
- What is sent to a server is still the title of the whole conversation (section titles are used only on your Mac).

## Send to a server (optional)

If you connect to a SyncLane server (for example one your company runs), you can send the conversations you choose to it over HTTPS, so your team can search past decisions together. The server is provided separately and is not part of this repository. **It is not available yet.**

- **If you do not connect, nothing is sent.** **Once you connect, every conversation except the ones you exclude is sent (automatic mode).** To send only the ones you pick, switch to manual mode (`synclane sync mode manual`) before the first sync, or exclude projects/tools with the rules.
- You connect by approving in your browser (the app never asks for a password). The device token is kept in the macOS keychain.
- What is sent: the text of the conversation (your messages and the AI's replies), the title, the project path and the times. You can exclude any conversation at any time.
- **Anything that looks like a secret is masked before sending.** API keys, tokens, private keys, passwords in URLs and assignments such as `password=…` or `…_key=…` are replaced with `[MASKED:kind]`. Only what is sent is masked; your local record and search are unchanged. It errs on the side of masking, so it may also mask something that is not a secret.
- **You can edit both lists in Settings → "Masking secrets"** (they are stored as files next to the database, which you can also edit directly).
- To send a value as it is, put it on its own line (`mask-allow.txt`; lines starting with `#` are comments). Only an **exact match** is let through, and the value is then sent as written.
- Names that only your organisation knows (host names, customer names, project code names) are not covered by the default rules. To always mask a word, put it on its own line (`mask-block.txt`). A plain line is matched as that exact text (case-sensitive); a line ending in `=` or `:` (such as `tabata_key=`) also masks the value that follows; a line starting with `re:` is a regular expression. Lines that are not valid regular expressions are ignored. If the same value is in `mask-allow.txt`, the block list wins.
- The local SQLite file stays the source of truth. If sending fails, importing and local search keep working. Disconnecting deletes the token (conversations already sent stay on the server).

```sh
synclane server connect https://sync.example.com   # approve in the browser
synclane sync list                                 # conversations and whether they are sent
synclane sync include 12 14 --visibility org       # pick conversations (org = visible to everyone on the server)
synclane sync exclude 13                           # never send this one
synclane sync mode auto                            # send everything except what you excluded
synclane sync                                      # send now (the app sends automatically)
synclane server disconnect
```

## Ask your AI (MCP)

Register it with Claude Code:

```sh
claude mcp add --scope user synclane -- /Applications/SyncLane.app/Contents/MacOS/synclane-app --mcp
```

For the Claude app (desktop), add this to `claude_desktop_config.json` (you can also copy it from the “MCP” tab in Settings):

```json
{ "mcpServers": { "synclane": { "command": "/Applications/SyncLane.app/Contents/MacOS/synclane-app", "args": ["--mcp"] } } }
```

Once registered, just ask your AI to “ask SyncLane”, and it searches your saved conversations to answer.

Your AI gets four tools (all read-only):

| Tool | What it does |
|---|---|
| `search_conversations` | Search conversations by keyword. Add `project` to limit it to one project |
| `get_session` | Read around a conversation you found |
| `list_projects` | List projects (name, location, number of conversations, tools used, last activity) |
| `get_project_context` | A project's recent state (titles of recent conversations and their last exchange) |

![Claude Code searching SyncLane and answering with a summary from the past](art/readme/en/ask-summary.png)

Once registered, that AI can read all of your saved conversations. Check first that they contain no passwords or sensitive code.

## Development

### Tauri app

A minimal window shows saved events in a day-by-day timeline.

```sh
cargo run --manifest-path src-tauri/Cargo.toml --offline
```

The window reads from the default SQLite file (`~/Library/Application Support/SyncLane/synclane.db`).

### Building on Windows (experimental, untested)

**The author has not run it on Windows.** The download (installer) is for macOS only. The code has Windows branches (for example the save location and opening the browser), but whether it builds and whether the window works has not been checked. If you try it, please tell us what went wrong in [Issues](https://github.com/MR-TABATA/SyncLane/issues).

You need: [Rust](https://rustup.rs) (MSVC toolchain), Visual Studio Build Tools (“Desktop development with C++”) and Git. The window (Tauri) also needs WebView2, which comes with Windows 11.

```powershell
git clone https://github.com/MR-TABATA/SyncLane.git
cd SyncLane

# The command (no window)
cargo build --release
.\target\release\synclane.exe --help

# The window
cargo run --manifest-path src-tauri/Cargo.toml
```

The database is at `%LOCALAPPDATA%\SyncLane\synclane.db`. It reads `%USERPROFILE%\.claude\projects` (Claude Code), `%USERPROFILE%\.codex\sessions` (Codex) and `%USERPROFILE%\.gemini\tmp` (Gemini CLI).

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

The app reads `~/.gemini/tmp/**/*.jsonl` every 30 seconds (only files whose size changed are read again). Gemini CLI writes the same conversation to many files (in a real log, 6 conversations had about 20,000 files), so only the newest copy of a conversation is read, and the last version of a message that was written many times is kept. The `import gemini` command reads the same logs and appends your input and Gemini's text replies. Gemini CLI's update log can repeat the same message array, so duplicates are dropped by message ID.

```sh
cargo run -- import gemini --date YYYY-MM-DD
cargo run -- import gemini --all
```

Omitting `--date` also targets every event, but use `--all` when you mean to import everything. `--date` and `--all` cannot be combined. Importing is idempotent: running it again on the same log does not duplicate events.

### Importing Antigravity CLI logs

The app reads `~/.gemini/antigravity-cli/conversations/*.db` (one SQLite file per conversation) every 30 seconds. Only conversations that were written to are read again, and the files are opened read-only, so Antigravity's records are never modified. Only your messages and the AI's replies are saved; tool runs and reasoning are not. The project cannot be determined yet, so every conversation shows as "Antigravity". There is no `import` command for it (the app's automatic import only).

- **The IDE version of Antigravity (`~/.gemini/antigravity/conversations/*.pb`) is not imported.** Its records appear to be encrypted and cannot be read. Conversations from the CLI (`agy`) are covered.
- The record format is not public, so the reading rules come from the structure of real records. If an Antigravity update changes the format, importing may stop working.

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
