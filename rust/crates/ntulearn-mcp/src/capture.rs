//! Browser-driven cookie capture via the Chrome DevTools Protocol (CDP).
//!
//! Used by `setup`, the `ntulearn_login` tool, and the server's self-healing
//! paths to give the user a "just log in once" experience on machines where
//! Firefox auto-read is unavailable (Chrome/Edge/Safari/Arc cookie stores are
//! encrypted and out of scope — we never read any real browser profile).
//!
//! Flow: launch a Chromium-family browser with a dedicated PERSISTENT capture
//! profile and a remote-debugging port, navigate to NTULearn, then poll the CDP
//! socket for a `BbRouter` cookie whose value validates against the live
//! Blackboard API.
//!
//! Friction fixes (these were the main causes of users dropping off):
//! * **No OS password dialogs.** A fresh Chromium profile normally asks the
//!   macOS Keychain ("Chrome Safe Storage") / GNOME Keyring / KWallet for its
//!   cookie-encryption key, which pops a password prompt on every capture. We
//!   launch with `--use-mock-keychain` + `--password-store=basic`, so the
//!   capture profile never touches the OS credential store.
//! * **Silent re-auth really is silent.** NTU SSO (Microsoft Entra) keeps its
//!   login state in *session* cookies, which Chromium discards when the capture
//!   window closes — so every re-capture used to demand the password again.
//!   Before closing we re-save those session cookies with a 30-day expiry in
//!   the (private, dedicated) capture profile. The IdP still enforces its own
//!   server-side session lifetime.
//! * **Headless first.** When the profile holds a live SSO session, re-capture
//!   runs in a headless browser — no window flashes. A visible window is only
//!   opened for interactive flows (`setup`, `ntulearn_login`) when a real login
//!   is needed.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use fs2::FileExt;
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
/// A silent SSO round-trip takes a few seconds; anything longer means the SSO
/// session is gone and a human has to log in. Kept below the ~60 s tool-call
/// timeout many MCP hosts enforce, so a 401 that triggers a silent re-login
/// still returns an answer in time.
const SILENT_TIMEOUT: Duration = Duration::from_secs(40);
/// In an Interactive capture the headless pass must fail even faster, so the
/// visible login window is already open before `ntulearn_login` hands back to
/// the agent with `waiting_for_user` (45 s).
const INTERACTIVE_SILENT_TIMEOUT: Duration = Duration::from_secs(25);
/// Lifetime given to SSO session cookies re-saved in the capture profile.
const PERSIST_SESSION_COOKIES_SECS: i64 = 30 * 24 * 60 * 60;

/// How a capture may interact with the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureMode {
    /// Headless only; never shows a window. Fails fast when the saved SSO
    /// session cannot re-authenticate by itself. Safe for background use.
    Silent,
    /// Tries `Silent` first (when the profile is known-good), then opens a
    /// visible window for the user to log in.
    Interactive,
}

/// Only one capture at a time per process: the background refresher, a 401
/// in a tool call and `ntulearn_login` must never open competing browsers on
/// the same profile (Chromium locks a profile to one instance).
static CAPTURE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Additionally, at most one *interactive* (visible-window) capture may run
/// per process. Without this, a burst of 401s while the user is away would
/// queue capture after capture on `CAPTURE_LOCK` and keep re-opening login
/// windows for hours.
static INTERACTIVE_IN_FLIGHT: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Guard claiming the single interactive-capture slot; releases it on drop.
pub struct InteractiveClaim;

impl Drop for InteractiveClaim {
    fn drop(&mut self) {
        INTERACTIVE_IN_FLIGHT.store(false, std::sync::atomic::Ordering::Release);
    }
}

/// Claim the single interactive-capture slot. `None` means a login window is
/// already open (or about to be) — reuse it instead of opening another.
pub fn try_claim_interactive() -> Option<InteractiveClaim> {
    INTERACTIVE_IN_FLIGHT
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::Acquire,
            std::sync::atomic::Ordering::Relaxed,
        )
        .ok()
        .map(|_| InteractiveClaim)
}

/// Hook invoked with every freshly captured cookie value (see
/// `set_install_hook`), so a background capture can hot-swap the value into a
/// long-running client without capture.rs knowing about it.
static INSTALL_HOOK: std::sync::OnceLock<Box<dyn Fn(String) + Send + Sync>> =
    std::sync::OnceLock::new();

/// Register the cookie-install hook (server startup). Called once; later
/// calls are ignored. The hook receives the raw validated `BbRouter` value.
pub fn set_install_hook(hook: Box<dyn Fn(String) + Send + Sync>) {
    let _ = INSTALL_HOOK.set(hook);
}

/// Persist a freshly captured cookie (config file for the next process) and
/// notify the live client (install hook, when running as a server).
fn deliver_cookie(value: &str) {
    crate::cookie::write_cookie(value);
    if let Some(hook) = INSTALL_HOOK.get() {
        hook(value.to_string());
    }
}

// ---------------------------------------------------------------------------
// Machine-wide capture coordination
//
// Everything above guards a single process. But the user may run several
// ntulearn-mcp processes at once (two MCP hosts, or the CLI next to the
// server) — all sharing ONE capture profile. Concurrent captures then fight
// over the profile's singleton lock, hand off to already-running browsers,
// and cascade through every installed Chromium, opening window after window.
// The pieces below make capture single-flight MACHINE-WIDE.
// ---------------------------------------------------------------------------

