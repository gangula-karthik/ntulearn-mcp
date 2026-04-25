from __future__ import annotations

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

from mcp.types import TextContent

from ntulearn_mcp import server
from ntulearn_mcp.client import BbRouterExpiredError, NTULearnClient


class _CookieEnvIsolation:
    """Mixin that snapshots NTULEARN_COOKIE + server._client and restores them."""

    def setUp(self) -> None:
        self._old_env = os.environ.get("NTULEARN_COOKIE")
        self._old_client = server._client

    def tearDown(self) -> None:
        if self._old_env is None:
            os.environ.pop("NTULEARN_COOKIE", None)
        else:
            os.environ["NTULEARN_COOKIE"] = self._old_env
        server._client = self._old_client


class MissingCookieTests(_CookieEnvIsolation, unittest.IsolatedAsyncioTestCase):
    async def test_missing_cookie_returns_mcp_error(self) -> None:
        os.environ.pop("NTULEARN_COOKIE", None)
        server._client = None
        with mock.patch.object(server, "read_bbrouter_cookie", return_value=None):
            result = await server.call_tool("list_courses", {})

        self.assertEqual(len(result), 1)
        self.assertIn("No NTULearn cookie found", result[0].text)


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
            result = await server.call_tool("list_courses", {})
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
                result = await server.call_tool("list_courses", {})
        finally:
            server._list_courses = original  # type: ignore[assignment]

        self.assertIn("No NTULearn cookie found", result[0].text)

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
            result = await server.call_tool("list_courses", {})
        finally:
            server._list_courses = original  # type: ignore[assignment]

        self.assertEqual(call_count, 2)  # initial + one retry, no third attempt
        self.assertIn("expired", result[0].text.lower())


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
        result = await server._get_gradebook(FakeGradebookClient(), {"course_id": "course-1"})
        payload = json.loads(result[0].text)

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
                result = await server._download_file(
                    FakeDownloadClient(), {"course_id": "course-1", "content_id": "content-1"}
                )
            finally:
                server.DOWNLOAD_DIR = old_download_dir

            payload = json.loads(result[0].text)
            saved = payload["files"][0]
            self.assertEqual(saved["filename"], "slides (2).pdf")
            self.assertEqual((Path(tmp) / "slides.pdf").read_bytes(), b"old slides")
            self.assertEqual((Path(tmp) / "slides (2).pdf").read_bytes(), b"new slides")

    async def test_blank_search_query_errors(self) -> None:
        result = await server._search_course_content(
            FakeSearchClient(), {"course_id": "course-1", "query": "   "}
        )

        self.assertIn("query cannot be blank", result[0].text)

    async def test_search_max_results_caps_output(self) -> None:
        result = await server._search_course_content(
            FakeSearchClient(),
            {"course_id": "course-1", "query": "match", "max_results": 2},
        )
        payload = json.loads(result[0].text)

        self.assertEqual(len(payload), 2)
        self.assertEqual([item["id"] for item in payload], ["1", "2"])


if __name__ == "__main__":
    unittest.main()
