from __future__ import annotations

import base64
import importlib
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "src"))

from mcp.types import ImageContent, TextContent

from ntulearn_mcp import server
from ntulearn_mcp.client import BbRouterExpiredError, NTULearnClient

# Minimal hand-crafted PDF v1.4 with the visible text "Hello PDF Fixture".
# 598 bytes raw — kept as a base64 constant to avoid a fixture file and the
# chicken-and-egg of generating a fixture with the library under test.
_TINY_PDF_B64 = (
    "JVBERi0xLjQKJeLjz9MKMSAwIG9iago8PCAvVHlwZSAvQ2F0YWxvZyAvUGFnZXMgMiAwIFIgPj4K"
    "ZW5kb2JqCjIgMCBvYmoKPDwgL1R5cGUgL1BhZ2VzIC9LaWRzIFszIDAgUl0gL0NvdW50IDEgPj4K"
    "ZW5kb2JqCjMgMCBvYmoKPDwgL1R5cGUgL1BhZ2UgL1BhcmVudCAyIDAgUiAvTWVkaWFCb3ggWzAg"
    "MCA2MTIgNzkyXSAvQ29udGVudHMgNCAwIFIgL1Jlc291cmNlcyA8PCAvRm9udCA8PCAvRjEgNSAw"
    "IFIgPj4gPj4gPj4KZW5kb2JqCjQgMCBvYmoKPDwgL0xlbmd0aCA0OSA+PgpzdHJlYW0KQlQKL0Yx"
    "IDI0IFRmCjcyIDcyMCBUZAooSGVsbG8gUERGIEZpeHR1cmUpIFRqCkVUCmVuZHN0cmVhbQplbmRv"
    "YmoKNSAwIG9iago8PCAvVHlwZSAvRm9udCAvU3VidHlwZSAvVHlwZTEgL0Jhc2VGb250IC9IZWx2"
    "ZXRpY2EgPj4KZW5kb2JqCnhyZWYKMCA2CjAwMDAwMDAwMDAgNjU1MzUgZiAKMDAwMDAwMDAxNSAw"
    "MDAwMCBuIAowMDAwMDAwMDY0IDAwMDAwIG4gCjAwMDAwMDAxMjEgMDAwMDAgbiAKMDAwMDAwMDI0"
    "NyAwMDAwMCBuIAowMDAwMDAwMzQ1IDAwMDAwIG4gCnRyYWlsZXIKPDwgL1NpemUgNiAvUm9vdCAx"
    "IDAgUiA+PgpzdGFydHhyZWYKNDE1CiUlRU9GCg=="
)
_TINY_PDF_BYTES = base64.b64decode(_TINY_PDF_B64)


# --- Office-format fixture builders ----------------------------------------
# These generate fixtures at test time using the same library under test.
# Unlike PDF (where pypdf is the consumer), here the value is testing OUR
# extraction wrapper — round-tripping through the library is fine.

def _make_docx_bytes(
    paragraphs: list[str], table_rows: list[list[str]] | None = None
) -> bytes:
    from io import BytesIO

    from docx import Document

    doc = Document()
    for p in paragraphs:
        doc.add_paragraph(p)
    if table_rows:
        table = doc.add_table(rows=len(table_rows), cols=len(table_rows[0]))
        for i, row in enumerate(table_rows):
            for j, val in enumerate(row):
                table.rows[i].cells[j].text = val
    buf = BytesIO()
    doc.save(buf)
    return buf.getvalue()


def _make_pptx_bytes(slides: list[dict[str, Any]]) -> bytes:
    """Build a deck. Each slide dict supports keys: title, body, notes."""
    from io import BytesIO

    from pptx import Presentation
    from pptx.util import Inches

    prs = Presentation()
    blank = prs.slide_layouts[6]  # Blank layout — gives us full control
    for s in slides:
        slide = prs.slides.add_slide(blank)
        if "title" in s:
            tb = slide.shapes.add_textbox(Inches(1), Inches(1), Inches(5), Inches(1))
            tb.text_frame.text = s["title"]
        if "body" in s:
            tb = slide.shapes.add_textbox(Inches(1), Inches(2.5), Inches(5), Inches(2))
            tb.text_frame.text = s["body"]
        if s.get("notes"):
            slide.notes_slide.notes_text_frame.text = s["notes"]
    buf = BytesIO()
    prs.save(buf)
    return buf.getvalue()


def _make_xlsx_bytes(sheets: dict[str, list[list[Any]]]) -> bytes:
    from io import BytesIO

    from openpyxl import Workbook

    wb = Workbook()
    # Remove the auto-created default sheet so test sheet names are exact.
    wb.remove(wb.active)
    for name, rows in sheets.items():
        ws = wb.create_sheet(name)
        for row in rows:
            ws.append(row)
    buf = BytesIO()
    wb.save(buf)
    return buf.getvalue()


class _CookieEnvIsolation:
    """Mixin that snapshots NTULEARN_COOKIE + server._client and restores them.

    Also stubs out the keychain cache so tests never touch the user's real
    OS keychain. Tests that want to exercise specific cache behavior can
    re-patch the same symbols inside the test body.
    """

    def setUp(self) -> None:
        self._old_env = os.environ.get("NTULEARN_COOKIE")
        self._old_client = server._client

        # Default: cache is empty and inert. Start patches here so every
        # cookie-related test gets the same hermetic baseline.
        self._cache_read_patch = mock.patch.object(
            server, "read_cached_cookie", return_value=None
        )
        self._cache_write_patch = mock.patch.object(
            server, "write_cached_cookie", return_value=False
        )
        self._cache_delete_patch = mock.patch.object(
            server, "delete_cached_cookie", return_value=None
        )
        self._cache_read_patch.start()
        self._cache_write_patch.start()
        self._cache_delete_patch.start()

    def tearDown(self) -> None:
        self._cache_delete_patch.stop()
        self._cache_write_patch.stop()
        self._cache_read_patch.stop()
        if self._old_env is None:
            os.environ.pop("NTULEARN_COOKIE", None)
        else:
            os.environ["NTULEARN_COOKIE"] = self._old_env
        server._client = self._old_client


