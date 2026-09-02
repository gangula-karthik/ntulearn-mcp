"""NTULearn MCP tool handlers (v0.3 capability suite).

One async handler per new tool. Every handler has the same contract::

    async def handle_<name>(client, args) -> tuple[list[TextContent], dict]

That is exactly the (unstructured, structured) pair ``server._emit`` returns,
so ``server.py`` can wire these up directly. Nothing in this module imports
``server.py`` — the shared, import-safe helpers live in ``common.py``.
"""

from __future__ import annotations

import asyncio
import csv
import io
import os
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from mcp.types import TextContent

from ntulearn_mcp import common

# ---------------------------------------------------------------------------
# Content-kind tables (mirrors the folder/file split server.py uses for
# downloads and search breadcrumbs).
# ---------------------------------------------------------------------------

_FILE_HANDLERS = frozenset({
    "resource/x-bb-document",
    "resource/x-bb-file",
    "resource/x-bb-external-link",
    "resource/x-bb-assignment",
    "resource/x-bb-asynch-assignment",
    "resource/x-bb-testsurvey_pool",
})
_FOLDER_HANDLERS = frozenset({
    "resource/x-bb-folder",
    "resource/x-bb-module",
    "resource/x-bb-courselink",
    "resource/x-bb-contentlink",
})

_INSTRUCTOR_ROLES = frozenset({
    "Instructor", "TeachingAssistant", "CourseBuilder", "CourseSupport",
})
_ASSIGNMENT_TYPES = ("GradebookColumn", "Assignment", "Test", "Survey")


def _handler_id(item: dict[str, Any]) -> str:
    return ((item.get("contentHandler") or {}).get("id") or "")


def _is_folder(item: dict[str, Any]) -> bool:
    if item.get("hasChildren") is True:
        return True
    hid = _handler_id(item)
    if not hid:
        return False
    if hid in _FOLDER_HANDLERS:
        return True
    return not hid.startswith("resource/x-bb-")


def _is_file_item(item: dict[str, Any]) -> bool:
    hid = _handler_id(item)
    if hid in _FILE_HANDLERS:
        return True
    if hid in _FOLDER_HANDLERS or not hid:
        return False
    return hid.startswith("resource/x-bb-")


def _item_title(item: dict[str, Any]) -> str:
    return item.get("title") or item.get("name") or "untitled"


def _item_description(item: dict[str, Any]) -> str:
    desc = item.get("description")
    if isinstance(desc, dict):
        desc = desc.get("text") or desc.get("rawText") or ""
    return common.strip_html(desc)


def _user_name(user: dict[str, Any]) -> str:
    name = user.get("name") or {}
    if isinstance(name, dict):
        given = name.get("given") or ""
        family = name.get("family") or ""
        full = " ".join(x for x in (given, family) if x)
        if full:
            return full
    return user.get("userName") or ""


def _user_role(user: dict[str, Any]) -> str:
    return user.get("courseRoleId") or user.get("role") or ""


# ---------------------------------------------------------------------------
# Content-tree walker + search
# ---------------------------------------------------------------------------


@dataclass
class ContentNode:
    """A resolved node in the course content tree."""

    item: dict[str, Any]
    breadcrumb: list[str] = field(default_factory=list)
    depth: int = 0

    @property
    def id(self) -> str:
        return self.item.get("id") or ""

    @property
    def title(self) -> str:
        return _item_title(self.item)


async def walk_content(
    client: Any, course_id: str, max_depth: int = 10
) -> list[ContentNode]:
    """Walk ``course_id`` from its root, returning every reachable node.

    Recursion is bounded by ``max_depth`` and a visited set (guards against
    accidental cycles in unusual courses). Node fetches that raise (course
    reorganisation, missing children endpoint) are treated as leaves rather
    than fatal errors.
    """
    nodes: list[ContentNode] = []
    seen: set[str] = set()

    async def rec(breadcrumb: list[str], content_id: str | None, depth: int) -> None:
        if depth > max_depth:
            return
        try:
            if content_id is None:
                children = await client.get_course_contents(course_id)
            else:
                children = await client.get_content_children(course_id, content_id)
        except Exception:
            return
        if not children:
            return
        for raw in children:
            item = dict(raw or {})
            cid = item.get("id")
            if not cid or cid in seen:
                continue
            seen.add(cid)
            crumb = breadcrumb + [_item_title(item)]
            nodes.append(ContentNode(item=item, breadcrumb=crumb, depth=depth))
            if _is_folder(item):
                await rec(crumb, cid, depth + 1)

    await rec([], None, 0)
    return nodes


