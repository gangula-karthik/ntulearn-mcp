use crate::handlers;
use crate::mcp::Tool;
use crate::mcp::ToolAnnotations;
use crate::registry::cli_spec::{a, b, s, CmdSpec};
use crate::registry::tool;
use crate::registry::ToolDef;
use crate::render;
use serde_json::json;

// ---------------------------------------------------------------------
// MCP schemas (verbatim from schemas.rs)
// ---------------------------------------------------------------------

fn mcp_summarize_course() -> Tool {
    Tool::new(
            r####"ntulearn_summarize_course"####.to_string(),
            r####"One-call course briefing: title, description, instructors, enrollment count, upcoming due dates, recent announcements, gradebook summary, and top-level content folders. Each sub-section degrades gracefully on error. Great first tool when meeting a new course. Example: ntulearn_summarize_course(course_id='_2706844_1')"####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"include_contents":{"type":"boolean","description":"Include the top-level content folders in the briefing.","default":true},"response_format":{"type":"string","description":"Return format: 'toon' (default; token-optimized, ~40% fewer tokens — best for agents), 'json' (structured) or 'markdown' (human-readable).","enum":["toon","json","markdown"],"default":"toon"}},"required":["course_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Summarize a course"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
}

fn mcp_whats_new() -> Tool {
    Tool::new(
            r####"ntulearn_whats_new"####.to_string(),
            r####"One-call 'what changed recently' digest: recent announcements + upcoming due dates + gradebook summary across your courses since a cutoff. Defaults to the tracker's last-seen time (or last 7 days). update_tracker=true records `since` as the new last-seen for future calls. For the RAW unfiltered due-date feed, use ntulearn_get_upcoming. Example: ntulearn_whats_new(update_tracker=true)"####.to_string(),
            json!({"type":"object","properties":{"course_ids":{"type":"array","description":"Optional list of course IDs to scope to. Omit to fan out across all available enrolled courses.","items":{"type":"string","pattern":"^[A-Za-z0-9_\\-:]+$"},"maxItems":200},"since":{"type":"string","description":"Optional ISO-8601 cutoff. Defaults to the tracker's last-seen time, or 7 days ago if never set."},"update_tracker":{"type":"boolean","description":"Persist `since` as the new last-seen marker for future ntulearn_whats_new calls.","default":false},"response_format":{"type":"string","description":"Return format: 'toon' (default; token-optimized, ~40% fewer tokens — best for agents), 'json' (structured) or 'markdown' (human-readable).","enum":["toon","json","markdown"],"default":"toon"}},"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"What's new across courses"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
}

// ---------------------------------------------------------------------
// Markdown renderers (verbatim render_for_tool arms)
// ---------------------------------------------------------------------

fn md_summarize_course(payload: &serde_json::Value) -> String {
    render::md_summarize_course(payload)
}

fn md_whats_new(payload: &serde_json::Value) -> String {
    render::md_whats_new(payload)
}

// ---------------------------------------------------------------------
// CLI command specs (verbatim from cli.rs COMMANDS; consts so the
// arg arrays are compile-time promoted to 'static)
// ---------------------------------------------------------------------

const CMD_SUMMARIZE_COURSE: CmdSpec = CmdSpec {
    short: "summarize_course",
    alias: "summary",
    summary: "one-shot course digest (staff, contents, dates)",
    args: &[
        s("course_id", true, "course id"),
        b("include_contents", "include content tree (default true)"),
    ],
    list_key: "",
    cols: &[],
};

const CMD_WHATS_NEW: CmdSpec = CmdSpec {
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
};

pub fn tools() -> Vec<ToolDef> {
    vec![
        tool! {
            short: "summarize_course",
            mcp: mcp_summarize_course(),
            cli: Some(CMD_SUMMARIZE_COURSE),
            markdown: Some(md_summarize_course),
            handler: handlers::h_summarize_course,
        },
        tool! {
            short: "whats_new",
            mcp: mcp_whats_new(),
            cli: Some(CMD_WHATS_NEW),
            markdown: Some(md_whats_new),
            handler: handlers::h_whats_new,
        },
    ]
}
