"""Parity cases for plugins.py.

`env` in the args is applied on both sides for the duration of the call
(None removes the variable); `files` ([[path, text | None], ...]) is written
or removed first, on both sides. Git checkouts are built once, in a temp dir,
when the cases are generated; `classify` against them fetches only from a
local upstream. The real machine is read with `fetch=False` and
GIT_OPTIONAL_LOCKS=0, so not even git's index refresh writes anything.
"""
import atexit
import json
import os
import shutil
import stat
import subprocess
import tempfile
from pathlib import Path

import plugins
import settings

_TMP = Path(tempfile.mkdtemp(prefix="almanac-parity-plugins-"))
atexit.register(shutil.rmtree, str(_TMP), True)
_n = 0
_PATH = os.environ.get("PATH", "")
_RO = {"GIT_OPTIONAL_LOCKS": "0"}


def _dir(tag):
    global _n
    _n += 1
    d = _TMP / f"{_n}-{tag}"
    d.mkdir(parents=True)
    return d


def _call(env, files, fn):
    def run():
        saved = {k: os.environ.get(k) for k in env}
        try:
            for k, v in env.items():
                if v is None:
                    os.environ.pop(k, None)
                else:
                    os.environ[k] = v
            for p, text in files:
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
    return run


def _case(func, args, fn, env=None, files=()):
    a = dict(args)
    env = env or {}
    files = [list(f) for f in files]
    if env:
        a["env"] = env
    if files:
        a["files"] = files
    return func, a, _call(env, files, fn)


def _git(*a, cwd):
    subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@t", *a],
                   cwd=cwd, check=True, capture_output=True)


def _repo(tag):
    """An upstream with one commit and a clone of it: (upstream, clone)."""
    d = _dir(tag)
    up = d / "up"
    _git("init", "-q", "-b", "main", str(up), cwd=d)
    (up / "manifest.json").write_text("{}")
    _git("add", ".", cwd=up)
    _git("commit", "-qm", "one", cwd=up)
    plug = d / "plugins" / f"{tag}.plugin"
    _git("clone", "-q", str(up), str(plug), cwd=d)
    return up, plug


def _advance(up, name="f"):
    (up / name).write_text("x")
    _git("add", ".", cwd=up)
    _git("commit", "-qm", f"add {name}", cwd=up)


def _head(d):
    return subprocess.run(["git", "-C", str(d), "rev-parse", "HEAD"],
                          capture_output=True, text=True).stdout.strip()


def _remotes(cache):
    return None if cache is None else {
        k: v.get("remote") if isinstance(v, dict) else v for k, v in cache.items()}


def _classify(d, cache, fetch):
    def run():
        c = None if cache is None else json.loads(json.dumps(cache))
        st = plugins.classify(Path(d), c, fetch)
        return {"state": st, "cache": _remotes(c)}
    return run


def _remote_fn(d, cache, fetch):
    def run():
        c = None if cache is None else json.loads(json.dumps(cache))
        return {"sha": plugins._remote(Path(d), c, fetch), "cache": _remotes(c)}
    return run


def _installed(fetch, cache):
    def run():
        c = None if cache is None else json.loads(json.dumps(cache))
        found = plugins.installed(fetch=fetch, cache=c)
        return {"installed": found, "has_panel": [i.has_panel for i in found],
                "cache": _remotes(c)}
    return run


def _scope():
    return {"scope": plugins.scope(), "old_exists": plugins._settings_path().exists(),
            "settings": settings.path().read_text() if settings.path().exists() else None}


def _set_scope(value):
    def run():
        plugins.set_scope(value)
        return settings.path().read_text()
    return run


