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

fn mcp_pcv_autofill() -> Tool {
    Tool::new(
            r####"ntulearn_pcv_autofill"####.to_string(),
            r####"Fill an ENTIRE Peerceptiv peer evaluation in one call with full-marks defaults: every teammate rated at the top level of every rubric criterion and every comment prompt answered with a positive, varied comment (prompt-aware: strengths / nothing-to-improve / justification wording). Overwrites existing ratings and comments. dry_run=true (default) previews every before→after change without sending anything; dry_run=false submits and returns evalComplete + completionWarning per teammate. Get content_id from ntulearn_pcv_status. Example: ntulearn_pcv_autofill(course_id='_2707079_1', content_id='_6241854_1', dry_run=true)"####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":r"^[A-Za-z0-9_\-:]+$"},"content_id":{"type":"string","description":"Content item ID of the Peerceptiv activity (e.g. _67890_1). Find it with ntulearn_pcv_status.","minLength":1,"maxLength":200,"pattern":r"^[A-Za-z0-9_\-:]+$"},"dry_run":{"type":"boolean","description":"Preview every before→after change WITHOUT sending (default true). Set false to rate everyone at full marks and submit the comments.","default":true},"response_format":{"type":"string","description":"Return format: 'toon' (default; token-optimized, ~40% fewer tokens — best for agents), 'json' (structured) or 'markdown' (human-readable).","enum":["toon","json","markdown"],"default":"toon"}},"required":["course_id","content_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Peerceptiv autofill evaluation"####.to_string()),
            read_only_hint: Some(false),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
}

fn mcp_pcv_comment() -> Tool {
    Tool::new(
            r####"ntulearn_pcv_comment"####.to_string(),
            r####"Write (upsert) your written comment for ONE teammate in a Peerceptiv peer evaluation; submitting again replaces your previous text for that prompt (creates the peer evaluation first if none exists yet — shown in dry-run too). Writes the first comment prompt, or every comment prompt when the rubric has several. dry_run=true (default) resolves the teammate and shows the text WITHOUT sending it; dry_run=false submits. Saved comments alone never complete an evaluation: real submissions return evalComplete and a completionWarning while any criterion is still unrated — rate with ntulearn_pcv_rate. Check which teammates still need work with ntulearn_pcv_status first. Example: ntulearn_pcv_comment(course_id='_2707079_1', content_id='_6241854_1', teammate='Isabelle', text='Isabelle consistently delivered her parts early and coordinated our meetings well.', dry_run=true)"####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"content_id":{"type":"string","description":"Content item ID of the Peerceptiv activity (e.g. _67890_1). Find it with ntulearn_pcv_status.","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"teammate":{"type":"string","description":"Teammate to comment on — case-insensitive substring of their name (e.g. 'Isabelle' matches 'GAN YU XUAN ISABELLE').","minLength":1,"maxLength":200},"text":{"type":"string","description":"The written comment text for the teammate (plain text; replaces any previous comment you wrote for this prompt).","minLength":1,"maxLength":8000},"dry_run":{"type":"boolean","description":"Resolve the teammate and show the text WITHOUT sending it (default true). Set false to actually submit.","default":true},"response_format":{"type":"string","description":"Return format: 'toon' (default; token-optimized, ~40% fewer tokens — best for agents), 'json' (structured) or 'markdown' (human-readable).","enum":["toon","json","markdown"],"default":"toon"}},"required":["course_id","content_id","teammate","text"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Peerceptiv comment on teammate"####.to_string()),
            read_only_hint: Some(false),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
}

fn mcp_pcv_evals() -> Tool {
    Tool::new(
            r####"ntulearn_pcv_evals"####.to_string(),
            r####"Read your Peerceptiv peer-evaluation reviews for one activity. direction='given': the rubric ratings you gave each teammate (criterion → score 1-10 + resolved level description like 'Always'; unrated criteria show a null score) plus your written comments. direction='received': what teammates gave you — empty with a 'not yet released' note until results are published, which is normal, not an error. Get content_id from ntulearn_pcv_status. Example: ntulearn_pcv_evals(course_id='_2707079_1', content_id='_6241854_1', direction='given')"####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"content_id":{"type":"string","description":"Content item ID of the Peerceptiv activity (e.g. _67890_1). Find it with ntulearn_pcv_status.","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"direction":{"type":"string","description":"'given' = ratings you gave teammates; 'received' = ratings teammates gave you (empty until released).","enum":["given","received"],"default":"given"},"response_format":{"type":"string","description":"Return format: 'toon' (default; token-optimized, ~40% fewer tokens — best for agents), 'json' (structured) or 'markdown' (human-readable).","enum":["toon","json","markdown"],"default":"toon"}},"required":["course_id","content_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Peerceptiv reviews (given/received)"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
}

fn mcp_pcv_rate() -> Tool {
    Tool::new(
            r####"ntulearn_pcv_rate"####.to_string(),
            r####"Set ONE rubric rating for ONE teammate in a Peerceptiv peer evaluation (upsert: patches the existing score for that teammate+criterion pair; creates the peer evaluation first if none exists yet — shown in dry-run too). A saved rating alone never completes an evaluation: real submissions (dry_run=false) return evalComplete and a completionWarning while any comment prompt is unanswered or criterion unrated — finish comments with ntulearn_pcv_comment. teammate and criterion match case-insensitively by substring; level matches a substring of the level description (e.g. 'Always') or a plain score number 1-10. dry_run=true (default) resolves everything and shows the before→after change WITHOUT sending it. See current values first with ntulearn_pcv_evals direction='given'. Example: ntulearn_pcv_rate(course_id='_2707079_1', content_id='_6241854_1', teammate='Isabelle', criterion='responsibilities', level='Always', dry_run=false)"####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"content_id":{"type":"string","description":"Content item ID of the Peerceptiv activity (e.g. _67890_1). Find it with ntulearn_pcv_status.","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"teammate":{"type":"string","description":"Teammate to rate — case-insensitive substring of their name (e.g. 'Isabelle' matches 'GAN YU XUAN ISABELLE').","minLength":1,"maxLength":200},"criterion":{"type":"string","description":"Rubric criterion — case-insensitive substring of its name (e.g. 'responsibilities').","minLength":1,"maxLength":200},"level":{"type":"string","description":"Rating level — case-insensitive substring of the level description (e.g. 'Always') or a plain score number 1-10 as a string (e.g. '9').","minLength":1,"maxLength":200},"dry_run":{"type":"boolean","description":"Resolve teammate/criterion/level and show the change WITHOUT sending it (default true). Set false to actually submit.","default":true},"response_format":{"type":"string","description":"Return format: 'toon' (default; token-optimized, ~40% fewer tokens — best for agents), 'json' (structured) or 'markdown' (human-readable).","enum":["toon","json","markdown"],"default":"toon"}},"required":["course_id","content_id","teammate","criterion","level"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Peerceptiv rate teammate"####.to_string()),
            read_only_hint: Some(false),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
}

fn mcp_pcv_status() -> Tool {
    Tool::new(
            r####"ntulearn_pcv_status"####.to_string(),
            r####"Peerceptiv peer-evaluation status for a course: assignment name, peer-evaluation deadline, how many of your required evaluations are complete, your group name, and each teammate with done/not-done. done means every rating criterion scored AND every comment prompt answered — Peerceptiv does NOT count a ratings-only evaluation as complete; incomplete teammates carry a missing object listing the unrated criteria and unanswered comment prompts. Omit content_id to auto-discover Peerceptiv items in the course content tree (LTI placements are probed; non-Peerceptiv tools are skipped, not errors). Use this first when checking peer-evaluation deadlines or which teammates you still owe. Example: ntulearn_pcv_status(course_id='_2707079_1')"####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"content_id":{"type":"string","description":"Content item ID of one specific Peerceptiv activity (e.g. _67890_1). Omit to auto-discover Peerceptiv items in the course content tree.","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"response_format":{"type":"string","description":"Return format: 'toon' (default; token-optimized, ~40% fewer tokens — best for agents), 'json' (structured) or 'markdown' (human-readable).","enum":["toon","json","markdown"],"default":"toon"}},"required":["course_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Peerceptiv activity status"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
}

// ---------------------------------------------------------------------
// Markdown renderers (verbatim render_for_tool arms)
// ---------------------------------------------------------------------

fn md_pcv_comment(payload: &serde_json::Value) -> String {
    render::md_pcv_comment(payload)
}

fn md_pcv_evals(payload: &serde_json::Value) -> String {
    render::md_pcv_evals(payload)
}

fn md_pcv_rate(payload: &serde_json::Value) -> String {
    render::md_pcv_rate(payload)
}

fn md_pcv_status(payload: &serde_json::Value) -> String {
    render::md_pcv_status(payload)
}

// ---------------------------------------------------------------------
// CLI command specs (verbatim from cli.rs COMMANDS; consts so the
// arg arrays are compile-time promoted to 'static)
// ---------------------------------------------------------------------

const CMD_PCV_AUTOFILL: CmdSpec = CmdSpec {
    short: "pcv_autofill",
    alias: "pcv-auto",
    summary: "fill whole peer eval: full marks + good comments (dry-run default)",
    args: &[
        s("course_id", true, "course id"),
        s("content_id", true, "pcv content item id"),
        b("dry_run", "preview only (default true)"),
    ],
    list_key: "results",
    cols: &["teammate", "ratingsSet", "commentsSet", "evalComplete"],
};

const CMD_PCV_COMMENT: CmdSpec = CmdSpec {
    short: "pcv_comment",
    alias: "pcv-comment",
    summary: "write the comment prompt(s) for a teammate",
    args: &[
        s("course_id", true, "course id"),
        s("content_id", true, "pcv content item id"),
        s("teammate", true, "substring of teammate name"),
        s("text", true, "comment text"),
        b("dry_run", "resolve + show only (default true)"),
    ],
    list_key: "",
    cols: &[],
};

const CMD_PCV_EVALS: CmdSpec = CmdSpec {
    short: "pcv_evals",
    alias: "pcv-evals",
    summary: "Peerceptiv peer-evaluation reviews given/received",
    args: &[
        s("course_id", true, "course id"),
        s("content_id", true, "pcv content item id"),
        e(
            "direction",
            &["given", "received"],
            "given = you rated teammates; received = ratings for you",
        ),
    ],
    list_key: "reviews",
    cols: &["teammate", "criterion", "score", "level"],
};

const CMD_PCV_RATE: CmdSpec = CmdSpec {
    short: "pcv_rate",
    alias: "pcv-rate",
    summary: "set one rubric rating for a teammate (dry-run by default)",
    args: &[
        s("course_id", true, "course id"),
        s("content_id", true, "pcv content item id"),
        s("teammate", true, "substring of teammate name"),
        s("criterion", true, "substring of criterion name"),
        s("level", true, "level description substring or score 1-10"),
        b("dry_run", "resolve + show only (default true)"),
    ],
    list_key: "",
    cols: &[],
};

const CMD_PCV_STATUS: CmdSpec = CmdSpec {
    short: "pcv_status",
    alias: "pcv",
    summary: "Peerceptiv peer-evaluation status (deadlines, progress, teammates)",
    args: &[
        s("course_id", true, "course id"),
        s(
            "content_id",
            false,
            "pcv content item id (omit = auto-discover)",
        ),
    ],
    list_key: "activities",
    cols: &["contentId", "title", "group"],
};

pub fn tools() -> Vec<ToolDef> {
    vec![
        tool! {
            short: "pcv_autofill",
            mcp: mcp_pcv_autofill(),
            cli: Some(CMD_PCV_AUTOFILL),
            markdown: None,
            handler: handlers::h_pcv_autofill,
        },
        tool! {
            short: "pcv_comment",
            mcp: mcp_pcv_comment(),
            cli: Some(CMD_PCV_COMMENT),
            markdown: Some(md_pcv_comment),
            handler: handlers::h_pcv_comment,
        },
        tool! {
            short: "pcv_evals",
            mcp: mcp_pcv_evals(),
            cli: Some(CMD_PCV_EVALS),
            markdown: Some(md_pcv_evals),
            handler: handlers::h_pcv_evals,
        },
        tool! {
            short: "pcv_rate",
            mcp: mcp_pcv_rate(),
            cli: Some(CMD_PCV_RATE),
            markdown: Some(md_pcv_rate),
            handler: handlers::h_pcv_rate,
        },
        tool! {
            short: "pcv_status",
            mcp: mcp_pcv_status(),
            cli: Some(CMD_PCV_STATUS),
            markdown: Some(md_pcv_status),
            handler: handlers::h_pcv_status,
        },
    ]
}
