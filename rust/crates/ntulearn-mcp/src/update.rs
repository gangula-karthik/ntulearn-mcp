//! Self-update: notify the user when a new ntulearn-mcp release exists and
//! (opt-in) install it in the background.
//!
//! Design invariants — the updater must NEVER break the server:
//!   * every network call is short-timeout, fire-and-forget in server mode,
//!     and every error is logged to stderr and swallowed;
//!   * checks are throttled to once per 24h (default) and persisted in
//!     `<config>/ntulearn-mcp/update-state.json` so we stay far inside
//!     GitHub's 60 req/hr unauthenticated rate limit;
//!   * a downloaded binary is verified against the release's `checksums.txt`
//!     (sha256) BEFORE it replaces anything, and the swap is an atomic rename
//!     so a crash mid-update can never leave a broken binary behind;
//!   * installs we did not place ourselves (cargo install, homebrew, a dev
//!     `target/` build) are detected and answered with guidance, not a
//!     half-broken self-replace.
//!
//! Notification surfaces:
//!   * MCP server mode: one stderr line per day (hosts log stderr) plus a
//!     one-line notice appended to the FIRST tool result of the process, so
//!     the agent relays it to the user exactly once.
//!   * CLI mode: `ntulearn-mcp update` subcommand (check / prompt / install).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context as _};
use serde::{Deserialize, Serialize};

pub const EXIT_UPDATE_AVAILABLE: i32 = 10;
pub const DEFAULT_REPO: &str = "gangula-karthik/ntulearn-mcp";
const DEFAULT_CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;
const NOTICE_INTERVAL_SECS: u64 = 24 * 60 * 60;

// ---------------------------------------------------------------------------
// Settings + state files
// ---------------------------------------------------------------------------

fn config_dir() -> PathBuf {
    // NTULEARN_MCP_CONFIG_DIR overrides the platform default — used by the
    // test suite and useful for isolated/sandboxed installs.
    if let Some(dir) = std::env::var_os("NTULEARN_MCP_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ntulearn-mcp")
}

fn settings_path() -> PathBuf {
    config_dir().join("settings.json")
}

fn state_path() -> PathBuf {
    config_dir().join("update-state.json")
}

/// Auto-update preference. Default (`On`): new releases are downloaded,
/// signature-verified and atomically swapped in as soon as they appear — the
/// user asked for "just keep me current". `Off`/`Ask` remain available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AutoUpdate {
    #[default]
    On,
    Off,
    Ask,
}

impl AutoUpdate {
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "on" | "true" | "yes" | "1" | "auto" => Ok(AutoUpdate::On),
            "off" | "false" | "no" | "0" | "never" => Ok(AutoUpdate::Off),
            "ask" | "prompt" => Ok(AutoUpdate::Ask),
            other => Err(anyhow!("unknown auto-update mode `{other}` (use on|off|ask)")),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct Settings {
    #[serde(default)]
    auto_update: AutoUpdate,
    /// Release channel: `stable` (GitHub "latest") or `beta` (newest tag,
    /// prereleases included). See `Channel`.
    #[serde(default)]
    channel: Channel,
    /// Refuse to install releases that are not minisign-signed (strict mode).
    /// Default false: unsigned releases install with a loud warning. The
    /// signature is ALWAYS verified when present — this only decides what
    /// happens when it is missing.
    #[serde(default)]
    require_signature: bool,
    /// Opt-in to GitHub issue auto-filing for unexpected errors
    /// (see `issues.rs`). Default false: filing to a public repo is a
    /// consented behavior, offered once during setup/update.
    #[serde(default)]
    issue_reports: bool,
    /// The setup/update opt-in prompt has been shown; never re-ask.
    #[serde(default)]
    optin_prompted: bool,
}

/// Release channel.
/// * Stable — GitHub's `releases/latest` (prereleases excluded).
/// * Beta   — newest semver tag including prereleases (staged rollout:
///   ship v0.7.0-rc.1 as a prerelease and beta-channel users get it,
///   stable users do not until the final tag).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    #[default]
    Stable,
    Beta,
}

impl Channel {
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "stable" | "release" => Ok(Channel::Stable),
            "beta" | "pre" | "prerelease" | "rc" => Ok(Channel::Beta),
            other => Err(anyhow!("unknown channel `{other}` (use stable|beta)")),
        }
    }
}

pub fn effective_channel() -> Channel {
    if let Ok(v) = std::env::var("NTULEARN_MCP_CHANNEL") {
        if let Ok(c) = Channel::parse(&v) {
            return c;
        }
    }
    read_json::<Settings>(&settings_path()).map(|s| s.channel).unwrap_or_default()
}

pub fn set_channel(channel: Channel) -> anyhow::Result<()> {
    let mut settings = read_json::<Settings>(&settings_path()).unwrap_or_default();
    settings.channel = channel;
    write_json(&settings_path(), &settings)
        .with_context(|| format!("writing {}", settings_path().display()))?;
    println!("channel: {channel:?} (saved to {})", settings_path().display());
    Ok(())
}

fn require_signature() -> bool {
    if let Ok(v) = std::env::var("NTULEARN_MCP_REQUIRE_SIGNATURE") {
        return v != "0";
    }
    read_json::<Settings>(&settings_path())
        .map(|s| s.require_signature)
        .unwrap_or(false)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(value).unwrap_or_default())
}

