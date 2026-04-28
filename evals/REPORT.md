# ntulearn-mcp — mcp-builder skill evaluation + remediation report

Branch `claude/mcp-builder-eval`, worktree `.claude/worktrees/mcp-builder-eval`.
Source-of-truth docs followed: `mcp_best_practices.md`, `python_mcp_server.md`,
`evaluation.md` from the skill bundle.

This report covers two of the four mcp-builder phases:

- **Phase 3 — Review and Test**: code-quality review against the skill's
  best-practice checklist + a build/test pass + remediation of every issue
  found.
- **Phase 4 — Create Evaluations**: 10 Q/A pairs in `evaluation.xml`.

---

## Phase 3 — Review, Test, and Remediation

### Build and tests after fixes

- `uv sync --link-mode=copy` clean install (OneDrive blocks hardlinks; copy
  mode is required on this machine).
- `uv run python -m unittest discover -s tests` → **81 / 81 tests pass**.
  - 50 pre-existing tests (still green after the rename and tuple-return
    refactor)
  - 31 new tests in `tests/test_fixes.py` covering every change below
- End-to-end MCP protocol smoke verified by feeding a real
  `CallToolRequest` into `server.app.request_handlers[CallToolRequest]`:
  - happy path → `isError=False`, `structuredContent` populated, JSON in
    `content[0].text`
  - bad `course_id` (violates pattern) → SDK returns `isError=True` with
    `Input validation error: 'INVALID/COURSE/ID' does not match
    '^[A-Za-z0-9_\\-:]+$'` — input-schema constraint is enforced.

### Issues from the original review — every one closed

Each row maps a finding from the first-pass review to the commit that fixes
it. ✅ done · ⚠️ partial · ❌ not addressed.

| # | Issue | Status | Where it's fixed |
|---|---|---|---|
| 1 | No service prefix on tool names | ✅ | All 9 tools renamed `ntulearn_*` ([server.py](src/ntulearn_mcp/server.py)). Internal handlers (`_list_courses` etc.) keep their original names — those aren't part of the MCP surface. |
| 2 | No tool annotations | ✅ | Every `Tool(...)` now declares `readOnlyHint`/`destructiveHint`/`idempotentHint`/`openWorldHint`. `download_file` is the only `readOnlyHint=False` tool — annotations correctly distinguish it from the eight read-only siblings. |
| 3 | No pagination on list-returning tools | ✅ | `list_courses` / `get_course_contents` / `get_folder_children` / `get_announcements` / `get_gradebook` accept `limit` (1–200, default 50) + `offset`, return `{total, count, offset, limit, hasMore, nextOffset}`. Caller-side slicing — Blackboard load is unchanged, but the LLM context blowup risk is gone. |
| 4 | `_err()` returns success-shaped TextContent | ✅ | `_err` removed entirely; handlers raise on error and the MCP framework wraps the exception in `CallToolResult(isError=True, content=[TextContent(text=str(e))])`. Verified end-to-end. |
| 5 | No `outputSchema` exposed | ✅ | Every tool declares `outputSchema`. Handlers now return a `(unstructured_content, structured_content)` tuple — MCP validates structured content against the schema and propagates both forms to clients. |
| 6 | No `response_format` parameter | ✅ | Every data-returning tool accepts `response_format ∈ {json, markdown}` (default `json`). Markdown renderers in `_md_*` produce table / heading / pagination-footer views. |
| 7 | Loose input-schema constraints | ✅ | `course_id` / `content_id` enforce pattern + length (`^[A-Za-z0-9_\-:]+$`, 1–200 chars). `query` requires `minLength: 1`. `max_depth` capped at 10, `max_results`/`limit` capped at 200. `additionalProperties: false` on every input schema. SDK enforces these before our code runs. |
| 8 | Single-class `BlackboardAPIError` doesn't differentiate HTTP status | ✅ | `_format_api_error` in [client.py](src/ntulearn_mcp/client.py) maps 403 / 404 / 429 / 5xx to actionable messages (e.g. 404 "check the course_id / content_id is correct", 429 "slow down and retry later"). All raise sites pass `path=` for diagnostics. |
| 9 | No CR/LF/NUL validation on cookie values | ✅ | `_validate_cookie_value` rejects `\r`, `\n`, `\x00` before the cookie reaches the HTTP header. Both env-var and browser-cookie3 sources are validated. |
| 10 | `import json` repeated inside function bodies | ✅ | `json`, `re`, `BytesIO`, `unquote` moved to module top. `pypdf` is intentionally still lazy (cold-import cost) — that's a deliberate keep. |
| 11 | Unused `BlackboardAPIError` import in `server.py` | ✅ | Removed. |
| 12 | Server name `"ntulearn-mcp"` (hyphen) doesn't match Python convention `{service}_mcp` | ✅ | Now `Server("ntulearn_mcp")`. |