class MissingCookieTests(_CookieEnvIsolation, unittest.IsolatedAsyncioTestCase):
    async def test_missing_cookie_raises(self) -> None:
        # call_tool now raises on errors so the MCP framework can wrap them
        # into CallToolResult(isError=True). Verify the message is preserved.
        os.environ.pop("NTULEARN_COOKIE", None)
        server._client = None
        with mock.patch.object(server, "read_bbrouter_cookie", return_value=None):
            with self.assertRaises(RuntimeError) as ctx:
                await server.call_tool("ntulearn_list_courses", {})
        self.assertIn("No NTULearn cookie found", str(ctx.exception))


class DotenvPrecedenceTests(unittest.TestCase):
    def test_env_value_wins_over_dotenv(self) -> None:
        env_path = ROOT / ".env"
        old_env = os.environ.get("NTULEARN_COOKIE")
        old_env_file = env_path.read_text(encoding="utf-8") if env_path.exists() else None
        try:
            os.environ["NTULEARN_COOKIE"] = "fresh-from-env"
            env_path.write_text("NTULEARN_COOKIE=stale-from-dotenv\n", encoding="utf-8")
            reloaded = importlib.reload(server)
            self.assertEqual(reloaded._resolve_cookie(), "fresh-from-env")
        finally:
            if old_env is None:
                os.environ.pop("NTULEARN_COOKIE", None)
            else:
                os.environ["NTULEARN_COOKIE"] = old_env

            if old_env_file is None:
                env_path.unlink(missing_ok=True)
            else:
                env_path.write_text(old_env_file, encoding="utf-8")

            importlib.reload(server)


class CookieResolutionTests(_CookieEnvIsolation, unittest.TestCase):
    def test_env_var_takes_precedence_over_browser(self) -> None:
        os.environ["NTULEARN_COOKIE"] = "from-env"
        with mock.patch.object(server, "read_bbrouter_cookie", return_value="from-browser"):
            self.assertEqual(server._resolve_cookie(), "from-env")

    def test_falls_back_to_browser_when_env_unset(self) -> None:
        os.environ.pop("NTULEARN_COOKIE", None)
        with mock.patch.object(server, "read_bbrouter_cookie", return_value="from-browser"):
            self.assertEqual(server._resolve_cookie(), "from-browser")

    def test_falls_back_to_browser_when_env_blank(self) -> None:
        os.environ["NTULEARN_COOKIE"] = "   "
        with mock.patch.object(server, "read_bbrouter_cookie", return_value="from-browser"):
            self.assertEqual(server._resolve_cookie(), "from-browser")

    def test_raises_when_both_sources_empty(self) -> None:
        os.environ.pop("NTULEARN_COOKIE", None)
        with mock.patch.object(server, "read_bbrouter_cookie", return_value=None):
            with self.assertRaises(RuntimeError) as ctx:
                server._resolve_cookie()
        self.assertIn("No NTULearn cookie found", str(ctx.exception))

    # --- Cache integration -----------------------------------------------
    # The hot path: every successful browser read mirrors the value into the
    # OS keychain so the next resolution can ride through a transient
    # browser-cookie3 failure (SQLite race, keychain timeout, ABE flake).

    def test_successful_browser_read_writes_value_to_cache(self) -> None:
        os.environ.pop("NTULEARN_COOKIE", None)
        with mock.patch.object(server, "read_bbrouter_cookie", return_value="fresh-value"):
            with mock.patch.object(server, "write_cached_cookie") as write:
                server._resolve_cookie()
        write.assert_called_once_with("fresh-value")

    def test_falls_back_to_cache_when_browser_returns_none(self) -> None:
        os.environ.pop("NTULEARN_COOKIE", None)
        with mock.patch.object(server, "read_bbrouter_cookie", return_value=None):
            with mock.patch.object(
                server, "read_cached_cookie", return_value="cached-value"
            ):
                self.assertEqual(server._resolve_cookie(), "cached-value")

    def test_browser_value_preferred_over_cache_when_both_present(self) -> None:
        # Cookie may have rotated since the cache was last written; the
        # browser is the source of truth, cache is just a fallback.
        os.environ.pop("NTULEARN_COOKIE", None)
        with mock.patch.object(
            server, "read_bbrouter_cookie", return_value="rotated-value"
        ):
            with mock.patch.object(
                server, "read_cached_cookie", return_value="stale-cached-value"
            ):
                self.assertEqual(server._resolve_cookie(), "rotated-value")

    def test_env_var_does_not_touch_cache(self) -> None:
        # An explicit NTULEARN_COOKIE is the user's deliberate override —
        # we shouldn't mirror it into the keychain (it might be a one-off
        # debugging value) or read from the keychain to second-guess it.
        os.environ["NTULEARN_COOKIE"] = "explicit"
        with mock.patch.object(server, "read_bbrouter_cookie") as browser:
            with mock.patch.object(server, "write_cached_cookie") as write:
                with mock.patch.object(server, "read_cached_cookie") as read_cache:
                    self.assertEqual(server._resolve_cookie(), "explicit")
        browser.assert_not_called()
        write.assert_not_called()
        read_cache.assert_not_called()

    def test_does_not_consult_cache_when_browser_succeeds(self) -> None:
        # Hot-path optimization: if the browser read works, we don't need
        # to bother reading the cache too.
        os.environ.pop("NTULEARN_COOKIE", None)
        with mock.patch.object(server, "read_bbrouter_cookie", return_value="from-browser"):
            with mock.patch.object(server, "read_cached_cookie") as read_cache:
                server._resolve_cookie()
        read_cache.assert_not_called()

    def test_raises_when_browser_fails_and_cache_empty(self) -> None:
        os.environ.pop("NTULEARN_COOKIE", None)
        with mock.patch.object(server, "read_bbrouter_cookie", return_value=None):
            with mock.patch.object(server, "read_cached_cookie", return_value=None):
                with self.assertRaises(RuntimeError) as ctx:
                    server._resolve_cookie()
        self.assertIn("No NTULearn cookie found", str(ctx.exception))