def _fake_omarchy(listing):
    """A bin dir whose `omarchy` prints `listing` (text) -- the Python test
    mocks subprocess.run; both sides here run this script."""
    d = _dir("bin")
    (d / "out.json").write_text(listing)
    f = d / "omarchy"
    f.write_text(f"#!/bin/sh\ncat '{d / 'out.json'}'\n")
    f.chmod(f.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    return f"{d}:{_PATH}"


def cases():
    yield "CONSTANTS", {}, lambda: {
        "kinds": [plugins.CURRENT, plugins.UPDATE, plugins.LOCAL_CHANGES,
                  plugins.LOCAL, plugins.ERROR],
        "FETCH_EVERY": plugins.FETCH_EVERY,
        "SCOPES": [[k, l, sorted(s) if s is not None else None] for k, l, s in plugins.SCOPES],
        "VENDORS": plugins.VENDORS, "SYMBOLS": plugins.SYMBOLS, "GENERIC": plugins.GENERIC}

    # ---- plugins_dir / _settings_path / restart_argv: real and pointed ----
    yield _case("plugins_dir", {}, plugins.plugins_dir)
    for v in ["/srv/conf", "/tmp/ünï cødé", "relative", "", None]:
        yield _case("plugins_dir", {}, plugins.plugins_dir, env={"XDG_CONFIG_HOME": v})
    yield _case("_settings_path", {}, plugins._settings_path)
    for v in ["/srv/state", "/tmp/日本", None]:
        yield _case("_settings_path", {}, plugins._settings_path, env={"XDG_STATE_HOME": v})
    for cc in [True, False]:
        yield _case("restart_argv", {"clear_cache": cc}, lambda cc=cc: plugins.restart_argv(cc))
        for v in ["/srv/cache", "/tmp/sp ace/ü", "/x//y/", None]:
            yield _case("restart_argv", {"clear_cache": cc},
                        lambda cc=cc: plugins.restart_argv(cc), env={"XDG_CACHE_HOME": v})
    yield _case("restart_argv", {}, plugins.restart_argv)

    # ---- State.describe / updatable ----
    details = {
        plugins.CURRENT: ["", "unchecked"],
        plugins.UPDATE: ["behind"],
        plugins.LOCAL_CHANGES: ["modified", "ahead", "diverged", "detached", "", "other"],
        plugins.LOCAL: ["symlink", "not-git", "no-origin", "no-commits", "", "ünï"],
        plugins.ERROR: ["fetch failed", "", "[Errno 2] No such file or directory: 'git'"],
    }
    for kind, ds in details.items():
        for det in ds:
            for behind in [0, 1, 23]:
                args = {"id": "a.b", "kind": kind, "detail": det, "behind": behind, "url": "u"}
                yield "State", args, (lambda a=args: (lambda st: {
                    "describe": st.describe(), "updatable": st.updatable})(
                    plugins.State(a["id"], a["kind"], a["detail"], a["behind"], a["url"])))

    # ---- classify / _remote / _count against built checkouts ----
    up, plug = _repo("current")
    yield "classify", {"d": str(plug), "cache": {}, "fetch": True}, _classify(plug, {}, True)
    yield "classify", {"d": str(plug), "cache": None, "fetch": True}, _classify(plug, None, True)
    yield "classify", {"d": str(plug), "cache": {}, "fetch": False}, _classify(plug, {}, False)

    up, plug = _repo("behind")
    _advance(up)
    _advance(up, "g")
    for fetch in [True, False]:
        yield "classify", {"d": str(plug), "cache": {}, "fetch": fetch}, _classify(plug, {}, fetch)
    upsha = _head(up)
    myhead = _head(plug)
    # A cached remote, fresh and stale; known locally only after the fetch above.
    for at in [9e12, 0, "9e12", "nonsense-is-not-reached", None]:
        if at == "nonsense-is-not-reached":
            continue
        c = {str(plug): {"remote": upsha, "fetched_at": at}}
        for fetch in [True, False]:
            yield "classify", {"d": str(plug), "cache": c, "fetch": fetch}, _classify(plug, c, fetch)
            yield "_remote", {"d": str(plug), "cache": c, "fetch": fetch}, _remote_fn(plug, c, fetch)
    bogus = {str(plug): {"remote": "0" * 40, "fetched_at": 9e12}}
    for fetch in [True, False]:
        yield "classify", {"d": str(plug), "cache": bogus, "fetch": fetch}, _classify(plug, bogus, fetch)
    # (A cache entry that is not a dict makes the Python raise AttributeError
    # out of classify; only this module writes the cache, so it has no case.)
    odd = {"other": {"remote": "x"}, str(plug): {}}
    yield "_remote", {"d": str(plug), "cache": odd, "fetch": False}, _remote_fn(plug, odd, False)
    yield "_count", {"d": str(plug), "head": myhead, "remote": upsha}, \
        lambda p=plug, h=myhead, r=upsha: plugins._count(p, h, r)
    yield "_count", {"d": str(plug), "head": upsha, "remote": myhead}, \
        lambda p=plug, h=myhead, r=upsha: plugins._count(p, r, h)
    yield "_count", {"d": str(plug), "head": "nope", "remote": upsha}, \
        lambda p=plug, r=upsha: plugins._count(p, "nope", r)

    up, plug = _repo("modified")
    _advance(up)
    (plug / "manifest.json").write_text('{"x": 1}')
    for fetch in [True, False]:
        yield "classify", {"d": str(plug), "cache": {}, "fetch": fetch}, _classify(plug, {}, fetch)
    (plug / "untracked.txt").write_text("u")

    up, plug = _repo("ahead")
    (plug / "manifest.json").write_text('{"x": 1}')
    _git("commit", "-qam", "mine", cwd=plug)
    yield "classify", {"d": str(plug), "cache": {}, "fetch": True}, _classify(plug, {}, True)

    up, plug = _repo("diverged")
    (plug / "manifest.json").write_text('{"x": 1}')
    _git("commit", "-qam", "mine", cwd=plug)
    _advance(up)
    yield "classify", {"d": str(plug), "cache": {}, "fetch": True}, _classify(plug, {}, True)

    up, plug = _repo("detached")
    _git("checkout", "-q", "--detach", "HEAD", cwd=plug)
    yield "classify", {"d": str(plug), "cache": {}, "fetch": True}, _classify(plug, {}, True)

    up, plug = _repo("noorigin")
    _git("remote", "remove", "origin", cwd=plug)
    yield "classify", {"d": str(plug), "cache": {}, "fetch": True}, _classify(plug, {}, True)

    up, plug = _repo("badorigin")
    _git("remote", "set-url", "origin", str(_TMP / "does-not-exist"), cwd=plug)
    for fetch in [True, False]:
        yield "classify", {"d": str(plug), "cache": {}, "fetch": fetch}, _classify(plug, {}, fetch)

    d = _dir("nocommits")
    empty = d / "plugins" / "empty.plugin"
    _git("init", "-q", str(empty), cwd=d)
    _git("remote", "add", "origin", "https://example.invalid/x.git", cwd=empty)
    yield "classify", {"d": str(empty), "cache": {}, "fetch": True}, _classify(empty, {}, True)

    up, plug = _repo("symlink")
    link = plug.parent / "dev.plugin"
    link.symlink_to(plug)
    yield "classify", {"d": str(link), "cache": {}, "fetch": True}, _classify(link, {}, True)
    dangling = plug.parent / "dangling.plugin"
    dangling.symlink_to(_TMP / "nowhere")
    yield "classify", {"d": str(dangling), "cache": {}, "fetch": True}, _classify(dangling, {}, True)

    plain = _dir("plain") / "mine ünï"
    plain.mkdir()
    yield "classify", {"d": str(plain), "cache": {}, "fetch": True}, _classify(plain, {}, True)
    yield "classify", {"d": str(_TMP / "missing"), "cache": {}, "fetch": True}, \
        _classify(_TMP / "missing", {}, True)
    # A trailing slash and a doubled one: str(Path) drops them.
    up, plug = _repo("slashes")
    _advance(up)
    messy = str(plug.parent) + "//" + plug.name + "/"
    yield "classify", {"d": messy, "cache": {}, "fetch": True}, _classify(messy, {}, True)

    # git missing: classify reports the OSError.
    up, plug = _repo("nogit")
    nobin = str(_dir("nobin"))
    yield _case("classify", {"d": str(plug), "cache": {}, "fetch": True},
                _classify(plug, {}, True), env={"PATH": nobin})

    # The real machine: every plugin folder, from the real fetch cache, offline.
    try:
        real_cache = json.loads((Path.home() / ".cache/almanac/git.json").read_text()).get("_plugins") or {}
    except (OSError, ValueError):
        real_cache = {}
    pdir = plugins.plugins_dir()
    for d in sorted(pdir.glob("*")) if pdir.is_dir() else []:
        for c in [real_cache, {}]:
            yield _case("classify", {"d": str(d), "cache": c, "fetch": False},
                        _classify(d, c, False), env=_RO)

    # ---- Installed.has_panel ----
    for kinds in [[], ["panel"], ["bar-widget"], ["overlay"], ["bar"], ["service", "overlay"],
                  ["Panel"], ["ünï"]]:
        yield "has_panel", {"id": "x", "name": "x", "kinds": kinds}, \
            (lambda k=kinds: plugins.Installed(id="x", name="x", kinds=tuple(k)).has_panel)

    # ---- _manifests ----
    yield "_manifests", {"root": "/usr/share/omarchy/shell/plugins"}, \
        lambda: plugins._manifests(Path("/usr/share/omarchy/shell/plugins"))
    root = _dir("manifests")
    tree = {
        "a/manifest.json": '{"id": "one", "name": "A"}',
        "b/manifest.json": '{"id": "one", "name": "B (duplicate)"}',
        "b/deep/er/manifest.json": '{"id": "deep", "kinds": ["panel"]}',
        "c/manifest.json": "not json",
        "d/manifest.json": '["a list"]',
        "e/manifest.json": '{"id": ""}',
        "f/manifest.json": '{"name": "no id"}',
        "g/manifest.json": '{"id": "ünï.日本", "description": "ü"}',
        ".hidden/manifest.json": '{"id": "hidden"}',
        "manifest.json": '{"id": "root"}',
        "h/other.json": '{"id": "other"}',
    }
    for rel, text in tree.items():
        p = root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text)
    (root / "i" / "manifest.json").mkdir(parents=True)
    (root / "j").mkdir()
    (root / "j" / "manifest.json").write_bytes(b'{"id": "\xff"}')
    (root / "link").symlink_to(root / "a")
    for r in [root, root / "b", root / "missing", root / "a" / "manifest.json"]:
        yield "_manifests", {"root": str(r)}, (lambda r=r: plugins._manifests(r))

    # ---- installed / enabled: the real machine, offline ----
    yield _case("installed", {"fetch": False, "cache": real_cache},
                _installed(False, real_cache), env=_RO)
    yield _case("installed", {"fetch": False, "cache": None},
                _installed(False, None), env=_RO)
    yield _case("enabled", {}, plugins.enabled)

    # ---- installed / enabled: a fake omarchy, a fake system and user ----
    base = _dir("page")
    shipped = base / "omarchy/shell/plugins"
    for rel, text in {
        "clock/manifest.json": json.dumps({"id": "omarchy.clock", "description": "The time",
                                           "version": "1.0.0", "kinds": ["bar-widget"]}),
        "bar/manifest.json": json.dumps({"id": "omarchy.bar", "name": "Shipped bar"}),
        "nested/x/manifest.json": json.dumps({"id": "omarchy.nested", "kinds": "panel"}),
    }.items():
        (shipped / rel).parent.mkdir(parents=True, exist_ok=True)
        (shipped / rel).write_text(text)
    config = base / "config"
    mine = config / "omarchy/plugins"
    for rel, text in {
        "me.thing/manifest.json": json.dumps({"id": "me.thing", "version": "1.0"}),
        "folder-name/manifest.json": json.dumps({"name": "Folder"}),
        "omarchy.clock/manifest.json": json.dumps({"id": "omarchy.clock",
                                                   "description": "My own clock"}),
        "broken/manifest.json": "{nope",
        "ünï/manifest.json": json.dumps({"id": "ünï.plug", "description": "日本"}),
    }.items():
        (mine / rel).parent.mkdir(parents=True, exist_ok=True)
        (mine / rel).write_text(text)
    (mine / "nomanifest").mkdir()
    (mine / "a-file").write_text("x")
    _gup, gplug = _repo("gitmine")
    gdir = mine / "git.plugin"
    shutil.copytree(gplug, gdir, symlinks=True)
    (gdir / "manifest.json").write_text(json.dumps({"id": "me.git", "kinds": ["overlay"]}))
    _git("commit", "-qam", "manifest", cwd=gdir)
    listed = [
        {"id": "omarchy.clock", "name": "Clock", "kinds": ["bar-widget"],
         "enabled": True, "canDisable": True, "firstParty": True},
        {"id": "omarchy.bar", "name": "", "kinds": [], "enabled": True,
         "canDisable": False, "firstParty": True},
        {"id": "omarchy.nested", "enabled": 1, "firstParty": "yes"},
        {"id": "me.thing", "name": "Thing", "kinds": ["panel"],
         "enabled": False, "canDisable": True, "firstParty": False},
        {"id": "folder-name", "enabled": True},
        {"id": "ünï.plug", "name": "Ünï", "enabled": None, "canDisable": None},
        {"id": "me.git", "name": "a git one"},
        {"id": "zzz", "name": "zebra"}, {"id": "", "name": "no id"}, {"name": "missing id"},
        "a string", 5, None,
        {"id": "omarchy.clock", "name": "Clock again", "enabled": False},
    ]
    env = {"OMARCHY_PATH": str(base / "omarchy"), "XDG_CONFIG_HOME": str(config),
           "PATH": _fake_omarchy(json.dumps(listed))}
    for fetch in [False, True]:
        for c in [None, {}]:
            yield _case("installed", {"fetch": fetch, "cache": c}, _installed(fetch, c), env=env)
    yield _case("enabled", {}, plugins.enabled, env=env)
    for listing in ["", "not json", "{}", '{"id": "x"}', '"str"', "[]", "[[1], {}]",
                    '[{"id": "a", "enabled": "no"}, {"id": "a", "enabled": 0}]']:
        e = dict(env, PATH=_fake_omarchy(listing))
        yield _case("installed", {"fetch": False, "cache": None}, _installed(False, None), env=e)
        yield _case("enabled", {}, plugins.enabled, env=e)
    e = dict(env, OMARCHY_PATH="")
    yield _case("installed", {"fetch": False, "cache": None}, _installed(False, None), env=e)
    e = dict(env, OMARCHY_PATH=None, XDG_CONFIG_HOME=str(_TMP / "no-config"))
    yield _case("installed", {"fetch": False, "cache": None}, _installed(False, None), env=e)
    # omarchy missing altogether
    e = dict(env, PATH=nobin)
    yield _case("installed", {"fetch": False, "cache": None}, _installed(False, None), env=e)
    yield _case("enabled", {}, plugins.enabled, env=e)

    # ---- words, vendor_for, symbol_for ----
    named = [
        ("tim.tailscale", "My Tailscale"), ("io.github.someone.lock-explorer", "Lock Screen Explorer"),
        ("x.usage", "AI Usage"), ("akitaonrails.ai-usagebar", "AI Usage"),
        ("com.omastorm.radar", "Radar"), ("crmne.hyprmoncfg", "Hyprmon"),
        ("jankeesvw.notification-center", "Notification Center"), ("lxp.vitals", "Vitals"),
        ("tim.appearance", "Appearance"), ("tim.monitor", "Monitor"), ("tim.tray", "Tray"),
        ("a.home-assistant", "HA"), ("a.b", "Home Assistant"), ("a.b", "home_assistant"),
        ("a.spotify_now", ""), ("", ""), ("noDots", "SLACK"), ("x.", "Weather"),
        ("a.b", "Mysteam"), ("a.b", "my steam"), ("a.1password", "Vault"),
        ("a.b", "ÜNÏ Bluetooth"), ("a.b", "Ünï"), ("io.github.claude", "x"),
        ("a.b", "bar"), ("a.b", "toolbar"), ("a.devtool", "Plugin I am writing"),
        ("omarchy.nightlight", "Night light"), ("a.b", "Wi-Fi"), ("a.wifi", ""),
        ("a.b", "İstanbul"), ("a.b", "ΣΊΣΥΦΟΣ"),
    ]
    for pid, name in named:
        args = {"id": pid, "name": name, "kinds": ["bar-widget"]}
        mk = (lambda p=pid, n=name: plugins.Installed(id=p, name=n, kinds=("bar-widget",)))
        yield "vendor_for", args, (lambda mk=mk: plugins.vendor_for(mk()))
        yield "symbol_for", args, (lambda mk=mk: plugins.symbol_for(mk()))
        yield "_own_words", args, (lambda mk=mk: plugins._own_words(mk()))
    for texts in [[], [""], ["A.B-C_D"], ["x", "", "Y Z"], ["Ünï.日本"], ["İ"]]:
        yield "_words", {"texts": texts}, (lambda t=texts: plugins._words(*t))

    # ---- github_owner ----
    for url in ["https://github.com/lxp/vitals.git", "git@github.com:lxp/vitals.git",
                "https://gitlab.com/a/b", "", "https://github.com/", "https://github.com/lxp",
                "https://github.com//x", "https://github.com/a.b-c_d/x", "http://github.com/a/b",
                "https://github.com/ünï/x", "https://github.com/a b/x", "git@github.com:/x",
                " https://github.com/a/b", "https://github.com/a/b/c", "https://GITHUB.com/a/b",
                "https://github.com/-/x", "https://github.com/日本/x"]:
        yield "github_owner", {"url": url}, (lambda u=url: plugins.github_owner(u))

    # ---- argv ----
    for pid in ["a.b", "", "ünï 日本", "--yes"]:
        for on in [True, False]:
            yield "toggle_argv", {"pid": pid, "on": on}, (lambda p=pid, o=on: plugins.toggle_argv(p, o))
        yield "update_argv", {"pid": pid}, (lambda p=pid: plugins.update_argv(p))

    # ---- in_scope ----
    for tk in [None, "official", "first-party", "reviewed", "verified", "reviewed-stale",
               "stale", "community", "", "Verified"]:
        for sc in ["verified", "changed", "all", "", "bogus"]:
            yield "in_scope", {"trust_key": tk, "scope": sc}, (lambda t=tk, s=sc: plugins.in_scope(t, s))

    # ---- scope / set_scope, each in its own state dir ----
    def scope_case(old, settings_text):
        st = _dir("state")
        files = []
        if settings_text is not None:
            files.append((str(st / "almanac/settings.json"), settings_text))
        if old is not None:
            files.append((str(st / "almanac/plugins.json"), old))
        return _case("scope", {}, _scope, env={"XDG_STATE_HOME": str(st)}, files=files)

    for old in [None, '{"scope": "verified"}', '{"scope": "changed"}', '{"scope": "bogus"}',
                '{"scope": 5}', '{"other": 1}', "not json", "[1, 2]", '"verified"', "",
                '{"scope": null}', '{"scope": "ünï"}']:
        for st_text in [None, '{"plugin_scope": "changed"}', '{"plugin_scope": 3}', "{bad"]:
            yield scope_case(old, st_text)
    if not plugins._settings_path().exists():          # never migrate the real file
        yield "scope", {}, _scope
    for value in ["verified", "changed", "all", "bogus", "", "ünï"]:
        st = _dir("set")
        for pre in [None, '{"plugin_scope": "verified", "other": true}']:
            files = [(str(st / "almanac/settings.json"), pre)] if pre else [
                (str(st / "almanac/settings.json"), None)]
            yield _case("set_scope", {"value": value}, _set_scope(value),
                        env={"XDG_STATE_HOME": str(st)}, files=files)

    # ==== review round 1: adversarial cases ====
    yield from _review_round_1()
    yield from _review_round_2()
    yield from _review_round_2_adversarial()
    yield from _review_round_3()


