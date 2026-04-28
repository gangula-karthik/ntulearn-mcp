"""NTULearn MCP server — Blackboard Learn REST API wrapper."""

from __future__ import annotations

import asyncio
import json
import os
import re
from io import BytesIO
from pathlib import Path
from typing import Any
from urllib.parse import unquote

from bs4 import BeautifulSoup
from dotenv import load_dotenv
from mcp.server import Server
from mcp.server.stdio import stdio_server
from mcp.types import Tool, TextContent

from ntulearn_mcp.client import BbRouterExpiredError, NTULearnClient
from ntulearn_mcp.cookie import read_bbrouter_cookie
from ntulearn_mcp.parsers import extract_all_files

load_dotenv()

# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------

BASE_URL = os.getenv("NTULEARN_BASE_URL", "https://ntulearn.ntu.edu.sg")
DOWNLOAD_DIR = Path(os.getenv("NTULEARN_DOWNLOAD_DIR", "./downloads"))

_TOOL_PREFIX = "ntulearn"

# Per-file and per-batch caps for read_file_content. download_bytes buffers the
# full response in memory, so without these a 200 MB attachment crashes the
# process and floods Claude's context with useless content.
_MAX_FILE_BYTES = 25 * 1024 * 1024
_MAX_TOTAL_BYTES = 40 * 1024 * 1024

# Pagination defaults / caps for list-returning tools.
_DEFAULT_LIMIT = 50
_MAX_LIMIT = 200
_MAX_DEPTH = 10

# Loose pattern for Blackboard course/content IDs (e.g. _12345_1, _abc-1_2).
_BB_ID_PATTERN = r"^[A-Za-z0-9_\-:]+$"

_TEXT_MIMETYPES = frozenset({
    "application/json",
    "application/xml",
    "application/javascript",
    "application/x-javascript",
    "application/x-yaml",
    "application/yaml",
    "application/ld+json",
    "application/x-sh",
})

_TEXT_EXTENSIONS = frozenset({
    "txt", "md", "markdown", "csv", "tsv", "json", "xml", "yaml", "yml",
    "html", "htm", "log", "py", "js", "ts", "rs", "go", "java", "c", "cpp",
    "h", "hpp", "sh", "bash", "zsh", "rb", "swift", "kt", "scala", "r",
    "ini", "toml", "cfg", "conf", "env",
})

_NO_COOKIE_MESSAGE = (
    "No NTULearn cookie found. Two options:\n"
    "  1. Log into https://ntulearn.ntu.edu.sg in a supported browser "
    "(Firefox works best on Windows; Chrome/Edge auto-read often needs "
    "admin), then restart your MCP host (e.g. Claude Desktop).\n"
    "  2. Set the NTULEARN_COOKIE env var manually — see README for "
    "the DevTools cookie-copy steps. This always works."
)

# ---------------------------------------------------------------------------
# Server + client setup
# ---------------------------------------------------------------------------

app = Server("ntulearn_mcp")
_client: NTULearnClient | None = None


def _validate_cookie_value(value: str) -> str:
    """Reject cookie values that would let an attacker inject extra headers.

    A pathological value with CR/LF would smuggle Set-Cookie or other
    headers into the request. Realistic exposure is small (the user has
    to put the bad value in NTULEARN_COOKIE themselves) but cheap defence
    in depth.
    """
    if "\r" in value or "\n" in value or "\x00" in value:
        raise RuntimeError(
            "NTULEARN_COOKIE contains illegal control characters "
            "(CR/LF/NUL). Re-copy the cookie value from DevTools."
        )
    return value


def _resolve_cookie() -> str:
    """Resolve the BbRouter cookie. Explicit env var wins over browser auto-read."""
    explicit = os.getenv("NTULEARN_COOKIE", "").strip()
    if explicit:
        return _validate_cookie_value(explicit)
    auto = read_bbrouter_cookie()
    if auto:
        return _validate_cookie_value(auto)
    raise RuntimeError(_NO_COOKIE_MESSAGE)


def get_client() -> NTULearnClient:
    global _client
    if _client is None:
        _client = NTULearnClient(BASE_URL, _resolve_cookie())
    return _client


async def _refresh_client() -> NTULearnClient:
    """Discard the current client, re-read the cookie, build a fresh client.

    Called after a 401 so that an expired cookie can be transparently swapped
    for the fresh value the user's browser already has.
    """
    global _client
    if _client is not None:
        await _client.close()
        _client = None
    _client = NTULearnClient(BASE_URL, _resolve_cookie())
    return _client


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _strip_content(item: dict[str, Any]) -> dict[str, Any]:
    """Reduce a raw Blackboard content item to the fields we care about."""
    handler = item.get("contentHandler", {}) or {}
    description_raw = item.get("description", {}) or {}
    return {
        "id": item.get("id"),
        "title": item.get("title"),
        "contentHandlerId": handler.get("id"),
        "hasChildren": item.get("hasChildren", False),
        "description": description_raw.get("rawText") if isinstance(description_raw, dict) else description_raw,
        "modified": item.get("modified"),
    }


async def _resolve_content_files(
    client: NTULearnClient, course_id: str, content_id: str
) -> tuple[dict[str, Any], str | None, list[tuple[str, str | None]]]:
    """Return (item, handler_id, pairs) for a content item.

    `pairs` is a list of (url, filename) tuples for every file attached to the
    item. Filename may be None when the parser couldn't determine one.
    Empty pairs means no resolvable file (caller decides how to surface that).
    """
    item = await client.get_content_item(course_id, content_id)
    handler_id = (item.get("contentHandler") or {}).get("id")

    pairs: list[tuple[str, str | None]] = []

    if handler_id == "resource/x-bb-file":
        attachments = await client.get_attachments(course_id, content_id)
        for att in attachments:
            url = await client.get_attachment_download_url(course_id, content_id, att["id"])
            if url:
                pairs.append((url, att.get("fileName")))
    else:
        body = item.get("body") or ""
        files = extract_all_files(body)
        if not files:
            desc = item.get("description") or {}
            body2 = (desc.get("rawText") if isinstance(desc, dict) else desc) or ""
            files = extract_all_files(body2)
        for f in files:
            url = f.get("url")
            if url:
                pairs.append((url, f.get("filename")))

    return item, handler_id, pairs


