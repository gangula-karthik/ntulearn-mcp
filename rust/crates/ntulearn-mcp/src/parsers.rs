use serde_json::{json, Value};
use scraper::{Html, Selector};

/// Extensions whose content is treated as plain text by `extract_content`
/// (the Python `_TEXT_EXTENSIONS` set from ntulearn_mcp/common.py).
const TEXT_EXTENSIONS: &[&str] = &[
    "txt", "md", "markdown", "csv", "tsv", "json", "xml", "yaml", "yml",
    "html", "htm", "log", "py", "js", "ts", "rs", "go", "java", "c", "cpp",
    "h", "hpp", "sh", "bash", "zsh", "rb", "swift", "kt", "scala", "r",
    "ini", "toml", "cfg", "conf", "env",
];

/// MIME types treated as plain text (Portuguese-style content-type from the
/// Blackboard CDN is normalized by `parse_content_type` first).
const TEXT_MIMETYPES: &[&str] = &[
    "application/json",
    "application/xml",
    "application/javascript",
    "application/x-javascript",
    "application/x-yaml",
    "application/yaml",
    "application/ld+json",
    "application/x-sh",
];

fn unwrap_bbfile(raw: &str) -> Option<String> {
    if !raw.trim_start().starts_with('{') {
        return Some(raw.to_string());
    }
    if let Ok(parsed) = serde_json::from_str::<Value>(raw) {
        if let Some(obj) = parsed.as_object() {
            for key in ["linkName", "displayName", "name", "filename"] {
                if let Some(v) = obj.get(key).and_then(|v| v.as_str()) {
                    if !v.is_empty() {
                        return Some(v.to_string());
                    }
                }
            }
        }
        // Was JSON but nothing usable parsed out: Python keeps the raw
        // data-bbfile string via `or filename` in extract_*.
        return Some(raw.to_string());
    }
    Some(raw.to_string())
}

/// Is this href a Blackboard file link? (Python: `"bbcswebdav" in href`.)
fn has_bbcswebdav(href: &str) -> bool {
    href.to_ascii_lowercase().contains("bbcswebdav")
}

/// One bbcswebdav anchor -> (href, filename, link_text).
fn bb_anchor_pairs(html_body: &str) -> Vec<(String, Option<String>, Option<String>)> {
    let mut out = Vec::new();
    let doc = Html::parse_fragment(html_body);
    let Ok(sel) = Selector::parse("a[href]") else { return out };
    for el in doc.select(&sel) {
        let Some(href) = el.value().attr("href") else { continue };
        if !has_bbcswebdav(href) {
            continue;
        }
        let filename = el
            .value()
            .attr("data-bbfile")
            .map(|s| s.to_string())
            .and_then(|b| unwrap_bbfile(&b));
        let link_text = el
            .text()
            .collect::<Vec<_>>()
            .join("")
            .trim()
            .to_string();
        let link_text = if link_text.is_empty() { None } else { Some(link_text) };
        out.push((href.to_string(), filename, link_text));
    }
    out
}

/// Parse a Blackboard content item's body HTML and return the first
/// bbcswebdav `(url, filename)` pair (parsers.py `extract_bbcswebdav_url`).
pub fn extract_bbcswebdav_url(html_body: &str) -> Option<(String, Option<String>)> {
    if html_body.is_empty() {
        return None;
    }
    for (href, filename, link_text) in bb_anchor_pairs(html_body) {
        let name = filename.or(link_text);
        return Some((href, name));
    }
    None
}

/// Return every bbcswebdav file link found in body HTML (parsers.py
/// `extract_all_files`). Each entry: {url, filename, link_text}.
pub fn extract_all_files(html_body: &str) -> Vec<Value> {
    if html_body.is_empty() {
        return Vec::new();
    }
    bb_anchor_pairs(html_body)
        .into_iter()
        .map(|(url, filename, link_text)| {
            json!({
                "url": url,
                "filename": filename,
                "link_text": link_text,
            })
        })
        .collect()
}

