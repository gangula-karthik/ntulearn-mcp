
//! NTULearn MCP server — Blackboard Learn REST API wrapper, Rust edition.
//!
//! Built on `ultrafast-mcp` (https://github.com/techgopal/ultrafast-mcp).
//! Registered as "ntulearn-mcp" with tool/resource/prompt parity with the
//! original Python implementation.

#![allow(dead_code)]

mod bench;
mod cache;
mod cli;
mod capture;
mod client;
mod cookie;
mod handlers;
mod parsers;
mod prompts;
mod render;
mod resources;
mod schemas;
mod setup;
mod tools;
mod update;

use std::path::PathBuf;
use std::time::Duration;
use std::sync::Arc;

use ultrafast_mcp::{ServerCapabilities, ServerInfo, UltraFastServer};

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
    if state.client.cookie_is_empty() {
        eprintln!(
            "WARNING: no BbRouter cookie found. Set NTULEARN_COOKIE (or log into NTULearn \
             in a supported browser). Authenticated calls will return 401."
        );
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
        tools: Some(ultrafast_mcp::ToolsCapability { list_changed: Some(false) }),
        resources: Some(ultrafast_mcp::ResourcesCapability {
            subscribe: Some(false),
            list_changed: Some(false),
        }),
        prompts: Some(ultrafast_mcp::PromptsCapability { list_changed: Some(false) }),
        ..Default::default()
    };

    let server = UltraFastServer::new(info, capabilities)
        .with_tool_handler(Arc::new(NtuToolHandler { state: state.clone() }))
        .with_resource_handler(Arc::new(NtuResourceHandler { state: state.clone() }))
        .with_prompt_handler(Arc::new(NtuPromptHandler));

    // Update notifications + (opt-in) auto-update. Runs in the background,
    // throttled to one network check per 24h; every failure is logged to
    // stderr and swallowed — it must never affect the server. With
    // auto-update ON the new binary is downloaded, checksum-verified, and
    // atomically swapped in; it activates on the next host restart.
    tokio::spawn(update::background_updater(state.update_notice.clone()));

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
    {
        let state = state.clone();
        tokio::spawn(async move {
            let threshold_secs = 45 * 60; // start refreshing under 45 min left
            let mut tick = tokio::time::interval(Duration::from_secs(60));
            loop {
                tick.tick().await;
                state.client.proactively_refresh(threshold_secs).await;
            }
        });
    }

    server.run_stdio().await?;
    Ok(())
}