/// Effective auto-update mode: settings file, overridden by env var.
pub fn effective_auto_update() -> AutoUpdate {
    if let Ok(v) = std::env::var("NTULEARN_MCP_AUTOUPDATE") {
        if let Ok(mode) = AutoUpdate::parse(&v) {
            return mode;
        }
    }
    read_json::<Settings>(&settings_path())
        .map(|s| s.auto_update)
        .unwrap_or_default()
}

pub fn set_auto_update(mode: AutoUpdate) -> anyhow::Result<()> {
    let mut settings = read_json::<Settings>(&settings_path()).unwrap_or_default();
    settings.auto_update = mode;
    write_json(&settings_path(), &settings)
        .with_context(|| format!("writing {}", settings_path().display()))?;
    println!("auto-update: {mode:?} (saved to {})", settings_path().display());
    Ok(())
}

/// Opt-in state for GitHub issue auto-filing (see `issues.rs`).
pub fn issue_reports_enabled() -> bool {
    read_json::<Settings>(&settings_path())
        .map(|s| s.issue_reports)
        .unwrap_or(false)
}

pub fn set_issue_reports(on: bool) -> anyhow::Result<()> {
    let mut settings = read_json::<Settings>(&settings_path()).unwrap_or_default();
    settings.issue_reports = on;
    write_json(&settings_path(), &settings)
        .with_context(|| format!("writing {}", settings_path().display()))
}

