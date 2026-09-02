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


class DataCacheTests(unittest.TestCase):
    """Unit tests for the method-level DataCache (LRU + SQLite)."""

    def _fresh(self, name: str) -> "cache.DataCache":
        import tempfile

        tmp = tempfile.mkdtemp() + f"/{name}.sqlite3"
        dc = cache.DataCache(cache_dir=tmp)
        dc.clear()
        return dc

    def test_roundtrip_and_ttl_expiry(self) -> None:
        dc = self._fresh("rt")
        dc.set("n", "k", [1, 2, 3], 60, user_scope="u1")
        self.assertEqual(dc.get("n", "k"), [1, 2, 3])
        # expired entry is gone
        dc.set("n", "k2", "x", -1)
        self.assertIsNone(dc.get("n", "k2"))

    def test_max_age_override_shrinks_validity_window(self) -> None:
        import time as _time

        dc = self._fresh("ma")
        dc.set("n", "k", "v", 600, user_scope="u1")
        _time.sleep(0.02)
        # stored 600s TTL, but the caller wants entries no older than 1ms
        self.assertIsNone(dc.get("n", "k", max_age=0.001))

    def test_user_scope_invalidation(self) -> None:
        dc = self._fresh("inv")
        dc.set("n", "u1:k1", "a", 600, user_scope="u1")
        dc.set("n", "u2:k2", "b", 600, user_scope="u2")
        dc.invalidate_user("u1")
        self.assertIsNone(dc.get("n", "u1:k1"))
        self.assertEqual(dc.get("n", "u2:k2"), "b")

    def test_sqlite_persistence_across_instances(self) -> None:
        import tempfile

        path = tempfile.mkdtemp() + "/persist.sqlite3"
        dc1 = cache.DataCache(cache_dir=path)
        dc1.set("n", "u1:k", {"x": 1}, 600, user_scope="u1")
        dc2 = cache.DataCache(cache_dir=path)
        self.assertEqual(dc2.get("n", "u1:k"), {"x": 1})

    def test_clear_removes_everything(self) -> None:
        import tempfile

        path = tempfile.mkdtemp() + "/clear.sqlite3"
        dc = cache.DataCache(cache_dir=path)
        dc.set("n", "k", "v", 600)
        dc.clear()
        self.assertIsNone(dc.get("n", "k"))

    def test_non_json_value_still_cached_in_memory(self) -> None:
        dc = self._fresh("nj")
        class O:  # noqa: N801
            pass
        dc.set("n", "k", O(), 600)  # not JSON-safe: memory-only, no crash
        # hit comes back from memory for the same object identity
        self.assertIsNotNone(dc.get("n", "k"))

    def test_lru_eviction_after_max_size(self) -> None:
        dc = cache.DataCache(cache_dir="/tmp/lru-evic.sqlite3", max_size=3)
        for i in range(5):
            dc.set("n", f"k{i}", i, 600)
        # RAM holds at most max_size entries; evicted ones live on in SQLite
        self.assertLessEqual(len(dc._mem), 3)
        self.assertIsNotNone(dc.get("n", "k4"))

    def test_singleton_is_stable(self) -> None:
        self.assertIs(cache.data_cache(), cache.data_cache())

    def test_ttl_table_has_entries_for_client_methods(self) -> None:
        for method in (
            "get_my_enrollments", "get_messages", "get_course", "get_term",
            "get_gradebook_attempts", "get_course_search", "tracker",
        ):
            self.assertIn(method, cache.DEFAULT_TTL_SECONDS)


class DataCacheModeTests(unittest.TestCase):
    """NTULEARN_CACHE_MODE=off / readonly drive no-op writes."""

    def test_off_mode_disables_reads_and_writes(self) -> None:
        import tempfile
        from unittest import mock as _mock

        path = tempfile.mkdtemp() + "/off.sqlite3"
        with _mock.patch.dict(
            "os.environ", {"NTULEARN_CACHE_MODE": "off"}, clear=False
        ):
            dc = cache.DataCache(cache_dir=path)
            dc.set("n", "k", "v", 600)
            self.assertIsNone(dc.get("n", "k"))

    def test_readonly_mode_serves_hits_but_does_not_write(self) -> None:
        import tempfile
        from unittest import mock as _mock

        path = tempfile.mkdtemp() + "/ro.sqlite3"
        cache.DataCache(cache_dir=path).set("n", "k", "v", 600)
        with _mock.patch.dict(
            "os.environ", {"NTULEARN_CACHE_MODE": "readonly"}, clear=False
        ):
            dc = cache.DataCache(cache_dir=path)
            self.assertEqual(dc.get("n", "k"), "v")
            dc.set("n", "k2", "w", 600)
        dc2 = cache.DataCache(cache_dir=path)
        self.assertIsNone(dc2.get("n", "k2"))
