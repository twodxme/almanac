"""Parity cases for inventory.py.

`env` in the args is applied on both sides for the duration of the call
(None removes the variable), and on the Python side the module-level paths
inventory computed at import (STATE, REGISTRY, _GIT_CACHE, _REGISTRY_CACHE)
are recomputed from it. `files` ({path: text | None}) is written or removed
first, on both sides, so a call that writes state starts each side from the
same place.

Package-manager output is either the real machine (read-only queries) or a
fake `pacman`/`checkupdates`/`yay`/`flatpak` early on PATH that prints a
fixture per first argument. Nothing here writes outside a temp dir: the
security tracker and git caches are pointed at temp copies, AppImage checks
run only against temp registries, and the real checkout is read with
GIT_OPTIONAL_LOCKS=0.
"""
import atexit
import json
import os
import shutil
import subprocess
import tempfile
import time
from pathlib import Path

import inventory

_TMP = Path(tempfile.mkdtemp(prefix="almanac-parity-inventory-"))
atexit.register(shutil.rmtree, str(_TMP), True)
_n = 0
_PATH = os.environ.get("PATH", "")
_HOME = str(Path.home())
_RO = {"GIT_OPTIONAL_LOCKS": "0"}


def _dir(tag):
    global _n
    _n += 1
    d = _TMP / f"{_n}-{tag}"
    d.mkdir(parents=True)
    return d


def _paths():
    inventory.STATE = Path(
        os.environ.get("XDG_STATE_HOME", Path.home() / ".local/state")) / "almanac"
    inventory.REGISTRY = inventory.STATE / "appimages.json"
    cache = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache"))
    inventory._GIT_CACHE = cache / "almanac/git.json"
    inventory._REGISTRY_CACHE = cache / "almanac/registry.json"


def _call(env, files, fn):
    def run():
        saved = {k: os.environ.get(k) for k in env}
        attrs = {k: getattr(inventory, k)
                 for k in ("STATE", "REGISTRY", "_GIT_CACHE", "_REGISTRY_CACHE")}
        try:
            for k, v in env.items():
                if v is None:
                    os.environ.pop(k, None)
                else:
                    os.environ[k] = v
            _paths()
            for p, text in files.items():
                if text is None:
                    Path(p).unlink(missing_ok=True)
                else:
                    Path(p).parent.mkdir(parents=True, exist_ok=True)
                    Path(p).write_text(text)
            return fn()
        finally:
            for k, v in saved.items():
                if v is None:
                    os.environ.pop(k, None)
                else:
                    os.environ[k] = v
            for k, v in attrs.items():
                setattr(inventory, k, v)
    return run


def _case(func, args, fn, env=None, files=None):
    a = dict(args)
    env = env or {}
    files = {str(k): v for k, v in (files or {}).items()}
    if env:
        a["env"] = env
    if files:
        a["files"] = files
    return func, a, _call(env, files, fn)


def _git(*a, cwd):
    subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@t", *a],
                   cwd=cwd, check=True, capture_output=True)


def _mask(v):
    """fetched_at is "now" on each side: compare only whether it is recent."""
    if isinstance(v, dict):
        out = {}
        for k, x in v.items():
            if k == "fetched_at" and isinstance(x, (int, float)) and not isinstance(x, bool):
                out[k] = "recent" if x > time.time() - 600 else "old"
            else:
                out[k] = _mask(x)
        return out
    return v


def _sorted(apps):
    return sorted(apps, key=lambda a: (a.extra.get("real_source", a.source), a.name))


def _apps_json(apps):
    return [{"name": a.name, "version": a.version, "source": a.source} for a in apps]


# ------------------------------------------------------------ fake commands --
def _fakebin(outputs):
    """A bin dir whose pacman/checkupdates/yay/flatpak print outputs[cmd][arg1]."""
    d = _dir("bin")
    for cmd, by_arg in outputs.items():
        for arg, text in by_arg.items():
            (d / f"{cmd}.out{arg}").write_text(text)
        script = d / cmd
        script.write_text(
            '#!/bin/sh\nf="$(dirname "$0")/%s.out$1"\n[ -f "$f" ] && cat "$f"\nexit 0\n' % cmd)
        script.chmod(0o755)
    return {"PATH": f"{d}:{_PATH}"}


_APPS = _dir("desktop") / "share/applications"
_APPS.mkdir(parents=True)
(_APPS / "foo.desktop").write_text(
    "[Desktop Entry]\nName=Foo\nIcon= foo-icon \nCategories=Utility;\nIcon=second\n")
(_APPS / "helper.desktop").write_text("[Desktop Entry]\nIcon=h\nNoDisplay=true\nCategories=X;\n")
(_APPS / "cats.desktop").write_text("Categories=Game;\r\nName=Ünï 日本語\n")
(_APPS / "bad.desktop").write_bytes(b"Icon=caf\xe9\xff\nCategories=\n")
(_APPS / "empty.desktop").write_text("[Desktop Entry]\nName=Nothing\n")

