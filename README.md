# ntulearn-mcp

MCP server for **NTULearn** (NTU Singapore's Blackboard Learn instance). Lets Claude Desktop,
Claude Code, Prime Agent, Cursor, Cline, and other MCP hosts answer questions about your courses,
announcements, calendar, and grades — and organise course files into a folder hierarchy on disk.

> This README is written for **AI agents performing the setup**, and for humans who want the same
> step-by-step. Follow the playbook in order; each step has a verification checkpoint. Keep reading
> for the full tool reference, authentication details, and troubleshooting.

---

---

## Using the Rust server (recommended)

This repo's **shipped server is the Rust binary** (an ultrafast-mcp rewrite of the same 21 tools,
with identical tool names and prompts). New installations should use it — skip the Python sections
and start here.

```bash
cd rust
cargo build --release
# binary: target/release/ntulearn-mcp
```

One-time cookie setup — just log in:

```bash
./target/release/ntulearn-mcp setup
```

`setup` launches a throwaway-profile browser (Chrome, Arc, Brave, Edge, or Chromium), opens
NTULearn for you, detects your `BbRouter` session cookie, validates it live, and saves it to
`<config>/ntulearn-mcp/cookie`. You only need to log in in the window that opens. No copy-paste,
no devtools, no OS-keychain prompts. If no supported browser is found (headless server), it falls
back to a manual paste flow.

Then register the **Rust binary** with your MCP host (adjust the path):

```bash
# e.g. Claude Code / Cursor / Cline — point the command at the Rust binary:
"/Users/you/ntulearn-mcp/rust/target/release/ntulearn-mcp"
```

Verify:

```bash
./target/release/ntulearn-mcp check   # expect: Cookie source + Live validity : OK (200)
```

Cookie refresh is on-demand and reuses the same capture flow:

```bash
./target/release/ntulearn-mcp refresh
```

> The Rust server never reads your OS keychain. It resolves the cookie only from
> `NTULEARN_COOKIE` env → config file → read-only Firefox `cookies.sqlite`, and the `setup`
> command acquires cookie values via your browser's own debugging channel (a throwaway profile), so
> no keychain access is needed or attempted.
>
> The Python implementation below remains fully supported as an alternative (same tools), if you
> prefer running it from source.

---

## Agent setup playbook


