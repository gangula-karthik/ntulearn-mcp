
//! ToolHandler for the 21 NTULearn tools: schema listing + dispatch.

use std::sync::Arc;

use ultrafast_mcp::{
    ListToolsRequest, ListToolsResponse, MCPResult, ToolCall, ToolContent, ToolHandler,
    ToolResult,
};

use crate::handlers;
use crate::schemas;
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

#[async_trait::async_trait]
impl ToolHandler for NtuToolHandler {
    async fn list_tools(&self, _req: ListToolsRequest) -> MCPResult<ListToolsResponse> {
        Ok(ListToolsResponse { tools: schemas::all_tools(), next_cursor: None })
    }

    async fn handle_tool_call(&self, call: ToolCall) -> MCPResult<ToolResult> {
        let args = call.arguments.unwrap_or_default();
        Ok(match handlers::dispatch(&self.state, &call.name, &args).await {
            Ok(contents) => content_result(contents),
            Err(msg) => err_result(msg),
        })
    }
}
