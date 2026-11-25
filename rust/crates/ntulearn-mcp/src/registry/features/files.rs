use crate::handlers;
use crate::mcp::Tool;
use crate::mcp::ToolAnnotations;
use crate::registry::cli_spec::{b, e, i, s, CmdSpec};
use crate::registry::tool;
use crate::registry::ToolDef;
use crate::render;
use serde_json::json;

// ---------------------------------------------------------------------
// MCP schemas (verbatim from schemas.rs)
// ---------------------------------------------------------------------

fn mcp_download_course() -> Tool {
    Tool::new(
            r####"ntulearn_download_course"####.to_string(),
            r####"Recursively download every file in a course's content tree to local disk, organised into course/subject folders. Skips already-downloaded files by default; skip_existing=false to force. Concurrency bounded by parallel. WRITE tool — saves files on your machine. For a single item's files, use ntulearn_download_file. Example: ntulearn_download_course(course_id='_2706844_1', include_extensions='pdf,pptx')"####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"destination_dir":{"type":"string","description":"Target root directory (absolute or ~-prefixed). Defaults to ~/Downloads/NTU/<course>.","minLength":1,"maxLength":1024},"max_depth":{"type":"integer","description":"Maximum recursion depth (capped at 10).","default":5,"minimum":1,"maximum":10},"include_extensions":{"type":"string","description":"Optional comma-separated file extensions to download, e.g. 'pdf,ppt,pptx,docx'. Omit to download all."},"skip_existing":{"type":"boolean","description":"Skip files already present at the destination.","default":true},"parallel":{"type":"integer","description":"Max concurrent downloads (1-16).","default":4,"minimum":1,"maximum":16},"response_format":{"type":"string","description":"Return format: 'toon' (default; token-optimized, ~40% fewer tokens — best for agents), 'json' (structured) or 'markdown' (human-readable).","enum":["toon","json","markdown"],"default":"toon"}},"required":["course_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Download an entire course to disk"####.to_string()),
            read_only_hint: Some(false),
            destructive_hint: Some(false),
            idempotent_hint: Some(false),
            open_world_hint: Some(true),
        })
}

fn mcp_download_file() -> Tool {
    Tool::new(
            r####"ntulearn_download_file"####.to_string(),
            r####"Download every file attached to a Blackboard content item to your local disk. Handles resource/x-bb-file (attachment API) and resource/x-bb-document (HTML body with bbcswebdav links). Pass destination_dir to target a folder (e.g. destination_dir='~/NTU/y3s1/sc2002/week 8/'). Returns saved files with local paths and sizes. To inspect content INLINE instead of saving, prefer ntulearn_read_file_content. Example: ntulearn_download_file(course_id='_2706844_1', content_id='_6099516_1')"####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"content_id":{"type":"string","description":"Content item ID (e.g. _67890_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"destination_dir":{"type":"string","description":"Optional target directory. Accepts absolute paths and `~`-prefixed paths (e.g. '~/NTU/y3s1/sc2002/week 8/'). Created if missing. Defaults to NTULEARN_DOWNLOAD_DIR env var, or ./downloads/ if unset.","minLength":1,"maxLength":1024},"response_format":{"type":"string","description":"Return format: 'toon' (default; token-optimized, ~40% fewer tokens — best for agents), 'json' (structured) or 'markdown' (human-readable).","enum":["toon","json","markdown"],"default":"toon"}},"required":["course_id","content_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Download files to local disk"####.to_string()),
            read_only_hint: Some(false),
            destructive_hint: Some(false),
            idempotent_hint: Some(false),
            open_world_hint: Some(true),
        })
}

