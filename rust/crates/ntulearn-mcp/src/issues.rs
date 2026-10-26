//! Opt-in GitHub issue auto-filing for unexpected errors, so bugs surface
//! with details attached instead of dying in a stderr nobody reads.
//!
//! Design rules:
//! * **Opt-in only.** Filing issues to a public repo is a consented behavior:
//!   the user is offered it during `setup` / `update` (see
//!   `update::maybe_offer_optins`) or opts in with `ntulearn-mcp issues on`.
//!   `NTULEARN_ISSUE_REPORTS=off` always wins.
//! * **Never operational spam.** Auth/session expiries, user-input errors and
//!   transient network failures self-heal or are the caller's fault — they are
//!   classified as expected and never filed. Only unexpected errors (parse
//!   failures, resolution dead-ends like "No download URL found") and panics
//!   are reported.
//! * **Once per signature per version.** Errors are normalized into a
//!   signature (IDs/emails/cookies redacted) and filed at most once per
//!   signature per binary version; a local marker file plus an open-issue
//!   title scan keep multiple machines from filing duplicates.
//! * **Never blocks, never breaks the server.** Recording is a counter bump;
//!   filing happens on a background task (10-min tick, needs >= 3
//!   occurrences). Panics bypass the threshold and file from a detached
//!   thread. Every failure is logged and swallowed.
//! * **Public-repo safe.** Bodies are sanitized: Blackboard content IDs,
//!   emails, cookie values and long tokens are redacted before anything
//!   leaves the machine.
//! * **Needs `gh`.** Issues are filed through the GitHub CLI (`gh issue
//!   create/list`) using the local gh auth; when gh is absent or unauthenticated
//!   the feature silently no-ops. Retarget with `NTULEARN_ISSUE_REPO`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::update::current_version;

/// Where issues are filed unless NTULEARN_ISSUE_REPO overrides.
const DEFAULT_REPO: &str = "gangula-karthik/ntulearn-mcp";
/// Occurrences of one signature (within a process) before an issue is filed.
const FILE_AFTER: u64 = 3;
/// How often the background reporter looks for reportable signatures.
const TICK: Duration = Duration::from_secs(10 * 60);
/// Upper bound for any gh subprocess.
const GH_TIMEOUT: Duration = Duration::from_secs(45);
/// Title/body/sample truncation.
const MAX_TITLE: usize = 120;
const MAX_BODY: usize = 4000;
const MAX_SAMPLE: usize = 300;

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
struct Entry {
    count: u64,
    first_seen: i64,
    last_seen: i64,
    /// Distinct sanitized messages, capped.
    samples: Vec<String>,
    /// Tools the error surfaced through, capped.
    tools: Vec<String>,
}

fn registry() -> &'static Mutex<HashMap<String, Entry>> {
    static REG: OnceLock<Mutex<HashMap<String, Entry>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Record a tool-call error. Cheap (lock + counter); call from every Err path.
/// `tool` is the short tool name ("download_file") or "panic".
pub fn record(tool: &str, message: &str) {
    if !enabled() || is_expected(message) {
        return;
    }
    let clean = sanitize(message);
    let sig = signature(&clean);
    let mut reg = registry().lock().unwrap();
    let e = reg.entry(sig).or_insert_with(|| Entry {
        first_seen: now_epoch(),
        ..Entry::default()
    });
    e.count += 1;
    e.last_seen = now_epoch();
    if !e.tools.iter().any(|t| t == tool) && e.tools.len() < 8 {
        e.tools.push(tool.to_string());
    }
    if !e.samples.contains(&clean) && e.samples.len() < 3 {
        e.samples.push(clean);
    }
}

/// Copy one registry entry (the panic hook files immediately, bypassing the
/// background task).
fn snapshot_entry(sig: &str) -> Option<Entry> {
    let reg = registry().lock().unwrap();
    reg.get(sig).cloned()
}

// ---------------------------------------------------------------------------
// Classification + sanitization
// ---------------------------------------------------------------------------

