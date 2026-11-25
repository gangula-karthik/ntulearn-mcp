use crate::handlers;
use crate::mcp::Tool;
use crate::mcp::ToolAnnotations;
use crate::registry::cli_spec::{a, b, e, i, s, CmdSpec};
use crate::registry::tool;
use crate::registry::ToolDef;
use crate::render;
use serde_json::json;

// ---------------------------------------------------------------------
// MCP schemas (verbatim from schemas.rs)
// ---------------------------------------------------------------------

fn mcp_get_announcements() -> Tool {
    Tool::new(
            r####"ntulearn_get_announcements"####.to_string(),
            r####"Get announcements across your enrolled courses, newest first. Fans out across every available course by default; pass course_ids=['_123_1'] to scope. Use since (ISO-8601) to filter to recent ones (e.g. 'this week'). Each item includes its courseId so cross-course views stay attributable. Paginate with limit/offset. Example: ntulearn_get_announcements(course_ids=['_2706844_1'], since='2026-09-01T00:00:00Z')"####.to_string(),
            json!({"type":"object","properties":{"course_ids":{"type":"array","description":"Optional list of course IDs to scope to. Omit to fan out across all available enrolled courses.","items":{"type":"string","pattern":"^[A-Za-z0-9_\\-:]+$"},"maxItems":200},"since":{"type":"string","description":"Optional ISO-8601 cutoff (e.g. '2026-05-09T00:00:00Z'). Only announcements with `created` on/after this time are returned. Filtered client-side after fetch."},"limit":{"type":"integer","description":"Max items to return per call (1–200, default 50). Use small values to keep results within the LLM's context.","minimum":1,"maximum":200,"default":50},"offset":{"type":"integer","description":"Number of items to skip for pagination. Use the nextOffset value returned by a previous call to walk pages.","minimum":0,"default":0},"response_format":{"type":"string","description":"Return format: 'toon' (default; token-optimized, ~40% fewer tokens — best for agents), 'json' (structured) or 'markdown' (human-readable).","enum":["toon","json","markdown"],"default":"toon"}},"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Get announcements (cross-course)"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
}

fn mcp_list_messages() -> Tool {
    Tool::new(
            r####"ntulearn_list_messages"####.to_string(),
            r####"List a user's Blackboard messages (mailbox). Defaults to the inbox; folder='sent' for the outbox. Optionally filter to unread_only or a since (ISO-8601) cutoff. Paginate with limit/offset. Example: ntulearn_list_messages(folder='inbox', unread_only=true)"####.to_string(),
            json!({"type":"object","properties":{"folder":{"type":"string","description":"Mailbox folder to read. Blackboard exposes 'inbox' (received) and 'sent' (outbox).","enum":["inbox","sent"],"default":"inbox"},"unread_only":{"type":"boolean","description":"Return only unread messages when true.","default":false},"since":{"type":"string","description":"Optional ISO-8601 cutoff (e.g. '2026-05-09T00:00:00Z'). Only items created on/after this time are included."},"limit":{"type":"integer","description":"Max items to return per call (1–200, default 50). Use small values to keep results within the LLM's context.","minimum":1,"maximum":200,"default":50},"offset":{"type":"integer","description":"Number of items to skip for pagination. Use the nextOffset value returned by a previous call to walk pages.","minimum":0,"default":0},"response_format":{"type":"string","description":"Return format: 'toon' (default; token-optimized, ~40% fewer tokens — best for agents), 'json' (structured) or 'markdown' (human-readable).","enum":["toon","json","markdown"],"default":"toon"}},"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"List user messages"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
}

fn mcp_read_message() -> Tool {
    Tool::new(
            r####"ntulearn_read_message"####.to_string(),
            r####"Read a single Blackboard message by ID: subject, full body, recipient list. Get the ID from ntulearn_list_messages. Example: ntulearn_read_message(message_id='_12345_1')"####.to_string(),
            json!({"type":"object","properties":{"message_id":{"type":"string","description":"Blackboard message ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"response_format":{"type":"string","description":"Return format: 'toon' (default; token-optimized, ~40% fewer tokens — best for agents), 'json' (structured) or 'markdown' (human-readable).","enum":["toon","json","markdown"],"default":"toon"}},"required":["message_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Read a user message"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
}

// ---------------------------------------------------------------------
// Markdown renderers (verbatim render_for_tool arms)
// ---------------------------------------------------------------------

fn md_get_announcements(payload: &serde_json::Value) -> String {
    let items = payload
        .get("announcements")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    render::md_announcements(&items, payload)
}

fn md_list_messages(payload: &serde_json::Value) -> String {
    render::md_messages(payload)
}

fn md_read_message(payload: &serde_json::Value) -> String {
    render::md_message(payload)
}

// ---------------------------------------------------------------------
// CLI command specs (verbatim from cli.rs COMMANDS; consts so the
// arg arrays are compile-time promoted to 'static)
// ---------------------------------------------------------------------

const CMD_GET_ANNOUNCEMENTS: CmdSpec = CmdSpec {
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
};

const CMD_LIST_MESSAGES: CmdSpec = CmdSpec {
    short: "list_messages",
    alias: "messages",
    summary: "course messages (inbox/sent)",
    args: &[
        e(
            "folder",
            &["inbox", "sent"],
            "mailbox folder (default inbox)",
        ),
        b("unread_only", "only unread"),
        s("since", false, "ISO-8601 cutoff"),
        i("limit", "max rows (default 50)"),
        i("offset", "pagination offset"),
    ],
    list_key: "messages",
    cols: &["sent", "courseId", "subject", "from", "unread"],
};

const CMD_READ_MESSAGE: CmdSpec = CmdSpec {
    short: "read_message",
    alias: "message",
    summary: "read one course message",
    args: &[s("message_id", true, "message id from `messages`")],
    list_key: "",
    cols: &[],
};

pub fn tools() -> Vec<ToolDef> {
    vec![
        tool! {
            short: "get_announcements",
            mcp: mcp_get_announcements(),
            cli: Some(CMD_GET_ANNOUNCEMENTS),
            markdown: Some(md_get_announcements),
            handler: handlers::h_get_announcements,
        },
        tool! {
            short: "list_messages",
            mcp: mcp_list_messages(),
            cli: Some(CMD_LIST_MESSAGES),
            markdown: Some(md_list_messages),
            handler: handlers::h_list_messages,
        },
        tool! {
            short: "read_message",
            mcp: mcp_read_message(),
            cli: Some(CMD_READ_MESSAGE),
            markdown: Some(md_read_message),
            handler: handlers::h_read_message,
        },
    ]
}
