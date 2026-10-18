"""Parity cases for autorun.py.

Nothing here updates anything. Every `update_*` case runs with `_run`
replaced by a recorder (and, for AppImages, check/download/install/save
replaced by recorders unless `real_check` asks for the real check on
files that cannot reach the network), the way test_app.py mocks them;
the Rust parity hook installs the same stand-ins. `_run` itself is only
exercised on harmless commands (true, false, sh -c echo, sleep).

Git checkouts for `update_plugins` are built once, in a temp dir, when the
cases are generated; classify() fetches only from that local upstream.
`main` runs with the three sections replaced by counters; its config is
either given (`cfg`, merged over DEFAULTS as load() does) or the real
auto-update.json, read only.
"""
import atexit
import contextlib
import copy
import io
import json
import os
import shutil
import subprocess
import sys
import tempfile
import types
from pathlib import Path
from unittest import mock

import autorun
import autoupdate
import agents
import appimage
import inventory
import plugins
import store

_TMP = Path(tempfile.mkdtemp(prefix="almanac-parity-autorun-"))
atexit.register(shutil.rmtree, str(_TMP), True)


def _git(*a, cwd):
    subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@t", *a],
                   cwd=cwd, check=True, capture_output=True)


def _repos():
    """plugins/: a.plugin (1 behind), b.plugin (current), dev.plugin (a
    symlink to a.plugin), .hidden.plugin (1 behind), plain (no git),
    nomanifest (a clone without manifest.json... removed), file.txt."""
    up = _TMP / "up"
    _git("init", "-q", "-b", "main", str(up), cwd=_TMP)
    (up / "manifest.json").write_text("{}")
    _git("add", ".", cwd=up)
    _git("commit", "-qm", "one", cwd=up)
    pdir = _TMP / "plugins"
    a = pdir / "a.plugin"
    _git("clone", "-q", str(up), str(a), cwd=_TMP)
    _git("clone", "-q", str(up), str(pdir / ".hidden.plugin"), cwd=_TMP)
    _git("clone", "-q", str(up), str(pdir / "ünï.plugin"), cwd=_TMP)
    (up / "f").write_text("x")
    _git("add", ".", cwd=up)
    _git("commit", "-qm", "two", cwd=up)
    _git("clone", "-q", str(up), str(pdir / "b.plugin"), cwd=_TMP)
    (pdir / "dev.plugin").symlink_to(a)
    (pdir / "plain").mkdir()
    (pdir / "plain" / "manifest.json").write_text("{}")
    (pdir / "nomanifest").mkdir()
    (pdir / "file.txt").write_text("x")
    return pdir


_PDIR = _repos()


# ------------------------------------------------------------------ log --
def _log(msg):
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf):
        autorun.log(msg)
    text = buf.getvalue()
    assert text.endswith("\n")
    return {"log": [text[:-1]]}


# ----------------------------------------------------------------- _run --
def _run(argv, timeout=1800):
    lines = []
    with mock.patch.object(autorun, "log", lines.append):
        ok = autorun._run(argv, timeout=timeout)
    return {"ok": ok, "log": lines}


# --------------------------------------------------------------- agents --
def _agents(apps, run_ok=True):
    lines, calls = [], []

    def fake_run(argv, timeout=1800):
        calls.append([list(argv), timeout])
        return run_ok

    objs = [inventory.App(**a) for a in apps]
    with mock.patch.object(agents, "detect", return_value=objs), \
         mock.patch.object(autorun, "_run", fake_run), \
         mock.patch.object(autorun, "log", lines.append):
        done = autorun.update_agents()
    return {"done": done, "log": lines, "calls": calls}


def _app(name, version, update="", cmd=None):
    extra = {} if cmd is None else {"update_cmd": cmd}
    return {"name": name, "version": version, "source": "agent", "update": update, "extra": extra}


