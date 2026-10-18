"""Parity cases for sources.py.

Nothing here touches the real user's files:
- parse/_norm are pure;
- _official runs `pacman -Si` (read-only) on the real machine;
- _aur gets its RPC answer from a patched _get (the Rust side gets the same
  bytes as `data`), plus one live, read-only query to the real AUR;
- _sums/_appimage fetch checksum files from an http.server on 127.0.0.1;
- resolve gets GitHub from patched ghcache.repo/latest_release and every
  other URL from a patched _get (the Rust side gets the same `meta`,
  `release` and `responses`); pacman is real;
- install_appimage downloads from the local server into a temp dir per side,
  with XDG_STATE_HOME/XDG_DATA_HOME and inventory.REGISTRY pointed there.
"""
import atexit
import functools
import hashlib
import http.server
import json
import os
import shutil
import tempfile
import threading
from pathlib import Path
from unittest import mock

import ghcache
import inventory
import sources

_TMP = tempfile.mkdtemp(prefix="almanac-parity-sources-")
atexit.register(shutil.rmtree, _TMP, True)


def _dir(*parts):
    d = Path(_TMP, *parts)
    d.mkdir(parents=True, exist_ok=True)
    return d


# ---- a local web server ------------------------------------------------------

_SRV = _dir("srv")
_APP_BODY = b"#!/bin/sh\nexit 0\n"
_APP_SHA = hashlib.sha256(_APP_BODY).hexdigest()
_FILES = {
    "app.AppImage": _APP_BODY,
    "SHA256SUMS": (f"{_APP_SHA}  app-2.0-x86_64.AppImage\n"
                   f"{'b' * 64} *other.AppImage\r\n"
                   "not a checksum line\n"
                   f"{'C' * 64}\n"
                   "\n").encode(),
    "app-2.0-x86_64.AppImage.sha256": f"{'D' * 64}\n".encode(),
    "checksums.txt": f"{'e' * 64}  app.AppImage  extra\n{'f' * 63}  short\n".encode(),
    "latin1.sha256": "caf\xe9 ".encode("latin-1") + b"a" * 64 + b"\n",
    "ünï.txt": "hello ünïcode".encode(),
    # review round 1: a checksum file whose name is not ASCII
    "ünï-sha256.txt": f"{'9' * 64}  app.AppImage\n".encode(),
}
for _n, _b in _FILES.items():
    (_SRV / _n).write_bytes(_b)


class _Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_a):
        pass


_server = http.server.ThreadingHTTPServer(
    ("127.0.0.1", 0), functools.partial(_Quiet, directory=str(_SRV)))
threading.Thread(target=_server.serve_forever, daemon=True).start()
BASE = f"http://127.0.0.1:{_server.server_address[1]}"


# ---- helpers -----------------------------------------------------------------

def _aur_case(data, names=("app", "app-bin"), homes=("https://github.com/owner/app",)):
    raw = data if isinstance(data, str) else json.dumps(data)

    def run():
        with mock.patch.object(sources, "_get", return_value=raw.encode()):
            return sources._aur(list(names), list(homes))
    return "_aur", {"names": list(names), "homes": list(homes), "data": raw}, run


def _resolve_case(link, meta, release=None, responses=None):
    responses = responses or {}

    def fake_get(url, timeout=20):
        body = responses.get(url)
        if body is None:
            raise OSError(f"no response for {url}")
        return body.encode()

    def run():
        with mock.patch.object(ghcache, "repo", return_value=meta), \
             mock.patch.object(ghcache, "latest_release", return_value=release), \
             mock.patch.object(sources, "_get", side_effect=fake_get):
            return sources.resolve(link)
    return "resolve", {"link": link, "meta": meta, "release": release,
                       "responses": responses}, run


def _aur_url(name):
    ns = [name, f"{name}-bin", f"{name}-git", f"{name}-appimage"]
    return "https://aur.archlinux.org/rpc/v5/info?" + "&".join(f"arg[]={n}" for n in ns)


def _tree(d):
    out = []
    for root, _dirs, files in os.walk(d):
        for f in files:
            out.append(str(Path(root, f).relative_to(d)))
    return sorted(out)


