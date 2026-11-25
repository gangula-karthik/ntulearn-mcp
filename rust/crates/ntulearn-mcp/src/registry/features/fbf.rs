use crate::handlers;
use crate::mcp::Tool;
use crate::mcp::ToolAnnotations;
use crate::registry::cli_spec::{b, e, s, CmdSpec};
use crate::registry::tool;
use crate::registry::ToolDef;
use crate::render;
use serde_json::json;

// ---------------------------------------------------------------------
// MCP schemas (verbatim from schemas.rs)
// ---------------------------------------------------------------------

fn mcp_fbf_comment() -> Tool {
    Tool::new(
            r####"ntulearn_fbf_comment"####.to_string(),
            r####"Write (upsert) your 'Overall comments' written review for ONE teammate in a FeedbackFruits peer evaluation; submitting again replaces your previous text. dry_run=true (default) resolves the teammate and shows the text WITHOUT sending it; dry_run=false submits. Check which teammates still need comments with ntulearn_fbf_status first. Example: ntulearn_fbf_comment(course_id='_2706844_1', content_id='_6208502_1', teammate='Cheryl', text='Cheryl consistently delivered her parts early and mediated disagreements well.', dry_run=true)"####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"content_id":{"type":"string","description":"Content item ID of the FeedbackFruits activity (e.g. _67890_1). Find it with ntulearn_fbf_status.","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"teammate":{"type":"string","description":"Teammate to comment on — case-insensitive substring of their name (e.g. 'Cheryl' matches 'WONG YUET LENG, CHERYL').","minLength":1,"maxLength":200},"text":{"type":"string","description":"The written review text for the teammate (plain text; replaces any previous comment you wrote).","minLength":1,"maxLength":8000},"improve_text":{"type":"string","description":"Optional different text for improvement-style criteria (title contains 'improve'), so praise is not pasted into an 'areas to improve' box. Defaults to text.","maxLength":8000},"dry_run":{"type":"boolean","description":"Resolve the teammate and show the text WITHOUT sending it (default true). Set false to actually submit.","default":true},"response_format":{"type":"string","description":"Return format: 'toon' (default; token-optimized, ~40% fewer tokens — best for agents), 'json' (structured) or 'markdown' (human-readable).","enum":["toon","json","markdown"],"default":"toon"}},"required":["course_id","content_id","teammate","text"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"FeedbackFruits comment on teammate"####.to_string()),
            read_only_hint: Some(false),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
}

fn mcp_fbf_rate() -> Tool {
    Tool::new(
            r####"ntulearn_fbf_rate"####.to_string(),
            r####"Set ONE rubric rating for ONE teammate in a FeedbackFruits peer evaluation (upsert: patches the existing rating for that teammate+criterion pair, else creates it). teammate, criterion and option match case-insensitively by substring against names/titles; option also accepts the exact weight number (e.g. 4). dry_run=true (default) resolves everything and shows the before→after change WITHOUT sending it. See current values first with ntulearn_fbf_reviews direction='given'. Example: ntulearn_fbf_rate(course_id='_2706844_1', content_id='_6208502_1', teammate='Cheryl', criterion='interpersonal', option='Always', dry_run=false)"####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"content_id":{"type":"string","description":"Content item ID of the FeedbackFruits activity (e.g. _67890_1). Find it with ntulearn_fbf_status.","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"teammate":{"type":"string","description":"Teammate to rate — case-insensitive substring of their name (e.g. 'Cheryl' matches 'WONG YUET LENG, CHERYL').","minLength":1,"maxLength":200},"criterion":{"type":"string","description":"Rubric criterion — case-insensitive substring of its title (e.g. 'interpersonal').","minLength":1,"maxLength":200},"option":{"type":"string","description":"Rating option — case-insensitive substring of its title (e.g. 'Always') or the exact weight number as a string (e.g. '4').","minLength":1,"maxLength":200},"dry_run":{"type":"boolean","description":"Resolve teammate/criterion/option and show the change WITHOUT sending it (default true). Set false to actually submit.","default":true},"response_format":{"type":"string","description":"Return format: 'toon' (default; token-optimized, ~40% fewer tokens — best for agents), 'json' (structured) or 'markdown' (human-readable).","enum":["toon","json","markdown"],"default":"toon"}},"required":["course_id","content_id","teammate","criterion","option"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"FeedbackFruits rate teammate"####.to_string()),
            read_only_hint: Some(false),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
}

fn mcp_fbf_reviews() -> Tool {
    Tool::new(
            r####"ntulearn_fbf_reviews"####.to_string(),
            r####"Read your FeedbackFruits peer-evaluation reviews for one activity. direction='given': the rubric ratings you gave each teammate (criterion → option + weight; unrated criteria show a null option). direction='received': what teammates gave you — empty with an explicit 'not yet released' note until the teacher publishes ratings/grades, which is normal, not an error. Get content_id from ntulearn_fbf_status. Example: ntulearn_fbf_reviews(course_id='_2706844_1', content_id='_6208502_1', direction='given')"####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"content_id":{"type":"string","description":"Content item ID of the FeedbackFruits activity (e.g. _67890_1). Find it with ntulearn_fbf_status.","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"direction":{"type":"string","description":"'given' = ratings you gave teammates; 'received' = ratings teammates gave you (empty until released).","enum":["given","received"],"default":"given"},"response_format":{"type":"string","description":"Return format: 'toon' (default; token-optimized, ~40% fewer tokens — best for agents), 'json' (structured) or 'markdown' (human-readable).","enum":["toon","json","markdown"],"default":"toon"}},"required":["course_id","content_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"FeedbackFruits reviews (given/received)"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
}

fn mcp_fbf_status() -> Tool {
    Tool::new(
            r####"ntulearn_fbf_status"####.to_string(),
            r####"FeedbackFruits peer-evaluation status for a course: activity titles, per-task deadlines, your progress % and done state, overall completion, and whether grades have been released. Omit content_id to auto-discover FeedbackFruits items in the course content tree (every LTI link is probed; non-FeedbackFruits tools are skipped, not errors). Use this first when checking peer-evaluation deadlines or what you still owe. Example: ntulearn_fbf_status(course_id='_2706844_1')"####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"content_id":{"type":"string","description":"Content item ID of one specific FeedbackFruits activity (e.g. _67890_1). Omit to auto-discover FeedbackFruits items in the course content tree.","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"response_format":{"type":"string","description":"Return format: 'toon' (default; token-optimized, ~40% fewer tokens — best for agents), 'json' (structured) or 'markdown' (human-readable).","enum":["toon","json","markdown"],"default":"toon"}},"required":["course_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"FeedbackFruits activity status"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
}

// ---------------------------------------------------------------------
// Markdown renderers (verbatim render_for_tool arms)
// ---------------------------------------------------------------------

fn md_fbf_comment(payload: &serde_json::Value) -> String {
    render::md_fbf_comment(payload)
}

fn md_fbf_rate(payload: &serde_json::Value) -> String {
    render::md_fbf_rate(payload)
}

fn md_fbf_reviews(payload: &serde_json::Value) -> String {
    render::md_fbf_reviews(payload)
}

fn md_fbf_status(payload: &serde_json::Value) -> String {
    render::md_fbf_status(payload)
}

// ---------------------------------------------------------------------
// CLI command specs (verbatim from cli.rs COMMANDS; consts so the
// arg arrays are compile-time promoted to 'static)
// ---------------------------------------------------------------------

const CMD_FBF_COMMENT: CmdSpec = CmdSpec {
    short: "fbf_comment",
    alias: "fbf-comment",
    summary: "write the 'Overall comments' review for a teammate",
    args: &[
        s("course_id", true, "course id"),
        s("content_id", true, "fbf content item id"),
        s("teammate", true, "substring of teammate name"),
        s("text", true, "review text"),
        s(
            "improve_text",
            false,
            "text for 'improve' criteria (default: text)",
        ),
        b("dry_run", "resolve + show only (default true)"),
    ],
    list_key: "",
    cols: &[],
};

const CMD_FBF_RATE: CmdSpec = CmdSpec {
    short: "fbf_rate",
    alias: "fbf-rate",
    summary: "set one rubric rating for a teammate (dry-run by default)",
    args: &[
        s("course_id", true, "course id"),
        s("content_id", true, "fbf content item id"),
        s("teammate", true, "substring of teammate name"),
        s("criterion", true, "substring of criterion title"),
        s("option", true, "option title substring or weight number"),
        b("dry_run", "resolve + show only (default true)"),
    ],
    list_key: "",
    cols: &[],
};

const CMD_FBF_REVIEWS: CmdSpec = CmdSpec {
    short: "fbf_reviews",
    alias: "fbf-reviews",
    summary: "peer-evaluation reviews given/received",
    args: &[
        s("course_id", true, "course id"),
        s("content_id", true, "fbf content item id"),
        e(
            "direction",
            &["given", "received"],
            "given = you rated teammates; received = ratings for you",
        ),
    ],
    list_key: "reviews",
    cols: &["teammate", "criterion", "option", "weight"],
};

const CMD_FBF_STATUS: CmdSpec = CmdSpec {
    short: "fbf_status",
    alias: "fbf",
    summary: "FeedbackFruits peer-evaluation status (tasks, deadlines, progress)",
    args: &[
        s("course_id", true, "course id"),
        s(
            "content_id",
            false,
            "fbf content item id (omit = auto-discover)",
        ),
    ],
    list_key: "activities",
    cols: &["contentId", "title", "completion", "gradesPublishedAt"],
};

pub fn tools() -> Vec<ToolDef> {
    vec![
        tool! {
            short: "fbf_comment",
            mcp: mcp_fbf_comment(),
            cli: Some(CMD_FBF_COMMENT),
            markdown: Some(md_fbf_comment),
            handler: handlers::h_fbf_comment,
        },
        tool! {
            short: "fbf_rate",
            mcp: mcp_fbf_rate(),
            cli: Some(CMD_FBF_RATE),
            markdown: Some(md_fbf_rate),
            handler: handlers::h_fbf_rate,
        },
        tool! {
            short: "fbf_reviews",
            mcp: mcp_fbf_reviews(),
            cli: Some(CMD_FBF_REVIEWS),
            markdown: Some(md_fbf_reviews),
            handler: handlers::h_fbf_reviews,
        },
        tool! {
            short: "fbf_status",
            mcp: mcp_fbf_status(),
            cli: Some(CMD_FBF_STATUS),
            markdown: Some(md_fbf_status),
            handler: handlers::h_fbf_status,
        },
    ]
}
