//! Output formatting — ports of the Python `render.py` `md_*` builders
//! plus the `_md_*` builders from `server.py` used by the 8 server-local
//! tools. Each builder takes the payload the handler returned and returns
//! copy-paste-friendly markdown, exactly like the Python originals.

use serde_json::Value;

/// Python `str(value or "—")` helper.
fn _dt(value: Option<&Value>) -> String {
    match value {
        Some(v) if !v.is_null() => {
            if let Some(s) = v.as_str() {
                if s.is_empty() { "—".to_string() } else { s.to_string() }
            } else if let Some(n) = v.as_u64() {
                if n == 0 { "—".to_string() } else { n.to_string() }
            } else if let Some(b) = v.as_bool() {
                if b { "True".to_string() } else { "—".to_string() }
            } else if let Some(f) = v.as_f64() {
                if f == 0.0 { "—".to_string() } else { format!("{}", f) }
            } else {
                serde_json::to_string(v).unwrap_or_else(|_| "—".to_string())
            }
        }
        _ => "—".to_string(),
    }
}

/// round(100.0 * count / total)% or "—" when total is zero.
fn _pct(count: usize, total: usize) -> String {
    if total > 0 {
        format!("{}%", (100.0 * count as f64 / total as f64).round() as i64)
    } else {
        "—".to_string()
    }
}

fn _gs<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(|x| x.as_str())
}


/// render.py `md_messages`.
pub fn md_messages(payload: &Value) -> String {
    let folder = _gs(payload, "folder").unwrap_or("inbox");
    let offset = payload.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let total = payload.get("total").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let msgs = payload.get("messages").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let hi = offset + msgs.len();
    let mut lines = vec![format!("# Messages — {folder} ({offset}-{hi} of {total})")];
    for m in &msgs {
        let flag = if m.get("read").and_then(|v| v.as_bool()) == Some(true) { "🟡" } else { "🔴" };
        let subject = m.get("subject").and_then(|v| v.as_str()).filter(|s| !s.is_empty())
            .unwrap_or("(no subject)");
        let from = m.get("fromUserId").and_then(|v| v.as_str()).unwrap_or("?");
        let created = _dt(m.get("created"));
        let id = m.get("id").and_then(|v| v.as_str()).unwrap_or("?");
        lines.push(format!("- {flag} **{subject}**"));
        lines.push(format!("  - from {from} · {created} · id={id}"));
    }
    if msgs.is_empty() {
        lines.push("_No messages._".to_string());
    }
    lines.join("\n")
}

/// render.py `md_message`.
pub fn md_message(payload: &Value) -> String {
    let subject = _gs(payload, "subject").filter(|s| !s.is_empty()).unwrap_or("(no subject)");
    let sender = payload
        .get("senderName")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| payload.get("senderId").and_then(|v| v.as_str()))
        .unwrap_or("?");
    let created = _dt(payload.get("created"));
    let read = payload.get("read").and_then(|v| v.as_bool()).unwrap_or(false);
    let mut lines = vec![
        format!("# {subject}"),
        format!("- from: {sender}"),
        format!("- created: {created} · read: {read}"),
    ];
    let recips = payload.get("recipients").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    if !recips.is_empty() {
        let names = recips
            .iter()
            .map(|r| {
                let n = r.get("name").and_then(|v| v.as_str()).filter(|s| !s.is_empty())
                    .or_else(|| r.get("id").and_then(|v| v.as_str()))
                    .unwrap_or("?");
                let role = r.get("role").and_then(|v| v.as_str()).unwrap_or("?");
                format!("{n} ({role})")
            })
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("- recipients: {names}"));
    }
    let body = payload.get("body").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    if !body.is_empty() {
        lines.push(String::new());
        lines.push("---".to_string());
        lines.push(body);
    }
    lines.join("\n")
}

