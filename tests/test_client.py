from __future__ import annotations

import sys
import unittest
from pathlib import Path

import httpx

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "src"))

from ntulearn_mcp.client import (
    BbRouterExpiredError,
    BlackboardAPIError,
    NTULearnClient,
)


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


class RetryTests(unittest.IsolatedAsyncioTestCase):
    """Retry+backoff only applies in production mode; tests flip the flag off.

    The constructor always injects a MockTransport (test mode) so the
    deterministic-mock contract holds; we then re-enable production
    behaviour explicitly for the code under test and neutralise backoff
    sleeps.
    """

    def _prod_client(self, handler, **kw):
        from unittest import mock

        client = NTULearnClient(
            "https://ntulearn.ntu.edu.sg",
            "secret",
            transport=httpx.MockTransport(handler),
            **kw,
        )
        client._test_mode = False
        client._cache_enabled = False
        patcher = mock.patch.object(NTULearnClient, "_backoff", return_value=0.0)
        patcher.start()
        self.addCleanup(patcher.stop)
        return client

    async def test_transient_429_then_success_is_retried(self) -> None:
        calls = {"n": 0}

        def handler(request: httpx.Request) -> httpx.Response:
            calls["n"] += 1
            if calls["n"] == 1:
                return httpx.Response(429, content=b"slow down")
            return httpx.Response(200, json={"id": "ok"})

        client = self._prod_client(handler)
        try:
            data = await client.get_course("_1_1")
        finally:
            await client.close()
        self.assertEqual(data, {"id": "ok"})
        self.assertEqual(calls["n"], 2)

    async def test_500_then_success_is_retried(self) -> None:
        calls = {"n": 0}

        def handler(request: httpx.Request) -> httpx.Response:
            calls["n"] += 1
            if calls["n"] == 1:
                return httpx.Response(500, content=b"boom")
            return httpx.Response(200, json={"id": "ok"})

        client = self._prod_client(handler)
        try:
            data = await client.get_course("_1_1")
        finally:
            await client.close()
        self.assertEqual(data, {"id": "ok"})
        self.assertEqual(calls["n"], 2)

    async def test_persistent_500_raises_after_exhausting_retries(self) -> None:
        calls = {"n": 0}

        def handler(request: httpx.Request) -> httpx.Response:
            calls["n"] += 1
            return httpx.Response(500, content=b"boom")

        client = self._prod_client(handler)
        try:
            with self.assertRaises(BlackboardAPIError) as ctx:
                await client.get_course("_1_1")
        finally:
            await client.close()
        self.assertEqual(ctx.exception.status_code, 500)
        self.assertEqual(calls["n"], 3)

    async def test_404_is_never_retried(self) -> None:
        calls = {"n": 0}

        def handler(request: httpx.Request) -> httpx.Response:
            calls["n"] += 1
            return httpx.Response(404, content=b"nope")

        client = self._prod_client(handler)
        try:
            with self.assertRaises(BlackboardAPIError):
                await client.get_course("_1_1")
        finally:
            await client.close()
        self.assertEqual(calls["n"], 1)

    async def test_401_raises_expired_and_invalidates_cache(self) -> None:
        import tempfile
        from unittest import mock
        from ntulearn_mcp.cache import DataCache

        dc = DataCache(cache_dir=tempfile.mkdtemp() + "/cache.sqlite3")
        scope = NTULearnClient(
            "https://x.example", "secret"
        )._user_scope
        dc.set("get_course", scope + ":abc", {"name": "cached"}, 600, user_scope=scope)

        def handler(request: httpx.Request) -> httpx.Response:
            return httpx.Response(401, content=b"expired")

        client = NTULearnClient(
            "https://ntulearn.ntu.edu.sg",
            "secret",
            transport=httpx.MockTransport(handler),
            data_cache=dc,
        )
        client._test_mode = False  # production path: caching + invalidation on
        client._cache_enabled = True
        with mock.patch.object(NTULearnClient, "_backoff", return_value=0.0):
            try:
                with self.assertRaises(BbRouterExpiredError):
                    await client.get_course("_1_1")
            finally:
                await client.close()
        self.assertIsNone(dc.get("get_course", scope + ":abc"))

    async def test_node_retry_count_is_one_in_default_test_mode(self) -> None:
        # MockTransport-only construction must NOT retry (deterministic tests).
        calls = {"n": 0}

        def handler(request: httpx.Request) -> httpx.Response:
            calls["n"] += 1
            return httpx.Response(429, content=b"slow down")

        client = NTULearnClient(
            "https://ntulearn.ntu.edu.sg",
            "secret",
            transport=httpx.MockTransport(handler),
        )
        try:
            with self.assertRaises(BlackboardAPIError) as ctx:
                await client.get_course("_1_1")
        finally:
            await client.close()
        self.assertEqual(ctx.exception.status_code, 429)
        self.assertEqual(calls["n"], 1)