async def search_course(
    client: Any, course_id: str, query: str, max_depth: int
) -> list[dict[str, Any]]:
    """Return content matches for ``query`` in ``course_id`` (breadcrumbed).

    Tries the Blackboard ``contents?search=`` backend first (cheap, server-side)
    and falls back to a client-side walk when it returns nothing.
    """
    lowered = query.lower()
    matches: list[dict[str, Any]] = []
    seen_ids: set[str] = set()

    async def add(item: dict[str, Any], breadcrumb: list[str]) -> None:
        title = _item_title(item)
        desc = _item_description(item)
        if lowered not in title.lower() and lowered not in desc.lower():
            return
        cid = item.get("id")
        if cid in seen_ids:
            return
        seen_ids.add(cid)
        matches.append({
            "courseId": course_id,
            "id": cid,
            "title": title,
            "kind": ("folder" if _is_folder(item) else "file"),
            "breadcrumb": breadcrumb,
            "modified": item.get("modified") or "",
            "description": desc,
        })

    try:
        server_matches = await client.get_course_search(course_id, query)
        if server_matches:
            for sm in server_matches:
                item = dict(sm or {})
                await add(item, [_item_title(item)])
            if matches:
                return matches
    except Exception:
        pass

    for node in await walk_content(client, course_id, max_depth=max_depth):
        await add(node.item, node.breadcrumb)
    return matches

# ---------------------------------------------------------------------------
# Download machinery (shared by handle_download_course)
# ---------------------------------------------------------------------------


@dataclass
class DownloadJob:
    course_id: str
    course_folder: Path
    content_title: str
    url: str
    raw_name: str
    safe_name: str = ""
    target_name: str = ""


async def _collect_download_jobs(
    client: Any, course_id: str, max_depth: int
) -> list[DownloadJob]:
    """Walk a course and resolve every file attachment to a download job."""
    jobs: list[DownloadJob] = []
    handler = (await client.get_course(course_id)) or {}
    course_name = common.safe_folder_name(
        handler.get("name") or handler.get("displayName") or course_id
    )
    course_folder = Path(f"{course_id} - {course_name}")
    for node in await walk_content(client, course_id, max_depth=max_depth):
        if not _is_file_item(node.item):
            continue
        item = node.item
        try:
            attachments = await client.get_attachments(course_id, item["id"])
        except Exception:
            attachments = []
        for att in attachments:
            att_id = att.get("id")
            if not att_id:
                continue
            try:
                url = await client.get_attachment_download_url(
                    course_id, item["id"], att_id
                )
            except Exception:
                continue
            raw_name = str(att.get("fileName") or _item_title(item) or "file")
            jobs.append(
                DownloadJob(
                    course_id=course_id,
                    course_folder=course_folder,
                    content_title=_item_title(item),
                    url=url,
                    raw_name=raw_name,
                    safe_name=common.sanitize_filename(raw_name),
                )
            )
    return jobs


async def _download_worker(
    client: Any,
    job: DownloadJob,
    dest_root: Path,
    *,
    sem: asyncio.Semaphore,
    skip_existing: bool,
    results: list[dict[str, Any]],
    skipped: list[dict[str, Any]],
    used_names: dict[str, set[str]],
    ext_filter: set[str] | None,
) -> None:
    folder = dest_root / job.course_folder
    await asyncio.to_thread(folder.mkdir, parents=True, exist_ok=True)
    name = job.target_name

    async with sem:
        if ext_filter is not None and _extension(name) not in ext_filter:
            skipped.append({
                "filename": name,
                "courseFolder": str(job.course_folder),
                "reason": "extension_filter",
            })
            return
        # Check the deduplicated *target* name, not the raw safe name: two
        # jobs with identical raw names run concurrently and would otherwise
        # race (one writes Syllabus.pdf, the other sees it and skips).
        if skip_existing and (folder / name).exists():
            skipped.append({
                "filename": name,
                "courseFolder": str(job.course_folder),
                "reason": "already_exists",
            })
            return
        local_path = folder / name
        try:
            content, _ = await client.download_bytes(job.url)
        except Exception as exc:
            skipped.append({
                "filename": name,
                "courseFolder": str(job.course_folder),
                "reason": f"download_failed: {type(exc).__name__}",
            })
            return
        await asyncio.to_thread(local_path.write_bytes, content)
        results.append({
            "filename": name,
            "courseFolder": str(job.course_folder),
            "localPath": str(local_path),
            "sizeBytes": len(content),
        })


def _extension(filename: str) -> str:
    if "." not in filename:
        return ""
    return filename.rpartition(".")[2].lower()


def _parse_extensions(raw: str | None) -> set[str] | None:
    """Accept 'pdf, docx' style CSV of extensions; None means accept all."""
    if raw is None:
        return None
    parts = [p.strip().lower().lstrip(".") for p in raw.split(",") if p.strip()]
    return set(parts) if parts else None


# ---------------------------------------------------------------------------
# Gradebook helpers
# ---------------------------------------------------------------------------


