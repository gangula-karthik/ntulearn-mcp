
//! HTML/Body parsers — port of `src/ntulearn_mcp/parsers.py`.
//!
//! subagent-C implements these. Contract:
//!   * extract_file_links(html) -> Vec<(display_name, href)> — find attachment
//!     links in the body HTML of a Blackboard content item.
//!   * strip_html(html) -> String — remove tags (markdown-ish).
//!   * content_text(html) -> String — clean body text for summaries.
//!   * sanitize_filename(name) -> String — filesystem-safe name.

/// Extract (display_name, href) pairs for file attachments in content HTML.
pub fn extract_file_links(_html: &str) -> Vec<(String, String)> {
    // TODO(subagent-C)
    Vec::new()
}

/// Strip HTML tags, keep text.
pub fn strip_html(_html: &str) -> String {
    // TODO(subagent-C)
    String::new()
}

/// Readable text content of a Blackboard content item body.
pub fn content_text(_html: &str) -> String {
    // TODO(subagent-C)
    String::new()
}

/// Filesystem-safe filename (trim, replace path separators, cap length).
pub fn sanitize_filename(name: &str) -> String {
    let mut out = name.chars().take(200).collect::<String>();
    for c in ['/', '\\', ':'] {
        out = out.replace(c, "_");
    }
    if out.is_empty() { out = "file".to_string(); }
    out
}
