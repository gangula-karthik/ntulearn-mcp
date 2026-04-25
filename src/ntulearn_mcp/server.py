"""NTULearn MCP server — Blackboard Learn REST API wrapper."""

from __future__ import annotations

import asyncio
import os
from pathlib import Path
from typing import Any

from dotenv import load_dotenv
from mcp.server import Server
from mcp.server.stdio import stdio_server
from mcp.types import Tool, TextContent

from .client import NTULearnClient, BbRouterExpiredError, BlackboardAPIError
from .cookie import read_bbrouter_cookie
from .parsers import extract_all_files

load_dotenv()

# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------

BASE_URL = os.getenv("NTULEARN_BASE_URL", "https://ntulearn.ntu.edu.sg")
DOWNLOAD_DIR = Path(os.getenv("NTULEARN_DOWNLOAD_DIR", "./downloads"))

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

app = Server("ntulearn-mcp")
_client: NTULearnClient | None = None


def _resolve_cookie() -> str:
    """Resolve the BbRouter cookie. Explicit env var wins over browser auto-read."""
    explicit = os.getenv("NTULEARN_COOKIE", "").strip()
    if explicit:
        return explicit
    auto = read_bbrouter_cookie()
    if auto:
        return auto
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


def _err(e: Exception) -> list[TextContent]:
    return [TextContent(type="text", text=f"Error: {e}")]


# ---------------------------------------------------------------------------
# Tool definitions
# ---------------------------------------------------------------------------

@app.list_tools()
async def list_tools() -> list[Tool]:
    return [
        Tool(
            name="list_courses",
            description=(
                "List all courses the current user is enrolled in on NTULearn. "
                "By default returns only active/available courses. "
                "Set include_disabled=true to also include unavailable ones."
            ),
            inputSchema={
                "type": "object",
                "properties": {
                    "include_disabled": {
                        "type": "boolean",
                        "description": "Include courses where availability.available != 'Yes'",
                        "default": False,
                    }
                },
            },
        ),
        Tool(
            name="get_course_contents",
            description=(
                "Get the top-level content tree for a course. "
                "Returns folders, documents, links, and assignments at the root level. "
                "Use get_folder_children to drill into items where hasChildren=true."
            ),
            inputSchema={
                "type": "object",
                "properties": {
                    "course_id": {
                        "type": "string",
                        "description": "Blackboard course ID (e.g. _12345_1)",
                    }
                },
                "required": ["course_id"],
            },
        ),
        Tool(
            name="get_folder_children",
            description=(
                "Get the children of a folder or lesson within a course. "
                "Use this to drill into content items where hasChildren=true."
            ),
            inputSchema={
                "type": "object",
                "properties": {
                    "course_id": {"type": "string", "description": "Blackboard course ID"},
                    "content_id": {"type": "string", "description": "Content item ID of the folder"},
                },
                "required": ["course_id", "content_id"],
            },
        ),
        Tool(
            name="search_course_content",
            description=(
                "Recursively search a course's entire content tree for items matching a query. "
                "Matches on title or description (case-insensitive substring). "
                "Returns matched items with their full breadcrumb path."
            ),
            inputSchema={
                "type": "object",
                "properties": {
                    "course_id": {"type": "string", "description": "Blackboard course ID"},
                    "query": {"type": "string", "description": "Search term (case-insensitive)"},
                    "max_depth": {
                        "type": "integer",
                        "description": "Maximum recursion depth (default 5)",
                        "default": 5,
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Maximum number of matching items to return (default 50)",
                        "default": 50,
                        "minimum": 1,
                    },
                },
                "required": ["course_id", "query"],
            },
        ),
        Tool(
            name="get_file_download_url",
            description=(
                "Get the download URL for a file attached to a Blackboard content item. "
                "Parses the item's HTML body to extract bbcswebdav URLs. "
                "Returns the URL and filename. Use download_file to actually fetch it."
            ),
            inputSchema={
                "type": "object",
                "properties": {
                    "course_id": {"type": "string", "description": "Blackboard course ID"},
                    "content_id": {"type": "string", "description": "Content item ID"},
                },
                "required": ["course_id", "content_id"],
            },
        ),
        Tool(
            name="download_file",
            description=(
                "Download every file attached to a Blackboard content item into the local "
                "download directory. Handles both resource/x-bb-file (attachment API) and "
                "resource/x-bb-document (HTML body with bbcswebdav links) handler types. "
                "Returns a list of saved files with their local paths and sizes."
            ),
            inputSchema={
                "type": "object",
                "properties": {
                    "course_id": {"type": "string", "description": "Blackboard course ID"},
                    "content_id": {"type": "string", "description": "Content item ID"},
                },
                "required": ["course_id", "content_id"],
            },
        ),
        Tool(
            name="get_announcements",
            description="Get announcements for a course, newest first.",
            inputSchema={
                "type": "object",
                "properties": {
                    "course_id": {"type": "string", "description": "Blackboard course ID"},
                },
                "required": ["course_id"],
            },
        ),
        Tool(
            name="get_gradebook",
            description=(
                "Get gradebook columns for a course, including your scores where available. "
                "Returns column names, max scores, and your grade for each."
            ),
            inputSchema={
                "type": "object",
                "properties": {
                    "course_id": {"type": "string", "description": "Blackboard course ID"},
                },
                "required": ["course_id"],
            },
        ),
    ]