/// render.py `md_course_users`.
pub fn md_course_users(payload: &Value) -> String {
    let course_id = _gs(payload, "courseId").unwrap_or("?");
    let offset = payload.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let total = payload.get("total").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let users = payload.get("users").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let hi = offset + users.len();
    let mut lines = vec![format!("# Course roster — {course_id} ({total} users, showing {offset}-{hi})")];
    for u in &users {
        let name = u.get("name").and_then(|v| v.as_str()).filter(|s| !s.is_empty())
            .or_else(|| u.get("userName").and_then(|v| v.as_str()))
            .or_else(|| u.get("id").and_then(|v| v.as_str()))
            .unwrap_or("?");
        let role = u.get("role").and_then(|v| v.as_str()).unwrap_or("?");
        let uname = u.get("userName").and_then(|v| v.as_str()).unwrap_or("");
        lines.push(format!("- **{name}** — {role} ({uname})"));
    }
    if users.is_empty() {
        lines.push("_No users._".to_string());
    }
    lines.join("\n")
}

/// render.py `md_course_groups`.
pub fn md_course_groups(payload: &Value) -> String {
    let course_id = _gs(payload, "courseId").unwrap_or("?");
    let total = payload.get("total").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let groups = payload.get("groups").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let mut lines = vec![format!("# Groups — {course_id} ({total})")];
    for g in &groups {
        let name = g.get("name").and_then(|v| v.as_str()).filter(|s| !s.is_empty())
            .or_else(|| g.get("id").and_then(|v| v.as_str()))
            .unwrap_or("?");
        let avail = if g.get("available").and_then(|v| v.as_bool()) == Some(true) { "✓" } else { "✗" };
        let id = g.get("id").and_then(|v| v.as_str()).unwrap_or("?");
        let count = match g.get("memberCount") {
            Some(Value::Number(n)) => format!(" · {n} members"),
            _ => String::new(),
        };
        lines.push(format!("- {avail} **{name}**{count} — id={id}"));
        if let Some(desc) = g.get("description").and_then(|v| v.as_str()).filter(|d| !d.is_empty()) {
            let short: String = desc.chars().take(160).collect();
            lines.push(format!("  - {short}"));
        }
    }
    if groups.is_empty() {
        lines.push("_No groups._".to_string());
    }
    lines.join("\n")
}

/// render.py `md_group_members`.
pub fn md_group_members(payload: &Value) -> String {
    let group_id = _gs(payload, "groupId").unwrap_or("?");
    let course_id = _gs(payload, "courseId").unwrap_or("?");
    let total = payload.get("total").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let users = payload.get("users").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let mut lines = vec![format!("# Group {group_id} — {course_id} ({total} members)")];
    for u in &users {
        let name = u.get("name").and_then(|v| v.as_str()).filter(|s| !s.is_empty())
            .or_else(|| u.get("userName").and_then(|v| v.as_str()))
            .or_else(|| u.get("id").and_then(|v| v.as_str()))
            .unwrap_or("?");
        let role = u.get("role").and_then(|v| v.as_str()).unwrap_or("?");
        lines.push(format!("- **{name}** — {role}"));
    }
    if users.is_empty() {
        lines.push("_No members._".to_string());
    }
    lines.join("\n")
}


/// render.py `md_gradebook_attempts`.
pub fn md_gradebook_attempts(payload: &Value) -> String {
    let course_id = _gs(payload, "courseId").unwrap_or("?");
    let column_id = _gs(payload, "columnId").unwrap_or("?");
    let total = payload.get("total").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let attempts = payload.get("attempts").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let mut lines = vec![format!("# Attempts — {course_id} / {column_id} ({total})")];
    for a in &attempts {
        let status = a.get("status").and_then(|v| v.as_str()).filter(|s| !s.is_empty())
            .unwrap_or("attempt");
        let score = a.get("score");
        let score_txt = match score {
            Some(Value::Number(n)) => format!("{n}/? "),
            _ => String::new(),
        };
        let user = a.get("userId").and_then(|v| v.as_str()).unwrap_or("?");
        let id = a.get("id").and_then(|v| v.as_str()).unwrap_or("?");
        let created = _dt(a.get("created"));
        lines.push(format!("- **{status}** {score_txt}— user {user} · id={id} · {created}"));
        if let Some(fb) = a.get("feedback") {
            if !(fb.is_null() || (fb.as_str().map(|s| s.is_empty()).unwrap_or(false))) {
                let s = serde_json::to_string(fb).unwrap_or_default();
                let short: String = s.chars().take(200).collect();
                lines.push(format!("    - feedback: {short}"));
            }
        }
    }
    if attempts.is_empty() {
        lines.push("_No attempts._".to_string());
    }
    lines.join("\n")
}

