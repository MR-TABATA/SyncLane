#!/usr/bin/env python3
"""claude.ai 公式エクスポートを SQLite(FTS5) に取り込む。標準ライブラリのみ。

  python3 claude_ai_import.py ingest <conversations.json | .zip | フォルダ>
  python3 claude_ai_import.py search 検索語 [検索語...] [--limit 20]
  python3 claude_ai_import.py watch      # メール確認→manifest取得→取り込みを自動で行う
  python3 claude_ai_import.py mail --dry-run   # Mailのリンク検出だけ確認(何も開かない)

保存するのは人間の入力とClaudeのテキスト応答のみ(ツール結果・思考は保存しない)。
重複は message uuid で防ぐので、何度取り込んでも安全。本文が変わった発言は更新する。

watch はダウンロードを ~/Downloads と Spotlight(ホーム以下すべて)で探す。ただし、取り込むのは
「ダウンロード元(kMDItemWhereFroms)が許可ホスト」のものだけ。出どころの記録が無いもの(Safari が zip を
展開したあとのフォルダなど)は、監視フォルダの直下にあるときだけ取り込む。決まった場所を足したいときは
CLAUDE_AI_DOWNLOADS に ":" 区切りで並べる。
"""
import argparse
import email
import fnmatch
import html
import json
import os
import re
import sqlite3
import subprocess
import sys
import time
import zipfile
from email import policy
from pathlib import Path
from urllib.parse import urlparse

DB = Path(os.environ.get("CLAUDE_AI_DB", "~/Library/Application Support/SyncLane/claude_ai.db")).expanduser()
WATCH_DIRS = [Path(p).expanduser() for p in os.environ.get("CLAUDE_AI_DOWNLOADS", "~/Downloads").split(os.pathsep) if p]
URL_TTL = 2 * 3600  # manifest の中の URL は約2時間で切れる。それより古い manifest は開かない
MAX_JSON_BYTES = 1 << 30  # 1 つの JSON を読む大きさの上限(展開後)。zip 爆弾でメモリを使い切らないため
MAX_ATTEMPTS = 3  # 1 つの manifest を開き直す回数の上限
RETRY_INTERVAL = 30 * 60  # 同じ manifest を開き直す間隔(秒)。ダウンロードの繰り返しを避ける

SCHEMA = """
CREATE TABLE IF NOT EXISTS conversations(
  uuid TEXT PRIMARY KEY, name TEXT, created_at TEXT, updated_at TEXT);
CREATE TABLE IF NOT EXISTS messages(
  uuid TEXT PRIMARY KEY, conversation_uuid TEXT NOT NULL REFERENCES conversations(uuid),
  sender TEXT, created_at TEXT, seq INTEGER, content TEXT);
CREATE INDEX IF NOT EXISTS idx_msg_conv ON messages(conversation_uuid, seq);
CREATE INDEX IF NOT EXISTS idx_msg_time ON messages(created_at);
CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
  content, content='messages', content_rowid='rowid', tokenize='trigram');
"""


def connect():
    DB.parent.mkdir(parents=True, exist_ok=True)
    db = sqlite3.connect(DB)
    db.executescript(SCHEMA)
    return db


def message_text(m):
    blocks = [b for b in (m.get("content") or []) if isinstance(b, dict) and b.get("type") == "text"]
    text = "\n".join(b.get("text") or "" for b in blocks if b.get("text")).strip()
    return text or (m.get("text") or "").strip()


def load_json(src: Path):
    if src.is_dir():
        src = next(src.rglob("conversations.json"), None)
        if src is None:
            raise FileNotFoundError("conversations.json が見つかりません")
    if src.suffix == ".zip":
        with zipfile.ZipFile(src) as z:
            info = next((i for i in z.infolist() if i.filename.rsplit("/", 1)[-1] == "conversations.json"), None)
            if info is None:
                raise FileNotFoundError("zip の中に conversations.json がありません")
            if info.file_size > MAX_JSON_BYTES:
                raise ValueError(f"大きすぎます({info.file_size} バイト)")
            with z.open(info) as f:
                data = f.read(MAX_JSON_BYTES + 1)  # 申告の大きさを信用せず、読む量にも上限を付ける
            if len(data) > MAX_JSON_BYTES:
                raise ValueError("展開した大きさが上限を超えました")
            return json.loads(data)
    if src.stat().st_size > MAX_JSON_BYTES:
        raise ValueError(f"大きすぎます({src.stat().st_size} バイト)")
    return json.loads(src.read_text(encoding="utf-8"))


