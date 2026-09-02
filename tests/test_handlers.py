"""Tests for the 13 new tool handlers (capabilities worktree, WT-B).

Every handler is exercised through the same contract ``server._dispatch``
will use: ``await handler(client, args) -> (blocks, payload)``. A lightweight
``FakeClient`` stands in for ``NTULearnClient`` so no network or cookie
resolution ever happens (runs stay fast and keychain-prompt-free).
"""

from __future__ import annotations

import asyncio
import tempfile
import unittest
from pathlib import Path

from mcp.types import TextContent

from ntulearn_mcp import common, handlers


class FakeClient:
    """Deterministic stand-in for NTULearnClient implementing the methods the
    handlers call. Method calls are recorded on ``self.calls``."""

    def __init__(self) -> None:
        self.calls: list[str] = []
        self.course = {
            "id": "_150_1",
            "name": "Algorithms",
            "displayName": "CS2040S - Algorithms",
            "termId": "T12345",
            "description": "<p>Design &amp; analysis of algorithms</p>",
        }
        self.term = {"id": "T12345", "name": "AY25/26 Sem 2", "startDate": "2026-01-12", "endDate": "2026-05-08"}
        self.enrollments = [
            {"courseId": "_150_1", "availability": {"available": "Yes"}, "courseName": "Algorithms"},
            {"courseId": "_151_1", "availability": {"available": "Yes"}, "courseName": "Databases"},
            {"courseId": "_152_1", "availability": {"available": "No"}, "courseName": "Old Course"},
        ]
        self.users = [
            {"id": "_1_1", "userName": "alice", "name": {"given": "Alice", "family": "Aye"}, "courseRoleId": "Instructor"},
            {"id": "_2_1", "userName": "bob", "name": {"given": "Bob", "family": "Bee"}, "courseRoleId": "Student"},
            {"id": "_3_1", "userName": "carol", "name": {"given": "Carol", "family": "Cee"}, "courseRoleId": "Student"},
        ]
        self.groups = [
            {"id": "_g1_1", "name": "Team Alpha", "description": "<b>cool</b> group", "availability": {"available": "Yes"}},
            {"id": "_g2_1", "name": "Team Beta", "description": "", "availability": {"available": "No"}},
        ]
        self.messages = [
            {"id": "_m1_1", "subject": "Midterm venue", "fromUserId": "_1_1", "created": "2026-09-01T09:00:00Z", "read": False, "folder": "Inbox"},
            {"id": "_m2_1", "subject": "Project group", "fromUserId": "_1_1", "created": "2026-08-30T09:00:00Z", "read": True, "folder": "Inbox"},
            {"id": "_m3_1", "subject": "Sent note", "fromUserId": "_2_1", "created": "2026-09-01T10:00:00Z", "read": True, "folder": "Sent"},
        ]
        self.message_body = {
            "_m1_1": {"subject": "Midterm venue", "fromUserId": "_1_1", "created": "2026-09-01T09:00:00Z", "read": False, "folder": "Inbox", "body": {"text": "Room LT27 at 4pm.<br/>Bring ID."}},
            "_m2_1": {"subject": "Project group", "fromUserId": "_1_1", "created": "2026-08-30T09:00:00Z", "read": True, "folder": "Inbox", "body": "Plain body"},
        }
        self.participants = {
            "_m1_1": [
                {"id": "_1_1", "name": {"given": "Alice", "family": "Aye"}, "courseRoleId": "Instructor"},
                {"id": "_4_1", "name": {"given": "Dan", "family": "Dee"}, "courseRoleId": "Student"},
            ],
            "_m2_1": [],
        }
        self.contents = [
            {"id": "_c1_1", "title": "Week 1", "hasChildren": True, "contentHandler": {"id": "resource/x-bb-folder"}},
            {"id": "_c2_1", "title": "Syllabus.pdf", "hasChildren": False, "contentHandler": {"id": "resource/x-bb-document"}, "modified": "2026-09-02T08:00:00Z"},
            {"id": "_c3_1", "title": "Notes", "hasChildren": True, "contentHandler": {"id": "resource/x-bb-folder"}, "modified": "2026-08-20T08:00:00Z"},
        ]
        self.children = {
            "_c1_1": [
                {"id": "_c4_1", "title": "Lecture slides.pdf", "hasChildren": False, "contentHandler": {"id": "resource/x-bb-document"}, "modified": "2026-09-03T08:00:00Z"},
                {"id": "_c5_1", "title": "Readings", "hasChildren": True, "contentHandler": {"id": "resource/x-bb-folder"}},
            ],
            "_c3_1": [
                {"id": "_c6_1", "title": "Tutorial 1", "hasChildren": False, "contentHandler": {"id": "resource/x-bb-document"}},
            ],
            "_c5_1": [],
        }
        self.attachments = {
            "_c2_1": [
                {"id": "_a1_1", "fileName": "Syllabus.pdf"},
                {"id": "_a2_1", "fileName": "Syllabus.pdf"},  # dup name for dedup
            ],
            "_c4_1": [{"id": "_a3_1", "fileName": "slides.pdf"}],
        }
        self.announcements = [
            {"id": "_an1_1", "title": "Welcome", "created": "2026-08-28T09:00:00Z", "body": "<p>Hello</p>"},
            {"id": "_an2_1", "title": "Deadline moved", "created": "2026-09-03T09:00:00Z", "body": "Now Friday"},
        ]
        self.calendar = [
            {"id": "_cal1_1", "title": "Midterm", "type": "GradebookColumn", "start": "2026-09-10T10:00:00Z", "end": "2026-09-10T12:00:00Z", "description": "<b>Closed book</b>"},
            {"id": "_cal2_1", "title": "Tutorial", "type": "OfficeHours", "start": "2026-09-01T10:00:00Z", "end": "2026-09-01T11:00:00Z", "description": ""},
        ]
        self.columns = [
            {"id": "_col1_1", "name": "Problem Set 1", "score": {"possible": 20}},
            {"id": "_col2_1", "name": "Midterm", "score": {"possible": 100}},
        ]
        self.user_grades = [
            {"columnId": "_col1_1", "score": {"score": 18}, "status": "OK"},
            {"columnId": "_col2_1", "score": {"score": 82}, "status": "OK"},
        ]
        self.attempts = [
            {"id": "_att1_1", "userId": "_2_1", "status": "InProgress", "score": {"score": 15}, "cumulatedScore": {"score": 30}, "feedback": "<p>good<\/p>", "created": "2026-09-01T08:00:00Z"},
        ]

    # --- client methods the handlers use -----------------------------------
    async def get_messages(self, folder="inbox", unread_only=False, since=None):
        self.calls.append("get_messages")
        msgs = [m for m in self.messages if m["folder"].lower() == folder.lower()]
        if unread_only:
            msgs = [m for m in msgs if not m["read"]]
        if since:
            msgs = [m for m in msgs if self._after(m["created"], since)]
        return msgs

    @staticmethod
    def _after(value, threshold):
        return common.parse_iso(value) >= common.parse_iso(threshold)

    async def get_message(self, message_id):
        self.calls.append("get_message")
        return self.message_body.get(message_id)

    async def get_message_participants(self, message_id):
        self.calls.append("get_message_participants")
        return self.participants.get(message_id, [])

    async def get_course(self, course_id):
        self.calls.append("get_course")
        return self.course if course_id == "_150_1" else {}

    async def get_course_users(self, course_id):
        self.calls.append("get_course_users")
        return self.users

    async def get_course_groups(self, course_id):
        self.calls.append("get_course_groups")
        return self.groups

    async def get_group_users(self, course_id, group_id):
        self.calls.append("get_group_users")
        if group_id == "_g1_1":
            return self.users
        return []

    async def get_gradebook_attempts(self, course_id, column_id):
        self.calls.append("get_gradebook_attempts")
        return self.attempts

    async def get_user_attempts(self, course_id, column_id, user_id):
        self.calls.append("get_user_attempts")
        return [a for a in self.attempts if a["userId"] == user_id]

    async def get_user_grades(self, course_id, user_id):
        self.calls.append("get_user_grades")
        return self.user_grades

    async def get_gradebook_columns(self, course_id):
        self.calls.append("get_gradebook_columns")
        return self.columns

    async def get_course_contents(self, course_id):
        self.calls.append("get_course_contents")
        return self.contents

    async def get_content_children(self, course_id, content_id):
        self.calls.append("get_content_children")
        return self.children.get(content_id, [])

    async def get_attachments(self, course_id, content_id):
        self.calls.append("get_attachments")
        return self.attachments.get(content_id, [])

    async def get_attachment_download_url(self, course_id, content_id, attachment_id):
        self.calls.append("get_attachment_download_url")
        return f"https://bb.example/{course_id}{content_id}{attachment_id}/download"

    async def download_bytes(self, url):
        self.calls.append("download_bytes")
        return b"0" * 16, {"content-type": "application/pdf"}

    async def get_my_enrollments(self):
        self.calls.append("get_my_enrollments")
        return self.enrollments

    async def get_my_user_id(self):
        self.calls.append("get_my_user_id")
        return "_2_1"

    async def get_announcements(self, course_id):
        self.calls.append("get_announcements")
        return self.announcements

    async def get_calendar_items(self, course_id=None, since=None, until=None):
        self.calls.append("get_calendar_items")
        out = list(self.calendar)
        if since:
            out = [i for i in out if self._after(i["start"], since)]
        if until:
            out = [i for i in out if self._after(until, i["start"])]
        return out

    async def get_term(self, term_id):
        self.calls.append("get_term")
        return self.term

    async def get_course_search(self, course_id, query):
        self.calls.append("get_course_search")
        return []

    async def close(self):
        self.calls.append("close")