# ------------------------------------------------------------ appimages --
def _appimages(reg, check=None, download_err=None, install_err=None, save_err=None,
               real_check=False):
    lines, events = [], []
    check = check or {}
    download_err = download_err or {}
    install_err = install_err or {}
    real = appimage.check

    def fake_check(rec):
        if real_check:
            return real(rec)
        key = next(k for k, v in data.items() if v is rec)
        c = check.get(key)
        if c is None:
            return {}
        if "err" in c:
            raise RuntimeError(c["err"])
        return c["ok"] or {}

    def fake_download(url, dest):
        events.append(["download", url, str(dest)])
        if str(dest) in download_err:
            raise OSError(download_err[str(dest)])

    def fake_install(dest, key):
        events.append(["install", str(dest), key])
        if key in install_err:
            raise OSError(install_err[key])
        return True

    def fake_save(r):
        events.append(["save", copy.deepcopy(r)])
        if save_err:
            raise OSError(save_err)

    data = copy.deepcopy(reg) if reg is not None else None
    patches = [mock.patch.object(appimage, "check", fake_check),
               mock.patch.object(appimage, "download", fake_download),
               mock.patch.object(appimage, "install_desktop_entry", fake_install),
               mock.patch.object(inventory, "save_registry", fake_save),
               mock.patch.object(autorun, "log", lines.append),
               mock.patch.object(autorun, "_run", lambda *a, **k: True)]
    if data is not None:
        patches.append(mock.patch.object(inventory, "_load_registry", lambda: data))
    else:
        real_load = inventory._load_registry

        def load():
            nonlocal data
            data = real_load()
            return data
        patches.append(mock.patch.object(inventory, "_load_registry", load))
    with contextlib.ExitStack() as st:
        for p in patches:
            st.enter_context(p)
        done = autorun.update_appimages()
    return {"done": done, "log": lines, "events": events}


def _latest(version, url="https://example.invalid/x.AppImage"):
    return {"version": version, "tag": "v" + version, "url": url, "asset": "x.AppImage",
            "published": "", "page": ""}


# -------------------------------------------------------------- plugins --
def _plugins(pdir, scope, trust, run_ok=True):
    lines, calls = [], []

    def fake_run(argv, timeout=1800):
        calls.append([list(argv), timeout])
        return run_ok

    if trust is None:
        cat = mock.patch.object(store, "catalog", side_effect=RuntimeError("offline"))
    else:
        rows = [types.SimpleNamespace(id=k, trust=types.SimpleNamespace(key=v))
                for k, v in trust.items()]
        cat = mock.patch.object(store, "catalog", return_value=rows)
    with mock.patch.object(plugins, "plugins_dir", return_value=Path(pdir)), \
         mock.patch.object(plugins, "scope", return_value=scope), cat, \
         mock.patch.object(autorun, "_run", fake_run), \
         mock.patch.object(autorun, "log", lines.append):
        done = autorun.update_plugins()
    return {"done": done, "log": lines, "calls": calls}


# ----------------------------------------------------------------- main --
def _main(argv, cfg=None, counts=None, columns="80"):
    lines, called = [], []
    counts = counts or {}

    def section(name):
        def f():
            called.append(name)
            return counts.get(name, 0)
        return f

    out, err = io.StringIO(), io.StringIO()
    if cfg is not None:
        merged = dict(autoupdate.DEFAULTS)
        merged.update(cfg)
        load = mock.patch.object(autoupdate, "load", return_value=merged)
    else:
        load = contextlib.nullcontext()
    with load, mock.patch.object(sys, "argv", ["autorun.py"]), \
         mock.patch.dict(os.environ, {"COLUMNS": columns}), \
         mock.patch.object(autorun, "update_agents", section("agents")), \
         mock.patch.object(autorun, "update_appimages", section("appimages")), \
         mock.patch.object(autorun, "update_plugins", section("plugins")), \
         mock.patch.object(autorun, "log", lines.append), \
         contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        try:
            code = autorun.main(argv)
        except SystemExit as exc:
            code = exc.code
    return {"code": code, "stdout": out.getvalue(), "stderr": err.getvalue(),
            "log": lines, "called": called}