def ingest(src):
    """取り込む。(新規, 更新) の発言数を返す。"""
    convs = load_json(Path(src).expanduser())
    db = connect()
    new = updated = 0
    with db:
        for c in convs:
            db.execute("INSERT INTO conversations VALUES(?,?,?,?) ON CONFLICT(uuid) DO UPDATE SET "
                       "name=excluded.name, updated_at=excluded.updated_at",
                       (c["uuid"], c.get("name"), c.get("created_at"), c.get("updated_at")))
            for i, m in enumerate(c.get("chat_messages") or []):
                text = message_text(m)
                if not text or not m.get("uuid"):
                    continue
                row = db.execute("SELECT rowid, content FROM messages WHERE uuid=?", (m["uuid"],)).fetchone()
                if row is None:
                    cur = db.execute("INSERT INTO messages VALUES(?,?,?,?,?,?)",
                                     (m["uuid"], c["uuid"], m.get("sender"), m.get("created_at"), i, text))
                    db.execute("INSERT INTO messages_fts(rowid, content) VALUES(?,?)", (cur.lastrowid, text))
                    new += 1
                elif row[1] != text:
                    # 発言が編集された。外部コンテンツ型の FTS は、古い本文を指定して消してから、新しい本文を入れる
                    db.execute("INSERT INTO messages_fts(messages_fts, rowid, content) VALUES('delete', ?, ?)", (row[0], row[1]))
                    db.execute("UPDATE messages SET content=?, sender=?, created_at=?, seq=? WHERE rowid=?",
                               (text, m.get("sender"), m.get("created_at"), i, row[0]))
                    db.execute("INSERT INTO messages_fts(rowid, content) VALUES(?,?)", (row[0], text))
                    updated += 1
    total = db.execute("SELECT count(*) FROM messages").fetchone()[0]
    db.close()
    print(f"取り込み: 会話{len(convs)}件 / 新規メッセージ{new}件 / 更新{updated}件 / DB合計{total}件 → {DB}")
    return new, updated


def search(terms, limit):
    db = connect()
    # trigramは3文字以上が必要。短い語はLIKEにフォールバック
    if all(len(t) >= 3 for t in terms):
        q = " AND ".join('"%s"' % t.replace('"', '""') for t in terms)
        rows = db.execute("""SELECT m.created_at, m.sender, c.name, snippet(messages_fts,0,'[',']','…',12)
            FROM messages_fts JOIN messages m ON m.rowid=messages_fts.rowid
            JOIN conversations c ON c.uuid=m.conversation_uuid
            WHERE messages_fts MATCH ? ORDER BY m.created_at DESC LIMIT ?""", (q, limit)).fetchall()
    else:
        where = " AND ".join("m.content LIKE ?" for _ in terms)
        rows = db.execute(f"""SELECT m.created_at, m.sender, c.name, substr(m.content,1,120)
            FROM messages m JOIN conversations c ON c.uuid=m.conversation_uuid
            WHERE {where} ORDER BY m.created_at DESC LIMIT ?""", [f"%{t}%" for t in terms] + [limit]).fetchall()
    db.close()
    for ts, who, name, snip in rows:
        # 時刻・送り手・本文が無い行(エクスポートの欠け)でも、表示で落ちない
        print(f"{(ts or '')[:16]} {who or '':9} {name or '(無題)'}\n    {(snip or '').replace(chr(10), ' ')}")
    print(f"-- {len(rows)}件")


# ---- Apple Mail から「エクスポート準備完了」メールのリンクを取り出す ----
MAIL_SUBJECT = "データのダウンロードの準備が完了しました"
ALLOWED_HOSTS = tuple(os.environ.get("CLAUDE_AI_LINK_HOSTS", "claude.ai,claude.com,anthropic.com").split(","))
RS = "\x1e"  # 区切りの文字(メール本文に出ない制御文字)
# 1 段目: 条件に合うメールの message id だけを返す(原文は取らない)
LIST_SCRIPT = """
on run argv
  set out to ""
  tell application "Mail"
    set cutoff to (current date) - 1 * days
    repeat with m in (messages of inbox whose date received > cutoff and sender contains "Anthropic" and subject contains (item 1 of argv))
      set out to out & (message id of m) & (character id 30)
    end repeat
  end tell
  return out
end run
"""
# 2 段目: 1 通の原文だけを返す。message id は引数(argv)で渡す。スクリプトの文字列には埋め込まない
# (メールの Message-ID は差出人が決められるので、埋め込むと AppleScript に命令を注入されうる)
SOURCE_SCRIPT = """
on run argv
  tell application "Mail"
    return source of (first message of inbox whose message id is (item 1 of argv))
  end tell
end run
"""