_QI = f"""Name            : foo
Version         : 1.0-1
Description     : A foo tool
                  that wraps
Architecture    : x86_64
URL             : https://foo.example
Licenses        : MIT
Groups          : None
Installed Size  : 1737.27 KiB
Depends On      : glibc  bar
                  baz
Build Date      : Fri 16 Oct 2026 04:08:05 PM PDT
Install Date    : Sat 17 Oct 2026 09:00:00 AM PDT
Validated By    : Signature

Name            : bär-ünï
Version         : 2
Description     : Ünïcödé 日本語
Installed Size  : 3.5 GiB
Build Date      : Mon 01 Jan 2024 12:00:00 AM UTC

Name            : libdep
Description     : a library
Installed Size  : 12 MiB
Required By     : foo
Optional Deps   : None

Name            :
Description     : no name
Weird line without colon
 continuation with: colon

Name            : ttf-old
Description     : a font
Build Date      : Mon 01 Jan 2018 12:00:00 AM UTC

Name            : auronly
Description     : from the AUR
Build Date      : Mon 01 Jan 2018 12:00:00 AM UTC
"""
_QL = f"""foo /usr/bin/foo
foo {_APPS}/helper.desktop
foo {_APPS}/foo.desktop
libdep /usr/lib/libdep.so
bär-ünï {_APPS}/cats.desktop
ttf-old {_APPS}/bad.desktop
auronly {_APPS}/missing.desktop
auronly {_APPS}/empty.desktop
auronly /usr/share/applications/
nospace
"""
_FAKE = _fakebin({
    "pacman": {
        "-Qi": _QI, "-Ql": _QL, "-Qlq": "/usr/bin/foo\n/usr/share/foo\n\n",
        "-Qentq": "foo\nbär-ünï\nomarchy-pkg\nttf-old\n",
        "-Qemq": "auronly\n",
        "-Qmq": "auronly\nauropending\n",
        "-Q": "foo 1.0-1\nbär-ünï 2\nlibdep 0.9-1\nomarchy-pkg 3.1.0-1\nauronly 1.0\n"
              "auropending 0.1\nttf-old 1\n\n",
        "-Qdtq": "libdep\nrust\nlibfoo-docs\n",
        "-Qq": "foo\nbar\n",
        "-Si": "Repository      : extra\nName            : foo\nVersion         : 1.2-1\n",
        "-Ss": "extra/foo 1.0-1 [installed]\n    A foo tool\nextra/newthing 2.0-1\n"
                "    A new thing\ncore/ünï 1\n    Unicode thing\n\nlonely\n"
                "extra/nodesc 1\nextra/twice 1\n    first\n    second\n",
    },
    "checkupdates": {"": "foo 1.0-1 -> 1.2-1\nlibdep 0.9-1 -> 1.0-1\nomarchy-pkg 3.1.0-1 -> 3.2.0-1\n"
                         "bad line\nbär-ünï 2 -> 2\n"},
    "yay": {"-Qua": "auronly 1.0 -> 2.0\nauropending 0.1 -> 0.2\n:: junk\n"},
    "flatpak": {
        "list": "org.foo.App\tFoo\t1.2\norg.bar.Bar\t\t\n\t\t\norg.baz.Baz\n"
                "org.ünï.App\tÜnï\t3\n",
        "remote-ls": "org.foo.App\n\norg.baz.Baz\n",
    },
})
_EMPTY = _fakebin({"pacman": {}, "checkupdates": {}, "yay": {}, "flatpak": {}})

# The security tracker cache: a temp copy of the real one (or empty, in which
# case Python fetches it into the temp dir first and Rust reads that).
_SEC = _dir("seccache")
_real_sec = Path.home() / ".cache/almanac/security.json"
if _real_sec.exists():
    (_SEC / "almanac").mkdir()
    shutil.copy(_real_sec, _SEC / "almanac/security.json")

# ---------------------------------------------------------------- appimages --
_AI = _dir("appimages")
(_AI / "apps").mkdir()
(_AI / "apps/live.AppImage").write_bytes(b"x" * 3000)
(_AI / "apps/other.AppImage").write_bytes(b"y")
os.utime(_AI / "apps/live.AppImage", (1700000000.5, 1700000000.5))
_REG = {
    "zeta": {"name": "zeta", "path": str(_AI / "apps/live.AppImage"), "version": "1.0",
             "description": "Zeta — ünï", "url": "https://github.com/a/zeta",
             "latest": {"version": "1.1", "url": "https://dl/zeta", "published": "2026-01-01",
                        "page": "https://github.com/a/zeta/releases"},
             "checked_at": time.time()},
    "alpha": {"path": str(_AI / "apps/missing.AppImage")},
    "Beta": {"name": "beta", "path": str(_AI / "apps//other.AppImage/"),
             "url": "", "latest": {"page": "https://page", "version": ""},
             "checked_at": "12"},
    "nopath": {"name": "np", "version": "", "description": ""},
    "日本": {"name": "日本", "path": "relative/thing.AppImage", "latest": None},
}
_STALE = {
    "stale": {"name": "stale", "path": str(_AI / "apps/other.AppImage"), "version": "1.0",
              "checked_at": 0},
}
_BROKEN = {
    "broken": {"name": "broken", "path": str(_AI / "apps/other.AppImage"),
               "update_spec": {"kind": "github"}, "checked_at": 0},
}


def _state_env(tag):
    d = _dir(tag)
    return d, {"XDG_STATE_HOME": str(d)}


# ---------------------------------------------------------------- git repos --
def _upstream_and_clone(tag):
    d = _dir(tag)
    up, clone = d / "up", d / "clone"
    _git("init", "-q", "-b", "main", str(up), cwd=d)
    _git("commit", "-q", "--allow-empty", "-m", "one", cwd=up)
    _git("clone", "-q", str(up), str(clone), cwd=d)
    _git("commit", "-q", "--allow-empty", "-m", "two — ünïcödé subject " + "x" * 100, cwd=up)
    _git("fetch", "-q", cwd=clone)
    return up, clone


_UP, _CLONE = _upstream_and_clone("behind")
_LONE = _dir("lone")
_git("init", "-q", str(_LONE), cwd=_LONE)
_NOTREPO = _dir("notrepo")

# local_builds: a data home with checkouts, a home with a launcher, a config
# home with Omarchy plugins.
_LB = _dir("localbuilds")
_DATA = _LB / "data"
_DATA.mkdir()
_git("clone", "-q", str(_UP), str(_DATA / "driver"), cwd=_LB)
_git("reset", "-q", "--hard", "HEAD~1", cwd=_DATA / "driver")
_git("init", "-q", str(_DATA / ".hidden-tool"), cwd=_LB)
_git("init", "-q", str(_DATA / "a-b"), cwd=_LB)
_git("init", "-q", str(_DATA / "a"), cwd=_LB)
(_DATA / "plain-dir").mkdir()
(_DATA / "afile").write_text("x")
(_DATA / "gitfile").mkdir()
(_DATA / "gitfile/.git").write_text("gitdir: /nonexistent\n")
_LBHOME = _LB / "home"
(_LBHOME / ".local/bin").mkdir(parents=True)
(_LBHOME / ".local/bin/driver").write_text("#!/bin/sh\n")
_CFG = _LB / "config"
_PLUG = _CFG / "omarchy/plugins"
(_PLUG / "with-manifest").mkdir(parents=True)
(_PLUG / "with-manifest/manifest.json").write_text(json.dumps(
    {"id": "me.with-manifest", "description": "Shows ünï things", "version": "0.3.0"}))
