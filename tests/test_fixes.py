"""Tests for the mcp-builder review fixes.

Covers:
- tool name prefixes + annotations + output schemas
- input schema validation (course_id pattern, query minLength, max bounds)
- pagination (limit/offset/has_more/next_offset)
- response_format=markdown
- cookie CR/LF rejection
- BlackboardAPIError class-of-status messaging
"""

from __future__ import annotations

import json
import os
import sys
import unittest
from pathlib import Path
from typing import Any
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "src"))

from ntulearn_mcp import server
from ntulearn_mcp.client import BlackboardAPIError


# ---------------------------------------------------------------------------
# Tool surface
# ---------------------------------------------------------------------------

class ToolSurfaceTests(unittest.IsolatedAsyncioTestCase):
    """Exercise the tool list as an LLM client would receive it."""

    async def asyncSetUp(self) -> None:
        self._tools = await server.list_tools()
        self._by_name = {t.name: t for t in self._tools}

    def test_all_tools_carry_ntulearn_prefix(self) -> None:
        for t in self._tools:
            self.assertTrue(
                t.name.startswith("ntulearn_"),
                f"{t.name} is missing the service prefix",
            )

    def test_all_tools_have_annotations(self) -> None:
        for t in self._tools:
            self.assertIsNotNone(t.annotations, f"{t.name} has no annotations")
            ann = t.annotations
            for key in ("readOnlyHint", "destructiveHint", "idempotentHint", "openWorldHint"):
                self.assertTrue(
                    hasattr(ann, key),
                    f"{t.name} annotations missing {key}",
                )

    def test_only_download_file_is_non_read_only(self) -> None:
        non_read_only = [t.name for t in self._tools if t.annotations and not t.annotations.readOnlyHint]
        self.assertEqual(non_read_only, ["ntulearn_download_file"])

    def test_pagination_params_on_list_tools(self) -> None:
        for tname in (
            "ntulearn_list_courses",
            "ntulearn_get_course_contents",
            "ntulearn_get_folder_children",
            "ntulearn_get_announcements",
            "ntulearn_get_gradebook",
        ):
            schema = self._by_name[tname].inputSchema
            self.assertIn("limit", schema["properties"], tname)
            self.assertIn("offset", schema["properties"], tname)
            self.assertIn("response_format", schema["properties"], tname)

    def test_course_id_pattern_constraint_present(self) -> None:
        # Every tool that accepts course_id should pin a pattern + length.
        for t in self._tools:
            props = t.inputSchema.get("properties", {})
            if "course_id" in props:
                self.assertIn("pattern", props["course_id"], t.name)
                self.assertIn("minLength", props["course_id"], t.name)

    def test_search_query_has_min_length_and_max_bounds(self) -> None:
        schema = self._by_name["ntulearn_search_course_content"].inputSchema
        q = schema["properties"]["query"]
        md = schema["properties"]["max_depth"]
        mr = schema["properties"]["max_results"]
        self.assertGreaterEqual(q["minLength"], 1)
        self.assertIn("maximum", md)
        self.assertIn("maximum", mr)

    def test_list_courses_has_output_schema(self) -> None:
        schema = self._by_name["ntulearn_list_courses"].outputSchema
        self.assertIsNotNone(schema)
        self.assertIn("courses", schema["properties"])
        for k in ("total", "count", "offset", "limit", "hasMore"):
            self.assertIn(k, schema["properties"])


# ---------------------------------------------------------------------------
# Pagination
# ---------------------------------------------------------------------------