def parse_mail_links(raw_source):
    """メール原文(MIME)からHTML内のリンクを返す。(href, ボタン文字列)のリスト"""
    msg = email.message_from_string(raw_source, policy=policy.default)
    body = msg.get_body(preferencelist=("html",))
    if body is None:
        return []
    doc = body.get_content()
    links = []
    for m in re.finditer(r'<a\b[^>]*href=["\']([^"\']+)["\'][^>]*>(.*?)</a>', doc, re.S | re.I):
        label = re.sub(r"<[^>]+>", "", m.group(2))
        links.append((html.unescape(m.group(1)), html.unescape(label).strip()))
    return links


def is_allowed_url(url):
    """https で、許可したホスト(ALLOWED_HOSTS)宛ての URL だけ True。manifest やメールの URL を開く前の検査。
    Python と Safari で解釈が割れる形(`\\`・userinfo の `@`)や、見分けにくいホスト(非 ASCII)は、拒否する"""
    if not isinstance(url, str) or not url:
        return False
    if any(ord(c) <= 0x20 or ord(c) == 0x7F or c == "\\" for c in url):
        return False  # 空白・制御文字・バックスラッシュ(Safari は / として扱う)
    try:
        u = urlparse(url)
        port = u.port
    except ValueError:
        return False
    host = u.hostname or ""
    if u.scheme != "https" or not host or not host.isascii():
        return False
    if u.username is not None or u.password is not None or "@" in u.netloc:
        return False
    if port not in (None, 443):
        return False
    return any(host == h or host.endswith("." + h) for h in ALLOWED_HOSTS)


def pick_download_link(links):
    for href, label in links:
        if "ダウンロード" in label or "download" in label.lower():
            return href, urlparse(href).hostname or "", is_allowed_url(href)
    return None


def _osascript(script, arg):
    out = subprocess.run(["osascript", "-e", script, arg], capture_output=True, text=True, timeout=120)
    if out.returncode != 0:
        raise RuntimeError("Mailの読み取りに失敗: " + out.stderr.strip())
    return out.stdout


def fetch_mail_candidates(seen):
    """まだ見ていない通知メールだけ、原文を取って返す。(message id, 原文)。
    毎回すべての原文を取らない(巡回ごとの負荷を避ける)"""
    ids = [i.strip() for i in _osascript(LIST_SCRIPT, MAIL_SUBJECT).split(RS) if i.strip()]
    for mid in ids:
        if mid not in seen:
            yield mid, _osascript(SOURCE_SCRIPT, mid)


def open_in_safari(url):
    """Safari で URL を開く。開けたら True(open の終了コードが 0 のとき)"""
    try:
        return subprocess.run(["open", "-a", "Safari", url], check=False, timeout=30).returncode == 0
    except (OSError, subprocess.TimeoutExpired):
        return False


def check_mail(seen, dry_run=False):
    """新しい通知メールがあればリンクをSafariで開く。開いた場合 True"""
    opened = False
    for mid, src in fetch_mail_candidates(seen):
        picked = pick_download_link(parse_mail_links(src))
        if not picked:
            print(f"メール {mid[:20]}…: ダウンロードのリンクが見つかりません")
            continue
        href, host, ok = picked
        if dry_run:
            print(f"検出: host={host} 許可={'はい' if ok else 'いいえ'} (URLは表示しません)")
            continue
        if not ok:
            seen.add(mid)  # 許可されないホストは、何度見ても同じ結果なので既読にする
            print(f"スキップ: 許可されていないホスト {host} (必要なら CLAUDE_AI_LINK_HOSTS に追加)", file=sys.stderr)
            continue
        if not open_in_safari(href):
            print(f"Safari を起動できませんでした (host={host})。次の巡回でやり直します", file=sys.stderr)
            continue
        seen.add(mid)  # 開けたあとにだけ既読にする。失敗したメールは、次の巡回でもう一度試す
        print(f"メールのリンクをSafariで開きました (host={host})")
        opened = True
    return opened