def _page_env(tag, listing, shipped_tree=None, mine_tree=None, omarchy_sep="/", config_sep="/"):
    """A fake omarchy printing `listing` (bytes or text), shipped manifests and
    the user's own, all under a fresh dir. `*_sep` lets a path carry "//"."""
    base = _dir(tag)
    for rel, text in (shipped_tree or {}).items():
        p = base / "omarchy/shell/plugins" / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text)
    for rel, text in (mine_tree or {}).items():
        p = base / "config/omarchy/plugins" / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text)
    (base / "omarchy").mkdir(exist_ok=True)
    (base / "config").mkdir(exist_ok=True)
    bindir = _dir(tag + "-bin")
    (bindir / "out.json").write_bytes(listing if isinstance(listing, bytes) else listing.encode())
    f = bindir / "omarchy"
    f.write_text(f"#!/bin/sh\ncat '{bindir / 'out.json'}'\n")
    f.chmod(0o755)
    return {"OMARCHY_PATH": str(base) + omarchy_sep + "omarchy",
            "XDG_CONFIG_HOME": str(base) + config_sep + "config",
            "PATH": f"{bindir}:{_PATH}"}


def _review_round_1():
    # R1. A symlinked dev plugin named with a trailing slash: Python's Path
    # drops the slash, so is_symlink() is still True -> LOCAL "symlink".
    up, plug = _repo("r1link")
    _advance(up)
    link = plug.parent / "dev.plugin"
    link.symlink_to(plug)
    d = str(link) + "/"
    yield "classify", {"d": d, "cache": {}, "fetch": True}, _classify(d, {}, True)

    # R2. A path ending in "..": Path.name is "..".
    dd = _dir("r2dots")
    (dd / "sub").mkdir()
    d = str(dd / "sub" / "..")
    yield "classify", {"d": d, "cache": {}, "fetch": True}, _classify(d, {}, True)

    # R3. A relative "./" path: str(Path) drops the leading "./" (cache key).
    up, plug = _repo("r3rel")
    _advance(up)
    rel = "./" + os.path.relpath(plug)
    yield "_remote", {"d": rel, "cache": {}, "fetch": True}, _remote_fn(rel, {}, True)

    # R4. fetched_at as a string float() accepts but Rust's parse does not.
    up, plug = _repo("r4underscore")
    _advance(up)
    mine_head = _head(plug)
    c = {str(plug): {"remote": mine_head, "fetched_at": "9_000_000_000_000"}}
    yield "_remote", {"d": str(plug), "cache": c, "fetch": True}, _remote_fn(plug, c, True)
    # R5. fetched_at float() rejects: Python raises ValueError out of classify.
    c = {str(plug): {"remote": mine_head, "fetched_at": "abc"}}
    yield "_remote", {"d": str(plug), "cache": c, "fetch": True}, _remote_fn(plug, c, True)
    yield "classify", {"d": str(plug), "cache": c, "fetch": True}, _classify(plug, c, True)
    c = {str(plug): {"remote": mine_head, "fetched_at": [1]}}
    yield "_remote", {"d": str(plug), "cache": c, "fetch": True}, _remote_fn(plug, c, True)
    # R6. A cache entry that is not a dict: Python raises AttributeError.
    c = {str(plug): "x"}
    yield "_remote", {"d": str(plug), "cache": c, "fetch": True}, _remote_fn(plug, c, True)
    # R7. A non-string remote: Python passes it to subprocess -> TypeError.
    c = {str(plug): {"remote": 5, "fetched_at": 9e12}}
    yield "_remote", {"d": str(plug), "cache": c, "fetch": False}, _remote_fn(plug, c, False)

    # R8. An origin URL that is not UTF-8: text=True makes Python raise
    # UnicodeDecodeError (a ValueError, not caught by classify).
    up, plug = _repo("r8bytes")
    cfg = plug / ".git" / "config"
    cfg.write_bytes(cfg.read_bytes().replace(str(up).encode(), str(up).encode() + b"\xff"))
    yield "classify", {"d": str(plug), "cache": {}, "fetch": False}, _classify(plug, {}, False)

    # R9. A git that exists but cannot be executed: EACCES, not ENOENT.
    up, plug = _repo("r9noexec")
    nx = _dir("r9bin")
    (nx / "git").write_text("#!/bin/sh\necho hi\n")
    (nx / "git").chmod(0o644)
    yield _case("classify", {"d": str(plug), "cache": {}, "fetch": True},
                _classify(plug, {}, True), env={"PATH": str(nx)})

    # R10. omarchy printing bytes that are not UTF-8: Python's decode fails
    # (ValueError, caught) -> [] / {}.
    e = _page_env("r10", b'[{"id": "a\xff", "name": "A", "enabled": true}]')
    yield _case("installed", {"fetch": False, "cache": None}, _installed(False, None), env=e)
    yield _case("enabled", {}, plugins.enabled, env=e)

    # R11. JSON that is not iterable: Python raises TypeError.
    for listing in ["5", "null", "true"]:
        e = _page_env("r11", listing)
        yield _case("installed", {"fetch": False, "cache": None}, _installed(False, None), env=e)
        yield _case("enabled", {}, plugins.enabled, env=e)

    # R12. _manifests: NaN (Python's json accepts it), non-string ids.
    root = _dir("r12")
    for rel, text in {"int/manifest.json": '{"id": 5}',
                      "bool/manifest.json": '{"id": true}',
                      "float/manifest.json": '{"id": 1.5}',
                      "intfloat/manifest.json": '{"id": 5.0, "dup": "of 5"}'}.items():
        (root / rel).parent.mkdir(parents=True, exist_ok=True)
        (root / rel).write_text(text)
    yield "_manifests", {"root": str(root)}, (lambda r=root: plugins._manifests(r))
    # NaN in a manifest: compared by folder, since NaN != NaN on any side.
    nroot = _dir("r12nan")
    for rel, text in {"nan/manifest.json": '{"id": "nan.plug", "v": NaN}',
                      "inf/manifest.json": '{"id": "inf.plug", "v": [Infinity, -Infinity]}',
                      "str/manifest.json": '{"id": "str.plug", "v": "NaN Infinity"}'}.items():
        (nroot / rel).parent.mkdir(parents=True, exist_ok=True)
        (nroot / rel).write_text(text)
    yield "_manifests_folders", {"root": str(nroot)}, \
        (lambda r=nroot: {k: str(f) for k, (_m, f) in plugins._manifests(r).items()})
    # A list id cannot be a dict key: TypeError.
    lroot = _dir("r12list")
    (lroot / "l").mkdir()
    (lroot / "l/manifest.json").write_text('{"id": [1]}')
    yield "_manifests", {"root": str(lroot)}, (lambda r=lroot: plugins._manifests(r))
    # R13. A root reached through "//": Path normalises the folder.
    r2 = str(root.parent) + "//" + root.name
    yield "_manifests", {"root": r2}, (lambda r=r2: plugins._manifests(Path(r)))
    # R14. A symlinked directory holding a manifest of its own.
    root = _dir("r14")
    (root / "real").mkdir()
    (root / "real/manifest.json").write_text('{"id": "real"}')
    other = _dir("r14other")
    (other / "manifest.json").write_text('{"id": "only-via-link"}')
    (root / "zlink").symlink_to(other)
    yield "_manifests", {"root": str(root)}, (lambda r=root: plugins._manifests(r))

    # R15. installed: a user manifest that is a list -> Python AttributeError.
    e = _page_env("r15", '[{"id": "x"}]', mine_tree={"x/manifest.json": "[1]"})
    yield _case("installed", {"fetch": False, "cache": None}, _installed(False, None), env=e)
    # R16. A user manifest with a non-string id: Python keys it by 5, so the
    # folder name does not make it the user's.
    e = _page_env("r16", '[{"id": "five", "name": "Five"}]',
                  mine_tree={"five/manifest.json": '{"id": 5}'})
    yield _case("installed", {"fetch": False, "cache": None}, _installed(False, None), env=e)
    # R17. description / version that are null or numbers: Python keeps them.
    e = _page_env("r17", '[{"id": "s.a"}, {"id": "s.b"}]',
                  shipped_tree={"a/manifest.json": '{"id": "s.a", "description": null, "version": 2}',
                                "b/manifest.json": '{"id": "s.b", "description": 5, "version": null}'})
    yield _case("installed", {"fetch": False, "cache": None}, _installed(False, None), env=e)
    # R18. kinds with non-string items: Python keeps 1 and None as they are.
    e = _page_env("r18", '[{"id": "k", "name": "K", "kinds": [1, null, "panel"]}]')
    yield _case("installed", {"fetch": False, "cache": None}, _installed(False, None), env=e)
    # R19. kinds that tuple() cannot iterate -> TypeError.
    e = _page_env("r19", '[{"id": "k", "name": "K", "kinds": 5}]')
    yield _case("installed", {"fetch": False, "cache": None}, _installed(False, None), env=e)
    # R20. non-string ids: Python keeps the int / bool.
    e = _page_env("r20", '[{"id": 5, "name": "Five"}, {"id": true, "name": "T"}]')
    yield _case("installed", {"fetch": False, "cache": None}, _installed(False, None), env=e)
    yield _case("enabled", {}, plugins.enabled, env=e)
    # R21. a non-string name: Python's sort calls .lower() on it -> AttributeError.
    e = _page_env("r21", '[{"id": "n", "name": 5}]')
    yield _case("installed", {"fetch": False, "cache": None}, _installed(False, None), env=e)
    # R22. OMARCHY_PATH / XDG_CONFIG_HOME with a doubled slash: Path normalises
    # the folder it reports.
    e = _page_env("r22", '[{"id": "s.a"}, {"id": "me.a"}]',
                  shipped_tree={"a/manifest.json": '{"id": "s.a"}'},
                  mine_tree={"mine/manifest.json": '{"id": "me.a"}'},
                  omarchy_sep="//", config_sep="//")
    yield _case("installed", {"fetch": False, "cache": None}, _installed(False, None), env=e)
    # R23. NaN in a user manifest.
    e = _page_env("r23", '[{"id": "me.nan"}]',
                  mine_tree={"nan/manifest.json": '{"id": "me.nan", "description": "d", "x": NaN}'})
    yield _case("installed", {"fetch": False, "cache": None}, _installed(False, None), env=e)

    # R24. github_owner: \w is str.isalnum() in Python; Rust's is_alphanumeric
    # also takes Other_Alphabetic marks and circled letters.
    for url in ["https://github.com/Ⓐ/x", "https://github.com/हिंदी/x",
                "https://github.com/aͅ/x", "https://github.com/á/x"]:
        yield "github_owner", {"url": url}, (lambda u=url: plugins.github_owner(u))

    # R25. State with a kind it does not know: Python raises KeyError.
    args = {"id": "a.b", "kind": "bogus", "detail": "", "behind": 0, "url": ""}
    yield "State", args, (lambda: plugins.State("a.b", "bogus").describe())

    # R26. scope(): the 1.4.0 file holding NaN (Python carries it over) or a
    # list (Python's `in {set}` raises TypeError).
    for old in ['{"scope": NaN}', '{"scope": [1]}', '{"scope": {"a": 1}}']:
        st = _dir("r26")
        yield _case("scope", {}, _scope, env={"XDG_STATE_HOME": str(st)},
                    files=[(str(st / "almanac/plugins.json"), old)])


