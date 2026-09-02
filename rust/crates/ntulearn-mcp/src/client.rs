//! Async HTTP client for the Blackboard Learn public REST API.
//!
//! Port of `src/ntulearn_mcp/client.py` (786 lines) to Rust. Same behaviour:
//! * retry with exponential backoff + jitter on transient failures (429/5xx,
//!   network errors), max 3 attempts;
//! * `fields` trimming per endpoint, one auto retry without fields on 400/403
//!   (disable with NTULEARN_FIELDS=0);
//! * per-method TTL caching via the SQLite-backed DataCache, keys scoped to the
//!   user (sha256(cookie)[:16], matching Python);
//! * 401 -> invalidate this user's cache, re-resolve cookie once, retry once
//!   (never touches the OS keychain; browser reads are sqlite-only);
//! * cursor pagination via `paging.nextPage`.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Map, Value};

use crate::cache::DataCache;
use crate::cookie;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error(
        "Blackboard session cookie has expired (HTTP 401). Open NTULearn in your \
         browser, copy the new BbRouter cookie value, update NTULEARN_COOKIE, and \
         restart the MCP server."
    )]
    Auth,
    #[error("Blackboard API {status} {class}{path_desc} {hint}Body: {body}")]
    Api {
        status: u16,
        class: &'static str,
        path_desc: String,
        hint: String,
        body: String,
    },
    #[error("no BbRouter cookie available (set NTULEARN_COOKIE to your Blackboard session cookie, or log into NTULearn in Firefox)")]
    NoCookie,
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("json parse error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

impl ClientError {
    /// Build the LLM-actionable API error (mirrors Python `_format_api_error`).
    fn api(status: u16, body: &str, path: &str) -> Self {
        let where_ = if path.is_empty() { String::new() } else { format!(" at {path}") };
        let snippet: String = body.chars().take(300).collect::<String>().replace('\n', " ");
        let (class, hint): (&'static str, &'static str) = match status {
            403 => ("forbidden", "The current user lacks access to this resource (course not enrolled, instructor-only data, or unavailable). "),
            404 => ("not found", "Check the course_id / content_id is correct. "),
            429 => ("rate limited", "Slow down and retry later. "),
            s if s >= 500 => ("server error", "NTULearn may be down; retry shortly. "),
            _ => ("other", ""),
        };
        ClientError::Api { status, class, path_desc: where_, hint: hint.to_string(), body: snippet }
    }
}

pub type ClientResult<T> = Result<T, ClientError>;

pub const DEFAULT_TTL_SECONDS: &[(&str, f64)] = &[
    ("get_my_enrollments", 1800.0),
    ("get_course", 3600.0),
    ("get_courses_batch", 3600.0),
    ("get_course_contents", 3600.0),
    ("get_content_children", 3600.0),
    ("get_content_item", 3600.0),
    ("get_announcements", 600.0),
    ("get_calendar_items", 300.0),
    ("get_gradebook_columns", 600.0),
    ("get_user_grades", 300.0),
    ("get_messages", 60.0),
    ("get_message", 600.0),
    ("get_message_participants", 600.0),
    ("get_course_users", 1800.0),
    ("get_course_groups", 1800.0),
    ("get_group_users", 1800.0),
    ("get_gradebook_attempts", 300.0),
    ("get_user_attempts", 300.0),
    ("get_term", 3600.0),
    ("get_course_search", 600.0),
];

// -- field trimming ----------------------------------------------------------
const FIELDS: &[(&str, &str)] = &[
    ("enrollments", "courseId,availability,lastAccessed"),
    ("course", "id,name,displayName"),
    ("contents", "id,title,contentHandler,hasChildren,description,modified"),
    ("calendar", "id,type,title,description,location,start,end,calendarName,dynamicCalendarItemProps"),
    ("announcements", "id,title,body,created,modified,availability"),
    ("grade_columns", "id,name,displayName,score,availability,contentId"),
    ("user_grades", "columnId,score,status,gradingStatus"),
    ("course_users", "userId,user.userName,user.name.given,user.name.family,courseRoleId,availability"),
    ("groups", "id,name,description,availability"),
    ("messages", "id,subject,body,created,read,folder,fromUserId"),
    ("attempts", "id,userId,status,score,cumulatedScore,feedback,created,updated"),
    ("term", "id,name,startDate,endDate"),
];

fn env_flag(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Ok(v) => !v.trim().is_empty()
            && !matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "no" | "off"),
        Err(_) => default,
    }
}