class CookieRefreshTests(_CookieEnvIsolation, unittest.IsolatedAsyncioTestCase):
    async def asyncTearDown(self) -> None:
        # _refresh_client may have built a real httpx-backed client; close it
        # so the test's event loop doesn't complain about leftover sockets.
        if server._client is not None and server._client is not self._old_client:
            await server._client.close()

    async def test_call_tool_refreshes_then_retries_after_401(self) -> None:
        os.environ["NTULEARN_COOKIE"] = "test-cookie"
        server._client = NTULearnClient(server.BASE_URL, "test-cookie")

        call_count = 0
        original = server._list_courses

        async def flaky(client: Any, args: dict[str, Any]) -> list[TextContent]:
            nonlocal call_count
            call_count += 1
            if call_count == 1:
                raise BbRouterExpiredError()
            return [TextContent(type="text", text="success")]

        try:
            server._list_courses = flaky  # type: ignore[assignment]
            result = await server.call_tool("ntulearn_list_courses", {})
        finally:
            server._list_courses = original  # type: ignore[assignment]

        self.assertEqual(call_count, 2)
        self.assertEqual(result[0].text, "success")

    async def test_call_tool_surfaces_error_when_refresh_finds_no_cookie(self) -> None:
        os.environ["NTULEARN_COOKIE"] = "test-cookie"
        server._client = NTULearnClient(server.BASE_URL, "test-cookie")

        original = server._list_courses

        async def always_expired(client: Any, args: dict[str, Any]) -> list[TextContent]:
            raise BbRouterExpiredError()

        try:
            server._list_courses = always_expired  # type: ignore[assignment]
            # Refresh will look for a cookie and find none.
            os.environ.pop("NTULEARN_COOKIE", None)
            with mock.patch.object(server, "read_bbrouter_cookie", return_value=None):
                with self.assertRaises(RuntimeError) as ctx:
                    await server.call_tool("ntulearn_list_courses", {})
        finally:
            server._list_courses = original  # type: ignore[assignment]

        self.assertIn("No NTULearn cookie found", str(ctx.exception))

    async def test_call_tool_retries_only_once(self) -> None:
        os.environ["NTULEARN_COOKIE"] = "test-cookie"
        server._client = NTULearnClient(server.BASE_URL, "test-cookie")

        call_count = 0
        original = server._list_courses

        async def always_expired(client: Any, args: dict[str, Any]) -> list[TextContent]:
            nonlocal call_count
            call_count += 1
            raise BbRouterExpiredError()

        try:
            server._list_courses = always_expired  # type: ignore[assignment]
            with self.assertRaises(BbRouterExpiredError):
                await server.call_tool("ntulearn_list_courses", {})
        finally:
            server._list_courses = original  # type: ignore[assignment]

        # initial call + one retry, no third attempt before the error propagates
        self.assertEqual(call_count, 2)

    async def test_refresh_invalidates_cache_before_resolving(self) -> None:
        # The cookie that just produced a 401 might be the one in the cache
        # (we may have used it as a fallback when the browser failed). If we
        # re-resolve without nuking the cache first and the browser is still
        # failing, we'd loop forever on the same dead value.
        os.environ["NTULEARN_COOKIE"] = "test-cookie"
        server._client = NTULearnClient(server.BASE_URL, "test-cookie")

        with mock.patch.object(server, "delete_cached_cookie") as delete:
            await server._refresh_client()

        delete.assert_called_once()


class FakeGradebookClient:
    async def get_gradebook_columns(self, course_id: str) -> list[dict[str, Any]]:
        return [{"id": "col-1", "name": "Quiz", "score": {"possible": 10}}]

    async def get_my_user_id(self) -> str:
        return "user-1"

    async def get_user_grades(self, course_id: str, user_id: str) -> list[dict[str, Any]]:
        raise RuntimeError("grades endpoint unavailable")


class FakeDownloadClient:
    async def get_content_item(self, course_id: str, content_id: str) -> dict[str, Any]:
        return {
            "title": "Lecture",
            "contentHandler": {"id": "resource/x-bb-file"},
        }

    async def get_attachments(self, course_id: str, content_id: str) -> list[dict[str, Any]]:
        return [{"id": "att-1", "fileName": "slides.pdf"}]

    async def get_attachment_download_url(
        self, course_id: str, content_id: str, attachment_id: str
    ) -> str:
        return "/download/slides.pdf"

    async def download_bytes(self, url: str) -> tuple[bytes, str | None]:
        return b"new slides", "application/pdf"


class FakeSearchClient:
    async def get_course_contents(self, course_id: str) -> list[dict[str, Any]]:
        return [
            {"id": "1", "title": "match one", "description": {}, "hasChildren": False},
            {"id": "2", "title": "match two", "description": {}, "hasChildren": False},
            {"id": "3", "title": "match three", "description": {}, "hasChildren": False},
        ]

    async def get_content_children(
        self, course_id: str, content_id: str
    ) -> list[dict[str, Any]]:
        return []


class ToolBehaviorTests(unittest.IsolatedAsyncioTestCase):
    async def test_gradebook_partial_failure_is_reported(self) -> None:
        _, payload = await server._get_gradebook(FakeGradebookClient(), {"course_id": "course-1"})

        self.assertFalse(payload["gradesAvailable"])
        self.assertIn("grades endpoint unavailable", payload["gradeFetchError"])
        self.assertEqual(payload["columns"][0]["name"], "Quiz")
        self.assertIsNone(payload["columns"][0]["score"])

    async def test_download_does_not_overwrite_existing_file(self) -> None:
        old_download_dir = server.DOWNLOAD_DIR
        with tempfile.TemporaryDirectory() as tmp:
            server.DOWNLOAD_DIR = Path(tmp)
            (server.DOWNLOAD_DIR / "slides.pdf").write_bytes(b"old slides")
            try:
                _, payload = await server._download_file(
                    FakeDownloadClient(), {"course_id": "course-1", "content_id": "content-1"}
                )
            finally:
                server.DOWNLOAD_DIR = old_download_dir

            saved = payload["files"][0]
            self.assertEqual(saved["filename"], "slides (2).pdf")
            self.assertEqual((Path(tmp) / "slides.pdf").read_bytes(), b"old slides")
            self.assertEqual((Path(tmp) / "slides (2).pdf").read_bytes(), b"new slides")

    async def test_blank_search_query_errors(self) -> None:
        with self.assertRaises(ValueError) as ctx:
            await server._search_course_content(
                FakeSearchClient(), {"course_id": "course-1", "query": "   "}
            )
        self.assertIn("query cannot be blank", str(ctx.exception))

    async def test_search_max_results_caps_output(self) -> None:
        _, payload = await server._search_course_content(
            FakeSearchClient(),
            {"course_id": "course-1", "query": "match", "max_results": 2},
        )

        self.assertEqual(payload["count"], 2)
        self.assertEqual([item["id"] for item in payload["matches"]], ["1", "2"])