def _file_extension(filename: str) -> str:
    if "." not in filename:
        return ""
    return filename.rpartition(".")[2].lower()


def _parse_content_type(content_type: str | None) -> tuple[str, str | None]:
    """Return (mime, charset) from a Content-Type header value."""
    if not content_type:
        return "", None
    parts = [p.strip() for p in content_type.split(";")]
    mime = parts[0].lower()
    charset = None
    for p in parts[1:]:
        if p.lower().startswith("charset="):
            charset = p.split("=", 1)[1].strip().strip('"').strip("'")
    return mime, charset


def _classify_kind(filename: str, content_type: str | None) -> str:
    """Return 'pdf', 'text', or 'binary'.

    Filename extension wins over content-type. Blackboard's bbcswebdav often
    serves files as application/octet-stream regardless of actual format, so
    trusting the header alone misclassifies most attachments.
    """
    ext = _file_extension(filename)
    if ext == "pdf":
        return "pdf"
    if ext in _TEXT_EXTENSIONS:
        return "text"

    mime, _ = _parse_content_type(content_type)
    if mime == "application/pdf":
        return "pdf"
    if mime.startswith("text/"):
        return "text"
    if mime in _TEXT_MIMETYPES:
        return "text"
    return "binary"


def _format_bytes(n: int) -> str:
    if n < 1024:
        return f"{n} B"
    if n < 1024 * 1024:
        return f"{n / 1024:.1f} KB"
    return f"{n / (1024 * 1024):.1f} MB"


def _extract_content(
    filename: str, content_type: str | None, content_bytes: bytes
) -> dict[str, Any]:
    """Detect file kind and extract text. Returns one entry for the response payload.

    Pure function — no I/O. Caller handles download retries and size caps.
    """
    size = len(content_bytes)
    kind = _classify_kind(filename, content_type)

    if kind == "pdf":
        from pypdf import PdfReader
        from pypdf.errors import PyPdfError

        try:
            reader = PdfReader(BytesIO(content_bytes))
            if reader.is_encrypted:
                # Many "encrypted" Blackboard PDFs unlock with an empty password.
                try:
                    reader.decrypt("")
                except Exception:
                    return {
                        "filename": filename,
                        "kind": "pdf",
                        "error": "PDF is password-protected. Cannot extract text.",
                        "sizeBytes": size,
                        "contentType": content_type,
                    }
            page_count = len(reader.pages)
            text = "\n\n".join((page.extract_text() or "") for page in reader.pages)
        except (PyPdfError, Exception) as e:
            return {
                "filename": filename,
                "kind": "pdf",
                "error": f"PDF extraction failed: {e}",
                "sizeBytes": size,
                "contentType": content_type,
            }

        out: dict[str, Any] = {
            "filename": filename,
            "kind": "pdf",
            "text": text,
            "pageCount": page_count,
            "sizeBytes": size,
            "contentType": content_type,
        }
        if not text.strip():
            out["warning"] = (
                "PDF appears to contain no extractable text "
                "(likely scanned images)."
            )
        return out

    if kind == "text":
        _, charset = _parse_content_type(content_type)
        text: str | None = None
        for enc in (charset, "utf-8", "latin-1"):
            if not enc:
                continue
            try:
                text = content_bytes.decode(enc)
                break
            except (UnicodeDecodeError, LookupError):
                continue
        if text is None:
            text = content_bytes.decode("utf-8", errors="replace")

        ext = _file_extension(filename)
        mime, _ = _parse_content_type(content_type)
        if ext in {"html", "htm"} or mime == "text/html":
            text = BeautifulSoup(text, "html.parser").get_text(separator="\n")
            text = "\n".join(line for line in (l.strip() for l in text.splitlines()) if line)

        return {
            "filename": filename,
            "kind": "text",
            "text": text,
            "sizeBytes": size,
            "contentType": content_type,
        }

    return {
        "filename": filename,
        "kind": "binary",
        "error": (
            f"Binary file ({content_type or 'unknown type'}, {_format_bytes(size)}). "
            "Cannot extract text. Use ntulearn_download_file to save it locally."
        ),
        "sizeBytes": size,
        "contentType": content_type,
    }


# ---------------------------------------------------------------------------
# Pagination + response-format helpers
# ---------------------------------------------------------------------------

def _slice_with_pagination(
    items: list[Any], offset: int, limit: int
) -> tuple[list[Any], dict[str, Any]]:
    """Slice items[offset:offset+limit] and return pagination metadata.

    The underlying Blackboard client paginates internally and returns full
    result sets, so this is caller-side slicing — its job is to keep the
    LLM's context bounded, not to reduce upstream load.
    """
    total = len(items)
    end = min(total, offset + limit)
    page = items[offset:end]
    next_offset = end if end < total else None
    return page, {
        "total": total,
        "count": len(page),
        "offset": offset,
        "limit": limit,
        "hasMore": next_offset is not None,
        "nextOffset": next_offset,
    }


def _resolve_pagination_args(args: dict[str, Any]) -> tuple[int, int]:
    """Validate and clamp limit/offset args. Defaults: offset=0, limit=_DEFAULT_LIMIT."""
    offset = int(args.get("offset", 0))
    limit = int(args.get("limit", _DEFAULT_LIMIT))
    if offset < 0:
        raise ValueError("offset must be >= 0")
    if limit < 1:
        raise ValueError("limit must be >= 1")
    if limit > _MAX_LIMIT:
        raise ValueError(f"limit must be <= {_MAX_LIMIT}")
    return offset, limit


def _resolve_response_format(args: dict[str, Any]) -> str:
    """Return 'json' or 'markdown'. Default: 'json' (machine-readable)."""
    fmt = str(args.get("response_format", "json")).lower()
    if fmt not in ("json", "markdown"):
        raise ValueError("response_format must be 'json' or 'markdown'")
    return fmt


def _emit(
    payload: dict[str, Any], text: str | None = None
) -> tuple[list[TextContent], dict[str, Any]]:
    """Return the (unstructured, structured) tuple MCP expects when both are present.

    `payload` is the JSON-serialisable structured content (validated against
    outputSchema by MCP). `text` is the rendered display text — defaults to a
    pretty-printed JSON copy of the payload for json mode.
    """
    if text is None:
        text = json.dumps(payload, indent=2)
    return [TextContent(type="text", text=text)], payload


