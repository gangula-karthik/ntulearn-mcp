# Extending ntulearn-mcp

Every NTULearn capability is a **tool** exposed three ways at once: an MCP
tool (`ntulearn_<name>`), a CLI command (kebab-case alias), and (optionally)
a markdown renderer for `response_format="markdown"`. All three are declared
in **one place**: the tool registry.

## The one-file rule

A tool is one `tool!{}` entry in one feature module:

```
rust/crates/ntulearn-mcp/src/registry/features/
├── courses.rs    course & content browsing
├── files.rs      download_file / read / dl-course
├── calendar.rs   ics export, due dates
├── grades.rs     gradebook, attempts, CSV export
├── messages.rs   messages + announcements
├── roster.rs     users, groups, members
├── digest.rs     whats_new, summarize_course
├── fbf.rs        FeedbackFruits peer evaluations (LTI)
├── pcv.rs        Peerceptiv peer evaluations (LTI)
└── kaltura.rs    Kaltura lecture videos (LTI)
```

The registry is compile-time statics (`OnceLock` + fn pointers): no trait
objects beyond one boxed future per call, no dynamic loading, no extra
dependencies.

## Adding a tool

### 1. Write the handler

Add `h_my_thing` to `handlers.rs` (existing modules import from there), or a
new `src/my_thing.rs` module for anything substantial:

```rust
pub async fn h_my_thing(state: &AppState, args: &Value) -> Result<Value, String> {
    // str_arg()/int_arg() helpers extract typed args (never `.to_string()`
    // on a JSON string — see the args_guard_tests regression in handlers.rs)
    let course_id = str_arg(args, "course_id").ok_or("course_id required")?;
    ...
    Ok(json!({ "things": [...] }))
}
```

Return the raw **payload**. TOON / JSON / markdown / compact TSV rendering
happens downstream, so one handler serves every output format.

### 2. Register it — one entry, one file

In the right feature module (or a new one):

```rust
crate::registry::tool! {
    short: "my_thing",              // dispatch key; MCP name = ntulearn_my_thing
    mcp: Tool::new(
        "ntulearn_my_thing".to_string(),
        "One-liner + Example: ntulearn_my_thing(course_id='_123_1')".to_string(),
        json!({ "type": "object", "properties": { ... }, "additionalProperties": false }),
    ).with_annotations(ToolAnnotations {
        title: Some("My thing".to_string()),
        read_only_hint: Some(true),
        ..Default::default()
    }),
    cli: Some(CmdSpec {
        short: "my_thing",
        alias: "my-thing",           // kebab-case CLI command
        summary: "one terse line for `help`",
        args: &[s("course_id", true, "course id")],  // s/i/b/a/e helpers
        list_key: "things",          // payload array rendered as compact TSV
        cols: &["id", "title"],      // TSV columns
    }),
    markdown: None,                  // or Some(md_my_thing) for --md output
    handler: handlers::h_my_thing,
}
```

Everything downstream picks it up automatically:

| consumer | how it finds the tool |
|---|---|
| MCP `tools/list` | `registry::all()` |
| MCP `tools/call` | `registry::dispatch_value()` |
| CLI parse / `help` | `registry::lookup_cli()` (`CLI_ORDER` in `cli.rs` curates ordering) |
| `--md` / `response_format=markdown` | `registry::markdown_for()` |

New commands appear in `help` after the curated `CLI_ORDER` list — add the
alias there only if you care where it sorts.

### 3. If you created a NEW feature module

Add it to `registry/features/mod.rs`:

```rust
mod my_feature;
...
out.extend(my_feature::tools());
```

### 4. Verify

```bash
cargo build --release
cargo test
ntulearn-mcp help my-thing          # renders from the registry
ntulearn-mcp my-thing --course-id _123_1
```

`selfbench` exercises discovery cost and end-to-end latency; the Kaltura e2e
script (`scripts/verify-kaltura-e2e.sh --dry-run`) checks render paths without
network.

## The one special case: `ntulearn_login`

Login is the only tool outside the registry: its handler takes
`Arc<AppState>` (the browser capture may outlive the call while the user is
still typing their password) and returns text, not a payload. It is wired by
hand in `tools.rs` next to its schema. Everything else goes through the
registry.

## Why a compile-time registry (design notes)

- **Before**: one tool = 4 edits in 4 formats across 3 files (schemas.rs
  literal, handler + dispatch match arm, render match arm, cli.rs CmdSpec).
  Miss one and the tool half-works.
- **After**: one entry in one file; the four consumers read the same source
  of truth. Nothing can drift.
- Handlers stay plain `async fn`s in `handlers.rs`/feature modules — testable
  and reusable — while routing data lives with the tool's schema and CLI spec.
- The registry is built once into a `OnceLock<Vec<ToolDef>>`; lookups are a
  linear scan over ≤ dozens of entries (nanoseconds, not a bottleneck — the
  cost of any tool call is the network round trips behind it).
