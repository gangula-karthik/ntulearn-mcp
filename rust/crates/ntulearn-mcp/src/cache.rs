//! Persistent TTL cache with an in-memory LRU on top of SQLite.
//!
//! Port of `src/ntulearn_mcp/cache.py` (DataCache). Defaults mirror the Python
//! side: SQLite file at `<data_dir>/ntulearn-mcp/cache.sqlite3` (override with
//! `NTULEARN_CACHE_DIR`), `NTULEARN_CACHE_MODE` = readwrite/readonly/off, and a
//! 4096-entry in-memory LRU. SQLite writes are best-effort: any failure marks
//! the backend broken and the cache degrades to memory-only without raising.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};
use serde_json::Value;

pub const DEFAULT_LRU_SIZE: usize = 4096;
const DEFAULT_BASE_TTL: f64 = 300.0;

/// Per-method default TTLs (seconds). Mirrors cache.py `DEFAULT_TTL_SECONDS`.
/// The "tracker" namespace backs common.whats_new's last-seen watermark.
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
    ("tracker", 30.0 * 24.0 * 60.0 * 60.0),
];

/// Default TTL (seconds) for a namespace, falling back to 300s.
pub fn default_ttl(namespace: &str) -> f64 {
    DEFAULT_TTL_SECONDS
        .iter()
        .find(|(n, _)| *n == namespace)
        .map(|(_, t)| *t)
        .unwrap_or(DEFAULT_BASE_TTL)
}

fn now_seconds() -> f64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs_f64(),
        Err(_) => 0.0,
    }
}

fn default_cache_path() -> PathBuf {
    if let Ok(override_dir) = std::env::var("NTULEARN_CACHE_DIR") {
        let trimmed = override_dir.trim();
        if !trimmed.is_empty() {
            // The env var may point at a directory or at the sqlite file itself
            // (Python treats it as the file path). Treat a trailing .sqlite3 as
            // a file; otherwise append the default file name.
            if trimmed.ends_with(".sqlite3") {
                return PathBuf::from(trimmed);
            }
            return PathBuf::from(trimmed).join("cache.sqlite3");
        }
    }
    let dir = dirs::cache_dir().unwrap_or_else(|| PathBuf::from("."));
    dir.join("ntulearn-mcp").join("cache.sqlite3")
}

fn cache_mode() -> String {
    std::env::var("NTULEARN_CACHE_MODE")
        .unwrap_or_else(|_| "readwrite".to_string())
        .trim()
        .to_ascii_lowercase()
}

/// Shared legacy key helper (kept for API compatibility with the baseline:
/// `method:path:q1=v1&...`). New typed cache keys are user-scoped hashes built
/// by the client (`_cache_key`), which is the Python-parity scheme.
pub fn cache_key(method: &str, path: &str, params: &[(&str, &str)]) -> String {
    let mut ps: Vec<(&str, &str)> = params.to_vec();
    ps.sort();
    let q: Vec<String> = ps.iter().map(|(k, v)| format!("{k}={v}")).collect();
    format!("{method}:{path}:{}", q.join("&"))
}

#[derive(Clone)]
struct MemEntry {
    value: Value,
    created_at: f64,
    expires_at: f64,
    user_scope: String,
    last_used: u64,
}

struct Inner {
    mem: HashMap<(String, String), MemEntry>,
    ticks: u64,
    schema_ok: bool,
    sqlite_broken: bool,
}

pub struct DataCache {
    inner: Mutex<Inner>,
    path: PathBuf,
    mode: String,
    max_size: usize,
}

impl DataCache {
    /// Open the persistent cache (best-effort; SQLite failures degrade to
    /// memory-only). File location mirrors the Python default cache dir.
    pub fn open() -> anyhow::Result<Self> {
        let path = default_cache_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        tracing::debug!(path = %path.display(), "opening data cache");
        Ok(DataCache {
            inner: Mutex::new(Inner {
                mem: HashMap::new(),
                ticks: 0,
                schema_ok: false,
                sqlite_broken: false,
            }),
            path,
            mode: cache_mode(),
            max_size: DEFAULT_LRU_SIZE,
        })
    }

