"""Parity cases for appimage.py.

Nothing here touches the real user's files or the network:
- read_update_info runs objcopy (read-only) on the AppImages on this machine
  and on a few files that are not AppImages;
- latest_github/check get their GitHub answer from a patched
  ghcache.get_json (the Rust side gets the same JSON as `data`), and the URL
  asked for is compared too;
- download fetches from an http.server on 127.0.0.1 into temp dirs, one per
  side, and the resulting files are compared;
- install/remove_desktop_entry run a fake AppImage (a shell script that
  "extracts" a desktop file and icons) with XDG_DATA_HOME pointed at a temp
  dir, again one per side.
"""
import atexit
import fnmatch
import functools
import http.server
import os
import shutil
import stat
import tempfile
import threading
from pathlib import Path
from unittest import mock

import appimage

_TMP = tempfile.mkdtemp(prefix="almanac-parity-appimage-")
atexit.register(shutil.rmtree, _TMP, True)


def _writable():
    # A read-only directory an install left behind must not survive rmtree.
    for root, dirs, _files in os.walk(_TMP):
        for d in dirs:
            try:
                os.chmod(os.path.join(root, d), 0o755)
            except OSError:
                pass


atexit.register(_writable)                              # runs before the rmtree


def _dir(*parts):
    d = Path(_TMP, *parts)
    d.mkdir(parents=True, exist_ok=True)
    return d


def _tree(d):
    out = {}
    for root, _dirs, files in os.walk(d):
        for f in files:
            p = Path(root, f)
            out[str(p.relative_to(d))] = p.read_bytes().decode("utf-8", "replace")
    return dict(sorted(out.items()))


def _with_env(env, fn):
    def run():
        with mock.patch.dict(os.environ, env):
            return fn()
    return run


def _err(fn):
    try:
        return fn()
    except Exception:                                   # noqa: BLE001
        return {"error": True}


# --------------------------------------------------------------------------
# read_update_info

def _read_cases():
    real = [Path.home() / ".local/state/almanac/apps" / n
            for n in ("vectorcraft.AppImage", "wordcraft.AppImage", "vectorcraft.previous")]
    real += list((Path.home() / "Downloads").glob("*.AppImage"))
    text = _dir("read") / "not-elf.AppImage"
    text.write_text("just text\n")
    uni = _dir("read") / "ünï cødé.AppImage"
    uni.write_bytes(b"\x7fELF garbage")
    paths = [str(p) for p in real] + [
        str(text), str(uni), "/usr/bin/ls", "/nonexistent/x.AppImage", "", _TMP]
    for p in paths:
        yield "read_update_info", {"path": p}, functools.partial(appimage.read_update_info, Path(p))


# --------------------------------------------------------------------------
# parse_update_info, _version_of, _vkey, fnmatch

_INFOS = [
    "gh-releases-zsync|storytold|vectorcraft|latest|vectorcraft-*-linux-x86_64.AppImage.zsync",
    "gh-releases-direct|o|r|v1.2|*.AppImage",
    "gh-releases-zsync|o|r|latest",
    "gh-releases-zsync|o|r|latest|p|extra|more",
    "gh-releases|||||",
    "zsync|https://example/x.zsync",
    "bintray-zsync|a|b|c|d",
    "",
    "|||||",
    "GH-RELEASES-zsync|o|r|latest|p",
    "gh-releases-zsync|öwnér|репо|latest|приложение-*.AppImage",
    " gh-releases-zsync|o|r|latest|p",
]

_TEXTS = ["v1.2.3", "app-0.10.2-x86_64.AppImage", "1", "release 2", "", "v.1.2",
          "1.2.3.4.5", "12.", ".5.6", "nightly", "١.٢.٣", "v٣.٤-beta", "1..2",
          "x-01.002", "2024.10.17-rc1", "１.２"]

_VKEYS = ["0.9.9", "0.10.2", "", "1", "1.2.3", "01.002", "nightly", "1..2",
          "١.٢", "1_0.2", "1__0", " 1. 2", "+1.-2", "1.", ".1", "1.2a",
          "１.２", "1.x"]

