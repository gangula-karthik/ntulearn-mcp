//! Tiny disk cache of raw API responses, shared by the Peerceptiv (pcv)
//! and FeedbackFruits (fbf) clients. One JSON file per service sits next to
//! the cookie jar; entries carry the raw response plus the unix second they
//! were fetched, and callers pick per-key TTLs:
//!
//! - static records (assignment, rubric, activity, task list): minutes;
//! - live progress rows: seconds, served stale-while-revalidate.
//!
//! Every write path must invalidate what it changed — a cached read after a
//! successful write would otherwise lie about progress counts.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Default)]
pub struct CachedResponses {
    #[serde(default)]
    pub entries: HashMap<String, CachedResponse>,
}

#[derive(Serialize, Deserialize)]
pub struct CachedResponse {
    pub v: Value,
    pub t: i64,
}

fn cache_file(name: &str) -> PathBuf {
    crate::cookie::cookie_file_path()
        .parent()
        .map(|p| p.join(name))
        .unwrap_or_else(|| PathBuf::from(name))
}

fn load(name: &str) -> CachedResponses {
    std::fs::read_to_string(cache_file(name))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save(name: &str, cache: &CachedResponses) {
    let path = cache_file(name);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string_pretty(cache) {
        let _ = std::fs::write(path, text);
    }
}

/// Fresh value for `key`, or None when missing/older than `ttl_secs`.
/// Prunes expired entries for the same key so a file cannot grow unboundedly
/// across assignment/term changes.
pub fn cached(name: &str, key: &str, ttl_secs: i64) -> Option<Value> {
    cached_with_age(name, key, ttl_secs).map(|(v, _)| v)
}

/// Same, but also reports the entry's age in seconds (for stale-while-
/// revalidate decisions).
pub fn cached_with_age(name: &str, key: &str, ttl_secs: i64) -> Option<(Value, i64)> {
    let mut cache = load(name);
    let now = now_secs();
    match cache.entries.get(key) {
        Some(e) if now - e.t < ttl_secs => Some((e.v.clone(), now - e.t)),
        _ => {
            if cache.entries.remove(key).is_some() {
                save(name, &cache);
            }
            None
        }
    }
}

pub fn store(name: &str, key: &str, v: &Value) {
    let mut cache = load(name);
    cache.entries.insert(
        key.to_string(),
        CachedResponse {
            v: v.clone(),
            t: now_secs(),
        },
    );
    save(name, &cache);
}

/// Drop every entry whose key starts with `prefix` (e.g. "progress/").
pub fn invalidate_prefix(name: &str, prefix: &str) {
    let mut cache = load(name);
    let before = cache.entries.len();
    cache.entries.retain(|k, _| !k.starts_with(prefix));
    if cache.entries.len() != before {
        save(name, &cache);
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
