# ntulearn-mcp

MCP server for **NTULearn** (NTU Singapore's Blackboard Learn instance). Lets Claude Desktop,
Claude Code, Prime Agent, Cursor, Cline, and other MCP hosts answer questions about your courses,
announcements, calendar, and grades — and organise course files into a folder hierarchy on disk.

The **shipped server is a single Rust binary** built on
[`ultrafast-mcp`](https://github.com/techgopal/ultrafast-mcp), with 21 tools, 2 prompt templates,
and a course-resource URI. No Python, no pip, no virtualenv.

---

## Quick start

```bash
cd rust
cargo build --release
# binary: rust/target/release/ntulearn-mcp
```

One-time cookie setup — just log in:

```bash
rust/target/release/ntulearn-mcp setup
```

`setup` opens NTULearn in a throwaway-profile browser (Chrome, Arc, Brave, Edge, or Chromium) and
watches the browser's own debugging channel for your `BbRouter` session cookie. You only log in in
the window that opens — no copy-paste, no devtools, no OS-keychain prompts. The captured cookie is
validated live against the API before it is saved to `<config>/ntulearn-mcp/cookie`. If no supported
browser is found (e.g. a headless server), it falls back to a one-time paste.

Verify:

```bash
rust/target/release/ntulearn-mcp check   # expect: Cookie source + Live validity : OK (200)
```

Register the **Rust binary** with your MCP host (adjust the path):

```bash
# e.g. Claude Code / Cursor / Cline — point the command at the Rust binary:
"/Users/you/ntulearn-mcp/rust/target/release/ntulearn-mcp"
```

### The server never touches your OS keychain

Cookie resolution is strictly: **`NTULEARN_COOKIE` env var → config file
(`<config>/ntulearn-mcp/cookie`) → read-only Firefox `cookies.sqlite`**. No keychain reads, no
password dialogs, no `security` commands. The `setup` command acquires a fresh cookie through a
throwaway browser's debugging port — keychain-free by construction.

### Session expiry

When NTULearn rejects a call with 401, the server re-resolves the cookie (env → config → Firefox),
**persists** a working value to the config file, and retries the call once. Refresh is never
proactive; it happens on a live 401 or when you run `ntulearn-mcp refresh`.

---

## Install from source

```bash
git clone https://github.com/gangula-karthik/ntulearn-mcp.git
cd ntulearn-mcp/rust
cargo build --release           # -> target/release/ntulearn-mcp
cargo test                      # 30 unit tests
target/release/ntulearn-mcp     # serve over stdio
```

## What it's for

Four prompts this server is built to make easy:

1. **"What announcements happened across my courses this week?"** → fans out across all enrolled courses, sorted newest first.
2. **"What assignments do I have due next week?"** → reads NTULearn's calendar, including gradable items (`type=GradebookColumn`).
3. **"Organise this semester's NTULearn content into `~/NTU/y3s1/sc2002/week 8/…` on my disk."** → walks the course tree and downloads files into a folder layout you describe in plain English.
4. **"Pull the assignment due dates and grading weightages out of this course briefing PDF."** → reads small text-heavy documents inline (no filesystem hop).

For multi-page, diagram-heavy lecture decks, **use `download_file` and drag the PDF into claude.ai**
— that path has a native vision rendering budget. MCP tool results are capped at 1 MB; this server
doesn't try to compete with drag-and-drop for full lecture decks.

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
| `ntulearn_read_file_content` | Read an attached file's content inline (no filesystem hop). |
| `ntulearn_list_messages` | List your NTULearn mailbox messages (inbox/sent). |
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

### Known environment limits (observed on real NTULearn)

- **`list_messages` / `read_message`** → Blackboard `/users/me/messages` returns **404 "API is not
  found"** on this NTU instance. The Messages REST API is not exposed; the tool returns a clean
  error (identical to the reference Python server). The tools remain for parity.
- **`get_group_members`** → returns 403 for student accounts unless the course exposes the data; an
  empty/error result is surfaced cleanly.
- **Calendar/upcoming windows** wider than ~16 weeks → NTULearn rejects with a 400 `courseErrors`
  entry; keep `since`/`until` within a semester.
- **`read_file_content`** extracts text from simple documents; for large/graphical PDFs prefer
  `download_file` and read the file in the client.

---

## Resources & Prompts

- **Resources** — `ntulearn://courses/{course_id}` returns a JSON course briefing (the same content
  as `ntulearn_summarize_course`). `list_resources` enumerates your enrolled courses, and the
  `{course_id}` URI template lets clients read any course directly.
- **Prompts** —
  - `ntulearn-weekly-brief` — args `courses` (optional comma-separated IDs) and `days` (default 7).
    Produces a prompt that chains `ntulearn_get_announcements` + `ntulearn_get_upcoming` over a
    computed since/until window.
  - `ntulearn-assignment-triage` — args `courses` and `days` (default 14). Produces a due-date
    triage prompt that chains `ntulearn_get_upcoming(type='GradebookColumn')` + `ntulearn_get_gradebook`.

## Example prompts

- *"What announcements went out across my courses this past week?"* — `get_announcements(since='...')`.
- *"What assignments do I have due in the next two weeks?"* — `get_upcoming(type='GradebookColumn')`.
- *"Show me the full calendar for the next 10 days."* — `get_upcoming(until='...')`.
- *"What's my current grade in `_12345_1`?"* — `get_gradebook(course_ids=['_12345_1'])`.
- *"Walk my enrolled courses and put each course's content under `~/NTU/y3s1/<course-name>/<topic>/…`."* — chains `list_courses` → `get_course_contents` → `download_file`.

## Authentication

The server resolves the `BbRouter` cookie strictly in this order:

1. **`NTULEARN_COOKIE` env var**
2. **Config file** at `<config>/ntulearn-mcp/cookie` (written by `setup`, `refresh`, or a successful
   401-refresh)
3. **Firefox** `cookies.sqlite` (read-only, plaintext — no keychain decryption)

Never proactive: the 401-refresh path runs only after a live 401. Run `ntulearn-mcp check` to see
the current source, expiry, and live validity.

### The `setup` command

`ntulearn-mcp setup` is the normal first-run path. It:

1. Checks for an existing cookie (env → config → Firefox) and validates it live. If valid: done.
2. Otherwise launches Chromium (Chrome/Arc/Brave/Edge) with a throwaway profile + local debugging
   port, opens NTULearn, and polls the DevTools protocol for the `BbRouter` cookie
   (15-minute login timeout). You just log in.
3. Validates the captured cookie live (`GET /learn/api/public/v1/users/me` must return 200) before
   saving it to the config file. A pre-login guest cookie is rejected, not saved.
4. Cleans up the browser window + profile automatically.

No supported browser → falls back to a one-time paste (accepts a bare value, `BbRouter=...`, or a
full `Cookie:` header).

### Manual cookie fallback

1. Open https://ntulearn.ntu.edu.sg in your browser and log in.
2. DevTools (`F12`) → **Application** → **Cookies** → `ntulearn.ntu.edu.sg`.
3. Copy the **Value** of the `BbRouter` cookie (starts with `expires:`).
4. Either run `ntulearn-mcp setup` and choose paste, or add it to your MCP host's `env` block:

   ```json
   {
     "mcpServers": {
       "ntulearn": {
         "command": "/Users/you/ntulearn-mcp/rust/target/release/ntulearn-mcp",
         "args": [],
         "env": { "NTULEARN_COOKIE": "expires:1234567890,id:..." }
       }
     }
   }
   ```

5. Restart your MCP host.

The cookie expires with your NTULearn session (days–weeks); re-run `setup` (or let the 401-refresh
resolve it) when it does.

---

## Optional configuration

| Env var | Default | Purpose |
|---|---|---|
| `NTULEARN_COOKIE` | — | Manual cookie fallback. |
| `NTULEARN_BASE_URL` | `https://ntulearn.ntu.edu.sg` | Change for a different Blackboard instance. |
| `NTULEARN_DOWNLOAD_DIR` | `./downloads` | Default `destination_dir` for `download_file` / `download_course`. |
| `NTULEARN_CACHE_DIR` | `~/.cache/ntulearn-mcp/cache.sqlite3` | SQLite persistence path for the response cache (best-effort; falls back to in-memory). |
| `NTULEARN_CACHE_MODE` | `readwrite` | `readwrite` (default), `readonly`, or `off`. |

## Development

```bash
cd rust
cargo test                 # 30 unit tests (cache, client, cookie, capture, setup)
cargo build --release
```

Project layout (`rust/crates/ntulearn-mcp/src/`):

```
main.rs      # ultrafast-mcp server wiring (stdio + setup/check/refresh CLI)
handlers.rs  # the 21 tool handlers
parsers.rs   # HTML body → download URL extraction (scraper)
render.rs    # markdown/csv/ics renderers
client.rs    # reqwest (HTTP/2, retries) Blackboard REST client + download
cache.rs     # SQLite + in-memory TTL cache (per-instance LRU, scoped keys)
cookie.rs    # layered cookie resolution (never keychain)
setup.rs     # setup / check / refresh subcommands
capture.rs   # throwaway-browser CDP cookie capture
resources.rs # course resource template + reader
prompts.rs   # prompt templates
schemas.rs   # auto-generated tool schemas
tools.rs     # tool definition registry
```

---

## Publishing / secret check

Before pushing to a remote, verify no credentials are in history or the tree:

```bash
git status --short   # no .env / downloads / cookie files staged
git log --all -p | grep -nE '(BbRouter=|expires:[0-9]{10,},id:|Set-Cookie|ghp_[A-Za-z0-9]{20,})' | grep -viE 'test|example|README' || echo "clean"
```

If anything real shows up, do **not** push — scrub it first (rotate the credential, remove it from
history with filter-repo or an interactive rebase).

---

## Troubleshooting

**"No NTULearn cookie found" / tools fail with 401.**
Run `ntulearn-mcp check` to see the cookie source and live validity, then `ntulearn-mcp setup` to
re-capture (or paste) a fresh cookie.

**MCP host lists "ntulearn" but tool calls return nothing.**
Make sure the server is started with the config file present (run `setup` once), and confirm it works
directly: `target/release/ntulearn-mcp check`.

**Prime Agent lists the server but tools aren't available.**
Run `/reload` in Prime Agent (or restart it) so settings are re-read.

**`read_file_content` returns "No download URL found" for an item.**
That content node is not a file (e.g. a text page or tool link). For real attached files use
`download_file` with a `destination_dir`.

**The server crashes on startup.**

```bash
rust/target/release/ntulearn-mcp
```

Most common cause: no cookie resolvable. The error message will guide you (or run `setup`).

---

## Disclaimer & responsible use

**Use at your own risk.** This is an unofficial, personal-use tool. It is **not** affiliated with,
endorsed by, or sponsored by NTU Singapore, Anthology Inc., or Blackboard Learn. NTULearn,
Blackboard, and related marks belong to their respective owners.

- **Your account, your responsibility.** Driving the LMS via your session cookie may be inconsistent
  with NTU's IT acceptable use policy or terms of service. Consult NTU policy if you're unsure.
- **Your cookie stays local.** The `BbRouter` cookie is read locally and sent only to
  `ntulearn.ntu.edu.sg`. The author never sees it.
- **Don't share cookie values.** Anyone with your `BbRouter` can act as you on NTULearn until it expires.
- **Don't run this on someone else's behalf.** Each user should run their own instance against their own account.

The MIT license disclaims all warranties — see [LICENSE](LICENSE).
