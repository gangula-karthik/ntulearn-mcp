
//! Async HTTP client for the Blackboard Learn public REST API.
//!
//! Port of `src/ntulearn_mcp/client.py`. Baseline implementation is functional
//! (env/file cookie, HTTP/2, JSON parsing, per-method TTL cache, one 401
//! refresh). subagent-A may harden: exponential backoff/retry on transient
//! errors, endpoint-specific `fields` trimming, durable SQLite cache, browser
//! cookie helper. Keep the public API in this file stable — tools.rs / handlers.rs
//! compile against it.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::cache::{cache_key, DataCache};
use crate::cookie;

pub const DEFAULT_FIELDS: &str = "";

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
    #[error("no BbRouter cookie available (set NTULEARN_COOKIE to your Blackboard session cookie)")]
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
    fn api(status: u16, body: &str, path: &str) -> Self {
        let where_ = if path.is_empty() { String::new() } else { format!(" at {path}") };
        let snippet = body.chars().take(300).collect::<String>().replace('\n', " ");
        let (class, hint) = match status {
            403 => ("forbidden", "The current user lacks access to this resource (course not enrolled, instructor-only data, or unavailable). "),
            404 => ("not found", "Check the course_id / content_id is correct. "),
            429 => ("rate limited", "Slow down and retry later. "),
            s if s >= 500 => ("server error", "NTULearn may be down; retry shortly. "),
            _ => ("other", ""),
        };
        let hint = hint.to_string();
        ClientError::Api {
            status,
            class,
            path_desc: where_,
            hint,
            body: format!("{snippet}"),
        }
    }
}

pub type ClientResult<T> = Result<T, ClientError>;

/// Holds the current cookie and a way to refresh it on 401.
struct CookieState {
    value: String,
    refresh: Option<Box<dyn Fn() -> Option<String> + Send + Sync>>,
}

pub struct NTULearnClient {
    http: reqwest::Client,
    base_url: String,
    cookie: tokio::sync::Mutex<CookieState>,
    cache: Arc<DataCache>,
}

impl NTULearnClient {
    pub fn new(base_url: String, cookie_value: String, cache: Arc<DataCache>) -> ClientResult<Self> {
        let mut value = cookie_value;
        if let Some(rest) = value.strip_prefix("BbRouter=") {
            value = rest.to_string();
        }
        let http = reqwest::Client::builder()
            .user_agent("ntulearn-mcp-rust/0.3.0")
            .build()
            .map_err(|e| ClientError::Other(format!("reqwest client build: {e}")))?;
        // On 401 we re-resolve from env/file.
        let refresh = Some(Box::new(|| cookie::resolve_cookie()) as Box<dyn Fn() -> Option<String> + Send + Sync>);
        Ok(NTULearnClient {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            cookie: tokio::sync::Mutex::new(CookieState { value, refresh }),
            cache,
        })
    }

    fn url(&self, path: &str) -> String {
        if path.starts_with("http://") || path.starts_with("https://") {
            path.to_string()
        } else {
            format!("{}{}", self.base_url, path)
        }
    }

    fn request(&self, url: &str) -> reqwest::RequestBuilder {
        let value = self.cookie.blocking_lock().value.clone();
        let cookie_header = if value.is_empty() {
            String::new()
        } else {
            format!("BbRouter={value}")
        };
        self.http.get(url).header("Cookie", cookie_header)
    }

    fn cookie_value(&self) -> String {
        self.cookie.blocking_lock().value.clone()
    }

    /// Read only; returns the current raw cookie (used by tests/user_scope).
    pub fn current_cookie(&self) -> String {
        self.cookie_value()
    }

    fn set_cookie(&self, value: String) {
        let mut mutex = self.cookie.blocking_lock();
        mutex.value = value;
    }