# ---- watch: manifest を見つけたら conversations の URL だけ Safari で開く ----
def arrived_at(p):
    """そのファイルが今の場所に来た時刻。zip から展開したファイルは中身の日付(1980年など)を
    mtime に持つので、展開・移動のたびに更新される ctime も見る"""
    st = p.stat()
    return max(st.st_mtime, st.st_ctime)


def find_files(pattern, spotlight=True):
    """WATCH_DIRS の直下と、(spotlight=True なら)Spotlight(ホーム以下)から pattern に合うものを返す。
    Spotlight は重いので、呼ぶ側が頻度を決める"""
    found = {p for d in WATCH_DIRS for p in d.glob(pattern)}
    if spotlight:
        try:
            out = subprocess.run(["mdfind", "-onlyin", str(Path.home()), "-name", pattern.rstrip("*").split("*")[0]],
                                 capture_output=True, text=True, timeout=30).stdout
            found |= {Path(line) for line in out.splitlines() if fnmatch.fnmatch(Path(line).name, pattern)}
        except (OSError, subprocess.TimeoutExpired):
            pass  # Spotlight が使えなくても WATCH_DIRS は見る
    return [p for p in found if p.exists() and "/.Trash/" not in str(p)]


def size_of(p):
    return sum(f.stat().st_size for f in p.rglob("*") if f.is_file()) if p.is_dir() else p.stat().st_size


def where_from_urls(p):
    """ファイルのダウンロード元 URL(kMDItemWhereFroms)。記録が無ければ空のリスト"""
    try:
        out = subprocess.run(["mdls", "-raw", "-name", "kMDItemWhereFroms", str(p)],
                             capture_output=True, text=True, timeout=10).stdout
    except (OSError, subprocess.TimeoutExpired):
        return []
    return re.findall(r'"([^"]+)"', out)


def came_from_allowed_source(p):
    """取り込んでよい出どころか。ダウンロード元が許可ホストなら True。記録が無いもの(zip を展開したあとなど)は、
    監視フォルダの直下にあるときだけ True。ダウンロード元が許可外なら False"""
    urls = where_from_urls(p)
    if urls:
        return any(is_allowed_url(u) for u in urls)
    try:
        return p.parent.resolve() in {d.resolve() for d in WATCH_DIRS}
    except OSError:
        return False


def find_downloaded(since, filename, spotlight=True):
    """manifest が宣言したファイル名(例: conversations-000.zip)と、展開後の conversations.json /
    同名のフォルダだけを対象にする。出どころの分からない conversations* は取り込まない"""
    stem = Path(filename).stem
    wanted = {filename, stem, "conversations.json"}
    for p in find_files("conversations*", spotlight):
        if p.name not in wanted or p.name.endswith(".download") or arrived_at(p) < since:
            continue
        if not came_from_allowed_source(p):
            print(f"スキップ: ダウンロード元が許可されていません ({p.name})", file=sys.stderr)
            continue
        before = size_of(p)
        time.sleep(3)  # 書き込み・展開の途中なら、大きさが変わる
        if p.exists() and size_of(p) == before:
            return p
    return None


def process_manifest(mf, wait_limit):
    """manifest 1 つを処理する。取り込めた、または取り込む対象が無いと確定したら True。
    URL が不正・タイムアウトなど、もう一度試す価値があるときは False(既読にしない)"""
    data = json.loads(mf.read_text())
    item = next((d for d in data.get("data_files", []) if d.get("category") == "conversations"), None)
    if not item:
        return True  # conversations を含まない manifest は、何度見ても同じ
    url, filename = item.get("export_url"), item.get("filename") or "conversations.zip"
    if not isinstance(url, str) or not is_allowed_url(url):
        print(f"スキップ: manifest の URL が https の許可ホストではありません ({mf.name})", file=sys.stderr)
        return True  # 同じ manifest は同じ結果なので、既読にする
    t0 = time.time()
    if not open_in_safari(url):
        print("Safari を起動できませんでした。次の巡回でやり直します", file=sys.stderr)
        return False
    polls = 0
    while time.time() - t0 < wait_limit:
        time.sleep(5)
        polls += 1
        # Spotlight は重い。待っている間は、6 回に 1 回(約 30 秒ごと)だけ使う。ほかの回は監視フォルダだけ見る
        got = find_downloaded(t0, filename, spotlight=(polls % 6 == 1))
        if not got:
            continue
        try:
            print(f"取り込みます: {got}")
            ingest(got)
            return True
        except (OSError, StopIteration, ValueError, zipfile.BadZipFile) as e:
            # zip を Safari が展開して消した、など。次に現れたものを待つ(JSONDecodeError は ValueError)
            print(f"読めませんでした ({e!r})。待ち続けます", file=sys.stderr)
    print("タイムアウト: ダウンロードを確認できませんでした。次の巡回でやり直します", file=sys.stderr)
    return False