def cases():
    # ---- log ----
    for m in ["hello", "", "→ ünï ☃", "two\nlines", "  indented  "]:
        yield "log", {"msg": m}, (lambda m=m: _log(m))

    # ---- _run: harmless commands only ----
    runs = [
        (["true"], 1800),
        (["false"], 1800),
        (["sh", "-c", "echo out; echo err >&2; exit 3"], 1800),
        (["sh", "-c", "echo only-err >&2"], 1800),
        (["sh", "-c", "seq 1 30; seq 1 15 >&2; exit 1"], 1800),
        (["sh", "-c", "printf 'a\\r\\nb\\rc\\013d\\n\\ne'"], 1800),
        (["sh", "-c", "echo 'ünï ☃'; echo '→' >&2; exit 2"], 1800),
        (["echo", "a b", "→"], 1800),
        (["sh", "-c", "kill -9 $$"], 1800),
        (["sh", "-c", "kill -TERM $$"], 1800),
        (["nonexistent-almanac-parity-xyz"], 1800),
        (["/etc/passwd"], 1800),
        (["/tmp"], 1800),
        (["./no/such/thing", "x"], 1800),
        (["sleep", "3"], 1),
        (["sh", "-c", "echo before; sleep 3"], 1),
        (["sh", "-c", "exit 255"], 300),
    ]
    for argv, t in runs:
        yield "_run", {"argv": argv, "timeout": t}, (lambda a=argv, t=t: _run(a, t))

    # ---- update_agents ----
    agent_sets = [
        ([], True),
        ([_app("claude", "2.1.0")], True),                                  # nothing behind
        ([_app("codex", "0.5.0", "0.6.0")], True),                          # no self-updater
        ([_app("codex", "0.5.0", "0.6.0", [])], True),                      # empty cmd
        ([_app("codex", "0.5.0", "0.6.0", "")], True),
        ([_app("ünï", "1", "", ["true"])], True),
        # these reach agents.hold_major (inventory.versions_behind)
        ([_app("opencode", "1.19.3", "2.0.30", ["true"])], True),           # major: held
        ([_app("claude", "2.1.265", "2.1.301", ["claude", "update"])], True),
        ([_app("claude", "2.1.265", "2.1.301", ["claude", "update"])], False),
        ([_app("x", "1.0", "1.1", "ab")], True),                            # list("ab")
        ([_app("a", "1.0", "1.1", ["a", "up"]), _app("b", "1.0", ""),
          _app("c", "3.0", "4.0", ["c"]), _app("d", "1.0", "1.0.1", ["d", "--yes"])], True),
    ]
    for apps, ok in agent_sets:
        yield "update_agents", {"apps": apps, "run_ok": ok}, (lambda a=apps, ok=ok: _agents(a, ok))

    # ---- update_appimages: faked check/download/install/save ----
    reg1 = {
        "kdenlive": {"path": "/opt/apps/kdenlive.AppImage", "version": "24.1", "latest": {"x": 1},
                     "checked_at": 99, "name": "Kdenlive"},
        "obsidian": {"path": "/opt/apps/obsidian.AppImage", "version": "1.5.3"},
        "nover": {"path": "/opt/apps//nover/./x.AppImage"},
        "numver": {"path": "/opt/ünï/a.AppImage", "version": 3},
        "nopath": {"version": "1"},
        "intpath": {"path": 5, "version": "1"},
    }
    fake_sets = [
        ({}, {}),
        (reg1, {}),
        (reg1, {"check": {"kdenlive": {"ok": _latest("25.0")}}}),
        (reg1, {"check": {"kdenlive": {"ok": _latest("25.0", url="")}}}),
        (reg1, {"check": {"kdenlive": {"ok": None}, "obsidian": {"err": "rate limited"}}}),
        (reg1, {"check": {k: {"ok": _latest("9.9")} for k in reg1}}),
        (reg1, {"check": {k: {"ok": _latest("9.9")} for k in reg1},
                "download_err": {"/opt/apps/obsidian.AppImage": "disk full"}}),
        (reg1, {"check": {k: {"ok": _latest("9.9")} for k in reg1},
                "install_err": {"kdenlive": "read-only"}}),
        (reg1, {"check": {"obsidian": {"ok": _latest("2.0 ünï")}}, "save_err": "denied"}),
        (reg1, {"check": {"nover": {"ok": _latest("1")}, "numver": {"ok": _latest("4")},
                          "nopath": {"ok": _latest("2")}, "intpath": {"ok": _latest("2")}}}),
    ]
    for reg, kw in fake_sets:
        args = {"reg": reg, "check": kw.get("check", {}),
                "download_err": kw.get("download_err", {}),
                "install_err": kw.get("install_err", {}), "save_err": kw.get("save_err")}
        yield "update_appimages", args, (lambda reg=reg, kw=kw: _appimages(reg, **kw))

    # the real check(), on records it can decide without the network
    f = _TMP / "plain.AppImage"
    f.write_bytes(b"not an elf")
    real_reg = {
        "missing": {"path": str(_TMP / "nope.AppImage"), "version": "1"},
        "noinfo": {"path": str(f), "version": "1"},
        "gitlab": {"path": str(f), "version": "1", "update_spec": {"kind": "gitlab"}},
        "noowner": {"path": str(f), "version": "1", "update_spec": {"kind": "github"}},
        "norepo": {"path": str(f), "version": "1", "update_spec": {"kind": "github", "owner": "o"}},
        "badspec": {"path": str(f), "version": "1", "update_spec": ["x"]},
    }
    yield "update_appimages", {"reg": real_reg, "real_check": True}, \
        (lambda: _appimages(real_reg, real_check=True))

    # the real registry loader, pointed at a temp state dir
    st = _TMP / "state"
    (st / "almanac").mkdir(parents=True)
    (st / "almanac" / "appimages.json").write_text(
        '{"a": {"path": "/nonexistent/a.AppImage", "version": "1"}}\n')
    yield "update_appimages", {"reg": None, "real_check": True, "env": {"XDG_STATE_HOME": str(st)}}, \
        (lambda: _with_registry(st, lambda: _appimages(None, real_check=True)))

    # ---- update_plugins ----
    pd = str(_PDIR)
    plugin_sets = [
        (pd, "all", {}, True),
        (pd, "all", None, True),                     # catalog fails: no trust
        (pd, "all", {}, False),
        (pd, "verified", {}, True),
        (pd, "verified", {"a.plugin": "verified"}, True),
        (pd, "verified", {"a.plugin": "verified", ".hidden.plugin": "unverified"}, True),
        (pd, "changed", {"a.plugin": "reviewed-stale", "ünï.plugin": "official"}, True),
        (pd, "official", {"ünï.plugin": "official"}, True),
        (pd, "no-such-scope", {}, True),
        (str(_TMP / "nonexistent"), "all", {}, True),
        (str(_PDIR / "file.txt"), "all", {}, True),
        (str(_PDIR / "plain"), "all", {}, True),
    ]
    for pdir, scope, trust, ok in plugin_sets:
        yield "update_plugins", {"pdir": pdir, "scope": scope, "trust": trust, "run_ok": ok}, \
            (lambda p=pdir, s=scope, t=trust, ok=ok: _plugins(p, s, t, ok))

    # ---- main ----
    on = {"agents": True, "appimages": True, "plugins": True}
    off = {"agents": False, "appimages": False, "plugins": False}
    mains = [
        ([], {}, {"agents": 2, "appimages": 1, "plugins": 5}, "80"),
        ([], on, {"agents": 2, "appimages": 1, "plugins": 5}, "80"),
        ([], off, {}, "80"),
        ([], {"agents": None, "appimages": 0, "plugins": "yes"}, {"plugins": 1}, "80"),
        ([], {"agents": [], "appimages": {}, "plugins": [1]}, {}, "80"),
        (["--agents"], off, {"agents": 3}, "80"),
        (["--plugins", "--appimages"], {}, {"plugins": 1}, "80"),
        (["--agents", "--agents"], {}, {}, "80"),
        (["--ag"], {}, {}, "80"),
        (["--app", "--pl"], {}, {}, "80"),
        (["--a"], {}, {}, "80"),
        (["--=x"], {}, {}, "80"),
        (["--ag=1"], {}, {}, "80"),
        (["--agents="], {}, {}, "80"),
        (["--plugins=a=b"], {}, {}, "80"),
        (["--help=x"], {}, {}, "80"),
        (["-h=x"], {}, {}, "80"),
        (["-h-x"], {}, {}, "80"),
        (["-hh=x"], {}, {}, "80"),
        (["-hx"], {}, {}, "80"),
        (["-hhh"], {}, {}, "80"),
        (["-x=1"], {}, {}, "80"),
        (["--bog=1"], {}, {}, "80"),
        (["--bogus"], {}, {}, "80"),
        (["--bogus", "--ag=1"], {}, {}, "80"),
        (["--bogus", "-h"], {}, {}, "80"),
        (["-h", "--a"], {}, {}, "80"),
        (["--ag=1", "-h"], {}, {}, "80"),
        (["foo", "bar", "--agents"], {}, {}, "80"),
        (["--", "--agents"], {}, {}, "80"),
        (["--agents", "--"], {}, {}, "80"),
        (["-"], {}, {}, "80"),
        (["-1"], {}, {}, "80"),
        (["-ah"], {}, {}, "80"),
        ([""], {}, {}, "80"),
        (["a b"], {}, {}, "80"),
        (["ünï"], {}, {}, "80"),
        (["-h"], {}, {}, "80"),
        (["--help"], {}, {}, "80"),
        (["--h"], {}, {}, "80"),
    ]
    for cols in ["40", "120", "60", "30", "20", "12", "1", "0", "abc", "200"]:
        mains.append((["--help"], {}, {}, cols))
        mains.append((["--bogus"], {}, {}, cols))
    for argv, cfg, counts, cols in mains:
        args = {"argv": argv, "cfg": cfg, "counts": counts, "env": {"COLUMNS": cols}}
        yield "main", args, (lambda a=argv, c=cfg, n=counts, col=cols: _main(a, c, n, col))
    # the real auto-update.json, read only
    for argv in [[], ["--plugins"]]:
        args = {"argv": argv, "cfg": None, "counts": {"agents": 1}, "env": {"COLUMNS": "80"}}
        yield "main", args, (lambda a=argv: _main(a, None, {"agents": 1}))

    # ---- review round 1 ----
    yield from _review1()
    # ---- review round 2 ----
    yield from _review2()
    # ---- review round 3 ----
    yield from _review3()


