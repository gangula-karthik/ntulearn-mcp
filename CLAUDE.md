# CLAUDE.md

Context for AI assistants resuming work on this repo. Captures the **decisions and constraints** that aren't visible in the code or git history. For *what* the code does, read the code; for *why*, read this.

## Project

`ntulearn-mcp` — Python MCP server wrapping the Blackboard Learn REST API for NTULearn (NTU Singapore's LMS). Lets MCP hosts (Claude Desktop, Cursor, Cline, Claude Code, etc.) interact with courses, content, downloads, announcements, and grades.

Source layout in [src/ntulearn_mcp/](src/ntulearn_mcp/):
- [server.py](src/ntulearn_mcp/server.py) — MCP entrypoint, tool handlers, cookie resolution, 401-retry wrapper
- [client.py](src/ntulearn_mcp/client.py) — async httpx-based Blackboard REST client
- [cookie.py](src/ntulearn_mcp/cookie.py) — browser cookie auto-read with bounded retry/backoff
- [cache.py](src/ntulearn_mcp/cache.py) — last-known-good cookie persisted to OS keychain via `keyring`
- [parsers.py](src/ntulearn_mcp/parsers.py) — HTML body → download URL extraction

## Audience and distribution decision

**Target audience:** tech-inclined NTU students reached via LinkedIn + GitHub. Not the general student population — anyone who can install `uv` and edit a JSON config file.

**Distribution path:** publish to PyPI as `ntulearn-mcp`; users invoke via `uvx ntulearn-mcp` from their MCP host's config. **Not yet published as of this session.** README now leads with the `uvx` flow as of v0.2.0.

**Explicitly rejected paths and why:**
- *AI-free product (Chrome extension dashboard, etc.):* user judged the deterministic value not high enough to compete with just opening NTULearn.
- *Hosted web app / Telegram bot:* requires custodian of N students' Blackboard sessions — privacy/policy nightmare. NTU has no public OAuth flow for student apps.
- *`.mcpb` Desktop Extensions:* nicer UX (single file, double-click install, OS-keychain config) but bundles Node.js, not Python — would mean either a Node-wraps-Python adapter or a second distribution artifact. Skipped for v1; revisit if `uvx` friction proves too high.
- *Scripted SSO login:* dead because of NTU's mandatory MFA (Microsoft Authenticator push). Don't try.

## v0.2.0 architectural pivot: "deep reader" → "student helper"

The product framing changed in v0.2.0. The previous framing — "ask Claude about NTULearn files inline via `read_file_content` with vision rendering by default" — turned out to be structurally hostile to MCP:

- **MCP enforces a 1 MB tool-result cap** (claude.ai web especially). PyMuPDF rendering at 2× zoom blows past this on any deck longer than ~5 pages.
- **MuPDF writes errors to stdout**, which is the JSON-RPC framing stream when the server runs over stdio. A single corrupt PDF can break the protocol mid-conversation. Observed in the wild: `Syntax_Differences_Python_vs_Java.pdf` (27 pages) triggered both the >1 MB error *and* two JSON-RPC frame corruption events in the same call.
- **claude.ai's drag-drop already exists** with a ~32 MB budget and native vision document blocks. The server should not try to compete with it.

The pivot reframes the server around **four prompts that fit MCP's payload shape**:

1. *"What announcements happened across my courses this week?"*
2. *"What assignments do I have due next week?"* (via Blackboard calendar)
3. *"Organise this semester's NTULearn content into `~/NTU/y3s1/sc2002/week 8/...` on disk."*
4. *"Pull due dates + grading weightages out of this course briefing PDF."* (small, text-heavy docs only)

For multi-page diagram-heavy lecture decks, users `download_file` to disk and drag the PDF into claude.ai. The server **shortens the path to drag-drop** rather than competing with it.

### Concrete changes in v0.2.0

| Tool | Change |
|---|---|
| `ntulearn_get_course_contents` | Gained optional `parent_id` — absorbs the old `get_folder_children` tool. |
| `ntulearn_get_folder_children` | **Removed.** Merged into `get_course_contents`. |
| `ntulearn_get_file_download_url` | **Removed.** Raw URLs aren't useful standalone (need the auth cookie); `download_file` already returns the resolved path. |
| `ntulearn_get_upcoming` | **NEW.** Wraps `GET /learn/api/public/v1/calendars/items`. Defaults to next 2 weeks across enrolled courses. `type='GradebookColumn'` filters to assignments. |
| `ntulearn_get_announcements` | Gained `course_ids` (optional, defaults to all enrolled, fanned out via `asyncio.gather`) and `since` (ISO-8601 filter). Returns newest-first. |
| `ntulearn_get_gradebook` | Gained `course_ids` (optional, defaults to all enrolled, fanned out). Per-course grade fetch failures flip global `gradesAvailable=False`. |
| `ntulearn_download_file` | Gained `destination_dir` (string; accepts absolute paths and `~`-prefixed paths; created on demand). Enables the `~/NTU/y3s1/sc2002/week 8/` hierarchy use case. Falls back to `$NTULEARN_DOWNLOAD_DIR` env var, then `./downloads/`. |
| `ntulearn_read_file_content` | PDF default flipped from `mode='auto'` (vision-by-default) to `mode='text'`. Vision is now opt-in (`mode='vision'` + a narrow `pages='5'` range is the supported pattern). |

### read_file_content correctness fixes (also v0.2.0)

Three fixes landed together — all needed regardless of the framing pivot:

1. **Stdout protection.** Wrap PyMuPDF rendering in a context manager that calls `fitz.TOOLS.mupdf_display_errors(False)` *and* does fd-level `os.dup2(devnull_fd, 1)` for the duration of the render. The Python-level redirect alone isn't enough — MuPDF emits at C level. Tested in `PDFStdoutProtectionTests` via `os.pipe()` + `os.dup(1)` to assert nothing leaks to the real fd 1.
2. **Byte-budget cap.** Cumulative cap on rendered bytes during vision mode, complementing the page-count cap. When hit, response carries `truncated_pages` + `truncation_reason` so the model can request the next range via existing `pages='X-Y'` parameter.
3. **DPI drop.** Vision zoom lowered from 2.0 → 1.3 (~96 DPI). Smaller bytes per page, still readable for diagrams.

### Final tool surface (8 tools, down from 9)

| Tool | Purpose |
|---|---|
| `ntulearn_list_courses` | Enrolled courses, paginated. |
| `ntulearn_get_course_contents` | Course tree walk. `parent_id` for drilling into folders. |
| `ntulearn_search_course_content` | BFS substring search within one course. |
| `ntulearn_get_upcoming` | Calendar items across enrolled courses (assignments via `type='GradebookColumn'`). |
| `ntulearn_get_announcements` | Cross-course announcements, newest first. |
| `ntulearn_get_gradebook` | Cross-course gradebook columns + scores. |
| `ntulearn_download_file` | File-to-disk with optional `destination_dir`. |
| `ntulearn_read_file_content` | Inline text (default) or vision (opt-in) read of small docs. |

## Cookie acquisition design

Blackboard auth is via the `BbRouter` cookie (`HttpOnly`, `Secure`, typically lasts days–weeks). Every approach has to either get the user to copy it from DevTools or read it from a browser they're already logged into.

**Resolution order in [server.py:_resolve_cookie](src/ntulearn_mcp/server.py)** (browser-first since v0.1.2):
1. [cookie.py:read_bbrouter_cookie](src/ntulearn_mcp/cookie.py) — walks Edge → Chrome → Firefox → Brave via `browser-cookie3` with bounded retry + exponential backoff (0.5s → 1.0s → 2.0s by default). Returns first valid value (validated by `expires:` prefix). On success the value is mirrored into the OS keychain via [cache.py:write_cached_cookie](src/ntulearn_mcp/cache.py). **This is the primary path** — the convenience the MCP server exists for.
2. `NTULEARN_COOKIE` env var — manual fallback (Windows + Chrome/Edge ABE, no logged-in browser, headless environments). The env var is a safety net, not an override: a fresh browser read wins over a possibly-stale env value.
3. [cache.py:read_cached_cookie](src/ntulearn_mcp/cache.py) — last-known-good value from the OS keychain (`keyring`: macOS Keychain / Windows Credential Manager / Linux Secret Service / KWallet). Catches the case where browser fails AND there's no env var seed, but a previous run did successfully read from the browser.
4. `RuntimeError` with a help message pointing to manual env-var setup.

**Why browser-first** (changed from env-first in v0.1.2): a stale `NTULEARN_COOKIE` lingering from one-time debugging or an old `.env` shouldn't preempt a fresh, working browser cookie. Putting the browser first makes the env var a true fallback rather than an override. If you need to force a specific cookie value, the cleanest way is now to ensure no browser auto-read can succeed (sign out of NTULearn in any auto-readable browser) — there is no "always-wins" override anymore.

**Mid-session expiry:** `call_tool` catches `BbRouterExpiredError`, calls `_refresh_client()` which:
1. Closes the existing httpx client.
2. **Invalidates the cookie cache** ([cache.py:delete_cached_cookie](src/ntulearn_mcp/cache.py)) — the cookie that just produced the 401 is dead, and leaving it cached would let the next resolution loop on the same dead value if `browser-cookie3` is also failing.
3. Re-runs `_resolve_cookie()` and rebuilds the client.

`call_tool` then retries the failed call once. Transparent for the user when the browser still has a fresh session.

**How the cache shifts the failure surface:**

| Scenario | Pre-cache | Post-cache |
|---|---|---|
| Mac/Linux, transient `browser-cookie3` race, ever-succeeded before | Retry exhausts → error, user retries the call | Cache fallback → success, user sees nothing |
| Windows + Chrome/Edge ABE, ever-succeeded before (Firefox once, admin run, env-var seed) | Permanent error until manual env var | Cache fallback for cookie's full lifetime → success |
| Genuinely no NTULearn login anywhere ever | Errors with help message | Same (cache empty too) |
| Cookie expires mid-session | 401 → refresh → re-read browser → use fresh value | Same; cache transparently nuked + rewritten |
| User logged out + back in (cookie rotated) | Browser auto-read gets fresh value, cache stale | Browser still wins over cache when both present; fresh value overwrites stale cache entry |

The cache does **not** fix:
- A cold-start machine with no NTULearn login anywhere (cache is empty).
- Windows + Chrome/Edge ABE on the *very first* resolution before any successful read has happened. Falls back to env var or "no cookie" error as before.

## Known limitation: Windows + Chrome/Edge

Chrome's App-Bound Encryption (rolled out 2024+) blocks `browser-cookie3` from reading cookies without admin privileges. As of April 2026 this hits **both Chrome and Edge** on Windows.

Live smoke test on the dev machine showed:
```
Edge: This operation requires admin. Please run as admin.
Chrome: This operation requires admin. Please run as admin.
```

Graceful degradation works (no crash, clean fall-through to error message), but the dream "no cookie config needed" flow doesn't work for the **majority** of the target audience (Windows + Chrome/Edge).

**Implications:**
- Auto-read works reliably for: Mac (any browser), Linux (any browser), Windows + Firefox.
- Manual env var fallback needed for: Windows + Chrome/Edge users — most NTU students.
- Do **not** advise users to "run Claude Desktop as admin" — elevates everything else too.

**Mac + Claude Desktop + `.mcpb`: auto-read works, but is intermittent.** Empirically observed on this dev machine — `mcp-server-ntulearn.log` shows 5+ successful auto-reads followed by a single "No NTULearn cookie found" error, then immediate success again 30s later in the same server process. The mcpb child can access Chrome's cookie DB / Keychain (otherwise it would never succeed), but reads occasionally race with browser writes, keychain access timeouts, or TCC re-evaluation.

**Mitigations now in place:**
- [cookie.py](src/ntulearn_mcp/cookie.py) does a bounded retry with exponential backoff (0.5s → 1.0s → 2.0s, 3 attempts total). Catches the in-band transient cases.
- [cache.py](src/ntulearn_mcp/cache.py) persists every successful read into the OS keychain. The next resolution reads the cache if `browser-cookie3` fails, so a single successful browser read carries the user through the cookie's full lifetime.
- [server.py:_refresh_client](src/ntulearn_mcp/server.py) invalidates the cache before re-resolving on 401, so we don't loop on a dead cookie.

**Net effect on the target platforms:**
- Mac/Linux (`browser-cookie3` works most of the time): user almost never sees a transient failure. Retry catches in-band races; cache catches the rest.
- Windows + Firefox: same as Mac/Linux.
- Windows + Chrome/Edge (ABE): browser auto-read fails permanently. **But** if the user has *ever* successfully resolved a cookie on this machine — via Firefox, an admin run, or seeding `NTULEARN_COOKIE` once — the cache holds the value for the cookie's lifetime (days–weeks). They keep getting silent renewals via the 401-refresh path until the underlying NTULearn session expires.

**Remaining failure mode:** cold-start on Windows + Chrome/Edge with no Firefox installed and no env-var seed. Browser auto-read hits ABE; cache is empty; we raise. User has to either install Firefox, paste the cookie once into `NTULEARN_COOKIE`, or accept the ABE block. None of those are zero-friction, but they're each one-time.

**Browser extension is no longer the only architectural escape.** Cache-backed `browser-cookie3` covers ~all "ever-succeeded" cases. Browser extension would still be the only way to make the cold-start Windows-Chrome path zero-friction.

## mcp-builder remediation pass (originally claude/mcp-builder-eval; still in force)

The pre-pivot work closed every issue raised by the `anthropic-skills:mcp-builder`
skill review. See [evals/REPORT.md](evals/REPORT.md) for the full audit + remediation
table. Highlights (still apply to the post-pivot 8-tool surface):

- All tools are namespaced `ntulearn_*` (no collision risk with other MCP servers).
- Every tool carries `readOnlyHint`/`destructiveHint`/`idempotentHint`/`openWorldHint`
  annotations. Only `ntulearn_download_file` is `readOnlyHint=False`.
- List-returning tools (`list_courses`, `get_course_contents`, `get_upcoming`,
  `get_announcements`, `get_gradebook`) accept `limit`/`offset` and return
  `{total, count, offset, limit, hasMore, nextOffset}` pagination metadata.
- Cross-course aggregators (`get_upcoming`, `get_announcements`, `get_gradebook`)
  fan out via `asyncio.gather(*, return_exceptions=True)`; per-course failures land
  in a `courseErrors` array on the response without sinking the rest.
- Every data-returning tool accepts `response_format ∈ {json, markdown}`.
- Every tool declares an `outputSchema`. Handlers return
  `(unstructured_content, structured_content)` tuples — MCP propagates both forms
  to clients and validates structured against the schema.
- Errors raise instead of returning success-shaped TextContent. The MCP framework
  wraps in `CallToolResult(isError=True, …)` automatically.
- `course_id`/`content_id` patterns + length, `query` `minLength`, `max_depth`
  capped at 10, `limit`/`max_results` capped at 200, `additionalProperties: false`
  on every input schema. SDK enforces before our code runs.
- `BlackboardAPIError` distinguishes 403/404/429/5xx with actionable messages
  including the request path. Calendar 429 is surfaced as a distinct "rate limited"
  message (Anthology docs warn non-3LO calendar calls can be throttled).
- `_validate_cookie_value` rejects CR/LF/NUL on cookie ingest (header-injection
  defence in depth).

## Test status

All 185 tests pass: `uv run python -m unittest discover -s tests`.

Notable test groupings worth knowing about:
- `tests/test_client.py::CalendarItemsTests` — covers the `get_calendar_items` wrapper (param forwarding, empty window, 429 → BlackboardAPIError with "rate limited" message).
- `tests/test_server.py::UpcomingTests` — cross-course fan-out, explicit `course_ids`, `type` filter, ISO-8601 validation, per-course failure recorded.
- `tests/test_server.py::AnnouncementsCrossCourseTests` + `GradebookCrossCourseTests` — verify fan-out + `since` filtering + courseId attribution + per-course error containment. Gradebook tests also verify the "any course's grade fetch fails → global `gradesAvailable=False`" contract.
- `tests/test_server.py::GetCourseContentsTests` — covers the `parent_id` branch (post-merge of `get_folder_children`).
- `tests/test_server.py::DownloadDestinationTests` — `destination_dir` happy path, `~` expansion, env-var fallback, validation errors. Note: tests must compare `Path.resolve()` outputs because on macOS `/var` symlinks to `/private/var`.
- `tests/test_server.py::PDFTextDefaultTests` + `PDFStdoutProtectionTests` — verify text default emits no `ImageContent` blocks, and that fd-level stdout protection actually catches MuPDF C-level emissions (via `os.dup(1)` + `os.pipe()` real-fd capture, not Python-level mocks).
- `tests/test_server.py::CookieResolutionTests` — covers the browser-first → env-fallback → cache-fallback resolution order (still in force post-pivot).

## Architecture gap: downloaded files are unreachable from Claude — RESOLVED, then reframed

Original problem: `download_file` writes to the user's local filesystem, but Claude Desktop's built-in tools (`bash`, code execution) run in a sandboxed container on Anthropic's servers and can't see local files. `localAgentModeTrustedFolders` does **not** bridge this (it's for Cowork / local agent mode, not standard chat tools), and `web_fetch` refuses URLs that didn't come from user input or prior search results — so Claude can't bypass the gap by hitting the bbcswebdav URL directly either.

**Pre-v0.2.0 fix:** `read_file_content` with PDF vision-by-default. Pulled bytes via the authenticated client, rendered every page to PNG, returned `TextContent` + `ImageContent` blocks inline. Worked for small docs; broke for multi-page lecture decks (>1 MB cap, MuPDF stdout corruption).

**v0.2.0 reframing:** `read_file_content` keeps its inline-read role but **PDFs default to text mode**. Vision is opt-in for narrow ranges (e.g. `mode='vision', pages='5'` to ask about a single diagram). For full-fidelity multi-page reading, users `download_file` to disk and drag the PDF into claude.ai (~32 MB budget, native vision document blocks). The server's job is to shorten the path to drag-drop, not compete with it.

Supported formats in `read_file_content`:
- **PDFs** — text by default (`pypdf`); vision opt-in (`pymupdf`) at 1.3× zoom (~96 DPI). Per-call caps: `_MAX_PDF_PAGES_VISION` page count, plus a cumulative byte-budget cap that emits `truncated_pages` + `truncation_reason` when hit so the model can request the next range. Stdout is fd-redirected during render (`os.dup2`) + `fitz.TOOLS.mupdf_display_errors(False)` so MuPDF's C-level emissions never corrupt JSON-RPC framing.
- Microsoft Office: `.docx` (paragraphs + tables), `.pptx` (per-slide shapes + speaker notes), `.xlsx` (all sheets, row-by-row, capped at 1000 rows/sheet to keep grade dumps from blowing up the response).
- Text-likes (txt, md, csv, json, xml, code, html with tags stripped — charset-aware decode).

True binaries (images, video, audio, archives, legacy `.doc`/`.ppt`/`.xls`) are listed under a `skipped` array with a "use download_file" message.

URL resolution is shared with `download_file` via `_resolve_content_files`.

**Out of scope (future tickets):**
- Image files via `ImageContent` — would let Claude see lecture diagrams uploaded as standalone `.jpg`/`.png`.
- Image extraction from inside `.pptx` slides (would catch the most common diagram case but requires mixed-content response).
- Legacy `.doc`/`.ppt`/`.xls` — generally rare on NTULearn; users can convert or use `download_file`.
- Streaming for very large PDFs / Office files (current implementation buffers the full file in memory; 25 MB cap mitigates worst case).
- A `get_course_tree(depth=N)` mega-fetch helper — tempting for the "organise a semester" use case but balloons response size. Model recursion via `get_course_contents` is good enough for now; revisit only if tool-call latency proves painful in real use.

## Open decisions / next steps

In rough priority order:

1. **Live smoke test v0.2.0 against real NTULearn.** Specifically:
   - `ntulearn_get_upcoming()` with no args → next 2 weeks across enrolled courses. Confirm `GradebookColumn` items carry due dates.
   - `ntulearn_get_upcoming(type='GradebookColumn', since=..., until=...)` → assignments-only window.
   - `ntulearn_read_file_content` on the previously-failing `SC2002_tutorials qns-2025S2.docx` + `Syntax_Differences_Python_vs_Java.pdf` → confirm no >1 MB error, no JSON-RPC corruption.
   - `ntulearn_download_file(..., destination_dir='~/NTU/y3s1/sc2002/week 8/')` → verify `~` expansion and on-demand directory creation.
2. **PyPI publication of 0.2.0.** `pyproject.toml` has the metadata; run `uv build` + `uv publish` (requires PyPI account + API token). Verify locally first with `uvx --from . ntulearn-mcp`.
3. **GitHub Actions for tag-triggered PyPI publishing** (optional polish).
4. **Test the full flow on a fresh machine** — verify `browser-cookie3` works on Mac with Chrome (it should — keychain protects it for the same user). Verify the new tools end-to-end.
5. **Image-content support** — add an `image` kind that returns `ImageContent` for standalone `.jpg`/`.png`, and consider extracting embedded images from `.pptx` slides so Claude can see lecture diagrams.
6. **Notion MCP chaining demo** — the "organise a semester" use case becomes powerful when paired with a Notion MCP that mirrors the folder structure into a digital binder. The server stays Notion-agnostic; the value emerges from the chain.

## Project conventions worth knowing

- **Tests use `unittest`, not pytest.** Async tests use `unittest.IsolatedAsyncioTestCase`. HTTP mocked via `httpx.MockTransport`. Match this style; don't introduce pytest.
- **Module-level globals are deliberate** — `app`, `_client`, `BASE_URL`, `DOWNLOAD_DIR` in [server.py](src/ntulearn_mcp/server.py). Tests monkey-patch attributes to override (see `_CookieEnvIsolation` mixin in [tests/test_server.py](tests/test_server.py)).
- **`load_dotenv()` runs at module import**, so reloading the module picks up `.env` changes. The `DotenvPrecedenceTests` test relies on this.
- **Existing commit convention:** no Claude co-author trailer (per user's saved memory rule). Commits should be short imperative, no `Co-Authored-By: Claude` line.

## Useful commands

```bash
uv sync                                          # install deps incl. dev
uv run python -m unittest discover -s tests      # full test suite
uv run python -m unittest discover -s tests -v   # verbose
uv run ntulearn-mcp                              # run the MCP server (stdio)
uv run mcp dev src/ntulearn_mcp/server.py        # interactive tool inspector

# Live smoke test — does browser-cookie3 work on this machine?
uv run python -c "import logging; logging.basicConfig(level=logging.DEBUG); from ntulearn_mcp.cookie import read_bbrouter_cookie; print(read_bbrouter_cookie())"

# Live smoke test — does the OS-keychain cache work on this machine?
uv run python -c "from ntulearn_mcp.cache import read_cached_cookie, write_cached_cookie, delete_cached_cookie; delete_cached_cookie(); write_cached_cookie('expires:9999999999,id:smoketest'); print(read_cached_cookie()); delete_cached_cookie()"
```

## Resuming on another machine

The conversation history that produced this state lives only on the original machine (Claude Code stores sessions locally as JSONL under `~/.claude/projects/<encoded-path>/`). To continue on a different machine, you have two practical options:

1. **Recommended:** push the current branch, pull on the other machine, start a fresh Claude session, and let this CLAUDE.md provide the context. The decisions are captured here; the conversation narrative isn't load-bearing.
2. **If you really need the literal transcript:** copy the session JSONL from `~/.claude/projects/<encoded-source-path>/<uuid>.jsonl` to the equivalent encoded path on the target machine. Path encoding replaces `/` and `\` with `-` based on the *absolute project path*, so the project must live at the same absolute path on both machines for `/resume` to find it. Fiddly; option 1 is cleaner.
