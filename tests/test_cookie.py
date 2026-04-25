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
    jar that browser would yield) or an Exception instance to raise.
    """
    ns = SimpleNamespace()
    for browser, response in per_browser.items():
        def make_getter(resp: Any):
            def getter(domain_name: str | None = None):
                if isinstance(resp, Exception):
                    raise resp
                return resp
            return getter
        setattr(ns, browser, make_getter(response))
    return ns


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
        result = read_bbrouter_cookie(module=mod)
        self.assertIsNone(result)

    def test_returns_none_when_every_browser_raises(self) -> None:
        mod = _fake_module({
            "edge": RuntimeError("ABE"),
            "chrome": RuntimeError("ABE"),
            "firefox": OSError("DB locked"),
            "brave": RuntimeError("ABE"),
        })
        result = read_bbrouter_cookie(module=mod)
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


if __name__ == "__main__":
    unittest.main()