_GLOBS = [
    ("vectorcraft-0.6.0-linux-x86_64.AppImage", "vectorcraft-*-linux-x86_64.AppImage"),
    ("vectorcraft-0.6.0-linux-aarch64.AppImage", "vectorcraft-*-linux-x86_64.AppImage"),
    ("a.AppImage", "*.AppImage"), ("A.appimage", "*.AppImage"), ("", "*"),
    ("abc", "a?c"), ("ac", "a?c"), ("a[c", "a[c"), ("ab", "a[bc]"), ("ad", "a[!bc]"),
    ("ab", "a[!bc]"), ("a-", "a[a-]"), ("a]", "a[]]"), ("a!", "a[!]"), ("ab", "a[z-a]"),
    ("ab", "a[!z-a]"), ("a\\", "a[\\]"), ("a.b", "a.b"), ("axb", "a.b"),
    ("a\nb", "a*b"), ("приложение-1.AppImage", "приложение-*.AppImage"),
    ("a^", "a[^]"), ("a&", "a[&&]"), ("x-1.2", "x-[0-9].[0-9]"), ("b", "[a-c-e]"),
    ("-", "[a-c-e]"), ("d", "[a-c-e]"), ("ab", "**b"), ("a(b)", "a(b)"),
    ("a|b", "a|b"), ("a+", "a+"), ("a[b", "a[[]b"),
]


def _simple_cases():
    for info in _INFOS:
        yield "parse_update_info", {"info": info}, functools.partial(appimage.parse_update_info, info)
    for t in _TEXTS:
        yield "_version_of", {"text": t}, functools.partial(appimage._version_of, t)
    for v in _VKEYS:
        yield "_vkey", {"v": v}, functools.partial(appimage._vkey, v)
    for name, pat in _GLOBS:
        yield "_fnmatch", {"name": name, "pat": pat}, functools.partial(fnmatch.fnmatch, name, pat)


# --------------------------------------------------------------------------
# latest_github / check, with GitHub's answer injected

def _release(tag="v2.0.0", assets=None, published="2026-09-01T10:00:00Z", page="https://github.com/o/r/releases/tag/v2.0.0"):
    if assets is None:
        assets = [{"name": "app-2.0.0-x86_64.AppImage.zsync", "browser_download_url": "https://dl/zsync"},
                  {"name": "app-2.0.0-aarch64.AppImage", "browser_download_url": "https://dl/arm"},
                  {"name": "app-2.0.0-x86_64.AppImage", "browser_download_url": "https://dl/x86"}]
    d = {"tag_name": tag, "assets": assets, "html_url": page}
    if published is not None:
        d["published_at"] = published
    return d


def _injected(data, fn):
    def run():
        urls = []

        def fake(url, *a, **kw):
            urls.append(url)
            return data
        with mock.patch("ghcache.get_json", side_effect=fake):
            result = _err(fn)
        return {"result": result, "urls": urls}
    return run


_RELEASES = [
    _release(),
    _release(tag="nightly"),
    _release(tag="nightly", assets=[{"name": "app-nightly.AppImage", "browser_download_url": "u"}]),
    _release(tag="", assets=[]),
    _release(tag="release-3.1", published=None),
    _release(published=""),
    _release(assets=[{"name": "app-9.9-x86_64.AppImage"}]),
    _release(assets=[{"browser_download_url": "u"}]),
    {},
    {"assets": None},
    {"assets": "x"},
    {"assets": ""},
    {"assets": {}},
    {"assets": {"k": 1}},
    {"assets": [1]},
    {"assets": [None]},
    {"assets": [{"name": None}]},
    {"assets": [{"name": 5}]},
    {"assets": 7},
    {"tag_name": "v١.٢", "assets": [{"name": "x.AppImage", "browser_download_url": "u"}]},
    {"tag_name": "версия-1.0", "assets": [{"name": "ü.AppImage", "browser_download_url": "ü"}],
     "published_at": "2026-ü1-01T00:00:00Z"},
    None,
    [],
    "text",
    5,
]

_PATTERNS = ["app-*-x86_64.AppImage.zsync", "app-*-x86_64.AppImage", "*.AppImage",
             "*.zsync.zsync", "", "nomatch-*", "app-[0-9]*-x86_64.AppImage"]
_RELEASE_ARGS = ["latest", "", "v1.0", "ünï/tag"]


def _latest_cases():
    for data in _RELEASES:
        for pattern in _PATTERNS[:3]:
            yield ("latest_github",
                   {"owner": "o", "repo": "r", "pattern": pattern, "release": "latest", "data": data},
                   _injected(data, functools.partial(appimage.latest_github, "o", "r", pattern, "latest")))
    for pattern in _PATTERNS:
        for rel in _RELEASE_ARGS:
            data = _release()
            yield ("latest_github",
                   {"owner": "øwner", "repo": "r-é", "pattern": pattern, "release": rel, "data": data},
                   _injected(data, functools.partial(appimage.latest_github, "øwner", "r-é", pattern, rel)))


