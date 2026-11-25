//! Agent-first CLI surface for ntulearn-mcp.
//!
//! Motivation (measured, see `selfbench`): MCP hosts inject **every** tool
//! schema into the model's context on **every** turn. With 26 tools that is
//! thousands of tokens before the agent does any work. A CLI pays
//! zero standing cost: the agent reads `ntulearn-mcp help` (~700 tokens) only
//! when it needs it and runs commands directly.
//!
//! Design rules (token-efficiency first):
//!   * `help` is one terse table - aliases, positional args, one-liners.
//!   * Default output is **compact TSV** (header row + tab-separated rows),
//!     not pretty JSON. `--json` opts back into the full payload.
//!   * Long values are truncated (`--max-chars`, `--full` disables).
//!   * `multi` runs several commands **concurrently** in one process (shared
//!     HTTP client + SQLite cache) - cheaper and faster than N invocations.
//!   * `selfbench` is the built-in performance harness (token estimates via
//!     chars/4 + end-to-end latency against a local mock API), so every
//!     optimization is measured, not guessed.

use std::sync::Arc;
use std::time::Instant;

use serde_json::{Map, Value};

use crate::AppState;
use crate::handlers;
use crate::registry::cli_spec::{ArgKind, CmdSpec};

// Command specs (alias, args, list_key, compact columns) now live next to
// each tool in src/registry/features/*.rs; the data types are in
// registry::cli_spec. This file owns parsing, help rendering and dispatch.

/// Presentation order for `help` — curated, not semantic. Tools listed here
/// render first, in this order; any registered tool NOT listed (e.g. a newly
/// added feature) appends after, so nothing is ever hidden from help.
const CLI_ORDER: &[&str] = &[
    "courses", "contents", "tree", "search", "find",
    "dl", "read", "upcoming", "announcements", "grades", "attempts",
    "messages", "message", "users", "groups", "members", "summary",
    "whats-new", "dl-course", "ics", "grades-csv",
    "fbf", "fbf-reviews", "fbf-rate", "fbf-comment",
    "pcv", "pcv-evals", "pcv-rate", "pcv-comment", "pcv-auto",
    "kaltura-list", "kaltura-transcripts", "kaltura-download",
];

pub fn find_cmd(name: &str) -> Option<&'static CmdSpec> {
    crate::registry::lookup_cli(name)
}

// ---------------------------------------------------------------------------
// Parsed invocation
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct Invocation {
    pub spec: &'static CmdSpec,
    pub args: Value,
    pub raw: bool,
    pub md: bool,
    pub fields: Option<Vec<String>>,
    pub max_chars: usize,
    /// Cap for long-form content fields (text/body/...) — larger than
    /// `max_chars` so `read` returns useful content by default.
    pub text_chars: usize,
    /// `--toon`: emit the full payload as TOON (token-optimized JSON).
    pub toon: bool,
}

#[derive(Clone)]
struct OutputFlags {
    raw: bool,
    md: bool,
    toon: bool,
    fields: Option<Vec<String>>,
    max_chars: usize,
    full: bool,
    /// Whether --max-chars was given explicitly (text-bearing fields get a
    /// larger default cap otherwise: agents invoke `read` to get content).
    chars_set: bool,
}

/// `--course-ids` -> `course_ids` (MCP arg names are snake_case).
fn kebab_to_snake(f: &str) -> String {
    f.replace('-', "_")
}

/// `course_ids` -> `course-ids` (for help text).
fn snake_to_kebab(f: &str) -> String {
    f.replace('_', "-")
}

