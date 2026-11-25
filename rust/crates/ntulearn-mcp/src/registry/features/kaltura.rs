use crate::handlers;
use crate::mcp::Tool;
use crate::mcp::ToolAnnotations;
use crate::registry::cli_spec::{e, s, CmdSpec};
use crate::registry::tool;
use crate::registry::ToolDef;
use crate::render;
use serde_json::json;

// ---------------------------------------------------------------------
// MCP schemas (verbatim from schemas.rs)
// ---------------------------------------------------------------------

fn mcp_kaltura_download() -> Tool {
    Tool::new(
            r####"ntulearn_kaltura_download"####.to_string(),
            r####"Download the MP4 of ONE Kaltura video via Kaltura's open CDN. quality='smallest' (default) picks the smallest flavor with a real resolution (height > 0), 'best' the largest non-original flavor, 'original' the source upload (often 1-2 GB — use only when you need full resolution). Returns the chosen flavor (id, resolution, size), bytes written and the local path. The file lands in {download dir}/kaltura/{course_id}/. Requires a specific content_id from ntulearn_kaltura_list. Example: ntulearn_kaltura_download(course_id='_2707070_1', content_id='_5976141_1', quality='smallest')"####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"content_id":{"type":"string","description":"Content item ID of the Kaltura video page (e.g. _67890_1). Find it with ntulearn_kaltura_list.","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"quality":{"type":"string","description":"Which flavor to download: 'smallest' (fewest bytes at a real resolution), 'best' (largest non-original) or 'original' (source upload, often 1-2 GB).","enum":["smallest","best","original"],"default":"smallest"},"response_format":{"type":"string","description":"Return format: 'toon' (default; token-optimized, ~40% fewer tokens — best for agents), 'json' (structured) or 'markdown' (human-readable).","enum":["toon","json","markdown"],"default":"toon"}},"required":["course_id","content_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Kaltura video download"####.to_string()),
            read_only_hint: Some(false),
            destructive_hint: Some(false),
            idempotent_hint: Some(false),
            open_world_hint: Some(true),
        })
}