(_PLUG / "broken-manifest").mkdir()
(_PLUG / "broken-manifest/manifest.json").write_text("{ nope")
(_PLUG / "no-manifest").mkdir()
(_PLUG / "a-file").write_text("x")
_LBCACHE = _LB / "cache"
_LB_ENV = {"XDG_DATA_HOME": str(_DATA), "XDG_CONFIG_HOME": str(_CFG),
           "XDG_CACHE_HOME": str(_LBCACHE), "HOME": str(_LBHOME), **_RO}

# ---------------------------------------------------------- plugin registry --
_PR = _dir("pluginreg")
(_PR / "cache/almanac").mkdir(parents=True)
_FIXREG = {
    "retiredPluginIds": ["gone"],
    "sources": [
        {"repo": "https://github.com/alice/zed-plugin/", "type": "plugin", "addedAt": "2026-03-01T00:00"},
        {"repo": "https://github.com/bob/Alpha", "type": "suite",
         "listingValidatedAt": "2026-01-01", "addedAt": "2026-02-01"},
        {"repo": "https://github.com/carol/gone"},
        {"repo": "https://github.com/dan/cat-list", "catalog": [{"name": "Listed", "description": "From a list", "author": "Dan"}],
         "addedAt": "2025-12-31"},
        {"repo": "https://github.com/eve/cat-dict", "catalog": {"name": "Dicted", "author": "Eve"},
         "listingValidatedAt": "x", "addedAt": "2026-05-05"},
        {"repo": "https://github.com/fay/cat-noname", "catalog": {"description": "nameless"}},
        {"repo": "lonely"},
        {"repo": ""},
        {"type": "plugin"},
        {"repo": "https://github.com/ünï/日本語-plugin", "addedAt": "2026-04-04"},
        {"repo": "https://github.com/me/installed-one", "addedAt": "2026-04-04", "catalog": []},
        {"repo": "https://github.com/x/foo-1", "addedAt": "2026-01-01"},
        {"repo": "https://github.com/x/foo-2", "addedAt": "2026-01-02"},
        {"repo": "https://github.com/x/foo-3", "addedAt": "2026-01-03"},
        {"repo": "https://github.com/x/foo-4", "addedAt": "2026-01-04"},
        {"repo": "https://github.com/x/foo-5", "addedAt": "2026-01-05"},
        {"repo": "https://github.com/x/foo-6", "addedAt": "2026-01-06"},
        {"repo": "https://github.com/x/foo-7", "addedAt": "2026-01-07"},
    ],
}
(_PR / "cache/almanac/registry.json").write_text(json.dumps(_FIXREG))
(_PR / "config/omarchy/plugins/installed-one").mkdir(parents=True)
(_PR / "config/omarchy/plugins/zed-plugin").write_text("not a dir")
_PR_ENV = {"XDG_CACHE_HOME": str(_PR / "cache"), "XDG_CONFIG_HOME": str(_PR / "config")}

_REALREG = _dir("realreg")
_real_reg = Path.home() / ".cache/almanac/registry.json"
if _real_reg.exists():
    (_REALREG / "almanac").mkdir()
    shutil.copy(_real_reg, _REALREG / "almanac/registry.json")
_REALREG_ENV = {"XDG_CACHE_HOME": str(_REALREG)}


def _real_explicit():
    out = subprocess.run(["pacman", "-Qeq"], capture_output=True, text=True).stdout.split()
    return out


