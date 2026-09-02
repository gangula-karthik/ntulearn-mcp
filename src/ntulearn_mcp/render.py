"""Markdown renderers for the ntulearn capability suite.

Each ``md_*`` function takes the same structured payload the handler returned
and returns a compact, copy-paste-friendly markdown string. Used only when
``response_format == "markdown"`` (the default is structured JSON).
"""

from __future__ import annotations

from typing import Any


def _dt(value: Any) -> str:
    return str(value or "—")


def _pct(count: int, total: int) -> str:
    return f"{round(100.0 * count / total)}%" if total else "—"


def md_messages(payload: dict[str, Any]) -> str:
    lines = [
        f"# Messages — {payload['folder']} "
        f"({payload['offset']}-{payload['offset'] + len(payload['messages'])} of {payload['total']})"
    ]
    for m in payload["messages"]:
        flag = "🟡" if m.get("read") else "🔴"
        lines.append(f"- {flag} **{m.get('subject') or '(no subject)'}**")
        lines.append(f"  - from {m.get('fromUserId') or '?'} · {_dt(m.get('created'))} · id={m.get('id')}")
    if not payload["messages"]:
        lines.append("_No messages._")
    return "\n".join(lines)


def md_message(payload: dict[str, Any]) -> str:
    body = (payload.get("body") or "").strip()
    lines = [
        f"# {payload.get('subject') or '(no subject)'}",
        f"- from: {payload.get('senderName') or payload.get('senderId') or '?'}",
        f"- created: {_dt(payload.get('created'))} · read: {payload.get('read')}",
    ]
    recips = payload.get("recipients") or []
    if recips:
        names = ", ".join(f"{r.get('name') or r.get('id')} ({r.get('role') or '?'})" for r in recips)
        lines.append(f"- recipients: {names}")
    if body:
        lines.append("")
        lines.append("---")
        lines.append(body)
    return "\n".join(lines)


def md_course_users(payload: dict[str, Any]) -> str:
    lines = [
        f"# Course roster — {payload['courseId']} "
        f"({payload['total']} users, showing {payload['offset']}-{payload['offset'] + len(payload['users'])})"
    ]
    for u in payload["users"]:
        lines.append(f"- **{u.get('name') or u.get('userName') or u.get('id')}** — {u.get('role') or '?'} ({u.get('userName') or ''})")
    if not payload["users"]:
        lines.append("_No users._")
    return "\n".join(lines)


def md_course_groups(payload: dict[str, Any]) -> str:
    lines = [f"# Groups — {payload['courseId']} ({payload['total']})"]
    for g in payload["groups"]:
        name = g.get("name") or g.get("id")
        avail = "✓" if g.get("available") else "✗"
        count = f" · {g.get('memberCount')} members" if g.get("memberCount") is not None else ""
        lines.append(f"- {avail} **{name}**{count} — id={g.get('id')}")
        if g.get("description"):
            lines.append(f"  - {g['description'][:160]}")
    if not payload["groups"]:
        lines.append("_No groups._")
    return "\n".join(lines)


def md_group_members(payload: dict[str, Any]) -> str:
    lines = [
        f"# Group {payload['groupId']} — {payload['courseId']} "
        f"({payload['total']} members)"
    ]
    for u in payload["users"]:
        lines.append(f"- **{u.get('name') or u.get('userName') or u.get('id')}** — {u.get('role') or '?'}")
    if not payload["users"]:
        lines.append("_No members._")
    return "\n".join(lines)


def md_gradebook_attempts(payload: dict[str, Any]) -> str:
    lines = [f"# Attempts — {payload['courseId']} / {payload['columnId']} ({payload['total']})"]
    for a in payload["attempts"]:
        score = a.get("score")
        score_txt = f"{score}/? " if score is not None else ""
        lines.append(f"- **{a.get('status') or 'attempt'}** {score_txt}— user {a.get('userId')} · id={a.get('id')} · {_dt(a.get('created'))}")
        if a.get("feedback"):
            lines.append(f"    - feedback: {str(a['feedback'])[:200]}")
    if not payload["attempts"]:
        lines.append("_No attempts._")
    return "\n".join(lines)


def _fmt_match(match: dict[str, Any]) -> str:
    return (
        f"- **{match.get('title') or match.get('id')}** "
        f"[{match.get('kind')} · {match.get('courseId')}] — "
        f"`{'/'.join(match.get('breadcrumb') or [])}`"
    )


def md_search_all_courses(payload: dict[str, Any]) -> str:
    lines = [
        f"# Search “{payload['query']}” — {payload['coursesSearched']} course(s), "
        f"{payload['maxResults']} result(s)"
    ]
    lines += [_fmt_match(m) for m in payload["matches"]]
    if payload.get("courseErrors"):
        lines.append("")
        lines.append(f"_⚠️ {len(payload['courseErrors'])} course(s) could not be searched._")
    if not payload["matches"]:
        lines.append("_No matches._")
    return "\n".join(lines)