def _md_pagination_footer(meta: dict[str, Any]) -> str:
    """Trailing line summarising pagination for markdown views."""
    if meta["hasMore"]:
        return (
            f"\n_Showing {meta['count']} of {meta['total']} "
            f"(offset {meta['offset']}). Pass offset={meta['nextOffset']} for the next page._"
        )
    return f"\n_Showing all {meta['total']}._"


# ---------------------------------------------------------------------------
# Tool definitions
# ---------------------------------------------------------------------------

# Common JSON-schema fragments for reuse.
_COURSE_ID_SCHEMA = {
    "type": "string",
    "description": "Blackboard course ID (e.g. _12345_1)",
    "minLength": 1,
    "maxLength": 200,
    "pattern": _BB_ID_PATTERN,
}

_CONTENT_ID_SCHEMA = {
    "type": "string",
    "description": "Content item ID (e.g. _67890_1)",
    "minLength": 1,
    "maxLength": 200,
    "pattern": _BB_ID_PATTERN,
}

_LIMIT_SCHEMA = {
    "type": "integer",
    "description": (
        f"Max items to return per call (1–{_MAX_LIMIT}, default {_DEFAULT_LIMIT}). "
        "Use small values to keep results within the LLM's context."
    ),
    "minimum": 1,
    "maximum": _MAX_LIMIT,
    "default": _DEFAULT_LIMIT,
}

_OFFSET_SCHEMA = {
    "type": "integer",
    "description": (
        "Number of items to skip for pagination. "
        "Use the nextOffset value returned by a previous call to walk pages."
    ),
    "minimum": 0,
    "default": 0,
}

_RESPONSE_FORMAT_SCHEMA = {
    "type": "string",
    "description": (
        "'json' returns a structured payload (default, recommended for agents). "
        "'markdown' returns a human-readable summary."
    ),
    "enum": ["json", "markdown"],
    "default": "json",
}

_PAGINATION_OUTPUT_FIELDS = {
    "total": {"type": "integer"},
    "count": {"type": "integer"},
    "offset": {"type": "integer"},
    "limit": {"type": "integer"},
    "hasMore": {"type": "boolean"},
    "nextOffset": {"type": ["integer", "null"]},
}

_CONTENT_ITEM_SCHEMA = {
    "type": "object",
    "properties": {
        "id": {"type": ["string", "null"]},
        "title": {"type": ["string", "null"]},
        "contentHandlerId": {"type": ["string", "null"]},
        "hasChildren": {"type": "boolean"},
        "description": {"type": ["string", "null"]},
        "modified": {"type": ["string", "null"]},
    },
}

_ANNOUNCEMENT_SCHEMA = {
    "type": "object",
    "properties": {
        "id": {"type": ["string", "null"]},
        "title": {"type": ["string", "null"]},
        "body": {"type": ["string", "null"]},
        "created": {"type": ["string", "null"]},
        "modified": {"type": ["string", "null"]},
        "available": {"type": ["string", "null"]},
    },
}

_GRADEBOOK_COLUMN_SCHEMA = {
    "type": "object",
    "properties": {
        "id": {"type": ["string", "null"]},
        "name": {"type": ["string", "null"]},
        "displayName": {"type": ["string", "null"]},
        "possible": {"type": ["number", "null"]},
        "available": {"type": ["string", "null"]},
        "contentId": {"type": ["string", "null"]},
        "score": {"type": ["number", "string", "null"]},
        "grade": {"type": ["string", "null"]},
        "status": {"type": ["string", "null"]},
    },
}

_FILE_INFO_SCHEMA = {
    "type": "object",
    "properties": {
        "url": {"type": ["string", "null"]},
        "filename": {"type": ["string", "null"]},
        "mimeType": {"type": ["string", "null"]},
        "link_text": {"type": ["string", "null"]},
        # download_file results
        "localPath": {"type": "string"},
        "sizeBytes": {"type": "integer"},
        # read_file_content results
        "kind": {"type": "string"},
        "text": {"type": "string"},
        "pageCount": {"type": "integer"},
        "contentType": {"type": ["string", "null"]},
        "warning": {"type": "string"},
        "error": {"type": "string"},
        "reason": {"type": "string"},
    },
}