    /// Open a SQLite connection (creating the table on first use).
    /// Returns None when caching is off or the backend is known-broken.
    fn sqlite_conn(&self, inner: &mut Inner) -> Option<Connection> {
        if self.mode == "off" || inner.sqlite_broken {
            return None;
        }
        let conn = match Connection::open(&self.path) {
            Ok(c) => c,
            Err(_) => {
                inner.sqlite_broken = true;
                return None;
            }
        };
        let _ = conn.busy_timeout(std::time::Duration::from_secs(2));
        if !inner.schema_ok {
            match conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS data_cache (\
                     namespace TEXT NOT NULL,\
                     key TEXT NOT NULL,\
                     value TEXT NOT NULL,\
                     created_at REAL NOT NULL,\
                     expires_at REAL NOT NULL,\
                     user_scope TEXT NOT NULL DEFAULT '',\
                     PRIMARY KEY (namespace, key))",
            ) {
                Ok(_) => {
                    let _ = conn.execute("PRAGMA synchronous = OFF", []);
                    inner.schema_ok = true;
                }
                Err(_) => {
                    inner.sqlite_broken = true;
                    return None;
                }
            }
        }
        Some(conn)
    }

    /// Read a value, honouring TTL and (optionally) a max-age bound on the
    /// entry's created time. Misses and stale entries return None.
    pub fn get(&self, namespace: &str, key: &str, max_age_seconds: Option<f64>) -> Option<Value> {
        if self.mode == "off" {
            return None;
        }
        let now = now_seconds();
        let mem_key = (namespace.to_string(), key.to_string());
        let mut g = match self.inner.lock() {
            Ok(g) => g,
            Err(_) => return None,
        };
        g.ticks += 1;

        if let Some(e) = g.mem.get(&mem_key) {
            let fresh = now < e.expires_at
                && match max_age_seconds {
                    Some(m) => now - e.created_at <= m,
                    None => true,
                };
            if fresh {
                let tick = g.ticks;
                let e = g.mem.get_mut(&mem_key).expect("checked above");
                e.last_used = tick;
                return Some(e.value.clone());
            }
            g.mem.remove(&mem_key);
        }

        let conn = self.sqlite_conn(&mut g)?;
        let row = conn
            .query_row(
                "SELECT value, created_at, expires_at, user_scope FROM data_cache \
                 WHERE namespace=?1 AND key=?2",
                params![namespace, key],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, f64>(1)?,
                        row.get::<_, f64>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .ok();
        if let Some((value_s, created, expires, scope)) = row {
            let fresh = now < expires
                && match max_age_seconds {
                    Some(m) => now - created <= m,
                    None => true,
                };
            if fresh {
                if let Ok(value) = serde_json::from_str::<Value>(&value_s) {
                    g.ticks += 1;
                    let last_used = g.ticks;
                    g.mem.insert(
                        mem_key,
                        MemEntry {
                            value: value.clone(),
                            created_at: created,
                            expires_at: expires,
                            user_scope: scope,
                            last_used,
                        },
                    );
                    Self::evict(&mut g, self.max_size);
                    return Some(value);
                }
            }
        }
        None
    }

    /// Store a JSON value with the given TTL, scoped to a user. SQLite write
    /// failures are swallowed; the in-memory entry still stands.
    #[allow(clippy::too_many_arguments)]
    pub fn set(
        &self,
        namespace: &str,
        key: &str,
        value: Value,
        ttl_seconds: f64,
        user_scope: Option<&str>,
    ) {
        if self.mode != "readwrite" {
            return; // readonly and off: no writes
        }
        if !ttl_seconds.is_finite() || ttl_seconds <= 0.0 {
            return;
        }
        let now = now_seconds();
        let scope = user_scope.unwrap_or("").to_string();
        let mem_key = (namespace.to_string(), key.to_string());
        let mut g = match self.inner.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        g.ticks += 1;
        let last_used = g.ticks;
        g.mem.insert(
            mem_key.clone(),
            MemEntry {
                value: value.clone(),
                created_at: now,
                expires_at: now + ttl_seconds,
                user_scope: scope.clone(),
                last_used,
            },
        );
        Self::evict(&mut g, self.max_size);

        let conn = self.sqlite_conn(&mut g);
        let Some(conn) = conn else { return };
        let encoded = serde_json::to_string(&value).ok();
        let Some(encoded) = encoded else { return };
        match conn.execute(
            "INSERT OR REPLACE INTO data_cache \
             (namespace, key, value, created_at, expires_at, user_scope) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![namespace, key, encoded, now, now + ttl_seconds, scope],
        ) {
            Ok(_) => {
            }
            Err(_) => {
                let mut g = match self.inner.lock() {
                    Ok(g) => g,
                    Err(_) => return,
                };
                g.sqlite_broken = true;
            }
        }
    }

    /// Remove a single entry from both backends.
    pub fn delete(&self, namespace: &str, key: &str) {
        let mem_key = (namespace.to_string(), key.to_string());
        if let Ok(mut g) = self.inner.lock() {
            g.mem.remove(&mem_key);
        }
        if self.mode == "off" {
            return;
        }
        let mut g = match self.inner.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let conn = self.sqlite_conn(&mut g);
        if let Some(conn) = conn {
            let _ = conn.execute(
                "DELETE FROM data_cache WHERE namespace=?1 AND key=?2",
                params![namespace, key],
            );
        }
    }

    /// Drop every entry owned by a user scope (used on 401 refresh).
    pub fn invalidate_user(&self, user_scope: &str) {
        if user_scope.is_empty() {
            return;
        }
        if let Ok(mut g) = self.inner.lock() {
            g.mem.retain(|k, e| {
                !(e.user_scope == user_scope || k.1.starts_with(&format!("{user_scope}:")))
            });
        }
        if self.mode == "off" {
            return;
        }
        let mut g = match self.inner.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let conn = self.sqlite_conn(&mut g);
        if let Some(conn) = conn {
            let _ = conn.execute(
                "DELETE FROM data_cache WHERE user_scope=?1 OR key LIKE ?2",
                params![user_scope, format!("{user_scope}:%")],
            );
        }
    }

    /// Remove everything from both backends.
    pub fn clear(&self) {
        if let Ok(mut g) = self.inner.lock() {
            g.mem.clear();
        }
        if self.mode == "off" {
            return;
        }
        let mut g = match self.inner.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let conn = self.sqlite_conn(&mut g);
        if let Some(conn) = conn {
            let _ = conn.execute("DELETE FROM data_cache", []);
        }
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.inner.lock().map(|g| g.mem.len()).unwrap_or(0)
    }

    /// Drop least-recently-used memory entries once the LRU is full.
    fn evict(inner: &mut Inner, max_size: usize) {
        while inner.mem.len() > max_size {
            let oldest = inner
                .mem
                .iter()
                .min_by_key(|(_, e)| e.last_used)
                .map(|(k, _)| k.clone());
            if let Some(k) = oldest {
                inner.mem.remove(&k);
            } else {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test-only constructor: fresh cache pointing at a unique temp path.
    fn fresh(tag: &str) -> DataCache {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "ntulearn-cache-test-{}-{}.sqlite3",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_file(&p);
        DataCache {
            inner: Mutex::new(Inner {
                mem: HashMap::new(),
                ticks: 0,
                schema_ok: false,
                sqlite_broken: false,
            }),
            path: p,
            mode: "readwrite".to_string(),
            max_size: DEFAULT_LRU_SIZE,
        }
    }

    fn fresh_sized(tag: &str, max_size: usize) -> DataCache {
        let mut dc = fresh(tag);
        dc.max_size = max_size;
        dc
    }

    #[test]
    fn default_ttls_exist() {
        assert_eq!(default_ttl("get_course"), 3600.0);
        assert_eq!(default_ttl("nope"), DEFAULT_BASE_TTL);
    }

    #[test]
    fn legacy_cache_key_sorted() {
        assert_eq!(
            cache_key("GET", "/x", &[("b", "2"), ("a", "1")]),
            "GET:/x:a=1&b=2"
        );
    }

    #[test]
    fn set_get_roundtrip() {
        let dc = fresh("roundtrip");
        dc.set("n", "k", serde_json::json!({"a": 1}), 600.0, Some("u1"));
        let hit = dc.get("n", "k", None);
        assert_eq!(hit, Some(serde_json::json!({"a": 1})));
    }

    #[test]
    fn ttl_expiry_returns_none() {
        let dc = fresh("ttl");
        dc.set("n", "k", serde_json::json!("v"), 0.001, Some("u1"));
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert_eq!(dc.get("n", "k", None), None);
    }

    #[test]
    fn max_age_honoured() {
        let dc = fresh("maxage");
        dc.set("n", "k", serde_json::json!("v"), 600.0, Some("u1"));
        // max_age=0 forces a re-read with an impossible fresh window (but the
        // value is still fresh for ttl): Python semantics `now - created <= 0`
        // after a microsecond of sleep -> miss.
        std::thread::sleep(std::time::Duration::from_micros(10));
        assert_eq!(dc.get("n", "k", Some(0.0)), None);
        // No max_age -> hit (within 600s ttl).
        assert_eq!(dc.get("n", "k", None), Some(serde_json::json!("v")));
    }

    #[test]
    fn user_scope_invalidation() {
        let dc = fresh("scope");
        dc.set("n", "u1:k1", serde_json::json!("a"), 600.0, Some("u1"));
        dc.set("n", "u2:k2", serde_json::json!("b"), 600.0, Some("u2"));
        dc.invalidate_user("u1");
        assert_eq!(dc.get("n", "u1:k1", None), None);
        assert_eq!(dc.get("n", "u2:k2", None), Some(serde_json::json!("b")));
    }

    #[test]
    fn clear_removes_everything() {
        let dc = fresh("clear");
        dc.set("n", "k", serde_json::json!("v"), 600.0, Some("u1"));
        dc.set("m", "k2", serde_json::json!("w"), 600.0, Some("u2"));
        dc.clear();
        assert_eq!(dc.get("n", "k", None), None);
        assert_eq!(dc.get("m", "k2", None), None);
    }

    #[test]
    fn delete_removes_entry() {
        let dc = fresh("delete");
        dc.set("n", "k", serde_json::json!("v"), 600.0, None);
        dc.delete("n", "k");
        assert_eq!(dc.get("n", "k", None), None);
    }

    #[test]
    fn off_mode_disables_reads_and_writes() {
        let mut dc = fresh("off");
        dc.mode = "off".to_string();
        dc.set("n", "k", serde_json::json!("v"), 600.0, None);
        assert_eq!(dc.get("n", "k", None), None);
    }

    #[test]
    fn readonly_mode_serves_hits_but_does_not_write() {
        let dc = fresh("ro");
        dc.set("n", "k", serde_json::json!("v"), 600.0, None);
        // now a readonly handle over the same path/backend
        let ro = DataCache {
            inner: Mutex::new(Inner {
                mem: HashMap::new(),
                ticks: 0,
                schema_ok: false,
                sqlite_broken: false,
            }),
            path: dc.path.clone(),
            mode: "readonly".to_string(),
            max_size: DEFAULT_LRU_SIZE,
        };
        // mem is empty; sqlite should serve the hit
        assert_eq!(ro.get("n", "k", None), Some(serde_json::json!("v")));
        // readonly must not write
        ro.set("n", "k2", serde_json::json!("w"), 600.0, None);
        let ro2 = DataCache {
            inner: Mutex::new(Inner {
                mem: HashMap::new(),
                ticks: 0,
                schema_ok: false,
                sqlite_broken: false,
            }),
            path: dc.path.clone(),
            mode: "readwrite".to_string(),
            max_size: DEFAULT_LRU_SIZE,
        };
        assert_eq!(ro2.get("n", "k2", None), None);
    }

    #[test]
    fn lru_eviction_after_max_size() {
        let dc = fresh_sized("lru", 3);
        for i in 0..5 {
            dc.set("n", &format!("k{i}"), serde_json::json!(i), 600.0, None);
        }
        assert!(dc.len() <= 3);
        assert_eq!(dc.get("n", "k4", None), Some(serde_json::json!(4)));
    }

    #[test]
    fn sqlite_persistence_across_instances() {
        let dc1 = fresh("persist");
        dc1.set("n", "u1:k", serde_json::json!({"x": 1}), 600.0, Some("u1"));
        let dc2 = DataCache {
            inner: Mutex::new(Inner {
                mem: HashMap::new(),
                ticks: 0,
                schema_ok: false,
                sqlite_broken: false,
            }),
            path: dc1.path.clone(),
            mode: "readwrite".to_string(),
            max_size: DEFAULT_LRU_SIZE,
        };
        assert_eq!(dc2.get("n", "u1:k", None), Some(serde_json::json!({"x": 1})));
    }
}
