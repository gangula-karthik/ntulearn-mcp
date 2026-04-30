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
