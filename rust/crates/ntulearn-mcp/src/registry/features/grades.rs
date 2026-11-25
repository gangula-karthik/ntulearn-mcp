use crate::handlers;
use crate::mcp::Tool;
use crate::mcp::ToolAnnotations;
use crate::registry::cli_spec::{a, i, s, CmdSpec};
use crate::registry::tool;
use crate::registry::ToolDef;
use crate::render;
use serde_json::json;

// ---------------------------------------------------------------------
// MCP schemas (verbatim from schemas.rs)
// ---------------------------------------------------------------------

fn mcp_export_gradebook_csv() -> Tool {
    Tool::new(
            r####"ntulearn_export_gradebook_csv"####.to_string(),
            r####"Export your gradebook columns and scores across courses as a CSV string for a spreadsheet. Header: courseId,column,possible,score,grade,status. For a structured/paginated view, use ntulearn_get_gradebook. Example: ntulearn_export_gradebook_csv()"####.to_string(),
            json!({"type":"object","properties":{"course_ids":{"type":"array","description":"Optional list of course IDs to scope to. Omit to fan out across all available enrolled courses.","items":{"type":"string","pattern":"^[A-Za-z0-9_\\-:]+$"},"maxItems":200},"response_format":{"type":"string","description":"Return format: 'toon' (default; token-optimized, ~40% fewer tokens — best for agents), 'json' (structured) or 'markdown' (human-readable).","enum":["toon","json","markdown"],"default":"toon"}},"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Export gradebook as CSV"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
}

fn mcp_get_gradebook() -> Tool {
    Tool::new(
            r####"ntulearn_get_gradebook"####.to_string(),
            r####"Get gradebook columns across your enrolled courses, including your scores where available. Fans out across every available course by default; pass course_ids to scope. Each column carries its courseId. Paginate with limit/offset. For a spreadsheet-ready export, use ntulearn_export_gradebook_csv; for submission attempts on one column, use ntulearn_get_gradebook_attempts. Example: ntulearn_get_gradebook(course_ids=['_2706844_1'])"####.to_string(),
            json!({"type":"object","properties":{"course_ids":{"type":"array","description":"Optional list of course IDs to scope to. Omit to fan out across all available enrolled courses.","items":{"type":"string","pattern":"^[A-Za-z0-9_\\-:]+$"},"maxItems":200},"limit":{"type":"integer","description":"Max items to return per call (1–200, default 50). Use small values to keep results within the LLM's context.","minimum":1,"maximum":200,"default":50},"offset":{"type":"integer","description":"Number of items to skip for pagination. Use the nextOffset value returned by a previous call to walk pages.","minimum":0,"default":0},"response_format":{"type":"string","description":"Return format: 'toon' (default; token-optimized, ~40% fewer tokens — best for agents), 'json' (structured) or 'markdown' (human-readable).","enum":["toon","json","markdown"],"default":"toon"}},"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Get gradebook (cross-course)"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
}

fn mcp_get_gradebook_attempts() -> Tool {
    Tool::new(
            r####"ntulearn_get_gradebook_attempts"####.to_string(),
            r####"List submission attempts for a gradebook column (assignment). Column IDs come from ntulearn_get_gradebook. Pass user_id to scope to one student's attempts. Paginate with limit/offset. Example: ntulearn_get_gradebook_attempts(course_id='_2706844_1', column_id='_67890_1')"####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"column_id":{"type":"string","description":"Gradebook column ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"user_id":{"type":"string","description":"Optional user ID to scope attempts to one student.","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"limit":{"type":"integer","description":"Max items to return per call (1–200, default 50). Use small values to keep results within the LLM's context.","minimum":1,"maximum":200,"default":50},"offset":{"type":"integer","description":"Number of items to skip for pagination. Use the nextOffset value returned by a previous call to walk pages.","minimum":0,"default":0},"response_format":{"type":"string","description":"Return format: 'toon' (default; token-optimized, ~40% fewer tokens — best for agents), 'json' (structured) or 'markdown' (human-readable).","enum":["toon","json","markdown"],"default":"toon"}},"required":["course_id","column_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Get gradebook attempts"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
}

// ---------------------------------------------------------------------
// Markdown renderers (verbatim render_for_tool arms)
// ---------------------------------------------------------------------

fn md_export_gradebook_csv(payload: &serde_json::Value) -> String {
    render::md_export_gradebook(payload)
}

fn md_get_gradebook(payload: &serde_json::Value) -> String {
    let columns = payload
        .get("columns")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let available = payload
        .get("gradesAvailable")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let err = payload
        .get("gradeFetchError")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    render::md_gradebook(&columns, payload, available, err.as_deref())
}

fn md_get_gradebook_attempts(payload: &serde_json::Value) -> String {
    render::md_gradebook_attempts(payload)
}

// ---------------------------------------------------------------------
// CLI command specs (verbatim from cli.rs COMMANDS; consts so the
// arg arrays are compile-time promoted to 'static)
// ---------------------------------------------------------------------

const CMD_EXPORT_GRADEBOOK_CSV: CmdSpec = CmdSpec {
    short: "export_gradebook_csv",
    alias: "grades-csv",
    summary: "export gradebook to .csv",
    args: &[a("course_ids", "scope to course ids")],
    list_key: "",
    cols: &[],
};

const CMD_GET_GRADEBOOK: CmdSpec = CmdSpec {
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
};

const CMD_GET_GRADEBOOK_ATTEMPTS: CmdSpec = CmdSpec {
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
};

pub fn tools() -> Vec<ToolDef> {
    vec![
        tool! {
            short: "export_gradebook_csv",
            mcp: mcp_export_gradebook_csv(),
            cli: Some(CMD_EXPORT_GRADEBOOK_CSV),
            markdown: Some(md_export_gradebook_csv),
            handler: handlers::h_export_gradebook_csv,
        },
        tool! {
            short: "get_gradebook",
            mcp: mcp_get_gradebook(),
            cli: Some(CMD_GET_GRADEBOOK),
            markdown: Some(md_get_gradebook),
            handler: handlers::h_get_gradebook,
        },
        tool! {
            short: "get_gradebook_attempts",
            mcp: mcp_get_gradebook_attempts(),
            cli: Some(CMD_GET_GRADEBOOK_ATTEMPTS),
            markdown: Some(md_get_gradebook_attempts),
            handler: handlers::h_get_gradebook_attempts,
        },
    ]
}