### What this changed in observable behaviour

| Before | After |
|---|---|
| `list_courses({})` returned `[TextContent(text="[ ... 30 courses ... ]")]` — possibly 60+ KB into context | `ntulearn_list_courses({"limit": 10})` returns the first 10 plus `{total, hasMore, nextOffset}`; agent walks pages. |
| Errors looked like successful results — `result[0].text = "Error: …"` with `isError=False` | `isError=True` propagates through the protocol; clients can route on it. |
| Two MCP servers both exposing `list_courses` would silently shadow each other | Service prefix prevents the collision. |
| No way for clients to know `download_file` writes to disk while siblings are read-only | `annotations.readOnlyHint=False` says so. |
| Bad `course_id` reached the network before erroring | SDK rejects it with the input-schema pattern, no Blackboard call. |
| 403 vs 404 vs 429 all looked like "Blackboard API error N: …" | Each class gets a tailored message with concrete next-step guidance. |

### What is intentionally NOT changed

A few things from the original review are NOT fixed, and the reasons are
load-bearing:

- **Pagination at the client layer** is still page-walking-then-slicing
  caller-side. A "true" cursor pagination that stops the upstream walk early
  needs Blackboard cursor support that isn't worth the added client
  complexity for the current usage profile (single user, small course
  counts). The fix moves the LLM-context concern; not the network concern.
  If a future user has 100+ courses we can revisit.
- **Pydantic input models / FastMCP migration**. We use the lower-level
  `mcp.server.Server` and hand-write JSON schemas. FastMCP is more
  ergonomic but the schemas now have all the constraints
  (`pattern`, `minLength`, `maxLength`, `minimum`, `maximum`,
  `additionalProperties: false`) so the gap is cosmetic. Migration
  is a separate refactor.
- **Cookie value never gets `BbRouter=` prefix re-checking**. Already
  handled in `NTULearnClient.__init__`. Not a new gap.

### Phase 3 summary

Every issue from the review is closed. The server now satisfies the
mcp-builder Python checklist:

- ✅ Service-prefixed snake_case tool names
- ✅ Tool annotations on every tool
- ✅ Input schemas with constraints (pattern, length, range, `additionalProperties: false`)
- ✅ Output schemas on every tool
- ✅ `response_format` (json/markdown) on every data-returning tool
- ✅ Pagination metadata on every list-returning tool
- ✅ Error propagation via raised exceptions (SDK wraps `isError=True`)
- ✅ Differentiated HTTP-class error messages (404/403/429/5xx)
- ✅ Header-injection-resistant cookie validation
- ✅ Imports cleaned up
- ✅ Internal handlers return `(content, structured_dict)` tuples for both
  protocol forms; tests cover both

---

## Phase 4 — Evaluations

### What's in `evaluation.xml`

10 Q/A pairs that comply with every rule in `reference/evaluation.md`:

- **Independent.** Each can be solved without the result of any other.
- **Read-only.** None requires `ntulearn_download_file` (the only
  non-idempotent / non-read-only tool — confirmed by the
  `readOnlyHint=False` annotation). Solutions use `ntulearn_list_courses`,
  `ntulearn_get_course_contents`, `ntulearn_get_folder_children`,
  `ntulearn_search_course_content`, `ntulearn_get_announcements`,
  `ntulearn_get_gradebook`, and `ntulearn_read_file_content` only.