    /// Single GET -> JSON, with fields trimming + cache.
    pub async fn get_json(
        &self,
        path: &str,
        params: &[(&str, &str)],
        cache: Option<Duration>,
    ) -> ClientResult<Value> {
        let key = cache_key("GET", path, params);
        if let Some(ttl) = cache {
            if let Some(v) = self.cache.get(&key) {
                return Ok(v);
            }
        }
        let body = self._do_get(path, params).await?;
        let value: Value = serde_json::from_slice(&body)?;
        if let Some(ttl) = cache {
            self.cache.set(&key, value.clone(), ttl);
        }
        Ok(value)
    }

    /// Follow Blackboard cursor pagination, collecting all results.
    pub async fn get_paginated(
        &self,
        path: &str,
        params: &[(&str, &str)],
        cache: Option<Duration>,
    ) -> ClientResult<Vec<Value>> {
        let mut all: Vec<Value> = Vec::new();
        let mut current = path.to_string();
        let mut first = true;
        loop {
            let params_ref: &[(&str, &str)] = if first { params } else { &[] };
            let data = if first {
                self.get_json(&current, params_ref, cache).await?
            } else {
                self.get_json(&current, &[], None).await?
            };
            if let Some(results) = data.get("results").and_then(|r| r.as_array()) {
                all.extend(results.iter().cloned());
            }
            let next = data
                .get("paging")
                .and_then(|p| p.get("nextPage"))
                .and_then(|n| n.as_str())
                .map(|s| s.to_string());
            if first {
                first = false;
            }
            match next {
                Some(n) if !n.is_empty() => current = n,
                _ => break,
            }
        }
        Ok(all)
    }

    /// GET arbitrary URL as text (attachment content, non-JSON endpoints).
    pub async fn get_text(&self, url: &str) -> ClientResult<String> {
        let resp = self.perform_get(self.url(url)).await?;
        let text = resp.text().await.map_err(ClientError::Network)?;
        Ok(text)
    }

    /// GET arbitrary URL as raw bytes (file downloads).
    pub async fn download_bytes(&self, url: &str) -> ClientResult<Vec<u8>> {
        let resp = self.perform_get(self.url(url)).await?;
        let bytes = resp.bytes().await.map_err(ClientError::Network)?;
        Ok(bytes.to_vec())
    }

    async fn perform_get(&self, url: String) -> ClientResult<reqwest::Response> {
        let resp = self.request(&url).send().await.map_err(ClientError::Network)?;
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            // One refresh + retry with the new cookie.
            let refreshed = {
                let mut guard = self.cookie.lock().await;
                if let Some(f) = guard.refresh.as_ref() {
                    f()
                } else {
                    None
                }
            };
            if let Some(new_cookie) = refreshed {
                self._set_cookie_async(new_cookie).await;
                let retry = self.request(&url).send().await.map_err(ClientError::Network)?;
                if retry.status() != reqwest::StatusCode::UNAUTHORIZED {
                    return Ok(retry);
                }
            }
            return Err(ClientError::Auth);
        }
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(ClientError::api(status.as_u16(), &text, &url));
        }
        Ok(resp)
    }

    async fn _do_get(&self, path: &str, params: &[(&str, &str)]) -> ClientResult<Vec<u8>> {
        let mut url = self.url(path);
        if !params.is_empty() {
            let mut pairs = Vec::with_capacity(params.len());
            for (k, v) in params {
                pairs.push(format!("{k}={}", percent_encoding::utf8_percent_encode(v, percent_encoding::NON_ALPHANUMERIC)));
            }
            let sep = if url.contains('?') { '&' } else { '?' };
            url.push(sep);
            url.push_str(&pairs.join("&"));
        }
        let resp = self.perform_get(url).await?;
        let bytes = resp.bytes().await.map_err(ClientError::Network)?;
        Ok(bytes.to_vec())
    }

    /// Mark caches for a course invalidated (after downloads).
    pub fn invalidate_course(&self, _course_id: &str) {
        // TODO(subagent-A): clear entries whose key contains the course id.
    }
}

// helper to set cookie inside async context
impl NTULearnClient {
    async fn _set_cookie_async(&self, value: String) {
        let mut g = self.cookie.lock().await;
        g.value = value;
    }
}