fn split_list(v: &str) -> Vec<String> {
    v.split(',')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

/// Parse `argv` (after the command token) into a JSON args object.
///
/// Supports `--flag value`, `--flag=value`, bare `--flag` (true), positional
/// values for required string args (in spec order), and the global output
/// flags. A value that itself starts with `--` must use `--flag=value`.
pub fn parse_invocation(cmd_token: &str, rest: &[String]) -> Result<Invocation, String> {
    let spec = find_cmd(cmd_token)
        .ok_or_else(|| format!("unknown command: {cmd_token} (run `ntulearn-mcp help`)"))?;

    let mut out = OutputFlags {
        raw: false,
        md: false,
        toon: false,
        fields: None,
        max_chars: 200,
        full: false,
        chars_set: false,
    };
    let mut map: Map<String, Value> = Map::new();
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0usize;

    let spec_arg = |flag: &str| spec.args.iter().find(|a| a.flag == flag);
    let known = |flag: &str| spec_arg(flag).is_some() || flag == "limit" || flag == "offset";

    while i < rest.len() {
        let tok = rest[i].as_str();
        if !tok.starts_with("--") {
            positional.push(tok.to_string());
            i += 1;
            continue;
        }
        let (name, inline_val) = match tok.split_once('=') {
            Some((n, v)) => (n.to_string(), Some(v.to_string())),
            None => (tok.to_string(), None),
        };
        match name.as_str() {
            "--json" => {
                out.raw = true;
                i += 1;
                continue;
            }
            "--md" | "--markdown" => {
                out.md = true;
                i += 1;
                continue;
            }
            "--toon" => {
                out.toon = true;
                i += 1;
                continue;
            }
            "--full" => {
                out.full = true;
                i += 1;
                continue;
            }
            "--fields" | "--max-chars" => {
                let v = match inline_val.clone() {
                    Some(v) => v,
                    None => match rest.get(i + 1) {
                        Some(v) if !v.starts_with("--") => {
                            i += 1;
                            v.clone()
                        }
                        _ => return Err(format!("{name} needs a value")),
                    },
                };
                if name == "--fields" {
                    out.fields = Some(split_list(&v));
                } else {
                    out.max_chars = v.parse().unwrap_or(200);
                    out.chars_set = true;
                }
                i += 1;
                continue;
            }
            _ => {}
        }
        let flag = kebab_to_snake(name.trim_start_matches('-'));
        if !known(&flag) {
            return Err(format!("unknown flag for `{}`: {name}", spec.alias));
        }
        let kind = spec_arg(&flag).map(|a| a.kind).unwrap_or(ArgKind::Int);
        let value: Option<String> = match inline_val {
            Some(v) => Some(v),
            None => match rest.get(i + 1) {
                Some(v) if !v.starts_with("--") => {
                    i += 1;
                    Some(v.clone())
                }
                _ if kind == ArgKind::Bool => None, // bare boolean flag = true
                _ => return Err(format!("--{} needs a value", snake_to_kebab(&flag))),
            },
        };
        let parsed = match (kind, value) {
            (ArgKind::Str, Some(v)) => Value::String(v),
            (ArgKind::Int, Some(v)) => Value::Number(
                v.parse::<i64>()
                    .map_err(|_| format!("--{flag} wants an integer, got {v:?}"))?
                    .into(),
            ),
            (ArgKind::Bool, Some(v)) => Value::Bool(
                v.parse::<bool>()
                    .map_err(|_| format!("--{flag} wants true/false, got {v:?}"))?,
            ),
            (ArgKind::Bool, None) => Value::Bool(true),
            (ArgKind::Arr, Some(v)) => {
                Value::Array(split_list(&v).into_iter().map(Value::String).collect())
            }
            (ArgKind::Enum(_), Some(v)) => Value::String(v),
            (_, None) => return Err(format!("--{} needs a value", snake_to_kebab(&flag))),
        };
        if let ArgKind::Enum(vals) = kind {
            if let Some(sv) = parsed.as_str() {
                if !vals.contains(&sv) {
                    return Err(format!(
                        "--{} must be one of: {}",
                        snake_to_kebab(&flag),
                        vals.join("|")
                    ));
                }
            }
        }
        map.insert(flag, parsed);
        i += 1;
    }

    // Positional values fill string args (required first, then optional) in
    // spec order: `contents _1_1 _2_2` == `--course-id _1_1 --parent-id _2_2`.
    let mut str_args: Vec<&str> = spec
        .args
        .iter()
        .filter(|a| a.required && a.kind == ArgKind::Str)
        .map(|a| a.flag)
        .collect();
    str_args.extend(
        spec.args
            .iter()
            .filter(|a| !a.required && a.kind == ArgKind::Str)
            .map(|a| a.flag),
    );
    if positional.len() > str_args.len() {
        return Err(format!(
            "`{}` takes at most {} positional arg(s) ({}); got {}",
            spec.alias,
            str_args.len(),
            str_args.join(", "),
            positional.len()
        ));
    }
    for (flag, val) in str_args.into_iter().zip(positional.into_iter()) {
        map.entry(flag.to_string()).or_insert(Value::String(val));
    }
    for a in spec.args.iter().filter(|a| a.required) {
        if !map.contains_key(a.flag) {
            return Err(format!(
                "`{}` requires --{} (or it positionally)",
                spec.alias,
                a.flag.replace('_', "-")
            ));
        }
    }

    if out.raw && out.md {
        return Err("--json and --md are mutually exclusive".to_string());
    }
    if out.toon && (out.raw || out.md) {
        return Err("--toon is mutually exclusive with --json and --md".to_string());
    }

    Ok(Invocation {
        spec,
        args: Value::Object(map),
        raw: out.raw,
        md: out.md,
        fields: out.fields,
        max_chars: if out.full { usize::MAX } else { out.max_chars },
        // Long-form content fields (file text, announcement bodies) default
        // to a 2000-char cap — 200 would make `read` useless by default.
        text_chars: if out.full || out.chars_set {
            if out.full { usize::MAX } else { out.max_chars }
        } else {
            2000
        },
        toon: out.toon,
    })
}

// ---------------------------------------------------------------------------
// Compact renderer
// ---------------------------------------------------------------------------

/// Token-terse tabular render of a handler payload.
///
/// Layout: a `#` summary line, a tab-separated header, then one
/// tab-separated row per item. Nested objects render as compact JSON, and
/// long values are truncated to `max_chars`.
pub fn render_compact(inv: &Invocation, payload: &Value) -> String {
    let spec = inv.spec;
    let max = inv.max_chars;
    let tmax = inv.text_chars;

    let items: Option<(&Value, &str)> = if spec.list_key.is_empty() {
        None
    } else {
        payload.get(spec.list_key).map(|v| (v, spec.list_key))
    }
    .or_else(|| {
        // generic fallback: first array-of-objects key in the payload
        payload.as_object().and_then(|o| {
            o.iter()
                .find(|(_, v)| {
                    v.as_array()
                        .map(|a| !a.is_empty() && a.iter().all(|x| x.is_object()))
                        .unwrap_or(false)
                })
                .map(|(k, v)| (v, k.as_str()))
        })
    });

    let mut lines: Vec<String> = Vec::new();

    if let Some((arr, key)) = items {
        let rows = arr.as_array().cloned().unwrap_or_default();
        let total = payload.get("total").and_then(|v| v.as_u64());
        let has_more = payload
            .get("hasMore")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let next = payload.get("nextOffset").and_then(|v| v.as_u64());
        let mut head = format!("# {} - {} row(s)", key, rows.len());
        if let Some(t) = total {
            if t as usize != rows.len() {
                head.push_str(&format!(" of {t}"));
            }
        }
        if has_more {
            head.push_str(&format!(" (more: --offset {})", next.unwrap_or(0)));
        }
        lines.push(head);
        // Per-item notes (failures, no-captions) must never be invisible: a
        // run whose every video failed still has to say why.
        push_notes(payload, max, &mut lines);
        if rows.is_empty() {
            return lines.join("\n");
        }

        // Column selection: --fields > per-tool preference > scalar keys of
        // the first item.
        let cols: Vec<String> = if let Some(f) = &inv.fields {
            f.clone()
        } else if !spec.cols.is_empty() {
            let present: Vec<&str> = rows[0]
                .as_object()
                .map(|o| o.keys().map(|k| k.as_str()).collect())
                .unwrap_or_default();
            spec.cols
                .iter()
                .filter(|c| present.contains(c))
                .map(|c| c.to_string())
                .collect()
        } else {
            rows[0]
                .as_object()
                .map(|o| {
                    o.iter()
                        .filter(|(_, v)| !v.is_object() && !v.is_array())
                        .map(|(k, _)| k.clone())
                        .collect()
                })
                .unwrap_or_default()
        };
        if cols.is_empty() {
            lines.push(serde_json::to_string(&rows[0]).unwrap_or_default());
            return lines.join("\n");
        }
        lines.push(cols.join("\t"));
        for row in &rows {
            let cells: Vec<String> = cols
                .iter()
                .map(|c| cell(c.as_str(), row.get(c), max, tmax))
                .collect();
            lines.push(cells.join("\t"));
        }
        push_notes(payload, max, &mut lines);
    } else {
        // Object payload: flat key = value lines (nested -> compact JSON).
        lines.push(format!("# {}", spec.alias));
        emit_kv(payload, max, tmax, &mut lines);
    }
    lines.join("\n")
}

/// A top-level non-empty `notes` array (per-item failure/no-captions rows)
/// rendered as `!`-prefixed lines under the table. Only
/// kaltura_transcripts emits this shape today; any tool that adopts the
/// same convention gets the same visibility.
fn push_notes(payload: &Value, max: usize, lines: &mut Vec<String>) {
    // Content items the discovery walk could not read — each may hide
    // embedded videos, so they are reported, never dropped.
    if let Some(skipped) = payload.get("skippedLinks").and_then(|v| v.as_array()) {
        if !skipped.is_empty() {
            lines.push(String::new());
            lines.push(format!(
                "! {} content item(s) skipped during discovery (may contain videos)",
                skipped.len()
            ));
            for s in skipped {
                let title = s
                    .get("title")
                    .and_then(|v| v.as_str())
                    .filter(|t| !t.trim().is_empty())
                    .unwrap_or_else(|| s.get("contentId").and_then(|v| v.as_str()).unwrap_or("?"));
                let reason = s.get("reason").and_then(|v| v.as_str()).unwrap_or("");
                lines.push(format!("! skipped {title}: {}", truncate(reason, max)));
            }
        }
    }
    let Some(notes) = payload.get("notes").and_then(|v| v.as_array()) else {
        return;
    };
    if notes.is_empty() {
        return;
    }
    lines.push(String::new());
    lines.push(format!("! {} note(s)", notes.len()));
    for n in notes {
        let title = n
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("untitled")
            .trim();
        let title = if title.is_empty() { "untitled" } else { title };
        let note = n.get("note").and_then(|v| v.as_str()).unwrap_or("");
        lines.push(format!("! {title}: {}", truncate(note, max)));
    }
}

/// Fields that carry long-form content: capped at `text_chars` (2000 by
/// default) instead of the generic 200-char cell cap.
fn is_text_key(k: &str) -> bool {
    matches!(
        k,
        "text" | "content" | "body" | "markdown" | "rawText" | "snippet" | "description"
    )
}

fn emit_kv(v: &Value, max: usize, tmax: usize, lines: &mut Vec<String>) {
    match v {
        Value::Object(o) => {
            for (k, val) in o {
                let cap = if is_text_key(k) { tmax } else { max };
                match val {
                    Value::Object(_) | Value::Array(_) => {
                        let sv = truncate(&serde_json::to_string(val).unwrap_or_default(), cap);
                        lines.push(format!("{k}: {sv}"));
                    }
                    _ => lines.push(format!("{k}: {}", truncate(&scalar(val), cap))),
                }
            }
        }
        other => lines.push(truncate(&scalar(other), max)),
    }
}

fn cell(key: &str, v: Option<&Value>, max: usize, tmax: usize) -> String {
    let cap = if is_text_key(key) { tmax } else { max };
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(sv)) => truncate(sv, cap),
        Some(Value::Object(_)) | Some(Value::Array(_)) => {
            truncate(&serde_json::to_string(v.unwrap()).unwrap_or_default(), cap)
        }
        Some(other) => scalar(other),
    }
}