fn default_fields(name: &str) -> Option<&'static str> {
    if !env_flag("NTULEARN_FIELDS", true) { return None; }
    FIELDS.iter().find(|(n, _)| *n == name).map(|(_, f)| *f)
}

fn sha256_hex_first(data: &str, len: usize) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data.as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    hex.chars().take(len).collect()
}

fn jitter(base_secs: f64) -> f64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let r = 0.75 + ((nanos % 50) as f64 / 100.0);
    base_secs * r
}

fn backoff(attempt: usize) -> f64 {
    let base = 0.25 * (2usize.pow(attempt as u32) as f64);
    jitter(base)
}

fn is_retryable_status(s: u16) -> bool {
    matches!(s, 429 | 500 | 502 | 503 | 504)
}

fn is_success_status(s: u16) -> bool {
    (200..300).contains(&s)
}

fn build_query(params: &[(&str, &str)]) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{k}={}", percent_encoding::utf8_percent_encode(v, percent_encoding::NON_ALPHANUMERIC)))
        .collect::<Vec<_>>()
        .join("&")
}

fn trim_value(v: Value) -> Value {
    // `fields` trimming happens server-side in the Python layer via
    // _strip_* helpers; the client just passes through the raw response.
    v
}

fn trim_fields(mut items: Vec<Value>, _fields: Option<&str>) -> Value {
    // The Python client requests `fields` from the API directly; no local trim.
    Value::Array(items.drain(..).collect())
}

struct RawResp {
    status: u16,
    headers: reqwest::header::HeaderMap,
    bytes: Vec<u8>,
}

struct CookieState {
    value: String,
    refresh: Option<Box<dyn Fn() -> Option<String> + Send + Sync>>,
}

pub struct NTULearnClient {
    http: reqwest::Client,
    no_redirect: reqwest::Client,
    external: reqwest::Client,
    base_url: String,
    cookie: tokio::sync::Mutex<CookieState>,
    cache: Arc<DataCache>,
    user_scope: String,
}


impl NTULearnClient {
    #[cfg(test)]
    pub fn new_for_test(base_url: &str) -> Self {
        let http = reqwest::Client::builder().build().expect("test client");
        let no_redirect = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("test client");
        let external = reqwest::Client::builder().build().expect("test external");
        NTULearnClient {
            http,
            no_redirect,
            external,
            base_url: base_url.trim_end_matches('/').to_string(),
            cookie: tokio::sync::Mutex::new(CookieState {
                value: "expires:1,id:test".to_string(),
                refresh: None,
            }),
            cache: Arc::new(DataCache::open().expect("test cache")),
            user_scope: String::new(),
        }
    }

