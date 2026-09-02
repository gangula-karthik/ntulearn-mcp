"""Blackboard Learn REST API client.

Performance and capability layer on top of the plain HTTP client:

* HTTP/2 (via ``h2``) and connection pooling for the authenticated client in
  production; both are disabled automatically when a MockTransport is injected
  (test mode) so mocked tests stay deterministic.
* Retry with exponential backoff on transient GET failures (429, 5xx, network
  errors). 401 never retries: it raises ``BbRouterExpiredError`` immediately.
* orjson parsing when available (disable with ``NTULEARN_JSON=0``).
* ``fields`` trimming with automatic retry-without-fields on HTTP 400/403
  (disable defaulting with ``NTULEARN_FIELDS=0``).
* Transparent method-level TTL caching (disable with ``NTULEARN_CACHE_MODE=off``).
  Cache keys embed a per-user scope, so different users never share entries,
  and a 401 invalidates the whole user's cache before raising.
* Two dozen capability methods: messages, course users/groups, group members,
  gradebook attempts, terms, course search, and more.
"""

from __future__ import annotations

import asyncio
import hashlib
import json
import os
import random
from typing import Any
from urllib.parse import urlsplit, urlunsplit

import httpx


# -- field trimming -----------------------------------------------------------
# Required field lists per endpoint. These are the *floor*: every key the
# server layer strips from a response must be present here so the server
# still has the data to trim. Env NTULEARN_FIELDS=0 disables defaulting.
_FIELDS = {
    "enrollments": "courseId,availability,lastAccessed",
    "course": "id,name,displayName",
    "contents": "id,title,contentHandler,hasChildren,description,modified",
    "calendar": "id,type,title,description,location,start,end,calendarName,dynamicCalendarItemProps",
    "announcements": "id,title,body,created,modified,availability",
    "grade_columns": "id,name,displayName,score,availability,contentId",
    "user_grades": "columnId,score,status,gradingStatus",
    "course_users": "id,userName,name,courseRoleId,availability",
    "groups": "id,name,description,availability",
    "messages": "id,subject,body,created,read,folder,fromUserId",
    "attempts": "id,userId,status,score,cumulatedScore,feedback,created,updated",
    "term": "id,name,startDate,endDate",
}

_RETRYABLE_STATUS = (429, 500, 502, 503, 504)
_MAX_ATTEMPTS_PROD = 3
_NETWORK_RETRYABLE = (
    httpx.TimeoutException,
    httpx.ConnectError,
    httpx.ReadError,
    httpx.RemoteProtocolError,
)

_USER_SCOPE_LEN = 16


def _user_scope(cookie_value: str) -> str:
    return hashlib.sha256(cookie_value.encode("utf-8")).hexdigest()[: _USER_SCOPE_LEN]


class BbRouterExpiredError(Exception):
    """Raised when the server responds with 401, indicating the BbRouter cookie has expired."""

    def __init__(self) -> None:
        super().__init__(
            "Blackboard session cookie has expired (HTTP 401). "
            "Open NTULearn in your browser, copy the new BbRouter cookie value, "
            "update NTULEARN_COOKIE in your .env file, and restart the MCP server."
        )


class BlackboardAPIError(Exception):
    """Raised for non-2xx responses other than 401.

    The message is tuned to be actionable for an LLM caller: it names the
    HTTP class (not_found / forbidden / rate_limited / server_error / other)
    and suggests a next step where one is obvious.
    """

    def __init__(self, status_code: int, body: str, *, path: str | None = None) -> None:
        self.status_code = status_code
        self.body = body
        self.path = path
        super().__init__(_format_api_error(status_code, body, path))


def _format_api_error(status_code: int, body: str, path: str | None) -> str:
    where = f" at {path}" if path else ""
    snippet = body[:300].replace("\n", " ")
    if status_code == 403:
        return (
            f"Blackboard API 403 forbidden{where}. The current user lacks "
            f"access to this resource (course not enrolled, instructor-only "
            f"data, or unavailable). Body: {snippet}"
        )
    if status_code == 404:
        return (
            f"Blackboard API 404 not found{where}. Check the course_id / "
            f"content_id is correct. Body: {snippet}"
        )
    if status_code == 429:
        return (
            f"Blackboard API 429 rate limited{where}. Slow down and retry "
            f"later. Body: {snippet}"
        )
    if 500 <= status_code < 600:
        return (
            f"Blackboard API {status_code} server error{where}. NTULearn "
            f"may be having issues; try again shortly. Body: {snippet}"
        )
    return f"Blackboard API error {status_code}{where}: {snippet}"


