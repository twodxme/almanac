"""Parity cases for watcher.py: the autostart entry (written and removed in a
temp XDG_CONFIG_HOME), Exec quoting, the pid file and the running check
(against a harmless sleeper started here with `--guard` in its command line),
the remembered-reports file, both announcers (with _notify mocked: the calls
are compared, nothing is shown), and the plugin-update scan over a fixture
plugins dir of local git repos.

Nothing on the machine changes: every write goes to a temp dir named in
XDG_CONFIG_HOME / XDG_STATE_HOME / XDG_RUNTIME_DIR. The real machine is only
read (enabled, launcher, running, the reported file). `plugin_updates` is not
run against the real plugins dir: it git-fetches into each checkout.
`main` runs a GLib loop until signalled and is not a parity case.
`launcher`'s fallback (no `almanac` on PATH) differs by design -- the Python
names its interpreter and app.py, the port names its own binary -- so it is
only compared with an `almanac` on PATH."""
import inspect
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from unittest import mock

import guard
import watcher

_ROOT = Path(tempfile.mkdtemp(prefix="almanac-parity-watcher-"))
_n = 0
_SIG = inspect.signature(watcher._notify)


def _dir():
    global _n
    _n += 1
    d = _ROOT / str(_n)
    d.mkdir()
    return str(d)


class _env:
    """mock.patch.dict that also unsets the keys given as None."""
    def __init__(self, env):
        self.env = env

    def __enter__(self):
        self.old = {k: os.environ.get(k) for k in self.env}
        for k, v in self.env.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v

    def __exit__(self, *exc):
        for k, v in self.old.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v


def _files(files):
    for p, text in (files or {}).items():
        p = Path(p)
        if text is None:
            p.unlink(missing_ok=True)
        else:
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(text)


def _case(func, fn, env=None, files=None, **kw):
    """(func, args, lambda): the Rust side sets the same env and files."""
    def run():
        with _env(env or {}):
            _files(files)
            return fn()
    args = dict(kw)
    if env:
        args["env"] = env
    if files:
        args["files"] = files
    return func, args, run


def _captured(fn):
    with mock.patch.object(watcher, "_notify") as notify:
        result = fn()
    calls = []
    for c in notify.call_args_list:
        b = _SIG.bind(*c.args, **c.kwargs)
        b.apply_defaults()
        calls.append([b.arguments["title"], b.arguments["body"],
                      b.arguments["page"], b.arguments["urgency"]])
    return {"result": result, "calls": calls}


def _settings(state, **values):
    return {f"{state}/almanac/settings.json": json.dumps(values)}


def _git(*args, cwd=None):
    env = {**os.environ, "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@t",
           "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@t",
           "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_SYSTEM": "/dev/null"}
    subprocess.run(["git", *args], cwd=cwd, env=env, check=True,
                   capture_output=True)


def _plugin_tree(cfg):
    """A plugins dir with one plugin per answer."""
    pdir = Path(cfg) / "omarchy" / "plugins"
    pdir.mkdir(parents=True)
    origin = Path(cfg) / "origin.git"
    work = Path(cfg) / "work"
    _git("init", "-q", "-b", "main", str(work))
    (work / "manifest.json").write_text("{}")
    _git("add", ".", cwd=work)
    _git("commit", "-q", "-m", "one", cwd=work)
    _git("clone", "-q", "--bare", str(work), str(origin))

    def clone(name):
        _git("clone", "-q", str(origin), str(pdir / name))
        return pdir / name
    clone("current")
    behind = [clone("behind"), clone("ü-behind"), clone(".hidden-behind"),
              clone("zz-behind"), clone("A-behind")]
    nomani = clone("no-manifest-behind")
    (nomani / "manifest.json").unlink()
    _git("commit", "-q", "-am", "drop manifest", cwd=nomani)
    modified = clone("modified-behind")
    # upstream moves on
    (work / "manifest.json").write_text('{"v": 2}')
    _git("commit", "-q", "-am", "two", cwd=work)
    _git("push", "-q", str(origin), "main", cwd=work)
    (modified / "manifest.json").write_text('{"mine": 1}')
    plain = pdir / "plain"
    plain.mkdir()
    (plain / "manifest.json").write_text("{}")
    (pdir / "linked").symlink_to(pdir / "behind")
    (pdir / "a-file").write_text("not a plugin")
    return behind


