"""Bulletproof keychain guard for the Python test suite.

This project has a HARD rule: NEVER touch the OS keychain (macOS login keychain
password prompts). The legacy Python server/client read browser cookies via
``browser_cookie3`` (which decrypts Chrome/Edge/Safari cookies with the login
keychain) and caches cookies via ``keyring`` (also keychain-backed). If a test
path resolves a cookie without stubbing those, running the suite pops a real
macOS "enter your password" dialog.

That must be impossible. This module must be imported BEFORE ``ntulearn_mcp``
in any test module. It installs fake ``browser_cookie3`` and ``keyring`` modules
into ``sys.modules`` whose every entrypoint raises or no-ops. Because both
libraries are imported lazily inside the library functions (``import
browser_cookie3`` / ``import keyring`` at call time), the fakes are what the
library actually sees — the real libraries are never imported, so the real
keychain is never touched.
"""

from __future__ import annotations

import sys
from types import ModuleType

_GUARD_MSG = (
    "Keychain guard: browser-cookie3 is FAKE inside the Python test suite; "
    "the real library (which decrypts browser cookies via the macOS login "
    "keychain) is intentionally not importable here."
)


class _FakeBrowserCookie3(ModuleType):
    def __getattr__(self, name: str):
        # The library calls module.chrome() / module.edge() / ... To be safe
        # against any call pattern, return a stub whose *call* raises. Attribute
        # access alone (e.g. hasattr) must not blow up.
        def _boom(*a, **k):
            raise RuntimeError(_GUARD_MSG)
        return _boom


class _FakeKeyring(ModuleType):
    def get_password(self, *a, **k):
        return None

    def set_password(self, *a, **k):
        return None

    def delete_password(self, *a, **k):
        return None

    def get_credential(self, *a, **k):
        return None


_fake_bc3 = _FakeBrowserCookie3("browser_cookie3")
_fake_kr = _FakeKeyring("keyring")

# Only install if the real module is not already imported (it must not be: this
# guard runs first). If something imported the real one earlier, fail loudly.
for _name, _fake in (("browser_cookie3", _fake_bc3), ("keyring", _fake_kr)):
    if _name in sys.modules and not isinstance(sys.modules[_name], (type(_fake),)):
        raise RuntimeError(
            f"keychain guard installed too late: the real '{_name}' was already "
            "imported. Import _keychain_guard before ntulearn_mcp in every test "
            "module."
        )
    sys.modules.setdefault(_name, _fake)
