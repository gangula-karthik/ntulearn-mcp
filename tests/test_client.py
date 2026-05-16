from __future__ import annotations

import sys
import unittest
from pathlib import Path

import httpx

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "src"))

from ntulearn_mcp.client import NTULearnClient


class DownloadSafetyTests(unittest.IsolatedAsyncioTestCase):
    async def test_same_origin_download_sends_cookie(self) -> None:
        seen_cookie: str | None = None

        def handler(request: httpx.Request) -> httpx.Response:
            nonlocal seen_cookie
            seen_cookie = request.headers.get("cookie")
            return httpx.Response(200, content=b"same-origin")

        client = NTULearnClient(
            "https://ntulearn.ntu.edu.sg",
            "secret",
            transport=httpx.MockTransport(handler),
        )
        try:
            content, _ = await client.download_bytes(
                "https://ntulearn.ntu.edu.sg/bbcswebdav/file.pdf"
            )
        finally:
            await client.close()

        self.assertEqual(content, b"same-origin")
        self.assertEqual(seen_cookie, "BbRouter=secret")

    async def test_allowed_external_download_omits_cookie(self) -> None:
        seen_cookie: str | None = None

        def internal_handler(request: httpx.Request) -> httpx.Response:
            raise AssertionError("external download should not use authenticated client")

        def external_handler(request: httpx.Request) -> httpx.Response:
            nonlocal seen_cookie
            seen_cookie = request.headers.get("cookie")
            return httpx.Response(200, content=b"external")

        client = NTULearnClient(
            "https://ntulearn.ntu.edu.sg",
            "secret",
            transport=httpx.MockTransport(internal_handler),
            external_transport=httpx.MockTransport(external_handler),
        )
        try:
            content, _ = await client.download_bytes(
                "https://alt-123.blackboard.com/bbcswebdav/file.pdf"
            )
        finally:
            await client.close()

        self.assertEqual(content, b"external")
        self.assertIsNone(seen_cookie)

    async def test_unsafe_external_download_host_is_rejected(self) -> None:
        def handler(request: httpx.Request) -> httpx.Response:
            raise AssertionError("unsafe URL should not be fetched")

        client = NTULearnClient(
            "https://ntulearn.ntu.edu.sg",
            "secret",
            transport=httpx.MockTransport(handler),
            external_transport=httpx.MockTransport(handler),
        )
        try:
            with self.assertRaisesRegex(ValueError, "Unsafe download URL host"):
                await client.download_bytes("https://evil.example/bbcswebdav/file.pdf")
        finally:
            await client.close()


class CalendarItemsTests(unittest.IsolatedAsyncioTestCase):
    """Coverage for the calendar wrapper that feeds ntulearn_get_upcoming."""

    async def test_courseid_since_until_and_type_are_forwarded(self) -> None:
        seen: dict[str, str] = {}

        def handler(request: httpx.Request) -> httpx.Response:
            seen.update(dict(request.url.params))
            return httpx.Response(
                200, json={"results": [{"id": "ci-1", "title": "Quiz"}], "paging": {}}
            )

        client = NTULearnClient(
            "https://ntulearn.ntu.edu.sg",
            "secret",
            transport=httpx.MockTransport(handler),
        )
        try:
            items = await client.get_calendar_items(
                course_id="_123_1",
                since="2026-05-23T00:00:00Z",
                until="2026-05-30T00:00:00Z",
                item_type="GradebookColumn",
            )
        finally:
            await client.close()

        self.assertEqual(seen["courseId"], "_123_1")
        self.assertEqual(seen["since"], "2026-05-23T00:00:00Z")
        self.assertEqual(seen["until"], "2026-05-30T00:00:00Z")
        self.assertEqual(seen["type"], "GradebookColumn")
        self.assertEqual(items, [{"id": "ci-1", "title": "Quiz"}])

    async def test_empty_window_returns_empty_list(self) -> None:
        def handler(request: httpx.Request) -> httpx.Response:
            return httpx.Response(200, json={"results": [], "paging": {}})

        client = NTULearnClient(
            "https://ntulearn.ntu.edu.sg",
            "secret",
            transport=httpx.MockTransport(handler),
        )
        try:
            items = await client.get_calendar_items(course_id="_1_1")
        finally:
            await client.close()

        self.assertEqual(items, [])

    async def test_429_raises_blackboard_api_error_with_rate_limit_message(
        self,
    ) -> None:
        # Anthology docs warn unscoped calendar calls under non-3LO auth can be
        # throttled — confirm we surface a 429 distinctly rather than crashing.
        from ntulearn_mcp.client import BlackboardAPIError

        def handler(request: httpx.Request) -> httpx.Response:
            return httpx.Response(429, content=b"throttled")

        client = NTULearnClient(
            "https://ntulearn.ntu.edu.sg",
            "secret",
            transport=httpx.MockTransport(handler),
        )
        try:
            with self.assertRaises(BlackboardAPIError) as ctx:
                await client.get_calendar_items(course_id="_1_1")
        finally:
            await client.close()

        self.assertEqual(ctx.exception.status_code, 429)
        self.assertIn("rate limited", str(ctx.exception).lower())


if __name__ == "__main__":
    unittest.main()
