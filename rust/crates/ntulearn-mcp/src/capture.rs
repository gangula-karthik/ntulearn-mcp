//! Browser-driven cookie capture via the Chrome DevTools Protocol (CDP).
//!
//! Used by `setup` to give the user a "just log in" experience on machines
//! where Firefox auto-read is unavailable (Chrome/Edge/Safari/Arc cookie stores
//! are encrypted and out of scope — we never touch the OS keychain).
//!
//! Flow: launch a Chromium-family browser with a THROWAWAY profile and a
//! remote-debugging port, navigate to NTULearn, then poll the CDP socket for a
//! `BbRouter` cookie whose value validates against the live Blackboard API.
//! The user only has to log in in the window that opens; we never see
//! credentials and never touch any real browser profile or keychain.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use crate::setup::validates_live;

/// The websocket stream type tokio-tungstenite yields for plain (non-TLS) ws://
/// connections to the local DevTools endpoint.
type WsStream = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

const POLL_EVERY_MS: u64 = 1500;
const DEVTOOLS_UP_TIMEOUT: Duration = Duration::from_secs(30);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Ordered candidate Chromium browser executables to try, keyed by label.
fn browser_candidates() -> Vec<(String, PathBuf)> {
    let mut v: Vec<(String, PathBuf)> = Vec::new();
    #[cfg(target_os = "macos")]
    v.extend([
        ("Chrome", "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
        ("Arc", "/Applications/Arc.app/Contents/MacOS/Arc"),
        ("Brave", "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser"),
        ("Chromium", "/Applications/Chromium.app/Contents/MacOS/Chromium"),
        ("Microsoft Edge", "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge"),
    ].into_iter().map(|(l, p)| (l.to_string(), PathBuf::from(p))));
    #[cfg(target_os = "linux")]
    v.extend([
        ("google-chrome", "/usr/bin/google-chrome"),
        ("google-chrome-stable", "/usr/bin/google-chrome-stable"),
        ("chromium", "/usr/bin/chromium"),
        ("chromium-browser", "/usr/bin/chromium-browser"),
        ("brave", "/usr/bin/brave-browser"),
        ("microsoft-edge", "/usr/bin/microsoft-edge"),
    ].into_iter().map(|(l, p)| (l.to_string(), PathBuf::from(p))));
    #[cfg(target_os = "windows")]
    {
        // Program-Files installs (system-wide).
        if let Ok(p) = std::env::var("PROGRAMFILES") {
            v.push(("Chrome", PathBuf::from(p).join("Google\\Chrome\\Application\\chrome.exe")));
        }
        if let Ok(p) = std::env::var("PROGRAMFILES(X86)") {
            v.push(("Chrome x86", PathBuf::from(p).join("Google\\Chrome\\Application\\chrome.exe")));
            v.push(("Edge", PathBuf::from(p).join("Microsoft\\Edge\\Application\\msedge.exe")));
            v.push(("Brave", PathBuf::from(p).join("BraveSoftware\\Brave-Browser\\Application\\brave.exe")));
        }
        // Per-user installs (the default for Chrome/Brave on Windows).
        if let Ok(p) = std::env::var("LOCALAPPDATA") {
            v.push(("Chrome per-user", PathBuf::from(p).join("Google\\Chrome\\Application\\chrome.exe")));
            v.push(("Brave per-user", PathBuf::from(p).join("BraveSoftware\\Brave-Browser\\Application\\brave.exe")));
        }
    }
    v.into_iter().filter(|(_, p)| PathBuf::from(p).exists()).collect()
}

/// Pick a free TCP port on localhost.
fn free_port() -> Option<u16> {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .ok()
        .and_then(|l| l.local_addr().ok())
        .map(|a| a.port())
}

struct Launched {
    child: Child,
    port: u16,
    profile: PathBuf,
}

impl Drop for Launched {
    fn drop(&mut self) {
        // Terminate the throwaway browser and delete its profile.
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.profile);
    }
}