@app.list_tools()
async def list_tools() -> list[Tool]:
    return [
        Tool(
            name=f"{_TOOL_PREFIX}_list_courses",
            description=(
                "List courses the current user is enrolled in on NTULearn. "
                "By default returns only active/available courses. "
                "Set include_disabled=true to also include unavailable ones. "
                "Supports pagination via limit/offset."
            ),
            annotations={
                "title": "List my NTULearn courses",
                "readOnlyHint": True,
                "destructiveHint": False,
                "idempotentHint": True,
                "openWorldHint": True,
            },
            inputSchema={
                "type": "object",
                "properties": {
                    "include_disabled": {
                        "type": "boolean",
                        "description": "Include courses where availability.available != 'Yes'",
                        "default": False,
                    },
                    "limit": _LIMIT_SCHEMA,
                    "offset": _OFFSET_SCHEMA,
                    "response_format": _RESPONSE_FORMAT_SCHEMA,
                },
                "additionalProperties": False,
            },
            outputSchema={
                "type": "object",
                "properties": {
                    "courses": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "courseId": {"type": "string"},
                                "title": {"type": "string"},
                                "available": {"type": "string"},
                                "lastAccessed": {"type": ["string", "null"]},
                            },
                            "required": ["courseId", "title"],
                        },
                    },
                    **_PAGINATION_OUTPUT_FIELDS,
                },
                "required": ["courses", "total", "count", "offset", "limit", "hasMore"],
            },
        ),
        Tool(
            name=f"{_TOOL_PREFIX}_get_course_contents",
            description=(
                "Get the top-level content tree for a course. "
                "Returns folders, documents, links, and assignments at the root level. "
                "Use ntulearn_get_folder_children to drill into items where hasChildren=true."
            ),
            annotations={
                "title": "Get top-level course contents",
                "readOnlyHint": True,
                "destructiveHint": False,
                "idempotentHint": True,
                "openWorldHint": True,
            },
            inputSchema={
                "type": "object",
                "properties": {
                    "course_id": _COURSE_ID_SCHEMA,
                    "limit": _LIMIT_SCHEMA,
                    "offset": _OFFSET_SCHEMA,
                    "response_format": _RESPONSE_FORMAT_SCHEMA,
                },
                "required": ["course_id"],
                "additionalProperties": False,
            },
            outputSchema={
                "type": "object",
                "properties": {
                    "items": {"type": "array", "items": _CONTENT_ITEM_SCHEMA},
                    **_PAGINATION_OUTPUT_FIELDS,
                },
                "required": ["items", "total", "count", "offset", "limit", "hasMore"],
            },
        ),
        Tool(
            name=f"{_TOOL_PREFIX}_get_folder_children",
            description=(
                "Get the children of a folder or lesson within a course. "
                "Use this to drill into content items where hasChildren=true."
            ),
            annotations={
                "title": "Get folder children",
                "readOnlyHint": True,
                "destructiveHint": False,
                "idempotentHint": True,
                "openWorldHint": True,
            },
            inputSchema={
                "type": "object",
                "properties": {
                    "course_id": _COURSE_ID_SCHEMA,
                    "content_id": {**_CONTENT_ID_SCHEMA, "description": "Content item ID of the folder"},
                    "limit": _LIMIT_SCHEMA,
                    "offset": _OFFSET_SCHEMA,
                    "response_format": _RESPONSE_FORMAT_SCHEMA,
                },
                "required": ["course_id", "content_id"],
                "additionalProperties": False,
            },
            outputSchema={
                "type": "object",
                "properties": {
                    "items": {"type": "array", "items": _CONTENT_ITEM_SCHEMA},
                    **_PAGINATION_OUTPUT_FIELDS,
                },
                "required": ["items", "total", "count", "offset", "limit", "hasMore"],
            },
        ),
        Tool(
            name=f"{_TOOL_PREFIX}_search_course_content",
            description=(
                "Recursively search a course's entire content tree for items matching a query. "
                "Matches on title or description (case-insensitive substring). "
                "Returns matched items with their full breadcrumb path."
            ),
            annotations={
                "title": "Search course content",
                "readOnlyHint": True,
                "destructiveHint": False,
                "idempotentHint": True,
                "openWorldHint": True,
            },
            inputSchema={
                "type": "object",
                "properties": {
                    "course_id": _COURSE_ID_SCHEMA,
                    "query": {
                        "type": "string",
                        "description": "Search term (case-insensitive substring)",
                        "minLength": 1,
                        "maxLength": 200,
                    },
                    "max_depth": {
                        "type": "integer",
                        "description": f"Maximum recursion depth (default 5, capped at {_MAX_DEPTH})",
                        "default": 5,
                        "minimum": 1,
                        "maximum": _MAX_DEPTH,
                    },
                    "max_results": {
                        "type": "integer",
                        "description": f"Maximum number of matching items to return (default 50, capped at {_MAX_LIMIT})",
                        "default": 50,
                        "minimum": 1,
                        "maximum": _MAX_LIMIT,
                    },
                    "response_format": _RESPONSE_FORMAT_SCHEMA,
                },
                "required": ["course_id", "query"],
                "additionalProperties": False,
            },
            outputSchema={
                "type": "object",
                "properties": {
                    "matches": {
                        "type": "array",
                        "items": {
                            **_CONTENT_ITEM_SCHEMA,
                            "properties": {
                                **_CONTENT_ITEM_SCHEMA["properties"],
                                "breadcrumb": {"type": "array", "items": {"type": "string"}},
                            },
                        },
                    },
                    "count": {"type": "integer"},
                },
                "required": ["matches", "count"],
            },
        ),
        Tool(
            name=f"{_TOOL_PREFIX}_get_file_download_url",
            description=(
                "Get the download URL(s) for files attached to a Blackboard content item. "
                "Parses both attachment-API items and HTML body bbcswebdav links. "
                "Use ntulearn_download_file to actually fetch them, or "
                "ntulearn_read_file_content to read text inline."
            ),
            annotations={
                "title": "Get file download URLs",
                "readOnlyHint": True,
                "destructiveHint": False,
                "idempotentHint": True,
                "openWorldHint": True,
            },
            inputSchema={
                "type": "object",
                "properties": {
                    "course_id": _COURSE_ID_SCHEMA,
                    "content_id": _CONTENT_ID_SCHEMA,
                    "response_format": _RESPONSE_FORMAT_SCHEMA,
                },
                "required": ["course_id", "content_id"],
                "additionalProperties": False,
            },
            outputSchema={
                "type": "object",
                "properties": {
                    "contentId": {"type": ["string", "null"]},
                    "title": {"type": ["string", "null"]},
                    "contentHandlerId": {"type": ["string", "null"]},
                    "files": {"type": "array", "items": _FILE_INFO_SCHEMA},
                    "error": {"type": "string"},
                },
                "required": ["files"],
            },
        ),
        Tool(
            name=f"{_TOOL_PREFIX}_download_file",
            description=(
                "Download every file attached to a Blackboard content item into the local "
                "download directory. Handles both resource/x-bb-file (attachment API) and "
                "resource/x-bb-document (HTML body with bbcswebdav links) handler types. "
                "Returns a list of saved files with their local paths and sizes. "
                "WARNING: writes to local filesystem — Claude Desktop's sandboxed "
                "tools cannot read those files. Use ntulearn_read_file_content "
                "instead to ask questions about content inline."
            ),
            annotations={
                "title": "Download files to local disk",
                "readOnlyHint": False,
                "destructiveHint": False,
                "idempotentHint": False,
                "openWorldHint": True,
            },
            inputSchema={
                "type": "object",
                "properties": {
                    "course_id": _COURSE_ID_SCHEMA,
                    "content_id": _CONTENT_ID_SCHEMA,
                    "response_format": _RESPONSE_FORMAT_SCHEMA,
                },
                "required": ["course_id", "content_id"],
                "additionalProperties": False,
            },
            outputSchema={
                "type": "object",
                "properties": {
                    "contentId": {"type": ["string", "null"]},
                    "title": {"type": ["string", "null"]},
                    "contentHandlerId": {"type": ["string", "null"]},
                    "files": {"type": "array", "items": _FILE_INFO_SCHEMA},
                    "error": {"type": "string"},
                },
                "required": ["files"],
            },
        ),
        Tool(
            name=f"{_TOOL_PREFIX}_read_file_content",
            description=(
                "Read the text content of files attached to a Blackboard content item, "
                "returned inline (no local-filesystem hop). Use this to ask questions "
                "about lecture material — ntulearn_download_file is for users who "
                "actually want the bytes on disk. "
                "Supports PDFs (text extraction via pypdf) and text-like files "
                "(txt, md, csv, json, xml, html with tags stripped, code files). "
                "Other binaries (images, video, .docx, .pptx) are listed under `skipped` "
                "with a clear message — fall back to ntulearn_download_file for those. "
                "Per-file cap 25 MB, batch cap 40 MB; oversized files are skipped."
            ),
            annotations={
                "title": "Read file content inline",
                "readOnlyHint": True,
                "destructiveHint": False,
                "idempotentHint": True,
                "openWorldHint": True,
            },
            inputSchema={
                "type": "object",
                "properties": {
                    "course_id": _COURSE_ID_SCHEMA,
                    "content_id": _CONTENT_ID_SCHEMA,
                    "response_format": _RESPONSE_FORMAT_SCHEMA,
                },
                "required": ["course_id", "content_id"],
                "additionalProperties": False,
            },
            outputSchema={
                "type": "object",
                "properties": {
                    "contentId": {"type": ["string", "null"]},
                    "title": {"type": ["string", "null"]},
                    "contentHandlerId": {"type": ["string", "null"]},
                    "files": {"type": "array", "items": _FILE_INFO_SCHEMA},
                    "skipped": {"type": "array", "items": _FILE_INFO_SCHEMA},
                    "error": {"type": "string"},
                },
                "required": ["files", "skipped"],
            },
        ),
        Tool(
            name=f"{_TOOL_PREFIX}_get_announcements",
            description="Get announcements for a course, newest first. Supports pagination.",
            annotations={
                "title": "Get course announcements",
                "readOnlyHint": True,
                "destructiveHint": False,
                "idempotentHint": True,
                "openWorldHint": True,
            },
            inputSchema={
                "type": "object",
                "properties": {
                    "course_id": _COURSE_ID_SCHEMA,
                    "limit": _LIMIT_SCHEMA,
                    "offset": _OFFSET_SCHEMA,
                    "response_format": _RESPONSE_FORMAT_SCHEMA,
                },
                "required": ["course_id"],
                "additionalProperties": False,
            },
            outputSchema={
                "type": "object",
                "properties": {
                    "announcements": {"type": "array", "items": _ANNOUNCEMENT_SCHEMA},
                    **_PAGINATION_OUTPUT_FIELDS,
                },
                "required": ["announcements", "total", "count", "offset", "limit", "hasMore"],
            },
        ),
        Tool(
            name=f"{_TOOL_PREFIX}_get_gradebook",
            description=(
                "Get gradebook columns for a course, including your scores where available. "
                "Returns column names, max scores, and your grade for each. "
                "Supports pagination."
            ),
            annotations={
                "title": "Get course gradebook",
                "readOnlyHint": True,
                "destructiveHint": False,
                "idempotentHint": True,
                "openWorldHint": True,
            },
            inputSchema={
                "type": "object",
                "properties": {
                    "course_id": _COURSE_ID_SCHEMA,
                    "limit": _LIMIT_SCHEMA,
                    "offset": _OFFSET_SCHEMA,
                    "response_format": _RESPONSE_FORMAT_SCHEMA,
                },
                "required": ["course_id"],
                "additionalProperties": False,
            },
            outputSchema={
                "type": "object",
                "properties": {
                    "columns": {"type": "array", "items": _GRADEBOOK_COLUMN_SCHEMA},
                    **_PAGINATION_OUTPUT_FIELDS,
                    "gradesAvailable": {"type": "boolean"},
                    "gradeFetchError": {"type": ["string", "null"]},
                },
                "required": [
                    "columns", "total", "count", "offset", "limit", "hasMore",
                    "gradesAvailable",
                ],
            },
        ),
    ]