/// Errors that are part of normal operation (self-healing or the caller's
/// fault) and must never be filed.
fn is_expected(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    const EXPECTED: &[&str] = &[
        // auth / session lifecycle — self-healing by design
        "401", "unauthorized", "forbidden", "cookie", "session", "login", "logged in",
        "expired", "sso", "stay signed in", "waiting_for_user", "authentication",
        // caller mistakes — the agent will retry with better arguments
        "unknown tool", "invalid", "missing required", "must be", "expected one of",
        "argument", "response_format", "unknown field", "not provided", "empty",
        // transient infrastructure — not this codebase's bug
        "timeout", "timed out", "connection", "connect", "dns", "network", "resolve",
        "temporarily", "rate limit", "too many requests", "502", "503", "504",
        "interrupted", "reset by peer",
        // interactive capture lifecycle
        "capture", "browser", "devtools", "chrome", "backoff",
    ];
    EXPECTED.iter().any(|k| m.contains(k))
}

/// Redact anything that must not reach a public repo.
pub fn sanitize(message: &str) -> String {
    let mut s = redact_bb_router(message.trim());
    s = redact_bb_ids(&s);
    s = redact_xid(&s);
    s = redact_emails(&s);
    s = redact_long_tokens(&s);
    s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() > MAX_SAMPLE {
        let cut: String = s.chars().take(MAX_SAMPLE).collect();
        s = cut + "...";
    }
    s
}

/// `BbRouter=<value>` (quoted or bare) -> `BbRouter=<redacted>`.
fn redact_bb_router(s: &str) -> String {
    let Some(start) = s.find("BbRouter=") else { return s.to_string() };
    let val_start = start + "BbRouter=".len();
    let val_end = s[val_start..]
        .find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
        .map(|i| val_start + i)
        .unwrap_or(s.len());
    let mut out = String::with_capacity(s.len());
    out.push_str(&s[..val_start]);
    out.push_str("<redacted>");
    out.push_str(&s[val_end..]);
    out
}

/// Blackboard IDs `_1234567_1` -> `_ID_` (all occurrences).
fn redact_bb_ids(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'_' && i + 1 < b.len() && b[i + 1].is_ascii_digit() {
            let mut j = i + 1;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j < b.len() && b[j] == b'_' {
                let mut k = j + 1;
                while k < b.len() && b[k].is_ascii_digit() {
                    k += 1;
                }
                if k > j + 1 {
                    out.push_str("_ID_");
                    i = k;
                    continue;
                }
            }
        }
        out.push_str(&s[i..i + 1]);
        i += 1;
    }
    out
}

/// `xid-123`, `pid-123`, `rid-123` -> `xid-<id>` etc. (all occurrences).
fn redact_xid(s: &str) -> String {
    let mut out = s.to_string();
    for prefix in ["xid-", "pid-", "rid-"] {
        while let Some(start) = out.find(prefix) {
            let digits_start = start + prefix.len();
            let digits_end = out[digits_start..]
                .find(|c: char| !c.is_ascii_digit())
                .map(|i| digits_start + i)
                .unwrap_or(out.len());
            if digits_end == digits_start {
                break; // "xid-" with no digits — leave it
            }
            out.replace_range(start..digits_end, &format!("{prefix}<id>"));
        }
    }
    out
}