def _review_round_2():
    # ==== review round 2: the fixes' own edges ====
    up, plug = _repo("r2float")
    _advance(up)
    mine_head = _head(plug)
    for at in [" 9_000_000_000_000 ", "1__0", "_1", "1_", "inf", "-Infinity", "NaN", "+9e12",
               ".5", "5.", "\t9e12\n", "9e1_2", "0x10", True, False, 0, [], {}, {"a": 1}, ""]:
        c = {str(plug): {"remote": mine_head, "fetched_at": at}}
        yield "_remote", {"d": str(plug), "cache": c, "fetch": True}, _remote_fn(plug, c, True)
    for entry in [[], 0, "", None, [1], 5]:
        c = {str(plug): entry}
        yield "_remote", {"d": str(plug), "cache": c, "fetch": False}, _remote_fn(plug, c, False)
    for sha in [True, [1], 1.5, "", None, "a\u0000b"]:
        c = {str(plug): {"remote": sha, "fetched_at": 9e12}}
        yield "_remote", {"d": str(plug), "cache": c, "fetch": False}, _remote_fn(plug, c, False)
    # Relative and odd spellings of one checkout.
    for d in ["//" + str(plug).lstrip("/"), str(plug) + "/.", str(plug) + "/./", "./" + os.path.relpath(plug) + "//"]:
        yield "classify", {"d": d, "cache": {}, "fetch": False}, _classify(d, {}, False)
    for d in ["", ".", "/", "..", "a/..", "//"]:
        yield "classify", {"d": d, "cache": None, "fetch": False}, _classify(d, None, False)

    # Dict-key semantics across mine / shipped / the listing.
    e = _page_env("r2keys", '[{"id": 5.0, "name": "F"}, {"id": 1, "name": "One"}, {"id": true, "name": "T", "enabled": 1}]',
                  mine_tree={"five/manifest.json": '{"id": 5, "description": 7}'},
                  shipped_tree={"t/manifest.json": '{"id": 1, "version": [1, 2]}'})
    yield _case("installed", {"fetch": False, "cache": None}, _installed(False, None), env=e)
    yield _case("enabled", {}, plugins.enabled, env=e)
    for listing in ['[{"id": [1]}]', '[{"id": {"a": 1}}]', '[{"id": "k", "name": "K", "kinds": [[1]]}]',
                    '[{"id": "k", "name": "K", "kinds": {"panel": 1}}]', '[{"id": "k", "name": "K", "kinds": "panel"}]',
                    '[{"id": "k", "name": "K", "kinds": 0}]', '[{"id": "k", "name": "K", "kinds": [1.5, true, "overlay"]}]',
                    '[{"id": "k", "name": null}]', '[{"id": 7}]', '[{"id": "a", "name": "b"}, {"id": "c", "name": []}]',
                    '[{"id": "a", "name": NaN}]', '[{"id": "a", "name": "A", "x": Infinity}]', '1.5', '"x"',
                    '[{"id": "a", "enabled": true}, {"id": "a", "enabled": false}]', '[{"id": 1}, {"id": true}]']:
        e = _page_env("r2list", listing)
        yield _case("installed", {"fetch": False, "cache": None}, _installed(False, None), env=e)
        yield _case("enabled", {}, plugins.enabled, env=e)
    # User manifests that are not dicts, or have odd ids.
    for text in ['"str"', "5", "null", '{"id": [1]}', '{"id": 0}', '{"id": ""}', '{"id": null}', "NaN"]:
        e = _page_env("r2mine", '[{"id": "m", "name": "M"}]', mine_tree={"m/manifest.json": text})
        yield _case("installed", {"fetch": False, "cache": None}, _installed(False, None), env=e)
    # Carriage returns in omarchy's output.
    e = _page_env("r2crlf", '[{"id": "a",\r\n "name": "A\\r"}]\r\n')
    yield _case("installed", {"fetch": False, "cache": None}, _installed(False, None), env=e)

    # github_owner: more categories.
    for url in ["https://github.com/a²/x", "https://github.com/Ⅻ/x", "https://github.com/٣/x",
                "https://github.com/a‿b/x", "https://github.com/ǅ/x", "https://github.com/ʰ/x"]:
        yield "github_owner", {"url": url}, (lambda u=url: plugins.github_owner(u))

    # scope: more carried values.
    for old in ['{"scope": Infinity}', '{"scope": true}', '{"scope": "all"}', '{"scope": []}', '{"scope": {}}']:
        st = _dir("r2scope")
        yield _case("scope", {}, _scope, env={"XDG_STATE_HOME": str(st)},
                    files=[(str(st / "almanac/plugins.json"), old)])