def _with_env(env, fn):
    with mock.patch.dict(os.environ, env):
        return fn()


# ------------------------------------------------------- review round 1 --
def _run_with_stdin(argv, text, timeout):
    """_run as the timer or a terminal would call it: stdin is inherited
    (here a pipe holding `text`), not /dev/null."""
    r, w = os.pipe()
    os.write(w, text.encode())
    os.close(w)
    saved = os.dup(0)
    os.dup2(r, 0)
    os.close(r)
    try:
        return _run(argv, timeout)
    finally:
        os.dup2(saved, 0)
        os.close(saved)


def _review1():
    # invalid UTF-8 on stdout: text=True decodes strictly and raises
    a = ["sh", "-c", "printf 'ok\\377\\n'"]
    yield "_run", {"argv": a, "timeout": 30}, (lambda a=a: _run(a, 30))
    # a grandchild keeps the pipes open after the child exits: Python's
    # communicate() times out, Rust waits for EOF with no limit
    a = ["sh", "-c", "(sleep 3) & exit 0"]
    yield "_run", {"argv": a, "timeout": 1}, (lambda a=a: _run(a, 1))
    # an executable script without a shebang: execve gives ENOEXEC
    ns = _TMP / "noshebang"
    ns.write_text("echo ran-by-sh\n")
    ns.chmod(0o755)
    a = [str(ns)]
    yield "_run", {"argv": a, "timeout": 30}, (lambda a=a: _run(a, 30))
    # stdin is inherited by the Python child
    a = ["head", "-n", "1"]
    # (the Rust side's own stdin is the request pipe, so it is handed the
    # same text to pass on)
    yield "_run", {"argv": a, "timeout": 5, "stdin": "from-stdin\n"}, \
        (lambda a=a: _run_with_stdin(a, "from-stdin\n", 5))

    # update_cmd holding a non-string, or not a list at all
    for cmd in [["true", 1], 5, True]:
        apps = [_app("x", "1.0", "1.1", cmd)]
        yield "update_agents", {"apps": apps, "run_ok": True}, (lambda a=apps: _agents(a, True))

    # check() results Python indexes outside the try
    regx = {"k": {"path": "/opt/k.AppImage", "version": "1"}}
    for ok in [{"url": "https://example.invalid/k"},
               {"version": 4, "url": "https://example.invalid/k"}]:
        kw = {"check": {"k": {"ok": ok}}}
        args = {"reg": regx, "check": kw["check"], "download_err": {}, "install_err": {},
                "save_err": None}
        yield "update_appimages", args, (lambda kw=kw: _appimages(regx, **kw))

    # the real check() on records that are not dicts / have an empty path
    odd = {"strrec": "x", "nullrec": None, "listrec": [1], "emptypath": {"path": "", "version": "1"}}
    yield "update_appimages", {"reg": odd, "real_check": True}, \
        (lambda: _appimages(odd, real_check=True))

    # a registry file that is valid JSON but not an object
    for i, text in enumerate(["[1, 2]\n", "null\n", '"x"\n']):
        st = _TMP / f"state-odd{i}"
        (st / "almanac").mkdir(parents=True)
        (st / "almanac" / "appimages.json").write_text(text)
        yield "update_appimages", {"reg": None, "real_check": True, "env": {"XDG_STATE_HOME": str(st)}}, \
            (lambda st=st: _with_registry(st, lambda: _appimages(None, real_check=True)))

    # COLUMNS as int() reads it
    for cols in ["1_20", "\uff11\uff12\uff10", " 50\n"]:
        args = {"argv": ["--help"], "cfg": {}, "counts": {}, "env": {"COLUMNS": cols}}
        yield "main", args, (lambda col=cols: _main(["--help"], {}, {}, col))
    # Python 3.14 argparse colours its output when FORCE_COLOR / PYTHON_COLORS ask
    for var in ["FORCE_COLOR", "PYTHON_COLORS"]:
        for argv in [["--bogus"], ["--help"]]:
            env = {"COLUMNS": "80", var: "1"}
            args = {"argv": argv, "cfg": {}, "counts": {}, "env": env}
            yield "main", args, (lambda a=argv, v=var: _with_env({v: "1"}, lambda: _main(a, {}, {}, "80")))


