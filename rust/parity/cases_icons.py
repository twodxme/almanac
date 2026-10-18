"""Parity cases for icons.py.

Nothing here touches the real cache: every case that reads or writes one
gets its own temp XDG_CACHE_HOME (and XDG_STATE_HOME for the settings
switch), laid out from the args before the call on both sides -- the Rust
side empties and rebuilds the same directory, so the Python run (which goes
first) cannot leak into it. Python's icons.CACHE/ICONS/MISSES are fixed at
import, so they are patched per case to match what Rust reads at call time.

The network is a local HTTP server on 127.0.0.1, up for the whole run (the
Rust binary is called while this process is still alive). Domain lookups go
only to `.invalid` and `localhost`, which fail without leaving the machine.
The real cache is read only, through `cached` / `_load_misses`.
"""
import atexit
import http.server
import os
import shutil
import tempfile
import threading
from pathlib import Path

import icons

_ROOT = Path(tempfile.mkdtemp(prefix="almanac-parity-icons-"))
# Removed when the run ends (the Rust side has finished by then).
atexit.register(shutil.rmtree, _ROOT, True)
_n = 0

PNG = b"\x89PNG\r\n\x1a\n" + bytes(range(256)) * 2
BODIES = {
    "/png": PNG,
    "/ico": b"\x00\x00\x01\x00" + b"\x01" * 100,
    "/gif": b"GIF89a" + b"\x02" * 100,
    "/jpg": b"\xff\xd8\xff\xe0" + b"\x03" * 100,
    "/svg": b"<svg xmlns='http://www.w3.org/2000/svg'>" + b" " * 60 + b"</svg>",
    "/svg-ws": b" \x0b\t\r\n\x0c<svg xmlns='http://www.w3.org/2000/svg'>" + b" " * 60 + b"</svg>",
    "/svg-late": b" " * 199 + b"<svg>" + b" " * 60,
    "/xml": b"<?xml version='1.0'?>" + b" " * 60,
    "/webp": b"RIFF" + b"\x04" * 100,
    "/html": b"<!doctype html><html><head></head><body>" + b"x" * 100 + b"</body></html>",
    "/small": b"\x89PNG tiny",
    "/exact64": b"\x89PNG" + b"\x05" * 60,
    "/just63": b"\x89PNG" + b"\x05" * 59,
    "/big": b"\x89PNG" + b"\x06" * (600 * 1024),
    "/empty": b"",
    "/utf8": "<svg>☃ élan 日本</svg>".encode() + b" " * 64,
}

_CHUNKED = b"".join(b"%x;ext\r\n%s\r\n" % (len(PNG[i:i + 100]), PNG[i:i + 100]) for i in range(0, len(PNG), 100))
RAW = {
    "/chunked": b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n" + _CHUNKED + b"0\r\nX-Trailer: 1\r\n\r\n",
    "/chunked-trunc": b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n" + _CHUNKED[:150],
    "/cl-bad": b"HTTP/1.1 200 OK\r\nContent-Length: 12abc\r\n\r\n" + PNG,
    "/http10": b"HTTP/1.0 200 OK\r\n\r\n" + PNG,
    "/folded": b"HTTP/1.1 200 OK\r\nX-A: 1\r\n  continued\r\nContent-Length:\r\n 80\r\n\r\n" + PNG,
    "/redir-rel": b"HTTP/1.1 302 Found\r\nLocation: ../png?x=1 2\r\nContent-Length: 0\r\n\r\n",
    "/redir-uri": b"HTTP/1.1 301 Moved\r\nURI: /png\r\nContent-Length: 0\r\n\r\n",
    "/redir-none": b"HTTP/1.1 302 Found\r\nContent-Length: 0\r\n\r\n",
    "/redir-mailto": b"HTTP/1.1 302 Found\r\nLocation: mailto:x@y.z\r\nContent-Length: 0\r\n\r\n",
    "/redir-body-short": b"HTTP/1.1 302 Found\r\nLocation: /png\r\nContent-Length: 50\r\n\r\nabc",
    "/redir-305": b"HTTP/1.1 305 Use Proxy\r\nLocation: /png\r\nContent-Length: 0\r\n\r\n",
    "/continue": b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 520\r\n\r\n" + PNG,
    "/badstatus": b"HTCPCP/1.0 200 OK\r\n\r\n" + PNG,
    "/http2": b"HTTP/2 200 OK\r\n\r\n" + PNG,
    "/nolinebreak": b"HTTP/1.1 200 OK\r\nContent-Length: 80",
}