fn _fmt_match(match_: &Value) -> String {
    let title = match_.get("title").and_then(|v| v.as_str()).filter(|s| !s.is_empty())
        .or_else(|| match_.get("id").and_then(|v| v.as_str()))
        .unwrap_or("?");
    let kind = match_.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    let course_id = match_.get("courseId").and_then(|v| v.as_str()).unwrap_or("");
    let crumb = match_.get("breadcrumb").and_then(|v| v.as_array()).cloned().unwrap_or_default()
        .iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join("/");
    format!("- **{title}** [{kind} · {course_id}] — `{crumb}`")
}

/// render.py `md_search_all_courses`.
pub fn md_search_all_courses(payload: &Value) -> String {
    let query = _gs(payload, "query").unwrap_or("");
    let courses = payload.get("coursesSearched").and_then(|v| v.as_u64()).unwrap_or(0);
    let max_results = payload.get("maxResults").and_then(|v| v.as_u64()).unwrap_or(0);
    let mut lines = vec![format!("# Search “{query}” — {courses} course(s), {max_results} result(s)")];
    let matches = payload.get("matches").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    for m in &matches {
        lines.push(_fmt_match(m));
    }
    let errors = payload.get("courseErrors").and_then(|v| v.as_object()).map(|o| o.len()).unwrap_or(0);
    if errors > 0 {
        lines.push(String::new());
        lines.push(format!("_⚠️ {errors} course(s) could not be searched._"));
    }
    if matches.is_empty() {
        lines.push("_No matches._".to_string());
    }
    lines.join("\n")
}

fn _render_tree_node(node: &Value, indent: usize) -> Vec<String> {
    let kind = node.get("kind").and_then(|v| v.as_str()).unwrap_or("file");
    let icon = if kind == "folder" { "📁" } else { "📄" };
    let title = node.get("title").and_then(|v| v.as_str()).filter(|s| !s.is_empty())
        .or_else(|| node.get("id").and_then(|v| v.as_str()))
        .unwrap_or("?");
    let pad = "  ".repeat(indent);
    let mut out = vec![format!("{pad}{icon} {title}")];
    if let Some(children) = node.get("children").and_then(|v| v.as_array()) {
        for ch in children {
            out.extend(_render_tree_node(ch, indent + 1));
        }
    }
    out
}

/// render.py `md_content_tree`.
pub fn md_content_tree(payload: &Value) -> String {
    let course_id = _gs(payload, "courseId").unwrap_or("?");
    let count = payload.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
    let total_nodes = payload.get("totalNodes").and_then(|v| v.as_u64()).unwrap_or(0);
    let mut lines = vec![format!("# Content tree — {course_id} ({count} top-level, {total_nodes} nodes)")];
    let tree = payload.get("tree").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    for node in &tree {
        lines.extend(_render_tree_node(node, 0));
    }
    if tree.is_empty() {
        lines.push("_No content._".to_string());
    }
    lines.join("\n")
}

