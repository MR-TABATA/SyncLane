"""claude_ai_import.py の試験。標準ライブラリだけ。Mail・Safari・Spotlight には触らない(すべて差し替える)。

  python3 -I -m unittest discover -s docs/claude_ai_import -p 'test_*.py'
"""
import contextlib
import io
import json
import sys
import tempfile
import unittest
import zipfile
from email.message import EmailMessage
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import claude_ai_import as mod  # noqa: E402


class FakeClock:
    """時間を進めるだけの偽物。sleep は待たずに、時計を進める"""

    def __init__(self):
        self.now = 1_000_000.0
        self.sleeps = []

    def time(self):
        return self.now

    def sleep(self, seconds):
        self.sleeps.append(seconds)
        self.now += seconds


def conversations(extra_text=""):
    return [
        {"uuid": "c1", "name": "認証の方式", "created_at": "2026-10-01T01:00:00Z", "updated_at": "2026-10-01T01:00:05Z", "chat_messages": [
            {"uuid": "m1", "sender": "human", "created_at": "2026-10-01T01:00:00Z", "text": "ログインの認証をどうする?" + extra_text,
             "content": [{"type": "text", "text": "ログインの認証をどうする?" + extra_text}]},
            {"uuid": "m2", "sender": "assistant", "created_at": "2026-10-01T01:00:05Z", "text": "ツールの結果",
             "content": [{"type": "thinking", "thinking": "考え中"}, {"type": "text", "text": "セッション方式にしましょう。"}, {"type": "tool_use"}]},
            {"uuid": "m3", "sender": "human", "created_at": "2026-10-01T01:01:00Z", "text": "   ", "content": []},
        ]},
    ]


class Base(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self._tmp.name)
        patcher = mock.patch.object(mod, "DB", self.dir / "test.db")
        patcher.start()
        self.addCleanup(patcher.stop)
        self.addCleanup(self._tmp.cleanup)

    def write_export(self, data, name="conversations.json"):
        path = self.dir / name
        path.write_text(json.dumps(data, ensure_ascii=False), encoding="utf-8")
        return path

    def run_quiet(self, fn, *args, **kwargs):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            result = fn(*args, **kwargs)
        return result, out.getvalue()


class IsAllowedUrlTest(unittest.TestCase):
    def test_allows_https_on_the_allowed_hosts_and_their_subdomains(self):
        for url in ["https://claude.ai/export/abc/download/tok", "https://a.claude.ai/x", "https://claude.com/x",
                    "https://www.anthropic.com/x", "https://claude.ai:443/x", "https://CLAUDE.AI/x"]:
            self.assertTrue(mod.is_allowed_url(url), url)

    def test_refuses_everything_else(self):
        cases = {
            "http": "http://claude.ai/x",
            "suffix only": "https://evilclaude.ai/x",
            "lookalike prefix": "https://claude.ai.evil.com/x",
            "userinfo": "https://claude.ai@evil.com/x",
            "userinfo reversed": "https://evil.com@claude.ai/x",
            "backslash trick": "https://evil.example\\@claude.ai/x",
            "backslash in path": "https://claude.ai\\evil/x",
            "other port": "https://claude.ai:8443/x",
            "no host": "https:///x",
            "file": "file:///etc/passwd",
            "javascript": "javascript:alert(1)",
            "space": "https://claude.ai/x y",
            "newline": "https://claude.ai/x\nhttps://evil.com",
            "non-ascii host": "https://claudeа.ai/x",
            "trailing dot": "https://claude.ai./x",
            "empty": "",
            "not a string": None,
        }
        for label, url in cases.items():
            self.assertFalse(mod.is_allowed_url(url), label)


