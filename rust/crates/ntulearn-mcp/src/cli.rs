//! Agent-first CLI surface for ntulearn-mcp.
//!
//! Motivation (measured, see `selfbench`): MCP hosts inject **every** tool
//! schema into the model's context on **every** turn. With 21 tools that is
//! tens of thousands of tokens before the agent does any work. A CLI pays
//! zero standing cost: the agent reads `ntulearn-mcp help` (~600 tokens) only
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

// ---------------------------------------------------------------------------
// Command spec table
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
pub enum ArgKind {
    Str,
    Int,
    Bool,
    Arr,
    Enum(&'static [&'static str]),
}

pub struct ArgSpec {
    pub flag: &'static str,
    pub kind: ArgKind,
    pub required: bool,
    pub hint: &'static str,
}

pub struct CmdSpec {
    /// MCP tool name without the `ntulearn_` prefix (dispatch key).
    pub short: &'static str,
    /// Short CLI alias (token-cheap to type).
    pub alias: &'static str,
    pub summary: &'static str,
    pub args: &'static [ArgSpec],
    /// Payload key that holds the item list for compact rendering.
    pub list_key: &'static str,
    /// Preferred compact columns (subset of item keys, in order).
    pub cols: &'static [&'static str],
}

const fn s(flag: &'static str, required: bool, hint: &'static str) -> ArgSpec {
    ArgSpec { flag, kind: ArgKind::Str, required, hint }
}
const fn i(flag: &'static str, hint: &'static str) -> ArgSpec {
    ArgSpec { flag, kind: ArgKind::Int, required: false, hint }
}
const fn b(flag: &'static str, hint: &'static str) -> ArgSpec {
    ArgSpec { flag, kind: ArgKind::Bool, required: false, hint }
}
const fn a(flag: &'static str, hint: &'static str) -> ArgSpec {
    ArgSpec { flag, kind: ArgKind::Arr, required: false, hint }
}
const fn e(flag: &'static str, vals: &'static [&'static str], hint: &'static str) -> ArgSpec {
    ArgSpec { flag, kind: ArgKind::Enum(vals), required: false, hint }
}

pub const COMMANDS: &[CmdSpec] = &[
    CmdSpec {
        short: "list_courses",
        alias: "courses",
        summary: "my enrolled courses (run this first for course ids)",
        args: &[b("include_disabled", "also list unavailable courses")],
        list_key: "courses",
        cols: &["courseId", "title", "available"],
    },
    CmdSpec {
        short: "get_course_contents",
        alias: "contents",
        summary: "one level of a course's content tree",
        args: &[
            s("course_id", true, "course id, e.g. _2706844_1"),
            s("parent_id", false, "folder id to drill into (omit = top level)"),
            i("limit", "max rows (default 50)"),
            i("offset", "pagination offset"),
        ],
        list_key: "items",
        cols: &["id", "title", "type", "hasChildren", "parentId"],
    },
    CmdSpec {
        short: "get_content_tree",
        alias: "tree",
        summary: "entire content tree of a course in one call",
        args: &[s("course_id", true, "course id"), i("max_depth", "recursion depth (default 5)")],
        list_key: "matches",
        cols: &["path", "title", "type", "id"],
    },
    CmdSpec {
        short: "search_course_content",
        alias: "search",
        summary: "search one course's content tree",
        args: &[
            s("course_id", true, "course id"),
            s("query", true, "case-insensitive substring"),
            i("max_depth", "recursion depth cap"),
            i("max_results", "max matches (default 50)"),
        ],
        list_key: "matches",
        cols: &["path", "title", "type", "id"],
    },
    CmdSpec {
        short: "search_all_courses",
        alias: "find",
        summary: "search content across ALL courses",
        args: &[
            s("query", true, "case-insensitive substring"),
            a("courseIds", "limit to these course ids"),
            i("max_depth", "recursion depth cap"),
            i("max_results", "max matches (default 50)"),
        ],
        list_key: "matches",
        cols: &["courseId", "path", "title", "type", "id"],
    },
    CmdSpec {
        short: "download_file",
        alias: "dl",
        summary: "download a content item's files to disk",
        args: &[
            s("course_id", true, "course id"),
            s("content_id", true, "content item id"),
            s("destination_dir", false, "target dir (default NTULEARN_DOWNLOAD_DIR or ./downloads)"),
        ],
        list_key: "files",
        cols: &["path", "bytes"],
    },
    CmdSpec {
        short: "read_file_content",
        alias: "read",
        summary: "read a content item's files inline (pdf/office/text)",
        args: &[
            s("course_id", true, "course id"),
            s("content_id", true, "content item id"),
            e("mode", &["text", "vision", "auto"], "pdf mode: text (cheap) or vision (diagrams)"),
            s("pages", false, "pdf page range, e.g. 1-10"),
        ],
        list_key: "files",
        cols: &["name", "pages", "chars", "text"],
    },
    CmdSpec {
        short: "get_upcoming",
        alias: "upcoming",
        summary: "calendar items + due dates across courses",
        args: &[
            a("course_ids", "scope to course ids"),
            s("since", false, "ISO-8601 start (default now)"),
            s("until", false, "ISO-8601 end (default +2w)"),
            e("type", &["Course", "GradebookColumn", "Institution", "OfficeHours", "Personal"], "item type; GradebookColumn = due dates"),
            i("limit", "max rows (default 50)"),
            i("offset", "pagination offset"),
        ],
        list_key: "items",
        cols: &["date", "type", "title", "courseId"],
    },
    CmdSpec {
        short: "get_announcements",
        alias: "announcements",
        summary: "announcements across courses, newest first",
        args: &[
            a("course_ids", "scope to course ids"),
            s("since", false, "ISO-8601 cutoff"),
            i("limit", "max rows (default 50)"),
            i("offset", "pagination offset"),
        ],
        list_key: "announcements",
        cols: &["created", "courseId", "title"],
    },
    CmdSpec {
        short: "get_gradebook",
        alias: "grades",
        summary: "gradebook columns + your grades across courses",
        args: &[
            a("course_ids", "scope to course ids"),
            i("limit", "max rows (default 50)"),
            i("offset", "pagination offset"),
        ],
        list_key: "columns",
        cols: &["courseId", "name", "score", "status", "dueDate"],
    },
    CmdSpec {
        short: "get_gradebook_attempts",
        alias: "attempts",
        summary: "attempts on one gradebook column",
        args: &[
            s("course_id", true, "course id"),
            s("column_id", true, "gradebook column id"),
            s("user_id", false, "filter to one user"),
            i("limit", "max rows (default 50)"),
            i("offset", "pagination offset"),
        ],
        list_key: "attempts",
        cols: &["userId", "status", "score", "attemptDate"],
    },
    CmdSpec {
        short: "list_messages",
        alias: "messages",
        summary: "course messages (inbox/sent)",
        args: &[
            e("folder", &["inbox", "sent"], "mailbox folder (default inbox)"),
            b("unread_only", "only unread"),
            s("since", false, "ISO-8601 cutoff"),
            i("limit", "max rows (default 50)"),
            i("offset", "pagination offset"),
        ],
        list_key: "messages",
        cols: &["sent", "courseId", "subject", "from", "unread"],
    },
    CmdSpec {
        short: "read_message",
        alias: "message",
        summary: "read one course message",
        args: &[s("message_id", true, "message id from `messages`")],
        list_key: "",
        cols: &[],
    },
    CmdSpec {
        short: "list_course_users",
        alias: "users",
        summary: "users enrolled in a course",
        args: &[
            s("course_id", true, "course id"),
            i("limit", "max rows (default 50)"),
            i("offset", "pagination offset"),
        ],
        list_key: "users",
        cols: &["name", "role", "userId"],
    },
    CmdSpec {
        short: "list_course_groups",
        alias: "groups",
        summary: "groups in a course",
        args: &[
            s("course_id", true, "course id"),
            i("limit", "max rows (default 50)"),
            i("offset", "pagination offset"),
        ],
        list_key: "groups",
        cols: &["name", "groupId", "memberCount"],
    },
    CmdSpec {
        short: "get_group_members",
        alias: "members",
        summary: "members of one group",
        args: &[s("course_id", true, "course id"), s("group_id", true, "group id")],
        list_key: "members",
        cols: &["name", "userId"],
    },
    CmdSpec {
        short: "summarize_course",
        alias: "summary",
        summary: "one-shot course digest (staff, contents, dates)",
        args: &[s("course_id", true, "course id"), b("include_contents", "include content tree (default true)")],
        list_key: "",
        cols: &[],
    },
    CmdSpec {
        short: "whats_new",
        alias: "whats-new",
        summary: "curated digest of everything new since a cutoff",
        args: &[
            a("course_ids", "scope to course ids"),
            s("since", false, "ISO-8601 cutoff"),
            b("update_tracker", "advance the last-seen watermark"),
        ],
        list_key: "",
        cols: &[],
    },
    CmdSpec {
        short: "download_course",
        alias: "dl-course",
        summary: "bulk-download a course's files",
        args: &[
            s("course_id", true, "course id"),
            s("destination_dir", false, "target dir"),
            i("max_depth", "recursion depth cap"),
            s("include_extensions", false, "comma list, e.g. pdf,pptx"),
            b("skip_existing", "default true"),
            i("parallel", "concurrent downloads (default 4)"),
        ],
        list_key: "files",
        cols: &["path", "bytes"],
    },
    CmdSpec {
        short: "export_calendar_ics",
        alias: "ics",
        summary: "export calendar items to an .ics file",
        args: &[
            a("course_ids", "scope to course ids"),
            s("since", false, "ISO-8601 start"),
            s("until", false, "ISO-8601 end"),
        ],
        list_key: "",
        cols: &[],
    },
    CmdSpec {
        short: "export_gradebook_csv",
        alias: "grades-csv",
        summary: "export gradebook to .csv",
        args: &[a("course_ids", "scope to course ids")],
        list_key: "",
        cols: &[],
    },
];

pub fn find_cmd(name: &str) -> Option<&'static CmdSpec> {
    let stripped = name.strip_prefix("ntulearn_").unwrap_or(name);
    COMMANDS.iter().find(|c| c.alias == name || c.short == stripped)
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
}