def _with_registry(st, fn):
    """inventory.REGISTRY is fixed when inventory is imported, so setting
    XDG_STATE_HOME afterwards does not move it: point it at the temp state
    dir as well (else the Python side reads the real user's registry)."""
    with mock.patch.dict(os.environ, {"XDG_STATE_HOME": str(st)}), \
         mock.patch.object(inventory, "REGISTRY", st / "almanac" / "appimages.json"):
        return fn()


# ------------------------------------------------------- review round 2 --
def _review2():
    # repr() escapes every non-printable character, not only C0/C1: the
    # TimeoutExpired text and the OSError filename both go through it
    for a, t in [(["sh", "-c", "sleep 3", "a\xa0b"], 1),
                 (["sh", "-c", "sleep 3", "\u200b\xad\u2028"], 1),
                 (["sh", "-c", "sleep 3", "\U0001f600"], 1),
                 (["no-such\xa0prog"], 30),
                 (["no-such\u200bprog"], 30),
                 ([""], 30)]:
        yield "_run", {"argv": a, "timeout": t}, (lambda a=a, t=t: _run(a, t))

    # a PATH whose only match is not executable
    pd = _TMP / "pathdir"
    pd.mkdir()
    (pd / "almanac-noexec").write_text("echo hi\n")
    env = {"PATH": str(pd)}
    a = ["almanac-noexec"]
    yield "_run", {"argv": a, "timeout": 30, "env": env}, \
        (lambda a=a, env=env: _with_env(env, lambda: _run(a, 30)))

    # update_cmd shapes list() accepts or rejects
    for cmd in [{"a": 1, "b": 2}, [["a"]], 1.5, None, 0, {}]:
        apps = [_app("x", "1.0", "1.1", cmd)]
        yield "update_agents", {"apps": apps, "run_ok": True}, (lambda a=apps: _agents(a, True))

    # registry numbers Python's json reads exactly and serde_json does not
    for i, text in enumerate([
            '{"big": {"path": "/opt/b.AppImage", "version": 100000000000000000000}}\n',
            '{"neg0": {"path": "/opt/n.AppImage", "version": -0}}\n',
            '{"inf": {"path": "/opt/i.AppImage", "version": 1e400}}\n',
            '{"nan": {"path": "/opt/n.AppImage", "version": NaN}}\n']):
        st = _TMP / f"state-num{i}"
        (st / "almanac").mkdir(parents=True)
        (st / "almanac" / "appimages.json").write_text(text)
        key = text.split('"')[1]
        check = {key: {"ok": _latest("2")}}
        args = {"reg": None, "check": check, "download_err": {}, "install_err": {},
                "save_err": None, "env": {"XDG_STATE_HOME": str(st)}}
        yield "update_appimages", args, \
            (lambda st=st, check=check: _with_registry(st, lambda: _appimages(None, check=check)))

    # pathlib normalisation of odd record paths
    regp = {"a": {"path": "//a//b/", "version": "1"}, "b": {"path": "./x/./y/", "version": "1"},
            "c": {"path": "/", "version": "1"}, "d": {"path": "a/../b", "version": "1"},
            "e": {"path": ".", "version": "1"}}
    kw = {"check": {k: {"ok": _latest("2")} for k in regp}}
    args = {"reg": regp, "check": kw["check"], "download_err": {}, "install_err": {}, "save_err": None}
    yield "update_appimages", args, (lambda kw=kw: _appimages(regp, **kw))

    # the real check() on record paths of the wrong type
    oddp = {"intpath": {"path": 5}, "nullpath": {"path": None}, "listpath": {"path": ["x"]},
            "strspec": {"path": str(_TMP / "plain.AppImage"), "update_spec": "abc"}}
    yield "update_appimages", {"reg": oddp, "real_check": True}, \
        (lambda: _appimages(oddp, real_check=True))

    # a plugin directory whose name is not UTF-8 (one commit behind)
    bd = _TMP / "bytes-plugins"
    bd.mkdir()
    up = _TMP / "up2"
    _git("init", "-q", "-b", "main", str(up), cwd=_TMP)
    (up / "manifest.json").write_text("{}")
    _git("add", ".", cwd=up)
    _git("commit", "-qm", "one", cwd=up)
    subprocess.run([b"git", b"clone", b"-q", str(up).encode(), str(bd).encode() + b"/\xff.plugin"],
                   check=True, capture_output=True)
    (up / "f").write_text("x")
    _git("add", ".", cwd=up)
    _git("commit", "-qm", "two", cwd=up)
    yield "update_plugins", {"pdir": str(bd), "scope": "all", "trust": {}, "run_ok": True}, \
        (lambda: _esc(_plugins(str(bd), "all", {}, True)))

    # config values Python reads by truthiness
    for cfg in [{"agents": 0.0, "appimages": "0", "plugins": -1},
                {"agents": "", "appimages": [0], "plugins": {"x": 0}}]:
        args = {"argv": [], "cfg": cfg, "counts": {}, "env": {"COLUMNS": "80"}}
        yield "main", args, (lambda c=cfg: _main([], c, {}, "80"))

    # more argparse
    for argv in [["-help"], ["-hh-"], ["--he"], ["--help", "--bogus"], ["--bogus", "--help"],
                 ["x", "-h"], ["--ap"], ["--p"], ["--", "--"], ["---agents"], ["-a"],
                 ["--agents x"], ["--ag=x y"], ["-1.5"], ["--ünï"], ["--plugins", "--plugins=1"],
                 ["-h", ""], ["--a", "--help"], ["--help", "--a"], ["-hh"], ["--hel=", "x"],
                 ["--agents", "-h", "--bogus"], ["-h\xa0"], ["--age\u00e9"]]:
        args = {"argv": argv, "cfg": {}, "counts": {}, "env": {"COLUMNS": "80"}}
        yield "main", args, (lambda a=argv: _main(a, {}, {}, "80"))
    for cols in ["+50", "50.0", "\u3000 60", "\U0001d7d1\U0001d7ce", "-5", "99999999999999999999999"]:
        args = {"argv": ["--help"], "cfg": {}, "counts": {}, "env": {"COLUMNS": cols}}
        yield "main", args, (lambda col=cols: _main(["--help"], {}, {}, col))


