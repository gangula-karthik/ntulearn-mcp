//! Tool dispatch: maps the 25 MCP tool names to typed handler functions.
//!
//! Port of `src/ntulearn_mcp/handlers.py` (registry: list_messages … summarize_course)
//! and the server-local tools in `src/ntulearn_mcp/server.py` (list_courses …
//! get_gradebook). Every handler returns a JSON `Value` payload whose keys, nulls and
//! types mirror the Python layer literally; `render_for_tool` then turns it into
//! markdown or pretty JSON depending on `response_format` (default "json").
//!
//! The Rust `NTULearnClient` now provides the full high-level contract surface
//! (get_course_contents, get_messages, get_calendar_items, …) mirroring
//! `ntulearn_mcp/client.py`. This file wraps those methods into small private
//! helpers returning Python-shaped `Vec<Value>` / `Value`, so every handler body
//! below matches the Python layer literally. 401 cookie-refresh and paging are
//! handled inside the client; the helpers pass `cache: None` to use each method's
//! default TTL, matching the Python `_with_cache(cache=None)` path.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;

use crate::mcp::ToolContent;
use serde_json::{Map, Value, json};

use crate::client::{ClientError, NTULearnClient};
use crate::fbf::{FbfCriterion, FbfOption, FbfRating, FbfReview, FbfSession, FbfTask};
use crate::kaltura::{FlavorAsset, KalturaEmbed, KalturaSession, TranscriptError};
use crate::{AppState, parsers, render};

// ---------------------------------------------------------------------------
// Constants (mirror handlers.py / server.py / common.py)
// ---------------------------------------------------------------------------

const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 200;
const MAX_DEPTH: usize = 10;
const MAX_FILE_BYTES: u64 = 25 * 1024 * 1024; // 25 MB
const MAX_TOTAL_BYTES: u64 = 40 * 1024 * 1024; // 40 MB
const FOLDER_HANDLERS: &[&str] = &[
    "resource/x-bb-folder",
    "resource/x-bb-module",
    "resource/x-bb-courselink",
    "resource/x-bb-contentlink",
];

const FILE_HANDLERS: &[&str] = &[
    "resource/x-bb-document",
    "resource/x-bb-file",
    "resource/x-bb-externallink",
    "resource/x-bb-assignment",
    "resource/x-bb-asynch-assignment",
    "resource/x-bb-testsurvey_pool",
];

const INSTRUCTOR_ROLES: &[&str] = &[
    "Instructor",
    "TeachingAssistant",
    "CourseBuilder",
    "CourseSupport",
];

const ASSIGNMENT_TYPES: &[&str] = &["GradebookColumn", "Assignment", "Test", "Survey"];

const CALENDAR_ITEM_TYPES: &[&str] = &[
    "Course",
    "GradebookColumn",
    "Institution",
    "OfficeHours",
    "Personal",
];

const BB_ID_PATTERN: &str = r"^[\w:.-]+$";

// What's-new "last seen" tracker (in-memory; matches tracker_get_last_seen()).
static LAST_SEEN: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
static DOWNLOAD_TIMESTAMP: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// dispatch
// ---------------------------------------------------------------------------

/// Route a tool name + JSON args to the right handler and return the raw
/// payload (used by the CLI, which renders it itself).
pub async fn dispatch_value(state: &AppState, name: &str, args: &Value) -> Result<Value, String> {
    let short = name.strip_prefix("ntulearn_").unwrap_or(name);
    let out: Value = match short {
        "list_courses" => h_list_courses(state, args).await?,
        "get_course_contents" => h_get_course_contents(state, args).await?,
        "search_course_content" => h_search_course_content(state, args).await?,
        "download_file" => h_download_file(state, args).await?,
        "read_file_content" => h_read_file_content(state, args).await?,
        "get_upcoming" => h_get_upcoming(state, args).await?,
        "get_announcements" => h_get_announcements(state, args).await?,
        "get_gradebook" => h_get_gradebook(state, args).await?,
        "list_messages" => h_list_messages(state, args).await?,
        "read_message" => h_read_message(state, args).await?,
        "list_course_users" => h_list_course_users(state, args).await?,
        "list_course_groups" => h_list_course_groups(state, args).await?,
        "get_group_members" => h_get_group_members(state, args).await?,
        "get_gradebook_attempts" => h_get_gradebook_attempts(state, args).await?,
        "search_all_courses" => h_search_all_courses(state, args).await?,
        "get_content_tree" => h_get_content_tree(state, args).await?,
        "download_course" => h_download_course(state, args).await?,
        "whats_new" => h_whats_new(state, args).await?,
        "export_calendar_ics" => h_export_calendar_ics(state, args).await?,
        "export_gradebook_csv" => h_export_gradebook_csv(state, args).await?,
        "summarize_course" => h_summarize_course(state, args).await?,
        "fbf_status" => h_fbf_status(state, args).await?,
        "fbf_reviews" => h_fbf_reviews(state, args).await?,
        "fbf_rate" => h_fbf_rate(state, args).await?,
        "fbf_comment" => h_fbf_comment(state, args).await?,
        "pcv_status" => h_pcv_status(state, args).await?,
        "pcv_evals" => h_pcv_evals(state, args).await?,
        "pcv_rate" => h_pcv_rate(state, args).await?,
        "pcv_comment" => h_pcv_comment(state, args).await?,
        "kaltura_list" => h_kaltura_list(state, args).await?,
        "kaltura_transcripts" => h_kaltura_transcripts(state, args).await?,
        "kaltura_download" => h_kaltura_download(state, args).await?,
        other => {
            return Err(format!("unknown tool: ntulearn_{other}"));
        }
    };
    Ok(out)
}

/// How long `ntulearn_login` blocks before handing back to the agent; many
/// MCP hosts time tool calls out after ~60 s.
const LOGIN_WAIT: std::time::Duration = std::time::Duration::from_secs(45);

/// `ntulearn_login`: validate the current session, else run an interactive
/// capture (silent SSO first, then a visible login window). The capture runs
/// in its own task so it survives this call returning early; the fresh cookie
/// is installed into the live client whenever it lands.
pub async fn login(state: std::sync::Arc<AppState>, args: &Value) -> Result<String, String> {
    let force = bool_arg(args, "force", false);
    let base = state.client.base_url().to_string();

    if !force {
        let mut current = state.client.current_cookie_value().await;
        if current.is_empty() {
            // Pick up a cookie that appeared since startup (written by
            // `setup`, `refresh`, another server process, or a background
            // capture) before opening any window.
            if let Some(v) = crate::cookie::resolve_cookie() {
                state.client.install_cookie(&v).await;
                current = v;
            }
        }
        if !current.is_empty() && crate::setup::validates_live(&base, &current).await == Ok(true) {
            return Ok(json!({ "status": "already_logged_in" }).to_string());
        }
    }
    if !crate::capture::browser_available() {
        return Err(
            "No Chromium-based browser (Chrome, Edge, Brave, Arc, Vivaldi, Opera) found, so \
                    the login window cannot open. Ask the user to run `ntulearn-mcp setup` in a \
                    terminal, which offers a manual cookie paste."
                .to_string(),
        );
    }

    // One login window at a time: if a capture is already running (a previous
    // ntulearn_login still waiting, or the 401 self-heal path having opened a
    // window), report that instead of opening another.
    let Some(claim) = crate::capture::try_claim_interactive() else {
        return Ok(json!({
            "status": "waiting_for_user",
            "message": "A NTULearn login window is already open. Ask the user to log in with \
                        their NTU account there (answer Yes to 'Stay signed in?'). The window \
                        closes by itself when done; call ntulearn_login again to confirm."
        })
        .to_string());
    };

    let task = tokio::spawn({
        let state = state.clone();
        async move {
            let _claim = claim;
            let res = crate::capture::capture_cookie(
                &base,
                crate::capture::CaptureMode::Interactive,
                true,
            )
            .await;
            if let Ok((val, _)) = &res {
                state.client.install_cookie(val).await;
            }
            res
        }
    });
    match tokio::time::timeout(LOGIN_WAIT, task).await {
        Ok(Ok(Ok((_, browser)))) => {
            Ok(json!({ "status": "logged_in", "browser": browser }).to_string())
        }
        Ok(Ok(Err(e))) => Err(format!(
            "Login did not complete: {e}. Ask the user to run `ntulearn-mcp setup` in a terminal."
        )),
        Ok(Err(e)) => Err(format!("login task failed: {e}")),
        Err(_) => Ok(json!({
            "status": "waiting_for_user",
            "message": "A NTULearn login window is open. Ask the user to log in with their NTU \
                        account there (answer Yes to 'Stay signed in?'). The window closes by \
                        itself when done; then call ntulearn_login again to confirm."
        })
        .to_string()),
    }
}

/// Route a tool name + JSON args to the right handler, then render (MCP path).
pub async fn dispatch(
    state: &AppState,
    name: &str,
    args: &Value,
) -> Result<Vec<ToolContent>, String> {
    let out = dispatch_value(state, name, args).await?;
    let short = name.strip_prefix("ntulearn_").unwrap_or(name);
    Ok(render_for_tool(short, args, &out))
}

// ---------------------------------------------------------------------------
// Argument + value helpers
// ---------------------------------------------------------------------------

/// Resolve the optional `response_format` (default "toon" — token-optimized). Invalid values are
/// rejected exactly like `common.resolve_response_format`.
pub fn response_format(args: &Value) -> Result<String, String> {
    let fmt = args
        .get("response_format")
        .map(|v| match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .unwrap_or_else(|| "toon".to_string())
        .to_lowercase();
    if fmt != "json" && fmt != "markdown" && fmt != "toon" {
        return Err("response_format must be 'json', 'markdown' or 'toon'".to_string());
    }
    Ok(fmt)
}

pub fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|v| v.as_str())
}

pub fn u64_arg(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(|v| v.as_u64())
}

pub fn bool_arg(args: &Value, key: &str, default: bool) -> bool {
    args.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
}

fn parse_int_arg(args: &Value, key: &str, default: i64) -> i64 {
    match args.get(key) {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(default),
        Some(Value::String(s)) => s.parse::<i64>().unwrap_or(default),
        _ => default,
    }
}

/// `common.resolve_pagination_args` — defaults offset=0, limit=50.
fn resolve_pagination_args(args: &Value) -> Result<(usize, usize), String> {
    let offset = parse_int_arg(args, "offset", 0);
    let limit = parse_int_arg(args, "limit", DEFAULT_LIMIT as i64);
    if offset < 0 {
        return Err("offset must be >= 0".to_string());
    }
    if limit < 1 {
        return Err("limit must be >= 1".to_string());
    }
    if limit > MAX_LIMIT as i64 {
        return Err(format!("limit must be <= {MAX_LIMIT}"));
    }
    Ok((offset as usize, limit as usize))
}

/// `common.slice_with_pagination` — slice + meta dict.
fn slice_with_pagination(items: &[Value], offset: usize, limit: usize) -> (Vec<Value>, Value) {
    let total = items.len();
    let end = total.min(offset.saturating_add(limit));
    let page = {
        let mut v = Vec::with_capacity(end.saturating_sub(offset));
        for it in items.iter().skip(offset).take(end - offset) {
            v.push(it.clone());
        }
        v
    };
    let next_offset = if end < total {
        Value::from(end as u64)
    } else {
        Value::Null
    };
    let meta = json!({
        "total": total,
        "count": page.len(),
        "offset": offset,
        "limit": limit,
        "hasMore": !next_offset.is_null(),
        "nextOffset": next_offset,
    });
    (page, meta)
}

/// `common.validate_iso8601` — cheap sanity check + unchanged return.
fn validate_iso8601(value: &str, name: &str) -> Result<String, String> {
    if value.trim().is_empty() {
        return Err(format!(
            "{name} must be a non-empty ISO-8601 timestamp string"
        ));
    }
    let normalised = value.to_string();
    let ok = chrono::DateTime::parse_from_rfc3339(&normalised).is_ok()
        || chrono::NaiveDateTime::parse_from_str(&normalised, "%Y-%m-%dT%H:%M:%S%.f").is_ok()
        || chrono::NaiveDate::parse_from_str(&normalised, "%Y-%m-%d").is_ok();
    if !ok {
        return Err(format!(
            "{name}={value:?} is not a valid ISO-8601 timestamp. \
             Expected format like '2026-05-09T00:00:00Z'."
        ));
    }
    Ok(value.to_string())
}

/// `common.validate_bb_id` — Blackboard-style ID (alnum, underscore, dash, colon, dot).
fn validate_bb_id(value: &str, name: &str) -> Result<String, String> {
    let s = value;
    let valid = !s.is_empty()
        && s.chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == ':' || c == '-' || c == '.');
    if !valid {
        return Err(format!(
            "{name} must be a valid Blackboard ID (letters, digits, _ - :), got {s:?}"
        ));
    }
    Ok(s.to_string())
}

fn resolve_pdf_mode(args: &Value) -> Result<String, String> {
    let mut mode = str_arg(args, "mode").unwrap_or("text").to_lowercase();
    if mode == "auto" {
        mode = "text".to_string();
    }
    if mode != "text" && mode != "vision" {
        return Err("mode must be 'text' or 'vision'".to_string());
    }
    Ok(mode)
}

/// `server._parse_page_range` — "1,3-5" -> {1,3,4,5}; None means all pages.
fn parse_page_range(spec: Option<&Value>) -> Result<Option<HashSet<u32>>, String> {
    let Some(spec) = spec else { return Ok(None) };
    let text = match spec {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        other => {
            return Err(format!(
                "pages must be a string like '1,3-5', got {other:?}"
            ));
        }
    };
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let mut set = HashSet::new();
    for part in text.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((a, b)) = part.split_once('-') {
            let start: u32 = a.trim().parse().map_err(|_| {
                format!("pages must be a set of 1-based page numbers, got {spec:?}")
            })?;
            let end: u32 = b.trim().parse().map_err(|_| {
                format!("pages must be a set of 1-based page numbers, got {spec:?}")
            })?;
            if start == 0 || end == 0 || start > end {
                return Err(format!(
                    "pages must be a set of 1-based page numbers, got {spec:?}"
                ));
            }
            for p in start..=end {
                set.insert(p);
            }
        } else {
            let p: u32 = part.parse().map_err(|_| {
                format!("pages must be a set of 1-based page numbers, got {spec:?}")
            })?;
            if p == 0 {
                return Err(format!(
                    "pages must be a set of 1-based page numbers, got {spec:?}"
                ));
            }
            set.insert(p);
        }
    }
    Ok(Some(set))
}

fn gs<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(|x| x.as_str())
}

fn str_or<'a>(v: &'a Value, key: &str, default: &'a str) -> &'a str {
    gs(v, key).unwrap_or(default)
}