async def _grade_brief(client: Any, course_id: str, user_id: str | None) -> dict[str, Any]:
    """Own-grade summary for a course: columns, possible totals, scores."""
    brief: dict[str, Any] = {}
    try:
        columns = await client.get_gradebook_columns(course_id)
        brief["columnCount"] = len(columns)
        graded = 0
        total_possible = 0.0
        earned = 0.0
        scored: list[dict[str, Any]] = []
        for col in columns:
            col_id = col.get("id")
            if not col_id:
                continue
            possible = _column_possible(col)
            total_possible += possible or 0.0
            per = {"columnId": col_id, "name": _column_name(col), "possible": possible}
            if user_id:
                try:
                    grades = await client.get_user_grades(course_id, user_id)
                except Exception:
                    grades = []
                own = next((g for g in grades if g.get("columnId") == col_id), None)
                if own is not None:
                    score = _grade_score(own)
                    per["score"] = score
                    per["status"] = own.get("status") or "OK"
                    if score is not None and possible:
                        graded += 1
                        earned += float(score)
                    scored.append(per)
                    continue
            per["status"] = "NoGrade"
            scored.append(per)
        brief["columnsWithScore"] = graded
        brief["totalPossible"] = round(total_possible, 2)
        if graded and total_possible:
            brief["averagePercent"] = round(100.0 * earned / total_possible, 1)
    except Exception:
        return brief
    return brief


def _column_name(col: dict[str, Any]) -> str:
    return col.get("name") or col.get("displayName") or col.get("id") or ""


def _column_possible(col: dict[str, Any]) -> float | None:
    score = col.get("score") or {}
    possible = score.get("possible") if isinstance(score, dict) else None
    if possible is None:
        possible = col.get("possible")
    try:
        return float(possible) if possible is not None else None
    except (TypeError, ValueError):
        return None


def _grade_score(grade: dict[str, Any]) -> float | None:
    score = grade.get("score") or {}
    if isinstance(score, dict):
        raw = score.get("score")
        if raw is None:
            raw = score.get("value")
    else:
        raw = grade.get("score")
    try:
        return float(raw) if raw is not None else None
    except (TypeError, ValueError):
        return None


# ---------------------------------------------------------------------------
# Course summary builder (shared by handle_summarize_course + resource)
# ---------------------------------------------------------------------------


async def build_course_summary(
    client: Any, course_id: str, *, include_contents: bool = True
) -> dict[str, Any]:
    """Best-effort one-call course briefing. Every sub-section degrades
    gracefully: errors are collected in ``courseErrors`` instead of raising
    (the one exception is BbRouterExpiredError, which the server must surface
    so it can refresh the cookie and retry)."""
    summary: dict[str, Any] = {
        "courseId": course_id,
        "courseErrors": [],
    }
    try:
        course = await client.get_course(course_id)
        course = course or {}
        summary["title"] = (
            course.get("name") or course.get("displayName") or course_id
        )
        desc = course.get("description")
        summary["description"] = _item_description(course) if isinstance(desc, str) else ""
        term_id = course.get("termId")
        if term_id:
            try:
                term = await client.get_term(term_id)
                summary["term"] = {
                    "id": term_id,
                    "name": (term or {}).get("name") or "",
                    "start": (term or {}).get("startDate") or "",
                    "end": (term or {}).get("endDate") or "",
                }
            except Exception as exc:
                summary["courseErrors"].append({"section": "term", "error": str(exc)})
    except Exception as exc:
        summary["courseErrors"].append({"section": "course", "error": str(exc)})

    # Instructors + enrollment count (roster may not be visible to all users)
    try:
        users = await client.get_course_users(course_id)
        summary["enrollmentCount"] = len(users)
        summary["instructors"] = [
            {"id": u.get("id"), "name": _user_name(u)}
            for u in users
            if _user_role(u) in _INSTRUCTOR_ROLES
        ][:10]
    except Exception as exc:
        summary["courseErrors"].append({"section": "roster", "error": str(exc)})

    try:
        cal = await client.get_calendar_items(course_id=course_id)
        summary["upcoming"] = [
            _calendar_brief(i)
            for i in cal
            if (i.get("type") in _ASSIGNMENT_TYPES or True)
        ][:10]
    except Exception as exc:
        summary["courseErrors"].append({"section": "calendar", "error": str(exc)})

    try:
        anns = await client.get_announcements(course_id)
        summary["recentAnnouncements"] = [
            {"id": a.get("id"), "title": a.get("title"), "created": a.get("created")}
            for a in anns
        ][:5]
    except Exception as exc:
        summary["courseErrors"].append({"section": "announcements", "error": str(exc)})

    try:
        user_id = await client.get_my_user_id()
    except Exception:
        user_id = None
    try:
        summary["gradeSummary"] = await _grade_brief(client, course_id, user_id)
    except Exception as exc:
        summary["courseErrors"].append({"section": "gradebook", "error": str(exc)})

    if include_contents:
        try:
            root = await client.get_course_contents(course_id)
            summary["contentTopFolders"] = [
                {
                    "id": item.get("id"),
                    "title": _item_title(item),
                    "hasChildren": bool(_is_folder(item)),
                }
                for item in root
            ][:20]
        except Exception as exc:
            summary["courseErrors"].append({"section": "contents", "error": str(exc)})
    return summary


