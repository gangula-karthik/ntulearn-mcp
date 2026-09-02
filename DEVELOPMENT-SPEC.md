# ntulearn-mcp v0.3 — Parallel build spec (do not ship; delete before release)

Three parallel work trees build against this shared contract. Each work tree
owns disjoint files. Merge on `main`, then run the FULL test suite and fix.

## Work tree ownership (disjoint file sets)

- **WT-A `perf-client`**: `src/ntulearn_mcp/client.py`, `src/ntulearn_mcp/cache.py`,
  `src/ntulearn_mcp/cookie.py`, `tests/test_client.py`, `tests/test_cache.py`, `tests/test_cookie.py`
- **WT-B `capabilities`**: `src/ntulearn_mcp/common.py`, `src/ntulearn_mcp/handlers.py`,
  `src/ntulearn_mcp/render.py`, `tests/test_handlers.py` (new files only)
- **WT-C `server`**: `src/ntulearn_mcp/server.py`, `tests/test_server.py`, `tests/test_fixes.py`,
  `manifest.json`, `README.md`, `pyproject.toml` (version bump 0.2.0 -> 0.3.0 ONLY;
  dependencies were already added on main by `uv add h2 orjson lxml`)

## Non-negotiable rules

1. Keep every existing public/module name + signature in `client.py`/`cache.py`/`cookie.py`/
   `server.py` that the existing tests call. The existing 116 tests must stay green after merge.
2. `handlers.py`/`common.py`/`render.py` MUST NOT import `server.py` (no cycle). They may import
   `ntulearn_mcp.client`, `ntulearn_mcp.parsers`, `ntulearn_mcp.cache` (guarded, see below), stdlib.
3. Test inside each work tree using the **main tree's venv** + PYTHONPATH override:
   `PYTHONPATH=<worktree>/src <mainrepo>/.venv/bin/python -m unittest discover -s <worktree>/tests`
   (existing venv already has mcp/httpx/pymupdf + h2/orjson/lxml). Do NOT run the other
   trees' tests. Keep your own tree's tests green and **add tests for all new behavior**.
4. Python 3.12. No new dependencies beyond h2/orjson/lxml. Optional heavy imports stay lazy.
5. All new tool handlers return `(blocks, payload)` where blocks is
   `list[TextContent]` (or with ImageContent) and payload is a JSON-serialisable dict.
6. Read-only by default. The only write tools are `ntulearn_download_file` (existing) and
   `ntulearn_download_course` (new). Everything else readOnlyHint=True.

## Client API contract (WT-A implements; WT-B/WT-C rely on it)

### Existing signatures to preserve exactly (don't change names/positional behaviour)
get_my_enrollments() / get_my_user_id() / get_course(course_id) / get_courses_batch(course_ids) /
get_course_contents(course_id) / get_content_children(course_id, content_id) /
get_content_item(course_id, content_id) / get_attachments(course_id, content_id) /
get_attachment_download_url(course_id, content_id, attachment_id) / get_announcements(course_id) /
get_calendar_items(*, course_id=None, since=None, until=None, item_type=None) /
get_gradebook_columns(course_id) / get_user_grades(course_id, user_id) /
download_bytes(url) / _download_response(url) / close().  Classes BbRouterExpiredError,
BlackboardAPIError (_format_api_error) unchanged.

### New client methods (all async, keyword-only new params, return parsed JSON)
- `get_messages(*, folder: str = "inbox", unread_only: bool = False, since: str | None = None) -> list[dict]`
  GET /learn/api/public/v1/users/me/messages (paginated full list).