/// render.py `md_download_course`.
pub fn md_download_course(payload: &Value) -> String {
    let name = payload.get("courseName").and_then(|v| v.as_str()).unwrap_or("?");
    let download_count = payload.get("downloadCount").and_then(|v| v.as_u64()).unwrap_or(0);
    let total_bytes = payload.get("totalBytes").and_then(|v| v.as_u64()).unwrap_or(0);
    let skipped_count = payload.get("skippedCount").and_then(|v| v.as_u64()).unwrap_or(0);
    let dest = payload.get("destinationDir").and_then(|v| v.as_str()).unwrap_or("?");
    let mut lines = vec![
        format!("# Downloaded {name}"),
        format!("- files saved: **{download_count}** ({total_bytes} B)"),
        format!("- skipped: {skipped_count} · destination: `{dest}`"),
    ];
    let mut folders: Vec<(String, usize)> = Vec::new();
    if let Some(files) = payload.get("files").and_then(|v| v.as_array()) {
        for f in files {
            if let Some(cf) = f.get("courseFolder").and_then(|v| v.as_str()) {
                if let Some(e) = folders.iter_mut().find(|(k, _)| k == cf) {
                    e.1 += 1;
                } else {
                    folders.push((cf.to_string(), 1));
                }
            }
        }
    }
    if !folders.is_empty() {
        lines.push(String::new());
        folders.sort();
        for (folder, n) in folders {
            lines.push(format!("- `{folder}` — {n} file(s)"));
        }
    }
    lines.join("\n")
}

/// render.py `md_whats_new`.
pub fn md_whats_new(payload: &Value) -> String {
    let since = _dt(payload.get("since"));
    let mut lines = vec![format!("# What's new since {since}")];
    let summary = payload.get("summary").cloned().unwrap_or_else(|| serde_json::json!({}));
    let ann = summary.get("announcements").and_then(|v| v.as_u64()).unwrap_or(0);
    let upc = summary.get("upcoming").and_then(|v| v.as_u64()).unwrap_or(0);
    let nf = summary.get("newFiles").and_then(|v| v.as_u64()).unwrap_or(0);
    lines.push(format!("- announcements: **{ann}** · upcoming: **{upc}** · new files: **{nf}**"));
    let courses = payload.get("courses").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    for entry in &courses {
        let name = entry.get("courseName").and_then(|v| v.as_str()).filter(|s| !s.is_empty())
            .unwrap_or_else(|| entry.get("courseId").and_then(|v| v.as_str()).unwrap_or("?"));
        let mut labels: Vec<String> = Vec::new();
        for kind in ["announcements", "upcoming", "newFiles"] {
            let n = entry.get(kind).and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
            if n > 0 {
                labels.push(format!("{kind}: {n}"));
            }
        }
        if !labels.is_empty() {
            lines.push(format!("- **{name}** — {}", labels.join(", ")));
            let anns = entry.get("announcements").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            for a in anns.iter().take(3) {
                let t = a.get("title").and_then(|v| v.as_str()).unwrap_or("?");
                lines.push(format!("    - 📢 {t} ({})", _dt(a.get("created"))));
            }
            let upcs = entry.get("upcoming").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            for u in upcs.iter().take(3) {
                let t = u.get("title").and_then(|v| v.as_str()).unwrap_or("?");
                lines.push(format!("    - 📅 {t} ({})", _dt(u.get("start"))));
            }
            let files = entry.get("newFiles").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            for f in files.iter().take(3) {
                let t = f.get("title").and_then(|v| v.as_str()).unwrap_or("?");
                lines.push(format!("    - 📄 {t} ({})", _dt(f.get("modified"))));
            }
        }
    }
    lines.join("\n")
}

/// render.py `md_export_calendar`.
pub fn md_export_calendar(payload: &Value) -> String {
    let item_count = payload.get("itemCount").and_then(|v| v.as_u64()).unwrap_or(0);
    let course_count = payload.get("courseCount").and_then(|v| v.as_u64()).unwrap_or(0);
    let since = _dt(payload.get("since"));
    let until = _dt(payload.get("until"));
    let lines = vec![
        format!("# Calendar export (.ics) — {item_count} events from {course_count} course(s)"),
        format!("- window: {since} → {until}"),
        "- you can import the ICS payload directly into most calendar apps.".to_string(),
    ];
    lines.join("\n")
}