fn mcp_read_file_content() -> Tool {
    Tool::new(
            r####"ntulearn_read_file_content"####.to_string(),
            r####"Read the content of files attached to a Blackboard content item, returned inline (no local-filesystem hop). Use this to answer questions about lecture material; ntulearn_download_file is only for actually wanting the bytes on disk. PDFs default to text mode (pypdf, cheap, best for written content); mode='vision' for diagram/equation/screenshot-heavy pages, pages='1-3' to bound payload size (~3K vision tokens/page). Microsoft Office (.docx, .pptx incl. speaker notes, .xlsx all sheets) and text-like files (txt, md, csv, json, xml, html tag-stripped, code) are supported. Other binaries (images, video, audio, archives, legacy .doc/.ppt/.xls) are listed under `skipped` — fall back to ntulearn_download_file. Per-file cap 25 MB, batch cap 40 MB, vision cap 50 pages. Example: ntulearn_read_file_content(course_id='_2706844_1', content_id='_6099516_1', mode='text')"####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"content_id":{"type":"string","description":"Content item ID (e.g. _67890_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"mode":{"type":"string","description":"PDF handling. 'text' (default) extracts text via pypdf — cheap, fits MCP's payload budget. 'vision' additionally renders each page as an image with PyMuPDF (~3K vision tokens per page); use for diagram/equation/handwritten content, ideally with a narrow `pages` range to stay under the 1 MB cap. Ignored for non-PDF files.","enum":["text","vision","auto"],"default":"text"},"pages":{"type":"string","description":"Optional page range for PDFs (1-indexed, inclusive). Examples: '1-10', '3', '1,3,5', '1-5,8,10-12'. Omit to read all pages (vision mode capped at 50 rendered pages). Especially useful with mode='vision' to keep the response under MCP's 1 MB cap."},"response_format":{"type":"string","description":"Return format: 'toon' (default; token-optimized, ~40% fewer tokens — best for agents), 'json' (structured) or 'markdown' (human-readable).","enum":["toon","json","markdown"],"default":"toon"}},"required":["course_id","content_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Read file content inline"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
}

// ---------------------------------------------------------------------
// Markdown renderers (verbatim render_for_tool arms)
// ---------------------------------------------------------------------

fn md_download_course(payload: &serde_json::Value) -> String {
    render::md_download_course(payload)
}

fn md_download_file(payload: &serde_json::Value) -> String {
    render::md_files(payload, "Files downloaded")
}

fn md_read_file_content(payload: &serde_json::Value) -> String {
    render::md_files(payload, "File contents")
}

// ---------------------------------------------------------------------
// CLI command specs (verbatim from cli.rs COMMANDS; consts so the
// arg arrays are compile-time promoted to 'static)
// ---------------------------------------------------------------------

const CMD_DOWNLOAD_COURSE: CmdSpec = CmdSpec {
    short: "download_course",
    alias: "dl-course",
    summary: "bulk-download a course's files",
    args: &[
        s("course_id", true, "course id"),
        s("destination_dir", false, "target dir"),
        i("max_depth", "recursion depth cap"),
        s("include_extensions", false, "comma list, e.g. pdf,pptx"),
        b("skip_existing", "default true"),
        i("parallel", "concurrent downloads (default 4)"),
    ],
    list_key: "files",
    cols: &["path", "bytes"],
};

const CMD_DOWNLOAD_FILE: CmdSpec = CmdSpec {
    short: "download_file",
    alias: "dl",
    summary: "download a content item's files to disk",
    args: &[
        s("course_id", true, "course id"),
        s("content_id", true, "content item id"),
        s(
            "destination_dir",
            false,
            "target dir (default NTULEARN_DOWNLOAD_DIR or ./downloads)",
        ),
    ],
    list_key: "files",
    cols: &["path", "bytes"],
};

const CMD_READ_FILE_CONTENT: CmdSpec = CmdSpec {
    short: "read_file_content",
    alias: "read",
    summary: "read a content item's files inline (pdf/office/text)",
    args: &[
        s("course_id", true, "course id"),
        s("content_id", true, "content item id"),
        e(
            "mode",
            &["text", "vision", "auto"],
            "pdf mode: text (cheap) or vision (diagrams)",
        ),
        s("pages", false, "pdf page range, e.g. 1-10"),
    ],
    list_key: "files",
    cols: &["name", "pages", "chars", "text"],
};

pub fn tools() -> Vec<ToolDef> {
    vec![
        tool! {
            short: "download_course",
            mcp: mcp_download_course(),
            cli: Some(CMD_DOWNLOAD_COURSE),
            markdown: Some(md_download_course),
            handler: handlers::h_download_course,
        },
        tool! {
            short: "download_file",
            mcp: mcp_download_file(),
            cli: Some(CMD_DOWNLOAD_FILE),
            markdown: Some(md_download_file),
            handler: handlers::h_download_file,
        },
        tool! {
            short: "read_file_content",
            mcp: mcp_read_file_content(),
            cli: Some(CMD_READ_FILE_CONTENT),
            markdown: Some(md_read_file_content),
            handler: handlers::h_read_file_content,
        },
    ]
}
