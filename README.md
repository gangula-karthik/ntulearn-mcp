# ntulearn-mcp

MCP server for **NTULearn** (NTU Singapore's Blackboard Learn instance). Lets Claude Desktop, Claude Code, Cursor, Cline, and other MCP hosts read your courses, content, announcements, and grades.

Built for tech-inclined NTU students. Requires Python 3.12+ and `uv`.

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

**OpenClaw:**

```bash
openclaw mcp set ntulearn '{"command":"uvx","args":["ntulearn-mcp"]}'
```

### 4. Restart your MCP host, then try it

Ask Claude: *"List my NTULearn courses."*

That's the whole flow for most users. Read [Authentication](#authentication) if you're on Windows with Chrome/Edge, or if step 4 doesn't return anything.

---

## Authentication

The server resolves your `BbRouter` cookie in this order:

1. **`NTULEARN_COOKIE` env var** — explicit override; always wins.
2. **Browser auto-read** — walks Edge → Chrome → Firefox → Brave via [`browser-cookie3`](https://pypi.org/project/browser-cookie3/), returns the first valid `BbRouter`.

When your session expires mid-conversation, the server catches the 401, re-reads from your browser, and retries the call once. If your browser still has a fresh session, this is invisible.

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

The cookie expires with your NTULearn session (days–weeks). When it does, repeat from step 1. **This will not auto-refresh** — manual override disables the browser auto-read fallback.

---

## Available tools

| Tool | Description |
|---|---|
| `list_courses` | List enrolled courses |
| `get_course_contents` | Top-level content tree for a course |
| `get_folder_children` | Children of a folder/lesson |
| `search_course_content` | Recursively search a course's content tree |
| `get_file_download_url` | Extract download URL from a content item |
| `download_file` | Download a file to your local downloads folder |
| `get_announcements` | Course announcements |
| `get_gradebook` | Gradebook columns and your scores |

## Example prompts

- *"List all my NTULearn courses."*
- *"Show me the content tree for course `_12345_1`."*
- *"Search for 'assignment' in course `_12345_1`."*
- *"Download the lecture slides from content item `_67890_1` in course `_12345_1`."*
- *"What are the latest announcements in my courses?"*
- *"Show me my grades for course `_12345_1`."*

---

## Optional configuration

| Env var | Default | Purpose |
|---|---|---|
| `NTULEARN_COOKIE` | (auto-read) | Override the cookie source |
| `NTULEARN_BASE_URL` | `https://ntulearn.ntu.edu.sg` | Change for a different Blackboard instance |
| `NTULEARN_DOWNLOAD_DIR` | `./downloads` | Where `download_file` saves files |

Set these in your MCP host's `env` block (same place as `NTULEARN_COOKIE` above).

---

## Troubleshooting

**"No NTULearn cookie found" / tools fail with 401.**
- Make sure you're logged into NTULearn in a supported browser.
- Check that no stale `NTULEARN_COOKIE` value is set anywhere — env vars and `.env` files **always** override browser auto-read. If you previously set it manually and want to switch to auto-read, delete the line.

**MCP host lists "ntulearn" but the tool calls hang or return nothing.**
On macOS, the first call may be blocked on a hidden Keychain prompt. See [macOS first-time setup](#macos-first-time-setup).

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
uv run python -m unittest discover -s tests      # run tests (24)
uv run ntulearn-mcp                              # run the server (stdio)
uv run mcp dev src/ntulearn_mcp/server.py        # interactive tool inspector
```

Project layout:

```
src/ntulearn_mcp/
├── server.py     # MCP entrypoint, tool handlers, cookie resolution
├── client.py     # async httpx-based Blackboard REST client
├── cookie.py     # browser cookie auto-read
├── parsers.py    # HTML body → download URL extraction
└── models.py     # Pydantic models (validation / reference)
```

Tests use `unittest` (not pytest); HTTP is mocked via `httpx.MockTransport`. See [CLAUDE.md](CLAUDE.md) for design decisions and known limitations.

---

## Disclaimer & responsible use

**Use at your own risk.** This is an unofficial, personal-use tool. It is **not** affiliated with, endorsed by, or sponsored by NTU Singapore, Anthology Inc., or Blackboard Learn. NTULearn, Blackboard, and related marks belong to their respective owners.

- **Your account, your responsibility.** Driving the LMS via your session cookie may be inconsistent with NTU's IT acceptable use policy or terms of service. You alone bear the consequences of how you use this tool — including potential account suspension. Consult NTU policy if you're unsure.
- **No credentials are exfiltrated.** The `BbRouter` cookie is read locally on your machine and sent only to `ntulearn.ntu.edu.sg`. No third party (not the author, not Anthropic, not Anthology) sees your cookie or session data.
- **Don't share cookie values.** Anyone with your `BbRouter` can act as you on NTULearn until it expires.
- **Don't run this on someone else's behalf.** Each user should run their own instance against their own account.

The MIT license disclaims all warranties — see [LICENSE](LICENSE).