class MailTest(Base):
    @staticmethod
    def make_mail(download_host="claude.ai"):
        """実際の Mail の原文と同じく、本文は MIME で正しくエンコードされた形(EmailMessage に組み立てさせる)"""
        body = (
            '<html><body><a href="https://example.invalid/help">ヘルプ</a>'
            f'<a href="https://{download_host}/export/abc/download/tok?a=1&amp;b=2"><span>データを</span>ダウンロード</a></body></html>'
        )
        msg = EmailMessage()
        msg["From"] = "Anthropic <no-reply@example.invalid>"
        msg["Subject"] = "test"
        msg.set_content("plain")
        msg.add_alternative(body, subtype="html")
        return msg.as_string()

    HTML_MAIL = None  # setUp で作る

    def setUp(self):
        super().setUp()
        self.HTML_MAIL = self.make_mail()

    def test_links_are_extracted_with_their_labels_and_unescaped(self):
        links = mod.parse_mail_links(self.HTML_MAIL)
        self.assertEqual(links[0], ("https://example.invalid/help", "ヘルプ"))
        self.assertEqual(links[1], ("https://claude.ai/export/abc/download/tok?a=1&b=2", "データをダウンロード"))
        self.assertEqual(mod.parse_mail_links("Subject: plain\n\nno html here"), [])

    def test_the_download_link_is_picked_by_its_label_and_checked(self):
        picked = mod.pick_download_link(mod.parse_mail_links(self.HTML_MAIL))
        self.assertEqual(picked, ("https://claude.ai/export/abc/download/tok?a=1&b=2", "claude.ai", True))
        bad = mod.pick_download_link([("https://evil.example\\@claude.ai/x", "ダウンロード")])
        self.assertFalse(bad[2])
        self.assertIsNone(mod.pick_download_link([("https://claude.ai/help", "ヘルプ")]))
        self.assertIsNone(mod.pick_download_link([]))

    def run_check(self, mails, open_result=True, seen=None):
        seen = set() if seen is None else seen
        with mock.patch.object(mod, "fetch_mail_candidates", return_value=iter(mails)), \
                mock.patch.object(mod, "open_in_safari", return_value=open_result) as opened, \
                contextlib.redirect_stderr(io.StringIO()), contextlib.redirect_stdout(io.StringIO()):
            result = mod.check_mail(seen)
        return result, seen, opened

    def test_a_mail_is_marked_seen_only_after_safari_opened_it(self):
        result, seen, opened = self.run_check([("id-ok", self.HTML_MAIL)])
        self.assertTrue(result)
        self.assertEqual(seen, {"id-ok"})
        opened.assert_called_once_with("https://claude.ai/export/abc/download/tok?a=1&b=2")

    def test_a_failed_safari_launch_leaves_the_mail_unseen_so_it_is_tried_again(self):
        result, seen, _ = self.run_check([("id-fail", self.HTML_MAIL)], open_result=False)
        self.assertFalse(result)
        self.assertEqual(seen, set())

    def test_a_link_to_a_disallowed_host_is_never_opened_and_is_marked_seen(self):
        evil = self.make_mail("claude.ai.evil.com")
        result, seen, opened = self.run_check([("id-evil", evil)])
        self.assertFalse(result)
        self.assertEqual(seen, {"id-evil"})
        opened.assert_not_called()

    def test_a_mail_without_a_download_link_is_skipped_and_dry_run_opens_nothing(self):
        result, seen, opened = self.run_check([("id-none", "Subject: x\n\nplain")])
        self.assertFalse(result)
        self.assertEqual(seen, set())
        opened.assert_not_called()
        with mock.patch.object(mod, "fetch_mail_candidates", return_value=iter([("id", self.HTML_MAIL)])), \
                mock.patch.object(mod, "open_in_safari") as opened, contextlib.redirect_stdout(io.StringIO()):
            self.assertFalse(mod.check_mail(set(), dry_run=True))
        opened.assert_not_called()

    def test_the_message_id_is_passed_as_an_argument_and_never_built_into_the_script(self):
        hostile = '<x"> & do shell script "touch /nonexistent-marker" & "@y>'
        calls = []

        def fake_run(args, **kwargs):
            calls.append(args)
            out = f"{hostile}{mod.RS}" if "repeat with m in" in args[2] else "SOURCE"
            return mock.Mock(returncode=0, stdout=out, stderr="")

        with mock.patch.object(mod.subprocess, "run", side_effect=fake_run):
            got = list(mod.fetch_mail_candidates(set()))
        self.assertEqual(got, [(hostile, "SOURCE")])
        for args in calls:
            self.assertNotIn(hostile, args[2], "スクリプトの文字列に、message id を埋め込んではいけない")
        self.assertEqual(calls[1][3], hostile)  # 引数として渡る

    def test_only_unseen_mails_have_their_source_fetched(self):
        calls = []

        def fake_run(args, **kwargs):
            calls.append(args)
            out = f"id-1{mod.RS}id-2{mod.RS}" if "repeat with m in" in args[2] else "SRC"
            return mock.Mock(returncode=0, stdout=out, stderr="")

        with mock.patch.object(mod.subprocess, "run", side_effect=fake_run):
            got = list(mod.fetch_mail_candidates({"id-1"}))
        self.assertEqual([mid for mid, _ in got], ["id-2"])
        self.assertEqual(len(calls), 2)  # 一覧 1 回 + 原文 1 回

    def test_a_mail_read_failure_is_an_error(self):
        with mock.patch.object(mod.subprocess, "run", return_value=mock.Mock(returncode=1, stdout="", stderr="boom")):
            with self.assertRaises(RuntimeError):
                list(mod.fetch_mail_candidates(set()))


