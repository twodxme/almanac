"""Parity cases for themeart.

Everything that writes goes under a private XDG_CACHE_HOME set here, before
themeart is imported (its CACHE is read at import time) and inherited by the
Rust process; files a Python case writes are removed again so the Rust side
draws its own.
"""
import hashlib
import os
import tempfile
from pathlib import Path

_TMP = Path(tempfile.mkdtemp(prefix="almanac-parity-themeart-"))
os.environ["XDG_CACHE_HOME"] = str(_TMP / "cache")

import themeart  # noqa: E402

themeart.CACHE = _TMP / "cache" / "almanac"
themeart.ART = themeart.CACHE / "themeart"


def _pixels(path):
    """sha1 of the pixels as cairo holds them, not of the encoded file."""
    import cairo
    try:
        s = cairo.ImageSurface.create_from_png(str(path))
    except Exception:
        return None
    data = bytes(s.get_data())
    w, h, stride = s.get_width(), s.get_height(), s.get_stride()
    rows = b"".join(data[y * stride:y * stride + w * 4] for y in range(h))
    return {"size": [w, h], "sha1": hashlib.sha1(rows).hexdigest()}


def _result(p, cleanup=True):
    if p is None:
        return None
    try:
        file_sha1 = hashlib.sha1(Path(p).read_bytes()).hexdigest()
    except OSError:
        file_sha1 = None
    out = {"path": str(p), "pixels": _pixels(p), "file_sha1": file_sha1}
    if cleanup:
        Path(p).unlink(missing_ok=True)
    return out


def _render(palette, dest):
    return _result(themeart.render(palette, Path(dest)))


def _thumb(repo, cleanup=True):
    return _result(themeart.thumbnail(repo), cleanup)


def _py_fetch(repo, env):
    """fetch_palette in a fresh interpreter with these variables set."""
    import json, subprocess, sys
    code = "import json, sys, themeart; print(json.dumps(themeart.fetch_palette(sys.argv[1])))"
    out = subprocess.run([sys.executable, "-c", code, repo], capture_output=True, text=True,
                         env={**os.environ, **env}, cwd=Path(themeart.__file__).parent, timeout=120)
    return json.loads(out.stdout)