/// `user@host.tld` -> `<email>` (all occurrences).
fn redact_emails(s: &str) -> String {
    let Some(at) = s.find('@') else { return s.to_string() };
    let local_start = s[..at]
        .rfind(|c: char| !(c.is_ascii_alphanumeric() || "._%+-".contains(c)))
        .map(|i| i + 1)
        .unwrap_or(0);
    let domain_end = s[at + 1..]
        .find(|c: char| !(c.is_ascii_alphanumeric() || ".-".contains(c)))
        .map(|i| at + 1 + i)
        .unwrap_or(s.len());
    if local_start >= at || domain_end <= at + 1 {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    out.push_str(&s[..local_start]);
    out.push_str("<email>");
    out.push_str(&s[domain_end..]);
    redact_emails(&out) // handle remaining '@'s
}

/// Alphanumeric runs of >= 40 chars (tokens, session ids, base64) -> `<token>`.
fn redact_long_tokens(s: &str) -> String {
    const MIN: usize = 40;
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_alphanumeric() {
            let mut j = i;
            while j < chars.len() && chars[j].is_ascii_alphanumeric() {
                j += 1;
            }
            if j - i >= MIN {
                out.push_str("<token>");
            } else {
                out.extend(&chars[i..j]);
            }
            i = j;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// The signature is the sanitized message, lowercased and trimmed: distinct
/// occurrences differ only in redacted IDs, so they collapse naturally.
fn signature(sanitized: &str) -> String {
    sanitized
        .chars()
        .take(MAX_TITLE)
        .collect::<String>()
        .to_ascii_lowercase()
}

// ---------------------------------------------------------------------------
// Config / markers
// ---------------------------------------------------------------------------

fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("NTULEARN_MCP_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ntulearn-mcp")
}

fn marker_path() -> PathBuf {
    config_dir().join("issues-filed.json")
}

pub fn repo() -> String {
    std::env::var("NTULEARN_ISSUE_REPO").unwrap_or_else(|_| DEFAULT_REPO.to_string())
}

/// Consent gate: env kill-switch, then the opt-in setting from `update`.
pub fn enabled() -> bool {
    if std::env::var("NTULEARN_ISSUE_REPORTS")
        .map(|v| v.eq_ignore_ascii_case("off"))
        .unwrap_or(false)
    {
        return false;
    }
    crate::update::issue_reports_enabled()
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct FiledMap(HashMap<String, FiledIssue>);

#[derive(Debug, Serialize, Deserialize, Clone)]
struct FiledIssue {
    number: u64,
    url: String,
    version: String,
    filed_at: i64,
}

fn load_filed() -> FiledMap {
    std::fs::read(marker_path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_filed(m: &FiledMap) {
    let _ = std::fs::create_dir_all(config_dir());
    if let Ok(json) = serde_json::to_vec_pretty(m) {
        let _ = std::fs::write(marker_path(), json);
    }
}

// ---------------------------------------------------------------------------
// Filing (blocking; run off the async runtime)
// ---------------------------------------------------------------------------

fn gh(args: &[&str]) -> Result<String, String> {
    use std::process::{Command, Stdio};
    let mut child = Command::new("gh")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("gh spawn failed: {e}"))?;
    // gh talks to the network; enforce the timeout by polling.
    let start = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            let out = child.wait_with_output().map_err(|e| e.to_string())?;
            if status.success() {
                return Ok(String::from_utf8_lossy(&out.stdout).trim().to_string());
            }
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(format!("gh {args:?} failed: {status} {stderr}"));
        }
        if start.elapsed() > GH_TIMEOUT {
            let _ = child.kill();
            return Err("gh timed out".into());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Open issues on the target repo: [(number, title)].
fn open_issue_titles() -> Result<Vec<(u64, String)>, String> {
    let out = gh(&[
        "issue",
        "list",
        "--repo",
        &repo(),
        "--state",
        "open",
        "--json",
        "number,title",
        "--limit",
        "300",
    ])?;
    let v: Value = serde_json::from_str(&out).map_err(|e| format!("bad gh json: {e}"))?;
    Ok(v.as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|r| {
                    let n = r.get("number")?.as_u64()?;
                    let t = r.get("title")?.as_str()?.to_string();
                    Some((n, t))
                })
                .collect()
        })
        .unwrap_or_default())
}

fn title_for(sig: &str) -> String {
    let t = format!("[auto] {sig}");
    if t.chars().count() > MAX_TITLE {
        t.chars().take(MAX_TITLE).collect()
    } else {
        t
    }
}

fn body_for(entry: &Entry, sig: &str) -> String {
    let mut b = String::new();
    b.push_str("Auto-filed by ntulearn-mcp after the same error kept occurring.\n\n");
    b.push_str(&format!("- **version:** {}\n", current_version()));
    b.push_str(&format!(
        "- **occurrences:** {} (first seen {}, last seen {})\n",
        entry.count, entry.first_seen, entry.last_seen
    ));
    b.push_str(&format!("- **tools:** {}\n", entry.tools.join(", ")));
    b.push_str(&format!(
        "- **os:** {} {}\n",
        std::env::consts::OS,
        std::env::consts::ARCH
    ));
    b.push_str("\n<details><summary>sanitized message samples</summary>\n\n```\n");
    for s in &entry.samples {
        b.push_str(s);
        b.push_str("\n\n");
    }
    b.push_str("```\n</details>\n");
    b.push_str(&format!("\n_signature:_ `{sig}`\n"));
    if b.chars().count() > MAX_BODY {
        b.chars().take(MAX_BODY).collect()
    } else {
        b
    }
}

/// File one signature if not already filed/marked. Ok(Some(url)) = filed now,
/// Ok(None) = already known, Err = reason it was skipped.
fn file_once(sig: &str, entry: &Entry) -> Result<Option<String>, String> {
    let key = format!("{sig}|{}", current_version());
    let mut filed = load_filed();
    if filed.0.contains_key(&key) {
        return Ok(None);
    }
    // Cross-machine dedupe: an open issue with the same title means somebody
    // (another machine, or markers wiped) already filed it.
    let title = title_for(sig);
    if let Ok(issues) = open_issue_titles() {
        if let Some((n, _)) = issues.iter().find(|(_, t)| t.as_str() == title.as_str()) {
            filed.0.insert(
                key,
                FiledIssue {
                    number: *n,
                    url: format!("https://github.com/{}/issues/{n}", repo()),
                    version: current_version().to_string(),
                    filed_at: now_epoch(),
                },
            );
            save_filed(&filed);
            return Ok(None);
        }
    }
    let body_path = std::env::temp_dir().join(format!("ntulearn-issue-{}.md", now_epoch()));
    std::fs::write(&body_path, body_for(entry, sig)).map_err(|e| e.to_string())?;
    let url = gh(&[
        "issue",
        "create",
        "--repo",
        &repo(),
        "--title",
        &title,
        "--body-file",
        &body_path.to_string_lossy(),
    ])?;
    let _ = std::fs::remove_file(&body_path);
    let number = url
        .rsplit('/')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    filed.0.insert(
        key,
        FiledIssue {
            number,
            url: url.clone(),
            version: current_version().to_string(),
            filed_at: now_epoch(),
        },
    );
    save_filed(&filed);
    Ok(Some(url))
}

// ---------------------------------------------------------------------------
// Background reporter + panic hook
// ---------------------------------------------------------------------------

/// Spawned from `main` next to the update loop: every TICK, file signatures
/// that crossed FILE_AFTER. Every failure is logged and swallowed.
pub async fn background_reporter() {
    let mut tick = tokio::time::interval(TICK);
    loop {
        tick.tick().await;
        let candidates: Vec<(String, Entry)> = {
            let reg = registry().lock().unwrap();
            reg.iter()
                .filter(|(_, e)| e.count >= FILE_AFTER)
                .map(|(k, e)| (k.clone(), e.clone()))
                .collect()
        };
        for (sig, entry) in candidates {
            let sig_for_log = sig.clone();
            let res = tokio::task::spawn_blocking(move || file_once(&sig, &entry)).await;
            match res {
                Ok(Ok(Some(url))) => tracing::info!("auto-filed issue for `{sig_for_log}`: {url}"),
                Ok(Ok(None)) => {}
                Ok(Err(e)) => tracing::warn!("issue filing skipped (`{sig_for_log}`): {e}"),
                Err(e) => tracing::warn!("issue filing task failed: {e}"),
            }
        }
    }
}

/// Panics bypass the threshold and file immediately (from a detached thread,
/// so a crashing task never blocks on the network). The previous hook still
/// runs, preserving the default crash output.
pub fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = format!("panic: {info}");
        record("panic", &msg);
        if enabled() {
            let sig = signature(&sanitize(&msg));
            if let Some(entry) = snapshot_entry(&sig) {
                std::thread::spawn(move || {
                    let _ = file_once(&sig, &entry);
                });
            }
        }
        prev(info);
    }));
}

