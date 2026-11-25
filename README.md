<p align="center">
  <img src="assets/banner.webp" alt="ntulearn-mcp — NTULearn MCP server: courses, announcements, grades, calendar and Kaltura lecture transcripts for AI agents" width="100%">
</p>

# ntulearn-mcp — NTULearn (NTU Blackboard Learn) MCP Server & AI Agent CLI

**ntulearn-mcp** is a [Model Context Protocol](https://modelcontextprotocol.io) (MCP) server and
agent-first CLI for **NTULearn**, Nanyang Technological University's Blackboard Learn instance.
Connect it to any MCP host — **Claude Desktop, Claude Code, Cursor, Cline, Prime Agent, or any
LLM agent** — and ask about your **courses, announcements, assignments and due dates, calendar,
grades and gradebook, course messages, class roster and groups, lecture files and content trees,
Kaltura lecture video transcripts, FeedbackFruits and Peerceptiv peer evaluations**, and export
your calendar to `.ics` and your gradebook to `.csv`.

One small self-contained Rust binary. No Python, no pip, no virtualenv. Built-in minimal MCP
stdio implementation, **34 MCP tools**, 2 prompt templates, 1 course resource URI — and the same
binary doubles as a **token-efficient command-line CLI** so your agent reads results in a few
hundred tokens instead of a few thousand.

**Website:** <https://ntulearn.krtk.app> · Rust source: [`rust/crates/ntulearn-mcp`](rust/crates/ntulearn-mcp)

## What you can do with it

- **"What announcements happened across my courses this week?"** — fans out across every enrolled
  course concurrently, newest first.
- **"What assignments are due next week?"** — reads the calendar including gradable items and
  gradebook columns.
- **"What are my grades so far?"** — gradebook columns and scores across all courses, plus per-assignment
  submission attempts; exportable to CSV.
- **"Download lecture 6's transcript"** — Kaltura caption tracks saved as `.vtt` + timestamp-stripped
  `.txt` with word counts, or "search every lecture for 'carbon tax'" in one call per course.
- **"Download this week's lecture slides"** — walk a course's content tree, download files to any
  folder hierarchy you describe (`~/NTU/y3s1/sc2002/week 8/…`), or bulk-download a whole course.
- **"How's my Peerceptiv / FeedbackFruits peer review going?"** — deadlines, your progress per
  teammate, the rubric, reviews given and (once released) received; submit ratings and comments,
  dry-run by default.
- **"Who's in my project group?"** — course roster, groups and group members.
- **"Read my course messages"** — inbox and sent, full bodies, no browser needed.
- **"Give me a course briefing"** — one-shot digest: instructors, upcoming dates, announcements,
  grades, top folders. Or a whole-semester "what's new" digest since any cutoff.

## Quick start

```bash
git clone https://github.com/gangula-karthik/ntulearn-mcp.git
cd ntulearn-mcp/rust
cargo build --release          # binary: rust/target/release/ntulearn-mcp
rust/target/release/ntulearn-mcp setup    # opens NTULearn, captures your session
rust/target/release/ntulearn-mcp check    # expect: Live validity : OK (200)
```

Register with your MCP host:

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

That's it. The binary serves MCP over stdio with no arguments and acts as a CLI with a command.
`setup` opens NTULearn in a dedicated browser window and captures your session — no copy-paste,
no devtools — after which sessions renew silently in the background. No terminal? Ask your agent
to call the `ntulearn_login` tool.

## MCP tools (34)

Most tools aggregate across all your enrolled courses by default — you rarely pass course IDs.

| Tool | What it does |
|---|---|
| `ntulearn_list_courses` | List enrolled courses (run this first for course IDs). |
| `ntulearn_get_course_contents` | One level of a course's content tree; `parent_id` drills into folders. |
| `ntulearn_get_content_tree` | A course's entire content tree as nested JSON (`max_depth` bounded). |
| `ntulearn_search_course_content` | Recursive substring search within one course. |
| `ntulearn_search_all_courses` | Search content across **all** courses with breadcrumbs. |
| `ntulearn_download_file` | Download a content item's files to disk; `destination_dir` builds hierarchies. |
| `ntulearn_download_course` | Bulk-download every file in a course. |
| `ntulearn_read_file_content` | Read an attached file inline — PDF, Office, plain text. |
| `ntulearn_get_upcoming` | Calendar items and due dates across courses (`type='GradebookColumn'` filters to assignments). |
| `ntulearn_get_announcements` | Announcements across courses, newest first, optional `since`. |
| `ntulearn_get_gradebook` | Gradebook columns and your scores across courses. |
| `ntulearn_get_gradebook_attempts` | Submission attempts for one assignment. |
| `ntulearn_whats_new` | One-call digest: announcements + upcoming + gradebook since a cutoff. |
| `ntulearn_summarize_course` | Course briefing: instructors, dates, announcements, grades, top folders. |
| `ntulearn_export_calendar_ics` | Export calendar items as an iCalendar `.ics` string. |
| `ntulearn_export_gradebook_csv` | Export your gradebook as CSV. |
| `ntulearn_list_messages` | Course messages (inbox/sent). |
| `ntulearn_read_message` | Read one message with full body and recipients. |
| `ntulearn_list_course_users` | Roster: instructors, TAs, students. |
| `ntulearn_list_course_groups` | Groups defined in a course. |
| `ntulearn_get_group_members` | Members of one group. |
| `ntulearn_fbf_status` | FeedbackFruits peer evaluation: deadlines, your progress, grade release. |
| `ntulearn_fbf_reviews` | Your peer reviews — given (`direction='given'`) or received (empty until released). |
| `ntulearn_fbf_rate` | Set one rubric rating for a teammate (upsert, dry-run by default). |
| `ntulearn_fbf_comment` | Write the "Overall comments" review for a teammate (dry-run by default). |
| `ntulearn_pcv_status` | Peerceptiv evaluation: assignment status, progress per teammate, rubric. |
| `ntulearn_pcv_evals` | Peerceptiv reviews given / received (received 403s until the instructor releases). |
| `ntulearn_pcv_rate` | Set one rubric rating — accepts a level description ("Always") or number (9). |
| `ntulearn_pcv_comment` | Write the comment prompts for a teammate. |
| `ntulearn_pcv_autofill` | Fill a whole peer evaluation: full marks + good comments (dry-run default). |
| `ntulearn_kaltura_list` | Enumerate Kaltura-embedded videos in a course (titles, entry IDs). |
| `ntulearn_kaltura_transcripts` | Download caption tracks as `.vtt` + `.txt` with word counts — one video or a whole course. |
| `ntulearn_kaltura_download` | Download a Kaltura video as MP4 from the open CDN (`smallest`/`best`/`original`). |
| `ntulearn_login` | Interactive browser login when the session expires. |

Plus: two prompt templates (`ntulearn-weekly-brief`, `ntulearn-assignment-triage`) that chain the
tools for you, and a resource URI `ntulearn://courses/{course_id}` returning a JSON course briefing.
Read-only tools default to the token-optimized TOON format; pass `response_format='json'` or
`'markdown'` when you want those instead.

## Why the CLI: about 64x fewer tokens

MCP hosts inject every tool's schema into context **every turn** (~8.8k tokens for 34 tools). The
same binary as a plain CLI prints compact TSV your agent reads in a few hundred tokens:

```bash
ntulearn-mcp courses                 # 6 courses ≈ 100 tokens (MCP tool result: ≈ 450)
ntulearn-mcp tree _2706844_1         # a course's content tree in one call
ntulearn-mcp whats-new --days 7      # announcements + due dates + grades digest
ntulearn-mcp multi "courses" "grades" "upcoming --type GradebookColumn"   # concurrent
```

Measured by the built-in `selfbench` benchmark: discovery 11.3x cheaper, task output 4.5x
cheaper, a 10-turn conversation ~65x cheaper. HTTP fan-out is concurrent (48 in flight,
singleflight-deduped, SQLite-cached): `whats_new` across 6 courses takes 60 ms instead of 703 ms;
a 403-node content tree walks in 0.13 s instead of 2.3 s.

## CLI reference

`ntulearn-mcp help` prints the live list; `ntulearn-mcp help <cmd>` shows one command's flags.
Lifecycle commands: `setup`, `check`, `refresh`, `update` (`--check/--yes/--auto/--status`),
`version`, `selfbench`, `multi` (concurrent commands in one process), `help`.

```
courses | contents | tree | search | find | dl | read | dl-course
upcoming | announcements | grades | attempts | ics | grades-csv | whats-new | summary
messages | message | users | groups | members
fbf | fbf-reviews | fbf-rate | fbf-comment
pcv | pcv-evals | pcv-rate | pcv-comment | pcv-auto
kaltura-list | kaltura-transcripts | kaltura-download
```

Output formats: compact TSV (default), `--json`, `--md`, `--toon`
([Token-Oriented Object Notation](https://github.com/toon-format/toon) — 40% smaller than pretty
JSON, the MCP default since v0.7.2). Trim with `--fields a,b`, `--limit N`, `--offset N`,
`--max-chars N`, `--full`. Every MCP tool argument works as a kebab-case flag
(`--course-ids a,b`, `--mode vision`, `--dry-run=false`).

## LTI integrations: Kaltura, FeedbackFruits, Peerceptiv

These features are invisible to the plain Blackboard REST API — ntulearn-mcp launches each
provider's LTI 1.3 chain with your existing session and talks to the provider's API directly:

- **Kaltura lecture videos** — discover every embed in a course, download caption tracks
  (`.vtt` + `.txt`, per-video word counts; videos without captions are notes, not errors) and
  stream MP4s from the open CDN (a 540p lecture ≈ 150 MB, originals to 1.7 GB). Protocol details
  and live-verification notes: [KALTURA_SPEC.md](docs/KALTURA_SPEC.md).
- **FeedbackFruits peer evaluations** — status, deadlines, your submitted ratings and, after the
  instructor publishes grades, received ratings (reported as `not yet released` before that).
- **Peerceptiv peer evaluations** — the same surface, plus `pcv-auto` to fill an entire
  evaluation in one call. Write tools default to `dry_run=true`.

## Authentication

The `BbRouter` session cookie is resolved in order: `NTULEARN_COOKIE` env var → config file →
Firefox `cookies.sqlite` (read-only). **No keychain access, ever** — the capture browser runs with
a mock keychain. `setup` opens NTULearn in a dedicated profile of any installed Chromium-based
browser (never your real profile), waits for the login, validates live, and saves it. Afterwards:
NTULearn cookies (~3 h) renew via headless re-login through the saved NTU SSO session; if even
SSO has ended, a login window opens by itself and the next retry succeeds. No Chromium browser at
all? `setup` falls back to a one-time paste.

## Configuration

All optional — defaults work for most users.

| Env var | Default | Purpose |
|---|---|---|
| `NTULEARN_COOKIE` | — | Manual cookie fallback. |
| `NTULEARN_BROWSER` | auto-detect | Chromium binary for login capture. |
| `NTULEARN_BASE_URL` | `https://ntulearn.ntu.edu.sg` | Other Blackboard Learn instances. |
| `NTULEARN_DOWNLOAD_DIR` | `./downloads` | Default download destination. |
| `NTULEARN_CACHE_DIR` / `NTULEARN_CACHE_MODE` | SQLite path / `readwrite` | Response cache (`readonly`, `off`). |
| `NTULEARN_HTTP_CONCURRENCY` | `48` | Max in-flight HTTP requests. |
| `NTULEARN_MCP_AUTOUPDATE` | `on` | `on` / `off` / `ask`. |

## Updates

New releases auto-install by default: checked at most every 24 h, sha256-verified against the
minisign-signed `checksums.txt` (public key committed as `minisign.pub` and baked into the
binary), atomically swapped in when your MCP host next restarts. `update --auto off`, `--check`,
`--yes`, `--channel beta` for control.

## Error reporting (opt-in)

The server can auto-file a GitHub issue when it keeps hitting the same *unexpected* error (auth
expiry and caller mistakes are never filed). Once per error signature per version; course IDs,
emails and cookies are redacted; requires `gh`. Opt in with `ntulearn-mcp issues on`.

## Troubleshooting

- **"Not logged in" / 401** — run `ntulearn-mcp setup`, or ask your agent to call `ntulearn_login`.
- **Tools listed but calls return nothing** — run the binary directly; the error message says
  what is wrong (usually an expired cookie).
- **Calendar windows wider than ~16 weeks** are rejected by NTULearn — keep `since`/`until`
  within a semester.
- **Large graphical PDFs** — use `download_file` and open it in your client instead of
  `read_file_content`.

## Development

```bash
cd rust
cargo test                              # 213 unit tests
cargo build --release
target/release/ntulearn-mcp selfbench   # token + latency + concurrency benchmark
```

**Adding a tool or a whole new feature** takes one `tool!{}` entry in one file under
`rust/crates/ntulearn-mcp/src/registry/features/` — [docs/EXTENDING.md](docs/EXTENDING.md) is the
step-by-step guide. The binary is deliberately dependency-light: ~450 lines of in-tree MCP stdio,
reqwest + rusqlite, a 5.7 MB release binary, ~64 s cold build.

Before pushing, check that no credential is staged or in history:

```bash
git log --all -p | grep -nE '(BbRouter=|expires:[0-9]{10,},id:|Set-Cookie|ghp_[A-Za-z0-9]{20,})' | grep -viE 'test|example|README' || echo "clean"
```

## Disclaimer

**Use at your own risk.** Unofficial, not affiliated with NTU Singapore, Anthropic Inc., or
Blackboard Learn. Driving the LMS with your session cookie may be inconsistent with NTU's
acceptable use policy. Your cookie stays local and is sent only to `ntulearn.ntu.edu.sg` — never
share it. Don't run this for someone else. Licensed under the
[PolyForm Noncommercial License 1.0.0](LICENSE).