def _install_case(tag, res, opt):
    py_root = _dir("install", tag, "py")
    rs_root = _dir("install", tag, "rs")

    def run():
        env = {"XDG_STATE_HOME": str(py_root / "state"),
               "XDG_DATA_HOME": str(py_root / "data")}
        lines = []
        with mock.patch.dict(os.environ, env), \
             mock.patch.object(inventory, "REGISTRY",
                               py_root / "state" / "almanac" / "appimages.json"):
            dest = sources.install_appimage(sources.Resolution(**res),
                                            sources.Option(**opt), log=lines.append)
        reg_path = py_root / "state" / "almanac" / "appimages.json"
        reg = None
        if reg_path.exists():
            # NaN/Infinity as their JSON tokens: the Rust hook's JSON cannot
            # carry a non-finite float.
            reg = json.loads(reg_path.read_text().replace(str(py_root), "<root>"),
                             parse_constant=lambda c: c)
        return {"path": str(Path(dest).relative_to(py_root)), "log": lines,
                "files": _tree(py_root), "registry": reg}
    return "install_appimage", {"root": str(rs_root), "res": res, "opt": opt}, run


def _asset(name, size=100, url=None, **kw):
    a = {"name": name, "size": size}
    if url is not False:
        a["browser_download_url"] = url if url is not None else f"{BASE}/{name}"
    a.update(kw)
    return a


# ---- cases -------------------------------------------------------------------