def _render_tree_node(node: dict[str, Any], indent: int = 0) -> list[str]:
    pad = "  " * indent
    icon = "📁" if node["kind"] == "folder" else "📄"
    out = [f"{pad}{icon} {node.get('title') or node.get('id')}"]
    for ch in node.get("children") or []:
        out += _render_tree_node(ch, indent + 1)
    return out


def md_content_tree(payload: dict[str, Any]) -> str:
    lines = [
        f"# Content tree — {payload['courseId']} "
        f"({payload['count']} top-level, {payload['totalNodes']} nodes)"
    ]
    for node in payload["tree"]:
        lines += _render_tree_node(node)
    if not payload["tree"]:
        lines.append("_No content._")
    return "\n".join(lines)


def md_download_course(payload: dict[str, Any]) -> str:
    lines = [
        f"# Downloaded {payload['courseName']}",
        f"- files saved: **{payload['downloadCount']}** ({payload['totalBytes']:,} B)",
        f"- skipped: {payload['skippedCount']} · destination: `{payload['destinationDir']}`",
    ]
    folders: dict[str, int] = {}
    for f in payload["files"]:
        folders[f["courseFolder"]] = folders.get(f["courseFolder"], 0) + 1
    if folders:
        lines.append("")
        for folder, n in sorted(folders.items()):
            lines.append(f"- `{folder}` — {n} file(s)")
    return "\n".join(lines)


def md_whats_new(payload: dict[str, Any]) -> str:
    s = payload["summary"]
    lines = [
        f"# What's new since {_dt(payload['since'])}",
        f"- announcements: **{s['announcements']}** · upcoming: **{s['upcoming']}** · new files: **{s['newFiles']}**",
    ]
    for entry in payload["courses"]:
        name = entry.get("courseName") or entry["courseId"]
        labels = []
        for kind in ("announcements", "upcoming", "newFiles"):
            n = len(entry.get(kind, []))
            if n:
                labels.append(f"{kind}: {n}")
        if labels:
            lines.append(f"- **{name}** — " + ", ".join(labels))
            for ann in entry.get("announcements", [])[:3]:
                lines.append(f"    - 📢 {ann.get('title')} ({_dt(ann.get('created'))})")
            for cal in entry.get("upcoming", [])[:3]:
                lines.append(f"    - 📅 {cal.get('title')} ({_dt(cal.get('start'))})")
            for f in entry.get("newFiles", [])[:3]:
                lines.append(f"    - 📄 {f.get('title')} ({_dt(f.get('modified'))})")
    return "\n".join(lines)


def md_export_calendar(payload: dict[str, Any]) -> str:
    lines = [
        f"# Calendar export (.ics) — {payload['itemCount']} events from {payload['courseCount']} course(s)",
        f"- window: {_dt(payload['since'])} → {_dt(payload['until'])}",
        f"- you can import the ICS payload directly into most calendar apps.",
    ]
    return "\n".join(lines)


def md_export_gradebook(payload: dict[str, Any]) -> str:
    lines = [
        f"# Gradebook export (.csv) — {payload['rowCount']} rows across {payload['courseCount']} course(s)",
        "You can paste the CSV payload into a file and open it in Sheets/Excel.",
    ]
    return "\n".join(lines)


def md_summarize_course(payload: dict[str, Any]) -> str:
    errors = payload.get("courseErrors") or []
    lines = [f"# {payload.get('title') or payload['courseId']}"]
    if payload.get("description"):
        lines.append("")
        lines.append(payload["description"][:300])
    lines.append("")
    term = payload.get("term") or {}
    if term.get("name"):
        lines.append(f"- term: **{term['name']}** ({_dt(term.get('start'))} → {_dt(term.get('end'))})")
    instructors = payload.get("instructors") or []
    if instructors:
        names = ", ".join(i.get("name") or i.get("id") for i in instructors[:5])
        lines.append(f"- instructors: {names}")
    if payload.get("enrollmentCount") is not None:
        lines.append(f"- enrolled: {payload['enrollmentCount']} user(s)")
    tops = payload.get("contentTopFolders") or []
    if tops:
        lines.append("")
        lines.append("Top-level folders:")
        for f in tops[:10]:
            lines.append(f"  - {'📁' if f.get('hasChildren') else '📄'} {f.get('title') or f.get('id')}")
    grades = payload.get("gradeSummary") or {}
    if grades.get("columnCount"):
        avg = grades.get("averagePercent")
        avg_txt = f" · avg {avg}%" if avg is not None else ""
        lines.append(
            f"- gradebook: {grades.get('columnCount')} column(s), "
            f"{grades.get('columnsWithScore')} graded{avg_txt}"
        )
    upcoming = payload.get("upcoming") or []
    if upcoming:
        lines.append("")
        lines.append("Upcoming:")
        for u in upcoming[:5]:
            lines.append(f"  - {_dt(u.get('start'))} · {u.get('title')}")
    if errors:
        lines.append("")
        lines.append(f"_⚠️ {len(errors)} section(s) unavailable: " +
                     "; ".join(e.get("section") for e in errors[:4]) + "_")
    return "\n".join(lines)
