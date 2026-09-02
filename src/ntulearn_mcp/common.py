"""Shared pure helpers for the NTULearn MCP tool handlers.

This module is the WT-B "capabilities" mirror of the utility functions that
live inside ``server.py``. Keeping them here lets the 13 new handlers reuse
the exact same argument coercion, pagination, markup, and HTML-stripping
semantics as the existing tools without importing ``server`` (which would
create an import cycle and blow up the module graph during tests).

Everything in this module is import-safe in isolation: ``server.py`` is never
imported, and the optional ``ntulearn_mcp.cache`` data API is only touched
lazily (and guarded) inside the tracker helpers.
"""

from __future__ import annotations

import json
import re
from datetime import datetime, timedelta, timezone
from typing import Any

from bs4 import BeautifulSoup
from mcp.types import TextContent

# Pagination defaults / caps. Must match server._DEFAULT_LIMIT / _MAX_LIMIT.
_DEFAULT_LIMIT = 50
_MAX_LIMIT = 200
_MAX_DEPTH = 10

# Extension / MIME classification tables mirroring server.py.
_TEXT_EXTENSIONS = frozenset({
    "txt", "md", "markdown", "csv", "tsv", "json", "xml", "yaml", "yml",
    "html", "htm", "log", "py", "js", "ts", "rs", "go", "java", "c", "cpp",
    "h", "hpp", "sh", "bash", "zsh", "rb", "swift", "kt", "scala", "r",
    "ini", "toml", "cfg", "conf", "env",
})

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

# ---------------------------------------------------------------------------
# Emission + argument coercion (mirrors server._emit / _resolve_* helpers)
# ---------------------------------------------------------------------------


def emit(payload: dict[str, Any], text: str | None = None) -> tuple[list[TextContent], dict[str, Any]]:
    """Return the (unstructured, structured) tuple MCP expects.

    ``payload`` is the JSON-serialisable structured content. ``text`` is the
    rendered display text and defaults to a pretty-printed JSON copy of the
    payload, mirroring ``server._emit``.
    """
    if text is None:
        text = json.dumps(payload, indent=2)
    return [TextContent(type="text", text=text)], payload


def resolve_pagination_args(args: dict[str, Any]) -> tuple[int, int]:
    """Validate and clamp limit/offset args. Defaults: offset=0, limit=50."""
    offset = int(args.get("offset", 0))
    limit = int(args.get("limit", _DEFAULT_LIMIT))
    if offset < 0:
        raise ValueError("offset must be >= 0")
    if limit < 1:
        raise ValueError("limit must be >= 1")
    if limit > _MAX_LIMIT:
        raise ValueError(f"limit must be <= {_MAX_LIMIT}")
    return offset, limit


def resolve_response_format(args: dict[str, Any]) -> str:
    """Return 'json' or 'markdown'. Default: 'json' (machine-readable)."""
    fmt = str(args.get("response_format", "json")).lower()
    if fmt not in ("json", "markdown"):
        raise ValueError("response_format must be 'json' or 'markdown'")
    return fmt