def run(handler, client, args):
    return asyncio.run(handler(client, args))


class HandlerTestBase(unittest.TestCase):
    def setUp(self):
        self.client = FakeClient()

    def assertJson(self, result):
        blocks, payload = result
        self.assertIsInstance(blocks, list)
        self.assertTrue(blocks)
        self.assertIsInstance(blocks[0], TextContent)
        self.assertIsInstance(payload, dict)
        return blocks, payload


class ListMessagesTests(HandlerTestBase):
    def test_default_inbox(self):
        _, payload = self.assertJson(run(handlers.handle_list_messages, self.client, {}))
        self.assertEqual(payload["folder"], "inbox")
        self.assertEqual(payload["total"], 2)
        self.assertEqual(payload["messages"][0]["id"], "_m1_1")

    def test_unread_only(self):
        _, payload = self.assertJson(
            run(handlers.handle_list_messages, self.client, {"unread_only": True})
        )
        self.assertEqual(payload["total"], 1)
        self.assertEqual(payload["messages"][0]["subject"], "Midterm venue")

    def test_sent_folder(self):
        _, payload = self.assertJson(
            run(handlers.handle_list_messages, self.client, {"folder": "sent"})
        )
        self.assertEqual(payload["total"], 1)
        self.assertEqual(payload["messages"][0]["id"], "_m3_1")

    def test_pagination(self):
        _, payload = self.assertJson(
            run(handlers.handle_list_messages, self.client, {"limit": 1, "offset": 0})
        )
        self.assertEqual(payload["count"], 1)
        self.assertTrue(payload["hasMore"])

    def test_markdown(self):
        blocks, _ = run(handlers.handle_list_messages, self.client, {"response_format": "markdown"})
        self.assertIn("Midterm venue", blocks[0].text)

    def test_bad_since(self):
        with self.assertRaises(ValueError):
            asyncio.run(
                handlers.handle_list_messages(self.client, {"since": "not-a-date"})
            )