def _calendar_brief(item: dict[str, Any]) -> dict[str, Any]:
    return {
        "id": item.get("id"),
        "title": item.get("title"),
        "type": item.get("type"),
        "start": item.get("start"),
        "end": item.get("end"),
    }


def _announcement_text(ann: dict[str, Any]) -> str:
    body = ann.get("body")
    if isinstance(body, dict):
        body = body.get("text") or body.get("rawText") or body.get("text", "")
    return common.strip_html(body)


# ---------------------------------------------------------------------------
# ICS / CSV builders
# ---------------------------------------------------------------------------


def _ics_escape(text: Any) -> str:
    return (
        str(text or "")
        .replace("\\", "\\\\")
        .replace(";", "\\;")
        .replace(",", "\\,")
        .replace("\n", "\\n")
    )


def build_ics(items: list[dict[str, Any]], *, scope: str) -> str:
    """Minimal RFC-5545 calendar. All timestamps are emitted as UTC with 'Z'."""
    lines = [
        "BEGIN:VCALENDAR",
        "VERSION:2.0",
        "PRODID:-//ntulearn-mcp//NTULearn calendar export//EN",
        "CALSCALE:GREGORIAN",
    ]
    for item in items:
        uid = item.get("uid") or f"{item.get('courseId') or 'x'}-{item.get('id') or 'x'}"
        title = _ics_escape(item.get("title") or "Untitled event")
        desc = _ics_escape(item.get("description") or "")
        location = _ics_escape(item.get("location") or "")
        start = _ics_dt(item.get("start"))
        end = _ics_dt(item.get("end")) or start
        lines.append("BEGIN:VEVENT")
        lines.append(f"UID:{uid}")
        lines.append(f"DTSTAMP:{_ics_dt(item.get('fetchedAt') or _now_dt())}")
        lines.append(f"DTSTART:{start}")
        lines.append(f"DTEND:{end}")
        lines.append(f"SUMMARY:{title}")
        if desc:
            lines.append(f"DESCRIPTION:{desc}")
        if location:
            lines.append(f"LOCATION:{location}")
        lines.append("END:VEVENT")
    lines.append("END:VCALENDAR")
    return "\r\n".join(lines)


def _now_dt() -> str:
    return datetime.now(timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")


def _ics_dt(value: Any) -> str:
    """Coerce an ISO string to a UTC 'Z' timestamp (assumes epoch 0 fallback)."""
    if not value:
        return "19700101T000000Z"
    try:
        dt = common.parse_iso(str(value))
    except Exception:
        return "19700101T000000Z"
    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=timezone.utc)
    dt = dt.astimezone(timezone.utc)
    return dt.strftime("%Y%m%dT%H%M%SZ")


def build_gradebook_csv(rows: list[dict[str, Any]]) -> str:
    out = io.StringIO()
    writer = csv.DictWriter(
        out,
        fieldnames=[
            "courseId", "columnId", "columnName", "possible", "score",
            "status", "grade",
        ],
    )
    writer.writeheader()
    for row in rows:
        writer.writerow({k: row.get(k, "") for k in writer.fieldnames})
    return out.getvalue()


# ---------------------------------------------------------------------------
# Tool handlers
# ---------------------------------------------------------------------------


async def handle_list_messages(
    client: Any, args: dict[str, Any]
) -> tuple[list[TextContent], dict[str, Any]]:
    """ntulearn_list_messages — read the user's Blackboard messages inbox."""
    folder = str(args.get("folder", "inbox")).lower()
    unread_only = bool(args.get("unread_only", False))
    since = args.get("since")
    if since:
        common.validate_iso8601(since, name="since")
    offset, limit = common.resolve_pagination_args(args)
    messages = await client.get_messages(
        folder=folder, unread_only=unread_only, since=since
    )
    messages = messages or []
    page, meta = common.slice_with_pagination(messages, offset, limit)
    payload = {
        "folder": folder,
        "unreadOnly": unread_only,
        "messages": page,
        **meta,
    }
    fmt = common.resolve_response_format(args)
    if fmt == "markdown":
        from ntulearn_mcp import render

        return common.emit(payload, render.md_messages(payload))
    return common.emit(payload)