def _check_cases():
    exists = _dir("check") / "thing.AppImage"
    exists.write_bytes(b"x")
    real = Path.home() / ".local/state/almanac/apps/vectorcraft.AppImage"
    spec = {"kind": "github", "owner": "o", "repo": "r", "release": "latest",
            "pattern": "app-*-x86_64.AppImage.zsync"}
    records = [
        {"path": str(exists), "version": "1.0.0", "update_spec": spec},
        {"path": str(exists), "version": "2.0.0", "update_spec": spec},
        {"path": str(exists), "version": "3.0", "update_spec": spec},
        {"path": str(exists), "version": "v1.9.9-beta", "update_spec": spec},
        {"path": str(exists), "version": "", "update_spec": spec},
        {"path": str(exists), "update_spec": spec},
        {"path": str(exists), "version": "unknown", "update_spec": spec},
        {"path": str(exists), "version": None, "update_spec": spec},
        {"path": str(exists), "version": 1.5, "update_spec": spec},
        {"path": str(exists), "version": "1.0", "update_spec": {"kind": "github", "owner": "o", "repo": "r"}},
        {"path": str(exists), "version": "1.0", "update_spec": {"kind": "github", "owner": "o"}},
        {"path": str(exists), "version": "1.0", "update_spec": {"kind": "github", "repo": "r"}},
        {"path": str(exists), "version": "1.0", "update_spec": {"kind": "zsync", "owner": "o", "repo": "r"}},
        {"path": str(exists), "version": "1.0", "update_spec": {"kind": "github", "owner": 5, "repo": True,
                                                                  "release": None, "pattern": "*.AppImage"}},
        {"path": str(exists), "version": "1.0", "update_spec": {"kind": "github", "owner": "o", "repo": "r",
                                                                  "pattern": None}},
        {"path": str(exists), "version": "1.0", "update_spec": {"kind": "github", "owner": "o", "repo": "r",
                                                                  "extra": [1, 2], "release": "v2.0.0"}},
        {"path": str(exists), "version": "1.0", "update_spec": "gh-releases|o|r|latest|p"},
        {"path": str(exists), "version": "1.0", "update_spec": {}},
        {"path": str(exists), "version": "1.0"},
        {"path": str(real), "version": "0.1.0"},
        {"path": str(real), "version": "0.12.0", "update_spec": None},
        {"path": str(real), "version": "0.1.0", "update_spec": []},
        {"path": "/nonexistent/x.AppImage", "version": "1.0", "update_spec": spec},
        {"version": "1.0", "update_spec": spec},
        {"path": "", "version": "1.0", "update_spec": spec},
        {"path": None, "version": "1.0", "update_spec": spec},
        {},
    ]
    for rec in records:
        for data in (_release(), _release(tag="nightly"), _release(assets=[]), None,
                     _release(tag="v1.0.0"), {"assets": None}):
            yield ("check", {"record": rec, "data": data},
                   _injected(data, functools.partial(appimage.check, dict(rec))))


# --------------------------------------------------------------------------
# download, against a local server

class _Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *a):
        pass


def _server(root):
    handler = functools.partial(_Quiet, directory=str(root))
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    atexit.register(srv.shutdown)
    return f"http://127.0.0.1:{srv.server_address[1]}"


def _download_cases():
    www = _dir("www")
    (www / "small").write_bytes(b"NEW")
    (www / "big").write_bytes(b"0123456789abcdef" * 40000)   # 640000 bytes: three chunks
    (www / "empty").write_bytes(b"")
    (www / "ünï").write_bytes("unicode ✓".encode())
    base = _server(www)
    import socket
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    dead_port = s.getsockname()[1]
    s.close()

    plans = [
        ("small", "app.AppImage", b"OLD"),
        ("small", "app.AppImage", None),
        ("big", "app.AppImage", b"OLD"),
        ("empty", "app.AppImage", b"OLD"),
        ("%C3%BCn%C3%AF", "ünï.AppImage", b"OLD"),
        ("small", "noext", b"OLD"),
        ("small", "x.tar.gz", None),
        ("missing", "app.AppImage", b"OLD"),
        (None, "app.AppImage", b"OLD"),
    ]
    for i, (path, name, old) in enumerate(plans):
        url = f"{base}/{path}" if path else f"http://127.0.0.1:{dead_port}/x"
        dirs = {}
        for side in ("py", "rs"):
            d = _dir(f"dl{i}", side)
            if old is not None:
                (d / name).write_bytes(old)
                if name.endswith(".AppImage"):
                    (d / name).chmod(0o644)
            dirs[side] = d

        def run(url=url, d=dirs["py"], name=name):
            seen = []
            try:
                p = appimage.download(url, d / name, seen.append)
                result = str(p.relative_to(d))
            except Exception:                           # noqa: BLE001
                result = {"error": True}
            return {"result": result, "progress": seen, "files": _tree(d)}
        yield "download", {"url": url, "dir": str(dirs["rs"]), "name": name}, run