fn u64_or(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(|x| x.as_u64()).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Time helpers (common.py)
// ---------------------------------------------------------------------------

pub(crate) fn now_iso() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn iso_minus_days(days: i64) -> String {
    (chrono::Utc::now() - chrono::Duration::days(days))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

fn default_since() -> String {
    iso_minus_days(7)
}

fn iso_from_now(days: i64, minute_of_day: u32) -> String {
    let now = chrono::Utc::now();
    let mut stamp = now + chrono::Duration::days(days);
    if minute_of_day <= 1439 {
        let date = stamp.date_naive();
        let time = chrono::NaiveTime::from_hms_opt(minute_of_day / 60, minute_of_day % 60, 0)
            .unwrap_or_else(|| stamp.time());
        let ndt = chrono::NaiveDateTime::new(date, time);
        stamp = chrono::DateTime::from_naive_utc_and_offset(ndt, chrono::Utc);
    }
    stamp.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// `_after(value, threshold)` — ISO value on/after threshold (missing never matches).
fn after_value(value: Option<&Value>, threshold: &str) -> bool {
    let Some(raw) = value.and_then(|v| v.as_str()) else {
        return false;
    };
    if raw.is_empty() {
        return false;
    }
    let parse = |s: &str| -> Option<chrono::DateTime<chrono::Utc>> {
        chrono::DateTime::parse_from_rfc3339(s)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .ok()
    };
    match (parse(raw), parse(threshold)) {
        (Some(a), Some(b)) => a >= b,
        _ => {
            // Fall back to lexicographic comparison for identical shapes
            // (e.g. plain dates) — matches datetime ordering for ISO strings.
            raw >= threshold
        }
    }
}

fn parse_iso_u64_for_sort(raw: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.timestamp())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Client contract helpers (re-implementations of the Python client methods)
// ---------------------------------------------------------------------------

fn _path_course(course_id: &str) -> String {
    format!("/learn/api/public/v1/courses/{course_id}")
}

fn _path_contents(course_id: &str) -> String {
    format!("/learn/api/public/v1/courses/{course_id}/contents")
}

fn _path_children(course_id: &str, content_id: &str) -> String {
    format!("/learn/api/public/v1/courses/{course_id}/contents/{content_id}/children")
}

fn _path_content_item(course_id: &str, content_id: &str) -> String {
    format!("/learn/api/public/v1/courses/{course_id}/contents/{content_id}")
}

fn _path_attachments(course_id: &str, content_id: &str) -> String {
    format!("/learn/api/public/v1/courses/{course_id}/contents/{content_id}/attachments")
}

fn _path_attachment_download(course_id: &str, content_id: &str, attachment_id: &str) -> String {
    format!(
        "/learn/api/public/v1/courses/{course_id}/contents/{content_id}/attachments/{attachment_id}/download"
    )
}

fn _path_gradebook_columns(course_id: &str) -> String {
    format!("/learn/api/public/v1/courses/{course_id}/gradebook/columns")
}

fn _path_user_grades(course_id: &str, user_id: &str) -> String {
    format!("/learn/api/public/v1/courses/{course_id}/gradebook/users/{user_id}")
}

fn _path_gradebook_attempts(course_id: &str, column_id: &str) -> String {
    format!("/learn/api/public/v1/courses/{course_id}/gradebook/columns/{column_id}/attempts")
}

fn _path_user_attempts(course_id: &str, column_id: &str, user_id: &str) -> String {
    format!(
        "/learn/api/public/v1/courses/{course_id}/gradebook/columns/{column_id}/users/{user_id}/attempts"
    )
}

fn _path_term(term_id: &str) -> String {
    format!("/learn/api/public/v1/terms/{term_id}")
}

fn _path_announcements(course_id: &str) -> String {
    format!("/learn/api/public/v1/courses/{course_id}/announcements")
}

fn _path_messages() -> &'static str {
    "/learn/api/public/v1/users/me/messages"
}

fn _path_message(message_id: &str) -> String {
    format!("/learn/api/public/v1/users/me/messages/{message_id}")
}

fn _path_message_participants(message_id: &str) -> String {
    format!("/learn/api/public/v1/users/me/messages/{message_id}/participants")
}

fn _path_course_users(course_id: &str) -> String {
    format!("/learn/api/public/v1/courses/{course_id}/users")
}

fn _path_course_groups(course_id: &str) -> String {
    format!("/learn/api/public/v1/courses/{course_id}/groups")
}

fn _path_group_users(course_id: &str, group_id: &str) -> String {
    format!("/learn/api/public/v1/courses/{course_id}/groups/{group_id}/users")
}

fn dbg_err(e: String) -> String {
    e
}

// ---------------------------------------------------------------------------
// Generic JSON primitives through the Rust client
// ---------------------------------------------------------------------------

fn err_str(e: ClientError) -> String {
    e.to_string()
}

// ---------------------------------------------------------------------------
// Client method layer — wraps the NTULearnClient high-level API with the
// Python contract-method shapes (Result<Vec<Value>> / Result<Value>), so the
// payload logic above stays identical to ntulearn_mcp/client.py + handlers.py.
// `cache: None` below means "use the method's default TTL", matching the
// Python `_with_cache(cache=None)` path.
// ---------------------------------------------------------------------------

/// as_list — typed list methods return `Value`; accept a plain array or the
/// `{results:[...]}` envelope get_paginated produces.
fn as_list(v: Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a,
        Value::Object(mut o) => match o.remove("results") {
            Some(Value::Array(a)) => a,
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// get_my_enrollments — `/users/me/courses` (paginated).
async fn me_enrollments(c: &NTULearnClient) -> Result<Vec<Value>, String> {
    let v = c.get_my_enrollments(None).await.map_err(err_str)?;
    Ok(as_list(v))
}

/// Enrollments with `expand=course`: course ids plus id -> display name from
/// the same response. The name map is empty when the server ignores `expand`,
/// in which case callers fall back to a per-course GET.
async fn enrollment_ids_and_names(
    c: &NTULearnClient,
) -> Result<(Vec<String>, HashMap<String, String>), String> {
    // If the tenant rejects `expand=course` outright, fall back to the plain
    // enrollment list; names then come from the per-course fallback.
    let v = match c.get_my_enrollments_expanded(None).await {
        Ok(v) => v,
        Err(_) => c.get_my_enrollments(None).await.map_err(err_str)?,
    };
    let mut ids = Vec::new();
    let mut names = HashMap::new();
    for e in as_list(v) {
        let avail = e
            .get("availability")
            .and_then(|v| v.get("available"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if avail != "Yes" {
            continue;
        }
        let Some(cid) = e.get("courseId").and_then(|v| v.as_str()) else {
            continue;
        };
        if cid.is_empty() {
            continue;
        }
        ids.push(cid.to_string());
        if let Some(name) = e
            .get("course")
            .and_then(|c| c.get("name"))
            .and_then(|v| v.as_str())
        {
            if !name.is_empty() {
                names.insert(cid.to_string(), name.to_string());
            }
        }
    }
    Ok((ids, names))
}

/// get_my_user_id — `/users/me` -> `id`.
async fn me_user_id(c: &NTULearnClient) -> Result<Option<String>, String> {
    let id = c.get_my_user_id().await.map_err(err_str)?;
    Ok(Some(id))
}

/// get_course — `/courses/{id}`.
async fn course(c: &NTULearnClient, course_id: &str) -> Result<Value, String> {
    c.get_course(course_id, None).await.map_err(err_str)
}

/// get_courses_batch — the client fetches courses and returns `{"id": cid,
/// "name": cid}` placeholders for 403/404 courses (exactly the Python batch).
async fn courses_batch(c: &NTULearnClient, course_ids: &[String]) -> Vec<Value> {
    match c.get_courses_batch(course_ids, None).await {
        Ok(v) => as_list(v),
        Err(_) => Vec::new(),
    }
}

/// get_course_contents — `/courses/{id}/contents` (paginated).
async fn course_contents(c: &NTULearnClient, course_id: &str) -> Result<Vec<Value>, String> {
    let v = c
        .get_course_contents(course_id, None)
        .await
        .map_err(err_str)?;
    Ok(as_list(v))
}

/// get_content_children — `/courses/{id}/contents/{cid}/children`.
async fn content_children(
    c: &NTULearnClient,
    course_id: &str,
    content_id: &str,
) -> Result<Vec<Value>, String> {
    let v = c
        .get_content_children(course_id, content_id, None)
        .await
        .map_err(err_str)?;
    Ok(as_list(v))
}

/// get_content_item — single content object.
async fn content_item(
    c: &NTULearnClient,
    course_id: &str,
    content_id: &str,
) -> Result<Value, String> {
    c.get_content_item(course_id, content_id, None)
        .await
        .map_err(err_str)
}

/// get_course_search — contents?search=… ; API errors become [] (Python does
/// exactly this inside the client method).
async fn course_search(c: &NTULearnClient, course_id: &str, query: &str) -> Vec<Value> {
    match c.get_course_search(course_id, query, None).await {
        Ok(v) => as_list(v),
        Err(_) => Vec::new(),
    }
}

/// get_attachments — `/contents/{cid}/attachments`.
async fn attachments(c: &NTULearnClient, course_id: &str, content_id: &str) -> Vec<Value> {
    match c.get_attachments(course_id, content_id, None).await {
        Ok(v) => as_list(v),
        Err(_) => Vec::new(),
    }
}

/// get_attachment_download_url — the shared client follows the redirect and
/// refreshes the BbRouter cookie on 401 internally.
async fn attachment_download_url(
    c: &NTULearnClient,
    course_id: &str,
    content_id: &str,
    attachment_id: &str,
) -> Result<String, String> {
    c.get_attachment_download_url(course_id, content_id, attachment_id)
        .await
        .map_err(err_str)
}

// ---------------------------------------------------------------------------
// Message / user / group / calendar / announcement client methods
// ---------------------------------------------------------------------------

async fn messages(
    c: &NTULearnClient,
    folder: &str,
    unread_only: bool,
    since: Option<&str>,
) -> Result<Vec<Value>, String> {
    let folder_arg = if folder.is_empty() {
        None
    } else {
        Some(folder)
    };
    let v = c
        .get_messages(folder_arg, unread_only, since, None)
        .await
        .map_err(err_str)?;
    Ok(as_list(v))
}

async fn message(c: &NTULearnClient, message_id: &str) -> Result<Value, String> {
    c.get_message(message_id, None).await.map_err(err_str)
}

async fn message_participants(c: &NTULearnClient, message_id: &str) -> Result<Vec<Value>, String> {
    let v = c
        .get_message_participants(message_id, None)
        .await
        .map_err(err_str)?;
    Ok(as_list(v))
}

async fn course_users(c: &NTULearnClient, course_id: &str) -> Result<Vec<Value>, String> {
    let v = c.get_course_users(course_id, None).await.map_err(err_str)?;
    Ok(as_list(v))
}

async fn course_groups(c: &NTULearnClient, course_id: &str) -> Result<Vec<Value>, String> {
    let v = c
        .get_course_groups(course_id, None)
        .await
        .map_err(err_str)?;
    Ok(as_list(v))
}

async fn group_users(
    c: &NTULearnClient,
    course_id: &str,
    group_id: &str,
) -> Result<Vec<Value>, String> {
    let v = c
        .get_group_users(course_id, group_id, None)
        .await
        .map_err(err_str)?;
    Ok(as_list(v))
}

async fn gradebook_columns(c: &NTULearnClient, course_id: &str) -> Result<Vec<Value>, String> {
    let v = c
        .get_gradebook_columns(course_id, None)
        .await
        .map_err(err_str)?;
    Ok(as_list(v))
}

async fn user_grades(
    c: &NTULearnClient,
    course_id: &str,
    user_id: &str,
) -> Result<Vec<Value>, String> {
    let v = c
        .get_user_grades(course_id, user_id, None)
        .await
        .map_err(err_str)?;
    Ok(as_list(v))
}

async fn gradebook_attempts(
    c: &NTULearnClient,
    course_id: &str,
    column_id: &str,
) -> Result<Vec<Value>, String> {
    let v = c
        .get_gradebook_attempts(course_id, column_id, None)
        .await
        .map_err(err_str)?;
    Ok(as_list(v))
}

async fn user_attempts(
    c: &NTULearnClient,
    course_id: &str,
    column_id: &str,
    user_id: &str,
) -> Result<Vec<Value>, String> {
    let v = c
        .get_user_attempts(course_id, column_id, user_id)
        .await
        .map_err(err_str)?;
    Ok(as_list(v))
}

async fn term(c: &NTULearnClient, term_id: &str) -> Result<Value, String> {
    c.get_term(term_id, None).await.map_err(err_str)
}

async fn announcements(c: &NTULearnClient, course_id: &str) -> Result<Vec<Value>, String> {
    let v = c
        .get_announcements(course_id, None)
        .await
        .map_err(err_str)?;
    Ok(as_list(v))
}

async fn calendar_items(
    c: &NTULearnClient,
    course_id: Option<&str>,
    since: Option<&str>,
    until: Option<&str>,
    item_type: Option<&str>,
) -> Result<Vec<Value>, String> {
    let v = c
        .get_calendar_items(course_id, since, until, item_type, None)
        .await
        .map_err(err_str)?;
    Ok(as_list(v))
}

/// Max concurrent requests when a tool fans out across courses or files.
/// Shares the client's global cap (`NTULEARN_HTTP_CONCURRENCY`, default 48):
/// per-fan-out limits below the global one were the real bottleneck, because
/// 44 courses at 6 was still eight waves.
fn fanout_limit() -> usize {
    crate::client::http_concurrency()
}

/// Run an async fn over items with bounded concurrency, returning results in
/// input order. Failures don't short-circuit — callers decide per result.
/// This is the fan-out primitive behind every all-courses tool: N courses
/// used to cost N round-trips serially; now they cost ceil(N/limit) waves.
async fn bounded_map<T, R, F, Fut>(items: Vec<T>, limit: usize, f: F) -> Vec<R>
where
    F: FnMut(T) -> Fut,
    Fut: std::future::Future<Output = R>,
{
    use futures_util::stream::StreamExt;
    futures_util::stream::iter(items)
        .map(f)
        .buffered(limit)
        .collect()
        .await
}

/// fan_out_course_ids — resolve `course_ids` arg (None/empty -> enrolled+available).
async fn fan_out_course_ids(
    c: &NTULearnClient,
    course_ids_arg: Option<&Value>,
) -> Result<Vec<String>, String> {
    let is_none_or_empty_list = match course_ids_arg {
        None => true,
        Some(Value::Array(a)) => a.is_empty(),
        Some(_) => false,
    };
    if is_none_or_empty_list {
        let enrollments = me_enrollments(c).await?;
        let mut ids = Vec::new();
        for e in enrollments {
            let avail = e
                .get("availability")
                .and_then(|v| v.get("available"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if avail == "Yes" {
                if let Some(cid) = e.get("courseId").and_then(|v| v.as_str()) {
                    if !cid.is_empty() {
                        ids.push(cid.to_string());
                    }
                }
            }
        }
        return Ok(ids);
    }
    let Some(Value::Array(arr)) = course_ids_arg else {
        return Err("course_ids must be a list of strings".to_string());
    };
    let mut out = Vec::with_capacity(arr.len());
    for v in arr {
        out.push(match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        });
    }
    Ok(out)
}

/// resolve_enrolled_course_ids (server._resolve_enrolled_course_ids).
async fn resolve_enrolled_course_ids(
    c: &NTULearnClient,
    include_disabled: bool,
) -> Result<Vec<String>, String> {
    let enrollments = me_enrollments(c).await?;
    let mut ids = Vec::new();
    for e in enrollments {
        if !include_disabled {
            let avail = e
                .get("availability")
                .and_then(|v| v.get("available"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if avail != "Yes" {
                continue;
            }
        }
        if let Some(cid) = e.get("courseId").and_then(|v| v.as_str()) {
            if !cid.is_empty() {
                ids.push(cid.to_string());
            }
        }
    }
    Ok(ids)
}

// ---------------------------------------------------------------------------
// Pure data helpers (handlers.py / server.py)
// ---------------------------------------------------------------------------

fn handler_id(item: &Value) -> String {
    item.get("contentHandler")
        .and_then(|h| h.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn is_folder(item: &Value) -> bool {
    if item.get("hasChildren").and_then(|v| v.as_bool()) == Some(true) {
        return true;
    }
    let hid = handler_id(item);
    if hid.is_empty() {
        return false;
    }
    if FOLDER_HANDLERS.contains(&hid.as_str()) {
        return true;
    }
    !hid.starts_with("resource/x-bb-")
}

fn is_file_item(item: &Value) -> bool {
    let hid = handler_id(item);
    if FILE_HANDLERS.contains(&hid.as_str()) {
        return true;
    }
    if FOLDER_HANDLERS.contains(&hid.as_str()) || hid.is_empty() {
        return false;
    }
    hid.starts_with("resource/x-bb-")
}

fn item_title(item: &Value) -> String {
    item.get("title")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            item.get("name")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or("untitled")
        .to_string()
}

fn item_description(item: &Value) -> String {
    let desc = item.get("description");
    let raw = match desc {
        Some(Value::Object(o)) => o
            .get("text")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| o.get("rawText").and_then(|v| v.as_str()))
            .unwrap_or(""),
        Some(Value::String(s)) => s.as_str(),
        _ => "",
    };
    parsers::strip_html(raw)
}

fn user_name(user: &Value) -> String {
    let name = user.get("name");
    if let Some(Value::Object(o)) = name {
        let given = o.get("given").and_then(|v| v.as_str()).unwrap_or("");
        let family = o.get("family").and_then(|v| v.as_str()).unwrap_or("");
        if !given.is_empty() || !family.is_empty() {
            let mut parts = Vec::new();
            if !given.is_empty() {
                parts.push(given);
            }
            if !family.is_empty() {
                parts.push(family);
            }
            return parts.join(" ");
        }
    }
    // Internal v1 user objects use flat givenName/familyName (no nested `name`).
    let given = user.get("givenName").and_then(|v| v.as_str()).unwrap_or("");
    let family = user
        .get("familyName")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !given.is_empty() || !family.is_empty() {
        let mut parts = Vec::new();
        if !given.is_empty() {
            parts.push(given);
        }
        if !family.is_empty() {
            parts.push(family);
        }
        return parts.join(" ");
    }
    user.get("userName")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn user_role(user: &Value) -> String {
    let raw = user
        .get("courseRoleId")
        .and_then(|v| v.as_str())
        .or_else(|| user.get("role").and_then(|v| v.as_str()))
        .or_else(|| {
            user.get("courseRole")
                .and_then(|v| v.get("identifier"))
                .and_then(|v| v.as_str())
        })
        .unwrap_or("")
        .to_string();
    // Internal v1 memberships use single-letter role codes like "S"; map them
    // to the full names that the public REST courseRoleId uses (and that the
    // INSTRUCTOR_ROLES set expects). Longer values pass through unchanged.
    match raw.as_str() {
        "S" => "Student".to_string(),
        "T" => "TeachingAssistant".to_string(),
        "P" => "PrimaryInstructor".to_string(),
        "I" => "Instructor".to_string(),
        "G" => "Grader".to_string(),
        "O" => "Observer".to_string(),
        other => other.to_string(),
    }
}

/// Render one user row for `list_course_users`. The Blackboard API nests
/// `userName` / `name` under a `user` object; the reference Python server
/// requested flat fields (`userName,name`) that the API silently drops, so
/// names came back empty. We request `user.*` fields and map from the nested
/// object, falling back to the flat shape for compatibility.
fn render_user_row(u: &Value) -> Value {
    let nested = u.get("user");
    let uname = u
        .get("userName")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            nested
                .and_then(|v| v.get("userName"))
                .and_then(|v| v.as_str())
        })
        .unwrap_or("")
        .to_string();
    let name = nested.map(user_name).unwrap_or_else(|| user_name(u));
    let role = user_role(u);
    let uid = u
        .get("userId")
        .or_else(|| u.get("id"))
        .cloned()
        .unwrap_or(Value::Null);
    json!({ "id": uid, "userName": uname, "name": name, "role": role })
}

/// server._strip_content — reduce a raw content item.
fn strip_content(item: &Value) -> Value {
    let handler = item.get("contentHandler");
    let description_raw = item.get("description");
    let description = match description_raw {
        Some(Value::Object(o)) => o.get("rawText").cloned().unwrap_or(Value::Null),
        Some(other) => other.clone(),
        None => Value::Null,
    };
    json!({
        "id": item.get("id").cloned().unwrap_or(Value::Null),
        "title": item.get("title").cloned().unwrap_or(Value::Null),
        "contentHandlerId": handler.and_then(|h| h.get("id")).cloned().unwrap_or(Value::Null),
        "hasChildren": item.get("hasChildren").cloned().unwrap_or(Value::Bool(false)),
        "description": description,
        "modified": item.get("modified").cloned().unwrap_or(Value::Null),
    })
}

/// server._strip_calendar_item — flatten dynamicCalendarItemProps.
fn strip_calendar_item(item: &Value, course_id: Option<&str>) -> Value {
    let dyn_ = item.get("dynamicCalendarItemProps");
    json!({
        "id": item.get("id").cloned().unwrap_or(Value::Null),
        "type": item.get("type").cloned().unwrap_or(Value::Null),
        "title": item.get("title").cloned().unwrap_or(Value::Null),
        "description": item.get("description").cloned().unwrap_or(Value::Null),
        "location": item.get("location").cloned().unwrap_or(Value::Null),
        "start": item.get("start").cloned().unwrap_or(Value::Null),
        "end": item.get("end").cloned().unwrap_or(Value::Null),
        "calendarName": item.get("calendarName").cloned().unwrap_or(Value::Null),
        "courseId": course_id.map(|s| Value::String(s.to_string())).unwrap_or(Value::Null),
        "eventType": dyn_.and_then(|d| d.get("eventType")).cloned().unwrap_or(Value::Null),
        "gradable": dyn_.and_then(|d| d.get("gradable")).cloned().unwrap_or(Value::Null),
        "attemptable": dyn_.and_then(|d| d.get("attemptable")).cloned().unwrap_or(Value::Null),
    })
}

fn calendar_brief(item: &Value) -> Value {
    json!({
        "id": item.get("id").cloned().unwrap_or(Value::Null),
        "title": item.get("title").cloned().unwrap_or(Value::Null),
        "type": item.get("type").cloned().unwrap_or(Value::Null),
        "start": item.get("start").cloned().unwrap_or(Value::Null),
        "end": item.get("end").cloned().unwrap_or(Value::Null),
    })
}

fn announcement_text(ann: &Value) -> String {
    let body = ann.get("body");
    let raw = match body {
        Some(Value::Object(o)) => o
            .get("text")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| o.get("rawText").and_then(|v| v.as_str()))
            .unwrap_or(""),
        Some(Value::String(s)) => s.as_str(),
        _ => "",
    };
    parsers::strip_html(raw)
}

// ---------------------------------------------------------------------------
// Content-tree walker + search (handlers.py)
// ---------------------------------------------------------------------------

struct ContentNode {
    item: Value,
    breadcrumb: Vec<String>,
    depth: usize,
    /// Parent content id ("" for top-level items). Used to assemble the tree
    /// in O(n) instead of rescanning every node at each level.
    parent: String,
}

impl ContentNode {
    fn id(&self) -> String {
        self.item
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    }
    fn title(&self) -> String {
        item_title(&self.item)
    }
}

/// walk_content — every reachable node in a course (bounded, cycle-guarded).
///
/// The previous implementation was a sequential DFS: each folder awaited its
/// children before the next folder started, so a course with 13 weeks and 5
/// folders per week cost ~70 round trips. This walks one *level* at a time in
/// parallel (root -> weeks -> folders -> files = ~4 waves), stores each
/// parent's children, then reconstructs the exact DFS pre-order in memory.
async fn walk_content(c: &NTULearnClient, course_id: &str, max_depth: usize) -> Vec<ContentNode> {
    let mut seen: HashSet<String> = HashSet::new();
    // parent content id ("" = course root) -> its children, in API order.
    let mut children_of: HashMap<String, Vec<Value>> = HashMap::new();
    // (parent id, depth its children are reported at)
    let mut frontier: Vec<(String, usize)> = vec![(String::new(), 0)];

    while !frontier.is_empty() {
        let results = bounded_map(frontier, fanout_limit(), |(parent_id, depth)| async move {
            let items = if parent_id.is_empty() {
                course_contents(c, course_id).await
            } else {
                content_children(c, course_id, &parent_id).await
            };
            (parent_id, depth, items)
        })
        .await;

        let mut next: Vec<(String, usize)> = Vec::new();
        for (parent_id, depth, items) in results {
            let Ok(items) = items else { continue };
            let mut kept: Vec<Value> = Vec::with_capacity(items.len());
            for raw in items {
                let item = if raw.is_null() { json!({}) } else { raw };
                let Some(cid) = item
                    .get("id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                else {
                    continue;
                };
                if cid.is_empty() || !seen.insert(cid.clone()) {
                    continue;
                }
                // A folder at `depth` is expanded only when its children are
                // still inside the depth budget (same rule as the old DFS).
                if is_folder(&item) && depth + 1 <= max_depth {
                    next.push((cid, depth + 1));
                }
                kept.push(item);
            }
            children_of.insert(parent_id, kept);
        }
        frontier = next;
    }

    fn push_preorder(
        parent: &str,
        depth: usize,
        breadcrumb: &[String],
        children_of: &HashMap<String, Vec<Value>>,
        out: &mut Vec<ContentNode>,
    ) {
        let Some(items) = children_of.get(parent) else {
            return;
        };
        for item in items {
            let title = item_title(item);
            let mut crumb = breadcrumb.to_vec();
            crumb.push(title);
            out.push(ContentNode {
                item: item.clone(),
                breadcrumb: crumb.clone(),
                depth,
                parent: parent.to_string(),
            });
            if is_folder(item) {
                if let Some(cid) = item.get("id").and_then(|v| v.as_str()) {
                    push_preorder(cid, depth + 1, &crumb, children_of, out);
                }
            }
        }
    }

    let mut nodes = Vec::new();
    push_preorder("", 0, &[], &children_of, &mut nodes);
    nodes
}

fn search_add(
    matches: &mut Vec<Value>,
    seen: &mut HashSet<String>,
    lowered: &str,
    course_id: &str,
    item: &Value,
    breadcrumb: Vec<String>,
) {
    let title = item_title(item);
    let desc = item_description(item);
    if !title.to_lowercase().contains(lowered) && !desc.to_lowercase().contains(lowered) {
        return;
    }
    let cid = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
    if seen.contains(cid) {
        return;
    }
    seen.insert(cid.to_string());
    matches.push(json!({
        "courseId": course_id,
        "id": item.get("id").cloned().unwrap_or(Value::Null),
        "title": title,
        "kind": if is_folder(item) { "folder" } else { "file" },
        "breadcrumb": breadcrumb,
        "modified": item.get("modified").and_then(|v| v.as_str()).unwrap_or(""),
        "description": desc,
    }));
}

/// search_course — server-side contents?search= first, client-side walk fallback.
async fn search_course(
    c: &NTULearnClient,
    course_id: &str,
    query: &str,
    max_depth: usize,
) -> Vec<Value> {
    let lowered = query.to_lowercase();
    let mut matches: Vec<Value> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    let server_matches = course_search(c, course_id, query).await;
    if !server_matches.is_empty() {
        for sm in &server_matches {
            let item = if sm.is_null() { json!({}) } else { sm.clone() };
            search_add(
                &mut matches,
                &mut seen,
                &lowered,
                course_id,
                &item,
                vec![item_title(&item)],
            );
        }
        if !matches.is_empty() {
            return matches;
        }
    }

    for node in walk_content(c, course_id, max_depth).await {
        search_add(
            &mut matches,
            &mut seen,
            &lowered,
            course_id,
            &node.item,
            node.breadcrumb,
        );
    }
    matches
}

// ---------------------------------------------------------------------------
// Download machinery (handlers.py)
// ---------------------------------------------------------------------------

struct DownloadJob {
    course_id: String,
    course_folder: String,
    content_title: String,
    url: String,
    raw_name: String,
    safe_name: String,
    target_name: String,
}

/// _collect_download_jobs — walk a course, resolve every file attachment.
async fn collect_download_jobs(
    c: &NTULearnClient,
    course_id: &str,
    max_depth: usize,
) -> Vec<DownloadJob> {
    let mut jobs: Vec<DownloadJob> = Vec::new();
    let course_raw = course(c, course_id).await.unwrap_or_else(|_| json!({}));
    let course_name = parsers::safe_folder_name(
        course_raw
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| course_raw.get("displayName").and_then(|v| v.as_str()))
            .unwrap_or(course_id),
    );
    let course_folder = format!("{course_id} - {course_name}");
    let file_nodes: Vec<ContentNode> = walk_content(c, course_id, max_depth)
        .await
        .into_iter()
        .filter(|n| is_file_item(&n.item))
        .collect();

    // Resolve (attachments, download URL) for every file concurrently. This
    // used to cost two sequential round trips per attachment; a 100-file
    // course spent ~200 RTTs here before any bytes were downloaded.
    let resolved = bounded_map(file_nodes, fanout_limit(), |node| async move {
        let item = node.item;
        let content_id = item
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if content_id.is_empty() {
            return Vec::new();
        }
        let atts = attachments(c, course_id, &content_id).await;
        let resolved_atts = bounded_map(atts, fanout_limit(), |att| {
            let content_id = content_id.clone();
            async move {
                let att_id = att
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let url = if att_id.is_empty() {
                    String::new()
                } else {
                    attachment_download_url(c, course_id, &content_id, &att_id)
                        .await
                        .unwrap_or_default()
                };
                (att, url)
            }
        })
        .await;
        resolved_atts
            .into_iter()
            .map(|(att, url)| (item.clone(), att, url))
            .collect::<Vec<_>>()
    })
    .await;

    for per_file in resolved {
        for (item, att, url) in per_file {
            if url.is_empty() {
                continue;
            }
            let raw_name = att
                .get("fileName")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| item_title(&item));
            let raw_name = if raw_name.is_empty() {
                "file".to_string()
            } else {
                raw_name
            };
            jobs.push(DownloadJob {
                course_id: course_id.to_string(),
                course_folder: course_folder.clone(),
                content_title: item_title(&item),
                url,
                raw_name: raw_name.clone(),
                safe_name: parsers::sanitize_filename(&raw_name),
                target_name: String::new(),
            });
        }
    }
    jobs
}

fn extension(filename: &str) -> String {
    parsers::file_extension(filename)
}

fn parse_extensions(raw: Option<&Value>) -> Option<HashSet<String>> {
    let raw = raw?;
    let text = match raw {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let mut set = HashSet::new();
    for p in text.split(',') {
        let p = p.trim().trim_start_matches('.').to_lowercase();
        if !p.is_empty() {
            set.insert(p);
        }
    }
    if set.is_empty() { None } else { Some(set) }
}

async fn download_worker(
    c: &NTULearnClient,
    job: &DownloadJob,
    dest_root: &Path,
    skip_existing: bool,
    results: &mut Vec<Value>,
    skipped: &mut Vec<Value>,
    ext_filter: Option<&HashSet<String>>,
) {
    let folder = dest_root.join(&job.course_folder);
    let _ = tokio::fs::create_dir_all(&folder).await;
    let name = job.target_name.clone();

    if let Some(filter) = ext_filter {
        if !filter.contains(&extension(&name)) {
            skipped.push(json!({
                "filename": name,
                "courseFolder": job.course_folder,
                "reason": "extension_filter",
            }));
            return;
        }
    }
    if skip_existing && folder.join(&name).exists() {
        skipped.push(json!({
            "filename": name,
            "courseFolder": job.course_folder,
            "reason": "already_exists",
        }));
        return;
    }
    let local_path = folder.join(&name);
    let Ok((content, _ct)) = c.download_bytes(&job.url).await else {
        skipped.push(json!({
            "filename": name,
            "courseFolder": job.course_folder,
            "reason": "download_failed: ClientError",
        }));
        return;
    };
    if tokio::fs::write(&local_path, &content).await.is_err() {
        skipped.push(json!({
            "filename": name,
            "courseFolder": job.course_folder,
            "reason": "download_failed: IoError",
        }));
        return;
    }
    results.push(json!({
        "filename": name,
        "courseFolder": job.course_folder,
        "localPath": local_path.to_string_lossy().to_string(),
        "sizeBytes": content.len(),
    }));
}

// ---------------------------------------------------------------------------
// Gradebook helpers (handlers.py)
// ---------------------------------------------------------------------------

fn column_name(col: &Value) -> String {
    col.get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            col.get("displayName")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
        })
        .or_else(|| col.get("id").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string()
}

fn coerce_f64(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.parse::<f64>().ok(),
        _ => None,
    }
}

/// _column_possible — `score.possible` (dict) else `col.possible`.
fn column_possible(col: &Value) -> Option<f64> {
    let score = col.get("score");
    let possible = match score {
        Some(Value::Object(o)) => o.get("possible").cloned(),
        _ => None,
    };
    let possible = possible.or_else(|| col.get("possible").cloned());
    coerce_f64(possible.as_ref())
}

/// _grade_score — `score.score` else `score.value`; number or numeric string.
fn grade_score(grade: &Value) -> Option<f64> {
    let score = grade.get("score");
    let raw = match score {
        Some(Value::Object(o)) => o.get("score").or_else(|| o.get("value")).cloned(),
        _ => score.cloned(),
    };
    coerce_f64(raw.as_ref())
}

/// _grade_brief — own-grade summary: columnCount / columnsWithScore /
/// totalPossible / averagePercent (the per-column `scored` list in the Python
/// original is built but not returned; we do the same).
async fn grade_brief(c: &NTULearnClient, course_id: &str, user_id: Option<&str>) -> Value {
    let columns = match gradebook_columns(c, course_id).await {
        Ok(cols) => cols,
        Err(_) => return json!({}),
    };
    // The grades payload is per-course, not per-column — fetch it once
    // instead of once per column.
    let grades = match user_id {
        Some(uid) => user_grades(c, course_id, uid).await.unwrap_or_default(),
        None => Vec::new(),
    };
    let mut graded = 0usize;
    let mut total_possible = 0.0f64;
    let mut earned = 0.0f64;
    for col in &columns {
        let Some(col_id) = col.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let possible = column_possible(col);
        total_possible += possible.unwrap_or(0.0);
        let own = grades
            .iter()
            .find(|g| g.get("columnId").and_then(|v| v.as_str()) == Some(col_id));
        if let Some(own_grade) = own {
            let score = grade_score(own_grade);
            if let (Some(sc), Some(ps)) = (score, possible) {
                if ps > 0.0 {
                    graded += 1;
                    earned += sc;
                }
            }
        }
    }
    let mut out = Map::new();
    out.insert("columnCount".to_string(), Value::from(columns.len()));
    out.insert("columnsWithScore".to_string(), Value::from(graded));
    out.insert(
        "totalPossible".to_string(),
        Value::from((total_possible * 100.0).round() / 100.0),
    );
    if graded > 0 && total_possible > 0.0 {
        out.insert(
            "averagePercent".to_string(),
            Value::from(((100.0 * earned / total_possible) * 10.0).round() / 10.0),
        );
    }
    Value::Object(out)
}

// ---------------------------------------------------------------------------
// Content summary builder (handlers.py build_course_summary)
// ---------------------------------------------------------------------------

async fn build_course_summary(
    c: &NTULearnClient,
    course_id: &str,
    include_contents: bool,
) -> Value {
    let mut summary = Map::new();
    summary.insert("courseId".to_string(), Value::String(course_id.to_string()));
    summary.insert("courseErrors".to_string(), Value::Array(Vec::new()));

    // Course + term
    let course_result = course(c, course_id).await;
    match &course_result {
        Ok(course) => {
            let name = course
                .get("name")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .or_else(|| course.get("displayName").and_then(|v| v.as_str()))
                .unwrap_or(course_id);
            summary.insert("title".to_string(), Value::String(name.to_string()));
            let desc = course.get("description");
            let desc_text = match desc {
                Some(Value::String(_)) => item_description(course),
                _ => String::new(),
            };
            summary.insert("description".to_string(), Value::String(desc_text));
            let term_id = course.get("termId").and_then(|v| v.as_str()).unwrap_or("");
            if !term_id.is_empty() {
                match term(c, term_id).await {
                    Ok(term_raw) => {
                        summary.insert(
                            "term".to_string(),
                            json!({
                                "id": term_id,
                                "name": term_raw.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                                "start": term_raw.get("startDate").and_then(|v| v.as_str()).unwrap_or(""),
                                "end": term_raw.get("endDate").and_then(|v| v.as_str()).unwrap_or(""),
                            }),
                        );
                    }
                    Err(e) => {
                        if let Value::Array(errs) = summary.get_mut("courseErrors").unwrap() {
                            errs.push(json!({"section": "term", "error": e}));
                        }
                    }
                }
            }
        }
        Err(e) => {
            if let Value::Array(errs) = summary.get_mut("courseErrors").unwrap() {
                errs.push(json!({"section": "course", "error": e}));
            }
        }
    }

    // Roster + enrollment count
    match course_users(c, course_id).await {
        Ok(users) => {
            summary.insert("enrollmentCount".to_string(), Value::from(users.len()));
            let mut instructors: Vec<Value> = Vec::new();
            for u in users.iter().take(1000) {
                if INSTRUCTOR_ROLES.contains(&user_role(u).as_str()) {
                    // Resolve id/name through the same nested `user.*` unwrapping
                    // as render_user_row (the public REST API nests the user
                    // object under `user`, with userId on the outer object).
                    let row = render_user_row(u);
                    instructors.push(json!({
                        "id": row.get("id").cloned().unwrap_or(Value::Null),
                        "name": row.get("name").cloned().unwrap_or(Value::String(String::new())),
                    }));
                    if instructors.len() >= 10 {
                        break;
                    }
                }
            }
            summary.insert("instructors".to_string(), Value::Array(instructors));
        }
        Err(e) => {
            if let Value::Array(errs) = summary.get_mut("courseErrors").unwrap() {
                errs.push(json!({"section": "roster", "error": e}));
            }
        }
    }

    // Calendar (upcoming)
    match calendar_items(c, Some(course_id), None, None, None).await {
        Ok(cal) => {
            let mut upcoming: Vec<Value> = Vec::new();
            for i in cal.iter().take(1000) {
                upcoming.push(calendar_brief(i));
                if upcoming.len() >= 10 {
                    break;
                }
            }
            summary.insert("upcoming".to_string(), Value::Array(upcoming));
        }
        Err(e) => {
            if let Value::Array(errs) = summary.get_mut("courseErrors").unwrap() {
                errs.push(json!({"section": "calendar", "error": e}));
            }
        }
    }

    // Announcements
    match announcements(c, course_id).await {
        Ok(anns) => {
            let mut recent: Vec<Value> = Vec::new();
            for a in anns.iter().take(1000) {
                recent.push(json!({
                    "id": a.get("id").cloned().unwrap_or(Value::Null),
                    "title": a.get("title").cloned().unwrap_or(Value::Null),
                    "created": a.get("created").cloned().unwrap_or(Value::Null),
                }));
                if recent.len() >= 5 {
                    break;
                }
            }
            summary.insert("recentAnnouncements".to_string(), Value::Array(recent));
        }
        Err(e) => {
            if let Value::Array(errs) = summary.get_mut("courseErrors").unwrap() {
                errs.push(json!({"section": "announcements", "error": e}));
            }
        }
    }

    // Own user id + grade summary
    let user_id = me_user_id(c).await.ok().flatten();
    let gb = grade_brief(c, course_id, user_id.as_deref()).await;
    summary.insert("gradeSummary".to_string(), gb);

    // Top-level content folders
    if include_contents {
        match course_contents(c, course_id).await {
            Ok(root) => {
                let mut tops: Vec<Value> = Vec::new();
                for item in root.iter().take(1000) {
                    tops.push(json!({
                        "id": item.get("id").cloned().unwrap_or(Value::Null),
                        "title": item_title(item),
                        "hasChildren": is_folder(item),
                    }));
                    if tops.len() >= 20 {
                        break;
                    }
                }
                summary.insert("contentTopFolders".to_string(), Value::Array(tops));
            }
            Err(e) => {
                if let Value::Array(errs) = summary.get_mut("courseErrors").unwrap() {
                    errs.push(json!({"section": "contents", "error": e}));
                }
            }
        }
    }

    Value::Object(summary)
}

// ---------------------------------------------------------------------------
// ICS / CSV builders (handlers.py)
// ---------------------------------------------------------------------------

fn ics_escape(text: Option<&Value>) -> String {
    let raw = match text {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    };
    raw.replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace('\n', "\\n")
}

fn ics_dt(value: Option<&Value>) -> String {
    let Some(raw) = value.and_then(|v| v.as_str()) else {
        return "19700101T000000Z".to_string();
    };
    if raw.is_empty() {
        return "19700101T000000Z".to_string();
    }
    let parsed = chrono::DateTime::parse_from_rfc3339(raw)
        .or_else(|_| {
            // naive ISO without offset -> assume UTC
            chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S%.f").map(|nd| {
                chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(nd, chrono::Utc)
                    .fixed_offset()
            })
        })
        .map(|dt| dt.with_timezone(&chrono::Utc));
    match parsed {
        Ok(dt) => dt.format("%Y%m%dT%H%M%SZ").to_string(),
        Err(_) => "19700101T000000Z".to_string(),
    }
}

fn build_ics(items: &[Value], _scope: &str) -> String {
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".to_string(),
        "PRODID:-//ntulearn-mcp//NTULearn calendar export//EN".to_string(),
        "CALSCALE:GREGORIAN".to_string(),
    ];
    for item in items {
        let uid = item
            .get("uid")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                format!(
                    "{}-{}",
                    item.get("courseId").and_then(|v| v.as_str()).unwrap_or("x"),
                    item.get("id").and_then(|v| v.as_str()).unwrap_or("x"),
                )
            });
        let title = ics_escape(Some(&json!(
            item.get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("Untitled event")
        )));
        let desc = ics_escape(item.get("description"));
        let location = ics_escape(item.get("location"));
        let start = ics_dt(item.get("start"));
        let end = {
            let e = ics_dt(item.get("end"));
            if e == "19700101T000000Z" {
                start.clone()
            } else {
                e
            }
        };
        let fetched_value = match item.get("fetchedAt") {
            Some(v) if !(v.is_null() || v.as_str().map(|s| s.is_empty()).unwrap_or(false)) => {
                v.clone()
            }
            _ => json!(now_iso()),
        };
        let fetched = ics_dt(Some(&fetched_value));
        lines.push("BEGIN:VEVENT".to_string());
        lines.push(format!("UID:{uid}"));
        lines.push(format!("DTSTAMP:{fetched}"));
        lines.push(format!("DTSTART:{start}"));
        lines.push(format!("DTEND:{end}"));
        lines.push(format!("SUMMARY:{title}"));
        if !desc.is_empty() {
            lines.push(format!("DESCRIPTION:{desc}"));
        }
        if !location.is_empty() {
            lines.push(format!("LOCATION:{location}"));
        }
        lines.push("END:VEVENT".to_string());
    }
    lines.push("END:VCALENDAR".to_string());
    lines.join("\r\n")
}

fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn build_gradebook_csv(rows: &[Value]) -> String {
    let headers = [
        "courseId",
        "columnId",
        "columnName",
        "possible",
        "score",
        "status",
        "grade",
    ];
    let mut out = String::new();
    out.push_str(&headers.join(","));
    for row in rows {
        let mut cells = Vec::with_capacity(headers.len());
        for h in headers {
            let v = row.get(h);
            let cell = match v {
                Some(Value::String(s)) => csv_escape(s),
                Some(Value::Number(n)) => n.to_string(),
                Some(Value::Bool(b)) => csv_escape(&b.to_string()),
                Some(Value::Null) | None => String::new(),
                Some(other) => csv_escape(&serde_json::to_string(other).unwrap_or_default()),
            };
            cells.push(cell);
        }
        out.push_str(&cells.join(","));
    }
    out
}

// ---------------------------------------------------------------------------
// Destination resolution (server._resolve_destination_dir / download_course)
// ---------------------------------------------------------------------------

fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~") {
        if let Some(home) = dirs::home_dir() {
            if rest.is_empty() {
                return home;
            }
            return home.join(rest.trim_start_matches('/'));
        }
    }
    PathBuf::from(p)
}

fn resolve_destination_dir(raw: Option<&Value>) -> Result<PathBuf, String> {
    if let Some(raw) = raw {
        let s = match raw {
            Value::String(s) => s.clone(),
            _ => return Err("destination_dir must be a string".to_string()),
        };
        let candidate = s.trim().to_string();
        if candidate.is_empty() {
            return Err("destination_dir cannot be empty".to_string());
        }
        return Ok(expand_tilde(&candidate));
    }
    if let Ok(env_val) = std::env::var("NTULEARN_DOWNLOAD_DIR") {
        if !env_val.trim().is_empty() {
            return Ok(expand_tilde(&env_val));
        }
    }
    Ok(PathBuf::from("./downloads"))
}

