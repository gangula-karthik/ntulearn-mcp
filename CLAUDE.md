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

**Distribution path:** publish to PyPI as `ntulearn-mcp`; users invoke via `uvx ntulearn-mcp` from their MCP host's config. **Not yet published as of this session.** README still describes the dev-from-source flow; needs rewrite to lead with the `uvx` flow.

**Explicitly rejected paths and why:**
- *AI-free product (Chrome extension dashboard, etc.):* user judged the deterministic value not high enough to compete with just opening NTULearn.
- *Hosted web app / Telegram bot:* requires custodian of N students' Blackboard sessions — privacy/policy nightmare. NTU has no public OAuth flow for student apps.
- *`.mcpb` Desktop Extensions:* nicer UX (single file, double-click install, OS-keychain config) but bundles Node.js, not Python — would mean either a Node-wraps-Python adapter or a second distribution artifact. Skipped for v1; revisit if `uvx` friction proves too high.
- *Scripted SSO login:* dead because of NTU's mandatory MFA (Microsoft Authenticator push). Don't try.

## Cookie acquisition design

Blackboard auth is via the `BbRouter` cookie (`HttpOnly`, `Secure`, typically lasts days–weeks). Every approach has to either get the user to copy it from DevTools or read it from a browser they're already logged into.

**Resolution order in [server.py:_resolve_cookie](src/ntulearn_mcp/server.py):**
1. `NTULEARN_COOKIE` env var (explicit override, always wins; never touches cache)
2. [cookie.py:read_bbrouter_cookie](src/ntulearn_mcp/cookie.py) — walks Edge → Chrome → Firefox → Brave via `browser-cookie3` with bounded retry + exponential backoff (0.5s → 1.0s → 2.0s by default). Returns first valid value (validated by `expires:` prefix). On success the value is mirrored into the OS keychain via [cache.py:write_cached_cookie](src/ntulearn_mcp/cache.py).
3. [cache.py:read_cached_cookie](src/ntulearn_mcp/cache.py) — last-known-good value from the OS keychain (`keyring`: macOS Keychain / Windows Credential Manager / Linux Secret Service / KWallet). This is the band-aid for both transient `browser-cookie3` failures (SQLite write-lock race, keychain timeout, TCC re-evaluation) and the permanent Windows + Chrome/Edge ABE block — once *any* successful read has happened on this machine, subsequent resolutions ride through the cookie's lifetime even if the browser path keeps failing.
4. `RuntimeError` with a help message pointing to manual env-var setup.

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

## mcp-builder remediation pass (claude/mcp-builder-eval)

The work on this branch closes every issue raised by the `anthropic-skills:mcp-builder`
skill review. See [evals/REPORT.md](evals/REPORT.md) for the full audit + remediation
table. Highlights:

- All 9 tools are renamed `ntulearn_*` (no more collision risk with other MCP servers).
- Every tool carries `readOnlyHint`/`destructiveHint`/`idempotentHint`/`openWorldHint`
  annotations. Only `ntulearn_download_file` is `readOnlyHint=False`.
- List-returning tools (`list_courses`, `get_course_contents`, `get_folder_children`,
  `get_announcements`, `get_gradebook`) accept `limit`/`offset` and return
  `{total, count, offset, limit, hasMore, nextOffset}` pagination metadata.
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
  including the request path.
- `_validate_cookie_value` rejects CR/LF/NUL on cookie ingest (header-injection
  defence in depth).
- 31 new tests in `tests/test_fixes.py` cover every behaviour change.

## Test status

All 155 tests pass: `uv run python -m unittest discover -s tests`.

| File | Status | Notes |
|---|---|---|
| [src/ntulearn_mcp/cookie.py](src/ntulearn_mcp/cookie.py) | modified | Dependency-injectable for tests via `module=` kwarg; added bounded retry + exponential backoff (0.5s → 1.0s → 2.0s), injectable `sleep` for fast tests, structured browser-error logging |
| [src/ntulearn_mcp/cache.py](src/ntulearn_mcp/cache.py) | new | OS-keychain cookie cache via `keyring`. Read/write/delete all degrade to no-op on backend failure |
| [src/ntulearn_mcp/server.py](src/ntulearn_mcp/server.py) | modified | `_resolve_cookie` now mirrors browser reads to cache and falls back to cache on browser failure; `_refresh_client` invalidates cache before re-resolving |
| [pyproject.toml](pyproject.toml) | modified | Added `browser-cookie3>=0.20.1`, `keyring>=25.0`; bumped version to 0.1.1 |
| [tests/test_cookie.py](tests/test_cookie.py) | modified | Original 8 tests + 3 new retry tests (`CookieRetryTests`) |
| [tests/test_cache.py](tests/test_cache.py) | new | 15 tests against an in-memory `FakeKeyring`; covers happy path + all backend-failure branches |
| [tests/test_server.py](tests/test_server.py) | modified | `_CookieEnvIsolation` now also stubs cache so tests never touch real keychain. Added 6 cache-integration tests in `CookieResolutionTests` and 1 cache-invalidation test in `CookieRefreshTests` |
| [uv.lock](uv.lock) | modified | Reflects new deps (`keyring`, `jaraco-classes`, `jaraco-context`, `jaraco-functools`, `more-itertools`) |

