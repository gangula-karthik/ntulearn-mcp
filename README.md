<p align="center">
  <img src="assets/banner.webp" alt="ntulearn-mcp — your NTULearn courses, announcements and grades, inside your agent" width="100%">
</p>

# ntulearn-mcp

**MCP server + agent-first CLI for NTULearn** — NTU Singapore's Blackboard Learn instance.
Point any MCP host (Claude Desktop, Claude Code, Cursor, Cline, Prime Agent) at it to ask about your
courses, announcements, calendar, and grades — and to organise course files into a folder hierarchy
on disk.

The shipped server is a **single Rust binary** built on
[`ultrafast-mcp`](https://github.com/techgopal/ultrafast-mcp), with 21 tools, 2 prompt templates, and
a course-resource URI. No Python, no pip, no virtualenv.

**Website:** <https://ntulearn-mcp.krtk.app>

---

## Quick start

```bash
git clone https://github.com/gangula-karthik/ntulearn-mcp.git
cd ntulearn-mcp/rust
cargo build --release          # binary: rust/target/release/ntulearn-mcp
```

Log in once — `setup` opens NTULearn in a dedicated browser window and captures your session for
you. No copy-paste, no devtools, no keychain/password prompts. After that the server renews the
session silently in the background. (No terminal? Ask your agent to call `ntulearn_login`.)

```bash
rust/target/release/ntulearn-mcp setup
rust/target/release/ntulearn-mcp check   # expect: Live validity : OK (200)
```

Register the binary with your MCP host:

```json
{
  "mcpServers": {
    "ntulearn": {
      "command": "/Users/you/ntulearn-mcp/rust/target/release/ntulearn-mcp",
      "args": []
    }
  }
}
```

That's it. The same binary serves MCP with no arguments and acts as a CLI with a command.

---

## Why the CLI: about 64x fewer tokens

MCP hosts inject every tool's JSON schema into context **every turn** — about 6.6k tokens for this
server's 21 tools. The same binary doubles as a plain CLI, whose terse tabular output an agent reads
in a few hundred tokens:

```bash
ntulearn-mcp courses                 # 6 courses ≈ 100 tokens (MCP tool result: ≈ 450)
ntulearn-mcp tree _2706844_1         # a course's content tree
ntulearn-mcp whats-new --days 7      # announcements + due dates + grades digest
ntulearn-mcp multi "courses" "grades" "upcoming --type GradebookColumn"   # concurrent
```

Measured with the built-in benchmark (`ntulearn-mcp selfbench`):

| Metric | MCP | CLI | Improvement |
|---|---|---|---|
| Discovery tokens (per conversation) | 6,639 (every turn) | 586 (once, on demand) | **11.3x** |
| Task output tokens (4-task suite) | 2,193 | 492 (compact TSV) | **4.5x** |
| 10-turn conversation model | ~68,583 tok | ~1,078 tok | **63.6x** |
| `courses` cold (2 RTTs, 25ms mock) | — | 59ms | **3.4x** vs v0.3 |
| `courses` warm (SQLite cache) | — | 82µs | — |
| 4 commands concurrently (`multi`) | 225ms sequential | 59ms (singleflight-deduped) | **3.8x** |

The CLI shares the same handlers, cache, and cookie lifecycle as the MCP server. Point your agent at
the binary and have it run `ntulearn-mcp help` (≈590 tokens, once) instead of registering 21 tools.

---

## What people ask for

1. **"What announcements happened across my courses this week?"** — fans out across all enrolled courses, newest first.
2. **"What assignments are due next week?"** — reads the calendar, including gradable items.
3. **"Organise this semester's content into `~/NTU/y3s1/sc2002/week 8/…`."** — walks the course tree and downloads into any folder layout you describe.
4. **"Pull the due dates and grade weightages out of this course briefing PDF."** — reads small text-heavy documents inline.

For multi-page, diagram-heavy lecture decks, use `download_file` and open the PDF in your client —
MCP tool results are capped at 1 MB, so this server does not compete with drag-and-drop for full decks.

---

## CLI reference

`ntulearn-mcp help` prints this list; `ntulearn-mcp help <cmd>` shows one command's flags.

```
courses                                  my enrolled courses (run this first for course ids)
contents     course-id                   one level of a course's content tree
tree         course-id                   entire content tree of a course in one call
search       course-id query             search one course's content tree
find         query                       search content across ALL courses
dl           course-id content-id        download a content item's files to disk
read         course-id content-id        read a content item's files inline (pdf/office/text)
upcoming                                 calendar items + due dates across courses
announcements                            announcements across courses, newest first
grades                                   gradebook columns + your grades across courses
attempts     course-id column-id         attempts on one gradebook column
messages                                 course messages (inbox/sent)
message      message-id                  read one course message
users        course-id                   users enrolled in a course
groups       course-id                   groups in a course
members      course-id group-id          members of one group
summary      course-id                   one-shot course digest (staff, contents, dates)
whats-new                                curated digest of everything new since a cutoff
dl-course    course-id                   bulk-download a course's files
ics                                      export calendar items to an .ics file
grades-csv                               export gradebook to .csv
```

Output is **compact TSV** by default. Flags: `--json`, `--md`, `--toon`, `--fields a,b`,
`--max-chars N`, `--full`, `--limit N`, `--offset N`. Every MCP tool argument is also accepted as a
kebab-case flag (`--include-disabled`, `--course-ids a,b`, `--mode vision`).

`multi` runs several commands concurrently in one process, deduplicating identical in-flight HTTP
fetches and sharing one cache:

```bash
ntulearn-mcp multi "courses" "announcements --since 2026-01-01" "grades"
# results arrive concatenated, each prefixed with: ===== <command> =====
```

### Output formats

| Format | Flag | Notes |
|---|---|---|
| Compact TSV | default | Smallest; field-capped, no nesting. |
| Markdown | `--md` | Human-readable. |
| JSON | `--json` | Raw payload. |
| TOON | `--toon` | Smallest **full-fidelity** format — **40% fewer tokens** than pretty JSON (52% on large lists). |

TOON ([Token-Oriented Object Notation](https://github.com/toon-format/toon), spec v4.1) declares
array shapes once and drops repeated keys, so the savings scale with how tabular the payload is.
Every MCP tool accepts `response_format: "toon"` too.

---

## Tools

21 tools, most doing cross-course aggregation by default — you rarely pass course IDs by hand.

| Tool | What it does |
|---|---|
| `ntulearn_list_courses` | List enrolled courses. |
| `ntulearn_get_course_contents` | Walk a course's content tree. Omit `parent_id` for the top level; pass it to drill into a folder. |
| `ntulearn_search_course_content` | Recursive substring search within one course. |
| `ntulearn_get_upcoming` | Calendar items across enrolled courses. Defaults to the next 2 weeks. `type='GradebookColumn'` filters to assignments. |
| `ntulearn_get_announcements` | Announcements across enrolled courses, newest first. Optional `since`. |
| `ntulearn_get_gradebook` | Gradebook columns across enrolled courses, with your scores when available. |
| `ntulearn_download_file` | Download every file on a content item to disk. `destination_dir` builds hierarchies. |
| `ntulearn_read_file_content` | Read an attached file's content inline (no filesystem hop). |
| `ntulearn_list_messages` | List mailbox messages (inbox/sent). |
| `ntulearn_read_message` | Read one message by ID, with full body and recipients. |
| `ntulearn_list_course_users` | List users in a course (instructors, TAs, students). |
| `ntulearn_list_course_groups` | List the groups defined in a course. |
| `ntulearn_get_group_members` | List the members of a course group. |
| `ntulearn_get_gradebook_attempts` | List submission attempts for an assignment. |
| `ntulearn_search_all_courses` | Search content across **all** courses; results carry courseId + breadcrumb. |
| `ntulearn_get_content_tree` | One course's entire content tree as nested JSON (bounded by `max_depth`). |
| `ntulearn_download_course` | Recursively download every file in a course to `~/Downloads/NTU/<course>`. |
| `ntulearn_whats_new` | One-call digest: announcements + upcoming + gradebook since a cutoff. |
| `ntulearn_export_calendar_ics` | Export calendar items as an iCalendar `.ics` string. |
| `ntulearn_export_gradebook_csv` | Export your gradebook as a CSV string. |
| `ntulearn_summarize_course` | Briefing for one course: instructors, upcoming, announcements, grades, top folders. |

Read-only tools default to `response_format='json'`; pass `'markdown'` for a readable summary.
List-returning tools accept `limit`/`offset`. A resource URI,
`ntulearn://courses/{course_id}`, returns a JSON course briefing, and two prompts
(`ntulearn-weekly-brief`, `ntulearn-assignment-triage`) chain the tools for you.

### Known environment limits

- **`list_messages` / `read_message`** — the public `/users/me/messages` REST API returns 404 on this
  instance, so the client walks the internal v1 conversation API instead (one cached mailbox flatten
  → per-course conversations) and derives recipients from the conversation.
- **`get_group_members`** — the public groups endpoint returns 403 for student accounts, so the
  client uses the internal v1 memberships endpoint instead.
- **Calendar windows wider than ~16 weeks** are rejected by NTULearn with a 400 `courseErrors` entry
  — keep `since`/`until` within a semester.
- **`read_file_content`** extracts text from simple documents; for large or graphical PDFs, use
  `download_file` and open the file in your client.

---

## Authentication

The `BbRouter` cookie is resolved strictly in this order:

1. `NTULEARN_COOKIE` environment variable
2. Config file at `<config>/ntulearn-mcp/cookie`
3. Firefox `cookies.sqlite` (read-only, plaintext)

**No keychain access, ever** — no password dialogs, no `security` commands. The capture browser is
launched with `--use-mock-keychain --password-store=basic`, so it never asks macOS Keychain, GNOME
Keyring or KWallet for anything.

`setup` (or the `ntulearn_login` tool) is the first-run path. It works with **any** Chromium-based
browser you have installed — Chrome, Edge, Brave, Arc, Vivaldi, Opera, Chromium — regardless of
which browser you normally use, and never touches your real browser profile. It opens NTULearn in a
dedicated profile (`<config>/ntulearn-mcp/capture-profile-v2`), waits for you to log in, validates
the cookie live, and saves it. Answer **Yes** to "Stay signed in?".

After that, sessions renew themselves:

- NTULearn cookies last ~3 h. Before one expires — or when a call hits 401 — the server re-logs in
  through the saved NTU SSO session in a **headless** browser: no window, no password.
- SSO session cookies are re-saved with a 30-day lifetime in the dedicated profile, so the silent
  re-login keeps working across browser and server restarts (NTU's IdP still enforces its own limit).
- When even the SSO session has ended, the login window opens **by itself** in the background (only
  on machines that have logged in through the capture flow before — a fresh install never spawns a
  window unasked). The failing tool returns "a browser login window has just opened"; log in there,
  and a retry of the same request succeeds — the fresh session is hot-swapped in the moment it lands.
  At most one window is ever open, no matter how many tools fail while you are away.
- Browsers never pile up: capture is single-flight **machine-wide** (a cross-process lock covers
  multiple MCP hosts and the CLI at once), orphaned capture browsers from a killed server are
  reaped automatically, an unattended login window is not re-opened for 30 min, and when the SSO
  session is dead all silent sign-in retries back off for 6 h instead of launching headless
  browsers that cannot succeed.

Each laptop needs its own one-time login. With no Chromium browser at all (or set
`NTULEARN_BROWSER` to a binary path), `setup` falls back to a one-time paste.

<details>
<summary>Manual cookie fallback</summary>

1. Log in at <https://ntulearn.ntu.edu.sg>.
2. DevTools (`F12`) → **Application** → **Cookies** → `ntulearn.ntu.edu.sg`.
3. Copy the **Value** of the `BbRouter` cookie (starts with `expires:`).
4. Run `ntulearn-mcp setup` and choose paste, or set it in your MCP host's `env` block:

   ```json
   { "env": { "NTULEARN_COOKIE": "expires:1234567890,id:..." } }
   ```

5. Restart your MCP host.

</details>

---

## Configuration

All optional — the defaults work for most users.

| Env var | Default | Purpose |
|---|---|---|
| `NTULEARN_COOKIE` | — | Manual cookie fallback. |
| `NTULEARN_BROWSER` | auto-detect | Path to a Chromium-based browser binary for login capture. |
| `NTULEARN_BASE_URL` | `https://ntulearn.ntu.edu.sg` | Change for a different Blackboard instance. |
| `NTULEARN_DOWNLOAD_DIR` | `./downloads` | Default `destination_dir` for `download_file` / `download_course`. |
| `NTULEARN_CACHE_DIR` | `~/.cache/ntulearn-mcp/cache.sqlite3` | SQLite response cache (falls back to in-memory). |
| `NTULEARN_CACHE_MODE` | `readwrite` | `readwrite`, `readonly`, or `off`. |
| `NTULEARN_MCP_AUTOUPDATE` | `ask` | `on`, `off`, or `ask`. |
| `NTULEARN_MCP_NO_UPDATE_CHECK` | — | Set to `1` to disable update checks. |
| `NTULEARN_MCP_UPDATE_INTERVAL_SECS` | 86400 | Check throttle. |
| `NTULEARN_MCP_REPO` | upstream repo | Override for forks. |
| `NTULEARN_MCP_REQUIRE_SIGNATURE` | — | Set to `1` to refuse unsigned releases. |

---

## Updates

The binary checks GitHub for a newer release at most once every 24 hours, and never blocks or breaks
the server. When one is available you get a one-line notice in the MCP host's log and at the start
of the session.

```bash
ntulearn-mcp update            # check + prompt before installing
ntulearn-mcp update --check    # exit 0 = current, 10 = update available
ntulearn-mcp update --yes      # install without prompting
ntulearn-mcp update --auto on  # background-install new releases
ntulearn-mcp update --channel beta   # try prereleases first
```

With `--auto on` the server downloads the release in the background, verifies its **sha256 against
the release's `checksums.txt`**, and atomically swaps the binary — the new version activates when
your MCP host next restarts the server. Every `checksums.txt` is **minisign-signed** by CI, and the
public key is committed as [`minisign.pub`](minisign.pub) and baked into the binary, so a tampered
release is refused. Set `require_signature: true` in `settings.json` to refuse unsigned releases outright. Installs the binary did not make itself (cargo, Homebrew, dev builds) are
detected and answered with the right update command instead of a self-replace. A Homebrew formula
template lives in [`packaging/homebrew/`](packaging/homebrew/).

**Cutting a release** (maintainers): bump `rust/Cargo.toml`, tag `v<version>`, push. The workflow
builds Linux, macOS (arm64 + x64), and Windows binaries, and publishes `checksums.txt` with its
minisign signature. Tags with a hyphen (e.g. `v0.8.0-rc.1`) publish as prereleases automatically.

---

## Troubleshooting

- **"Not logged in to NTULearn", or tools fail with 401** — ask your agent to call `ntulearn_login`,
  or run `ntulearn-mcp setup`. `ntulearn-mcp check` shows the cookie source and live validity.
- **A macOS "wants to use your confidential information" prompt** — that came from versions before
  the mock-keychain fix; update, click Deny, and log in once more with `setup`.
- **MCP host lists the server but tool calls return nothing** — run
  `rust/target/release/ntulearn-mcp` directly. The usual cause is an unresolvable cookie; the error
  message will say so.
- **Prime Agent lists the server but tools aren't available** — run `/reload` (or restart it) so
  settings are re-read.
- **`read_file_content` returns "No download URL found"** — that content node is not a file (a text
  page or tool link, say). Use `download_file` for real attached files.

---

## Development

```bash
cd rust
cargo test                              # 53 unit tests
cargo build --release
target/release/ntulearn-mcp selfbench   # token + latency + concurrency benchmark
```

Layout (`rust/crates/ntulearn-mcp/src/`):

```
main.rs      no args → MCP stdio server; command → CLI; setup/check/refresh
cli.rs       the 21 commands, compact TSV renderer, `multi` concurrency
handlers.rs  the 21 tool handlers (shared by MCP server and CLI)
bench.rs     selfbench + mock Blackboard API
client.rs    reqwest (HTTP/2, retries, parallel fan-out, singleflight dedup)
cache.rs     SQLite (WAL, persistent conn) + in-memory TTL cache
cookie.rs    layered cookie resolution (never keychain)
setup.rs     setup / check / refresh subcommands
capture.rs   throwaway-browser CDP cookie capture
parsers.rs   HTML body → download URL extraction
render.rs    markdown / csv / ics renderers
resources.rs course resource template + reader
prompts.rs   prompt templates
schemas.rs   tool schemas
tools.rs     tool definition registry
```

Before pushing, check that no credential is staged or in history:

```bash
git status --short
git log --all -p | grep -nE '(BbRouter=|expires:[0-9]{10,},id:|Set-Cookie|ghp_[A-Za-z0-9]{20,})' | grep -viE 'test|example|README' || echo "clean"
```

If anything real shows up, do **not** push. Rotate the credential first.

---

## Disclaimer

**Use at your own risk.** This is an unofficial, personal-use tool. It is **not** affiliated with,
endorsed by, or sponsored by NTU Singapore, Anthology Inc., or Blackboard Learn.

- **Your account, your responsibility.** Driving the LMS with your session cookie may be
  inconsistent with NTU's acceptable use policy. Check NTU policy if you are unsure.
- **Your cookie stays local.** It is read locally and sent only to `ntulearn.ntu.edu.sg`.
- **Don't share cookie values.** Anyone with your `BbRouter` can act as you until it expires.
- **Don't run this for someone else.** Each user should run their own instance against their own account.

MIT licensed — see [LICENSE](LICENSE). All warranties are disclaimed.