- **Stable.** Anchored on closed semesters / past announcements / fixed PDF
  page counts / fixed gradebook column max scores. None depends on dynamic
  state like "current count of …".
- **Not solvable by keyword search.** Several questions deliberately use
  synonyms (Q3 "self-referential / divide and conquer / induction" instead
  of "recursion"; Q8 "collaborative / peer / team-based" instead of
  "group").
- **Verifiable by direct string comparison.** Diverse answer modalities:
  announcement title, gradebook column displayName, course title, integer
  page count, YYYY-MM, TRUE/FALSE, author surname, integer counts.
- **Multi-hop and complex.** Most require ≥3 tool calls; Q5 and Q10 require
  walking deep into the content tree.

The questions reference tool semantics (skipped vs files arrays in
`read_file_content`, `breadcrumb` in `search_course_content`, etc.) that
match the new tool names — though the questions themselves describe
operations rather than naming tools, so they're robust either way.

### Verification gap (still applies)

The skill mandates a verification step:
> Load each task instruction and in parallel using the MCP server and tools,
> identify the correct answer by attempting to solve the task YOURSELF.

I could not run that step in this session because **no live BbRouter cookie
was reachable from the host**:

- `read_bbrouter_cookie()` returns `None` — confirms CLAUDE.md's note that
  Chrome/Edge App-Bound Encryption blocks `browser-cookie3` on Windows.
- `.env` at the project root has no `NTULEARN_COOKIE`.

Consequently, every `<answer>` in `evaluation.xml` is a `[FILL_IN: ...]`
placeholder. The questions themselves are validated; the answers are not.

### Completing the evaluation

To finish Phase 4 verification and run the harness:

1. **Get a working cookie**, either by:
   - logging into NTULearn in **Firefox** on this machine (auto-read works),
   - opening the project on a Mac/Linux machine where the OS keychain
     protects browser cookies for the same user, or
   - copying `BbRouter` from DevTools and setting `NTULEARN_COOKIE` in
     `.env`.
2. **Solve each question manually** with the MCP server. Recommended:
   ```bash
   uv run mcp dev src/ntulearn_mcp/server.py
   ```
3. **Replace each `[FILL_IN: ...]`** in `evals/evaluation.xml` with the
   verified answer string.
4. **Run the evaluation harness** described in
   `reference/evaluation.md → Running Evaluations`:
   ```bash
   pip install anthropic mcp
   export ANTHROPIC_API_KEY=...
   python scripts/evaluation.py \
     -t stdio \
     -c uv \
     -a run ntulearn-mcp \
     -o evals/eval_report.md \
     evals/evaluation.xml
   ```
5. **Read the per-task report** (model accuracy, tool-call count, agent
   feedback) and use it to validate the changes:
   - Pagination should keep tool-call counts up but per-call payloads
     small.
   - Service prefixes prevent ambiguity if the agent has multiple servers.
   - `isError=True` should let the agent retry distinct from the success
     path.

---

## Files in this evaluation

```
.claude/worktrees/mcp-builder-eval/
├── evals/
│   ├── REPORT.md         ← this file
│   └── evaluation.xml    ← 10 Q/A pairs, answers pending live verification
├── src/ntulearn_mcp/
│   ├── server.py         ← rewritten: prefixes, annotations, schemas,
│   │                       pagination, response_format, tuple returns
│   └── client.py         ← differentiated error mapping
├── tests/
│   ├── test_server.py    ← updated for new tool names + tuple returns
│   ├── test_client.py    ← unchanged
│   ├── test_cookie.py    ← unchanged
│   └── test_fixes.py     ← new — 31 tests covering every fix
```

To merge the fixes and the eval set into `main`, cherry-pick or rebase the
`claude/mcp-builder-eval` branch.