class PaginationTests(unittest.TestCase):
    def test_slice_first_page(self) -> None:
        items = list(range(100))
        page, meta = server._slice_with_pagination(items, 0, 25)
        self.assertEqual(page, list(range(25)))
        self.assertEqual(meta["total"], 100)
        self.assertEqual(meta["count"], 25)
        self.assertEqual(meta["offset"], 0)
        self.assertEqual(meta["limit"], 25)
        self.assertTrue(meta["hasMore"])
        self.assertEqual(meta["nextOffset"], 25)

    def test_slice_last_page(self) -> None:
        items = list(range(30))
        page, meta = server._slice_with_pagination(items, 25, 25)
        self.assertEqual(page, list(range(25, 30)))
        self.assertEqual(meta["count"], 5)
        self.assertFalse(meta["hasMore"])
        self.assertIsNone(meta["nextOffset"])

    def test_slice_empty(self) -> None:
        page, meta = server._slice_with_pagination([], 0, 25)
        self.assertEqual(page, [])
        self.assertEqual(meta["total"], 0)
        self.assertFalse(meta["hasMore"])

    def test_resolve_pagination_args_defaults(self) -> None:
        offset, limit = server._resolve_pagination_args({})
        self.assertEqual(offset, 0)
        self.assertEqual(limit, server._DEFAULT_LIMIT)

    def test_resolve_pagination_rejects_negative_offset(self) -> None:
        with self.assertRaises(ValueError):
            server._resolve_pagination_args({"offset": -1})

    def test_resolve_pagination_rejects_oversize_limit(self) -> None:
        with self.assertRaises(ValueError):
            server._resolve_pagination_args({"limit": server._MAX_LIMIT + 1})

    def test_resolve_pagination_rejects_zero_limit(self) -> None:
        with self.assertRaises(ValueError):
            server._resolve_pagination_args({"limit": 0})


class FakeListClient:
    """Returns 60 fake enrollments — enough to exercise pagination."""

    async def get_my_enrollments(self) -> list[dict[str, Any]]:
        return [
            {
                "courseId": f"_c{i}_1",
                "availability": {"available": "Yes"},
                "lastAccessed": f"2024-01-{(i % 28) + 1:02d}",
            }
            for i in range(60)
        ]

    async def get_courses_batch(self, course_ids: list[str]) -> list[dict[str, Any]]:
        return [{"id": cid, "name": f"Course {cid}"} for cid in course_ids]


class ListCoursesPaginationTests(unittest.IsolatedAsyncioTestCase):
    async def test_first_page(self) -> None:
        _, payload = await server._list_courses(FakeListClient(), {"limit": 20})
        self.assertEqual(payload["total"], 60)
        self.assertEqual(payload["count"], 20)
        self.assertEqual(len(payload["courses"]), 20)
        self.assertTrue(payload["hasMore"])
        self.assertEqual(payload["nextOffset"], 20)

    async def test_walking_pages(self) -> None:
        seen = set()
        offset = 0
        while True:
            _, payload = await server._list_courses(
                FakeListClient(), {"limit": 25, "offset": offset}
            )
            for c in payload["courses"]:
                seen.add(c["courseId"])
            if not payload["hasMore"]:
                break
            offset = payload["nextOffset"]
        self.assertEqual(len(seen), 60)


# ---------------------------------------------------------------------------
# Response format = markdown
# ---------------------------------------------------------------------------

class MarkdownFormatTests(unittest.IsolatedAsyncioTestCase):
    async def test_list_courses_markdown(self) -> None:
        content, structured = await server._list_courses(
            FakeListClient(), {"limit": 5, "response_format": "markdown"}
        )
        text = content[0].text
        self.assertIn("# Courses", text)
        self.assertIn("- **Course _c", text)
        # Pagination footer is human-readable, not JSON.
        self.assertIn("Showing 5 of 60", text)
        # Structured payload is still attached for clients that prefer it.
        self.assertEqual(structured["total"], 60)
        self.assertEqual(structured["count"], 5)

    async def test_invalid_response_format_raises(self) -> None:
        with self.assertRaises(ValueError):
            server._resolve_response_format({"response_format": "yaml"})


# ---------------------------------------------------------------------------
# Cookie validation
# ---------------------------------------------------------------------------

