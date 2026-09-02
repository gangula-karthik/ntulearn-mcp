//! Cookie setup / diagnostics subcommands (run outside the MCP stdio loop).
//!
//! * `setup`  — interactive one-time cookie acquisition: auto-detect a cookie
//!   already available (env / config file / Firefox), otherwise open NTULearn
//!   in the default browser, accept a pasted cookie, validate it live against
//!   the Blackboard API and persist it to the config file.
//! * `check`  — report the current cookie state (source, expiry, whether it
//!   validates live), without changing anything.
//! * `refresh`— on-demand: re-resolve the cookie from all sources, validate it,
//!   and persist a working value to the config file. Refresh never happens
//!   proactively; only on a 401 during a call (best-effort) or here on request.

use std::io::Write;
use std::time::Duration;

use crate::cookie;

const DEFAULT_BASE: &str = "https://ntulearn.ntu.edu.sg";

/// Live validity check: `GET {base}/learn/api/public/v1/users/me` with the given cookie.
/// Returns Ok(true) on 200, Ok(false) on 401, Err on network/other.
pub(crate) async fn validates_live(base: &str, value: &str) -> Result<bool, String> {
    let url = format!(
        "{}/learn/api/public/v1/users/me",
        base.trim_end_matches('/')
    );
    let client = reqwest::Client::builder()
        .user_agent("ntulearn-mcp-rust-setup/0.3")
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("failed to build HTTP client: {e}"))?;
    let resp = client
        .get(&url)
        .header("Cookie", format!("BbRouter={value}"))
        .send()
        .await
        .map_err(|e| format!("network error talking to {base}: {e}"))?;
    match resp.status().as_u16() {
        200 => Ok(true),
        401 => Ok(false),
        s => Err(format!("unexpected HTTP {s} from {url}")),
    }
}

/// Open the default browser to the NTU login page (best-effort).
fn open_login_page(base: &str) {
    let url = base.trim_end_matches('/').to_string();
    #[cfg(target_os = "macos")]
    let cmd = std::process::Command::new("open").arg(&url).spawn();
    #[cfg(target_os = "linux")]
    let cmd = std::process::Command::new("xdg-open").arg(&url).spawn();
    #[cfg(target_os = "windows")]
    let cmd = std::process::Command::new("cmd").args(["/c", "start", ""]).arg(&url).spawn();
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let cmd = Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "no opener"));
    match cmd {
        Ok(_) => println!("Opened {url} in your default browser."),
        Err(e) => println!("(could not auto-open a browser: {e}; open {url} manually)"),
    }
}

/// Read a single trimmed line from stdin.
fn read_line_trim(prompt: &str) -> Option<String> {
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let n = std::io::stdin().read_line(&mut line).ok()?;
    if n == 0 {
        return None; // EOF
    }
    let t = line.trim().to_string();
    if t.is_empty() { None } else { Some(t) }
}

/// Normalize a pasted value: strip `BbRouter=` and any leading/trailing quotes.
fn normalize_paste(v: &str) -> String {
    let mut t = v.trim().to_string();
    // Accept a full `Cookie: ...` header: take the trailing BbRouter=... part.
    if let Some(rest) = t.strip_prefix("Cookie:") {
        t = rest.trim().to_string();
    }
    if let Some(rest) = t.strip_prefix("BbRouter=") {
        t = rest.to_string();
    }
    // Drop any other cookies preceding BbRouter in a multi-cookie header.
    if let Some(rest) = t.find("BbRouter=") {
        t = t[rest + "BbRouter=".len()..].to_string();
    }
    // Truncate at a `;` — the cookie-name separator in a `Cookie:` header.
    // BbRouter values themselves use commas and never contain `;`.
    if let Some(sc) = t.find(';') {
        t.truncate(sc);
    }
    t = t.trim_matches(|ch| ch == '"' || ch == '\'').to_string();
    t.trim().to_string()
}

/// Persist a validated cookie to the config file (best-effort), print state.
fn persist(value: &str) {
    cookie::write_cookie(value);
    let path = cookie::cookie_file_path();
    println!("Saved to {}", path.display());
    if let Some(secs) = cookie::seconds_remaining(value) {
        let days = secs as f64 / 86400.0;
        println!(
            "Expires in {} ({}).",
            if days >= 1.0 {
                format!("{days:.1} days")
            } else {
                format!("{secs} seconds")
            },
            if secs <= 0 { "EXPIRED/NEW" } else { "valid" }
        );
    } else {
        println!("Expiry: not embedded in cookie value (session cookie).");
    }
}

/// `ntulearn-mcp setup`
pub async fn run_setup() -> i32 {
    // Ctrl+C during capture cancels the in-flight capture future, which drops
    // `Launched` (RAII) and cleans up the throwaway browser + temp profile.
    tokio::select! {
        code = run_setup_inner() => code,
        _ = tokio::signal::ctrl_c() => {
            eprintln!("\nInterrupted. Cancelling capture and cleaning up.\nYou can run `ntulearn-mcp setup` again anytime.");
            130
        }
    }
}