# ---------------------------------------------------------------------------
# Tool handlers
# ---------------------------------------------------------------------------

@app.call_tool()
async def call_tool(name: str, arguments: dict[str, Any]) -> list[TextContent]:
    try:
        return await _dispatch(name, arguments)
    except BbRouterExpiredError:
        # Cookie expired mid-session: try once to swap in a fresh one from the
        # user's browser, then retry. If either refresh or retry fails, surface.
        try:
            await _refresh_client()
        except Exception as e:
            return _err(e)
        try:
            return await _dispatch(name, arguments)
        except Exception as e:
            return _err(e)
    except Exception as e:
        return _err(e)


async def _dispatch(name: str, arguments: dict[str, Any]) -> list[TextContent]:
    client = get_client()
    if name == "list_courses":
        return await _list_courses(client, arguments)
    elif name == "get_course_contents":
        return await _get_course_contents(client, arguments)
    elif name == "get_folder_children":
        return await _get_folder_children(client, arguments)
    elif name == "search_course_content":
        return await _search_course_content(client, arguments)
    elif name == "get_file_download_url":
        return await _get_file_download_url(client, arguments)
    elif name == "download_file":
        return await _download_file(client, arguments)
    elif name == "get_announcements":
        return await _get_announcements(client, arguments)
    elif name == "get_gradebook":
        return await _get_gradebook(client, arguments)
    else:
        return _err(ValueError(f"Unknown tool: {name}"))


# ---------------------------------------------------------------------------
# Individual tool implementations
# ---------------------------------------------------------------------------

async def _list_courses(client: NTULearnClient, args: dict[str, Any]) -> list[TextContent]:
    import json

    include_disabled = args.get("include_disabled", False)
    enrollments = await client.get_my_enrollments()

    if not include_disabled:
        enrollments = [
            e for e in enrollments
            if (e.get("availability") or {}).get("available") == "Yes"
        ]

    course_ids = [e["courseId"] for e in enrollments]
    if not course_ids:
        return [TextContent(type="text", text=json.dumps([], indent=2))]

    # Build a map of lastAccessed per courseId from enrollments
    last_accessed_map: dict[str, str | None] = {
        e["courseId"]: e.get("lastAccessed") for e in enrollments
    }
    availability_map: dict[str, str] = {
        e["courseId"]: (e.get("availability") or {}).get("available", "Unknown")
        for e in enrollments
    }

    courses = await client.get_courses_batch(course_ids)

    result = []
    for course in courses:
        cid = course.get("id")
        result.append({
            "courseId": cid,
            "title": course.get("name") or course.get("displayName") or course.get("id"),
            "available": availability_map.get(cid, "Unknown"),
            "lastAccessed": last_accessed_map.get(cid),
        })

    result.sort(key=lambda c: c["lastAccessed"] or "", reverse=True)
    return [TextContent(type="text", text=json.dumps(result, indent=2))]