/// render.py `md_export_gradebook`.
pub fn md_export_gradebook(payload: &Value) -> String {
    let row_count = payload.get("rowCount").and_then(|v| v.as_u64()).unwrap_or(0);
    let course_count = payload.get("courseCount").and_then(|v| v.as_u64()).unwrap_or(0);
    let lines = vec![
        format!("# Gradebook export (.csv) — {row_count} rows across {course_count} course(s)"),
        "You can paste the CSV payload into a file and open it in Sheets/Excel.".to_string(),
    ];
    lines.join("\n")
}

/// render.py `md_summarize_course`.
pub fn md_summarize_course(payload: &Value) -> String {
    let course_id = _gs(payload, "courseId").unwrap_or("?");
    let title = _gs(payload, "title").unwrap_or(course_id);
    let errors = payload.get("courseErrors").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let mut lines = vec![format!("# {title}")];
    if let Some(desc) = payload.get("description").and_then(|v| v.as_str()).filter(|d| !d.is_empty()) {
        lines.push(String::new());
        let short: String = desc.chars().take(300).collect();
        lines.push(short);
    }
    lines.push(String::new());
    let term = payload.get("term").cloned().unwrap_or_else(|| serde_json::json!({}));
    if let Some(term_name) = term.get("name").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        let ts = _dt(term.get("start"));
        let te = _dt(term.get("end"));
        lines.push(format!("- term: **{term_name}** ({ts} → {te})"));
    }
    let instructors = payload.get("instructors").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    if !instructors.is_empty() {
        let names = instructors.iter().take(5)
            .map(|i| i.get("name").and_then(|v| v.as_str()).filter(|s| !s.is_empty())
                .or_else(|| i.get("id").and_then(|v| v.as_str())).unwrap_or("?"))
            .collect::<Vec<_>>().join(", ");
        lines.push(format!("- instructors: {names}"));
    }
    if let Some(enr) = payload.get("enrollmentCount") {
        if let Some(n) = enr.as_u64() {
            lines.push(format!("- enrolled: {n} user(s)"));
        }
    }
    let tops = payload.get("contentTopFolders").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    if !tops.is_empty() {
        lines.push(String::new());
        lines.push("Top-level folders:".to_string());
        for f in tops.iter().take(10) {
            let icon = if f.get("hasChildren").and_then(|v| v.as_bool()) == Some(true) { "📁" } else { "📄" };
            let t = f.get("title").and_then(|v| v.as_str()).filter(|s| !s.is_empty())
                .or_else(|| f.get("id").and_then(|v| v.as_str())).unwrap_or("?");
            lines.push(format!("  - {icon} {t}"));
        }
    }
    let grades = payload.get("gradeSummary").cloned().unwrap_or_else(|| serde_json::json!({}));
    if let Some(cc) = grades.get("columnCount").and_then(|v| v.as_u64()) {
        if cc > 0 {
            let with = grades.get("columnsWithScore").and_then(|v| v.as_u64()).unwrap_or(0);
            let mut avg_txt = String::new();
            if let Some(avg) = grades.get("averagePercent") {
                if let Some(f) = avg.as_f64() {
                    avg_txt = format!(" · avg {f}%");
                }
            }
            lines.push(format!("- gradebook: {cc} column(s), {with} graded{avg_txt}"));
        }
    }
    let upcoming = payload.get("upcoming").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    if !upcoming.is_empty() {
        lines.push(String::new());
        lines.push("Upcoming:".to_string());
        for u in upcoming.iter().take(5) {
            let t = u.get("title").and_then(|v| v.as_str()).unwrap_or("?");
            lines.push(format!("  - {} · {t}", _dt(u.get("start"))));
        }
    }
    if !errors.is_empty() {
        let sections = errors.iter().take(4)
            .map(|e| e.get("section").and_then(|v| v.as_str()).unwrap_or("?").to_string())
            .collect::<Vec<_>>().join(", ");
        lines.push(String::new());
        lines.push(format!("_⚠️ {} section(s) unavailable: {sections}_", errors.len()));
    }
    lines.join("\n")
}