class ContentClassificationTests(unittest.TestCase):
    """Pure-function tests for _classify_kind — extension wins over content-type."""

    def test_pdf_extension_classified_as_pdf(self) -> None:
        self.assertEqual(server._classify_kind("slides.pdf", None), "pdf")

    def test_pdf_mime_classified_as_pdf(self) -> None:
        self.assertEqual(server._classify_kind("file", "application/pdf"), "pdf")

    def test_pdf_extension_wins_over_octet_stream(self) -> None:
        # The bbcswebdav case: server says octet-stream but extension is .pdf.
        self.assertEqual(
            server._classify_kind("lecture.pdf", "application/octet-stream"),
            "pdf",
        )

    def test_text_mime_classified_as_text(self) -> None:
        self.assertEqual(server._classify_kind("a", "text/plain"), "text")
        self.assertEqual(server._classify_kind("a", "text/html; charset=utf-8"), "text")
        self.assertEqual(server._classify_kind("a", "application/json"), "text")

    def test_text_extension_classified_as_text(self) -> None:
        for fname in ("notes.md", "data.csv", "config.yaml", "main.py", "readme.txt"):
            self.assertEqual(server._classify_kind(fname, None), "text", fname)

    def test_image_classified_as_binary(self) -> None:
        self.assertEqual(server._classify_kind("logo.png", "image/png"), "binary")

    def test_unknown_classified_as_binary(self) -> None:
        self.assertEqual(server._classify_kind("blob", None), "binary")
        self.assertEqual(
            server._classify_kind("video.mp4", "video/mp4"), "binary"
        )

    def test_docx_extension_classified(self) -> None:
        self.assertEqual(server._classify_kind("brief.docx", None), "docx")

    def test_pptx_extension_classified(self) -> None:
        self.assertEqual(server._classify_kind("slides.pptx", None), "pptx")

    def test_xlsx_extension_classified(self) -> None:
        self.assertEqual(server._classify_kind("data.xlsx", None), "xlsx")

    def test_office_mime_classified(self) -> None:
        cases = [
            (
                "application/vnd.openxmlformats-officedocument."
                "wordprocessingml.document",
                "docx",
            ),
            (
                "application/vnd.openxmlformats-officedocument."
                "presentationml.presentation",
                "pptx",
            ),
            (
                "application/vnd.openxmlformats-officedocument."
                "spreadsheetml.sheet",
                "xlsx",
            ),
        ]
        for mime, expected in cases:
            with self.subTest(mime=mime):
                self.assertEqual(server._classify_kind("file", mime), expected)

    def test_docx_extension_wins_over_octet_stream(self) -> None:
        # bbcswebdav case for Office files — same logic as the PDF test.
        self.assertEqual(
            server._classify_kind("brief.docx", "application/octet-stream"),
            "docx",
        )