// ---------------------------------------------------------------------------
// CLI: `ntulearn-mcp issues [on|off|status]`
// ---------------------------------------------------------------------------

pub fn run_cli(args: &[String]) -> i32 {
    match args.first().map(|s| s.as_str()) {
        Some("on") => match crate::update::set_issue_reports(true) {
            Ok(()) => {
                println!("issue auto-filing: ON (repo {})", repo());
                0
            }
            Err(e) => {
                eprintln!("error: {e}");
                1
            }
        },
        Some("off") => match crate::update::set_issue_reports(false) {
            Ok(()) => {
                println!("issue auto-filing: OFF");
                0
            }
            Err(e) => {
                eprintln!("error: {e}");
                1
            }
        },
        _ => {
            println!(
                "issue auto-filing: {} (repo {})",
                if enabled() { "ON" } else { "OFF" },
                repo()
            );
            println!("gh available: {}", gh(&["--version"]).is_ok());
            let filed = load_filed();
            println!(
                "filed this version: {} signature(s)",
                filed
                    .0
                    .values()
                    .filter(|f| f.version == current_version())
                    .count()
            );
            println!("usage: ntulearn-mcp issues [on|off]");
            0
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_redacts_ids_emails_cookies() {
        let out = sanitize(
            "course _2712444_1 content _5862835_1 failed for a@ntu.edu.sg BbRouter=deadbeef123 xid-998877 end",
        );
        assert!(!out.contains("2712444"), "{out}");
        assert!(!out.contains("5862835"), "{out}");
        assert!(!out.contains("a@ntu.edu.sg"), "{out}");
        assert!(!out.contains("deadbeef123"), "{out}");
        assert!(!out.contains("998877"), "{out}");
        assert!(out.contains("_ID_"), "{out}");
        assert!(out.contains("<email>"), "{out}");
        assert!(out.contains("<redacted>"), "{out}");
        assert!(out.contains("xid-<id>"), "{out}");
    }

    #[test]
    fn expected_errors_are_not_filed() {
        assert!(is_expected("401 Unauthorized: cookie expired"));
        assert!(is_expected("missing required argument: course_id"));
        assert!(is_expected("request timed out"));
        assert!(is_expected("silent capture failed: browser not found"));
        assert!(!is_expected("No download URL found. Content handler type may not be supported."));
        assert!(!is_expected("failed to parse gradebook payload"));
    }

    #[test]
    fn signature_collapses_ids() {
        let a = signature(&sanitize("course _2712444_1 has no download url"));
        let b = signature(&sanitize("course _5862835_1 has no download url"));
        assert_eq!(a, b);
    }

    #[test]
    fn long_tokens_are_redacted() {
        let token = "a".repeat(64);
        let out = sanitize(&format!("bad payload: {token}"));
        assert!(out.contains("<token>"), "{out}");
        assert!(!out.contains(&token), "{out}");
    }

    #[test]
    fn filed_map_roundtrip() {
        let mut m = FiledMap::default();
        m.0.insert(
            "sig|0.7.1".into(),
            FiledIssue { number: 5, url: "u".into(), version: "0.7.1".into(), filed_at: 1 },
        );
        let json = serde_json::to_vec(&m).unwrap();
        let back: FiledMap = serde_json::from_slice(&json).unwrap();
        assert_eq!(back.0["sig|0.7.1"].number, 5);
    }
}