fn scalar(v: &Value) -> String {
    match v {
        Value::String(sv) => sv.clone(),
        Value::Bool(bv) => bv.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

fn truncate(sv: &str, max: usize) -> String {
    if sv.chars().count() <= max {
        return sv.to_string();
    }
    let cut: String = sv.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}\u{2026}")
}

// ---------------------------------------------------------------------------
// Help
// ---------------------------------------------------------------------------

pub fn help_text() -> String {
    let mut out = String::new();
    out.push_str("ntulearn-mcp - NTULearn (Blackboard) for agents\n\n");
    out.push_str("usage: ntulearn-mcp <command> [positional args] [--flags]\n\n");
    out.push_str("  help [cmd]              this list / command detail\n");
    out.push_str("  multi \"cmd ...\" ...     run several commands CONCURRENTLY in one process\n");
    out.push_str("  selfbench               built-in token + latency benchmark\n");
    out.push_str("  setup | check | refresh cookie lifecycle\n");
    out.push_str(
        "  update                  check for / install a newer release (--check, --yes,\n",
    );
    out.push_str("                          --auto on|off|ask, --status)\n");
    out.push_str("  version                 print this binary's version\n");
    out.push_str("  (no args)               run as an MCP server over stdio\n\n");
    out.push_str("commands (positional args shown; all also accept --kebab-flags):\n");
    let mut shown: Vec<&'static CmdSpec> = Vec::new();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for alias in CLI_ORDER {
        if let Some(c) = find_cmd(alias) {
            if seen.insert(c.alias) {
                shown.push(c);
            }
        }
    }
    // Tools registered after the curated list (new features) append at the
    // end so help never hides a registered command.
    for def in crate::registry::all() {
        if let Some(c) = def.cli.as_ref() {
            if seen.insert(c.alias) {
                shown.push(c);
            }
        }
    }
    for c in shown {
        let req: Vec<String> = c
            .args
            .iter()
            .filter(|a| a.required)
            .map(|a| snake_to_kebab(a.flag))
            .collect();
        out.push_str(&format!(
            "  {:<12} {:<26} {}\n",
            c.alias,
            req.join(" "),
            c.summary
        ));
    }
    out.push_str("\noutput: compact TSV by default; --json = raw payload; --md = markdown\n");
    out.push_str(
        "trim:   --fields a,b | --max-chars N (default 200; --full disables) | --limit N\n",
    );
    out.push_str(
        "        text-bearing fields (text/body/...) default to a 2000-char cap instead\n",
    );
    out
}