class _Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def do_GET(self):
        path = self.path.split("?")[0]
        if path == "/gzip":
            # A server that compresses whatever the client asked for.
            import gzip
            body = gzip.compress(PNG)
            self.send_response(200)
            self.send_header("Content-Encoding", "gzip")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        if path == "/trunc":
            # Content-Length promises more than is sent before the close.
            self.send_response(200)
            self.send_header("Content-Length", "5000")
            self.end_headers()
            self.wfile.write(PNG[:300])
            return
        if path == "/trickle":
            # Every read is quick, the whole body takes longer than the timeout.
            import time as _t
            body = PNG[:100]
            self.send_response(200)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            for i in range(0, len(body), 7):
                self.wfile.write(body[i:i + 7])
                self.wfile.flush()
                _t.sleep(1)
            return
        raw = path[len("/sub"):] if path.startswith("/sub/") else path
        if raw in RAW:
            # Written byte for byte: framings BaseHTTPRequestHandler won't make.
            self.close_connection = True
            self.wfile.write(RAW[raw])
            self.wfile.flush()
            return
        if path.startswith("/chain/"):
            n = int(path.rsplit("/", 1)[1])
            self.send_response(302)
            self.send_header("Location", "/png" if n <= 0 else f"/chain/{n - 1}")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        if path == "/redir":
            self.send_response(302)
            self.send_header("Location", "/png")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        if path == "/ua":
            body = (self.headers.get("User-Agent", "") + " " * 80).encode()
            self.send_response(200)
        elif path == "/204":
            self.send_response(204)
            self.end_headers()
            return
        elif path == "/500":
            body = PNG
            self.send_response(500)
        elif path == "/203":
            body = PNG
            self.send_response(203)
        elif path in BODIES:
            body = BODIES[path]
            self.send_response(200)
        else:
            body = b"not found" + b" " * 100
            self.send_response(404)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


_server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), _Handler)
threading.Thread(target=_server.serve_forever, daemon=True).start()
BASE = f"http://127.0.0.1:{_server.server_address[1]}"


def _dirs():
    global _n
    _n += 1
    d = _ROOT / str(_n)
    d.mkdir()
    return str(d / "cache"), str(d / "state")


def _prepare(cache_home, setup, state_home, settings):
    shutil.rmtree(cache_home, ignore_errors=True)
    root = Path(cache_home)
    root.mkdir(parents=True, exist_ok=True)
    for e in setup:
        p = root / e["path"]
        p.parent.mkdir(parents=True, exist_ok=True)
        if "bytes" in e:
            p.write_bytes(bytes(e["bytes"]))
        elif "text" in e:
            p.write_text(e["text"])
        elif "dir" in e:
            p.mkdir(parents=True, exist_ok=True)
        elif "symlink" in e:
            os.symlink(e["symlink"], p)
    if state_home is not None:
        p = Path(state_home) / "almanac" / "settings.json"
        if p.exists():
            p.unlink()
        if settings is not None:
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(settings)


def _after(cache_home, key):
    root = Path(cache_home)
    files = []

    def walk(d):
        try:
            entries = list(os.scandir(d))
        except OSError:
            return
        for e in entries:
            if e.is_dir(follow_symlinks=False):
                walk(e.path)
            else:
                rel = os.path.relpath(e.path, root)
                # The misses file's size depends on the time stamp's digits.
                size = None if rel.endswith("icon-misses.json") else os.lstat(e.path).st_size
                files.append([rel, size])
    walk(root)
    files.sort(key=lambda f: f[0])
    m = root / "almanac" / "icon-misses.json"
    try:
        text = m.read_text()
    except (OSError, ValueError):
        text = None
    if text is not None:
        import json
        needle = json.dumps(key) + ": "
        i = text.find(needle)
        if i >= 0:
            start = i + len(needle)
            end = start
            while end < len(text) and (text[end].isascii() and text[end].isdigit() or text[end] in ".eE-+"):
                end += 1
            text = text[:start] + "T" + text[end:]
    return {"files": files, "misses": text}