// ===========================================================================
// server.py `_md_*` builders (used by the 8 server-local tools)
// ===========================================================================

/// Trailing pagination line (server `_md_pagination_footer`).
pub fn md_pagination_footer(meta: &Value) -> String {
    let has_more = meta.get("hasMore").and_then(|v| v.as_bool()).unwrap_or(false);
    let count = meta.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
    let total = meta.get("total").and_then(|v| v.as_u64()).unwrap_or(0);
    if has_more {
        let offset = meta.get("offset").and_then(|v| v.as_u64()).unwrap_or(0);
        let next = meta.get("nextOffset").and_then(|v| v.as_u64()).unwrap_or(offset);
        format!("\n_Showing {count} of {total} (offset {offset}). Pass offset={next} for the next page._")
    } else {
        format!("\n_Showing all {total}._")
    }
}

/// `# Courses (N total)` list body — header + items (no footer). The stub
/// signature (courses, total) is kept; callers wanting the pagination footer
/// should use [`md_courses_full`].
pub fn md_courses(courses: &[Value], total: usize) -> String {
    let mut lines = vec![format!("# Courses ({total} total)"), String::new()];
    if courses.is_empty() {
        lines.push("_No courses to show._".to_string());
    } else {
        for c in courses {
            let title = c.get("title").and_then(Value::as_str).unwrap_or("?");
            let cid = c.get("courseId").and_then(Value::as_str).unwrap_or("?");
            let available = c.get("available").and_then(Value::as_str).unwrap_or("?");
            let last = c.get("lastAccessed").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or("—");
            lines.push(format!("- **{title}** `{cid}` · available={available} · last accessed {last}"));
        }
    }
    lines.join("\n")
}

/// Full `_md_courses(courses, meta)` port including the footer.
pub fn md_courses_full(courses: &[Value], meta: &Value) -> String {
    let total = meta.get("total").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let mut body = md_courses(courses, total);
    body.push('\n');
    body.push_str(&md_pagination_footer(meta));
    body
}

/// `# Content items (N total)` list body — header + items (no footer).
pub fn md_content_items(items: &[Value], total: usize) -> String {
    let mut lines = vec![format!("# Content items ({total} total)"), String::new()];
    if items.is_empty() {
        lines.push("_No items._".to_string());
    } else {
        for it in items {
            let arrow = if it.get("hasChildren").and_then(Value::as_bool) == Some(true) { "📁" } else { "📄" };
            let title = it.get("title").and_then(Value::as_str).unwrap_or("?");
            let id = it.get("id").and_then(Value::as_str).unwrap_or("?");
            let handler = it.get("contentHandlerId").and_then(Value::as_str).unwrap_or("?");
            lines.push(format!("- {arrow} **{title}** `{id}` · handler={handler}"));
        }
    }
    lines.join("\n")
}

/// Full `_md_content_items(items, meta)` port including the footer.
pub fn md_content_items_full(items: &[Value], meta: &Value) -> String {
    let total = meta.get("total").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let mut body = md_content_items(items, total);
    body.push('\n');
    body.push_str(&md_pagination_footer(meta));
    body
}

/// `_md_upcoming(items, meta, course_errors)`.
pub fn md_upcoming(items: &[Value], meta: &Value, course_errors: &Value) -> String {
    let total = meta.get("total").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let n_err = course_errors.as_object().map(|o| o.len()).unwrap_or(0);
    let mut lines = vec![format!("# Upcoming ({total} total)"), String::new()];
    if n_err > 0 {
        lines.push(format!("_Note: {n_err} course(s) returned errors and were skipped._"));
        lines.push(String::new());
    }
    if items.is_empty() {
        lines.push("_Nothing scheduled in the window._".to_string());
    } else {
        lines.push("| When | Title | Type | Course | Gradable |".to_string());
        lines.push("|---|---|---|---|---|".to_string());
        for it in items {
            let start = it.get("start").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or("—").to_string();
            let end = it.get("end").and_then(Value::as_str).filter(|s| !s.is_empty());
            let when = match end {
                Some(e) if e != start => format!("{start} → {e}"),
                _ => start,
            };
            let gradable = it.get("gradable");
            let gradable_str = match gradable {
                Some(Value::Bool(true)) => "Yes",
                Some(Value::Bool(false)) => "No",
                _ => "—",
            };
            lines.push(format!(
                "| {when} | {} | {} | {} | {gradable_str} |",
                it.get("title").and_then(Value::as_str).unwrap_or("?"),
                it.get("type").and_then(Value::as_str).unwrap_or("—"),
                it.get("courseId").and_then(Value::as_str).unwrap_or("—"),
            ));
        }
    }
    lines.push(md_pagination_footer(meta));
    lines.join("\n")
}

