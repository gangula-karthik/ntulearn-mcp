//! Tool dispatch: maps the 21 MCP tool names to typed handler functions.
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

use serde_json::{json, Map, Value};
use ultrafast_mcp::ToolContent;

use crate::client::{ClientError, NTULearnClient};
use crate::{parsers, render, AppState};

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

/// Route a tool name + JSON args to the right handler, then render.
pub async fn dispatch(
    state: &AppState,
    name: &str,
    args: &Value,
) -> Result<Vec<ToolContent>, String> {
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
        other => {
            return Err(format!("unknown tool: ntulearn_{other}"));
        }
    };
    Ok(render_for_tool(short, args, &out))
}

// ---------------------------------------------------------------------------
// Argument + value helpers
// ---------------------------------------------------------------------------

/// Resolve the optional `response_format` (default "json"). Invalid values are
/// rejected exactly like `common.resolve_response_format`.
pub fn response_format(args: &Value) -> Result<String, String> {
    let fmt = args
        .get("response_format")
        .map(|v| match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .unwrap_or_else(|| "json".to_string())
        .to_lowercase();
    if fmt != "json" && fmt != "markdown" {
        return Err("response_format must be 'json' or 'markdown'".to_string());
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
        return Err(format!("{name} must be a non-empty ISO-8601 timestamp string"));
    }
    let normalised = if value.ends_with('Z') {
        format!("{}Z", &value[..value.len() - 1])
    } else {
        value.to_string()
    };
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
        other => return Err(format!("pages must be a string like '1,3-5', got {other:?}")),
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
                return Err(format!("pages must be a set of 1-based page numbers, got {spec:?}"));
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
        let time = chrono::NaiveTime::from_hms_opt(
            minute_of_day / 60,
            minute_of_day % 60,
            0,
        )
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
    let v = c.get_course_contents(course_id, None).await.map_err(err_str)?;
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
async fn content_item(c: &NTULearnClient, course_id: &str, content_id: &str) -> Result<Value, String> {
    c.get_content_item(course_id, content_id, None).await.map_err(err_str)
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
    let folder_arg = if folder.is_empty() { None } else { Some(folder) };
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
    let v = c.get_message_participants(message_id, None).await.map_err(err_str)?;
    Ok(as_list(v))
}

async fn course_users(c: &NTULearnClient, course_id: &str) -> Result<Vec<Value>, String> {
    let v = c.get_course_users(course_id, None).await.map_err(err_str)?;
    Ok(as_list(v))
}

async fn course_groups(c: &NTULearnClient, course_id: &str) -> Result<Vec<Value>, String> {
    let v = c.get_course_groups(course_id, None).await.map_err(err_str)?;
    Ok(as_list(v))
}

async fn group_users(c: &NTULearnClient, course_id: &str, group_id: &str) -> Result<Vec<Value>, String> {
    let v = c.get_group_users(course_id, group_id, None).await.map_err(err_str)?;
    Ok(as_list(v))
}

async fn gradebook_columns(c: &NTULearnClient, course_id: &str) -> Result<Vec<Value>, String> {
    let v = c.get_gradebook_columns(course_id, None).await.map_err(err_str)?;
    Ok(as_list(v))
}

async fn user_grades(c: &NTULearnClient, course_id: &str, user_id: &str) -> Result<Vec<Value>, String> {
    let v = c.get_user_grades(course_id, user_id, None).await.map_err(err_str)?;
    Ok(as_list(v))
}

async fn gradebook_attempts(c: &NTULearnClient, course_id: &str, column_id: &str) -> Result<Vec<Value>, String> {
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
    let v = c.get_announcements(course_id, None).await.map_err(err_str)?;
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

/// fan_out_course_ids — resolve `course_ids` arg (None/empty -> enrolled+available).
async fn fan_out_course_ids(c: &NTULearnClient, course_ids_arg: Option<&Value>) -> Result<Vec<String>, String> {
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
async fn resolve_enrolled_course_ids(c: &NTULearnClient, include_disabled: bool) -> Result<Vec<String>, String> {
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
        .or_else(|| item.get("name").and_then(|v| v.as_str()).filter(|s| !s.is_empty()))
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
    user.get("userName").and_then(|v| v.as_str()).unwrap_or("").to_string()
}

fn user_role(user: &Value) -> String {
    user.get("courseRoleId")
        .and_then(|v| v.as_str())
        .or_else(|| user.get("role").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string()
}

/// server._strip_content — reduce a raw content item.
fn strip_content(item: &Value) -> Value {
    let handler = item.get("contentHandler");
    let description_raw = item.get("description");
    let description = match description_raw {
        Some(Value::Object(o)) => o
            .get("rawText")
            .cloned()
            .unwrap_or(Value::Null),
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
}

impl ContentNode {
    fn id(&self) -> String {
        self.item.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string()
    }
    fn title(&self) -> String {
        item_title(&self.item)
    }
}

type WalkBox<'b> = std::pin::Pin<
    Box<
        dyn std::future::Future<
                    Output = (HashSet<String>, Vec<ContentNode>),
                > + Send + 'b,
    >,
>;

/// walk_rec — recursive pre-order walk; owned-state threading avoids the
/// infinitely-sized future error without changing the DFS order or payload.
fn walk_rec<'b>(
    c: &'b NTULearnClient,
    course_id: &'b str,
    max_depth: usize,
    content_id: Option<String>,
    depth: usize,
    breadcrumb: Vec<String>,
    seen: HashSet<String>,
    nodes: Vec<ContentNode>,
) -> WalkBox<'b> {
    Box::pin(async move {
        if depth > max_depth {
            return (seen, nodes);
        }
        let children = match content_id {
            None => course_contents(c, course_id).await,
            Some(ref cid) => content_children(c, course_id, cid).await,
        };
        let Ok(children) = children else { return (seen, nodes) };
        if children.is_empty() {
            return (seen, nodes);
        }
        let mut seen = seen;
        let mut nodes = nodes;
        for raw in children {
            let item = if raw.is_null() { json!({}) } else { raw };
            let Some(cid) = item.get("id").and_then(|v| v.as_str()).map(|s| s.to_string()) else {
                continue;
            };
            if cid.is_empty() || seen.contains(&cid) {
                continue;
            }
            seen.insert(cid.clone());
            let title = item_title(&item);
            let mut crumb = breadcrumb.clone();
            crumb.push(title);
            nodes.push(ContentNode {
                item: item.clone(),
                breadcrumb: crumb.clone(),
                depth,
            });
            if is_folder(&item) {
                let (s2, n2) = walk_rec(
                    c,
                    course_id,
                    max_depth,
                    Some(cid),
                    depth + 1,
                    crumb,
                    seen,
                    nodes,
                )
                .await;
                seen = s2;
                nodes = n2;
            }
        }
        (seen, nodes)
    })
}

/// walk_content — every reachable node in a course (bounded, cycle-guarded).
async fn walk_content(c: &NTULearnClient, course_id: &str, max_depth: usize) -> Vec<ContentNode> {
    let (_seen, nodes) = walk_rec(
        c,
        course_id,
        max_depth,
        None,
        0,
        Vec::new(),
        HashSet::new(),
        Vec::new(),
    )
    .await;
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
            search_add(&mut matches, &mut seen, &lowered, course_id, &item, vec![item_title(&item)]);
        }
        if !matches.is_empty() {
            return matches;
        }
    }

    for node in walk_content(c, course_id, max_depth).await {
        search_add(&mut matches, &mut seen, &lowered, course_id, &node.item, node.breadcrumb);
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
async fn collect_download_jobs(c: &NTULearnClient, course_id: &str, max_depth: usize) -> Vec<DownloadJob> {
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
    for node in walk_content(c, course_id, max_depth).await {
        if !is_file_item(&node.item) {
            continue;
        }
        let item = &node.item;
        let Some(content_id) = item.get("id").and_then(|v| v.as_str()).map(|s| s.to_string()) else {
            continue;
        };
        let atts = attachments(c, course_id, &content_id).await;
        for att in &atts {
            let Some(att_id) = att.get("id").and_then(|v| v.as_str()).map(|s| s.to_string()) else {
                continue;
            };
            let url = match attachment_download_url(c, course_id, &content_id, &att_id).await {
                Ok(u) => u,
                Err(_) => continue,
            };
            let raw_name = att
                .get("fileName")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| item_title(item));
            let raw_name = if raw_name.is_empty() { "file".to_string() } else { raw_name };
            jobs.push(DownloadJob {
                course_id: course_id.to_string(),
                course_folder: course_folder.clone(),
                content_title: item_title(item),
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
    let Some(raw) = raw else { return None };
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
    if set.is_empty() {
        None
    } else {
        Some(set)
    }
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
    let _ = std::fs::create_dir_all(&folder);
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
    if std::fs::write(&local_path, &content).is_err() {
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
        .or_else(|| col.get("displayName").and_then(|v| v.as_str()).filter(|s| !s.is_empty()))
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
        Some(Value::Object(o)) => o
            .get("score")
            .or_else(|| o.get("value"))
            .cloned(),
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
    let mut graded = 0usize;
    let mut total_possible = 0.0f64;
    let mut earned = 0.0f64;
    for col in &columns {
        let Some(col_id) = col.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let possible = column_possible(col);
        total_possible += possible.unwrap_or(0.0);
        if let Some(uid) = user_id {
            let grades = user_grades(c, course_id, uid).await.unwrap_or_default();
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
                continue;
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
                    instructors.push(json!({
                        "id": u.get("id").cloned().unwrap_or(Value::Null),
                        "name": user_name(u),
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
            chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S%.f")
                .map(|nd| chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(nd, chrono::Utc).fixed_offset())
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
        let title = ics_escape(Some(&json!(item.get("title").and_then(|v| v.as_str()).unwrap_or("Untitled event"))));
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
            Some(v)
                if !(v.is_null()
                    || v.as_str().map(|s| s.is_empty()).unwrap_or(false)) =>
            {
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

fn rpartition<'a>(s: &'a str, sep: char) -> Option<(&'a str, &'a str)> {
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
        .filter_map(|e| e.get("courseId").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .collect();

    if course_ids.is_empty() {
        let (_, meta) = slice_with_pagination(&[], offset, limit);
        return Ok(json!({"courses": [], "total": meta["total"], "count": meta["count"],
            "offset": meta["offset"], "limit": meta["limit"], "hasMore": meta["hasMore"],
            "nextOffset": meta["nextOffset"]}));
    }

    let mut last_accessed_map: HashMap<String, Value> = HashMap::new();
    let mut availability_map: HashMap<String, String> = HashMap::new();
    for e in &enrollments {
        if let Some(cid) = e.get("courseId").and_then(|v| v.as_str()) {
            last_accessed_map.insert(cid.to_string(), e.get("lastAccessed").cloned().unwrap_or(Value::Null));
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
    let stripped: Vec<Value> = items.iter().map(|it| strip_content(it)).collect();
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
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = (Vec<Value>, HashSet<String>)> + Send + 'b>> {
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
                    Some(Value::Object(o)) => o.get("rawText").and_then(|v| v.as_str()).unwrap_or(""),
                    Some(Value::String(s)) => s.as_str(),
                    _ => "",
                }
                .to_string();
                let mut current_path = path.clone();
                current_path.push(title.to_string());
                if query.contains(title.to_lowercase().as_str()) || query.contains(desc.to_lowercase().as_str()) {
                    let mut stripped = strip_content(item);
                    if let Some(s) = stripped.as_object_mut() {
                        s.insert("breadcrumb".to_string(), Value::Array(current_path.iter().map(|x| Value::String(x.clone())).collect()));
                    }
                    matches.push(stripped);
                }
                let has_children = item.get("hasChildren").and_then(|v| v.as_bool()).unwrap_or(false);
                if has_children && (matches.len() as i64) < max_results && !item_id.is_empty() {
                    match content_children(c, course_id, item_id).await {
                        Ok(children) => {
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
                        Err(_) => {}
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
    let item = content_item(c, course_id, content_id).await.unwrap_or_else(|_| json!({}));
    let handler_id = item
        .get("contentHandler")
        .and_then(|h| h.get("id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let mut pairs: Vec<(String, Option<String>)> = Vec::new();
    if handler_id.as_deref() == Some("resource/x-bb-file") {
        for att in attachments(c, course_id, content_id).await {
            let Some(att_id) = att.get("id").and_then(|v| v.as_str()) else { continue };
            let url = attachment_download_url(c, course_id, content_id, att_id)
                .await
                .unwrap_or_default();
            if !url.is_empty() {
                let fname = att.get("fileName").and_then(|v| v.as_str()).map(|s| s.to_string());
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
                Some(Value::Object(o)) => o.get("rawText").and_then(|v| v.as_str()).map(|s| s.to_string()),
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

    if let Err(e) = std::fs::create_dir_all(&dest_dir) {
        return Err(format!("{e}"));
    }

    let mut used_names: HashSet<String> = HashSet::new();
    let mut saved: Vec<Value> = Vec::new();
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

        let dest = dest_dir.join(&filename);
        let (content, _ct) = match state.client.download_bytes(url).await {
            Ok(t) => t,
            Err(e) => return Err(format!("download failed: {e}")),
        };
        if let Err(e) = std::fs::write(&dest, &content) {
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

    for (url, detected_filename) in &pairs {
        let mut filename = detected_filename.clone().unwrap_or_default();
        if filename.is_empty() {
            let url_path = url.split('?').next().unwrap_or("");
            let last = url_path.rsplit('/').next().unwrap_or("");
            let decoded = percent_encoding::percent_decode_str(last)
                .decode_utf8_lossy()
                .to_string();
            filename = if decoded.is_empty() { "download".to_string() } else { decoded };
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

        let (content_bytes, ct_raw) = match state.client.download_bytes(url).await {
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
        Some(v) => Some(validate_iso8601(&v.to_string(), "since")?),
        None => None,
    };
    let until = match until_val {
        Some(v) => Some(validate_iso8601(&v.to_string(), "until")?),
        None => None,
    };
    let item_type = args.get("type").map(|v| v.to_string());
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
    for cid in &course_ids {
        match calendar_items(&state.client, Some(cid), since.as_deref(), until.as_deref(), item_type.as_deref()).await {
            Ok(rows) => {
                for raw in rows {
                    items.push(strip_calendar_item(&raw, Some(cid)));
                }
            }
            Err(e) => {
                course_errors.insert(cid.clone(), Value::String(e));
            }
        }
    }
    items.sort_by(|a, b| {
        let sa = a.get("start").and_then(|v| v.as_str()).unwrap_or("\u{FFFF}");
        let sb = b.get("start").and_then(|v| v.as_str()).unwrap_or("\u{FFFF}");
        sa.cmp(sb)
    });

    let (page, meta) = slice_with_pagination(&items, offset, limit);
    let mut payload = Map::new();
    payload.insert("items".to_string(), Value::Array(page));
    merge_meta(&mut payload, meta);
    payload.insert(
        "courseIdsQueried".to_string(),
        Value::Array(course_ids.iter().map(|x| Value::String(x.clone())).collect()),
    );
    payload.insert("courseErrors".to_string(), Value::Object(course_errors));
    Ok(Value::Object(payload))
}

async fn h_get_announcements(state: &AppState, args: &Value) -> Result<Value, String> {
    let since_val = args.get("since");
    let since = match since_val {
        Some(v) => Some(validate_iso8601(&v.to_string(), "since")?),
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
    for cid in &course_ids {
        match announcements(&state.client, cid).await {
            Ok(anns) => {
                for a in anns {
                    let body_raw = a.get("body");
                    let body_html = match body_raw {
                        Some(Value::Object(o)) => o.get("rawText").and_then(|v| v.as_str()).unwrap_or(""),
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
        Value::Array(course_ids.iter().map(|x| Value::String(x.clone())).collect()),
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
        payload.insert("gradeFetchError".to_string(), grade_fetch_error.map(Value::String).unwrap_or(Value::Null));
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
    for cid in &course_ids {
        let columns = match gradebook_columns(&state.client, cid).await {
            Ok(cols) => cols,
            Err(e) => {
                course_errors.insert(cid.clone(), Value::String(e));
                continue;
            }
        };
        let mut per_course_grade_error: Option<String> = None;
        let grades_raw = if let Some(uid) = &user_id {
            match user_grades(&state.client, cid, uid).await {
                Ok(gs) => gs,
                Err(e) => {
                    per_course_grade_error = Some(e);
                    Vec::new()
                }
            }
        } else {
            Vec::new()
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
    payload.insert("gradeFetchError".to_string(), grade_fetch_error.map(Value::String).unwrap_or(Value::Null));
    payload.insert(
        "courseIdsQueried".to_string(),
        Value::Array(course_ids.iter().map(|x| Value::String(x.clone())).collect()),
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
    let since = args.get("since").map(|v| v.to_string());
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
    let subject = msg.get("subject").and_then(|v| v.as_str()).unwrap_or("").to_string();
    payload.insert("subject".to_string(), Value::String(subject));
    payload.insert("body".to_string(), Value::String(parsers::strip_html(&body_text)));
    let created = msg.get("created").and_then(|v| v.as_str()).unwrap_or("").to_string();
    payload.insert("created".to_string(), Value::String(created));
    let read = msg.get("read").and_then(|v| v.as_bool()).unwrap_or(true);
    payload.insert("read".to_string(), Value::Bool(read));
    let folder = msg.get("folder").and_then(|v| v.as_str()).unwrap_or("").to_string();
    payload.insert("folder".to_string(), Value::String(folder));
    let sender_id = msg.get("fromUserId").and_then(|v| v.as_str()).unwrap_or("").to_string();
    payload.insert("senderId".to_string(), Value::String(sender_id.clone()));
    match message_participants(&state.client, &message_id).await {
        Ok(participants) => {
            let recipients: Vec<Value> = participants
                .iter()
                .map(|p| json!({
                    "id": p.get("id"),
                    "name": user_name(p),
                    "role": user_role(p),
                }))
                .collect();
            payload.insert("recipients".to_string(), Value::Array(recipients));
            let sender = participants
                .iter()
                .find(|p| p.get("id").and_then(|v| v.as_str()) == Some(sender_id.as_str()));
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
    let rendered: Vec<Value> = page
        .iter()
        .map(|u| json!({
            "id": u.get("id"),
            "userName": u.get("userName").and_then(|v| v.as_str()).unwrap_or(""),
            "name": user_name(u),
            "role": user_role(u),
        }))
        .collect();
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
        entry.insert("id".to_string(), g.get("id").cloned().unwrap_or(Value::Null));
        let name = g
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| g.get("title").and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string();
        entry.insert("name".to_string(), Value::String(name));
        let desc = g.get("description").and_then(|v| v.as_str()).unwrap_or("");
        entry.insert("description".to_string(), Value::String(parsers::strip_html(desc)));
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
    let rendered: Vec<Value> = page
        .iter()
        .map(|u| json!({
            "id": u.get("id"),
            "userName": u.get("userName").and_then(|v| v.as_str()).unwrap_or(""),
            "name": user_name(u),
            "role": user_role(u),
        }))
        .collect();
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
    let user_id = args.get("user_id").map(|v| v.to_string());
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
    let query = args.get("query").map(|v| v.to_string()).unwrap_or_default().trim().to_string();
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
    for cid in &course_ids {
        let found = search_course(&state.client, cid, &query, max_depth as usize).await;
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

    // Group nodes by depth for O(n) child lookups.
    let mut by_depth: HashMap<usize, Vec<&ContentNode>> = HashMap::new();
    for n in &nodes {
        by_depth.entry(n.depth).or_default().push(n);
    }

    fn to_node<'a>(
        node: &'a ContentNode,
        by_depth: &HashMap<usize, Vec<&'a ContentNode>>,
    ) -> Value {
        let children: Vec<Value> = by_depth
            .get(&(node.depth + 1))
            .map(|candidates| {
                candidates
                    .iter()
                    .filter(|c| c.breadcrumb.len() > 1 && &c.breadcrumb[..c.breadcrumb.len() - 1] == node.breadcrumb.as_slice())
                    .take(100)
                    .map(|c| to_node(c, by_depth))
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

    let roots = by_depth.get(&0).cloned().unwrap_or_default();
    let tree: Vec<Value> = roots.iter().map(|r| to_node(r, &by_depth)).collect();
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
    let dest_raw = args.get("destination_dir").map(|v| v.to_string()).unwrap_or_default().trim().to_string();
    let crs = course(&state.client, &course_id).await.unwrap_or_else(|_| json!({}));
    let course_name = parsers::safe_folder_name(
        crs.get("name").and_then(|v| v.as_str()).filter(|s| !s.is_empty())
            .or_else(|| crs.get("displayName").and_then(|v| v.as_str()))
            .unwrap_or(&course_id),
    );
    let dest_root: PathBuf = if dest_raw.is_empty() {
        let d = if let Some(home) = dirs::home_dir() {
            home.join("Downloads").join("NTULearn")
        } else {
            PathBuf::from("./Downloads/NTULearn")
        };
        d
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
        let ka = a.get("courseFolder").and_then(|v| v.as_str()).unwrap_or("").to_string()
            + a.get("filename").and_then(|v| v.as_str()).unwrap_or("");
        let kb = b.get("courseFolder").and_then(|v| v.as_str()).unwrap_or("").to_string()
            + b.get("filename").and_then(|v| v.as_str()).unwrap_or("");
        ka.cmp(&kb)
    });
    let total_bytes: u64 = results.iter().map(|r| r.get("sizeBytes").and_then(|v| v.as_u64()).unwrap_or(0)).sum();
    let mut payload = Map::new();
    payload.insert("courseId".to_string(), Value::String(course_id));
    payload.insert("courseName".to_string(), Value::String(course_name));
    payload.insert("destinationDir".to_string(), Value::String(dest_root.to_string_lossy().to_string()));
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
    let course_ids = fan_out_course_ids(&state.client, args.get("course_ids")).await?;
    let since = match args.get("since") {
        Some(v) => {
            let s = v.to_string();
            validate_iso8601(&s, "since")?;
            s
        }
        None => tracker_get_last_seen().unwrap_or_else(default_since),
    };
    let update_tracker = bool_arg(args, "update_tracker", false);
    let _fmt = response_format(args)?;

    let mut per_course: Vec<Value> = Vec::new();
    let mut errors: Map<String, Value> = Map::new();
    for cid in &course_ids {
        let mut entry = Map::new();
        entry.insert("courseId".to_string(), Value::String(cid.clone()));
        // course name
        match course(&state.client, cid).await {
            Ok(crs) => {
                let name = crs
                    .get("name")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .or_else(|| crs.get("displayName").and_then(|v| v.as_str()))
                    .unwrap_or(cid);
                entry.insert("courseName".to_string(), Value::String(name.to_string()));
            }
            Err(e) => {
                errors.insert(cid.clone(), Value::String(format!("course: {e}")));
            }
        }
        // announcements
        match announcements(&state.client, cid).await {
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
                if !errors.contains_key(cid) {
                    errors.insert(cid.clone(), Value::String(format!("announcements: {e}")));
                }
            }
        }
        // calendar
        match calendar_items(&state.client, Some(cid), Some(&since), None, None).await {
            Ok(cal) => {
                let mut list = Vec::new();
                for i in cal {
                    if after_value(i.get("start"), &since) {
                        list.push(calendar_brief(&i));
                    }
                }
                entry.insert("upcoming".to_string(), Value::Array(list));
            }
            Err(e) => {
                if !errors.contains_key(cid) {
                    errors.insert(cid.clone(), Value::String(format!("calendar: {e}")));
                }
            }
        }
        // new files (top-level only)
        match course_contents(&state.client, cid).await {
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
                if !errors.contains_key(cid) {
                    errors.insert(cid.clone(), Value::String(format!("contents: {e}")));
                }
            }
        }
        per_course.push(Value::Object(entry));
    }

    let fetched_at = now_iso();
    if update_tracker {
        tracker_set_last_seen(fetched_at.clone());
    }
    let count_ann: usize = per_course.iter().map(|e| e.get("announcements").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0)).sum();
    let count_up: usize = per_course.iter().map(|e| e.get("upcoming").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0)).sum();
    let count_files: usize = per_course.iter().map(|e| e.get("newFiles").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0)).sum();
    let mut payload = Map::new();
    payload.insert("since".to_string(), Value::String(since));
    payload.insert("fetchedAt".to_string(), Value::String(fetched_at));
    payload.insert("courseCount".to_string(), Value::from(per_course.len()));
    payload.insert("summary".to_string(), json!({
        "announcements": count_ann,
        "upcoming": count_up,
        "newFiles": count_files,
    }));
    payload.insert("courses".to_string(), Value::Array(per_course));
    payload.insert("courseErrors".to_string(), Value::Object(errors));
    Ok(Value::Object(payload))
}


async fn h_export_calendar_ics(state: &AppState, args: &Value) -> Result<Value, String> {
    let course_ids = fan_out_course_ids(&state.client, args.get("course_ids")).await?;
    let since = match args.get("since") {
        Some(v) => v.to_string(),
        None => iso_from_now(0, 0),
    };
    let until = match args.get("until") {
        Some(v) => v.to_string(),
        None => iso_from_now(30, 0),
    };
    validate_iso8601(&since, "since")?;
    validate_iso8601(&until, "until")?;
    let _fmt = response_format(args)?;

    let mut items: Vec<Value> = Vec::new();
    let mut errors: Map<String, Value> = Map::new();
    for cid in &course_ids {
        match calendar_items(&state.client, Some(cid), Some(&since), Some(&until), None).await {
            Ok(cal) => {
                for mut i in cal {
                    let item_id = i.get("id").and_then(|v| v.as_str()).unwrap_or("x").to_string();
                    if let Some(o) = i.as_object_mut() {
                        o.insert("courseId".to_string(), Value::String(cid.clone()));
                        o.insert("uid".to_string(), Value::String(format!("{}-{}", cid, item_id)));
                    }
                    items.push(i);
                }
            }
            Err(e) => {
                errors.insert(cid.clone(), Value::String(e));
            }
        }
    }
    let scope = course_ids.iter().take(5).cloned().collect::<Vec<_>>().join("; ");
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
    let user_id = match me_user_id(&state.client).await {
        Ok(uid) => uid,
        Err(_) => None,
    };
    let mut rows: Vec<Value> = Vec::new();
    let mut errors: Map<String, Value> = Map::new();
    for cid in &course_ids {
        match gradebook_columns(&state.client, cid).await {
            Ok(columns) => {
                let grades = match &user_id {
                    Some(uid) => user_grades(&state.client, cid, uid).await.unwrap_or_default(),
                    None => Vec::new(),
                };
                for col in &columns {
                    let col_id = col.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    let own = grades.iter().find(|g| {
                        g.get("columnId").and_then(|v| v.as_str()) == Some(col_id)
                    });
                    let score = own.map(|o| grade_score(o)).flatten();
                    let status = own.map(|o| o.get("status").and_then(|v| v.as_str()).unwrap_or("").to_string()).unwrap_or_default();
                    rows.push(json!({
                        "courseId": cid,
                        "columnId": col_id,
                        "columnName": column_name(col),
                        "possible": column_possible(col).map(|p| Value::from(p)).unwrap_or(Value::Null),
                        "score": score.map(|s| Value::from(s)).unwrap_or(Value::Null),
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
                let courses = payload.get("courses").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                render::md_courses_full(&courses, payload)
            }
            "get_course_contents" => {
                let items = payload.get("items").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                render::md_content_items_full(&items, payload)
            }
            "search_course_content" => {
                let matches = payload.get("matches").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                render::md_search_results(&matches)
            }
            "download_file" => render::md_files(payload, "Files downloaded"),
            "read_file_content" => render::md_files(payload, "File contents"),
            "get_upcoming" => {
                let items = payload.get("items").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                let errors = payload.get("courseErrors").cloned().unwrap_or(json!({}));
                render::md_upcoming(&items, payload, &errors)
            }
            "get_announcements" => {
                let items = payload.get("announcements").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                render::md_announcements(&items, payload)
            }
            "get_gradebook" => {
                let columns = payload.get("columns").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                let available = payload.get("gradesAvailable").and_then(|v| v.as_bool()).unwrap_or(true);
                let err = payload.get("gradeFetchError").and_then(|v| v.as_str()).map(|s| s.to_string());
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
            _ => String::new(),
        }
    } else {
        String::new()
    };
    let mut out: Vec<ToolContent> = Vec::new();
    if !text.is_empty() {
        out.push(ToolContent::text(text));
    }
    let json_text = serde_json::to_string_pretty(payload).unwrap_or_else(|_| "{}".to_string());
    out.push(ToolContent::text(json_text));
    out
}