class ContentExtractionTests(unittest.TestCase):
    """Pure-function tests for _extract_content."""

    def test_extracts_pdf_text(self) -> None:
        result = server._extract_content("hello.pdf", "application/pdf", _TINY_PDF_BYTES)
        self.assertEqual(result["kind"], "pdf")
        self.assertIn("Hello PDF Fixture", result["text"])
        self.assertEqual(result["pageCount"], 1)
        self.assertEqual(result["filename"], "hello.pdf")
        self.assertNotIn("warning", result)

    def test_decodes_utf8_text(self) -> None:
        result = server._extract_content("note.txt", "text/plain", "héllo".encode("utf-8"))
        self.assertEqual(result["kind"], "text")
        self.assertEqual(result["text"], "héllo")

    def test_decodes_with_charset_from_content_type(self) -> None:
        result = server._extract_content(
            "note.txt", "text/plain; charset=latin-1", "café".encode("latin-1")
        )
        self.assertEqual(result["text"], "café")

    def test_strips_html_tags(self) -> None:
        html = b"<html><body><h1>Hi</h1><p>Para <b>bold</b></p><script>x</script></body></html>"
        result = server._extract_content("page.html", "text/html", html)
        self.assertEqual(result["kind"], "text")
        self.assertNotIn("<h1>", result["text"])
        self.assertNotIn("<p>", result["text"])
        self.assertIn("Hi", result["text"])
        self.assertIn("bold", result["text"])

    def test_binary_returns_error_payload(self) -> None:
        png_magic = b"\x89PNG\r\n\x1a\n" + b"\x00" * 100
        result = server._extract_content("logo.png", "image/png", png_magic)
        self.assertEqual(result["kind"], "binary")
        self.assertIn("download_file", result["error"])
        self.assertIn("image/png", result["error"])

    def test_pdf_extension_wins_over_octet_stream(self) -> None:
        result = server._extract_content(
            "lecture.pdf", "application/octet-stream", _TINY_PDF_BYTES
        )
        self.assertEqual(result["kind"], "pdf")
        self.assertIn("Hello PDF Fixture", result["text"])

    def test_corrupted_pdf_returns_error(self) -> None:
        result = server._extract_content("bad.pdf", "application/pdf", b"not really a pdf")
        self.assertEqual(result["kind"], "pdf")
        self.assertIn("error", result)

    def test_extracts_docx_paragraphs(self) -> None:
        bytes_ = _make_docx_bytes(["First paragraph.", "Second paragraph."])
        result = server._extract_content("brief.docx", None, bytes_)
        self.assertEqual(result["kind"], "docx")
        self.assertIn("First paragraph", result["text"])
        self.assertIn("Second paragraph", result["text"])
        self.assertGreaterEqual(result["paragraphCount"], 2)
        self.assertEqual(result["tableCount"], 0)

    def test_extracts_docx_table(self) -> None:
        bytes_ = _make_docx_bytes(
            ["Header paragraph"],
            table_rows=[["A1", "B1"], ["A2", "B2"]],
        )
        result = server._extract_content("data.docx", None, bytes_)
        self.assertEqual(result["kind"], "docx")
        for cell in ("A1", "B1", "A2", "B2"):
            self.assertIn(cell, result["text"])
        self.assertEqual(result["tableCount"], 1)

    def test_extracts_pptx_slides(self) -> None:
        bytes_ = _make_pptx_bytes([
            {"title": "Slide One", "body": "Content one"},
            {"title": "Slide Two", "body": "Content two"},
            {"title": "Slide Three", "body": "Content three"},
        ])
        result = server._extract_content("deck.pptx", None, bytes_)
        self.assertEqual(result["kind"], "pptx")
        self.assertEqual(result["slideCount"], 3)
        self.assertIn("## Slide 1", result["text"])
        self.assertIn("## Slide 3", result["text"])
        self.assertIn("Slide One", result["text"])
        self.assertIn("Content three", result["text"])

    def test_extracts_pptx_speaker_notes(self) -> None:
        bytes_ = _make_pptx_bytes([
            {"title": "Title", "body": "Body", "notes": "Remember to mention X."},
        ])
        result = server._extract_content("deck.pptx", None, bytes_)
        self.assertEqual(result["kind"], "pptx")
        self.assertIn("Speaker notes", result["text"])
        self.assertIn("Remember to mention X", result["text"])

    def test_extracts_xlsx_sheets(self) -> None:
        bytes_ = _make_xlsx_bytes({
            "Grades": [["Student", "Score"], ["Alice", 85], ["Bob", 92]],
            "Summary": [["Mean", 88.5]],
        })
        result = server._extract_content("grades.xlsx", None, bytes_)
        self.assertEqual(result["kind"], "xlsx")
        self.assertEqual(result["sheetCount"], 2)
        self.assertIn("## Sheet: Grades", result["text"])
        self.assertIn("## Sheet: Summary", result["text"])
        self.assertIn("Alice", result["text"])
        self.assertIn("85", result["text"])
        self.assertIn("88.5", result["text"])
        self.assertNotIn("warning", result)

    def test_xlsx_truncates_above_row_cap(self) -> None:
        # One sheet exceeds the cap; assert the warning + truncation marker.
        rows = [[f"r{i}", i] for i in range(server._MAX_XLSX_ROWS_PER_SHEET + 50)]
        bytes_ = _make_xlsx_bytes({"Big": rows})
        result = server._extract_content("big.xlsx", None, bytes_)
        self.assertEqual(result["kind"], "xlsx")
        self.assertIn("warning", result)
        self.assertIn("truncated", result["text"])

    def test_corrupted_docx_returns_error(self) -> None:
        result = server._extract_content("bad.docx", None, b"not a docx file at all")
        self.assertEqual(result["kind"], "docx")
        self.assertIn("error", result)
        self.assertNotIn("text", result)


class _StubResolveClient:
    """Implements only get_content_item — for handlers that hit _resolve_content_files
    and short-circuit on no-pairs. Avoids needing get_attachments etc."""

    def __init__(self, item: dict[str, Any]) -> None:
        self._item = item

    async def get_content_item(self, course_id: str, content_id: str) -> dict[str, Any]:
        return self._item


class FakeReadClient:
    """Mock for _read_file_content that maps URLs to (bytes, content_type)."""

    def __init__(
        self,
        url_to_payload: dict[str, tuple[bytes, str | None]],
        item: dict[str, Any] | None = None,
        attachments: list[dict[str, Any]] | None = None,
        attachment_urls: dict[str, str] | None = None,
    ) -> None:
        self._url_to_payload = url_to_payload
        self._item = item or {
            "title": "Lecture 1",
            "contentHandler": {"id": "resource/x-bb-file"},
        }
        self._attachments = attachments or []
        self._attachment_urls = attachment_urls or {}

    async def get_content_item(self, course_id: str, content_id: str) -> dict[str, Any]:
        return self._item

    async def get_attachments(self, course_id: str, content_id: str) -> list[dict[str, Any]]:
        return self._attachments

    async def get_attachment_download_url(
        self, course_id: str, content_id: str, attachment_id: str
    ) -> str:
        return self._attachment_urls[attachment_id]

    async def download_bytes(self, url: str) -> tuple[bytes, str | None]:
        if url not in self._url_to_payload:
            raise AssertionError(f"unexpected download URL: {url}")
        return self._url_to_payload[url]