def _case(func, fn, key="", setup=(), settings=None, state=True, **kw):
    """One case in its own cache (and state) dir; fn runs with them in place."""
    cache_home, state_home = _dirs()
    args = {"cache_home": cache_home, "setup": list(setup), "key": key, **kw}
    if state:
        args["state_home"] = state_home
        args["settings"] = settings
    else:
        state_home = None

    def run():
        old = {v: os.environ.get(v) for v in ("XDG_CACHE_HOME", "XDG_STATE_HOME")}
        saved = (icons.CACHE, icons.ICONS, icons.MISSES)
        _prepare(cache_home, list(setup), state_home, settings)
        os.environ["XDG_CACHE_HOME"] = cache_home
        if state_home is not None:
            os.environ["XDG_STATE_HOME"] = state_home
        icons.CACHE = Path(cache_home) / "almanac"
        icons.ICONS = icons.CACHE / "icons"
        icons.MISSES = icons.CACHE / "icon-misses.json"
        try:
            return fn(cache_home)
        finally:
            icons.CACHE, icons.ICONS, icons.MISSES = saved
            for v, x in old.items():
                if x is None:
                    os.environ.pop(v, None)
                else:
                    os.environ[v] = x
    return func, args, run


def _fetch_case(key, url="", domain="", **kw):
    def fn(cache_home):
        r = icons.fetch(key, url, domain)
        return {"result": r, "after": _after(cache_home, key)}
    return _case("fetch", fn, key=key, url=url, domain=domain, **kw)


def _miss(key, text):
    return {"path": "almanac/icon-misses.json", "text": text}


def _icon(name, data):
    return {"path": f"almanac/icons/{name}", "bytes": list(data)}


OFF = '{"net_vendor_icons": false}'
ON = '{"net_vendor_icons": true}'

HOSTS = ["mcp.stripe.com", "stripe.com", "com", "", ".", "..", "a..b", "www.bbc.co.uk",
         "x.y.ac.jp", "a.gov.uk", "co.uk", "foo.co", "a.b.c.d.e", ".leading.dot.org",
         "trailing.dot.net.", "ÜNÏ.例え.テスト", "xn--n3h.com", "192.168.1.1",
         "localhost", "a.com.org", "mcp.notion.com", " sp ace .com", "a.b.CO.uk"]

URLS = ["https://mcp.stripe.com/v1", "http://a.b:8080/x", "HTTPS://upper.com/",
        "ftp://files.example.org", "https://", "https:///path", "mailto:x@y.z",
        "", "stripe.com", "https://user@host.com/", "https://[::1]:80/",
        "https://例え.jp/パス", "git+ssh://x.org/y", "https://a.b\n/c", "ws://sock.io:1",
        "https://h.com?q=1", "https://h.com#frag", "https://h.com/"]

KEYS = ["stripe.com", "gh-anthropics", "a b/c\\d", "ünï cödé", "日本", "", ".", "..",
        "-_.", "key*?[x]", "\x00nul", "tab\tkey", "emoji😀"]


