//! The tool registry — the single place a tool is declared.
//!
//! Every NTULearn tool (MCP schema, CLI command, markdown renderer, async
//! handler) is one [`ToolDef`] entry in one feature module under
//! `registry/features/`. The four consumers — `tools/list` (MCP schema
//! listing), `handlers::dispatch_value` (execution), `render_for_tool`
//! (markdown rendering) and the CLI parser/`help` — all read from this
//! registry, so a contributor wires a new tool in exactly one file.
//!
//! Adding a tool (see docs/EXTENDING.md for the full walkthrough):
//!
//! ```ignore
//! // src/registry/features/my_feature.rs
//! crate::registry::tool! {
//!     short: "my_thing",                       // dispatch key, no ntulearn_ prefix
//!     mcp: crate::mcp::Tool::new(/* ... */),   // schema shown to MCP hosts
//!     cli: Some(crate::registry::cli_spec::CmdSpec { /* ... */ }),
//!     markdown: Some(render::md_my_thing),     // payload -> markdown, or None
//!     handler: handlers::h_my_thing,           // async fn(&AppState, &Value)
//! }
//! ```
//!
//! then one `extend(my_feature::tools());` line in `features/mod.rs`.
//! Compile-time statics only: no trait objects beyond the one boxed future
//! per call, no dynamic loading, no new dependencies.

pub mod cli_spec;
pub mod features;

use std::sync::OnceLock;

use futures_util::future::BoxFuture;
use serde_json::Value;

use crate::mcp::Tool;
use crate::AppState;
use cli_spec::CmdSpec;

/// What a handler returns: the raw JSON payload. Rendering (TOON / JSON /
/// markdown / compact TSV) happens downstream, so one handler serves every
/// output format.
pub type HandlerResult = Result<Value, String>;

/// A handler is any `async fn(&AppState, &Value) -> Result<Value, String>`.
/// Stored as a plain fn pointer returning a boxed future — the same shape
/// every `handlers::h_*` function already has.
pub type HandlerFn = for<'a> fn(&'a AppState, &'a Value) -> BoxFuture<'a, HandlerResult>;

/// Markdown renderer: payload in, human-readable markdown out. `None` means
/// the tool has no bespoke markdown (the MCP path then emits TOON/JSON only,
/// exactly like the old `render_for_tool` fallback arm).
pub type MarkdownFn = fn(&Value) -> String;

/// One tool, end to end. This is the extension interface: everything a tool
/// needs, in one struct, declared in one place.
pub struct ToolDef {
    /// Dispatch key WITHOUT the `ntulearn_` prefix (e.g. `kaltura_list`).
    /// Must be unique; `features::build` panics on duplicates at startup.
    pub short: &'static str,
    /// Full MCP tool definition: name (`ntulearn_<short>`), description,
    /// input schema, annotations.
    pub tool: Tool,
    /// CLI command spec (alias, args, list_key, compact columns).
    /// `None` = MCP-only tool, invisible to the CLI.
    pub cli: Option<CmdSpec>,
    /// Optional markdown renderer for `response_format="markdown"`.
    pub markdown: Option<MarkdownFn>,
    /// The async handler both the MCP server and the CLI call.
    pub handler: HandlerFn,
}

/// Declare a [`ToolDef`] without repeating struct plumbing.
///
/// `handler` takes any `async fn(&AppState, &Value) -> Result<Value, String>`
/// path or non-capturing closure; the macro boxes the future.
macro_rules! tool {
    (
        short: $short:expr,
        mcp: $mcp:expr,
        cli: $cli:expr,
        markdown: $markdown:expr,
        handler: $handler:expr $(,)?
    ) => {
        $crate::registry::ToolDef {
            short: $short,
            tool: $mcp,
            cli: $cli,
            markdown: $markdown,
            handler: |state, args| ::std::boxed::Box::pin($handler(state, args)),
        }
    };
}
pub(crate) use tool;

static REGISTRY: OnceLock<Vec<ToolDef>> = OnceLock::new();

/// All registered tools, built once and shared. The first caller pays the
/// build; everyone after (concurrent `tools/list` included) gets a static
/// slice — same contract the old `schemas::all_tools` OnceLock had.
pub fn all() -> &'static [ToolDef] {
    REGISTRY.get_or_init(features::build)
}

/// Look a tool up by its short name (dispatch key).
pub fn lookup(short: &str) -> Option<&'static ToolDef> {
    all().iter().find(|d| d.short == short)
}

/// Resolve a CLI token (alias like `grades` or short name like
/// `ntulearn_get_gradebook`) to its command spec.
pub fn lookup_cli(token: &str) -> Option<&'static CmdSpec> {
    let stripped = token.strip_prefix("ntulearn_").unwrap_or(token);
    all().iter().find_map(|d| {
        let cli = d.cli.as_ref()?;
        (cli.alias == token || d.short == stripped).then_some(cli)
    })
}

/// Markdown renderer for a short name, if the tool declared one.
pub fn markdown_for(short: &str) -> Option<MarkdownFn> {
    lookup(short).and_then(|d| d.markdown)
}

/// Dispatch a short name + args to its handler. CLI and MCP share this.
pub async fn dispatch_value(state: &AppState, short: &str, args: &Value) -> HandlerResult {
    match lookup(short) {
        Some(def) => (def.handler)(state, args).await,
        None => Err(format!("unknown tool: ntulearn_{short}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every tool registers with a unique short name and a name that matches
    /// `ntulearn_<short>` — guards the dispatch key invariant.
    #[test]
    fn short_names_unique_and_tool_names_consistent() {
        let all = all();
        assert!(!all.is_empty());
        let mut seen = std::collections::HashSet::new();
        for d in all {
            assert!(
                seen.insert(d.short),
                "duplicate short name in registry: {}",
                d.short
            );
            assert_eq!(d.tool.name, format!("ntulearn_{}", d.short));
            if let Some(cli) = &d.cli {
                assert!(!cli.alias.is_empty(), "empty CLI alias for {}", d.short);
            }
        }
    }

    /// Every CLI spec resolves back through `lookup_cli`, by alias and by
    /// short name — the two spellings the CLI parser accepts.
    #[test]
    fn cli_specs_resolve_by_alias_and_short() {
        for d in all() {
            if let Some(cli) = &d.cli {
                let by_alias = lookup_cli(cli.alias);
                assert!(by_alias.is_some(), "alias {} not found", cli.alias);
                let by_short = lookup_cli(&format!("ntulearn_{}", d.short));
                assert!(by_short.is_some(), "short {} not found", d.short);
            }
        }
    }
}