def cases():
    # ---- pure functions
    for name in ("google-chrome", "openssh", "nordvpn-bin", "linux", "mkinitcpio", "bat",
                 "Firefox", "FIREFOX-BIN", "curl-git", "linux-git-bin", "systemd-bin-git",
                 "", "ünï", "OpenSSL"):
        yield "criticality", {"name": name}, lambda n=name: inventory.criticality(n)

    for name in ("rust", "python-build", "python-installer", "cmake", "logiops-debug",
                 "foo-docs", "libfoo", "", "Rust", "x-debug-x", "日本-docs"):
        yield "orphan_advice", {"name": name}, lambda n=name: inventory.orphan_advice(n)

    for cur, lat in (("1.2.9", "3.0.0"), ("0.6.0", "0.12.0"), ("1.2.3", "1.2.9"),
                     ("0.29.0-1", "0.29.0-2"), ("1:1.2.96.518-2", "1:1.3.4.258-1"),
                     ("20181225-1", "20261002.r63-3"), ("2026.10.16", "2026.10.17"),
                     ("", "1.0"), ("git-abc123", "git-def456"), ("1.2.3.4", "1.2.3.6"),
                     ("1.2", "1.2.0.1"), ("3.0", "2.9"), ("1.2a.3", "1.3"), ("v1.2", "1.3"),
                     ("1~rc1", "1"), ("1+b", "2+c"), ("1:2:3.4", "3.5"), ("2026.13.1", "2026.14.1"),
                     ("1980.1", "1980.2"), ("2026.10.32", "2026.10.33"), ("100001", "1"),
                     ("1.0", "1.0"), ("ünï1.0", "1.0"), ("1.ünï", "2"), ("12345678901234567890.1", "1.2"),
                     ("١.٢", "١.٥"), ("1.２", "1.９")):
        # (A difference beyond 64 bits -- "1.99999999999999999999" -> "1.1" --
        # is a Python big int; the port saturates at i64.)
        yield ("versions_behind", {"current": cur, "latest": lat},
               lambda c=cur, l=lat: inventory.versions_behind(c, l))

    for v in ("1737.27 KiB", "10 MiB", "1 GiB", "512 B", "5 TiB", "abc KiB", "5", "",
              "1_000 KiB", "1__0 KiB", "_1 KiB", "1.5e3 KiB", "  2   MiB  extra", "-3 MiB",
              "２ MiB", "1,5 MiB", "+.5 GiB", "0x10 MiB"):
        yield "size_mb", {"value": v}, lambda v=v: inventory.size_mb(v)

    for v in ("Fri 16 Oct 2026 04:08:05 PM PDT", "Sat 17 Oct 2026", "16 Oct 2026",
              "Fri 16 oct 2026 4:08:05 pm", "Fri 31 Feb 2026", "Fri 16 Oct 2026 13:08:05 PM",
              "Fri 16 Oct 2026 12:00:00 AM", "Fri 16 Oct 2026 12:30:00 PM",
              "Sun 08 Mar 2026 02:30:00 AM", "Sun 01 Nov 2026 01:30:00 AM",
              "Sun 29 Mar 2026 02:30:00 AM", "", "   ", "Fri 16 Okt 2026", "Fri 16 Oct 0000",
              "Fri 16 Oct 2026 04:08:61 PM", "Fri 16 Oct 2026 04:08:60 PM",
              "Fri 16 October 2026", "Fri 6 Oct 2026 4:8:5 AM", "x 16 Oct 2026 04:08:05 PM extra",
              "Fri 16 Oct 2026 04:08 PM", "Fri 016 Oct 2026", "Fri 16 Oct 26", "Fri 16 OCT 2026",
              "Fri\t16 Oct  2026", "Fri 16 Oct 2026 04:08:05 PM", "日本 16 Oct 2026",
              "Fri 0 Oct 2026", "Fri 29 Feb 2024", "Fri 29 Feb 2025", "Fri ١٦ Oct ٢٠٢٦",
              "Fri １６ Oct ２０２６ 0４:08:05 PM"):
        yield "date_ts", {"value": v}, lambda v=v: inventory.date_ts(v)

    for url in ("https://github.com/a/b.git", "git@github.com:a/b.git", "  https://x/y  ",
                "http://insecure/a", "", "git@gitlab.com:a/b", "https://github.com/a/b.git.git",
                "git@github.com:", "https://ünï/日本.git", "ssh://git@github.com/a/b"):
        yield "github_page", {"url": url}, lambda u=url: inventory._github_page(u)

    for entry in ({"repo": "https://github.com/a/b"}, {"repo": "https://github.com/a/b/"},
                  {"repo": "b"}, {"repo": ""}, {}, {"repo": None},
                  {"catalog": [{"name": "N", "description": "D", "author": "A"}], "repo": "x/y"},
                  {"catalog": [], "repo": "x/y"}, {"catalog": {"name": ""}, "repo": "x/y"},
                  {"catalog": {"name": "Only"}}, {"catalog": [{"description": "d"}], "repo": "q/r"},
                  {"catalog": "str", "repo": "a//"}, {"repo": "https://github.com/ünï/日本語"},
                  {"repo": "///"}):
        yield "plugin_name", {"entry": entry}, lambda e=entry: inventory._plugin_name(e)

    for apps in ([("cursor", "repo"), ("cursor-bin", "aur")],
                 [("foo", "repo"), ("foo", "orphan")],
                 [("firefox", "repo"), ("chromium", "repo")],
                 [("Zed-Desktop", "repo"), ("zed-git", "aur"), ("zed-bin-git", "aur"),
                  ("zed", "appimage"), ("Ünï", "repo"), ("ünï-AppImage", "appimage"),
                  ("a-bin-bin", "aur"), ("a-bin", "aur"), ("x", "orphan"), ("x-bin", "orphan")],
                 []):
        objs = [{"name": n, "version": "1", "source": s} for n, s in apps]
        yield ("duplicates", {"apps": objs},
               lambda o=objs: inventory.duplicates([inventory.App(**x) for x in o]))

    old = "Mon 01 Jan 2018 12:00:00 AM UTC"
    new = time.strftime("%a %d %b %Y 12:00:00 AM UTC", time.localtime(time.time() - 86400))
    aur = {"orph": {"out_of_date": False, "last_modified": 1.0, "maintainer": ""},
           "flag": {"out_of_date": True, "last_modified": 1.0, "maintainer": "m"},
           "fine": {"out_of_date": False, "last_modified": 1.0, "maintainer": "m"}}
    app_desk = {"icon": "x", "categories": ""}
    for name, source, desk, meta in (
            ("orph", "aur", {}, {}), ("orph", "repo", {}, {}), ("flag", "repo", {}, {}),
            ("fine", "aur", app_desk, {"built": old}), ("fine", "aur", {}, {"built": old}),
            ("ttf-x", "repo", app_desk, {"built": old}), ("Noto-Fonts", "repo", app_desk, {"built": old}),
            ("papirus-icon-theme", "repo", app_desk, {"built": old}),
            ("app", "repo", {"icon": "", "categories": "Game;"}, {"built": old}),
            ("app", "repo", app_desk, {"built": new}), ("app", "repo", app_desk, {}),
            ("app", "repo", app_desk, {"built": "garbage"}), ("ünï", "repo", app_desk, {"built": old})):
        yield ("staleness", {"name": name, "source": source, "aur": aur, "desk": desk, "meta": meta},
               lambda a=(name, source, desk, meta): inventory._staleness(a[0], a[1], aur, a[2], a[3]))

    # ---- fake package managers
    for names in (["foo"], ["foo", "x"], []):
        yield ("package_info", {"names": names, "env": _FAKE},
               _call(_FAKE, {}, lambda n=names: inventory._package_info(n)))
        yield ("desktop_map", {"names": names, "env": _FAKE},
               _call(_FAKE, {}, lambda n=names: inventory._desktop_map(n)))
    yield "pending_repo", {"env": _FAKE}, _call(_FAKE, {}, inventory._pending_repo)
    yield "pending_aur", {"env": _FAKE}, _call(_FAKE, {}, inventory._pending_aur)
    yield "synced_version", {"name": "foo", "env": _FAKE}, _call(_FAKE, {}, lambda: inventory.synced_version("foo"))
    yield "orphans", {"env": _FAKE}, _call(_FAKE, {}, inventory.orphans)
    yield "flatpaks", {"env": _FAKE}, _call(_FAKE, {}, inventory.flatpaks)
    yield "packages", {"sort": True, "env": {**_FAKE, "XDG_CACHE_HOME": str(_SEC)}}, \
        _call({**_FAKE, "XDG_CACHE_HOME": str(_SEC)}, {}, lambda: _sorted(inventory.packages()))
    for app in ({"name": "foo", "version": "1", "source": "repo", "extra": {"k": [1, "ü"]}},
                {"name": "foo", "version": "1", "source": "orphan"},
                {"name": "foo", "version": "1", "source": "flatpak", "extra": {"a": 1}}):
        yield ("details", {"app": app, "env": _FAKE},
               _call(_FAKE, {}, lambda a=app: inventory.details(inventory.App(**a))))
    for q in ("foo", " fo ", "x"):
        yield ("search_available", {"query": q, "limit": 60, "only_origin": "repo", "env": _FAKE},
               _call(_FAKE, {}, lambda q=q: [a for a in inventory.search_available(q)
                                             if a.extra.get("origin") == "repo"]))
    yield ("search_available", {"query": "ab", "limit": 2, "env": _EMPTY},
           _call(_EMPTY, {}, lambda: inventory.search_available("ab", 2)))
    for fn in ("orphans", "flatpaks", "pending_repo", "pending_aur", "packages"):
        py = {"pending_repo": inventory._pending_repo, "pending_aur": inventory._pending_aur}.get(
            fn, getattr(inventory, fn, None))
        env = {**_EMPTY, "XDG_CACHE_HOME": str(_SEC)}
        yield fn, {"env": env}, _call(env, {}, py)

    # ---- AppImage registry
    d, env = _state_env("reg-load")
    target = d / "almanac/appimages.json"
    # (A registry that is valid JSON but not an object -- "[1, 2]", "null" --
    # is handed on as-is by the Python and crashes every caller's .items();
    # the port reads it as {}.)
    for text in ('{"a": {"name": "a"}}', "{ this is not json", "",
                 '{"ü": {"name": "日本"}}', None):
        yield _case("load_registry", {}, inventory._load_registry, env, {target: text})

    def save_and_read(data):
        def run():
            inventory.save_registry(data)
            return {"text": inventory.REGISTRY.read_text(),
                    "tmp": sorted(p.name for p in inventory.STATE.glob("*.tmp"))}
        return run
    for data in ({"a": {"name": "a"}}, {}, {"ü": {"x": [1, 2.5, None, True], "日本": "—"}}):
        d, env = _state_env("reg-save")
        yield _case("save_registry", {"data": data}, save_and_read(data), env)
    blocker = _dir("reg-blocked") / "file"
    blocker.write_text("x")
    yield _case("save_registry", {"data": {"a": 1}}, save_and_read({"a": 1}),
                {"XDG_STATE_HOME": str(blocker)})

    for reg, check in ((_REG, False), (_REG, True), (_STALE, False), (_STALE, True),
                       (_BROKEN, False), (_BROKEN, True), ({}, True)):
        d, env = _state_env("ai")
        target = d / "almanac/appimages.json"
        yield _case("appimages", {"check_updates": check},
                    lambda c=check: inventory.appimages(c), env, {target: json.dumps(reg)})

    # ---- git
    def behind(repo, cache, mask=False):
        def run():
            c = json.loads(json.dumps(cache))
            r = inventory._git_behind(Path(repo), c)
            out = {"result": r, "cache": c}
            return _mask(out) if mask else out
        return run
    recent = {"fetched_at": time.time() - 60}
    for repo, cache, mask in (
            (_CLONE, {str(_CLONE): recent}, False),
            (_CLONE, {}, True),
            (_CLONE, {str(_CLONE): {"fetched_at": "garbage"}, "other": 1}, True),
            (_LONE, {}, False),
            (_NOTREPO, {str(_NOTREPO): {"behind": 3, "fetched_at": 5}}, False),
            (_TMP / "does-not-exist", {}, False)):
        yield ("git_behind", {"repo": str(repo), "cache": cache, "mask_time": mask},
               behind(repo, cache, mask))

    for text in ('{"/x": {"tracked": false, "fetched_at": 1.5}, "_plugins": {}}', "garbage", None):
        d = _dir("gitcache")
        yield _case("git_cache", {}, inventory._git_cache, {"XDG_CACHE_HOME": str(d)},
                    {d / "almanac/git.json": text})

    def builds(check):
        def run():
            apps = inventory.local_builds(check_updates=check)
            if not check:
                return apps
            cache = json.loads(inventory._GIT_CACHE.read_text()) if inventory._GIT_CACHE.exists() else None
            return {"apps": [_mask(json.loads(json.dumps(a.__dict__))) for a in apps],
                    "cache": _mask(cache)}
        return run
    for check in (False, True):
        yield _case("local_builds", {"check_updates": check, "mask_time": check}, builds(check),
                    _LB_ENV, {_LBCACHE / "almanac/git.json": None})

    # ---- plugin registry
    yield _case("plugin_registry", {"force": False}, lambda: inventory.plugin_registry(), _PR_ENV)
    d = _dir("prnull")
    yield _case("plugin_registry", {"force": False}, lambda: inventory.plugin_registry(),
                {"XDG_CACHE_HOME": str(d)}, {d / "almanac/registry.json": "[]"})
    for q, limit in (("", 40), ("foo", 40), ("  FOO ", 2), ("FOO", 1), ("ünï", 40), ("listed", 40),
                     ("gone", 40), ("zzzz", 40), ("", 3), ("", 0)):
        yield _case("search_plugins", {"query": q, "limit": limit},
                    lambda q=q, l=limit: inventory.search_plugins(q, l), _PR_ENV)

    # ---- the real machine (read-only)
    if os.environ.get("ALMANAC_DEMO"):
        yield "collect", {}, inventory.collect
    names = _real_explicit()
    yield "package_info", {"names": names}, lambda: inventory._package_info(names)
    yield "desktop_map", {"names": names}, lambda: inventory._desktop_map(names)
    yield "synced_version", {"name": "bash"}, lambda: inventory.synced_version("bash")
    yield "synced_version", {"name": "no-such-pkg-xyz"}, lambda: inventory.synced_version("no-such-pkg-xyz")
    yield "orphans", {}, inventory.orphans
    yield "flatpaks", {}, inventory.flatpaks
    yield "pending_aur", {}, inventory._pending_aur
    yield _case("packages", {"sort": True}, lambda: _sorted(inventory.packages()),
                {"XDG_CACHE_HOME": str(_SEC)})
    for name, source in (("bash", "repo"), ("pacman", "system"), ("no-such-pkg-xyz", "repo"),
                         ("bash", "flatpak")):
        app = {"name": name, "version": "1", "source": source}
        yield "details", {"app": app}, lambda a=app: inventory.details(inventory.App(**a))
    yield "appimages", {"check_updates": False}, lambda: inventory.appimages(False)
    yield "load_registry", {}, inventory._load_registry
    yield "git_cache", {}, inventory._git_cache
    yield _case("local_builds", {"check_updates": False},
                lambda: inventory.local_builds(check_updates=False), dict(_RO))
    yield "aur_health", {"names": []}, lambda: inventory.aur_health([])
    yield ("aur_health", {"names": ["yay", "no-such-pkg-xyz", "ünï", "a b&c"]},
           lambda: inventory.aur_health(["yay", "no-such-pkg-xyz", "ünï", "a b&c"]))
    for q in ("firefox", "x", "", "  ", "日本語", "zzzqqqxxyy", "a/b c&d"):
        yield "search_available", {"query": q, "limit": 60}, lambda q=q: inventory.search_available(q)
    yield "search_available", {"query": "rust", "limit": 5}, lambda: inventory.search_available("rust", 5)
    if _real_reg.exists():
        for q in ("", "bar", "theme", "日本"):
            yield _case("search_plugins", {"query": q, "limit": 40},
                        lambda q=q: inventory.search_plugins(q), _REALREG_ENV)