def cases():
    yield from _review_cases()
    yield from _review2_cases()
    # Constants and paths (the real environment, read only).
    for name in ("CACHE", "ICONS", "MISSES", "MISS_TTL", "UA", "PATHS"):
        yield name, {}, (lambda name=name: getattr(icons, name))

    for h in HOSTS:
        yield "registrable", {"host": h}, (lambda h=h: icons.registrable(h))
    for u in URLS:
        yield "host_of", {"url": u}, (lambda u=u: icons.host_of(u))
        h = icons.host_of(u)
        yield "registrable", {"host": h}, (lambda h=h: icons.registrable(h))
    for o in ["anthropics", "", "a b", "ünï", "org/repo", "Basecamp"]:
        yield "github_avatar", {"owner": o}, (lambda o=o: icons.github_avatar(o))
    for k in KEYS:
        yield "_safe", {"key": k}, (lambda k=k: icons._safe(k))

    datas = list(BODIES.values()) + [b"", b"\x89", b"\x89PN", b"\x00\x00\x01", b"\xff\xd8",
                                     b"\xff\xd8\xff", b"<sv", b"   <svg", b"\x0b<svg", b"\x85<svg",
                                     b"\xa0<svg", b"<SVG", b"RIFF", b"\x00\x00\x02\x00xxxx"]
    for d in datas:
        if len(d) < 4096:
            yield "_kind", {"data": list(d)}, (lambda d=d: icons._kind(d))

    # The real machine: what is cached, and the misses file, read only.
    real = icons.ICONS
    keys = sorted({p.name.rsplit(".", 1)[0] for p in real.glob("*")})[:40] if real.is_dir() else []
    for k in keys + ["github.com", "nothing-here", "", "gh-"]:
        yield "cached", {"key": k}, (lambda k=k: icons.cached(k))
    yield "_load_misses", {}, icons._load_misses

    # cached() against a laid-out cache.
    layouts = [
        [],
        [_icon("a_b.png", b"")],
        [_icon("a_b.png", b""), _icon("a_b.ico", b"x")],
        [_icon("a_b", b"x")],                         # no dot: not a match
        [_icon("a_b.", b"x")],                        # empty suffix matches
        [_icon("a_b.tar.gz", b"x")],
        [_icon("A_B.png", b"x")],                     # case-sensitive
        [{"path": "almanac/icons/a_b.d", "dir": True}],
        [{"path": "almanac/icons/a_b.lnk", "symlink": "/nonexistent/target"}],
        [{"path": "almanac/icons/a_b.lnk", "symlink": "../../a_b.real"}, {"path": "a_b.real", "text": "data"}],
        [{"path": "almanac/icons", "text": "not a dir"}],
    ]
    for setup in layouts:
        for k in ("a b", "a_b", "aéb", "a/b", ""):
            yield _case("cached", lambda c, k=k: icons.cached(k), key=k, setup=setup, state=False)
    for k in KEYS:
        if k:
            setup = [_icon(icons._safe(k) + ".svg", b"<svg/>")]
            yield _case("cached", lambda c, k=k: icons.cached(k), key=k, setup=setup, state=False)

    # _load_misses on files of every shape.
    miss_files = ["", "{}", "[]", "null", "3", '"s"', '{"a": 1}', '{"a": 1.5, "b": "x"}',
                  "{broken", '{"a": NaN}', '{"a": 1}\n', "﻿{}", '{"a": 1, "a": 2}',
                  '{"ü": [1, {"x": null}]}', '{"a": 1e5, "b": -0.0, "c": 12345678901234567}',
                  '{"a": 1792209534.0209115, "b": 0.1, "c": -5, "d": 18446744073709551615}',
                  '{"a": "NaN", "b": "-1", "c": "\\"2\\""}', '{"a": 01}', '{"a": +1}', '{"a": .5}']
    for t in miss_files:
        if "NaN" in t:
            continue                     # NaN is not JSON; covered through fetch below
        yield _case("_load_misses", lambda c: icons._load_misses(), setup=[_miss("", t)], state=False)
    yield _case("_load_misses", lambda c: icons._load_misses(), state=False)
    yield _case("_load_misses", lambda c: icons._load_misses(),
                setup=[{"path": "almanac/icon-misses.json", "dir": True}], state=False)
    yield _case("_load_misses", lambda c: icons._load_misses(),
                setup=[{"path": "almanac/icon-misses.json", "bytes": list(b'{"a": "\xff"}')}], state=False)

    # _save_miss: the file written back (the fresh time stamp hidden).
    for t in [None, "{}", '{"other": 1792209534.0209115, "x": 5}', '{"k": 0}',
              '{"a": NaN, "b": Infinity, "c": -Infinity}', '{"ü日": 1.0, "e": 1e20}',
              "[1]", "null", '"s"', "{broken", '{"k": "old", "z": [1, {"y": null}]}',
              '{"big": 123456789012345678901234567890, "f": 1e400, "n": -0, "z": 0.1, "m": -1E-7}',
              '{"a": [1.5e+3, 2.50, 3E2], "s": "1e5 NaN \\" 7"}', '{"a": 01}', '{"a": 1.}', '{"a": -}']:
        for k in ("k", "ünï 😀", 'q"uote'):
            setup = [] if t is None else [_miss("", t)]
            yield _case("_save_miss", lambda c, k=k: (icons._save_miss(k), _after(c, k))[1],
                        key=k, setup=setup, state=False)
    # The cache directory cannot be made: the miss is silently not saved.
    yield _case("_save_miss", lambda c: (icons._save_miss("k"), _after(c, "k"))[1], key="k",
                setup=[{"path": "almanac", "text": "a file"}], state=False)

    # _fetch against the local server.
    for p in ["/png", "/ico", "/svg-ws", "/small", "/empty", "/utf8", "/ua", "/204", "/203",
              "/redir", "/404", "/500", "/html"]:
        url = BASE + p
        yield "_fetch", {"url": url}, (lambda url=url: list(icons._fetch(url)))
    for url in ["not a url", "", "http://127.0.0.1:1/x", "https://nothing.invalid/favicon.ico"]:
        yield "_fetch", {"url": url}, (lambda url=url: list(icons._fetch(url)))

    # fetch: every branch.
    yield _fetch_case("", url=BASE + "/png")
    yield _fetch_case("k", url=BASE + "/png", settings=OFF)
    yield _fetch_case("k", url=BASE + "/png", settings=None)            # default is on
    yield _fetch_case("k", url=BASE + "/png", settings='{"net_vendor_icons": "nope"}')
    yield _fetch_case("k", url=BASE + "/png", settings="{broken")
    yield _fetch_case("k", url=BASE + "/png", settings=OFF, setup=[_icon("k.ico", b"x")])
    yield _fetch_case("k", url=BASE + "/png", settings=ON, setup=[_icon("k.ico", b"x")])
    yield _fetch_case("k", url=BASE + "/gif", settings=ON, setup=[_icon("k.ico", b"")])
    yield _fetch_case("k", url=BASE + "/png", settings=ON,
                      setup=[{"path": "almanac/icons/k.lnk", "symlink": "/nonexistent"}])
    for p in ["/png", "/ico", "/gif", "/jpg", "/svg", "/svg-ws", "/svg-late", "/xml", "/webp",
              "/html", "/small", "/exact64", "/just63", "/big", "/empty", "/utf8", "/redir",
              "/404", "/500", "/204", "/203", "/ua"]:
        yield _fetch_case("k", url=BASE + p, settings=ON)
    for k in KEYS:
        if k:
            yield _fetch_case(k, url=BASE + "/svg", settings=ON)
    # Misses: recent, old, and every malformed shape.
    for t in ['{"k": 9999999999}', '{"k": 0}', '{"k": 1}', '{"k": true}', '{"k": false}',
              '{"k": NaN}', '{"k": Infinity}', '{"k": -Infinity}', '{"k": "1"}', '{"k": null}',
              '{"k": [1]}', '{"other": 9999999999}', "[]", "null", "3", '"s"', "{broken", "",
              '{"k": 1e300}', '{"k": -1e300}']:
        yield _fetch_case("k", url=BASE + "/png", settings=ON, setup=[_miss("", t)])
        yield _fetch_case("k", url=BASE + "/404", settings=ON, setup=[_miss("", t)])
    # Nothing usable: every candidate fails and the miss is remembered.
    for url, domain in [("", "nothing.invalid"), ("", "mcp.nothing.invalid"), ("", "localhost"),
                        (BASE + "/404", "a.b.nothing.invalid"), ("not a url", ""),
                        ("file:///etc/hostname", ""), ("", ""), (BASE + "/html", "")]:
        yield _fetch_case("k", url=url, domain=domain, settings=ON)
    # An explicit url wins over the domain.
    yield _fetch_case("k", url=BASE + "/ico", domain="nothing.invalid", settings=ON)
    # The icon cannot be saved: None, and no miss recorded.
    yield _fetch_case("k", url=BASE + "/png", settings=ON,
                      setup=[{"path": "almanac/icons", "text": "not a dir"}])
    yield _fetch_case("k", url=BASE + "/png", settings=ON,
                      setup=[{"path": "almanac/icons/k.png", "dir": True},
                             {"path": "almanac/icons/k.png/x", "text": "x"}])
    # A stale .tmp left behind is overwritten.
    yield _fetch_case("k", url=BASE + "/png", settings=ON, setup=[_icon("k.png.tmp", b"old")])