# --------------------------------------------------------------------------
# install_desktop_entry / remove_desktop_entry, with a fake AppImage

_EXTRACT = """#!/bin/sh
[ "$1" = "--appimage-extract" ] || exit 1
r=squashfs-root
mkdir -p $r/usr/share/icons/hicolor/256x256/apps $r/usr/share/icons/hicolor/48x48/apps \
         $r/usr/share/icons/hicolor/32x32/apps
printf 'PNG256' > $r/usr/share/icons/hicolor/256x256/apps/thing.png
printf 'PNG48' > $r/usr/share/icons/hicolor/48x48/apps/thing.png
printf 'PNG32' > $r/usr/share/icons/hicolor/32x32/apps/thing.png
printf '%s' "$DESKTOP" > $r/thing.desktop
exit 3
"""

_DESKTOPS = {
    "plain": "[Desktop Entry]\nName=Thing\nExec=AppRun --opt %U %F\nIcon=thing\nTryExec=AppRun\nType=Application\n",
    "crlf": "[Desktop Entry]\r\nName=Thing\r\nExec=AppRun\r\nIcon=thing\r\n",
    "codes": "[Desktop Entry]\nExec=  AppRun  %f\t%u   \nExec=\nExecX=keep\n Icon=indented\n",
    "unicode": "[Desktop Entry]\nName=Thïng ✓\nComment=Ünïcode\x0bsplit\nExec=AppRun %U\n",
    "invalid": "[Desktop Entry]\nName=\xff\xfe bad\nExec=AppRun\n",
    "noeol": "[Desktop Entry]\nExec=AppRun %F",
}


def _fake_appimage(dirname, desktop, extract=True, executable=True):
    d = _dir("apps", dirname)
    p = d / "Thing Ünï.AppImage"
    if extract:
        body = _EXTRACT.replace('"$DESKTOP"', "'" + desktop.replace("'", "'\\''") + "'")
    else:
        body = "#!/bin/sh\nexit 0\n"
    p.write_bytes(body.encode("utf-8", "surrogateescape") if "\xff" not in body
                  else body.encode("latin-1", "replace"))
    mode = p.stat().st_mode
    p.chmod(mode | stat.S_IXUSR if executable else mode & ~0o111)
    return p


def _install_cases():
    plans = [(k, v, True, True, "thing") for k, v in _DESKTOPS.items()]
    plans += [("noextract", "", False, True, "thing"),
              ("notexec", "", True, False, "thing"),
              ("uniname", _DESKTOPS["plain"], True, True, "ünï-app")]
    for i, (key, desktop, extract, executable, name) in enumerate(plans):
        app = _fake_appimage(f"{i}-{key}", desktop, extract, executable)
        homes = {side: _dir(f"inst{i}", side) for side in ("py", "rs")}

        def run(app=app, home=homes["py"], name=name):
            with mock.patch.dict(os.environ, {"XDG_DATA_HOME": str(home)}):
                result = _err(lambda: appimage.install_desktop_entry(app, name))
            files = {k: v for k, v in _tree(home).items()
                     if not k.endswith(("icon-theme.cache", "mimeinfo.cache"))}
            return {"result": result, "files": files}
        yield ("install_desktop_entry",
               {"path": str(app), "name": name, "env": {"XDG_DATA_HOME": str(homes["rs"])}}, run)
    # A path that is not there at all.
    homes = {side: _dir("inst-missing", side) for side in ("py", "rs")}
    missing = str(_dir("apps") / "gone.AppImage")

    def run_missing(home=homes["py"]):
        with mock.patch.dict(os.environ, {"XDG_DATA_HOME": str(home)}):
            result = _err(lambda: appimage.install_desktop_entry(Path(missing), "gone"))
        return {"result": result, "files": _tree(home)}
    yield ("install_desktop_entry",
           {"path": missing, "name": "gone", "env": {"XDG_DATA_HOME": str(homes["rs"])}}, run_missing)