def _review_round_2_adversarial():
    # ==== review round 2 (adversarial): new suspected differences ====
    # A1. str.strip() also strips \x1c-\x1f; Rust's trim() does not.
    up, plug = _repo("a1strip")
    _git("config", "remote.origin.url", str(up) + "\x1f", cwd=plug)
    yield _case("classify", {"d": str(plug), "cache": {}, "fetch": False}, _classify(plug, {}, False), env=_RO)
    # A2. A genuine U+FFFD in git's output decodes fine in Python.
    up, plug = _repo("a2fffd")
    _git("config", "remote.origin.url", str(up) + "\ufffd", cwd=plug)
    yield _case("classify", {"d": str(plug), "cache": {}, "fetch": False}, _classify(plug, {}, False), env=_RO)
    # A3. ... and in omarchy's (a plugin name holding U+FFFD).
    e = _page_env("a3fffd", '[{"id": "a", "name": "A\ufffd", "enabled": true}]')
    yield _case("installed", {"fetch": False, "cache": None}, _installed(False, None), env=e)
    yield _case("enabled", {}, plugins.enabled, env=e)
    # A4. Numbers past f64 (Python reads inf) and lone surrogates.
    for listing in ['[{"id": "a", "name": "A", "x": 1e400}]',
                    '[{"id": "a", "name": "A", "x": "\\ud800"}]',
                    '[{"id": 123456789012345678901234567890, "enabled": true}]',
                    '[{"id": -0.0, "enabled": true}, {"id": 1e16}, {"id": 1e22}]']:
        e = _page_env("a4", listing)
        yield _case("installed", {"fetch": False, "cache": None}, _installed(False, None), env=e)
        yield _case("enabled", {}, plugins.enabled, env=e)
    # A5. _manifests: an id of NaN (truthy in Python), a manifest with 1e400.
    root = _dir("a5")
    for rel, text in {"nanid/manifest.json": '{"id": NaN}',
                      "big/manifest.json": '{"id": "big", "v": 1e400}',
                      "sur/manifest.json": '{"id": "sur", "v": "\\udc00"}'}.items():
        (root / rel).parent.mkdir(parents=True, exist_ok=True)
        (root / rel).write_text(text)
    yield "_manifests_folders", {"root": str(root)}, \
        (lambda r=root: {str(k): str(f) for k, (_m, f) in plugins._manifests(r).items()})
    # A6. scope(): an old file Python's json reads but serde does not.
    for old in ['{"scope": "verified", "x": 1e400}', '{"scope": "changed", "x": "\\ud800"}']:
        st = _dir("a6")
        yield _case("scope", {}, _scope, env={"XDG_STATE_HOME": str(st)},
                    files=[(str(st / "almanac/plugins.json"), old)])
    # A7. rglob order with duplicate ids at several depths.
    root = _dir("a7")
    n = 0
    for top in ["a", "b", "c", "d"]:
        for sub in ["", "x", "y/z"]:
            n += 1
            p = root / top / sub / "manifest.json" if sub else root / top / "manifest.json"
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(json.dumps({"id": "dup" if n % 2 else "dup2", "n": n}))
    yield "_manifests", {"root": str(root)}, (lambda r=root: plugins._manifests(r))
    # A8. A git the user cannot execute though some x bit is set (EACCES),
    # and one with no #! line (ENOEXEC in Python; sh fallback or ENOENT in Rust).
    up, plug = _repo("a8")
    for mode, text, tag in [(0o645, "#!/bin/sh\necho hi\n", "a8mode"), (0o755, "echo hi\n", "a8noexec")]:
        nx = _dir(tag)
        (nx / "git").write_text(text)
        (nx / "git").chmod(mode)
        yield _case("classify", {"d": str(plug), "cache": {}, "fetch": True},
                    _classify(plug, {}, True), env={"PATH": str(nx)})
    # A9. Sorting by name.lower() with non-ASCII names.
    names = ["Zed", "\u00e9clair", "\u00c9a", "\u01c5x", "\u0130b", "ib", "\u1e9eS", "ss", "a", "B", "_", "~"]
    e = _page_env("a9", json.dumps([{"id": f"p{i}", "name": nm} for i, nm in enumerate(names)]))
    yield _case("installed", {"fetch": False, "cache": None}, _installed(False, None), env=e)
    # A10. float() of non-ASCII digits (fullwidth, Arabic-Indic): Python reads
    # them; the Rust port refuses them (ValueError).
    up, plug = _repo("a10")
    _advance(up)
    mine_head = _head(plug)
    for at in ["９ｅ１２", "٩٠٠٠٠٠٠٠٠٠٠٠٠"]:
        c = {str(plug): {"remote": mine_head, "fetched_at": at}}
        yield "_remote", {"d": str(plug), "cache": c, "fetch": True}, _remote_fn(plug, c, True)
    # A11. The user's folders: hidden ones, and which duplicate id wins
    # (the last in sorted order).
    tree = {f"{n}/manifest.json": json.dumps({"id": "same", "description": n})
            for n in ["b", "a", "A", "a.b", "a-b", "é", ".hidden", "_x", "a b"]}
    tree[".dot/manifest.json"] = json.dumps({"id": "dot.only"})
    e = _page_env("a11", '[{"id": "same"}, {"id": "dot.only"}]', mine_tree=tree)
    yield _case("installed", {"fetch": False, "cache": None}, _installed(False, None), env=e)


