use crate::handlers;
use crate::mcp::Tool;
use crate::mcp::ToolAnnotations;
use crate::registry::cli_spec::{a, e, i, s, CmdSpec};
use crate::registry::tool;
use crate::registry::ToolDef;
use crate::render;
use serde_json::json;

// ---------------------------------------------------------------------
// MCP schemas (verbatim from schemas.rs)
// ---------------------------------------------------------------------

fn mcp_export_calendar_ics() -> Tool {
    Tool::new(
            r####"ntulearn_export_calendar_ics"####.to_string(),
            r####"Export calendar items (including assignment due dates) as an iCalendar (.ics) string you can paste into Google Calendar / Apple Calendar / Outlook. Optionally scope with course_ids and a since/until window. Example: ntulearn_export_calendar_ics(course_ids=['_2706844_1'])"####.to_string(),
            json!({"type":"object","properties":{"course_ids":{"type":"array","description":"Optional list of course IDs to scope to. Omit to fan out across all available enrolled courses.","items":{"type":"string","pattern":"^[A-Za-z0-9_\\-:]+$"},"maxItems":200},"since":{"type":"string","description":"Optional ISO-8601 cutoff (e.g. '2026-05-09T00:00:00Z'). Only items created on/after this time are included."},"until":{"type":"string","description":"Optional ISO-8601 end of the window (e.g. '2026-05-23T00:00:00Z')."},"response_format":{"type":"string","description":"Return format: 'toon' (default; token-optimized, ~40% fewer tokens — best for agents), 'json' (structured) or 'markdown' (human-readable).","enum":["toon","json","markdown"],"default":"toon"}},"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Export calendar as ICS"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
}

fn mcp_get_upcoming() -> Tool {
    Tool::new(
            r####"ntulearn_get_upcoming"####.to_string(),
            r####"Get upcoming calendar items and assignment due dates across your enrolled courses (raw feed). Wraps Blackboard's calendar API — due dates surface as items with type='GradebookColumn'. Defaults to the next 2 weeks across every available course (fans out in parallel). Scope with course_ids, since/until (ISO-8601), or type (e.g. type='GradebookColumn' for due dates only). For a curated cross-course digest, use ntulearn_whats_new instead. Example: ntulearn_get_upcoming(course_ids=['_2706844_1'], type='GradebookColumn')"####.to_string(),
            json!({"type":"object","properties":{"since":{"type":"string","description":"ISO-8601 start of the window (e.g. '2026-05-09T00:00:00Z'). Omit to default to now."},"until":{"type":"string","description":"ISO-8601 end of the window. Omit to default to two weeks after `since`."},"course_ids":{"type":"array","description":"Optional list of course IDs to scope to. Omit to fan out across all available enrolled courses.","items":{"type":"string","pattern":"^[A-Za-z0-9_\\-:]+$"},"maxItems":200},"type":{"type":"string","description":"Optional calendar item type filter. Use 'GradebookColumn' for assignment due dates only.","enum":["Course","GradebookColumn","Institution","OfficeHours","Personal"]},"limit":{"type":"integer","description":"Max items to return per call (1–200, default 50). Use small values to keep results within the LLM's context.","minimum":1,"maximum":200,"default":50},"offset":{"type":"integer","description":"Number of items to skip for pagination. Use the nextOffset value returned by a previous call to walk pages.","minimum":0,"default":0},"response_format":{"type":"string","description":"Return format: 'toon' (default; token-optimized, ~40% fewer tokens — best for agents), 'json' (structured) or 'markdown' (human-readable).","enum":["toon","json","markdown"],"default":"toon"}},"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Get upcoming items / due dates"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
}

// ---------------------------------------------------------------------
// Markdown renderers (verbatim render_for_tool arms)
// ---------------------------------------------------------------------

fn md_export_calendar_ics(payload: &serde_json::Value) -> String {
    render::md_export_calendar(payload)
}

fn md_get_upcoming(payload: &serde_json::Value) -> String {
    let items = payload
        .get("items")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let errors = payload.get("courseErrors").cloned().unwrap_or(json!({}));
    render::md_upcoming(&items, payload, &errors)
}

// ---------------------------------------------------------------------
// CLI command specs (verbatim from cli.rs COMMANDS; consts so the
// arg arrays are compile-time promoted to 'static)
// ---------------------------------------------------------------------

const CMD_EXPORT_CALENDAR_ICS: CmdSpec = CmdSpec {
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
};

const CMD_GET_UPCOMING: CmdSpec = CmdSpec {
    short: "get_upcoming",
    alias: "upcoming",
    summary: "calendar items + due dates across courses",
    args: &[
        a("course_ids", "scope to course ids"),
        s("since", false, "ISO-8601 start (default now)"),
        s("until", false, "ISO-8601 end (default +2w)"),
        e(
            "type",
            &[
                "Course",
                "GradebookColumn",
                "Institution",
                "OfficeHours",
                "Personal",
            ],
            "item type; GradebookColumn = due dates",
        ),
        i("limit", "max rows (default 50)"),
        i("offset", "pagination offset"),
    ],
    list_key: "items",
    cols: &["date", "type", "title", "courseId"],
};

pub fn tools() -> Vec<ToolDef> {
    vec![
        tool! {
            short: "export_calendar_ics",
            mcp: mcp_export_calendar_ics(),
            cli: Some(CMD_EXPORT_CALENDAR_ICS),
            markdown: Some(md_export_calendar_ics),
            handler: handlers::h_export_calendar_ics,
        },
        tool! {
            short: "get_upcoming",
            mcp: mcp_get_upcoming(),
            cli: Some(CMD_GET_UPCOMING),
            markdown: Some(md_get_upcoming),
            handler: handlers::h_get_upcoming,
        },
    ]
}
