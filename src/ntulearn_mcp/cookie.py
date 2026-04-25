"""Read the BbRouter cookie from a local browser session.

Lets the MCP server work without requiring the user to copy the BbRouter
cookie into their config: if they're already logged into NTULearn in a
supported browser, we pick up the cookie automatically and re-read it on
demand when the server gets a 401.
"""

from __future__ import annotations

import logging
from typing import Any, Callable

logger = logging.getLogger(__name__)

NTULEARN_DOMAIN = "ntulearn.ntu.edu.sg"
COOKIE_NAME = "BbRouter"

# Order: most-likely-to-have-a-fresh-cookie first. Edge is the default browser
# on Windows; Chrome is the most common daily; Firefox is most likely to work
# on Windows even when Chrome's App-Bound Encryption blocks reads; Brave is a
# Chromium variant kept for completeness.
DEFAULT_BROWSERS: tuple[str, ...] = ("edge", "chrome", "firefox", "brave")


def read_bbrouter_cookie(
    *,
    browsers: tuple[str, ...] = DEFAULT_BROWSERS,
    module: Any | None = None,
) -> str | None:
    """Return the first valid BbRouter cookie value found across local browsers.

    Returns None if no browser has a logged-in NTULearn session, or if every
    browser raises while reading (e.g. Chrome's App-Bound Encryption on
    Windows). The caller should fall back to the NTULEARN_COOKIE env var.
    """
    if module is None:
        try:
            import browser_cookie3  # type: ignore[import-untyped]
        except ImportError:
            logger.warning(
                "browser-cookie3 is not installed; skipping browser cookie auto-read"
            )
            return None
        module = browser_cookie3

    for name in browsers:
        getter: Callable[..., Any] | None = getattr(module, name, None)
        if getter is None:
            continue
        try:
            cookie_jar = getter(domain_name=NTULEARN_DOMAIN)
        except Exception as e:
            logger.debug("Couldn't read cookies from %s: %s", name, e)
            continue

        for c in cookie_jar:
            if c.name == COOKIE_NAME and _is_valid_bbrouter(c.value):
                logger.info("Read BbRouter cookie from %s", name)
                return c.value

    return None


def _is_valid_bbrouter(value: str | None) -> bool:
    """Reject obviously-bad BbRouter values.

    Real cookies look like ``expires:1234567890,id:...``. Chrome's App-Bound
    Encryption on Windows can produce decrypt-to-garbage values that show up
    under the BbRouter name; the prefix check skips those so we keep walking
    to the next browser.
    """
    return bool(value) and value.startswith("expires:")