# ---------------------------------------------------------------------------
# Tool handlers
# ---------------------------------------------------------------------------

@app.call_tool()
async def call_tool(
    name: str, arguments: dict[str, Any]
) -> tuple[list[TextContent], dict[str, Any]]:
    """Dispatch a tool call. Errors are raised so MCP wraps them with isError=True.

    On 401, swap in a fresh cookie once and retry. If retry still fails, the
    exception propagates and the SDK marks the result as an error.

    Returns a (unstructured_content, structured_content) tuple — both forms
    are propagated by the MCP framework so clients can use whichever they
    prefer (and the structured form is validated against any outputSchema
    declared on the tool).
    """
    try:
        return await _dispatch(name, arguments)
    except BbRouterExpiredError:
        await _refresh_client()
        return await _dispatch(name, arguments)


async def _dispatch(
    name: str, arguments: dict[str, Any]
) -> tuple[list[TextContent], dict[str, Any]]:
    client = get_client()
    handlers = {
        f"{_TOOL_PREFIX}_list_courses": _list_courses,
        f"{_TOOL_PREFIX}_get_course_contents": _get_course_contents,
        f"{_TOOL_PREFIX}_get_folder_children": _get_folder_children,
        f"{_TOOL_PREFIX}_search_course_content": _search_course_content,
        f"{_TOOL_PREFIX}_get_file_download_url": _get_file_download_url,
        f"{_TOOL_PREFIX}_download_file": _download_file,
        f"{_TOOL_PREFIX}_read_file_content": _read_file_content,
        f"{_TOOL_PREFIX}_get_announcements": _get_announcements,
        f"{_TOOL_PREFIX}_get_gradebook": _get_gradebook,
    }
    handler = handlers.get(name)
    if handler is None:
        raise ValueError(
            f"Unknown tool: {name}. Available: {sorted(handlers.keys())}"
        )
    return await handler(client, arguments)


# ---------------------------------------------------------------------------
# Markdown rendering helpers
# ---------------------------------------------------------------------------

def _md_courses(courses: list[dict[str, Any]], meta: dict[str, Any]) -> str:
    lines = [f"# Courses ({meta['total']} total)", ""]
    if not courses:
        lines.append("_No courses to show._")
    else:
        for c in courses:
            last = c.get("lastAccessed") or "—"
            lines.append(
                f"- **{c.get('title', '?')}** "
                f"`{c.get('courseId', '?')}` "
                f"· available={c.get('available', '?')} "
                f"· last accessed {last}"
            )
    lines.append(_md_pagination_footer(meta))
    return "\n".join(lines)


