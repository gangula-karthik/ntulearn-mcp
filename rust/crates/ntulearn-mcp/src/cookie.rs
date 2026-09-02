//! Resolve the BbRouter session cookie.
//!
//! Layered resolution (never prompts, never touches the OS keychain):
//!   1. NTULEARN_COOKIE env var
//!   2. cookie file at <config_dir>/ntulearn-mcp/cookie (written by this
//!      server whenever a working cookie is observed / re-resolved)
//!   3. Browser cookie DB read — a read-only scan of Firefox's `cookies.sqlite`.
//!      Firefox values are plaintext (no OS-keychain decryption needed), which
//!      is why it is the only browser store attempted here.
//!
//! Chrome/Edge/Safari auto-read is DEFERRED: their cookie values are encrypted
//! with keys that live in the OS keychain / DPAPI, and this server is under a
//! hard constraint to NEVER touch the OS keychain and never run `security
//! find-generic-password -g` / `security dump-keychain -d` (those pop blocking
//! macOS password dialogs). Set NTULEARN_COOKIE or write the cookie file for
//! those setups.

use std::path::PathBuf;

pub const COOKIE_FILE_NAME: &str = "cookie";
const COOKIE_HOST: &str = "ntulearn.ntu.edu.sg";
const COOKIE_NAME: &str = "BbRouter";

fn env_cookie() -> Option<String> {
    std::env::var("NTULEARN_COOKIE").ok().filter(|v| !v.trim().is_empty())
}

pub fn cookie_file_path() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("ntulearn-mcp").join(COOKIE_FILE_NAME)
}

/// Where a resolved cookie came from (for diagnostics).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CookieSource {
    Env,
    File,
    Firefox,
    None,
}

impl CookieSource {
    pub fn as_str(self) -> &'static str {
        match self {
            CookieSource::Env => "NTULEARN_COOKIE env var",
            CookieSource::File => "config file",
            CookieSource::Firefox => "Firefox cookies.sqlite",
            CookieSource::None => "none",
        }
    }
}

/// Parse the `expires:<epoch-seconds>` prefix of a BbRouter value.
/// Returns None if the prefix is absent or not a number (session cookie).
pub fn cookie_expiry(value: &str) -> Option<i64> {
    let v = value.trim();
    let rest = v.strip_prefix("expires:")?;
    let until_sep = rest.split(|ch: char| ch == ',' || ch.is_whitespace()).next().unwrap_or("");
    if until_sep.is_empty() || !until_sep.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    until_sep.parse::<i64>().ok()
}

/// User-visible seconds remaining until the cookie expires (None = unknown).
pub fn seconds_remaining(value: &str) -> Option<i64> {
    let exp = cookie_expiry(value)?;
    if exp <= 0 {
        return None;
    }
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
    Some(exp.saturating_sub(now))
}

/// Resolve the first valid cookie available, reporting which source won.
pub fn resolve_cookie_with_source() -> (Option<String>, CookieSource) {
    if let Some(mut v) = env_cookie() {
        v = v.trim().to_string();
        strip_prefix(&mut v);
        if is_valid_value(&v) {
            return (Some(v), CookieSource::Env);
        }
    }
    if let Some(v) = read_cookie_file() {
        return (Some(v), CookieSource::File);
    }
    if let Some(v) = browser_cookie() {
        return (Some(v), CookieSource::Firefox);
    }
    (None, CookieSource::None)
}

/// A valid BbRouter value starts with `expires:` (mirrors cookie.py
/// `_is_valid_bbrouter`); everything else is decrypt-to-garbage or junk.
fn is_valid_value(v: &str) -> bool {
    !v.trim().is_empty() && v.trim_start().starts_with("expires:")
}

fn strip_prefix(v: &mut String) {
    if let Some(rest) = v.strip_prefix("BbRouter=") {
        *v = rest.to_string();
    }
}

fn read_cookie_file() -> Option<String> {
    let content = std::fs::read_to_string(cookie_file_path()).ok()?;
    let mut v = content.trim().to_string();
    strip_prefix(&mut v);
    if is_valid_value(&v) {
        Some(v)
    } else {
        None
    }
}

/// Best-effort read of the BbRouter cookie from the local Firefox profile.
/// Values in cookies.sqlite are plaintext (unless a master password is set, in
/// which case the read simply fails and we fall through). Guarded by try/catch
/// so a locked/absent DB never breaks resolution.
fn browser_cookie() -> Option<String> {
    let profiles_dirs = firefox_profile_roots();
    for root in profiles_dirs {
        let entries = match std::fs::read_dir(&root) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let profile = entry.path();
            let db = profile.join("cookies.sqlite");
            if !db.is_file() {
                continue;
            }
            if let Some(value) = read_firefox_cookie(&db) {
                return Some(value);
            }
        }
    }
    None
}

fn firefox_profile_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = dirs::home_dir() {
        #[cfg(target_os = "macos")]
        roots.push(home.join("Library/Application Support/Firefox/Profiles"));
        #[cfg(not(target_os = "macos"))]
        roots.push(home.join(".mozilla/firefox"));
    }
    if let Some(data) = dirs::data_dir() {
        // Flatpak / snap layouts.
        let flatpak = data.join("var/app/org.mozilla.firefox/.mozilla/firefox");
        if flatpak.is_dir() {
            roots.push(flatpak);
        }
    }
    roots
}

fn read_firefox_cookie(db: &std::path::Path) -> Option<String> {
    use rusqlite::{Connection, OpenFlags, params};
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    let mut stmt = conn
        .prepare(
            "SELECT name, value FROM moz_cookies \
             WHERE host LIKE ?1 AND name = ?2 ORDER BY lastAccessed DESC",
        )
        .ok()?;
    let rows = stmt
        .query_map(params![format!("%{COOKIE_HOST}"), COOKIE_NAME], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .ok()?;
    for row in rows.flatten() {
        let (_name, value) = row;
        if is_valid_value(&value) {
            return Some(value);
        }
    }
    None
}

/// First valid cookie available, in priority order. Returns None if none.
pub fn resolve_cookie() -> Option<String> {
    resolve_cookie_with_source().0
}

/// Persist a working cookie for later runs (best-effort; ignores errors).
pub fn write_cookie(value: &str) {
    let mut v = value.trim().to_string();
    strip_prefix(&mut v);
    if !is_valid_value(&v) {
        return;
    }
    let p = cookie_file_path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(p, &v);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_prefix() {
        let mut v = "BbRouter=expires:123,id:abc".to_string();
        strip_prefix(&mut v);
        assert_eq!(v, "expires:123,id:abc");
        assert!(is_valid_value(&v));
        assert!(!is_valid_value("garbage"));
    }
    #[test]
    fn cookie_expiry_parsing() {
        assert_eq!(cookie_expiry("expires:9999999999,id:abc"), Some(9999999999));
        assert_eq!(cookie_expiry("expires:0"), Some(0));
        // session cookie / no numeric prefix
        assert_eq!(cookie_expiry("expires:,id:x"), None);
        assert_eq!(cookie_expiry("id:abc"), None);
        assert_eq!(cookie_expiry("expires:abc,id:x"), None);
    }

    #[test]
    fn seconds_remaining_sane() {
        // Far-future expiry -> large positive remaining.
        let fut = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 86400 * 30;
        let rem = seconds_remaining(&format!("expires:{fut},id:x")).unwrap();
        assert!(rem > 86_400 * 29, "expected ~30 days left, got {rem}");
        // expired
        let rem2 = seconds_remaining("expires:1000,id:x").unwrap();
        assert!(rem2 <= 0);
    }

}