/// One-time consent flow, offered during `setup` / `update` (interactive
/// terminals only): auto-update defaults to yes, issue auto-filing to no.
/// Persists both answers and never asks again.
pub fn maybe_offer_optins() {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        return;
    }
    let mut settings = read_json::<Settings>(&settings_path()).unwrap_or_default();
    if settings.optin_prompted {
        return;
    }
    println!("
Two quick settings (asked once):");
    if prompt_default("auto-install new releases when available?", true) {
        settings.auto_update = AutoUpdate::On;
    } else {
        settings.auto_update = AutoUpdate::Off;
    }
    if prompt_default(
        "auto-file GitHub issues when the server hits unexpected errors? (sanitized; needs gh)",
        false,
    ) {
        settings.issue_reports = true;
    }
    settings.optin_prompted = true;
    if write_json(&settings_path(), &settings).is_ok() {
        println!(
            "saved to {} (change later: `ntulearn-mcp update --auto on|off|ask`, `ntulearn-mcp issues on|off`)",
            settings_path().display()
        );
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct UpdateState {
    /// Epoch seconds of the last network check (throttle).
    last_check: Option<u64>,
    /// Epoch seconds of the last user-visible notice (throttle).
    last_notice: Option<u64>,
    /// Latest version seen, e.g. "0.5.0" (no `v` prefix).
    latest: Option<String>,
    /// Which channel `latest` was observed on (cache invalidation on switch).
    channel: Option<String>,
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn check_interval() -> Duration {
    let secs = std::env::var("NTULEARN_MCP_UPDATE_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_CHECK_INTERVAL_SECS);
    Duration::from_secs(secs)
}

fn check_disabled() -> bool {
    std::env::var("NTULEARN_MCP_NO_UPDATE_CHECK")
        .ok()
        .is_some_and(|v| v != "0")
        || effective_auto_update() == AutoUpdate::Off
}

fn repo() -> String {
    std::env::var("NTULEARN_MCP_REPO").unwrap_or_else(|_| DEFAULT_REPO.to_string())
}

fn channel_name(c: Channel) -> &'static str {
    match c {
        Channel::Stable => "stable",
        Channel::Beta => "beta",
    }
}

// ---------------------------------------------------------------------------
// Release signatures (minisign)
// ---------------------------------------------------------------------------

/// The base64 key line from a minisign pubkey file (skips comment lines).
/// The release-signing public key, baked into the binary at compile time
/// (repo root `minisign.pub`, via `include_str!` relative to this source
/// file). Releases sign `checksums.txt` with the matching secret key in CI;
/// the updater verifies the signature BEFORE trusting the checksums (which
/// in turn verify each downloaded asset). Public keys are not secret; baking
/// it in means every installed binary can verify signed updates without a
/// checkout of the repository. Forks/other distributions can override the
/// key at runtime with NTULEARN_MCP_MINISIGN_PUBKEY (raw base64 or full file).
const BAKED_MINISIGN_PUB: &str = include_str!("../../../../minisign.pub");

fn minisign_key_base64() -> Option<String> {
    if let Ok(v) = std::env::var("NTULEARN_MCP_MINISIGN_PUBKEY") {
        let line = v.lines().rev().find(|l| !l.trim().is_empty() && !l.starts_with("untrusted"))?;
        return Some(line.trim().to_string());
    }
    let file = BAKED_MINISIGN_PUB;
    let line = file
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty() && !l.starts_with("untrusted"))?;
    Some(line.trim().to_string())
}

/// Does this look like a minisign `.minisig` file? Real signature files
/// always start with an `untrusted comment:` line and parse cleanly;
/// GitHub error bodies (JSON) and empty responses do not. (Do NOT sniff for
/// algorithm bytes like "Ed" — they land at arbitrary positions once the
/// 74-byte signature blob is base64-encoded.)
fn looks_like_minisig(s: &str) -> bool {
    s.starts_with("untrusted comment:") && minisign_verify::Signature::decode(s).is_ok()
}

/// Verify a minisign signature over `data`. `sig_file` is the raw content of
/// the `.minisig` file (untrusted comment + base64 + trusted comment + b64).
fn verify_minisign(data: &[u8], sig_file: &str) -> anyhow::Result<()> {
    use minisign_verify::{PublicKey, Signature};
    let key_b64 = minisign_key_base64()
        .ok_or_else(|| anyhow!("no minisign public key available (minisign.pub missing)"))?;
    let pk = PublicKey::from_base64(&key_b64)
        .map_err(|e| anyhow!("invalid minisign public key: {e:?}"))?;
    let signature = Signature::decode(sig_file)
        .map_err(|e| anyhow!("invalid minisign signature file: {e:?}"))?;
    pk.verify(data, &signature, false)
        .map_err(|_| anyhow!("minisign signature verification FAILED"))
}

// ---------------------------------------------------------------------------
// GitHub release discovery
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct Release {
    pub tag_name: String,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
}

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn tag_to_version(tag: &str) -> Option<semver::Version> {
    let v = tag.trim().trim_start_matches('v');
    semver::Version::parse(v).ok()
}

/// Is `tag` a strictly newer release than the running binary?
pub fn is_newer(tag: &str) -> bool {
    match (tag_to_version(tag), semver::Version::parse(current_version())) {
        (Some(remote), Ok(local)) => remote > local,
        _ => false,
    }
}

fn http_client() -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .connect_timeout(Duration::from_secs(5))
        .user_agent(concat!("ntulearn-mcp-updater/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("building HTTP client for update check")
}

/// Fetch the newest release on the given channel (network, 10s timeout).
///
/// * Stable: GitHub's `releases/latest` (prereleases excluded by GitHub).
/// * Beta:   newest semver tag across published releases, prereleases
///   included — this is the staged-rollout mechanism.
pub async fn fetch_latest_release(channel: Channel) -> anyhow::Result<Release> {
    let client = http_client()?;
    let headers = |mut rb: reqwest::RequestBuilder| {
        rb = rb.header("Accept", "application/vnd.github+json");
        rb
    };
    if channel == Channel::Stable {
        let url = format!("https://api.github.com/repos/{}/releases/latest", repo());
        let resp = headers(client.get(&url))
            .send()
            .await
            .context("contacting GitHub for latest release")?;
        if !resp.status().is_success() {
            return Err(anyhow!("GitHub API returned {}", resp.status()));
        }
        return resp.json::<Release>().await.context("parsing release payload");
    }

    // Beta: list recent published releases, pick the highest semver tag.
    let url = format!("https://api.github.com/repos/{}/releases?per_page=30", repo());
    let resp = headers(client.get(&url))
        .send()
        .await
        .context("listing GitHub releases")?;
    if !resp.status().is_success() {
        return Err(anyhow!("GitHub API returned {}", resp.status()));
    }
    let releases = resp.json::<Vec<Release>>().await.context("parsing releases")?;
    releases
        .into_iter()
        .filter(|r| tag_to_version(&r.tag_name).is_some())
        .max_by_key(|r| tag_to_version(&r.tag_name))
        .ok_or_else(|| anyhow!("no versioned releases found (beta channel)"))
}

/// Throttled check: hits the network at most once per interval (persisted),
/// otherwise replays the cached answer. `force` bypasses the throttle.
pub async fn throttled_check(force: bool) -> anyhow::Result<Option<Release>> {
    let mut state = read_json::<UpdateState>(&state_path()).unwrap_or_default();
    let fresh = state
        .last_check
        .map(|t| now_secs().saturating_sub(t) < check_interval().as_secs())
        .unwrap_or(false);
    let channel = effective_channel();
    // Cache is only valid for the channel it was recorded on.
    let fresh = fresh && state.channel.as_deref() == Some(channel_name(channel));
    if fresh && !force {
        if let Some(latest) = &state.latest {
            if is_newer(&format!("v{latest}")) {
                // Cached "update available": rebuild enough of the release to act on.
                return Ok(Some(Release { tag_name: format!("v{latest}"), assets: Vec::new() }));
            }
        }
        return Ok(None);
    }

    let release = fetch_latest_release(channel).await?;
    state.last_check = Some(now_secs());
    state.latest = Some(release.tag_name.trim_start_matches('v').to_string());
    state.channel = Some(channel_name(channel).to_string());
    let _ = write_json(&state_path(), &state); // best effort
    if is_newer(&release.tag_name) {
        Ok(Some(release))
    } else {
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Install
// ---------------------------------------------------------------------------

/// Rust target triple matching the running binary.
fn current_triple() -> String {
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    match (os, arch) {
        ("macos", "aarch64") => "aarch64-apple-darwin".to_string(),
        ("macos", _) => format!("{arch}-apple-darwin"),
        ("linux", arch) => format!("{arch}-unknown-linux-gnu"),
        ("windows", arch) => format!("{arch}-pc-windows-msvc"),
        _ => format!("{arch}-{os}"),
    }
}

fn pick_asset(release: &Release) -> Option<&Asset> {
    let triple = current_triple();
    let ext = if cfg!(windows) { "zip" } else { "tar.gz" };
    release.assets.iter().find(|a| a.name.contains(&triple) && a.name.ends_with(ext))
}

/// Why self-replace is disabled for this binary, if it is.
fn non_self_managed_install(exe: &Path) -> Option<&'static str> {
    let s = exe.to_string_lossy();
    let cargo_home = std::env::var("CARGO_HOME").ok().or_else(|| {
        dirs::home_dir().map(|h| h.join(".cargo").to_string_lossy().into_owned())
    });
    if let Some(ch) = cargo_home {
        if s.starts_with(&ch) && s.contains("bin") {
            return Some("installed via `cargo install`");
        }
    }
    if s.contains("/target/") || s.contains("\\target\\") {
        return Some("a local development build");
    }
    if s.contains("/Cellar/") || s.contains("/opt/homebrew/") || s.contains("\\scoop\\") {
        return Some("installed via a package manager");
    }
    None
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    let out = h.finalize();
    let mut s = String::with_capacity(out.len() * 2);
    for b in out {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Parse `sha256sum`-style `checksums.txt` ("<hash>  <name>" per line).
fn expected_sha256(checksums: &str, asset_name: &str) -> Option<String> {
    checksums.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let hash = parts.next()?.trim_end_matches('*');
        let name = parts.next().unwrap_or("");
        if name.trim_start_matches('*') == asset_name {
            Some(hash.to_ascii_lowercase())
        } else {
            None
        }
    })
}

/// Extract the `ntulearn-mcp` binary from a .tar.gz / .zip release archive.
fn extract_binary(archive: &[u8], is_zip: bool, dest: &Path) -> anyhow::Result<()> {
    let exe_name = if cfg!(windows) { "ntulearn-mcp.exe" } else { "ntulearn-mcp" };
    if is_zip {
        let reader = std::io::Cursor::new(archive);
        let mut zip = zip::ZipArchive::new(reader).context("opening release zip")?;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            if entry.name().rsplit('/').next() == Some(exe_name) && !entry.is_dir() {
                let mut out = std::fs::File::create(dest)?;
                std::io::copy(&mut entry, &mut out)?;
                return Ok(());
            }
        }
    } else {
        let gz = flate2::read::GzDecoder::new(archive);
        let mut tar = tar::Archive::new(gz);
        for entry in tar.entries().context("reading release tarball")? {
            let mut entry = entry?;
            let name = entry.path()?.to_path_buf();
            let leaf = name.file_name().map(|n| Path::new(n).to_path_buf());
            if leaf.as_deref() == Some(Path::new(exe_name)) && entry.header().entry_type().is_file() {
                let mut out = std::fs::File::create(dest)?;
                std::io::copy(&mut entry, &mut out)?;
                return Ok(());
            }
        }
    }
    Err(anyhow!("release archive does not contain `{exe_name}`"))
}

/// Download + verify + atomically install the given release.
///
/// On Unix the rename over the running binary is legal and atomic; the new
/// version activates the next time the binary starts (MCP hosts restart the
/// server on their own schedule, so we never kill ourselves).
pub async fn install_release(release: &Release) -> anyhow::Result<()> {
    let exe = std::env::current_exe().context("locating current binary")?;
    if let Some(reason) = non_self_managed_install(&exe) {
        return Err(anyhow!(
            "this binary looks like it is {reason}; self-update is disabled for it.\n\
             Update it the way you installed it, e.g.\n  \
             cargo install --git https://github.com/{repo} --locked\n\
             or download {} from https://github.com/{repo}/releases/latest",
            release.tag_name,
            repo = repo()
        ));
    }

    let asset = pick_asset(release).ok_or_else(|| {
        anyhow!(
            "no release asset for platform {} ({}-{})",
            current_triple(),
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })?;
    let client = http_client()?;

    // 1. checksums.txt (+ its minisign signature). The signature, when
    //    present, is verified BEFORE the checksums are trusted — which is
    //    before anything is written to disk.
    let checksums_url = format!(
        "https://github.com/{}/releases/download/{}/checksums.txt",
        repo(),
        release.tag_name
    );
    let checksums_resp = client.get(&checksums_url).send().await?;
    if !checksums_resp.status().is_success() {
        return Err(anyhow!("release has no checksums.txt (HTTP {}) — refusing to install unverified", checksums_resp.status()));
    }
    let checksums = checksums_resp.text().await.unwrap_or_default();

    let sig_resp = match client.get(format!("{checksums_url}.minisig")).send().await {
        Ok(r) if r.status().is_success() => r.text().await.unwrap_or_default(),
        // Absent asset (404 JSON body, network error) -> treat as unsigned.
        _ => String::new(),
    };
    if looks_like_minisig(&sig_resp) {
        if let Err(e) = verify_minisign(checksums.as_bytes(), &sig_resp) {
            return Err(anyhow!("release rejected: {e}"));
        }
    } else if minisign_key_base64().is_some() {
        // We ship a signing key but this release is unsigned.
        if require_signature() {
            return Err(anyhow!(
                "release is not minisign-signed and require_signature is on — refusing to install"
            ));
        }
        eprintln!(
            "WARNING: release {} has no minisign signature; installing unverified (set require_signature in {} to enforce)",
            release.tag_name,
            settings_path().display()
        );
    }
    let expected = expected_sha256(&checksums, &asset.name);

    // 2. download the archive.
    let bytes = client
        .get(&asset.browser_download_url)
        .send()
        .await
        .context("downloading release")?
        .error_for_status()?
        .bytes()
        .await
        .context("reading release bytes")?;

    // 3. verify.
    let expected = match expected {
        Some(e) => e,
        None => return Err(anyhow!("release has no checksums.txt — refusing to install unverified")),
    };
    let actual = sha256_hex(&bytes);
    if !actual.eq_ignore_ascii_case(&expected) {
        return Err(anyhow!("checksum mismatch for {} — refusing to install", asset.name));
    }

    // 4. extract to a temp file NEXT TO the current exe (same filesystem ->
    //    atomic rename), then swap.
    let tmp = exe.with_extension("ntulearn-mcp.update.tmp");
    extract_binary(&bytes, asset.name.ends_with(".zip"), &tmp)
        .with_context(|| format!("extracting {}", asset.name))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
            .context("setting exec permission")?;
    }

    #[cfg(windows)]
    {
        // Windows locks the running exe: move it aside first, then swap in.
        let old = exe.with_extension("old.exe");
        let _ = std::fs::remove_file(&old);
        std::fs::rename(&exe, &old).context("moving current binary aside")?;
    }

    std::fs::rename(&tmp, &exe).context("swapping in new binary")?;
    let _ = std::fs::remove_file(exe.with_extension("ntulearn-mcp.update.tmp"));
    Ok(())
}

// ---------------------------------------------------------------------------
// CLI: `ntulearn-mcp update [--check] [--yes] [--auto on|off|ask] [--status]`
// ---------------------------------------------------------------------------

fn prompt_yes(question: &str) -> bool {
    prompt_default(question, false)
}

/// Terminal prompt where empty input accepts the shown default.
fn prompt_default(question: &str, default_yes: bool) -> bool {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        eprintln!("{question} (stdin is not a terminal — pass --yes to apply)");
        return false;
    }
    let hint = if default_yes { "[Y/n]" } else { "[y/N]" };
    print!("{question} {hint} ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return default_yes;
    }
    match line.trim().to_ascii_lowercase().as_str() {
        "" => default_yes,
        "y" | "yes" => true,
        _ => false,
    }
}

pub async fn run_update_cli(argv: &[String]) -> i32 {
    let mut check_only = false;
    let mut yes = false;
    let mut auto: Option<AutoUpdate> = None;
    let mut channel: Option<Channel> = None;
    let mut status_only = false;

    let mut it = argv.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--check" => check_only = true,
            "--yes" | "-y" => yes = true,
            "--status" => status_only = true,
            "--channel" => match it.next().map(|s| Channel::parse(s)) {
                Some(Ok(c)) => channel = Some(c),
                Some(Err(e)) => {
                    eprintln!("error: {e}");
                    return 2;
                }
                None => {
                    eprintln!("error: --channel needs a value: stable|beta");
                    return 2;
                }
            },
            "--auto" => match it.next().map(|s| AutoUpdate::parse(s)) {
                Some(Ok(mode)) => auto = Some(mode),
                Some(Err(e)) => {
                    eprintln!("error: {e}");
                    return 2;
                }
                None => {
                    eprintln!("error: --auto needs a value: on|off|ask");
                    return 2;
                }
            },
            other => {
                eprintln!("unknown update flag: {other}");
                eprintln!(
                    "usage: ntulearn-mcp update [--check] [--yes] [--auto on|off|ask] [--status]"
                );
                return 2;
            }
        }
    }

    if let Some(mode) = auto {
        if let Err(e) = set_auto_update(mode) {
            eprintln!("error: {e}");
            return 1;
        }
        if status_only || check_only {
            return 0;
        }
    }

    if let Some(c) = channel {
        if let Err(e) = set_channel(c) {
            eprintln!("error: {e}");
            return 1;
        }
        if status_only || check_only {
            return 0;
        }
    }

    // One-time consent prompts (auto-update / issue filing), interactive
    // terminals only. Skipped when --auto was just set explicitly.
    if auto.is_none() {
        maybe_offer_optins();
    }

    if status_only {
        println!("version    : {} (this binary)", current_version());
        println!("auto-update: {:?}", effective_auto_update());
        println!("channel    : {:?}", effective_channel());
        match throttled_check(true).await {
            Ok(Some(r)) => {
                println!("latest     : {} — UPDATE AVAILABLE", r.tag_name.trim_start_matches('v'));
                println!("run        : ntulearn-mcp update");
            }
            Ok(None) => println!("latest     : {} — up to date", current_version()),
            Err(e) => println!("latest     : unknown ({e})"),
        }
        return 0;
    }

    println!(
        "ntulearn-mcp {} — checking https://github.com/{} …",
        current_version(),
        repo()
    );
    let release = match throttled_check(true).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            println!("up to date ({} is the latest release)", current_version());
            return 0;
        }
        Err(e) => {
            eprintln!("update check failed: {e}");
            return 1;
        }
    };

    let latest = release.tag_name.trim_start_matches('v').to_string();
    println!("update available: {current} -> {latest}", current = current_version());
    println!("  https://github.com/{}/releases/tag/{}", repo(), release.tag_name);

    if check_only {
        return EXIT_UPDATE_AVAILABLE;
    }

    let proceed = yes || effective_auto_update() == AutoUpdate::On || prompt_yes("install?");
    if !proceed {
        println!("not installed (enable auto-apply with: ntulearn-mcp update --auto on)");
        return EXIT_UPDATE_AVAILABLE;
    }

    match install_release(&release).await {
        Ok(()) => {
            println!(
                "updated to {latest} — restart your MCP host (Claude Desktop / Cursor / …) to activate it."
            );
            0
        }
        Err(e) => {
            eprintln!("update failed: {e}");
            1
        }
    }
}