class ReadFileContentTests(unittest.IsolatedAsyncioTestCase):
    """End-to-end tests for the _read_file_content handler."""

    async def test_extracts_pdf_text(self) -> None:
        client = FakeReadClient(
            url_to_payload={"/url/slides.pdf": (_TINY_PDF_BYTES, "application/pdf")},
            attachments=[{"id": "att-1", "fileName": "slides.pdf"}],
            attachment_urls={"att-1": "/url/slides.pdf"},
        )
        _, payload = await server._read_file_content(
            client, {"course_id": "c", "content_id": "i"}
        )
        self.assertEqual(len(payload["files"]), 1)
        self.assertEqual(len(payload["skipped"]), 0)
        f = payload["files"][0]
        self.assertEqual(f["kind"], "pdf")
        self.assertEqual(f["filename"], "slides.pdf")
        self.assertIn("Hello PDF Fixture", f["text"])
        self.assertEqual(f["pageCount"], 1)

    async def test_decodes_text_file(self) -> None:
        client = FakeReadClient(
            url_to_payload={"/u/notes.txt": (b"hello world", "text/plain")},
            attachments=[{"id": "a", "fileName": "notes.txt"}],
            attachment_urls={"a": "/u/notes.txt"},
        )
        _, payload = await server._read_file_content(
            client, {"course_id": "c", "content_id": "i"}
        )
        self.assertEqual(payload["files"][0]["kind"], "text")
        self.assertEqual(payload["files"][0]["text"], "hello world")

    async def test_strips_html(self) -> None:
        html = b"<html><body><h1>Title</h1><p>Body text</p></body></html>"
        client = FakeReadClient(
            url_to_payload={"/u/page.html": (html, "text/html; charset=utf-8")},
            attachments=[{"id": "a", "fileName": "page.html"}],
            attachment_urls={"a": "/u/page.html"},
        )
        _, payload = await server._read_file_content(
            client, {"course_id": "c", "content_id": "i"}
        )
        text = payload["files"][0]["text"]
        self.assertNotIn("<h1>", text)
        self.assertNotIn("<body>", text)
        self.assertIn("Title", text)
        self.assertIn("Body text", text)

    async def test_refuses_binary(self) -> None:
        client = FakeReadClient(
            url_to_payload={"/u/img.png": (b"\x89PNG\r\n\x1a\n" + b"\x00" * 50, "image/png")},
            attachments=[{"id": "a", "fileName": "img.png"}],
            attachment_urls={"a": "/u/img.png"},
        )
        _, payload = await server._read_file_content(
            client, {"course_id": "c", "content_id": "i"}
        )
        self.assertEqual(payload["files"], [])
        self.assertEqual(len(payload["skipped"]), 1)
        skipped = payload["skipped"][0]
        self.assertEqual(skipped["filename"], "img.png")
        self.assertIn("download_file", skipped["reason"])

    async def test_size_cap_per_file(self) -> None:
        oversized = b"\x00" * (server._MAX_FILE_BYTES + 1)
        client = FakeReadClient(
            url_to_payload={"/u/huge.pdf": (oversized, "application/pdf")},
            attachments=[{"id": "a", "fileName": "huge.pdf"}],
            attachment_urls={"a": "/u/huge.pdf"},
        )
        _, payload = await server._read_file_content(
            client, {"course_id": "c", "content_id": "i"}
        )
        self.assertEqual(payload["files"], [])
        self.assertEqual(len(payload["skipped"]), 1)
        self.assertIn("too large", payload["skipped"][0]["reason"].lower())

    async def test_octet_stream_with_pdf_extension(self) -> None:
        # Common bbcswebdav case — server says octet-stream, extension says PDF.
        client = FakeReadClient(
            url_to_payload={
                "/u/lecture.pdf": (_TINY_PDF_BYTES, "application/octet-stream")
            },
            attachments=[{"id": "a", "fileName": "lecture.pdf"}],
            attachment_urls={"a": "/u/lecture.pdf"},
        )
        _, payload = await server._read_file_content(
            client, {"course_id": "c", "content_id": "i"}
        )
        self.assertEqual(payload["files"][0]["kind"], "pdf")
        self.assertIn("Hello PDF Fixture", payload["files"][0]["text"])

    async def test_no_files_returns_error_payload(self) -> None:
        # resource/x-bb-document with no extractable URLs in body.
        client = FakeReadClient(
            url_to_payload={},
            item={
                "title": "Empty Item",
                "contentHandler": {"id": "resource/x-bb-document"},
                "body": "<p>just some text, no links</p>",
            },
        )
        _, payload = await server._read_file_content(
            client, {"course_id": "c", "content_id": "i"}
        )
        self.assertIn("error", payload)
        self.assertEqual(payload["title"], "Empty Item")
        self.assertEqual(payload["contentHandlerId"], "resource/x-bb-document")

    async def test_per_url_401_retry(self) -> None:
        # First download raises BbRouterExpiredError; the inline retry path
        # calls _refresh_client (patched to return a fresh stub) and re-downloads.
        expired_once = {"done": False}

        class FlakyClient(FakeReadClient):
            async def download_bytes(
                self, url: str
            ) -> tuple[bytes, str | None]:
                if not expired_once["done"]:
                    expired_once["done"] = True
                    raise BbRouterExpiredError()
                return await super().download_bytes(url)

        flaky = FlakyClient(
            url_to_payload={"/u/notes.txt": (b"after retry", "text/plain")},
            attachments=[{"id": "a", "fileName": "notes.txt"}],
            attachment_urls={"a": "/u/notes.txt"},
        )

        refreshed = FakeReadClient(
            url_to_payload={"/u/notes.txt": (b"after retry", "text/plain")},
        )

        async def fake_refresh() -> Any:
            return refreshed

        with mock.patch.object(server, "_refresh_client", fake_refresh):
            _, payload = await server._read_file_content(
                flaky, {"course_id": "c", "content_id": "i"}
            )

        self.assertTrue(expired_once["done"])
        self.assertEqual(payload["files"][0]["text"], "after retry")

    async def test_handles_multiple_files(self) -> None:
        client = FakeReadClient(
            url_to_payload={
                "/u/a.txt": (b"alpha", "text/plain"),
                "/u/b.png": (b"\x89PNG\r\n\x1a\n", "image/png"),
                "/u/c.pdf": (_TINY_PDF_BYTES, "application/pdf"),
            },
            attachments=[
                {"id": "1", "fileName": "a.txt"},
                {"id": "2", "fileName": "b.png"},
                {"id": "3", "fileName": "c.pdf"},
            ],
            attachment_urls={"1": "/u/a.txt", "2": "/u/b.png", "3": "/u/c.pdf"},
        )
        _, payload = await server._read_file_content(
            client, {"course_id": "c", "content_id": "i"}
        )
        kinds = sorted(f["kind"] for f in payload["files"])
        self.assertEqual(kinds, ["pdf", "text"])
        self.assertEqual(len(payload["skipped"]), 1)
        self.assertEqual(payload["skipped"][0]["filename"], "b.png")

    async def test_reads_docx_attachment(self) -> None:
        # End-to-end happy path for one Office format. The per-format unit
        # tests in ContentExtractionTests cover pptx/xlsx parsing details;
        # here we just verify the handler routes Office files into `files`
        # (not `skipped`) and threads through the content_type override.
        docx_bytes = _make_docx_bytes(["Assignment brief content."])
        client = FakeReadClient(
            url_to_payload={
                "/u/brief.docx": (docx_bytes, "application/octet-stream")
            },
            attachments=[{"id": "a", "fileName": "brief.docx"}],
            attachment_urls={"a": "/u/brief.docx"},
        )
        _, payload = await server._read_file_content(
            client, {"course_id": "c", "content_id": "i"}
        )
        self.assertEqual(len(payload["files"]), 1)
        self.assertEqual(payload["skipped"], [])
        f = payload["files"][0]
        self.assertEqual(f["kind"], "docx")
        self.assertIn("Assignment brief content", f["text"])