class ProcessManifestTest(Base):
    def manifest(self, url="https://claude.ai/export/abc/download/tok", category="conversations"):
        path = self.dir / "manifest-1.json"
        path.write_text(json.dumps({"data_files": [{"category": category, "export_url": url, "filename": "conversations-000.zip"}]}))
        return path

    def run_manifest(self, path, safari=True, found=None, ingest_effect=None, wait_limit=60):
        clock = FakeClock()
        with mock.patch.object(mod, "time", clock), \
                mock.patch.object(mod, "open_in_safari", return_value=safari) as opened, \
                mock.patch.object(mod, "find_downloaded", return_value=found) as finder, \
                mock.patch.object(mod, "ingest", side_effect=ingest_effect) as ingested, \
                contextlib.redirect_stderr(io.StringIO()), contextlib.redirect_stdout(io.StringIO()):
            result = mod.process_manifest(path, wait_limit)
        return result, opened, finder, ingested, clock

    def test_a_manifest_without_conversations_is_settled(self):
        result, opened, *_ = self.run_manifest(self.manifest(category="projects"))
        self.assertTrue(result)
        opened.assert_not_called()

    def test_a_disallowed_url_is_settled_and_never_opened(self):
        for url in ["https://evil.com/x", "http://claude.ai/x", "https://evil.example\\@claude.ai/x", None]:
            result, opened, *_ = self.run_manifest(self.manifest(url=url))
            self.assertTrue(result, url)
            opened.assert_not_called()

    def test_a_safari_launch_failure_is_retried_later(self):
        result, opened, finder, *_ = self.run_manifest(self.manifest(), safari=False)
        self.assertFalse(result)
        opened.assert_called_once()
        finder.assert_not_called()

    def test_a_timeout_is_retried_later_and_spotlight_is_used_only_now_and_then(self):
        result, _, finder, ingested, clock = self.run_manifest(self.manifest(), found=None, wait_limit=60)
        self.assertFalse(result)
        ingested.assert_not_called()
        self.assertEqual(len(finder.call_args_list), 12)  # 60 秒 / 5 秒
        spotlight = [c.kwargs["spotlight"] for c in finder.call_args_list]
        self.assertEqual(spotlight.count(True), 2)  # 1 回目と 7 回目だけ

    def test_a_download_is_ingested_and_settled(self):
        found = self.dir / "conversations-000.zip"
        result, _, _, ingested, _ = self.run_manifest(self.manifest(), found=found)
        self.assertTrue(result)
        ingested.assert_called_once_with(found)

    def test_an_unreadable_download_keeps_waiting_and_then_times_out(self):
        found = self.dir / "conversations-000.zip"
        result, _, _, ingested, _ = self.run_manifest(self.manifest(), found=found, ingest_effect=zipfile.BadZipFile("x"), wait_limit=20)
        self.assertFalse(result)
        self.assertGreaterEqual(ingested.call_count, 2)

    def test_a_broken_manifest_raises_so_that_the_caller_can_retry(self):
        path = self.dir / "manifest-bad.json"
        path.write_text("{ not json")
        with self.assertRaises(ValueError):
            mod.process_manifest(path, 10)