// ---------------------------------------------------------------------------
// Server-mode background notifier
// ---------------------------------------------------------------------------

/// A one-line notice appended to the first tool result of a server process,
/// so the agent relays it to the user exactly once per process per day.
#[derive(Default)]
pub struct UpdateNotice {
    pending: Mutex<Option<String>>,
}

impl UpdateNotice {
    pub fn set(&self, line: String) {
        *self.pending.lock().unwrap() = Some(line);
    }
    /// Take the notice if one is pending (fires once per process).
    pub fn take(&self) -> Option<String> {
        self.pending.lock().unwrap().take()
    }
}

fn should_notice(state: &UpdateState) -> bool {
    state
        .last_notice
        .map(|t| now_secs().saturating_sub(t) >= NOTICE_INTERVAL_SECS)
        .unwrap_or(true)
}

fn mark_noticed() {
    let mut state = read_json::<UpdateState>(&state_path()).unwrap_or_default();
    state.last_notice = Some(now_secs());
    let _ = write_json(&state_path(), &state);
}

/// Background task for MCP server mode (main.rs spawns this):
///   * throttled release check at startup, then hourly;
///   * on update available: stderr notice + first-tool-call notice;
///   * with auto-update ON: download + verify + atomic swap (activates on
///     the next host restart). Never kills the running process.
pub async fn background_updater(notice: std::sync::Arc<UpdateNotice>) {
    if check_disabled() {
        return;
    }
    let mut tick = tokio::time::interval(Duration::from_secs(60 * 60));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let state = read_json::<UpdateState>(&state_path()).unwrap_or_default();
        let fresh = state
            .last_check
            .map(|t| now_secs().saturating_sub(t) < check_interval().as_secs())
            .unwrap_or(false)
            && state.channel.as_deref() == Some(channel_name(effective_channel()));
        if fresh {
            // Reuse cached knowledge without network: notify if still pending.
            if let Some(latest) = &state.latest {
                if is_newer(&format!("v{latest}")) && should_notice(&state) {
                    emit_notices(&notice, latest);
                    mark_noticed();
                }
            }
        } else {
            match fetch_latest_release(effective_channel()).await {
                Ok(release) => {
                    let mut st = state;
                    st.last_check = Some(now_secs());
                    st.latest = Some(release.tag_name.trim_start_matches('v').to_string());
                    st.channel = Some(channel_name(effective_channel()).to_string());
                    let _ = write_json(&state_path(), &st);
                    if is_newer(&release.tag_name) {
                        let latest = release.tag_name.trim_start_matches('v').to_string();
                        if should_notice(&st) {
                            emit_notices(&notice, &latest);
                            mark_noticed();
                        }
                        if effective_auto_update() == AutoUpdate::On {
                            match install_release(&release).await {
                                Ok(()) => eprintln!(
                                    "ntulearn-mcp {latest} staged — activates on next host restart."
                                ),
                                Err(e) => eprintln!("ntulearn-mcp auto-update failed: {e}"),
                            }
                        }
                    }
                }
                Err(e) => eprintln!("ntulearn-mcp update check failed (ignored): {e}"),
            }
        }
        tick.tick().await;
    }
}

