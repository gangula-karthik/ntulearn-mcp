"""Persist the last-known-good BbRouter cookie in the OS keychain.

`browser-cookie3` reads can fail transiently (SQLite write-lock race,
keychain access timeout, TCC re-evaluation) or permanently on Windows
+ Chrome/Edge under App-Bound Encryption. Caching the most recent
successful read in the OS keychain gives us a fallback that rides
through transient failures and stretches a single successful browser
read across the cookie's full lifetime (typically days–weeks).

Storage backend (chosen by ``keyring``):
- macOS: Keychain Services
- Linux: Secret Service / KWallet (or in-memory fallback if neither is up)
- Windows: Credential Manager

Every operation degrades to a no-op on failure: a missing or broken
keyring backend, an absent entry, or any other exception is logged at
DEBUG level but never propagated. The caller falls through to the
existing "no cookie" error path rather than seeing a cache-related
exception bubble up.
"""

from __future__ import annotations

import logging
from typing import Any

logger = logging.getLogger(__name__)

# Service name shows up in macOS Keychain Access etc., so make it identifiable.
_SERVICE = "ntulearn-mcp"
# `keyring` requires (service, username); the cookie isn't tied to a username
# at our layer so we use a fixed sentinel.
_USERNAME = "BbRouter"


def _get_module(module: Any | None = None) -> Any | None:
    """Return the keyring module (or a test override). None if unavailable."""
    if module is not None:
        return module
    try:
        import keyring  # type: ignore[import-untyped]
    except ImportError:
        logger.debug("keyring is not installed; cookie cache disabled")
        return None
    return keyring


def read_cached_cookie(*, module: Any | None = None) -> str | None:
    """Return the cached BbRouter cookie value, or None.

    None covers all failure modes: keyring not installed, no entry exists,
    backend unavailable, value rejected by the validity check. Callers
    should treat None as "no cache" and continue down their existing
    fallback path.
    """
    keyring = _get_module(module)
    if keyring is None:
        return None
    try:
        value = keyring.get_password(_SERVICE, _USERNAME)
    except Exception as e:
        # Headless Linux without DBus, locked Windows credential store,
        # macOS keychain access denied — never let it crash the server.
        logger.debug("Cookie cache read failed: %s: %s", type(e).__name__, e)
        return None
    if value and _is_valid(value):
        return value
    return None


def write_cached_cookie(value: str, *, module: Any | None = None) -> bool:
    """Persist the BbRouter cookie value to the OS keychain.

    Returns True on success, False otherwise (no keyring, write failure,
    invalid value). Failures are logged at DEBUG and not propagated:
    caching is a best-effort optimisation; cookie auth still works without
    it.
    """
    if not _is_valid(value):
        return False
    keyring = _get_module(module)
    if keyring is None:
        return False
    try:
        keyring.set_password(_SERVICE, _USERNAME, value)
    except Exception as e:
        logger.debug("Cookie cache write failed: %s: %s", type(e).__name__, e)
        return False
    logger.debug("Cached BbRouter cookie")
    return True


def delete_cached_cookie(*, module: Any | None = None) -> None:
    """Invalidate the cached cookie. No-op if there's nothing to delete.

    Called on 401 to nuke the value that just failed so the next
    resolution doesn't loop on the same dead cookie.
    """
    keyring = _get_module(module)
    if keyring is None:
        return
    try:
        keyring.delete_password(_SERVICE, _USERNAME)
        logger.debug("Invalidated cached BbRouter cookie")
    except Exception as e:
        # `keyring.errors.PasswordDeleteError` when the entry is already
        # absent — fine, it's the state we wanted anyway.
        logger.debug(
            "Cookie cache delete (no-op or failed): %s: %s",
            type(e).__name__, e,
        )


def _is_valid(value: str | None) -> bool:
    """Reject obviously-bad cookie values before they reach storage / use.

    Real BbRouter cookies start with ``expires:``. Catching corrupt
    values here means a single bad write can't poison the cache for the
    cookie's entire lifetime.
    """
    return bool(value) and value.startswith("expires:")


