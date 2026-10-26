
//! NTULearn MCP server — Blackboard Learn REST API wrapper, Rust edition.
//!
//! MCP stdio server implemented in-tree (src/mcp.rs): plain JSON-RPC 2.0 over
//! newline-delimited stdio. Registered as "ntulearn-mcp" with
//! tool/resource/prompt parity with the original Python implementation.

#![allow(dead_code)]

mod bench;
mod cache;
mod cli;
mod capture;
mod client;
mod cookie;
mod handlers;
mod issues;
mod mcp;
mod parsers;
mod prompts;
mod render;
mod resources;
mod schemas;
mod setup;
mod toon;
mod tools;
mod update;

use std::path::PathBuf;
use std::time::Duration;
use std::sync::Arc;

use crate::mcp::{McpServer, ServerCapabilities, ServerInfo};

use crate::cache::DataCache;
use crate::client::NTULearnClient;
use crate::cookie::resolve_cookie;
use crate::prompts::NtuPromptHandler;
use crate::resources::NtuResourceHandler;
use crate::tools::NtuToolHandler;

/// Shared server state handed to every handler.
pub struct AppState {
    pub client: NTULearnClient,
    pub download_dir: PathBuf,
    /// One-line "update available" notice, appended to the FIRST tool result
    /// of the process so the agent relays it to the user exactly once.
    /// Filled by the background updater task (see `update::background_updater`).
    pub update_notice: Arc<update::UpdateNotice>,
}

const BASE_URL: &str = "https://ntulearn.ntu.edu.sg";

