# ntulearn-mcp

MCP server for **NTULearn** (NTU Singapore's Blackboard Learn instance). Lets Claude Desktop, Claude Code, Cursor, Cline, and other MCP hosts answer questions about your courses, announcements, calendar, and grades — and organise course files into a folder hierarchy on your disk.

Built for tech-inclined NTU students. Requires Python 3.12+ and `uv`.

---

## What it's for

Four prompts this server is built to make easy:

1. **"What announcements happened across my courses this week?"** → fans out across all enrolled courses, sorted newest first.
2. **"What assignments do I have due next week?"** → reads NTULearn's calendar, including gradable items (`type=GradebookColumn`).
3. **"Organise this semester's NTULearn content into `~/NTU/y3s1/sc2002/week 8/…` on my disk."** → walks the course tree and downloads files into a folder layout you describe in plain English.
4. **"Pull the assignment due dates and grading weightages out of this course briefing PDF."** → reads small text-heavy PDFs / Office docs inline (no filesystem hop).

For multi-page, diagram-heavy lecture decks, **use `download_file` and drag the PDF into claude.ai** — that path has a 32 MB budget and native vision rendering. MCP tool results are capped at 1 MB; this server doesn't try to compete with drag-and-drop for full lecture decks.

---

## Quick start

### 1. Install `uv`

```bash
# macOS / Linux
curl -LsSf https://astral.sh/uv/install.sh | sh

# Windows (PowerShell)
irm https://astral.sh/uv/install.ps1 | iex
```

### 2. Log into NTULearn in your browser

Open https://ntulearn.ntu.edu.sg in **Chrome, Edge, Firefox, or Brave** and log in. The server reads your `BbRouter` cookie automatically from whichever browser has a fresh session — no copy-paste required on most setups.

### 3. Add the server to your MCP host

**Claude Desktop** — edit `claude_desktop_config.json`:

- macOS: `~/Library/Application Support/Claude/claude_desktop_config.json`
- Windows: `%APPDATA%\Claude\claude_desktop_config.json`

```json
{
  "mcpServers": {
    "ntulearn": {
      "command": "uvx",
      "args": ["ntulearn-mcp"]
    }
  }
}
```

**Claude Code:**

```bash
claude mcp add ntulearn -- uvx ntulearn-mcp
```

**Cursor** — edit `~/.cursor/mcp.json` with the same shape as Claude Desktop above.

### 4. Restart your MCP host, then try it

Ask Claude: *"What's due in NTULearn over the next two weeks?"*

That's the whole flow for most users. Read [Authentication](#authentication) if you're on Windows with Chrome/Edge, or if the call doesn't return anything.

---

## Tools

8 tools. Most do cross-course aggregation by default — you almost never need to pass course IDs by hand.

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

When your session expires mid-conversation, the server catches the 401, invalidates the cache, re-reads from your browser, and retries the call once. If your browser still has a fresh session, this is invisible.

### Platform support for auto-read

| Platform | Browser | Auto-read | Notes |
|---|---|---|---|
| macOS | any | ✅ | One-time Keychain prompt — see [macOS first-time setup](#macos-first-time-setup) |
| Linux | any | ✅ | May prompt for keyring unlock on Chromium |
| Windows | Firefox | ✅ | |
| Windows | Chrome / Edge | ❌ | Blocked by [App-Bound Encryption](https://security.googleblog.com/2024/07/improving-security-of-chrome-cookies-on.html) — use [manual fallback](#manual-cookie-fallback) |

**Windows + Chrome/Edge users:** Chrome's ABE (rolled out 2024) prevents non-admin processes from reading cookies. Don't elevate Claude Desktop to admin to work around this — it elevates everything else too. Use the manual cookie fallback below, or switch to Firefox for NTULearn.

### macOS first-time setup

The first time the server reads cookies from a Chromium browser on macOS, you'll see:

> *"uv wants to access key 'Chrome' in your keychain"*

Click **Always Allow** and enter your macOS login password. You won't see the prompt again.

**If the prompt doesn't appear** (it can be suppressed when the MCP server runs as a child of Claude Desktop), bootstrap the approval from your own Terminal:

```bash
uvx --from ntulearn-mcp python -c "from ntulearn_mcp.cookie import read_bbrouter_cookie; print(read_bbrouter_cookie() or 'no cookie found')"
```

The Keychain dialog will appear in front of Terminal. Approve it, then your MCP host will work afterwards.

### Manual cookie fallback

If auto-read doesn't work for you:

1. Open https://ntulearn.ntu.edu.sg in your browser and log in.
2. Open DevTools (`F12`) → **Application** → **Cookies** → `ntulearn.ntu.edu.sg`.
3. Copy the **Value** of the `BbRouter` cookie (starts with `expires:`).
4. Add it to your MCP config:

   ```json
   {
     "mcpServers": {
       "ntulearn": {
         "command": "uvx",
         "args": ["ntulearn-mcp"],
         "env": {
           "NTULEARN_COOKIE": "expires:1234567890,id:..."
         }
       }
     }
   }
   ```

5. Restart your MCP host.

The cookie expires with your NTULearn session (days–weeks). When it does, repeat from step 1. As of 0.2.0 the env var is a **fallback**, not an override — if a browser auto-read succeeds, the fresh browser value wins.

---

## Optional configuration

| Env var | Default | Purpose |
|---|---|---|
| `NTULEARN_COOKIE` | (auto-read) | Manual cookie fallback. |
| `NTULEARN_BASE_URL` | `https://ntulearn.ntu.edu.sg` | Change for a different Blackboard instance. |
| `NTULEARN_DOWNLOAD_DIR` | `./downloads` | Default `destination_dir` for `download_file` when no per-call value is passed. |

Set these in your MCP host's `env` block (same place as `NTULEARN_COOKIE` above).

---

## Troubleshooting

**"No NTULearn cookie found" / tools fail with 401.**
Make sure you're logged into NTULearn in a supported browser. If you're on Windows + Chrome/Edge, set `NTULEARN_COOKIE` per the [manual fallback](#manual-cookie-fallback).

**MCP host lists "ntulearn" but the tool calls hang or return nothing.**
On macOS, the first call may be blocked on a hidden Keychain prompt. See [macOS first-time setup](#macos-first-time-setup).

**`read_file_content` returns "would exceed batch cap" / nothing useful for a big PDF.**
That's expected for multi-page lecture decks. Use `download_file` (with a `destination_dir` if you want it organised) and drag the resulting file into claude.ai for full-fidelity reading. `read_file_content` is for small documents (briefs, tutorials) you want to ask questions about inline.

**The server crashes on startup.**
Run it directly to see the error:

```bash
uvx ntulearn-mcp
```

Most common cause: no cookie resolvable. The error message will guide you.

**Auto-read worked yesterday, doesn't work today.**
Your browser session probably expired. Open NTULearn in your browser, complete SSO + MFA, then retry — auto-refresh handles the rest.

---

## Contributing

```bash
git clone https://github.com/kingdonkatsu/ntulearn-mcp.git
cd ntulearn-mcp
uv sync                                          # install deps incl. dev
uv run python -m unittest discover -s tests      # run tests
uv run ntulearn-mcp                              # run the server (stdio)
uv run mcp dev src/ntulearn_mcp/server.py        # interactive tool inspector
```

Project layout:

```
src/ntulearn_mcp/
├── server.py     # MCP entrypoint, tool handlers, cookie resolution
├── client.py     # async httpx-based Blackboard REST client
├── cookie.py     # browser cookie auto-read
├── cache.py      # last-known-good cookie cache (OS keychain)
└── parsers.py    # HTML body → download URL extraction
```

Tests use `unittest` (not pytest); HTTP is mocked via `httpx.MockTransport`.

---

## Disclaimer & responsible use

**Use at your own risk.** This is an unofficial, personal-use tool. It is **not** affiliated with, endorsed by, or sponsored by NTU Singapore, Anthology Inc., or Blackboard Learn. NTULearn, Blackboard, and related marks belong to their respective owners.

- **Your account, your responsibility.** Driving the LMS via your session cookie may be inconsistent with NTU's IT acceptable use policy or terms of service. You alone bear the consequences of how you use this tool — including potential account suspension. Consult NTU policy if you're unsure.
- **Your cookie stays local.** The `BbRouter` cookie is read locally on your machine and sent only to `ntulearn.ntu.edu.sg`. The author never sees it.
- **LMS data follows your MCP host's privacy settings.** The data this server returns to the MCP host (course content, announcements, grades, file metadata) is handled by that host like any other tool result. In hosted clients (e.g. Claude Desktop, Cursor), tool results are typically sent to the model provider as part of the conversation. Review your host's data-handling policy if that matters to you.
- **Don't share cookie values.** Anyone with your `BbRouter` can act as you on NTULearn until it expires.
- **Don't run this on someone else's behalf.** Each user should run their own instance against their own account.

The MIT license disclaims all warranties — see [LICENSE](LICENSE).
