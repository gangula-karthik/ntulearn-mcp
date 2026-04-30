from __future__ import annotations

import sys
import unittest
from pathlib import Path
from types import SimpleNamespace
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "src"))

from ntulearn_mcp.cookie import read_bbrouter_cookie


class FakeCookie:
    def __init__(self, name: str, value: str, domain: str = "ntulearn.ntu.edu.sg") -> None:
        self.name = name
        self.value = value
        self.domain = domain


def _fake_module(per_browser: dict[str, Any]) -> SimpleNamespace:
    """Build a fake browser_cookie3-like module.

    `per_browser` maps browser name → either a list of FakeCookie (the cookie
    jar that browser would yield), an Exception instance to raise, or a list
    of responses to return on successive calls (use a list-of-lists / list-with-
    mixed-types for retry tests).
    """
    ns = SimpleNamespace()
    for browser, response in per_browser.items():
        def make_getter(resp: Any):
            # If response is a list whose first element is itself a list /
            # Exception, treat it as a sequence of per-call responses (used
            # for retry tests). Otherwise treat the whole thing as the single
            # response returned on every call.
            is_sequence_of_responses = (
                isinstance(resp, list)
                and resp
                and all(isinstance(r, (list, Exception)) for r in resp)
            )
            calls = iter(resp) if is_sequence_of_responses else None

            def getter(domain_name: str | None = None):
                this_response = next(calls) if calls is not None else resp
                if isinstance(this_response, Exception):
                    raise this_response
                return this_response
            return getter
        setattr(ns, browser, make_getter(response))
    return ns


# Tests that expect a None result would otherwise sleep through the full retry
# backoff (0.5s + 1.0s = 1.5s) before returning. Inject a no-op sleeper.
_no_sleep = lambda _seconds: None


class CookieTests(unittest.TestCase):
    def test_returns_first_browser_with_valid_cookie(self) -> None:
        mod = _fake_module({
            "edge": [FakeCookie("BbRouter", "expires:111,id:abc")],
            "chrome": [FakeCookie("BbRouter", "expires:222,id:def")],
        })
        result = read_bbrouter_cookie(browsers=("edge", "chrome"), module=mod)
        self.assertEqual(result, "expires:111,id:abc")

    def test_skips_browser_that_raises(self) -> None:
        mod = _fake_module({
            "edge": RuntimeError("App-Bound Encryption blocked the read"),
            "chrome": [FakeCookie("BbRouter", "expires:222,id:def")],
        })
        result = read_bbrouter_cookie(browsers=("edge", "chrome"), module=mod)
        self.assertEqual(result, "expires:222,id:def")

    def test_returns_none_when_no_browser_yields_cookie(self) -> None:
        mod = _fake_module({
            "edge": [],
            "chrome": [],
            "firefox": [],
            "brave": [],
        })
        result = read_bbrouter_cookie(module=mod, sleep=_no_sleep)
        self.assertIsNone(result)

    def test_returns_none_when_every_browser_raises(self) -> None:
        mod = _fake_module({
            "edge": RuntimeError("ABE"),
            "chrome": RuntimeError("ABE"),
            "firefox": OSError("DB locked"),
            "brave": RuntimeError("ABE"),
        })
        result = read_bbrouter_cookie(module=mod, sleep=_no_sleep)
        self.assertIsNone(result)

    def test_ignores_unrelated_cookies(self) -> None:
        mod = _fake_module({
            "edge": [
                FakeCookie("session_id", "expires:123,id:abc"),
                FakeCookie("XSRF-TOKEN", "expires:456,id:def"),
                FakeCookie("BbRouter", "expires:111,id:abc"),
            ],
        })
        result = read_bbrouter_cookie(browsers=("edge",), module=mod)
        self.assertEqual(result, "expires:111,id:abc")

    def test_rejects_garbage_bbrouter_value_and_falls_through(self) -> None:
        # Simulates Chrome's App-Bound Encryption decrypting to junk: the row
        # has the right cookie name but the value isn't a real BbRouter token.
        mod = _fake_module({
            "edge": [FakeCookie("BbRouter", "\x00\x01\x02garbage")],
            "chrome": [FakeCookie("BbRouter", "expires:222,id:def")],
        })
        result = read_bbrouter_cookie(browsers=("edge", "chrome"), module=mod)
        self.assertEqual(result, "expires:222,id:def")

    def test_skips_browsers_missing_from_module(self) -> None:
        # Older browser_cookie3 may not have all browser getters; we should
        # quietly skip rather than crash.
        mod = SimpleNamespace()
        mod.edge = lambda **kwargs: [FakeCookie("BbRouter", "expires:111,id:abc")]
        result = read_bbrouter_cookie(
            browsers=("nonexistent", "edge", "alsofake"),
            module=mod,
        )
        self.assertEqual(result, "expires:111,id:abc")

    def test_browser_order_is_respected(self) -> None:
        # If two browsers both have a valid cookie, the first one in the
        # browsers tuple wins.
        mod = _fake_module({
            "edge": [FakeCookie("BbRouter", "expires:111,id:edge")],
            "chrome": [FakeCookie("BbRouter", "expires:222,id:chrome")],
        })
        chrome_first = read_bbrouter_cookie(browsers=("chrome", "edge"), module=mod)
        self.assertEqual(chrome_first, "expires:222,id:chrome")
        edge_first = read_bbrouter_cookie(browsers=("edge", "chrome"), module=mod)
        self.assertEqual(edge_first, "expires:111,id:edge")