#[derive(Clone)]
struct OutputFlags {
    raw: bool,
    md: bool,
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
    v.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect()
}

/// Parse `argv` (after the command token) into a JSON args object.
///
/// Supports `--flag value`, `--flag=value`, bare `--flag` (true), positional
/// values for required string args (in spec order), and the global output
/// flags. A value that itself starts with `--` must use `--flag=value`.
pub fn parse_invocation(cmd_token: &str, rest: &[String]) -> Result<Invocation, String> {
    let spec = find_cmd(cmd_token)
        .ok_or_else(|| format!("unknown command: {cmd_token} (run `ntulearn-mcp help`)"))?;

    let mut out = OutputFlags { raw: false, md: false, fields: None, max_chars: 200, full: false, chars_set: false };
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
                    return Err(format!("--{} must be one of: {}", snake_to_kebab(&flag), vals.join("|")));
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
        let has_more = payload.get("hasMore").and_then(|v| v.as_bool()).unwrap_or(false);
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
    } else {
        // Object payload: flat key = value lines (nested -> compact JSON).
        lines.push(format!("# {}", spec.alias));
        emit_kv(payload, max, tmax, &mut lines);
    }
    lines.join("\n")
}

/// Fields that carry long-form content: capped at `text_chars` (2000 by
/// default) instead of the generic 200-char cell cap.
fn is_text_key(k: &str) -> bool {
    matches!(k, "text" | "content" | "body" | "markdown" | "rawText" | "snippet" | "description")
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
        Some(Value::Object(_)) | Some(Value::Array(_)) => truncate(
            &serde_json::to_string(v.unwrap()).unwrap_or_default(),
            cap,
        ),
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
    out.push_str("  (no args)               run as an MCP server over stdio\n\n");
    out.push_str("commands (positional args shown; all also accept --kebab-flags):\n");
    for c in COMMANDS {
        let req: Vec<String> = c
            .args
            .iter()
            .filter(|a| a.required)
            .map(|a| snake_to_kebab(a.flag))
            .collect();
        out.push_str(&format!("  {:<12} {:<26} {}\n", c.alias, req.join(" "), c.summary));
    }
    out.push_str("\noutput: compact TSV by default; --json = raw payload; --md = markdown\n");
    out.push_str("trim:   --fields a,b | --max-chars N (default 200; --full disables) | --limit N\n");
    out.push_str("        text-bearing fields (text/body/...) default to a 2000-char cap instead\n");
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
        out.push_str(&format!("\n  usage: ntulearn-mcp {} {}\n", spec.alias, req.join(" ")));
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
    out.push_str("\nglobal: --json  --md  --fields a,b  --max-chars N  --full\n");
    out
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

/// Run one invocation against shared state and return stdout text.
pub async fn run_one(state: &Arc<AppState>, inv: &Invocation) -> Result<String, String> {
    let payload = handlers::dispatch_value(state, inv.spec.short, &inv.args).await?;
    if inv.raw {
        return Ok(serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string()));
    }
    if inv.md {
        // Reuse the MCP markdown renderer via the full dispatch path.
        let args = {
            let mut m = inv.args.as_object().cloned().unwrap_or_default();
            m.insert("response_format".to_string(), Value::String("markdown".to_string()));
            Value::Object(m)
        };
        let rendered =
            handlers::dispatch(state, &format!("ntulearn_{}", inv.spec.short), &args).await?;
        let text: Vec<String> = rendered
            .into_iter()
            .filter_map(|c| match c {
                ultrafast_mcp::ToolContent::Text { text } => Some(text),
                _ => None,
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
                direct.push((part.clone(), format!("ntulearn-mcp {}", env!("CARGO_PKG_VERSION"))));
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
    println!("# {ok}/{} ok in {:.1?}", direct.len() + invs.len(), t0.elapsed());
    if fails > 0 {
        1
    } else {
        0
    }
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
        let inv =
            parse_invocation("contents", &toks(&["_2706844_1", "_123_1", "--limit", "10"])).unwrap();
        assert_eq!(inv.spec.short, "get_course_contents");
        assert_eq!(inv.args["course_id"], "_2706844_1");
        assert_eq!(inv.args["parent_id"], "_123_1");
        assert_eq!(inv.args["limit"], 10);
    }

    #[test]
    fn parses_kebab_flags_and_eq_values() {
        let inv = parse_invocation(
            "announcements",
            &toks(&["--course-ids=_1_1,_2_2", "--since=2026-09-01T00:00:00Z", "--limit=5"]),
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
        let inv = parse_invocation("messages", &toks(&["--unread-only", "--folder", "sent"])).unwrap();
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
        let text_cell = out.lines().last().unwrap().split('\t').last().unwrap();
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
}