/// Wait until the DevTools HTTP endpoint answers `GET /json/version`.
async fn wait_devtools_up(port: u16) -> bool {
    let deadline = std::time::Instant::now() + DEVTOOLS_UP_TIMEOUT;
    while std::time::Instant::now() < deadline {
        if http_get_json(port, "/json/version").await.is_some() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    false
}

async fn http_get_json(port: u16, path: &str) -> Option<Value> {
    let url = format!("http://127.0.0.1:{port}{path}");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(4))
        .build()
        .ok()?;
    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json::<Value>().await.ok()
}

/// Find a page target's websocket URL from `/json/list`.
fn find_page_ws(targets: &Value) -> Option<String> {
    let arr = targets.as_array()?;
    for t in arr {
        if t.get("type").and_then(Value::as_str) == Some("page") {
            if let Some(ws) = t.get("webSocketDebuggerUrl").and_then(Value::as_str) {
                return Some(ws.to_string());
            }
        }
    }
    None
}

/// One CDP call: send `{"id": n, "method": m, "params": p}` and return the
/// matching response `result`. Ignores events and non-matching ids.
async fn cdp_call(
    ws: &mut WsStream,
    ws_url: &str,
    msg_id: u32,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let msg = serde_json::json!({ "id": msg_id, "method": method, "params": params });
    ws.send(Message::Text(msg.to_string().into()))
        .await
        .map_err(|e| format!("CDP send ({method}): {e}"))?;
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        if std::time::Instant::now() > deadline {
            return Err(format!("CDP timeout waiting for {method} (ws {ws_url})"));
        }
        match tokio::time::timeout(Duration::from_secs(5), ws.next()).await {
            Ok(Some(Ok(Message::Text(txt)))) => {
                let v: Value = serde_json::from_str(&txt).unwrap_or(Value::Null);
                if v.get("id").and_then(Value::as_u64) == Some(msg_id as u64) {
                    if let Some(err) = v.get("error") {
                        return Err(format!("CDP error for {method}: {err}"));
                    }
                    return Ok(v.get("result").cloned().unwrap_or(Value::Null));
                }
            }
            Ok(Some(Ok(Message::Ping(p)))) => {
                let _ = ws.send(Message::Pong(p)).await;
            }
            Ok(Some(Err(e))) => return Err(format!("CDP recv: {e}")),
            Ok(Some(Ok(_))) => {} // Binary/Close/other — ignore
            Ok(None) => return Err("CDP socket closed".to_string()),
            Err(_) => {} // timeout on this frame; keep looping
        }
    }
}