def _remove_cases():
    names = ["thing", "a*", "ünï", "missing", "a[1]"]
    for i, name in enumerate(names):
        homes = {}
        for side in ("py", "rs"):
            h = _dir(f"rm{i}", side)
            for rel in ["applications/almanac-thing.desktop", "applications/almanac-a*.desktop",
                        "applications/almanac-ünï.desktop", "applications/other.desktop",
                        "icons/hicolor/256x256/apps/almanac-thing.png",
                        "icons/hicolor/48x48/apps/almanac-thing.png",
                        "icons/hicolor/48x48/apps/almanac-thing.svg",
                        "icons/hicolor/48x48/apps/almanac-a1.png",
                        "icons/hicolor/48x48/apps/almanac-ab.png",
                        "icons/hicolor/48x48/apps/almanac-a*.png",
                        "icons/hicolor/48x48/apps/almanac-ünï.png",
                        "icons/hicolor/scalable/apps/almanac-ünï.png",
                        "icons/hicolor/.hidden/apps/almanac-thing.png",
                        "icons/hicolor/x/almanac-thing.png",
                        "icons/other/48x48/apps/almanac-thing.png"]:
                p = h / rel
                p.parent.mkdir(parents=True, exist_ok=True)
                p.write_text(rel)
            homes[side] = h

        def run(home=homes["py"], name=name):
            with mock.patch.dict(os.environ, {"XDG_DATA_HOME": str(home)}):
                result = _err(lambda: appimage.remove_desktop_entry(name))
            return {"result": result, "files": _tree(home)}
        yield ("remove_desktop_entry",
               {"name": name, "env": {"XDG_DATA_HOME": str(homes["rs"])}}, run)


# --------------------------------------------------------------------------
# Review round 1: shapes and environments the earlier cases did not reach.

_R1_RELEASES = [
    {"tag_name": "v2.0", "assets": [{"name": "a.AppImage", "browser_download_url": None}]},
    {"tag_name": "v2.0", "assets": [{"name": "a.AppImage", "browser_download_url": 5}]},
    {"tag_name": None, "assets": [{"name": "a-3.1.AppImage", "browser_download_url": "u"}]},
    {"tag_name": None, "assets": [{"name": "a.AppImage", "browser_download_url": "u"}]},
    {"tag_name": 5, "assets": [{"name": "a.AppImage", "browser_download_url": "u"}]},
    {"tag_name": ["v1.0"], "assets": []},
    {"tag_name": "v2.0", "assets": [], "published_at": 20260901},
    {"tag_name": "v2.0", "assets": [], "published_at": ["2026", "09"]},
    {"tag_name": "v2.0", "assets": [], "published_at": True},
    {"tag_name": "v2.0", "assets": [], "html_url": None},
    {"tag_name": "v2.0", "assets": [], "html_url": 7},
]


def _r1_latest_cases():
    for data in _R1_RELEASES:
        yield ("latest_github",
               {"owner": "o", "repo": "r", "pattern": "*.AppImage", "release": "latest", "data": data},
               _injected(data, functools.partial(appimage.latest_github, "o", "r", "*.AppImage", "latest")))


def _r1_check_cases():
    exists = _dir("check") / "thing.AppImage"
    exists.write_bytes(b"x")
    big = 10 ** 20
    specs = [
        {"kind": "github", "owner": {"a": 1}, "repo": "r"},
        {"kind": "github", "owner": "o", "repo": [1, 2]},
        {"kind": "github", "owner": big, "repo": "r"},
        {"kind": "github", "owner": "o", "repo": "r", "release": ["v1", "v2"]},
        {"kind": "github", "owner": "o", "repo": "r", "release": {"t": "v1"}},
    ]
    for spec in specs:
        rec = {"path": str(exists), "version": "1.0", "update_spec": spec}
        data = _release()
        yield ("check", {"record": rec, "data": data},
               _injected(data, functools.partial(appimage.check, dict(rec))))
    spec = {"kind": "github", "owner": "o", "repo": "r", "pattern": "*.AppImage"}
    huge = [
        # Components past i128: Python compares them exactly.
        ("1.1" + "0" * 42, "v1.2" + "0" * 42),
        ("1.2" + "0" * 42, "v1.1" + "0" * 42),
        # Python's int() refuses more than 4300 digits (ValueError).
        ("1." + "1" * 5000, "v2.0.0"),
    ]
    for have, tag in huge:
        rec = {"path": str(exists), "version": have, "update_spec": spec}
        data = _release(tag=tag, assets=[{"name": "a.AppImage", "browser_download_url": "u"}])
        yield ("check", {"record": rec, "data": data},
               _injected(data, functools.partial(appimage.check, dict(rec))))
    for v in ["99999999999999999999", "1." + "1" * 5000]:
        yield "_vkey", {"v": v}, functools.partial(appimage._vkey, v)