pub fn help_command(cmd_token: &str) -> String {
    let spec = match find_cmd(cmd_token) {
        Some(sv) => sv,
        None => return format!("unknown command: {cmd_token}"),
    };
    let mut out = format!(
        "ntulearn-mcp {}  (mcp tool: ntulearn_{})\n  {}\n",
        spec.alias, spec.short, spec.summary
    );
    let req: Vec<String> = spec
        .args
        .iter()
        .filter(|a| a.required)
        .map(|a| snake_to_kebab(a.flag))
        .collect();
    if !req.is_empty() {
        out.push_str(&format!(
            "\n  usage: ntulearn-mcp {} {}\n",
            spec.alias,
            req.join(" ")
        ));
    }
    out.push_str("\nflags:\n");
    for a in spec.args.iter() {
        let type_name_owned = match a.kind {
            ArgKind::Str => "str".to_string(),
            ArgKind::Int => "int".to_string(),
            ArgKind::Bool => "true|false".to_string(),
            ArgKind::Arr => "a,b,c".to_string(),
            ArgKind::Enum(v) => v.join("|"),
        };
        out.push_str(&format!(
            "  --{} <{}>{}\n",
            a.flag.replace('_', "-"),
            type_name_owned,
            if a.required { "  (required)" } else { "" }
        ));
        if !a.hint.is_empty() {
            out.push_str(&format!("      {}\n", a.hint));
        }
    }
    out.push_str("\nglobal: --json  --toon  --md  --fields a,b  --max-chars N  --full\n");
    out
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

/// Run one invocation against shared state and return stdout text.
pub async fn run_one(state: &Arc<AppState>, inv: &Invocation) -> Result<String, String> {
    let payload = handlers::dispatch_value(state, inv.spec.short, &inv.args).await?;
    if inv.toon {
        return Ok(crate::toon::encode(&payload));
    }
    if inv.raw {
        return Ok(serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string()));
    }
    if inv.md {
        // Reuse the MCP markdown renderer via the full dispatch path.
        let args = {
            let mut m = inv.args.as_object().cloned().unwrap_or_default();
            m.insert(
                "response_format".to_string(),
                Value::String("markdown".to_string()),
            );
            Value::Object(m)
        };
        let rendered =
            handlers::dispatch(state, &format!("ntulearn_{}", inv.spec.short), &args).await?;
        let text: Vec<String> = rendered
            .into_iter()
            .map(|c| match c {
                crate::mcp::ToolContent::Text { text } => text,
            })
            .collect();
        return Ok(text.join("\n"));
    }
    Ok(render_compact(inv, &payload))
}