/// `_md_announcements(items, meta)`.
pub fn md_announcements(items: &[Value], meta: &Value) -> String {
    let total = meta.get("total").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let mut lines = vec![format!("# Announcements ({total} total)"), String::new()];
    if items.is_empty() {
        lines.push("_No announcements._".to_string());
    } else {
        for a in items {
            let created = a.get("created").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or("—");
            let title = a.get("title").and_then(Value::as_str).unwrap_or("?");
            let mut header = format!("## {title}  ·  {created}");
            if let Some(course_id) = a.get("courseId").and_then(Value::as_str) {
                header.push_str(&format!("  ·  `{course_id}`"));
            }
            lines.push(header);
            let body = a.get("body").and_then(Value::as_str).unwrap_or("").trim().to_string();
            lines.push(if body.is_empty() { "_(no body)_".to_string() } else { body });
            lines.push(String::new());
        }
    }
    lines.push(md_pagination_footer(meta));
    lines.join("\n")
}

/// `_md_gradebook(columns, meta, grades_available, error)`.
pub fn md_gradebook(columns: &[Value], meta: &Value, grades_available: bool, error: Option<&str>) -> String {
    let total = meta.get("total").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let mut lines = vec![format!("# Gradebook ({total} columns)"), String::new()];
    if !grades_available {
        lines.push(format!("_Grades not available: {}", error.unwrap_or("")));
        lines.push(String::new());
    }
    if columns.is_empty() {
        lines.push("_No columns._".to_string());
    } else {
        lines.push("| Course | Column | Possible | Score | Grade | Status |".to_string());
        lines.push("|---|---|---|---|---|---|".to_string());
        for c in columns {
            let course_id = c.get("courseId").and_then(Value::as_str).unwrap_or("—");
            let col = c.get("displayName").and_then(Value::as_str).filter(|s| !s.is_empty())
                .or_else(|| c.get("name").and_then(Value::as_str))
                .unwrap_or("?");
            let possible = dt_or_dash(c.get("possible"));
            let score = dt_or_dash(c.get("score"));
            let grade = dt_or_dash(c.get("grade"));
            let status = dt_or_dash(c.get("status"));
            lines.push(format!("| `{course_id}` | {col} | {possible} | {score} | {grade} | {status} |"));
        }
    }
    lines.push(md_pagination_footer(meta));
    lines.join("\n")
}

fn dt_or_dash(v: Option<&Value>) -> String {
    match v {
        Some(x) if !x.is_null() => {
            if let Some(s) = x.as_str() { if s.is_empty() { "—".to_string() } else { s.to_string() } }
            else { serde_json::to_string(x).unwrap_or_else(|_| "—".to_string()) }
        }
        _ => "—".to_string(),
    }
}

/// `_md_search_results(matches)`.
pub fn md_search_results(matches: &[Value]) -> String {
    let mut lines = vec![format!("# Search matches ({})", matches.len()), String::new()];
    if matches.is_empty() {
        lines.push("_No matches._".to_string());
    } else {
        for m in matches {
            let title = m.get("title").and_then(Value::as_str).unwrap_or("?");
            let id = m.get("id").and_then(Value::as_str).unwrap_or("?");
            let crumb = m.get("breadcrumb").and_then(|v| v.as_array()).cloned().unwrap_or_default()
                .iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(" › ");
            lines.push(format!("- **{title}** `{id}`  "));
            lines.push(format!("  _{crumb}_"));
        }
    }
    lines.join("\n")
}

