"""Parity cases for ghcache.py.

Nothing here reaches GitHub. The network cases talk to a small HTTP server
on 127.0.0.1 started by this file (it lives as long as run.py does, so the
Rust side talks to the same server), and every case that touches the cache
gets its own temp XDG_CACHE_HOME, rebuilt from the args on both sides before
the call. Settings come from a temp XDG_STATE_HOME with net_gh_token off, so
no token is read for them. The real machine is only read: its rate file
(budget/describe/can_spend) and `gh auth token` (compared as a SHA-1).
"""
import hashlib
import json
import math
import os
import socket
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import ghcache

_ROOT = Path(tempfile.mkdtemp(prefix="almanac-parity-ghcache-"))
_n = 0

_RATE = {"X-RateLimit-Remaining": "4000", "X-RateLimit-Limit": "5000",
         "X-RateLimit-Reset": "1999999999"}


class _Handler(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def _send(self, code, body=b"", headers=None):
        self.send_response(code)
        for k, v in (headers or {}).items():
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if body:
            self.wfile.write(body)

    def do_GET(self):
        p = self.path
        if p.startswith("/ok"):
            self._send(200, b'{"tag_name": "v2.0", "n": [1, 2.5, null]}',
                       {**_RATE, "ETag": '"abc"', "Last-Modified": "Wed, 01 Jan 2025 00:00:00 GMT"})
        elif p.startswith("/etag"):
            if self.headers.get("If-None-Match") == '"abc"':
                self._send(304, b"", _RATE)
            else:
                self._send(200, b'{"fresh": true}', {**_RATE, "ETag": '"abc"'})
        elif p.startswith("/modified"):
            if self.headers.get("If-Modified-Since"):
                self._send(304, b"", {"X-RateLimit-Remaining": "77"})
            else:
                self._send(200, b'{"fresh": "mod"}', {"Last-Modified": "Thu, 02 Jan 2025 00:00:00 GMT"})
        elif p.startswith("/echo"):
            want = ("user-agent", "accept", "x-github-api-version", "if-none-match",
                    "if-modified-since", "authorization")
            got = {k.lower(): v for k, v in self.headers.items() if k.lower() in want}
            self._send(200, json.dumps(got, sort_keys=True).encode())
        elif p.startswith("/unicode"):
            self._send(200, json.dumps({"name": "café ☕ 日本"}, ensure_ascii=False).encode(), _RATE)
        elif p.startswith("/empty"):
            self._send(200, b"", _RATE)
        elif p.startswith("/null"):
            self._send(200, b"null")
        elif p.startswith("/spaces"):
            self._send(200, b"   ")
        elif p.startswith("/bad"):
            self._send(200, b"not json {", _RATE)
        elif p.startswith("/latin"):
            self._send(200, b'"\xff\xfe"')
        elif p.startswith("/redir"):
            self._send(302, b"", {"Location": "/ok"})
        elif p.startswith("/rate_bad"):
            self._send(200, b"1", {"X-RateLimit-Remaining": "abc", "X-RateLimit-Limit": "60"})
        elif p.startswith("/rate_float"):
            self._send(200, b"2", {"X-RateLimit-Remaining": "1.5"})
        elif p.startswith("/rate_nolimit"):
            self._send(200, b"3", {"X-RateLimit-Remaining": "10"})
        elif p.startswith("/rate_badreset"):
            self._send(200, b"4", {"X-RateLimit-Remaining": "10", "X-RateLimit-Reset": "soon"})
        elif p.startswith("/low"):
            self._send(200, b'"spent"', {"X-RateLimit-Remaining": "2", "X-RateLimit-Limit": "60",
                                        "X-RateLimit-Reset": "1999999999"})
        # --- review round 1 endpoints ---
        elif p.startswith("/bigint"):
            self._send(200, b'{"n": 123456789012345678901234567890}')
        elif p.startswith("/hugeint"):
            self._send(200, b"[1" + b"0" * 400 + b"]")
        elif p.startswith("/inf"):
            self._send(200, b'[1e400]')
        elif p.startswith("/nanlit"):
            self._send(200, b'[Infinity, -Infinity]')
        elif p.startswith("/surrogate"):
            self._send(200, b'"\\ud800"')
        elif p.startswith("/deep"):
            self._send(200, b"[" * 200 + b"]" * 200)
        elif p.startswith("/tagl"):
            self._send(200, b'"v"', {"ETag": '"\xe9"'})
        elif p.startswith("/loop"):
            self._send(302, b"", {**_RATE, "Location": "/loop"})
        elif p.startswith("/hop_echo"):
            self._send(302, b"", {"Location": "/echo"})
        elif p.startswith("/slow"):
            self.send_response(200)
            self.send_header("Content-Length", "13")
            self.end_headers()
            for ch in b"[" + b" " * 11 + b"]":
                self.wfile.write(bytes([ch]))
                self.wfile.flush()
                time.sleep(2.1)
        else:
            code = int(p.strip("/").split("?")[0] or 404) if p.strip("/").split("?")[0].isdigit() else 404
            self._send(code, b'{"message": "nope"}', _RATE)


_server = ThreadingHTTPServer(("127.0.0.1", 0), _Handler)
threading.Thread(target=_server.serve_forever, daemon=True).start()
BASE = f"http://127.0.0.1:{_server.server_address[1]}"


def _closed_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


DEAD = f"http://127.0.0.1:{_closed_port()}/gone"


# Review round 1: a stand-in `gh` first on PATH. It answers from
# $XDG_STATE_HOME/fakegh.out (exit code from fakegh.code) when that file is
# there, and otherwise runs the real gh, so the real-machine cases still see
# the real credential. PATH is set before run.py launches the Rust side, so
# both sides see the same `gh`.
import shutil as _shutil
_REAL_GH = _shutil.which("gh") or "/nonexistent/gh"
_BIN = _ROOT / "bin"
_BIN.mkdir()
(_BIN / "gh").write_text(
    "#!/bin/sh\n"
    'if [ -n "$XDG_STATE_HOME" ] && [ -f "$XDG_STATE_HOME/fakegh.out" ]; then\n'
    '  cat "$XDG_STATE_HOME/fakegh.out"; exit "$(cat "$XDG_STATE_HOME/fakegh.code" 2>/dev/null || echo 0)"\n'
    "fi\n"
    f'exec "{_REAL_GH}" "$@"\n')
(_BIN / "gh").chmod(0o755)
os.environ["PATH"] = f"{_BIN}:{os.environ.get('PATH', '')}"


def _dir():
    global _n
    _n += 1
    d = _ROOT / str(_n)
    d.mkdir()
    return str(d)


def _setup(a):
    """What the Rust parity hook does with the same args."""
    saved = (ghcache.CACHE, ghcache.DIR, ghcache.STATE, os.environ.get("XDG_STATE_HOME"))
    if "cache_home" in a:
        ghcache.CACHE = Path(a["cache_home"]) / "almanac"
        ghcache.DIR = ghcache.CACHE / "github"
        ghcache.STATE = ghcache.CACHE / "github-rate.json"
        import shutil
        shutil.rmtree(ghcache.CACHE, ignore_errors=True)
        ghcache.DIR.mkdir(parents=True)
        for url, e in a.get("entries", {}).items():
            ghcache._path(url).write_text(json.dumps(e))
        for url, t in a.get("raw", {}).items():
            ghcache._path(url).write_text(t)
        if a.get("state") is not None:
            ghcache.STATE.write_text(a["state"])
    if "state_home" in a:
        os.environ["XDG_STATE_HOME"] = a["state_home"]
        import settings
        p = settings.path()
        p.parent.mkdir(parents=True, exist_ok=True)
        if "settings" in a:
            p.write_text(json.dumps(a["settings"]))
    return saved


def _restore(saved):
    ghcache.CACHE, ghcache.DIR, ghcache.STATE, old = saved
    if old is None:
        os.environ.pop("XDG_STATE_HOME", None)
    else:
        os.environ["XDG_STATE_HOME"] = old


def _state_report():
    try:
        st = json.loads(ghcache.STATE.read_text())
    except (OSError, ValueError):
        return None
    if isinstance(st, dict):
        at = st.pop("at", None)
        st["at_present"] = at is not None
    return st


def _report(a, url, result):
    orig_at = a.get("entries", {}).get(url, {}).get("at")
    try:
        e = json.loads(ghcache._path(url).read_text())
    except (OSError, ValueError):
        e = None
    if isinstance(e, dict):
        at = e.pop("at", None)
        e["at_changed"] = at != orig_at
    return {"result": result, "entry": e, "state": _state_report()}


def _sha(t):
    return hashlib.sha1(t.encode()).hexdigest() if t else ""


def _exact(v):
    """The value as JSON can carry it to the runner: what serde_json cannot
    hold (an int beyond 64 bits, inf/nan, a lone surrogate) as a tagged
    object, the same tagging the Rust hook applies to its exact value."""
    def str_ok(s):
        try:
            s.encode("utf-8")
            return True
        except UnicodeEncodeError:
            return False
    if v is None or isinstance(v, bool):
        return v
    if isinstance(v, int):
        return v if -2**63 <= v < 2**64 else {"$py": "int", "v": str(v)}
    if isinstance(v, float):
        return v if math.isfinite(v) else {"$py": "float", "v": repr(v)}
    if isinstance(v, str):
        return v if str_ok(v) else {"$py": "str", "v": json.dumps(v)}
    if isinstance(v, (list, tuple)):
        return [_exact(x) for x in v]
    if isinstance(v, dict):
        return {(k if not isinstance(k, str) or str_ok(k) else json.dumps(k)): _exact(x) for k, x in v.items()}
    return v


def _call(a, fn):
    def run():
        saved = _setup(a)
        try:
            return _exact(fn())
        finally:
            _restore(saved)
    return run


def _env(**kw):
    """Temp cache + temp settings with the token off."""
    a = {"cache_home": _dir(), "state_home": _dir(), "settings": {"net_gh_token": False}}
    a.update(kw)
    return a


def _get(url, *, ttl=None, allow_stale=None, accept=None, **kw):
    a = _env(url=url, **kw)
    k = {}
    if ttl is not None:
        a["ttl"] = k["ttl"] = ttl
    if allow_stale is not None:
        a["allow_stale"] = k["allow_stale"] = allow_stale
    if accept is not None:
        a["accept"] = k["accept"] = accept
    return "get_json", a, _call(a, lambda: _report(a, url, ghcache.get_json(url, **k)))


def _rate(remaining=3, limit=60, reset_in=3600 * 5 + 45, **extra):
    st = {"remaining": remaining, "limit": limit, "reset": int(time.time()) + reset_in, **extra}
    return json.dumps(st)


def cases():
    now = time.time()
    future = int(now) + 3600 * 5 + 45      # 345 minutes, with room for the run
    past = int(now) - 100

    # --- token ---------------------------------------------------------
    off = {"state_home": _dir(), "settings": {"net_gh_token": False}}
    yield "token", off, _call(off, lambda: _sha(ghcache.token()))

    def _reset():
        ghcache._token_checked = False
        return _sha(ghcache.token())
    # The real machine. Start from a clean memo on both sides: in a full run
    # this Python process has already run other modules' cases (agents reads
    # ghcache with gh off PATH and memoises ""), while each Rust module runs
    # in a fresh process.
    yield "reset_token", {}, _reset
    yield "token", {}, lambda: _sha(ghcache.token())                 # memoised
    yield "token", off, _call(off, lambda: _sha(ghcache.token()))    # off beats the memo

    yield "reset_token", {}, _reset
    yield "reset_token", off, _call(off, _reset)

    # --- _path / _read / _state ----------------------------------------
    for url in ("https://api.github.com/repos/a/b", "", "https://x/ünï ☕?q=1&b=2"):
        a = _env(url=url)
        yield "_path", a, _call(a, lambda url=url: ghcache._path(url))
    u = "https://api.github.com/repos/a/b"
    a = _env(url=u, entries={u: {"at": 1.5, "body": {"x": "é"}, "etag": "e", "modified": ""}})
    yield "_read", a, _call(a, lambda: ghcache._read(u))
    a = _env(url=u)
    yield "_read", a, _call(a, lambda: ghcache._read(u))
    a = _env(url=u, raw={u: "{broken"})
    yield "_read", a, _call(a, lambda: ghcache._read(u))
    a = _env(url=u, raw={u: "{}"})
    yield "_read", a, _call(a, lambda: ghcache._read(u))
    a = _env(url=u, raw={u: "\xff not utf8"})
    yield "_read", a, _call(a, lambda: ghcache._read(u))
    for st in (None, "", "{oops", '{"remaining": 5, "limit": 60, "reset": 12, "at": 1.0}', "{}"):
        a = _env(state=st)
        yield "_state", a, _call(a, ghcache._state)

    # --- _note_limits ----------------------------------------------------
    for h in ({"X-RateLimit-Remaining": "42", "X-RateLimit-Limit": "60", "X-RateLimit-Reset": "1700000000"},
              {"x-ratelimit-remaining": "42"},
              {"X-RateLimit-Remaining": " 7 ", "X-RateLimit-Limit": "", "X-RateLimit-Reset": ""},
              {"X-RateLimit-Limit": "60"},
              {"X-RateLimit-Remaining": "abc"},
              {"X-RateLimit-Remaining": "1", "X-RateLimit-Limit": "x"},
              {"X-RateLimit-Remaining": "1", "X-RateLimit-Reset": "1.5"},
              {"X-RateLimit-Remaining": "-3", "X-RateLimit-Limit": "+60"},
              {}):
        a = _env(headers=h, state='{"remaining": 999}')

        class _H(dict):
            def get(self, k, d=None):
                for kk, v in self.items():
                    if kk.lower() == k.lower():
                        return v
                return d
        yield "_note_limits", a, _call(a, lambda h=h: (ghcache._note_limits(_H(h)), _state_report())[1])

    # --- budget / can_spend / describe -----------------------------------
    states = [
        None, "", "{bad",
        _rate(3, 60),                                   # low, known
        _rate(4000, 5000),                              # plenty
        _rate(8, 60), _rate(9, 60),                     # around RESERVE
        _rate(0, 60, reset_in=-100),                    # rolled over: limit
        json.dumps({"remaining": 0, "limit": 0, "reset": past}),   # rolled over, no limit
        json.dumps({"remaining": 12, "limit": 60, "reset": 0}),    # no reset
        json.dumps({"remaining": 12, "limit": 60}),
        json.dumps({"limit": 60, "reset": future}),     # unknown remaining
        json.dumps({"remaining": 12, "limit": 60, "reset": int(now) + 20}),  # under a minute
    ]
    for st in states:
        for fn in ("budget", "can_spend", "describe"):
            a = _env(state=st)
            yield fn, a, _call(a, getattr(ghcache, fn))
    # Rolled over with no limit and the real settings: the token decides.
    a = {"cache_home": _dir(), "state": json.dumps({"remaining": 0, "reset": past})}
    yield "budget", a, _call(a, ghcache.budget)
    yield "describe", a, _call(a, ghcache.describe)
    # The real machine, read only.
    yield "budget", {}, ghcache.budget
    yield "can_spend", {}, ghcache.can_spend
    yield "describe", {}, ghcache.describe

    # --- get_json ----------------------------------------------------------
    fresh = {"at": now, "body": {"cached": True}, "etag": '"abc"', "modified": ""}
    old = {"at": 1000.0, "body": {"cached": "old"}, "etag": '"abc"', "modified": ""}
    oldmod = {"at": 1000.0, "body": ["old-mod"], "etag": "", "modified": "Wed, 01 Jan 2025 00:00:00 GMT"}
    oldwrong = {"at": 1000.0, "body": "x", "etag": '"zzz"', "modified": "Wed"}
    nullbody = {"at": 1000.0, "body": None, "etag": '"abc"'}

    yield _get(BASE + "/ok")
    yield _get(BASE + "/ok", entries={BASE + "/ok": fresh})                  # inside TTL
    yield _get(BASE + "/ok", ttl=0, entries={BASE + "/ok": fresh})           # TTL 0
    yield _get(BASE + "/etag", entries={BASE + "/etag": old})                # 304
    yield _get(BASE + "/etag", entries={BASE + "/etag": nullbody})           # 304, null body
    yield _get(BASE + "/etag", entries={BASE + "/etag": oldwrong})           # changed
    yield _get(BASE + "/etag")                                               # 304 needs an entry
    yield _get(BASE + "/modified", entries={BASE + "/modified": oldmod})     # If-Modified-Since
    yield _get(BASE + "/modified")
    yield _get(BASE + "/echo")
    yield _get(BASE + "/echo", accept="application/vnd.github.mercy-preview+json")
    yield _get(BASE + "/echo", entries={BASE + "/echo": old})
    yield _get(BASE + "/echo", entries={BASE + "/echo": oldmod})
    yield _get(BASE + "/echo", entries={BASE + "/echo": oldwrong})
    yield _get(BASE + "/unicode")
    yield _get(BASE + "/empty")
    yield _get(BASE + "/null")
    yield _get(BASE + "/spaces", entries={BASE + "/spaces": old})
    yield _get(BASE + "/bad")
    yield _get(BASE + "/bad", entries={BASE + "/bad": old})
    yield _get(BASE + "/bad", entries={BASE + "/bad": old}, allow_stale=False)
    yield _get(BASE + "/latin", entries={BASE + "/latin": old})
    yield _get(BASE + "/redir")
    yield _get(BASE + "/rate_bad")
    yield _get(BASE + "/rate_float")
    yield _get(BASE + "/rate_nolimit")
    yield _get(BASE + "/rate_badreset")
    yield _get(BASE + "/low")
    for code in (403, 404, 429, 500, 502, 503, 418):
        url = f"{BASE}/{code}"
        yield _get(url)
        yield _get(url, entries={url: old})
        yield _get(url, entries={url: old}, allow_stale=False)
    yield _get(DEAD)
    yield _get(DEAD, entries={DEAD: old})
    yield _get(DEAD, entries={DEAD: old}, allow_stale=False)
    # A URL with no scheme is left out: Python's Request() raises ValueError
    # before the try (outside get_json's "never raises"); the Rust port
    # returns the stale entry / None instead.
    # The budget guard: nothing is spent, stale or nothing comes back.
    yield _get(BASE + "/ok", state=_rate(3, 60), entries={BASE + "/ok": old})
    yield _get(BASE + "/ok", state=_rate(3, 60), entries={BASE + "/ok": old}, allow_stale=False)
    yield _get(BASE + "/ok", state=_rate(3, 60))
    yield _get(BASE + "/ok", state=_rate(9, 60))
    yield _get(BASE + "/ok", state=_rate(0, 60, reset_in=-10))               # rolled over: spend
    # Broken or empty entries on disk.
    yield _get(BASE + "/ok", raw={BASE + "/ok": "{broken"})
    yield _get(BASE + "/ok", raw={BASE + "/ok": "{}"})
    yield _get(BASE + "/ok", raw={BASE + "/ok": '{"body": 5}'})              # no "at": stale
    yield _get(BASE + "/404", raw={BASE + "/404": '{"body": 5}'})

    # --- latest_release / repo / org_repos (served from the cache only) ------
    for fn, key, val, url in (
            ("latest_release", "owner_repo", "basecamp/omarchy", f"{ghcache.API}/repos/basecamp/omarchy/releases/latest"),
            ("repo", "owner_repo", "basecamp/omarchy", f"{ghcache.API}/repos/basecamp/omarchy"),
            ("org_repos", "org", "omacom-io", f"{ghcache.API}/orgs/omacom-io/repos?per_page=100"),
            ("repo", "owner_repo", "ünï/☕", f"{ghcache.API}/repos/ünï/☕")):
        f = getattr(ghcache, fn)
        a = _env(**{key: val}, entries={url: fresh})
        yield fn, a, _call(a, lambda f=f, a=a, val=val, url=url: _report(a, url, f(val)))
        # A caller's own TTL: ten days old, a thirty-day TTL.
        tendays = {**old, "at": now - 864000}
        a = _env(**{key: val}, ttl=2592000, entries={url: tendays})
        yield fn, a, _call(a, lambda f=f, a=a, val=val, url=url: _report(a, url, f(val, 2592000)))
        # Out of budget: never asks, stale or nothing.
        a = _env(**{key: val}, state=_rate(1, 60), entries={url: old})
        yield fn, a, _call(a, lambda f=f, a=a, val=val, url=url: _report(a, url, f(val)))
        a = _env(**{key: val}, state=_rate(1, 60))
        yield fn, a, _call(a, lambda f=f, a=a, val=val, url=url: _report(a, url, f(val)))


    # --- review round 1 ----------------------------------------------------
    u = "https://api.github.com/repos/a/b"
    # _read / _state of JSON that is not an object: Python returns it as is.
    for raw in ("[1, 2]", '"s"', "5", "true"):
        a = _env(url=u, raw={u: raw})
        yield "_read", a, _call(a, lambda: ghcache._read(u))
    for st in ("[1]", "null", '"x"'):
        a = _env(state=st)
        yield "_state", a, _call(a, ghcache._state)
        a = _env(state=st)
        yield "budget", a, _call(a, ghcache.budget)
    # Odd but valid JSON values in the rate file.
    odd = [
        json.dumps({"remaining": 8.5, "limit": 60, "reset": future}),
        json.dumps({"remaining": 12, "limit": None, "reset": future}),
        json.dumps({"remaining": 12, "limit": 60, "reset": future + 0.9}),
        json.dumps({"remaining": "100", "limit": 60, "reset": future}),
        json.dumps({"remaining": None, "limit": 60, "reset": future}),
        json.dumps({"remaining": 0, "limit": "60", "reset": past}),
        json.dumps({"remaining": 12, "limit": 60.5, "reset": future}),
        json.dumps({"remaining": 12, "limit": 60, "reset": "soon"}),
    ]
    for st in odd:
        for fn in ("budget", "can_spend", "describe"):
            a = _env(state=st)
            yield fn, a, _call(a, getattr(ghcache, fn))
    # int() of header values the way Python reads them.
    for h in ({"X-RateLimit-Remaining": "1_000"},
              {"X-RateLimit-Remaining": "\u0663"},
              {"X-RateLimit-Remaining": "99999999999999999999"},
              {"X-RateLimit-Remaining": "5", "X-RateLimit-Limit": "6_0"}):
        a = _env(headers=h, state='{"remaining": 999}')

        class _H2(dict):
            def get(self, k, d=None):
                for kk, v in self.items():
                    if kk.lower() == k.lower():
                        return v
                return d
        yield "_note_limits", a, _call(a, lambda h=h: (ghcache._note_limits(_H2(h)), _state_report())[1])
    # Entries whose fields have the wrong type.
    yield _get(BASE + "/ok", raw={BASE + "/ok": '{"at": "x", "body": 1}'})
    yield _get(BASE + "/ok", raw={BASE + "/ok": '{"at": null, "body": 1}'})
    yield _get(BASE + "/ok", raw={BASE + "/ok": "[1, 2]"})
    yield _get(BASE + "/echo", entries={BASE + "/echo": {"at": 1000.0, "body": 1, "etag": 5}})
    yield _get(BASE + "/echo", entries={BASE + "/echo": {"at": 1000.0, "body": 1, "etag": True}})
    yield _get(BASE + "/echo", entries={BASE + "/echo": {"at": 1000.0, "body": 1, "etag": "", "modified": 7}})
    # Bodies Python's json accepts and serde_json does not; both sides report
    # them exactly through _exact().
    for path in ("/bigint", "/hugeint", "/inf", "/nanlit", "/surrogate", "/deep"):
        yield _get(BASE + path)
    # A header that is not UTF-8 (latin-1 on the wire), kept and sent back.
    yield _get(BASE + "/tagl")
    # A redirect loop: urllib raises HTTPError(302) with the last headers.
    yield _get(BASE + "/loop")
    yield _get(BASE + "/loop", entries={BASE + "/loop": old})
    # Conditional headers across a redirect.
    yield _get(BASE + "/hop_echo", entries={BASE + "/hop_echo": old})
    # urllib's other schemes.
    yield _get("data:application/json,[1,2]")
    jf = _ROOT / "local.json"
    jf.write_text('{"local": true}')
    yield _get(jf.as_uri())
    # No scheme: Python's Request() raises before the try.
    yield _get("api.github.com/repos/a/b")
    # A caller's ttl that is not a non-negative int.
    yield _get(BASE + "/ok", ttl=-1, entries={BASE + "/ok": fresh})
    yield _get(BASE + "/ok", ttl=2.5e9, entries={BASE + "/ok": old})
    # /slow (a body trickling in over ~27 s) is not yielded here: it holds
    # the run up long enough to age the time-pinned budget cases above.

    # --- token, through the stand-in gh -------------------------------------
    def _fake(out: bytes, code=0):
        a = {"state_home": _dir(), "settings": {"net_gh_token": True}}
        Path(a["state_home"], "fakegh.out").write_bytes(out)
        Path(a["state_home"], "fakegh.code").write_text(str(code))
        return a
    for out, code in ((b"gho_" + b"x" * 30 + b"\n", 0),
                      (b"gho_" + b"x" * 30 + b"\n", 1),
                      (b"x" * 20 + b"\n", 0),
                      (b"\x1fgho_" + b"x" * 17 + b"\x1f\n", 0),
                      (b"\xc2\xa0gho_" + b"x" * 30 + b"\n", 0),
                      (b"gho_" + b"\xff" * 30 + b"\n", 0)):
        a = _fake(out, code)
        yield "reset_token", a, _call(a, _reset)
    # Leave the memo as the real machine has it.
    yield "reset_token", {}, _reset


# --- review round 2 ----------------------------------------------------------
# More endpoints, served by the same test server (checked before the round-1
# ones), and cases appended after round 1's.
import sys as _sys
_sys.setrecursionlimit(20000)       # the runner's normalise() on deep bodies

_r1_get = _Handler.do_GET


def _r2_raw(h, lines, body, status=b"200 OK"):
    h.wfile.write(b"HTTP/1.1 " + status + b"\r\n" + b"".join(l + b"\r\n" for l in lines)
                  + b"Content-Length: %d\r\nConnection: close\r\n\r\n" % len(body) + body)
    h.close_connection = True


def _r2_get(self):
    p = self.path
    if p.startswith("/r2trail"):
        _r2_raw(self, [b'ETag: "abc"   ', b"X-RateLimit-Remaining: 4000   "], b'"trail"')
    elif p.startswith("/r2ctrl"):
        _r2_raw(self, [b"X-Odd: a\x01b"], b'"ctrl"')
    elif p.startswith("/r2fold"):
        _r2_raw(self, [b"X-Odd: a", b" b"], b'"fold"')
    elif p.startswith("/r2sp"):
        _r2_raw(self, [b"X Odd: v", b"X-RateLimit-Remaining: 5"], b'"sp"')
    elif p.startswith("/r2many"):
        _r2_raw(self, [b"X-H%d: v" % i for i in range(110)], b'"many"')
    elif p.startswith("/r2big"):
        _r2_raw(self, [b"X-A: " + b"a" * 40000, b"X-B: " + b"b" * 40000], b'"big"')
    elif p.startswith("/r2dupe"):
        _r2_raw(self, [b"X-RateLimit-Remaining: 5", b"X-RateLimit-Remaining: 100",
                       b"X-RateLimit-Limit: 60", b'ETag: "one"', b'ETag: "two"'], b'"dupe"')
    elif p.startswith("/r2path"):
        _r2_raw(self, [], json.dumps({"path": p}).encode())
    elif p.startswith("/r2rq"):
        _r2_raw(self, [b'Location: /r2path"{x}`'], b"", b"302 Found")
    elif p.startswith("/r2rbs"):
        _r2_raw(self, [b"Location: /r2path\\x"], b"", b"302 Found")
    elif p.startswith("/r2rfrag"):
        _r2_raw(self, [b"Location: /r2path#frag"], b"", b"302 Found")
    elif p.startswith("/r2deep"):
        _r2_raw(self, [], b"[" * 1100 + b"]" * 1100)
    else:
        _r1_get(self)


_Handler.do_GET = _r2_get
_cases_r1 = cases


def cases():
    yield from _cases_r1()
    now = time.time()
    future = int(now) + 3600 * 5 + 45

    # Response headers: http.client keeps trailing whitespace, accepts any
    # control byte but CR/LF, obs-fold, 101+ headers is an error, a 64 KB
    # line limit per header (not per block), and a malformed header line
    # ends the header block instead of failing the request.
    for path in ("/r2trail", "/r2ctrl", "/r2fold", "/r2sp", "/r2many", "/r2big", "/r2dupe"):
        yield _get(BASE + path)
    # Redirect targets: urljoin + quote(safe=punctuation) vs url::Url::join.
    for path in ("/r2rq", "/r2rbs", "/r2rfrag"):
        yield _get(BASE + path)
    # Request targets sent as they are by http.client.
    yield _get(BASE + '/r2path"{|}^`')
    yield _get(BASE + "/r2path/a/../b")
    # Userinfo: urllib takes "u:p@127.0.0.1" as the host name.
    yield _get(BASE.replace("http://", "http://u:p@") + "/r2path")
    # Request header values http.client sends and the http crate refuses.
    for etag in ("a\x01b", "a\x7fb", "a\r\n b"):
        yield _get(BASE + "/echo", entries={BASE + "/echo": {"at": 1000.0, "body": "stale", "etag": etag}})
    # Nesting deeper than 1000 levels: fine for Python's json (C stack bound).
    yield _get(BASE + "/r2deep")
    u = "https://api.github.com/repos/a/b"
    deep = "[" * 1100 + "]" * 1100
    a = _env(url=u, raw={u: deep})
    yield "_read", a, _call(a, lambda: ghcache._read(u))
    a = _env(url=u, raw={u: '{"at": 9e18, "body": ' + deep + "}"})
    yield "get_json", a, _call(a, lambda: _report(a, u, ghcache.get_json(u)))
    a = _env(state=deep)
    yield "_state", a, _call(a, ghcache._state)
    # file: URL authority resolved by gethostbyname, not compared as text.
    jf = _ROOT / "local2.json"
    jf.write_text('{"local": 2}')
    yield _get("file://LOCALHOST" + str(jf))
    # repr() of non-printable code points inside a container.
    st = json.dumps({"remaining": ["͸", "؀", "\U000e0001"], "limit": 60, "reset": future})
    a = _env(state=st)
    yield "describe", a, _call(a, ghcache.describe)
    st = json.dumps({"remaining": {"k": "⁦x"}, "limit": [0.1, -0.0, 1e16], "reset": future})
    a = _env(state=st)
    yield "describe", a, _call(a, ghcache.describe)
    # Numbers json reads differently than the obvious way.
    for raw in ("-0", "-0.0", "1E+2", "[1e-400, 2.5e-324]", '{"a": 1, "a": 2, "b": 3}'):
        a = _env(url=u, raw={u: raw})
        yield "_read", a, _call(a, lambda: ghcache._read(u))

    def _reset():
        ghcache._token_checked = False
        return _sha(ghcache.token())
    yield "reset_token", {}, _reset


# --- review round 3 ----------------------------------------------------------
# The HTTP layer is now http.client's own rules, ported: these responses are
# written byte for byte, to pin the parsing on both sides.
def _r3_send(h, data):
    h.wfile.write(data)
    h.close_connection = True


_R3 = {
    "/r3chunked": b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nETag: c\r\n\r\n"
                  b"3;ext=1\r\n[1,\r\n0x_3\r\n 2]\r\n0\r\nX-Trailer: t\r\n\r\n",
    "/r3chunkbad": b"HTTP/1.1 200 OK\r\nTransfer-Encoding: Chunked\r\n\r\nzz\r\n[1]\r\n0\r\n\r\n",
    "/r3chunkshort": b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n10\r\n[1]",
    "/r3chunkneg": b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n-1\r\n[1]\r\n0\r\n\r\n",
    "/r3short": b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n[1]",
    "/r3badlen": b"HTTP/1.1 200 OK\r\nContent-Length: ten\r\n\r\n[2]",
    "/r3neglen": b"HTTP/1.1 200 OK\r\nContent-Length: -5\r\n\r\n[3]",
    "/r3lenws": b"HTTP/1.1 200 OK\r\nContent-Length:  3_0 \r\n\r\n" + b'"' + b"x" * 28 + b'"',
    "/r3eof": b"HTTP/1.0 200 OK\r\nETag: e\r\n\r\n[4]",
    "/r3interim": b"HTTP/1.1 100 Continue\r\nX: y\r\n\r\nHTTP/1.1 200 OK\r\nETag: i\r\nContent-Length: 3\r\n\r\n[5]",
    "/r3http2": b"HTTP/2.0 200 OK\r\nContent-Length: 3\r\n\r\n[6]",
    "/r3badstatus": b"HTTP/1.1 2x0 OK\r\nContent-Length: 3\r\n\r\n[7]",
    "/r3nostatus": b"ICY 200 OK\r\nContent-Length: 3\r\n\r\n[8]",
    "/r3statusws": b"HTTP/1.1\t+2_00\r\nContent-Length: 3\r\n\r\n[9]",
    "/r3noreason": b"HTTP/1.1 200\r\nETag: nr\r\nContent-Length: 3\r\n\r\n[10]"[:-4] + b"[10]",
    "/r3from": b"HTTP/1.1 200 OK\r\nFrom someone\r\nETag: f\r\nFrom x\r\nX-RateLimit-Remaining: 9\r\n"
               b"Content-Length: 3\r\n\r\n[11]"[:-4] + b"[11]",
    "/r3cr": b"HTTP/1.1 200 OK\r\nETag: a\rX-RateLimit-Remaining: 7\r\nContent-Length: 4\r\n\r\n[12]",
    "/r3cont": b"HTTP/1.1 200 OK\r\n first-is-continuation\r\nETag:\r\n  folded\r\n\tmore\r\nContent-Length: 4\r\n\r\n[13]",
    "/r3colon": b"HTTP/1.1 200 OK\r\n:nameless\r\n cont\r\nETag: co\r\nContent-Length: 4\r\n\r\n[14]",
    "/r3lf": b"HTTP/1.1 200 OK\nETag: lf\nContent-Length: 4\n\n[15]",
    "/r3h99": b"HTTP/1.1 200 OK\r\n" + b"".join(b"X-%d: v\r\n" % i for i in range(98))
              + b"Content-Length: 4\r\n\r\n[16]",
    "/r3h100": b"HTTP/1.1 200 OK\r\n" + b"".join(b"X-%d: v\r\n" % i for i in range(99))
               + b"Content-Length: 4\r\n\r\n[17]",
    "/r3line64k": b"HTTP/1.1 200 OK\r\nX-A: " + b"a" * (65536 - 7) + b"\r\nContent-Length: 4\r\n\r\n[18]",
    "/r3line64k1": b"HTTP/1.1 200 OK\r\nX-A: " + b"a" * (65536 - 6) + b"\r\nContent-Length: 4\r\n\r\n[19]",
    "/r3latin": b"HTTP/1.1 200 OK\r\nETag: \xe9t\xe9\xa0 \r\nContent-Length: 4\r\n\r\n[20]",
    "/r3304": b"HTTP/1.1 304 Not Modified\r\nX-RateLimit-Remaining: 33\r\nContent-Length: 50\r\n\r\n",
    "/r3204": b"HTTP/1.1 204 No Content\r\nX-RateLimit-Remaining: 34\r\n\r\n",
    "/r3rquery": b"HTTP/1.1 302 Found\r\nLocation: ?q=1\r\nContent-Length: 0\r\n\r\n",
    "/r3rdots": b"HTTP/1.1 301 Moved\r\nLocation: ./a/../../r2path/x/./y/..\r\nContent-Length: 0\r\n\r\n",
    "/r3rparams": b"HTTP/1.1 307 Temp\r\nLocation: /r2path;p=1?x=2\r\nContent-Length: 0\r\n\r\n",
    "/r3rlatin": b"HTTP/1.1 308 Perm\r\nLocation: /r2path\xe9 \xff\r\nContent-Length: 0\r\n\r\n",
    "/r3rv6": b"HTTP/1.1 302 Found\r\nLocation: http://[::1/x\r\nX-RateLimit-Remaining: 12\r\nContent-Length: 0\r\n\r\n",
    "/r3rjs": b"HTTP/1.1 302 Found\r\nLocation: javascript:alert(1)\r\nX-RateLimit-Remaining: 13\r\nContent-Length: 0\r\n\r\n",
    "/r3ruri": b"HTTP/1.1 303 See\r\nURI: /r2path/uri\r\nContent-Length: 0\r\n\r\n",
    "/r3rnone": b"HTTP/1.1 302 Found\r\nX-RateLimit-Remaining: 14\r\nContent-Length: 0\r\n\r\n",
    "/r3rshort": b"HTTP/1.1 302 Found\r\nLocation: /r2path\r\nContent-Length: 9\r\n\r\nab",
    "/r3rempty": b"HTTP/1.1 302 Found\r\nLocation:\r\nContent-Length: 0\r\n\r\n",
    "/r3rnet": b"HTTP/1.1 302 Found\r\nLocation: //127.0.0.1:%d\r\nContent-Length: 0\r\n\r\n",
    "/r3close": b"",
}

_r2_get_prev = _Handler.do_GET


def _r3_get(self):
    p = self.path.split("?")[0].split("#")[0]
    if p in _R3:
        data = _R3[p]
        if b"%d" in data:
            data = data % _server.server_address[1]
        _r3_send(self, data)
    else:
        _r2_get_prev(self)


_Handler.do_GET = _r3_get
_cases_r2 = cases


def cases():
    yield from _cases_r2()
    old = {"at": 1000.0, "body": "stale", "etag": "old"}
    for path in sorted(_R3):
        yield _get(BASE + path)
        yield _get(BASE + path, entries={BASE + path: old})
    port = _server.server_address[1]
    # How urllib reads the URL itself.
    yield _get(f"http://127.0.0.1%3a{port}/r2path")              # the host is unquoted
    yield _get(f"http://127.0.0.1:{port}?x=1")                    # a selector gets its /
    yield _get(f"http://127.0.0.1:{port}/r2path#a#b")              # only the last # splits
    yield _get(f"http://127.0.0.1:{str(port)[:2]}_{str(port)[2:]}/r2path")   # int() reads the port
    yield _get(f"http://127.0.0.1: {port}/r2path")                # a space in the host
    yield _get(f"http://127.0.0.1:{port}/r2pathé")           # not ASCII: refused
    yield _get(f"http://127.0.0.1:{port}:/r2path")                # a nonnumeric port
    yield _get(f"http://127.0.0.1:/r2path")                       # empty port: 80
    yield _get(f"http://[::1/r2path")                             # Request() raises
    yield _get(f"http://[127.0.0.1]:{port}/r2path")               # IPv4 in brackets raises
    yield _get(f"http://／.example/r2path")                   # NFKC delimiter raises
    yield _get(f"<URL: http://127.0.0.1:{port}/r2path >")          # unwrap()
    yield _get(f"HTTP://127.0.0.1:{port}/r2path")
    yield _get(f"http:///r2path")                                 # no host given
    yield _get(f"http://127.0.0.1:{port}/r2path/%7e?a=%20")
    yield _get(f"http://127.0.0.1:{port}/r3close")
    # file: URLs and their authority.
    jf = _ROOT / "local3.json"
    jf.write_text('{"local": 3}')
    for auth in ("", "localhost", "127.0.0.1", "127.1", os.uname().nodename, "example.invalid", "LocalHost"):
        yield _get(f"file://{auth}{jf}")
    yield _get(f"file:{jf}")
    yield _get(f"file://localhost{jf}?q#f")
    yield _get(f"file://localhost{str(jf).replace('local3', 'local%33')}")
