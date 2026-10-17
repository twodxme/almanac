"""One GitHub cache, shared by everything that asks GitHub anything.

Three modules used to call api.github.com independently, each with its own
expiry and no idea what the others had spent. Fourteen ArtCraft release
lookups plus five agent releases is nineteen of sixty unauthenticated calls,
so two refreshes in an hour left the store with blank versions.

This fixes that four ways, in order of how much they matter:

1. **A token, when there is one.** `gh auth token` raises the ceiling from 60
   requests an hour to 5,000. It is read on demand, never written to disk here
   and never logged.
2. **Conditional requests.** Every response's ETag is kept and sent back as
   If-None-Match. GitHub answers 304 for unchanged data, and *304s do not
   count against the rate limit* -- so re-checking something that has not
   moved is free.
3. **A real cache.** Inside the TTL nothing touches the network at all.
4. **A budget guard.** The remaining quota is read from the response headers.
   When it runs low the cache is served stale instead of spending the last
   few calls, so a refresh degrades to "slightly old" rather than "empty".
"""
from __future__ import annotations

import hashlib
import json
import os
import subprocess
import threading
import time
import urllib.error
import urllib.request
from pathlib import Path

CACHE = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")) / "almanac"
DIR = CACHE / "github"
STATE = CACHE / "github-rate.json"

UA = "almanac"
API = "https://api.github.com"
DEFAULT_TTL = 6 * 3600
# Leave a few calls for whatever the user does next in a terminal.
RESERVE = 8

_lock = threading.Lock()
_token: str | None = None
_token_checked = False


def token() -> str:
    """The user's existing gh credential, if they have one.

    Read through `gh auth token` rather than from a config file, so it keeps
    working wherever gh stores it -- here that is the system keyring. Held in
    memory only.
    """
    global _token, _token_checked
    if _token_checked:
        return _token or ""
    _token_checked = True
    try:
        out = subprocess.run(["gh", "auth", "token"], capture_output=True,
                             text=True, timeout=15)
        tok = (out.stdout or "").strip()
        _token = tok if out.returncode == 0 and len(tok) > 20 else ""
    except (OSError, subprocess.SubprocessError):
        _token = ""
    return _token or ""


def _path(url: str) -> Path:
    return DIR / f"{hashlib.sha1(url.encode()).hexdigest()}.json"


def _read(url: str) -> dict | None:
    try:
        return json.loads(_path(url).read_text())
    except (OSError, ValueError):
        return None


def _write(url: str, entry: dict) -> None:
    try:
        DIR.mkdir(parents=True, exist_ok=True)
        p = _path(url)
        tmp = p.with_suffix(".tmp")
        tmp.write_text(json.dumps(entry))
        tmp.replace(p)
    except OSError:
        pass


def _state() -> dict:
    try:
        return json.loads(STATE.read_text())
    except (OSError, ValueError):
        return {}


def _note_limits(headers) -> None:
    """Record what the response said about the remaining quota."""
    try:
        remaining = headers.get("X-RateLimit-Remaining")
        reset = headers.get("X-RateLimit-Reset")
        if remaining is None:
            return
        CACHE.mkdir(parents=True, exist_ok=True)
        STATE.write_text(json.dumps({
            "remaining": int(remaining),
            "limit": int(headers.get("X-RateLimit-Limit") or 0),
            "reset": int(reset or 0),
            "at": time.time(),
        }))
    except (OSError, ValueError, TypeError):
        pass


def budget() -> dict:
    """What is left, as far as the last response told us."""
    st = _state()
    reset = st.get("reset", 0)
    if reset and time.time() > reset:
        # The window rolled over; the recorded number is meaningless now.
        return {"remaining": st.get("limit") or (5000 if token() else 60),
                "limit": st.get("limit", 0), "reset": 0, "known": False}
    return {"remaining": st.get("remaining", -1), "limit": st.get("limit", 0),
            "reset": reset, "known": "remaining" in st}


def can_spend() -> bool:
    b = budget()
    return not b["known"] or b["remaining"] > RESERVE


def get_json(url: str, ttl: int = DEFAULT_TTL, allow_stale: bool = True):
    """A cached GitHub API GET. Returns parsed JSON, or None.

    Never raises for network or quota problems: callers want "what do we know
    about this", and a listing that is six hours old beats no listing.
    """
    with _lock:
        entry = _read(url)
        now = time.time()

        if entry and now - entry.get("at", 0) < ttl:
            return entry.get("body")

        if not can_spend():
            # Out of budget. A 304 would be free, but a miss would not, and
            # there is no way to know which it is before spending it.
            return entry.get("body") if (entry and allow_stale) else None

        headers = {"User-Agent": UA,
                   "Accept": "application/vnd.github+json",
                   "X-GitHub-Api-Version": "2022-11-28"}
        tok = token()
        if tok:
            headers["Authorization"] = f"Bearer {tok}"
        if entry and entry.get("etag"):
            headers["If-None-Match"] = entry["etag"]
        elif entry and entry.get("modified"):
            headers["If-Modified-Since"] = entry["modified"]

        req = urllib.request.Request(url, headers=headers)
        try:
            with urllib.request.urlopen(req, timeout=25) as r:
                raw = r.read()
                _note_limits(r.headers)
                body = json.loads(raw.decode() or "null")
                _write(url, {"at": now, "body": body,
                             "etag": r.headers.get("ETag", ""),
                             "modified": r.headers.get("Last-Modified", "")})
                return body
        except urllib.error.HTTPError as exc:
            _note_limits(exc.headers)
            if exc.code == 304 and entry:
                # Unchanged, and this cost nothing. Reset the clock so the
                # next call inside the TTL does not even ask.
                entry["at"] = now
                _write(url, entry)
                return entry.get("body")
            if entry and allow_stale and exc.code in (403, 429, 500, 502, 503):
                return entry.get("body")
            return entry.get("body") if (entry and allow_stale) else None
        except Exception:
            return entry.get("body") if (entry and allow_stale) else None


def latest_release(owner_repo: str, ttl: int = DEFAULT_TTL):
    return get_json(f"{API}/repos/{owner_repo}/releases/latest", ttl)


def repo(owner_repo: str, ttl: int = DEFAULT_TTL):
    return get_json(f"{API}/repos/{owner_repo}", ttl)


def org_repos(org: str, ttl: int = DEFAULT_TTL):
    return get_json(f"{API}/orgs/{org}/repos?per_page=100", ttl)


def describe() -> str:
    """One line for the UI about where the quota stands."""
    b = budget()
    auth = "authenticated" if token() else "unauthenticated"
    if not b["known"]:
        return f"GitHub API: {auth}"
    left = b["remaining"]
    mins = max(0, int((b["reset"] - time.time()) / 60)) if b["reset"] else 0
    return (f"GitHub API: {left}/{b['limit']} left ({auth})"
            + (f", resets in {mins}m" if mins else ""))