class FieldsFallbackTests(unittest.IsolatedAsyncioTestCase):
    async def test_400_with_fields_retries_without_fields(self) -> None:
        from unittest import mock

        calls: list[dict] = []

        def handler(request: httpx.Request) -> httpx.Response:
            calls.append(dict(request.url.params))
            if "fields" in request.url.params:
                return httpx.Response(400, content=b"unsupported fields")
            return httpx.Response(200, json={"id": "ok", "name": "x"})

        client = NTULearnClient(
            "https://ntulearn.ntu.edu.sg",
            "secret",
            transport=httpx.MockTransport(handler),
        )
        client._test_mode = False  # enable fields defaulting for this method
        try:
            data = await client.get_course("_1_1")
        finally:
            await client.close()
        self.assertEqual(len(calls), 2)
        self.assertIn("fields", calls[0])
        self.assertNotIn("fields", calls[1])
        self.assertEqual(data["name"], "x")

    async def test_fields_defaults_are_applied_in_prod_mode(self) -> None:
        def handler(request: httpx.Request) -> httpx.Response:
            self.assertIn("fields", request.url.params)
            self.assertIn("courseId", request.url.params["fields"])
            return httpx.Response(200, json={"results": [], "paging": {}})

        client = NTULearnClient(
            "https://ntulearn.ntu.edu.sg",
            "secret",
            transport=httpx.MockTransport(handler),
        )
        client._test_mode = False  # enable fields defaulting
        client._cache_enabled = False
        try:
            await client.get_my_enrollments()
        finally:
            await client.close()


class CacheBehaviourTests(unittest.IsolatedAsyncioTestCase):
    async def test_second_call_hits_cache(self) -> None:
        import tempfile
        from ntulearn_mcp.cache import DataCache

        dc = DataCache(cache_dir=tempfile.mkdtemp() + "/c.sqlite3")
        calls = {"n": 0}

        def handler(request: httpx.Request) -> httpx.Response:
            calls["n"] += 1
            return httpx.Response(200, json={"results": [{"courseId": "C"}], "paging": {}})

        client = NTULearnClient(
            "https://ntulearn.ntu.edu.sg",
            "secret",
            transport=httpx.MockTransport(handler),
            data_cache=dc,
        )
        client._cache_enabled = True  # enable caching for the injection test
        try:
            first = await client.get_my_enrollments()
            second = await client.get_my_enrollments()
        finally:
            await client.close()
        self.assertEqual(first, [{"courseId": "C"}])
        self.assertEqual(second, [{"courseId": "C"}])
        self.assertEqual(calls["n"], 1)

    async def test_cache_false_bypasses_cache(self) -> None:
        import tempfile
        from ntulearn_mcp.cache import DataCache

        dc = DataCache(cache_dir=tempfile.mkdtemp() + "/c.sqlite3")
        calls = {"n": 0}

        def handler(request: httpx.Request) -> httpx.Response:
            calls["n"] += 1
            return httpx.Response(200, json={"results": [], "paging": {}})

        client = NTULearnClient(
            "https://ntulearn.ntu.edu.sg",
            "secret",
            transport=httpx.MockTransport(handler),
            data_cache=dc,
        )
        client._cache_enabled = True
        try:
            await client.get_my_enrollments(cache=False)
            await client.get_my_enrollments(cache=False)
        finally:
            await client.close()
        self.assertEqual(calls["n"], 2)

    async def test_explicit_ttl_is_honoured(self) -> None:
        import tempfile
        from ntulearn_mcp.cache import DataCache

        dc = DataCache(cache_dir=tempfile.mkdtemp() + "/c.sqlite3")
        calls = {"n": 0}

        def handler(request: httpx.Request) -> httpx.Response:
            calls["n"] += 1
            return httpx.Response(200, json={"id": "C"})

        client = NTULearnClient(
            "https://ntulearn.ntu.edu.sg",
            "secret",
            transport=httpx.MockTransport(handler),
            data_cache=dc,
        )
        client._cache_enabled = True
        try:
            await client.get_course("_1_1", cache=7.5)
            await client.get_course("_1_1", cache=7.5)
        finally:
            await client.close()
        self.assertEqual(calls["n"], 1)


