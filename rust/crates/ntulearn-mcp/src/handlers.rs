
//! Tool dispatch: maps the 21 MCP tool names to typed handler functions.
//!
//! Every handler mirrors a Python counterpart in `src/ntulearn_mcp/handlers.py`
//! (same name without the leading `h_`). Handlers return `Ok(contents)` for
//! success, `Err(LLM-readable message)` for failure; `tools.rs` wraps them with
//! `is_error` flags so callers see actionable text (matching the Python layer).

use serde_json::{json, Value};
use ultrafast_mcp::ToolContent;

use crate::AppState;

pub async fn dispatch(
    state: &AppState,
    name: &str,
    args: &Value,
) -> Result<Vec<ToolContent>, String> {
    let short = name.strip_prefix("ntulearn_").unwrap_or(name);
    let out: Value = match short {
        "list_courses" => h_list_courses(state, args).await?,
        "get_course_contents" => h_get_course_contents(state, args).await?,
        "search_course_content" => h_search_course_content(state, args).await?,
        "download_file" => h_download_file(state, args).await?,
        "read_file_content" => h_read_file_content(state, args).await?,
        "get_upcoming" => h_get_upcoming(state, args).await?,
        "get_announcements" => h_get_announcements(state, args).await?,
        "get_gradebook" => h_get_gradebook(state, args).await?,
        "list_messages" => h_list_messages(state, args).await?,
        "read_message" => h_read_message(state, args).await?,
        "list_course_users" => h_list_course_users(state, args).await?,
        "list_course_groups" => h_list_course_groups(state, args).await?,
        "get_group_members" => h_get_group_members(state, args).await?,
        "get_gradebook_attempts" => h_get_gradebook_attempts(state, args).await?,
        "search_all_courses" => h_search_all_courses(state, args).await?,
        "get_content_tree" => h_get_content_tree(state, args).await?,
        "download_course" => h_download_course(state, args).await?,
        "whats_new" => h_whats_new(state, args).await?,
        "export_calendar_ics" => h_export_calendar_ics(state, args).await?,
        "export_gradebook_csv" => h_export_gradebook_csv(state, args).await?,
        "summarize_course" => h_summarize_course(state, args).await?,
        other => {
            return Err(format!("unknown tool: ntulearn_{other}"));
        }
    };

    // Render policy: tools that already produce files (ics/csv) or markdown are
    // handled by the individual handlers. Everything else defaults to markdown.
    let text = render_for_tool(short, &out);
    Ok(vec![ToolContent::text(text)])
}

/// Shared arg helpers -------------------------------------------------------

/// Resolve the optional `response_format` (default "markdown").
pub fn response_format(args: &Value) -> String {
    args.get("response_format")
        .and_then(|v| v.as_str())
        .unwrap_or("markdown")
        .to_string()
}

pub fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|v| v.as_str())
}

pub fn u64_arg(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(|v| v.as_u64())
}

pub fn bool_arg(args: &Value, key: &str, default: bool) -> bool {
    args.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
}

// TODO(subagent-B): implement each handler below, porting the logic from
// src/ntulearn_mcp/handlers.py. Use crate::render::* helpers for markdown/csv/ics
// and crate::client / crate::parsers for data + HTML. Keep the `Value` in / text
// out contract. The next 21 bodies are placeholders that produce honest errors.

async fn not_impl(name: &str) -> Result<Value, String> {
    Err(format!(
        "[rust] tool ntulearn_{name} is not yet implemented in the Rust port"
    ))
}

macro_rules! nimp {
    ($name:ident) => {
        async fn $name(_s: &AppState, _a: &Value) -> Result<Value, String> {
            not_impl(stringify!($name)).await
        }
    };
}

nimp!(h_list_courses);
nimp!(h_get_course_contents);
nimp!(h_search_course_content);
nimp!(h_download_file);
nimp!(h_read_file_content);
nimp!(h_get_upcoming);
nimp!(h_get_announcements);
nimp!(h_get_gradebook);
nimp!(h_list_messages);
nimp!(h_read_message);
nimp!(h_list_course_users);
nimp!(h_list_course_groups);
nimp!(h_get_group_members);
nimp!(h_get_gradebook_attempts);
nimp!(h_search_all_courses);
nimp!(h_get_content_tree);
nimp!(h_download_course);
nimp!(h_whats_new);
nimp!(h_export_calendar_ics);
nimp!(h_export_gradebook_csv);
nimp!(h_summarize_course);

/// Placeholder renderer; subagent-C replaces with the full render module.
fn render_for_tool(_short: &str, out: &Value) -> String {
    serde_json::to_string_pretty(out).unwrap_or_default()
}