async def handle_read_message(
    client: Any, args: dict[str, Any]
) -> tuple[list[TextContent], dict[str, Any]]:
    """ntulearn_read_message — fetch one message + its participants."""
    message_id = common.validate_bb_id(args["message_id"], name="message_id")
    msg = await client.get_message(message_id)
    msg = msg or {}
    body = msg.get("body")
    if isinstance(body, dict):
        body = body.get("text") or body.get("rawText") or ""
    payload: dict[str, Any] = {
        "id": message_id,
        "subject": msg.get("subject") or "",
        "body": common.strip_html(body),
        "created": msg.get("created") or "",
        "read": bool(msg.get("read", True)),
        "folder": msg.get("folder") or "",
        "senderId": msg.get("fromUserId") or "",
    }
    try:
        participants = await client.get_message_participants(message_id)
        participants = participants or []
        payload["recipients"] = [
            {"id": p.get("id"), "name": _user_name(p), "role": _user_role(p)}
            for p in participants
        ]
        sender = next(
            (p for p in participants if p.get("id") == payload.get("senderId")),
            None,
        )
        if sender is not None:
            payload["senderName"] = _user_name(sender)
    except Exception:
        payload["recipients"] = []
    if not payload.get("senderName"):
        payload["senderName"] = ""
    fmt = common.resolve_response_format(args)
    if fmt == "markdown":
        from ntulearn_mcp import render

        return common.emit(payload, render.md_message(payload))
    return common.emit(payload)


async def handle_list_course_users(
    client: Any, args: dict[str, Any]
) -> tuple[list[TextContent], dict[str, Any]]:
    """ntulearn_list_course_users — roster for one course."""
    course_id = common.validate_bb_id(args["course_id"], name="course_id")
    offset, limit = common.resolve_pagination_args(args)
    users = await client.get_course_users(course_id)
    users = users or []
    page, meta = common.slice_with_pagination(users, offset, limit)
    payload = {
        "courseId": course_id,
        "users": [
            {
                "id": u.get("id"),
                "userName": u.get("userName") or "",
                "name": _user_name(u),
                "role": _user_role(u),
            }
            for u in page
        ],
        **meta,
    }
    fmt = common.resolve_response_format(args)
    if fmt == "markdown":
        from ntulearn_mcp import render

        return common.emit(payload, render.md_course_users(payload))
    return common.emit(payload)


async def handle_list_course_groups(
    client: Any, args: dict[str, Any]
) -> tuple[list[TextContent], dict[str, Any]]:
    """ntulearn_list_course_groups — group list for one course."""
    course_id = common.validate_bb_id(args["course_id"], name="course_id")
    offset, limit = common.resolve_pagination_args(args)
    groups = await client.get_course_groups(course_id)
    groups = groups or []
    page, meta = common.slice_with_pagination(groups, offset, limit)
    include_members = bool(args.get("include_members", False))
    rendered: list[dict[str, Any]] = []
    for g in page:
        entry = {
            "id": g.get("id"),
            "name": g.get("name") or g.get("title") or "",
            "description": common.strip_html(g.get("description")),
            "available": ((g.get("availability") or {}).get("available") == "Yes"),
        }
        if include_members:
            try:
                members = await client.get_group_users(course_id, g.get("id"))
                entry["memberCount"] = len(members or [])
            except Exception:
                entry["memberCount"] = 0
        rendered.append(entry)
    payload = {"courseId": course_id, "groups": rendered, **meta}
    fmt = common.resolve_response_format(args)
    if fmt == "markdown":
        from ntulearn_mcp import render

        return common.emit(payload, render.md_course_groups(payload))
    return common.emit(payload)


async def handle_get_group_members(
    client: Any, args: dict[str, Any]
) -> tuple[list[TextContent], dict[str, Any]]:
    """ntulearn_get_group_members — members of one group."""
    course_id = common.validate_bb_id(args["course_id"], name="course_id")
    group_id = common.validate_bb_id(args["group_id"], name="group_id")
    offset, limit = common.resolve_pagination_args(args)
    users = await client.get_group_users(course_id, group_id)
    users = users or []
    page, meta = common.slice_with_pagination(users, offset, limit)
    payload = {
        "courseId": course_id,
        "groupId": group_id,
        "users": [
            {
                "id": u.get("id"),
                "userName": u.get("userName") or "",
                "name": _user_name(u),
                "role": _user_role(u),
            }
            for u in page
        ],
        **meta,
    }
    fmt = common.resolve_response_format(args)
    if fmt == "markdown":
        from ntulearn_mcp import render

        return common.emit(payload, render.md_group_members(payload))
    return common.emit(payload)


async def handle_get_gradebook_attempts(
    client: Any, args: dict[str, Any]
) -> tuple[list[TextContent], dict[str, Any]]:
    """ntulearn_get_gradebook_attempts — attempts for a gradebook column."""
    course_id = common.validate_bb_id(args["course_id"], name="course_id")
    column_id = common.validate_bb_id(args["column_id"], name="column_id")
    user_id = args.get("user_id")
    if user_id:
        user_id = common.validate_bb_id(user_id, name="user_id")
    offset, limit = common.resolve_pagination_args(args)
    if user_id:
        attempts = await client.get_user_attempts(course_id, column_id, user_id)
    else:
        attempts = await client.get_gradebook_attempts(course_id, column_id)
    attempts = attempts or []
    page, meta = common.slice_with_pagination(attempts, offset, limit)
    payload = {
        "courseId": course_id,
        "columnId": column_id,
        "attempts": [
            {
                "id": a.get("id"),
                "userId": a.get("userId") or a.get("user") or "",
                "status": a.get("status") or "",
                "score": _grade_score(a),
                "cumulatedScore": _grade_score(a.get("cumulatedScore") or {}),
                "feedback": common.strip_html(a.get("feedback")),
                "created": a.get("created") or a.get("createdAt") or "",
            }
            for a in page
        ],
        **meta,
    }
    fmt = common.resolve_response_format(args)
    if fmt == "markdown":
        from ntulearn_mcp import render

        return common.emit(payload, render.md_gradebook_attempts(payload))
    return common.emit(payload)