class RetryTest(unittest.TestCase):
    def test_retries_are_spaced_and_limited(self):
        attempts = {}
        self.assertTrue(mod.may_retry(attempts, "m", 100.0))
        attempts["m"] = (1, 100.0)
        self.assertFalse(mod.may_retry(attempts, "m", 100.0 + mod.RETRY_INTERVAL - 1))
        self.assertTrue(mod.may_retry(attempts, "m", 100.0 + mod.RETRY_INTERVAL))
        attempts["m"] = (mod.MAX_ATTEMPTS, 100.0)
        self.assertFalse(mod.may_retry(attempts, "m", 10**9))


class DownloadSourceTest(Base):
    def test_only_the_declared_names_from_an_allowed_source_are_taken(self):
        watch = self.dir / "Downloads"
        watch.mkdir()
        good = watch / "conversations-000.zip"
        good.write_bytes(b"x")
        other = watch / "conversations-999.zip"
        other.write_bytes(b"x")
        clock = FakeClock()
        with mock.patch.object(mod, "WATCH_DIRS", [watch]), mock.patch.object(mod, "time", clock), \
                mock.patch.object(mod, "find_files", return_value=[other, good]), \
                mock.patch.object(mod, "where_from_urls", return_value=["https://claude.ai/export/abc/download/tok"]):
            self.assertEqual(mod.find_downloaded(0, "conversations-000.zip"), good)

    def test_a_download_from_another_host_is_refused(self):
        watch = self.dir / "Downloads"
        watch.mkdir()
        bad = watch / "conversations-000.zip"
        bad.write_bytes(b"x")
        clock = FakeClock()
        with mock.patch.object(mod, "WATCH_DIRS", [watch]), mock.patch.object(mod, "time", clock), \
                mock.patch.object(mod, "find_files", return_value=[bad]), \
                mock.patch.object(mod, "where_from_urls", return_value=["https://evil.example/conversations-000.zip"]), \
                contextlib.redirect_stderr(io.StringIO()):
            self.assertIsNone(mod.find_downloaded(0, "conversations-000.zip"))

    def test_without_a_record_only_the_watched_folder_itself_is_trusted(self):
        watch = self.dir / "Downloads"
        (watch / "sub").mkdir(parents=True)
        inside = watch / "conversations.json"
        inside.write_text("[]")
        deeper = watch / "sub" / "conversations.json"
        deeper.write_text("[]")
        with mock.patch.object(mod, "WATCH_DIRS", [watch]), mock.patch.object(mod, "where_from_urls", return_value=[]):
            self.assertTrue(mod.came_from_allowed_source(inside))
            self.assertFalse(mod.came_from_allowed_source(deeper))

    def test_where_from_urls_reads_the_mdls_output(self):
        out = '(\n    "https://claude.ai/export/abc/download/tok",\n    "https://claude.ai/"\n)\n'
        with mock.patch.object(mod.subprocess, "run", return_value=mock.Mock(stdout=out)):
            self.assertEqual(mod.where_from_urls(Path("x")), ["https://claude.ai/export/abc/download/tok", "https://claude.ai/"])
        with mock.patch.object(mod.subprocess, "run", return_value=mock.Mock(stdout="(null)")):
            self.assertEqual(mod.where_from_urls(Path("x")), [])


