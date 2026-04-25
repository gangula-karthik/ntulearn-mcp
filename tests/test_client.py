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


if __name__ == "__main__":
    unittest.main()
