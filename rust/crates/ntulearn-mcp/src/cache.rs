
//! Per-method TTL cache. Baseline: in-memory map (fast, per-process).
//! subagent-A may add durable SQLite persistence (backend parity with the
//! Python `data_cache` at ~/.cache/ntulearn-mcp).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

struct Entry {
    value: Value,
    expires_at: Instant,
}

#[derive(Default)]
struct Inner {
    map: HashMap<String, Entry>,
}

pub struct DataCache {
    inner: Mutex<Inner>,
}

impl DataCache {
    pub fn open() -> anyhow::Result<Self> {
        Ok(DataCache { inner: Mutex::new(Inner::default()) })
    }

    pub fn get(&self, key: &str) -> Option<Value> {
        let mut g = self.inner.lock().ok()?;
        match g.map.get(key) {
            Some(e) if e.expires_at > Instant::now() => Some(e.value.clone()),
            Some(_) => {
                g.map.remove(key);
                None
            }
            None => None,
        }
    }

    pub fn set(&self, key: &str, value: Value, ttl: Duration) {
        if let Ok(mut g) = self.inner.lock() {
            g.map.insert(
                key.to_string(),
                Entry { value, expires_at: Instant::now() + ttl },
            );
        }
    }

    pub fn clear(&self) {
        if let Ok(mut g) = self.inner.lock() {
            g.map.clear();
        }
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.inner.lock().map(|g| g.map.len()).unwrap_or(0)
    }
}

/// Shared cache keys: method + path + sorted query string.
pub fn cache_key(method: &str, path: &str, params: &[(&str, &str)]) -> String {
    use std::collections::BTreeMap;
    let mut ps: BTreeMap<&str, &str> = BTreeMap::new();
    for (k, v) in params {
        ps.insert(k, v);
    }
    let q: Vec<String> = ps.iter().map(|(k, v)| format!("{k}={v}")).collect();
    format!("{method}:{path}:{}", q.join("&"))
}