class _Odd(http.server.BaseHTTPRequestHandler):
    """Responses SimpleHTTPRequestHandler never gives."""
    protocol_version = "HTTP/1.0"

    def log_message(self, *a):
        pass

    def do_GET(self):
        import gzip
        p = self.path
        if p == "/short":                       # Content-Length promises more than comes
            self.send_response(200)
            self.send_header("Content-Length", "100")
            self.end_headers()
            self.wfile.write(b"NEW")
        elif p == "/gzip":                      # encoded although identity was asked
            body = gzip.compress(b"NEW-PLAIN-CONTENT")
            self.send_response(200)
            self.send_header("Content-Encoding", "gzip")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        elif p == "/nolen":                     # body until close
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b"NOLEN")
        elif p == "/spacelen":
            self.send_response(200)
            self.send_header("Content-Length", " 3 ")
            self.end_headers()
            self.wfile.write(b"NEW")
        elif p.startswith("/redir"):
            code = int(p[len("/redir"):] or 302)
            self.send_response(code)
            self.send_header("Location", "/nolen")
            self.send_header("Content-Length", "0")
            self.end_headers()
        else:
            self.send_response(404)
            self.send_header("Content-Length", "0")
            self.end_headers()


def _r1_download_cases():
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), _Odd)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    atexit.register(srv.shutdown)
    odd = f"http://127.0.0.1:{srv.server_address[1]}"
    www = _dir("www")
    (www / "small").write_bytes(b"NEW")
    (www / "ünï").write_bytes("unicode ✓".encode())
    simple = _server(www)
    urls = [
        f"{odd}/short", f"{odd}/gzip", f"{odd}/nolen", f"{odd}/spacelen",
        f"{odd}/redir301", f"{odd}/redir307", f"{odd}/redir308",
        f"file://{www / 'small'}",
        "data:,NEWDATA",
        f"{simple}/ünï",
        f"{simple}/sm all",
        # Review round 2: urlopen's local schemes and its URL clean-up.
        "data:text/plain;base64,TkVXLUI2NA==",
        "data:;base64,TkV",
        "data:;base64,T",
        "data:,a%41%zz#frag",
        "data:nocomma",
        "DATA:,UPPER",
        "  <URL:data:,WRAPPED>  ",
        f"file://localhost{www / 'small'}",
        f"file://{www / 'sm%61ll'}?q=1",
        f"file://{www}",
        "file:///nonexistent/x",
        "file://elsewhere.invalid/x",
        "gopher://127.0.0.1/x",
        "no-scheme",
    ]
    for i, url in enumerate(urls):
        dirs = {}
        for side in ("py", "rs"):
            d = _dir(f"r1dl{i}", side)
            (d / "app.AppImage").write_bytes(b"OLD")
            (d / "app.AppImage").chmod(0o644)
            dirs[side] = d

        def run(url=url, d=dirs["py"]):
            seen = []
            try:
                p = appimage.download(url, d / "app.AppImage", seen.append)
                result = str(p.relative_to(d))
            except Exception:                           # noqa: BLE001
                result = {"error": True}
            return {"result": result, "progress": seen, "files": _tree(d)}
        yield "download", {"url": url, "dir": str(dirs["rs"]), "name": "app.AppImage"}, run


_R1_EXTRACT = """#!/bin/sh
[ "$1" = "--appimage-extract" ] || exit 1
r=squashfs-root
mkdir -p $r/usr/share/icons/hicolor/256x256/apps $r/ro
printf 'PNG256' > $r/usr/share/icons/hicolor/256x256/apps/thing.png
printf '[Desktop Entry]\nExec=AppRun %%U\nIcon=thing\n' > $r/thing.desktop
printf 'x' > $r/ro/file
MODE
exit 0
"""


def _r1_install_cases():
    plans = [
        # A read-only icon (squashfs keeps modes): copy2 copies it fine.
        ("roicon", "chmod 444 $r/usr/share/icons/hicolor/256x256/apps/thing.png"),
        # A read-only directory in the extracted tree: TemporaryDirectory
        # still removes it; anything left behind shows up in the tree.
        ("rodir", "chmod 555 $r/ro"),
    ]
    for key, mode in plans:
        homes = {}
        for side in ("py", "rs"):
            # The data home is the AppImage's own directory, so a temporary
            # directory left beside the AppImage is part of the compared tree.
            h = _dir(f"r1inst-{key}", side)
            app = h / "Thing.AppImage"
            app.write_text(_R1_EXTRACT.replace("MODE", mode))
            app.chmod(0o755)
            homes[side] = (h, app)

        def run(h=homes["py"][0], app=homes["py"][1], rs=homes["rs"][0]):
            with mock.patch.dict(os.environ, {"XDG_DATA_HOME": str(h)}):
                result = _err(lambda: appimage.install_desktop_entry(app, "thing"))
            # The .desktop names the AppImage's own path: each side has its own.
            files = {k: v.replace(str(h), str(rs)) for k, v in _tree(h).items()
                     if not k.endswith(("icon-theme.cache", "mimeinfo.cache"))}
            return {"result": result, "files": files}
        h, app = homes["rs"]
        yield ("install_desktop_entry",
               {"path": str(app), "name": "thing", "env": {"XDG_DATA_HOME": str(h)}}, run)