def _esc(v):
    """A result with each lone surrogate written as its \\udcff escape, as
    the Rust side writes them (its strings cannot hold one, and neither can
    the runner's JSON). The same text Python's stderr prints for one."""
    if isinstance(v, str):
        return "".join("\\u%04x" % ord(c) if 0xd800 <= ord(c) < 0xe000 else c for c in v)
    if isinstance(v, dict):
        return {_esc(k): _esc(x) for k, x in v.items()}
    if isinstance(v, (list, tuple)):
        return [_esc(x) for x in v]
    return v


# ------------------------------------------------------- review round 3 --
def _agents_detect():
    lines, calls = [], []

    def fake_run(argv, timeout=1800):
        calls.append([list(argv), timeout])
        return True

    with mock.patch.object(autorun, "_run", fake_run), \
         mock.patch.object(autorun, "log", lines.append):
        done = autorun.update_agents()
    return {"done": done, "log": lines, "calls": calls}


def _trust_keys(rows):
    try:
        d = {p.id: p.trust.key for p in [store.Plugin(**r) for r in rows]}
    except Exception:                                   # noqa: BLE001
        return {}
    # an id that is not a str can never match a plugin directory's name
    return {k: v for k, v in d.items() if isinstance(k, str)}


def _save_registry(st):
    with _with_registry_ctx(st):
        inventory.save_registry(inventory._load_registry())
        return inventory.REGISTRY.read_text()