/// Entry point for the CLI (argv without the program name).
pub async fn run_cli(argv: &[String]) -> i32 {
    let Some(first) = argv.first() else {
        eprintln!("{}", help_text());
        return 2;
    };
    match first.as_str() {
        "help" | "--help" | "-h" => {
            if let Some(cmd) = argv.get(1) {
                print!("{}", help_command(cmd));
            } else {
                print!("{}", help_text());
            }
            return 0;
        }
        "multi" | "parallel" => return run_multi(&argv[1..]).await,
        "selfbench" => {
            let verbose = argv.iter().any(|v| v == "--verbose");
            return crate::bench::run(verbose).await;
        }
        _ => {}
    }

    let inv = match parse_invocation(first, &argv[1..]) {
        Ok(inv) => inv,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let state = match crate::build_state().await {
        Ok(sv) => sv,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let t0 = Instant::now();
    match run_one(&state, &inv).await {
        Ok(text) => {
            println!("{text}");
            if std::env::var("NTULEARN_CLI_TIMING").as_deref() == Ok("1") {
                eprintln!("# wall: {:.1?}", t0.elapsed());
            }
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

/// Split a `multi` command string on whitespace, respecting quotes.
fn split_command(sv: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for ch in sv.chars() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => cur.push(ch),
            None => match ch {
                '"' | '\'' => quote = Some(ch),
                c if c.is_whitespace() => {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                }
                c => cur.push(c),
            },
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// `ntulearn-mcp multi "courses" "announcements --limit 5" ...` - all
/// commands run concurrently against one shared HTTP client + cache.
async fn run_multi(parts: &[String]) -> i32 {
    if parts.is_empty() {
        eprintln!("usage: ntulearn-mcp multi \"<cmd> [args]\" [\"<cmd2> ...\" ...]");
        return 2;
    }
    let mut invs: Vec<(String, Invocation)> = Vec::with_capacity(parts.len());
    // help/version results rendered directly (no state, no dispatch).
    let mut direct: Vec<(String, String)> = Vec::new();
    for part in parts {
        let toks = split_command(part);
        let Some(cmd) = toks.first() else { continue };
        // help/version inside multi: no state needed, just render it.
        match cmd.as_str() {
            "help" | "--help" | "-h" => {
                let text = match toks.get(1) {
                    Some(c) => help_command(c),
                    None => help_text(),
                };
                direct.push((part.clone(), text));
                continue;
            }
            "version" | "--version" | "-V" => {
                direct.push((
                    part.clone(),
                    format!("ntulearn-mcp {}", env!("CARGO_PKG_VERSION")),
                ));
                continue;
            }
            _ => {}
        }
        match parse_invocation(cmd, &toks[1..]) {
            Ok(inv) => invs.push((part.clone(), inv)),
            Err(e) => {
                eprintln!("error in {part:?}: {e}");
                return 2;
            }
        }
    }
    // All commands were help/version: no state or dispatch needed.
    if invs.is_empty() {
        for (label, text) in &direct {
            println!("== {label} ==");
            println!("{text}");
            println!();
        }
        println!("# {}/{} ok", direct.len(), direct.len());
        return 0;
    }
    let state = match crate::build_state().await {
        Ok(sv) => sv,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let t0 = Instant::now();
    let mut set = tokio::task::JoinSet::new();
    for (_, inv) in &invs {
        let state = state.clone();
        let inv = inv.clone();
        set.spawn(async move { run_one(&state, &inv).await });
    }
    let mut results: Vec<Result<String, String>> = Vec::with_capacity(invs.len());
    while let Some(res) = set.join_next().await {
        results.push(res.unwrap_or_else(|e| Err(format!("task panicked: {e}"))));
    }
    let mut fails = 0usize;
    for (label, text) in &direct {
        println!("== {label} ==");
        println!("{text}");
        println!();
    }
    for ((label, _), res) in invs.iter().zip(results.into_iter()) {
        println!("== {label} ==");
        match res {
            Ok(text) => println!("{text}"),
            Err(e) => {
                println!("ERROR: {e}");
                fails += 1;
            }
        }
        println!();
    }
    let ok = direct.len() + invs.len() - fails;
    println!(
        "# {ok}/{} ok in {:.1?}",
        direct.len() + invs.len(),
        t0.elapsed()
    );
    if fails > 0 { 1 } else { 0 }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(v: &[&str]) -> Vec<String> {
        v.iter().map(|sv| sv.to_string()).collect()
    }

    #[test]
    fn parses_positional_required_args() {
        let inv = parse_invocation(
            "contents",
            &toks(&["_2706844_1", "_123_1", "--limit", "10"]),
        )
        .unwrap();
        assert_eq!(inv.spec.short, "get_course_contents");
        assert_eq!(inv.args["course_id"], "_2706844_1");
        assert_eq!(inv.args["parent_id"], "_123_1");
        assert_eq!(inv.args["limit"], 10);
    }

    #[test]
    fn parses_kebab_flags_and_eq_values() {
        let inv = parse_invocation(
            "announcements",
            &toks(&[
                "--course-ids=_1_1,_2_2",
                "--since=2026-09-01T00:00:00Z",
                "--limit=5",
            ]),
        )
        .unwrap();
        assert_eq!(inv.args["course_ids"][0], "_1_1");
        assert_eq!(inv.args["since"], "2026-09-01T00:00:00Z");
        assert_eq!(inv.args["limit"], 5);
    }

    #[test]
    fn bare_bool_flag_is_true() {
        let inv = parse_invocation("courses", &toks(&["--include-disabled"])).unwrap();
        assert_eq!(inv.args["include_disabled"], true);
    }

    #[test]
    fn bare_bool_flag_followed_by_another_flag() {
        let inv =
            parse_invocation("messages", &toks(&["--unread-only", "--folder", "sent"])).unwrap();
        assert_eq!(inv.args["unread_only"], true);
        assert_eq!(inv.args["folder"], "sent");
    }

    #[test]
    fn enum_validation_rejects_bad_value() {
        assert!(parse_invocation("read", &toks(&["_1_1", "_2_2", "--mode", "bogus"])).is_err());
    }

    #[test]
    fn missing_required_is_an_error() {
        assert!(parse_invocation("contents", &toks(&[])).is_err());
    }

    #[test]
    fn unknown_flag_is_rejected() {
        assert!(parse_invocation("courses", &toks(&["--nope"])).is_err());
    }

    #[test]
    fn ntulearn_prefixed_names_resolve() {
        let inv = parse_invocation("ntulearn_list_courses", &toks(&[])).unwrap();
        assert_eq!(inv.spec.alias, "courses");
    }

    #[test]
    fn compact_render_shows_partial_totals_notes_and_skips() {
        // A partial transcript run must never look complete: `total` makes
        // the head line say "of N", and notes/skips are spelled out.
        let inv = parse_invocation("kaltura-transcripts", &toks(&["_2707070_1"])).unwrap();
        let payload = serde_json::json!({
            "courseId": "_2707070_1",
            "total": 3,
            "videoCount": 2,
            "videos": [
                {"contentId": "_1_1", "title": "Lecture A", "entryId": "0_a", "words": 100, "segments": 2},
                {"contentId": "_2_1", "title": "Lecture B", "entryId": "0_b", "words": 200, "segments": 3}
            ],
            "notes": [
                {"title": "Lecture C", "note": "no caption track on this video"}
            ],
            "skippedLinks": [
                {"contentId": "_9_9", "title": "Broken item", "reason": "HTTP 403"}
            ]
        });
        let text = render_compact(&inv, &payload);
        assert!(
            text.contains("# videos - 2 row(s) of 3"),
            "head must disclose the total: {text}"
        );
        assert!(text.contains("! 1 note(s)"), "notes header missing: {text}");
        assert!(text.contains("Lecture C: no caption track"), "{text}");
        assert!(
            text.contains("! 1 content item(s) skipped during discovery"),
            "{text}"
        );
        assert!(text.contains("skipped Broken item: HTTP 403"), "{text}");
    }

    #[test]
    fn compact_render_all_failed_run_still_explains_itself() {
        let inv = parse_invocation("kaltura-transcripts", &toks(&["_2707070_1"])).unwrap();
        let payload = serde_json::json!({
            "courseId": "_2707070_1",
            "total": 25,
            "videoCount": 0,
            "videos": [],
            "notes": [
                {"title": "Lecture A", "note": "launch failed: session expired"}
            ]
        });
        let text = render_compact(&inv, &payload);
        assert!(text.contains("# videos - 0 row(s) of 25"), "{text}");
        assert!(text.contains("launch failed"), "{text}");
    }

    #[test]
    fn compact_render_is_tsv_with_header() {
        let inv = parse_invocation("courses", &toks(&[])).unwrap();
        let payload = serde_json::json!({
            "courses": [
                {"courseId": "_1_1", "title": "SC2002", "available": "Yes", "lastAccessed": "2026-09-01"},
                {"courseId": "_2_2", "title": "HW0001", "available": "Yes", "lastAccessed": "2026-09-02"}
            ],
            "total": 2, "count": 2, "offset": 0, "limit": 50,
            "hasMore": false, "nextOffset": null
        });
        let text = render_compact(&inv, &payload);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "# courses - 2 row(s)");
        assert_eq!(lines[1], "courseId\ttitle\tavailable");
        assert_eq!(lines[2], "_1_1\tSC2002\tYes");
    }

    #[test]
    fn text_fields_get_larger_default_cap() {
        use serde_json::json;
        let inv = parse_invocation("read", &toks(&["_1_1", "_2_2"])).unwrap();
        assert_eq!(inv.max_chars, 200);
        assert_eq!(inv.text_chars, 2000);
        let payload = json!({
            "files": [{
                "name": "notes.pdf",
                "pages": 3,
                "chars": 9000,
                "text": "x".repeat(2600),
            }]
        });
        let out = render_compact(&inv, &payload);
        let text_cell = out
            .lines()
            .next_back()
            .unwrap()
            .split('\t')
            .next_back()
            .unwrap();
        // capped at 2000 chars + ellipsis marker
        assert_eq!(text_cell.chars().count(), 2000);
        // explicit --max-chars applies to text too
        let inv2 = parse_invocation("read", &toks(&["_1_1", "_2_2", "--max-chars", "50"])).unwrap();
        assert_eq!(inv2.text_chars, 50);
        // --full disables caps entirely
        let inv3 = parse_invocation("read", &toks(&["_1_1", "_2_2", "--full"])).unwrap();
        assert_eq!(inv3.text_chars, usize::MAX);
    }

    fn compact_render_truncates_long_values() {
        let inv = parse_invocation("courses", &toks(&[])).unwrap();
        let long = "x".repeat(500);
        let payload = serde_json::json!({
            "courses": [{"courseId": "_1_1", "title": long, "available": "Yes"}],
            "total": 1
        });
        let text = render_compact(&inv, &payload);
        assert!(text.lines().nth(2).unwrap().chars().count() < 220);
    }

    #[test]
    fn fields_selection_is_respected() {
        let inv = parse_invocation("courses", &toks(&["--fields=courseId"])).unwrap();
        let payload = serde_json::json!({
            "courses": [{"courseId": "_1_1", "title": "SC2002", "available": "Yes"}],
            "total": 1
        });
        let text = render_compact(&inv, &payload);
        assert!(text.contains("courseId"));
        assert!(!text.contains("title"));
    }

    #[test]
    fn split_command_respects_quotes() {
        assert_eq!(
            split_command("search _1_1 --query \"week 8\""),
            vec!["search", "_1_1", "--query", "week 8"]
        );
    }

    #[test]
    fn toon_flag_parsing_and_exclusivity() {
        let inv = parse_invocation("courses", &toks(&["--toon"])).unwrap();
        assert!(inv.toon && !inv.raw && !inv.md);
        // Mutually exclusive with --json and --md.
        assert!(parse_invocation("courses", &toks(&["--toon", "--json"])).is_err());
        assert!(parse_invocation("courses", &toks(&["--toon", "--md"])).is_err());
        // Default off.
        let inv = parse_invocation("courses", &toks(&[])).unwrap();
        assert!(!inv.toon);
    }
}