/// Lexical `Path.resolve()` (Python resolve(strict=False)): absolutise,
/// collapse `.`/`..`, without touching the filesystem.
fn path_abs(dest: &Path) -> PathBuf {
    let dest = expand_tilde(&dest.to_string_lossy());
    let mut out = std::path::PathBuf::new();
    for comp in dest.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !out.pop() {
                    // reached root; keep as-is (Python would resolve .. at root to root)
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.is_relative() {
        match std::env::current_dir() {
            Ok(cwd) => {
                let joined = cwd.join(out);
                // re-collapse
                return path_abs(&joined);
            }
            Err(_) => return out,
        }
    }
    out
}

fn sanitize_filename_quick(name: &str) -> String {
    name.replace(['\\', '/', '*', '?', ':', '"', '<', '>', '|'], "_")
}

fn rpartition(s: &str, sep: char) -> Option<(&str, &str)> {
    match s.rfind(sep) {
        Some(i) => Some((&s[..i], &s[i + 1..])),
        None => None,
    }
}

fn deduplicate_filename(name: &str, used: &HashSet<String>, dest_dir: &Path) -> String {
    let mut candidate = name.to_string();
    let (stem, dot, ext) = match rpartition(name, '.') {
        Some((base, e)) => (base.to_string(), true, e.to_string()),
        None => (name.to_string(), false, String::new()),
    };
    let mut n = 2usize;
    while used.contains(&candidate) || dest_dir.join(&candidate).exists() {
        candidate = if dot {
            format!("{stem} ({n}).{ext}")
        } else {
            format!("{stem} ({n})")
        };
        n += 1;
    }
    candidate
}

// ---------------------------------------------------------------------------
// Server-local tools (server.py)
// ---------------------------------------------------------------------------

async fn h_list_courses(state: &AppState, args: &Value) -> Result<Value, String> {
    let include_disabled = bool_arg(args, "include_disabled", false);
    let (offset, limit) = resolve_pagination_args(args)?;
    let _fmt = response_format(args)?;

    let enrollments = me_enrollments(&state.client).await?;
    let mut enrollments = enrollments;
    if !include_disabled {
        enrollments.retain(|e| {
            e.get("availability")
                .and_then(|a| a.get("available"))
                .and_then(|v| v.as_str())
                == Some("Yes")
        });
    }

    let course_ids: Vec<String> = enrollments
        .iter()
        .filter_map(|e| {
            e.get("courseId")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .collect();

    if course_ids.is_empty() {
        let (_, meta) = slice_with_pagination(&[], offset, limit);
        return Ok(
            json!({"courses": [], "total": meta["total"], "count": meta["count"],
            "offset": meta["offset"], "limit": meta["limit"], "hasMore": meta["hasMore"],
            "nextOffset": meta["nextOffset"]}),
        );
    }

    let mut last_accessed_map: HashMap<String, Value> = HashMap::new();
    let mut availability_map: HashMap<String, String> = HashMap::new();
    for e in &enrollments {
        if let Some(cid) = e.get("courseId").and_then(|v| v.as_str()) {
            last_accessed_map.insert(
                cid.to_string(),
                e.get("lastAccessed").cloned().unwrap_or(Value::Null),
            );
            let avail = e
                .get("availability")
                .and_then(|a| a.get("available"))
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown");
            availability_map.insert(cid.to_string(), avail.to_string());
        }
    }

    let courses_raw = courses_batch(&state.client, &course_ids).await;
    let mut rows: Vec<Value> = Vec::new();
    for course in &courses_raw {
        let cid = course.get("id").and_then(|v| v.as_str()).unwrap_or("");
        rows.push(json!({
            "courseId": course.get("id").cloned().unwrap_or(Value::Null),
            "title": course.get("name").and_then(|v| v.as_str()).filter(|s| !s.is_empty())
                .or_else(|| course.get("displayName").and_then(|v| v.as_str()))
                .or_else(|| course.get("id").and_then(|v| v.as_str()))
                .unwrap_or("")
                .to_string(),
            "available": availability_map.get(cid).cloned().unwrap_or_else(|| "Unknown".to_string()),
            "lastAccessed": last_accessed_map.get(cid).cloned().unwrap_or(Value::Null),
        }));
    }
    // rows.sort(key=lambda c: c["lastAccessed"] or "", reverse=True)
    rows.sort_by(|a, b| {
        let la = a.get("lastAccessed").and_then(|v| v.as_str()).unwrap_or("");
        let lb = b.get("lastAccessed").and_then(|v| v.as_str()).unwrap_or("");
        lb.cmp(la)
    });

    let (page, meta) = slice_with_pagination(&rows, offset, limit);
    let mut payload = Map::new();
    payload.insert("courses".to_string(), Value::Array(page));
    merge_meta(&mut payload, meta);
    Ok(Value::Object(payload))
}

fn merge_meta(payload: &mut Map<String, Value>, meta: Value) {
    if let Value::Object(m) = meta {
        for (k, v) in m {
            payload.insert(k, v);
        }
    }
}

async fn h_get_course_contents(state: &AppState, args: &Value) -> Result<Value, String> {
    let course_id = str_arg(args, "course_id").unwrap_or("").to_string();
    let parent_id = str_arg(args, "parent_id").map(|s| s.to_string());
    let (offset, limit) = resolve_pagination_args(args)?;
    let _fmt = response_format(args)?;

    let items: Vec<Value> = match &parent_id {
        Some(pid) => content_children(&state.client, &course_id, pid).await?,
        None => course_contents(&state.client, &course_id).await?,
    };
    let stripped: Vec<Value> = items.iter().map(strip_content).collect();
    let (page, meta) = slice_with_pagination(&stripped, offset, limit);
    let mut payload = Map::new();
    payload.insert("items".to_string(), Value::Array(page));
    merge_meta(&mut payload, meta);
    Ok(Value::Object(payload))
}

async fn h_search_course_content(state: &AppState, args: &Value) -> Result<Value, String> {
    let course_id = str_arg(args, "course_id").unwrap_or("").to_string();
    let query = str_arg(args, "query").unwrap_or("").trim().to_lowercase();
    if query.is_empty() {
        return Err("search query cannot be blank.".to_string());
    }
    let max_depth = parse_int_arg(args, "max_depth", 5);
    let max_results = parse_int_arg(args, "max_results", 50);
    if max_depth < 1 || max_depth > MAX_DEPTH as i64 {
        return Err(format!("max_depth must be 1..{MAX_DEPTH}"));
    }
    if max_results < 1 || max_results > MAX_LIMIT as i64 {
        return Err(format!("max_results must be 1..{MAX_LIMIT}"));
    }
    let _fmt = response_format(args)?;

    let matches: Vec<Value> = Vec::new();
    let visited: HashSet<String> = HashSet::new();

    let top_level = course_contents(&state.client, &course_id).await?;
    // Sequential walk (mirrors the gather/semaphore version's payload exactly).
    // Owned-state recursion (Box::pin) — same pre-order walk as the Python
    // recursive gather, but without an infinitely-sized future.
    type WalkFuture<'b> = std::pin::Pin<
        Box<dyn std::future::Future<Output = (Vec<Value>, HashSet<String>)> + Send + 'b>,
    >;

    #[allow(clippy::too_many_arguments)] // state threads through the recursion by design
    fn rec_walk<'b>(
        c: &'b NTULearnClient,
        course_id: &'b str,
        query: &'b str,
        items: Vec<Value>,
        path: Vec<String>,
        depth: usize,
        max_depth: i64,
        max_results: i64,
        matches: Vec<Value>,
        visited: HashSet<String>,
    ) -> WalkFuture<'b> {
        Box::pin(async move {
            if depth > max_depth as usize || (matches.len() as i64) >= max_results {
                return (matches, visited);
            }
            let mut matches = matches;
            let mut visited = visited;
            for item in &items {
                if (matches.len() as i64) >= max_results {
                    break;
                }
                let item_id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
                if !item_id.is_empty() {
                    if visited.contains(item_id) {
                        continue;
                    }
                    visited.insert(item_id.to_string());
                }
                let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("");
                let desc_raw = item.get("description");
                let desc = match desc_raw {
                    Some(Value::Object(o)) => {
                        o.get("rawText").and_then(|v| v.as_str()).unwrap_or("")
                    }
                    Some(Value::String(s)) => s.as_str(),
                    _ => "",
                }
                .to_string();
                let mut current_path = path.clone();
                current_path.push(title.to_string());
                if query.contains(title.to_lowercase().as_str())
                    || query.contains(desc.to_lowercase().as_str())
                {
                    let mut stripped = strip_content(item);
                    if let Some(s) = stripped.as_object_mut() {
                        s.insert(
                            "breadcrumb".to_string(),
                            Value::Array(
                                current_path
                                    .iter()
                                    .map(|x| Value::String(x.clone()))
                                    .collect(),
                            ),
                        );
                    }
                    matches.push(stripped);
                }
                let has_children = item
                    .get("hasChildren")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if has_children && (matches.len() as i64) < max_results && !item_id.is_empty() {
                    if let Ok(children) = content_children(c, course_id, item_id).await {
                        let (m2, v2) = rec_walk(
                            c,
                            course_id,
                            query,
                            children,
                            current_path,
                            depth + 1,
                            max_depth,
                            max_results,
                            matches,
                            visited,
                        )
                        .await;
                        matches = m2;
                        visited = v2;
                    }
                }
            }
            (matches, visited)
        })
    }

    let (matches, _visited) = rec_walk(
        &state.client,
        &course_id,
        &query,
        top_level,
        Vec::new(),
        0,
        max_depth,
        max_results,
        matches,
        visited,
    )
    .await;

    Ok(json!({"matches": matches, "count": matches.len()}))
}

// ---------------------------------------------------------------------------
// _resolve_content_files + download_file
// ---------------------------------------------------------------------------

async fn resolve_content_files(
    c: &NTULearnClient,
    course_id: &str,
    content_id: &str,
) -> (Value, Option<String>, Vec<(String, Option<String>)>) {
    // Body-inclusive fetch: x-bb-file attachments cover files, but every
    // other handler type (e.g. resource/x-bb-document pages) embeds its file
    // links in the body HTML, which the default contents field set omits.
    let item = c
        .get_content_item_with_body(course_id, content_id, None)
        .await
        .unwrap_or_else(|_| json!({}));
    let handler_id = item
        .get("contentHandler")
        .and_then(|h| h.get("id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let mut pairs: Vec<(String, Option<String>)> = Vec::new();
    if handler_id.as_deref() == Some("resource/x-bb-file") {
        let atts = attachments(c, course_id, content_id).await;
        // Resolve every attachment URL concurrently (was one RTT each).
        let resolved = bounded_map(atts, fanout_limit(), |att| async move {
            let att_id = att
                .get("id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let url = match &att_id {
                Some(id) => attachment_download_url(c, course_id, content_id, id)
                    .await
                    .unwrap_or_default(),
                None => String::new(),
            };
            let fname = att
                .get("fileName")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            (url, fname)
        })
        .await;
        for (url, fname) in resolved {
            if !url.is_empty() {
                pairs.push((url, fname));
            }
        }
    } else {
        let body = item.get("body");
        let body_s = match body {
            Some(Value::String(s)) => Some(s.clone()),
            _ => None,
        };
        let mut files = match &body_s {
            Some(b) => parsers::extract_all_files(b),
            None => Vec::new(),
        };
        if files.is_empty() {
            let desc = item.get("description");
            let body2 = match desc {
                Some(Value::Object(o)) => o
                    .get("rawText")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                Some(Value::String(s)) => Some(s.clone()),
                _ => None,
            };
            if let Some(b) = body2 {
                files = parsers::extract_all_files(&b);
            }
        }
        for f in files {
            if let Some(url) = f.get("url").and_then(|v| v.as_str()) {
                let fname = f
                    .get("filename")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .filter(|s| !s.is_empty());
                pairs.push((url.to_string(), fname));
            }
        }
    }
    (item, handler_id, pairs)
}

async fn h_download_file(state: &AppState, args: &Value) -> Result<Value, String> {
    let course_id = str_arg(args, "course_id").unwrap_or("").to_string();
    let content_id = str_arg(args, "content_id").unwrap_or("").to_string();
    let _fmt = response_format(args)?;
    let dest_dir = resolve_destination_dir(args.get("destination_dir"))?;

    let (item, handler_id, pairs) =
        resolve_content_files(&state.client, &course_id, &content_id).await;

    if pairs.is_empty() {
        let payload = json!({
            "contentId": content_id,
            "title": item.get("title").cloned().unwrap_or(Value::Null),
            "contentHandlerId": handler_id,
            "files": [],
            "destinationDir": dest_dir.to_string_lossy().to_string(),
            "error": "No download URL found. Content handler type may not be supported.",
        });
        return Ok(payload);
    }

    if let Err(e) = tokio::fs::create_dir_all(&dest_dir).await {
        return Err(format!("{e}"));
    }

    // Assign unique local filenames first (serial, deterministic), then fetch
    // the files concurrently — a multi-file week page lands in a fraction of
    // the wall time. The first failure in file order still fails the call;
    // other in-flight files may already have been written, same as before.
    let mut used_names: HashSet<String> = HashSet::new();
    let mut plan: Vec<(String, String)> = Vec::with_capacity(pairs.len());
    for (url, detected_filename) in &pairs {
        let filename = match detected_filename {
            Some(f) => f.clone(),
            None => {
                let url_path = url.split('?').next().unwrap_or("");
                let last = url_path.rsplit('/').next().unwrap_or("");
                let decoded = percent_encoding::percent_decode_str(last)
                    .decode_utf8_lossy()
                    .to_string();
                if decoded.is_empty() {
                    "download".to_string()
                } else {
                    decoded
                }
            }
        };
        let filename = sanitize_filename_quick(&filename);
        let filename = deduplicate_filename(&filename, &used_names, &dest_dir);
        used_names.insert(filename.clone());
        plan.push((url.clone(), filename));
    }

    let downloaded = bounded_map(plan, fanout_limit(), |(url, filename)| {
        let client = &state.client;
        async move {
            let res = client.download_bytes(&url).await;
            (filename, res)
        }
    })
    .await;

    let mut saved: Vec<Value> = Vec::new();
    for (filename, res) in downloaded {
        let dest = dest_dir.join(&filename);
        let (content, _ct) = match res {
            Ok(t) => t,
            Err(e) => return Err(format!("download failed: {e}")),
        };
        if let Err(e) = tokio::fs::write(&dest, &content).await {
            return Err(format!("{e}"));
        }
        saved.push(json!({
            "localPath": dest.to_string_lossy().to_string(),
            "filename": filename,
            "sizeBytes": content.len(),
        }));
    }

    let resolved_dest = path_abs(&dest_dir).to_string_lossy().to_string();
    Ok(json!({
        "contentId": content_id,
        "title": item.get("title").cloned().unwrap_or(Value::Null),
        "files": saved,
        "destinationDir": resolved_dest,
    }))
}

async fn coerce_course_ids(args: &Value, c: &NTULearnClient) -> Result<Vec<String>, String> {
    let raw = args.get("course_ids");
    let none_or_empty = match raw {
        None => true,
        Some(Value::Array(a)) => a.is_empty(),
        Some(_) => false,
    };
    if none_or_empty {
        return resolve_enrolled_course_ids(c, false).await;
    }
    match raw {
        Some(Value::Array(a)) => {
            let mut ids = Vec::with_capacity(a.len());
            for v in a {
                match v {
                    Value::String(s) => ids.push(s.clone()),
                    other => ids.push(other.to_string()),
                }
            }
            Ok(ids)
        }
        _ => Err("course_ids must be a list of strings".to_string()),
    }
}

async fn h_read_file_content(state: &AppState, args: &Value) -> Result<Value, String> {
    let course_id = str_arg(args, "course_id").unwrap_or("").to_string();
    let content_id = str_arg(args, "content_id").unwrap_or("").to_string();
    let _fmt = response_format(args)?;
    let _pdf_mode = resolve_pdf_mode(args)?;
    let _pages = parse_page_range(args.get("pages")).unwrap_or(None);

    let (item, handler_id, pairs) =
        resolve_content_files(&state.client, &course_id, &content_id).await;

    if pairs.is_empty() {
        return Ok(json!({
            "contentId": content_id,
            "title": item.get("title").cloned().unwrap_or(Value::Null),
            "contentHandlerId": handler_id,
            "files": [],
            "skipped": [],
            "error": "No download URL found. Content handler type may not be supported.",
        }));
    }

    let mut files_out: Vec<Value> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();
    let mut total_bytes: u64 = 0;

    // Fetch every file first (bounded), then apply the size caps in order.
    // The old loop downloaded one file at a time; a page with 10 PDFs cost
    // 10 sequential round trips.
    let downloads = bounded_map(pairs.clone(), fanout_limit(), |(url, detected_filename)| {
        let c = &state.client;
        async move {
            let res = c.download_bytes(&url).await;
            (url, detected_filename, res)
        }
    })
    .await;

    for (url, detected_filename, res) in downloads {
        let mut filename = detected_filename.clone().unwrap_or_default();
        if filename.is_empty() {
            let url_path = url.split('?').next().unwrap_or("");
            let last = url_path.rsplit('/').next().unwrap_or("");
            let decoded = percent_encoding::percent_decode_str(last)
                .decode_utf8_lossy()
                .to_string();
            filename = if decoded.is_empty() {
                "download".to_string()
            } else {
                decoded
            };
        }

        if total_bytes >= MAX_TOTAL_BYTES {
            skipped.push(json!({
                "filename": filename,
                "reason": format!(
                    "Skipped: cumulative batch size already exceeds {} cap. Use ntulearn_download_file.",
                    parsers::format_bytes(MAX_TOTAL_BYTES),
                ),
            }));
            continue;
        }

        let (content_bytes, ct_raw) = match res {
            Ok(t) => t,
            Err(e) => return Err(format!("download failed: {e}")),
        };
        let size = content_bytes.len() as u64;
        let content_type: Option<String> = ct_raw;

        if size > MAX_FILE_BYTES {
            skipped.push(json!({
                "filename": filename,
                "reason": format!(
                    "File too large ({} > {} cap). Use ntulearn_download_file.",
                    parsers::format_bytes(size),
                    parsers::format_bytes(MAX_FILE_BYTES),
                ),
                "sizeBytes": size,
                "contentType": content_type,
            }));
            continue;
        }
        if total_bytes + size > MAX_TOTAL_BYTES {
            skipped.push(json!({
                "filename": filename,
                "reason": format!(
                    "Skipped: would exceed batch cap of {}. Use ntulearn_download_file.",
                    parsers::format_bytes(MAX_TOTAL_BYTES),
                ),
                "sizeBytes": size,
                "contentType": content_type,
            }));
            continue;
        }
        total_bytes += size;
        let entry = parsers::extract_content(&filename, content_type.as_deref(), &content_bytes);
        if entry.get("kind").and_then(|v| v.as_str()) == Some("binary") {
            skipped.push(json!({
                "filename": filename,
                "reason": entry.get("error").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                "sizeBytes": size,
                "contentType": content_type,
            }));
            continue;
        }
        files_out.push(entry);
    }

    Ok(json!({
        "contentId": content_id,
        "title": item.get("title").cloned().unwrap_or(Value::Null),
        "files": files_out,
        "skipped": skipped,
    }))
}

async fn h_get_upcoming(state: &AppState, args: &Value) -> Result<Value, String> {
    let since_val = args.get("since");
    let until_val = args.get("until");
    let since = match since_val {
        Some(v) => Some(validate_iso8601(
            v.as_str().ok_or("since must be a string")?,
            "since",
        )?),
        None => None,
    };
    let until = match until_val {
        Some(v) => Some(validate_iso8601(
            v.as_str().ok_or("until must be a string")?,
            "until",
        )?),
        None => None,
    };
    let item_type = args
        .get("type")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    if let Some(t) = &item_type {
        if !CALENDAR_ITEM_TYPES.contains(&t.as_str()) {
            return Err(format!(
                "type must be one of {CALENDAR_ITEM_TYPES:?}; got {t:?}"
            ));
        }
    }
    let course_ids = coerce_course_ids(args, &state.client).await?;
    let (offset, limit) = resolve_pagination_args(args)?;
    let _fmt = response_format(args)?;

    if course_ids.is_empty() {
        let (_, meta) = slice_with_pagination(&[], offset, limit);
        let mut payload = Map::new();
        payload.insert("items".to_string(), Value::Array(Vec::new()));
        merge_meta(&mut payload, meta);
        payload.insert("courseIdsQueried".to_string(), Value::Array(Vec::new()));
        payload.insert("courseErrors".to_string(), json!({}));
        return Ok(Value::Object(payload));
    }

    let mut items: Vec<Value> = Vec::new();
    let mut course_errors = Map::new();
    let per_course = bounded_map(course_ids.clone(), fanout_limit(), |cid| {
        let c = &state.client;
        let since = since.as_deref();
        let until = until.as_deref();
        let item_type = item_type.as_deref();
        async move {
            let res = calendar_items(c, Some(&cid), since, until, item_type).await;
            (cid, res)
        }
    })
    .await;
    for (cid, res) in per_course {
        match res {
            Ok(rows) => {
                for raw in rows {
                    items.push(strip_calendar_item(&raw, Some(&cid)));
                }
            }
            Err(e) => {
                course_errors.insert(cid, Value::String(e));
            }
        }
    }
    items.sort_by(|a, b| {
        let sa = a
            .get("start")
            .and_then(|v| v.as_str())
            .unwrap_or("\u{FFFF}");
        let sb = b
            .get("start")
            .and_then(|v| v.as_str())
            .unwrap_or("\u{FFFF}");
        sa.cmp(sb)
    });

    let (page, meta) = slice_with_pagination(&items, offset, limit);
    let mut payload = Map::new();
    payload.insert("items".to_string(), Value::Array(page));
    merge_meta(&mut payload, meta);
    payload.insert(
        "courseIdsQueried".to_string(),
        Value::Array(
            course_ids
                .iter()
                .map(|x| Value::String(x.clone()))
                .collect(),
        ),
    );
    payload.insert("courseErrors".to_string(), Value::Object(course_errors));
    Ok(Value::Object(payload))
}

async fn h_get_announcements(state: &AppState, args: &Value) -> Result<Value, String> {
    let since_val = args.get("since");
    let since = match since_val {
        Some(v) => Some(validate_iso8601(
            v.as_str().ok_or("since must be a string")?,
            "since",
        )?),
        None => None,
    };
    let course_ids = coerce_course_ids(args, &state.client).await?;
    let (offset, limit) = resolve_pagination_args(args)?;
    let _fmt = response_format(args)?;

    if course_ids.is_empty() {
        let (_, meta) = slice_with_pagination(&[], offset, limit);
        let mut payload = Map::new();
        payload.insert("announcements".to_string(), Value::Array(Vec::new()));
        merge_meta(&mut payload, meta);
        payload.insert("courseIdsQueried".to_string(), Value::Array(Vec::new()));
        payload.insert("courseErrors".to_string(), json!({}));
        return Ok(Value::Object(payload));
    }

    let mut rows: Vec<Value> = Vec::new();
    let mut course_errors = Map::new();
    let per_course = bounded_map(course_ids.clone(), fanout_limit(), |cid| {
        let c = &state.client;
        async move {
            let res = announcements(c, &cid).await;
            (cid, res)
        }
    })
    .await;
    for (cid, anns_res) in per_course {
        match anns_res {
            Ok(anns) => {
                for a in anns {
                    let body_raw = a.get("body");
                    let body_html = match body_raw {
                        Some(Value::Object(o)) => {
                            o.get("rawText").and_then(|v| v.as_str()).unwrap_or("")
                        }
                        Some(Value::String(s2)) => s2.as_str(),
                        _ => "",
                    };
                    let created = a.get("created").and_then(|v| v.as_str()).unwrap_or("");
                    if let Some(since) = &since {
                        if !created.is_empty() && created < since.as_str() {
                            continue;
                        }
                    }
                    rows.push(json!({
                        "id": a.get("id").cloned().unwrap_or(Value::Null),
                        "courseId": cid,
                        "title": a.get("title").cloned().unwrap_or(Value::Null),
                        "body": parsers::strip_html(body_html),
                        "created": a.get("created").cloned().unwrap_or(Value::Null),
                        "modified": a.get("modified").cloned().unwrap_or(Value::Null),
                        "available": a.get("availability").and_then(|v| v.get("available")).cloned().unwrap_or(Value::Null),
                    }));
                }
            }
            Err(e) => {
                course_errors.insert(cid.clone(), Value::String(e));
            }
        }
    }
    rows.sort_by(|a, b| {
        let ca = a.get("created").and_then(|v| v.as_str()).unwrap_or("");
        let cb = b.get("created").and_then(|v| v.as_str()).unwrap_or("");
        cb.cmp(ca)
    });
    let (page, meta) = slice_with_pagination(&rows, offset, limit);
    let mut payload = Map::new();
    payload.insert("announcements".to_string(), Value::Array(page));
    merge_meta(&mut payload, meta);
    payload.insert(
        "courseIdsQueried".to_string(),
        Value::Array(
            course_ids
                .iter()
                .map(|x| Value::String(x.clone()))
                .collect(),
        ),
    );
    payload.insert("courseErrors".to_string(), Value::Object(course_errors));
    Ok(Value::Object(payload))
}

async fn h_get_gradebook(state: &AppState, args: &Value) -> Result<Value, String> {
    let course_ids = coerce_course_ids(args, &state.client).await?;
    let (offset, limit) = resolve_pagination_args(args)?;
    let _fmt = response_format(args)?;

    let mut grades_available = true;
    let mut grade_fetch_error: Option<String> = None;
    let user_id = match me_user_id(&state.client).await {
        Ok(uid) => uid,
        Err(e) => {
            grades_available = false;
            grade_fetch_error = Some(e);
            None
        }
    };

    if course_ids.is_empty() {
        let (_, meta) = slice_with_pagination(&[], offset, limit);
        let mut payload = Map::new();
        payload.insert("columns".to_string(), Value::Array(Vec::new()));
        merge_meta(&mut payload, meta);
        payload.insert("gradesAvailable".to_string(), Value::Bool(grades_available));
        payload.insert(
            "gradeFetchError".to_string(),
            grade_fetch_error.map(Value::String).unwrap_or(Value::Null),
        );
        payload.insert("courseIdsQueried".to_string(), Value::Array(Vec::new()));
        payload.insert("courseErrors".to_string(), json!({}));
        return Ok(Value::Object(payload));
    }

    let mut columns_result: Vec<Value> = Vec::new();
    let mut course_errors = Map::new();
    let mut grade_errors: Vec<String> = Vec::new();
    if let Some(gfe) = &grade_fetch_error {
        grade_errors.push(gfe.clone());
    }
    // Fetch (columns, grades) for every course concurrently, then assemble
    // below in course order.
    let per_course = bounded_map(course_ids.clone(), fanout_limit(), |cid| {
        let c = &state.client;
        let uid = user_id.clone();
        async move {
            // Columns and grades are independent; fetching them together
            // halves this course's contribution to the command's latency.
            let (columns, grades) = tokio::join!(gradebook_columns(c, &cid), async {
                if let Some(uid) = &uid {
                    user_grades(c, &cid, uid).await
                } else {
                    Ok(Vec::new())
                }
            },);
            (cid, columns, grades)
        }
    })
    .await;
    for (cid, columns_res, grades_res) in per_course {
        let columns = match columns_res {
            Ok(cols) => cols,
            Err(e) => {
                course_errors.insert(cid, Value::String(e));
                continue;
            }
        };
        let mut per_course_grade_error: Option<String> = None;
        let grades_raw = match grades_res {
            Ok(gs) => gs,
            Err(e) => {
                per_course_grade_error = Some(e);
                Vec::new()
            }
        };
        if let Some(pge) = &per_course_grade_error {
            grade_errors.push(pge.clone());
        }
        let mut grade_map: HashMap<String, Value> = HashMap::new();
        for g in &grades_raw {
            if let Some(cid2) = g.get("columnId").and_then(|v| v.as_str()) {
                grade_map.insert(cid2.to_string(), g.clone());
            }
        }
        for col in &columns {
            let col_id = col.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let score_raw = col.get("score");
            let possible = match score_raw {
                Some(Value::Object(o)) => o.get("possible").cloned(),
                _ => None,
            };
            let grade_entry = grade_map.get(col_id);
            columns_result.push(json!({
                "id": col.get("id").cloned().unwrap_or(Value::Null),
                "courseId": cid,
                "name": col.get("name").cloned().unwrap_or(Value::Null),
                "displayName": col.get("displayName").cloned().unwrap_or(Value::Null),
                "possible": possible.unwrap_or(Value::Null),
                "available": col.get("availability").and_then(|v| v.get("available")).cloned().unwrap_or(Value::Null),
                "contentId": col.get("contentId").cloned().unwrap_or(Value::Null),
                "score": grade_entry.and_then(|g| g.get("score")).cloned().unwrap_or(Value::Null),
                "grade": grade_entry.and_then(|g| g.get("grade")).cloned().unwrap_or(Value::Null),
                "status": grade_entry.and_then(|g| g.get("status")).cloned().unwrap_or(Value::Null),
            }));
        }
    }

    if !grade_errors.is_empty() {
        grades_available = false;
        if grade_fetch_error.is_none() {
            let mut seen = std::collections::HashSet::new();
            let mut dedup: Vec<String> = Vec::with_capacity(grade_errors.len());
            for e in &grade_errors {
                if seen.insert(e.clone()) {
                    dedup.push(e.clone());
                }
            }
            grade_fetch_error = Some(dedup.join("; "));
        }
    }

    let (page, meta) = slice_with_pagination(&columns_result, offset, limit);
    let mut payload = Map::new();
    payload.insert("columns".to_string(), Value::Array(page));
    merge_meta(&mut payload, meta);
    payload.insert("gradesAvailable".to_string(), Value::Bool(grades_available));
    payload.insert(
        "gradeFetchError".to_string(),
        grade_fetch_error.map(Value::String).unwrap_or(Value::Null),
    );
    payload.insert(
        "courseIdsQueried".to_string(),
        Value::Array(
            course_ids
                .iter()
                .map(|x| Value::String(x.clone()))
                .collect(),
        ),
    );
    payload.insert("courseErrors".to_string(), Value::Object(course_errors));
    Ok(Value::Object(payload))
}

// ---------------------------------------------------------------------------
// Registry handlers (handlers.py)
// ---------------------------------------------------------------------------

async fn h_list_messages(state: &AppState, args: &Value) -> Result<Value, String> {
    let folder = str_arg(args, "folder").unwrap_or("inbox").to_lowercase();
    let unread_only = bool_arg(args, "unread_only", false);
    let since = args
        .get("since")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    if let Some(s) = &since {
        validate_iso8601(s, "since")?;
    }
    let (offset, limit) = resolve_pagination_args(args)?;
    let _fmt = response_format(args)?;
    let msgs = messages(&state.client, &folder, unread_only, since.as_deref()).await?;
    let (page, meta) = slice_with_pagination(&msgs, offset, limit);
    let mut payload = Map::new();
    payload.insert("folder".to_string(), Value::String(folder.clone()));
    payload.insert("unreadOnly".to_string(), Value::Bool(unread_only));
    payload.insert("messages".to_string(), Value::Array(page));
    merge_meta(&mut payload, meta);
    Ok(Value::Object(payload))
}

async fn h_read_message(state: &AppState, args: &Value) -> Result<Value, String> {
    let message_id = str_arg(args, "message_id").unwrap_or("").to_string();
    validate_bb_id(&message_id, "message_id")?;
    let _fmt = response_format(args)?;
    let msg = message(&state.client, &message_id).await?;
    let mut body = msg.get("body").cloned().unwrap_or(Value::Null);
    if let Some(o) = body.as_object() {
        let text = o
            .get("text")
            .and_then(|v| v.as_str())
            .or_else(|| o.get("rawText").and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string();
        body = Value::String(text);
    } else if let Some(s) = body.as_str() {
        body = Value::String(s.to_string());
    } else {
        body = Value::String(String::new());
    }
    let body_text = match &body {
        Value::String(b) => b.clone(),
        _ => String::new(),
    };
    let mut payload = Map::new();
    payload.insert("id".to_string(), Value::String(message_id.clone()));
    let subject = msg
        .get("subject")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    payload.insert("subject".to_string(), Value::String(subject));
    payload.insert(
        "body".to_string(),
        Value::String(parsers::strip_html(&body_text)),
    );
    let created = msg
        .get("created")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    payload.insert("created".to_string(), Value::String(created));
    let read = msg.get("read").and_then(|v| v.as_bool()).unwrap_or(true);
    payload.insert("read".to_string(), Value::Bool(read));
    let folder = msg
        .get("folder")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    payload.insert("folder".to_string(), Value::String(folder));
    let sender_id = msg
        .get("fromUserId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    payload.insert("senderId".to_string(), Value::String(sender_id.clone()));
    match message_participants(&state.client, &message_id).await {
        Ok(participants) => {
            let recipients: Vec<Value> = participants
                .iter()
                .map(|p| {
                    let row = render_user_row(p);
                    let id = row.get("id").cloned().unwrap_or(Value::Null);
                    json!({ "id": id, "name": row.get("name"), "role": row.get("role") })
                })
                .collect();
            payload.insert("recipients".to_string(), Value::Array(recipients));
            let sender = participants.iter().find(|p| {
                p.get("userId")
                    .and_then(|v| v.as_str())
                    .or_else(|| p.get("id").and_then(|v| v.as_str()))
                    == Some(sender_id.as_str())
            });
            match sender {
                Some(s) => {
                    payload.insert("senderName".to_string(), Value::String(user_name(s)));
                }
                None => {
                    payload.insert("senderName".to_string(), Value::String(String::new()));
                }
            }
        }
        Err(_) => {
            payload.insert("recipients".to_string(), Value::Array(Vec::new()));
            payload.insert("senderName".to_string(), Value::String(String::new()));
        }
    }
    Ok(Value::Object(payload))
}

async fn h_list_course_users(state: &AppState, args: &Value) -> Result<Value, String> {
    let course_id = str_arg(args, "course_id").unwrap_or("").to_string();
    validate_bb_id(&course_id, "course_id")?;
    let (offset, limit) = resolve_pagination_args(args)?;
    let _fmt = response_format(args)?;
    let users = course_users(&state.client, &course_id).await?;
    let (page, meta) = slice_with_pagination(&users, offset, limit);
    let rendered: Vec<Value> = page.iter().map(render_user_row).collect();
    let mut payload = Map::new();
    payload.insert("courseId".to_string(), Value::String(course_id));
    payload.insert("users".to_string(), Value::Array(rendered));
    merge_meta(&mut payload, meta);
    Ok(Value::Object(payload))
}

async fn h_list_course_groups(state: &AppState, args: &Value) -> Result<Value, String> {
    let course_id = str_arg(args, "course_id").unwrap_or("").to_string();
    validate_bb_id(&course_id, "course_id")?;
    let (offset, limit) = resolve_pagination_args(args)?;
    let _fmt = response_format(args)?;
    let groups = course_groups(&state.client, &course_id).await?;
    let (page, meta) = slice_with_pagination(&groups, offset, limit);
    let include_members = bool_arg(args, "include_members", false);
    let mut rendered: Vec<Value> = Vec::new();
    for g in &page {
        let mut entry = Map::new();
        entry.insert(
            "id".to_string(),
            g.get("id").cloned().unwrap_or(Value::Null),
        );
        let name = g
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| g.get("title").and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string();
        entry.insert("name".to_string(), Value::String(name));
        let desc = g.get("description").and_then(|v| v.as_str()).unwrap_or("");
        entry.insert(
            "description".to_string(),
            Value::String(parsers::strip_html(desc)),
        );
        let available = g
            .get("availability")
            .and_then(|a| a.get("available"))
            .and_then(|v| v.as_str())
            == Some("Yes");
        entry.insert("available".to_string(), Value::Bool(available));
        if include_members {
            let gid = g.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let count = match group_users(&state.client, &course_id, gid).await {
                Ok(members) => members.len(),
                Err(_) => 0,
            };
            entry.insert("memberCount".to_string(), Value::from(count));
        }
        rendered.push(Value::Object(entry));
    }
    let mut payload = Map::new();
    payload.insert("courseId".to_string(), Value::String(course_id));
    payload.insert("groups".to_string(), Value::Array(rendered));
    merge_meta(&mut payload, meta);
    Ok(Value::Object(payload))
}

async fn h_get_group_members(state: &AppState, args: &Value) -> Result<Value, String> {
    let course_id = str_arg(args, "course_id").unwrap_or("").to_string();
    validate_bb_id(&course_id, "course_id")?;
    let group_id = str_arg(args, "group_id").unwrap_or("").to_string();
    validate_bb_id(&group_id, "group_id")?;
    let (offset, limit) = resolve_pagination_args(args)?;
    let _fmt = response_format(args)?;
    let users = group_users(&state.client, &course_id, &group_id).await?;
    let (page, meta) = slice_with_pagination(&users, offset, limit);
    let rendered: Vec<Value> = page.iter().map(render_user_row).collect();
    let mut payload = Map::new();
    payload.insert("courseId".to_string(), Value::String(course_id));
    payload.insert("groupId".to_string(), Value::String(group_id));
    payload.insert("users".to_string(), Value::Array(rendered));
    merge_meta(&mut payload, meta);
    Ok(Value::Object(payload))
}

async fn h_get_gradebook_attempts(state: &AppState, args: &Value) -> Result<Value, String> {
    let course_id = str_arg(args, "course_id").unwrap_or("").to_string();
    validate_bb_id(&course_id, "course_id")?;
    let column_id = str_arg(args, "column_id").unwrap_or("").to_string();
    validate_bb_id(&column_id, "column_id")?;
    let user_id = args
        .get("user_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    if let Some(uid) = &user_id {
        validate_bb_id(uid, "user_id")?;
    }
    let (offset, limit) = resolve_pagination_args(args)?;
    let _fmt = response_format(args)?;
    let attempts = match &user_id {
        Some(uid) => user_attempts(&state.client, &course_id, &column_id, uid).await?,
        None => gradebook_attempts(&state.client, &course_id, &column_id).await?,
    };
    let (page, meta) = slice_with_pagination(&attempts, offset, limit);
    let rendered: Vec<Value> = page
        .iter()
        .map(|a| {
            let user = a
                .get("userId")
                .and_then(|v| v.as_str())
                .or_else(|| a.get("user").and_then(|v| v.as_str()))
                .unwrap_or("")
                .to_string();
            let created = a
                .get("created")
                .and_then(|v| v.as_str())
                .or_else(|| a.get("createdAt").and_then(|v| v.as_str()))
                .unwrap_or("")
                .to_string();
            let cum = a.get("cumulatedScore");
            json!({
                "id": a.get("id"),
                "userId": user,
                "status": a.get("status").and_then(|v| v.as_str()).unwrap_or(""),
                "score": grade_score(a),
                "cumulatedScore": grade_score(cum.unwrap_or(&Value::Null)),
                "feedback": parsers::strip_html(a.get("feedback").and_then(|v| v.as_str()).unwrap_or("")),
                "created": created,
            })
        })
        .collect();
    let mut payload = Map::new();
    payload.insert("courseId".to_string(), Value::String(course_id));
    payload.insert("columnId".to_string(), Value::String(column_id));
    payload.insert("attempts".to_string(), Value::Array(rendered));
    merge_meta(&mut payload, meta);
    Ok(Value::Object(payload))
}

async fn h_search_all_courses(state: &AppState, args: &Value) -> Result<Value, String> {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if query.is_empty() {
        return Err("query must be a non-empty string".to_string());
    }
    let mut max_depth = parse_int_arg(args, "max_depth", 3);
    max_depth = max_depth.clamp(1, 10);
    let mut max_results = parse_int_arg(args, "max_results", 50);
    max_results = max_results.clamp(1, 200);
    let course_ids = fan_out_course_ids(&state.client, args.get("course_ids")).await?;
    let _fmt = response_format(args)?;
    let mut matches: Vec<Value> = Vec::new();
    let course_errors = Map::new();
    // Deep per-course walks are the slowest fan-out in the toolset — run the
    // courses concurrently so all-courses searches finish in a fraction of
    // the wall time.
    let per_course = bounded_map(course_ids.clone(), fanout_limit(), |cid| {
        let c = &state.client;
        let query = query.clone();
        async move { search_course(c, &cid, &query, max_depth as usize).await }
    })
    .await;
    for found in per_course {
        matches.extend(found);
    }
    if matches.len() > max_results as usize {
        matches.truncate(max_results as usize);
    }
    let mut payload = Map::new();
    payload.insert("query".to_string(), Value::String(query));
    payload.insert("maxResults".to_string(), Value::from(matches.len()));
    payload.insert("coursesSearched".to_string(), Value::from(course_ids.len()));
    payload.insert("matches".to_string(), Value::Array(matches));
    payload.insert("courseErrors".to_string(), Value::Object(course_errors));
    Ok(Value::Object(payload))
}

async fn h_get_content_tree(state: &AppState, args: &Value) -> Result<Value, String> {
    let course_id = str_arg(args, "course_id").unwrap_or("").to_string();
    validate_bb_id(&course_id, "course_id")?;
    let mut max_depth = parse_int_arg(args, "max_depth", 5);
    max_depth = max_depth.clamp(1, 10);
    let _fmt = response_format(args)?;
    let nodes = walk_content(&state.client, &course_id, max_depth as usize).await;

    // Parent-id -> children, built in one pass. The previous version scanned
    // every node at depth+1 for each node (O(n^2)) and matched on breadcrumb
    // titles, which merged same-titled siblings.
    let mut children_map: HashMap<String, Vec<&ContentNode>> = HashMap::new();
    for n in &nodes {
        children_map.entry(n.parent.clone()).or_default().push(n);
    }

    fn to_node<'a>(
        node: &'a ContentNode,
        children_map: &HashMap<String, Vec<&'a ContentNode>>,
    ) -> Value {
        let children: Vec<Value> = children_map
            .get(&node.id())
            .map(|kids| {
                kids.iter()
                    .take(100)
                    .map(|c| to_node(c, children_map))
                    .collect()
            })
            .unwrap_or_default();
        json!({
            "id": node.id(),
            "title": node.title(),
            "kind": if is_folder(&node.item) { "folder" } else { "file" },
            "hasChildren": is_folder(&node.item),
            "children": children,
        })
    }

    let roots = children_map.get("").cloned().unwrap_or_default();
    let tree: Vec<Value> = roots.iter().map(|r| to_node(r, &children_map)).collect();
    let mut payload = Map::new();
    payload.insert("courseId".to_string(), Value::String(course_id));
    payload.insert("count".to_string(), Value::from(tree.len()));
    payload.insert("totalNodes".to_string(), Value::from(nodes.len()));
    payload.insert("tree".to_string(), Value::Array(tree));
    Ok(Value::Object(payload))
}

async fn h_download_course(state: &AppState, args: &Value) -> Result<Value, String> {
    let course_id = str_arg(args, "course_id").unwrap_or("").to_string();
    validate_bb_id(&course_id, "course_id")?;
    let dest_raw = args
        .get("destination_dir")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let crs = course(&state.client, &course_id)
        .await
        .unwrap_or_else(|_| json!({}));
    let course_name = parsers::safe_folder_name(
        crs.get("name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| crs.get("displayName").and_then(|v| v.as_str()))
            .unwrap_or(&course_id),
    );
    let dest_root: PathBuf = if dest_raw.is_empty() {
        if let Some(home) = dirs::home_dir() {
            home.join("Downloads").join("NTULearn")
        } else {
            PathBuf::from("./Downloads/NTULearn")
        }
    } else {
        let expanded = expand_tilde(&dest_raw);
        if !expanded.is_absolute() {
            return Err("destination_dir must be an absolute path".to_string());
        }
        expanded
    };
    let mut max_depth = parse_int_arg(args, "max_depth", 3);
    max_depth = max_depth.clamp(1, 10);
    let skip_existing = bool_arg(args, "skip_existing", true);
    let ext_filter = parse_extensions(args.get("include_extensions"));
    let _fmt = response_format(args)?;

    let mut jobs = collect_download_jobs(&state.client, &course_id, max_depth as usize).await;
    // Assign unique target names up front (Python comment: before ANY download starts).
    let mut used_names: HashMap<String, HashSet<String>> = HashMap::new();
    for job in &mut jobs {
        let used = used_names.entry(job.course_folder.clone()).or_default();
        let mut name = job.safe_name.clone();
        let (base, dot, suffix) = match rpartition(&name, '.') {
            Some((stem, ext)) if !stem.is_empty() => (stem.to_string(), true, ext.to_string()),
            _ => (name.clone(), false, String::new()),
        };
        let mut n = 2usize;
        while used.contains(&name) {
            name = if dot {
                format!("{base} ({n}).{suffix}")
            } else {
                format!("{base} ({n})")
            };
            n += 1;
        }
        used.insert(name.clone());
        job.target_name = name;
    }

    let mut results: Vec<Value> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();
    for job in &jobs {
        // sequential (Python uses a semaphore; order is preserved and clients
        // are exercised one at a time — payload parity is identical).
        download_worker(
            &state.client,
            job,
            &dest_root,
            skip_existing,
            &mut results,
            &mut skipped,
            ext_filter.as_ref(),
        )
        .await;
    }
    results.sort_by(|a, b| {
        let ka = a
            .get("courseFolder")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
            + a.get("filename").and_then(|v| v.as_str()).unwrap_or("");
        let kb = b
            .get("courseFolder")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
            + b.get("filename").and_then(|v| v.as_str()).unwrap_or("");
        ka.cmp(&kb)
    });
    let total_bytes: u64 = results
        .iter()
        .map(|r| r.get("sizeBytes").and_then(|v| v.as_u64()).unwrap_or(0))
        .sum();
    let mut payload = Map::new();
    payload.insert("courseId".to_string(), Value::String(course_id));
    payload.insert("courseName".to_string(), Value::String(course_name));
    payload.insert(
        "destinationDir".to_string(),
        Value::String(dest_root.to_string_lossy().to_string()),
    );
    payload.insert("downloadCount".to_string(), Value::from(results.len()));
    payload.insert("skippedCount".to_string(), Value::from(skipped.len()));
    payload.insert("totalBytes".to_string(), Value::from(total_bytes));
    payload.insert("files".to_string(), Value::Array(results));
    payload.insert("skipped".to_string(), Value::Array(skipped));
    Ok(Value::Object(payload))
}

fn tracker_get_last_seen() -> Option<String> {
    LAST_SEEN.lock().ok().and_then(|g| g.clone())
}

fn tracker_set_last_seen(s: String) {
    if let Ok(mut g) = LAST_SEEN.lock() {
        *g = Some(s);
    }
}

async fn h_whats_new(state: &AppState, args: &Value) -> Result<Value, String> {
    // Enrollments with `expand=course` give ids and names in the same request
    // that `fan_out_course_ids` would have made anyway, removing the N extra
    // `/courses/{id}` GETs the digest used to issue just for display names.
    let explicit_ids = match args.get("course_ids") {
        None => false,
        Some(Value::Array(a)) => !a.is_empty(),
        Some(_) => true,
    };
    let (course_ids, name_hints) = if explicit_ids {
        (
            fan_out_course_ids(&state.client, args.get("course_ids")).await?,
            HashMap::new(),
        )
    } else {
        enrollment_ids_and_names(&state.client).await?
    };
    let since = match args.get("since").and_then(|v| v.as_str()) {
        Some(s) => validate_iso8601(s, "since")?,
        None => tracker_get_last_seen().unwrap_or_else(default_since),
    };
    let update_tracker = bool_arg(args, "update_tracker", false);
    let _fmt = response_format(args)?;

    let mut per_course: Vec<Value> = Vec::new();
    let mut errors: Map<String, Value> = Map::new();
    // One digest entry per course = 4 API calls. Courses run concurrently
    // (bounded); within a course the sources stay sequential and only the
    // first failure per course is reported, matching the original semantics.
    let fan = bounded_map(course_ids.clone(), fanout_limit(), |cid| {
        let c = &state.client;
        let name_hints = &name_hints;
        let since = since.clone();
        async move {
            let mut entry = Map::new();
            entry.insert("courseId".to_string(), Value::String(cid.clone()));
            let mut first_error: Option<String> = None;
            // Announcements and contents are independent and run together.
            // The course name usually arrives in the enrollment response, so
            // only a miss costs a `/courses/{id}` round trip.
            let name_hint = name_hints.get(&cid).cloned();
            let (anns_res, contents_res, course_res) =
                tokio::join!(announcements(c, &cid), course_contents(c, &cid), async {
                    match &name_hint {
                        Some(_) => None,
                        None => Some(course(c, &cid).await),
                    }
                },);
            // course name
            match (name_hint, course_res) {
                (Some(name), _) => {
                    entry.insert("courseName".to_string(), Value::String(name));
                }
                (None, Some(Ok(crs))) => {
                    let name = crs
                        .get("name")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .or_else(|| crs.get("displayName").and_then(|v| v.as_str()))
                        .unwrap_or(&cid);
                    entry.insert("courseName".to_string(), Value::String(name.to_string()));
                }
                (None, Some(Err(e))) => {
                    first_error = Some(format!("course: {e}"));
                }
                (None, None) => {}
            }
            // announcements
            match anns_res {
                Ok(anns) => {
                    let mut list = Vec::new();
                    for a in anns {
                        if !after_value(a.get("created"), &since) {
                            continue;
                        }
                        let mut body = announcement_text(&a);
                        if body.chars().count() > 500 {
                            body = body.chars().take(500).collect();
                        }
                        list.push(json!({
                            "id": a.get("id"),
                            "title": a.get("title"),
                            "created": a.get("created"),
                            "body": body,
                        }));
                    }
                    entry.insert("announcements".to_string(), Value::Array(list));
                }
                Err(e) => {
                    if first_error.is_none() {
                        first_error = Some(format!("announcements: {e}"));
                    }
                }
            }
            // new files (top-level only)
            match contents_res {
                Ok(root) => {
                    let mut list = Vec::new();
                    for item in root {
                        if is_file_item(&item) && after_value(item.get("modified"), &since) {
                            list.push(json!({
                                "id": item.get("id"),
                                "title": item_title(&item),
                                "modified": item.get("modified").and_then(|v| v.as_str()).unwrap_or(""),
                            }));
                        }
                    }
                    entry.insert("newFiles".to_string(), Value::Array(list));
                }
                Err(e) => {
                    if first_error.is_none() {
                        first_error = Some(format!("contents: {e}"));
                    }
                }
            }
            (cid, entry, first_error)
        }
    });
    // One calendar request covers every course; the old code made one per
    // course. It runs alongside the per-course fan-out.
    let calendar_fut = calendar_items(&state.client, None, Some(&since), None, None);
    let (results, cal_res) = tokio::join!(fan, calendar_fut);
    let mut cal_by_course: HashMap<String, Vec<Value>> = HashMap::new();
    let cal_error = match cal_res {
        Ok(items) => {
            for item in items {
                if !after_value(item.get("start"), &since) {
                    continue;
                }
                if let Some(cid) = item.get("courseId").and_then(|v| v.as_str()) {
                    cal_by_course
                        .entry(cid.to_string())
                        .or_default()
                        .push(calendar_brief(&item));
                }
            }
            None
        }
        Err(e) => Some(e),
    };
    for (cid, mut entry, mut first_error) in results {
        entry.insert(
            "upcoming".to_string(),
            Value::Array(cal_by_course.remove(&cid).unwrap_or_default()),
        );
        if first_error.is_none() {
            if let Some(e) = &cal_error {
                first_error = Some(format!("calendar: {e}"));
            }
        }
        if let Some(e) = first_error {
            errors.insert(cid, Value::String(e));
        }
        per_course.push(Value::Object(entry));
    }

    let fetched_at = now_iso();
    if update_tracker {
        tracker_set_last_seen(fetched_at.clone());
    }
    let count_ann: usize = per_course
        .iter()
        .map(|e| {
            e.get("announcements")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0)
        })
        .sum();
    let count_up: usize = per_course
        .iter()
        .map(|e| {
            e.get("upcoming")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0)
        })
        .sum();
    let count_files: usize = per_course
        .iter()
        .map(|e| {
            e.get("newFiles")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0)
        })
        .sum();
    let mut payload = Map::new();
    payload.insert("since".to_string(), Value::String(since));
    payload.insert("fetchedAt".to_string(), Value::String(fetched_at));
    payload.insert("courseCount".to_string(), Value::from(per_course.len()));
    payload.insert(
        "summary".to_string(),
        json!({
            "announcements": count_ann,
            "upcoming": count_up,
            "newFiles": count_files,
        }),
    );
    payload.insert("courses".to_string(), Value::Array(per_course));
    payload.insert("courseErrors".to_string(), Value::Object(errors));
    Ok(Value::Object(payload))
}

async fn h_export_calendar_ics(state: &AppState, args: &Value) -> Result<Value, String> {
    let course_ids = fan_out_course_ids(&state.client, args.get("course_ids")).await?;
    let since = match args.get("since").and_then(|v| v.as_str()) {
        Some(v) => v.to_string(),
        None => iso_from_now(0, 0),
    };
    let until = match args.get("until").and_then(|v| v.as_str()) {
        Some(v) => v.to_string(),
        None => iso_from_now(30, 0),
    };
    validate_iso8601(&since, "since")?;
    validate_iso8601(&until, "until")?;
    let _fmt = response_format(args)?;

    let mut items: Vec<Value> = Vec::new();
    let mut errors: Map<String, Value> = Map::new();
    let per_course = bounded_map(course_ids.clone(), fanout_limit(), |cid| {
        let c = &state.client;
        let since = since.clone();
        let until = until.clone();
        async move {
            let res = calendar_items(c, Some(&cid), Some(&since), Some(&until), None).await;
            (cid, res)
        }
    })
    .await;
    for (cid, res) in per_course {
        match res {
            Ok(cal) => {
                for mut i in cal {
                    let item_id = i
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("x")
                        .to_string();
                    if let Some(o) = i.as_object_mut() {
                        o.insert("courseId".to_string(), Value::String(cid.clone()));
                        o.insert(
                            "uid".to_string(),
                            Value::String(format!("{}-{}", cid, item_id)),
                        );
                    }
                    items.push(i);
                }
            }
            Err(e) => {
                errors.insert(cid, Value::String(e));
            }
        }
    }
    let scope = course_ids
        .iter()
        .take(5)
        .cloned()
        .collect::<Vec<_>>()
        .join("; ");
    let ics = build_ics(&items, &scope);
    let mut payload = Map::new();
    payload.insert("itemCount".to_string(), Value::from(items.len()));
    payload.insert("courseCount".to_string(), Value::from(course_ids.len()));
    payload.insert("since".to_string(), Value::String(since));
    payload.insert("until".to_string(), Value::String(until));
    payload.insert("supported".to_string(), Value::Bool(true));
    payload.insert("ics".to_string(), Value::String(ics));
    payload.insert("courseErrors".to_string(), Value::Object(errors));
    Ok(Value::Object(payload))
}

async fn h_export_gradebook_csv(state: &AppState, args: &Value) -> Result<Value, String> {
    let course_ids = fan_out_course_ids(&state.client, args.get("course_ids")).await?;
    let _fmt = response_format(args)?;
    let user_id = me_user_id(&state.client).await.unwrap_or_default();
    let mut rows: Vec<Value> = Vec::new();
    let mut errors: Map<String, Value> = Map::new();
    let per_course = bounded_map(course_ids.clone(), fanout_limit(), |cid| {
        let c = &state.client;
        let uid = user_id.clone();
        async move {
            let columns = gradebook_columns(c, &cid).await;
            let grades = if let Some(uid) = &uid {
                user_grades(c, &cid, uid).await.unwrap_or_default()
            } else {
                Vec::new()
            };
            (cid, columns, grades)
        }
    })
    .await;
    for (cid, columns_res, grades) in per_course {
        match columns_res {
            Ok(columns) => {
                for col in &columns {
                    let col_id = col.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    let own = grades
                        .iter()
                        .find(|g| g.get("columnId").and_then(|v| v.as_str()) == Some(col_id));
                    let score = own.and_then(grade_score);
                    let status = own
                        .map(|o| {
                            o.get("status")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string()
                        })
                        .unwrap_or_default();
                    rows.push(json!({
                        "courseId": cid,
                        "columnId": col_id,
                        "columnName": column_name(col),
                        "possible": column_possible(col).map(Value::from).unwrap_or(Value::Null),
                        "score": score.map(Value::from).unwrap_or(Value::Null),
                        "status": status,
                        "grade": score.map(|s| s.to_string()).unwrap_or_default(),
                    }));
                }
            }
            Err(e) => {
                errors.insert(cid.clone(), Value::String(e));
            }
        }
    }
    let csv_text = build_gradebook_csv(&rows);
    let mut payload = Map::new();
    payload.insert("rowCount".to_string(), Value::from(rows.len()));
    payload.insert("courseCount".to_string(), Value::from(course_ids.len()));
    payload.insert("csv".to_string(), Value::String(csv_text));
    payload.insert("courseErrors".to_string(), Value::Object(errors));
    Ok(Value::Object(payload))
}

async fn h_summarize_course(state: &AppState, args: &Value) -> Result<Value, String> {
    let course_id = str_arg(args, "course_id").unwrap_or("").to_string();
    validate_bb_id(&course_id, "course_id")?;
    let include_contents = bool_arg(args, "include_contents", true);
    let _fmt = response_format(args)?;
    Ok(build_course_summary(&state.client, &course_id, include_contents).await)
}

// ---------------------------------------------------------------------------
// FeedbackFruits peer evaluation (fbf.rs) — fbf_status / fbf_reviews /
// fbf_rate / fbf_comment. FeedbackFruits activities are LTI placements in
// the Blackboard content tree; fbf.rs owns the launch chain (content_id ->
// FBF access token + activity id, cached in fbf.json), these handlers own
// argument resolution and payload assembly. API semantics: docs/FBF_SPEC.md §2-§4.
// ---------------------------------------------------------------------------

/// LTI tool placement content type — every FeedbackFruits activity (and every
/// other LTI tool) surfaces as one of these in the content tree.
const BLTI_LINK_HANDLER: &str = "resource/x-bb-blti-link";

/// Find the FBF task a tool operates on by title substring. "Give ratings"
/// (rubric ratings) and "Overall comments" (written reviews) share the kind
/// `user-review-tasks`, so the title is the only discriminator.
fn fbf_find_task<'a>(tasks: &'a [FbfTask], needle: &str) -> Result<&'a FbfTask, String> {
    let n = needle.to_lowercase();
    let hits: Vec<&FbfTask> = tasks
        .iter()
        .filter(|t| {
            t.title
                .as_deref()
                .map(|s| s.to_lowercase().contains(&n))
                .unwrap_or(false)
        })
        .collect();
    match hits.len() {
        0 => {
            let names: Vec<String> = tasks
                .iter()
                .map(|t| t.title.clone().unwrap_or_else(|| t.kind.clone()))
                .collect();
            Err(format!(
                "no FeedbackFruits task titled like {needle:?}; this activity has: {}",
                names.join("; ")
            ))
        }
        1 => Ok(hits[0]),
        _ => Err(format!(
            "multiple tasks match {needle:?}: {}",
            hits.iter()
                .map(|t| t.title.clone().unwrap_or_default())
                .collect::<Vec<_>>()
                .join("; ")
        )),
    }
}