class IngestAndSearchTest(Base):
    def count(self, sql):
        db = mod.connect()
        try:
            return db.execute(sql).fetchone()[0]
        finally:
            db.close()

    def test_importing_twice_adds_nothing_and_the_index_has_no_duplicates(self):
        path = self.write_export(conversations())
        self.assertEqual(self.run_quiet(mod.ingest, path)[0], (2, 0))  # 空白だけの発言は入らない
        self.assertEqual(self.run_quiet(mod.ingest, path)[0], (0, 0))
        self.assertEqual(self.count("SELECT count(*) FROM messages"), 2)
        self.assertEqual(self.count("SELECT count(*) FROM messages_fts WHERE messages_fts MATCH '\"認証を\"'"), 1)

    def test_an_edited_message_is_updated_in_the_table_and_in_the_index(self):
        self.run_quiet(mod.ingest, self.write_export(conversations()))
        edited = self.write_export(conversations(extra_text="(追記: 二要素も)"))
        self.assertEqual(self.run_quiet(mod.ingest, edited)[0], (0, 1))
        self.assertEqual(self.count("SELECT count(*) FROM messages_fts WHERE messages_fts MATCH '\"二要素も\"'"), 1)
        # 新しい本文は古い本文を含むので、古い行が索引に残っていれば 2 件に当たる(残っていなければ 1 件)
        self.assertEqual(self.count("SELECT count(*) FROM messages_fts WHERE messages_fts MATCH '\"どうする?\"'"), 1)
        self.assertEqual(self.run_quiet(mod.ingest, edited)[0], (0, 0))

    def test_message_text_uses_text_blocks_and_falls_back_to_text(self):
        self.assertEqual(mod.message_text({"content": [{"type": "thinking", "thinking": "x"}, {"type": "text", "text": "a"}, {"type": "text", "text": "b"}]}), "a\nb")
        self.assertEqual(mod.message_text({"text": " fallback ", "content": [{"type": "tool_use"}]}), "fallback")
        self.assertEqual(mod.message_text({"content": ["not a dict", None]}), "")
        self.assertEqual(mod.message_text({}), "")

    def test_a_two_character_term_uses_the_like_path_and_a_long_one_uses_the_index(self):
        self.run_quiet(mod.ingest, self.write_export(conversations()))
        _, short = self.run_quiet(mod.search, ["認証"], 20)
        self.assertIn("-- 1件", short)
        self.assertIn("認証の方式", short)
        _, long = self.run_quiet(mod.search, ["セッション方式"], 20)
        self.assertIn("-- 1件", long)
        _, none = self.run_quiet(mod.search, ["存在しない言葉"], 20)
        self.assertIn("-- 0件", none)

    def test_a_message_without_time_or_sender_does_not_break_the_search(self):
        data = [{"uuid": "c9", "name": None, "chat_messages": [{"uuid": "m9", "text": "欠けた行です"}]}]
        self.run_quiet(mod.ingest, self.write_export(data))
        _, out = self.run_quiet(mod.search, ["欠けた"], 20)
        self.assertIn("-- 1件", out)
        self.assertIn("(無題)", out)
        _, out = self.run_quiet(mod.search, ["欠"], 20)
        self.assertIn("-- 1件", out)

    def test_messages_without_a_uuid_are_skipped_and_a_conversation_without_messages_is_fine(self):
        data = [{"uuid": "c8", "chat_messages": [{"sender": "human", "text": "uuid なし"}]}, {"uuid": "c7"}]
        self.assertEqual(self.run_quiet(mod.ingest, self.write_export(data))[0], (0, 0))
        self.assertEqual(self.count("SELECT count(*) FROM conversations"), 2)


class LoadJsonTest(Base):
    def make_zip(self, name, members):
        path = self.dir / name
        with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
            for member, body in members.items():
                z.writestr(member, body)
        return path

    def test_a_zip_a_folder_and_a_file_all_load(self):
        body = json.dumps(conversations(), ensure_ascii=False)
        z = self.make_zip("conversations-000.zip", {"deep/conversations.json": body, "users.json": "not json"})
        self.assertEqual(mod.load_json(z)[0]["uuid"], "c1")
        (self.dir / "folder" / "a").mkdir(parents=True)
        (self.dir / "folder" / "a" / "conversations.json").write_text(body, encoding="utf-8")
        self.assertEqual(mod.load_json(self.dir / "folder")[0]["uuid"], "c1")
        self.assertEqual(mod.load_json(self.write_export(conversations()))[0]["uuid"], "c1")

    def test_a_missing_conversations_json_is_a_clear_error_not_a_bare_stopiteration(self):
        empty = self.dir / "empty"
        empty.mkdir()
        with self.assertRaises(FileNotFoundError):
            mod.load_json(empty)
        with self.assertRaises(FileNotFoundError):
            mod.load_json(self.make_zip("other.zip", {"users.json": "[]"}))

    def test_the_size_limit_stops_a_big_zip_member_and_a_big_file(self):
        z = self.make_zip("big.zip", {"conversations.json": json.dumps([{"uuid": "x" * 500}])})
        with mock.patch.object(mod, "MAX_JSON_BYTES", 100):
            with self.assertRaises(ValueError):
                mod.load_json(z)
            with self.assertRaises(ValueError):
                mod.load_json(self.write_export([{"uuid": "x" * 500}]))


if __name__ == "__main__":
    unittest.main()