/// Build the shared AppState (client + cache + download dir).
///
/// Used by both the MCP server loop and the CLI subcommands, so cookie
/// resolution, caching, and HTTP client config behave identically.
pub async fn build_state() -> anyhow::Result<Arc<AppState>> {
    let base_url = std::env::var("NTULEARN_BASE_URL").unwrap_or_else(|_| BASE_URL.to_string());
    let download_dir = std::env::var("NTULEARN_DOWNLOAD_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./downloads"));

    // Cookie: env -> cookie file -> (browser helper). Never touches the OS
    // keychain and never triggers a password prompt. A missing cookie only
    // warns in server mode (long-lived process); CLI commands surface the
    // resulting 401 directly in their own error output.
    let cookie = resolve_cookie().unwrap_or_default();

    let data_cache = Arc::new(DataCache::open()?);
    let client = NTULearnClient::new(base_url, cookie, data_cache)?;

    Ok(Arc::new(AppState {
        client,
        download_dir,
        update_notice: Arc::new(update::UpdateNotice::default()),
    }))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Subcommands run without entering the stdio MCP loop:
    //   * setup/check/refresh — cookie lifecycle
    //   * everything else    — the agent-first CLI (see cli.rs), including
    //     `help`, `multi` (concurrent fan-out) and `selfbench`
    // No arguments at all -> MCP server over stdio (MCP hosts spawn it bare).
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match argv.first().map(|s| s.as_str()) {
        None | Some("") => {}
        Some(arg) => {
            let code = match arg {
                "setup" => setup::run_setup().await,
                "check" => setup::run_check().await,
                "refresh" => setup::run_refresh().await,
                "update" => update::run_update_cli(&argv[1..]).await,
                "issues" => issues::run_cli(&argv[1..]),
                "version" | "--version" | "-V" => {
                    println!("ntulearn-mcp {}", update::current_version());
                    std::process::exit(0);
                }
                other => {
                    let _ = other;
                    std::process::exit(cli::run_cli(&argv).await);
                }
            };
            std::process::exit(code);
        }
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "warn,ntulearn_mcp=info".to_string()),
        )
        .with_writer(std::io::stderr)
        .init();

    let state = build_state().await?;

    // Freshly captured cookies (401 self-heal login window, background
    // re-capture) are hot-swapped into the live client the moment they land,
    // so the agent's retry succeeds without another round trip.
    {
        let state_for_hook = state.clone();
        crate::capture::set_install_hook(Box::new(move |val: String| {
            let state = state_for_hook.clone();
            tokio::spawn(async move {
                state.client.install_cookie(&val).await;
            });
        }));
    }

    if state.client.cookie_is_empty() {
        if capture::capture_profile_ready() {
            eprintln!("ntulearn-mcp: no current session; signing in silently with the saved NTU login.");
        } else {
            eprintln!(
                "WARNING: not logged in to NTULearn. Run `ntulearn-mcp setup` once, or ask \
                 your agent to call the ntulearn_login tool."
            );
        }
    }

    let info = ServerInfo {
        name: "ntulearn-mcp".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        description: Some(
            "Blackboard Learn (NTU NTULearn) REST API via Model Context Protocol. \
             Read your enrolled courses, course content trees, announcements, gradebook, \
             messages, groups, calendar and due dates; download and read attached files."
                .to_string(),
        ),
        authors: None,
        homepage: None,
        license: None,
        repository: None,
    };

    let capabilities = ServerCapabilities {
        tools: Some(crate::mcp::ToolsCapability { list_changed: Some(false) }),
        resources: Some(crate::mcp::ResourcesCapability {
            subscribe: Some(false),
            list_changed: Some(false),
        }),
        prompts: Some(crate::mcp::PromptsCapability { list_changed: Some(false) }),
    };

    let server = McpServer::new(info, capabilities)
        .with_tool_handler(Arc::new(NtuToolHandler { state: state.clone() }))
        .with_resource_handler(Arc::new(NtuResourceHandler { state: state.clone() }))
        .with_prompt_handler(Arc::new(NtuPromptHandler));

    // Update notifications + (opt-in) auto-update. Runs in the background,
    // throttled to one network check per 24h; every failure is logged to
    // stderr and swallowed — it must never affect the server. With
    // auto-update ON the new binary is downloaded, checksum-verified, and
    // atomically swapped in; it activates on the next host restart.
    tokio::spawn(update::background_updater(state.update_notice.clone()));

    // Auto-file GitHub issues for unexpected errors (opt-in; see issues.rs).
    // Recording happens on every tool-call Err path; this task does the
    // network filing off the hot path. The panic hook files immediately.
    tokio::spawn(issues::background_reporter());
    issues::install_panic_hook();

    // Pre-warm the static tool catalog before entering the stdio loop so
    // the first `tools/list` (tool discovery) is already a cache hit —
    // zero cold start on the very first client connect/reload.
    let _ = schemas::all_tools();

    // Proactive cookie maintenance. NTULearn sessions are absolute ~3 h
    // (server-controlled; there is no client-side "remember me" on this SAML
    // flow), so instead of extending the cookie we refresh it ahead of time:
    // re-resolve from all sources every minute and, when the in-memory value is
    // nearly expired, hot-swap to a fresher one (a signed-in Firefox profile's
    // cookies.sqlite is the usual auto-source). Long-lived server processes
    // never surface a 401 and never need a manual `setup` re-run.
    //
    // When no source has a live cookie, fall back to a SILENT re-capture: the
    // persistent capture profile holds the NTU SSO session, so a headless
    // browser re-authenticates through SSO with no window and no prompt. It
    // never opens a visible window — when the SSO session itself has expired,
    // the next tool call returns an error telling the agent to call
    // ntulearn_login. Guarded by the known-good marker so a fresh install never
    // launches a browser, and rate-limited to one attempt per 30 min. When the
    // silent sign-in times out, a machine-wide backoff marker stops every
    // process from retrying for 6 h (no headless-browser churn while nobody
    // can log in).
    {
        let state = state.clone();
        tokio::spawn(async move {
            let threshold_secs = 45 * 60; // start refreshing under 45 min left
            let base = std::env::var("NTULEARN_BASE_URL")
                .unwrap_or_else(|_| BASE_URL.to_string());
            let mut tick = tokio::time::interval(Duration::from_secs(60));
            let mut last_capture: Option<std::time::Instant> = None;
            loop {
                tick.tick().await;
                state.client.proactively_refresh(threshold_secs).await;

                let current = state.client.current_cookie_value().await;
                let needs_capture = current.is_empty()
                    || cookie::should_refresh(&current, threshold_secs);
                if !needs_capture
                    || !capture::capture_profile_ready()
                    || capture::silent_capture_futile()
                {
                    continue;
                }
                let cooled_down = last_capture
                    .map(|t| t.elapsed() >= Duration::from_secs(30 * 60))
                    .unwrap_or(true);
                if !cooled_down {
                    continue;
                }
                last_capture = Some(std::time::Instant::now());
                tracing::info!("cookie nearly expired and no live source; silent re-capture starting");
                match capture::capture_cookie(&base, capture::CaptureMode::Silent, true).await {
                    Ok((val, browser)) => {
                        state.client.install_cookie(&val).await;
                        tracing::info!("auto re-capture via {browser} succeeded; session healed");
                    }
                    Err(e) => {
                        tracing::warn!("auto re-capture failed: {e}");
                    }
                }
            }
        });
    }

    server.run_stdio().await?;
    Ok(())
}