class ReadMessageTests(HandlerTestBase):
    def test_html_body_stripped(self):
        _, payload = self.assertJson(
            run(handlers.handle_read_message, self.client, {"message_id": "_m1_1"})
        )
        self.assertEqual(payload["subject"], "Midterm venue")
        self.assertIn("Room LT27 at 4pm.", payload["body"])
        self.assertNotIn("<br/>", payload["body"])
        self.assertEqual(payload["senderName"], "Alice Aye")
        self.assertEqual(len(payload["recipients"]), 2)

    def test_missing_body_message(self):
        _, payload = self.assertJson(
            run(handlers.handle_read_message, self.client, {"message_id": "_m2_1"})
        )
        self.assertEqual(payload["senderName"], "")


class RosterTests(HandlerTestBase):
    def test_course_users(self):
        _, payload = self.assertJson(
            run(handlers.handle_list_course_users, self.client, {"course_id": "_150_1"})
        )
        self.assertEqual(payload["total"], 3)
        alice = next(u for u in payload["users"] if u["id"] == "_1_1")
        self.assertEqual(alice["name"], "Alice Aye")
        self.assertEqual(alice["role"], "Instructor")

    def test_course_users_limit(self):
        _, payload = self.assertJson(
            run(handlers.handle_list_course_users, self.client, {"course_id": "_150_1", "limit": 2})
        )
        self.assertEqual(payload["count"], 2)

    def test_course_users_bad_id(self):
        with self.assertRaises(ValueError):
            asyncio.run(handlers.handle_list_course_users(self.client, {"course_id": "bad id!"}))

    def test_groups_with_member_count(self):
        _, payload = self.assertJson(
            run(handlers.handle_list_course_groups, self.client, {"course_id": "_150_1", "include_members": True})
        )
        self.assertEqual(payload["total"], 2)
        alpha = next(g for g in payload["groups"] if g["id"] == "_g1_1")
        self.assertEqual(alpha["memberCount"], 3)
        self.assertTrue(alpha["available"])

    def test_group_members(self):
        _, payload = self.assertJson(
            run(handlers.handle_get_group_members, self.client, {"course_id": "_150_1", "group_id": "_g1_1"})
        )
        self.assertEqual(payload["total"], 3)
        self.assertEqual(payload["users"][0]["userName"], "alice")