/// Resolve a case-insensitive substring against candidate labels. Exactly one
/// hit wins; zero or many return an error listing the candidates so the
/// agent can retry with a longer substring instead of guessing.
fn fbf_resolve_unique(what: &str, needle: &str, labels: &[String]) -> Result<usize, String> {
    let n = needle.to_lowercase();
    let hits: Vec<usize> = labels
        .iter()
        .enumerate()
        .filter(|(_, l)| l.to_lowercase().contains(&n))
        .map(|(i, _)| i)
        .collect();
    match hits.len() {
        0 => Err(format!(
            "no {what} matches {needle:?}. Available: {}",
            labels.join("; ")
        )),
        1 => Ok(hits[0]),
        _ => Err(format!(
            "{what} {needle:?} is ambiguous — matches: {}. Use a longer substring.",
            hits.iter()
                .map(|&i| labels[i].as_str())
                .collect::<Vec<_>>()
                .join("; ")
        )),
    }
}

/// My allocations on a task with counterpart names resolved. A failed name
/// lookup must not hide the allocation — the raw user id is a usable label
/// and every write path only needs the allocation id.
async fn fbf_given_with_names(
    session: &FbfSession,
    task: &FbfTask,
) -> Result<Vec<(String, String, String)>, String> {
    let allocations = session.allocations_given(&task.id).await?;
    let mut out = Vec::with_capacity(allocations.len());
    for (alloc_id, ratee) in allocations {
        let name = session
            .user_name(&ratee)
            .await
            .unwrap_or_else(|_| ratee.clone());
        out.push((alloc_id, ratee, name));
    }
    Ok(out)
}

/// Rating rows for a set of allocations. Given direction: `user_id` is the
/// ratee; received direction: the giver — the tuple shape is identical, so
/// one builder serves both. Criteria are fetched per allocation (they are
/// allocation-scoped in the API); option objects are fetched once per
/// distinct id because the same rubric repeats for every teammate.
/// `include_unrated` emits a null-option row per unrated criterion (given
/// direction uses this to show what is still owed).
async fn fbf_rating_rows(
    session: &FbfSession,
    allocations: Vec<(String, String)>,
    include_unrated: bool,
) -> Result<(Vec<Value>, usize), String> {
    if allocations.is_empty() {
        return Ok((Vec::new(), 0));
    }
    let fetched: Vec<(String, Vec<FbfCriterion>, Vec<FbfRating>)> = bounded_map(
        allocations,
        fanout_limit(),
        |(alloc_id, user_id)| async move {
            let name = session
                .user_name(&user_id)
                .await
                .unwrap_or_else(|_| user_id.clone());
            let criteria = session.criteria(&alloc_id).await?;
            let ratings = session.ratings(&alloc_id).await?;
            Ok::<_, String>((name, criteria, ratings))
        },
    )
    .await
    .into_iter()
    .collect::<Result<_, String>>()?;

    // Distinct option ids across every criteria set plus every rated option,
    // each fetched exactly once.
    let mut option_ids: Vec<String> = Vec::new();
    for (_, criteria, ratings) in &fetched {
        for c in criteria {
            for oid in &c.option_ids {
                if !option_ids.contains(oid) {
                    option_ids.push(oid.clone());
                }
            }
        }
        for r in ratings {
            if !option_ids.contains(&r.option_id) {
                option_ids.push(r.option_id.clone());
            }
        }
    }
    let options: HashMap<String, FbfOption> =
        bounded_map(option_ids, fanout_limit(), |oid| async move {
            let o = session.option(&oid).await?;
            Ok::<_, String>((oid, o))
        })
        .await
        .into_iter()
        .collect::<Result<HashMap<_, _>, String>>()?;

    let mut rows: Vec<Value> = Vec::new();
    let mut unrated = 0usize;
    for (name, criteria, ratings) in fetched {
        for c in &criteria {
            let existing = ratings.iter().find(|r| r.criterion_id == c.id);
            let (option, weight) = match existing {
                Some(r) => match options.get(&r.option_id) {
                    // An unknown option id still reports the id itself, never
                    // a silently wrong title.
                    Some(o) => (Value::String(o.title.clone()), Value::from(o.weight)),
                    None => (Value::String(r.option_id.clone()), Value::Null),
                },
                None => {
                    if include_unrated {
                        unrated += 1;
                        (Value::Null, Value::Null)
                    } else {
                        continue;
                    }
                }
            };
            rows.push(json!({
                "teammate": name,
                "criterion": c.title,
                "option": option,
                "weight": weight,
            }));
        }
    }
    Ok((rows, unrated))
}

/// Written reviews received on the "Overall comments" task (empty until the
/// teacher publishes, same as received ratings).
async fn fbf_received_comments(
    session: &FbfSession,
    task: Option<&FbfTask>,
) -> Result<Vec<Value>, String> {
    let Some(task) = task else {
        return Ok(Vec::new());
    };
    let allocations = session.allocations_received(&task.id).await?;
    let fetched: Vec<(String, Vec<FbfReview>)> = bounded_map(
        allocations,
        fanout_limit(),
        |(alloc_id, giver)| async move {
            let name = session
                .user_name(&giver)
                .await
                .unwrap_or_else(|_| giver.clone());
            let reviews = session.reviews(&alloc_id).await?;
            Ok::<_, String>((name, reviews))
        },
    )
    .await
    .into_iter()
    .collect::<Result<_, String>>()?;
    let mut rows: Vec<Value> = Vec::new();
    for (name, reviews) in fetched {
        for rev in reviews {
            if rev.body.trim().is_empty() {
                continue;
            }
            rows.push(json!({ "teammate": name, "comment": rev.body }));
        }
    }
    Ok(rows)
}

/// `ntulearn_fbf_status` — FeedbackFruits peer-evaluation status: per-task
/// deadlines, my progress and done state, activity-level completion, and
/// whether grades have been released. With content_id omitted, every LTI
/// link in the course content tree is probed; non-FeedbackFruits tools are
/// skipped, not errors.
async fn h_fbf_status(state: &AppState, args: &Value) -> Result<Value, String> {
    let course_id = str_arg(args, "course_id").unwrap_or("").to_string();
    validate_bb_id(&course_id, "course_id")?;
    let content_id = str_arg(args, "content_id").map(|s| s.to_string());
    if let Some(cid) = &content_id {
        validate_bb_id(cid, "content_id")?;
    }
    let _fmt = response_format(args)?;

    let candidates: Vec<(String, String)> = match &content_id {
        Some(cid) => vec![(cid.clone(), String::new())],
        None => {
            // Discovery: the full content walk (same primitive as
            // get_content_tree) filtered to LTI placements — the only items
            // a FeedbackFruits launch could succeed on.
            let nodes = walk_content(&state.client, &course_id, MAX_DEPTH).await;
            nodes
                .iter()
                .filter(|n| handler_id(&n.item) == BLTI_LINK_HANDLER)
                .filter_map(|n| {
                    n.item
                        .get("id")
                        .and_then(|v| v.as_str())
                        .map(|id| (id.to_string(), item_title(&n.item)))
                })
                .collect()
        }
    };

    let mut activities: Vec<Value> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();
    for (cid, title) in &candidates {
        // Launches run sequentially on purpose: each writes the shared
        // fbf.json session cache and the LTI chain rides the same NTULearn
        // cookie — concurrent chains would race on both.
        match crate::fbf::FbfSession::launch(state, &course_id, cid).await {
            Ok(session) => match fbf_activity_report(&session, cid).await {
                Ok(report) => activities.push(report),
                Err(e) => {
                    if content_id.is_some() {
                        // An explicit content_id the caller asked for must
                        // fail loudly rather than vanish into "skipped".
                        return Err(format!(
                            "content_id {cid} opened as FeedbackFruits but its activity could not be read: {e}"
                        ));
                    }
                    skipped.push(json!({"contentId": cid, "title": title, "reason": e}));
                }
            },
            Err(e) => {
                if content_id.is_some() {
                    return Err(format!(
                        "content_id {cid} is not a FeedbackFruits activity (launch failed): {e}"
                    ));
                }
                skipped.push(json!({"contentId": cid, "title": title, "reason": e}));
            }
        }
    }

    let mut payload = Map::new();
    payload.insert("courseId".to_string(), Value::String(course_id));
    if let Some(cid) = &content_id {
        payload.insert("contentId".to_string(), Value::String(cid.clone()));
    }
    if candidates.is_empty() {
        // A course with no LTI placements is a valid (boring) answer.
        payload.insert(
            "note".to_string(),
            Value::String(
                "no LTI tool links (resource/x-bb-blti-link) found in this course's content tree — there is no FeedbackFruits activity to report"
                    .to_string(),
            ),
        );
    }
    payload.insert("activities".to_string(), Value::Array(activities));
    payload.insert("skippedLinks".to_string(), Value::Array(skipped));
    Ok(Value::Object(payload))
}