/// Extract (display_name, href) pairs for attached files in content HTML.
/// display_name prefers data-bbfile, else the anchor text.
pub fn extract_file_links(html: &str) -> Vec<(String, String)> {
    bb_anchor_pairs(html)
        .into_iter()
        .map(|(href, filename, link_text)| {
            let name = filename.or(link_text).unwrap_or_else(|| href.clone());
            (name, href)
        })
        .collect()
}

/// All text nodes under the document body, flattened.
fn body_text_parts(html: &str) -> Vec<String> {
    let doc = Html::parse_fragment(html);
    // Best effort: walk every element's text iterator; joining text nodes
    // with "\n" approximates BeautifulSoup's get_text(separator="\n").
    let sel = Selector::parse("*").unwrap();
    let mut parts: Vec<String> = Vec::new();
    for el in doc.select(&sel) {
        for t in el.text() {
            parts.push(t.to_string());
        }
    }
    if parts.is_empty() {
        // Nothing selectable; fall back to raw text nodes.
        let mut collected = Vec::new();
        for node in doc.tree.nodes() {
            if let scraper::Node::Text(t) = node.value() {
                collected.push(t.to_string());
            }
        }
        parts = collected;
    }
    parts
}

fn collapse_lines(input: &str) -> String {
    input
        .split('\n')
        .map(|seg| seg.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Strip HTML tags, keep collapsed text (server `_strip_html` semantics).
pub fn strip_html(value: &str) -> String {
    if value.trim().is_empty() {
        return String::new();
    }
    let parts = body_text_parts(value);
    collapse_lines(&parts.join("\n"))
}

/// Readable text content of a Blackboard content item body.
pub fn content_text(html: &str) -> String {
    strip_html(html)
}


/// Filesystem-safe filename (common.py `sanitize_filename`): path-hostile
/// chars -> "_", capped at 200 chars.
pub fn sanitize_filename(name: &str) -> String {
    let mut out: String = name
        .replace('\\', "_")
        .replace('/', "_")
        .replace('*', "_")
        .replace('?', "_")
        .replace(':', "_")
        .replace('"', "_")
        .replace('<', "_")
        .replace('>', "_")
        .replace('|', "_");
    if out.chars().count() > 200 {
        out = out.chars().take(200).collect();
    }
    if out.is_empty() {
        out = "file".to_string();
    }
    out
}

/// Slugify a course/term name for use as a local folder name (common.py
/// `safe_folder_name`).
pub fn safe_folder_name(name: &str) -> String {
    if name.trim().is_empty() {
        return "untitled".to_string();
    }
    let mut slug: String = String::new();
    for ch in name.chars() {
        if ch.is_alphanumeric() || ch == '_' || ch == '.' || ch == '-' || ch == ' ' {
            slug.push(ch);
        } else {
            slug.push('_');
        }
    }
    let mut out = String::new();
    let mut in_space = false;
    for ch in slug.trim().chars() {
        if ch == ' ' {
            if !in_space {
                out.push(' ');
                in_space = true;
            }
        } else {
            out.push(ch);
            in_space = false;
        }
    }
    let out = out.trim_matches(|c| c == ' ' || c == '.' || c == '_').to_string();
    if out.is_empty() {
        "untitled".to_string()
    } else {
        out
    }
}

/// Lowercased extension of a filename (common.py `_file_extension`).
pub fn file_extension(filename: &str) -> String {
    match filename.rfind('.') {
        Some(i) if i + 1 < filename.len() => filename[i + 1..].to_lowercase(),
        _ => String::new(),
    }
}

/// Return (mime, charset) from a Content-Type header value
/// (common.py `_parse_content_type`).
pub fn parse_content_type(content_type: Option<&str>) -> (String, Option<String>) {
    let Some(ct) = content_type else {
        return (String::new(), None);
    };
    let mut parts = ct.split(';').map(|p| p.trim());
    let mime = parts.next().unwrap_or("").to_lowercase();
    let mut charset = None;
    for p in parts {
        let lower = p.to_lowercase();
        if let Some(rest) = lower.strip_prefix("charset=") {
            let v = rest.trim().trim_matches('\'').to_string();
            charset = Some(v);
        }
    }
    (mime, charset)
}

/// Classify a downloaded file: pdf | docx | pptx | xlsx | text | binary
/// (common.py `classify_kind` - extension wins over MIME).
pub fn classify_kind(filename: &str, content_type: Option<&str>) -> String {
    let ext = file_extension(filename);
    match ext.as_str() {
        "pdf" => return "pdf".to_string(),
        "docx" => return "docx".to_string(),
        "pptx" => return "pptx".to_string(),
        "xlsx" => return "xlsx".to_string(),
        _ => {}
    }
    if TEXT_EXTENSIONS.contains(&ext.as_str()) {
        return "text".to_string();
    }
    let (mime, _) = parse_content_type(content_type);
    if mime == "application/pdf" {
        return "pdf".to_string();
    }
    if mime.contains("wordprocessingml") {
        return "docx".to_string();
    }
    if mime.contains("presentationml") {
        return "pptx".to_string();
    }
    if mime.contains("spreadsheetml") {
        return "xlsx".to_string();
    }
    if mime.starts_with("text/") {
        return "text".to_string();
    }
    if TEXT_MIMETYPES.contains(&mime.as_str()) {
        return "text".to_string();
    }
    "binary".to_string()
}

/// Format a float like Python's `str(float)`: integral values get a
/// trailing ".0" (10.0 -> "10.0"), otherwise minimal representation.
pub fn format_py_float(v: f64) -> String {
    if v == v.trunc() && v.is_finite() {
        format!("{:.1}", v)
    } else {
        format!("{}", v)
    }
}

/// Human-readable byte size (server `_format_bytes`).
pub fn format_bytes(n: u64) -> String {
    if n >= 1 << 20 {
        format!("{:.1} MB", n as f64 / (1 << 20) as f64)
    } else if n >= 1 << 10 {
        format!("{:.1} KB", n as f64 / (1 << 10) as f64)
    } else {
        format!("{n} B")
    }
}

/// Detect file kind and extract text - one response entry (server
/// `_extract_content`). PDF/office extraction is not yet supported in the
/// Rust port; those kinds return honest error entries.
pub fn extract_content(filename: &str, content_type: Option<&str>, bytes: &[u8]) -> Value {
    let size = bytes.len() as u64;
    let kind = classify_kind(filename, content_type);

    match kind.as_str() {
        "text" => {
            let text = match std::str::from_utf8(bytes) {
                Ok(t) => t.to_string(),
                Err(_) => bytes.iter().map(|&b| b as char).collect::<String>(),
            };
            let ext = file_extension(filename);
            let (mime, _) = parse_content_type(content_type);
            let html_like = ext == "html" || ext == "htm" || mime == "text/html";
            let text = if html_like { strip_html(&text) } else { text };
            json!({
                "filename": filename,
                "kind": "text",
                "text": text,
                "sizeBytes": size,
                "contentType": content_type,
            })
        }
        k if k == "pdf" || k == "docx" || k == "pptx" || k == "xlsx" => json!({
            "filename": filename,
            "kind": k,
            "error": format!(
                "{k} text extraction is not yet supported in the Rust port \
                 (use ntulearn_download_file to save it locally, then read the \
                 file with a local tool)."
            ),
            "sizeBytes": size,
            "contentType": content_type,
        }),
        _ => json!({
            "filename": filename,
            "kind": "binary",
            "error": format!(
                "Binary file ({}). Cannot extract text. \
                 Use ntulearn_download_file to save it locally.",
                content_type.unwrap_or("unknown type"),
            ),
            "sizeBytes": size,
            "contentType": content_type,
        }),
    }
}