# ---------------------------------------------------------------- review round 1


def _case_at(func, fn, cache_home, key="", setup=(), settings=None, **kw):
    """`_case`, with a cache_home spelled as given (not normalised)."""
    _, state_home = _dirs()
    args = {"cache_home": cache_home, "setup": list(setup), "key": key,
            "state_home": state_home, "settings": settings, **kw}

    def run():
        old = {v: os.environ.get(v) for v in ("XDG_CACHE_HOME", "XDG_STATE_HOME")}
        saved = (icons.CACHE, icons.ICONS, icons.MISSES)
        _prepare(cache_home, list(setup), state_home, settings)
        os.environ["XDG_CACHE_HOME"] = cache_home
        os.environ["XDG_STATE_HOME"] = state_home
        icons.CACHE = Path(cache_home) / "almanac"
        icons.ICONS = icons.CACHE / "icons"
        icons.MISSES = icons.CACHE / "icon-misses.json"
        try:
            return fn(cache_home)
        finally:
            icons.CACHE, icons.ICONS, icons.MISSES = saved
            for v, x in old.items():
                if x is None:
                    os.environ.pop(v, None)
                else:
                    os.environ[v] = x
    return func, args, run


def _review_cases():
    # Ints past float range: Python's `time.time() - int` raises OverflowError.
    big = "1" + "0" * 400
    for t in ['{"k": %s}' % big, '{"k": -%s}' % big]:
        yield _fetch_case("k", url=BASE + "/png", settings=ON, setup=[_miss("", t)])
    # Valid for Python's json, not for serde: a lone surrogate, deep nesting.
    yield _fetch_case("k", url=BASE + "/png", settings=ON,
                      setup=[_miss("", '{"\\ud800": 1, "k": 9999999999}')])
    yield _fetch_case("k", url=BASE + "/png", settings=ON,
                      setup=[_miss("", '{"k": 9999999999, "z": ' + "[" * 200 + "]" * 200 + "}")])
    yield _case("_load_misses", lambda c: icons._load_misses(), state=False,
                setup=[_miss("", '{"z": ' + "[" * 200 + "]" * 200 + "}")])
    # Not JSON for Python (a number as a key).
    yield _fetch_case("k", url=BASE + "/png", settings=ON,
                      setup=[_miss("", '{1: 5, "k": 9999999999}')])
    yield _case("_load_misses", lambda c: icons._load_misses(), state=False,
                setup=[_miss("", '{1: 5}')])
    # A string that looks like the Rust side's number marker.
    yield _fetch_case("k", url=BASE + "/png", settings=ON,
                      setup=[_miss("", '{"k": "\\u0000num:0"}')])
    yield _case("_load_misses", lambda c: icons._load_misses(), state=False,
                setup=[_miss("", '{"z": "\\u0000num:7"}')])
    yield _case("_save_miss", lambda c: (icons._save_miss("k"), _after(c, "k"))[1], key="k",
                setup=[_miss("", '{"z": "\\u0000num:abc"}')], state=False)

    # The transfer: urllib vs ureq.
    for p in ["/gzip", "/trunc", "/chain/9", "/chain/10", "/chain/11", "/png#frag", "/trickle"]:
        url = BASE + p
        yield "_fetch", {"url": url}, (lambda url=url: list(icons._fetch(url)))
    for url in [BASE.replace("http", "HTTP") + "/png", BASE.replace("://", "://user:pw@") + "/png",
                "file:///etc/hostname", "data:image/png;base64,iVBORw0KGgo="]:
        yield "_fetch", {"url": url}, (lambda url=url: list(icons._fetch(url)))
    for p in ["/gzip", "/trunc", "/chain/10", "/chain/11", "/png#frag"] + sorted(RAW):
        yield _fetch_case("k", url=BASE + p, settings=ON)
    for p in sorted(RAW):
        url = BASE + "/sub" + p if p == "/redir-rel" else BASE + p
        yield "_fetch", {"url": url}, (lambda url=url: list(icons._fetch(url)))
    for url in ["file://localhost/etc/hostname", "file:/etc/hostname", "file:///etc", "file:///etc/hostname?q#f",
                "file://nothing.invalid/etc/hostname", "data:,abc", "data:;base64,abc", "data:text/plain",
                "<URL:" + BASE + "/png>", "  " + BASE + "/png  ", BASE + "/p ng", BASE + "/p\u00e9", "http://[::1/x",
                "http:///x", "http://:80/x", BASE + ":/png", BASE.replace("127.0.0.1", "127.0.0.1\u00e9") + "/png"]:
        yield "_fetch", {"url": url}, (lambda url=url: list(icons._fetch(url)))
    yield _fetch_case("k", url=BASE.replace("http", "HTTP") + "/png", settings=ON)

    # A non-ASCII host: urllib IDNA-encodes it (fullwidth letters fold to ASCII).
    url = BASE.replace("127.0.0.1", "\uff4c\uff4f\uff43\uff41\uff4c\uff48\uff4f\uff53\uff54") + "/png"
    yield "_fetch", {"url": url}, (lambda url=url: list(icons._fetch(url)))
    yield _fetch_case("k", url=url, settings=ON)

    # A file name that is not UTF-8 in the icons directory.
    raw = _ROOT / "nonutf8"
    raw.mkdir(exist_ok=True)
    with open(os.path.join(os.fsencode(raw), b"a_b.\xff"), "wb") as f:
        f.write(b"\x89PNG" + b"x" * 80)
    # JSON cannot carry the surrogate-escaped name, so both sides write the
    # path with U+FFFD for the byte that is not UTF-8.
    def lossy(p):
        return None if p is None else os.fsencode(p).decode("utf-8", "replace")
    yield _case("cached", lambda c: lossy(icons.cached("a b")), key="a b", state=False,
                setup=[{"path": "almanac/icons", "symlink": str(raw)}])

    def fetch_lossy(c):
        return {"result": lossy(icons.fetch("a b", BASE + "/png", "")), "after": _after(c, "a b")}
    yield _case("fetch", fetch_lossy, key="a b", url=BASE + "/png", domain="", settings=ON,
                setup=[{"path": "almanac/icons", "symlink": str(raw)}])

    # XDG_CACHE_HOME spelled with // and /./: pathlib normalises, PathBuf keeps it.
    for spell in ("//", "/./"):
        d = str(_ROOT / f"spell{len(spell)}")
        home = d + spell + "cache"
        yield _case_at("cached", lambda c: icons.cached("k"), home, key="k",
                       setup=[_icon("k.png", PNG)])
        yield _case_at("fetch", lambda c: {"result": icons.fetch("k", BASE + "/png"), "after": _after(c, "k")}, home, key="k",
                       settings=ON, url=BASE + "/png")