async def _get_course_contents(client: NTULearnClient, args: dict[str, Any]) -> list[TextContent]:
    import json

    course_id = args["course_id"]
    items = await client.get_course_contents(course_id)
    stripped = [_strip_content(item) for item in items]
    return [TextContent(type="text", text=json.dumps(stripped, indent=2))]


async def _get_folder_children(client: NTULearnClient, args: dict[str, Any]) -> list[TextContent]:
    import json

    course_id = args["course_id"]
    content_id = args["content_id"]
    items = await client.get_content_children(course_id, content_id)
    stripped = [_strip_content(item) for item in items]
    return [TextContent(type="text", text=json.dumps(stripped, indent=2))]


async def _search_course_content(client: NTULearnClient, args: dict[str, Any]) -> list[TextContent]:
    import json

    course_id = args["course_id"]
    query = str(args["query"]).strip().lower()
    if not query:
        return _err(ValueError("search_course_content query cannot be blank."))

    max_depth = int(args.get("max_depth", 5))
    max_results = max(1, int(args.get("max_results", 50)))

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

    return [TextContent(type="text", text=json.dumps(matches, indent=2))]


async def _get_file_download_url(client: NTULearnClient, args: dict[str, Any]) -> list[TextContent]:
    import json

    course_id = args["course_id"]
    content_id = args["content_id"]

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
        result = {
            "contentId": content_id,
            "title": item.get("title"),
            "files": files,
        }
        return [TextContent(type="text", text=json.dumps(result, indent=2))]

    # resource/x-bb-document and others: parse HTML body
    body = item.get("body") or ""
    files = extract_all_files(body)
    if not files:
        desc = item.get("description") or {}
        body2 = (desc.get("rawText") if isinstance(desc, dict) else desc) or ""
        files = extract_all_files(body2)

    if not files:
        return [TextContent(type="text", text=json.dumps({
            "error": "No download URL found. Content handler type may not be supported.",
            "contentHandlerId": handler_id,
            "title": item.get("title"),
        }, indent=2))]

    result = {
        "contentId": content_id,
        "title": item.get("title"),
        "files": files,
    }
    return [TextContent(type="text", text=json.dumps(result, indent=2))]


async def _download_file(client: NTULearnClient, args: dict[str, Any]) -> list[TextContent]:
    import json
    import re
    from urllib.parse import unquote

    course_id = args["course_id"]
    content_id = args["content_id"]

    item = await client.get_content_item(course_id, content_id)
    handler_id = (item.get("contentHandler") or {}).get("id")

    # Collect (url, filename) pairs for every file attached to this item.
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

    if not pairs:
        return [TextContent(type="text", text=json.dumps({
            "error": "No download URL found. Content handler type may not be supported.",
            "contentHandlerId": handler_id,
            "title": item.get("title"),
        }, indent=2))]

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

        # Disambiguate against this batch and files already on disk.
        filename = _deduplicate(filename)
        used_names.add(filename)

        dest = DOWNLOAD_DIR / filename
        # Refresh the cookie inline so already-saved files in this batch
        # aren't re-downloaded under a deduped name on a top-level retry.
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

    result = {
        "contentId": content_id,
        "title": item.get("title"),
        "files": saved,
    }
    return [TextContent(type="text", text=json.dumps(result, indent=2))]


async def _get_announcements(client: NTULearnClient, args: dict[str, Any]) -> list[TextContent]:
    import json

    course_id = args["course_id"]
    announcements = await client.get_announcements(course_id)

    result = []
    for a in announcements:
        body_raw = a.get("body") or {}
        result.append({
            "id": a.get("id"),
            "title": a.get("title"),
            "body": (body_raw.get("rawText") if isinstance(body_raw, dict) else body_raw),
            "created": a.get("created"),
            "modified": a.get("modified"),
            "available": (a.get("availability") or {}).get("available"),
        })

    return [TextContent(type="text", text=json.dumps(result, indent=2))]


async def _get_gradebook(client: NTULearnClient, args: dict[str, Any]) -> list[TextContent]:
    import json

    course_id = args["course_id"]
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

    result = {
        "columns": columns_result,
        "gradesAvailable": grades_available,
        "gradeFetchError": grade_fetch_error,
    }
    return [TextContent(type="text", text=json.dumps(result, indent=2))]


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