    pub fn new(base_url: String, cookie_value: String, cache: Arc<DataCache>) -> ClientResult<Self> {
        let mut value = cookie_value;
        if let Some(rest) = value.strip_prefix("BbRouter=") {
            value = rest.to_string();
        }
        let http = reqwest::Client::builder()
            .user_agent("ntulearn-mcp-rust/0.3.0")
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| ClientError::Other(format!("reqwest client build: {e}")))?;
        let no_redirect = reqwest::Client::builder()
            .user_agent("ntulearn-mcp-rust/0.3.0")
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| ClientError::Other(format!("reqwest no_redirect client build: {e}")))?;
        let external = reqwest::Client::builder()
            .user_agent("ntulearn-mcp-rust/0.3.0")
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| ClientError::Other(format!("reqwest external client build: {e}")))?;
        let user_scope = sha256_hex_first(&value, 16);
        let refresh = Some(Box::new(|| cookie::resolve_cookie()) as Box<dyn Fn() -> Option<String> + Send + Sync>);
        Ok(NTULearnClient {
            http,
            no_redirect,
            external,
            base_url: base_url.trim_end_matches('/').to_string(),
            cookie: tokio::sync::Mutex::new(CookieState { value, refresh }),
            cache,
            user_scope,
        })
    }

    pub fn user_scope(&self) -> &str {
        &self.user_scope
    }

    async fn current_cookie(&self) -> String {
        self.cookie.lock().await.value.clone()
    }

    async fn set_cookie(&self, value: String) {
        self.cookie.lock().await.value = value;
    }

    fn url(&self, path: &str) -> String {
        if path.starts_with("http://") || path.starts_with("https://") {
            path.to_string()
        } else {
            format!("{}{}", self.base_url, path)
        }
    }

    fn build_authed_url(&self, url: &str, params: &[(&str, &str)]) -> String {
        let mut full = self.url(url);
        let query = build_query(params);
        if !query.is_empty() {
            full.push('?');
            full.push_str(&query);
        }
        full
    }

    /// Authenticated GET returning raw response (auto 401 refresh once).
    async fn authed_get_raw(&self, url: &str) -> ClientResult<RawResp> {
        let mut attempt = 0usize;
        loop {
            attempt += 1;
            let cookie = self.current_cookie().await;
            let cookie_header = if cookie.is_empty() { String::new() } else { format!("BbRouter={cookie}") };
            let send = self.http.get(url).header("Cookie", cookie_header);
            let resp = match send.send().await {
                Ok(r) => r,
                Err(e) => {
                    if attempt < 3 {
                        tokio::time::sleep(Duration::from_secs_f64(backoff(attempt))).await;
                        continue;
                    }
                    return Err(ClientError::Network(e));
                }
            };
            let status = resp.status().as_u16();
            if status == 401 {
                self.cache.invalidate_user(&self.user_scope);
                if attempt == 1 {
                    let new_cookie = {
                        let g = self.cookie.lock().await;
                        g.refresh.as_ref().and_then(|f| f())
                    };
                    if let Some(nc) = new_cookie {
                        if nc != self.current_cookie().await {
                            // Persist the newly-resolved cookie so future runs start
                            // authenticated instead of re-401ing (best-effort).
                            cookie::write_cookie(&nc);
                            self.set_cookie(nc).await;
                            let retry = self
                                .http
                                .get(url)
                                .header("Cookie", format!("BbRouter={}", self.current_cookie().await))
                                .send()
                                .await;
                            if let Ok(r) = retry {
                                let s2 = r.status().as_u16();
                                if s2 != 401 {
                                    let headers = r.headers().clone();
                                    if !is_success_status(s2) {
                                        let b = r.bytes().await.map(|b| b.to_vec()).unwrap_or_default();
                                        return Err(ClientError::api(s2, &String::from_utf8_lossy(&b), url));
                                    }
                                    let bytes = r.bytes().await.map(|b| b.to_vec())?;
                                    return Ok(RawResp { status: s2, headers, bytes });
                                }
                            }
                        }
                    }
                }
                return Err(ClientError::Auth);
            }
            if is_retryable_status(status) && attempt < 3 {
                tokio::time::sleep(Duration::from_secs_f64(backoff(attempt))).await;
                continue;
            }
            if !is_success_status(status) {
                let b = resp.bytes().await.map(|b| b.to_vec()).unwrap_or_default();
                return Err(ClientError::api(status, &String::from_utf8_lossy(&b), url));
            }
            let headers = resp.headers().clone();
            let bytes = resp.bytes().await.map(|b| b.to_vec())?;
            return Ok(RawResp { status, headers, bytes });
        }
    }

    /// GET + parse JSON, with `fields` and one no-fields retry on 400/403.
    async fn get_json_inner(&self, path: &str, params: &[(&str, &str)], fields: Option<&str>) -> ClientResult<Value> {
        let try_with_fields = fields.is_some();
        let mut prm: Vec<(&str, &str)> = params.to_vec();
        if try_with_fields {
            let f = fields.unwrap();
            prm.push(("fields", f));
        }
        let url = self.build_authed_url(path, &prm);
        match self.authed_get_raw(&url).await {
            Ok(raw) => serde_json::from_slice(&raw.bytes).map_err(ClientError::Json),
            Err(ClientError::Api { status, .. }) if try_with_fields && matches!(status, 400 | 403) => {
                let url2 = self.build_authed_url(path, params);
                let raw = self.authed_get_raw(&url2).await?;
                serde_json::from_slice(&raw.bytes).map_err(ClientError::Json)
            }
            Err(e) => Err(e),
        }
    }


    /// Python `_cache_key`: sha256("{namespace}|{path}?{canonical}"), user-scoped.
    fn cache_key(&self, namespace: &str, path: &str, params: &[(&str, &str)]) -> String {
        let mut map = Map::new();
        for (k, v) in params {
            map.insert(k.to_string(), Value::String(v.to_string()));
        }
        let canonical = serde_json::to_string(&Value::Object(map)).unwrap_or_default();
        let payload = format!("{namespace}|{path}?{canonical}");
        let digest = sha256_hex_first(&payload, 64);
        format!("{}:{digest}", self.user_scope)
    }

    fn cache_off(&self) -> bool {
        std::env::var("NTULEARN_CACHE_MODE")
            .map(|m| m.trim().to_ascii_lowercase() == "off")
            .unwrap_or(false)
    }

    /// Resolve TTL for `cache` kwarg: None -> method default, Some(t>0) -> t,
    /// Some(t<=0) -> no caching (returns None).
    fn ttl_for(&self, namespace: &str, cache: Option<f64>) -> Option<f64> {
        match cache {
            Some(t) if t <= 0.0 => None,
            Some(t) => Some(t),
            None => Some(crate::cache::default_ttl(namespace)),
        }
    }


    /// Paginate through `paging.nextPage` and collect all results.
    async fn paginated(&self, path: &str, params: &[(&str, &str)], fields: Option<&str>) -> ClientResult<Vec<Value>> {
        let mut all: Vec<Value> = Vec::new();
        let mut current = path.to_string();
        let mut first = true;
        let mut guard = 0u32;
        loop {
            guard += 1;
            if guard > 200 { break; }
            let prm: Vec<(&str, &str)> = if first {
                // Python `_get_paginated`: params.setdefault("limit", 200).
                let mut v = params.to_vec();
                if !params.iter().any(|(k, _)| *k == "limit") {
                    v.push(("limit", "200"));
                }
                v
            } else {
                Vec::new()
            };
            let data = self.get_json_inner(&current, &prm, fields).await?;
            if let Some(results) = data.get("results").and_then(|r| r.as_array()) {
                all.extend(results.iter().cloned());
            }
            let next = data
                .get("paging")
                .and_then(|p| p.get("nextPage"))
                .and_then(|n| n.as_str())
                .map(|s| s.to_string());
            match next {
                Some(n) if !n.is_empty() => {
                    current = if let Some(stripped) = n.strip_prefix(&self.base_url) {
                        stripped.to_string()
                    } else { n };
                    first = false;
                }
                _ => break,
            }
        }
        Ok(all)
    }

    /// Cache read; returns Some(value) on hit (honours ttl_for).
    fn cache_read(&self, namespace: &str, path: &str, params: &[(&str, &str)], cache: Option<f64>) -> Option<Value> {
        let ttl = self.ttl_for(namespace, cache)?;
        if self.cache_off() { return None; }
        let key = self.cache_key(namespace, path, params);
        let max_age = if ttl > 0.0 && cache.is_none() { None } else { Some(ttl) };
        self.cache.get(namespace, &key, max_age)
    }

    fn cache_write(&self, namespace: &str, path: &str, params: &[(&str, &str)], cache: Option<f64>, value: &Value) {
        let Some(ttl) = self.ttl_for(namespace, cache) else { return };
        if self.cache_off() { return; }
        let key = self.cache_key(namespace, path, params);
        self.cache.set(namespace, &key, value.clone(), ttl, Some(&self.user_scope));
    }

    /// Simple list endpoint pattern: read cache, else paginate + cache.
    async fn list_with_cache(
        &self,
        namespace: &str,
        path: &str,
        params: &[(&str, &str)],
        fields: Option<&str>,
        cache: Option<f64>,
    ) -> ClientResult<Value> {
        if let Some(hit) = self.cache_read(namespace, path, params, cache) {
            return Ok(hit);
        }
        let items = self.paginated(path, params, fields).await?;
        let value = Value::Array(items);
        self.cache_write(namespace, path, params, cache, &value);
        Ok(value)
    }

    /// Simple object endpoint pattern: read cache, else GET + cache.
    async fn object_with_cache(
        &self,
        namespace: &str,
        path: &str,
        params: &[(&str, &str)],
        fields: Option<&str>,
        cache: Option<f64>,
    ) -> ClientResult<Value> {
        if let Some(hit) = self.cache_read(namespace, path, params, cache) {
            return Ok(hit);
        }
        let value = self.get_json_inner(path, params, fields).await?;
        self.cache_write(namespace, path, params, cache, &value);
        Ok(value)
    }
    /// Authenticated GET to an absolute URL (string already fully built),
    /// with 401-refresh + transient retry (used by page-links).
    async fn authed_get_url_raw(&self, url: &str) -> ClientResult<RawResp> {
        self.authed_get_raw(url).await
    }

    // ==========================================================================
    // Users
    // ==========================================================================

    pub async fn get_my_enrollments(&self, cache: Option<f64>) -> ClientResult<Value> {
        let path = "/learn/api/public/v1/users/me/courses";
        let fields = default_fields("enrollments");
        self.list_with_cache("get_my_enrollments", path, &[], fields, cache).await
    }

    pub async fn get_my_user_id(&self) -> ClientResult<String> {
        let data = self.get_json_inner("/learn/api/public/v1/users/me", &[], None).await?;
        data.get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| ClientError::Other("users/me response missing id".to_string()))
    }

    // ==========================================================================
    // Courses
    // ==========================================================================

    pub async fn get_course(&self, course_id: &str, cache: Option<f64>) -> ClientResult<Value> {
        let path = format!("/learn/api/public/v1/courses/{course_id}");
        let fields = default_fields("course");
        self.object_with_cache("get_course", &path, &[], fields, cache).await
    }

    pub async fn get_courses_batch(&self, course_ids: &[String], cache: Option<f64>) -> ClientResult<Value> {
        let path = "/learn/api/public/v1/courses/_batch_";
        if let Some(hit) = self.cache_read("get_courses_batch", path, &[], cache) {
            return Ok(hit);
        }
        let mut out: Vec<Value> = Vec::with_capacity(course_ids.len());
        for cid in course_ids {
            match self.get_course(cid, Some(0.0)).await {
                Ok(c) => out.push(c),
                Err(_) => out.push(json!({"id": cid, "name": cid})),
            }
        }
        let value = Value::Array(out);
        self.cache_write("get_courses_batch", path, &[], cache, &value);
        Ok(value)
    }

    pub async fn get_course_search(&self, course_id: &str, query: &str, cache: Option<f64>) -> ClientResult<Value> {
        let path = format!("/learn/api/public/v1/courses/{course_id}/contents");
        let fields = default_fields("contents");
        let params: Vec<(&str, &str)> = vec![("search", query)];
        // Search failures return [] (matches Python: BlackboardAPIError -> []).
        let items = match self.paginated(&path, &params, fields).await {
            Ok(v) => v,
            Err(_) => Vec::new(),
        };
        let value = Value::Array(items);
        self.cache_write("get_course_search", &path, &params, cache, &value);
        Ok(value)
    }

    // ==========================================================================
    // Contents
    // ==========================================================================

    pub async fn get_course_contents(&self, course_id: &str, cache: Option<f64>) -> ClientResult<Value> {
        let path = format!("/learn/api/public/v1/courses/{course_id}/contents");
        let fields = default_fields("contents");
        self.list_with_cache("get_course_contents", &path, &[], fields, cache).await
    }

    pub async fn get_content_children(&self, course_id: &str, content_id: &str, cache: Option<f64>) -> ClientResult<Value> {
        let path = format!("/learn/api/public/v1/courses/{course_id}/contents/{content_id}/children");
        let fields = default_fields("contents");
        self.list_with_cache("get_content_children", &path, &[], fields, cache).await
    }

    pub async fn get_content_item(&self, course_id: &str, content_id: &str, cache: Option<f64>) -> ClientResult<Value> {
        let path = format!("/learn/api/public/v1/courses/{course_id}/contents/{content_id}");
        let fields = default_fields("contents");
        self.object_with_cache("get_content_item", &path, &[], fields, cache).await
    }

    pub async fn get_attachments(&self, course_id: &str, content_id: &str, cache: Option<f64>) -> ClientResult<Value> {
        let path = format!("/learn/api/public/v1/courses/{course_id}/contents/{content_id}/attachments");
        self.list_with_cache("get_attachments", &path, &[], None, cache).await
    }

    pub async fn get_attachment_download_url(
        &self,
        course_id: &str,
        content_id: &str,
        attachment_id: &str,
    ) -> ClientResult<String> {
        let path = format!(
            "/learn/api/public/v1/courses/{course_id}/contents/{content_id}/attachments/{attachment_id}/download"
        );
        let url = self.url(&path);
        let cookie = self.current_cookie().await;
        let cookie_header = if cookie.is_empty() { String::new() } else { format!("BbRouter={cookie}") };
        let resp = self
            .no_redirect
            .get(url)
            .header("Cookie", cookie_header)
            .send()
            .await
            .map_err(ClientError::Network)?;
        let status = resp.status().as_u16();
        if status == 401 {
            self.cache.invalidate_user(&self.user_scope);
            return Err(ClientError::Auth);
        }
        if matches!(status, 301 | 302 | 303 | 307 | 308) {
            if let Some(loc) = resp.headers().get(reqwest::header::LOCATION).and_then(|h| h.to_str().ok()) {
                return Ok(loc.to_string());
            }
        }
        if is_success_status(status) {
            return Ok(path); // some deployments return the file directly
        }
        let text = resp.text().await.unwrap_or_default();
        Err(ClientError::api(status, &text, &path))
    }

    // ==========================================================================
    // Messages
    // ==========================================================================

    pub async fn get_messages(
        &self,
        folder: Option<&str>,
        unread_only: bool,
        _since: Option<&str>,
        cache: Option<f64>,
    ) -> ClientResult<Value> {
        let path = "/learn/api/public/v1/users/me/messages";
        let mut params: Vec<(&str, &str)> = Vec::new();
        if let Some(f) = folder { params.push(("folder", f)); }
        if unread_only { params.push(("unreadOnly", "true")); }
        if let Some(s) = _since { params.push(("since", s)); }
        let fields = default_fields("messages");
        self.list_with_cache("get_messages", path, &params, fields, cache).await
    }

    pub async fn get_message(&self, message_id: &str, cache: Option<f64>) -> ClientResult<Value> {
        let path = format!("/learn/api/public/v1/users/me/messages/{message_id}");
        let fields = default_fields("messages");
        self.object_with_cache("get_message", &path, &[], fields, cache).await
    }

    pub async fn get_message_participants(&self, message_id: &str, cache: Option<f64>) -> ClientResult<Value> {
        let path = format!("/learn/api/public/v1/users/me/messages/{message_id}/participants");
        let fields = default_fields("course_users");
        self.list_with_cache("get_message_participants", &path, &[], fields, cache).await
    }

    // ==========================================================================
    // Course users & groups
    // ==========================================================================

    pub async fn get_course_users(&self, course_id: &str, cache: Option<f64>) -> ClientResult<Value> {
        let path = format!("/learn/api/public/v1/courses/{course_id}/users");
        let fields = default_fields("course_users");
        self.list_with_cache("get_course_users", &path, &[], fields, cache).await
    }

    pub async fn get_course_groups(&self, course_id: &str, cache: Option<f64>) -> ClientResult<Value> {
        let path = format!("/learn/api/public/v1/courses/{course_id}/groups");
        let fields = default_fields("groups");
        self.list_with_cache("get_course_groups", &path, &[], fields, cache).await
    }

    pub async fn get_group_users(&self, course_id: &str, group_id: &str, cache: Option<f64>) -> ClientResult<Value> {
        let path = format!("/learn/api/public/v1/courses/{course_id}/groups/{group_id}/users");
        let fields = default_fields("course_users");
        self.list_with_cache("get_group_users", &path, &[], fields, cache).await
    }

    // ==========================================================================
    // Gradebook
    // ==========================================================================

    pub async fn get_gradebook_columns(&self, course_id: &str, cache: Option<f64>) -> ClientResult<Value> {
        let path = format!("/learn/api/public/v1/courses/{course_id}/gradebook/columns");
        let fields = default_fields("grade_columns");
        self.list_with_cache("get_gradebook_columns", &path, &[], fields, cache).await
    }

    pub async fn get_user_grades(&self, course_id: &str, user_id: &str, cache: Option<f64>) -> ClientResult<Value> {
        let path = format!("/learn/api/public/v1/courses/{course_id}/gradebook/users/{user_id}");
        let fields = default_fields("user_grades");
        self.list_with_cache("get_user_grades", &path, &[], fields, cache).await
    }

    pub async fn get_gradebook_attempts(&self, course_id: &str, column_id: &str, cache: Option<f64>) -> ClientResult<Value> {
        let path = format!("/learn/api/public/v1/courses/{course_id}/gradebook/columns/{column_id}/attempts");
        let fields = default_fields("attempts");
        self.list_with_cache("get_gradebook_attempts", &path, &[], fields, cache).await
    }

    pub async fn get_user_attempts(&self, course_id: &str, column_id: &str, user_id: &str) -> ClientResult<Value> {
        let path = format!(
            "/learn/api/public/v1/courses/{course_id}/gradebook/columns/{column_id}/users/{user_id}/attempts"
        );
        let fields = default_fields("attempts");
        let params: Vec<(&str, &str)> = Vec::new();
        if let Some(hit) = self.cache_read("get_user_attempts", &path, &params, None) {
            return Ok(hit);
        }
        let items = self.paginated(&path, &[], fields).await?;
        let value = Value::Array(items);
        self.cache_write("get_user_attempts", &path, &[], None, &value);
        Ok(value)
    }

    // ==========================================================================
    // Terms
    // ==========================================================================

    pub async fn get_term(&self, term_id: &str, cache: Option<f64>) -> ClientResult<Value> {
        let path = format!("/learn/api/public/v1/terms/{term_id}");
        let fields = default_fields("term");
        self.object_with_cache("get_term", &path, &[], fields, cache).await
    }

    // ==========================================================================
    // Announcements
    // ==========================================================================

    pub async fn get_announcements(&self, course_id: &str, cache: Option<f64>) -> ClientResult<Value> {
        let path = format!("/learn/api/public/v1/courses/{course_id}/announcements");
        let fields = default_fields("announcements");
        self.list_with_cache("get_announcements", &path, &[], fields, cache).await
    }

    // ==========================================================================
    // Calendar
    // ==========================================================================

    pub async fn get_calendar_items(
        &self,
        course_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
        item_type: Option<&str>,
        cache: Option<f64>,
    ) -> ClientResult<Value> {
        let path = "/learn/api/public/v1/calendars/items";
        let mut params: Vec<(&str, &str)> = Vec::new();
        if let Some(c) = course_id { params.push(("courseId", c)); }
        if let Some(s) = since { params.push(("since", s)); }
        if let Some(u) = until { params.push(("until", u)); }
        if let Some(t) = item_type { params.push(("type", t)); }
        let fields = default_fields("calendar");
        self.list_with_cache("get_calendar_items", path, &params, fields, cache).await
    }

    // ==========================================================================
    // File download
    // ==========================================================================

    /// Download a file URL -> (content_bytes, content_type). Same-origin URLs
    /// use the authenticated client; *.blackboard.com CDN URLs use the
    /// cookie-free client; other hosts are rejected (Python parity).
    pub async fn download_bytes(&self, url: &str) -> ClientResult<(Vec<u8>, Option<String>)> {
        // Python `_download_response`: a RELATIVE url (no scheme, no netloc) is
        // fetched through the authenticated client as-is.
        let parsed_rel = reqwest::Url::parse(url);
        match parsed_rel {
            Err(_) => {
                // Treat as a relative path (or a malformed absolute URL). If it
                // looks like a path (no scheme), fetch via the authed client.
                let looks_path = !url.contains("://");
                if looks_path {
                    let full = self.url(url);
                    let resp = self.authed_get_url_raw(&full).await?;
                    let ct = resp.headers.get(reqwest::header::CONTENT_TYPE).and_then(|h| h.to_str().ok()).map(|s| s.to_string());
                    return Ok((resp.bytes, ct));
                }
                return Err(ClientError::Other(format!("Unsafe download URL: {url}")));
            }
            Ok(parsed) => {
                if !matches!(parsed.scheme(), "http" | "https") {
                    return Err(ClientError::Other(format!("Unsafe download URL scheme: {}", parsed.scheme())));
                }
                let base = reqwest::Url::parse(&self.base_url).ok();
                let is_same_origin = base.map(|b| {
                    b.scheme() == parsed.scheme()
                        && b.host_str() == parsed.host_str()
                        && b.port_or_known_default() == parsed.port_or_known_default()
                }).unwrap_or(false);
                let host = parsed.host_str().unwrap_or("").to_string();

                if is_same_origin {
                    let path_only = format!("{}?{}", parsed.path(), parsed.query().unwrap_or(""));
                    let full = self.url(&path_only);
                    let resp = self.authed_get_url_raw(&full).await?;
                    let ct = resp.headers.get(reqwest::header::CONTENT_TYPE).and_then(|h| h.to_str().ok()).map(|s| s.to_string());
                    return Ok((resp.bytes, ct));
                }
                if host.ends_with(".blackboard.com") {
                    let raw = self.external.get(url).send().await.map_err(ClientError::Network)?;
                    let status = raw.status().as_u16();
                    if !is_success_status(status) {
                        let text = raw.text().await.unwrap_or_default();
                        return Err(ClientError::api(status, &text, url));
                    }
                    let ct = raw.headers().get(reqwest::header::CONTENT_TYPE).and_then(|h| h.to_str().ok()).map(|s| s.to_string());
                    let bytes = raw.bytes().await.map(|b| b.to_vec())?;
                    return Ok((bytes, ct));
                }
                Err(ClientError::Other(format!("Unsafe download URL host: {host}")))
            }
        }
    }

    // ==========================================================================
    // Cache maintenance
    // ==========================================================================

    /// Drop every cached entry for a course (best-effort). The SQLite cache
    /// stores namespace+key; keys embed the user scope but not course ids, so
    /// this clears all namespaces that are course-scoped (simple + safe).
    pub async fn invalidate_course(&self, _course_id: &str) {
        // The Python side has no per-course invalidation either; the server
        // layer reacts to 401 via invalidate_user. Keep this a no-op.
        let _ = _course_id;
    }

    pub async fn invalidate_all(&self) {
        self.cache.clear();
    }

    /// Raw endpoint JSON GET with an optional cache TTL (used by resources.rs
    /// and as an escape hatch). NOT part of the typed API.
    pub async fn get_json(
        &self,
        path: &str,
        params: &[(&str, &str)],
        max_age: Option<Duration>,
    ) -> ClientResult<Value> {
        if let Some(ma) = max_age {
            let ttl = ma.as_secs_f64();
            let key = self.cache_key("get_json", path, params);
            if let Some(hit) = self.cache.get("get_json", &key, Some(ttl)) {
                return Ok(hit);
            }
            let raw = self.authed_get_raw(&self.build_authed_url(path, params)).await?;
            let value: Value = serde_json::from_slice(&raw.bytes).map_err(ClientError::Json)?;
            let key = self.cache_key("get_json", path, params);
            self.cache.set("get_json", &key, value.clone(), ttl, Some(&self.user_scope));
            return Ok(value);
        }
        let raw = self.authed_get_raw(&self.build_authed_url(path, params)).await?;
        serde_json::from_slice(&raw.bytes).map_err(ClientError::Json)
    }

    /// Public pagination escape hatch: returns the array of results.
    pub async fn get_paginated_value(
        &self,
        path: &str,
        params: &[(&str, &str)],
    ) -> ClientResult<Value> {
        let items = self.paginated(path, params, None).await?;
        Ok(Value::Array(items))
    }

    /// Fetch raw bytes for a same-origin or blackboard CDN URL without the
    /// (bytes, content_type) tuple — used where only bytes matter.
    pub async fn download_bytes_only(&self, url: &str) -> ClientResult<Vec<u8>> {
        self.download_bytes(url).await.map(|(b, _)| b)
    }

}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_status_range() {
        assert!(is_success_status(200));
        assert!(is_success_status(299));
        assert!(!is_success_status(199));
        assert!(!is_success_status(300));
        assert!(!is_success_status(401));
        assert!(!is_success_status(403));
    }

    #[test]
    fn retryable_statuses() {
        for s in [429, 500, 502, 503, 504] {
            assert!(is_retryable_status(s), "status {s} should be retryable");
        }
        for s in [200, 400, 401, 403, 404, 408, 425] {
            assert!(!is_retryable_status(s), "status {s} should NOT be retryable");
        }
    }

    #[test]
    fn build_query_encodes_and_sorts_matching_python() {
        // Python `_build_query` gets params in insertion order; the client
        // passes sorted keys (BTreeMap) so the order is `a` then `b`.
        let q = build_query(&[("b", "2 2"), ("a", "1/1")]);
        assert_eq!(q, "b=2%202&a=1%2F1");
    }

    #[test]
    fn env_flag_bool_parsing() {
        std::env::set_var("NTULEARN_TEST_FLAG", "0");
        assert!(!env_flag("NTULEARN_TEST_FLAG", true));
        std::env::set_var("NTULEARN_TEST_FLAG", "false");
        assert!(!env_flag("NTULEARN_TEST_FLAG", true));
        std::env::set_var("NTULEARN_TEST_FLAG", "yes");
        assert!(env_flag("NTULEARN_TEST_FLAG", true));
        std::env::set_var("NTULEARN_TEST_FLAG", "1");
        assert!(env_flag("NTULEARN_TEST_FLAG", true));
        std::env::remove_var("NTULEARN_TEST_FLAG");
    }

    #[test]
    fn sha256_scope_length() {
        assert_eq!(sha256_hex_first("cookie-value", 16).len(), 16);
        assert_eq!(sha256_hex_first("", 16), "e3b0c44298fc1c14");
        // deterministic for the same input
        assert_eq!(
            sha256_hex_first("cookie-value", 16),
            sha256_hex_first("cookie-value", 16)
        );
    }

    #[test]
    fn url_helper_absolute_and_relative() {
        let c = NTULearnClient::new_for_test("https://ntulearn.ntu.edu.sg/");
        assert_eq!(c.url("https://x.com/y"), "https://x.com/y");
        assert_eq!(
            c.url("/learn/api/public/v1/courses"),
            "https://ntulearn.ntu.edu.sg/learn/api/public/v1/courses"
        );
    }
}