# ---------------------------------------------------------------- review round 2

import json as _json
import ssl as _ssl
import subprocess as _sp

_TLS = _ROOT / "tls"


def _tls_setup():
    """A private CA (trusted through SSL_CERT_FILE, on both sides) and leaf
    certificates of several shapes, made with the openssl CLI."""
    _TLS.mkdir(exist_ok=True)
    o = ["openssl"]
    q = {"stdout": _sp.DEVNULL, "stderr": _sp.DEVNULL, "check": True, "cwd": _TLS}
    _sp.run(o + ["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", "ca.key", "-out", "ca.pem",
                 "-days", "30", "-subj", "/CN=Parity CA", "-addext", "basicConstraints=critical,CA:TRUE",
                 "-addext", "keyUsage=critical,keyCertSign,cRLSign", "-addext", "subjectKeyIdentifier=hash"], **q)
    exts = {
        # A positive control: SAN for localhost and 127.0.0.1.
        "good": "subjectAltName=DNS:localhost,IP:127.0.0.1\nauthorityKeyIdentifier=keyid\n"
                "basicConstraints=CA:FALSE\nkeyUsage=digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n",
        # No subjectAltName: OpenSSL (hostname_checks_common_name) falls back to the CN.
        "cn": "authorityKeyIdentifier=keyid\nbasicConstraints=CA:FALSE\n",
    }
    for name, ext in exts.items():
        (_TLS / f"{name}.ext").write_text(ext)
        _sp.run(o + ["req", "-newkey", "rsa:2048", "-nodes", "-keyout", f"{name}.key", "-out", f"{name}.csr",
                     "-subj", "/CN=localhost"], **q)
        _sp.run(o + ["x509", "-req", "-in", f"{name}.csr", "-CA", "ca.pem", "-CAkey", "ca.key",
                     "-CAcreateserial", "-out", f"{name}.pem", "-days", "10", "-extfile", f"{name}.ext"], **q)
    os.environ["SSL_CERT_FILE"] = str(_TLS / "ca.pem")