# ===========================================================================
# Data cache: transparent method-level TTL cache for the client, plus the
# shared "tracker" namespace used by the capabilities suite.
#
# Separate from the BbRouter cookie cache above. Two backends:
#   * in-memory LRU (OrderedDict, max ~4096 entries) — always on, lost on exit
#   * optional SQLite persistence under NTULEARN_CACHE_DIR — survives restarts
# SQLite writes are best-effort: any failure degrades to memory-only caching
# and never raises into the caller.
# ===========================================================================

from collections import OrderedDict
import json as _json
import os
import sqlite3
import threading
import time
from pathlib import Path
from typing import Any

DEFAULT_LRU_SIZE = 4096

# Per-method default TTLs (seconds). Mirrors the client contract table in
# DEVELOPMENT-SPEC.md. The "tracker" namespace backs common.whats_new's
# last-seen watermark with a 30-day lifetime.
DEFAULT_TTL_SECONDS: dict[str, float] = {
    "get_my_enrollments": 1800,
    "get_course": 3600,
    "get_courses_batch": 3600,
    "get_course_contents": 3600,
    "get_content_children": 3600,
    "get_content_item": 3600,
    "get_announcements": 600,
    "get_calendar_items": 300,
    "get_gradebook_columns": 600,
    "get_user_grades": 300,
    "get_messages": 60,
    "get_message": 600,
    "get_message_participants": 600,
    "get_course_users": 1800,
    "get_course_groups": 1800,
    "get_group_users": 1800,
    "get_gradebook_attempts": 300,
    "get_user_attempts": 300,
    "get_term": 3600,
    "get_course_search": 600,
    "tracker": 30 * 24 * 60 * 60,
}


def _default_cache_dir() -> Path:
    override = os.environ.get("NTULEARN_CACHE_DIR")
    if override:
        return Path(override)
    return Path.home() / ".cache" / "ntulearn-mcp" / "cache.sqlite3"


def _cache_mode() -> str:
    return os.environ.get("NTULEARN_CACHE_MODE", "readwrite").lower()


def _json_value(value: Any) -> str | None:
    """Serialise a value for SQLite; None if it isn't JSON-safe."""
    try:
        return _json.dumps(value, separators=(",", ":"), ensure_ascii=False)
    except (TypeError, ValueError):
        return None


def _json_load(value: str) -> Any | None:
    try:
        return _json.loads(value)
    except (TypeError, ValueError):
        return None