/// Backoff marker: the saved SSO session is known dead, so silent headless
/// sign-ins cannot succeed until the next interactive login. Written after a
/// silent pass times out; TTL is generous because Microsoft Entra server-side
/// sessions last hours — retrying sooner would just launch browsers that
/// always end in the same dead end.
const SILENT_FUTILE_MARKER: &str = "silent-futile-until";
const SILENT_FUTILE_SECS: i64 = 6 * 60 * 60;

/// Backoff marker: a background-spawned login window timed out unattended.
/// Do not keep throwing windows at an away user; they can still open one
/// themselves at any time via `ntulearn_login` / `setup`.
const INTERACTIVE_COOLDOWN_MARKER: &str = "interactive-cooldown-until";
const INTERACTIVE_COOLDOWN_SECS: i64 = 30 * 60;

const CAPTURE_LOCK_FILE: &str = "capture.lock";
const CAPTURE_STATE_FILE: &str = "capture.state";

fn marker_file(base: &Path, name: &str) -> PathBuf {
    base.join(name)
}

fn read_marker(base: &Path, name: &str) -> Option<i64> {
    std::fs::read_to_string(marker_file(base, name))
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn write_marker(base: &Path, name: &str, until_epoch: i64) {
    let p = marker_file(base, name);
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(p, until_epoch.to_string());
}

fn clear_marker(base: &Path, name: &str) {
    let _ = std::fs::remove_file(marker_file(base, name));
}

/// True when the saved SSO session is known dead and further silent sign-in
/// attempts are pointless until the next interactive login. A marker file, so
/// every ntulearn-mcp process on the machine backs off together.
pub fn silent_capture_futile() -> bool {
    read_marker(&config_base(), SILENT_FUTILE_MARKER).is_some_and(|t| t > now_epoch())
}

/// True right after a background login window timed out without the user
/// logging in — suppress further auto-spawned windows.
pub fn interactive_cooldown_active() -> bool {
    read_marker(&config_base(), INTERACTIVE_COOLDOWN_MARKER).is_some_and(|t| t > now_epoch())
}

fn mark_silent_futile() {
    write_marker(
        &config_base(),
        SILENT_FUTILE_MARKER,
        now_epoch() + SILENT_FUTILE_SECS,
    );
}

fn mark_interactive_cooldown() {
    write_marker(
        &config_base(),
        INTERACTIVE_COOLDOWN_MARKER,
        now_epoch() + INTERACTIVE_COOLDOWN_SECS,
    );
}

/// Called on every successful capture: the session is alive again, so every
/// backoff marker is obsolete.
fn clear_backoff_markers() {
    let base = config_base();
    clear_marker(&base, SILENT_FUTILE_MARKER);
    clear_marker(&base, INTERACTIVE_COOLDOWN_MARKER);
}

/// Kernel-held exclusive lock (flock / LockFileEx) making capture
/// single-flight machine-wide: only ONE ntulearn-mcp process may drive the
/// shared capture profile at a time. Auto-released by the OS if the process
/// dies, so a killed server never wedges capture forever.
struct CrossProcessLock {
    _file: std::fs::File,
}

impl Drop for CrossProcessLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(capture_state_path());
    }
}

fn capture_lock_path() -> PathBuf {
    config_base().join(CAPTURE_LOCK_FILE)
}

fn capture_state_path() -> PathBuf {
    config_base().join(CAPTURE_STATE_FILE)
}

fn try_capture_lock() -> Option<CrossProcessLock> {
    let path = capture_lock_path();
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .ok()?;
    file.try_lock_exclusive().ok()?;
    Some(CrossProcessLock { _file: file })
}

/// What the current lock holder is doing (best-effort; written after taking
/// the lock, read only by processes that failed to take it).
fn write_capture_state(mode: CaptureMode) {
    let kind = if mode == CaptureMode::Silent {
        "silent"
    } else {
        "interactive"
    };
    let _ = std::fs::write(
        capture_state_path(),
        format!("{kind}\n{}", std::process::id()),
    );
}

fn capture_state_is_interactive() -> bool {
    std::fs::read_to_string(capture_state_path())
        .map(|s| s.starts_with("interactive"))
        .unwrap_or(false)
}

/// Acquire the machine-wide capture lock. Silent captures fail fast (a
/// background task must never pile up waiting); an interactive capture waits
/// out a concurrent silent one (≤ ~40 s), but never opens a SECOND login
/// window while another process has one open.
async fn acquire_capture_lock(mode: CaptureMode) -> Result<CrossProcessLock, String> {
    if let Some(l) = try_capture_lock() {
        return Ok(l);
    }
    let window_open = || {
        "a NTULearn login window opened by another ntulearn-mcp process is \
         already active — finish logging in there"
            .to_string()
    };
    if capture_state_is_interactive() {
        return Err(window_open());
    }
    if mode == CaptureMode::Silent {
        return Err("another ntulearn-mcp process is already signing in".into());
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if let Some(l) = try_capture_lock() {
            return Ok(l);
        }
        if capture_state_is_interactive() {
            return Err(window_open());
        }
        if std::time::Instant::now() >= deadline {
            return Err(
                "another ntulearn-mcp process is still signing in; retry in a moment".into(),
            );
        }
    }
}