def _tls_server(cert, tls12_ciphers=None):
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), _Handler)
    ctx = _ssl.SSLContext(_ssl.PROTOCOL_TLS_SERVER)
    ctx.load_cert_chain(str(_TLS / f"{cert}.pem"), str(_TLS / f"{cert}.key"))
    if tls12_ciphers:
        ctx.maximum_version = _ssl.TLSVersion.TLSv1_2
        ctx.set_ciphers(tls12_ciphers)
    srv.socket = ctx.wrap_socket(srv.socket, server_side=True)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv.server_address[1]


# OpenSSL reads SSL_CERT_FILE once per process, at the first default
# context. When the whole suite runs in one process, earlier modules have
# already made HTTPS contexts, so the CA set by _tls_setup would never reach
# the Python side (while the Rust binary, a fresh process per module, sees
# it). So the Python side of the TLS cases runs in a fresh interpreter too,
# with the environment the case has in place (SSL_CERT_FILE, XDG dirs, demo).
_CHILD = r"""
import json, sys
from pathlib import Path
a = json.loads(sys.argv[1])
sys.path.insert(0, a["root"])
import icons
if a["cache"]:
    icons.CACHE = Path(a["cache"]) / "almanac"
    icons.ICONS = icons.CACHE / "icons"
    icons.MISSES = icons.CACHE / "icon-misses.json"
try:
    if a["func"] == "_fetch":
        r = list(icons._fetch(a["url"]))
    else:
        r = icons.fetch(a["key"], a["url"])
        r = None if r is None else str(r)
    out = {"ok": r}
except BaseException as e:
    out = {"raise": f"{type(e).__name__}: {e}"}
sys.stdout.write(json.dumps(out))
"""


