
//! Resolve the BbRouter session cookie.
//!
//! Layered resolution (never prompts, never touches the OS keychain):
//!   1. NTULEARN_COOKIE env var
//!   2. cookie file at ~/.config/ntulearn-mcp/cookie (written by this server
//!      whenever a working cookie is observed)
//!   3. (optional, subagent-A) browser auto-read via the bundled Python helper
//!      if the original venv is discoverable — browser_cookie3 read only.
//!
//! A bare value is fine; callers strip/keep the `BbRouter=` prefix consistently.

use std::path::PathBuf;

pub const COOKIE_FILE_NAME: &str = "cookie";

fn env_cookie() -> Option<String> {
    std::env::var("NTULEARN_COOKIE").ok().filter(|v| !v.trim().is_empty())
}

fn cookie_file_path() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("ntulearn-mcp").join(COOKIE_FILE_NAME)
}

fn strip_prefix(v: &mut String) {
    if let Some(rest) = v.strip_prefix("BbRouter=") {
        *v = rest.to_string();
    }
}

/// First valid cookie available, in priority order. Returns None if none.
pub fn resolve_cookie() -> Option<String> {
    if let Some(mut v) = env_cookie() {
        strip_prefix(&mut v);
        if !v.is_empty() {
            return Some(v);
        }
    }
    if let Ok(mut v) = std::fs::read_to_string(cookie_file_path()) {
        v = v.trim().to_string();
        strip_prefix(&mut v);
        if !v.is_empty() {
            return Some(v);
        }
    }
    // TODO(subagent-A): browser auto-read helper here.
    None
}

/// Persist a working cookie for later runs (best-effort; ignores errors).
pub fn write_cookie(value: &str) {
    let mut v = value.to_string();
    strip_prefix(&mut v);
    if v.is_empty() {
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
        let mut v = "BbRouter=abc123".to_string();
        strip_prefix(&mut v);
        assert_eq!(v, "abc123");
    }
}