def _r1_remove_cases():
    homes = {}
    for side in ("py", "rs"):
        h = _dir("r1rm-slash", side)
        for rel in ["applications/almanac-x/y.desktop",
                    "icons/hicolor/48x48/apps/almanac-x/y.png",
                    "icons/hicolor/48x48/apps/almanac-x/z.png"]:
            p = h / rel
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(rel)
        homes[side] = h

    def run(home=homes["py"]):
        with mock.patch.dict(os.environ, {"XDG_DATA_HOME": str(home)}):
            result = _err(lambda: appimage.remove_desktop_entry("x/y"))
        return {"result": result, "files": _tree(home)}
    yield ("remove_desktop_entry", {"name": "x/y", "env": {"XDG_DATA_HOME": str(homes["rs"])}}, run)


# --------------------------------------------------------------------------
# Review round 2 (adversarial): differences found by reading the two side by side.

def _r2_simple_cases():
    # fnmatch: a range that is dropped can leave '!' at the front of the
    # class, which Python then reads as negation ([a-*!b] -> [^b]).
    for name, pat in [("c", "[a-*!b]"), ("b", "[a-*!b]"), ("!", "[a-*!b]"),
                      ("c", "[a-*!]"), ("!", "[a-*!]")]:
        yield "_fnmatch", {"name": name, "pat": pat}, functools.partial(fnmatch.fnmatch, name, pat)
    data = _release()
    pattern = "app-[a-*!b]*-x86_64.AppImage"
    yield ("latest_github",
           {"owner": "o", "repo": "r", "pattern": pattern, "release": "latest", "data": data},
           _injected(data, functools.partial(appimage.latest_github, "o", "r", pattern, "latest")))


def _r2_check_cases():
    exists = _dir("check") / "thing.AppImage"
    exists.write_bytes(b"x")
    # A record that is not a dict: Python's record.get raises.
    for rec in (None, [], "text", 5):
        data = _release()
        yield ("check", {"record": rec, "data": data},
               _injected(data, functools.partial(appimage.check, rec)))
    # Values formatted into the URL: an int past 2**64 and a large float.
    specs = [
        {"kind": "github", "owner": 10 ** 20 + 1, "repo": "r"},
        {"kind": "github", "owner": "o", "repo": "r", "release": 1e20},
        {"kind": "github", "owner": ["͸", "⁥"], "repo": "r"},
    ]
    for spec in specs:
        rec = {"path": str(exists), "version": "1.0", "update_spec": spec}
        data = _release()
        yield ("check", {"record": rec, "data": data},
               _injected(data, functools.partial(appimage.check, dict(rec))))


class _Odd2(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"

    def log_message(self, *a):
        pass

    def do_GET(self):
        p = self.path
        if p.startswith("/status"):             # a non-2xx that is not an error to ureq
            self.send_response(int(p[len("/status"):]))
            self.send_header("Content-Length", "3")
            self.end_headers()
            self.wfile.write(b"NEW")
        elif p == "/noloc":                     # a redirect without Location
            self.send_response(302)
            self.send_header("Content-Length", "3")
            self.end_headers()
            self.wfile.write(b"NEW")
        elif p.startswith("/len"):              # odd Content-Length values
            self.send_response(200)
            self.send_header("Content-Length", {"/len-neg": "-5", "/len-us": "1_0",
                                                "/len-plus": "+10"}[p])
            self.end_headers()
            self.wfile.write(b"0123456789")
        else:
            self.send_response(200)
            self.send_header("Content-Length", "3")
            self.end_headers()
            self.wfile.write(b"NEW")


def _r2_https_server():
    """An HTTPS server on 127.0.0.1 with a self-signed certificate."""
    import ssl
    import subprocess
    d = _dir("tls")
    cert, key = d / "cert.pem", d / "key.pem"
    subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "2",
                    "-subj", "/CN=127.0.0.1", "-addext", "subjectAltName=IP:127.0.0.1",
                    "-keyout", str(key), "-out", str(cert)], capture_output=True, check=True)
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), _Odd2)
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.load_cert_chain(str(cert), str(key))
    srv.socket = ctx.wrap_socket(srv.socket, server_side=True)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    atexit.register(srv.shutdown)
    return f"https://127.0.0.1:{srv.server_address[1]}", str(cert)