# ------------------------------------------------- review round 1 (adversarial) --
def _fakebin_bytes(outputs):
    """Like _fakebin, but the fixtures are raw bytes (invalid UTF-8 allowed)."""
    d = _dir("binb")
    for cmd, by_arg in outputs.items():
        for arg, data in by_arg.items():
            (d / f"{cmd}.out{arg}").write_bytes(data)
        script = d / cmd
        script.write_text(
            '#!/bin/sh\nf="$(dirname "$0")/%s.out$1"\n[ -f "$f" ] && cat "$f"\nexit 0\n' % cmd)
        script.chmod(0o755)
    return {"PATH": f"{d}:{_PATH}"}


def _review_cases():
    # _run decodes with text=True: invalid UTF-8 is a UnicodeDecodeError that
    # _run does not catch.
    bad = _fakebin_bytes({"checkupdates": {"": b"caf\xe9 1 -> 2\n"},
                          "flatpak": {"list": b"org.x\tB\xffad\t1\n"}})
    yield "pending_repo", {"env": bad}, _call(bad, {}, inventory._pending_repo)
    yield "flatpaks", {"env": bad}, _call(bad, {}, inventory.flatpaks)

    # A "Version" line with no colon: IndexError in the Python.
    nocolon = _fakebin({"pacman": {"-Si": "Versionless line\nVersion : 9\n"}})
    yield ("synced_version", {"name": "x", "env": nocolon},
           _call(nocolon, {}, lambda: inventory.synced_version("x")))

    # A one-word `pacman -Q` line: dict() raises ValueError.
    oneword = _fakebin({"pacman": {"-Qdtq": "libdep\n", "-Q": "libdep\n", "-Qi": ""}})
    yield "orphans", {"env": oneword}, _call(oneword, {}, inventory.orphans)

    # details(): a field named "" is falsy, so the next indented line is
    # parsed as its own field rather than a continuation.
    emptyfield = _fakebin({"pacman": {"-Qi": ": x\n Depends On : glibc\n", "-Qlq": ""}})
    app = {"name": "foo", "version": "1", "source": "repo"}
    yield ("details", {"app": app, "env": emptyfield},
           _call(emptyfield, {}, lambda: inventory.details(inventory.App(**app))))

    # ---- malformed AppImage registries
    for tag, reg in (
            ("nondict", {"x": "just a string"}),
            ("nullpath", {"x": {"path": None}}),
            ("numpath", {"x": {"path": 5}}),
            ("strlatest", {"x": {"path": "/nonexistent/a.AppImage", "latest": "1.2"}}),
            ("types", {"x": {"name": None, "version": 2, "description": None,
                             "path": "/nonexistent/a.AppImage",
                             "latest": {"version": None, "url": None}}})):
        d, env = _state_env("ai-" + tag)
        yield _case("appimages", {"check_updates": False, "tag": tag},
                    lambda: inventory.appimages(False), env,
                    {d / "almanac/appimages.json": json.dumps(reg)})

    # save_registry failing after a check: the Python lets the OSError out.
    d, env = _state_env("ai-ro")
    (d / "almanac").mkdir()
    (d / "almanac/appimages.json").write_text(json.dumps(_STALE))
    (d / "almanac").chmod(0o555)
    atexit.register(lambda p=d / "almanac": p.chmod(0o755))
    yield _case("appimages", {"check_updates": True, "readonly": True},
                lambda: inventory.appimages(True), env)

    # Valid JSON that is not an object.
    d, env = _state_env("reg-list")
    yield _case("load_registry", {"list": True}, inventory._load_registry, env,
                {d / "almanac/appimages.json": "[1, 2]"})
    d = _dir("gitcache-list")
    yield _case("git_cache", {"list": True}, inventory._git_cache, {"XDG_CACHE_HOME": str(d)},
                {d / "almanac/git.json": "[]"})

    # local_builds with a git cache that is a list: AttributeError in the Python.
    yield _case("local_builds", {"check_updates": True, "listcache": True},
                lambda: inventory.local_builds(check_updates=True),
                _LB_ENV, {_LBCACHE / "almanac/git.json": "[]"})
    # ... and with "_plugins" not a dict.
    yield _case("local_builds", {"check_updates": True, "plugins_list": True},
                lambda: inventory.local_builds(check_updates=True),
                _LB_ENV, {_LBCACHE / "almanac/git.json": '{"_plugins": []}'})

    # _git_behind with a cache entry that is truthy but not a dict.
    c_nd = {str(_CLONE): "x"}
    def nondict(c=c_nd):
        cc = json.loads(json.dumps(c))
        return _mask({"result": inventory._git_behind(_CLONE, cc), "cache": cc})
    yield "git_behind", {"repo": str(_CLONE), "cache": c_nd, "mask_time": True}, nondict

    # A subject with a carriage return: text=True turns it into "\n".
    d = _dir("cr")
    up, clone = d / "up", d / "clone"
    _git("init", "-q", "-b", "main", str(up), cwd=d)
    _git("commit", "-q", "--allow-empty", "-m", "one", cwd=up)
    _git("clone", "-q", str(up), str(clone), cwd=d)
    _git("commit", "-q", "--allow-empty", "-m", "fix\rthing", cwd=up)
    _git("fetch", "-q", cwd=clone)
    c = {str(clone): {"fetched_at": time.time() - 60}}
    def cr(c=c):
        cc = json.loads(json.dumps(c))
        return _mask({"result": inventory._git_behind(clone, cc), "cache": cc})
    yield "git_behind", {"repo": str(clone), "cache": c, "mask_time": True}, cr

    # Plugin manifests that are not objects, or carry non-string values.
    for tag, text in (("list", "[1]"), ("types", json.dumps({"version": 1, "description": None, "id": 7}))):
        root = _dir("manifest-" + tag)
        (root / "data").mkdir()
        (root / "home").mkdir()
        (root / "config/omarchy/plugins/p").mkdir(parents=True)
        (root / "config/omarchy/plugins/p/manifest.json").write_text(text)
        env = {"XDG_DATA_HOME": str(root / "data"), "XDG_CONFIG_HOME": str(root / "config"),
               "XDG_CACHE_HOME": str(root / "cache"), "HOME": str(root / "home"), **_RO}
        yield _case("local_builds", {"check_updates": False, "manifest": tag},
                    lambda: inventory.local_builds(check_updates=False), env)

    # XDG_DATA_HOME spelled with a doubled slash: pathlib normalises it.
    env = dict(_LB_ENV, XDG_DATA_HOME=str(_LB) + "//data")
    yield _case("local_builds", {"check_updates": False, "slashes": True},
                lambda: inventory.local_builds(check_updates=False), env)

    # ---- plugin registry
    entry = {"catalog": {"name": "N", "description": None, "author": None}, "repo": "a/b"}
    yield "plugin_name", {"entry": entry}, lambda: inventory._plugin_name(entry)
    entry2 = {"catalog": {"name": "N", "description": None, "author": "Eve"}, "repo": "https://github.com/eve/n"}
    for tag, reg in (("list", []), ("strentry", {"sources": ["x", {"repo": "https://github.com/a/b"}]}),
                     ("nulldesc", {"sources": [entry2]}), ("nulltype", {"sources": [{"repo": "a/b", "type": None}]})):
        d = _dir("pr-" + tag)
        yield _case("search_plugins", {"query": "", "limit": 40, "tag": tag},
                    lambda: inventory.search_plugins("", 40),
                    {"XDG_CACHE_HOME": str(d / "cache"), "XDG_CONFIG_HOME": str(d / "config")},
                    {d / "cache/almanac/registry.json": json.dumps(reg)})

    # ---- versions_behind beyond 64 bits (Python int is unbounded)
    for cur, lat in (("1.0", "1.99999999999999999999"), ("1", "99999.1"),
                     ("1.0", "1.9223372036854775808")):
        yield ("versions_behind", {"current": cur, "latest": lat},
               lambda c=cur, l=lat: inventory.versions_behind(c, l))

    # ---- date_ts at the edges of the calendar and the tz database
    for v in ("Mon 01 Jan 0001", "Mon 01 Jan 0001 12:00:00 AM", "Fri 31 Dec 9999 11:59:59 PM",
              "Sun 18 Nov 1883 12:00:00 PM", "Sat 01 Jan 1000", "Mon 01 Jul 2200 12:00:00 PM",
              "Sun 10 Mar 2120 02:30:00 AM", "Sun 03 Nov 2120 01:30:00 AM",
              "Fri 16 Oct 2026 04:08:05 ＰＭ", "Fri 16 ſep 2026"):
        yield "date_ts", {"value": v}, lambda v=v: inventory.date_ts(v)