/// One activity rendered for fbf_status: per-task deadline / progress / done,
/// activity-level completion (the same number the FBF UI shows, from the
/// activity's own progress object) and the grades release state.
async fn fbf_activity_report(session: &FbfSession, content_id: &str) -> Result<Value, String> {
    let activity_id = session.activity_id().to_string();
    let activity = session.activity(&activity_id).await?;
    let tasks = session.tasks(&activity_id).await?;

    // Task progress lookups run together; a task without a progress
    // relationship (e.g. instructions) reports null, not a fabricated 0.
    let task_rows = bounded_map(tasks, fanout_limit(), |t| async move {
        let progress = match &t.progress_id {
            Some(pid) => session.progress(pid).await.ok(),
            None => None,
        };
        json!({
            "title": t.title.clone().unwrap_or_else(|| t.kind.clone()),
            "type": t.kind,
            "deadline": t.deadline,
            "progress": progress,
            "done": progress.map(|p| p >= 100),
        })
    })
    .await;

    let completion = match &activity.progress_id {
        Some(pid) => session.progress(pid).await.ok(),
        None => None,
    };
    // grades-published-at stays null until the teacher releases results; a
    // failed release-state read must not sink the whole report.
    let release = session
        .release_state(&activity_id)
        .await
        .unwrap_or_else(|_| json!({}));

    Ok(json!({
        "contentId": content_id,
        "activityId": activity.id,
        "title": activity.title,
        "completion": completion,
        "gradesPublishedAt": release.get("gradesPublishedAt").cloned().unwrap_or(Value::Null),
        "tasks": task_rows,
    }))
}

/// `ntulearn_fbf_reviews` — rubric ratings on one FBF activity.
/// direction='given': what I rated each teammate, one row per criterion
/// (unrated criteria show a null option). direction='received': what
/// teammates rated me — empty until the teacher publishes, which is
/// reported explicitly, never as an error (FBF answers data: [] until then).
async fn h_fbf_reviews(state: &AppState, args: &Value) -> Result<Value, String> {
    let course_id = str_arg(args, "course_id").unwrap_or("").to_string();
    validate_bb_id(&course_id, "course_id")?;
    let content_id = str_arg(args, "content_id").unwrap_or("").to_string();
    validate_bb_id(&content_id, "content_id")?;
    let direction = str_arg(args, "direction").unwrap_or("given").to_lowercase();
    if direction != "given" && direction != "received" {
        return Err("direction must be 'given' or 'received'".to_string());
    }
    let _fmt = response_format(args)?;

    let session = crate::fbf::FbfSession::launch(state, &course_id, &content_id).await?;
    let activity_id = session.activity_id().to_string();
    let activity = session.activity(&activity_id).await?;
    let tasks = session.tasks(&activity_id).await?;
    let ratings_task = fbf_find_task(&tasks, "rating")?;

    let mut payload = Map::new();
    payload.insert("courseId".to_string(), Value::String(course_id));
    payload.insert("contentId".to_string(), Value::String(content_id));
    payload.insert("activityId".to_string(), Value::String(activity_id.clone()));
    payload.insert("activityTitle".to_string(), Value::String(activity.title));
    payload.insert("direction".to_string(), Value::String(direction.clone()));

    if direction == "given" {
        let allocations = session.allocations_given(&ratings_task.id).await?;
        let teammates = allocations.len();
        let (rows, unrated) = fbf_rating_rows(&session, allocations, true).await?;
        payload.insert("teammates".to_string(), Value::from(teammates as u64));
        payload.insert("unrated".to_string(), Value::from(unrated as u64));
        payload.insert("reviews".to_string(), Value::Array(rows));
        return Ok(Value::Object(payload));
    }

    // direction == "received". Ratings-published-at / grades-published-at
    // stay null until the teacher releases; until then FBF returns no data
    // and the honest report is "not yet released", not an error.
    let release = session.release_state(&activity_id).await?;
    let any_published = [
        "ratingsPublishedAt",
        "reviewsPublishedAt",
        "gradesPublishedAt",
    ]
    .iter()
    .any(|k| release.get(k).is_some_and(|v| !v.is_null()));
    let received_allocations = session.allocations_received(&ratings_task.id).await?;
    let (rows, _) = fbf_rating_rows(&session, received_allocations, false).await?;

    // Written reviews received live on the "Overall comments" task's own
    // allocations; its absence just means this activity collects none.
    let comments_task = tasks.iter().find(|t| {
        t.title
            .as_deref()
            .map(|s| s.to_lowercase().contains("overall comment"))
            .unwrap_or(false)
    });
    let comments = fbf_received_comments(&session, comments_task).await?;

    let released = any_published || !rows.is_empty() || !comments.is_empty();
    payload.insert("releaseState".to_string(), release);
    payload.insert("released".to_string(), Value::Bool(released));
    if !released {
        payload.insert(
            "note".to_string(),
            Value::String(
                "reviews from teammates are not yet released — FeedbackFruits returns no data until the teacher publishes ratings/grades (ratingsPublishedAt / gradesPublishedAt are still null)"
                    .to_string(),
            ),
        );
    }
    payload.insert("reviews".to_string(), Value::Array(rows));
    payload.insert("comments".to_string(), Value::Array(comments));
    Ok(Value::Object(payload))
}

