// AUTO-GENERATED from the Python manifest (/tmp/tool_manifest.json). Do not edit by hand.
use serde_json::json;
use std::sync::OnceLock;
use ultrafast_mcp::{Tool, ToolAnnotations};

/// Static tool catalog, built lazily exactly once. Subsequent `tools/list`
/// calls clone from here — no schema reconstruction or re-parsing per request.
static TOOL_CATALOG: OnceLock<Vec<Tool>> = OnceLock::new();

/// All 22 NTULearn tool definitions with their exact input schemas.
///
/// Built **once** and cached in a `OnceLock`, so every `tools/list` (tool
/// discovery) is an O(1) cache hit instead of reconstructing all schemas and
/// annotations from scratch on each request. Discovery stays ultra fast even
/// with many concurrent connects/reloads.
pub fn all_tools() -> Vec<Tool> {
    TOOL_CATALOG.get_or_init(build_tool_catalog).clone()
}

fn build_tool_catalog() -> Vec<Tool> {

    vec![
        Tool::new(
            r####"ntulearn_list_courses"####.to_string(),
            r####"List courses you are enrolled in on NTULearn. Use this FIRST to obtain course IDs (_12345_1) needed by other tools. By default returns only active/available courses; set include_disabled=true to include unavailable ones. Returns id, title, availability. Paginate with limit/offset. To find ANY course (including ones you are not enrolled in), use ntulearn_search_all_courses instead. Example: ntulearn_list_courses(limit=25)"####.to_string(),
            json!({"type":"object","properties":{"include_disabled":{"type":"boolean","description":"Include courses where availability.available != 'Yes'","default":false},"limit":{"type":"integer","description":"Max items to return per call (1–200, default 50). Use small values to keep results within the LLM's context.","minimum":1,"maximum":200,"default":50},"offset":{"type":"integer","description":"Number of items to skip for pagination. Use the nextOffset value returned by a previous call to walk pages.","minimum":0,"default":0},"response_format":{"type":"string","description":"Return format: 'json' (default; structured), 'markdown' (human-readable) or 'toon' (token-optimized, ~40% fewer tokens than json — best for agents).","enum":["json","markdown","toon"],"default":"json"}},"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"List my NTULearn courses"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
        
        ,
        Tool::new(
            r####"ntulearn_get_course_contents"####.to_string(),
            r####"Walk a course's content tree one level at a time (shallow). Omit parent_id to list the top-level items (folders, documents, links, assignments); pass the parent_id of a folder where hasChildren=true to drill into its children. Returns items with id, title, type, hasChildren. For the ENTIRE tree in one call, use ntulearn_get_content_tree instead. Example: ntulearn_get_course_contents(course_id='_2706844_1')"####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"parent_id":{"type":"string","description":"Optional content item ID of a folder/lesson to list children of. Omit to list the course's top-level items.","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"limit":{"type":"integer","description":"Max items to return per call (1–200, default 50). Use small values to keep results within the LLM's context.","minimum":1,"maximum":200,"default":50},"offset":{"type":"integer","description":"Number of items to skip for pagination. Use the nextOffset value returned by a previous call to walk pages.","minimum":0,"default":0},"response_format":{"type":"string","description":"Return format: 'json' (default; structured), 'markdown' (human-readable) or 'toon' (token-optimized, ~40% fewer tokens than json — best for agents).","enum":["json","markdown","toon"],"default":"json"}},"required":["course_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Get course contents (root or folder children)"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
        
        ,
        Tool::new(
            r####"ntulearn_search_course_content"####.to_string(),
            r####"Recursively search ONE course's entire content tree for items matching a query (title or description; case-insensitive substring). Returns matched items with breadcrumb paths. Searching across ALL your courses at once? Use ntulearn_search_all_courses. Example: ntulearn_search_course_content(course_id='_2706844_1', query='assignment')"####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"query":{"type":"string","description":"Search term (case-insensitive substring)","minLength":1,"maxLength":200},"max_depth":{"type":"integer","description":"Maximum recursion depth (default 5, capped at 10)","default":5,"minimum":1,"maximum":10},"max_results":{"type":"integer","description":"Maximum number of matching items to return (default 50, capped at 200)","default":50,"minimum":1,"maximum":200},"response_format":{"type":"string","description":"Return format: 'json' (default; structured), 'markdown' (human-readable) or 'toon' (token-optimized, ~40% fewer tokens than json — best for agents).","enum":["json","markdown","toon"],"default":"json"}},"required":["course_id","query"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Search course content"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
        
        ,
        Tool::new(
            r####"ntulearn_download_file"####.to_string(),
            r####"Download every file attached to a Blackboard content item to your local disk. Handles resource/x-bb-file (attachment API) and resource/x-bb-document (HTML body with bbcswebdav links). Pass destination_dir to target a folder (e.g. destination_dir='~/NTU/y3s1/sc2002/week 8/'). Returns saved files with local paths and sizes. To inspect content INLINE instead of saving, prefer ntulearn_read_file_content. Example: ntulearn_download_file(course_id='_2706844_1', content_id='_6099516_1')"####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"content_id":{"type":"string","description":"Content item ID (e.g. _67890_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"destination_dir":{"type":"string","description":"Optional target directory. Accepts absolute paths and `~`-prefixed paths (e.g. '~/NTU/y3s1/sc2002/week 8/'). Created if missing. Defaults to NTULEARN_DOWNLOAD_DIR env var, or ./downloads/ if unset.","minLength":1,"maxLength":1024},"response_format":{"type":"string","description":"Return format: 'json' (default; structured), 'markdown' (human-readable) or 'toon' (token-optimized, ~40% fewer tokens than json — best for agents).","enum":["json","markdown","toon"],"default":"json"}},"required":["course_id","content_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Download files to local disk"####.to_string()),
            read_only_hint: Some(false),
            destructive_hint: Some(false),
            idempotent_hint: Some(false),
            open_world_hint: Some(true),
        })
        
        ,
        Tool::new(
            r####"ntulearn_read_file_content"####.to_string(),
            r####"Read the content of files attached to a Blackboard content item, returned inline (no local-filesystem hop). Use this to answer questions about lecture material; ntulearn_download_file is only for actually wanting the bytes on disk. PDFs default to text mode (pypdf, cheap, best for written content); mode='vision' for diagram/equation/screenshot-heavy pages, pages='1-3' to bound payload size (~3K vision tokens/page). Microsoft Office (.docx, .pptx incl. speaker notes, .xlsx all sheets) and text-like files (txt, md, csv, json, xml, html tag-stripped, code) are supported. Other binaries (images, video, audio, archives, legacy .doc/.ppt/.xls) are listed under `skipped` — fall back to ntulearn_download_file. Per-file cap 25 MB, batch cap 40 MB, vision cap 50 pages. Example: ntulearn_read_file_content(course_id='_2706844_1', content_id='_6099516_1', mode='text')"####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"content_id":{"type":"string","description":"Content item ID (e.g. _67890_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"mode":{"type":"string","description":"PDF handling. 'text' (default) extracts text via pypdf — cheap, fits MCP's payload budget. 'vision' additionally renders each page as an image with PyMuPDF (~3K vision tokens per page); use for diagram/equation/handwritten content, ideally with a narrow `pages` range to stay under the 1 MB cap. Ignored for non-PDF files.","enum":["text","vision","auto"],"default":"text"},"pages":{"type":"string","description":"Optional page range for PDFs (1-indexed, inclusive). Examples: '1-10', '3', '1,3,5', '1-5,8,10-12'. Omit to read all pages (vision mode capped at 50 rendered pages). Especially useful with mode='vision' to keep the response under MCP's 1 MB cap."},"response_format":{"type":"string","description":"Return format: 'json' (default; structured), 'markdown' (human-readable) or 'toon' (token-optimized, ~40% fewer tokens than json — best for agents).","enum":["json","markdown","toon"],"default":"json"}},"required":["course_id","content_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Read file content inline"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
        
        ,
        Tool::new(
            r####"ntulearn_get_upcoming"####.to_string(),
            r####"Get upcoming calendar items and assignment due dates across your enrolled courses (raw feed). Wraps Blackboard's calendar API — due dates surface as items with type='GradebookColumn'. Defaults to the next 2 weeks across every available course (fans out in parallel). Scope with course_ids, since/until (ISO-8601), or type (e.g. type='GradebookColumn' for due dates only). For a curated cross-course digest, use ntulearn_whats_new instead. Example: ntulearn_get_upcoming(course_ids=['_2706844_1'], type='GradebookColumn')"####.to_string(),
            json!({"type":"object","properties":{"since":{"type":"string","description":"ISO-8601 start of the window (e.g. '2026-05-09T00:00:00Z'). Omit to default to now."},"until":{"type":"string","description":"ISO-8601 end of the window. Omit to default to two weeks after `since`."},"course_ids":{"type":"array","description":"Optional list of course IDs to scope to. Omit to fan out across all available enrolled courses.","items":{"type":"string","pattern":"^[A-Za-z0-9_\\-:]+$"},"maxItems":200},"type":{"type":"string","description":"Optional calendar item type filter. Use 'GradebookColumn' for assignment due dates only.","enum":["Course","GradebookColumn","Institution","OfficeHours","Personal"]},"limit":{"type":"integer","description":"Max items to return per call (1–200, default 50). Use small values to keep results within the LLM's context.","minimum":1,"maximum":200,"default":50},"offset":{"type":"integer","description":"Number of items to skip for pagination. Use the nextOffset value returned by a previous call to walk pages.","minimum":0,"default":0},"response_format":{"type":"string","description":"Return format: 'json' (default; structured), 'markdown' (human-readable) or 'toon' (token-optimized, ~40% fewer tokens than json — best for agents).","enum":["json","markdown","toon"],"default":"json"}},"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Get upcoming items / due dates"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
        
        ,
        Tool::new(
            r####"ntulearn_get_announcements"####.to_string(),
            r####"Get announcements across your enrolled courses, newest first. Fans out across every available course by default; pass course_ids=['_123_1'] to scope. Use since (ISO-8601) to filter to recent ones (e.g. 'this week'). Each item includes its courseId so cross-course views stay attributable. Paginate with limit/offset. Example: ntulearn_get_announcements(course_ids=['_2706844_1'], since='2026-09-01T00:00:00Z')"####.to_string(),
            json!({"type":"object","properties":{"course_ids":{"type":"array","description":"Optional list of course IDs to scope to. Omit to fan out across all available enrolled courses.","items":{"type":"string","pattern":"^[A-Za-z0-9_\\-:]+$"},"maxItems":200},"since":{"type":"string","description":"Optional ISO-8601 cutoff (e.g. '2026-05-09T00:00:00Z'). Only announcements with `created` on/after this time are returned. Filtered client-side after fetch."},"limit":{"type":"integer","description":"Max items to return per call (1–200, default 50). Use small values to keep results within the LLM's context.","minimum":1,"maximum":200,"default":50},"offset":{"type":"integer","description":"Number of items to skip for pagination. Use the nextOffset value returned by a previous call to walk pages.","minimum":0,"default":0},"response_format":{"type":"string","description":"Return format: 'json' (default; structured), 'markdown' (human-readable) or 'toon' (token-optimized, ~40% fewer tokens than json — best for agents).","enum":["json","markdown","toon"],"default":"json"}},"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Get announcements (cross-course)"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
        
        ,
        Tool::new(
            r####"ntulearn_get_gradebook"####.to_string(),
            r####"Get gradebook columns across your enrolled courses, including your scores where available. Fans out across every available course by default; pass course_ids to scope. Each column carries its courseId. Paginate with limit/offset. For a spreadsheet-ready export, use ntulearn_export_gradebook_csv; for submission attempts on one column, use ntulearn_get_gradebook_attempts. Example: ntulearn_get_gradebook(course_ids=['_2706844_1'])"####.to_string(),
            json!({"type":"object","properties":{"course_ids":{"type":"array","description":"Optional list of course IDs to scope to. Omit to fan out across all available enrolled courses.","items":{"type":"string","pattern":"^[A-Za-z0-9_\\-:]+$"},"maxItems":200},"limit":{"type":"integer","description":"Max items to return per call (1–200, default 50). Use small values to keep results within the LLM's context.","minimum":1,"maximum":200,"default":50},"offset":{"type":"integer","description":"Number of items to skip for pagination. Use the nextOffset value returned by a previous call to walk pages.","minimum":0,"default":0},"response_format":{"type":"string","description":"Return format: 'json' (default; structured), 'markdown' (human-readable) or 'toon' (token-optimized, ~40% fewer tokens than json — best for agents).","enum":["json","markdown","toon"],"default":"json"}},"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Get gradebook (cross-course)"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
        
        ,
        Tool::new(
            r####"ntulearn_list_messages"####.to_string(),
            r####"List a user's Blackboard messages (mailbox). Defaults to the inbox; folder='sent' for the outbox. Optionally filter to unread_only or a since (ISO-8601) cutoff. Paginate with limit/offset. Example: ntulearn_list_messages(folder='inbox', unread_only=true)"####.to_string(),
            json!({"type":"object","properties":{"folder":{"type":"string","description":"Mailbox folder to read. Blackboard exposes 'inbox' (received) and 'sent' (outbox).","enum":["inbox","sent"],"default":"inbox"},"unread_only":{"type":"boolean","description":"Return only unread messages when true.","default":false},"since":{"type":"string","description":"Optional ISO-8601 cutoff (e.g. '2026-05-09T00:00:00Z'). Only items created on/after this time are included."},"limit":{"type":"integer","description":"Max items to return per call (1–200, default 50). Use small values to keep results within the LLM's context.","minimum":1,"maximum":200,"default":50},"offset":{"type":"integer","description":"Number of items to skip for pagination. Use the nextOffset value returned by a previous call to walk pages.","minimum":0,"default":0},"response_format":{"type":"string","description":"Return format: 'json' (default; structured), 'markdown' (human-readable) or 'toon' (token-optimized, ~40% fewer tokens than json — best for agents).","enum":["json","markdown","toon"],"default":"json"}},"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"List user messages"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
        
        ,
        Tool::new(
            r####"ntulearn_read_message"####.to_string(),
            r####"Read a single Blackboard message by ID: subject, full body, recipient list. Get the ID from ntulearn_list_messages. Example: ntulearn_read_message(message_id='_12345_1')"####.to_string(),
            json!({"type":"object","properties":{"message_id":{"type":"string","description":"Blackboard message ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"response_format":{"type":"string","description":"Return format: 'json' (default; structured), 'markdown' (human-readable) or 'toon' (token-optimized, ~40% fewer tokens than json — best for agents).","enum":["json","markdown","toon"],"default":"json"}},"required":["message_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Read a user message"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
        
        ,
        Tool::new(
            r####"ntulearn_list_course_users"####.to_string(),
            r####"List the users enrolled in a course (instructors, teaching staff, students as the API exposes them). Useful for rosters and 'who teaches this'. Paginate with limit/offset. Example: ntulearn_list_course_users(course_id='_2706844_1')"####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"limit":{"type":"integer","description":"Max items to return per call (1–200, default 50). Use small values to keep results within the LLM's context.","minimum":1,"maximum":200,"default":50},"offset":{"type":"integer","description":"Number of items to skip for pagination. Use the nextOffset value returned by a previous call to walk pages.","minimum":0,"default":0},"response_format":{"type":"string","description":"Return format: 'json' (default; structured), 'markdown' (human-readable) or 'toon' (token-optimized, ~40% fewer tokens than json — best for agents).","enum":["json","markdown","toon"],"default":"json"}},"required":["course_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"List course users"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
        
        ,
        Tool::new(
            r####"ntulearn_list_course_groups"####.to_string(),
            r####"List the groups defined in a course (e.g. tutorial/lab groups). Paginate with limit/offset. Example: ntulearn_list_course_groups(course_id='_2706844_1')"####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"limit":{"type":"integer","description":"Max items to return per call (1–200, default 50). Use small values to keep results within the LLM's context.","minimum":1,"maximum":200,"default":50},"offset":{"type":"integer","description":"Number of items to skip for pagination. Use the nextOffset value returned by a previous call to walk pages.","minimum":0,"default":0},"response_format":{"type":"string","description":"Return format: 'json' (default; structured), 'markdown' (human-readable) or 'toon' (token-optimized, ~40% fewer tokens than json — best for agents).","enum":["json","markdown","toon"],"default":"json"}},"required":["course_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"List course groups"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
        
        ,
        Tool::new(
            r####"ntulearn_get_group_members"####.to_string(),
            r####"List the members of a specific course group (e.g. your tutorial group's roster). Group IDs come from ntulearn_list_course_groups. Example: ntulearn_get_group_members(course_id='_2706844_1', group_id='_12345_1')"####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"group_id":{"type":"string","description":"Group ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"response_format":{"type":"string","description":"Return format: 'json' (default; structured), 'markdown' (human-readable) or 'toon' (token-optimized, ~40% fewer tokens than json — best for agents).","enum":["json","markdown","toon"],"default":"json"}},"required":["course_id","group_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Get course group members"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
        
        ,
        Tool::new(
            r####"ntulearn_get_gradebook_attempts"####.to_string(),
            r####"List submission attempts for a gradebook column (assignment). Column IDs come from ntulearn_get_gradebook. Pass user_id to scope to one student's attempts. Paginate with limit/offset. Example: ntulearn_get_gradebook_attempts(course_id='_2706844_1', column_id='_67890_1')"####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"column_id":{"type":"string","description":"Gradebook column ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"user_id":{"type":"string","description":"Optional user ID to scope attempts to one student.","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"limit":{"type":"integer","description":"Max items to return per call (1–200, default 50). Use small values to keep results within the LLM's context.","minimum":1,"maximum":200,"default":50},"offset":{"type":"integer","description":"Number of items to skip for pagination. Use the nextOffset value returned by a previous call to walk pages.","minimum":0,"default":0},"response_format":{"type":"string","description":"Return format: 'json' (default; structured), 'markdown' (human-readable) or 'toon' (token-optimized, ~40% fewer tokens than json — best for agents).","enum":["json","markdown","toon"],"default":"json"}},"required":["course_id","column_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Get gradebook attempts"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
        
        ,
        Tool::new(
            r####"ntulearn_search_all_courses"####.to_string(),
            r####"Search across ALL your enrolled courses' content trees for items matching a query. Like ntulearn_search_course_content but scoped to every available course at once; each match carries its courseId and breadcrumb for attribution. Limit with max_results. Example: ntulearn_search_all_courses(query='midterm')"####.to_string(),
            json!({"type":"object","properties":{"query":{"type":"string","description":"Search term (case-insensitive substring)","minLength":1,"maxLength":200},"course_ids":{"type":"array","description":"Optional list of course IDs to scope to. Omit to fan out across all available enrolled courses.","items":{"type":"string","pattern":"^[A-Za-z0-9_\\-:]+$"},"maxItems":200},"max_depth":{"type":"integer","description":"Maximum recursion depth per course (default 3, capped at 10).","default":3,"minimum":1,"maximum":10},"max_results":{"type":"integer","description":"Maximum number of matching items to return (default 50, capped at 200)","default":50,"minimum":1,"maximum":200},"response_format":{"type":"string","description":"Return format: 'json' (default; structured), 'markdown' (human-readable) or 'toon' (token-optimized, ~40% fewer tokens than json — best for agents).","enum":["json","markdown","toon"],"default":"json"}},"required":["query"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Search content across all courses"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
        
        ,
        Tool::new(
            r####"ntulearn_get_content_tree"####.to_string(),
            r####"Return a course's ENTIRE content tree as nested JSON (folders with children) in one call — unlike ntulearn_get_course_contents, which lists one level per call. Bounds the walk with max_depth (capped 10). Prefer when you need the full outline/map of a course; prefer ntulearn_get_course_contents for browsing folder by folder. Example: ntulearn_get_content_tree(course_id='_2706844_1', max_depth=3)"####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"max_depth":{"type":"integer","description":"Maximum recursion depth (capped at 10).","default":5,"minimum":1,"maximum":10},"response_format":{"type":"string","description":"Return format: 'json' (default; structured), 'markdown' (human-readable) or 'toon' (token-optimized, ~40% fewer tokens than json — best for agents).","enum":["json","markdown","toon"],"default":"json"}},"required":["course_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Get full course content tree"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
        
        ,
        Tool::new(
            r####"ntulearn_download_course"####.to_string(),
            r####"Recursively download every file in a course's content tree to local disk, organised into course/subject folders. Skips already-downloaded files by default; skip_existing=false to force. Concurrency bounded by parallel. WRITE tool — saves files on your machine. For a single item's files, use ntulearn_download_file. Example: ntulearn_download_course(course_id='_2706844_1', include_extensions='pdf,pptx')"####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"destination_dir":{"type":"string","description":"Target root directory (absolute or ~-prefixed). Defaults to ~/Downloads/NTU/<course>.","minLength":1,"maxLength":1024},"max_depth":{"type":"integer","description":"Maximum recursion depth (capped at 10).","default":5,"minimum":1,"maximum":10},"include_extensions":{"type":"string","description":"Optional comma-separated file extensions to download, e.g. 'pdf,ppt,pptx,docx'. Omit to download all."},"skip_existing":{"type":"boolean","description":"Skip files already present at the destination.","default":true},"parallel":{"type":"integer","description":"Max concurrent downloads (1-16).","default":4,"minimum":1,"maximum":16},"response_format":{"type":"string","description":"Return format: 'json' (default; structured), 'markdown' (human-readable) or 'toon' (token-optimized, ~40% fewer tokens than json — best for agents).","enum":["json","markdown","toon"],"default":"json"}},"required":["course_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Download an entire course to disk"####.to_string()),
            read_only_hint: Some(false),
            destructive_hint: Some(false),
            idempotent_hint: Some(false),
            open_world_hint: Some(true),
        })
        
        ,
        Tool::new(
            r####"ntulearn_whats_new"####.to_string(),
            r####"One-call 'what changed recently' digest: recent announcements + upcoming due dates + gradebook summary across your courses since a cutoff. Defaults to the tracker's last-seen time (or last 7 days). update_tracker=true records `since` as the new last-seen for future calls. For the RAW unfiltered due-date feed, use ntulearn_get_upcoming. Example: ntulearn_whats_new(update_tracker=true)"####.to_string(),
            json!({"type":"object","properties":{"course_ids":{"type":"array","description":"Optional list of course IDs to scope to. Omit to fan out across all available enrolled courses.","items":{"type":"string","pattern":"^[A-Za-z0-9_\\-:]+$"},"maxItems":200},"since":{"type":"string","description":"Optional ISO-8601 cutoff. Defaults to the tracker's last-seen time, or 7 days ago if never set."},"update_tracker":{"type":"boolean","description":"Persist `since` as the new last-seen marker for future ntulearn_whats_new calls.","default":false},"response_format":{"type":"string","description":"Return format: 'json' (default; structured), 'markdown' (human-readable) or 'toon' (token-optimized, ~40% fewer tokens than json — best for agents).","enum":["json","markdown","toon"],"default":"json"}},"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"What's new across courses"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
        
        ,
        Tool::new(
            r####"ntulearn_export_calendar_ics"####.to_string(),
            r####"Export calendar items (including assignment due dates) as an iCalendar (.ics) string you can paste into Google Calendar / Apple Calendar / Outlook. Optionally scope with course_ids and a since/until window. Example: ntulearn_export_calendar_ics(course_ids=['_2706844_1'])"####.to_string(),
            json!({"type":"object","properties":{"course_ids":{"type":"array","description":"Optional list of course IDs to scope to. Omit to fan out across all available enrolled courses.","items":{"type":"string","pattern":"^[A-Za-z0-9_\\-:]+$"},"maxItems":200},"since":{"type":"string","description":"Optional ISO-8601 cutoff (e.g. '2026-05-09T00:00:00Z'). Only items created on/after this time are included."},"until":{"type":"string","description":"Optional ISO-8601 end of the window (e.g. '2026-05-23T00:00:00Z')."},"response_format":{"type":"string","description":"Return format: 'json' (default; structured), 'markdown' (human-readable) or 'toon' (token-optimized, ~40% fewer tokens than json — best for agents).","enum":["json","markdown","toon"],"default":"json"}},"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Export calendar as ICS"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
        
        ,
        Tool::new(
            r####"ntulearn_export_gradebook_csv"####.to_string(),
            r####"Export your gradebook columns and scores across courses as a CSV string for a spreadsheet. Header: courseId,column,possible,score,grade,status. For a structured/paginated view, use ntulearn_get_gradebook. Example: ntulearn_export_gradebook_csv()"####.to_string(),
            json!({"type":"object","properties":{"course_ids":{"type":"array","description":"Optional list of course IDs to scope to. Omit to fan out across all available enrolled courses.","items":{"type":"string","pattern":"^[A-Za-z0-9_\\-:]+$"},"maxItems":200},"response_format":{"type":"string","description":"Return format: 'json' (default; structured), 'markdown' (human-readable) or 'toon' (token-optimized, ~40% fewer tokens than json — best for agents).","enum":["json","markdown","toon"],"default":"json"}},"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Export gradebook as CSV"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
        
        ,
        Tool::new(
            r####"ntulearn_summarize_course"####.to_string(),
            r####"One-call course briefing: title, description, instructors, enrollment count, upcoming due dates, recent announcements, gradebook summary, and top-level content folders. Each sub-section degrades gracefully on error. Great first tool when meeting a new course. Example: ntulearn_summarize_course(course_id='_2706844_1')"####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"include_contents":{"type":"boolean","description":"Include the top-level content folders in the briefing.","default":true},"response_format":{"type":"string","description":"Return format: 'json' (default; structured), 'markdown' (human-readable) or 'toon' (token-optimized, ~40% fewer tokens than json — best for agents).","enum":["json","markdown","toon"],"default":"json"}},"required":["course_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Summarize a course"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })

        ,
        Tool::new(
            r####"ntulearn_login"####.to_string(),
            r####"Log in to NTULearn. Call this when another ntulearn_* tool reports 'Not logged in' / HTTP 401, or before first use. If a saved NTU login exists it renews the session silently; otherwise it opens a browser window where the USER must log in with their NTU account — tell them to do so (and to answer Yes to 'Stay signed in?'). Note: when a tool fails with 401 and says a login window 'has just opened', the window is already up — just wait for the user to log in, then retry that tool (no need to call this first). Returns once logged in, or after ~45 s with status 'waiting_for_user' while the window stays open; then call it again to confirm. Example: ntulearn_login()"####.to_string(),
            json!({"type":"object","properties":{"force":{"type":"boolean","description":"Log in again even if the current session is still valid.","default":false}},"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Log in to NTULearn"####.to_string()),
            read_only_hint: Some(false),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
        ,
    ]
}

#[cfg(test)]
mod discovery_quality {
    use super::*;
    use serde_json::Value;

    /// "Intern test": an agent should be able to use every tool correctly given
    /// only its description + parameter docs. Guards against future regressions
    /// in tool discoverability (missing descriptions / examples / titles).
    #[test]
    fn every_tool_is_intern_test_ready() {
        let tools = all_tools();
        assert_eq!(tools.len(), 22, "tool surface changed; update this test");
        for t in &tools {
            assert!(!t.name.is_empty(), "tool name empty");
            assert!(
                t.description.len() >= 120,
                "{}: description too short ({} chars) — agents need when-to-use + an example",
                t.name,
                t.description.len()
            );
            assert!(
                t.description.contains("Example:"),
                "{}: description must contain an 'Example:' call",
                t.name
            );
            assert!(
                t.input_schema.get("additionalProperties") == Some(&Value::Bool(false)),
                "{}: schema must set additionalProperties=false",
                t.name
            );
            let ann = t.annotations.as_ref().expect("tool must carry annotations");
            assert!(
                ann.title.as_deref().map(|s| !s.is_empty()).unwrap_or(false),
                "{}: missing human title",
                t.name
            );
        }
    }

    /// No parameter may lack a description — agents pick arguments from these.
    #[test]
    fn every_parameter_has_description() {
        for t in all_tools() {
            let props = t
                .input_schema
                .get("properties")
                .and_then(|v| v.as_object())
                .expect("tool must declare a properties object");
            for (k, v) in props {
                let desc = v.get("description").and_then(|d| d.as_str()).unwrap_or("");
                assert!(
                    !desc.trim().is_empty(),
                    "{}: parameter `{}` lacks a description",
                    t.name,
                    k
                );
            }
        }
    }
}