def _md_content_items(items: list[dict[str, Any]], meta: dict[str, Any]) -> str:
    lines = [f"# Content items ({meta['total']} total)", ""]
    if not items:
        lines.append("_No items._")
    else:
        for it in items:
            arrow = "📁" if it.get("hasChildren") else "📄"
            lines.append(
                f"- {arrow} **{it.get('title', '?')}** "
                f"`{it.get('id', '?')}` "
                f"· handler={it.get('contentHandlerId', '?')}"
            )
    lines.append(_md_pagination_footer(meta))
    return "\n".join(lines)


def _md_announcements(items: list[dict[str, Any]], meta: dict[str, Any]) -> str:
    lines = [f"# Announcements ({meta['total']} total)", ""]
    if not items:
        lines.append("_No announcements._")
    else:
        for a in items:
            created = a.get("created") or "—"
            lines.append(f"## {a.get('title', '?')}  ·  {created}")
            body = (a.get("body") or "").strip()
            lines.append(body if body else "_(no body)_")
            lines.append("")
    lines.append(_md_pagination_footer(meta))
    return "\n".join(lines)


def _md_gradebook(columns: list[dict[str, Any]], meta: dict[str, Any], grades_available: bool, error: str | None) -> str:
    lines = [f"# Gradebook ({meta['total']} columns)", ""]
    if not grades_available:
        lines.append(f"_Grades not available: {error}_")
        lines.append("")
    if not columns:
        lines.append("_No columns._")
    else:
        lines.append("| Column | Possible | Score | Grade | Status |")
        lines.append("|---|---|---|---|---|")
        for c in columns:
            lines.append(
                f"| {c.get('displayName') or c.get('name') or '?'} "
                f"| {c.get('possible', '—')} "
                f"| {c.get('score', '—')} "
                f"| {c.get('grade', '—')} "
                f"| {c.get('status', '—')} |"
            )
    lines.append(_md_pagination_footer(meta))
    return "\n".join(lines)


def _md_search_results(matches: list[dict[str, Any]]) -> str:
    lines = [f"# Search matches ({len(matches)})", ""]
    if not matches:
        lines.append("_No matches._")
    else:
        for m in matches:
            crumb = " › ".join(m.get("breadcrumb") or [])
            lines.append(f"- **{m.get('title', '?')}** `{m.get('id', '?')}`  ")
            lines.append(f"  _{crumb}_")
    return "\n".join(lines)


def _md_files(payload: dict[str, Any], heading: str) -> str:
    lines = [f"# {heading}", "", f"**Item:** {payload.get('title', '?')}  ", ""]
    files = payload.get("files") or []
    skipped = payload.get("skipped") or []
    if files:
        lines.append("## Files")
        for f in files:
            if "localPath" in f:
                lines.append(
                    f"- `{f['filename']}` ({_format_bytes(f.get('sizeBytes', 0))}) "
                    f"→ `{f['localPath']}`"
                )
            elif "text" in f:
                pages = f" · {f['pageCount']} pages" if f.get("pageCount") else ""
                lines.append(
                    f"### {f['filename']} ({f['kind']}{pages}, "
                    f"{_format_bytes(f.get('sizeBytes', 0))})"
                )
                text = f.get("text", "").strip()
                if not text:
                    lines.append("_(empty)_")
                elif len(text) > 5000:
                    lines.append(text[:5000] + "\n…_(truncated in markdown view; use response_format='json' for full text)_")
                else:
                    lines.append(text)
            else:
                lines.append(f"- `{f.get('filename', '?')}` (url: {f.get('url', '?')})")
        lines.append("")
    if skipped:
        lines.append("## Skipped")
        for s in skipped:
            lines.append(f"- `{s['filename']}`: {s['reason']}")
    if not files and not skipped:
        lines.append("_(nothing to show)_")
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# Individual tool implementations
# ---------------------------------------------------------------------------

async def _list_courses(
    client: NTULearnClient, args: dict[str, Any]
) -> tuple[list[TextContent], dict[str, Any]]:
    include_disabled = bool(args.get("include_disabled", False))
    offset, limit = _resolve_pagination_args(args)
    fmt = _resolve_response_format(args)

    enrollments = await client.get_my_enrollments()
    if not include_disabled:
        enrollments = [
            e for e in enrollments
            if (e.get("availability") or {}).get("available") == "Yes"
        ]

    course_ids = [e["courseId"] for e in enrollments]
    if not course_ids:
        _, meta = _slice_with_pagination([], offset, limit)
        payload = {"courses": [], **meta}
        text = _md_courses([], meta) if fmt == "markdown" else None
        return _emit(payload, text)

    last_accessed_map = {e["courseId"]: e.get("lastAccessed") for e in enrollments}
    availability_map = {
        e["courseId"]: (e.get("availability") or {}).get("available", "Unknown")
        for e in enrollments
    }

    courses_raw = await client.get_courses_batch(course_ids)
    rows = []
    for course in courses_raw:
        cid = course.get("id")
        rows.append({
            "courseId": cid,
            "title": course.get("name") or course.get("displayName") or course.get("id"),
            "available": availability_map.get(cid, "Unknown"),
            "lastAccessed": last_accessed_map.get(cid),
        })
    rows.sort(key=lambda c: c["lastAccessed"] or "", reverse=True)

    page, meta = _slice_with_pagination(rows, offset, limit)
    payload = {"courses": page, **meta}
    text = _md_courses(page, meta) if fmt == "markdown" else None
    return _emit(payload, text)


async def _get_course_contents(
    client: NTULearnClient, args: dict[str, Any]
) -> tuple[list[TextContent], dict[str, Any]]:
    course_id = args["course_id"]
    offset, limit = _resolve_pagination_args(args)
    fmt = _resolve_response_format(args)

    items = await client.get_course_contents(course_id)
    stripped = [_strip_content(item) for item in items]
    page, meta = _slice_with_pagination(stripped, offset, limit)
    payload = {"items": page, **meta}
    text = _md_content_items(page, meta) if fmt == "markdown" else None
    return _emit(payload, text)


