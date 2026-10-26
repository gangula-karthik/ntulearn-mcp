//! Minimal MCP (Model Context Protocol) stdio server.
//!
//! Replaces the `ultrafast-mcp` framework, whose "http" feature (the only
//! feature combination that compiles — its stdio re-exports are unconditionally
//! gated on the http transport) dragged in axum, hyper, h2, tower-http,
//! async-compression, brotli and ring. This server speaks plain JSON-RPC 2.0
//! over newline-delimited stdio, which is the only transport MCP hosts use
//! with `ntulearn-mcp` (they spawn it bare).
//!
//! Wire format follows the MCP specification: camelCase field names, one JSON
//! message per line, requests carry an `id`, notifications do not and never
//! get a reply.

use std::sync::Arc;

use serde::Serialize;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;

use crate::AppState;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Handler error type: any `anyhow::Error` converts into a wire error.
#[derive(Debug)]
pub struct McpError(pub String);

impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for McpError {}

impl From<anyhow::Error> for McpError {
    fn from(e: anyhow::Error) -> Self {
        McpError(format!("{e:#}"))
    }
}

pub type MCPResult<T> = Result<T, McpError>;

// ---------------------------------------------------------------------------
// Tool types
// ---------------------------------------------------------------------------

/// A single content block. The handlers only ever emit text.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ToolContent {
    Text { text: String },
}

impl ToolContent {
    pub fn text(text: impl Into<String>) -> Self {
        ToolContent::Text { text: text.into() }
    }
}

/// Result of a `tools/call`: content blocks plus the error flag.
#[derive(Clone, Debug, Serialize)]
pub struct ToolResult {
    pub content: Vec<ToolContent>,
    #[serde(rename = "isError", skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

/// Tool metadata hints for clients (MCP `annotations` object).
#[derive(Clone, Debug, Default, Serialize)]
pub struct ToolAnnotations {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(rename = "readOnlyHint", skip_serializing_if = "Option::is_none")]
    pub read_only_hint: Option<bool>,
    #[serde(rename = "destructiveHint", skip_serializing_if = "Option::is_none")]
    pub destructive_hint: Option<bool>,
    #[serde(rename = "idempotentHint", skip_serializing_if = "Option::is_none")]
    pub idempotent_hint: Option<bool>,
    #[serde(rename = "openWorldHint", skip_serializing_if = "Option::is_none")]
    pub open_world_hint: Option<bool>,
}

/// One tool definition, as returned by `tools/list`.
#[derive(Clone, Debug, Serialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<ToolAnnotations>,
}

impl Tool {
    pub fn new(name: String, description: String, input_schema: Value) -> Self {
        Tool { name, description, input_schema, annotations: None }
    }

    pub fn with_annotations(mut self, annotations: ToolAnnotations) -> Self {
        self.annotations = Some(annotations);
        self
    }
}

#[derive(Debug, Default)]
pub struct ListToolsRequest;
#[derive(Debug, Serialize)]
pub struct ListToolsResponse {
    pub tools: Vec<Tool>,
    #[serde(rename = "nextCursor", skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// Params of a `tools/call`.
#[derive(Debug)]
pub struct ToolCall {
    pub name: String,
    pub arguments: Option<Value>,
}

#[async_trait::async_trait]
pub trait ToolHandler: Send + Sync {
    async fn list_tools(&self, req: ListToolsRequest) -> MCPResult<ListToolsResponse>;
    async fn handle_tool_call(&self, call: ToolCall) -> MCPResult<ToolResult>;
}

// ---------------------------------------------------------------------------
// Resource types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct ResourceContent {
    pub uri: String,
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    pub text: String,
}

impl ResourceContent {
    pub fn text(uri: impl Into<String>, text: impl Into<String>) -> Self {
        ResourceContent { uri: uri.into(), mime_type: None, text: text.into() }
    }
}

#[derive(Debug, Serialize)]
pub struct ReadResourceResponse {
    pub contents: Vec<ResourceContent>,
}

