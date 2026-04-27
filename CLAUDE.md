# CLAUDE.md

Context for AI assistants resuming work on this repo. Captures the **decisions and constraints** that aren't visible in the code or git history. For *what* the code does, read the code; for *why*, read this.

## Project

`ntulearn-mcp` — Python MCP server wrapping the Blackboard Learn REST API for NTULearn (NTU Singapore's LMS). Lets MCP hosts (Claude Desktop, Cursor, Cline, Claude Code, etc.) interact with courses, content, downloads, announcements, and grades.

Source layout in [src/ntulearn_mcp/](src/ntulearn_mcp/):
- [server.py](src/ntulearn_mcp/server.py) — MCP entrypoint, tool handlers, cookie resolution, 401-retry wrapper
- [client.py](src/ntulearn_mcp/client.py) — async httpx-based Blackboard REST client
- [cookie.py](src/ntulearn_mcp/cookie.py) — browser cookie auto-read (added in this session)
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
1. `NTULEARN_COOKIE` env var (explicit override, always wins)
2. [cookie.py:read_bbrouter_cookie](src/ntulearn_mcp/cookie.py) — walks Edge → Chrome → Firefox → Brave via `browser-cookie3`, returns first valid value (validated by `expires:` prefix to reject ABE-decrypt-to-garbage)
3. `RuntimeError` with a help message pointing to manual setup

**Mid-session expiry:** `call_tool` catches `BbRouterExpiredError`, calls `_refresh_client()` (which re-runs resolution and rebuilds the httpx client), retries the call once, then surfaces. Transparent for the user when the browser still has a fresh session.

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

**Open question:** is `browser-cookie3` worth keeping as default given how often it falls through on the target platform? Two reasons it still earns its keep:
1. Cross-platform users (Mac/Linux) get the magic UX.
2. Even when initial resolution falls back to env var, the **mid-session refresh** still works for non-Chromium-on-Windows browsers — auto-handles cookie expiry without user intervention.

If usage data shows ~all friends are Windows-Chrome, consider escalating to a browser extension (the `chrome.cookies` API is unaffected by ABE) as the actual primary path.

## What's implemented this session

All 24 tests pass: `uv run python -m unittest discover -s tests`.

| File | Status | Notes |
|---|---|---|
| [src/ntulearn_mcp/cookie.py](src/ntulearn_mcp/cookie.py) | new | Dependency-injectable for tests via `module=` kwarg |
| [src/ntulearn_mcp/server.py](src/ntulearn_mcp/server.py) | modified | Removed module-level `COOKIE` constant; added `_resolve_cookie`, `_refresh_client`, `_dispatch`; refactored `call_tool` for 401-retry |
| [pyproject.toml](pyproject.toml) | modified | Added `browser-cookie3>=0.20.1` |
| [tests/test_cookie.py](tests/test_cookie.py) | new | 8 tests, all use a `_fake_module` SimpleNamespace — no real browser access |
| [tests/test_server.py](tests/test_server.py) | modified | Existing tests adapted; added `CookieResolutionTests` (4) and `CookieRefreshTests` (3) |
| [uv.lock](uv.lock) | modified | Reflects new deps (cryptography, pycryptodomex, lz4, pywin32, etc.) |

Changes are **uncommitted** in this worktree on branch `claude/pedantic-taussig-9c8400`. To resume on another machine, see "Resuming on another machine" below.

## Architecture gap: downloaded files are unreachable from Claude — RESOLVED

Original problem: `download_file` writes to the user's local filesystem, but Claude Desktop's built-in tools (`bash`, code execution) run in a sandboxed container on Anthropic's servers and can't see local files. `localAgentModeTrustedFolders` does **not** bridge this (it's for Cowork / local agent mode, not standard chat tools), and `web_fetch` refuses URLs that didn't come from user input or prior search results — so Claude can't bypass the gap by hitting the bbcswebdav URL directly either.

**Resolved by `read_file_content` tool.** Added in [src/ntulearn_mcp/server.py](src/ntulearn_mcp/server.py) — fetches bytes via the authenticated client, extracts text inline (PDFs via `pypdf`, text-like files decoded directly with charset/HTML handling), returns the content as `TextContent`. No filesystem hop. Per-file cap 25 MB, batch cap 40 MB. Binaries (and `.docx`/`.pptx` for now) are listed under a `skipped` array with a "use download_file" message.

URL resolution is shared with `download_file` via `_resolve_content_files`. `download_file` is kept — different job (users who actually want bytes on disk).

**Out of scope (future tickets):**
- Office formats (`.docx`, `.pptx`, `.xlsx`) — extremely common on NTULearn but require new deps (`python-docx`, `python-pptx`, `openpyxl`).
- Image files via `ImageContent` — would let Claude see lecture diagrams directly.
- Streaming for very large PDFs (current implementation buffers the full file in memory; 25 MB cap mitigates worst case).

## Open decisions / next steps

In rough priority order:

1. **Decide on browser-cookie3 as primary vs. demoting to nice-to-have.** Depends on user's appetite for the Windows-Chrome/Edge fallback friction. Either ship as-is and document, or escalate to browser-extension primary.
2. **Rewrite README** to lead with the `uvx ntulearn-mcp` flow (5-step Claude Desktop config), demote dev-from-source to a "Contributing" section, document both auto and manual cookie paths honestly. Should also mention `read_file_content` as the primary tool for asking questions about content (vs `download_file` for "save to disk").
3. **PyPI publication.** `pyproject.toml` needs more metadata (`license`, `authors`, `urls`, `classifiers`). Then `uv build` + `uv publish` (requires PyPI account + API token). Verify locally first with `uvx --from . ntulearn-mcp`.
4. **GitHub Actions for tag-triggered PyPI publishing** (optional polish).
5. **Test the full flow on a fresh machine** (Mac, planned for next session) — verify `browser-cookie3` actually works on Mac with Chrome (it should — keychain protects it for the same user). Also exercise `read_file_content` against a real PDF.
6. **Office-format support** — add `python-docx` / `python-pptx` extraction to `read_file_content` once usage shows it's worth the dep cost.

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
```

## Resuming on another machine

The conversation history that produced this state lives only on the original machine (Claude Code stores sessions locally as JSONL under `~/.claude/projects/<encoded-path>/`). To continue on a different machine, you have two practical options:

1. **Recommended:** push this branch (`claude/pedantic-taussig-9c8400`), pull on the other machine, start a fresh Claude session, and let this CLAUDE.md provide the context. The decisions are captured here; the conversation narrative isn't load-bearing.
2. **If you really need the literal transcript:** copy the session JSONL from `~/.claude/projects/<encoded-source-path>/<uuid>.jsonl` to the equivalent encoded path on the target machine. Path encoding replaces `/` and `\` with `-` based on the *absolute project path*, so the project must live at the same absolute path on both machines for `/resume` to find it. Fiddly; option 1 is cleaner.
