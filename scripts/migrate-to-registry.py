#!/usr/bin/env python3
"""One-time migration: schemas.rs + cli.rs COMMANDS + handlers.rs dispatch /
render_for_tool arms  ->  src/registry/features/*.rs.

Every code block (Tool::new chain, CmdSpec literal, markdown arm body) is
copied BYTE-FOR-BYTE from the old tables; only the surrounding plumbing is
generated. Run from the repo root. Kept in the repo as the record of how
the registry was seeded.
"""

import re
import sys
from pathlib import Path

import os
SRC = Path(os.environ.get("NTULEARN_MIGRATE_SRC",
                          Path(__file__).resolve().parents[1] / "rust" / "crates" / "ntulearn-mcp" / "src"))
OUT = Path(__file__).resolve().parents[1] / "rust" / "crates" / "ntulearn-mcp" / "src" / "registry" / "features"

GROUPS = {  # short-name prefix / explicit membership -> feature module
    "courses": ["list_courses", "get_course_contents", "get_content_tree",
                "search_course_content", "search_all_courses"],
    "files": ["download_file", "read_file_content", "download_course"],
    "calendar": ["get_upcoming", "export_calendar_ics"],
    "grades": ["get_gradebook", "get_gradebook_attempts", "export_gradebook_csv"],
    "messages": ["list_messages", "read_message", "get_announcements"],
    "roster": ["list_course_users", "list_course_groups", "get_group_members"],
    "digest": ["whats_new", "summarize_course"],
}
for pfx, mod in (("fbf_", "fbf"), ("pcv_", "pcv"), ("kaltura_", "kaltura")):
    GROUPS[mod] = pfx  # prefix marker


def scan_balanced(text, open_idx):
    """text[open_idx] is an opener; return index of its matching closer."""
    pairs = {"(": ")", "[": "]", "{": "}"}
    opener = text[open_idx]
    closer = pairs[opener]
    depth, i, n = 0, open_idx, len(text)
    while i < n:
        c = text[i]
        if c == '"':
            i += 1
            while i < n:
                if text[i] == "\\":
                    i += 2
                    continue
                if text[i] == '"':
                    break
                i += 1
        elif c == "r" and i + 1 < n and text[i + 1] in '#"':
            j = i + 1
            hashes = 0
            while j < n and text[j] == "#":
                hashes += 1
                j += 1
            if j < n and text[j] == '"':
                term = '"' + "#" * hashes
                end = text.find(term, j + 1)
                i = end + len(term) - 1
            else:
                i += 1
                continue
        elif text.startswith("//", i):
            nl = text.find("\n", i)
            i = nl if nl >= 0 else n - 1
        elif text.startswith("/*", i):
            i = text.find("*/", i) + 1
        elif c == opener:
            depth += 1
        elif c == closer:
            depth -= 1
            if depth == 0:
                return i
        i += 1
    raise ValueError("unbalanced %s at %d" % (opener, open_idx))


def extract_tool_blocks(text):
    """Each `Tool::new(...)` chain, keyed by short name."""
    out = {}
    idx = 0
    while True:
        i = text.find("Tool::new(", idx)
        if i < 0:
            break
        open_idx = text.find("(", i)
        close_idx = scan_balanced(text, open_idx)
        block = text[i:close_idx + 1]
        rest = text[close_idx + 1:]
        m = re.match(r"\s*\.with_annotations\(", rest)
        if m:
            a_open = close_idx + 1 + m.end() - 1
            a_close = scan_balanced(text, a_open)
            block = text[i:a_close + 1]
            close_idx = a_close
        nm = re.search(r'r#*"ntulearn_([a-z_]+)"', block)
        if not nm:
            raise ValueError("no tool name in block at %d: %r" % (i, block[:80]))
        out[nm.group(1)] = block
        idx = close_idx + 1
    return out


def extract_cmd_blocks(text):
    """Each `CmdSpec { ... }` literal, keyed by short name."""
    out = {}
    idx = 0
    while True:
        i = text.find("CmdSpec {", idx)
        if i < 0:
            break
        open_idx = text.find("{", i)
        close_idx = scan_balanced(text, open_idx)
        block = text[i:close_idx + 1]
        nm = re.search(r'short:\s*"([a-z_]+)"', block)
        if not nm:
            raise ValueError("no short name in CmdSpec at %d" % i)
        out[nm.group(1)] = block
        idx = close_idx + 1
    return out