async def handle_search_all_courses(
    client: Any, args: dict[str, Any]
) -> tuple[list[TextContent], dict[str, Any]]:
    """ntulearn_search_all_courses — find content matching a query across the
    user's enrolled courses."""
    query = str(args.get("query", "")).strip()
    if not query:
        raise ValueError("query must be a non-empty string")
    max_depth = int(args.get("max_depth", 3))
    max_depth = max(1, min(max_depth, 10))
    max_results = int(args.get("max_results", 50))
    max_results = max(1, min(max_results, 200))
    course_ids = await common.fan_out_course_ids(client, args.get("course_ids"))
    matches: list[dict[str, Any]] = []
    course_errors: dict[str, str] = {}

    async def search_one(course_id: str) -> None:
        try:
            found = await search_course(client, course_id, query, max_depth)
            matches.extend(found)
        except Exception as exc:
            course_errors[course_id] = f"{type(exc).__name__}: {exc}"

    await asyncio.gather(*(search_one(cid) for cid in course_ids))

    matches = matches[: max_results]
    payload = {
        "query": query,
        "maxResults": len(matches),
        "coursesSearched": len(course_ids),
        "matches": matches,
        "courseErrors": course_errors,
    }
    fmt = common.resolve_response_format(args)
    if fmt == "markdown":
        from ntulearn_mcp import render

        return common.emit(payload, render.md_search_all_courses(payload))
    return common.emit(payload)


async def handle_get_content_tree(
    client: Any, args: dict[str, Any]
) -> tuple[list[TextContent], dict[str, Any]]:
    """ntulearn_get_content_tree — nested content-tree for one course."""
    course_id = common.validate_bb_id(args["course_id"], name="course_id")
    max_depth = int(args.get("max_depth", 5))
    max_depth = max(1, min(max_depth, 10))
    nodes = await walk_content(client, course_id, max_depth=max_depth)

    def to_node(node: ContentNode, nodes_by_depth: dict[int, list[ContentNode]]) -> dict[str, Any]:
        item = node.item
        children = [
            c for c in nodes_by_depth.get(node.depth + 1, [])
            if len(c.breadcrumb) > 1 and c.breadcrumb[:-1] == node.breadcrumb
        ]
        return {
            "id": node.id,
            "title": node.title,
            "kind": "folder" if _is_folder(item) else "file",
            "hasChildren": bool(_is_folder(item)),
            "children": [to_node(c, nodes_by_depth) for c in children][:100],
        }

    by_depth: dict[int, list[ContentNode]] = {}
    for n in nodes:
        by_depth.setdefault(n.depth, []).append(n)
    roots = by_depth.get(0, [])
    tree = [to_node(r, by_depth) for r in roots]
    payload = {
        "courseId": course_id,
        "count": len(tree),
        "totalNodes": len(nodes),
        "tree": tree,
    }
    fmt = common.resolve_response_format(args)
    if fmt == "markdown":
        from ntulearn_mcp import render

        return common.emit(payload, render.md_content_tree(payload))
    return common.emit(payload)