class GradebookAttemptsTests(HandlerTestBase):
    def test_all_attempts(self):
        _, payload = self.assertJson(
            run(handlers.handle_get_gradebook_attempts, self.client, {"course_id": "_150_1", "column_id": "_col1_1"})
        )
        self.assertEqual(payload["total"], 1)
        self.assertEqual(payload["attempts"][0]["score"], 15.0)
        self.assertNotIn("<p>", payload["attempts"][0]["feedback"])

    def test_user_scoped(self):
        _, payload = self.assertJson(
            run(handlers.handle_get_gradebook_attempts, self.client, {"course_id": "_150_1", "column_id": "_col1_1", "user_id": "_1_9"})
        )
        self.assertEqual(payload["total"], 0)
        self.assertIn("get_user_attempts", self.client.calls)


class SearchTests(HandlerTestBase):
    def test_search_finds_walked_match(self):
        _, payload = self.assertJson(
            run(handlers.handle_search_all_courses, self.client, {"query": "slides"})
        )
        self.assertEqual(payload["coursesSearched"], 2)  # disabled course excluded
        self.assertTrue(any("slides" in m["title"].lower() for m in payload["matches"]))
        m = next(x for x in payload["matches"] if x["title"] == "Lecture slides.pdf")
        self.assertEqual(m["breadcrumb"], ["Week 1", "Lecture slides.pdf"])

    def test_search_helper_fallback(self):
        matches = asyncio.run(handlers.search_course(self.client, "_150_1", "tutorial", 3))
        self.assertTrue(any("Tutorial 1" == m["title"] for m in matches))

    def test_search_requires_query(self):
        with self.assertRaises(ValueError):
            asyncio.run(handlers.handle_search_all_courses(self.client, {}))

    def test_max_results_cap(self):
        _, payload = self.assertJson(
            run(handlers.handle_search_all_courses, self.client, {"query": "1", "max_depth": 4})
        )
        self.assertLessEqual(payload["maxResults"], 200)