/// `ntulearn_fbf_rate` — upsert ONE rubric rating for ONE teammate.
/// teammate/criterion/option resolve case-insensitively by substring
/// (option also accepts its exact weight number); dry_run=true (default)
/// resolves everything and shows the request without sending it.
async fn h_fbf_rate(state: &AppState, args: &Value) -> Result<Value, String> {
    let course_id = str_arg(args, "course_id").unwrap_or("").to_string();
    validate_bb_id(&course_id, "course_id")?;
    let content_id = str_arg(args, "content_id").unwrap_or("").to_string();
    validate_bb_id(&content_id, "content_id")?;
    let teammate = str_arg(args, "teammate").unwrap_or("").trim().to_string();
    let criterion = str_arg(args, "criterion").unwrap_or("").trim().to_string();
    // The schema declares a string, but an agent may send the weight as a
    // JSON number; accept both (same convention as parse_page_range).
    let option_arg = match args.get("option") {
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
    .trim()
    .to_string();
    if teammate.is_empty() {
        return Err("teammate must be a non-empty substring of a teammate name".to_string());
    }
    if criterion.is_empty() {
        return Err("criterion must be a non-empty substring of a criterion title".to_string());
    }
    if option_arg.is_empty() {
        return Err(
            "option must be a non-empty option title substring or the exact weight number"
                .to_string(),
        );
    }
    let dry_run = bool_arg(args, "dry_run", true);
    let _fmt = response_format(args)?;

    let session = crate::fbf::FbfSession::launch(state, &course_id, &content_id).await?;
    let activity_id = session.activity_id().to_string();
    let activity = session.activity(&activity_id).await?;
    let tasks = session.tasks(&activity_id).await?;
    let task = fbf_find_task(&tasks, "rating")?;

    let allocations = fbf_given_with_names(&session, task).await?;
    let names: Vec<String> = allocations.iter().map(|(_, _, n)| n.clone()).collect();
    let idx = fbf_resolve_unique("teammate", &teammate, &names)?;
    let (alloc_id, _ratee, ratee_name) = &allocations[idx];

    let criteria = session.criteria(alloc_id).await?;
    let crit_labels: Vec<String> = criteria.iter().map(|c| c.title.clone()).collect();
    let crit = &criteria[fbf_resolve_unique("criterion", &criterion, &crit_labels)?];

    // Option: substring on titles, or the exact weight number (matched by
    // value, so rubrics with other scales than 0-4 still resolve).
    let weight_filter = option_arg.parse::<i64>().ok();
    let needle = option_arg.to_lowercase();
    let sess = &session;
    let options = bounded_map(crit.option_ids.clone(), fanout_limit(), |oid| async move {
        sess.option(&oid).await
    })
    .await
    .into_iter()
    .collect::<Result<Vec<_>, String>>()?;
    let mut hits: Vec<&FbfOption> = Vec::new();
    for o in &options {
        let by_title = o.title.to_lowercase().contains(&needle);
        let by_weight = weight_filter.map(|w| o.weight == w).unwrap_or(false);
        if by_title || by_weight {
            hits.push(o);
        }
    }
    let opt = match hits.len() {
        0 => {
            let labels: Vec<String> = options
                .iter()
                .map(|o| format!("{} (weight {})", o.title, o.weight))
                .collect();
            return Err(format!(
                "no option matches {option_arg:?} on criterion {:?}. Available: {}",
                crit.title,
                labels.join("; ")
            ));
        }
        1 => hits[0],
        _ => {
            let labels: Vec<String> = hits
                .iter()
                .map(|o| format!("{} (weight {})", o.title, o.weight))
                .collect();
            return Err(format!(
                "option {option_arg:?} is ambiguous — matches: {}. Use the exact title or the weight number.",
                labels.join("; ")
            ));
        }
    };

    // before → after: the existing rating for this (teammate, criterion)
    // pair, so the agent sees what it is about to change.
    let existing = session
        .ratings(alloc_id)
        .await?
        .into_iter()
        .find(|r| r.criterion_id == crit.id);
    let before = match &existing {
        Some(r) => match options.iter().find(|o| o.id == r.option_id) {
            Some(o) => json!({"option": o.title, "weight": o.weight}),
            None => json!({"option": r.option_id, "weight": null}),
        },
        None => Value::Null,
    };

    let result = session
        .upsert_rating(alloc_id, &crit.id, &opt.id, &activity_id, dry_run)
        .await?;

    Ok(json!({
        "courseId": course_id,
        "contentId": content_id,
        "activityId": activity_id,
        "activityTitle": activity.title,
        "teammate": ratee_name,
        "criterion": crit.title,
        "before": before,
        "after": {"option": opt.title, "weight": opt.weight},
        "dryRun": dry_run,
        "result": result,
    }))
}

/// `ntulearn_fbf_comment` — upsert the written "Overall comments" review for
/// ONE teammate. dry_run=true (default) resolves the teammate/task and shows
/// the text without sending it.
async fn h_fbf_comment(state: &AppState, args: &Value) -> Result<Value, String> {
    let course_id = str_arg(args, "course_id").unwrap_or("").to_string();
    validate_bb_id(&course_id, "course_id")?;
    let content_id = str_arg(args, "content_id").unwrap_or("").to_string();
    validate_bb_id(&content_id, "content_id")?;
    let teammate = str_arg(args, "teammate").unwrap_or("").trim().to_string();
    let text = str_arg(args, "text").unwrap_or("").trim().to_string();
    // Optional override for "areas to improve" style criteria, so the same
    // praise is not pasted into an improvement box.
    let improve_text = str_arg(args, "improve_text")
        .unwrap_or("")
        .trim()
        .to_string();
    if teammate.is_empty() {
        return Err("teammate must be a non-empty substring of a teammate name".to_string());
    }
    if text.is_empty() {
        return Err("text must be a non-empty review".to_string());
    }
    let dry_run = bool_arg(args, "dry_run", true);
    let _fmt = response_format(args)?;

    let session = crate::fbf::FbfSession::launch(state, &course_id, &content_id).await?;
    let activity_id = session.activity_id().to_string();
    let activity = session.activity(&activity_id).await?;
    let tasks = session.tasks(&activity_id).await?;
    let task = fbf_find_task(&tasks, "overall comment")?;

    let allocations = fbf_given_with_names(&session, task).await?;
    let names: Vec<String> = allocations.iter().map(|(_, _, n)| n.clone()).collect();
    let idx = fbf_resolve_unique("teammate", &teammate, &names)?;
    let (alloc_id, _ratee, ratee_name) = &allocations[idx];

    // The existing written review (upsert replaces it); shown so the agent
    // sees what it is about to overwrite.
    let before = session
        .reviews(alloc_id)
        .await?
        .into_iter()
        .map(|r| r.body)
        .find(|b| !b.trim().is_empty())
        .unwrap_or_default();

    let result = session
        .upsert_review(alloc_id, &activity_id, &text, &improve_text, dry_run)
        .await?;

    Ok(json!({
        "courseId": course_id,
        "contentId": content_id,
        "activityId": activity_id,
        "activityTitle": activity.title,
        "teammate": ratee_name,
        "task": task.title.clone().unwrap_or_else(|| "Overall comments".to_string()),
        "before": if before.is_empty() { Value::Null } else { Value::String(before) },
        "after": text,
        "dryRun": dry_run,
        "result": result,
    }))
}

// ---------------------------------------------------------------------------
// Peerceptiv (PCV) peer evaluation (docs/PCV_SPEC.md §4)
// ---------------------------------------------------------------------------

/// LTI *placement* content type — Peerceptiv activities surface as
/// `resource/x-bb-bltiplacement-*` in the content tree (FeedbackFruits uses
/// `resource/x-bb-blti-link`, so the two filters never collide).
const PCV_PLACEMENT_PREFIX: &str = "resource/x-bb-bltiplacement";

/// Display label for a Peerceptiv user: `lastName` is the real name,
/// `firstName` the school prefix ("CCDS", "EEE", ...).
fn pcv_display_name(first: &str, last: &str) -> String {
    let last = last.trim();
    if last.is_empty() {
        first.trim().to_string()
    } else {
        last.to_string()
    }
}

/// Group members as (userId, firstName, lastName) tuples.
fn pcv_group_members(group: &Value) -> Vec<(String, String, String)> {
    group
        .get("groupMembers")
        .and_then(|v| v.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|m| {
                    let u = m.get("user")?;
                    Some((
                        gs(u, "userId").unwrap_or("").to_string(),
                        gs(u, "firstName").unwrap_or("").to_string(),
                        gs(u, "lastName").unwrap_or("").to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Resolve a teammate by case-insensitive substring against the group
/// members' display names. Both name forms are matched (real name in
/// lastName, "school realname" combined), so "Isabelle" and "MAE" both
/// work; exactly one hit wins.
fn pcv_resolve_teammate(
    members: &[(String, String, String)],
    my_uid: &str,
    needle: &str,
) -> Result<(String, String), String> {
    let n = needle.to_lowercase();
    let teammates: Vec<&(String, String, String)> =
        members.iter().filter(|(uid, _, _)| uid != my_uid).collect();
    let hits: Vec<&(String, String, String)> = teammates
        .iter()
        .copied()
        .filter(|(_, first, last)| {
            let last_l = last.to_lowercase();
            let full_l = format!("{} {}", first, last).to_lowercase();
            last_l.contains(&n) || full_l.contains(&n)
        })
        .collect();
    let names = |rows: &[&(String, String, String)]| -> Vec<String> {
        rows.iter()
            .map(|(_, f, l)| pcv_display_name(f, l))
            .collect()
    };
    match hits.len() {
        0 => Err(format!(
            "no teammate matches {:?}. Your group: {}",
            needle,
            names(&teammates).join("; ")
        )),
        1 => Ok((hits[0].0.clone(), pcv_display_name(&hits[0].1, &hits[0].2))),
        _ => Err(format!(
            "teammate {:?} is ambiguous — matches: {}. Use a longer substring.",
            needle,
            names(&hits).join("; ")
        )),
    }
}

/// `ntulearn_pcv_status` — Peerceptiv peer-evaluation status: assignment
/// name, deadlines, evaluation progress, group name and per-teammate
/// done/not-done. With content_id omitted, every LTI placement in the
/// course content tree is probed; non-Peerceptiv tools are skipped, not
/// errors.
async fn h_pcv_status(state: &AppState, args: &Value) -> Result<Value, String> {
    let course_id = str_arg(args, "course_id").unwrap_or("").to_string();
    validate_bb_id(&course_id, "course_id")?;
    let content_id = str_arg(args, "content_id").map(|s| s.to_string());
    if let Some(cid) = &content_id {
        validate_bb_id(cid, "content_id")?;
    }
    let _fmt = response_format(args)?;

    let candidates: Vec<(String, String)> = match &content_id {
        Some(cid) => vec![(cid.clone(), String::new())],
        None => {
            // Discovery: the full content walk (same primitive as
            // get_content_tree) filtered to LTI placements — the only items
            // a Peerceptiv launch could succeed on.
            let nodes = walk_content(&state.client, &course_id, MAX_DEPTH).await;
            nodes
                .iter()
                .filter(|n| handler_id(&n.item).starts_with(PCV_PLACEMENT_PREFIX))
                .filter_map(|n| {
                    n.item
                        .get("id")
                        .and_then(|v| v.as_str())
                        .map(|id| (id.to_string(), item_title(&n.item)))
                })
                .collect()
        }
    };

    let mut activities: Vec<Value> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();
    for (cid, title) in &candidates {
        // Launches run sequentially on purpose: each writes the shared
        // pcv.json session cache and the LTI chain rides the same NTULearn
        // cookie — concurrent chains would race on both.
        match crate::pcv::PcvSession::launch(state, &course_id, cid).await {
            Ok(session) => match pcv_activity_report(&session, cid).await {
                Ok(report) => activities.push(report),
                Err(e) => {
                    if content_id.is_some() {
                        // An explicit content_id the caller asked for must
                        // fail loudly rather than vanish into "skipped".
                        return Err(format!(
                            "content_id {cid} opened as Peerceptiv but its assignment could not be read: {e}"
                        ));
                    }
                    skipped.push(json!({"contentId": cid, "title": title, "reason": e}));
                }
            },
            Err(e) => {
                if content_id.is_some() {
                    return Err(format!(
                        "content_id {cid} is not a Peerceptiv activity (launch failed): {e}"
                    ));
                }
                skipped.push(json!({"contentId": cid, "title": title, "reason": e}));
            }
        }
    }

    let mut payload = Map::new();
    payload.insert("courseId".to_string(), Value::String(course_id));
    if let Some(cid) = &content_id {
        payload.insert("contentId".to_string(), Value::String(cid.clone()));
    }
    if candidates.is_empty() {
        // A course with no LTI placements is a valid (boring) answer.
        payload.insert(
            "note".to_string(),
            Value::String(
                "no LTI placements (resource/x-bb-bltiplacement) found in this course's content tree — there is no Peerceptiv activity to report"
                    .to_string(),
            ),
        );
    }
    payload.insert("activities".to_string(), Value::Array(activities));
    payload.insert("skippedLinks".to_string(), Value::Array(skipped));
    Ok(Value::Object(payload))
}

/// One activity rendered for pcv_status: assignment name, deadlines, my
/// evaluation progress, group name and teammates with done/not-done.
async fn pcv_activity_report(
    session: &crate::pcv::PcvSession,
    content_id: &str,
) -> Result<Value, String> {
    let aid = session.assignment_id().to_string();
    let uid = session.user_id().to_string();
    let assignment = session.assignment(&aid).await?;
    let progress = session.progress(&aid, &uid).await?;

    // Group members minus self = evaluation targets. The progress object's
    // own group is authoritative; fall back to the groups listing when it
    // carries no members.
    let progress_group = progress.get("group").cloned().unwrap_or(Value::Null);
    let group = if pcv_group_members(&progress_group).len() > 1 {
        progress_group
    } else {
        session.my_group(&aid, &uid).await.unwrap_or(progress_group)
    };
    let members = pcv_group_members(&group);

    // Per-teammate done state. progress.completedEvaluations carries full
    // eval records when any exist; otherwise fall back to my given evals
    // (rated on every rubric item == done).
    let mut done_uids: HashSet<String> = progress
        .get("completedEvaluations")
        .and_then(|v| v.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|e| {
                    e.get("peer")
                        .and_then(|p| gs(p, "userId"))
                        .map(String::from)
                })
                .collect()
        })
        .unwrap_or_default();
    let given = session.evals_given(&aid, &uid).await?;
    let rubric = session.rubric(&aid, "MEMBER").await.ok();
    let n_rating_items = rubric.as_ref().map(|r| r.rating_items.len()).unwrap_or(0);
    if done_uids.is_empty() && !given.is_empty() {
        for e in &given {
            let complete = n_rating_items == 0 || e.review_ratings.len() >= n_rating_items;
            if complete {
                done_uids.insert(e.peer.user_id.clone());
            }
        }
    }

    let teammates: Vec<Value> = members
        .iter()
        .filter(|(muid, _, _)| muid != &uid)
        .map(|(muid, first, last)| {
            json!({
                "userId": muid,
                "name": pcv_display_name(first, last),
                "done": done_uids.contains(muid),
            })
        })
        .collect();

    Ok(json!({
        "contentId": content_id,
        "assignmentId": aid,
        "pcvCourseId": session.pcv_course_id(),
        "title": gs(&assignment, "assignmentName").unwrap_or("untitled"),
        "deadlines": {
            "peerEvaluationDeadline": assignment.get("peerEvaluationDeadline").cloned().unwrap_or(Value::Null),
            "effectivePeerEvaluationDeadline": assignment.get("effectivePeerEvaluationDeadline").cloned().unwrap_or(Value::Null),
            "submissionDeadline": assignment.get("submissionDeadline").cloned().unwrap_or(Value::Null),
            "reviewDeadline": assignment.get("reviewDeadline").cloned().unwrap_or(Value::Null),
        },
        "progress": crate::pcv::progress_summary(&progress),
        "group": gs(&group, "groupName").unwrap_or(""),
        "teammates": teammates,
    }))
}

/// `ntulearn_pcv_evals` — ratings + comments for one Peerceptiv activity.
/// direction='given': what I gave each teammate, one row per criterion
/// (unrated criteria show a null score). direction='received': what
/// teammates gave me — empty until results are released, which is reported
/// explicitly, never as an error.
async fn h_pcv_evals(state: &AppState, args: &Value) -> Result<Value, String> {
    let course_id = str_arg(args, "course_id").unwrap_or("").to_string();
    validate_bb_id(&course_id, "course_id")?;
    let content_id = str_arg(args, "content_id").unwrap_or("").to_string();
    validate_bb_id(&content_id, "content_id")?;
    let direction = str_arg(args, "direction").unwrap_or("given").to_lowercase();
    if direction != "given" && direction != "received" {
        return Err("direction must be 'given' or 'received'".to_string());
    }
    let _fmt = response_format(args)?;

    let session = crate::pcv::PcvSession::launch(state, &course_id, &content_id).await?;
    let aid = session.assignment_id().to_string();
    let uid = session.user_id().to_string();
    let assignment = session.assignment(&aid).await?;
    let rubric = session.rubric(&aid, "MEMBER").await?;

    let evals = if direction == "given" {
        session.evals_given(&aid, &uid).await?
    } else {
        // Pre-release, Peerceptiv gates the received endpoint itself (403
        // while hideRatingResults & co. are true) — a normal state that is
        // reported, never an error. A 403 with the flags off is a real
        // session problem and surfaces normally.
        let hidden = [
            "hideRatingResults",
            "hideCommentResults",
            "hideGradeResults",
        ]
        .iter()
        .any(|k| assignment.get(k).and_then(|v| v.as_bool()) == Some(true));
        match session.evals_received(&aid, &uid).await {
            Ok(v) => v,
            Err(e) if hidden => Vec::new(),
            Err(e) => return Err(e),
        }
    };

    let include_unrated = direction == "given";
    let mut reviews: Vec<Value> = Vec::new();
    let mut unrated = 0usize;
    for e in &evals {
        let mate = pcv_display_name(&e.peer.first_name, &e.peer.last_name);
        for item in &rubric.rating_items {
            let existing = e
                .review_ratings
                .iter()
                .find(|r| r.rating_id == item.rating_id);
            match existing {
                Some(r) => {
                    let level = item
                        .rating_levels
                        .iter()
                        .find(|l| l.score == r.score)
                        .map(|l| l.level_description.clone())
                        .unwrap_or_default();
                    reviews.push(json!({
                        "teammate": mate,
                        "criterion": item.name,
                        "score": r.score,
                        "level": level,
                    }));
                }
                None => {
                    if include_unrated {
                        unrated += 1;
                        reviews.push(json!({
                            "teammate": mate,
                            "criterion": item.name,
                            "score": Value::Null,
                            "level": Value::Null,
                        }));
                    }
                }
            }
        }
    }

    let mut comments: Vec<Value> = Vec::new();
    for e in &evals {
        let mate = pcv_display_name(&e.peer.first_name, &e.peer.last_name);
        for c in &e.review_comments {
            if c.comment.trim().is_empty() {
                continue;
            }
            let prompt = rubric
                .comment_items
                .iter()
                .find(|ci| ci.comment_id == c.comment_id)
                .map(|ci| ci.comment_name.clone())
                .unwrap_or_default();
            comments.push(json!({
                "teammate": mate,
                "prompt": prompt,
                "comment": c.comment,
            }));
        }
    }

    let mut payload = Map::new();
    payload.insert("courseId".to_string(), Value::String(course_id));
    payload.insert("contentId".to_string(), Value::String(content_id));
    payload.insert("assignmentId".to_string(), Value::String(aid));
    payload.insert(
        "assignmentTitle".to_string(),
        Value::String(
            gs(&assignment, "assignmentName")
                .unwrap_or("untitled")
                .to_string(),
        ),
    );
    payload.insert("direction".to_string(), Value::String(direction.clone()));
    payload.insert("teammates".to_string(), Value::from(evals.len() as u64));
    payload.insert("unrated".to_string(), Value::from(unrated as u64));
    if direction == "received" && reviews.is_empty() && comments.is_empty() {
        payload.insert("released".to_string(), Value::Bool(false));
        payload.insert(
            "note".to_string(),
            Value::String(
                "reviews from teammates are not yet released — Peerceptiv returns no data until results are published"
                    .to_string(),
            ),
        );
    }
    payload.insert("reviews".to_string(), Value::Array(reviews));
    payload.insert("comments".to_string(), Value::Array(comments));
    Ok(Value::Object(payload))
}

/// `ntulearn_pcv_rate` — upsert ONE rubric rating for ONE teammate.
/// teammate/criterion resolve case-insensitively by substring; level is a
/// level-description substring ("Always") or a plain score 1-10. The peer
/// evaluation is found-or-created (the create shows in dry-run too).
/// dry_run=true (default) resolves everything and shows the request
/// without sending it.
async fn h_pcv_rate(state: &AppState, args: &Value) -> Result<Value, String> {
    let course_id = str_arg(args, "course_id").unwrap_or("").to_string();
    validate_bb_id(&course_id, "course_id")?;
    let content_id = str_arg(args, "content_id").unwrap_or("").to_string();
    validate_bb_id(&content_id, "content_id")?;
    let teammate = str_arg(args, "teammate").unwrap_or("").trim().to_string();
    let criterion = str_arg(args, "criterion").unwrap_or("").trim().to_string();
    // The schema declares a string, but an agent may send the score as a
    // JSON number; accept both (same convention as parse_page_range).
    let level = match args.get("level") {
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
    .trim()
    .to_string();
    if teammate.is_empty() {
        return Err("teammate must be a non-empty substring of a teammate name".to_string());
    }
    if criterion.is_empty() {
        return Err("criterion must be a non-empty substring of a criterion name".to_string());
    }
    if level.is_empty() {
        return Err(
            "level must be a level description substring (e.g. 'Always') or a score 1-10"
                .to_string(),
        );
    }
    let dry_run = bool_arg(args, "dry_run", true);
    let _fmt = response_format(args)?;

    let session = crate::pcv::PcvSession::launch(state, &course_id, &content_id).await?;
    let aid = session.assignment_id().to_string();
    let uid = session.user_id().to_string();
    let assignment = session.assignment(&aid).await?;
    let rubric = session.rubric(&aid, "MEMBER").await?;

    // Teammate: my group members minus self.
    let group = session.my_group(&aid, &uid).await?;
    let members = pcv_group_members(&group);
    let (peer_uid, peer_name) = pcv_resolve_teammate(&members, &uid, &teammate)?;

    // Criterion: substring against rubric rating names.
    let crit_labels: Vec<String> = rubric.rating_items.iter().map(|r| r.name.clone()).collect();
    let item = &rubric.rating_items[fbf_resolve_unique("criterion", &criterion, &crit_labels)?];

    // Level: a plain 1-10 score or a level-description substring, resolved
    // against this criterion's own levels.
    let score = item.resolve_level(&level)?;
    let level_desc = item
        .rating_levels
        .iter()
        .find(|l| l.score == score)
        .map(|l| l.level_description.clone())
        .unwrap_or_default();

    // Find-or-create my eval for this teammate. A dry-run create mints no
    // real peerEvalId, so the rating itself is previewed rather than
    // upserted in that case.
    let given = session.evals_given(&aid, &uid).await?;
    let existing = given.iter().find(|e| e.peer.user_id == peer_uid);
    let (eval_id, created, create_result) = match existing {
        Some(e) => (Some(e.peer_eval_id.clone()), false, Value::Null),
        None => {
            let res = session.create_eval(&aid, &peer_uid, dry_run).await?;
            let id = if dry_run {
                None
            } else {
                res.get("response")
                    .and_then(|r| r.get("peerEvalId"))
                    .and_then(|v| v.as_str())
                    .map(String::from)
            };
            (id, true, res)
        }
    };

    // before → after: the existing score for this (teammate, criterion)
    // pair, so the agent sees what it is about to change.
    let before = existing
        .and_then(|e| {
            e.review_ratings
                .iter()
                .find(|r| r.rating_id == item.rating_id)
        })
        .map(|r| {
            json!({
                "score": r.score,
                "level": item
                    .rating_levels
                    .iter()
                    .find(|l| l.score == r.score)
                    .map(|l| l.level_description.clone())
                    .unwrap_or_default(),
            })
        });

    let upsert = match &eval_id {
        Some(id) => Some(
            session
                .upsert_rating(id, &item.rating_id, score, dry_run)
                .await?,
        ),
        None => None,
    };

    let mut payload = Map::new();
    payload.insert("courseId".to_string(), Value::String(course_id));
    payload.insert("contentId".to_string(), Value::String(content_id));
    payload.insert("assignmentId".to_string(), Value::String(aid));
    payload.insert(
        "assignmentTitle".to_string(),
        Value::String(
            gs(&assignment, "assignmentName")
                .unwrap_or("untitled")
                .to_string(),
        ),
    );
    payload.insert("teammate".to_string(), Value::String(peer_name));
    payload.insert("criterion".to_string(), Value::String(item.name.clone()));
    payload.insert("before".to_string(), before.unwrap_or(Value::Null));
    payload.insert(
        "after".to_string(),
        json!({"score": score, "level": level_desc}),
    );
    payload.insert("createdEval".to_string(), Value::Bool(created));
    if created && dry_run {
        payload.insert(
            "note".to_string(),
            Value::String(
                "no peer evaluation exists for this teammate yet — dry run shows the eval that would be created plus this rating"
                    .to_string(),
            ),
        );
    }
    payload.insert("dryRun".to_string(), Value::Bool(dry_run));
    payload.insert("result".to_string(), upsert.unwrap_or(create_result));
    Ok(Value::Object(payload))
}

/// `ntulearn_pcv_comment` — upsert the written comment(s) for ONE teammate.
/// Writes the first comment prompt, or every prompt when the rubric has
/// several (mirroring fbf_comment's multi-criterion behavior). Find-or-
/// create eval, before/after shown. dry_run=true (default) resolves the
/// teammate and shows the text without sending it.
async fn h_pcv_comment(state: &AppState, args: &Value) -> Result<Value, String> {
    let course_id = str_arg(args, "course_id").unwrap_or("").to_string();
    validate_bb_id(&course_id, "course_id")?;
    let content_id = str_arg(args, "content_id").unwrap_or("").to_string();
    validate_bb_id(&content_id, "content_id")?;
    let teammate = str_arg(args, "teammate").unwrap_or("").trim().to_string();
    let text = str_arg(args, "text").unwrap_or("").trim().to_string();
    if teammate.is_empty() {
        return Err("teammate must be a non-empty substring of a teammate name".to_string());
    }
    if text.is_empty() {
        return Err("text must be a non-empty comment".to_string());
    }
    let dry_run = bool_arg(args, "dry_run", true);
    let _fmt = response_format(args)?;

    let session = crate::pcv::PcvSession::launch(state, &course_id, &content_id).await?;
    let aid = session.assignment_id().to_string();
    let uid = session.user_id().to_string();
    let assignment = session.assignment(&aid).await?;
    let rubric = session.rubric(&aid, "MEMBER").await?;

    let group = session.my_group(&aid, &uid).await?;
    let members = pcv_group_members(&group);
    let (peer_uid, peer_name) = pcv_resolve_teammate(&members, &uid, &teammate)?;

    // Find-or-create my eval for this teammate (same contract as rate).
    let given = session.evals_given(&aid, &uid).await?;
    let existing = given.iter().find(|e| e.peer.user_id == peer_uid);
    let (eval_id, created, create_result) = match existing {
        Some(e) => (Some(e.peer_eval_id.clone()), false, Value::Null),
        None => {
            let res = session.create_eval(&aid, &peer_uid, dry_run).await?;
            let id = if dry_run {
                None
            } else {
                res.get("response")
                    .and_then(|r| r.get("peerEvalId"))
                    .and_then(|v| v.as_str())
                    .map(String::from)
            };
            (id, true, res)
        }
    };

    let mut comment_rows: Vec<Value> = Vec::new();
    match &eval_id {
        Some(id) => {
            for ci in &rubric.comment_items {
                let before = existing
                    .and_then(|e| {
                        e.review_comments
                            .iter()
                            .find(|c| c.comment_id == ci.comment_id)
                    })
                    .map(|c| c.comment.clone())
                    .unwrap_or_default();
                let result = session
                    .upsert_comment(id, &ci.comment_id, &text, dry_run)
                    .await?;
                comment_rows.push(json!({
                    "commentId": ci.comment_id,
                    "prompt": ci.comment_name,
                    "before": if before.is_empty() { Value::Null } else { Value::String(before) },
                    "result": result,
                }));
            }
        }
        None => {
            // Dry-run create: show what each prompt would receive.
            for ci in &rubric.comment_items {
                comment_rows.push(json!({
                    "commentId": ci.comment_id,
                    "prompt": ci.comment_name,
                    "before": Value::Null,
                }));
            }
        }
    }

    let mut payload = Map::new();
    payload.insert("courseId".to_string(), Value::String(course_id));
    payload.insert("contentId".to_string(), Value::String(content_id));
    payload.insert("assignmentId".to_string(), Value::String(aid));
    payload.insert(
        "assignmentTitle".to_string(),
        Value::String(
            gs(&assignment, "assignmentName")
                .unwrap_or("untitled")
                .to_string(),
        ),
    );
    payload.insert("teammate".to_string(), Value::String(peer_name));
    payload.insert("comments".to_string(), Value::Array(comment_rows));
    payload.insert("after".to_string(), Value::String(text));
    payload.insert("createdEval".to_string(), Value::Bool(created));
    if created && dry_run {
        payload.insert(
            "note".to_string(),
            Value::String(
                "no peer evaluation exists for this teammate yet — dry run shows the eval that would be created plus this comment"
                    .to_string(),
            ),
        );
    }
    payload.insert("dryRun".to_string(), Value::Bool(dry_run));
    if eval_id.is_none() {
        payload.insert("result".to_string(), create_result);
    }
    Ok(Value::Object(payload))
}

// ---------------------------------------------------------------------------
// Kaltura transcripts / video downloads (kaltura.rs; docs/KALTURA_SPEC.md)
// ---------------------------------------------------------------------------

/// Concurrent transcript fetches across a whole course. This bound IS the
/// politeness policy: 25 videos used to walk one at a time with a fixed
/// pause each; now at most this many manifest/playlist/segment requests
/// are in flight at once (VTT segments within one video add a further
/// bound of 6, see kaltura.rs). A real browser player issues comparable
/// bursts.
const KALTURA_ENTRY_CONCURRENCY: usize = 12;

/// `format` argument of kaltura_transcripts: which files to write per video.
fn resolve_transcript_format(args: &Value) -> Result<String, String> {
    let fmt = args
        .get("format")
        .map(|v| match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .unwrap_or_else(|| "both".to_string())
        .to_lowercase();
    if fmt != "vtt" && fmt != "txt" && fmt != "both" {
        return Err("format must be 'vtt', 'txt' or 'both'".to_string());
    }
    Ok(fmt)
}

/// `quality` argument of kaltura_download: which flavor to download.
fn resolve_quality(args: &Value) -> Result<String, String> {
    let q = args
        .get("quality")
        .map(|v| match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .unwrap_or_else(|| "smallest".to_string())
        .to_lowercase();
    if q != "smallest" && q != "best" && q != "original" {
        return Err("quality must be 'smallest', 'best' or 'original'".to_string());
    }
    Ok(q)
}

/// `save_dir` argument of kaltura_transcripts: explicit directory or the
/// server's download dir (resolved from NTULEARN_DOWNLOAD_DIR at startup).
fn resolve_save_dir(args: &Value, state: &AppState) -> Result<PathBuf, String> {
    match args.get("save_dir") {
        None => Ok(state.download_dir.clone()),
        Some(Value::String(s)) => {
            let s = s.trim();
            if s.is_empty() {
                return Err(
                    "save_dir cannot be empty — pass a directory path or omit it".to_string(),
                );
            }
            Ok(expand_tilde(s))
        }
        Some(_) => Err("save_dir must be a string directory path".to_string()),
    }
}

/// `videos` rows shared by kaltura_list and kaltura_transcripts discovery:
/// one row per embedded video with the ids the follow-up tools need.
fn kaltura_video_rows(embeds: &[KalturaEmbed]) -> Vec<Value> {
    embeds
        .iter()
        .map(|e| {
            json!({
                "contentId": e.content_id,
                "title": if e.title.trim().is_empty() { "untitled" } else { &e.title },
                "entryId": e.entry_id,
                "placementId": e.placement_id,
            })
        })
        .collect()
}

/// Fill empty embed titles ("" in the data-bbfile) with the content item's
/// own title, so listing rows and transcript filenames stay useful.
fn embeds_with_title_fallback(
    mut embeds: Vec<KalturaEmbed>,
    item_title: &str,
) -> Vec<KalturaEmbed> {
    for e in &mut embeds {
        if e.title.trim().is_empty() {
            e.title = item_title.to_string();
        }
    }
    embeds
}

/// Every Kaltura embed in one content item's body (REST + parse only — no
/// LTI launch; the embed metadata sits in the body's data-bbfile JSON).
async fn content_item_embeds(
    c: &NTULearnClient,
    course_id: &str,
    content_id: &str,
) -> Result<Vec<KalturaEmbed>, String> {
    let item = c
        .get_content_item_with_body(course_id, content_id, None)
        .await
        .map_err(err_str)?;
    Ok(embeds_with_title_fallback(
        crate::kaltura::extract_embeds_for(content_id, gs(&item, "body").unwrap_or("")),
        &item_title(&item),
    ))
}

/// Auto-discovery: walk the whole course content tree (parents + children,
/// same primitive as pcv_status) and parse every non-folder item's body for
/// Kaltura embeds. Body fetches run with bounded concurrency; an item whose
/// body cannot be read is collected in `skipped`, never an error — course
/// trees routinely contain items the REST API refuses to serve.
async fn discover_kaltura_embeds(
    c: &NTULearnClient,
    course_id: &str,
) -> (Vec<KalturaEmbed>, Vec<Value>) {
    let nodes: Vec<(String, String)> = walk_content(c, course_id, MAX_DEPTH)
        .await
        .into_iter()
        .filter(|n| !is_folder(&n.item))
        .filter_map(|n| {
            let id = n.item.get("id").and_then(|v| v.as_str())?.to_string();
            Some((id, item_title(&n.item)))
        })
        .collect();

    let fetched = bounded_map(nodes, fanout_limit(), |(content_id, title)| async move {
        let res = c
            .get_content_item_with_body(course_id, &content_id, None)
            .await;
        (content_id, title, res)
    })
    .await;

    let mut embeds: Vec<KalturaEmbed> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();
    for (content_id, title, res) in fetched {
        match res {
            Ok(item) => embeds.extend(embeds_with_title_fallback(
                crate::kaltura::extract_embeds_for(&content_id, gs(&item, "body").unwrap_or("")),
                &title,
            )),
            Err(e) => skipped.push(json!({
                "contentId": content_id,
                "title": title,
                "reason": err_str(e),
            })),
        }
    }
    (embeds, skipped)
}

/// One failed transcript fetch as a per-video note row. A video without
/// captions is a NOTE, never a whole-call error: the caller asked for the
/// course's captions and one caption-less video must not hide the rest. The
/// no-captions wording lives here (stable); other errors pass through the
/// core module's LLM-readable message (which mentions ntulearn_login when
/// it smells like auth).
fn transcript_outcome_as_note(
    content_id: &str,
    title: &str,
    entry_id: &str,
    err: &TranscriptError,
) -> Value {
    let note = match err {
        TranscriptError::NoCaptions => "no caption track — this video has no captions".to_string(),
        other => other.message(),
    };
    json!({
        "contentId": content_id,
        "title": title,
        "entryId": entry_id,
        "note": note,
    })
}

/// Whether a Kaltura error message smells like an expired session (the
/// player KS minted at launch died mid-run). Those are worth one relaunch +
/// retry; anything else (DNS, 5xx, malformed manifest) is not.
fn is_auth_shaped(message: &str) -> bool {
    let m = message.to_lowercase();
    m.contains("invalid_ks")
        || m.contains("401")
        || m.contains("unauthorized")
        || m.contains("expired")
        || m.contains("ntulearn_login")
}

/// Choose the flavor to download. 'smallest' = fewest bytes among flavors
/// with a nonzero height (audio-only flavors have height 0); 'best' = the
/// largest non-original flavor; 'original' = the source upload. When the
/// request cannot be satisfied (e.g. 'original' on a re-encoded entry), the
/// smallest available flavor is returned with an explanatory note so the
/// download still succeeds.
fn pick_flavor(
    flavors: &[FlavorAsset],
    quality: &str,
) -> Result<(FlavorAsset, Option<String>), String> {
    if flavors.is_empty() {
        return Err("Kaltura returned no downloadable flavors for this entry".to_string());
    }
    let requested: Option<&FlavorAsset> = match quality {
        "original" => flavors.iter().find(|f| f.is_original),
        "best" => flavors
            .iter()
            .filter(|f| !f.is_original)
            .max_by_key(|f| f.size_bytes),
        // default: 'smallest'
        _ => flavors
            .iter()
            .filter(|f| f.height > 0)
            .min_by_key(|f| f.size_bytes),
    };
    match requested {
        Some(f) => Ok(((*f).clone(), None)),
        None => {
            let fallback = flavors
                .iter()
                .min_by_key(|f| f.size_bytes)
                .cloned()
                .ok_or_else(|| {
                    "Kaltura returned no downloadable flavors for this entry".to_string()
                })?;
            Ok((
                fallback,
                Some(format!(
                    "no '{quality}' flavor available for this entry — downloaded the smallest flavor instead"
                )),
            ))
        }
    }
}

/// `ntulearn_kaltura_list` — every Kaltura-embedded video in a course:
/// content id, title, Kaltura entry id and LTI placement id. Discovery is
/// pure REST (no LTI launch): the embed metadata sits in the content body's
/// data-bbfile JSON. Non-Kaltura LTI tools are skipped, not errors.
async fn h_kaltura_list(state: &AppState, args: &Value) -> Result<Value, String> {
    let course_id = str_arg(args, "course_id").unwrap_or("").to_string();
    validate_bb_id(&course_id, "course_id")?;
    let content_id = str_arg(args, "content_id").map(|s| s.to_string());
    if let Some(cid) = &content_id {
        validate_bb_id(cid, "content_id")?;
    }
    let _fmt = response_format(args)?;

    let (embeds, skipped): (Vec<KalturaEmbed>, Vec<Value>) = match &content_id {
        Some(cid) => {
            let embeds = content_item_embeds(&state.client, &course_id, cid).await?;
            if embeds.is_empty() {
                return Err(format!(
                    "content_id {cid} contains no Kaltura video embeds — run ntulearn_kaltura_list without content_id to see every video in the course"
                ));
            }
            (embeds, Vec::new())
        }
        None => discover_kaltura_embeds(&state.client, &course_id).await,
    };

    let mut payload = Map::new();
    payload.insert("courseId".to_string(), Value::String(course_id));
    if let Some(cid) = &content_id {
        payload.insert("contentId".to_string(), Value::String(cid.clone()));
    }
    if !skipped.is_empty() {
        payload.insert("skippedLinks".to_string(), Value::Array(skipped));
    }
    if embeds.is_empty() {
        payload.insert(
            "note".to_string(),
            Value::String(
                "no Kaltura video embeds found in this course's content tree — videos may be linked another way (direct URL, non-Kaltura tool)"
                    .to_string(),
            ),
        );
    }
    payload.insert("videoCount".to_string(), json!(embeds.len()));
    payload.insert(
        "videos".to_string(),
        Value::Array(kaltura_video_rows(&embeds)),
    );
    Ok(Value::Object(payload))
}

/// `ntulearn_kaltura_transcripts` — caption tracks for one video or the
/// whole course, written as .vtt/.txt files. The session is launched lazily
/// once (one launch's KS covers every entry of the course, live-verified)
/// and relaunched only when an auth-shaped error appears mid-run.
async fn h_kaltura_transcripts(state: &AppState, args: &Value) -> Result<Value, String> {
    let course_id = str_arg(args, "course_id").unwrap_or("").to_string();
    validate_bb_id(&course_id, "course_id")?;
    let content_id = str_arg(args, "content_id").map(|s| s.to_string());
    if let Some(cid) = &content_id {
        validate_bb_id(cid, "content_id")?;
    }
    let format = resolve_transcript_format(args)?;
    let save_dir = resolve_save_dir(args, state)?;
    let _fmt = response_format(args)?;

    let (embeds, skipped): (Vec<KalturaEmbed>, Vec<Value>) = match &content_id {
        Some(cid) => {
            let embeds = content_item_embeds(&state.client, &course_id, cid).await?;
            if embeds.is_empty() {
                return Err(format!(
                    "content_id {cid} contains no Kaltura video embeds — run ntulearn_kaltura_list to find one"
                ));
            }
            (embeds, Vec::new())
        }
        None => discover_kaltura_embeds(&state.client, &course_id).await,
    };

    let out_dir = save_dir.join("kaltura").join(&course_id);
    let mut payload = Map::new();
    payload.insert("courseId".to_string(), Value::String(course_id.clone()));
    if let Some(cid) = &content_id {
        payload.insert("contentId".to_string(), Value::String(cid.clone()));
    }
    if !skipped.is_empty() {
        payload.insert("skippedLinks".to_string(), Value::Array(skipped));
    }
    payload.insert("format".to_string(), Value::String(format.clone()));
    payload.insert(
        "outputDir".to_string(),
        Value::String(out_dir.to_string_lossy().to_string()),
    );
    if embeds.is_empty() {
        payload.insert(
            "note".to_string(),
            Value::String(
                "no Kaltura video embeds found in this course's content tree — there are no transcripts to fetch"
                    .to_string(),
            ),
        );
        payload.insert("videoCount".to_string(), json!(0));
        payload.insert("videos".to_string(), Value::Array(Vec::new()));
        payload.insert("notes".to_string(), Value::Array(Vec::new()));
        return Ok(Value::Object(payload));
    }

    if let Err(e) = tokio::fs::create_dir_all(&out_dir).await {
        return Err(format!(
            "cannot create transcript directory {}: {e}",
            out_dir.to_string_lossy()
        ));
    }

    // Launch once, up front, through the first embed — with the on-disk
    // session cache (kaltura.rs) this is normally a cache hit; a cold miss
    // runs the ~7-hop LTI chain once instead of hidden inside the first
    // entry's task. An explicit content_id the caller named must fail
    // loudly; in whole-course mode a failed launch degrades to per-entry
    // notes (one broken placement must not hide the others).
    let session: Option<KalturaSession> =
        match KalturaSession::launch(state, &course_id, &embeds[0].content_id).await {
            Ok(s) => Some(s),
            Err(e) => {
                if content_id.is_some() {
                    return Err(format!(
                        "Kaltura launch for content_id {} failed: {e}",
                        embeds[0].content_id
                    ));
                }
                None
            }
        };

    // Fetch every entry's transcript with bounded concurrency; bounded_map
    // preserves input order, so rows and notes come out in course order.
    // Failures never short-circuit the course run.
    let outcomes: Vec<Value> = bounded_map(embeds.clone(), KALTURA_ENTRY_CONCURRENCY, |embed| {
        let session = session.clone();
        let course_id = course_id.clone();
        let format = format.clone();
        let out_dir = out_dir.clone();
        let state = state;
        async move {
            // Lazy per-entry launch only when the up-front launch failed
            // (whole-course mode): each entry tries its own placement.
            let session = match session {
                Some(s) => s,
                None => match KalturaSession::launch(state, &course_id, &embed.content_id).await {
                    Ok(s) => s,
                    Err(e) => {
                        return transcript_outcome_as_note(
                            &embed.content_id,
                            &embed.title,
                            &embed.entry_id,
                            &TranscriptError::Network(format!("launch failed: {e}")),
                        );
                    }
                },
            };
            let mut result = session.transcript(&embed.entry_id).await;
            // Auth-shaped failure -> the KS died mid-run; relaunch once
            // (relaunch_if_stale de-duplicates concurrent relaunches) and
            // retry before reporting the video as a note.
            if let Err(TranscriptError::Network(m)) = &result {
                if is_auth_shaped(m) {
                    result = match KalturaSession::relaunch_if_stale(
                        state,
                        &course_id,
                        session.ks(),
                        &embed.content_id,
                    )
                    .await
                    {
                        Ok(s) => s.transcript(&embed.entry_id).await,
                        Err(e) => Err(TranscriptError::Network(format!(
                            "session relaunch failed: {e}"
                        ))),
                    };
                }
            }
            match result {
                Ok(t) => {
                    let title = if embed.title.trim().is_empty() {
                        "untitled".to_string()
                    } else {
                        embed.title.clone()
                    };
                    let base = parsers::sanitize_filename(&title);
                    let mut row = json!({
                        "contentId": embed.content_id,
                        "title": title,
                        "entryId": embed.entry_id,
                        "label": t.label,
                        "words": t.words,
                        "segments": t.segments,
                    });
                    if format != "txt" {
                        let path = out_dir.join(format!("{base}.vtt"));
                        if let Err(e) = tokio::fs::write(&path, &t.vtt).await {
                            return json!({
                                "contentId": embed.content_id,
                                "title": embed.title,
                                "entryId": embed.entry_id,
                                "note": format!("cannot write {}: {e}", path.to_string_lossy()),
                            });
                        }
                        row["vttFile"] = Value::String(path.to_string_lossy().to_string());
                    }
                    if format != "vtt" {
                        let path = out_dir.join(format!("{base}.txt"));
                        if let Err(e) = tokio::fs::write(&path, &t.text).await {
                            return json!({
                                "contentId": embed.content_id,
                                "title": embed.title,
                                "entryId": embed.entry_id,
                                "note": format!("cannot write {}: {e}", path.to_string_lossy()),
                            });
                        }
                        row["txtFile"] = Value::String(path.to_string_lossy().to_string());
                    }
                    row
                }
                Err(e) => {
                    transcript_outcome_as_note(&embed.content_id, &embed.title, &embed.entry_id, &e)
                }
            }
        }
    })
    .await;
    let mut videos: Vec<Value> = Vec::new();
    let mut notes: Vec<Value> = Vec::new();
    for outcome in outcomes {
        if outcome.get("note").is_some() {
            notes.push(outcome);
        } else {
            videos.push(outcome);
        }
    }

    payload.insert("videoCount".to_string(), json!(videos.len()));
    payload.insert("videos".to_string(), Value::Array(videos));
    payload.insert("notes".to_string(), Value::Array(notes));
    Ok(Value::Object(payload))
}

/// `ntulearn_kaltura_download` — the MP4 of one Kaltura video from
/// Kaltura's open CDN. quality 'smallest' (default) = fewest bytes with a
/// real resolution, 'best' = largest non-original flavor, 'original' =
/// source upload.
async fn h_kaltura_download(state: &AppState, args: &Value) -> Result<Value, String> {
    let course_id = str_arg(args, "course_id").unwrap_or("").to_string();
    validate_bb_id(&course_id, "course_id")?;
    let content_id = str_arg(args, "content_id").unwrap_or("").to_string();
    validate_bb_id(&content_id, "content_id")?;
    let quality = resolve_quality(args)?;
    let _fmt = response_format(args)?;

    let mut embeds = content_item_embeds(&state.client, &course_id, &content_id).await?;
    if embeds.is_empty() {
        return Err(format!(
            "content_id {content_id} contains no Kaltura video embeds — run ntulearn_kaltura_list to find one"
        ));
    }
    // One content item can embed several videos; the download takes the
    // first and says so when siblings exist.
    let mut notes: Vec<String> = Vec::new();
    if embeds.len() > 1 {
        notes.push(format!(
            "content item embeds {} videos; downloading the first (entry {}). Run ntulearn_kaltura_list with this content_id to see all entries",
            embeds.len(),
            embeds[0].entry_id
        ));
    }
    let embed = embeds.swap_remove(0);

    let session = KalturaSession::launch(state, &course_id, &embed.content_id).await?;
    let flavors = session.flavors(&embed.entry_id).await?;
    let (flavor, fallback_note) = pick_flavor(&flavors, &quality)?;
    if let Some(n) = fallback_note {
        notes.push(n);
    }

    let out_dir = state.download_dir.join("kaltura").join(&course_id);
    if let Err(e) = tokio::fs::create_dir_all(&out_dir).await {
        return Err(format!(
            "cannot create download directory {}: {e}",
            out_dir.to_string_lossy()
        ));
    }
    let title = if embed.title.trim().is_empty() {
        "untitled".to_string()
    } else {
        embed.title.clone()
    };
    let ext = if flavor.file_ext.is_empty() {
        "mp4".to_string()
    } else {
        flavor.file_ext.clone()
    };
    let path = out_dir.join(format!("{}.{}", parsers::sanitize_filename(&title), ext));
    let bytes = session
        .download_video(&embed.entry_id, &flavor, &path)
        .await?;

    let mut payload = json!({
        "courseId": course_id,
        "contentId": content_id,
        "entryId": embed.entry_id,
        "title": title,
        "quality": quality,
        "flavor": {
            "id": flavor.id,
            "width": flavor.width,
            "height": flavor.height,
            "sizeBytes": flavor.size_bytes,
            "isOriginal": flavor.is_original,
            "version": flavor.version,
            "fileExt": flavor.file_ext,
        },
        "bytes": bytes,
        "path": path.to_string_lossy().to_string(),
    });
    if !notes.is_empty() {
        payload["note"] = Value::String(notes.join(" · "));
    }
    Ok(payload)
}

// ---------------------------------------------------------------------------
// render_for_tool — mirror of Python common.emit: markdown text (when
// requested) followed by the pretty-JSON payload, or just the payload.
// ---------------------------------------------------------------------------

fn render_for_tool(short: &str, args: &Value, payload: &Value) -> Vec<ToolContent> {
    let fmt = match response_format(args) {
        Ok(f) => f,
        Err(e) => {
            // Response formatting is validated separately in each handler; this
            // path is unreachable in practice.
            return vec![ToolContent::text(e)];
        }
    };
    let text = if fmt == "markdown" {
        match short {
            "list_courses" => {
                let courses = payload
                    .get("courses")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                render::md_courses_full(&courses, payload)
            }
            "get_course_contents" => {
                let items = payload
                    .get("items")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                render::md_content_items_full(&items, payload)
            }
            "search_course_content" => {
                let matches = payload
                    .get("matches")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                render::md_search_results(&matches)
            }
            "download_file" => render::md_files(payload, "Files downloaded"),
            "read_file_content" => render::md_files(payload, "File contents"),
            "get_upcoming" => {
                let items = payload
                    .get("items")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                let errors = payload.get("courseErrors").cloned().unwrap_or(json!({}));
                render::md_upcoming(&items, payload, &errors)
            }
            "get_announcements" => {
                let items = payload
                    .get("announcements")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                render::md_announcements(&items, payload)
            }
            "get_gradebook" => {
                let columns = payload
                    .get("columns")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                let available = payload
                    .get("gradesAvailable")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                let err = payload
                    .get("gradeFetchError")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                render::md_gradebook(&columns, payload, available, err.as_deref())
            }
            "list_messages" => render::md_messages(payload),
            "read_message" => render::md_message(payload),
            "list_course_users" => render::md_course_users(payload),
            "list_course_groups" => render::md_course_groups(payload),
            "get_group_members" => render::md_group_members(payload),
            "get_gradebook_attempts" => render::md_gradebook_attempts(payload),
            "search_all_courses" => render::md_search_all_courses(payload),
            "get_content_tree" => render::md_content_tree(payload),
            "download_course" => render::md_download_course(payload),
            "whats_new" => render::md_whats_new(payload),
            "export_calendar_ics" => render::md_export_calendar(payload),
            "export_gradebook_csv" => render::md_export_gradebook(payload),
            "summarize_course" => render::md_summarize_course(payload),
            "fbf_status" => render::md_fbf_status(payload),
            "fbf_reviews" => render::md_fbf_reviews(payload),
            "fbf_rate" => render::md_fbf_rate(payload),
            "fbf_comment" => render::md_fbf_comment(payload),
            "pcv_status" => render::md_pcv_status(payload),
            "pcv_evals" => render::md_pcv_evals(payload),
            "pcv_rate" => render::md_pcv_rate(payload),
            "pcv_comment" => render::md_pcv_comment(payload),
            "kaltura_list" => render::md_kaltura_list(payload),
            "kaltura_transcripts" => render::md_kaltura_transcripts(payload),
            "kaltura_download" => render::md_kaltura_download(payload),
            _ => String::new(),
        }
    } else {
        String::new()
    };
    if fmt == "toon" {
        // Token-optimized payload: TOON instead of pretty JSON
        // (spec: https://github.com/toon-format/spec).
        return vec![ToolContent::text(crate::toon::encode(payload))];
    }
    let mut out: Vec<ToolContent> = Vec::new();
    if !text.is_empty() {
        out.push(ToolContent::text(text));
    }
    let json_text = serde_json::to_string_pretty(payload).unwrap_or_else(|_| "{}".to_string());
    out.push(ToolContent::text(json_text));
    out
}

#[cfg(test)]
mod args_guard_tests {
    use super::*;

    /// Regression guard: `serde_json::Value::to_string()` on a JSON *string*
    /// yields the JSON-encoded form WITH surrounding double quotes, which
    /// previously corrupted `since`/`until`/`type`/`query`/`user_id`/
    /// `destination_dir` argument extraction (e.g. "2026-01-01T00:00:00Z"
    /// became "\"2026-01-01T00:00:00Z\""). String args must go through
    /// `.as_str()`, never `.to_string()`.
    #[test]
    fn json_string_to_string_has_quotes() {
        let v = serde_json::json!("2026-02-01T00:00:00Z");
        let encoded = v.to_string();
        assert_eq!(encoded, "\"2026-02-01T00:00:00Z\"");
        assert_ne!(encoded, "2026-02-01T00:00:00Z");
        // the correct extraction path
        let via_str = v.as_str().unwrap_or("");
        assert_eq!(via_str, "2026-02-01T00:00:00Z");
    }

    #[test]
    fn str_arg_returns_unquoted() {
        let args = serde_json::json!({
            "since": "2026-02-01T00:00:00Z",
            "query": "lecture",
            "type": "GradebookColumn",
            "user_id": "user-1",
            "destination_dir": "/tmp/x"
        });
        assert_eq!(str_arg(&args, "since"), Some("2026-02-01T00:00:00Z"));
        assert_eq!(str_arg(&args, "query"), Some("lecture"));
        assert_eq!(str_arg(&args, "type"), Some("GradebookColumn"));
        assert_eq!(str_arg(&args, "user_id"), Some("user-1"));
        assert_eq!(str_arg(&args, "destination_dir"), Some("/tmp/x"));
    }

    /// Regression: list_course_users must surface names from the nested
    /// `user` object the Blackboard API actually returns, not the flat
    /// `userName`/`name` fields the reference Python server requested
    /// (which the API silently drops, leaving names empty).
    #[test]
    fn render_user_row_reads_nested_user_object() {
        use serde_json::json;
        let row = render_user_row(&json!({
            "userId": "_2275684_1",
            "user": {
                "userName": "junrong001",
                "name": {"given": "CCDS", "family": "LIM JUN RONG"}
            },
            "availability": {"available": "Yes"},
            "courseRoleId": "TeachingAssistant"
        }));
        assert_eq!(row["id"], json!("_2275684_1"));
        assert_eq!(row["userName"], json!("junrong001"));
        assert_eq!(row["name"], json!("CCDS LIM JUN RONG"));
        assert_eq!(row["role"], json!("TeachingAssistant"));
    }

    #[test]
    fn render_user_row_falls_back_to_flat_shape() {
        use serde_json::json;
        let row = render_user_row(&json!({
            "id": "_old_1",
            "userName": "flatuser",
            "name": "Flat User",
            "courseRoleId": "Student"
        }));
        assert_eq!(row["id"], json!("_old_1"));
        assert_eq!(row["userName"], json!("flatuser"));
        // user_name() on a string `name` falls back to userName (parity with
        // the Python `_user_name` helper).
        assert_eq!(row["name"], json!("flatuser"));
        assert_eq!(row["role"], json!("Student"));
    }

    #[test]
    fn user_role_maps_internal_single_letter_codes() {
        use serde_json::json;
        // single-letter internal role codes -> full public REST names
        assert_eq!(user_role(&json!({"role": "S"})), "Student");
        assert_eq!(user_role(&json!({"role": "T"})), "TeachingAssistant");
        assert_eq!(user_role(&json!({"role": "P"})), "PrimaryInstructor");
        assert_eq!(user_role(&json!({"role": "I"})), "Instructor");
        assert_eq!(user_role(&json!({"role": "G"})), "Grader");
        assert_eq!(user_role(&json!({"role": "O"})), "Observer");
        // courseRole.identifier is used as a fallback
        assert_eq!(
            user_role(&json!({"courseRole": {"identifier": "S"}})),
            "Student"
        );
        // full names pass through unchanged (public REST courseRoleId)
        assert_eq!(user_role(&json!({"courseRoleId": "Student"})), "Student");
        assert_eq!(
            user_role(&json!({"courseRoleId": "TeachingAssistant"})),
            "TeachingAssistant"
        );
        // unhandled longer values pass through
        assert_eq!(user_role(&json!({"role": "BB_SPECTATOR"})), "BB_SPECTATOR");
        // empty
        assert_eq!(user_role(&json!({})), "");
    }

    #[test]
    fn user_name_reads_internal_flat_given_family() {
        use serde_json::json;
        // internal v1 user object: flat givenName/familyName, no nested `name`
        assert_eq!(
            user_name(
                &json!({"id": "_7770000_1", "userName": "fennec42", "givenName": "Fennec", "familyName": "Twobyte"})
            ),
            "Fennec Twobyte"
        );
        // nested name object still wins
        assert_eq!(
            user_name(&json!({"userName": "ju", "name": {"given": "Alex", "family": "Tan"}})),
            "Alex Tan"
        );
        // string `name` falls back to userName then flat
        assert_eq!(user_name(&json!({"userName": "ju", "name": "plain"})), "ju");
    }

    #[test]
    fn toon_response_format_accepted_and_rendered() {
        let args = serde_json::json!({"response_format": "toon"});
        assert_eq!(response_format(&args).unwrap(), "toon");
        // Invalid still rejected.
        assert!(response_format(&serde_json::json!({"response_format": "yaml"})).is_err());
        // render_for_tool emits TOON (tabular header) instead of pretty JSON.
        let payload = serde_json::json!({
            "courses": [
                {"id": "_1_1", "name": "SC2002"},
                {"id": "_2_1", "name": "SC3000"}
            ]
        });
        let out = render_for_tool("list_courses", &args, &payload);
        assert_eq!(out.len(), 1);
        let ToolContent::Text { text } = &out[0];
        let text = text.clone();
        assert!(text.starts_with("courses[2]{id,name}:"), "got: {text}");
        assert!(!text.contains("  \""), "should not be pretty JSON: {text}");
    }
}

// ---------------------------------------------------------------------------
// Kaltura wiring tests (stub-backed: the pure arg/payload logic only —
// network paths are exercised by the e2e script after the core merges)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod kaltura_wiring_tests {
    use super::*;

    fn test_state() -> AppState {
        AppState {
            client: NTULearnClient::new_for_test("https://ntulearn.ntu.edu.sg/"),
            download_dir: std::env::temp_dir(),
            update_notice: std::sync::Arc::new(crate::update::UpdateNotice::default()),
        }
    }

    fn embed(content_id: &str, title: &str, entry_id: &str) -> KalturaEmbed {
        KalturaEmbed {
            content_id: content_id.to_string(),
            title: title.to_string(),
            entry_id: entry_id.to_string(),
            placement_id: "_1326_1".to_string(),
            embed_url: format!(
                "https://ntulearnv1.ntu.edu.sg/browseandembed/index/media/entryid/{entry_id}/"
            ),
        }
    }

    fn flavor(id: &str, w: u32, h: u32, size: u64, original: bool) -> FlavorAsset {
        FlavorAsset {
            id: id.to_string(),
            width: w,
            height: h,
            size_bytes: size,
            is_original: original,
            version: 12,
            file_ext: "mp4".to_string(),
        }
    }

    /// dispatch_value routes the three kaltura_* names to their handlers:
    /// each call reaches its handler's argument validation (a missing
    /// course_id is rejected there) instead of the "unknown tool" fallback —
    /// and no network is touched on that path.
    #[tokio::test]
    async fn dispatch_routes_the_three_kaltura_tools() {
        let state = test_state();
        for name in ["kaltura_list", "kaltura_transcripts", "kaltura_download"] {
            let err = dispatch_value(&state, name, &json!({}))
                .await
                .expect_err("missing course_id must be rejected");
            assert!(
                err.contains("course_id must be a valid Blackboard ID"),
                "{name}: {err}"
            );
        }
        let err = dispatch_value(&state, "kaltura_bogus", &json!({}))
            .await
            .expect_err("unknown tool must be rejected");
        assert!(err.contains("unknown tool"), "{err}");
    }

    #[test]
    fn kaltura_arg_validation() {
        // missing / empty course_id
        assert!(validate_bb_id("", "course_id").is_err());
        // response_format reuses the shared guard
        assert!(response_format(&json!({"response_format": "yaml"})).is_err());
        // quality enum
        assert!(resolve_quality(&json!({"quality": "hd"})).is_err());
        assert_eq!(resolve_quality(&json!({})).unwrap(), "smallest");
        assert_eq!(
            resolve_quality(&json!({"quality": "ORIGINAL"})).unwrap(),
            "original"
        );
        // transcript format enum
        assert!(resolve_transcript_format(&json!({"format": "srt"})).is_err());
        assert_eq!(resolve_transcript_format(&json!({})).unwrap(), "both");
        assert_eq!(
            resolve_transcript_format(&json!({"format": "VTT"})).unwrap(),
            "vtt"
        );
        assert_eq!(
            resolve_transcript_format(&json!({"format": "txt"})).unwrap(),
            "txt"
        );
        // save_dir: non-string and empty are rejected, absent falls back to
        // the state download dir
        let state = test_state();
        assert!(resolve_save_dir(&json!({"save_dir": 3}), &state).is_err());
        assert!(resolve_save_dir(&json!({"save_dir": "  "}), &state).is_err());
        assert_eq!(
            resolve_save_dir(&json!({}), &state).unwrap(),
            state.download_dir
        );
    }

    /// A video without captions (or whose transcript fetch fails) maps to a
    /// per-video NOTE row — never a whole-call error. The no-captions
    /// wording is owned here so it survives the stub -> core swap.
    #[test]
    fn transcript_no_captions_is_a_note_not_an_error() {
        let note = transcript_outcome_as_note(
            "_5976141_1",
            "Topic 6 Part 1",
            "0_88lztt5a",
            &TranscriptError::NoCaptions,
        );
        assert_eq!(note["contentId"], json!("_5976141_1"));
        assert_eq!(note["title"], json!("Topic 6 Part 1"));
        assert_eq!(note["entryId"], json!("0_88lztt5a"));
        let text = note["note"].as_str().expect("note text");
        assert!(text.contains("no caption"), "note: {text}");

        // other failures pass the core module's message through verbatim
        let err = TranscriptError::Network("INVALID_KS: bad signature; run ntulearn_login".into());
        let note = transcript_outcome_as_note("_1_1", "Lecture", "0_x", &err);
        assert_eq!(note["note"], json!(err.message()));
    }

    #[test]
    fn auth_shaped_errors_are_detected_for_relaunch() {
        assert!(is_auth_shaped("INVALID_KS: signature did not match"));
        assert!(is_auth_shaped("HTTP 401 Unauthorized"));
        assert!(is_auth_shaped("ks expired"));
        assert!(is_auth_shaped("run ntulearn_login to refresh the session"));
        assert!(!is_auth_shaped("connection reset by peer"));
        assert!(!is_auth_shaped("malformed m3u8 playlist"));
    }

    /// kaltura_list / discovery output shape: one row per embed, ids intact,
    /// empty data-bbfile titles replaced (fallback at discovery, "untitled"
    /// as the final guard in the row builder).
    #[test]
    fn kaltura_list_output_shape() {
        let embeds = vec![
            embed("_5976141_1", "Topic 6: Energy Part 1", "0_88lztt5a"),
            embed("_5976142_1", "", "0_td7lgutt"),
        ];
        let rows = kaltura_video_rows(&embeds);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["contentId"], json!("_5976141_1"));
        assert_eq!(rows[0]["title"], json!("Topic 6: Energy Part 1"));
        assert_eq!(rows[0]["entryId"], json!("0_88lztt5a"));
        assert_eq!(rows[0]["placementId"], json!("_1326_1"));
        // a missing embed title falls back to "untitled" in the row
        assert_eq!(rows[1]["title"], json!("untitled"));

        // at discovery time the content item's own title fills the gap
        let filled = embeds_with_title_fallback(embeds, "Week 6 reading");
        assert_eq!(filled[1].title, "Week 6 reading");
        assert_eq!(filled[0].title, "Topic 6: Energy Part 1");
    }

    /// kaltura_download flavor selection: 'smallest' skips audio-only
    /// flavors (height 0), 'best' skips the original, 'original' needs
    /// isOriginal, and unsatisfiable requests fall back with a note.
    #[test]
    fn kaltura_download_picks_flavors() {
        // real flavor_assets_two.xml shape: 1080p original + 540p rendition
        let flavors = vec![
            flavor("0_mzsjnuj6", 1920, 1080, 1_696_369_677, true),
            flavor("0_8v2w0ar3", 960, 540, 123_694_567, false),
        ];
        let (f, note) = pick_flavor(&flavors, "smallest").expect("smallest");
        assert_eq!(f.id, "0_8v2w0ar3");
        assert!(note.is_none());

        let (f, _) = pick_flavor(&flavors, "best").expect("best");
        assert_eq!(f.id, "0_8v2w0ar3");

        let (f, _) = pick_flavor(&flavors, "original").expect("original");
        assert_eq!(f.id, "0_mzsjnuj6");

        // audio-only flavors (height 0) never win 'smallest'
        let with_audio = vec![
            flavor("0_audio", 0, 0, 3_000_000, false),
            flavor("0_540", 960, 540, 123_694_567, false),
        ];
        let (f, _) = pick_flavor(&with_audio, "smallest").expect("smallest");
        assert_eq!(f.id, "0_540");

        // no original flavor -> smallest fallback + explanatory note
        let no_original = vec![flavor("0_a", 640, 360, 30_000_000, false)];
        let (f, note) = pick_flavor(&no_original, "original").expect("fallback");
        assert_eq!(f.id, "0_a");
        let note = note.expect("fallback note");
        assert!(note.contains("no 'original' flavor"), "{note}");

        // nothing to choose from -> LLM-readable error
        assert!(pick_flavor(&[], "smallest").is_err());
    }

    /// markdown renderers: list rows, transcript rows + notes, download
    /// summary all render without the pretty JSON (and without panicking on
    /// sparse payloads).
    #[test]
    fn kaltura_markdown_renderers() {
        let list = json!({
            "courseId": "_2707070_1",
            "videoCount": 1,
            "videos": [ {
                "contentId": "_5976141_1",
                "title": "Topic 6 Part 1",
                "entryId": "0_88lztt5a",
                "placementId": "_1326_1",
            } ],
        });
        let md = render::md_kaltura_list(&list);
        assert!(md.contains("Kaltura videos — _2707070_1"), "{md}");
        assert!(md.contains("`0_88lztt5a`"), "{md}");

        let transcripts = json!({
            "courseId": "_2707070_1",
            "format": "both",
            "outputDir": "/tmp/dl/kaltura/_2707070_1",
            "videoCount": 1,
            "videos": [ {
                "contentId": "_5976141_1",
                "title": "Topic 6 Part 1",
                "entryId": "0_88lztt5a",
                "label": "Edited",
                "words": 3553,
                "segments": 4,
                "vttFile": "/tmp/dl/kaltura/_2707070_1/Topic 6 Part 1.vtt",
                "txtFile": "/tmp/dl/kaltura/_2707070_1/Topic 6 Part 1.txt",
            } ],
            "notes": [ {
                "contentId": "_5976142_1",
                "title": "Topic 6 Part 2",
                "entryId": "0_td7lgutt",
                "note": "no caption track — this video has no captions",
            } ],
        });
        let md = render::md_kaltura_transcripts(&transcripts);
        assert!(md.contains("3553 words"), "{md}");
        assert!(md.contains("no caption track"), "{md}");

        let download = json!({
            "courseId": "_2707070_1",
            "contentId": "_5976141_1",
            "entryId": "0_88lztt5a",
            "title": "Topic 6 Part 1",
            "quality": "smallest",
            "flavor": {
                "id": "0_8v2w0ar3", "width": 960, "height": 540,
                "sizeBytes": 123694567, "isOriginal": false, "version": 12, "fileExt": "mp4",
            },
            "bytes": 123694567,
            "path": "/tmp/dl/kaltura/_2707070_1/Topic 6 Part 1.mp4",
        });
        let md = render::md_kaltura_download(&download);
        assert!(md.contains("960×540"), "{md}");
        assert!(md.contains("quality: **smallest**"), "{md}");
    }
}