def _fresh(func, url, key="", cache=None):
    import sys
    args = {"root": str(Path(icons.__file__).resolve().parent), "func": func, "url": url,
            "key": key, "cache": cache}
    out = _sp.run([sys.executable, "-c", _CHILD, _json.dumps(args)], capture_output=True,
                  text=True, env=dict(os.environ), timeout=120)
    r = _json.loads(out.stdout)
    if "raise" in r:
        raise RuntimeError(r["raise"])
    return r["ok"]


def _tls_fetch_case(key, url, **kw):
    def fn(cache_home):
        r = _fresh("fetch", url, key=key, cache=cache_home)
        return {"result": r, "after": _after(cache_home, key)}
    return _case("fetch", fn, key=key, url=url, domain="", **kw)


def _review2_cases():
    # -- TLS: OpenSSL (urllib's default context) vs rustls.
    try:
        _tls_setup()
        servers = {"good": _tls_server("good"), "cn": _tls_server("cn"),
                   "cbc": _tls_server("good", "ECDHE-RSA-AES128-SHA256"),
                   "cbc384": _tls_server("good", "ECDHE-RSA-AES256-SHA384")}
    except (OSError, _sp.CalledProcessError, _ssl.SSLError):
        servers = {}
    for name, port in servers.items():
        for host in ("localhost", "127.0.0.1", "localhost."):
            url = f"https://{host}:{port}/png"
            yield "_fetch", {"url": url}, (lambda url=url: _fresh("_fetch", url))
            yield _tls_fetch_case("k", url=url, settings=ON)
    if "good" in servers:
        # A close with no close_notify (Python's server never sends one),
        # mid-body and at the end of an unframed body; and other framings.
        for p in ["/trunc", "/http10", "/chunked", "/chunked-trunc", "/redir", "/gzip", "/nolinebreak"]:
            url = f"https://localhost:{servers['good']}{p}"
            yield "_fetch", {"url": url}, (lambda url=url: _fresh("_fetch", url))
        url = f"https://LOCALHOST:{servers['good']}/png"
        yield "_fetch", {"url": url}, (lambda url=url: _fresh("_fetch", url))

    # -- json nesting. Python's limit is ~52,105 containers but moves by
    # +-20 from run to run (C stack placement): 52,103 deep loads in one run
    # and raises in the next. So the cases sit clear of that band on both
    # sides; inside it the Python answer is not reproducible.
    for n in (52_000, 52_250):
        t = '{"k": 9999999999, "z": ' + "[" * n + "]" * n + "}"
        yield _fetch_case("k", url=BASE + "/png", settings=ON, setup=[_miss("", t)])
    for n in (52_000, 52_250):
        t = '{"z": ' + "[" * n + "]" * n + "}"
        yield _case("_save_miss", lambda c: (icons._save_miss("k"), _after(c, "k"))[1], key="k",
                    setup=[_miss("", t)], state=False)
    # Nested dicts load at one level each but cost ~1.8 to dump (limit
    # ~28,950): these load fine, then the first dumps and the second raises.
    for n in (28_500, 29_500):
        t = '{"z": ' + '{"a": ' * n + "1" + "}" * n + "}"
        yield _case("_save_miss", lambda c: (icons._save_miss("k"), _after(c, "k"))[1], key="k",
                    setup=[_miss("", t)], state=False)

    # -- settings: a RecursionError from settings.get propagates out of fetch.
    deep = '{"x": ' + "[" * 60_000 + "]" * 60_000 + "}"
    yield _fetch_case("k", url=BASE + "/png", settings=deep)