class NewEndpointTests(unittest.IsolatedAsyncioTestCase):
    """Each new capability method hits the documented endpoint and parses JSON."""

    async def test_get_message_participants_endpoint(self) -> None:
        seen = {}

        def handler(request: httpx.Request) -> httpx.Response:
            seen["url"] = request.url.path
            return httpx.Response(200, json={"results": [{"id": "u1"}], "paging": {}})

        client = NTULearnClient(
            "https://ntulearn.ntu.edu.sg",
            "secret",
            transport=httpx.MockTransport(handler),
        )
        try:
            people = await client.get_message_participants("m1")
        finally:
            await client.close()
        self.assertTrue(seen["url"].endswith("/users/me/messages/m1/participants"))
        self.assertEqual(people, [{"id": "u1"}])

    async def test_get_messages_forwards_filters(self) -> None:
        seen = {}

        def handler(request: httpx.Request) -> httpx.Response:
            seen.update(dict(request.url.params))
            return httpx.Response(200, json={"results": [], "paging": {}})

        client = NTULearnClient(
            "https://ntulearn.ntu.edu.sg",
            "secret",
            transport=httpx.MockTransport(handler),
        )
        try:
            await client.get_messages(folder="sent", unread_only=True, since="2026-01-01T00:00:00Z")
        finally:
            await client.close()
        self.assertEqual(seen["folder"], "sent")
        self.assertEqual(seen["unreadOnly"], "true")
        self.assertEqual(seen["since"], "2026-01-01T00:00:00Z")

    async def test_get_course_users_endpoint(self) -> None:
        seen = {}

        def handler(request: httpx.Request) -> httpx.Response:
            seen["url"] = request.url.path
            return httpx.Response(200, json={"results": [{"id": "u1"}], "paging": {}})

        client = NTULearnClient(
            "https://ntulearn.ntu.edu.sg",
            "secret",
            transport=httpx.MockTransport(handler),
        )
        try:
            users = await client.get_course_users("_1_1")
        finally:
            await client.close()
        self.assertTrue(seen["url"].startswith("/learn/api/public/v1/courses/_1_1/users"))
        self.assertEqual(users, [{"id": "u1"}])

    async def test_get_group_users_endpoint(self) -> None:
        def handler(request: httpx.Request) -> httpx.Response:
            self.assertTrue(
                request.url.path.endswith("/courses/_1_1/groups/g9/users")
            )
            return httpx.Response(200, json={"results": [], "paging": {}})

        client = NTULearnClient(
            "https://ntulearn.ntu.edu.sg",
            "secret",
            transport=httpx.MockTransport(handler),
        )
        try:
            await client.get_group_users("_1_1", "g9")
        finally:
            await client.close()

    async def test_get_gradebook_attempts_endpoint(self) -> None:
        def handler(request: httpx.Request) -> httpx.Response:
            self.assertTrue(
                request.url.path.endswith("/courses/_1_1/gradebook/columns/c9/attempts")
            )
            return httpx.Response(200, json={"results": [{"id": "a1"}], "paging": {}})

        client = NTULearnClient(
            "https://ntulearn.ntu.edu.sg",
            "secret",
            transport=httpx.MockTransport(handler),
        )
        try:
            attempts = await client.get_gradebook_attempts("_1_1", "c9")
        finally:
            await client.close()
        self.assertEqual(attempts, [{"id": "a1"}])

    async def test_get_user_attempts_endpoint(self) -> None:
        def handler(request: httpx.Request) -> httpx.Response:
            self.assertTrue(
                request.url.path.endswith("/courses/_1_1/gradebook/columns/c9/users/u7/attempts")
            )
            return httpx.Response(200, json={"results": [], "paging": {}})

        client = NTULearnClient(
            "https://ntulearn.ntu.edu.sg",
            "secret",
            transport=httpx.MockTransport(handler),
        )
        try:
            await client.get_user_attempts("_1_1", "c9", "u7")
        finally:
            await client.close()

    async def test_get_term_endpoint(self) -> None:
        def handler(request: httpx.Request) -> httpx.Response:
            self.assertTrue(request.url.path.endswith("/terms/t1"))
            return httpx.Response(200, json={"id": "t1", "name": "2026"})

        client = NTULearnClient(
            "https://ntulearn.ntu.edu.sg",
            "secret",
            transport=httpx.MockTransport(handler),
        )
        try:
            term = await client.get_term("t1")
        finally:
            await client.close()
        self.assertEqual(term["name"], "2026")

    async def test_get_course_search_endpoint_and_fallback(self) -> None:
        def failable(request: httpx.Request) -> httpx.Response:
            self.assertIn("search", request.url.params)
            return httpx.Response(400, content=b"search unsupported")

        client = NTULearnClient(
            "https://ntulearn.ntu.edu.sg",
            "secret",
            transport=httpx.MockTransport(failable),
        )
        try:
            results = await client.get_course_search("_1_1", "lab 1")
        finally:
            await client.close()
        # Search unsupported on the server: fall back to empty rather than crash
        self.assertEqual(results, [])
