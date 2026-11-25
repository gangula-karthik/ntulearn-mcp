use crate::handlers;
use crate::mcp::Tool;
use crate::mcp::ToolAnnotations;
use crate::registry::cli_spec::{a, b, i, s, CmdSpec};
use crate::registry::tool;
use crate::registry::ToolDef;
use crate::render;
use serde_json::json;

// ---------------------------------------------------------------------
// MCP schemas (verbatim from schemas.rs)
// ---------------------------------------------------------------------

fn mcp_get_content_tree() -> Tool {
    Tool::new(
            r####"ntulearn_get_content_tree"####.to_string(),
            r####"Return a course's ENTIRE content tree as nested JSON (folders with children) in one call — unlike ntulearn_get_course_contents, which lists one level per call. Bounds the walk with max_depth (capped 10). Prefer when you need the full outline/map of a course; prefer ntulearn_get_course_contents for browsing folder by folder. Example: ntulearn_get_content_tree(course_id='_2706844_1', max_depth=3)"####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"max_depth":{"type":"integer","description":"Maximum recursion depth (capped at 10).","default":5,"minimum":1,"maximum":10},"response_format":{"type":"string","description":"Return format: 'toon' (default; token-optimized, ~40% fewer tokens — best for agents), 'json' (structured) or 'markdown' (human-readable).","enum":["toon","json","markdown"],"default":"toon"}},"required":["course_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Get full course content tree"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
}

fn mcp_get_course_contents() -> Tool {
    Tool::new(
            r####"ntulearn_get_course_contents"####.to_string(),
            r####"Walk a course's content tree one level at a time (shallow). Omit parent_id to list the top-level items (folders, documents, links, assignments); pass the parent_id of a folder where hasChildren=true to drill into its children. Returns items with id, title, type, hasChildren. For the ENTIRE tree in one call, use ntulearn_get_content_tree instead. Example: ntulearn_get_course_contents(course_id='_2706844_1')"####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"parent_id":{"type":"string","description":"Optional content item ID of a folder/lesson to list children of. Omit to list the course's top-level items.","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"limit":{"type":"integer","description":"Max items to return per call (1–200, default 50). Use small values to keep results within the LLM's context.","minimum":1,"maximum":200,"default":50},"offset":{"type":"integer","description":"Number of items to skip for pagination. Use the nextOffset value returned by a previous call to walk pages.","minimum":0,"default":0},"response_format":{"type":"string","description":"Return format: 'toon' (default; token-optimized, ~40% fewer tokens — best for agents), 'json' (structured) or 'markdown' (human-readable).","enum":["toon","json","markdown"],"default":"toon"}},"required":["course_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Get course contents (root or folder children)"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
}

fn mcp_list_courses() -> Tool {
    Tool::new(
            r####"ntulearn_list_courses"####.to_string(),
            r####"List courses you are enrolled in on NTULearn. Use this FIRST to obtain course IDs (_12345_1) needed by other tools. By default returns only active/available courses; set include_disabled=true to include unavailable ones. Returns id, title, availability. Paginate with limit/offset. To find ANY course (including ones you are not enrolled in), use ntulearn_search_all_courses instead. Example: ntulearn_list_courses(limit=25)"####.to_string(),
            json!({"type":"object","properties":{"include_disabled":{"type":"boolean","description":"Include courses where availability.available != 'Yes'","default":false},"limit":{"type":"integer","description":"Max items to return per call (1–200, default 50). Use small values to keep results within the LLM's context.","minimum":1,"maximum":200,"default":50},"offset":{"type":"integer","description":"Number of items to skip for pagination. Use the nextOffset value returned by a previous call to walk pages.","minimum":0,"default":0},"response_format":{"type":"string","description":"Return format: 'toon' (default; token-optimized, ~40% fewer tokens — best for agents), 'json' (structured) or 'markdown' (human-readable).","enum":["toon","json","markdown"],"default":"toon"}},"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"List my NTULearn courses"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
}

fn mcp_search_all_courses() -> Tool {
    Tool::new(
            r####"ntulearn_search_all_courses"####.to_string(),
            r####"Search across ALL your enrolled courses' content trees for items matching a query. Like ntulearn_search_course_content but scoped to every available course at once; each match carries its courseId and breadcrumb for attribution. Limit with max_results. Example: ntulearn_search_all_courses(query='midterm')"####.to_string(),
            json!({"type":"object","properties":{"query":{"type":"string","description":"Search term (case-insensitive substring)","minLength":1,"maxLength":200},"course_ids":{"type":"array","description":"Optional list of course IDs to scope to. Omit to fan out across all available enrolled courses.","items":{"type":"string","pattern":"^[A-Za-z0-9_\\-:]+$"},"maxItems":200},"max_depth":{"type":"integer","description":"Maximum recursion depth per course (default 3, capped at 10).","default":3,"minimum":1,"maximum":10},"max_results":{"type":"integer","description":"Maximum number of matching items to return (default 50, capped at 200)","default":50,"minimum":1,"maximum":200},"response_format":{"type":"string","description":"Return format: 'toon' (default; token-optimized, ~40% fewer tokens — best for agents), 'json' (structured) or 'markdown' (human-readable).","enum":["toon","json","markdown"],"default":"toon"}},"required":["query"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Search content across all courses"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
}

fn mcp_search_course_content() -> Tool {
    Tool::new(
            r####"ntulearn_search_course_content"####.to_string(),
            r####"Recursively search ONE course's entire content tree for items matching a query (title or description; case-insensitive substring). Returns matched items with breadcrumb paths. Searching across ALL your courses at once? Use ntulearn_search_all_courses. Example: ntulearn_search_course_content(course_id='_2706844_1', query='assignment')"####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"query":{"type":"string","description":"Search term (case-insensitive substring)","minLength":1,"maxLength":200},"max_depth":{"type":"integer","description":"Maximum recursion depth (default 5, capped at 10)","default":5,"minimum":1,"maximum":10},"max_results":{"type":"integer","description":"Maximum number of matching items to return (default 50, capped at 200)","default":50,"minimum":1,"maximum":200},"response_format":{"type":"string","description":"Return format: 'toon' (default; token-optimized, ~40% fewer tokens — best for agents), 'json' (structured) or 'markdown' (human-readable).","enum":["toon","json","markdown"],"default":"toon"}},"required":["course_id","query"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Search course content"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
}

// ---------------------------------------------------------------------
// Markdown renderers (verbatim render_for_tool arms)
// ---------------------------------------------------------------------

fn md_get_content_tree(payload: &serde_json::Value) -> String {
    render::md_content_tree(payload)
}

fn md_get_course_contents(payload: &serde_json::Value) -> String {
    let items = payload
        .get("items")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    render::md_content_items_full(&items, payload)
}

fn md_list_courses(payload: &serde_json::Value) -> String {
    let courses = payload
        .get("courses")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    render::md_courses_full(&courses, payload)
}

fn md_search_all_courses(payload: &serde_json::Value) -> String {
    render::md_search_all_courses(payload)
}

fn md_search_course_content(payload: &serde_json::Value) -> String {
    let matches = payload
        .get("matches")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    render::md_search_results(&matches)
}

// ---------------------------------------------------------------------
// CLI command specs (verbatim from cli.rs COMMANDS; consts so the
// arg arrays are compile-time promoted to 'static)
// ---------------------------------------------------------------------

const CMD_GET_CONTENT_TREE: CmdSpec = CmdSpec {
    short: "get_content_tree",
    alias: "tree",
    summary: "entire content tree of a course in one call",
    args: &[
        s("course_id", true, "course id"),
        i("max_depth", "recursion depth (default 5)"),
    ],
    list_key: "matches",
    cols: &["path", "title", "type", "id"],
};

const CMD_GET_COURSE_CONTENTS: CmdSpec = CmdSpec {
    short: "get_course_contents",
    alias: "contents",
    summary: "one level of a course's content tree",
    args: &[
        s("course_id", true, "course id, e.g. _2706844_1"),
        s(
            "parent_id",
            false,
            "folder id to drill into (omit = top level)",
        ),
        i("limit", "max rows (default 50)"),
        i("offset", "pagination offset"),
    ],
    list_key: "items",
    cols: &["id", "title", "type", "hasChildren", "parentId"],
};

const CMD_LIST_COURSES: CmdSpec = CmdSpec {
    short: "list_courses",
    alias: "courses",
    summary: "my enrolled courses (run this first for course ids)",
    args: &[b("include_disabled", "also list unavailable courses")],
    list_key: "courses",
    cols: &["courseId", "title", "available"],
};

const CMD_SEARCH_ALL_COURSES: CmdSpec = CmdSpec {
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
};

const CMD_SEARCH_COURSE_CONTENT: CmdSpec = CmdSpec {
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
};

pub fn tools() -> Vec<ToolDef> {
    vec![
        tool! {
            short: "get_content_tree",
            mcp: mcp_get_content_tree(),
            cli: Some(CMD_GET_CONTENT_TREE),
            markdown: Some(md_get_content_tree),
            handler: handlers::h_get_content_tree,
        },
        tool! {
            short: "get_course_contents",
            mcp: mcp_get_course_contents(),
            cli: Some(CMD_GET_COURSE_CONTENTS),
            markdown: Some(md_get_course_contents),
            handler: handlers::h_get_course_contents,
        },
        tool! {
            short: "list_courses",
            mcp: mcp_list_courses(),
            cli: Some(CMD_LIST_COURSES),
            markdown: Some(md_list_courses),
            handler: handlers::h_list_courses,
        },
        tool! {
            short: "search_all_courses",
            mcp: mcp_search_all_courses(),
            cli: Some(CMD_SEARCH_ALL_COURSES),
            markdown: Some(md_search_all_courses),
            handler: handlers::h_search_all_courses,
        },
        tool! {
            short: "search_course_content",
            mcp: mcp_search_course_content(),
            cli: Some(CMD_SEARCH_COURSE_CONTENT),
            markdown: Some(md_search_course_content),
            handler: handlers::h_search_course_content,
        },
    ]
}