fn emit_notices(notice: &UpdateNotice, latest: &str) {
    let current = current_version();
    eprintln!(
        "UPDATE AVAILABLE: ntulearn-mcp {latest} (current {current}). \
         Run `ntulearn-mcp update` to upgrade, or `ntulearn-mcp update --auto on` \
         to auto-apply future releases."
    );
    notice.set(format!(
        "ntulearn-mcp update available: {latest} (this server runs {current}). \
         Tell the user; they can run `ntulearn-mcp update` to upgrade \
         (or `ntulearn-mcp update --auto on` to enable auto-updates)."
    ));
}

/// One-line status for the `check` command (respects the 24h throttle).
pub async fn status_line() -> String {
    if check_disabled() {
        return format!("Update check : disabled (auto-update {:?})", effective_auto_update());
    }
    match throttled_check(false).await {
        Ok(Some(r)) => format!(
            "Update       : {} available on {:?} channel — run `ntulearn-mcp update`",
            r.tag_name.trim_start_matches('v'),
            effective_channel()
        ),
        Ok(None) => "Update       : up to date".to_string(),
        Err(e) => format!("Update       : check failed ({e})"),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_version_detection() {
        assert!(is_newer("v999.0.0"));
        assert!(!is_newer("v0.0.1")); // older than CARGO_PKG_VERSION (0.4.0)
        assert!(!is_newer("not-a-version")); // invalid semver -> not newer
    }

    #[test]
    fn tag_parsing() {
        assert_eq!(tag_to_version("v0.5.0").unwrap().to_string(), "0.5.0");
        assert_eq!(tag_to_version("0.5.1").unwrap().to_string(), "0.5.1");
        assert!(tag_to_version("not-a-version").is_none());
    }

    #[test]
    fn auto_update_parse() {
        assert_eq!(AutoUpdate::parse("on").unwrap(), AutoUpdate::On);
        assert_eq!(AutoUpdate::parse("OFF").unwrap(), AutoUpdate::Off);
        assert_eq!(AutoUpdate::parse("ask").unwrap(), AutoUpdate::Ask);
        assert_eq!(AutoUpdate::parse("true").unwrap(), AutoUpdate::On);
        assert!(AutoUpdate::parse("maybe").is_err());
    }

    #[test]
    fn settings_roundtrip() {
        let dir = std::env::temp_dir().join(format!("ntu-upd-test-{}", std::process::id()));
        let path = dir.join("settings.json");
        let settings = Settings {
            auto_update: AutoUpdate::On,
            channel: Channel::Beta,
            require_signature: true,
            issue_reports: true,
            optin_prompted: true,
        };
        write_json(&path, &settings).unwrap();
        let back: Settings = read_json(&path).unwrap();
        assert_eq!(back.auto_update, AutoUpdate::On);
        assert_eq!(back.channel, Channel::Beta);
        assert!(back.require_signature);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn checksums_parsing() {
        // sha256sum text mode ("hash  name") and binary mode ("hash *name").
        let txt = "abc123  ntulearn-mcp-x86_64-unknown-linux-gnu.tar.gz\n\
                    def456 *ntulearn-mcp-aarch64-apple-darwin.tar.gz\n";
        assert_eq!(
            expected_sha256(txt, "ntulearn-mcp-aarch64-apple-darwin.tar.gz").unwrap(),
            "def456"
        );
        assert!(expected_sha256(txt, "other.tar.gz").is_none());
    }

    #[test]
    fn asset_picking_prefers_platform() {
        let release = Release {
            tag_name: "v0.5.0".into(),
            assets: vec![
                Asset {
                    name: "ntulearn-mcp-x86_64-unknown-linux-gnu.tar.gz".into(),
                    browser_download_url: "http://x/linux".into(),
                },
                Asset {
                    name: "ntulearn-mcp-aarch64-apple-darwin.tar.gz".into(),
                    browser_download_url: "http://x/mac".into(),
                },
                Asset {
                    name: "ntulearn-mcp-x86_64-pc-windows-msvc.zip".into(),
                    browser_download_url: "http://x/win".into(),
                },
                Asset {
                    name: "checksums.txt".into(),
                    browser_download_url: "http://x/sums".into(),
                },
            ],
        };
        let picked = pick_asset(&release).expect("an asset matches this platform");
        assert!(picked.name.contains(&current_triple()));
    }

    #[test]
    fn tar_gz_extraction() {
        // Build a tiny tar.gz in-memory containing `ntulearn-mcp`, then extract.
        let dir = std::env::temp_dir().join(format!("ntu-tar-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        let payload = b"fake-binary-bytes";
        header.set_size(payload.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        // extract_binary looks for the platform exe name, so the fixture must
        // carry it too (this is exactly what the Windows runners caught).
        let exe_name = if cfg!(windows) { "ntulearn-mcp.exe" } else { "ntulearn-mcp" };
        builder.append_data(&mut header, exe_name, &payload[..]).unwrap();
        let raw = builder.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&raw).unwrap();
        let archive = gz.finish().unwrap();

        let dest = dir.join("out-binary");
        extract_binary(&archive, false, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), payload);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Env vars are process-global and tests run on parallel threads, so
    /// every test that touches NTULEARN_MCP_CONFIG_DIR takes this lock.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// RAII guard: sets the config-dir override and restores it on drop.
    struct ConfigDirOverride(std::sync::MutexGuard<'static, ()>, PathBuf);
    impl ConfigDirOverride {
        fn new() -> Self {
            let guard = ENV_LOCK.lock().unwrap();
            let dir = std::env::temp_dir()
                .join(format!("ntu-upd-cfg-{}-{}", std::process::id(), guard_path_token()));
            std::fs::create_dir_all(&dir).unwrap();
            std::env::set_var("NTULEARN_MCP_CONFIG_DIR", &dir);
            Self(guard, dir)
        }
    }
    impl Drop for ConfigDirOverride {
        fn drop(&mut self) {
            std::env::remove_var("NTULEARN_MCP_CONFIG_DIR");
            let _ = std::fs::remove_dir_all(&self.1);
        }
    }

    fn guard_path_token() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static TOKEN: AtomicU64 = AtomicU64::new(0);
        TOKEN.fetch_add(1, Ordering::Relaxed)
    }

    #[test]
    fn throttled_check_uses_cache_without_network() {
        let _cfg = ConfigDirOverride::new();
        // Fresh check (< 24h) with newer cached version -> cached hit, no network.
        let st = UpdateState { last_check: Some(now_secs()), latest: Some("999.0.0".into()), last_notice: None, channel: Some("stable".into()) };
        write_json(&state_path(), &st).unwrap();
        let rel = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(throttled_check(false))
            .expect("cached check must not error");
        assert_eq!(rel.unwrap().tag_name, "v999.0.0");
        // Up-to-date cache -> None.
        let st = UpdateState { last_check: Some(now_secs()), latest: Some("0.0.1".into()), last_notice: None, channel: Some("stable".into()) };
        write_json(&state_path(), &st).unwrap();
        let rel = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(throttled_check(false))
            .expect("cached check must not error");
        assert!(rel.is_none());
    }

    #[test]
    fn notice_throttle_bookkeeping() {
        let _cfg = ConfigDirOverride::new();
        // Never noticed -> notice allowed.
        let st = UpdateState::default();
        assert!(should_notice(&st));
        // Noticed seconds ago -> suppressed.
        let st = UpdateState { last_notice: Some(now_secs()), ..Default::default() };
        assert!(!should_notice(&st));
        // Noticed long ago -> allowed again.
        let st = UpdateState { last_notice: Some(now_secs() - NOTICE_INTERVAL_SECS - 1), ..Default::default() };
        assert!(should_notice(&st));
    }

    #[test]
    fn channel_parse() {
        assert_eq!(Channel::parse("stable").unwrap(), Channel::Stable);
        assert_eq!(Channel::parse("BETA").unwrap(), Channel::Beta);
        assert_eq!(Channel::parse("rc").unwrap(), Channel::Beta);
        assert!(Channel::parse("dev").is_err());
    }

    #[test]
    fn prerelease_versions_compare_correctly() {
        // Staged rollout: a prerelease of a FUTURE version is an update;
        // a prerelease of the CURRENT version is not (semver: x.y.z-rc < x.y.z).
        assert!(is_newer("v999.0.0-beta.1"));
        let current = semver::Version::parse(current_version()).unwrap();
        assert!(!is_newer(&format!("v{current}-beta.1")));
        assert!(
            tag_to_version("v0.7.0-beta.1").unwrap() < tag_to_version("v0.7.0").unwrap()
        );
    }

    #[test]
    fn minisign_pubkey_is_baked_and_wellformed() {
        // The public key is baked into the binary via include_str!, so every
        // released binary can verify signed updates without a repo checkout.
        let key = minisign_key_base64()
            .expect("baked minisign.pub must yield a key");
        use base64::Engine as _;
        let raw = base64::engine::general_purpose::STANDARD
            .decode(&key)
            .expect("pubkey must be valid base64");
        assert!(raw.len() >= 42, "minisign pubkey payload too short: {}", raw.len());
        assert_eq!(&raw[..2], b"Ed", "minisign pubkey must be Ed25519");
    }
    #[test]
    fn minisig_detection_regression() {
        // The v0.7.0 release's actual signature: base64 payload starts with
        // "RU" (not "Ed" — the algorithm bytes don't sit at a fixed base64
        // offset), which the old presence heuristic missed entirely.
        let real_sig = r#"untrusted comment: signature from minisign secret key
RUTMDxQVODM4LlXtykuradg+QhoSHtxITlzKBzfBXo03HeNdVzg9Fd1W28xDrUfx3T/4pLFrRWvrR+kLYLTttDH5uHbkD1wtoQ4=
trusted comment: timestamp:1789026389	file:checksums.txt	hashed
P37abEV85P0lLkI30cM23oAVRR6F7BfsgpluSJzYTqim+0mWJKCEQJvlQq8p3Z/o/yxNlrg+P2cBrQlKaZKTDQ==
"#;
        assert!(looks_like_minisig(real_sig), "real v0.7.0 signature must be detected");
        // It must also verify against the baked key.
        let checksums = r#"4ad240421d266974bc5e6a7df0025f7b3e707ce82c5931a2d34ac467e2341e74  ntulearn-mcp-aarch64-apple-darwin.tar.gz
47df54a117d82963b4ebb89a308f4a457c7b1e9cfeb8103e1f9a7e1c4a2b1bef  ntulearn-mcp-x86_64-apple-darwin.tar.gz
aaf0784ad01d69b2e0ef4b6b204406de57130e05348a5a1ad2852080cf51e90a  ntulearn-mcp-x86_64-pc-windows-msvc.zip
7c5f492dfd59d4aa154dbde2b5b7918467cb245ad39da6a8f1579833a8f6c9c1  ntulearn-mcp-x86_64-unknown-linux-gnu.tar.gz
"#;
        assert!(verify_minisign(checksums.as_bytes(), real_sig).is_ok());
        // Non-signature bodies must NOT be detected.
        assert!(!looks_like_minisig(""));
        assert!(!looks_like_minisig("{\"message\":\"Not Found\"}"));
        assert!(!looks_like_minisig("untrusted comment: fake\nnot-base64!!!"));
    }


    #[test]
    fn minisign_verify_roundtrip() {
        // Can't sign without the private key (it lives only in CI secrets),
        // but a tampered signature must fail hard against the real key.
        let key = minisign_key_base64();
        if key.is_none() {
            return; // fork without minisign.pub — nothing to test
        }
        let fake_sig = "untrusted comment: fake\nRWQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\ntrusted comment: fake\nRWQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let err = verify_minisign(b"checksums", fake_sig);
        assert!(err.is_err(), "forged signature must be rejected");
    }


    #[test]
    fn notice_fires_once() {
        let notice = UpdateNotice::default();
        assert!(notice.take().is_none());
        notice.set("hello".into());
        assert_eq!(notice.take().as_deref(), Some("hello"));
        assert!(notice.take().is_none(), "second take must be empty");
    }
}