- `get_message(message_id: str) -> dict`  GET /learn/api/public/v1/users/me/messages/{id}
- `get_message_participants(message_id: str) -> list[dict]`  GET .../messages/{id}/participants
- `get_course_users(course_id: str) -> list[dict]`  GET /courses/{id}/users (paginated)
- `get_course_groups(course_id: str) -> list[dict]`  GET /courses/{id}/groups (paginated)
- `get_group_users(course_id: str, group_id: str) -> list[dict]`  GET /courses/{id}/groups/{gid}/users
- `get_gradebook_attempts(course_id: str, column_id: str) -> list[dict]`  GET /courses/{id}/gradebook/columns/{colId}/attempts
- `get_user_attempts(course_id: str, column_id: str, user_id: str) -> list[dict]`  GET .../columns/{colId}/users/{userId}/attempts
- `get_term(term_id: str) -> dict`  GET /terms/{id}
- `get_course_search(course_id: str, query: str) -> list[dict]`  GET /courses/{id}/contents?search={query} (may return {} — handler falls back to walking)

### Performance features (WT-A, client.py)
- **Test/diagnostic mode**: when the client is constructed with `transport=` or `external_transport=`
  injected (MockTransport), ALL of retry/cache/http2/fields are disabled. This keeps the existing
  mocked tests deterministic. Prod mode = no injected transport.
- **HTTP/2**: `httpx.AsyncClient(http2=True, limits=...` in prod mode. Env `NTULEARN_HTTP2=0` disables.
- **Retry with backoff**: wrap GETs. Retry on 429, 5xx, httpx.TimeoutException/ConnectError/ReadError/
  RemoteProtocolError. 3 attempts, base delay 0.25s, factor 2, jitter +/-25%; sleep via `asyncio.sleep`.
  NEVER retry 401 (always raise BbRouterExpiredError immediately). After retries exhaust, raise the last
  error. Keep error message contract (429 -> "rate limited..." etc. stays via BlackboardAPIError).
- **Transparent method-level TTL cache** (through cache.py data API, see below):
  cached methods and default TTLs:
  get_my_enrollments=1800s, get_course=3600, get_courses_batch=3600,
  get_course_contents=3600, get_content_children=3600, get_content_item=3600,
  get_announcements=600, get_calendar_items=300, get_gradebook_columns=600,
  get_user_grades=300, get_messages=60, get_message=600, get_message_participants=600,
  get_course_users=1800, get_course_groups=1800, get_group_users=1800,
  get_gradebook_attempts=300, get_user_attempts=300, get_term=3600, get_course_search=600.
  Add keyword-only `cache: bool | float | None = None` to each cached method: None=use default TTL,
  True=default TTL, False=bypass, number=explicit seconds. Cache key = sha256(user_scope + url + params),
  user_scope = first 16 chars sha256 of cookie value. On 401 / refresh, invalidate that user scope
  (cache.invalidate_data_user). Cache must be skipped for non-cacheable responses (e.g. paging >1 page
  is fine — cache only the FINAL assembled result list).
- **`fields` trimming** (prod mode): add optional `fields: list[str] | None = None` to `_get`/`_get_paginated`;
  when set, append `fields=`. Set safe default field lists on these methods (must include every key the
  server strips, listed in REQUIRED FIELDS below). If the response is HTTP 400/403 AND fields were sent,
  retry the request once WITHOUT fields (server-specific rejection safety).
  REQUIRED FIELDS (server strip functions): contents-> id,title,contentHandler,hasChildren,description,modified;
  calendar-> id,type,title,description,location,start,end,calendarName,dynamicCalendarItemProps;
  announcements-> id,title,body,created,modified,availability;
  gradebook columns-> id,name,displayName,score,availability,contentId;
  enrollments-> courseId,availability,lastAccessed; course detail-> id,name,displayName;
  course users-> id,name,familyName,givenName,role; groups-> id,name,description,availability;
  messages-> id,subject,body,created,read,folder,fromUserId; attempts-> id,userId,status,score,feedback, createdAt, cumulatedScore.
  Env `NTULEARN_FIELDS=0` disables fields defaulting globally.
- **orjson** for parsing prod responses when available (fall back to stdlib json). Env `NTULEARN_JSON=0`.

## Cache data API (WT-A implements in cache.py; the existing cookie-cache functions stay as-is)