fn mcp_kaltura_list() -> Tool {
    Tool::new(
            r####"ntulearn_kaltura_list"####.to_string(),
            r####"List the Kaltura-embedded lecture videos of a course: for every video found anywhere in the content tree (folders are walked), its content_id, title, Kaltura entry_id and placement_id. Pure REST discovery — no video player or LTI launch is involved, and non-Kaltura LTI tools are skipped, not errors. Use this first to find the content_id for ntulearn_kaltura_transcripts or ntulearn_kaltura_download. Example: ntulearn_kaltura_list(course_id='_2707070_1')"####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"content_id":{"type":"string","description":"Content item ID of one specific page to inspect (e.g. _67890_1). Omit to walk the entire course content tree.","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"response_format":{"type":"string","description":"Return format: 'toon' (default; token-optimized, ~40% fewer tokens — best for agents), 'json' (structured) or 'markdown' (human-readable).","enum":["toon","json","markdown"],"default":"toon"}},"required":["course_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Kaltura video listing"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
}

fn mcp_kaltura_transcripts() -> Tool {
    Tool::new(
            r####"ntulearn_kaltura_transcripts"####.to_string(),
            r####"Download the caption/transcript tracks of Kaltura lecture videos as .vtt and .txt files. Omit content_id to fetch captions for EVERY video in the course (one 0.3s pause between videos). Files are written to {save_dir}/kaltura/{course_id}/{video title}.vtt and .txt (save_dir defaults to the server's download dir); format='vtt' keeps timestamps, 'txt' strips them, 'both' (default) writes both. A video with no captions becomes a per-video note in the result, never a whole-call error. Find content ids with ntulearn_kaltura_list. Example: ntulearn_kaltura_transcripts(course_id='_2707070_1', content_id='_5976141_1')"####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"content_id":{"type":"string","description":"Content item ID of one specific page to inspect (e.g. _67890_1). Omit to walk the entire course content tree.","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"save_dir":{"type":"string","description":"Directory to write transcript files under (created if missing; accepts ~ paths). Files land in kaltura/{course_id}/ inside it. Defaults to the server's download dir (NTULEARN_DOWNLOAD_DIR).","minLength":1,"maxLength":1024},"format":{"type":"string","description":"Which files to write: 'vtt' (with timestamps), 'txt' (timestamps stripped) or 'both'.","enum":["vtt","txt","both"],"default":"both"},"response_format":{"type":"string","description":"Return format: 'toon' (default; token-optimized, ~40% fewer tokens — best for agents), 'json' (structured) or 'markdown' (human-readable).","enum":["toon","json","markdown"],"default":"toon"}},"required":["course_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Kaltura transcripts"####.to_string()),
            read_only_hint: Some(false),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
}

// ---------------------------------------------------------------------
// Markdown renderers (verbatim render_for_tool arms)
// ---------------------------------------------------------------------

fn md_kaltura_download(payload: &serde_json::Value) -> String {
    render::md_kaltura_download(payload)
}

fn md_kaltura_list(payload: &serde_json::Value) -> String {
    render::md_kaltura_list(payload)
}

fn md_kaltura_transcripts(payload: &serde_json::Value) -> String {
    render::md_kaltura_transcripts(payload)
}

// ---------------------------------------------------------------------
// CLI command specs (verbatim from cli.rs COMMANDS; consts so the
// arg arrays are compile-time promoted to 'static)
// ---------------------------------------------------------------------

const CMD_KALTURA_DOWNLOAD: CmdSpec = CmdSpec {
    short: "kaltura_download",
    alias: "kaltura-download",
    summary: "download one Kaltura video's mp4 from the open CDN",
    args: &[
        s("course_id", true, "course id"),
        s("content_id", true, "kaltura content item id"),
        e(
            "quality",
            &["smallest", "best", "original"],
            "flavor to pick (default smallest)",
        ),
    ],
    list_key: "",
    cols: &[],
};

const CMD_KALTURA_LIST: CmdSpec = CmdSpec {
    short: "kaltura_list",
    alias: "kaltura-list",
    summary: "Kaltura-embedded videos in a course (entry ids for transcripts/downloads)",
    args: &[
        s("course_id", true, "course id"),
        s(
            "content_id",
            false,
            "one content item only (omit = walk whole course)",
        ),
    ],
    list_key: "videos",
    cols: &["contentId", "title", "entryId", "placementId"],
};

const CMD_KALTURA_TRANSCRIPTS: CmdSpec = CmdSpec {
    short: "kaltura_transcripts",
    alias: "kaltura-transcripts",
    summary: "download caption tracks as .vtt/.txt (whole course or one video)",
    args: &[
        s("course_id", true, "course id"),
        s(
            "content_id",
            false,
            "kaltura content item id (omit = every video)",
        ),
        s(
            "save_dir",
            false,
            "output dir (default NTULEARN_DOWNLOAD_DIR)",
        ),
        e(
            "format",
            &["vtt", "txt", "both"],
            "files to write (default both)",
        ),
    ],
    list_key: "videos",
    cols: &["title", "entryId", "words", "segments"],
};

pub fn tools() -> Vec<ToolDef> {
    vec![
        tool! {
            short: "kaltura_download",
            mcp: mcp_kaltura_download(),
            cli: Some(CMD_KALTURA_DOWNLOAD),
            markdown: Some(md_kaltura_download),
            handler: handlers::h_kaltura_download,
        },
        tool! {
            short: "kaltura_list",
            mcp: mcp_kaltura_list(),
            cli: Some(CMD_KALTURA_LIST),
            markdown: Some(md_kaltura_list),
            handler: handlers::h_kaltura_list,
        },
        tool! {
            short: "kaltura_transcripts",
            mcp: mcp_kaltura_transcripts(),
            cli: Some(CMD_KALTURA_TRANSCRIPTS),
            markdown: Some(md_kaltura_transcripts),
            handler: handlers::h_kaltura_transcripts,
        },
    ]
}