async fn run_setup_inner() -> i32 {
    let base = std::env::var("NTULEARN_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE.to_string());
    println!("NTULearn MCP — cookie setup");
    println!("Base URL: {base}\n");

    // Pass 1: is a cookie already available from env / file / Firefox?
    let (existing, source) = cookie::resolve_cookie_with_source();
    if let Some(val) = existing {
        print!("Found a cookie from {}... ", source.as_str());
        match validates_live(&base, &val).await {
            Ok(true) => {
                println!("it validates against the live API ✅");
                persist(&val);
                println!("\nYou are ready to use ntulearn-mcp (no changes needed).");
                return 0;
            }
            Ok(false) => {
                println!("but it is not accepted by the API (session expired).");
                println!("A fresh one will be needed.\n");
            }
            Err(e) => {
                println!("but live validation could not run ({e}).");
                println!("Keeping the existing value.\n");
                return 0;
            }
        }
    }

    // Pass 2: try the fully-automatic capture first (Chromium CDP). The user
    // only logs in; we never need a paste.
    println!("No valid working cookie yet.");
    if let Ok((val, browser)) = crate::capture::capture_cookie(&base).await {
        persist(&val);
        println!("Auto-captured a live session cookie via {browser} and saved it.");
        println!();
        println!("You are ready to use ntulearn-mcp (no changes needed).");
        return 0;
    }
    println!("Automatic capture was not available; falling back to manual paste.\n");

    // Pass 2b: walk the user through getting a fresh cookie by hand.
    println!("How to get one quickly:");
    println!("  1. If you use Firefox: log into {base} there, then re-run `setup`.")
    ;
    println!("  2. Otherwise, copy the BbRouter cookie value from your browser's");
    println!("     devtools (Application -> Cookies -> ntulearn.ntu.edu.sg -> BbRouter).");
    println!();
    open_login_page(&base);
    println!();
    let paste = read_line_trim("Paste your BbRouter cookie value (or the whole Cookie header) here: ");
    let Some(raw) = paste else {
        println!("No input. Set NTULEARN_COOKIE=<value> and re-run `setup`, or run \
                  the server directly.");
        return 1;
    };
    let cookie_value = normalize_paste(&raw);
    if !cookie_value.starts_with("expires:") {
        eprintln!("That does not look like a BbRouter value (must start with `expires:`).");
        return 1;
    }
    match validates_live(&base, &cookie_value).await {
        Ok(true) => {
            println!("Validated against the live API ✅");
            persist(&cookie_value);
            println!("\nDone. You can now run `ntulearn-mcp` (or `ntulearn-mcp refresh` later).");
            0
        }
        Ok(false) => {
            eprintln!("The pasted cookie was rejected by the API (401). It may be expired or invalid.");
            1
        }
        Err(e) => {
            eprintln!("Could not validate live: {e}. Saved anyway? No — nothing was stored.");
            println!("Run `check` once your network/credentials are ready, or paste again.");
            1
        }
    }
}

/// `ntulearn-mcp check`
pub async fn run_check() -> i32 {
    let base = std::env::var("NTULEARN_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE.to_string());
    println!("NTULearn MCP — cookie status");
    println!("Base URL: {base}\n  config file: {}", cookie::cookie_file_path().display());
    let (cookie, source) = cookie::resolve_cookie_with_source();
    match &cookie {
        None => {
            println!("No cookie found (environment, config file, or Firefox).");
            println!("Run `ntulearn-mcp setup` to acquire one quickly.");
            1
        }
        Some(val) => {
            println!("Cookie source : {}", source.as_str());
            if let Some(secs) = cookie::seconds_remaining(val) {
                let days = secs as f64 / 86400.0;
                let label = if secs <= 0 { "EXPIRED".to_string() } else { "valid".to_string() };
                println!(
                    "Expires in    : {} ({label})",
                    if days >= 1.0 { format!("{days:.1} days") } else { format!("{secs} seconds") }
                );
            } else {
                println!("Expires in    : unknown (session cookie)");
            }
            match validates_live(&base, val).await {
                Ok(true) => { println!("Live validity : OK (200)"); 0 }
                Ok(false) => { println!("Live validity : EXPIRED (401)"); 1 }
                Err(e) => { println!("Live validity : unknown ({e})"); 2 }
            }
        }
    }
}

/// `ntulearn-mcp refresh` — on-demand cookie refresh.
pub async fn run_refresh() -> i32 {
    let base = std::env::var("NTULEARN_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE.to_string());
    println!("NTULearn MCP — cookie refresh (on-demand)");
    let (cookie, source) = cookie::resolve_cookie_with_source();
    match cookie {
        None => {
            println!("No cookie available to refresh. Run `setup` first.");
            1
        }
        Some(val) => {
            println!("Re-resolved cookie from {} and checking it live...", source.as_str());
            match validates_live(&base, &val).await {
                Ok(true) => {
                    println!("Still valid — refreshing not needed.");
                    (return 0);
                }
                Ok(false) => {
                    println!("Current cookie is expired (401). No newer one available from \
                              env / config file / Firefox.");
                    println!("Run `ntulearn-mcp setup` to re-capture a fresh cookie (auto-login \
                              browser, or paste). Or set a fresh NTULEARN_COOKIE and run `refresh` again.");
                    1
                }
                Err(e) => {
                    println!("Could not reach {base}: {e}. Nothing changed.");
                    2
                }
            }
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paste_normalizes_plain_value() {
        assert_eq!(normalize_paste("expires:1788,id:ABC"), "expires:1788,id:ABC");
    }

    #[test]
    fn paste_normalizes_brouter_prefix() {
        assert_eq!(normalize_paste("BbRouter=expires:1788,id:ABC"), "expires:1788,id:ABC");
    }

    #[test]
    fn paste_normalizes_full_cookie_header() {
        // A whole `Cookie:` header pasted verbatim must still yield the BbRouter value.
        let hdr = "Cookie: bbsession=xxx; bb_session_id=yyy; BbRouter=expires:1788,id:ABC; foo=1";
        assert_eq!(normalize_paste(hdr), "expires:1788,id:ABC");
    }

    #[test]
    fn paste_strips_quotes() {
        assert_eq!(normalize_paste("\"expires:1788,id:ABC\""), "expires:1788,id:ABC");
    }
}