_orig_cases = cases


def cases():
    yield from _orig_cases()
    yield from _review_cases()


# ------------------------------------------------- review round 2 (adversarial) --
def _fakegit(revlist):
    """A bin dir whose `git -C repo <sub> ...` claims an upstream and prints
    `revlist` for rev-list; everything else prints nothing."""
    d = _dir("fakegit")
    (d / "revlist.out").write_text(revlist)
    script = d / "git"
    script.write_text(
        '#!/bin/sh\ncase "$3" in\n  rev-list) cat "$(dirname "$0")/revlist.out";;\n'
        '  log) echo subj;;\nesac\nexit 0\n')
    script.chmod(0o755)
    return {"PATH": f"{d}:{_PATH}"}


def _review2_cases():
    # _strptime's %d/%Y/%M/%S use `\d`, which matches every Unicode decimal
    # digit in Python's re -- and int() reads them.
    for v in ("Fri 16 Oct ٢٠٢٦", "Fri 1٦ Oct 2026", "Fri 16 Oct 2026 04:0٨:05 PM",
              "Fri 16 Oct 2026 04:٨:05 PM", "Fri 16 Oct 2026 04:08:٥ PM",
              "Fri 16 Oct ２０２６"):
        yield "date_ts", {"value": v, "r2": True}, lambda v=v: inventory.date_ts(v)

    # A NUL byte in an argv element: subprocess raises ValueError, which _run
    # does not catch.
    yield ("synced_version", {"name": "a\x00b", "env": _FAKE},
           _call(_FAKE, {}, lambda: inventory.synced_version("a\x00b")))
    app = {"name": "a\x00b", "version": "1", "source": "repo"}
    yield ("details", {"app": app, "env": _FAKE},
           _call(_FAKE, {}, lambda: inventory.details(inventory.App(**app))))

    # int(behind or 0) takes underscores and non-ASCII digits.
    for tag, out in (("underscore", "1_0\n"), ("arabic", "٣\n"), ("spaced", " +4 \n")):
        env = _fakegit(out)
        c = {str(_LONE): {"fetched_at": time.time() - 60}}
        def fg(c=c):
            cc = json.loads(json.dumps(c))
            return _mask({"result": inventory._git_behind(_LONE, cc), "cache": cc})
        yield ("git_behind", {"repo": str(_LONE), "cache": c, "mask_time": True,
                              "env": env, "tag": tag}, _call(env, {}, fg))

    # json.loads takes NaN/Infinity and lone surrogate escapes; serde_json
    # does not, so the Rust treats such a file as unreadable.
    for text in ('{"a": {"name": "a", "checked_at": Infinity}}',
                 '{"a": {"name": "\\ud800x"}}'):
        d, env = _state_env("reg-nonfinite")
        yield _case("load_registry", {"r2": text}, _wired(inventory._load_registry), env,
                    {d / "almanac/appimages.json": text})
    d = _dir("gitcache-inf")
    yield _case("git_cache", {"r2": True}, _wired(inventory._git_cache), {"XDG_CACHE_HOME": str(d)},
                {d / "almanac/git.json": '{"/x": {"tracked": false, "fetched_at": Infinity}}'})
    root = _dir("manifest-nan")
    (root / "data").mkdir()
    (root / "home").mkdir()
    (root / "config/omarchy/plugins/p").mkdir(parents=True)
    (root / "config/omarchy/plugins/p/manifest.json").write_text(
        '{"description": "Shows things", "version": "1.0", "score": NaN}')
    env = {"XDG_DATA_HOME": str(root / "data"), "XDG_CONFIG_HOME": str(root / "config"),
           "XDG_CACHE_HOME": str(root / "cache"), "HOME": str(root / "home"), **_RO}
    yield _case("local_builds", {"check_updates": False, "manifest": "nan"},
                lambda: inventory.local_builds(check_updates=False), env)
    d = _dir("pr-inf")
    reg = '{"sources": [{"repo": "https://github.com/a/b"}], "x": Infinity}'
    yield _case("search_plugins", {"query": "", "limit": 40, "r2": "inf"},
                lambda: inventory.search_plugins("", 40),
                {"XDG_CACHE_HOME": str(d / "cache"), "XDG_CONFIG_HOME": str(d / "config")},
                {d / "cache/almanac/registry.json": reg})

    # A cached fetched_at of "inf" is a float to Python, and stays one.
    c = {str(_LONE): {"fetched_at": "inf"}}
    def inf(c=c):
        cc = json.loads(json.dumps(c))
        return _wire({"result": inventory._git_behind(_LONE, cc), "cache": cc})
    yield "git_behind", {"repo": str(_LONE), "cache": c, "mask_time": False, "r2": True}, inf

    # str() of a list in an f-string is Python's repr, not JSON.
    reg = {"sources": [{"repo": "https://github.com/a/b",
                        "catalog": {"name": "N", "author": ["Al"]}}]}
    for q in ("", "['al']"):
        d = _dir("pr-listauthor")
        yield _case("search_plugins", {"query": q, "limit": 40, "r2": "listauthor"},
                    lambda q=q: inventory.search_plugins(q, 40),
                    {"XDG_CACHE_HOME": str(d / "cache"), "XDG_CONFIG_HOME": str(d / "config")},
                    {d / "cache/almanac/registry.json": json.dumps(reg)})

    # A checkout whose directory name is not UTF-8: pathlib keeps it with
    # surrogateescape; the port turns it into U+FFFD.
    root = _dir("nonutf8")
    data = root / "data"
    data.mkdir()
    os.mkdir(os.fsencode(str(data)) + b"/caf\xe9")
    _git("init", "-q", os.fsdecode(os.fsencode(str(data)) + b"/caf\xe9"), cwd=root)
    (root / "home").mkdir()
    env = {"XDG_DATA_HOME": str(data), "XDG_CONFIG_HOME": str(root / "config"),
           "XDG_CACHE_HOME": str(root / "cache"), "HOME": str(root / "home"), **_RO}
    yield _case("local_builds", {"check_updates": False, "r2": "nonutf8"},
                _wired(lambda: inventory.local_builds(check_updates=False)), env)

    # Numbers and strings json.dumps writes in its own way.
    for data in ({"big": 2 ** 70}, {"f": [1e16, 1.5e-07, -0.0, 1e300, 5e-324, 1e22, 1.0]},
                 {"s": "😀\x00\x1f\x7f\b\f"}):
        d, env = _state_env("reg-save2")
        yield _case("save_registry", {"data": data, "data_text": json.dumps(data), "r2": True},
                    save_and_read2(data), env)


def _wire(v):
    """What the parity JSON cannot carry, as both sides send it: a NaN or
    infinite float as its repr, a str with a lone surrogate as its
    json.dumps text."""
    import dataclasses
    import math
    if dataclasses.is_dataclass(v) and not isinstance(v, type):
        v = {f.name: getattr(v, f.name) for f in dataclasses.fields(v)}
    if isinstance(v, float) and not math.isfinite(v):
        return repr(v)
    if isinstance(v, str):
        try:
            v.encode("utf-8")
        except UnicodeEncodeError:
            return json.dumps(v)
        return v
    if isinstance(v, dict):
        return {_wire(k): _wire(x) for k, x in v.items()}
    if isinstance(v, (list, tuple)):
        return [_wire(x) for x in v]
    return v


def _wired(fn):
    return lambda: _wire(fn())


def save_and_read2(data):
    def run():
        inventory.save_registry(data)
        return {"text": inventory.REGISTRY.read_text(),
                "tmp": sorted(p.name for p in inventory.STATE.glob("*.tmp"))}
    return run


_r1_cases = cases


def cases():
    yield from _r1_cases()
    yield from _review2_cases()