class ResolveContentFilesTests(unittest.IsolatedAsyncioTestCase):
    """Direct tests of the _resolve_content_files helper."""

    async def test_resolves_attachment_handler(self) -> None:
        client = FakeReadClient(
            url_to_payload={},
            attachments=[
                {"id": "a", "fileName": "x.pdf"},
                {"id": "b", "fileName": "y.docx"},
            ],
            attachment_urls={"a": "/u/x.pdf", "b": "/u/y.docx"},
        )
        item, handler_id, pairs = await server._resolve_content_files(client, "c", "i")
        self.assertEqual(handler_id, "resource/x-bb-file")
        self.assertEqual(pairs, [("/u/x.pdf", "x.pdf"), ("/u/y.docx", "y.docx")])
        self.assertEqual(item["title"], "Lecture 1")

    async def test_resolves_html_body_handler(self) -> None:
        # Use the real extract_all_files path via a body containing bbcswebdav links.
        body = (
            '<a href="https://ntulearn.ntu.edu.sg/bbcswebdav/'
            'pid-1/notes.pdf">Notes</a>'
        )
        client = FakeReadClient(
            url_to_payload={},
            item={
                "title": "Doc",
                "contentHandler": {"id": "resource/x-bb-document"},
                "body": body,
            },
        )
        _, handler_id, pairs = await server._resolve_content_files(client, "c", "i")
        self.assertEqual(handler_id, "resource/x-bb-document")
        self.assertEqual(len(pairs), 1)
        self.assertIn("bbcswebdav", pairs[0][0])

    async def test_no_files_returns_empty_pairs(self) -> None:
        client = FakeReadClient(
            url_to_payload={},
            item={
                "title": "Empty",
                "contentHandler": {"id": "resource/x-bb-document"},
                "body": "<p>nothing useful</p>",
            },
        )
        _, _, pairs = await server._resolve_content_files(client, "c", "i")
        self.assertEqual(pairs, [])


class PdfModeAndPageRangeParsingTests(unittest.TestCase):
    """Pure-function tests for the PDF mode/pages arg parsers."""

    def test_resolve_pdf_mode_default_is_auto(self) -> None:
        self.assertEqual(server._resolve_pdf_mode({}), "auto")

    def test_resolve_pdf_mode_explicit_text(self) -> None:
        self.assertEqual(server._resolve_pdf_mode({"mode": "text"}), "text")

    def test_resolve_pdf_mode_case_insensitive(self) -> None:
        self.assertEqual(server._resolve_pdf_mode({"mode": "AUTO"}), "auto")

    def test_resolve_pdf_mode_invalid_raises(self) -> None:
        with self.assertRaises(ValueError):
            server._resolve_pdf_mode({"mode": "vision"})

    def test_parse_page_range_none(self) -> None:
        self.assertIsNone(server._parse_page_range(None))

    def test_parse_page_range_empty(self) -> None:
        self.assertIsNone(server._parse_page_range(""))
        self.assertIsNone(server._parse_page_range("   "))

    def test_parse_page_range_single_page(self) -> None:
        self.assertEqual(server._parse_page_range("5"), {5})

    def test_parse_page_range_simple_range(self) -> None:
        self.assertEqual(server._parse_page_range("1-3"), {1, 2, 3})

    def test_parse_page_range_mixed(self) -> None:
        self.assertEqual(
            server._parse_page_range("1-3,5,8-9"), {1, 2, 3, 5, 8, 9}
        )

    def test_parse_page_range_whitespace_tolerated(self) -> None:
        self.assertEqual(server._parse_page_range(" 1 - 3 , 5 "), {1, 2, 3, 5})

    def test_parse_page_range_invalid_token_raises(self) -> None:
        with self.assertRaises(ValueError):
            server._parse_page_range("abc")

    def test_parse_page_range_zero_raises(self) -> None:
        with self.assertRaises(ValueError):
            server._parse_page_range("0")

    def test_parse_page_range_inverted_raises(self) -> None:
        with self.assertRaises(ValueError):
            server._parse_page_range("5-3")


class PdfVisionExtractionTests(unittest.TestCase):
    """Pure-function tests for _extract_pdf_vision."""

    def test_renders_each_page_as_png(self) -> None:
        result = server._extract_pdf_vision(
            "slides.pdf", "application/pdf", _TINY_PDF_BYTES, len(_TINY_PDF_BYTES), None
        )
        self.assertEqual(result["kind"], "pdf")
        self.assertEqual(result["pageCount"], 1)
        self.assertEqual(result["pagesRendered"], [1])
        images = result["_images"]
        self.assertEqual(len(images), 1)
        label, png_bytes = images[0]
        self.assertIn("page 1", label)
        # PNG magic header — confirms PyMuPDF actually rendered an image.
        self.assertTrue(png_bytes.startswith(b"\x89PNG\r\n\x1a\n"))

    def test_text_includes_per_page_header(self) -> None:
        result = server._extract_pdf_vision(
            "slides.pdf", "application/pdf", _TINY_PDF_BYTES, len(_TINY_PDF_BYTES), None
        )
        self.assertIn("## Page 1", result["text"])
        self.assertIn("Hello PDF Fixture", result["text"])

    def test_pages_filter_restricts_render(self) -> None:
        # Filter to a non-existent page — extractor should render nothing
        # and report empty pagesRendered. This exercises the 1-indexed
        # bounds check without needing a multi-page fixture.
        result = server._extract_pdf_vision(
            "slides.pdf",
            "application/pdf",
            _TINY_PDF_BYTES,
            len(_TINY_PDF_BYTES),
            {99},
        )
        self.assertEqual(result["pageCount"], 1)
        self.assertEqual(result["pagesRendered"], [])
        self.assertEqual(result["_images"], [])

    def test_corrupted_pdf_returns_error(self) -> None:
        result = server._extract_pdf_vision(
            "bad.pdf", "application/pdf", b"not really a pdf", 16, None
        )
        self.assertEqual(result["kind"], "pdf")
        self.assertIn("error", result)
        self.assertNotIn("_images", result)