async def handle_download_course(
    client: Any, args: dict[str, Any]
) -> tuple[list[TextContent], dict[str, Any]]:
    """ntulearn_download_course — mirror a course's files to a local folder.

    This is a WRITE tool: it writes files under ``destination_dir``
    (default ~/Downloads/NTULearn/<course>) and never modifies anything else.
    """
    course_id = common.validate_bb_id(args["course_id"], name="course_id")
    dest_raw = str(args.get("destination_dir") or "").strip()
    course = (await client.get_course(course_id)) or {}
    course_name = common.safe_folder_name(
        course.get("name") or course.get("displayName") or course_id
    )
    if dest_raw:
        dest_root = Path(dest_raw).expanduser()
        if not dest_root.is_absolute():
            raise ValueError("destination_dir must be an absolute path")
    else:
        dest_root = (Path.home() / "Downloads" / "NTULearn")
    max_depth = int(args.get("max_depth", 3))
    max_depth = max(1, min(max_depth, 10))
    skip_existing = bool(args.get("skip_existing", True))
    parallel = int(args.get("parallel", 4))
    parallel = max(1, min(parallel, 16))
    ext_filter = _parse_extensions(args.get("include_extensions"))

    jobs = await _collect_download_jobs(client, course_id, max_depth=max_depth)
    # Assign every job a unique target name up front (before ANY download
    # starts) so concurrent writers can never collide on the same filename.
    used_names: dict[str, set[str]] = {}
    for job in jobs:
        used = used_names.setdefault(str(job.course_folder), set())
        name = job.safe_name
        stem, dot, ext = name.rpartition(".")
        base = stem if dot else name
        suffix = ext if dot else ""
        n = 2
        while name in used:
            name = f"{base} ({n}).{suffix}" if suffix else f"{base} ({n})"
            n += 1
        used.add(name)
        job.target_name = name
    sem = asyncio.Semaphore(parallel)
    results: list[dict[str, Any]] = []
    skipped: list[dict[str, Any]] = []
    await asyncio.gather(
        *(
            _download_worker(
                client,
                job,
                dest_root,
                sem=sem,
                skip_existing=skip_existing,
                results=results,
                skipped=skipped,
                used_names=used_names,
                ext_filter=ext_filter,
            )
            for job in jobs
        )
    )
    results.sort(key=lambda r: r["courseFolder"] + r["filename"])
    payload = {
        "courseId": course_id,
        "courseName": course_name,
        "destinationDir": str(dest_root),
        "downloadCount": len(results),
        "skippedCount": len(skipped),
        "totalBytes": sum(r["sizeBytes"] for r in results),
        "files": results,
        "skipped": skipped,
    }
    fmt = common.resolve_response_format(args)
    if fmt == "markdown":
        from ntulearn_mcp import render

        return common.emit(payload, render.md_download_course(payload))
    return common.emit(payload)


async def handle_whats_new(
    client: Any, args: dict[str, Any]
) -> tuple[list[TextContent], dict[str, Any]]:
    """ntulearn_whats_new — recent announcements/calendar/files per course."""
    course_ids = await common.fan_out_course_ids(client, args.get("course_ids"))
    since = args.get("since")
    if since:
        common.validate_iso8601(since, name="since")
    else:
        since = common.tracker_get_last_seen() or common.default_since()
    update_tracker = bool(args.get("update_tracker", False))

    per_course: list[dict[str, Any]] = []
    errors: dict[str, str] = {}

    async def one(course_id: str) -> None:
        entry: dict[str, Any] = {"courseId": course_id}
        try:
            course = await client.get_course(course_id)
            entry["courseName"] = (
                (course or {}).get("name") or (course or {}).get("displayName") or course_id
            )
        except Exception as exc:
            errors[course_id] = f"course: {exc}"
        try:
            anns = await client.get_announcements(course_id)
            entry["announcements"] = [
                {
                    "id": a.get("id"),
                    "title": a.get("title"),
                    "created": a.get("created"),
                    "body": _announcement_text(a)[:500],
                }
                for a in anns
                if _after(a.get("created"), since)
            ]
        except Exception as exc:
            errors.setdefault(course_id, f"announcements: {exc}")
        try:
            cal = await client.get_calendar_items(course_id=course_id, since=since)
            entry["upcoming"] = [
                _calendar_brief(i)
                for i in cal
                if _after(i.get("start"), since)
            ]
        except Exception as exc:
            errors.setdefault(course_id, f"calendar: {exc}")
        try:
            root = await client.get_course_contents(course_id)
            entry["newFiles"] = [
                {
                    "id": item.get("id"),
                    "title": _item_title(item),
                    "modified": item.get("modified") or "",
                }
                for item in root
                if _is_file_item(item) and _after(item.get("modified"), since)
            ]
        except Exception as exc:
            errors.setdefault(course_id, f"contents: {exc}")
        per_course.append(entry)

    await asyncio.gather(*(one(cid) for cid in course_ids))

    fetched_at = common.now_iso()
    if update_tracker:
        common.tracker_set_last_seen(fetched_at)
    total = {
        "announcements": sum(len(e.get("announcements", [])) for e in per_course),
        "upcoming": sum(len(e.get("upcoming", [])) for e in per_course),
        "newFiles": sum(len(e.get("newFiles", [])) for e in per_course),
    }
    payload = {
        "since": since,
        "fetchedAt": fetched_at,
        "courseCount": len(per_course),
        "summary": total,
        "courses": per_course,
        "courseErrors": errors,
    }
    fmt = common.resolve_response_format(args)
    if fmt == "markdown":
        from ntulearn_mcp import render

        return common.emit(payload, render.md_whats_new(payload))
    return common.emit(payload)


def _after(value: Any, threshold: str) -> bool:
    """True if ISO ``value`` is on/after ``threshold`` (missing values never match)."""
    if not value:
        return False
    try:
        return common.parse_iso(str(value)) >= common.parse_iso(threshold)
    except Exception:
        return False