Prerequisites: **Python 3.12+**, [`uv`](https://docs.astral.sh/uv/), and a logged-in NTULearn session
in **Chrome, Edge, Firefox, or Brave**.

### 0. Security rules — read before doing anything else

- **Never commit a cookie value.** `NTULEARN_COOKIE` and the `BbRouter` cookie value are session
  credentials. Anyone holding them can act as the student on NTULearn until they expire.
- `.env` and `downloads/` are already `.gitignore`d. Keep it that way. Do **not** `git add -f` them.
- Put your real values only in a local `.env` (or the MCP host's `env` block / your OS keychain).
  Only placeholder values belong in committed files (see `.env.example`).
- Before `git push`, run the secret sanity grep in [Publishing / secret check](#publishing--secret-check).
- On macOS the server stores a **last-known-good cookie in your OS keychain** — that is the intended
  secret store. Do not copy keychain values into code or configs.
- Only two tools write to disk (`ntulearn_download_file`, `ntulearn_download_course`); everything
  else is read-only. Keep automated workflows read-only unless download is requested.

### 1. Install

From this repository (most reliable for agents):

```bash
git clone https://github.com/gangula-karthik/ntulearn-mcp.git
cd ntulearn-mcp
uv sync                                        # creates .venv with all deps
```

Verification:

```bash
.venv/bin/ntulearn-mcp --help 2>&1 | head -5   # should print MCP stdio info, not a traceback
.venv/bin/python -m unittest discover -s tests  # expect: Ran 274 tests ... OK
```

> If the package is published to PyPI, the same server is available as `uvx ntulearn-mcp`
> (no clone needed). The rest of this guide uses the `.venv/bin/ntulearn-mcp` path — substitute
> `uvx ntulearn-mcp` if you installed that way.

### 2. Cookie (authentication) — no secret copying required on most setups

The server resolves your `BbRouter` cookie itself, in this order:

1. **Browser auto-read** — walks Edge → Chrome → Firefox → Brave via `browser-cookie3`.
2. **`NTULEARN_COOKIE` env var** — manual fallback (Windows + Chrome/Edge with App-Bound
   Encryption, headless machines, CI).
3. **Last-known-good cache in the OS keychain** — covers transient browser-read failures for the
   cookie's full lifetime once any path has succeeded once.

So the normal flow is: *the user is logged into NTULearn in their browser, and the server reads the
cookie itself*. On macOS the **first** read may trigger a one-time Keychain approval dialog — tell
the user to click **Always Allow** (see [macOS first-time setup](#macos-first-time-setup)).

If browser auto-read cannot work (e.g. Windows + Chrome ABE), use the
[manual cookie fallback](#manual-cookie-fallback) and put the value only in the MCP host's `env`
block or a local `.env` — **never in committed files**.

### 3. Register the server with an MCP host

**Prime Agent** (this user's primary host):

```bash
prime-agent mcp add ntulearn -- /path/to/ntulearn-mcp/.venv/bin/ntulearn-mcp
prime-agent mcp list                 # should list ntulearn: stdio
```

Then run `/reload` inside Prime Agent (or restart it) to activate. After that, the server appears
under the generic MCP connections alongside `google-docs` and `notion-school`, and an agent can
verify it from the kernel:

```python
await mcp.list_tools("ntulearn")     # expect 21 tools
```

**Claude Code**:

```bash
# from source
claude mcp add ntulearn -- /path/to/ntulearn-mcp/.venv/bin/ntulearn-mcp
# or from PyPI
claude mcp add ntulearn -- uvx ntulearn-mcp
```

**Claude Desktop** — edit `claude_desktop_config.json`:

- macOS: `~/Library/Application Support/Claude/claude_desktop_config.json`
- Windows: `%APPDATA%\Claude\claude_desktop_config.json`

```json
{
  "mcpServers": {
    "ntulearn": {
      "command": "/path/to/ntulearn-mcp/.venv/bin/ntulearn-mcp",
      "args": []
    }
  }
}
```

**Cursor** — edit `~/.cursor/mcp.json` with the same shape as Claude Desktop above.

### 4. Verify end-to-end

1. Restart / reload the MCP host.
2. Ask one of these:
   - *"What's due in NTULearn over the next two weeks?"*
   - *"What announcements went out across my courses this past week?"*
3. If tools list 401 errors, the cookie is expired or missing — see [Troubleshooting](#troubleshooting).

---

## What it's for

Four prompts this server is built to make easy:

1. **"What announcements happened across my courses this week?"** → fans out across all enrolled courses, sorted newest first.
2. **"What assignments do I have due next week?"** → reads NTULearn's calendar, including gradable items (`type=GradebookColumn`).
3. **"Organise this semester's NTULearn content into `~/NTU/y3s1/sc2002/week 8/…` on my disk."** → walks the course tree and downloads files into a folder layout you describe in plain English.
4. **"Pull the assignment due dates and grading weightages out of this course briefing PDF."** → reads small text-heavy PDFs / Office docs inline (no filesystem hop).

For multi-page, diagram-heavy lecture decks, **use `download_file` and drag the PDF into claude.ai** — that path has a 32 MB budget and native vision rendering. MCP tool results are capped at 1 MB; this server doesn't try to compete with drag-and-drop for full lecture decks.

---

## Tools

21 tools. Most do cross-course aggregation by default — you almost never need to pass course IDs by hand.

| Tool | What it does |
|---|---|
| `ntulearn_list_courses` | List enrolled courses. |
| `ntulearn_get_course_contents` | Walk a course's content tree. Omit `parent_id` for the top level; pass it to drill into a folder. |
| `ntulearn_search_course_content` | Recursive substring search within one course. |
| `ntulearn_get_upcoming` | **Calendar items across enrolled courses.** Defaults to the next 2 weeks. `type='GradebookColumn'` filters to assignments. |
| `ntulearn_get_announcements` | **Announcements across enrolled courses, newest first.** Optional `since` for "this week". |
| `ntulearn_get_gradebook` | **Gradebook columns across enrolled courses,** with your scores when available. |
| `ntulearn_download_file` | Download every file on a content item to disk. `destination_dir` lets you build hierarchies (`~/NTU/y3s1/sc2002/week 8/`). |
| `ntulearn_read_file_content` | Read attached file content inline (no filesystem hop). PDFs default to **text** mode; pass `mode='vision'` + a narrow `pages` range for diagram-heavy pages. |
| `ntulearn_list_messages` | List your NTULearn mailbox messages (inbox/sent). Filter to unread or since a date. |
| `ntulearn_read_message` | Read one message by ID, with full body and recipients. |
| `ntulearn_list_course_users` | List users in a course (instructors, TAs, students). |
| `ntulearn_list_course_groups` | List the groups defined in a course (tutorial/lab groups). |
| `ntulearn_get_group_members` | List the members of a specific course group. |
| `ntulearn_get_gradebook_attempts` | List submission attempts for an assignment (gradebook column). |
| `ntulearn_search_all_courses` | Search content across **all** courses at once; results carry courseId + breadcrumb. |
| `ntulearn_get_content_tree` | Return one course's entire content tree as nested JSON (bounded by `max_depth`). |
| `ntulearn_download_course` | Recursively download every file in a course to `~/Downloads/NTU/<course>` (write tool). |
| `ntulearn_whats_new` | One-call digest: announcements + upcoming + gradebook summary since a cutoff. |
| `ntulearn_export_calendar_ics` | Export calendar items (incl. due dates) as an iCalendar `.ics` string. |
| `ntulearn_export_gradebook_csv` | Export your gradebook as a CSV string for a spreadsheet. |
| `ntulearn_summarize_course` | Bite-size briefing of one course: instructors, upcoming, announcements, grades, top folders. |

Most read-only tools default to `response_format='json'`; pass `response_format='markdown'` for a
human-readable summary. List-returning tools accept `limit`/`offset` pagination.

---

## Resources & Prompts

The server exposes dynamic **resources** and two **prompt templates**:

- **Resources** — `ntulearn://courses/{course_id}` returns a JSON course briefing (the same content
  as `ntulearn_summarize_course`). `list_resources` enumerates your enrolled courses, and the
  `{course_id}` URI template lets clients read any course directly.
- **Prompts** —
  - `ntulearn-weekly-brief` — args `courses` (optional comma-separated IDs) and `days` (default 7).
    Produces a prompt that chains `ntulearn_get_announcements` + `ntulearn_get_upcoming` over a
    computed since/until window.
  - `ntulearn-assignment-triage` — args `courses` and `days` (default 14). Produces a due-date
    triage prompt that chains `ntulearn_get_upcoming(type='GradebookColumn')` + `ntulearn_get_gradebook`.

Prompts return prompt **text only**; the model executes the chained tools itself.

## Example prompts

- *"What announcements went out across my courses this past week?"* — `get_announcements(since='2026-05-09T00:00:00Z')`.
- *"What assignments do I have due in the next two weeks?"* — `get_upcoming(type='GradebookColumn')`.
- *"Show me the full calendar for the next 10 days."* — `get_upcoming(until='2026-05-26T00:00:00Z')`.
- *"What's my current grade in `_12345_1`?"* — `get_gradebook(course_ids=['_12345_1'])`.
- *"Read me the assignment brief — `_67890_1` in `_12345_1`."* — `read_file_content` text mode.
- *"There's a UML diagram on slide 5 of this deck I want to ask about."* — `read_file_content(mode='vision', pages='5')`.

## Walkthrough: organising a semester

> *"Walk my enrolled courses and put each course's content under `~/NTU/y3s1/<course-name>/<topic>/…`."*

The model chains tools roughly like this:

1. `ntulearn_list_courses` → enrolled course list.
2. For each course: `ntulearn_get_course_contents(course_id)` → top-level folders.
3. For each folder: `ntulearn_get_course_contents(course_id, parent_id=...)` → child items (recurse).
4. For each file-bearing content item: `ntulearn_download_file(course_id, content_id, destination_dir='~/NTU/y3s1/<course>/<topic>/')`.

`destination_dir` accepts absolute paths and `~`-prefixed paths and is created on demand. Pair this with the Notion MCP server if you want the result mirrored to a digital binder.

---

## Authentication

The server resolves your `BbRouter` cookie in this order:

1. **Browser auto-read** — walks Edge → Chrome → Firefox → Brave via [`browser-cookie3`](https://pypi.org/project/browser-cookie3/), returns the first valid `BbRouter`.
2. **`NTULEARN_COOKIE` env var** — manual fallback when no browser auto-read can succeed (Windows + Chrome/Edge ABE, headless, etc.).
3. **Last-known-good cache** in your OS keychain (macOS Keychain / Windows Credential Manager / Linux Secret Service) — covers transient browser-read failures for the cookie's full lifetime once any path has succeeded once.

When your session expires mid-conversation, the server catches the 401, invalidates the cache,
re-reads from your browser, and retries the call once. If your browser still has a fresh session,
this is invisible.

### Platform support for auto-read

| Platform | Browser | Auto-read | Notes |
|---|---|---|---|
| macOS | any | ✅ | One-time Keychain prompt — see [macOS first-time setup](#macos-first-time-setup) |
| Linux | any | ✅ | May prompt for keyring unlock on Chromium |
| Windows | Firefox | ✅ | |
| Windows | Chrome / Edge | ❌ | Blocked by [App-Bound Encryption](https://security.googleblog.com/2024/07/improving-security-of-chrome-cookies-on.html) — use [manual fallback](#manual-cookie-fallback) |

**Windows + Chrome/Edge users:** Chrome's ABE (rolled out 2024) prevents non-admin processes from
reading cookies. Don't elevate Claude Desktop to admin to work around this — it elevates everything
else too. Use the manual cookie fallback below, or switch to Firefox for NTULearn.

### macOS first-time setup

The first time the server reads cookies from a Chromium browser on macOS, you'll see:

> *"uv wants to access key 'Chrome' in your keychain"*

Click **Always Allow** and enter your macOS login password. You won't see the prompt again.

**If the prompt doesn't appear** (it can be suppressed when the MCP server runs as a child of a
host app), bootstrap the approval from your own Terminal:

```bash
/path/to/ntulearn-mcp/.venv/bin/python -c "from ntulearn_mcp.cookie import read_bbrouter_cookie; print(read_bbrouter_cookie() or 'no cookie found')"
```

The Keychain dialog will appear in front of Terminal. Approve it, then your MCP host will work afterwards.

### Manual cookie fallback

If auto-read doesn't work for you:

1. Open https://ntulearn.ntu.edu.sg in your browser and log in.
2. Open DevTools (`F12`) → **Application** → **Cookies** → `ntulearn.ntu.edu.sg`.
3. Copy the **Value** of the `BbRouter` cookie (starts with `expires:`).
4. Add it to your MCP host's `env` block (never commit it):

   ```json
   {
     "mcpServers": {
       "ntulearn": {
         "command": "/path/to/ntulearn-mcp/.venv/bin/ntulearn-mcp",
         "args": [],
         "env": {
           "NTULEARN_COOKIE": "expires:1234567890,id:..."
         }
       }
     }
   }
   ```

5. Restart your MCP host.

The cookie expires with your NTULearn session (days–weeks). When it does, repeat from step 1.
The env var is a **fallback**, not an override — if a browser auto-read succeeds, the fresh browser
value wins.

---

## Optional configuration

| Env var | Default | Purpose |
|---|---|---|
| `NTULEARN_COOKIE` | (auto-read) | Manual cookie fallback. |
| `NTULEARN_BASE_URL` | `https://ntulearn.ntu.edu.sg` | Change for a different Blackboard instance. |
| `NTULEARN_DOWNLOAD_DIR` | `./downloads` | Default `destination_dir` for `download_file` / `download_course` when no per-call value is passed. |
| `NTULEARN_CACHE_DIR` | `~/.cache/ntulearn-mcp/cache.sqlite3` | SQLite persistence path for the response cache (best-effort; falls back to in-memory). |
| `NTULEARN_CACHE_MODE` | `readwrite` | `readwrite` (default), `readonly` (never evict/write), or `off` (no-op cache). |
| `NTULEARN_HTTP2` | on | Set `0` to disable HTTP/2 on the underlying httpx client. |
| `NTULEARN_FIELDS` | on | Set `0` to disable default `fields=` trimming on API responses. |
| `NTULEARN_JSON` | on (orjson if available) | Set `0` to force stdlib `json` instead of orjson. |

Set these in your MCP host's `env` block (same place as `NTULEARN_COOKIE` above).

---

## Development

```bash
git clone https://github.com/gangula-karthik/ntulearn-mcp.git
cd ntulearn-mcp
uv sync                                          # install deps incl. dev
.venv/bin/python -m unittest discover -s tests   # run tests (expect 274 OK)
.venv/bin/ntulearn-mcp                           # run the server (stdio)
.venv/bin/python -m mcp dev src/ntulearn_mcp/server.py   # interactive tool inspector
```

Project layout:

```
src/ntulearn_mcp/
├── server.py     # MCP entrypoint, tool handlers, cookie resolution
├── handlers.py   # the 13 v0.3 tool handlers
├── common.py     # shared handler helpers
├── render.py     # markdown/csv/ics renderers for new tools
├── client.py     # async httpx (HTTP/2, retries) Blackboard REST client
├── cache.py      # response + last-known-good cookie cache (SQLite / OS keychain)
├── cookie.py     # browser cookie auto-read
└── parsers.py    # HTML body → download URL extraction
```

Tests use `unittest` (not pytest); HTTP is mocked via `httpx.MockTransport`.

---

## Rust implementation (ultrafast-mcp)

The server is also implemented in Rust on top of
[`ultrafast-mcp`](https://github.com/techgopal/ultrafast-mcp) — a full
performance-oriented rewrite of the same 21-tool surface with identical tool
names, prompts, and resource template.

```bash
cd rust
cargo build --release          # binary at target/release/ntulearn-mcp
cargo test                     # 26 unit tests (cache, client, cookie, capture)
target/release/ntulearn-mcp    # serve over stdio
```

### Cookie setup & on-demand refresh

The Rust server has three CLI subcommands that run outside the stdio MCP loop:

```bash
ntulearn-mcp setup      # one-time interactive cookie acquisition
ntulearn-mcp check      # show cookie state (source, expiry, live validity)
ntulearn-mcp refresh    # on-demand cookie refresh + re-validate
```

- `setup` first looks for a cookie you already have (env var, config file,
  Firefox), and validates it live against the API. If none is found, it tries
  **fully-automatic capture**: on macOS/Linux it launches Chrome (or Arc,
  Brave, Edge, Chromium) in a throwaway profile with a local debugging port,
  opens NTULearn, and watches the browser's DevTools channel for the `BbRouter`
  session cookie. The user only has to log in in the window that opens — no
  copy-and-paste, no devtools, no keychain access. The captured value is
  validated live against the API before being saved to the config file
  (`<config>/ntulearn-mcp/cookie`), and the throwaway window/profile is closed
  automatically. If no supported browser is found (e.g. headless server), it
  falls back to opening the login page and accepting a one-time paste of the
  `BbRouter` value (or full `Cookie` header).
- `check` reports where the current cookie came from, how long it is valid (if
  expiry is embedded), and whether it still works live — without changing anything.
- `refresh` re-resolves the cookie from all sources and re-validates it, and
  saves a working value to the config file. Refresh is never proactive: it runs
  only when you invoke it (or when a live call returns 401, in which case the
  newly-resolved cookie is persisted so the next run starts authenticated).

Differences from the Python server (intentional):

- Cookie resolution never touches the OS keychain and never prompts: env var →
  config-file (`<config>/ntulearn-mcp/cookie`) → Firefox `cookies.sqlite`
  plaintext read. Chrome/Edge/Safari auto-read is not attempted (encrypted
  stores).
- 401 now invalidates the cache, re-resolves the cookie once, and retries
  (Python raises immediately); the refreshed cookie is persisted to the config
  file for future runs.

Project layout (`rust/crates/ntulearn-mcp/src/`):

```
main.rs     # ultrafast-mcp server wiring (stdio)
handlers.rs # the 21 tool handlers (Python-parity emit / response_format)
parsers.rs  # HTML body → download URL extraction (scraper)
render.rs   # markdown/csv/ics renderers
client.rs   # reqwest (HTTP/2, retries) Blackboard REST client + download
cache.rs    # SQLite + in-memory TTL cache (per-instance LRU, scoped keys)
cookie.rs   # layered cookie resolution (never keychain)
```

---

## Publishing / secret check

Before pushing to a remote, run this to confirm no credentials are in history or the tree:

```bash
git status --short                                  # no .env / downloads / cookie files staged
git grep -n -I -E '(BbRouter=|expires:|Set-Cookie|ghp_[A-Za-z0-9]{20,}|-----BEGIN .*PRIVATE KEY-----)' $(git rev-list --all) -- 2>/dev/null | grep -viE 'test|\.env\.example|README' || echo "clean"
```

If anything real shows up, do **not** push — scrub it first (rotate the credential, remove it from
history with a filter-repo or interactive rebase, then force-push only if you must and own the
remote).

---

## Troubleshooting

**"No NTULearn cookie found" / tools fail with 401.**
Make sure you're logged into NTULearn in a supported browser. If you're on Windows + Chrome/Edge, set `NTULEARN_COOKIE` per the [manual fallback](#manual-cookie-fallback).

**MCP host lists "ntulearn" but the tool calls hang or return nothing.**
On macOS, the first call may be blocked on a hidden Keychain prompt. See [macOS first-time setup](#macos-first-time-setup).

**Prime Agent lists the server but tools aren't available.**
Run `/reload` in Prime Agent (or restart it) so settings are re-read and the integration activates.

**`read_file_content` returns "would exceed batch cap" / nothing useful for a big PDF.**
That's expected for multi-page lecture decks. Use `download_file` (with a `destination_dir` if you want it organised) and drag the resulting file into claude.ai for full-fidelity reading. `read_file_content` is for small documents (briefs, tutorials) you want to ask questions about inline.

**The server crashes on startup.**
Run it directly to see the error:

```bash
.venv/bin/ntulearn-mcp
```

Most common cause: no cookie resolvable. The error message will guide you.

**Auto-read worked yesterday, doesn't work today.**
Your browser session probably expired. Open NTULearn in your browser, complete SSO + MFA, then retry — auto-refresh handles the rest.

---

## Disclaimer & responsible use

**Use at your own risk.** This is an unofficial, personal-use tool. It is **not** affiliated with,
endorsed by, or sponsored by NTU Singapore, Anthology Inc., or Blackboard Learn. NTULearn,
Blackboard, and related marks belong to their respective owners.

- **Your account, your responsibility.** Driving the LMS via your session cookie may be inconsistent
  with NTU's IT acceptable use policy or terms of service. You alone bear the consequences of how you
  use this tool — including potential account suspension. Consult NTU policy if you're unsure.
- **Your cookie stays local.** The `BbRouter` cookie is read locally on your machine and sent only to
  `ntulearn.ntu.edu.sg`. The author never sees it.
- **LMS data follows your MCP host's privacy settings.** The data this server returns to the MCP host
  (course content, announcements, grades, file metadata) is handled by that host like any other tool
  result. In hosted clients (e.g. Claude Desktop, Cursor), tool results are typically sent to the model
  provider as part of the conversation. Review your host's data-handling policy if that matters to you.
- **Don't share cookie values.** Anyone with your `BbRouter` can act as you on NTULearn until it expires.
- **Don't run this on someone else's behalf.** Each user should run their own instance against their own account.

The MIT license disclaims all warranties — see [LICENSE](LICENSE).
