"""Read the BbRouter cookie from a local browser session.

Lets the MCP server work without requiring the user to copy the BbRouter
cookie into their config: if they're already logged into NTULearn in a
supported browser, we pick up the cookie automatically and re-read it on
demand when the server gets a 401.
"""

from __future__ import annotations

import logging
import time
from typing import Any, Callable

logger = logging.getLogger(__name__)

NTULEARN_DOMAIN = "ntulearn.ntu.edu.sg"
COOKIE_NAME = "BbRouter"

# Order: most-likely-to-have-a-fresh-cookie first. Edge is the default browser
# on Windows; Chrome is the most common daily; Firefox is most likely to work
# on Windows even when Chrome's App-Bound Encryption blocks reads; Brave is a
# Chromium variant kept for completeness.
DEFAULT_BROWSERS: tuple[str, ...] = ("edge", "chrome", "firefox", "brave")

# Empirically (see CLAUDE.md) Mac + Claude Desktop's mcpb child can read Chrome
# cookies most of the time but occasionally returns nothing — SQLite read
# racing with browser writes, keychain access timeouts, TCC re-evaluation. A
# small bounded retry catches the transient case without slowing the deterministic
# failure modes (no NTULearn login at all, ABE on Windows) by more than a second.
_DEFAULT_RETRIES = 2
_DEFAULT_RETRY_DELAY_SECONDS = 0.5


def read_bbrouter_cookie(
    *,
    browsers: tuple[str, ...] = DEFAULT_BROWSERS,
    module: Any | None = None,
    retries: int = _DEFAULT_RETRIES,
    retry_delay: float = _DEFAULT_RETRY_DELAY_SECONDS,
    sleep: Callable[[float], None] = time.sleep,
) -> str | None:
    """Return the first valid BbRouter cookie value found across local browsers.

    Walks ``browsers`` in order, first browser with a valid cookie wins.
    Returns None if no browser has a logged-in NTULearn session, or if every
    browser raises while reading (e.g. Chrome's App-Bound Encryption on
    Windows). On transient empty reads we retry the full walk up to
    ``retries`` times with exponential backoff.

    The ``sleep`` parameter is injectable so tests can run with retries=2
    without actually waiting.
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

    last_browser_errors: dict[str, str] = {}
    delay = retry_delay
    total_attempts = retries + 1

    for attempt in range(total_attempts):
        last_browser_errors.clear()
        for name in browsers:
            getter: Callable[..., Any] | None = getattr(module, name, None)
            if getter is None:
                continue
            try:
                cookie_jar = getter(domain_name=NTULEARN_DOMAIN)
            except Exception as e:
                last_browser_errors[name] = f"{type(e).__name__}: {e}"
                logger.debug("Couldn't read cookies from %s: %s", name, e)
                continue

            for c in cookie_jar:
                if c.name == COOKIE_NAME and _is_valid_bbrouter(c.value):
                    if attempt > 0:
                        logger.info(
                            "Read BbRouter cookie from %s on attempt %d/%d",
                            name, attempt + 1, total_attempts,
                        )
                    else:
                        logger.info("Read BbRouter cookie from %s", name)
                    return c.value

        if attempt < retries:
            logger.info(
                "No valid BbRouter cookie on attempt %d/%d; "
                "retrying in %.1fs (browser errors: %s)",
                attempt + 1, total_attempts, delay,
                last_browser_errors or "none — no browser yielded a BbRouter cookie",
            )
            sleep(delay)
            delay *= 2  # gentle exponential backoff: 0.5s → 1.0s → 2.0s

    if last_browser_errors:
        logger.warning(
            "Could not auto-read BbRouter cookie after %d attempt(s). "
            "Browser errors on final attempt: %s",
            total_attempts, last_browser_errors,
        )
    else:
        logger.info(
            "No BbRouter cookie present in any of %s after %d attempt(s); "
            "is the user logged into NTULearn in a supported browser?",
            list(browsers), total_attempts,
        )
    return None


def _is_valid_bbrouter(value: str | None) -> bool:
    """Reject obviously-bad BbRouter values.

    Real cookies look like ``expires:1234567890,id:...``. Chrome's App-Bound
    Encryption on Windows can produce decrypt-to-garbage values that show up
    under the BbRouter name; the prefix check skips those so we keep walking
    to the next browser.
    """
    return bool(value) and value.startswith("expires:")