@contextlib.contextmanager
def _with_registry_ctx(st):
    with mock.patch.dict(os.environ, {"XDG_STATE_HOME": str(st)}), \
         mock.patch.object(inventory, "REGISTRY", st / "almanac" / "appimages.json"):
        yield


def _review3():
    import cases_agents as ca

    # agents.detect() raising is not caught: a latest-versions cache that
    # holds a list makes _load_latest's blob.get raise AttributeError
    ls = {"codex": [{"version": "0.9.1", "active": True}]}
    d = ca._mise_dir(ls=ls, outdated={})
    fenv = {"PATH": f"{ca._FAKE}:/usr/bin", "FAKE_MISE_DIR": d}
    mise_od = json.dumps({"at": ca.NOW, "data": {"codex": {"current": "0.8", "latest": "0.9.1",
                                                           "release_url": "https://codex"}}})
    good = json.dumps({"at": ca.NOW, "versions": {"anthropics/claude-code": ["2.0.9", "https://rel"]}})
    for lc in ["[1, 2]", good]:
        func, args, run = ca.case("detect", {}, _agents_detect, env=fenv,
                                  files={"agent-latest.json": lc, "mise-outdated.json": mise_od})
        yield "update_agents_detect", args, run

    # p.trust raising inside update_plugins' try empties the trust table
    base = {"name": "n", "market": "omarchy"}
    trust_rows = [
        [{**base, "id": "ok"}, {**base, "id": "bad", "upstream_status": "failed",
                                "upstream_checked_at": None}],
        [{**base, "id": "ok", "upstream_status": "failed", "upstream_checked_at": "2026-01-02T03:04"}],
        [{**base, "id": "r", "review_method": "maintainer-reviewed", "reviewed_at": 5}],
        [{**base, "id": "r", "review_method": "maintainer-reviewed", "reviewed_at": 0}],
        [{**base, "id": "v", "coverage": "snapshot-verified", "validated_commit": None}],
        [{**base, "id": "v", "coverage": "update-unverified", "validated_commit": "abc",
          "upstream_commit": 7}],
        [{**base, "id": "v", "coverage": "update-unverified", "validated_commit": ["a"],
          "upstream_commit": "b"}],
        [{**base, "id": "m", "market": ["x"]}],
        [{**base, "id": ["x"]}],
        [{**base, "id": 5}, {**base, "id": "s"}],
        [{**base, "id": "b", "built_in": 1, "upstream_checked_at": None}],
        [{**base, "id": "u", "upstream_status": "unreachable", "upstream_checked_at": {"a": 1}}],
        [{**base, "id": "u", "upstream_status": "unreachable", "upstream_checked_at": [1, 2]}],
        [{**base, "id": "fp", "market": "artcraft", "repo": "https://github.com/storytold/x"}],
        [{**base, "id": "fp", "market": "artcraft", "repo": 5}],
        [{**base, "id": "fp", "market": "artcraft", "repo": "https://github.com/other/x",
          "upstream_status": "failed", "upstream_checked_at": 3}],
        [{**base, "id": "x", "market": None}],
    ]
    for rows in trust_rows:
        yield "trust_keys", {"rows": rows}, (lambda r=rows: _trust_keys(r))

    # argv that is not UTF-8: argparse reports it (stderr shows the escape)
    for raw in [[b"\xff"], [b"--agents=\xfe"], [b"--ag\xff"], [b"-h\xff"], [b"--agents", b"x\xffy"]]:
        argv = [os.fsdecode(b) for b in raw]
        args = {"argv_bytes": [list(b) for b in raw], "cfg": {}, "counts": {}, "env": {"COLUMNS": "80"}}
        yield "main", args, (lambda a=argv: _esc(_main(a, {}, {}, "80")))

    # _run on arguments that are not UTF-8
    for raw, t in [([b"sh", b"-c", b"exit 3", b"\xff"], 30),
                   ([b"sh", b"-c", b"sleep 3", b"a\xffb"], 1),
                   ([b"no-such-\xff"], 30)]:
        argv = [os.fsdecode(b) for b in raw]
        yield "_run", {"argv_bytes": [list(b) for b in raw], "timeout": t}, \
            (lambda a=argv, t=t: _esc(_run(a, t)))

    # registry strings with lone surrogate escapes
    st = _TMP / "state-sur"
    (st / "almanac").mkdir(parents=True)
    (st / "almanac" / "appimages.json").write_text(
        '{"k\\udcff": {"path": "/opt/\\udcffx.AppImage", "version": "1\\ud800"}, '
        '"p": {"path": "/opt/p.AppImage", "version": "\\ud83d\\ude00 \\u00e9"}}\n')
    check_py = {"k\udcff": {"ok": _latest("2")}, "p": {"ok": _latest("3")}}
    check_rs = {_esc(k): v for k, v in check_py.items()}
    args = {"reg": None, "check": check_rs, "download_err": {}, "install_err": {},
            "save_err": None, "env": {"XDG_STATE_HOME": str(st)}}
    yield "update_appimages", args, \
        (lambda: _esc(_with_registry(st, lambda: _appimages(None, check=check_py))))

    # what save_registry writes back: numbers and strings exactly as read
    texts = [
        '{"big": {"version": 100000000000000000000, "n": -0, "f": 1e400, "g": -1e400, '
        '"h": NaN, "i": 1.5e-400, "j": 1E5, "k": -0.0, "l": 12345678901234567890123}}\n',
        '{"s": "\\udcff\\u00e9\\u2028\\ud83d\\ude00 \\" \\\\ \\/ \\b\\f\\n\\r\\t\\u007f", '
        '"dup": 1, "dup": [true, false, null, [], {}], "a": {"x": [1, {"y": 2.5}]}}\n',
        '{"a": 1}  \n\t',
        '[1, 2',
        '{"a": 1,}',
        '{"a": 01}',
        '{"a": "x\ty"}',
        '{"a": 1} x',
        '\ufeff{"a": 1}',
        '{"a": 1.}',
        '{"a": 1e}',
        '{"a": -}',
        '{"a": "\\ud800\\u0041"}',
        '{"a": [' + '1' * 4301 + ']}',
        '{"a": [' + '1' * 4300 + ']}',
        '',
        '  ',
    ]
    for i, text in enumerate(texts):
        py, rs = _TMP / f"save-py{i}", _TMP / f"save-rs{i}"
        for s in (py, rs):
            (s / "almanac").mkdir(parents=True)
            (s / "almanac" / "appimages.json").write_text(text)
        yield "save_registry", {"env": {"XDG_STATE_HOME": str(rs)}}, (lambda s=py: _save_registry(s))
