#!/usr/bin/env python3
"""Keep the NTULearn BbRouter cookie fresh, using the user's real Arc session.

How it works
------------
1. Read the stored cookie's remaining lifetime.
2. If it still has more than --min-remaining seconds, do nothing.
3. Otherwise open NTULearn in Arc. Arc's NTU/Microsoft SSO session
   re-authenticates silently -- no password, no login page.
4. Re-read Arc's cookie store, validate the cookie live against the API,
   and save it only if the API returns 200.

Never prints or logs the cookie value. Uses the macOS Keychain key
"Arc Safe Storage" (approved once via "Always Allow").
"""
import argparse, datetime, pathlib, subprocess, sys, time, urllib.error, urllib.request

DOMAIN = "ntulearn.ntu.edu.sg"
NAME = "BbRouter"
URL = "https://ntulearn.ntu.edu.sg"
API = URL + "/learn/api/public/v1/users/me"
BASE = pathlib.Path.home() / "Library/Application Support/ntulearn-mcp"
COOKIE = BASE / "cookie"
LOG = BASE / "refresh.log"


def log(msg):
    line = f"{datetime.datetime.now():%Y-%m-%d %H:%M:%S}  {msg}"
    print(line, flush=True)
    try:
        with LOG.open("a") as fh:
            fh.write(line + "\n")
    except Exception:
        pass


def expiry_of(v):
    try:
        return int(v.split("expires:", 1)[1].split(",", 1)[0])
    except Exception:
        return None


def stored_remaining():
    try:
        v = COOKIE.read_text().strip()
    except Exception:
        return None, None
    exp = expiry_of(v)
    return (exp - int(time.time())) if exp else None, v


def read_arc():
    import browser_cookie3
    for c in browser_cookie3.arc(domain_name=DOMAIN):
        if c.name == NAME and c.value.startswith("expires:"):
            return c.value
    return None


def validate(v):
    req = urllib.request.Request(API, headers={"Cookie": f"{NAME}={v}", "Accept": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=25) as r:
            return r.status
    except urllib.error.HTTPError as e:
        return e.code
    except Exception as e:
        return f"ERR {type(e).__name__}"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--min-remaining", type=int, default=2700)
    ap.add_argument("--force", action="store_true")
    ap.add_argument("--wait", type=int, default=20, help="seconds to let Arc load the page")
    a = ap.parse_args()

    remaining, _ = stored_remaining()
    if not a.force and remaining is not None and remaining > a.min_remaining:
        log(f"fresh: {remaining}s left (> {a.min_remaining}s) - nothing to do")
        return 0

    log(f"stale: {remaining if remaining is not None else 'no cookie'} - opening Arc to re-auth")
    try:
        subprocess.run(["open", "-a", "Arc", URL], check=True)
    except Exception as e:
        log(f"could not open Arc: {e}")
        return 2
    time.sleep(a.wait)

    try:
        v = read_arc()
    except Exception as e:
        log(f"arc read failed: {type(e).__name__}: {e}")
        return 3
    if not v:
        log("no valid BbRouter in Arc after opening the tab")
        return 3

    exp = expiry_of(v)
    status = validate(v)
    if status == 200:
        COOKIE.write_text(v + "\n")
        log(f"SAVED ok: len={len(v)} expires={exp} ({'' if not exp else exp - int(time.time())}s) live=200")
        return 0
    log(f"NOT saved: live check returned {status} (Arc session may need a manual login)")
    return 4


if __name__ == "__main__":
    sys.exit(main())