async def handle_export_calendar_ics(
    client: Any, args: dict[str, Any]
) -> tuple[list[TextContent], dict[str, Any]]:
    """ntulearn_export_calendar_ics — all calendar items as an ICS file."""
    course_ids = await common.fan_out_course_ids(client, args.get("course_ids"))
    since = args.get("since") or common.iso_from_now(0, minute_of_day=0)
    until = args.get("until") or common.iso_from_now(30, minute_of_day=0)
    common.validate_iso8601(since, name="since")
    common.validate_iso8601(until, name="until")

    items: list[dict[str, Any]] = []
    errors: dict[str, str] = {}

    async def one(course_id: str) -> None:
        try:
            cal = await client.get_calendar_items(
                course_id=course_id, since=since, until=until
            )
            for i in cal or []:
                items.append({
                    **i,
                    "courseId": course_id,
                    "uid": f"{course_id}-{i.get('id')}",
                })
        except Exception as exc:
            errors[course_id] = f"{type(exc).__name__}: {exc}"

    await asyncio.gather(*(one(cid) for cid in course_ids))
    ics = build_ics(items, scope="; ".join(course_ids[:5]))
    payload = {
        "itemCount": len(items),
        "courseCount": len(course_ids),
        "since": since,
        "until": until,
        "supported": True,
        "ics": ics,
        "courseErrors": errors,
    }
    fmt = common.resolve_response_format(args)
    if fmt == "markdown":
        from ntulearn_mcp import render

        return common.emit(payload, render.md_export_calendar(payload))
    return common.emit(payload)


async def handle_export_gradebook_csv(
    client: Any, args: dict[str, Any]
) -> tuple[list[TextContent], dict[str, Any]]:
    """ntulearn_export_gradebook_csv — own grades across courses as CSV."""
    course_ids = await common.fan_out_course_ids(client, args.get("course_ids"))
    try:
        user_id = await client.get_my_user_id()
    except Exception:
        user_id = None
    rows: list[dict[str, Any]] = []
    errors: dict[str, str] = {}

    async def one(course_id: str) -> None:
        try:
            columns = await client.get_gradebook_columns(course_id)
            if user_id:
                grades = await client.get_user_grades(course_id, user_id)
            else:
                grades = []
            for col in columns:
                col_id = col.get("id")
                own = next((g for g in grades if g.get("columnId") == col_id), None)
                score = _grade_score(own)
                rows.append({
                    "courseId": course_id,
                    "columnId": col_id,
                    "columnName": _column_name(col),
                    "possible": _column_possible(col),
                    "score": score,
                    "status": (own or {}).get("status") if own else "",
                    "grade": "" if score is None else str(score),
                })
        except Exception as exc:
            errors[course_id] = f"{type(exc).__name__}: {exc}"

    await asyncio.gather(*(one(cid) for cid in course_ids))
    csv_text = build_gradebook_csv(rows)
    payload = {
        "rowCount": len(rows),
        "courseCount": len(course_ids),
        "csv": csv_text,
        "courseErrors": errors,
    }
    fmt = common.resolve_response_format(args)
    if fmt == "markdown":
        from ntulearn_mcp import render

        return common.emit(payload, render.md_export_gradebook(payload))
    return common.emit(payload)


async def handle_summarize_course(
    client: Any, args: dict[str, Any]
) -> tuple[list[TextContent], dict[str, Any]]:
    """ntulearn_summarize_course — one-call course briefing."""
    course_id = common.validate_bb_id(args["course_id"], name="course_id")
    include_contents = bool(args.get("include_contents", True))
    payload = await build_course_summary(
        client, course_id, include_contents=include_contents
    )
    fmt = common.resolve_response_format(args)
    if fmt == "markdown":
        from ntulearn_mcp import render

        return common.emit(payload, render.md_summarize_course(payload))
    return common.emit(payload)


# ---------------------------------------------------------------------------
# Registry: tool name (without the ntulearn_ prefix) -> handler callable.
# server.py imports this to wire dispatch without a hard import cycle.
# ---------------------------------------------------------------------------

REGISTRY: dict[str, Any] = {
    "list_messages": handle_list_messages,
    "read_message": handle_read_message,
    "list_course_users": handle_list_course_users,
    "list_course_groups": handle_list_course_groups,
    "get_group_members": handle_get_group_members,
    "get_gradebook_attempts": handle_get_gradebook_attempts,
    "search_all_courses": handle_search_all_courses,
    "get_content_tree": handle_get_content_tree,
    "download_course": handle_download_course,
    "whats_new": handle_whats_new,
    "export_calendar_ics": handle_export_calendar_ics,
    "export_gradebook_csv": handle_export_gradebook_csv,
    "summarize_course": handle_summarize_course,
}


def handle_for_tool(tool_name: str) -> Any | None:
    """Return the handler for a fully-prefixed tool name (ntulearn_<x>)."""
    if not tool_name.startswith("ntulearn_"):
        return None
    return REGISTRY.get(tool_name[len("ntulearn_"):])