def _review_round_3():
    # ==== review round 3: the fixes for round 2's findings ====
    # Output that is not UTF-8, on stdout or only on stderr: Python raises
    # UnicodeDecodeError out of classify (it is not an OSError).
    up, plug = _repo("r3bytes")
    for tag, script in [("r3out", "#!/bin/sh\nprintf 'x\\377'\n"), ("r3err", "#!/bin/sh\nprintf 'ok\\n'; printf '\\342\\202' >&2\n")]:
        nx = _dir(tag)
        (nx / "git").write_text(script)
        (nx / "git").chmod(0o755)
        yield _case("classify", {"d": str(plug), "cache": {}, "fetch": False},
                    _classify(plug, {}, False), env={"PATH": str(nx)})
    # A git found after a PATH entry that cannot be run: the first error is
    # kept only if nothing starts.
    nx = _dir("r3path")
    (nx / "git").write_text("#!/bin/sh\necho hi\n")
    (nx / "git").chmod(0o644)
    real = os.path.dirname(shutil.which("git"))
    yield _case("classify", {"d": str(plug), "cache": {}, "fetch": False},
                _classify(plug, {}, False), env={"PATH": f"{nx}:{real}", **_RO})
    yield _case("classify", {"d": str(plug), "cache": {}, "fetch": True},
                _classify(plug, {}, True), env={"PATH": f"/nonexistent:{nx}"})
    # float() of \x1c-padded and NBSP-padded times: only the latter reads.
    up, plug = _repo("r3float")
    _advance(up)
    mine_head = _head(plug)
    for at in ["\x1c9e12", "9e12\x1f", "\u00a09e12\u3000", "\u0669e12", "\u00b2"]:
        c = {str(plug): {"remote": mine_head, "fetched_at": at}}
        yield "_remote", {"d": str(plug), "cache": c, "fetch": True}, _remote_fn(plug, c, True)
    # scope(): values only Python's json holds, carried over exactly.
    for old in ['{"scope": 123456789012345678901234567890}', '{"scope": "\\ud800"}',
                '{"scope": 1e400}', '{"scope": "verified", "scope": "changed"}', '\ufeff{"scope": "verified"}']:
        st = _dir("r3scope")
        yield _case("scope", {}, _scope, env={"XDG_STATE_HOME": str(st)},
                    files=[(str(st / "almanac/plugins.json"), old)])
    # Listings with what serde cannot read, and keys equal across types.
    for listing in ['[{"id": "a", "name": "A", "enabled": NaN}]', '[{"id": "a", "name": "A", "enabled": -0.0}]',
                    '[{"id": 10000000000000000000000, "enabled": true}, {"id": 1e22, "enabled": false}]',
                    '[{"id": "a", "name": Infinity}]',
                    '[{"id": "a", "name": "A", "x": ' + '[' * 3000 + ']' * 3000 + '}]']:
        e = _page_env("r3list", listing)
        yield _case("installed", {"fetch": False, "cache": None}, _installed(False, None), env=e)
        yield _case("enabled", {}, plugins.enabled, env=e)
    # An inf kind or a NaN id reaches the output, which serde cannot carry
    # (it writes null), so only what is computed from them is compared.
    for listing in ['[{"id": "a", "name": "A", "kinds": ["panel", 1e400]}]', '[{"id": NaN, "name": "N"}]']:
        e = _page_env("r3inf", listing)
        yield _case("enabled", {}, plugins.enabled, env=e)
        yield _case("installed_names", {"fetch": False},
                    lambda: [[i.name, i.has_panel, i.enabled] for i in plugins.installed(fetch=False)], env=e)