def _r2_download_cases():
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), _Odd2)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    atexit.register(srv.shutdown)
    odd = f"http://127.0.0.1:{srv.server_address[1]}"
    import socket
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    dead = f"http://127.0.0.1:{s.getsockname()[1]}"
    s.close()
    https, cert = _r2_https_server()
    plans = [
        # Must come first: urllib builds its opener (and TLS context, which
        # reads SSL_CERT_FILE) on the first urlopen of the process.
        (f"{https}/ok", {"SSL_CERT_FILE": cert}),
        ("data:,abc#", {}),
        (f"{odd}/status300", {}),
        (f"{odd}/status304", {}),
        (f"{odd}/status203", {}),
        (f"{odd}/noloc", {}),
        (f"{odd}/len-neg", {}),
        (f"{odd}/len-us", {}),
        (f"{odd}/len-plus", {}),
        # Proxies: urllib picks one per scheme, ignores all_proxy for http,
        # and ignores HTTP_PROXY under CGI (REQUEST_METHOD set).
        (f"{odd}/ok", {"HTTPS_PROXY": dead, "https_proxy": dead}),
        (f"{odd}/ok", {"ALL_PROXY": dead, "all_proxy": dead}),
        (f"{odd}/ok", {"REQUEST_METHOD": "GET", "HTTP_PROXY": dead}),
        (f"HTTP://127.0.0.1:{srv.server_address[1]}/ok", {}),
    ]
    for i, (url, env) in enumerate(plans):
        dirs = {}
        for side in ("py", "rs"):
            d = _dir(f"r2dl{i}", side)
            (d / "app.AppImage").write_bytes(b"OLD")
            (d / "app.AppImage").chmod(0o644)
            dirs[side] = d

        def run(url=url, d=dirs["py"], env=env):
            seen = []
            try:
                with mock.patch.dict(os.environ, env):
                    p = appimage.download(url, d / "app.AppImage", seen.append)
                result = str(p.relative_to(d))
            except Exception:                           # noqa: BLE001
                result = {"error": True}
            return {"result": result, "progress": seen, "files": _tree(d)}
        yield "download", {"url": url, "dir": str(dirs["rs"]), "name": "app.AppImage", "env": env}, run


_R2_EXTRACT = """#!/bin/sh
[ "$1" = "--appimage-extract" ] || exit 1
r=squashfs-root
mkdir -p $r/usr/share/icons/hicolor/256x256/apps/dir.png
printf '[Desktop Entry]\\nExec=AppRun %%U\\nIcon=thing\\n' > $r/thing.desktop
BODY
exit 0
"""


def _r2_install_cases():
    plans = [
        # The first *.png is a directory: copy2 raises before creating the copy.
        ("icondir", ""),
    ]
    if os.environ.get("APPIMAGE_PARITY_SLOW"):
        # A helper the AppImage leaves running keeps stdout open past the
        # 180 s timeout: Python gives up (False), Rust waits for it.
        plans.append(("grandchild", "rmdir $r/usr/share/icons/hicolor/256x256/apps/dir.png\n(sleep 190) &"))
    for key, body in plans:
        homes = {}
        for side in ("py", "rs"):
            h = _dir(f"r2inst-{key}", side)
            app = h / "Thing.AppImage"
            app.write_text(_R2_EXTRACT.replace("BODY", body))
            app.chmod(0o755)
            homes[side] = (h, app)

        def run(h=homes["py"][0], app=homes["py"][1], rs=homes["rs"][0]):
            with mock.patch.dict(os.environ, {"XDG_DATA_HOME": str(h)}):
                result = _err(lambda: appimage.install_desktop_entry(app, "thing"))
            files = {k: v.replace(str(h), str(rs)) for k, v in _tree(h).items()
                     if not k.endswith(("icon-theme.cache", "mimeinfo.cache"))}
            return {"result": result, "files": files}
        h, app = homes["rs"]
        yield ("install_desktop_entry",
               {"path": str(app), "name": "thing", "env": {"XDG_DATA_HOME": str(h)}}, run)


def cases():
    yield from _r2_download_cases()
    yield from _r2_simple_cases()
    yield from _r2_check_cases()
    yield from _r2_install_cases()
    yield from _r1_latest_cases()
    yield from _r1_check_cases()
    yield from _r1_download_cases()
    yield from _r1_install_cases()
    yield from _r1_remove_cases()
    yield from _read_cases()
    yield from _simple_cases()
    yield from _latest_cases()
    yield from _check_cases()
    yield from _download_cases()
    yield from _install_cases()
    yield from _remove_cases()