/// `_md_files(payload, heading)`.
pub fn md_files(payload: &Value, heading: &str) -> String {
    let title = payload.get("title").and_then(Value::as_str).unwrap_or("?");
    let mut lines = vec![
        format!("# {heading}"),
        String::new(),
        format!("**Item:** {title}  "),
        String::new(),
    ];
    let files = payload.get("files").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let skipped = payload.get("skipped").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    if !files.is_empty() {
        lines.push("## Files".to_string());
        for f in &files {
            if let Some(local) = f.get("localPath").and_then(Value::as_str) {
                let filename = f.get("filename").and_then(Value::as_str).unwrap_or("?");
                let size = f.get("sizeBytes").and_then(|v| v.as_u64()).unwrap_or(0);
                lines.push(format!("- `{filename}` ({}) → `{local}`", crate::parsers::format_bytes(size)));
            } else if f.get("text").is_some() {
                let filename = f.get("filename").and_then(Value::as_str).unwrap_or("?");
                let kind = f.get("kind").and_then(Value::as_str).unwrap_or("?");
                let mut count_bits: Vec<String> = Vec::new();
                if let Some(pc) = f.get("pageCount").and_then(|v| v.as_u64()) {
                    if pc > 0 {
                        count_bits.push(format!("{pc} pages"));
                    }
                }
                if let Some(sc) = f.get("slideCount").and_then(|v| v.as_u64()) {
                    if sc > 0 {
                        count_bits.push(format!("{sc} slides"));
                    }
                }
                if let Some(sc) = f.get("sheetCount").and_then(|v| v.as_u64()) {
                    if sc > 0 {
                        count_bits.push(format!("{sc} sheets"));
                    }
                }
                let counts = if count_bits.is_empty() { String::new() } else { format!(" · {}", count_bits.join(", ")) };
                let size = f.get("sizeBytes").and_then(|v| v.as_u64()).unwrap_or(0);
                lines.push(format!("### {filename} ({kind}{counts}, {})", crate::parsers::format_bytes(size)));
                let text = f.get("text").and_then(Value::as_str).unwrap_or("").trim().to_string();
                if text.is_empty() {
                    lines.push("_(empty)_".to_string());
                } else if text.chars().count() > 5000 {
                    let truncated: String = text.chars().take(5000).collect();
                    lines.push(truncated + "\n…_(truncated in markdown view; use response_format='json' for full text)_");
                } else {
                    lines.push(text);
                }
            } else {
                let filename = f.get("filename").and_then(Value::as_str).unwrap_or("?");
                let url = f.get("url").and_then(Value::as_str).unwrap_or("?");
                lines.push(format!("- `{filename}` (url: {url})"));
            }
        }
        lines.push(String::new());
    }
    if !skipped.is_empty() {
        lines.push("## Skipped".to_string());
        for s in &skipped {
            let filename = s.get("filename").and_then(Value::as_str).unwrap_or("?");
            let reason = s.get("reason").and_then(Value::as_str).unwrap_or("");
            lines.push(format!("- `{filename}`: {reason}"));
        }
    }
    if files.is_empty() && skipped.is_empty() {
        lines.push("_(nothing to show)_".to_string());
    }
    lines.join("\n")
}

/// Generic `# {title} ({total} total)` list (kept for compatibility).
pub fn generic_list(title: &str, items: &[Value], total: usize) -> String {
    let mut lines = vec![format!("# {title} ({total} total)"), String::new()];
    if items.is_empty() {
        lines.push("_No items._".to_string());
    } else {
        for it in items {
            lines.push(serde_json::to_string_pretty(it).unwrap_or_default());
        }
    }
    lines.join("\n")
}

/// Human-readable byte size (server `_format_bytes`).
pub fn format_bytes(n: u64) -> String {
    crate::parsers::format_bytes(n)
}