async def _get_folder_children(
    client: NTULearnClient, args: dict[str, Any]
) -> tuple[list[TextContent], dict[str, Any]]:
    course_id = args["course_id"]
    content_id = args["content_id"]
    offset, limit = _resolve_pagination_args(args)
    fmt = _resolve_response_format(args)

    items = await client.get_content_children(course_id, content_id)
    stripped = [_strip_content(item) for item in items]
    page, meta = _slice_with_pagination(stripped, offset, limit)
    payload = {"items": page, **meta}
    text = _md_content_items(page, meta) if fmt == "markdown" else None
    return _emit(payload, text)


async def _search_course_content(
    client: NTULearnClient, args: dict[str, Any]
) -> tuple[list[TextContent], dict[str, Any]]:
    course_id = args["course_id"]
    query = str(args["query"]).strip().lower()
    if not query:
        raise ValueError("search query cannot be blank.")

    max_depth = int(args.get("max_depth", 5))
    max_results = int(args.get("max_results", 50))
    if max_depth < 1 or max_depth > _MAX_DEPTH:
        raise ValueError(f"max_depth must be 1..{_MAX_DEPTH}")
    if max_results < 1 or max_results > _MAX_LIMIT:
        raise ValueError(f"max_results must be 1..{_MAX_LIMIT}")
    fmt = _resolve_response_format(args)

    matches: list[dict[str, Any]] = []
    visited: set[str] = set()
    semaphore = asyncio.Semaphore(5)

    async def walk(items: list[dict[str, Any]], path: list[str], depth: int) -> None:
        if depth > max_depth or len(matches) >= max_results:
            return

        child_tasks = []
        for item in items:
            if len(matches) >= max_results:
                break

            item_id = item.get("id")
            if item_id:
                if item_id in visited:
                    continue
                visited.add(item_id)

            title = item.get("title") or ""
            desc_raw = item.get("description") or {}
            desc = (desc_raw.get("rawText") if isinstance(desc_raw, dict) else desc_raw) or ""

            current_path = path + [title]

            if query in title.lower() or query in desc.lower():
                stripped = _strip_content(item)
                stripped["breadcrumb"] = current_path
                matches.append(stripped)

            if item.get("hasChildren") and len(matches) < max_results:
                async def fetch_children(i=item, p=current_path):
                    async with semaphore:
                        children = await client.get_content_children(course_id, i["id"])
                    await walk(children, p, depth + 1)
                child_tasks.append(fetch_children())

        if child_tasks:
            await asyncio.gather(*child_tasks)

    top_level = await client.get_course_contents(course_id)
    await walk(top_level, [], 0)

    payload = {"matches": matches, "count": len(matches)}
    text = _md_search_results(matches) if fmt == "markdown" else None
    return _emit(payload, text)


async def _get_file_download_url(
    client: NTULearnClient, args: dict[str, Any]
) -> tuple[list[TextContent], dict[str, Any]]:
    course_id = args["course_id"]
    content_id = args["content_id"]
    fmt = _resolve_response_format(args)

    item = await client.get_content_item(course_id, content_id)
    handler_id = (item.get("contentHandler") or {}).get("id")

    if handler_id == "resource/x-bb-file":
        attachments = await client.get_attachments(course_id, content_id)
        files = []
        for att in attachments:
            url = await client.get_attachment_download_url(course_id, content_id, att["id"])
            files.append({
                "url": url,
                "filename": att.get("fileName"),
                "mimeType": att.get("mimeType"),
                "link_text": None,
            })
    else:
        body = item.get("body") or ""
        files = extract_all_files(body)
        if not files:
            desc = item.get("description") or {}
            body2 = (desc.get("rawText") if isinstance(desc, dict) else desc) or ""
            files = extract_all_files(body2)

    if not files:
        payload = {
            "contentId": content_id,
            "title": item.get("title"),
            "contentHandlerId": handler_id,
            "files": [],
            "error": "No download URL found. Content handler type may not be supported.",
        }
        text = (
            f"# No files\n\nItem **{item.get('title', '?')}** "
            f"(handler `{handler_id}`) has no resolvable file links."
        ) if fmt == "markdown" else None
        return _emit(payload, text)

    payload = {
        "contentId": content_id,
        "title": item.get("title"),
        "contentHandlerId": handler_id,
        "files": files,
    }
    text = _md_files(payload, "File download URLs") if fmt == "markdown" else None
    return _emit(payload, text)


async def _download_file(
    client: NTULearnClient, args: dict[str, Any]
) -> tuple[list[TextContent], dict[str, Any]]:
    course_id = args["course_id"]
    content_id = args["content_id"]
    fmt = _resolve_response_format(args)

    item, handler_id, pairs = await _resolve_content_files(client, course_id, content_id)

    if not pairs:
        payload = {
            "contentId": content_id,
            "title": item.get("title"),
            "contentHandlerId": handler_id,
            "files": [],
            "error": "No download URL found. Content handler type may not be supported.",
        }
        text = (
            f"# Nothing downloaded\n\nItem **{item.get('title', '?')}** "
            f"(handler `{handler_id}`) has no resolvable file links."
        ) if fmt == "markdown" else None
        return _emit(payload, text)

    DOWNLOAD_DIR.mkdir(parents=True, exist_ok=True)

    def _sanitize(name: str) -> str:
        return re.sub(r'[\\/*?:"<>|]', "_", name)

    def _deduplicate(name: str) -> str:
        candidate = name
        stem, dot, ext = name.rpartition(".")
        base = stem if dot else name
        suffix = ext if dot else ""
        n = 2
        while candidate in used_names or (DOWNLOAD_DIR / candidate).exists():
            candidate = f"{base} ({n}).{suffix}" if suffix else f"{base} ({n})"
            n += 1
        return candidate

    used_names: set[str] = set()
    saved: list[dict[str, Any]] = []

    for url, detected_filename in pairs:
        filename = detected_filename
        if not filename:
            url_path = url.split("?")[0]
            filename = unquote(url_path.split("/")[-1]) or "download"
        filename = _sanitize(filename)
        filename = _deduplicate(filename)
        used_names.add(filename)

        dest = DOWNLOAD_DIR / filename
        try:
            content_bytes, _ = await client.download_bytes(url)
        except BbRouterExpiredError:
            client = await _refresh_client()
            content_bytes, _ = await client.download_bytes(url)
        dest.write_bytes(content_bytes)

        saved.append({
            "localPath": str(dest.resolve()),
            "filename": filename,
            "sizeBytes": len(content_bytes),
        })

    payload = {
        "contentId": content_id,
        "title": item.get("title"),
        "files": saved,
    }
    text = _md_files(payload, "Files downloaded") if fmt == "markdown" else None
    return _emit(payload, text)