#[derive(Debug, Serialize)]
pub struct ResourceTemplate {
    #[serde(rename = "uriTemplate")]
    pub uri_template: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct ListResourcesResponse {
    pub resources: Vec<Value>,
    #[serde(rename = "nextCursor", skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct ListResourceTemplatesResponse {
    #[serde(rename = "resourceTemplates")]
    pub resource_templates: Vec<ResourceTemplate>,
    #[serde(rename = "nextCursor", skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Default)]
pub struct ListResourcesRequest;
#[derive(Debug, Default)]
pub struct ListResourceTemplatesRequest;

#[derive(Debug)]
pub struct ReadResourceRequest {
    pub uri: String,
}

#[async_trait::async_trait]
pub trait ResourceHandler: Send + Sync {
    async fn read_resource(&self, request: ReadResourceRequest) -> MCPResult<ReadResourceResponse>;
    async fn list_resources(&self, request: ListResourcesRequest) -> MCPResult<ListResourcesResponse>;
    async fn list_resource_templates(
        &self,
        request: ListResourceTemplatesRequest,
    ) -> MCPResult<ListResourceTemplatesResponse>;
}

// ---------------------------------------------------------------------------
// Prompt types
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type")]
pub enum PromptContent {
    Text { text: String },
}

impl PromptContent {
    pub fn text(text: impl Into<String>) -> Self {
        PromptContent::Text { text: text.into() }
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PromptRole {
    User,
    Assistant,
}

#[derive(Clone, Debug, Serialize)]
pub struct PromptMessage {
    pub role: PromptRole,
    pub content: PromptContent,
}

#[derive(Clone, Debug, Serialize)]
pub struct PromptArgument {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
}

impl PromptArgument {
    pub fn new(name: String) -> Self {
        PromptArgument { name, description: None, required: None }
    }

    pub fn with_description(mut self, description: String) -> Self {
        self.description = Some(description);
        self
    }

    pub fn required(mut self, required: bool) -> Self {
        self.required = Some(required);
        self
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Prompt {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Vec<PromptArgument>>,
}

impl Prompt {
    pub fn new(name: String) -> Self {
        Prompt { name, description: None, arguments: None }
    }

    pub fn with_description(mut self, description: String) -> Self {
        self.description = Some(description);
        self
    }

    pub fn with_arguments(mut self, arguments: Vec<PromptArgument>) -> Self {
        self.arguments = Some(arguments);
        self
    }
}

#[derive(Debug, Default, Serialize)]
pub struct ListPromptsResponse {
    pub prompts: Vec<Prompt>,
    #[serde(rename = "nextCursor", skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct GetPromptResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub messages: Vec<PromptMessage>,
}

#[derive(Debug, Default)]
pub struct ListPromptsRequest;

#[derive(Debug)]
pub struct GetPromptRequest {
    pub name: String,
    pub arguments: Option<Value>,
}

#[async_trait::async_trait]
pub trait PromptHandler: Send + Sync {
    async fn list_prompts(&self, req: ListPromptsRequest) -> MCPResult<ListPromptsResponse>;
    async fn get_prompt(&self, req: GetPromptRequest) -> MCPResult<GetPromptResponse>;
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authors: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct ToolsCapability {
    #[serde(rename = "listChanged", skip_serializing_if = "Option::is_none")]
    pub list_changed: Option<bool>,
}

#[derive(Debug, Default, Serialize)]
pub struct ResourcesCapability {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscribe: Option<bool>,
    #[serde(rename = "listChanged", skip_serializing_if = "Option::is_none")]
    pub list_changed: Option<bool>,
}

#[derive(Debug, Default, Serialize)]
pub struct PromptsCapability {
    #[serde(rename = "listChanged", skip_serializing_if = "Option::is_none")]
    pub list_changed: Option<bool>,
}

#[derive(Debug, Default, Serialize)]
pub struct ServerCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolsCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourcesCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompts: Option<PromptsCapability>,
}

/// The stdio MCP server. Build with [`McpServer::new`], attach handlers with
/// the `with_*` builders, then [`run_stdio`](McpServer::run_stdio).
pub struct McpServer {
    info: ServerInfo,
    capabilities: ServerCapabilities,
    tool_handler: Option<Arc<dyn ToolHandler>>,
    resource_handler: Option<Arc<dyn ResourceHandler>>,
    prompt_handler: Option<Arc<dyn PromptHandler>>,
}

impl McpServer {
    pub fn new(info: ServerInfo, capabilities: ServerCapabilities) -> Self {
        McpServer {
            info,
            capabilities,
            tool_handler: None,
            resource_handler: None,
            prompt_handler: None,
        }
    }

    pub fn with_tool_handler(mut self, handler: Arc<dyn ToolHandler>) -> Self {
        self.tool_handler = Some(handler);
        self
    }

    pub fn with_resource_handler(mut self, handler: Arc<dyn ResourceHandler>) -> Self {
        self.resource_handler = Some(handler);
        self
    }

    pub fn with_prompt_handler(mut self, handler: Arc<dyn PromptHandler>) -> Self {
        self.prompt_handler = Some(handler);
        self
    }

    /// Serve newline-delimited JSON-RPC 2.0 on stdin/stdout until EOF.
    pub async fn run_stdio(self) -> anyhow::Result<()> {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let server = Arc::new(self);
        let mut stdout: tokio::io::BufWriter<tokio::io::Stdout> =
            tokio::io::BufWriter::new(tokio::io::stdout());
        let stdin = tokio::io::stdin();
        let mut lines = BufReader::new(stdin).lines();
        while let Some(line) = lines.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }
            let msg: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(e) => {
                    Self::write(&mut stdout, &json!({
                        "jsonrpc": "2.0", "id": null,
                        "error": {"code": -32700, "message": format!("Parse error: {e}")}
                    }))
                    .await?;
                    continue;
                }
            };
            let method = msg.get("method").and_then(Value::as_str).unwrap_or_default();
            let id = msg.get("id").cloned();
            if id.is_none() {
                continue; // notification — never reply
            }
            let id = id.unwrap();
            let params = msg.get("params").cloned().unwrap_or(Value::Null);
            let result = server.handle(method, &params).await;
            let reply = match result {
                Ok(v) => json!({"jsonrpc": "2.0", "id": id, "result": v}),
                Err(e) => {
                    // -32601 method not found, -32603 internal error otherwise.
                    let code = if e.0 == "method not found" { -32601 } else { -32603 };
                    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": e.0}})
                }
            };
            Self::write(&mut stdout, &reply).await?;
        }
        stdout.flush().await?;
        Ok(())
    }

    async fn write<W: tokio::io::AsyncWrite + Unpin>(
        w: &mut W,
        value: &Value,
    ) -> anyhow::Result<()> {
        let mut s = serde_json::to_string(value)?;
        s.push('\n');
        w.write_all(s.as_bytes()).await?;
        w.flush().await?;
        Ok(())
    }

    /// Dispatch one request. Returns the `result` object or an error.
    async fn handle(&self, method: &str, params: &Value) -> MCPResult<Value> {
        match method {
            "initialize" => {
                // Echo the client's protocol version (spec-blessed when we
                // can speak it); fall back to the 2024-11-05 baseline.
                let version = params
                    .get("protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or("2024-11-05");
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": self.capabilities,
                    "serverInfo": self.info,
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => {
                let handler = self.tool_handler.as_ref().ok_or_else(|| no_handler("tools"))?;
                let resp = handler.list_tools(ListToolsRequest).await?;
                to_value(&resp)
            }
            "tools/call" => {
                let handler = self.tool_handler.as_ref().ok_or_else(|| no_handler("tools"))?;
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let call = ToolCall { name, arguments: params.get("arguments").cloned() };
                let resp = handler.handle_tool_call(call).await?;
                to_value(&resp)
            }
            "resources/list" => {
                let handler = self.resource_handler.as_ref().ok_or_else(|| no_handler("resources"))?;
                let resp = handler.list_resources(ListResourcesRequest).await?;
                to_value(&resp)
            }
            "resources/templates/list" => {
                let handler = self.resource_handler.as_ref().ok_or_else(|| no_handler("resources"))?;
                let resp = handler
                    .list_resource_templates(ListResourceTemplatesRequest)
                    .await?;
                to_value(&resp)
            }
            "resources/read" => {
                let handler = self.resource_handler.as_ref().ok_or_else(|| no_handler("resources"))?;
                let uri = params
                    .get("uri")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let resp = handler.read_resource(ReadResourceRequest { uri }).await?;
                to_value(&resp)
            }
            "prompts/list" => {
                let handler = self.prompt_handler.as_ref().ok_or_else(|| no_handler("prompts"))?;
                let resp = handler.list_prompts(ListPromptsRequest).await?;
                to_value(&resp)
            }
            "prompts/get" => {
                let handler = self.prompt_handler.as_ref().ok_or_else(|| no_handler("prompts"))?;
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let req = GetPromptRequest {
                    name,
                    arguments: params.get("arguments").cloned(),
                };
                let resp = handler.get_prompt(req).await?;
                to_value(&resp)
            }
            _ => Err(McpError("method not found".to_string())),
        }
    }
}

fn no_handler(kind: &str) -> McpError {
    McpError(format!("no {kind} handler installed"))
}

fn to_value<T: Serialize>(v: &T) -> MCPResult<Value> {
    serde_json::to_value(v).map_err(|e| McpError(format!("serialization failed: {e}")))
}

// The AppState import is only here to document the intended wiring in main.rs:
// handlers hold `Arc<AppState>`; the server itself is transport-only.
#[allow(dead_code)]
fn _assert_state_type(state: &Arc<AppState>) {
    let _ = state;
}

// ---------------------------------------------------------------------------
// Tests: wire-format round trips for the JSON-RPC loop pieces.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_result_serializes_camel_case() {
        let r = ToolResult { content: vec![ToolContent::text("hi")], is_error: Some(false) };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["content"][0]["type"], "text");
        assert_eq!(v["content"][0]["text"], "hi");
        assert_eq!(v["isError"], false);
    }

    #[test]
    fn tool_schema_fields() {
        let t = Tool::new("x".into(), "d".into(), json!({"type": "object"}))
            .with_annotations(ToolAnnotations {
                read_only_hint: Some(true),
                ..Default::default()
            });
        let v = serde_json::to_value(&t).unwrap();
        assert_eq!(v["name"], "x");
        assert_eq!(v["inputSchema"]["type"], "object");
        assert_eq!(v["annotations"]["readOnlyHint"], true);
    }

    #[test]
    fn prompt_types_serialize() {
        let p = Prompt::new("n".into())
            .with_description("d".into())
            .with_arguments(vec![PromptArgument::new("a".into()).required(false)]);
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["arguments"][0]["name"], "a");
        let m = PromptMessage {
            role: PromptRole::User,
            content: PromptContent::text("hello"),
        };
        let mv = serde_json::to_value(&m).unwrap();
        assert_eq!(mv["role"], "user");
        assert_eq!(mv["content"]["text"], "hello");
    }

    #[test]
    fn resource_template_serializes() {
        let t = ResourceTemplate {
            uri_template: "ntulearn://courses/{course_id}".into(),
            name: "brief".into(),
            description: None,
            mime_type: Some("application/json".into()),
        };
        let v = serde_json::to_value(&t).unwrap();
        assert_eq!(v["uriTemplate"], "ntulearn://courses/{course_id}");
        assert_eq!(v["mimeType"], "application/json");
    }

    struct EchoHandler;

    #[async_trait::async_trait]
    impl ToolHandler for EchoHandler {
        async fn list_tools(&self, _req: ListToolsRequest) -> MCPResult<ListToolsResponse> {
            Ok(ListToolsResponse {
                tools: vec![Tool::new("echo".into(), "echo".into(), json!({"type": "object"}))],
                next_cursor: None,
            })
        }

        async fn handle_tool_call(&self, call: ToolCall) -> MCPResult<ToolResult> {
            if call.name == "boom" {
                return Err(McpError("kaboom".into()));
            }
            Ok(ToolResult {
                content: vec![ToolContent::text(format!(
                    "{}",
                    call.arguments.unwrap_or(Value::Null)
                ))],
                is_error: Some(false),
            })
        }
    }

    #[tokio::test]
    async fn dispatch_covers_the_core_methods() {
        let server = McpServer::new(
            ServerInfo {
                name: "t".into(),
                version: "1".into(),
                description: None,
                authors: None,
                homepage: None,
                license: None,
                repository: None,
            },
            ServerCapabilities::default(),
        )
        .with_tool_handler(Arc::new(EchoHandler));

        // initialize echoes the client's protocol version
        let r = server
            .handle("initialize", &json!({"protocolVersion": "2025-06-18"}))
            .await
            .unwrap();
        assert_eq!(r["protocolVersion"], "2025-06-18");
        assert_eq!(r["serverInfo"]["name"], "t");

        // tools/list
        let r = server.handle("tools/list", &Value::Null).await.unwrap();
        assert_eq!(r["tools"][0]["name"], "echo");

        // tools/call
        let r = server
            .handle("tools/call", &json!({"name": "echo", "arguments": {"a": 1}}))
            .await
            .unwrap();
        assert_eq!(r["content"][0]["text"], r#"{"a":1}"#);
        assert_eq!(r["isError"], false);

        // unknown method
        assert!(server.handle("bogus/method", &Value::Null).await.is_err());

        // handler error propagates as a wire error
        let e = server.handle("tools/call", &json!({"name": "boom"})).await.unwrap_err();
        assert_eq!(e.0, "kaboom");
    }
}