class CookieRetryTests(unittest.TestCase):
    """Resilience to transient browser-cookie3 failures.

    Empirical motivation in CLAUDE.md: Mac + Claude Desktop mcpb child reads
    Chrome cookies most of the time but occasionally returns an empty jar
    (SQLite write-lock race / keychain timeout / TCC). A bounded retry walks
    the full browser list again with backoff before giving up.
    """

    def test_succeeds_on_retry_after_initial_empty_read(self) -> None:
        # First call: every browser yields an empty cookie jar (the race).
        # Second call: chrome yields a valid cookie (race resolved).
        mod = _fake_module({
            "edge": [[], []],
            "chrome": [[], [FakeCookie("BbRouter", "expires:222,id:def")]],
        })
        sleeps: list[float] = []
        result = read_bbrouter_cookie(
            browsers=("edge", "chrome"),
            module=mod,
            sleep=sleeps.append,
        )
        self.assertEqual(result, "expires:222,id:def")
        self.assertEqual(sleeps, [0.5])  # exactly one backoff before retry

    def test_gives_up_after_configured_retry_count(self) -> None:
        # Every attempt fails; we should walk the browser list exactly
        # retries+1 times and call sleep retries times.
        call_count = {"edge": 0}

        def edge_getter(domain_name: str | None = None) -> list[Any]:
            call_count["edge"] += 1
            return []

        mod = SimpleNamespace(edge=edge_getter)
        sleeps: list[float] = []
        result = read_bbrouter_cookie(
            browsers=("edge",),
            module=mod,
            retries=3,
            sleep=sleeps.append,
        )
        self.assertIsNone(result)
        self.assertEqual(call_count["edge"], 4)  # 1 initial + 3 retries
        self.assertEqual(sleeps, [0.5, 1.0, 2.0])  # exponential backoff

    def test_retries_disabled_with_retries_zero(self) -> None:
        # retries=0 means a single attempt — no sleeps at all. Useful when
        # the caller has its own retry policy or wants a fast probe.
        call_count = {"edge": 0}

        def edge_getter(domain_name: str | None = None) -> list[Any]:
            call_count["edge"] += 1
            return []

        mod = SimpleNamespace(edge=edge_getter)
        sleeps: list[float] = []
        result = read_bbrouter_cookie(
            browsers=("edge",),
            module=mod,
            retries=0,
            sleep=sleeps.append,
        )
        self.assertIsNone(result)
        self.assertEqual(call_count["edge"], 1)
        self.assertEqual(sleeps, [])


if __name__ == "__main__":
    unittest.main()