def parse_markdown_arms(handlers):
    rf_start = handlers.find("fn render_for_tool")
    rf_end = handlers.find("\n}\n", rf_start)
    rf = handlers[rf_start:rf_end]
    md_for = {}
    for m in re.finditer(r'"([a-z_]+)"\s*=>\s*', rf):
        short = m.group(1)
        rest_idx = m.end()
        while rf[rest_idx].isspace():
            rest_idx += 1
        if rf[rest_idx] == "{":
            close = scan_balanced(rf, rest_idx)
            body = rf[rest_idx + 1:close].strip()
            md_for[short] = ("block", body)
        else:
            i = rest_idx
            while i < len(rf):
                if rf[i] in "([{":
                    i = scan_balanced(rf, i)
                elif rf[i] == ",":
                    break
                i += 1
            md_for[short] = ("expr", rf[rest_idx:i].rstrip())
    return md_for


TITLES = {
    "courses": ("Course and content browsing", "courses, content tree, search"),
    "files": ("File download and inline reading", "download_file / read / dl-course"),
    "calendar": ("Calendar and due dates", "get_upcoming / export_calendar_ics"),
    "grades": ("Gradebook", "columns, attempts, CSV export"),
    "messages": ("Course messages and announcements", "mailbox, read, announcements"),
    "roster": ("Rosters and groups", "users, groups, group members"),
    "digest": ("Digests", "whats_new, summarize_course"),
    "fbf": ("FeedbackFruits peer evaluations (LTI)", "status / reviews / rate / comment"),
    "pcv": ("Peerceptiv peer evaluations (LTI)", "status / evals / rate / comment / autofill"),
    "kaltura": ("Kaltura lecture videos (LTI)", "list / transcripts / download"),
}


def emit_module(mod, shorts, mcp_blocks, cmd_blocks, handler_for, md_for):
    title, subtitle = TITLES[mod]
    mcp_fns, md_fns, entries, cmd_consts = [], [], [], []
    helpers, needs_json, needs_render, needs_annotations = set(), False, False, False

    for short in shorts:
        mcp_fns.append("fn mcp_%s() -> Tool {\n    %s\n}" % (short, mcp_blocks[short]))
        if "json!" in mcp_blocks[short]:
            needs_json = True
        if "ToolAnnotations" in mcp_blocks[short]:
            needs_annotations = True
        md_field = "None"
        if short in md_for:
            kind, body = md_for[short]
            md_fns.append("fn md_%s(payload: &serde_json::Value) -> String {\n    %s\n}"
                          % (short, body))
            md_field = "Some(md_%s)" % short
            if "json!" in body:
                needs_json = True
            if "render::" in body:
                needs_render = True
        spec = cmd_blocks[short]
        for helper in "sibae":
            if re.search(r"\b%s\(" % helper, spec):
                helpers.add(helper)
        if "json!" in spec:
            needs_json = True
        cmd_consts.append("const CMD_%s: CmdSpec = %s;" % (short.upper(), spec))
        entries.append(
            "        tool! {\n"
            '            short: "%s",\n'
            "            mcp: mcp_%s(),\n"
            "            cli: Some(CMD_%s),\n"
            "            markdown: %s,\n"
            "            handler: handlers::%s,\n"
            "        }" % (short, short, short.upper(), md_field, handler_for[short]))

    imports = ["use crate::handlers;"]
    if needs_json:
        imports.append("use serde_json::json;")
    imports.append("use crate::mcp::Tool;")
    if needs_annotations:
        imports.append("use crate::mcp::ToolAnnotations;")
    cli_imports = ", ".join(["CmdSpec"] + sorted(helpers))
    imports.append("use crate::registry::cli_spec::%s;" % ("{%s}" % cli_imports))
    imports.append("use crate::registry::tool;")
    imports.append("use crate::registry::ToolDef;")
    if needs_render:
        imports.append("use crate::render;")

    src = "\n".join(imports) + "\n\n"
    src += "// ---------------------------------------------------------------------\n"
    src += "// MCP schemas (verbatim from schemas.rs)\n"
    src += "// ---------------------------------------------------------------------\n\n"
    src += "\n\n".join(mcp_fns) + "\n\n"
    if md_fns:
        src += "// ---------------------------------------------------------------------\n"
        src += "// Markdown renderers (verbatim render_for_tool arms)\n"
        src += "// ---------------------------------------------------------------------\n\n"
        src += "\n\n".join(md_fns) + "\n\n"
    src += "// ---------------------------------------------------------------------\n"
    src += "// CLI command specs (verbatim from cli.rs COMMANDS; consts so the\n"
    src += "// arg arrays are compile-time promoted to 'static)\n"
    src += "// ---------------------------------------------------------------------\n\n"
    src += "\n\n".join(cmd_consts) + "\n\n"
    src += "pub fn tools() -> Vec<ToolDef> {\n    vec![\n"
    src += ",\n".join(entries)
    src += "\n    ]\n}\n"
    (OUT / (mod + ".rs")).write_text(src)
    print("wrote %s.rs (%d tools)" % (mod, len(shorts)))