def may_retry(attempts, name, now):
    """この manifest をもう一度処理してよいか。回数の上限(MAX_ATTEMPTS)と、前回からの間隔(RETRY_INTERVAL)を守る"""
    count, last = attempts.get(name, (0, 0.0))
    return count < MAX_ATTEMPTS and (count == 0 or now - last >= RETRY_INTERVAL)


def watch(interval=30, wait_limit=600):
    state = DB.parent / "claude_ai_seen_manifests.txt"
    seen = set(state.read_text().split()) if state.exists() else set()
    mail_state = DB.parent / "claude_ai_seen_mails.txt"
    seen_mail = set(mail_state.read_text().split("\n")) if mail_state.exists() else set()
    attempts = {}  # manifest 名 → (試した回数, 最後に試した時刻)
    print(f"監視中: Mail の通知 → manifest-*.json ({', '.join(map(str, WATCH_DIRS))} と Spotlight)")
    loops = 0
    while True:
        try:
            before = len(seen_mail)
            check_mail(seen_mail)
            if len(seen_mail) != before:
                mail_state.parent.mkdir(parents=True, exist_ok=True)
                mail_state.write_text("\n".join(sorted(seen_mail)))
        except Exception as e:  # Mailが閉じている等でも監視は続ける
            print("mail確認エラー:", e, file=sys.stderr)
        loops += 1
        # Spotlight は重い。巡回 10 回に 1 回(約 5 分ごと)だけ使う。ほかの回は監視フォルダだけ見る
        for mf in find_files("manifest-*.json", spotlight=(loops % 10 == 1)):
            if mf.name in seen or arrived_at(mf) < time.time() - URL_TTL:
                continue
            if not may_retry(attempts, mf.name, time.time()):
                if attempts.get(mf.name, (0, 0.0))[0] >= MAX_ATTEMPTS:
                    print(f"あきらめました({MAX_ATTEMPTS} 回試しても取り込めません): {mf.name}", file=sys.stderr)
                    seen.add(mf.name)
                    state.parent.mkdir(parents=True, exist_ok=True)
                    state.write_text("\n".join(sorted(seen)))
                continue
            count, _ = attempts.get(mf.name, (0, 0.0))
            attempts[mf.name] = (count + 1, time.time())
            print(f"manifest を見つけました: {mf}")
            try:
                done = process_manifest(mf, wait_limit)
            except Exception as e:  # 1 つの manifest の失敗で、監視全体を止めない
                print(f"manifest の処理に失敗しました ({mf.name}): {e!r}。{RETRY_INTERVAL // 60} 分後にやり直します", file=sys.stderr)
                continue
            if done:  # 取り込めた(または対象が無いと確定した)ときだけ既読にする
                seen.add(mf.name)
                state.parent.mkdir(parents=True, exist_ok=True)
                state.write_text("\n".join(sorted(seen)))
        time.sleep(interval)


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("watch")
    p = sub.add_parser("mail")
    p.add_argument("--dry-run", action="store_true")
    p = sub.add_parser("ingest")
    p.add_argument("src")
    p = sub.add_parser("search")
    p.add_argument("terms", nargs="+")
    p.add_argument("--limit", type=int, default=20)
    a = ap.parse_args()
    {"ingest": lambda: ingest(a.src), "search": lambda: search(a.terms, a.limit), "watch": watch,
     "mail": lambda: check_mail(set(), dry_run=a.dry_run)}[a.cmd]()