async def _read_file_content(
    client: NTULearnClient, args: dict[str, Any]
) -> tuple[list[TextContent], dict[str, Any]]:
    """Resolve files attached to a content item, fetch bytes, return text inline.

    Bypasses the local-filesystem hop that breaks Claude Desktop's sandbox:
    rather than writing to ./downloads, the bytes are extracted (PDFs via
    pypdf, text-likes decoded directly) and returned as TextContent.
    """
    course_id = args["course_id"]
    content_id = args["content_id"]
    fmt = _resolve_response_format(args)

    item, handler_id, pairs = await _resolve_content_files(client, course_id, content_id)

    if not pairs:
        payload = {
            "contentId": content_id,
            "title": item.get("title"),
            "contentHandlerId": handler_id,
            "files": [],
            "skipped": [],
            "error": "No download URL found. Content handler type may not be supported.",
        }
        text = (
            f"# No content\n\nItem **{item.get('title', '?')}** "
            f"(handler `{handler_id}`) has no resolvable file links."
        ) if fmt == "markdown" else None
        return _emit(payload, text)

    files_out: list[dict[str, Any]] = []
    skipped: list[dict[str, Any]] = []
    total_bytes = 0

    for url, detected_filename in pairs:
        filename = detected_filename
        if not filename:
            url_path = url.split("?")[0]
            filename = unquote(url_path.split("/")[-1]) or "download"

        if total_bytes >= _MAX_TOTAL_BYTES:
            skipped.append({
                "filename": filename,
                "reason": (
                    f"Skipped: cumulative batch size already exceeds "
                    f"{_format_bytes(_MAX_TOTAL_BYTES)} cap. Use ntulearn_download_file."
                ),
            })
            continue

        try:
            content_bytes, content_type = await client.download_bytes(url)
        except BbRouterExpiredError:
            client = await _refresh_client()
            content_bytes, content_type = await client.download_bytes(url)

        size = len(content_bytes)
        if size > _MAX_FILE_BYTES:
            skipped.append({
                "filename": filename,
                "reason": (
                    f"File too large ({_format_bytes(size)} > "
                    f"{_format_bytes(_MAX_FILE_BYTES)} cap). Use ntulearn_download_file."
                ),
                "sizeBytes": size,
                "contentType": content_type,
            })
            continue

        if total_bytes + size > _MAX_TOTAL_BYTES:
            skipped.append({
                "filename": filename,
                "reason": (
                    f"Skipped: would exceed batch cap of "
                    f"{_format_bytes(_MAX_TOTAL_BYTES)}. Use ntulearn_download_file."
                ),
                "sizeBytes": size,
                "contentType": content_type,
            })
            continue

        total_bytes += size
        entry = _extract_content(filename, content_type, content_bytes)
        if entry.get("kind") == "binary":
            skipped.append({
                "filename": filename,
                "reason": entry["error"],
                "sizeBytes": size,
                "contentType": content_type,
            })
        else:
            files_out.append(entry)

    payload = {
        "contentId": content_id,
        "title": item.get("title"),
        "files": files_out,
        "skipped": skipped,
    }
    text = _md_files(payload, "File contents") if fmt == "markdown" else None
    return _emit(payload, text)


async def _get_announcements(
    client: NTULearnClient, args: dict[str, Any]
) -> tuple[list[TextContent], dict[str, Any]]:
    course_id = args["course_id"]
    offset, limit = _resolve_pagination_args(args)
    fmt = _resolve_response_format(args)

    announcements = await client.get_announcements(course_id)

    rows = []
    for a in announcements:
        body_raw = a.get("body") or {}
        rows.append({
            "id": a.get("id"),
            "title": a.get("title"),
            "body": (body_raw.get("rawText") if isinstance(body_raw, dict) else body_raw),
            "created": a.get("created"),
            "modified": a.get("modified"),
            "available": (a.get("availability") or {}).get("available"),
        })

    page, meta = _slice_with_pagination(rows, offset, limit)
    payload = {"announcements": page, **meta}
    text = _md_announcements(page, meta) if fmt == "markdown" else None
    return _emit(payload, text)


async def _get_gradebook(
    client: NTULearnClient, args: dict[str, Any]
) -> tuple[list[TextContent], dict[str, Any]]:
    course_id = args["course_id"]
    offset, limit = _resolve_pagination_args(args)
    fmt = _resolve_response_format(args)

    columns = await client.get_gradebook_columns(course_id)
    grades_available = True
    grade_fetch_error: str | None = None

    try:
        user_id = await client.get_my_user_id()
        grades_raw = await client.get_user_grades(course_id, user_id)
    except BbRouterExpiredError:
        raise
    except Exception as e:
        grades_available = False
        grade_fetch_error = str(e)
        grades_raw = []

    if grades_available:
        grade_map: dict[str, dict[str, Any]] = {
            g["columnId"]: g for g in grades_raw if "columnId" in g
        }
    else:
        grade_map = {}

    columns_result = []
    for col in columns:
        col_id = col.get("id")
        score = col.get("score") or {}
        grade_entry = grade_map.get(col_id, {})
        columns_result.append({
            "id": col_id,
            "name": col.get("name"),
            "displayName": col.get("displayName"),
            "possible": score.get("possible"),
            "available": (col.get("availability") or {}).get("available"),
            "contentId": col.get("contentId"),
            "score": grade_entry.get("score"),
            "grade": grade_entry.get("grade"),
            "status": grade_entry.get("status"),
        })

    page, meta = _slice_with_pagination(columns_result, offset, limit)
    payload = {
        "columns": page,
        **meta,
        "gradesAvailable": grades_available,
        "gradeFetchError": grade_fetch_error,
    }
    text = (
        _md_gradebook(page, meta, grades_available, grade_fetch_error)
        if fmt == "markdown"
        else None
    )
    return _emit(payload, text)


# ---------------------------------------------------------------------------
# Entrypoint
# ---------------------------------------------------------------------------

async def _run() -> None:
    async with stdio_server() as (read_stream, write_stream):
        await app.run(
            read_stream,
            write_stream,
            app.create_initialization_options(),
        )


def main() -> None:
    asyncio.run(_run())


if __name__ == "__main__":
    main()