/// Kill a capture browser orphaned by a previous process (e.g. the MCP host
/// was killed while a login window was open). The orphan keeps the capture
/// profile's singleton lock, so every later capture would hand off to it and
/// cascade through other installed browsers — the "it keeps opening a bunch
/// of browser instances" failure mode.
///
/// Safety: only runs while WE hold the machine-wide capture lock, so anything
/// found is by definition an orphan; and it only kills a process whose
/// command line names OUR capture profile, never a reused PID or the user's
/// own browser.
async fn reap_orphan_capture_browser() {
    #[cfg(not(target_os = "windows"))]
    {
        let lock = capture_profile_dir().join("SingletonLock");
        let Ok(target) = std::fs::read_link(&lock) else {
            return; // no singleton lock — nothing is holding the profile
        };
        let t = target.to_string_lossy();
        let Some(pid) = t.rsplit('-').next().and_then(|s| s.parse::<u32>().ok()) else {
            return;
        };
        if pid == 0 || pid == std::process::id() {
            return;
        }
        // Chromium singleton targets are "<hostname>-<pid>".
        let Some(cmdline) = process_command_line(pid) else {
            return; // not running (or unknown) — stale lock, nothing to kill
        };
        if !cmdline.contains("capture-profile") {
            return; // PID was reused by an unrelated process — hands off
        }
        tracing::info!("reaping orphaned capture browser (pid {pid})");
        let _ = std::process::Command::new("kill")
            .arg(pid.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output();
        for _ in 0..40 {
            if process_command_line(pid).is_none() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        tracing::warn!("orphaned capture browser (pid {pid}) did not exit");
    }
    #[cfg(target_os = "windows")]
    {
        // Windows Chromium uses a named-object singleton, not a readable lock
        // file; orphan reaping is skipped there.
        let _ = ();
    }
}

/// Full command line of a pid, or None when it is not running / cannot be
/// determined. POSIX only.
#[cfg(not(target_os = "windows"))]
fn process_command_line(pid: u32) -> Option<String> {
    let out = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Start a background interactive capture after a failed request (401
/// self-heal). Returns `true` when a login window is open or opening —
/// either just started, or one was already running.
///
/// Only fires when the capture profile is known-good (the user has logged in
/// through this app before): a fresh install must never see a browser window
/// it did not ask for.
pub fn spawn_interactive_background(base: String) -> bool {
    if !capture_profile_ready() || !browser_available() {
        return false;
    }
    // A previous background window already timed out unattended — the user is
    // away. Stop auto-opening windows; they can open one themselves at any
    // time via ntulearn_login / setup (which never check this cooldown).
    if interactive_cooldown_active() {
        return false;
    }
    let Some(claim) = try_claim_interactive() else {
        return true; // a login window is already open — reuse it
    };
    tokio::spawn(async move {
        let _claim = claim;
        match capture_cookie(&base, CaptureMode::Interactive, true).await {
            Ok((_val, browser)) => {
                // deliver_cookie already ran inside capture_cookie.
                tracing::info!("background login window: signed in via {browser}");
            }
            Err(e) => {
                tracing::warn!("background login window capture failed: {e}");
                // Nobody logged in. Cool down instead of re-opening windows
                // for hours (each retry costs a 15-min window).
                mark_interactive_cooldown();
            }
        }
    });
    true
}

fn config_base() -> PathBuf {
    dirs::config_dir().unwrap_or_else(std::env::temp_dir).join("ntulearn-mcp")
}

/// Persistent capture profile directory (kept between runs).
///
/// `-v2`: profiles created before the mock-keychain switch have cookies
/// encrypted with the real OS keychain key, which the mock keychain cannot
/// decrypt; starting clean avoids a half-broken profile.
fn capture_profile_dir() -> PathBuf {
    config_base().join("capture-profile-v2")
}

/// Marker written after the first successful capture. Silent / background
/// capture only runs once the profile is known-good, so a fresh install never
/// spawns a browser nobody asked for.
fn capture_ok_marker() -> PathBuf {
    config_base().join("capture-profile-v2-ok")
}

/// True once a capture has succeeded at least once (profile is known-good).
pub fn capture_profile_ready() -> bool {
    capture_ok_marker().is_file()
}

/// Ordered candidate Chromium browser executables to try, keyed by label.
fn browser_candidates() -> Vec<(String, PathBuf)> {
    let mut v: Vec<(String, PathBuf)> = Vec::new();
    #[cfg(target_os = "macos")]
    {
        let mut roots = vec![PathBuf::from("/Applications")];
        if let Some(home) = dirs::home_dir() {
            // Drag-installed without admin rights.
            roots.push(home.join("Applications"));
        }
        for root in roots {
            v.extend([
                ("Chrome", "Google Chrome.app/Contents/MacOS/Google Chrome"),
                ("Arc", "Arc.app/Contents/MacOS/Arc"),
                ("Brave", "Brave Browser.app/Contents/MacOS/Brave Browser"),
                ("Chromium", "Chromium.app/Contents/MacOS/Chromium"),
                ("Microsoft Edge", "Microsoft Edge.app/Contents/MacOS/Microsoft Edge"),
                ("Vivaldi", "Vivaldi.app/Contents/MacOS/Vivaldi"),
                ("Opera", "Opera.app/Contents/MacOS/Opera"),
            ].into_iter().map(|(l, p)| (l.to_string(), root.join(p))));
        }
    }
    #[cfg(target_os = "linux")]
    v.extend([
        ("google-chrome", "/usr/bin/google-chrome"),
        ("google-chrome-stable", "/usr/bin/google-chrome-stable"),
        ("chromium", "/usr/bin/chromium"),
        ("chromium-browser", "/usr/bin/chromium-browser"),
        ("chromium snap", "/snap/bin/chromium"),
        ("brave", "/usr/bin/brave-browser"),
        ("microsoft-edge", "/usr/bin/microsoft-edge"),
        ("vivaldi", "/usr/bin/vivaldi"),
    ].into_iter().map(|(l, p)| (l.to_string(), PathBuf::from(p))));
    #[cfg(target_os = "windows")]
    {
        // Edge first among system installs: it ships with every Windows 10/11
        // machine, so capture works even for Firefox-only users.
        if let Ok(p) = std::env::var("PROGRAMFILES") {
            v.push(("Chrome".to_string(), PathBuf::from(&p).join("Google\\Chrome\\Application\\chrome.exe")));
            v.push(("Edge".to_string(), PathBuf::from(&p).join("Microsoft\\Edge\\Application\\msedge.exe")));
            v.push(("Brave".to_string(), PathBuf::from(&p).join("BraveSoftware\\Brave-Browser\\Application\\brave.exe")));
        }
        if let Ok(p) = std::env::var("PROGRAMFILES(X86)") {
            v.push(("Chrome x86".to_string(), PathBuf::from(&p).join("Google\\Chrome\\Application\\chrome.exe")));
            v.push(("Edge x86".to_string(), PathBuf::from(&p).join("Microsoft\\Edge\\Application\\msedge.exe")));
            v.push(("Brave x86".to_string(), PathBuf::from(&p).join("BraveSoftware\\Brave-Browser\\Application\\brave.exe")));
        }
        // Per-user installs (the default for Chrome/Brave on Windows).
        if let Ok(p) = std::env::var("LOCALAPPDATA") {
            v.push(("Chrome per-user".to_string(), PathBuf::from(&p).join("Google\\Chrome\\Application\\chrome.exe")));
            v.push(("Brave per-user".to_string(), PathBuf::from(&p).join("BraveSoftware\\Brave-Browser\\Application\\brave.exe")));
        }
    }
    // An explicit override wins (portable installs, unusual paths).
    if let Ok(p) = std::env::var("NTULEARN_BROWSER") {
        if !p.trim().is_empty() {
            v.insert(0, ("NTULEARN_BROWSER".to_string(), PathBuf::from(p.trim())));
        }
    }
    let mut seen = std::collections::HashSet::new();
    v.into_iter()
        .filter(|(_, p)| p.exists() && seen.insert(p.clone()))
        .collect()
}

/// True when at least one drivable browser is installed.
pub fn browser_available() -> bool {
    !browser_candidates().is_empty()
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
    /// False when the launched binary handed the URL off to (or re-used) an
    /// already-running browser that is NOT using our capture profile. Such a
    /// process belongs to the user and must never be killed or closed — see
    /// `profile_is_ours`.
    kill_on_drop: bool,
}

impl Drop for Launched {
    fn drop(&mut self) {
        // Terminate the capture browser — but only when the process is really
        // our own capture instance. The profile is deliberately KEPT: it holds
        // the NTU SSO session, which makes the next capture a silent re-auth.
        if self.kill_on_drop {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// True when the browser actually used our capture profile (these markers only
/// appear once a profile has been initialised). Used to decide whether the
/// process we launched is disposable.
fn profile_is_ours(profile: &Path) -> bool {
    ["Local State", "Default", "First Run"]
        .iter()
        .any(|m| profile.join(m).exists())
}

/// Command-line switches for the capture browser.
fn launch_args(profile: &Path, port: u16, headless: bool) -> Vec<String> {
    let mut args = vec![
        format!("--user-data-dir={}", profile.display()),
        format!("--remote-debugging-port={port}"),
        "--remote-allow-origins=*".to_string(),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
        // Never ask the OS credential store for the profile's cookie key:
        // macOS Keychain prompt / GNOME Keyring / KWallet unlock dialogs.
        "--use-mock-keychain".to_string(),
        "--password-store=basic".to_string(),
        "--disable-sync".to_string(),
    ];
    if headless {
        args.push("--headless=new".to_string());
    } else {
        args.push("--window-position=120,120".to_string());
        args.push("--window-size=1100,860".to_string());
        args.push("--new-window".to_string());
    }
    args.push("about:blank".to_string());
    args
}

/// Wait until the DevTools HTTP endpoint answers `GET /json/version`, or the
/// child exits (a single-instance handoff to an already-running browser).
async fn wait_devtools_up(port: u16, child: &mut Child) -> Option<Value> {
    let deadline = std::time::Instant::now() + DEVTOOLS_UP_TIMEOUT;
    while std::time::Instant::now() < deadline {
        if let Some(v) = http_get_json(port, "/json/version").await {
            return Some(v);
        }
        if let Ok(Some(_)) = child.try_wait() {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    None
}

async fn http_get_json(port: u16, path: &str) -> Option<Value> {
    let url = format!("http://127.0.0.1:{port}{path}");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(4))
        .no_proxy()
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
    targets
        .as_array()?
        .iter()
        .filter(|t| t.get("type").and_then(Value::as_str) == Some("page"))
        .find_map(|t| t.get("webSocketDebuggerUrl").and_then(Value::as_str))
        .map(str::to_string)
}

/// One CDP call: send `{"id": n, "method": m, "params": p}` and return the
/// matching response `result`. Ignores events and non-matching ids.
async fn cdp_call(ws: &mut WsStream, msg_id: u32, method: &str, params: Value) -> Result<Value, String> {
    let msg = serde_json::json!({ "id": msg_id, "method": method, "params": params });
    ws.send(Message::Text(msg.to_string().into()))
        .await
        .map_err(|e| format!("CDP send ({method}): {e}"))?;
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        if std::time::Instant::now() > deadline {
            return Err(format!("CDP timeout waiting for {method}"));
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

/// Monotonic CDP message ids for one websocket session.
struct Cdp {
    ws: WsStream,
    next_id: u32,
}

impl Cdp {
    async fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next_id += 1;
        cdp_call(&mut self.ws, self.next_id, method, params).await
    }

    /// All cookies currently in the browser via `Network.getAllCookies`.
    async fn all_cookies(&mut self) -> Vec<Value> {
        self.call("Network.getAllCookies", Value::Null)
            .await
            .ok()
            .and_then(|r| r.get("cookies").and_then(Value::as_array).cloned())
            .unwrap_or_default()
    }
}

/// Extract a `BbRouter` cookie value from the cookie list.
fn find_bbrouter(cookies: &[Value]) -> Option<String> {
    cookies
        .iter()
        .filter(|c| c.get("name").and_then(Value::as_str) == Some("BbRouter"))
        .filter_map(|c| c.get("value").and_then(Value::as_str))
        .find(|v| v.starts_with("expires:"))
        .map(str::to_string)
}

/// `Network.setCookies` params that re-save every *session* cookie (no expiry)
/// with an absolute expiry, so the SSO login survives the capture browser
/// closing. Returns None when there is nothing to convert.
fn persist_session_cookies_params(cookies: &[Value], now_epoch: i64) -> Option<Value> {
    let expires = (now_epoch + PERSIST_SESSION_COOKIES_SECS) as f64;
    let converted: Vec<Value> = cookies
        .iter()
        .filter(|c| c.get("session").and_then(Value::as_bool) == Some(true))
        .filter_map(|c| {
            let name = c.get("name")?.as_str()?;
            let value = c.get("value")?.as_str()?;
            let domain = c.get("domain")?.as_str()?;
            let mut out = serde_json::json!({
                "name": name,
                "value": value,
                "domain": domain,
                "path": c.get("path").and_then(Value::as_str).unwrap_or("/"),
                "secure": c.get("secure").and_then(Value::as_bool).unwrap_or(false),
                "httpOnly": c.get("httpOnly").and_then(Value::as_bool).unwrap_or(false),
                "expires": expires,
            });
            if let Some(ss) = c.get("sameSite").and_then(Value::as_str) {
                out["sameSite"] = Value::String(ss.to_string());
            }
            Some(out)
        })
        .collect();
    if converted.is_empty() {
        None
    } else {
        Some(serde_json::json!({ "cookies": converted }))
    }
}

fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The user's default web browser, as an OS identifier.
/// macOS: LaunchServices https handler (e.g. "company.thebrowser.browser").
/// Linux: xdg-settings desktop file (e.g. "google-chrome.desktop").
/// Returns None when unknown or unsupported on this platform.
fn default_browser_id() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let home = dirs::home_dir()?;
        let plist = home.join(
            "Library/Preferences/com.apple.LaunchServices/com.apple.launchservices.secure.plist",
        );
        let out = std::process::Command::new("plutil")
            .args(["-convert", "json", "-o", "-"])
            .arg(&plist)
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let v: Value = serde_json::from_slice(&out.stdout).ok()?;
        for h in v.get("LSHandlers")?.as_array()? {
            if h.get("LSHandlerURLScheme").and_then(Value::as_str) == Some("https") {
                return h
                    .get("LSHandlerRoleAll")
                    .and_then(Value::as_str)
                    .map(|s| s.to_lowercase());
            }
        }
        None
    }
    #[cfg(target_os = "linux")]
    {
        let out = std::process::Command::new("xdg-settings")
            .args(["get", "default-web-browser"])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&out.stdout).trim().to_lowercase())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    None
}

/// Map a default-browser identifier to a keyword matching our candidate
/// labels. Only Chromium-family browsers can be driven over CDP; Safari and
/// Firefox return None and the candidates keep their default order.
fn default_browser_keyword(id: &str) -> Option<&'static str> {
    let id = id.to_lowercase();
    if id.contains("thebrowser") || id.contains("arc") {
        Some("arc")
    } else if id.contains("brave") {
        Some("brave")
    } else if id.contains("edge") {
        Some("edge")
    } else if id.contains("vivaldi") {
        Some("vivaldi")
    } else if id.contains("opera") {
        Some("opera")
    } else if id.contains("chromium") {
        Some("chromium")
    } else if id.contains("chrome") {
        Some("chrome")
    } else {
        None
    }
}

/// Candidate browsers with the user's DEFAULT browser first (when it is a
/// Chromium-family browser we can drive), then the usual order as fallback.
fn ordered_candidates() -> Vec<(String, PathBuf)> {
    let mut candidates = browser_candidates();
    if let Some(kw) = default_browser_id().as_deref().and_then(default_browser_keyword) {
        if let Some(pos) = candidates
            .iter()
            .position(|(label, _)| label.to_lowercase().contains(kw))
        {
            let preferred = candidates.remove(pos);
            candidates.insert(0, preferred);
        }
    }
    candidates
}

/// Run the capture flow. Returns a cookie value that already validated against
/// the live API (`users/me` 200), plus the browser label used.
///
/// Browsers are tried in order, with the user's DEFAULT browser first (when it
/// is a Chromium-family browser we can drive). If one cannot be driven — for
/// example a single-instance handoff to an already-running browser, which never
/// opens a debugging port — the next candidate is tried.
///
/// `quiet`: suppress stdout progress lines (used when called from the
/// long-running MCP server, where stdout is the protocol channel).
pub async fn capture_cookie(base: &str, mode: CaptureMode, quiet: bool) -> Result<(String, String), String> {
    let _guard = CAPTURE_LOCK.lock().await;

    // A recent silent sign-in already proved the saved session is dead; do
    // not launch another headless browser to learn it again.
    if mode == CaptureMode::Silent && silent_capture_futile() {
        return Err(
            "saved NTU session is known expired — sign in via ntulearn_login (opens a window)".into(),
        );
    }

    // Single-flight machine-wide: concurrent captures from other processes
    // (another MCP host, the CLI) would fight over this profile and cascade
    // through every installed browser.
    let _cross = acquire_capture_lock(mode).await?;
    write_capture_state(mode);
    reap_orphan_capture_browser().await;

    let candidates = ordered_candidates();
    if candidates.is_empty() {
        return Err("no supported browser found (need Chrome, Edge, Brave, Arc, Vivaldi, Opera \
                    or Chromium; or set NTULEARN_BROWSER to a Chromium-based browser binary)"
            .into());
    }
    let profile = capture_profile_dir();
    std::fs::create_dir_all(&profile)
        .map_err(|e| format!("cannot create capture profile {}: {e}", profile.display()))?;

    // When the session is known dead, skip the silent pass entirely and open
    // the window straight away.
    let sso_saved = capture_profile_ready() && !silent_capture_futile();
    if mode == CaptureMode::Silent && !capture_profile_ready() {
        return Err("no saved NTU login yet — run `ntulearn-mcp setup` (or the ntulearn_login tool) once".into());
    }
    // Pass 1: headless, riding the saved SSO session. No window, no prompt.
    // In Interactive mode this pass gets a shorter budget so the visible
    // window (pass 2) opens before `ntulearn_login` returns `waiting_for_user`.
    let silent_budget = if mode == CaptureMode::Silent {
        SILENT_TIMEOUT
    } else {
        INTERACTIVE_SILENT_TIMEOUT
    };
    let mut last_err = "no supported browser could be driven".to_string();
    if sso_saved {
        if !quiet {
            println!("Signing in silently with your saved NTU session...");
        }
        for (label, bin) in &candidates {
            match capture_with_browser(label, bin, &profile, base, true, quiet, silent_budget).await {
                Ok(val) => {
                    clear_backoff_markers();
                    deliver_cookie(&val);
                    return Ok((val, label.clone()));
                }
                Err(e) => {
                    tracing::info!("silent capture via {label} failed: {e}");
                    last_err = e;
                    // Only a launch failure is worth retrying with another
                    // browser; a timeout means the SSO session itself expired.
                    if !e_is_launch_failure(&last_err) {
                        // The SSO session is dead: remember it so no process
                        // keeps launching headless browsers that cannot
                        // succeed until a human logs in.
                        if last_err.contains("did not sign in by itself") {
                            mark_silent_futile();
                        }
                        break;
                    }
                }
            }
        }
        if mode == CaptureMode::Silent {
            return Err(format!("silent sign-in failed ({last_err}); the saved NTU session has likely expired"));
        }
        if !quiet {
            println!("Saved session has expired — opening a login window.\n");
        }
    }

    // Pass 2: a visible window for the user to log in.
    if !quiet {
        println!("Log in with your NTU account in the browser window that opens.");
        println!("Tip: answer \"Yes\" to \"Stay signed in?\" — future sign-ins then happen");
        println!("automatically in the background, with no window and no password.");
        println!();
    }
    for (label, bin) in &candidates {
        if !quiet {
            println!("Opening {label}...");
        }
        match capture_with_browser(label, bin, &profile, base, false, quiet, LOGIN_TIMEOUT).await {
            Ok(val) => {
                clear_backoff_markers();
                deliver_cookie(&val);
                return Ok((val, label.clone()));
            }
            Err(e) => {
                if !quiet {
                    println!("  {label} could not be used: {e}");
                }
                let launch_failure = e_is_launch_failure(&e);
                last_err = e;
                if !launch_failure {
                    break; // the user saw a window; don't open another browser
                }
            }
        }
    }
    Err(last_err)
}

const LAUNCH_FAILURE_PREFIX: &str = "launch: ";

fn e_is_launch_failure(e: &str) -> bool {
    e.starts_with(LAUNCH_FAILURE_PREFIX)
}

/// A single capture attempt against one browser binary. `Launched` (RAII)
/// guarantees the process is terminated on every exit path; the persistent
/// profile is always kept. Errors that mean "this browser could not be driven
/// at all" carry `LAUNCH_FAILURE_PREFIX`.
async fn capture_with_browser(
    label: &str,
    bin: &Path,
    profile: &Path,
    base: &str,
    headless: bool,
    quiet: bool,
    timeout: Duration,
) -> Result<String, String> {
    let launch_err = |m: String| format!("{LAUNCH_FAILURE_PREFIX}{m}");
    let port = free_port().ok_or_else(|| launch_err("no free port on 127.0.0.1".into()))?;

    let child = Command::new(bin)
        .args(launch_args(profile, port, headless))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| launch_err(format!("failed to launch {label}: {e}")))?;
    let mut launched = Launched { child, kill_on_drop: true };

    let version = wait_devtools_up(port, &mut launched.child).await.ok_or_else(|| {
        launch_err(format!("{label} did not open its debugging port (already running without debugging?)"))
    })?;

    // Treat the process as disposable only when it really used OUR profile. A
    // browser that ignored --user-data-dir and hooked into the user's own
    // session must never be closed or killed.
    launched.kill_on_drop = profile_is_ours(profile);

    let targets = http_get_json(port, "/json/list")
        .await
        .ok_or_else(|| launch_err("cannot list CDP targets".into()))?;
    let ws_url = find_page_ws(&targets).ok_or_else(|| launch_err("no page target found in CDP".into()))?;
    let (ws, _) = connect_async(&ws_url)
        .await
        .map_err(|e| launch_err(format!("CDP websocket connect: {e}")))?;
    let mut cdp = Cdp { ws, next_id: 1000 };
    let _ = cdp.call("Network.enable", Value::Null).await;
    let _ = cdp.call("Page.enable", Value::Null).await;

    // Headless Chromium advertises "HeadlessChrome" in its UA, which Microsoft
    // sign-in may treat as a bot. Present the regular UA instead.
    if let Some(ua) = version.get("User-Agent").and_then(Value::as_str) {
        if ua.contains("Headless") {
            let ua = ua.replace("HeadlessChrome", "Chrome");
            let _ = cdp.call("Network.setUserAgentOverride", serde_json::json!({ "userAgent": ua })).await;
        }
    }
    cdp.call("Page.navigate", serde_json::json!({ "url": base }))
        .await
        .map_err(launch_err)?;

    if !quiet && !headless {
        println!("A {label} window opened at {base}.");
        println!("It closes by itself once you are logged in.\n");
    }

    let deadline = std::time::Instant::now() + timeout;
    let mut checked: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut last_note = std::time::Instant::now();
    loop {
        if std::time::Instant::now() >= deadline {
            return Err(if headless {
                "saved session did not sign in by itself".into()
            } else {
                "timed out waiting for you to log in (15 min)".into()
            });
        }
        let cookies = cdp.all_cookies().await;
        if let Some(val) = find_bbrouter(&cookies) {
            if checked.insert(val.clone()) {
                match validates_live(base, &val).await {
                    Ok(true) => {
                        if !quiet {
                            println!("  -> logged in; session saved");
                        }
                        // Keep the SSO login across browser restarts so the
                        // next capture is silent.
                        if let Some(params) = persist_session_cookies_params(&cookies, now_epoch()) {
                            if let Err(e) = cdp.call("Network.setCookies", params).await {
                                tracing::warn!("could not persist SSO session cookies: {e}");
                            }
                        }
                        let _ = std::fs::write(capture_ok_marker(), "ok");
                        // Close gracefully (flushes the cookie store to disk)
                        // — ONLY our own capture instance.
                        if launched.kill_on_drop {
                            let _ = cdp.call("Browser.close", Value::Null).await;
                            for _ in 0..20 {
                                if let Ok(Some(_)) = launched.child.try_wait() {
                                    break;
                                }
                                tokio::time::sleep(Duration::from_millis(150)).await;
                            }
                        }
                        return Ok(val);
                    }
                    Ok(false) => {} // pre-login cookie; keep waiting
                    Err(e) => tracing::info!("live validation unavailable ({e}); continue waiting"),
                }
            }
        }
        if !quiet && !headless && last_note.elapsed() >= Duration::from_secs(30) {
            let remain = deadline.saturating_duration_since(std::time::Instant::now()).as_secs();
            println!("  ... waiting for you to finish logging in ({}m {}s left)", remain / 60, remain % 60);
            last_note = std::time::Instant::now();
        }
        tokio::time::sleep(Duration::from_millis(POLL_EVERY_MS)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactive_claim_is_single_flight() {
        let a = try_claim_interactive();
        assert!(a.is_some(), "first claim must succeed");
        assert!(
            try_claim_interactive().is_none(),
            "second claim must fail while a login window is in flight"
        );
        drop(a);
        assert!(
            try_claim_interactive().is_some(),
            "claim must be reusable after the capture finished"
        );
    }

    #[test]
    fn markers_roundtrip_and_expire() {
        let dir = std::env::temp_dir().join(format!("ntulearn-markers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // not set -> inactive
        assert!(read_marker(&dir, "x-until").is_none());

        // set in the future -> active
        write_marker(&dir, "x-until", now_epoch() + 60);
        assert_eq!(read_marker(&dir, "x-until"), Some(now_epoch() + 60));

        // expired -> inactive (reader treats it as gone)
        write_marker(&dir, "x-until", now_epoch() - 1);
        assert!(read_marker(&dir, "x-until").is_none_or(|t| t <= now_epoch()));

        // cleared
        clear_marker(&dir, "x-until");
        assert!(read_marker(&dir, "x-until").is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn singleton_lock_target_parses_to_pid() {
        // Chromium singleton targets look like "<hostname>-<pid>".
        let t = "karthiks-macbook.local-79563";
        let pid = t.rsplit('-').next().and_then(|s| s.parse::<u32>().ok());
        assert_eq!(pid, Some(79563));
        // A target without a trailing pid must not parse into one.
        let bad = "karthiks-macbook.local";
        assert!(bad.rsplit('-').next().and_then(|s| s.parse::<u32>().ok()).is_none());
    }

    fn ck(name: &str, value: &str) -> Value {
        serde_json::json!({ "name": name, "value": value, "domain": "ntulearn.ntu.edu.sg" })
    }

    #[test]
    fn default_browser_keyword_maps_known_browsers() {
        assert_eq!(default_browser_keyword("company.thebrowser.browser"), Some("arc"));
        assert_eq!(default_browser_keyword("com.google.chrome"), Some("chrome"));
        assert_eq!(default_browser_keyword("com.brave.browser"), Some("brave"));
        assert_eq!(default_browser_keyword("com.microsoft.edgemac"), Some("edge"));
        assert_eq!(default_browser_keyword("org.chromium.chromium"), Some("chromium"));
        assert_eq!(default_browser_keyword("com.vivaldi.vivaldi"), Some("vivaldi"));
        // Safari / Firefox cannot be driven over CDP: keep the default order.
        assert_eq!(default_browser_keyword("com.apple.safari"), None);
        assert_eq!(default_browser_keyword("org.mozilla.firefox"), None);
    }

    #[test]
    fn ordered_candidates_keeps_all_browsers() {
        let all = browser_candidates();
        let ordered = ordered_candidates();
        assert_eq!(all.len(), ordered.len());
        let mut a: Vec<_> = all.iter().map(|(l, _)| l.clone()).collect();
        let mut b: Vec<_> = ordered.iter().map(|(l, _)| l.clone()).collect();
        a.sort();
        b.sort();
        assert_eq!(a, b);
    }

    #[test]
    fn launch_args_never_touch_os_credential_store() {
        let p = PathBuf::from("/tmp/x");
        for headless in [true, false] {
            let args = launch_args(&p, 9222, headless);
            assert!(args.iter().any(|a| a == "--use-mock-keychain"));
            assert!(args.iter().any(|a| a == "--password-store=basic"));
            assert_eq!(args.iter().any(|a| a.starts_with("--headless")), headless);
        }
    }

    #[test]
    fn persists_only_session_cookies() {
        let cookies = vec![
            serde_json::json!({"name": "ESTSAUTH", "value": "a", "domain": ".login.microsoftonline.com",
                               "path": "/", "secure": true, "httpOnly": true, "sameSite": "None", "session": true}),
            serde_json::json!({"name": "BbRouter", "value": "expires:1,id:x", "domain": "ntulearn.ntu.edu.sg",
                               "path": "/", "session": false, "expires": 1.0}),
        ];
        let p = persist_session_cookies_params(&cookies, 1_000).unwrap();
        let arr = p["cookies"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"], "ESTSAUTH");
        assert_eq!(arr[0]["sameSite"], "None");
        assert_eq!(arr[0]["expires"].as_f64().unwrap() as i64, 1_000 + PERSIST_SESSION_COOKIES_SECS);
        assert!(persist_session_cookies_params(&cookies[1..], 1_000).is_none());
    }

    #[test]
    fn ignore_wrong_name_and_empty() {
        let cookies = vec![ck("session", "expires:1,id:x"), ck("BbRouter", "")];
        assert_eq!(find_bbrouter(&cookies), None);
    }

    #[test]
    fn picks_expires_prefixed_brouter() {
        let cookies = vec![ck("_ga", "x"), ck("BbRouter", "expires:1788335698,id:ABC")];
        assert_eq!(find_bbrouter(&cookies), Some("expires:1788335698,id:ABC".into()));
    }

    #[test]
    fn skips_bbrouter_without_expires_prefix() {
        // Blackboard always returns `expires:<epoch>,id:...`; anything else
        // (junk/error cookie) is not a real session and must be ignored.
        let cookies = vec![ck("BbRouter", "some:prefixed:value")];
        assert_eq!(find_bbrouter(&cookies), None);
    }

    /// Real-browser check of the two friction fixes: a headless capture
    /// browser with the mock keychain starts without an OS prompt, and a
    /// session cookie re-saved via `persist_session_cookies_params` survives a
    /// browser restart on the same profile. Run with
    /// `cargo test -p ntulearn-mcp -- --ignored session_cookie_survives_restart`.
    #[tokio::test]
    #[ignore]
    async fn session_cookie_survives_restart() {
        let (label, bin) = browser_candidates().into_iter().next().expect("a Chromium browser");
        let profile = std::env::temp_dir().join(format!("ntulearn-capture-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&profile);

        async fn session(bin: &Path, profile: &Path) -> (Launched, Cdp) {
            let port = free_port().unwrap();
            let child = Command::new(bin)
                .args(launch_args(profile, port, true))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let mut l = Launched { child, kill_on_drop: true };
            wait_devtools_up(port, &mut l.child).await.expect("devtools up");
            let targets = http_get_json(port, "/json/list").await.unwrap();
            let (ws, _) = connect_async(find_page_ws(&targets).unwrap()).await.unwrap();
            let mut cdp = Cdp { ws, next_id: 1 };
            cdp.call("Network.enable", Value::Null).await.unwrap();
            (l, cdp)
        }

        {
            let (mut l, mut cdp) = session(&bin, &profile).await;
            cdp.call("Network.setCookie", serde_json::json!({
                "name": "SSO", "value": "v1", "domain": "example.com", "path": "/", "secure": true
            })).await.unwrap();
            let cookies = cdp.all_cookies().await;
            assert!(cookies.iter().any(|c| c["name"] == "SSO" && c["session"] == true));
            let params = persist_session_cookies_params(&cookies, now_epoch()).unwrap();
            cdp.call("Network.setCookies", params).await.unwrap();
            let _ = cdp.call("Browser.close", Value::Null).await;
            let _ = l.child.wait();
        }
        {
            let (_l, mut cdp) = session(&bin, &profile).await;
            let cookies = cdp.all_cookies().await;
            assert!(
                cookies.iter().any(|c| c["name"] == "SSO" && c["value"] == "v1"),
                "{label}: session cookie did not survive restart: {cookies:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&profile);
    }
}