- `data_cache() -> "DataCache"` module-level singleton (lazy init).
- class `DataCache`: `get(namespace, key, *, max_age=None) -> Any | None`,
  `set(namespace, key, value, ttl) -> None`, `delete(namespace, key) -> None`,
  `invalidate_user(user_scope) -> None`, `clear() -> None`.
- Backends: in-memory LRU (OrderedDict, max ~4096 entries) + optional SQLite persistence under
  `cache_dir` (env `NTULEARN_CACHE_DIR`, default `~/.cache/ntulearn-mcp/cache.sqlite3`; on mac) .
  SQLite stores JSON strings with (namespace, key, expires_at, user_scope). Writes best-effort
  (failures degrade to memory-only, never raise).
- `DEFAULT_TTL_SECONDS: dict[str, float]` mirroring the client table above.
- Env `NTULEARN_CACHE_MODE`: `readwrite` (default), `readonly`, or `off` -> no-op methods.
- Values must be JSON-serialisable (list/dict); non-serialisable -> don't cache.
- IMPORTANT: when a custom transport is injected (test mode) the client never calls the cache, but the
  cache module itself must stay unit-testable with an injected temp dir (monkeypatch cache_dir).

## Handler contract (WT-B implements; WT-C wires)

`common.py` provides pure helpers (mirror of server.py's, so server.py is untouched):
- `emit(payload, text=None) -> tuple[list[TextContent], dict]` (same shape as server._emit)
- `resolve_pagination_args(args) -> (offset, limit)` (default _DEFAULT_LIMIT=50, clamp 1..200)
- `resolve_response_format(args) -> bool` (json default)
- `slice_with_pagination(items, offset, limit) -> (page, meta)` (mirror _slice_with_pagination)
- `validate_iso8601(value, *, name) -> str` (mirror server._validate_iso8601)
- `now_iso() -> str` (UTC ISO with Z)
- `fan_out_course_ids(client, course_ids_arg) -> list[str]` : if arg omitted/empty -> enrolled available
  course ids; validates list of strings.
- `strip_html(value) -> str`, `sanitize_filename(name)`, `deduplicate_filename(name, used, dest_dir)`,
  `classify_kind(filename, content_type)` (mirror logic in server/parsers).
- `tracker_get_last_seen() / tracker_set_last_seen(iso) -> None` : try `ntulearn_mcp.cache.data_cache()`
  namespace "tracker"; if import or cache fails fall back to an in-module dict. Tests monkeypatch these.

handlers.py — one async function per tool:
`async def handle_<tool>(client, args) -> tuple[list[TextContent|ImageContent], dict]`
1. handle_list_messages(client, args): folder, unread_only, since, limit/offset, response_format.
   Output: messages[], + pagination meta.
2. handle_read_message(client, args): message_id (required). Output: message{} with id, subject,
   body(text), senderName, senderId, createdAt, read, recipients[].
3. handle_list_course_users(client, args): course_id required, limit/offset, response_format.
   Output: users[] with id, name, role, userName?; + meta.
4. handle_list_course_groups(client, args): course_id required, limit/offset, response_format.
   Output: groups[] id,name,description,available; + meta.
5. handle_get_group_members(client, args): course_id, group_id required. Output: users[].
6. handle_get_gradebook_attempts(client, args): course_id, column_id required, user_id optional.
   Output: attempts[] id,userId,userName?,status,score,feedback,cumulatedScore,createdAt; + meta.
7. handle_search_all_courses(client, args): query required, course_ids?, max_depth (default 3, cap 10),
   max_results (default 50, cap 200), response_format. Output: matches[] with courseId + breadcrumb[] +
   title + id; count.
8. handle_get_content_tree(client, args): course_id required, max_depth (default 5 cap 10),
   response_format. Output: tree:{id,title,hasChildren,children:[]}, count, totalNodes.
9. handle_download_course(client, args): course_id required, destination_dir (required-ish, default
   DOWNLOAD-like "~/Downloads/NTU/<course>"), max_depth, include_extensions? (csv of extensions,
   omit=all), skip_existing bool default true, parallel int default 4 (1..16), response_format.
   Output: files[] localPath,filename,sizeBytes,courseFolder; skipped[]; totalBytes. WRITE tool
   (destructiveHint False). Uses client.get_content_item/get_attachments/get_attachment_download_url/
   download_bytes + parsers.extract_all_files + common helpers. Concurrency via asyncio.Semaphore.
10. handle_whats_new(client, args): course_ids?, since (optional ISO; default = tracker last seen,
    fallback now-7d), update_tracker bool default False, response_format. Output:
    {since, announcements[], upcoming[], gradebookSummary?, fetchedAt}. Marks tracker if update_tracker.
11. handle_export_calendar_ics(client, args): course_ids?, since?, until?, response_format.
    Output: ics (string), itemCount, supported:true. Build minimal ICS (BEGIN:VCALENDAR ... ).
12. handle_export_gradebook_csv(client, args): course_ids?, response_format.
    Output: csv (string), columnCount, courseCount. Header: courseId,column,possible,score,grade,status.
13. handle_summarize_course(client, args): course_id required, include_contents bool default true,
    response_format. Output: courseId,title,description?,instructors[],enrollmentCount?,
    upcoming[],recentAnnouncements[],gradeSummary?{columnCount,columnsWithScore,possibleTotal},
    contentTopFolders[]. Each sub-section degrades gracefully on error/courseErrors.

All cross-course handlers: fan out via fan_out_course_ids using asyncio.gather(return_exceptions=True);
re-raise BbRouterExpiredError; collect per-course errors into courseErrors dict in payload (for list-style
tools that support course_ids). Markdown renderers live in render.py (one per tool, names md_<tool>).

### render.py — md_<tool>(payload, ...) functions returning markdown strings (match existing md style).

### tests/test_handlers.py — Fake clients (fake cap of the new client methods) + >= one test per handler
covering happy path, pagination clamps, and cross-course error handling.

## Server integration (WT-C implements)

1. Register the 13 new tools in `list_tools()` with full inputSchema/outputSchema/annotations
   (title, readOnlyHint, destructiveHint, idempotentHint, openWorldHint), ntulearn_ prefix, pagination
   + response_format where a list is returned. Reuse existing schema fragments in server.py
   (_COURSE_ID_SCHEMA, _CONTENT_ID_SCHEMA, _LIMIT_SCHEMA, _OFFSET_SCHEMA, _RESPONSE_FORMAT_SCHEMA,
   _PAGINATION_OUTPUT_FIELDS, _BB_ID_PATTERN).
2. `_dispatch` gains the 13 handlers by importing `from ntulearn_mcp import handlers` (name mapping
   `ntulearn_<x>` -> `handlers.handle_<x>`).
3. Update `tests/test_fixes.py` `test_only_download_file_is_non_read_only` to expect
   `["ntulearn_download_file", "ntulearn_download_course"]` (contract intentionally extended).
4. Add MCP **resources**: `@app.list_resources` / `@app.read_resource` for URI
   `ntulearn://courses/{course_id}` returning JSON course briefing (build by calling
   handlers.handle_summarize_course(client, {"course_id": cid, "response_format": "json"}) and embedding
   its payload as resource content text/JSON; missing course -> clear error). Also `@app.list_resource_templates`
   exposing the `{course_id}` template. Read of a template URI pattern served by read_resource.
5. Add MCP **prompts**: `@app.list_prompts` / `@app.get_prompt`:
   - `ntulearn-weekly-brief`: args courses (optional string), days (int default 7) -> prompt text that
     chains ntulearn_get_upcoming + ntulearn_get_announcements with computed since/until.
   - `ntulearn-assignment-triage`: args courses (optional), days (default 14) -> prompt for due-date triage.
6. Version bump to 0.3.0 in pyproject.toml + manifest.json (+ any __version__). Dependencies stay as
   already-added on main (h2/orjson/lxml).
7. README: new tools table rows, cache fields/http2 env-var docs, resources/prompts section,
   bump "8 tools" -> "21 tools".
8. Keep test_server.py/main existing tests green; add integration tests only if quick (optional).
