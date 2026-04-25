"""Blackboard Learn REST API client."""

from __future__ import annotations

import asyncio
from typing import Any
from urllib.parse import urlsplit, urlunsplit

import httpx


class BbRouterExpiredError(Exception):
    """Raised when the server responds with 401, indicating the BbRouter cookie has expired."""

    def __init__(self) -> None:
        super().__init__(
            "Blackboard session cookie has expired (HTTP 401). "
            "Open NTULearn in your browser, copy the new BbRouter cookie value, "
            "update NTULEARN_COOKIE in your .env file, and restart the MCP server."
        )


class BlackboardAPIError(Exception):
    """Raised for non-2xx responses other than 401."""

    def __init__(self, status_code: int, body: str) -> None:
        self.status_code = status_code
        self.body = body
        super().__init__(f"Blackboard API error {status_code}: {body[:500]}")


class NTULearnClient:
    """Async HTTP client for the Blackboard Learn public REST API."""

    def __init__(
        self,
        base_url: str,
        cookie_value: str,
        *,
        transport: httpx.AsyncBaseTransport | None = None,
        external_transport: httpx.AsyncBaseTransport | None = None,
    ) -> None:
        # Strip the "BbRouter=" prefix if the user included it
        if cookie_value.startswith("BbRouter="):
            cookie_value = cookie_value[len("BbRouter="):]

        self._base_url = base_url.rstrip("/")
        self._cookie_value = cookie_value
        self._client = httpx.AsyncClient(
            base_url=self._base_url,
            headers={
                "Cookie": f"BbRouter={self._cookie_value}",
                "Accept": "application/json",
            },
            timeout=30.0,
            follow_redirects=True,
            transport=transport,
        )
        self._external_client = httpx.AsyncClient(
            timeout=30.0,
            follow_redirects=True,
            transport=external_transport,
        )

    async def close(self) -> None:
        await self._client.aclose()
        await self._external_client.aclose()

    async def _get(self, path: str, params: dict[str, Any] | None = None) -> Any:
        response = await self._client.get(path, params=params)
        if response.status_code == 401:
            raise BbRouterExpiredError()
        if not response.is_success:
            raise BlackboardAPIError(response.status_code, response.text)
        return response.json()

    async def _get_paginated(self, path: str, params: dict[str, Any] | None = None) -> list[Any]:
        """Follow Blackboard's cursor-based pagination, collecting all results."""
        params = dict(params or {})
        params.setdefault("limit", 200)
        results: list[Any] = []

        while True:
            data = await self._get(path, params)
            results.extend(data.get("results", []))
            paging = data.get("paging", {})
            next_page = paging.get("nextPage")
            if not next_page:
                break
            # nextPage is a full path like /learn/api/public/v1/...
            # Strip the base URL prefix if present
            if next_page.startswith(self._base_url):
                next_page = next_page[len(self._base_url):]
            path = next_page
            params = {}  # cursor already embedded in the path

        return results

    # -------------------------------------------------------------------------
    # Users
    # -------------------------------------------------------------------------

    async def get_my_enrollments(self) -> list[dict[str, Any]]:
        return await self._get_paginated("/learn/api/public/v1/users/me/courses")

    async def get_my_user_id(self) -> str:
        data = await self._get("/learn/api/public/v1/users/me")
        return data["id"]

    # -------------------------------------------------------------------------
    # Courses
    # -------------------------------------------------------------------------

    async def get_course(self, course_id: str) -> dict[str, Any]:
        return await self._get(f"/learn/api/public/v1/courses/{course_id}")

    async def get_courses_batch(self, course_ids: list[str]) -> list[dict[str, Any]]:
        """Fetch multiple courses concurrently.

        Individual 403/404 errors (private or unavailable courses) are swallowed;
        those courses are returned with just their ID so the caller can still list them.
        """
        tasks = [self.get_course(cid) for cid in course_ids]
        results = await asyncio.gather(*tasks, return_exceptions=True)
        out = []
        for cid, result in zip(course_ids, results):
            if isinstance(result, Exception):
                out.append({"id": cid, "name": cid})
            else:
                out.append(result)
        return out

    # -------------------------------------------------------------------------
    # Contents
    # -------------------------------------------------------------------------

    async def get_course_contents(self, course_id: str) -> list[dict[str, Any]]:
        return await self._get_paginated(f"/learn/api/public/v1/courses/{course_id}/contents")

    async def get_content_children(self, course_id: str, content_id: str) -> list[dict[str, Any]]:
        return await self._get_paginated(
            f"/learn/api/public/v1/courses/{course_id}/contents/{content_id}/children"
        )

    async def get_content_item(self, course_id: str, content_id: str) -> dict[str, Any]:
        return await self._get(
            f"/learn/api/public/v1/courses/{course_id}/contents/{content_id}"
        )

    async def get_attachments(self, course_id: str, content_id: str) -> list[dict[str, Any]]:
        """Return attachment metadata for a content item (resource/x-bb-file items)."""
        return await self._get_paginated(
            f"/learn/api/public/v1/courses/{course_id}/contents/{content_id}/attachments"
        )

    async def get_attachment_download_url(
        self, course_id: str, content_id: str, attachment_id: str
    ) -> str:
        """Return the signed download URL for an attachment.

        Calls the download endpoint, which responds with a 302 redirect to a
        pre-signed bbcswebdav URL. Returns the Location header value.
        """
        path = (
            f"/learn/api/public/v1/courses/{course_id}/contents/{content_id}"
            f"/attachments/{attachment_id}/download"
        )
        response = await self._client.get(path, follow_redirects=False)
        if response.status_code == 401:
            raise BbRouterExpiredError()
        if response.status_code in (301, 302, 303, 307, 308):
            location = response.headers.get("location")
            if location:
                return location
        if response.is_success:
            # Some versions return the file directly
            return path  # caller will download via _client
        raise BlackboardAPIError(response.status_code, response.text)

    # -------------------------------------------------------------------------
    # Announcements
    # -------------------------------------------------------------------------

    async def get_announcements(self, course_id: str) -> list[dict[str, Any]]:
        return await self._get_paginated(f"/learn/api/public/v1/courses/{course_id}/announcements")

    # -------------------------------------------------------------------------
    # Gradebook
    # -------------------------------------------------------------------------

    async def get_gradebook_columns(self, course_id: str) -> list[dict[str, Any]]:
        return await self._get_paginated(
            f"/learn/api/public/v1/courses/{course_id}/gradebook/columns"
        )

    async def get_user_grades(self, course_id: str, user_id: str) -> list[dict[str, Any]]:
        return await self._get_paginated(
            f"/learn/api/public/v1/courses/{course_id}/gradebook/users/{user_id}"
        )

    # -------------------------------------------------------------------------
    # File download
    # -------------------------------------------------------------------------

    async def download_bytes(self, url: str) -> tuple[bytes, str | None]:
        """Download a file URL, returning (content_bytes, content_type).

        Same-origin URLs use the authenticated NTULearn client. Allowed
        Blackboard CDN URLs use a separate cookie-free client.
        """
        response = await self._download_response(url)

        if response.status_code == 401:
            raise BbRouterExpiredError()
        if not response.is_success:
            raise BlackboardAPIError(response.status_code, response.text)

        content_type = response.headers.get("content-type")
        return response.content, content_type

    async def _download_response(self, url: str) -> httpx.Response:
        parsed = urlsplit(url)
        if not parsed.scheme and not parsed.netloc:
            return await self._client.get(url)

        if parsed.scheme not in ("http", "https"):
            raise ValueError(f"Unsafe download URL scheme: {parsed.scheme}")

        base = urlsplit(self._base_url)
        if (
            parsed.scheme == base.scheme
            and parsed.hostname == base.hostname
            and (parsed.port or _default_port(parsed.scheme))
            == (base.port or _default_port(base.scheme))
        ):
            path = urlunsplit(("", "", parsed.path or "/", parsed.query, ""))
            return await self._client.get(path)

        host = parsed.hostname or ""
        if host.endswith(".blackboard.com"):
            return await self._external_client.get(url)

        raise ValueError(f"Unsafe download URL host: {host}")


def _default_port(scheme: str) -> int | None:
    if scheme == "http":
        return 80
    if scheme == "https":
        return 443
    return None
