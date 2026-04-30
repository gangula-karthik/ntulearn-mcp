from __future__ import annotations

import sys
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "src"))

from ntulearn_mcp import cache
from ntulearn_mcp.cache import (
    delete_cached_cookie,
    read_cached_cookie,
    write_cached_cookie,
)


class FakeKeyring:
    """In-memory stand-in for the `keyring` module.

    Mirrors the three functions we use (``get_password``, ``set_password``,
    ``delete_password``) and lets each one be configured to raise — that
    covers the "broken backend" branches in cache.py.
    """

    def __init__(
        self,
        *,
        store: dict[tuple[str, str], str] | None = None,
        get_error: Exception | None = None,
        set_error: Exception | None = None,
        delete_error: Exception | None = None,
    ) -> None:
        self.store: dict[tuple[str, str], str] = store or {}
        self.get_error = get_error
        self.set_error = set_error
        self.delete_error = delete_error
        self.calls: list[tuple[str, str, str]] = []

    def get_password(self, service: str, username: str) -> str | None:
        self.calls.append(("get", service, username))
        if self.get_error is not None:
            raise self.get_error
        return self.store.get((service, username))

    def set_password(self, service: str, username: str, value: str) -> None:
        self.calls.append(("set", service, username))
        if self.set_error is not None:
            raise self.set_error
        self.store[(service, username)] = value

    def delete_password(self, service: str, username: str) -> None:
        self.calls.append(("delete", service, username))
        if self.delete_error is not None:
            raise self.delete_error
        self.store.pop((service, username), None)


VALID_COOKIE = "expires:1700000000,id:abc123"
ANOTHER_VALID_COOKIE = "expires:1800000000,id:def456"


class CacheReadTests(unittest.TestCase):
    def test_returns_value_when_present_and_valid(self) -> None:
        kr = FakeKeyring(store={("ntulearn-mcp", "BbRouter"): VALID_COOKIE})
        self.assertEqual(read_cached_cookie(module=kr), VALID_COOKIE)

    def test_returns_none_when_no_entry(self) -> None:
        kr = FakeKeyring()
        self.assertIsNone(read_cached_cookie(module=kr))

    def test_returns_none_when_value_is_invalid(self) -> None:
        # A garbage value (no `expires:` prefix) shouldn't be returned even
        # if it's somehow ended up in the keychain — same validity check
        # we apply at the browser-read layer.
        kr = FakeKeyring(store={("ntulearn-mcp", "BbRouter"): "junk"})
        self.assertIsNone(read_cached_cookie(module=kr))

    def test_returns_none_when_keyring_raises(self) -> None:
        # Headless Linux without DBus, locked Windows credential store,
        # macOS keychain access denied — never propagate.
        kr = FakeKeyring(get_error=RuntimeError("no backend available"))
        self.assertIsNone(read_cached_cookie(module=kr))


class CacheWriteTests(unittest.TestCase):
    def test_writes_valid_cookie(self) -> None:
        kr = FakeKeyring()
        ok = write_cached_cookie(VALID_COOKIE, module=kr)
        self.assertTrue(ok)
        self.assertEqual(kr.store[("ntulearn-mcp", "BbRouter")], VALID_COOKIE)

    def test_overwrites_existing_value(self) -> None:
        # Cookie rotation: a fresh read should supersede the stored value
        # transparently rather than accumulating entries.
        kr = FakeKeyring(store={("ntulearn-mcp", "BbRouter"): VALID_COOKIE})
        ok = write_cached_cookie(ANOTHER_VALID_COOKIE, module=kr)
        self.assertTrue(ok)
        self.assertEqual(
            kr.store[("ntulearn-mcp", "BbRouter")], ANOTHER_VALID_COOKIE
        )

    def test_rejects_invalid_value_without_calling_keyring(self) -> None:
        # We never want a cookie that doesn't look like a real BbRouter
        # value (e.g., ABE-decrypt-to-garbage) poisoning the cache for the
        # cookie's full lifetime.
        kr = FakeKeyring()
        ok = write_cached_cookie("not-a-real-cookie", module=kr)
        self.assertFalse(ok)
        self.assertEqual(kr.calls, [])

    def test_returns_false_when_keyring_raises(self) -> None:
        kr = FakeKeyring(set_error=RuntimeError("locked"))
        ok = write_cached_cookie(VALID_COOKIE, module=kr)
        self.assertFalse(ok)


class CacheDeleteTests(unittest.TestCase):
    def test_deletes_existing_entry(self) -> None:
        kr = FakeKeyring(store={("ntulearn-mcp", "BbRouter"): VALID_COOKIE})
        delete_cached_cookie(module=kr)
        self.assertNotIn(("ntulearn-mcp", "BbRouter"), kr.store)

    def test_no_op_when_entry_absent(self) -> None:
        # Real keyring backends raise PasswordDeleteError when there's
        # nothing to delete; we swallow it because the post-condition we
        # want is "no entry," which already holds.
        kr = FakeKeyring(delete_error=RuntimeError("not found"))
        delete_cached_cookie(module=kr)  # must not raise
        self.assertEqual(kr.calls, [("delete", "ntulearn-mcp", "BbRouter")])

    def test_swallows_keyring_errors(self) -> None:
        # Generic backend failure on delete shouldn't crash the server
        # mid-401-refresh.
        kr = FakeKeyring(delete_error=RuntimeError("backend exploded"))
        delete_cached_cookie(module=kr)


class CacheModuleResolutionTests(unittest.TestCase):
    """When `keyring` isn't installed, every cache function should be a no-op.

    `_get_module` returns None when the import fails. Patching it here is
    the cleanest way to exercise that branch without manipulating
    ``sys.modules`` (which would leak into other tests).
    """

    def test_read_returns_none_without_keyring(self) -> None:
        with mock.patch.object(cache, "_get_module", return_value=None):
            self.assertIsNone(read_cached_cookie())

    def test_write_returns_false_without_keyring(self) -> None:
        with mock.patch.object(cache, "_get_module", return_value=None):
            self.assertFalse(write_cached_cookie(VALID_COOKIE))

    def test_delete_does_not_raise_without_keyring(self) -> None:
        with mock.patch.object(cache, "_get_module", return_value=None):
            delete_cached_cookie()  # must not raise

    def test_get_module_returns_injected_module_unchanged(self) -> None:
        # The dependency-injection path tests rely on this contract:
        # if you pass a module, you get it back without going near `import`.
        kr = FakeKeyring()
        self.assertIs(cache._get_module(kr), kr)


if __name__ == "__main__":
    unittest.main()