def slice_with_pagination(
    items: list[Any], offset: int, limit: int
) -> tuple[list[Any], dict[str, Any]]:
    """Slice items[offset:offset+limit] and return pagination metadata.

    Mirrors ``server._slice_with_pagination``: the underlying Blackboard client
    paginates internally and returns full result sets, so this is caller-side
    slicing that keeps the LLM's context bounded.
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


def validate_iso8601(value: str, *, name: str) -> str:
    """Accept an ISO-8601 datetime string and return it unchanged.

    Mirrors ``server._validate_iso8601``: round-trips through
    ``datetime.fromisoformat`` for a cheap sanity check, normalising a
    trailing ``Z`` because ``fromisoformat`` only handles it natively from
    3.11+.
    """
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"{name} must be a non-empty ISO-8601 timestamp string")
    normalised = value.replace("Z", "+00:00") if value.endswith("Z") else value
    try:
        datetime.fromisoformat(normalised)
    except ValueError as e:
        raise ValueError(
            f"{name}={value!r} is not a valid ISO-8601 timestamp. "
            "Expected format like '2026-05-09T00:00:00Z'."
        ) from e
    return value


def now_iso() -> str:
    """Return the current UTC time as an ISO-8601 string with a trailing Z."""
    return (
        datetime.now(timezone.utc)
        .replace(microsecond=0)
        .isoformat()
        .replace("+00:00", "Z")
    )


def _iso_minus_days(days: int) -> str:
    """Return an ISO timestamp ``days`` in the past (UTC, Z-suffixed)."""
    stamp = datetime.now(timezone.utc) - timedelta(days=days)
    return stamp.replace(microsecond=0).isoformat().replace("+00:00", "Z")


def default_since() -> str:
    """Default 'since' for tracker-style aggregations: exactly 7 days back."""
    return _iso_minus_days(7)


async def fan_out_course_ids(client: Any, course_ids_arg: Any) -> list[str]:
    """Resolve the course_ids arg into a concrete list of course IDs.

    Omitted / empty -> the user's enrolled, currently-available course IDs
    (mirrors ``server._resolve_enrolled_course_ids``, which skips courses whose
    availability is not 'Yes'). Otherwise validates that the value is a list
    of strings.
    """
    if course_ids_arg is None or (
        isinstance(course_ids_arg, list) and not course_ids_arg
    ):
        enrollments = await client.get_my_enrollments()
        enrollments = [
            e for e in enrollments
            if (e.get("availability") or {}).get("available") == "Yes"
        ]
        return [e["courseId"] for e in enrollments if e.get("courseId")]
    if not isinstance(course_ids_arg, list):
        raise ValueError("course_ids must be a list of strings")
    return [str(cid) for cid in course_ids_arg]


# ---------------------------------------------------------------------------
# Text + filename helpers (mirrors server._strip_html / download sanitising)
# ---------------------------------------------------------------------------


def strip_html(value: Any) -> str:
    """Strip HTML tags and collapse whitespace from a possibly-HTML string.

    Mirrors ``server._strip_html``: accepts non-strings (None and other falsy
    values return ``""``; truthy non-strings are coerced via ``str()``).
    """
    if not value:
        return ""
    if not isinstance(value, str):
        value = str(value)
    text = BeautifulSoup(value, "html.parser").get_text(separator="\n")
    return "\n".join(
        line for line in (segment.strip() for segment in text.splitlines()) if line
    )


def sanitize_filename(name: str) -> str:
    """Replace path-hostile characters with underscores.

    Mirrors the ``_sanitize`` helper used by ``server._download_file``.
    """
    return re.sub(r'[\\/*?:"<>|]', "_", name)


def deduplicate_filename(name: str, used_names: set[str], dest_dir: Any) -> str:
    """Return ``name`` with a " (n)" suffix if it collides with used/existing files.

    Mirrors the ``_deduplicate`` helper used by ``server._download_file``: a
    name is rejected when it is already in ``used_names`` OR already exists on
    disk under ``dest_dir``.
    """
    candidate = name
    stem, dot, ext = name.rpartition(".")
    base = stem if dot else name
    suffix = ext if dot else ""
    n = 2
    while candidate in used_names or (dest_dir / candidate).exists():
        candidate = f"{base} ({n}).{suffix}" if suffix else f"{base} ({n})"
        n += 1
    return candidate


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
            charset = p.split("=", 1)[1].strip().strip("'").strip("'")
    return mime, charset


def classify_kind(filename: str, content_type: str | None) -> str:
    """Return 'pdf', 'docx', 'pptx', 'xlsx', 'text', or 'binary'.

    Mirrors ``server._classify_kind``: the filename extension wins over the
    content type because Blackboard's bbcswebdav often serves everything as
    application/octet-stream.
    """
    ext = _file_extension(filename)
    if ext == "pdf":
        return "pdf"
    if ext == "docx":
        return "docx"
    if ext == "pptx":
        return "pptx"
    if ext == "xlsx":
        return "xlsx"
    if ext in _TEXT_EXTENSIONS:
        return "text"

    mime, _ = _parse_content_type(content_type)
    if mime == "application/pdf":
        return "pdf"
    if "wordprocessingml" in mime:
        return "docx"
    if "presentationml" in mime:
        return "pptx"
    if "spreadsheetml" in mime:
        return "xlsx"
    if mime.startswith("text/"):
        return "text"
    if mime in _TEXT_MIMETYPES:
        return "text"
    return "binary"


# ---------------------------------------------------------------------------
# Tracker helpers (whats_new "last seen" watermark)
# ---------------------------------------------------------------------------

_TRACKER_NAMESPACE = "tracker"
_TRACKER_KEY = "last_seen"
# In-module fallback when ntulearn_mcp.cache.data_cache() is unavailable.
_TRACKER_FALLBACK: dict[str, str] = {}
_TRACKER_TTL_SECONDS = 30 * 24 * 60 * 60  # 30 days


def _tracker_backend() -> Any | None:
    """Return the cache data API (if available) or None.

    Import and access are fully guarded: a missing/old ``cache`` module (no
    ``data_cache`` yet), or a broken backend, just yields None and callers fall
    back to the in-module dict.
    """
    try:
        from ntulearn_mcp import cache  # guarded optional import

        return cache.data_cache()
    except Exception:
        return None


def tracker_get_last_seen() -> str | None:
    """Return the stored 'last seen' ISO timestamp, or None if unset."""
    backend = _tracker_backend()
    if backend is not None:
        try:
            value = backend.get(_TRACKER_NAMESPACE, _TRACKER_KEY)
            if isinstance(value, str) and value:
                return value
        except Exception:
            pass
    return _TRACKER_FALLBACK.get(_TRACKER_KEY)


def tracker_set_last_seen(iso: str) -> None:
    """Persist the 'last seen' ISO timestamp (cache first, in-module fallback)."""
    _TRACKER_FALLBACK[_TRACKER_KEY] = iso
    backend = _tracker_backend()
    if backend is not None:
        try:
            backend.set(
                _TRACKER_NAMESPACE, _TRACKER_KEY, iso, ttl=_TRACKER_TTL_SECONDS
            )
        except Exception:
            pass


# ---------------------------------------------------------------------------
# Extra helpers (ID validation, ISO arithmetic, folder names)
# ---------------------------------------------------------------------------

BB_ID_PATTERN = r"^[A-Za-z0-9_\-:]+$"


def validate_bb_id(value: Any, *, name: str) -> str:
    """Validate a Blackboard-style ID (alnum, underscore, dash, colon)."""
    if not isinstance(value, str) or not re.match(BB_ID_PATTERN, value):
        raise ValueError(
            f"{name} must be a valid Blackboard ID (letters, digits, _ - :), "
            f"got {value!r}"
        )
    return value


def parse_iso(value: str) -> datetime:
    """Parse an ISO-8601 string (Z or +HH:MM) into an aware datetime."""
    normalised = value.replace("Z", "+00:00") if value.endswith("Z") else value
    if "+" not in normalised[10:]:
        normalised += "+00:00"
    return datetime.fromisoformat(normalised)


def iso_from_now(days: int, *, minute_of_day: int = 0) -> str:
    """Return an ISO timestamp ``days`` from now (UTC, Z-suffixed)."""
    stamp = datetime.now(timezone.utc) + timedelta(days=days)
    if 0 <= minute_of_day <= 1439:
        stamp = stamp.replace(hour=minute_of_day // 60, minute=minute_of_day % 60, second=0, microsecond=0)
    return stamp.replace(microsecond=0).isoformat().replace("+00:00", "Z")


def safe_folder_name(name: str) -> str:
    """Slugify a course/term name for use as a local folder name."""
    if not isinstance(name, str) or not name.strip():
        return "untitled"
    slug = re.sub(r"[^\w.\- ]+", "_", name).strip()
    slug = re.sub(r"\s+", " ", slug).strip(" ._")
    return slug or "untitled"