def _proxy(auth=None):
    """A tiny CONNECT proxy on localhost, alive for the whole run (the Rust
    side uses it too). With auth ("user:password") it wants Basic auth."""
    import base64, socket, threading
    srv = socket.socket()
    srv.bind(("127.0.0.1", 0))
    srv.listen(16)
    want = "Basic " + base64.b64encode(auth.encode()).decode() if auth else None

    def pipe(a, b):
        try:
            while d := a.recv(65536):
                b.sendall(d)
        except OSError:
            pass
        finally:
            try:
                b.shutdown(socket.SHUT_WR)
            except OSError:
                pass

    def handle(c):
        try:
            data = b""
            while b"\r\n\r\n" not in data:
                chunk = c.recv(4096)
                if not chunk:
                    return
                data += chunk
            head = data.split(b"\r\n\r\n", 1)[0].decode("latin-1").split("\r\n")
            method, target, _ = head[0].split(" ", 2)
            hdrs = {k.lower(): v for k, _, v in (h.partition(": ") for h in head[1:])}
            if method != "CONNECT":
                c.sendall(b"HTTP/1.1 405 Method Not Allowed\r\n\r\n")
                return
            if want and hdrs.get("proxy-authorization") != want:
                c.sendall(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
                return
            host, _, p = target.rpartition(":")
            up = socket.create_connection((host, int(p)), timeout=20)
            c.sendall(b"HTTP/1.1 200 Connection established\r\n\r\n")
            t = threading.Thread(target=pipe, args=(up, c), daemon=True)
            t.start()
            pipe(c, up)
            t.join()
        except Exception:
            pass
        finally:
            c.close()

    def serve():
        while True:
            c, _ = srv.accept()
            threading.Thread(target=handle, args=(c,), daemon=True).start()

    threading.Thread(target=serve, daemon=True).start()
    return srv.getsockname()[1]


APP_RECURSION_LIMIT = 1000  # sys.getrecursionlimit() in the app


def _toml_ok(text, frames_above=None):
    """Whether tomllib takes the document. With frames_above, tomllib.loads
    is called with exactly that many Python frames above it."""
    import sys
    import tomllib
    import tomllib._parser as tp

    def call():
        # The app runs with Python's default recursion limit (1000), and
        # tomllib sized MAX_KEY_PARTS from it at import. Other case modules
        # raise the limit for the whole runner (cases_ghcache sets 20000 at
        # import), so pin both to the app's values for this call.
        old_limit, old_parts = sys.getrecursionlimit(), tp.MAX_KEY_PARTS
        sys.setrecursionlimit(APP_RECURSION_LIMIT)
        tp.MAX_KEY_PARTS = APP_RECURSION_LIMIT
        try:
            tomllib.loads(text)
            return True
        except Exception:
            return False
        finally:
            sys.setrecursionlimit(old_limit)
            tp.MAX_KEY_PARTS = old_parts
    if frames_above is None:
        return call()
    depth, f = 0, sys._getframe()
    while f:
        depth, f = depth + 1, f.f_back

    def pad(n):
        return pad(n - 1) if n else call()
    # _toml_ok is frame number `depth`; pad(k) puts pad(0) at depth + 1 + k,
    # call() one deeper and loads one deeper still, so loads has
    # depth + 2 + k frames above it.
    return pad(frames_above - depth - 2)


TOKYO = {
    "accent": "#7aa2f7", "background": "#1a1b26", "foreground": "#a9b1d6",
    "red": "#f7768e", "green": "#9ece6a", "yellow": "#e0af68", "blue": "#7aa2f7",
    "magenta": "#bb9af7", "cyan": "#7dcfff", "mode": "dark",
}
REPOS = [
    "https://github.com/a/b", "https://github.com/a", "", "github.com/a/b/c",
    "https://github.com/owner/name/tree/main", "https://github.com//a//b",
    "https://github.com/ünï/cödé", "not a url", "https://github.com/a/b.git",
]


def cases():
    for r in REPOS:
        yield "palette_url", {"repo": r}, lambda r=r: themeart.palette_url(r)
        yield "art_path", {"repo": r}, lambda r=r: themeart.art_path(r)

    for v in ["#fff", "#FFFFFF", "#123456", "#abc", "#12345", "#1234567", "fff",
              "", "#ggg", "#abcdef\n", " #abc", "#ÿÿÿ", "#٠١٢"]:
        yield "_rgb", {"value": v}, lambda v=v: list(themeart._rgb(v))

    # Network: real themes on GitHub, plus repos that do not exist.
    for r in ["https://github.com/bjarneo/omarchy-ash-theme", "https://github.com/bjarneo/omarchy-aura-theme",
              "https://github.com/JaxonWright/omarchy-midnight-theme", "https://github.com/basecamp/omarchy",
              "https://github.com/no-such-owner-xyz/no-such-theme",
              "", "https://github.com/a"]:
        yield "fetch_palette", {"repo": r}, lambda r=r: themeart.fetch_palette(r)

    d = str(_TMP / "render")
    palettes = [
        {},
        TOKYO,
        {"background": "#fff"},
        {"mode": "light"},
        {"foreground": "", "background": "#000000", "red": "#f00"},
        {"dark_background": "#101010", "darker_background": "#050505",
         "lighter_background": "#303030", "muted": "#777", "orange": "#ff8800",
         "accent": "bad", "red": "#zzz"},
        {"background": "#abcdef\n", "blue": "#00f"},
        {"ünïcode": "#123", "background": "#٠١٢"},
    ]
    for i, p in enumerate(palettes):
        dest = f"{d}/p{i}.png"
        yield "render", {"palette": p, "dest": dest}, lambda p=p, dest=dest: _render(p, dest)
    yield "render", {"palette": TOKYO, "dest": f"{d}/deep/er/x"}, lambda: _render(TOKYO, f"{d}/deep/er/x")

    # thumbnail: nothing fetchable, a cached file, an empty cached file, a real theme.
    yield "thumbnail", {"repo": ""}, lambda: _thumb("")
    yield "thumbnail", {"repo": "https://github.com/a"}, lambda: _thumb("https://github.com/a")
    cached = "https://github.com/cached/theme"
    p = themeart.art_path(cached)
    p.parent.mkdir(parents=True, exist_ok=True)
    themeart.render(TOKYO, p)
    yield "thumbnail", {"repo": cached}, lambda: _thumb(cached, cleanup=False)
    empty = "https://github.com/no-such-owner-xyz/no-such-theme"
    themeart.art_path(empty).write_bytes(b"")
    yield "thumbnail", {"repo": empty}, lambda: _thumb(empty, cleanup=False)
    real = "https://github.com/bjarneo/omarchy-aura-theme"
    yield "thumbnail", {"repo": real}, lambda: _thumb(real)

    # Review round 1: divergences found by the adversarial reviewer.
    # "#abc\n" passes _HEX ($ matches before a trailing newline) but leaves
    # h = "abc\n" (len 4), so int(h[4:6], 16) raises in Python.
    for v in ["#abc\n", "#ABC\n"]:
        yield "_rgb", {"value": v}, lambda v=v: list(themeart._rgb(v))
    # The same value outside render's try (base colours) raises; inside it
    # (an ANSI chip) render returns None.
    yield "render", {"palette": {"background": "#abc\n"}, "dest": f"{d}/r1a.png"}, \
        lambda: _render({"background": "#abc\n"}, f"{d}/r1a.png")
    yield "render", {"palette": {"background": "#000", "red": "#abc\n"}, "dest": f"{d}/r1b.png"}, \
        lambda: _render({"background": "#000", "red": "#abc\n"}, f"{d}/r1b.png")
    # Path(dest) normalises: trailing slash, doubled slashes and "." parts.
    for dest in [f"{d}/r1c.png/", f"{d}//r1d.png", f"{d}/./r1e.png", f"{d}/sub/../r1f.png"]:
        yield "render", {"palette": TOKYO, "dest": dest}, lambda dest=dest: _render(TOKYO, dest)
    yield "render", {"palette": TOKYO, "dest": ""}, lambda: _render(TOKYO, "")

    # Review round 2.
    # A cached preview that is a directory: exists() and a non-zero st_size,
    # so thumbnail hands back the directory path without drawing.
    dir_repo = "https://github.com/cached/dir"
    themeart.art_path(dir_repo).mkdir(parents=True, exist_ok=True)
    yield "thumbnail", {"repo": dir_repo}, lambda: {"path": str(themeart.thumbnail(dir_repo)), "pixels": None,
                                                    "file_sha1": None}

    # Review round 3.
    # The fetch reads the environment the way urllib does. Each Python side
    # runs in a fresh interpreter with the variables set (OpenSSL and the
    # proxy handler read them once per process); the Rust side gets the same
    # variables as "_env", laid over its own environment for that call only.
    aura = "https://github.com/bjarneo/omarchy-aura-theme"
    port = _proxy()
    aport = _proxy(auth="u:p@ss")
    envs = [
        # urllib verifies against OpenSSL's default paths: no usable CA, no fetch.
        {"SSL_CERT_FILE": "/dev/null", "SSL_CERT_DIR": "/nonexistent"},
        {"SSL_CERT_FILE": "/dev/null"},
        {"SSL_CERT_DIR": "/nonexistent"},
        {"SSL_CERT_FILE": "/nonexistent", "SSL_CERT_DIR": "/nonexistent:/etc/ssl/certs"},
        # Only https_proxy counts for an https URL; all_proxy never does.
        {"http_proxy": "http://127.0.0.1:9"},
        {"HTTP_PROXY": "http://127.0.0.1:9"},
        {"all_proxy": "http://127.0.0.1:9"},
        {"ALL_PROXY": "http://127.0.0.1:9", "http_proxy": "http://127.0.0.1:9"},
        {"https_proxy": "http://127.0.0.1:9"},
        {"HTTPS_PROXY": "http://127.0.0.1:9"},
        {"HTTPS_PROXY": "http://127.0.0.1:9", "https_proxy": ""},
        {"https_proxy": "http://127.0.0.1:9", "no_proxy": "raw.githubusercontent.com"},
        {"https_proxy": "http://127.0.0.1:9", "NO_PROXY": ".githubusercontent.com"},
        {"https_proxy": "http://127.0.0.1:9", "no_proxy": "*"},
        {"https_proxy": "http://127.0.0.1:9", "no_proxy": "hubusercontent.com"},
        {"https_proxy": "http:/127.0.0.1:9", "no_proxy": "*"},
        {"https_proxy": "http://127.0.0.1:x"},
        # Through a CONNECT proxy, with and without a scheme, with credentials.
        {"https_proxy": f"http://127.0.0.1:{port}"},
        {"https_proxy": f"127.0.0.1:{port}"},
        {"https_proxy": f"socks5://127.0.0.1:{port}/"},
        {"https_proxy": f"http://u:p%40ss@127.0.0.1:{aport}"},
        {"https_proxy": f"http://u:wrong@127.0.0.1:{aport}"},
        {"https_proxy": f"http://u@127.0.0.1:{aport}"},
    ]
    for env in envs:
        yield "fetch_palette", {"repo": aura, "_env": env}, lambda env=env: _py_fetch(aura, env)
    # Python's dict keeps colors.toml's order.
    for r in [aura, "https://github.com/bjarneo/omarchy-ash-theme"]:
        yield "fetch_palette", {"repo": r, "_keys": True}, lambda r=r: list(themeart.fetch_palette(r))

    # 'mode' is lowercased with Python's Unicode 16 tables.
    for s in ["DARK", "Light", "꟎꟒꟔", "\U00016ea0X", "ʕΣ", "꟎Σ",
              "AΣ", "AΣB", "AΣ᫏B", "İ", "ẞΩK", "ΩΣ Σ"]:
        yield "lower", {"s": s}, lambda s=s: s.lower()

    # tomllib, not a TOML library: what it accepts and refuses.
    docs = [
        "x = 99999999999999999999", "x = -9223372036854775809", "x = 0xffffffffffffffffff",
        "x = 1e400", "x = -1e400", "x = 1_000.5e1_0", "x = " + "1" * 4300, "x = " + "1" * 4301,
        "x = " + "1_" * 4300 + "1", "x = +" + "9" * 4300, "x = 0x" + "f" * 5000, "x = 0b2", "x = 01",
        "x = 0000-01-01", "x = 0001-01-01", "x = 1979-05-27T07:32:60Z", "x = 07:32:60",
        "x = 2023-02-29", "x = 2024-02-29", "x = 1900-02-29", "x = 2000-02-29", "x = 2024-04-31",
        "x = 1979-05-27T07:32:00.999999-07:00", "x = 1979-05-27 07:32:00", "x = 1979-05-27T",
        "x = 1979-05-27T25:00:00", "x = 07:32", "x = 07:32:00.1234567", "x = 1979-05-27T07:32:00Z",
        "x = inf\ny = -nan\nz = +inf", "x = true\ny = false", "x = tru",
        "[a]\nb=1\n[a.c]\n[[d]]\n[[d]]\ne.f='x'", "[a]\n[a]", "a={b=1}\n[a.c]", "a.b=1\n[a]",
        "a=1\na=2", "a=[]\n[[a]]", "[[a]]\n[a]", "a=[1]\n[a.b]", "t={a=1,\n b=2}", "t={a=1,}",
        "x='''a\nb'''''", 'x="""a\\\n   b"""', 'x="\\U0001F600"', 'x="\\uD800"', 'x="\\e"',
        "x='a\tb'", "x='a\x7fb'", "# c\x01", "﻿x=1", "x=1\r\ny=2", "x=1\ry=2",
        "a.b.c=1\na.b.d=2", "[x.y]\n[x]\nz=1", "[x]\ny.z=1\n[x.y]",
        'x = [1, [2, {a=3}], "s", # c\n ]', "x = {a.b=1, a.c=2}", "x = {a.b=1, a.b.c=2}",
        "\"k\" = 1\n'k2' = 2\n\"\" = 3", "a . b = 1", "1.2 = 3", "x = 'café'\ny = \"é\\u00e9\"",
    ]
    for doc in docs:
        yield "toml_ok", {"text": doc}, lambda doc=doc: _toml_ok(doc)
    # Python's recursion limit, reached from the same depth the app's
    # thumbnail worker calls tomllib from.
    shapes = [  # (document for nesting depth n, deepest n Python takes)
        (lambda n: "x = " + "[" * n + "]" * n, 492),
        (lambda n: "x = " + "[" * n + "'s'" + "]" * n, 492),
        (lambda n: "x = " + "[" * n + '"""\\u0041"""' + "]" * n, 490),
        (lambda n: "x = " + "{a=" * n + "1" + "}" * n, 328),
        (lambda n: "x = " + "[{a=" * n + "1" + "}]" * n, 196),
        (lambda n: "x = " + "{a.b=" * n + "1" + "}" * n, 328),
        (lambda n: "[t]\nx = " + "[" * n + "]" * n, 492),
        (lambda n: "x = " + "[" * n + "2024-01-01T00:00:00+01:00" + "]" * n, 492),
    ]
    for shape, top in shapes:
        for n in (top - 1, top, top + 1):
            doc = shape(n)
            yield "toml_ok", {"text": doc}, lambda doc=doc: _toml_ok(doc, frames_above=10)
