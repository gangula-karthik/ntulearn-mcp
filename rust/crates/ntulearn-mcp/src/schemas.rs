// AUTO-GENERATED from the Python manifest (/tmp/tool_manifest.json). Do not edit by hand.
use serde_json::{json, Value};
use ultrafast_mcp::{Tool, ToolAnnotations};

/// All 21 NTULearn tool definitions with their exact input schemas.
pub fn all_tools() -> Vec<Tool> {
    vec![
        Tool::new(
            r####"ntulearn_list_courses"####.to_string(),
            r####"List courses the current user is enrolled in on NTULearn. By default returns only active/available courses. Set include_disabled=true to also include unavailable ones. Supports pagination via limit/offset."####.to_string(),
            json!({"type":"object","properties":{"include_disabled":{"type":"boolean","description":"Include courses where availability.available != 'Yes'","default":false},"limit":{"type":"integer","description":"Max items to return per call (1–200, default 50). Use small values to keep results within the LLM's context.","minimum":1,"maximum":200,"default":50},"offset":{"type":"integer","description":"Number of items to skip for pagination. Use the nextOffset value returned by a previous call to walk pages.","minimum":0,"default":0},"response_format":{"type":"string","description":"'json' returns a structured payload (default, recommended for agents). 'markdown' returns a human-readable summary.","enum":["json","markdown"],"default":"json"}},"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"List my NTULearn courses"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
        .with_output_schema(json!({"type":"object","properties":{"courses":{"type":"array","items":{"type":"object","properties":{"courseId":{"type":"string"},"title":{"type":"string"},"available":{"type":"string"},"lastAccessed":{"type":["string","null"]}},"required":["courseId","title"]}},"total":{"type":"integer"},"count":{"type":"integer"},"offset":{"type":"integer"},"limit":{"type":"integer"},"hasMore":{"type":"boolean"},"nextOffset":{"type":["integer","null"]}},"required":["courses","total","count","offset","limit","hasMore"]}))
        ,
        Tool::new(
            r####"ntulearn_get_course_contents"####.to_string(),
            r####"Walk a course's content tree. Omit parent_id to get the top-level items (folders, documents, links, assignments); pass parent_id of a folder/lesson where hasChildren=true to drill into its children. Supports pagination."####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"parent_id":{"type":"string","description":"Optional content item ID of a folder/lesson to list children of. Omit to list the course's top-level items.","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"limit":{"type":"integer","description":"Max items to return per call (1–200, default 50). Use small values to keep results within the LLM's context.","minimum":1,"maximum":200,"default":50},"offset":{"type":"integer","description":"Number of items to skip for pagination. Use the nextOffset value returned by a previous call to walk pages.","minimum":0,"default":0},"response_format":{"type":"string","description":"'json' returns a structured payload (default, recommended for agents). 'markdown' returns a human-readable summary.","enum":["json","markdown"],"default":"json"}},"required":["course_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Get course contents (root or folder children)"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
        .with_output_schema(json!({"type":"object","properties":{"items":{"type":"array","items":{"type":"object","properties":{"id":{"type":["string","null"]},"title":{"type":["string","null"]},"contentHandlerId":{"type":["string","null"]},"hasChildren":{"type":"boolean"},"description":{"type":["string","null"]},"modified":{"type":["string","null"]}}}},"total":{"type":"integer"},"count":{"type":"integer"},"offset":{"type":"integer"},"limit":{"type":"integer"},"hasMore":{"type":"boolean"},"nextOffset":{"type":["integer","null"]}},"required":["items","total","count","offset","limit","hasMore"]}))
        ,
        Tool::new(
            r####"ntulearn_search_course_content"####.to_string(),
            r####"Recursively search a course's entire content tree for items matching a query. Matches on title or description (case-insensitive substring). Returns matched items with their full breadcrumb path."####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"query":{"type":"string","description":"Search term (case-insensitive substring)","minLength":1,"maxLength":200},"max_depth":{"type":"integer","description":"Maximum recursion depth (default 5, capped at 10)","default":5,"minimum":1,"maximum":10},"max_results":{"type":"integer","description":"Maximum number of matching items to return (default 50, capped at 200)","default":50,"minimum":1,"maximum":200},"response_format":{"type":"string","description":"'json' returns a structured payload (default, recommended for agents). 'markdown' returns a human-readable summary.","enum":["json","markdown"],"default":"json"}},"required":["course_id","query"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Search course content"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
        .with_output_schema(json!({"type":"object","properties":{"matches":{"type":"array","items":{"type":"object","properties":{"id":{"type":["string","null"]},"title":{"type":["string","null"]},"contentHandlerId":{"type":["string","null"]},"hasChildren":{"type":"boolean"},"description":{"type":["string","null"]},"modified":{"type":["string","null"]},"breadcrumb":{"type":"array","items":{"type":"string"}}}}},"count":{"type":"integer"}},"required":["matches","count"]}))
        ,
        Tool::new(
            r####"ntulearn_download_file"####.to_string(),
            r####"Download every file attached to a Blackboard content item to local disk. Handles both resource/x-bb-file (attachment API) and resource/x-bb-document (HTML body with bbcswebdav links) handler types. Pass destination_dir to target a specific folder — useful for organising a semester (e.g. destination_dir='~/NTU/y3s1/sc2002/week 8/'). Returns saved files with their resolved local paths and sizes. Use ntulearn_read_file_content if you want to inspect the content inline rather than saving to disk."####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"content_id":{"type":"string","description":"Content item ID (e.g. _67890_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"destination_dir":{"type":"string","description":"Optional target directory. Accepts absolute paths and `~`-prefixed paths (e.g. '~/NTU/y3s1/sc2002/week 8/'). Created if missing. Defaults to NTULEARN_DOWNLOAD_DIR env var, or ./downloads/ if unset.","minLength":1,"maxLength":1024},"response_format":{"type":"string","description":"'json' returns a structured payload (default, recommended for agents). 'markdown' returns a human-readable summary.","enum":["json","markdown"],"default":"json"}},"required":["course_id","content_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Download files to local disk"####.to_string()),
            read_only_hint: Some(false),
            destructive_hint: Some(false),
            idempotent_hint: Some(false),
            open_world_hint: Some(true),
        })
        .with_output_schema(json!({"type":"object","properties":{"contentId":{"type":["string","null"]},"title":{"type":["string","null"]},"contentHandlerId":{"type":["string","null"]},"files":{"type":"array","items":{"type":"object","properties":{"url":{"type":["string","null"]},"filename":{"type":["string","null"]},"mimeType":{"type":["string","null"]},"link_text":{"type":["string","null"]},"localPath":{"type":"string"},"sizeBytes":{"type":"integer"},"kind":{"type":"string"},"text":{"type":"string"},"pageCount":{"type":"integer"},"pagesRendered":{"type":"array","items":{"type":"integer"}},"truncatedPages":{"type":"integer"},"truncationReason":{"type":"string","enum":["byte_budget","page_cap"]},"paragraphCount":{"type":"integer"},"tableCount":{"type":"integer"},"slideCount":{"type":"integer"},"sheetCount":{"type":"integer"},"contentType":{"type":["string","null"]},"warning":{"type":"string"},"error":{"type":"string"},"reason":{"type":"string"}}}},"destinationDir":{"type":"string"},"error":{"type":"string"}},"required":["files"]}))
        ,
        Tool::new(
            r####"ntulearn_read_file_content"####.to_string(),
            r####"Read the content of files attached to a Blackboard content item, returned inline (no local-filesystem hop). Use this to ask questions about lecture material — ntulearn_download_file is for users who actually want the bytes on disk. PDFs default to text mode (via pypdf — cheap and almost always what you want for written content). Pass mode='vision' for diagram-, equation-, or screenshot-heavy pages; pair with pages='5' or pages='1-3' to keep the payload under MCP's 1 MB cap (~3K vision tokens per page). For multi-page diagram-heavy decks, prefer ntulearn_download_file plus drag-and-drop into claude.ai. Also supports Microsoft Office formats (.docx, .pptx with speaker notes, .xlsx with all sheets) and text-like files (txt, md, csv, json, xml, html with tags stripped, code files). Other binaries (images, video, audio, archives, legacy .doc/.ppt/.xls) are listed under `skipped` — fall back to ntulearn_download_file for those. Per-file cap 25 MB, batch cap 40 MB, vision cap 50 rendered pages; oversized files are skipped."####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"content_id":{"type":"string","description":"Content item ID (e.g. _67890_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"mode":{"type":"string","description":"PDF handling. 'text' (default) extracts text via pypdf — cheap, fits MCP's payload budget. 'vision' additionally renders each page as an image with PyMuPDF (~3K vision tokens per page); use for diagram/equation/handwritten content, ideally with a narrow `pages` range to stay under the 1 MB cap. Ignored for non-PDF files.","enum":["text","vision","auto"],"default":"text"},"pages":{"type":"string","description":"Optional page range for PDFs (1-indexed, inclusive). Examples: '1-10', '3', '1,3,5', '1-5,8,10-12'. Omit to read all pages (vision mode capped at 50 rendered pages). Especially useful with mode='vision' to keep the response under MCP's 1 MB cap."},"response_format":{"type":"string","description":"'json' returns a structured payload (default, recommended for agents). 'markdown' returns a human-readable summary.","enum":["json","markdown"],"default":"json"}},"required":["course_id","content_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Read file content inline"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
        .with_output_schema(json!({"type":"object","properties":{"contentId":{"type":["string","null"]},"title":{"type":["string","null"]},"contentHandlerId":{"type":["string","null"]},"files":{"type":"array","items":{"type":"object","properties":{"url":{"type":["string","null"]},"filename":{"type":["string","null"]},"mimeType":{"type":["string","null"]},"link_text":{"type":["string","null"]},"localPath":{"type":"string"},"sizeBytes":{"type":"integer"},"kind":{"type":"string"},"text":{"type":"string"},"pageCount":{"type":"integer"},"pagesRendered":{"type":"array","items":{"type":"integer"}},"truncatedPages":{"type":"integer"},"truncationReason":{"type":"string","enum":["byte_budget","page_cap"]},"paragraphCount":{"type":"integer"},"tableCount":{"type":"integer"},"slideCount":{"type":"integer"},"sheetCount":{"type":"integer"},"contentType":{"type":["string","null"]},"warning":{"type":"string"},"error":{"type":"string"},"reason":{"type":"string"}}}},"skipped":{"type":"array","items":{"type":"object","properties":{"url":{"type":["string","null"]},"filename":{"type":["string","null"]},"mimeType":{"type":["string","null"]},"link_text":{"type":["string","null"]},"localPath":{"type":"string"},"sizeBytes":{"type":"integer"},"kind":{"type":"string"},"text":{"type":"string"},"pageCount":{"type":"integer"},"pagesRendered":{"type":"array","items":{"type":"integer"}},"truncatedPages":{"type":"integer"},"truncationReason":{"type":"string","enum":["byte_budget","page_cap"]},"paragraphCount":{"type":"integer"},"tableCount":{"type":"integer"},"slideCount":{"type":"integer"},"sheetCount":{"type":"integer"},"contentType":{"type":["string","null"]},"warning":{"type":"string"},"error":{"type":"string"},"reason":{"type":"string"}}}},"error":{"type":"string"}},"required":["files","skipped"]}))
        ,
        Tool::new(
            r####"ntulearn_get_upcoming"####.to_string(),
            r####"Get upcoming calendar items and assignment due dates across your enrolled courses. Wraps Blackboard's calendar API — assignment due dates surface as items with type='GradebookColumn'. By default returns the next 2 weeks across every available course (server fans out per-course in parallel). Pass course_ids to scope to specific courses, since/until (ISO-8601) to override the window, or type to filter (e.g. type='GradebookColumn' for due dates only)."####.to_string(),
            json!({"type":"object","properties":{"since":{"type":"string","description":"ISO-8601 start of the window (e.g. '2026-05-09T00:00:00Z'). Omit to default to now."},"until":{"type":"string","description":"ISO-8601 end of the window. Omit to default to two weeks after `since`."},"course_ids":{"type":"array","description":"Optional list of course IDs to scope to. Omit to fan out across all available enrolled courses.","items":{"type":"string","pattern":"^[A-Za-z0-9_\\-:]+$"},"maxItems":200},"type":{"type":"string","description":"Optional calendar item type filter. Use 'GradebookColumn' for assignment due dates only.","enum":["Course","GradebookColumn","Institution","OfficeHours","Personal"]},"limit":{"type":"integer","description":"Max items to return per call (1–200, default 50). Use small values to keep results within the LLM's context.","minimum":1,"maximum":200,"default":50},"offset":{"type":"integer","description":"Number of items to skip for pagination. Use the nextOffset value returned by a previous call to walk pages.","minimum":0,"default":0},"response_format":{"type":"string","description":"'json' returns a structured payload (default, recommended for agents). 'markdown' returns a human-readable summary.","enum":["json","markdown"],"default":"json"}},"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Get upcoming items / due dates"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
        .with_output_schema(json!({"type":"object","properties":{"items":{"type":"array","items":{"type":"object","properties":{"id":{"type":["string","null"]},"type":{"type":["string","null"]},"title":{"type":["string","null"]},"description":{"type":["string","null"]},"location":{"type":["string","null"]},"start":{"type":["string","null"]},"end":{"type":["string","null"]},"calendarName":{"type":["string","null"]},"courseId":{"type":["string","null"]},"eventType":{"type":["string","null"]},"gradable":{"type":["boolean","null"]},"attemptable":{"type":["boolean","null"]}}}},"total":{"type":"integer"},"count":{"type":"integer"},"offset":{"type":"integer"},"limit":{"type":"integer"},"hasMore":{"type":"boolean"},"nextOffset":{"type":["integer","null"]},"courseIdsQueried":{"type":"array","items":{"type":"string"}},"courseErrors":{"type":"object","additionalProperties":{"type":"string"}}},"required":["items","total","count","offset","limit","hasMore"]}))
        ,
        Tool::new(
            r####"ntulearn_get_announcements"####.to_string(),
            r####"Get announcements across your enrolled courses, newest first. By default fans out across every available course; pass course_ids=['_123_1'] to scope. Use since (ISO-8601) to filter to recent announcements only (e.g. "this week"). Each item includes the courseId it was posted to so cross-course views stay attributable. Supports pagination."####.to_string(),
            json!({"type":"object","properties":{"course_ids":{"type":"array","description":"Optional list of course IDs to scope to. Omit to fan out across all available enrolled courses.","items":{"type":"string","pattern":"^[A-Za-z0-9_\\-:]+$"},"maxItems":200},"since":{"type":"string","description":"Optional ISO-8601 cutoff (e.g. '2026-05-09T00:00:00Z'). Only announcements with `created` on/after this time are returned. Filtered client-side after fetch."},"limit":{"type":"integer","description":"Max items to return per call (1–200, default 50). Use small values to keep results within the LLM's context.","minimum":1,"maximum":200,"default":50},"offset":{"type":"integer","description":"Number of items to skip for pagination. Use the nextOffset value returned by a previous call to walk pages.","minimum":0,"default":0},"response_format":{"type":"string","description":"'json' returns a structured payload (default, recommended for agents). 'markdown' returns a human-readable summary.","enum":["json","markdown"],"default":"json"}},"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Get announcements (cross-course)"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
        .with_output_schema(json!({"type":"object","properties":{"announcements":{"type":"array","items":{"type":"object","properties":{"id":{"type":["string","null"]},"courseId":{"type":["string","null"]},"title":{"type":["string","null"]},"body":{"type":["string","null"]},"created":{"type":["string","null"]},"modified":{"type":["string","null"]},"available":{"type":["string","null"]}}}},"total":{"type":"integer"},"count":{"type":"integer"},"offset":{"type":"integer"},"limit":{"type":"integer"},"hasMore":{"type":"boolean"},"nextOffset":{"type":["integer","null"]},"courseIdsQueried":{"type":"array","items":{"type":"string"}},"courseErrors":{"type":"object","additionalProperties":{"type":"string"}}},"required":["announcements","total","count","offset","limit","hasMore"]}))
        ,
        Tool::new(
            r####"ntulearn_get_gradebook"####.to_string(),
            r####"Get gradebook columns across your enrolled courses, including your scores where available. By default fans out across every available course; pass course_ids=['_123_1'] to scope. Each column carries the courseId it belongs to so cross-course views stay attributable. Supports pagination."####.to_string(),
            json!({"type":"object","properties":{"course_ids":{"type":"array","description":"Optional list of course IDs to scope to. Omit to fan out across all available enrolled courses.","items":{"type":"string","pattern":"^[A-Za-z0-9_\\-:]+$"},"maxItems":200},"limit":{"type":"integer","description":"Max items to return per call (1–200, default 50). Use small values to keep results within the LLM's context.","minimum":1,"maximum":200,"default":50},"offset":{"type":"integer","description":"Number of items to skip for pagination. Use the nextOffset value returned by a previous call to walk pages.","minimum":0,"default":0},"response_format":{"type":"string","description":"'json' returns a structured payload (default, recommended for agents). 'markdown' returns a human-readable summary.","enum":["json","markdown"],"default":"json"}},"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Get gradebook (cross-course)"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
        .with_output_schema(json!({"type":"object","properties":{"columns":{"type":"array","items":{"type":"object","properties":{"id":{"type":["string","null"]},"courseId":{"type":["string","null"]},"name":{"type":["string","null"]},"displayName":{"type":["string","null"]},"possible":{"type":["number","null"]},"available":{"type":["string","null"]},"contentId":{"type":["string","null"]},"score":{"type":["number","string","null"]},"grade":{"type":["string","null"]},"status":{"type":["string","null"]}}}},"total":{"type":"integer"},"count":{"type":"integer"},"offset":{"type":"integer"},"limit":{"type":"integer"},"hasMore":{"type":"boolean"},"nextOffset":{"type":["integer","null"]},"gradesAvailable":{"type":"boolean"},"gradeFetchError":{"type":["string","null"]},"courseIdsQueried":{"type":"array","items":{"type":"string"}},"courseErrors":{"type":"object","additionalProperties":{"type":"string"}}},"required":["columns","total","count","offset","limit","hasMore","gradesAvailable"]}))
        ,
        Tool::new(
            r####"ntulearn_list_messages"####.to_string(),
            r####"List a user's Blackboard messages (mailbox) on NTULearn. Defaults to the inbox; pass folder='sent' for the outbox. Optionally filter to unread messages or a since (ISO-8601) cutoff. Supports pagination."####.to_string(),
            json!({"type":"object","properties":{"folder":{"type":"string","description":"Mailbox folder to read. Blackboard exposes 'inbox' (received) and 'sent' (outbox).","enum":["inbox","sent"],"default":"inbox"},"unread_only":{"type":"boolean","description":"Return only unread messages when true.","default":false},"since":{"type":"string","description":"Optional ISO-8601 cutoff (e.g. '2026-05-09T00:00:00Z'). Only items created on/after this time are included."},"limit":{"type":"integer","description":"Max items to return per call (1–200, default 50). Use small values to keep results within the LLM's context.","minimum":1,"maximum":200,"default":50},"offset":{"type":"integer","description":"Number of items to skip for pagination. Use the nextOffset value returned by a previous call to walk pages.","minimum":0,"default":0},"response_format":{"type":"string","description":"'json' returns a structured payload (default, recommended for agents). 'markdown' returns a human-readable summary.","enum":["json","markdown"],"default":"json"}},"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"List user messages"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
        .with_output_schema(json!({"type":"object","properties":{"messages":{"type":"array","items":{"type":"object","properties":{"id":{"type":["string","null"]},"subject":{"type":["string","null"]},"body":{"type":["string","null"]},"senderName":{"type":["string","null"]},"senderId":{"type":["string","null"]},"createdAt":{"type":["string","null"]},"read":{"type":"boolean"},"recipients":{"type":"array","items":{"type":"object"}}}}},"total":{"type":"integer"},"count":{"type":"integer"},"offset":{"type":"integer"},"limit":{"type":"integer"},"hasMore":{"type":"boolean"},"nextOffset":{"type":["integer","null"]}},"required":["messages","total","count","offset","limit","hasMore"]}))
        ,
        Tool::new(
            r####"ntulearn_read_message"####.to_string(),
            r####"Read a single Blackboard message by ID, including its subject, full body and recipient list. The ID comes from ntulearn_list_messages."####.to_string(),
            json!({"type":"object","properties":{"message_id":{"type":"string","description":"Blackboard message ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"response_format":{"type":"string","description":"'json' returns a structured payload (default, recommended for agents). 'markdown' returns a human-readable summary.","enum":["json","markdown"],"default":"json"}},"required":["message_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Read a user message"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
        .with_output_schema(json!({"type":"object","properties":{"message":{"type":"object","properties":{"id":{"type":["string","null"]},"subject":{"type":["string","null"]},"body":{"type":["string","null"]},"senderName":{"type":["string","null"]},"senderId":{"type":["string","null"]},"createdAt":{"type":["string","null"]},"read":{"type":"boolean"},"recipients":{"type":"array","items":{"type":"object"}}}}},"required":["message"]}))
        ,
        Tool::new(
            r####"ntulearn_list_course_users"####.to_string(),
            r####"List the users enrolled in a course (instructors, teaching staff and students as the API exposes them). Supports pagination."####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"limit":{"type":"integer","description":"Max items to return per call (1–200, default 50). Use small values to keep results within the LLM's context.","minimum":1,"maximum":200,"default":50},"offset":{"type":"integer","description":"Number of items to skip for pagination. Use the nextOffset value returned by a previous call to walk pages.","minimum":0,"default":0},"response_format":{"type":"string","description":"'json' returns a structured payload (default, recommended for agents). 'markdown' returns a human-readable summary.","enum":["json","markdown"],"default":"json"}},"required":["course_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"List course users"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
        .with_output_schema(json!({"type":"object","properties":{"users":{"type":"array","items":{"type":"object","properties":{"id":{"type":["string","null"]},"name":{"type":["string","null"]},"role":{"type":["string","null"]},"userName":{"type":["string","null"]}}}},"total":{"type":"integer"},"count":{"type":"integer"},"offset":{"type":"integer"},"limit":{"type":"integer"},"hasMore":{"type":"boolean"},"nextOffset":{"type":["integer","null"]}},"required":["users","total","count","offset","limit","hasMore"]}))
        ,
        Tool::new(
            r####"ntulearn_list_course_groups"####.to_string(),
            r####"List the groups defined in a course (e.g. tutorial/lab groups). Supports pagination."####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"limit":{"type":"integer","description":"Max items to return per call (1–200, default 50). Use small values to keep results within the LLM's context.","minimum":1,"maximum":200,"default":50},"offset":{"type":"integer","description":"Number of items to skip for pagination. Use the nextOffset value returned by a previous call to walk pages.","minimum":0,"default":0},"response_format":{"type":"string","description":"'json' returns a structured payload (default, recommended for agents). 'markdown' returns a human-readable summary.","enum":["json","markdown"],"default":"json"}},"required":["course_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"List course groups"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
        .with_output_schema(json!({"type":"object","properties":{"groups":{"type":"array","items":{"type":"object","properties":{"id":{"type":["string","null"]},"name":{"type":["string","null"]},"description":{"type":["string","null"]},"available":{"type":["string","null"]}}}},"total":{"type":"integer"},"count":{"type":"integer"},"offset":{"type":"integer"},"limit":{"type":"integer"},"hasMore":{"type":"boolean"},"nextOffset":{"type":["integer","null"]}},"required":["groups","total","count","offset","limit","hasMore"]}))
        ,
        Tool::new(
            r####"ntulearn_get_group_members"####.to_string(),
            r####"List the members of a specific course group (e.g. your tutorial group's roster)."####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"group_id":{"type":"string","description":"Group ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"response_format":{"type":"string","description":"'json' returns a structured payload (default, recommended for agents). 'markdown' returns a human-readable summary.","enum":["json","markdown"],"default":"json"}},"required":["course_id","group_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Get course group members"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
        .with_output_schema(json!({"type":"object","properties":{"users":{"type":"array","items":{"type":"object","properties":{"id":{"type":["string","null"]},"name":{"type":["string","null"]},"role":{"type":["string","null"]},"userName":{"type":["string","null"]}}}},"courseId":{"type":"string"},"groupId":{"type":"string"}},"required":["users"]}))
        ,
        Tool::new(
            r####"ntulearn_get_gradebook_attempts"####.to_string(),
            r####"List submission attempts for a gradebook column (assignment). Pass user_id to scope to one student's attempts. Supports pagination."####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"column_id":{"type":"string","description":"Gradebook column ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"user_id":{"type":"string","description":"Optional user ID to scope attempts to one student.","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"limit":{"type":"integer","description":"Max items to return per call (1–200, default 50). Use small values to keep results within the LLM's context.","minimum":1,"maximum":200,"default":50},"offset":{"type":"integer","description":"Number of items to skip for pagination. Use the nextOffset value returned by a previous call to walk pages.","minimum":0,"default":0},"response_format":{"type":"string","description":"'json' returns a structured payload (default, recommended for agents). 'markdown' returns a human-readable summary.","enum":["json","markdown"],"default":"json"}},"required":["course_id","column_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Get gradebook attempts"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
        .with_output_schema(json!({"type":"object","properties":{"attempts":{"type":"array","items":{"type":"object","properties":{"id":{"type":["string","null"]},"userId":{"type":["string","null"]},"userName":{"type":["string","null"]},"status":{"type":["string","null"]},"score":{"type":["number","string","null"]},"feedback":{"type":["string","null"]},"cumulatedScore":{"type":["number","string","null"]},"createdAt":{"type":["string","null"]}}}},"total":{"type":"integer"},"count":{"type":"integer"},"offset":{"type":"integer"},"limit":{"type":"integer"},"hasMore":{"type":"boolean"},"nextOffset":{"type":["integer","null"]}},"required":["attempts","total","count","offset","limit","hasMore"]}))
        ,
        Tool::new(
            r####"ntulearn_search_all_courses"####.to_string(),
            r####"Search across ALL enrolled courses' content trees for items matching a query. Like ntulearn_search_course_content but scoped to every available course at once, with per-item courseId and breadcrumb for attribution."####.to_string(),
            json!({"type":"object","properties":{"query":{"type":"string","description":"Search term (case-insensitive substring)","minLength":1,"maxLength":200},"course_ids":{"type":"array","description":"Optional list of course IDs to scope to. Omit to fan out across all available enrolled courses.","items":{"type":"string","pattern":"^[A-Za-z0-9_\\-:]+$"},"maxItems":200},"max_depth":{"type":"integer","description":"Maximum recursion depth per course (default 3, capped at 10).","default":3,"minimum":1,"maximum":10},"max_results":{"type":"integer","description":"Maximum number of matching items to return (default 50, capped at 200)","default":50,"minimum":1,"maximum":200},"response_format":{"type":"string","description":"'json' returns a structured payload (default, recommended for agents). 'markdown' returns a human-readable summary.","enum":["json","markdown"],"default":"json"}},"required":["query"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Search content across all courses"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
        .with_output_schema(json!({"type":"object","properties":{"matches":{"type":"array","items":{"type":"object","properties":{"id":{"type":["string","null"]},"title":{"type":["string","null"]},"contentHandlerId":{"type":["string","null"]},"hasChildren":{"type":"boolean"},"description":{"type":["string","null"]},"modified":{"type":["string","null"]},"courseId":{"type":["string","null"]},"breadcrumb":{"type":"array","items":{"type":"string"}}}}},"count":{"type":"integer"},"courseIdsQueried":{"type":"array","items":{"type":"string"}},"courseErrors":{"type":"object","additionalProperties":{"type":"string"}}},"required":["matches","count"]}))
        ,
        Tool::new(
            r####"ntulearn_get_content_tree"####.to_string(),
            r####"Return a course's ENTIRE content tree as nested JSON (folders with children), unlike ntulearn_get_course_contents which returns one level per call. Use max_depth to bound the walk."####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"max_depth":{"type":"integer","description":"Maximum recursion depth (capped at 10).","default":5,"minimum":1,"maximum":10},"response_format":{"type":"string","description":"'json' returns a structured payload (default, recommended for agents). 'markdown' returns a human-readable summary.","enum":["json","markdown"],"default":"json"}},"required":["course_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Get full course content tree"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
        .with_output_schema(json!({"type":"object","properties":{"courseId":{"type":"string"},"tree":{"type":"object","properties":{"id":{"type":["string","null"]},"title":{"type":["string","null"]},"hasChildren":{"type":"boolean"},"children":{"type":"array","items":{"type":"object"}}}},"count":{"type":"integer"},"totalNodes":{"type":"integer"}},"required":["courseId","tree","count","totalNodes"]}))
        ,
        Tool::new(
            r####"ntulearn_download_course"####.to_string(),
            r####"Recursively download every file in a course's content tree to local disk, organised into course/subject folders. Skips already-downloaded files by default; pass skip_existing=false to re-download. Concurrency is bounded by `parallel`. Write tool (saves files on your machine)."####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"destination_dir":{"type":"string","description":"Target root directory (absolute or ~-prefixed). Defaults to ~/Downloads/NTU/<course>.","minLength":1,"maxLength":1024},"max_depth":{"type":"integer","description":"Maximum recursion depth (capped at 10).","default":5,"minimum":1,"maximum":10},"include_extensions":{"type":"string","description":"Optional comma-separated file extensions to download, e.g. 'pdf,ppt,pptx,docx'. Omit to download all."},"skip_existing":{"type":"boolean","description":"Skip files already present at the destination.","default":true},"parallel":{"type":"integer","description":"Max concurrent downloads (1-16).","default":4,"minimum":1,"maximum":16},"response_format":{"type":"string","description":"'json' returns a structured payload (default, recommended for agents). 'markdown' returns a human-readable summary.","enum":["json","markdown"],"default":"json"}},"required":["course_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Download an entire course to disk"####.to_string()),
            read_only_hint: Some(false),
            destructive_hint: Some(false),
            idempotent_hint: Some(false),
            open_world_hint: Some(true),
        })
        .with_output_schema(json!({"type":"object","properties":{"courseId":{"type":"string"},"files":{"type":"array","items":{"type":"object","properties":{"localPath":{"type":"string"},"filename":{"type":"string"},"sizeBytes":{"type":"integer"},"courseFolder":{"type":"string"}}}},"skipped":{"type":"array","items":{"type":"object"}},"totalBytes":{"type":"integer"},"destinationDir":{"type":"string"}},"required":["courseId","files","totalBytes"]}))
        ,
        Tool::new(
            r####"ntulearn_whats_new"####.to_string(),
            r####"One-call 'what changed recently' digest: announcements, upcoming due dates and a gradebook summary across your courses since a cutoff. Defaults to the tracker's last-seen time (or the last 7 days). Set update_tracker=true to record `since` as the new last-seen for future calls."####.to_string(),
            json!({"type":"object","properties":{"course_ids":{"type":"array","description":"Optional list of course IDs to scope to. Omit to fan out across all available enrolled courses.","items":{"type":"string","pattern":"^[A-Za-z0-9_\\-:]+$"},"maxItems":200},"since":{"type":"string","description":"Optional ISO-8601 cutoff. Defaults to the tracker's last-seen time, or 7 days ago if never set."},"update_tracker":{"type":"boolean","description":"Persist `since` as the new last-seen marker for future ntulearn_whats_new calls.","default":false},"response_format":{"type":"string","description":"'json' returns a structured payload (default, recommended for agents). 'markdown' returns a human-readable summary.","enum":["json","markdown"],"default":"json"}},"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"What's new across courses"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
        .with_output_schema(json!({"type":"object","properties":{"since":{"type":"string"},"fetchedAt":{"type":"string"},"announcements":{"type":"array","items":{"type":"object","properties":{"id":{"type":["string","null"]},"courseId":{"type":["string","null"]},"title":{"type":["string","null"]},"body":{"type":["string","null"]},"created":{"type":["string","null"]},"modified":{"type":["string","null"]},"available":{"type":["string","null"]}}}},"upcoming":{"type":"array","items":{"type":"object","properties":{"id":{"type":["string","null"]},"type":{"type":["string","null"]},"title":{"type":["string","null"]},"description":{"type":["string","null"]},"location":{"type":["string","null"]},"start":{"type":["string","null"]},"end":{"type":["string","null"]},"calendarName":{"type":["string","null"]},"courseId":{"type":["string","null"]},"eventType":{"type":["string","null"]},"gradable":{"type":["boolean","null"]},"attemptable":{"type":["boolean","null"]}}}},"gradebookSummary":{"type":["object","null"],"properties":{"columnCount":{"type":"integer"},"columnsWithScore":{"type":"integer"},"possibleTotal":{"type":["number","null"]}}},"courseErrors":{"type":"object","additionalProperties":{"type":"string"}}},"required":["since","fetchedAt","announcements","upcoming"]}))
        ,
        Tool::new(
            r####"ntulearn_export_calendar_ics"####.to_string(),
            r####"Export calendar items (including assignment due dates) as an iCalendar (.ics) string you can paste into Google Calendar / Apple Calendar / Outlook. Optionally scope to course_ids and a since/until window."####.to_string(),
            json!({"type":"object","properties":{"course_ids":{"type":"array","description":"Optional list of course IDs to scope to. Omit to fan out across all available enrolled courses.","items":{"type":"string","pattern":"^[A-Za-z0-9_\\-:]+$"},"maxItems":200},"since":{"type":"string","description":"Optional ISO-8601 cutoff (e.g. '2026-05-09T00:00:00Z'). Only items created on/after this time are included."},"until":{"type":"string","description":"Optional ISO-8601 end of the window (e.g. '2026-05-23T00:00:00Z')."},"response_format":{"type":"string","description":"'json' returns a structured payload (default, recommended for agents). 'markdown' returns a human-readable summary.","enum":["json","markdown"],"default":"json"}},"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Export calendar as ICS"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
        .with_output_schema(json!({"type":"object","properties":{"ics":{"type":"string"},"itemCount":{"type":"integer"},"supported":{"type":"boolean"}},"required":["ics","itemCount","supported"]}))
        ,
        Tool::new(
            r####"ntulearn_export_gradebook_csv"####.to_string(),
            r####"Export your gradebook columns and scores across courses as a CSV string you can paste into a spreadsheet. Header: courseId,column,possible,score,grade,status."####.to_string(),
            json!({"type":"object","properties":{"course_ids":{"type":"array","description":"Optional list of course IDs to scope to. Omit to fan out across all available enrolled courses.","items":{"type":"string","pattern":"^[A-Za-z0-9_\\-:]+$"},"maxItems":200},"response_format":{"type":"string","description":"'json' returns a structured payload (default, recommended for agents). 'markdown' returns a human-readable summary.","enum":["json","markdown"],"default":"json"}},"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Export gradebook as CSV"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
        .with_output_schema(json!({"type":"object","properties":{"csv":{"type":"string"},"columnCount":{"type":"integer"},"courseCount":{"type":"integer"}},"required":["csv","columnCount","courseCount"]}))
        ,
        Tool::new(
            r####"ntulearn_summarize_course"####.to_string(),
            r####"Bite-size briefing of a single course: title, description, instructors, enrollment count, upcoming due dates, recent announcements, a gradebook summary and the top-level content folders. Each sub-section degrades gracefully on error."####.to_string(),
            json!({"type":"object","properties":{"course_id":{"type":"string","description":"Blackboard course ID (e.g. _12345_1)","minLength":1,"maxLength":200,"pattern":"^[A-Za-z0-9_\\-:]+$"},"include_contents":{"type":"boolean","description":"Include the top-level content folders in the briefing.","default":true},"response_format":{"type":"string","description":"'json' returns a structured payload (default, recommended for agents). 'markdown' returns a human-readable summary.","enum":["json","markdown"],"default":"json"}},"required":["course_id"],"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Summarize a course"####.to_string()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
        .with_output_schema(json!({"type":"object","properties":{"courseId":{"type":"string"},"title":{"type":["string","null"]},"description":{"type":["string","null"]},"instructors":{"type":"array","items":{"type":"object"}},"enrollmentCount":{"type":["integer","null"]},"upcoming":{"type":"array","items":{"type":"object","properties":{"id":{"type":["string","null"]},"type":{"type":["string","null"]},"title":{"type":["string","null"]},"description":{"type":["string","null"]},"location":{"type":["string","null"]},"start":{"type":["string","null"]},"end":{"type":["string","null"]},"calendarName":{"type":["string","null"]},"courseId":{"type":["string","null"]},"eventType":{"type":["string","null"]},"gradable":{"type":["boolean","null"]},"attemptable":{"type":["boolean","null"]}}}},"recentAnnouncements":{"type":"array","items":{"type":"object","properties":{"id":{"type":["string","null"]},"courseId":{"type":["string","null"]},"title":{"type":["string","null"]},"body":{"type":["string","null"]},"created":{"type":["string","null"]},"modified":{"type":["string","null"]},"available":{"type":["string","null"]}}}},"gradeSummary":{"type":["object","null"],"properties":{"columnCount":{"type":"integer"},"columnsWithScore":{"type":"integer"},"possibleTotal":{"type":["number","null"]}}},"contentTopFolders":{"type":"array","items":{"type":"object","properties":{"id":{"type":["string","null"]},"title":{"type":["string","null"]},"contentHandlerId":{"type":["string","null"]},"hasChildren":{"type":"boolean"},"description":{"type":["string","null"]},"modified":{"type":["string","null"]}}}}},"required":["courseId"]}))
        ,
    ]
}