/// All cookies currently in the browser via `Network.getAllCookies`.
async fn all_cookies(
    ws: &mut WsStream,
    port: u16,
) -> Vec<Value> {
    let res = cdp_call(ws, &format!("ws://127.0.0.1:{port}/devtools"), 1000, "Network.getAllCookies", Value::Null)
        .await
        .unwrap_or(Value::Null);
    res.get("cookies")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Extract a `BbRouter` cookie value from the cookie list.
fn find_bbrouter(cookies: &[Value]) -> Option<String> {
    for c in cookies {
        let name = c.get("name").and_then(Value::as_str).unwrap_or("");
        if name != "BbRouter" {
            continue;
        }
        let value = c.get("value").and_then(Value::as_str).unwrap_or("").to_string();
        // Skip obviously-empty cookie jar entries.
        if !value.is_empty() && (value.starts_with("expires:") || value.contains("expires:")) {
            return Some(value);
        }
    }
    None
}

/// Run the whole capture flow. Returns a cookie value that already validated
/// against the live API (`[users/me]` 200).
pub async fn capture_cookie(base: &str) -> Result<(String, String), String> {
    let candidates = browser_candidates();
    let Some((label, bin)) = candidates.into_iter().next() else {
        return Err("no supported browser found (need Chrome, Arc, Brave, Edge, or Chromium)".into());
    };

    let port = free_port().ok_or("no free port on 127.0.0.1")?;
    let profile = std::env::temp_dir().join(format!("ntulearn-mcp-capture-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&profile);
    if std::fs::create_dir_all(&profile).is_err() {
        return Err(format!("cannot create temp profile {}", profile.display()));
    }

    println!(
        "Starting {label} with a throwaway profile on port {port}...\n"
    );
    let child = Command::new(bin)
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg(format!("--remote-debugging-port={port}"))
        .arg("--remote-allow-origins=*")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--window-position=120,120")
        .arg("--window-size=1280,900")
        .arg("--new-window")
        .arg(base)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to launch {label}: {e}"))?;

    let _launched = Launched { child, port, profile: profile.clone() };

    if !wait_devtools_up(port).await {
        return Err(format!("browser ({label}) did not open its debugging port {port}"));
    }

    let targets = http_get_json(port, "/json/list")
        .await
        .ok_or_else(|| "cannot list CDP targets".to_string())?;
    let ws_url = find_page_ws(&targets)
        .ok_or_else(|| "no page target found in CDP".to_string())?;

    let (mut ws, _) = connect_async(&ws_url)
        .await
        .map_err(|e| format!("CDP websocket connect: {e}"))?;
    let _ = cdp_call(&mut ws, &ws_url, 1001, "Network.enable", Value::Null).await;
    let _ = cdp_call(&mut ws, &ws_url, 1002, "Page.enable", Value::Null).await;

    println!("A browser window opened at {base}.");
    println!("Log in with your NTU account in THAT window (throwaway profile).");
    println!("I will detect the session cookie automatically and save it.\n");

    let deadline = std::time::Instant::now() + LOGIN_TIMEOUT;
    let mut checked: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut last_note = std::time::Instant::now();
    loop {
        if std::time::Instant::now() >= deadline {
            return Err("timed out waiting for you to log in (15 min)".into());
        }
        let cookies = all_cookies(&mut ws, port).await;
        if let Some(val) = find_bbrouter(&cookies) {
            if !checked.contains(&val) {
                checked.insert(val.clone());
                println!("  → found a BbRouter value; validating against the live API...");
                match validates_live(base, &val).await {
                    Ok(true) => {
                        println!("  → session cookie validated ✅");
                        let _ = cdp_call(&mut ws, &ws_url, 1003, "Browser.close", Value::Null).await;
                        return Ok((val, label.to_string()));
                    }
                    Ok(false) => {
                        println!("  → not the live session yet (probably the pre-login cookie); wait…");
                    }
                    Err(e) => {
                        println!("  → live validation unavailable ({e}); continue waiting");
                    }
                }
            }
        }
        if last_note.elapsed() >= Duration::from_secs(20) {
            let remain = deadline.saturating_duration_since(std::time::Instant::now()).as_secs();
            println!("  … still waiting for the logged-in session cookie ({}m {}s left, then falls back to paste)", remain / 60, remain % 60);
            last_note = std::time::Instant::now();
        }
        tokio::time::sleep(Duration::from_millis(POLL_EVERY_MS)).await;
    }
}

// Explicitly drop to silence an unused-variable warning when `launched` is
// only relied on for its Drop side effect at the end of the function.
#[allow(dead_code)]
fn _keep_launched(_: &Launched) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn ck(name: &str, value: &str) -> Value {
        serde_json::json!({ "name": name, "value": value, "domain": "ntulearn.ntu.edu.sg" })
    }

    #[test]
    fn ignore_wrong_name_and_empty() {
        let cookies = vec![ck("session", "expires:1,id:x"), ck("BbRouter", "")];
        assert_eq!(find_bbrouter(&cookies), None);
    }

    #[test]
    fn picks_expires_prefixed_brouter() {
        let cookies = vec![
            ck("_ga", "x"),
            ck("BbRouter", "expires:1788335698,id:ABC"),
        ];
        assert_eq!(find_bbrouter(&cookies), Some("expires:1788335698,id:ABC".into()));
    }

    #[test]
    fn skips_bbrouter_without_expires_prefix() {
        // Blackboard always returns `expires:<epoch>,id:...`; anything else
        // (junk/error cookie) is not a real session and must be ignored.
        let cookies = vec![ck("BbRouter", "some:prefixed:value")];
        assert_eq!(find_bbrouter(&cookies), None);
    }
}