class DataCache:
    """TTL key-value store with an in-memory LRU + optional SQLite backend.

    Key format is the caller's responsibility but is conventionally
    ``f"{user_scope}:{sha256(namespace|path|params)}"`` so one user's entries
    never collide with another's and ``invalidate_user`` can target a scope.
    """

    def __init__(
        self,
        *,
        cache_dir: Path | None = None,
        max_size: int = DEFAULT_LRU_SIZE,
    ) -> None:
        # (namespace, key) -> (created_at, expires_at, value, user_scope)
        self._mem: OrderedDict[tuple[str, str], tuple[float, float, Any, str]] = OrderedDict()
        self._max_size = max_size
        self._lock = threading.Lock()
        self._mode = _cache_mode()
        self._cache_dir = Path(cache_dir) if cache_dir is not None else _default_cache_dir()
        self._schema_ok = False
        self._sqlite_broken = False

    # -- backend plumbing ---------------------------------------------------

    def _sqlite_conn(self) -> sqlite3.Connection | None:
        """Open (and if needed create) the SQLite database. Best-effort."""
        if self._mode == "off":
            return None
        if self._sqlite_broken:
            return None
        try:
            if not self._schema_ok:
                self._cache_dir.parent.mkdir(parents=True, exist_ok=True)
                conn = sqlite3.connect(str(self._cache_dir), timeout=2.0)
                conn.execute(
                    "CREATE TABLE IF NOT EXISTS data_cache ("
                    " namespace TEXT NOT NULL,"
                    " key TEXT NOT NULL,"
                    " value TEXT NOT NULL,"
                    " created_at REAL NOT NULL,"
                    " expires_at REAL NOT NULL,"
                    " user_scope TEXT NOT NULL DEFAULT '',"
                    " PRIMARY KEY (namespace, key))"
                )
                conn.commit()
                conn.close()
                self._schema_ok = True
            return sqlite3.connect(str(self._cache_dir), timeout=2.0)
        except Exception:
            self._sqlite_broken = True
            return None

    # -- API ----------------------------------------------------------------

    def get(self, namespace: str, key: str, *, max_age: float | None = None) -> Any | None:
        if self._mode == "off":
            return None
        now = time.time()
        with self._lock:
            entry = self._mem.get((namespace, key))
            if entry is not None:
                created, expires, value, _scope = entry
                if now < expires and (max_age is None or now - created <= max_age):
                    self._mem.move_to_end((namespace, key))
                    return value
                self._mem.pop((namespace, key), None)
            conn = self._sqlite_conn()
            if conn is not None:
                try:
                    row = conn.execute(
                        "SELECT value, created_at, expires_at, user_scope FROM data_cache "
                        "WHERE namespace=? AND key=?",
                        (namespace, key),
                    ).fetchone()
                except Exception:
                    row = None
                finally:
                    conn.close()
                if row is not None:
                    value_s, created, expires, scope = row
                    value = _json_load(value_s)
                    if (
                        now < expires
                        and (max_age is None or now - created <= max_age)
                        and value is not None
                    ):
                        self._mem[(namespace, key)] = (created, expires, value, scope)
                        self._evict_mem()
                        return value
        return None

    def set(
        self,
        namespace: str,
        key: str,
        value: Any,
        ttl: float,
        *,
        user_scope: str | None = None,
    ) -> None:
        if self._mode != "readwrite":
            return  # readonly and off: no writes
        if ttl is None or ttl <= 0:
            return
        now = time.time()
        expires = now + ttl
        scope = user_scope or ""
        with self._lock:
            self._mem[(namespace, key)] = (now, expires, value, scope)
            self._evict_mem()
            conn = self._sqlite_conn()
            if conn is not None:
                encoded = _json_value(value)
                if encoded is not None:
                    try:
                        conn.execute(
                            "INSERT OR REPLACE INTO data_cache "
                            "(namespace, key, value, created_at, expires_at, user_scope) "
                            "VALUES (?,?,?,?,?,?)",
                            (namespace, key, encoded, now, expires, scope),
                        )
                        conn.commit()
                    except Exception:
                        self._sqlite_broken = True
                    finally:
                        conn.close()

    def delete(self, namespace: str, key: str) -> None:
        with self._lock:
            self._mem.pop((namespace, key), None)
        conn = self._sqlite_conn()
        if conn is not None:
            try:
                conn.execute(
                    "DELETE FROM data_cache WHERE namespace=? AND key=?",
                    (namespace, key),
                )
                conn.commit()
            except Exception:
                pass
            finally:
                conn.close()

    def invalidate_user(self, user_scope: str) -> None:
        """Drop every entry belonging to a user scope (used on 401 refresh)."""
        if not user_scope:
            return
        with self._lock:
            for k in [
                k
                for k, (_t, _e, _v, scope) in self._mem.items()
                if scope == user_scope or k[1].startswith(user_scope + ":")
            ]:
                self._mem.pop(k, None)
        conn = self._sqlite_conn()
        if conn is not None:
            try:
                conn.execute(
                    "DELETE FROM data_cache WHERE user_scope=? OR key LIKE ?",
                    (user_scope, user_scope + ":%"),
                )
                conn.commit()
            except Exception:
                pass
            finally:
                conn.close()

    def clear(self) -> None:
        with self._lock:
            self._mem.clear()
        conn = self._sqlite_conn()
        if conn is not None:
            try:
                conn.execute("DELETE FROM data_cache")
                conn.commit()
            except Exception:
                pass
            finally:
                conn.close()

    # -- internals ----------------------------------------------------------

    def _evict_mem(self) -> None:
        while len(self._mem) > self._max_size:
            self._mem.popitem(last=False)


_DATA_CACHE_SINGLETON: DataCache | None = None
_DATA_CACHE_LOCK = threading.Lock()


def data_cache() -> DataCache:
    """Return the process-wide DataCache (lazily initialised, thread-safe)."""
    global _DATA_CACHE_SINGLETON
    if _DATA_CACHE_SINGLETON is None:
        with _DATA_CACHE_LOCK:
            if _DATA_CACHE_SINGLETON is None:
                _DATA_CACHE_SINGLETON = DataCache()
    return _DATA_CACHE_SINGLETON