class NTULearnClient:
    """Async HTTP client for the Blackboard Learn public REST API."""

    def __init__(
        self,
        base_url: str,
        cookie_value: str,
        *,
        transport: httpx.AsyncBaseTransport | None = None,
        external_transport: httpx.AsyncBaseTransport | None = None,
        data_cache: Any | None = None,
    ) -> None:
        # Strip the "BbRouter=" prefix if the user included it
        if cookie_value.startswith("BbRouter="):
            cookie_value = cookie_value[len("BbRouter="):]

        self._base_url = base_url.rstrip("/")
        self._cookie_value = cookie_value
        # Test mode: any injected transport disables retry/cache/http2/fields
        # so mocked tests stay fully deterministic.
        self._test_mode = transport is not None or external_transport is not None

        self._user_scope = _user_scope(cookie_value)
        self._injected_data_cache = data_cache

        try:
            import orjson  # type: ignore[import-untyped]

            self._orjson = (
                orjson
                if not self._test_mode and _env_flag("NTULEARN_JSON", default=True)
                else None
            )
        except ImportError:
            self._orjson = None

        limits = httpx.Limits(max_connections=32, max_keepalive_connections=16)
        self._client = httpx.AsyncClient(
            base_url=self._base_url,
            headers={
                "Cookie": f"BbRouter={self._cookie_value}",
                "Accept": "application/json",
            },
            timeout=30.0,
            follow_redirects=True,
            transport=transport,
            http2=not self._test_mode and _env_flag("NTULEARN_HTTP2", default=True),
            limits=limits,
        )
        self._external_client = httpx.AsyncClient(
            timeout=30.0,
            follow_redirects=True,
            transport=external_transport,
        )

        self._cache_enabled = not self._test_mode and os.environ.get(
            "NTULEARN_CACHE_MODE", "readwrite"
        ).lower() != "off"

    async def close(self) -> None:
        await self._client.aclose()
        await self._external_client.aclose()

    @property
    def user_scope(self) -> str:
        """Per-user cache scope (16 hex chars). Stable for a given cookie."""
        return self._user_scope

    # ------------------------------------------------------------------
    # Low-level request machinery
    # ------------------------------------------------------------------

    def _default_fields(self, name: str) -> str | None:
        """Return the fields string for a method (None if disabled/test mode)."""
        if self._test_mode:
            return None
        if not _env_flag("NTULEARN_FIELDS", default=True):
            return None
        return _FIELDS.get(name)

    def _parse(self, response: httpx.Response) -> Any:
        if self._orjson is not None:
            return self._orjson.loads(response.content)
        return response.json()

    def _backoff(self, attempt: int) -> float:
        base = 0.25 * (2 ** attempt)
        return base * random.uniform(0.75, 1.25)

    async def _do_get(self, path: str, params: dict[str, Any] | None) -> Any:
        """GET with retry+backoff. 401 raises immediately and never retries."""
        max_attempts = 1 if self._test_mode else _MAX_ATTEMPTS_PROD
        last_status: int | None = None
        last_body = ""
        for attempt in range(max_attempts):
            try:
                response = await self._client.get(path, params=params)
            except _NETWORK_RETRYABLE as e:
                if attempt + 1 < max_attempts:
                    await asyncio.sleep(self._backoff(attempt))
                    continue
                raise BlackboardAPIError(0, f"Network error: {type(e).__name__}", path=path)

            if response.status_code == 401:
                self._invalidate_caches()
                raise BbRouterExpiredError()
            if response.status_code in _RETRYABLE_STATUS:
                last_status = response.status_code
                last_body = response.text
                if attempt + 1 < max_attempts:
                    await asyncio.sleep(self._backoff(attempt))
                    continue
                raise BlackboardAPIError(last_status, last_body, path=path)
            if not response.is_success:
                raise BlackboardAPIError(response.status_code, response.text, path=path)
            return self._parse(response)
        raise BlackboardAPIError(last_status or 0, last_body, path=path)  # unreachable

    async def _get(
        self,
        path: str,
        params: dict[str, Any] | None = None,
        *,
        fields: str | None = None,
    ) -> Any:
        params = dict(params or {})
        if fields:
            params["fields"] = fields
        try:
            return await self._do_get(path, params)
        except BlackboardAPIError as exc:
            # Some endpoints reject specific field names or the fields feature
            # under certain auth scopes. Retry once without the fields param.
            if fields and exc.status_code in (400, 403):
                params.pop("fields", None)
                return await self._do_get(path, params)
            raise

    async def _get_paginated(
        self,
        path: str,
        params: dict[str, Any] | None = None,
        *,
        fields: str | None = None,
    ) -> list[Any]:
        """Follow Blackboard's cursor-based pagination, collecting all results."""
        params = dict(params or {})
        params.setdefault("limit", 200)
        if fields:
            params["fields"] = fields
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

    # ------------------------------------------------------------------
    # Cache helpers
    # ------------------------------------------------------------------

    def _cache_key(self, namespace: str, path: str, params: dict[str, Any] | None = None) -> str:
        canonical = json.dumps(
            sorted((str(k), str(v)) for k, v in (params or {}).items()),
            separators=(",", ":"),
        )
        payload = f"{namespace}|{path}?{canonical}".encode("utf-8")
        digest = hashlib.sha256(payload).hexdigest()
        return f"{self._user_scope}:{digest}"

    def _data_cache(self):
        if not self._cache_enabled:
            return None
        if self._injected_data_cache is not None:
            return self._injected_data_cache
        from ntulearn_mcp.cache import data_cache

        return data_cache()

    async def _with_cache(
        self,
        namespace: str,
        path: str,
        params: dict[str, Any] | None,
        cache: bool | float | None,
        fetch,
    ) -> Any:
        """Run ``fetch()``, honouring the per-method ``cache=`` kwarg."""
        if cache is False:
            return await fetch()
        if cache is not None and cache is not True and float(cache) <= 0:
            return await fetch()
        dc = self._data_cache()
        if dc is None:
            return await fetch()
        from ntulearn_mcp.cache import DEFAULT_TTL_SECONDS

        ttl = DEFAULT_TTL_SECONDS.get(namespace, 300.0) if cache in (None, True) else float(cache)
        if ttl <= 0:
            return await fetch()
        key = self._cache_key(namespace, path, params)
        hit = dc.get(namespace, key, max_age=ttl)
        if hit is not None:
            return hit
        value = await fetch()
        dc.set(namespace, key, value, ttl, user_scope=self._user_scope)
        return value

    def _invalidate_caches(self) -> None:
        """Drop all cache entries for this user (called on 401)."""
        if self._test_mode:
            return
        dc = self._data_cache()
        if dc is None:
            return
        try:
            dc.invalidate_user(self._user_scope)
        except Exception:
            pass  # cache invalidation is best-effort

    # ------------------------------------------------------------------
    # Users
    # ------------------------------------------------------------------

    async def get_my_enrollments(
        self, *, cache: bool | float | None = None
    ) -> list[dict[str, Any]]:
        async def fetch() -> list[dict[str, Any]]:
            return await self._get_paginated(
                "/learn/api/public/v1/users/me/courses",
                fields=self._default_fields("enrollments"),
            )

        return await self._with_cache(
            "get_my_enrollments", "/learn/api/public/v1/users/me/courses", {}, cache, fetch
        )

    async def get_my_user_id(self) -> str:
        data = await self._get("/learn/api/public/v1/users/me")
        return data["id"]

    # ------------------------------------------------------------------
    # Courses
    # ------------------------------------------------------------------

    async def get_course(
        self, course_id: str, *, cache: bool | float | None = None
    ) -> dict[str, Any]:
        async def fetch() -> dict[str, Any]:
            return await self._get(
                f"/learn/api/public/v1/courses/{course_id}",
                fields=self._default_fields("course"),
            )

        path = f"/learn/api/public/v1/courses/{course_id}"
        return await self._with_cache("get_course", path, {}, cache, fetch)

    async def get_courses_batch(
        self,
        course_ids: list[str],
        *,
        cache: bool | float | None = None,
    ) -> list[dict[str, Any]]:
        """Fetch multiple courses concurrently.

        Individual 403/404 errors (private or unavailable courses) are swallowed;
        those courses are returned with just their ID so the caller can still list them.
        """

        async def fetch() -> list[dict[str, Any]]:
            tasks = [self.get_course(cid) for cid in course_ids]
            results = await asyncio.gather(*tasks, return_exceptions=True)
            out: list[dict[str, Any]] = []
            for cid, result in zip(course_ids, results):
                if isinstance(result, Exception):
                    out.append({"id": cid, "name": cid})
                else:
                    out.append(result)
            return out

        return await self._with_cache(
            "get_courses_batch", "/learn/api/public/v1/courses/_batch_", {}, cache, fetch
        )

    async def get_course_search(
        self,
        course_id: str,
        query: str,
        *,
        cache: bool | float | None = None,
    ) -> list[dict[str, Any]]:
        """Search a course's contents. Some deployments reject the search
        parameter; the caller falls back to walking the content tree."""
        path = f"/learn/api/public/v1/courses/{course_id}/contents"
        params = {"search": query}

        async def fetch() -> list[dict[str, Any]]:
            try:
                return await self._get_paginated(
                    path, params, fields=self._default_fields("contents")
                )
            except BlackboardAPIError:
                return []

        return await self._with_cache("get_course_search", path, params, cache, fetch)

    # ------------------------------------------------------------------
    # Contents
    # ------------------------------------------------------------------

    async def get_course_contents(
        self, course_id: str, *, cache: bool | float | None = None
    ) -> list[dict[str, Any]]:
        async def fetch() -> list[dict[str, Any]]:
            return await self._get_paginated(
                f"/learn/api/public/v1/courses/{course_id}/contents",
                fields=self._default_fields("contents"),
            )

        path = f"/learn/api/public/v1/courses/{course_id}/contents"
        return await self._with_cache("get_course_contents", path, {}, cache, fetch)

    async def get_content_children(
        self,
        course_id: str,
        content_id: str,
        *,
        cache: bool | float | None = None,
    ) -> list[dict[str, Any]]:
        async def fetch() -> list[dict[str, Any]]:
            return await self._get_paginated(
                f"/learn/api/public/v1/courses/{course_id}/contents/{content_id}/children",
                fields=self._default_fields("contents"),
            )

        path = f"/learn/api/public/v1/courses/{course_id}/contents/{content_id}/children"
        return await self._with_cache("get_content_children", path, {}, cache, fetch)

    async def get_content_item(
        self, course_id: str, content_id: str, *, cache: bool | float | None = None
    ) -> dict[str, Any]:
        async def fetch() -> dict[str, Any]:
            return await self._get(
                f"/learn/api/public/v1/courses/{course_id}/contents/{content_id}",
                fields=self._default_fields("contents"),
            )

        path = f"/learn/api/public/v1/courses/{course_id}/contents/{content_id}"
        return await self._with_cache("get_content_item", path, {}, cache, fetch)

    async def get_attachments(
        self, course_id: str, content_id: str, *, cache: bool | float | None = None
    ) -> list[dict[str, Any]]:
        """Return attachment metadata for a content item (resource/x-bb-file items)."""
        async def fetch() -> list[dict[str, Any]]:
            return await self._get_paginated(
                f"/learn/api/public/v1/courses/{course_id}/contents/{content_id}/attachments"
            )

        path = f"/learn/api/public/v1/courses/{course_id}/contents/{content_id}/attachments"
        return await self._with_cache("get_attachments", path, {}, cache, fetch)

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
            self._invalidate_caches()
            raise BbRouterExpiredError()
        if response.status_code in (301, 302, 303, 307, 308):
            location = response.headers.get("location")
            if location:
                return location
        if response.is_success:
            # Some versions return the file directly
            return path  # caller will download via _client
        raise BlackboardAPIError(response.status_code, response.text, path=path)

    # ------------------------------------------------------------------
    # Messages
    # ------------------------------------------------------------------

    async def get_messages(
        self,
        *,
        folder: str | None = None,
        unread_only: bool = False,
        since: str | None = None,
        cache: bool | float | None = None,
    ) -> list[dict[str, Any]]:
        """Messages in the user's mailbox (default: inbox).

        ``folder`` may be a mailbox folder name; ``since`` an ISO-8601
        timestamp to bound the window; ``unread_only`` filters to unread.
        """
        path = "/learn/api/public/v1/users/me/messages"
        params: dict[str, Any] = {}
        if folder:
            params["folder"] = folder
        if unread_only:
            params["unreadOnly"] = "true"
        if since:
            params["since"] = since

        async def fetch() -> list[dict[str, Any]]:
            return await self._get_paginated(path, params, fields=self._default_fields("messages"))

        return await self._with_cache("get_messages", path, params, cache, fetch)

    async def get_message(
        self, message_id: str, *, cache: bool | float | None = None
    ) -> dict[str, Any]:
        path = f"/learn/api/public/v1/users/me/messages/{message_id}"

        async def fetch() -> dict[str, Any]:
            return await self._get(path, fields=self._default_fields("messages"))

        return await self._with_cache("get_message", path, {}, cache, fetch)

    async def get_message_participants(
        self, message_id: str, *, cache: bool | float | None = None
    ) -> list[dict[str, Any]]:
        path = f"/learn/api/public/v1/users/me/messages/{message_id}/participants"

        async def fetch() -> list[dict[str, Any]]:
            return await self._get_paginated(path, fields=self._default_fields("course_users"))

        return await self._with_cache("get_message_participants", path, {}, cache, fetch)

    # ------------------------------------------------------------------
    # Course users & groups
    # ------------------------------------------------------------------

    async def get_course_users(
        self, course_id: str, *, cache: bool | float | None = None
    ) -> list[dict[str, Any]]:
        path = f"/learn/api/public/v1/courses/{course_id}/users"

        async def fetch() -> list[dict[str, Any]]:
            return await self._get_paginated(path, fields=self._default_fields("course_users"))

        return await self._with_cache("get_course_users", path, {}, cache, fetch)

    async def get_course_groups(
        self, course_id: str, *, cache: bool | float | None = None
    ) -> list[dict[str, Any]]:
        path = f"/learn/api/public/v1/courses/{course_id}/groups"

        async def fetch() -> list[dict[str, Any]]:
            return await self._get_paginated(path, fields=self._default_fields("groups"))

        return await self._with_cache("get_course_groups", path, {}, cache, fetch)

    async def get_group_users(
        self, course_id: str, group_id: str, *, cache: bool | float | None = None
    ) -> list[dict[str, Any]]:
        path = f"/learn/api/public/v1/courses/{course_id}/groups/{group_id}/users"

        async def fetch() -> list[dict[str, Any]]:
            return await self._get_paginated(path, fields=self._default_fields("course_users"))

        return await self._with_cache("get_group_users", path, {}, cache, fetch)

    # ------------------------------------------------------------------
    # Gradebook
    # ------------------------------------------------------------------

    async def get_gradebook_columns(
        self, course_id: str, *, cache: bool | float | None = None
    ) -> list[dict[str, Any]]:
        async def fetch() -> list[dict[str, Any]]:
            return await self._get_paginated(
                f"/learn/api/public/v1/courses/{course_id}/gradebook/columns",
                fields=self._default_fields("grade_columns"),
            )

        path = f"/learn/api/public/v1/courses/{course_id}/gradebook/columns"
        return await self._with_cache("get_gradebook_columns", path, {}, cache, fetch)

    async def get_user_grades(
        self, course_id: str, user_id: str, *, cache: bool | float | None = None
    ) -> list[dict[str, Any]]:
        async def fetch() -> list[dict[str, Any]]:
            return await self._get_paginated(
                f"/learn/api/public/v1/courses/{course_id}/gradebook/users/{user_id}",
                fields=self._default_fields("user_grades"),
            )

        path = f"/learn/api/public/v1/courses/{course_id}/gradebook/users/{user_id}"
        return await self._with_cache("get_user_grades", path, {}, cache, fetch)

    async def get_gradebook_attempts(
        self, course_id: str, column_id: str, *, cache: bool | float | None = None
    ) -> list[dict[str, Any]]:
        path = f"/learn/api/public/v1/courses/{course_id}/gradebook/columns/{column_id}/attempts"

        async def fetch() -> list[dict[str, Any]]:
            return await self._get_paginated(path, fields=self._default_fields("attempts"))

        return await self._with_cache("get_gradebook_attempts", path, {}, cache, fetch)

    async def get_user_attempts(
        self,
        course_id: str,
        column_id: str,
        user_id: str,
        *,
        cache: bool | float | None = None,
    ) -> list[dict[str, Any]]:
        path = (
            f"/learn/api/public/v1/courses/{course_id}/gradebook/columns/{column_id}"
            f"/users/{user_id}/attempts"
        )

        async def fetch() -> list[dict[str, Any]]:
            return await self._get_paginated(path, fields=self._default_fields("attempts"))

        return await self._with_cache("get_user_attempts", path, {}, cache, fetch)

    # ------------------------------------------------------------------
    # Terms
    # ------------------------------------------------------------------

    async def get_term(
        self, term_id: str, *, cache: bool | float | None = None
    ) -> dict[str, Any]:
        path = f"/learn/api/public/v1/terms/{term_id}"

        async def fetch() -> dict[str, Any]:
            return await self._get(path, fields=self._default_fields("term"))

        return await self._with_cache("get_term", path, {}, cache, fetch)

    # ------------------------------------------------------------------
    # Announcements
    # ------------------------------------------------------------------

    async def get_announcements(
        self, course_id: str, *, cache: bool | float | None = None
    ) -> list[dict[str, Any]]:
        async def fetch() -> list[dict[str, Any]]:
            return await self._get_paginated(
                f"/learn/api/public/v1/courses/{course_id}/announcements",
                fields=self._default_fields("announcements"),
            )

        path = f"/learn/api/public/v1/courses/{course_id}/announcements"
        return await self._with_cache("get_announcements", path, {}, cache, fetch)

    # ------------------------------------------------------------------
    # Calendar
    # ------------------------------------------------------------------

    async def get_calendar_items(
        self,
        *,
        course_id: str | None = None,
        since: str | None = None,
        until: str | None = None,
        item_type: str | None = None,
        cache: bool | float | None = None,
    ) -> list[dict[str, Any]]:
        """Fetch calendar items, optionally scoped to a course.

        Wraps GET /learn/api/public/v1/calendars/items. Note Anthology's
        docs warn that unscoped calls (no courseId) under non-3LO auth can
        attempt to dump the entire institution calendar — server layer
        defaults to fanning out across the user's enrolled courses rather
        than relying on the unscoped path.

        Assignment due dates surface as items with type='GradebookColumn'.
        """
        path = "/learn/api/public/v1/calendars/items"
        params: dict[str, Any] = {}
        if course_id is not None:
            params["courseId"] = course_id
        if since is not None:
            params["since"] = since
        if until is not None:
            params["until"] = until
        if item_type is not None:
            params["type"] = item_type

        async def fetch() -> list[dict[str, Any]]:
            return await self._get_paginated(path, params, fields=self._default_fields("calendar"))

        return await self._with_cache("get_calendar_items", path, params, cache, fetch)

    # ------------------------------------------------------------------
    # File download
    # ------------------------------------------------------------------

    async def download_bytes(self, url: str) -> tuple[bytes, str | None]:
        """Download a file URL, returning (content_bytes, content_type).

        Same-origin URLs use the authenticated NTULearn client. Allowed
        Blackboard CDN URLs use a separate cookie-free client.
        """
        response = await self._download_response(url)

        if response.status_code == 401:
            raise BbRouterExpiredError()
        if not response.is_success:
            raise BlackboardAPIError(response.status_code, response.text, path=url)

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


def _env_flag(name: str, *, default: bool) -> bool:
    value = os.environ.get(name, "")
    if not value:
        return default
    return value.strip().lower() not in ("0", "false", "no", "off")