def cases():
    cfg = _dir()

    # ---- constants and paths
    yield "ENTRY_NAME", {}, lambda: watcher.ENTRY_NAME
    yield "PLUGIN_CHECK_EVERY", {}, lambda: watcher.PLUGIN_CHECK_EVERY
    yield "autostart_path", {}, lambda: watcher.autostart_path()
    yield _case("autostart_path", watcher.autostart_path, env={"XDG_CONFIG_HOME": cfg})
    yield _case("autostart_path", watcher.autostart_path, env={"XDG_CONFIG_HOME": "/tmp/ü dir"})
    yield _case("autostart_path", watcher.autostart_path, env={"XDG_CONFIG_HOME": "", "HOME": "/home/nobody"})
    yield "_reported_path", {}, lambda: watcher._reported_path()
    yield _case("_reported_path", watcher._reported_path, env={"XDG_STATE_HOME": cfg})
    yield _case("_plugins_seen_path", watcher._plugins_seen_path, env={"XDG_STATE_HOME": cfg})
    yield "_plugins_seen_path", {}, lambda: watcher._plugins_seen_path()
    yield "_pid_path", {}, lambda: watcher._pid_path()
    yield _case("_pid_path", watcher._pid_path, env={"XDG_RUNTIME_DIR": "/run/x"})
    yield _case("_pid_path", watcher._pid_path, env={"XDG_RUNTIME_DIR": "", "XDG_STATE_HOME": "/s"})
    yield _case("_pid_path", watcher._pid_path, env={"XDG_RUNTIME_DIR": None, "XDG_STATE_HOME": "/s"})

    # ---- launcher: the real PATH, and a fake almanac on a PATH of its own
    yield "launcher", {}, lambda: watcher.launcher()
    fake = Path(_dir())
    (fake / "almanac").write_text("#!/bin/sh\n")
    (fake / "almanac").chmod(0o755)
    yield _case("launcher", watcher.launcher, env={"PATH": f"{fake}:/usr/bin"})
    nonexec = Path(_dir())
    (nonexec / "almanac").write_text("#!/bin/sh\n")
    yield _case("launcher", watcher.launcher, env={"PATH": f"{nonexec}:{fake}"})
    spaced = Path(_dir()) / "with space $x"
    spaced.mkdir()
    (spaced / "almanac").write_text("#!/bin/sh\n")
    (spaced / "almanac").chmod(0o755)
    yield _case("launcher", watcher.launcher, env={"PATH": str(spaced)})

    # ---- enabled / enable / disable
    yield "enabled", {}, lambda: watcher.enabled()
    yield _case("enabled", watcher.enabled, env={"XDG_CONFIG_HOME": _dir()})
    d = _dir()
    yield _case("enabled", watcher.enabled, env={"XDG_CONFIG_HOME": d},
                files={f"{d}/autostart/{watcher.ENTRY_NAME}": "[Desktop Entry]\n"})
    d = _dir()
    os.makedirs(f"{d}/autostart/{watcher.ENTRY_NAME}")      # a directory is not an entry
    yield _case("enabled", watcher.enabled, env={"XDG_CONFIG_HOME": d})

    def enable_then_read():
        watcher.enable(start_now=False)
        p = watcher.autostart_path()
        return {"entry": p.read_text() if p.is_file() else None, "enabled": watcher.enabled()}
    for path in [f"{fake}:/usr/bin", str(spaced)]:
        yield _case("enable", enable_then_read,
                    env={"XDG_CONFIG_HOME": _dir(), "PATH": path}, start_now=False)
    # an existing entry is rewritten
    d = _dir()
    yield _case("enable", enable_then_read,
                env={"XDG_CONFIG_HOME": d, "PATH": f"{fake}:/usr/bin"},
                files={f"{d}/autostart/{watcher.ENTRY_NAME}": "old"}, start_now=False)
    # the autostart dir cannot be made: a file is in the way
    d = _dir()
    yield _case("enable", enable_then_read,
                env={"XDG_CONFIG_HOME": d, "PATH": f"{fake}:/usr/bin"},
                files={f"{d}/autostart": "a file"}, start_now=False)

    def disable_then_read():
        watcher.disable()
        return {"enabled": watcher.enabled()}
    run = _dir()        # no pid file: nothing is signalled
    d = _dir()
    yield _case("disable", disable_then_read,
                env={"XDG_CONFIG_HOME": d, "XDG_RUNTIME_DIR": run},
                files={f"{d}/autostart/{watcher.ENTRY_NAME}": "[Desktop Entry]\n"})
    yield _case("disable", disable_then_read,
                env={"XDG_CONFIG_HOME": _dir(), "XDG_RUNTIME_DIR": run})
    d = _dir()
    os.makedirs(f"{d}/autostart/{watcher.ENTRY_NAME}")      # unlink fails: IsADirectoryError
    yield _case("disable", disable_then_read,
                env={"XDG_CONFIG_HOME": d, "XDG_RUNTIME_DIR": run})

    # ---- _quote
    for arg in ["/usr/bin/almanac", "/path with space/app.py", "--guard", "",
                'say "hi"', "back\\slash", "$HOME", "`cmd`", "a'b", "tab\there",
                "new\nline", "x>y", "~/bin", "a|b&c;d", "*?#()", "ü/ünïcode",
                "日本語", "%u %f", "=equals", 'all $`"\\ at once']:
        yield "_quote", {"arg": arg}, (lambda a=arg: watcher._quote(a))

    # ---- running: the real session, and pid files of every shape
    yield "running", {}, lambda: watcher.running()
    sleeper = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(300)", "--guard"],
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                               start_new_session=True)
    other = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(300)"],
                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                             start_new_session=True)
    pid = sleeper.pid
    digits = str(pid)
    contents = [str(pid), f"  {pid}\n", f"+{pid}", f"{digits[0]}_{digits[1:]}",
                f"0{pid}", str(other.pid), "1", "0", "-1", f"-{pid}", "", "abc",
                f"{pid}x", f"{pid} {pid}", "99999999", "1__2", "_1", "\n",
                "٣", f"{pid}\x00"]
    for text in contents:
        r = _dir()
        yield _case("running", watcher.running, env={"XDG_RUNTIME_DIR": r},
                    files={f"{r}/almanac-guard.pid": text})
    yield _case("running", watcher.running, env={"XDG_RUNTIME_DIR": _dir()})
    s = _dir()
    yield _case("running", watcher.running, env={"XDG_RUNTIME_DIR": "", "XDG_STATE_HOME": s},
                files={f"{s}/almanac/almanac-guard.pid": str(pid)})
    r = _dir()
    Path(r, "almanac-guard.pid").write_bytes(b"\xff\xfe")     # not UTF-8
    yield _case("running", watcher.running, env={"XDG_RUNTIME_DIR": r})

    # ---- _load_reported / _save_reported
    yield "_load_reported", {}, lambda: watcher._load_reported()
    for text in ['["/c/a.json.bak.1700000000", "/c/b.bak.1"]', "[]", "{}",
                 '{"k": 1, "ü": 2}', '"abc"', "123", "null", "true", "not json",
                 "", '[["nested"]]', '[{"a": 1}]', '["ü/日本.bak.1", "x"]',
                 '["dup", "dup"]', "﻿[]", " [\"a\"] \n"]:
        st = _dir()
        yield _case("_load_reported", watcher._load_reported, env={"XDG_STATE_HOME": st},
                    files={f"{st}/almanac/guard-reported.json": text})
    yield _case("_load_reported", watcher._load_reported, env={"XDG_STATE_HOME": _dir()})

    def save_then_read(seen):
        def run():
            watcher._save_reported(set(seen))
            p = watcher._reported_path()
            return p.read_text() if p.is_file() else None
        return run
    for seen in [[], ["b", "a"], ["ü/日本.bak.1", "\"quoted\"", "z", "A"], ["tab\t", "\x01"]]:
        yield _case("_save_reported", save_then_read(seen),
                    env={"XDG_STATE_HOME": _dir()}, seen=seen)
    st = _dir()
    yield _case("_save_reported", save_then_read(["a"]), env={"XDG_STATE_HOME": st},
                files={f"{st}/almanac": "a file in the way"}, seen=["a"])

    # ---- announce
    def reset(path, backup, when=1.0, lost=()):
        return {"path": path, "backup": backup, "when": when, "lost": list(lost)}

    def announce_case(found, seen, env):
        def run():
            rs = [guard.Reset(Path(r["path"]), Path(r["backup"]), r["when"], r["lost"])
                  for r in found]
            return _captured(lambda: watcher.announce(rs, set(seen)))
        st = _dir()
        env = dict(env)
        settings = env.pop("_settings", {})
        e = {"XDG_STATE_HOME": st, **env}
        return _case("announce", run, env=e, files=_settings(st, **settings),
                     found=found, seen=seen)

    one = reset("/c/omarchy/shell.json", "/c/omarchy/shell.json.bak.1700000000", 1.0, ["x"])
    two = reset("/c/hypr/bindings.conf", "/c/hypr/bindings.conf.bak.1700000001", 2.0, ["a", "b"])
    none_lost = reset("/elsewhere/ü.json", "/elsewhere/ü.json.bak.1700000002")
    yield announce_case([], [], {"XDG_CONFIG_HOME": "/c"})
    yield announce_case([one], [], {"XDG_CONFIG_HOME": "/c"})
    yield announce_case([one, two, none_lost], [], {"XDG_CONFIG_HOME": "/c"})
    yield announce_case([one, two], [one["backup"]], {"XDG_CONFIG_HOME": "/c"})
    yield announce_case([one, two], [one["backup"], two["backup"], "/old"], {"XDG_CONFIG_HOME": "/c"})
    yield announce_case([one, two], ["/old"], {"XDG_CONFIG_HOME": "/other"})
    yield announce_case([one, none_lost], [], {"XDG_CONFIG_HOME": "/c", "_settings": {"notify_resets": False}})
    yield announce_case([one], [], {"XDG_CONFIG_HOME": "/c", "_settings": {"notify_resets": "yes"}})
    yield announce_case([reset("/c/x", "/c/x.bak.1", 1.0, [str(i) for i in range(12)])], [],
                        {"XDG_CONFIG_HOME": "/c"})

    # ---- plugin_updates
    yield _case("plugin_updates", watcher.plugin_updates, env={"XDG_CONFIG_HOME": _dir()})
    tree = _dir()
    _plugin_tree(tree)
    yield _case("plugin_updates", watcher.plugin_updates, env={"XDG_CONFIG_HOME": tree})
    d = _dir()
    Path(d, "omarchy").mkdir()
    Path(d, "omarchy", "plugins").write_text("a file, not a dir")
    yield _case("plugin_updates", watcher.plugin_updates, env={"XDG_CONFIG_HOME": d})

    # ---- announce_plugin_updates
    def apu(waiting, last, **settings):
        st = _dir()
        return _case("announce_plugin_updates",
                     lambda: _captured(lambda: watcher.announce_plugin_updates(list(waiting), set(last))),
                     env={"XDG_STATE_HOME": st}, files=_settings(st, **settings),
                     waiting=waiting, last=last)
    yield apu([], [])
    yield apu([], ["a"])
    yield apu(["a", "b"], [])
    yield apu(["a", "b"], ["a", "b"])
    yield apu(["a"], ["a", "b"])
    yield apu(["a", "c"], ["a"])
    yield apu(["e", "d", "c", "b", "a"], [])
    yield apu(["d", "c", "b", "a"], [])
    yield apu(["ü", "日本", "z", "Z", "a", "é"], ["z"])
    yield apu(["a", "a"], [])
    yield apu(["a"], [], notify_plugin_updates=False)
    yield apu(["a"], [], notify_plugin_updates=0)

    # ---- review round 1: suspected divergences
    # running: Python's int() takes any Unicode decimal digits (fullwidth
    # here) and strips every str.isspace() character, \x1c-\x1f included.
    full = "".join(chr(0xFF10 + int(c)) for c in digits)
    for text in [full, f"\x1c{pid}", f"{pid}\x1f", f"\x85{pid}", f" {pid}",
                 "2147483648", "99999999999999999999", "-2147483649"]:
        r = _dir()
        yield _case("running", watcher.running, env={"XDG_RUNTIME_DIR": r},
                    files={f"{r}/almanac-guard.pid": text})
    # _load_reported: json values that are hashable but not strings
    for text in ['[1, 2]', '[true]', '[null]', '[1.5]', '["a", 1]']:
        st = _dir()
        yield _case("_load_reported", watcher._load_reported, env={"XDG_STATE_HOME": st},
                    files={f"{st}/almanac/guard-reported.json": text})

    # ---- review round 2
    # running: more scripts' digits and spaces; ints that need no C int.
    for text in [f"\u3000{pid}\u2028", "".join(chr(0x660 + int(c)) for c in digits),
                 "".join(chr(0x1D7CE + int(c)) for c in digits), "\u00b2", f"{pid}_",
                 "2147483647", "-2147483648", "1" * 4301, "0" * 4300 + "1",
                 f"- {pid}", f"＋{pid}"]:
        r = _dir()
        yield _case("running", watcher.running, env={"XDG_RUNTIME_DIR": r},
                    files={f"{r}/almanac-guard.pid": text})

    # The reported file loaded and saved back, as a guard restart does:
    # lone surrogates (which no JSON transport carries) are compared as the
    # file text they come back as; mixed kinds are the TypeError of sorted().
    def roundtrip():
        watcher._save_reported(watcher._load_reported())
        p = watcher._reported_path()
        return p.read_text() if p.is_file() else None
    for text in ['["\\ud800"]', '{"\\ud800": 1}', '"\\ud800"', '["\\udc80", "b", "a"]',
                 '["\\ud83d\\ude00", "\\ud83d", "\\ude00"]', '["\\ud83d\\u0041"]',
                 '[1, 2]', '[true, 1, 1.0]', '[1.0, true]', '[false, 0, -0.0]',
                 '[-0.0, 0]', '[2, 1.5, -1, 1e400, -Infinity]', '[NaN]', '[Infinity]',
                 '[123456789012345678901234567890, 1.2345678901234568e+29]',
                 '[1e22, 10000000000000000000000]', '[null]', '[null, null]',
                 '["a", 1]', '[null, 1]', '["a", null]', '[true, "x"]', '[1, 2, "x"]',
                 '["a", [1]]', '["a", {}]', '[1,]', '[01]', '[1.]', '[1e5]', '[1E-2]',
                 '[-]', '[- 1]', '[nan]', '["\t"]', '["\\x"]', '["\\u12"]',
                 '["\\u00e9\\/"]', '{"a" 1}', '{"a": 1,}', '{"a": 1, "a": 2}',
                 '{"x": [[[{"y": NaN}]]], "z": null}', '{1: 2}', '[\'a\']',
                 '[' + '1' * 4301 + ']', '[' + '1' * 4300 + ']', '\r\n[ "a" ,\t"b" ]\n',
                 '[' + '[' * 1000 + ']' * 1000 + ']', '"a\\ud800b"', 'true', '1.5', '-Infinity']:
        st = _dir()
        yield _case("_reported_roundtrip", roundtrip, env={"XDG_STATE_HOME": st},
                    files={f"{st}/almanac/guard-reported.json": text})
    # deep nesting: the decoder's RecursionError, which is not caught
    st = _dir()
    yield _case("_reported_roundtrip", roundtrip, env={"XDG_STATE_HOME": st},
                files={f"{st}/almanac/guard-reported.json": "[" * 200000 + "]" * 200000})

    # shutil.which itself: CS_PATH when PATH is unset, nothing when it is empty
    for name, path in [("sh", None), ("sh", ""), ("sh", "/nonexistent::/usr/bin"),
                       ("almanac", f"{fake}"), ("almanac", f"{fake}/"), ("almanac", f"{fake}//"),
                       ("no-such-tool-x", None)]:
        yield _case("_which", lambda n=name: shutil.which(n), env={"PATH": path}, name=name)
    # launcher: shutil.which asks os.access(X_OK) for *this* user; an x bit
    # for others only does not make it runnable for us.
    oexec = Path(_dir())
    (oexec / "almanac").write_text("#!/bin/sh\n")
    (oexec / "almanac").chmod(0o601)
    yield _case("launcher", watcher.launcher, env={"PATH": f"{oexec}:{fake}"})
    gexec = Path(_dir())
    (gexec / "almanac").write_text("#!/bin/sh\n")
    (gexec / "almanac").chmod(0o610)
    yield _case("launcher", watcher.launcher, env={"PATH": f"{gexec}:{fake}"})
    # a FIFO with an x bit: shutil.which only asks "exists, X_OK, not a dir"
    fifo = Path(_dir())
    os.mkfifo(fifo / "almanac", 0o755)
    yield _case("launcher", watcher.launcher, env={"PATH": f"{fifo}:{fake}"})
    # settings of the wrong type fall back to the default (True)
    for v in ["", None, 1, [], 0.0, "false"]:
        yield announce_case([one], [], {"XDG_CONFIG_HOME": "/c", "_settings": {"notify_resets": v}})
        yield apu(["a"], [], notify_plugin_updates=v)

    # ---- review round 2 (adversarial pass)
    # json's NaN literal is one shared float object (decoder._CONSTANTS), so
    # a set keeps a single NaN however many the file lists. Where sorted()
    # leaves that NaN follows the set's order, which hangs on the NaN
    # object's address (it differs between a bare script, this runner and
    # a process that imported gi), so the members are compared, not their
    # order.
    def roundtrip_members():
        text = roundtrip()
        return None if text is None else sorted(text[1:-1].split(", "))
    for text in ['[NaN, NaN]', '[NaN, 1, NaN]', '[Infinity, Infinity, NaN, NaN, -Infinity]']:
        st = _dir()
        yield _case("_reported_roundtrip_members", roundtrip_members, env={"XDG_STATE_HOME": st},
                    files={f"{st}/almanac/guard-reported.json": text})
    # values the JSON transport cannot carry as numbers (and NaN != NaN):
    # compared as json.dumps text
    for text in ['[1e400]', '[NaN]', '[-Infinity]']:
        st = _dir()
        yield _case("_load_reported_dumps", lambda: json.dumps(sorted(watcher._load_reported())),
                    env={"XDG_STATE_HOME": st},
                    files={f"{st}/almanac/guard-reported.json": text})
    # nesting just past the port's fixed limit (52000), below Python's
    for depth in [52001, 52050, 52090]:
        st = _dir()
        yield _case("_reported_roundtrip", roundtrip, env={"XDG_STATE_HOME": st},
                    files={f"{st}/almanac/guard-reported.json": "[" * depth + "]" * depth})
    # Path() normalises what the env var gives; the port joins it raw
    for rt in ["/run//x/", ".", "/run/./x", "///run", "rel/dir"]:
        yield _case("_pid_path", watcher._pid_path, env={"XDG_RUNTIME_DIR": rt})
    # surrogate escapes around a would-be pair
    for text in ['["\\ud800\\x"]', '["\\ud800\\u12"]', '["\\ud800\\"]', '["\\ud800\\ud800"]',
                 '["\\udc00\\ud800"]', '["\\ud800\\u00e9"]', '["\\uD83D\\uDE00"]']:
        st = _dir()
        yield _case("_reported_roundtrip", roundtrip, env={"XDG_STATE_HOME": st},
                    files={f"{st}/almanac/guard-reported.json": text})
    # plugin dirs whose names are not UTF-8: Python's d.name is
    # surrogateescape'd, and sorts by code point (U+DC80 after U+00E9).
    tree = _dir()
    _plugin_tree(tree)
    pdir = os.fsencode(Path(tree) / "omarchy" / "plugins")
    for raw in [b"\x80-behind", b"\xc3\xa9-behind", b"\xff-behind"]:
        _git("clone", "-q", os.fsdecode(Path(tree) / "origin.git"), os.fsdecode(pdir + b"/" + raw))
        # make it behind: reset to the first commit
        _git("reset", "-q", "--hard", "HEAD~1", cwd=os.fsdecode(pdir + b"/" + raw))
    # json.dumps of each name: lone surrogates escaped, as no JSON transport
    # carries them
    yield _case("_plugin_updates_escaped", lambda: [json.dumps(n) for n in watcher.plugin_updates()],
                env={"XDG_CONFIG_HOME": tree})