Changes are **uncommitted** in this worktree on branch `claude/pedantic-taussig-9c8400`. To resume on another machine, see "Resuming on another machine" below.

## Architecture gap: downloaded files are unreachable from Claude — RESOLVED

Original problem: `download_file` writes to the user's local filesystem, but Claude Desktop's built-in tools (`bash`, code execution) run in a sandboxed container on Anthropic's servers and can't see local files. `localAgentModeTrustedFolders` does **not** bridge this (it's for Cowork / local agent mode, not standard chat tools), and `web_fetch` refuses URLs that didn't come from user input or prior search results — so Claude can't bypass the gap by hitting the bbcswebdav URL directly either.

**Resolved by `read_file_content` tool.** Added in [src/ntulearn_mcp/server.py](src/ntulearn_mcp/server.py) — fetches bytes via the authenticated client, extracts text inline, returns the content as `TextContent` (plus `ImageContent` blocks for PDFs in the default mode). No filesystem hop. Per-file cap 25 MB, batch cap 40 MB. Supported formats:
- **PDFs — vision mode by default** (`pymupdf`): each page is rendered as a PNG and text-extracted, so Claude sees diagrams, equations, and scanned content at the same depth as a drag-and-drop into Claude.ai. Costs ~3K vision tokens per rendered page; capped at `_MAX_PDF_PAGES_VISION = 50` pages per call. Pass `mode='text'` to skip rendering and use the cheaper `pypdf`-only path for pure-prose PDFs, or `pages='1-10'` / `pages='1,3,5'` to restrict the page range.
- Microsoft Office: `.docx` (paragraphs + tables), `.pptx` (per-slide shapes + speaker notes), `.xlsx` (all sheets, row-by-row, capped at 1000 rows/sheet to keep grade dumps from blowing up the response)
- Text-likes (txt, md, csv, json, xml, code, html with tags stripped — charset-aware decode)

True binaries (images, video, audio, archives, legacy `.doc`/`.ppt`/`.xls`) are listed under a `skipped` array with a "use download_file" message.

URL resolution is shared with `download_file` via `_resolve_content_files`. `download_file` is kept — different job (users who actually want bytes on disk).

**Out of scope (future tickets):**
- Image files via `ImageContent` — would let Claude see lecture diagrams uploaded as standalone `.jpg`/`.png`.
- Image extraction from inside `.pptx` slides (would catch the most common diagram case but requires mixed-content response).
- Legacy `.doc`/`.ppt`/`.xls` — generally rare on NTULearn; users can convert or use `download_file`.
- Streaming for very large PDFs / Office files (current implementation buffers the full file in memory; 25 MB cap mitigates worst case).

## Open decisions / next steps

In rough priority order:

1. **Ship 0.1.1 with cookie.py retry + cache.py keychain layer.** Test on the real Windows-Chrome path before publishing — the Mac smoke test passed but the Windows ABE-then-cache codepath needs verification with at least one successful seed read.
2. **Rewrite README** to lead with the `uvx ntulearn-mcp` flow (5-step Claude Desktop config), demote dev-from-source to a "Contributing" section, document both auto and manual cookie paths honestly. Should also mention `read_file_content` as the primary tool for asking questions about content (vs `download_file` for "save to disk").
3. **PyPI publication.** `pyproject.toml` needs more metadata (`license`, `authors`, `urls`, `classifiers`). Then `uv build` + `uv publish` (requires PyPI account + API token). Verify locally first with `uvx --from . ntulearn-mcp`.
4. **GitHub Actions for tag-triggered PyPI publishing** (optional polish).
5. **Test the full flow on a fresh machine** (Mac, planned for next session) — verify `browser-cookie3` actually works on Mac with Chrome (it should — keychain protects it for the same user). Also exercise `read_file_content` against real PDF / Office files.
6. **Image-content support** — add an `image` kind that returns `ImageContent` for standalone `.jpg`/`.png`, and consider extracting embedded images from `.pptx` slides so Claude can see lecture diagrams.

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

1. **Recommended:** push this branch (`claude/pedantic-taussig-9c8400`), pull on the other machine, start a fresh Claude session, and let this CLAUDE.md provide the context. The decisions are captured here; the conversation narrative isn't load-bearing.
2. **If you really need the literal transcript:** copy the session JSONL from `~/.claude/projects/<encoded-source-path>/<uuid>.jsonl` to the equivalent encoded path on the target machine. Path encoding replaces `/` and `\` with `-` based on the *absolute project path*, so the project must live at the same absolute path on both machines for `/resume` to find it. Fiddly; option 1 is cleaner.