def main():
    schemas = (SRC / "schemas.rs").read_text()
    cli = (SRC / "cli.rs").read_text()
    handlers = (SRC / "handlers.rs").read_text()

    mcp_blocks = extract_tool_blocks(schemas)
    # ntulearn_login is the one special case: its handler takes Arc<AppState>
    # (the capture may outlive the call) and returns text, not a Value. It
    # stays hand-wired in tools.rs; its schema moves there too.
    login_block = mcp_blocks.pop("login", None)
    if login_block is None:
        print("login tool not found in schemas.rs", file=sys.stderr)
        return 1
    (OUT.parent / "login_tool.rs").write_text(login_block + "\n")
    cli_start = cli.find("pub const COMMANDS")
    cli_end = cli.find("pub fn find_cmd")
    cmd_blocks = extract_cmd_blocks(cli[cli_start:cli_end])

    dv_start = handlers.find("pub async fn dispatch_value")
    dv_end = handlers.find("\n}", dv_start)
    handler_for = dict(re.findall(r'"([a-z_]+)"\s*=>\s*(h_\w+)\(state, args\)',
                                  handlers[dv_start:dv_end]))

    md_for = parse_markdown_arms(handlers)

    missing_handler = sorted(set(mcp_blocks) - set(handler_for))
    missing_cli = sorted(set(mcp_blocks) - set(cmd_blocks))
    if missing_handler or missing_cli:
        print("INCONSISTENT TABLES", file=sys.stderr)
        print("  no handler:", missing_handler, file=sys.stderr)
        print("  no cli spec:", missing_cli, file=sys.stderr)
        return 1
    print("%d tools: mcp=%d cli=%d handlers=%d md-arms=%d"
          % (len(mcp_blocks), len(mcp_blocks), len(cmd_blocks),
             len(handler_for), len(md_for)))

    grouped = {g: [] for g in GROUPS}
    for short in mcp_blocks:
        placed = False
        for modname, members in GROUPS.items():
            if isinstance(members, list) and short in members:
                grouped[modname].append(short)
                placed = True
                break
        if not placed:
            for modname, pfx in GROUPS.items():
                if isinstance(pfx, str) and short.startswith(pfx):
                    grouped[modname].append(short)
                    placed = True
                    break
        if not placed:
            print("no group for %s" % short, file=sys.stderr)
            return 1

    OUT.mkdir(parents=True, exist_ok=True)
    all_mods = []
    for mod, shorts in grouped.items():
        if shorts:
            all_mods.append(mod)
            emit_module(mod, sorted(shorts), mcp_blocks, cmd_blocks,
                        handler_for, md_for)

    mods = "\n".join("mod %s;" % m for m in all_mods)
    exts = "\n".join("    out.extend(%s::tools());" % m for m in all_mods)
    modrs = (
        "//! Feature modules - each owns its tools end to end (schema, CLI\n"
        "//! spec, markdown renderer, handler binding). Add a new feature by\n"
        "//! creating a module with a pub fn tools() -> Vec<ToolDef> and one\n"
        "//! extend line here.\n\n"
        "use crate::registry::ToolDef;\n\n"
        + mods + "\n\n"
        "/// Assemble the full registry. Called once (OnceLock in the parent module).\n"
        "pub fn build() -> Vec<ToolDef> {\n"
        "    let mut out = Vec::new();\n"
        + exts + "\n"
        "    out\n}\n")
    (OUT / "mod.rs").write_text(modrs)
    print("wrote features/mod.rs with %d modules" % len(all_mods))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
