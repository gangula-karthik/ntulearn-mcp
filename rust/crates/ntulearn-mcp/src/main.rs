
//! NTULearn MCP server — Blackboard Learn REST API wrapper, Rust edition.
//!
//! Built on `ultrafast-mcp` (https://github.com/techgopal/ultrafast-mcp).
//! Registered as "ntulearn-mcp" with tool/resource/prompt parity with the
//! original Python implementation.

#![allow(dead_code)]

mod cache;
mod client;
mod cookie;
mod handlers;
mod parsers;
mod prompts;
mod render;
mod resources;
mod schemas;
mod tools;

use std::path::PathBuf;
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
}

const BASE_URL: &str = "https://ntulearn.ntu.edu.sg";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "warn,ntulearn_mcp=info".to_string()),
        )
        .with_writer(std::io::stderr)
        .init();

    let base_url = std::env::var("NTULEARN_BASE_URL").unwrap_or_else(|_| BASE_URL.to_string());
    let download_dir = std::env::var("NTULEARN_DOWNLOAD_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./downloads"));

    // Cookie: env -> cookie file -> (browser helper). Never touches the OS
    // keychain and never triggers a password prompt.
    let cookie = match resolve_cookie() {
        Some(c) => c,
        None => {
            eprintln!(
                "WARNING: no BbRouter cookie found. Set NTULEARN_COOKIE (or log into NTULearn \
                 in a supported browser). Authenticated calls will return 401."
            );
            String::new()
        }
    };

    let data_cache = Arc::new(DataCache::open()?);
    let client = NTULearnClient::new(base_url, cookie, data_cache)?;

    let state = Arc::new(AppState {
        client,
        download_dir: download_dir.clone(),
    });

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

    server.run_stdio().await?;
    Ok(())
}