class ContentTreeTests(HandlerTestBase):
    def test_nested_tree(self):
        _, payload = self.assertJson(
            run(handlers.handle_get_content_tree, self.client, {"course_id": "_150_1"})
        )
        self.assertEqual(payload["totalNodes"], 6)
        week1 = next(n for n in payload["tree"] if n["id"] == "_c1_1")
        self.assertEqual(week1["kind"], "folder")
        self.assertEqual(len(week1["children"]), 2)
        self.assertEqual(week1["children"][0]["title"], "Lecture slides.pdf")

    def test_markdown_tree(self):
        blocks, _ = run(handlers.handle_get_content_tree, self.client, {"course_id": "_150_1", "response_format": "markdown"})
        self.assertIn("Week 1", blocks[0].text)
        self.assertIn("Lecture slides.pdf", blocks[0].text)


class DownloadCourseTests(HandlerTestBase):
    def test_downloads_files(self):
        with tempfile.TemporaryDirectory() as tmp:
            blocks, payload = self.assertJson(
                run(handlers.handle_download_course, self.client, {"course_id": "_150_1", "destination_dir": tmp})
            )
            self.assertEqual(payload["downloadCount"], 3)  # 2 dup + 1 slide
            self.assertEqual(payload["skippedCount"], 0)
            self.assertEqual(payload["totalBytes"], 48)
            names = [f["filename"] for f in payload["files"]]
            self.assertEqual(names.count("Syllabus.pdf"), 1)
            self.assertEqual(names.count("Syllabus (2).pdf"), 1)
            dst = Path(tmp)
            self.assertTrue((dst / next(x["localPath"] for x in payload["files"])).exists())

    def test_skip_existing(self):
        with tempfile.TemporaryDirectory() as tmp:
            asyncio.run(handlers.handle_download_course(self.client, {"course_id": "_150_1", "destination_dir": tmp}))
            _, payload = self.assertJson(
                run(handlers.handle_download_course, self.client, {"course_id": "_150_1", "destination_dir": tmp})
            )
            self.assertEqual(payload["downloadCount"], 0)
            self.assertEqual(payload["skippedCount"], 3)  # 3 unique files skipped

    def test_extension_filter(self):
        with tempfile.TemporaryDirectory() as tmp:
            _, payload = self.assertJson(
                run(handlers.handle_download_course, self.client, {"course_id": "_150_1", "destination_dir": tmp, "include_extensions": "pdf"})
            )
            self.assertEqual(payload["downloadCount"], 3)

    def test_extension_filter_excludes(self):
        with tempfile.TemporaryDirectory() as tmp:
            _, payload = self.assertJson(
                run(handlers.handle_download_course, self.client, {"course_id": "_150_1", "destination_dir": tmp, "include_extensions": "docx"})
            )
            self.assertEqual(payload["downloadCount"], 0)
            self.assertEqual(payload["skippedCount"], 3)  # 3 unique files skipped

    def test_relative_path_rejected(self):
        with self.assertRaises(ValueError):
            asyncio.run(handlers.handle_download_course(self.client, {"course_id": "_150_1", "destination_dir": "relative/path"}))


