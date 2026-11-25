<p align="center">
  <img src="assets/banner.webp" alt="ntulearn-mcp — your NTULearn courses, announcements and grades, inside your agent" width="100%">
</p>

# ntulearn-mcp

**MCP server + agent-first CLI for NTULearn** (NTU Singapore's Blackboard Learn instance).
Ask your agent about your courses, announcements, calendar, and grades, or have it organise
course files into a folder hierarchy on disk. One small Rust binary, no Python, no virtualenv.
Built-in minimal MCP stdio implementation, 33 tools, 2 prompt templates.

**Website:** <https://ntulearn.krtk.app>

## Quick start

```bash
git clone https://github.com/gangula-karthik/ntulearn-mcp.git
cd ntulearn-mcp/rust
cargo build --release          # binary: rust/target/release/ntulearn-mcp
rust/target/release/ntulearn-mcp setup    # opens NTULearn, captures your session
rust/target/release/ntulearn-mcp check    # expect: Live validity : OK (200)
```

Register with your MCP host (Claude Desktop, Claude Code, Cursor, Cline, Prime Agent):

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

The same binary serves MCP with no arguments and acts as a CLI with a command.
Sessions renew silently in the background; no terminal? Ask your agent to call `ntulearn_login`.

## Why the CLI: about 64x fewer tokens

MCP hosts inject every tool's schema into context **every turn** (~8.8k tokens for 33 tools).
The CLI's compact TSV output reads in a few hundred tokens instead:

```bash
ntulearn-mcp courses                 # 6 courses ≈ 100 tokens (MCP tool result: ≈ 450)
ntulearn-mcp tree _2706844_1         # a course's content tree
ntulearn-mcp whats-new --days 7      # announcements + due dates + grades digest
ntulearn-mcp multi "courses" "grades"   # several commands concurrently
```

Measured by `ntulearn-mcp selfbench`: discovery 11.3x cheaper, task output 4.5x cheaper, a
10-turn conversation ~65x cheaper. Fetches fan out concurrently (bounded at 48 in flight) with
a shared cache — `whats_new` across 6 courses: 60 ms instead of 703 ms; a 403-node content
tree: 0.13 s instead of 2.3 s.

## CLI reference

`ntulearn-mcp help` prints the full list; `ntulearn-mcp help <cmd>` shows one command's flags.
Every MCP tool argument is also accepted as a kebab-case flag. Output is compact TSV by default;
`--json`, `--md`, `--toon` (token-optimized full fidelity, the MCP default since v0.7.2),
`--fields a,b`, `--limit N`, `--offset N` also available.

## Tools

33 tools, most doing cross-course aggregation by default — you rarely pass course IDs by hand:

- **Courses/content** — `list_courses`, `get_course_contents`, `get_content_tree`,
  `search_course_content`, `search_all_courses`, `download_file`, `download_course`,
  `read_file_content`
- **Calendar/grades** — `get_upcoming`, `get_gradebook`, `get_gradebook_attempts`,
  `whats_new`, `summarize_course`, `export_calendar_ics`, `export_gradebook_csv`
- **Messages/people** — `list_messages`, `read_message`, `list_course_users`,
  `list_course_groups`, `get_group_members`
- **LTI integrations** — FeedbackFruits (`fbf_status`/`fbf_reviews`/`fbf_rate`/`fbf_comment`),
  Peerceptiv (`pcv_*` equivalents), Kaltura video (`kaltura_list`/`kaltura_transcripts`/`kaltura_download`)

Peer evaluations (FeedbackFruits, Peerceptiv) are launched through LTI with your existing
session and read from the provider's API directly — including rubric ratings and, after the
instructor releases results, what teammates gave you. Write tools (`fbf_rate`, `pcv_rate`, …)
default to `dry_run=true`.

Kaltura lecture videos: `kaltura_transcripts` saves `.vtt` + timestamp-stripped `.txt` with word
counts per video; `kaltura_download` streams the MP4 from the open CDN. Protocol details:
[KALTURA_SPEC.md](docs/KALTURA_SPEC.md).

Known limits: `list_messages`/`get_group_members` use internal v1 endpoints (the public ones 403/404
for students); calendar windows wider than ~16 weeks are rejected; for large graphical PDFs use
`download_file` instead of `read_file_content`.

## Authentication

The `BbRouter` cookie is resolved in order: `NTULEARN_COOKIE` env var → config file → Firefox
`cookies.sqlite` (read-only). **No keychain access, ever.**

`setup` opens NTULearn in a dedicated browser profile (any Chromium-based browser, never your real
profile), waits for you to log in, validates, and saves the cookie. Afterwards everything renews
itself: NTULearn cookies (~3 h) via a headless re-login through the saved NTU SSO session; if even
SSO has ended, a login window opens by itself and the next retry succeeds. Without any Chromium
browser, `setup` falls back to a one-time paste (details: <details><summary>manual cookie
fallback</summary>DevTools → Application → Cookies → copy the `BbRouter` value → put it in
`NTULEARN_COOKIE` or the MCP host's `env` block.</details>).

## Configuration

All optional — defaults work for most users.

| Env var | Default | Purpose |
|---|---|---|
| `NTULEARN_COOKIE` | — | Manual cookie fallback. |
| `NTULEARN_BROWSER` | auto-detect | Chromium binary for login capture. |
| `NTULEARN_BASE_URL` | `https://ntulearn.ntu.edu.sg` | Other Blackboard instances. |
| `NTULEARN_DOWNLOAD_DIR` | `./downloads` | Default download destination. |
| `NTULEARN_CACHE_DIR` / `NTULEARN_CACHE_MODE` | SQLite path / `readwrite` | Response cache (`readonly`, `off`). |
| `NTULEARN_HTTP_CONCURRENCY` | `48` | Max in-flight HTTP requests. |
| `NTULEARN_MCP_AUTOUPDATE` | `on` | `on` / `off` / `ask`. |

## Updates

New releases auto-install by default: checked at most every 24 h, sha256-verified against the
minisign-signed `checksums.txt` (`minisign.pub` is committed and baked into the binary), and
atomically swapped in when your MCP host next restarts. `ntulearn-mcp update --auto off` or
`--check` / `--yes` / `--channel beta` for control.

## Error reporting (opt-in)

The server can auto-file a GitHub issue when it keeps hitting the same *unexpected* error
(auth expiry and caller mistakes are never filed). Once per error signature per version;
course IDs, emails, and cookies are redacted; needs `gh`. Opt in with `ntulearn-mcp issues on`.

## Troubleshooting

- **"Not logged in" / 401** — run `ntulearn-mcp setup`, or ask your agent to call `ntulearn_login`.
- **Tools listed but calls return nothing** — run the binary directly; the error message says
  what is wrong (usually the cookie).
- **`read_file_content` says "No download URL found"** — the item is likely a text page or tool
  link, not a file.

## Development

```bash
cd rust
cargo test                              # 213 unit tests
cargo build --release
target/release/ntulearn-mcp selfbench   # token + latency + concurrency benchmark
```

**Adding a tool** takes one `tool!{}` entry in one file under
`rust/crates/ntulearn-mcp/src/registry/features/` — [docs/EXTENDING.md](docs/EXTENDING.md) is
the step-by-step guide. The binary is deliberately dependency-light (~450 lines of in-tree MCP
stdio, reqwest + rusqlite; 5.7 MB release binary, ~64 s cold build).

Before pushing, check that no credential is staged or in history:

```bash
git log --all -p | grep -nE '(BbRouter=|expires:[0-9]{10,},id:|Set-Cookie|ghp_[A-Za-z0-9]{20,})' | grep -viE 'test|example|README' || echo "clean"
```

## Disclaimer

**Use at your own risk.** Unofficial, not affiliated with NTU Singapore, Anthropic Inc., or
Blackboard Learn. Driving the LMS with your session cookie may be inconsistent with NTU's
acceptable use policy. Your cookie stays local and is sent only to `ntulearn.ntu.edu.sg` —
never share it. Don't run this for someone else. Licensed under the
[PolyForm Noncommercial License 1.0.0](LICENSE).
