
//! ToolHandler for the 30 NTULearn tools: schema listing + dispatch.

use std::sync::Arc;

use crate::mcp::{
    ListToolsRequest, ListToolsResponse, MCPResult, Tool, ToolAnnotations, ToolCall,
    ToolContent, ToolHandler, ToolResult,
};

use crate::handlers;
use crate::issues;
use crate::registry;
use crate::AppState;

/// Build a text-only successful tool result.
pub fn text_result(content: String) -> ToolResult {
    ToolResult { content: vec![ToolContent::text(content)], is_error: Some(false) }
}

/// Build an error tool result with an LLM-readable message (mirrors the Python
/// server's "is_error" behaviour).
pub fn err_result(message: String) -> ToolResult {
    ToolResult { content: vec![ToolContent::text(message)], is_error: Some(true) }
}

/// Multi-content success (e.g. file reads that return several blobs).
pub fn content_result(content: Vec<ToolContent>) -> ToolResult {
    ToolResult { content, is_error: Some(false) }
}

pub struct NtuToolHandler {
    pub state: Arc<AppState>,
}

/// The `ntulearn_login` schema (verbatim from the pre-registry schemas.rs).
/// Login is the single tool not in the registry: see `list_tools`.
pub(crate) fn login_tool() -> crate::mcp::Tool {
    use serde_json::json;
    Tool::new(
            r####"ntulearn_login"####.to_string(),
            r####"Log in to NTULearn. Call this when another ntulearn_* tool reports 'Not logged in' / HTTP 401, or before first use. If a saved NTU login exists it renews the session silently; otherwise it opens a browser window where the USER must log in with their NTU account — tell them to do so (and to answer Yes to 'Stay signed in?'). Note: when a tool fails with 401 and says a login window 'has just opened', the window is already up — just wait for the user to log in, then retry that tool (no need to call this first). Returns once logged in, or after ~45 s with status 'waiting_for_user' while the window stays open; then call it again to confirm. Example: ntulearn_login()"####.to_string(),
            json!({"type":"object","properties":{"force":{"type":"boolean","description":"Log in again even if the current session is still valid.","default":false}},"additionalProperties":false}),
        )
        .with_annotations(ToolAnnotations {
            title: Some(r####"Log in to NTULearn"####.to_string()),
            read_only_hint: Some(false),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
        })
}

#[async_trait::async_trait]
impl ToolHandler for NtuToolHandler {
    async fn list_tools(&self, _req: ListToolsRequest) -> MCPResult<ListToolsResponse> {
        // Registry tools plus the one hand-wired special case (login), kept
        // here next to its dispatch: its handler takes Arc<AppState> because
        // the login capture may outlive the call while the user is still
        // typing their password.
        let mut tools: Vec<crate::mcp::Tool> =
            registry::all().iter().map(|d| d.tool.clone()).collect();
        tools.push(login_tool());
        Ok(ListToolsResponse { tools, next_cursor: None })
    }

    async fn handle_tool_call(&self, call: ToolCall) -> MCPResult<ToolResult> {
        let args = call.arguments.unwrap_or_default();
        // Login needs the shared Arc: the capture may outlive this call while
        // the user is still typing their password.
        let outcome = if call.name == "ntulearn_login" {
            handlers::login(self.state.clone(), &args).await.map(|t| vec![ToolContent::text(t)])
        } else {
            handlers::dispatch(&self.state, &call.name, &args).await
        };
        let mut result = match outcome {
            Ok(contents) => content_result(contents),
            Err(msg) => {
                // Unexpected errors (not auth/input/network) are counted for
                // the opt-in GitHub issue auto-filer. See issues.rs.
                issues::record(call.name.trim_start_matches("ntulearn_"), &msg);
                err_result(msg)
            }
        };
        // Update notification: appended to the first tool call of the process
        // only (the notice self-clears), throttled to once per day by the
        // background updater. ~25 tokens, once, so the agent can tell the
        // user a new release exists.
        if let Some(notice) = self.state.update_notice.take() {
            result.content.push(ToolContent::text(notice));
        }
        Ok(result)
    }
}
