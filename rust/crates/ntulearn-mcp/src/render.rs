
//! Output formatting — port of the Python `_md_*` / ICS / CSV builders.
//!
//! subagent-C implements these against the exact JSON shapes returned by the
//! Blackboard REST API (same shapes the Python version consumes). Contract:
//! each builder takes the API values + a meta map and returns Markdown/CSV/ICS
//! text. Default implementations pass through JSON so the server is usable
//! before full parity.

use serde_json::Value;

/// "# Courses (N total)" list (port of _md_courses).
pub fn md_courses(courses: &[Value], total: usize) -> String {
    let mut lines = vec![format!("# Courses ({total} total)"), String::new()];
    if courses.is_empty() {
        lines.push("_No courses to show._".to_string());
    }
    for c in courses {
        let title = c.get("name").and_then(Value::as_str).unwrap_or("?");
        let cid = c.get("courseId").and_then(Value::as_str).unwrap_or("?");
        lines.push(format!("- **{title}** `{cid}`"));
    }
    lines.join("\n")
}

/// Content items list (port of _md_content_items).
pub fn md_content_items(items: &[Value], total: usize) -> String {
    let mut lines = vec![format!("# Content items ({total} total)"), String::new()];
    if items.is_empty() {
        lines.push("_No items._".to_string());
    }
    for it in items {
        let title = it.get("title").and_then(Value::as_str).unwrap_or("?");
        let id = it.get("id").and_then(Value::as_str).unwrap_or("?");
        lines.push(format!("- 📄 **{title}** `{id}`"));
    }
    lines.join("\n")
}

/// Placeholder for the remaining builders (upcoming/announcements/gradebook/
/// search/files/messages/summaries). subagent-C replaces with full markdown.
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

pub fn format_bytes(n: u64) -> String {
    if n >= 1 << 20 { format!("{:.1} MB", n as f64 / (1 << 20) as f64) }
    else if n >= 1 << 10 { format!("{:.1} KB", n as f64 / (1 << 10) as f64) }
    else { format!("{n} B") }
}