class CookieValidationTests(unittest.TestCase):
    def setUp(self) -> None:
        self._old = os.environ.get("NTULEARN_COOKIE")

    def tearDown(self) -> None:
        if self._old is None:
            os.environ.pop("NTULEARN_COOKIE", None)
        else:
            os.environ["NTULEARN_COOKIE"] = self._old

    def test_rejects_crlf_in_explicit_cookie(self) -> None:
        os.environ["NTULEARN_COOKIE"] = "expires:1234,id:abc\r\nSet-Cookie: evil=1"
        with self.assertRaises(RuntimeError) as ctx:
            server._resolve_cookie()
        self.assertIn("control characters", str(ctx.exception))

    def test_rejects_lf_in_explicit_cookie(self) -> None:
        os.environ["NTULEARN_COOKIE"] = "expires:1234,id:abc\nbad"
        with self.assertRaises(RuntimeError):
            server._resolve_cookie()

    def test_rejects_nul_via_validator(self) -> None:
        # os.environ on Windows refuses NUL bytes outright, so test the
        # validator directly. This still covers the case where a NUL
        # somehow reaches the validator from another source (e.g. browser
        # cookie auto-read).
        with self.assertRaises(RuntimeError) as ctx:
            server._validate_cookie_value("expires:1234,id:abc\x00")
        self.assertIn("control characters", str(ctx.exception))

    def test_rejects_crlf_from_browser_value(self) -> None:
        os.environ.pop("NTULEARN_COOKIE", None)
        with mock.patch.object(
            server, "read_bbrouter_cookie", return_value="expires:1\r\nbad"
        ):
            with self.assertRaises(RuntimeError):
                server._resolve_cookie()

    def test_accepts_safe_value(self) -> None:
        os.environ["NTULEARN_COOKIE"] = "expires:9999,id:abc123,signature:def"
        self.assertEqual(
            server._resolve_cookie(),
            "expires:9999,id:abc123,signature:def",
        )


# ---------------------------------------------------------------------------
# BlackboardAPIError messaging
# ---------------------------------------------------------------------------

class APIErrorMessageTests(unittest.TestCase):
    def test_403_explains_access(self) -> None:
        e = BlackboardAPIError(403, "no access", path="/x")
        self.assertIn("403 forbidden", str(e))
        self.assertIn("not enrolled", str(e))

    def test_404_explains_id(self) -> None:
        e = BlackboardAPIError(404, "missing", path="/x/y")
        self.assertIn("404 not found", str(e))
        self.assertIn("course_id", str(e))

    def test_429_says_rate_limit(self) -> None:
        self.assertIn("rate limited", str(BlackboardAPIError(429, "x")))

    def test_5xx_says_server_error(self) -> None:
        self.assertIn("server error", str(BlackboardAPIError(500, "boom")))
        self.assertIn("server error", str(BlackboardAPIError(503, "down")))

    def test_other_status_falls_through(self) -> None:
        msg = str(BlackboardAPIError(418, "teapot"))
        self.assertIn("418", msg)
        self.assertIn("teapot", msg)

    def test_path_included_when_provided(self) -> None:
        msg = str(BlackboardAPIError(404, "nope", path="/learn/api/x"))
        self.assertIn("/learn/api/x", msg)

    def test_attributes_preserved(self) -> None:
        e = BlackboardAPIError(404, "nope", path="/p")
        self.assertEqual(e.status_code, 404)
        self.assertEqual(e.body, "nope")
        self.assertEqual(e.path, "/p")


# ---------------------------------------------------------------------------
# Dispatch — unknown tool name
# ---------------------------------------------------------------------------

class DispatchTests(unittest.IsolatedAsyncioTestCase):
    async def test_unknown_tool_lists_known(self) -> None:
        os.environ["NTULEARN_COOKIE"] = "expires:1,test:test"
        old_client = server._client
        try:
            with self.assertRaises(ValueError) as ctx:
                await server.call_tool("not_a_real_tool", {})
            msg = str(ctx.exception)
            self.assertIn("Unknown tool", msg)
            self.assertIn("ntulearn_list_courses", msg)
        finally:
            server._client = old_client


# ---------------------------------------------------------------------------
# HTML stripping (Bug 1 fix)
# ---------------------------------------------------------------------------

