use crate::handlers;
use crate::mcp::Tool;
use crate::mcp::ToolAnnotations;
use crate::registry::cli_spec::{i, s, CmdSpec};
use crate::registry::tool;
use crate::registry::ToolDef;
use crate::render;
use serde_json::json;

// ---------------------------------------------------------------------
// MCP schemas (verbatim from schemas.rs)
// ---------------------------------------------------------------------

fn mcp_get_group_members() -> Tool {
    Tool::new(
            r####"ntulearn_get_group_members"####.to_string(),
            r####"List the members of a specific course group (e.g. your tutorial group's roster). Group IDs come from ntulearn_list_course_groups. Example: ntulearn_get_group_members(course_id='_2706844_1', group_id='_12345_1')"####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"group_id":{"type":"string","description":"Group ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"response_format":{"type":"string","description":"Return format: 'toon' (default; token-optimized, ~40% fewer tokens — best for agents), 'json' (structured) or 'markdown' (human-readable).","enum":["toon","json","markdown"],"default":"toon"}},"required":["course_id","group_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Get course group members"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
}

fn mcp_list_course_groups() -> Tool {
    Tool::new(
            r####"ntulearn_list_course_groups"####.to_string(),
            r####"List the groups defined in a course (e.g. tutorial/lab groups). Paginate with limit/offset. Example: ntulearn_list_course_groups(course_id='_2706844_1')"####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"limit":{"type":"integer","description":"Max items to return per call (1–200, default 50). Use small values to keep results within the LLM's context.","minimum":1,"maximum":200,"default":50},"offset":{"type":"integer","description":"Number of items to skip for pagination. Use the nextOffset value returned by a previous call to walk pages.","minimum":0,"default":0},"response_format":{"type":"string","description":"Return format: 'toon' (default; token-optimized, ~40% fewer tokens — best for agents), 'json' (structured) or 'markdown' (human-readable).","enum":["toon","json","markdown"],"default":"toon"}},"required":["course_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"List course groups"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
}

fn mcp_list_course_users() -> Tool {
    Tool::new(
            r####"ntulearn_list_course_users"####.to_string(),
            r####"List the users enrolled in a course (instructors, teaching staff, students as the API exposes them). Useful for rosters and 'who teaches this'. Paginate with limit/offset. Example: ntulearn_list_course_users(course_id='_2706844_1')"####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"limit":{"type":"integer","description":"Max items to return per call (1–200, default 50). Use small values to keep results within the LLM's context.","minimum":1,"maximum":200,"default":50},"offset":{"type":"integer","description":"Number of items to skip for pagination. Use the nextOffset value returned by a previous call to walk pages.","minimum":0,"default":0},"response_format":{"type":"string","description":"Return format: 'toon' (default; token-optimized, ~40% fewer tokens — best for agents), 'json' (structured) or 'markdown' (human-readable).","enum":["toon","json","markdown"],"default":"toon"}},"required":["course_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"List course users"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
}

// ---------------------------------------------------------------------
// Markdown renderers (verbatim render_for_tool arms)
// ---------------------------------------------------------------------

fn md_get_group_members(payload: &serde_json::Value) -> String {
    render::md_group_members(payload)
}

fn md_list_course_groups(payload: &serde_json::Value) -> String {
    render::md_course_groups(payload)
}

fn md_list_course_users(payload: &serde_json::Value) -> String {
    render::md_course_users(payload)
}

// ---------------------------------------------------------------------
// CLI command specs (verbatim from cli.rs COMMANDS; consts so the
// arg arrays are compile-time promoted to 'static)
// ---------------------------------------------------------------------

const CMD_GET_GROUP_MEMBERS: CmdSpec = CmdSpec {
    short: "get_group_members",
    alias: "members",
    summary: "members of one group",
    args: &[
        s("course_id", true, "course id"),
        s("group_id", true, "group id"),
    ],
    list_key: "members",
    cols: &["name", "userId"],
};

const CMD_LIST_COURSE_GROUPS: CmdSpec = CmdSpec {
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
};

const CMD_LIST_COURSE_USERS: CmdSpec = CmdSpec {
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
};

pub fn tools() -> Vec<ToolDef> {
    vec![
        tool! {
            short: "get_group_members",
            mcp: mcp_get_group_members(),
            cli: Some(CMD_GET_GROUP_MEMBERS),
            markdown: Some(md_get_group_members),
            handler: handlers::h_get_group_members,
        },
        tool! {
            short: "list_course_groups",
            mcp: mcp_list_course_groups(),
            cli: Some(CMD_LIST_COURSE_GROUPS),
            markdown: Some(md_list_course_groups),
            handler: handlers::h_list_course_groups,
        },
        tool! {
            short: "list_course_users",
            mcp: mcp_list_course_users(),
            cli: Some(CMD_LIST_COURSE_USERS),
            markdown: Some(md_list_course_users),
            handler: handlers::h_list_course_users,
        },
    ]
}