class WhatsNewTests(HandlerTestBase):
    def test_since_filter(self):
        common.tracker_set_last_seen("2026-09-02T00:00:00Z")
        _, payload = self.assertJson(run(handlers.handle_whats_new, self.client, {}))
        self.assertEqual(payload["summary"]["announcements"], 2)  # Deadline moved in each of 2 courses
        self.assertEqual(payload["summary"]["newFiles"], 2)  # 1 root-level file x 2 courses
        self.assertEqual(payload["courseCount"], 2)

    def test_explicit_since(self):
        _, payload = self.assertJson(
            run(handlers.handle_whats_new, self.client, {"since": "2026-08-30T00:00:00Z", "update_tracker": True})
        )
        self.assertTrue(payload["summary"]["announcements"] >= 1)

    def test_update_tracker(self):
        before = common.tracker_get_last_seen()
        asyncio.run(handlers.handle_whats_new(self.client, {"update_tracker": True}))
        self.assertNotEqual(common.tracker_get_last_seen(), before)


class ExportTests(HandlerTestBase):
    def test_ics(self):
        _, payload = self.assertJson(run(handlers.handle_export_calendar_ics, self.client, {}))
        self.assertEqual(payload["itemCount"], 2)
        self.assertIn("BEGIN:VCALENDAR", payload["ics"])
        self.assertIn("END:VCALENDAR", payload["ics"])
        self.assertEqual(payload["ics"].count("BEGIN:VEVENT"), 2)
        self.assertIn("Midterm", payload["ics"])
        self.assertIn("Closed book", payload["ics"])  # HTML strip + escape

    def test_ics_window(self):
        _, payload = self.assertJson(
            run(handlers.handle_export_calendar_ics, self.client, {"since": "2026-09-05T00:00:00Z"})
        )
        self.assertEqual(payload["itemCount"], 2)

    def test_csv(self):
        _, payload = self.assertJson(run(handlers.handle_export_gradebook_csv, self.client, {"course_ids": ["_150_1"]}))
        self.assertEqual(payload["rowCount"], 2)
        self.assertIn("Problem Set 1", payload["csv"])
        self.assertTrue(payload["csv"].startswith("courseId,columnId,"))


class SummarizeTests(HandlerTestBase):
    def test_summary(self):
        _, payload = self.assertJson(
            run(handlers.handle_summarize_course, self.client, {"course_id": "_150_1"})
        )
        self.assertEqual(payload["title"], "Algorithms")
        self.assertIn("design", payload["description"].lower())
        self.assertEqual(payload["term"]["name"], "AY25/26 Sem 2")
        self.assertEqual(payload["instructors"][0]["name"], "Alice Aye")
        self.assertEqual(payload["enrollmentCount"], 3)
        self.assertEqual(payload["gradeSummary"]["columnCount"], 2)
        self.assertIsNotNone(payload["gradeSummary"]["averagePercent"])
        self.assertGreaterEqual(len(payload["contentTopFolders"]), 3)
        self.assertEqual(len(payload["recentAnnouncements"]), 2)
        self.assertGreaterEqual(len(payload["upcoming"]), 2)

    def test_summary_markdown(self):
        blocks, _ = run(handlers.handle_summarize_course, self.client, {"course_id": "_150_1", "response_format": "markdown"})
        self.assertIn("Algorithms", blocks[0].text)


class RegistryTests(unittest.TestCase):
    def test_all_handlers_register(self):
        self.assertEqual(len(handlers.REGISTRY), 13)
        for name in handlers.REGISTRY:
            self.assertTrue(hasattr(handlers, "handle_" + name))

    def test_via_handle_for_tool(self):
        self.assertIs(handlers.handle_for_tool("ntulearn_summarize_course"), handlers.handle_summarize_course)
        self.assertIsNone(handlers.handle_for_tool("ntulearn_download_file"))
        self.assertIsNone(handlers.handle_for_tool("summarize_course"))

    def test_download_course_is_the_only_writable_new_tool(self):
        # Mirrors test_fixes.py's expectation: exactly two write tools, and
        # only download_course comes from the new handlers registry.
        writable_new = {"ntulearn_download_course"}
        self.assertEqual(
            handlers.REGISTRY.keys() & {w.replace("ntulearn_", "") for w in writable_new},
            {"download_course"},
        )


if __name__ == "__main__":
    unittest.main()