class StripHtmlTests(unittest.TestCase):
    """The shared _strip_html helper, used by both the file-content text path
    and the announcements path. Centralises the previously-duplicated logic."""

    def test_strips_basic_tags(self) -> None:
        html = "<p>Hello <strong>world</strong>!</p>"
        # BeautifulSoup's separator="\n" inserts a newline at every tag boundary,
        # including inline tags. This matches the long-standing behaviour of the
        # HTML-file extraction path in _extract_content; staying consistent.
        self.assertEqual(server._strip_html(html), "Hello\nworld\n!")

    def test_collapses_blank_lines(self) -> None:
        html = "<p>One</p><p></p><p>Two</p>"
        self.assertEqual(server._strip_html(html), "One\nTwo")

    def test_unwraps_html_entities(self) -> None:
        html = "<p>Tom &amp; Jerry</p>"
        self.assertEqual(server._strip_html(html), "Tom & Jerry")

    def test_handles_nbsp(self) -> None:
        html = "<p>a&nbsp;b</p>"
        # BeautifulSoup decodes &nbsp; to U+00A0; we don't normalise that further,
        # but the tag is gone.
        result = server._strip_html(html)
        self.assertNotIn("<", result)
        self.assertNotIn("&nbsp", result)
        self.assertIn("a", result)
        self.assertIn("b", result)

    def test_passes_plain_text_through(self) -> None:
        self.assertEqual(server._strip_html("just words"), "just words")

    def test_none_returns_empty(self) -> None:
        self.assertEqual(server._strip_html(None), "")

    def test_empty_string_returns_empty(self) -> None:
        self.assertEqual(server._strip_html(""), "")

    def test_non_string_coerced(self) -> None:
        # Defensive: a callsite passing an int or similar shouldn't crash.
        self.assertEqual(server._strip_html(42), "42")


class FakeAnnouncementsClient:
    """Minimal client surface used by _get_announcements."""

    def __init__(self, announcements: list[dict[str, Any]]) -> None:
        self._announcements = announcements

    async def get_announcements(self, course_id: str) -> list[dict[str, Any]]:
        return self._announcements


class GetAnnouncementsHtmlStripTests(unittest.IsolatedAsyncioTestCase):
    """Bug 1 — announcement bodies must come back as plain text."""

    async def test_html_body_is_stripped(self) -> None:
        client = FakeAnnouncementsClient([
            {
                "id": "_a1_1",
                "title": "Welcome",
                "body": {"rawText": "<p>Dear students, the deadline is <strong>Friday</strong>.</p>"},
                "created": "2025-01-01T00:00:00Z",
                "modified": "2025-01-01T00:00:00Z",
                "availability": {"available": "Yes"},
            }
        ])
        _, payload = await server._get_announcements(client, {"course_id": "_c_1"})
        body = payload["announcements"][0]["body"]
        self.assertNotIn("<", body)
        self.assertNotIn(">", body)
        self.assertIn("Dear students", body)
        self.assertIn("Friday", body)

    async def test_table_body_is_stripped(self) -> None:
        # Many real announcements wrap deadlines/agendas in tables.
        client = FakeAnnouncementsClient([
            {
                "id": "_a2_1",
                "title": "Schedule",
                "body": {
                    "rawText": (
                        "<table><tr><td>Mon</td><td>Lab 1</td></tr>"
                        "<tr><td>Tue</td><td>Lab 2</td></tr></table>"
                    )
                },
                "created": "2025-01-02T00:00:00Z",
                "modified": "2025-01-02T00:00:00Z",
                "availability": {"available": "Yes"},
            }
        ])
        _, payload = await server._get_announcements(client, {"course_id": "_c_1"})
        body = payload["announcements"][0]["body"]
        self.assertNotIn("<table", body)
        self.assertNotIn("<tr", body)
        self.assertNotIn("<td", body)
        for token in ("Mon", "Lab 1", "Tue", "Lab 2"):
            self.assertIn(token, body)

    async def test_missing_body_does_not_crash(self) -> None:
        client = FakeAnnouncementsClient([
            {
                "id": "_a3_1",
                "title": "Empty",
                "body": None,
                "created": "2025-01-03T00:00:00Z",
                "modified": "2025-01-03T00:00:00Z",
                "availability": {"available": "Yes"},
            }
        ])
        _, payload = await server._get_announcements(client, {"course_id": "_c_1"})
        self.assertEqual(payload["announcements"][0]["body"], "")

    async def test_string_body_also_stripped(self) -> None:
        # Some Blackboard endpoints return a bare string body instead of a dict.
        client = FakeAnnouncementsClient([
            {
                "id": "_a4_1",
                "title": "Bare string",
                "body": "<p>Plain wrapped</p>",
                "created": "2025-01-04T00:00:00Z",
                "modified": "2025-01-04T00:00:00Z",
                "availability": {"available": "Yes"},
            }
        ])
        _, payload = await server._get_announcements(client, {"course_id": "_c_1"})
        self.assertEqual(payload["announcements"][0]["body"], "Plain wrapped")


if __name__ == "__main__":
    unittest.main()