class ReadFileContentVisionTests(unittest.IsolatedAsyncioTestCase):
    """End-to-end tests for the vision/text mode handling in _read_file_content."""

    async def test_auto_mode_emits_image_blocks(self) -> None:
        client = FakeReadClient(
            url_to_payload={"/u/slides.pdf": (_TINY_PDF_BYTES, "application/pdf")},
            attachments=[{"id": "a", "fileName": "slides.pdf"}],
            attachment_urls={"a": "/u/slides.pdf"},
        )
        content, payload = await server._read_file_content(
            client, {"course_id": "c", "content_id": "i"}
        )
        # Structured payload still has the text + page count.
        f = payload["files"][0]
        self.assertEqual(f["kind"], "pdf")
        self.assertEqual(f["pageCount"], 1)
        self.assertEqual(f["pagesRendered"], [1])
        # Structured payload must NOT carry the raw image bytes — those
        # only belong in the unstructured content list as ImageContent.
        self.assertNotIn("_images", f)
        # Unstructured content includes one TextContent + one ImageContent.
        text_blocks = [b for b in content if isinstance(b, TextContent)]
        image_blocks = [b for b in content if isinstance(b, ImageContent)]
        self.assertEqual(len(text_blocks), 1)
        self.assertEqual(len(image_blocks), 1)
        self.assertEqual(image_blocks[0].mimeType, "image/png")
        # base64 data is non-empty and decodes back to a PNG.
        self.assertTrue(image_blocks[0].data)
        self.assertTrue(
            base64.b64decode(image_blocks[0].data).startswith(b"\x89PNG\r\n\x1a\n")
        )

    async def test_text_mode_skips_image_rendering(self) -> None:
        client = FakeReadClient(
            url_to_payload={"/u/slides.pdf": (_TINY_PDF_BYTES, "application/pdf")},
            attachments=[{"id": "a", "fileName": "slides.pdf"}],
            attachment_urls={"a": "/u/slides.pdf"},
        )
        content, payload = await server._read_file_content(
            client, {"course_id": "c", "content_id": "i", "mode": "text"}
        )
        f = payload["files"][0]
        self.assertEqual(f["kind"], "pdf")
        # text-mode pypdf path does not populate pagesRendered.
        self.assertNotIn("pagesRendered", f)
        self.assertIn("Hello PDF Fixture", f["text"])
        # No ImageContent blocks in the unstructured content list.
        image_blocks = [b for b in content if isinstance(b, ImageContent)]
        self.assertEqual(image_blocks, [])

    async def test_invalid_mode_raises(self) -> None:
        client = FakeReadClient(
            url_to_payload={"/u/slides.pdf": (_TINY_PDF_BYTES, "application/pdf")},
            attachments=[{"id": "a", "fileName": "slides.pdf"}],
            attachment_urls={"a": "/u/slides.pdf"},
        )
        with self.assertRaises(ValueError):
            await server._read_file_content(
                client,
                {"course_id": "c", "content_id": "i", "mode": "vision"},
            )

    async def test_pages_filter_threads_through(self) -> None:
        # Single-page PDF; pages="1" preserves it. Asserts the arg actually
        # reaches _extract_pdf_vision (vs. being silently dropped).
        client = FakeReadClient(
            url_to_payload={"/u/slides.pdf": (_TINY_PDF_BYTES, "application/pdf")},
            attachments=[{"id": "a", "fileName": "slides.pdf"}],
            attachment_urls={"a": "/u/slides.pdf"},
        )
        _, payload = await server._read_file_content(
            client,
            {"course_id": "c", "content_id": "i", "pages": "1"},
        )
        self.assertEqual(payload["files"][0]["pagesRendered"], [1])

    async def test_pages_filter_excludes_out_of_range(self) -> None:
        client = FakeReadClient(
            url_to_payload={"/u/slides.pdf": (_TINY_PDF_BYTES, "application/pdf")},
            attachments=[{"id": "a", "fileName": "slides.pdf"}],
            attachment_urls={"a": "/u/slides.pdf"},
        )
        content, payload = await server._read_file_content(
            client,
            {"course_id": "c", "content_id": "i", "pages": "99"},
        )
        # Page 99 doesn't exist on a 1-page PDF — extractor should render
        # nothing and we should see no ImageContent.
        self.assertEqual(payload["files"][0]["pagesRendered"], [])
        image_blocks = [b for b in content if isinstance(b, ImageContent)]
        self.assertEqual(image_blocks, [])

    async def test_invalid_pages_arg_raises(self) -> None:
        client = FakeReadClient(
            url_to_payload={"/u/slides.pdf": (_TINY_PDF_BYTES, "application/pdf")},
            attachments=[{"id": "a", "fileName": "slides.pdf"}],
            attachment_urls={"a": "/u/slides.pdf"},
        )
        with self.assertRaises(ValueError):
            await server._read_file_content(
                client,
                {"course_id": "c", "content_id": "i", "pages": "abc"},
            )

    async def test_text_mode_does_not_apply_to_office_formats(self) -> None:
        # mode='text' is only meaningful for PDFs; .docx still extracts
        # paragraphs via python-docx and produces no ImageContent.
        docx_bytes = _make_docx_bytes(["Sample text body."])
        client = FakeReadClient(
            url_to_payload={"/u/brief.docx": (docx_bytes, None)},
            attachments=[{"id": "a", "fileName": "brief.docx"}],
            attachment_urls={"a": "/u/brief.docx"},
        )
        content, payload = await server._read_file_content(
            client, {"course_id": "c", "content_id": "i", "mode": "text"}
        )
        self.assertEqual(payload["files"][0]["kind"], "docx")
        self.assertIn("Sample text body", payload["files"][0]["text"])
        image_blocks = [b for b in content if isinstance(b, ImageContent)]
        self.assertEqual(image_blocks, [])


if __name__ == "__main__":
    unittest.main()