def cases():
    # parse
    for link in ["https://github.com/storytold/wordcraft",
                 "github.com/storytold/wordcraft/releases",
                 "https://github.com/storytold/wordcraft.git",
                 "http://www.github.com/a/b?tab=readme",
                 "https://github.com/a/b#readme",
                 "https://github.com/a/b.git/",
                 "https://github.com/a/b.git.git",
                 "  github.com/a.b/c-d_e \n",
                 "\x1cgithub.com/a/b\x1f",
                 "https://github.com/a",
                 "https://github.com/a/",
                 "https://gitlab.com/a/b", "wordcraft", "", "   ",
                 "GITHUB.COM/a/b", "git@github.com:a/b.git",
                 "ftp://github.com/a/b",
                 "https://github.com/a/b\nmore",
                 "https://github.com/a/b/tree/main/x\ty",
                 "https://github.com/ünï/cödé", "https://github.com/日本/語",
                 "https://github.com/a/b%20c", "https://github.com/a b/c",
                 "https://github.com/a/.git", "https://github.com/a/..",
                 "https://github.com/a/b?x", "https://github.com/a/b/"]:
        yield "parse", {"link": link}, lambda l=link: sources.parse(l)

    # _norm
    for url in ["https://github.com/O/R", "https://github.com/o/r/",
                "HTTPS://WWW.GitHub.com/o/r.git", "http://www.x.org///",
                " https://x.org/ ", "https://https://x.org", "www.x.org",
                "https://x.org/a.git/", "x.GIT", "", "   ", "ÜNÏ.org/Σ",
                "https://www.www.x.org", "github.com/o/r.git.git"]:
        yield "_norm", {"url": url}, lambda u=url: sources._norm(u)

    # _official: the real machine
    for name in ["pacman", "linux", "firefox", "bash", "pacman-contrib",
                 "glibc", "no-such-package-almanac", "", "ü", "--help"]:
        yield "_official", {"name": name}, lambda n=name: sources._official(n)

    # _aur on fixed answers
    yield _aur_case({"results": [
        {"Name": "app", "Version": "1", "NumVotes": 50, "Maintainer": "m",
         "URL": "https://github.com/someone-else/app"},
        {"Name": "app-bin", "Version": "1", "NumVotes": 50, "Maintainer": "m",
         "URL": "https://app.example.org"}]},
        homes=("https://github.com/owner/app", "https://app.example.org/"))
    yield _aur_case({"results": [
        {"Name": "app", "Version": "2.0-1", "NumVotes": 3, "Maintainer": None,
         "URL": "https://github.com/owner/app.git", "OutOfDate": 1700000000},
        {"Name": "app-git", "Version": "r1.abc-1", "NumVotes": 0,
         "Maintainer": "x", "URL": None},
        {"Name": "app-appimage", "Version": "2", "NumVotes": "12",
         "Maintainer": "y", "URL": "HTTP://WWW.GITHUB.COM/owner/app/"},
        {"Name": "app-bin", "NumVotes": 4.9, "Maintainer": "", "URL": ""},
        {}]}, names=("app", "app-git", "app-appimage", "app-bin"),
        homes=("https://github.com/owner/app", ""))
    yield _aur_case({"results": [
        {"Name": "app", "Version": "1", "NumVotes": True, "Maintainer": "m",
         "URL": "https://github.com/owner/app", "OutOfDate": 0}]})
    yield _aur_case({"results": []})
    yield _aur_case({"resultcount": 0})
    yield _aur_case({"results": {}})
    yield _aur_case("not json")
    yield _aur_case("")
    yield _aur_case({"results": [{"Name": "ünï", "Version": "1", "NumVotes": 9,
                                  "Maintainer": "m", "URL": "https://ünï.org"}]},
                    names=("ünï", "a b&c"), homes=("https://ÜNÏ.org/",))
    # shapes the Python raises on
    yield _aur_case([1, 2])
    yield _aur_case({"results": [{"Name": None}]})
    yield _aur_case({"results": [{"Name": "a", "NumVotes": "x"}]})
    yield _aur_case({"results": [{"Name": "a", "URL": 5}]})
    yield _aur_case({"results": ["a"]})
    yield _aur_case({"results": 5})
    # the real AUR, read-only
    names, homes = ["yay", "yay-bin", "yay-git"], ["https://github.com/Jguer/yay"]
    yield ("_aur", {"names": names, "homes": homes, "live": True},
           lambda: sources._aur(names, homes))

    # _sums, from the local server
    sums_sets = [
        [_asset("SHA256SUMS")],
        [_asset("app-2.0-x86_64.AppImage.sha256")],
        [_asset("checksums.txt"), _asset("app.AppImage", 10**8)],
        [_asset("SHA256SUMS", size=70000)],
        [_asset("SHA256SUMS", url=False)],
        [_asset("SHA256SUMS", url=f"{BASE}/missing")],
        [_asset("SHA256SUMS", url="not a url")],
        [_asset("SHA256SUMS", url=123)],
        [_asset("latin1.sha256")],
        [_asset("SHA256SUMS"), _asset("app-2.0-x86_64.AppImage.sha256")],
        [{"name": "notes.txt"}],
        [{"name": "SHA256SUMS", "browser_download_url": f"{BASE}/SHA256SUMS"}],
        [_asset("SHA256SUMS", size=1.5)],
        [],
        # shapes the Python raises on
        [{"name": "SHA256SUMS", "size": None}],
        [{"name": None}],
        ["x"],
    ]
    for assets in sums_sets:
        yield "_sums", {"assets": assets}, lambda a=assets: sources._sums(a)

    # _appimage
    releases = [
        {"tag_name": "v1.2.0", "assets": [
            {"name": "app-1.2.0-aarch64.AppImage", "size": 1, "browser_download_url": "u1"},
            {"name": "app-1.2.0-x86_64.AppImage", "size": 1048576,
             "browser_download_url": "u2", "digest": "sha256:" + "a" * 64}]},
        {"tag_name": "v1", "assets": [
            {"name": "app-1-x86_64.AppImage", "size": 1, "browser_download_url": "u"}]},
        {"tag_name": "vv2.0", "assets": [
            _asset("app-2.0-x86_64.AppImage", 52428800), _asset("SHA256SUMS")]},
        {"tag_name": "2.0", "assets": [
            _asset("app-2.0.AppImage", 3 * 1048576 // 2),
            _asset("app-2.0-amd64.AppImage", 1572864),
            _asset("app-2.0-X64.AppImage", 1)]},
        {"tag_name": None, "assets": [
            _asset("app-2.0-x86_64.AppImage", None, digest="md5:abc"),
            _asset("app-2.0-x86_64.AppImage.sha256")]},
        {"assets": [_asset("app-armhf.AppImage"), _asset("app-i686.AppImage"),
                    _asset("app-RISCV.AppImage"), _asset("app.appimage")]},
        {"tag_name": "v3", "assets": [_asset("app.AppImage", 123456789, url=False,
                                             digest="")]},
        {"tag_name": "v3", "assets": [_asset("app.AppImage", 2.5 * 1048576)]},
        {"tag_name": "v3", "assets": [_asset("app.AppImage", 0.05 * 1048576)]},
        {"tag_name": "v3", "assets": [_asset("app.AppImage", 0.15 * 1048576)]},
        {"tag_name": "v1", "assets": None},
        {"tag_name": "v1"},
        {},
        {"tag_name": "v1", "assets": [{"name": "ünï-x86_64.AppImage", "size": 7}]},
        # shapes the Python raises on
        {"tag_name": "v1", "assets": [{"name": 5}]},
        {"tag_name": 5, "assets": [_asset("app.AppImage", digest="sha256:" + "a" * 64)]},
        {"tag_name": "v1", "assets": [_asset("app.AppImage", "big", digest="sha256:x")]},
        {"tag_name": "v1", "assets": 7},
    ]
    for rel in releases:
        yield "_appimage", {"release": rel}, lambda r=rel: sources._appimage(r)

    # resolve
    meta = {"name": "Wordcraft", "owner": {"login": "StoryTold"},
            "description": "Write things", "stargazers_count": 1234,
            "license": {"spdx_id": "GPL-3.0"}, "archived": False,
            "homepage": "https://wordcraft.example.org"}
    aur_answer = json.dumps({"results": [
        {"Name": "wordcraft", "Version": "1.0-1", "NumVotes": 20, "Maintainer": "m",
         "URL": "https://github.com/storytold/wordcraft"},
        {"Name": "wordcraft-bin", "Version": "1.0-1", "NumVotes": 2, "Maintainer": "m",
         "URL": "https://wordcraft.example.org/"},
        {"Name": "wordcraft-git", "Version": "r9-1", "NumVotes": 7, "Maintainer": None,
         "URL": "https://github.com/evil/wordcraft"}]})
    release = {"tag_name": "v1.0", "assets": [
        _asset("Wordcraft-1.0-x86_64.AppImage", 80 * 1048576,
               digest="sha256:" + "c" * 64)]}
    yield _resolve_case("https://github.com/storytold/wordcraft", meta, release,
                        {_aur_url("wordcraft"): aur_answer})
    yield _resolve_case("https://github.com/storytold/wordcraft",
                        dict(meta, archived=True, license=None, description=None,
                             stargazers_count=None, homepage=None, owner={}),
                        release, {_aur_url("wordcraft"): aur_answer})
    yield _resolve_case("https://github.com/storytold/wordcraft", meta, None, {})
    yield _resolve_case("https://github.com/storytold/wordcraft", meta, ["x"], {})
    # a real official package: pacman answers for real
    yield _resolve_case("https://github.com/mozilla/firefox",
                        {"name": "Firefox", "owner": {"login": "mozilla"}}, None,
                        {_aur_url("firefox"): '{"results": []}'})
    yield _resolve_case("https://github.com/bminor/bash.git",
                        {"name": "bash", "stargazers_count": "77", "archived": 1},
                        {"tag_name": "x", "assets": []}, {})
    yield _resolve_case("https://gitlab.com/a/b", meta)
    yield _resolve_case("", meta)
    yield _resolve_case("https://github.com/a/b", None)
    yield _resolve_case("https://github.com/a/b", {})
    yield _resolve_case("https://github.com/a/b", {"name": ""})
    yield _resolve_case("https://github.com/a/b", ["name"])
    yield _resolve_case("https://github.com/a/b", "a string")
    yield _resolve_case("https://github.com/ünï/cödé", {"name": "Cödé"}, None,
                        {"https://aur.archlinux.org/rpc/v5/info?arg[]=c%C3%B6d%C3%A9&arg[]=c%C3%B6d%C3%A9-bin&arg[]=c%C3%B6d%C3%A9-git&arg[]=c%C3%B6d%C3%A9-appimage":
                         '{"results": [{"Name": "cödé", "NumVotes": 5, "Maintainer": "m", "URL": "https://github.com/ünï/Cödé"}]}'})
    # shapes the Python raises on
    yield _resolve_case("https://github.com/a/b", {"name": "b", "owner": None})
    yield _resolve_case("https://github.com/a/b", {"name": "b", "license": "MIT"})
    yield _resolve_case("https://github.com/a/b", {"name": 5})

    # Resolution properties
    for res in [{"owner": "o", "repo": "r"}, {"owner": "", "repo": ""},
                {"owner": "ü", "repo": "r", "options": [
                    {"kind": "aur", "name": "a", "version": "1", "title": "t"},
                    {"kind": "repo", "name": "b", "version": "2", "title": "u"}]}]:
        def mk(r=res):
            r = dict(r)
            r["options"] = [sources.Option(**o) for o in r.get("options", [])]
            return sources.Resolution(**r)
        yield "Resolution.url", {"res": res}, lambda m=mk: m().url
        yield "Resolution.best", {"res": res}, lambda m=mk: m().best

    # install_appimage, into temp dirs
    res = {"owner": "o", "repo": "App Ünï", "description": "d"}
    opt = {"kind": "appimage", "name": "app-1-x86_64.AppImage", "version": "1",
           "title": "AppImage", "url": f"{BASE}/app.AppImage", "sha256": "0" * 64,
           "size_mb": 2.5}
    yield _install_case("mismatch", res, opt)
    yield _install_case("missing", res, dict(opt, url=f"{BASE}/missing"))
    yield _install_case("verified", res, dict(opt, sha256=_APP_SHA))
    yield _install_case("unverified", {"owner": "o", "repo": "app"},
                        dict(opt, sha256="", version="", size_mb=0.5))


# ---- review round 1: suspected differences ----------------------------------

def _raises_as_value(fn):
    """The harness accepts any Rust dict with an "error" key when Python
    raises, and a Resolution always has one; so turn a Python raise into a
    plain value, which a Rust Resolution cannot equal. The case passes
    "raises_as_value" so the Rust hook reports its Err the same way."""
    def run():
        try:
            return fn()
        except Exception as exc:                          # noqa: BLE001
            return f"python raised {type(exc).__name__}"
    return run


def review_cases():
    # _aur: json.loads(bytes) accepts a UTF-8 BOM, NaN, and integers past i64
    yield _aur_case("\ufeff" + json.dumps({"results": [
        {"Name": "app", "Version": "1", "NumVotes": 9, "Maintainer": "m",
         "URL": "https://github.com/owner/app"}]}))
    yield _aur_case('{"results": [{"Name": "app", "Version": "1", "NumVotes": 9, '
                    '"Popularity": NaN, "Maintainer": "m", "URL": ""}]}')
    yield _aur_case('{"results": [{"Name": "app", "Version": "1", '
                    '"NumVotes": 100000000000000000000, "Maintainer": "m", '
                    '"URL": "https://github.com/owner/app"}]}')
    # _aur: int() takes any Unicode decimal digits
    yield _aur_case({"results": [{"Name": "app", "Version": "1", "NumVotes": "１２",
                                  "Maintainer": "m", "URL": ""}]})
    # _aur: Version is whatever the RPC sent
    yield _aur_case({"results": [{"Name": "app", "Version": None, "NumVotes": 9,
                                  "Maintainer": "m", "URL": ""}]})
    yield _aur_case({"results": [{"Name": "app", "Version": 2, "NumVotes": 9,
                                  "Maintainer": "m", "URL": ""}]})
    # _aur: NumVotes 1e400 is inf to Python, int() raises
    yield _aur_case('{"results": [{"Name": "app", "NumVotes": 1e400}]}')

    # _sums: urllib reads data: and file: URLs; non-ASCII paths
    yield "_sums", {"assets": [_asset("SHA256SUMS", url="data:," + "a" * 64 + "%20%20app.AppImage")]}, \
        lambda: sources._sums([_asset("SHA256SUMS", url="data:," + "a" * 64 + "%20%20app.AppImage")])
    fa = [_asset("SHA256SUMS", url=f"file://{_SRV}/SHA256SUMS")]
    yield "_sums", {"assets": fa}, lambda: sources._sums(fa)
    ua = [_asset("ünï-sha256.txt")]
    yield "_sums", {"assets": ua}, lambda: sources._sums(ua)

    # _appimage: re.I in Python folds i with dotless ı (and s with ſ)
    for rel in [{"tag_name": "v1", "assets": [_asset("app-rıscv.AppImage", digest="sha256:" + "a" * 64)]},
                {"tag_name": "v1", "assets": [_asset("app-ı686.AppImage", digest="sha256:" + "a" * 64)]},
                {"tag_name": "v1", "assets": [_asset("app-riſcv.AppImage", digest="sha256:" + "a" * 64)]},
                # browser_download_url present but null
                {"tag_name": "v1", "assets": [_asset("app.AppImage", url=None, digest="sha256:" + "a" * 64)
                                              | {"browser_download_url": None}]}]:
        yield "_appimage", {"release": rel}, lambda r=rel: sources._appimage(r)

    # resolve: a homepage that is not a string only matters once the AUR answers
    m = {"name": "zz-no-such-almanac", "homepage": 5}
    yield ("resolve", {"link": "https://github.com/a/zz-no-such-almanac", "meta": m,
                       "release": None, "responses": {}, "raises_as_value": True},
           _raises_as_value(lambda: _resolve_case("https://github.com/a/zz-no-such-almanac", m)[2]()))
    # resolve: an AUR answer of the wrong shape raises in Python
    m2 = {"name": "zz-no-such-almanac"}
    resp = {_aur_url("zz-no-such-almanac"): '{"results": 5}'}
    yield ("resolve", {"link": "https://github.com/a/zz-no-such-almanac", "meta": m2,
                       "release": None, "responses": resp, "raises_as_value": True},
           _raises_as_value(lambda: _resolve_case("https://github.com/a/zz-no-such-almanac", m2, None, resp)[2]()))
    # resolve: values GitHub sent that are not strings are kept as they are
    for extra in [{"owner": {"login": 5}}, {"description": 5}, {"description": True},
                  {"license": {"spdx_id": 5}}, {"stargazers_count": "１２"}]:
        mm = dict({"name": "zz-no-such-almanac"}, **extra)
        yield _resolve_case("https://github.com/a/zz-no-such-almanac", mm)


_base_cases = cases


def cases():                                              # noqa: F811
    yield from _base_cases()
    yield from review_cases()


# ---- review round 2: the fixes, from more sides ------------------------------

def review2_cases():
    # _get: data: and file: URLs as urllib reads them
    for url in ["data:,hello%20world", "data:text/plain;base64,aGVsbG8=",
                "data:;base64,aGVs bG8=#frag", "data:,a#b", "DATA:,x",
                "data:;base64,aGk", "data:no-comma", "<URL:data:,wrapped>",
                f"file://{_SRV}/SHA256SUMS", f"file://localhost{_SRV}/SHA256SUMS#x",
                f"file://{_SRV}/%C3%BCn%C3%AF.txt", f"file://{_SRV}/missing",
                f"file://{_SRV}", "file://example.invalid/x",
                f"{BASE}/SHA256SUMS", f"{BASE}/missing", "http:///x", "gopher://x/y",
                "no scheme"]:
        yield "_get", {"url": url}, lambda u=url: sources._get(u)
    # _aur: odd numbers and values
    yield _aur_case('{"results": [{"Name": "app", "NumVotes": NaN}]}')
    yield _aur_case('{"results": [{"Name": "app", "NumVotes": -Infinity}]}')
    yield _aur_case('{"results": [{"Name": "app", "NumVotes": 1e19, "Maintainer": "m"}]}')
    yield _aur_case('{"results": [{"Name": "app", "NumVotes": -1e30, "Maintainer": "m"}]}')
    yield _aur_case('{"results": [{"Name": "app", "NumVotes": -100000000000000000000}]}')
    yield _aur_case('{"results": [{"Name": "app", "NumVotes": 4.99, "Version": [1, "a"]}]}')
    yield _aur_case('{"results": [{"Name": "app", "NumVotes": " -٣ ", "Version": {"a": null}}]}')
    yield _aur_case('{"results": [{"Name": "app", "NumVotes": "1" , "Name": "app-git"}]}')
    yield _aur_case('{"results": [{"Name": "app", "URL": ["x"]}]}')
    yield _aur_case('{"results": [{"Name": "app", "URL": 0, "Version": false}]}')
    yield _aur_case('{"results": {"a": 1}}')
    yield _aur_case('{"results": "ab"}')
    yield _aur_case('{"results": null}')
    yield _aur_case('"just a string"')
    yield _aur_case('{"results": []} x')
    yield _aur_case('{"results": [], }')
    yield _aur_case(' \n{"results": [{"Name": "a\\u00fc\\ud83d\\ude00", "NumVotes": 7}]}\n')
    yield _aur_case("﻿﻿{}")
    yield _aur_case('{"results": [{"Name": "app", "NumVotes": 1' + "0" * 5000 + '}]}')
    # _appimage: odd values
    for rel in [
        {"tag_name": "v1", "assets": [_asset("app.AppImage", url=5, digest="sha256:" + "a" * 64)]},
        {"tag_name": "v1", "assets": [_asset("app.AppImage", url=False)
                                      | {"browser_download_url": ["x"]}]},
        {"tag_name": "v1", "assets": [_asset("app-İ386.AppImage"), _asset("APP-X86_64.AppImage", 10**20)]},
        {"tag_name": "v1", "assets": {"app.AppImage": 1}},
        {"tag_name": "v1", "assets": "ab"},
    ]:
        yield "_appimage", {"release": rel}, lambda r=rel: sources._appimage(r)
    # resolve: values kept as GitHub sent them, and raises
    for extra in [{"stargazers_count": 10**20}, {"stargazers_count": 1.9e3},
                  {"owner": {"login": ["x"]}}, {"description": {"a": 1}},
                  {"license": {"spdx_id": None}}, {"homepage": "https://x.org"}]:
        mm = dict({"name": "zz-no-such-almanac"}, **extra)
        yield _resolve_case("https://github.com/a/zz-no-such-almanac", mm)
    m3 = {"name": "zz-no-such-almanac", "homepage": 5}
    resp3 = {_aur_url("zz-no-such-almanac"): '{"results": []}'}
    for meta, resp in [(m3, resp3), ({"name": "zz-no-such-almanac", "stargazers_count": "x"}, {}),
                       ({"name": 5}, {}), ({"name": "zz-no-such-almanac", "owner": 5}, {})]:
        yield ("resolve", {"link": "https://github.com/a/zz-no-such-almanac", "meta": meta,
                           "release": None, "responses": resp, "raises_as_value": True},
               _raises_as_value(lambda m=meta, r=resp: _resolve_case(
                   "https://github.com/a/zz-no-such-almanac", m, None, r)[2]()))


_round1_cases = cases


def cases():                                              # noqa: F811
    yield from _round1_cases()
    yield from review2_cases()


# ---- review round 3 ------------------------------------------------------------

import socket as _socket

# checksum files whose names hold characters urllib sends as they are
for _n in ["a|b.sha256", "a{b}.sha256", 'q"q.sha256', "c^d.sha256", "e`f.sha256",
           "g\\h.sha256", "sp ace.sha256", "bad.sha256"]:
    (_SRV / _n).write_bytes(f"{'1' * 64}  app.AppImage\n".encode())
(_SRV / "bad.sha256").write_bytes(b"2" * 64 + b"  x\xe2\x82\xf0\x9f.AppImage\n"
                                  + b"3" * 64 + b"  y\xed\xa0\x80z\xc0\xaf.AppImage\n")
_PORT = _server.server_address[1]


def _seeded_install_case(tag, seed, res, opt):
    """install_appimage over an AppImage registry that already holds `seed`
    (raw text), on both sides."""
    for side in ("py", "rs"):
        p = _dir("install", tag, side, "state", "almanac")
        (p / "appimages.json").write_text(seed)
    return _install_case(tag, res, opt)


def review3_cases():
    # _get: characters urllib passes through untouched, userinfo, odd forms
    for url in [f"{BASE}/a|b.sha256", f"{BASE}/a{{b}}.sha256", f'{BASE}/q"q.sha256',
                f"{BASE}/c^d.sha256", f"{BASE}/e`f.sha256", f"{BASE}/g\\h.sha256",
                f"{BASE}/sp ace.sha256", f"{BASE}/sp%20ace.sha256",
                f"http://u:p@127.0.0.1:{_PORT}/SHA256SUMS",
                f"HTTP://127.0.0.1:{_PORT}/SHA256SUMS",
                f"http://127.0.0.1:{_PORT}?x", f"http://127.0.0.1:{_PORT}#x",
                f"file://127.0.0.1{_SRV}/SHA256SUMS",
                f"file://{_socket.gethostname()}{_SRV}/SHA256SUMS",
                f"file://LOCALHOST{_SRV}/SHA256SUMS",
                f"file://{_SRV}/SHA256SUMS?q=1"]:
        yield "_get", {"url": url}, lambda u=url: sources._get(u)
    # _sums: such a checksum file, and one that is not valid UTF-8
    for name in ["a|b.sha256", "a{b}.sha256", 'q"q.sha256', "c^d.sha256",
                 "e`f.sha256", "g\\h.sha256", "bad.sha256"]:
        aa = [_asset(name)]
        yield "_sums", {"assets": aa}, lambda a=aa: sources._sums(a)
    # _appimage: its checksum only in such a file
    rel = {"tag_name": "v1", "assets": [_asset("app.AppImage"), _asset("a|b.sha256")]}
    yield "_appimage", {"release": rel}, lambda r=rel: sources._appimage(r)

    # parse/_norm: letters new in Unicode 16 (Garay)
    for link in ["https://github.com/a/\U00010D50", "https://github.com/\U00010D70/b"]:
        yield "parse", {"link": link}, lambda l=link: sources.parse(l)
    yield "_norm", {"url": "\U00010D50.org"}, lambda: sources._norm("\U00010D50.org")

    # resolve: an owner login that is not a string is formatted with repr()
    # into res.url, which _aur compares against the AUR's URL
    for login in [["᠎"], ["͸"], ["\U000E0001"]]:
        m = {"name": "zz-no-such-almanac", "owner": {"login": login}}
        url = f"https://github.com/{login}/zz-no-such-almanac"
        resp = {_aur_url("zz-no-such-almanac"): json.dumps({"results": [
            {"Name": "zz-no-such-almanac", "NumVotes": 9, "Maintainer": "m", "URL": url}]})}
        yield _resolve_case("https://github.com/a/zz-no-such-almanac", m, None, resp)

    # install_appimage over a registry Python reads but serde does not
    opt = {"kind": "appimage", "name": "app-1-x86_64.AppImage", "version": "1",
           "title": "AppImage", "url": f"{BASE}/app.AppImage", "sha256": _APP_SHA,
           "size_mb": 2.5}
    res = {"owner": "o", "repo": "app", "description": "d"}
    yield _seeded_install_case("reglist", "[1]\n", res, opt)
    yield _seeded_install_case("regnan", '{"other": {"name": "other", "checked_at": NaN}}\n',
                               res, opt)
    yield _seeded_install_case("regbig", '{"other": {"name": "other", "checked_at": 100000000000000000000000}}\n',
                               res, opt)
    # ...and the registry text each side left on disk (the Rust side runs
    # its cases in order, after its install above)
    py_reg = Path(_TMP, "install", "regbig", "py", "state", "almanac", "appimages.json")
    rs_root = Path(_TMP, "install", "regbig", "rs")
    yield ("_get", {"url": f"file://{rs_root}/state/almanac/appimages.json"},
           lambda: py_reg.read_text().replace(str(py_reg.parents[2]), str(rs_root)))


_round2_cases = cases


def cases():                                              # noqa: F811
    yield from _round2_cases()
    yield from review3_cases()
