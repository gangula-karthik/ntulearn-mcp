//! CLI command spec types — pure data, owned by the registry.
//!
//! Moved verbatim from `cli.rs`: a feature module declares its `CmdSpec`
//! next to its MCP schema and handler, and the CLI parser / `help` renderer
//! consume them through `registry::lookup_cli` / `registry::all`. `cli.rs`
//! keeps the behavior (parsing, rendering, dispatch); this file is the shape.

#[derive(Clone, Copy, PartialEq)]
pub enum ArgKind {
    Str,
    Int,
    Bool,
    Arr,
    Enum(&'static [&'static str]),
}

pub struct ArgSpec {
    pub flag: &'static str,
    pub kind: ArgKind,
    pub required: bool,
    pub hint: &'static str,
}

pub struct CmdSpec {
    /// MCP tool name without the `ntulearn_` prefix (dispatch key).
    pub short: &'static str,
    /// Short CLI alias (token-cheap to type).
    pub alias: &'static str,
    pub summary: &'static str,
    pub args: &'static [ArgSpec],
    /// Payload key that holds the item list for compact rendering.
    pub list_key: &'static str,
    /// Preferred compact columns (subset of item keys, in order).
    pub cols: &'static [&'static str],
}

pub const fn s(flag: &'static str, required: bool, hint: &'static str) -> ArgSpec {
    ArgSpec {
        flag,
        kind: ArgKind::Str,
        required,
        hint,
    }
}
pub const fn i(flag: &'static str, hint: &'static str) -> ArgSpec {
    ArgSpec {
        flag,
        kind: ArgKind::Int,
        required: false,
        hint,
    }
}
pub const fn b(flag: &'static str, hint: &'static str) -> ArgSpec {
    ArgSpec {
        flag,
        kind: ArgKind::Bool,
        required: false,
        hint,
    }
}
pub const fn a(flag: &'static str, hint: &'static str) -> ArgSpec {
    ArgSpec {
        flag,
        kind: ArgKind::Arr,
        required: false,
        hint,
    }
}
pub const fn e(flag: &'static str, vals: &'static [&'static str], hint: &'static str) -> ArgSpec {
    ArgSpec {
        flag,
        kind: ArgKind::Enum(vals),
        required: false,
        hint,
    }
}
