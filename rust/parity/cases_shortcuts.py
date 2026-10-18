"""Parity cases for shortcuts.py.

Readers run against the real machine (the user's own bindings.lua and live
`hyprctl` -- read-only queries only) and against fixture configs in a temp
dir: `env` in the args (XDG_CONFIG_HOME, PATH) is applied on both sides for
the call. `set_shortcut` never reaches the real Hyprland: both sides use the
same scripted hyprctl (the Python test's mock), and each side gets its own
fresh copy of the fixture file, so the Python call cannot change what the
Rust call sees. Nothing outside the temp dir is written.
"""
import atexit
import json
import os
import re
import shutil
import stat
import subprocess
import tempfile
from pathlib import Path
from unittest import mock

import shortcuts

_TMP = Path(tempfile.mkdtemp(prefix="almanac-parity-shortcuts-"))
atexit.register(shutil.rmtree, _TMP, True)
_N = [0]


def _call(env, fn):
    def run():
        saved = {k: os.environ.get(k) for k in env}
        try:
            for k, v in env.items():
                if v is None:
                    os.environ.pop(k, None)
                else:
                    os.environ[k] = v
            return fn()
        finally:
            for k, v in saved.items():
                if v is None:
                    os.environ.pop(k, None)
                else:
                    os.environ[k] = v
    return run


def _config(content, mode=0o644, kind="file"):
    """A fresh XDG_CONFIG_HOME whose hypr/bindings.lua is `content`."""
    _N[0] += 1
    home = _TMP / f"c{_N[0]}"
    hypr = home / "hypr"
    hypr.mkdir(parents=True)
    path = hypr / "bindings.lua"
    if kind == "file":
        if isinstance(content, bytes):
            path.write_bytes(content)
        else:
            path.write_bytes(content.encode("utf-8"))
        path.chmod(mode)
    elif kind == "symlink":
        real = home / "real.lua"
        real.write_text(content)
        path.symlink_to(real)
    elif kind == "dir":
        path.mkdir()
    return str(home)


def _bin(*names):
    d = _TMP / ("bin-" + "-".join(names))
    d.mkdir(exist_ok=True)
    for n in names:
        f = d / n
        f.write_text("#!/bin/sh\nexit 0\n")
        f.chmod(0o755)
    return str(d)


ORIGINAL = '-- mine\no.bind("SUPER + SPACE", "Omarchy menu", "x")\n'
BINDS = [{"modmask": 64, "key": "SPACE", "description": "Omarchy menu",
          "keycode": 0, "mouse": False, "catch_all": False, "submap": ""}]


def _block(pid, combo):
    return shortcuts._block(pid, combo)


def _fake(fake):
    state = {"reloaded": False}
    calls = []

    def run(*args):
        calls.append(list(args))
        if fake.get("fail") and args and args[0] == fake["fail"]:
            raise ValueError("fake failure")
        if args[:2] == ("-j", "binds"):
            return fake.get("binds", "")
        if args == ("reload",):
            state["reloaded"] = True
            return "ok"
        if args == ("configerrors",):
            return fake.get("after", "") if state["reloaded"] else fake.get("before", "")
        return ""
    return run, calls


def _set(content, pid, combo, fake, report=True, mode=0o644):
    """One set_shortcut case: separate fixture copies for each side."""
    py_home, rs_home = _config(content, mode), _config(content, mode)

    def run():
        hypr, calls = _fake(fake)
        with mock.patch.object(shortcuts, "_hyprctl", side_effect=hypr):
            if not report:
                return shortcuts.set_shortcut(pid, combo)
            try:
                result, error = shortcuts.set_shortcut(pid, combo), None
            except ValueError as e:
                if type(e) is not ValueError:
                    raise
                result, error = None, str(e)
        try:
            file = shortcuts.bindings_path().read_text()
        except OSError:
            file = None
        return {"result": result, "error": error, "file": file, "calls": calls}
    args = {"pid": pid, "combo": combo, "fake": fake, "report": report,
            "env": {"XDG_CONFIG_HOME": rs_home}}
    return "set_shortcut", args, _call({"XDG_CONFIG_HOME": py_home}, run)


def cases():
    real = {}
    # -- bindings_path
    yield "bindings_path", {}, shortcuts.bindings_path
    for v in ["/srv/cfg", "relative", "/tmp/ünï cødé", ""]:
        env = {"XDG_CONFIG_HOME": v}
        yield "bindings_path", {"env": env}, _call(env, shortcuts.bindings_path)

    # -- available: the real machine, then fixtures
    yield "available", {}, shortcuts.available
    with_bind = ORIGINAL
    hypr_path = _bin("hyprctl")
    none_path = _bin()
    scen = [
        (_config(with_bind), hypr_path),
        (_config(with_bind), none_path),
        (_config("-- nothing bound here\n"), hypr_path),
        (_config("o.bind"), hypr_path),
        (_config("o.\r\nbind("), hypr_path),
        (_config(b"\xff\xfeo.bind(\xff"), hypr_path),
        (_config(with_bind, kind="symlink"), hypr_path),
        (_config(with_bind, kind="dir"), hypr_path),
        (_config(None, kind="none"), hypr_path),
        (str(_TMP / "missing"), hypr_path),
    ]
    for home, path in scen:
        env = {"XDG_CONFIG_HOME": home, "PATH": path}
        yield "available", {"env": env}, _call(env, shortcuts.available)

    # -- normalise
    for v in ["super+alt+v", "alt+super+v", "SUPER + ALT + V", "ctrl+shift+f12",
              "control+t", "Control + Super + Return", "super+shift+ctrl+alt+bracketleft",
              "v", "super", "super+alt", "super+v+b", "super+super+v", "super+v+v",
              "", "+", "super+", "super++v", "super+f13", "super+f0", "super+f1",
              "super+f10", "super+a1", "super+xf86audioraisevolume", " super \t+ v ",
              "super+v\x1c", "super+v\n", "super+ä", "super+ß", "super+ı", "super+ſ",
              "ＳＵＰＥＲ+v", "super+日", "super+controll", "ctrl+control+v",
              "super+code:65", "super+mouse:272", "shift+space", "super + slash",
              "SUPER+comma", "super+0", "super+9", "super+-", "caps+v"]:
        yield "normalise", {"value": v}, (lambda v=v: shortcuts.normalise(v))

    # -- _block
    for pid, combo in [("lxp.vitals", "SUPER + ALT + V"), ("ünï.plug", "SUPER + V"),
                       ('q"uote', 'a"b'), ("x\\y", "C\tD"), ("", "")]:
        yield "_block", {"pid": pid, "combo": combo}, (lambda p=pid, c=combo: shortcuts._block(p, c))

    # -- current, with raw text
    good = ORIGINAL + _block("lxp.vitals", "SUPER + ALT + V")
    raws = [
        ("lxp.vitals", ""),
        ("lxp.vitals", ORIGINAL),
        ("lxp.vitals", good),
        ("lxp.other", good),
        ("lxp", good),
        ("lxp.vitals", good.replace("toggle", "summon")),
        ("lxp.vitals", good.replace('"SUPER + ALT + V"', '"SUPER + ALT + W"', 1)),
        ("lxp.vitals", good.replace('o.bind("SUPER + ALT + V"', 'o.bind(SUPER')),
        ("lxp.vitals", good + _block("lxp.vitals", "SUPER + B")),
        ("lxp.vitals", good.rstrip("\n")),
        ("lxp.vitals", good.lstrip("\n")),
        ("lxp.vitals", "\n" + good.lstrip("\n")),
        ("ünï.plug", ORIGINAL + _block("ünï.plug", "SUPER + Ü")),
        ("a.b", ORIGINAL + _block("a.b", "SUPER + V").replace("\n-- almanac-shortcut-end", "\nextra\n-- almanac-shortcut-end")),
        ("a.b", "\n-- almanac-shortcut-start: a.b\n-- almanac-shortcut-end: a.b\n"),
        ("a.b", "\n-- almanac-shortcut-start: aXb\no.bind(\"S\", \"Almanac: aXb\", \"omarchy-shell shell toggle aXb\")\n-- almanac-shortcut-end: aXb\n"),
        ("a+b", ORIGINAL + _block("a+b", "SUPER + V")),
        ("a(b", ORIGINAL + _block("a(b", "SUPER + V")),
    ]
    for pid, raw in raws:
        yield "current", {"pid": pid, "raw": raw}, (lambda p=pid, r=raw: shortcuts.current(p, r))

    # -- current / _read against files
    files = [
        (good, 0o644, "file"), (ORIGINAL, 0o600, "file"), (good.replace("\n", "\r\n"), 0o644, "file"),
        (good.replace("\n", "\r"), 0o640, "file"), (b"\xff" + good.encode(), 0o644, "file"),
        (good, 0o4755, "file"), (good, 0o644, "symlink"), (good, 0o644, "dir"),
        (None, 0o644, "none"), ("x" * (1024 * 1024) + "\n", 0o644, "file"),
        ("x" * (1024 * 1024 - 1) + "\n", 0o644, "file"), ("", 0o644, "file"),
        ("ünï 日本\n" + good, 0o644, "file"),
    ]
    for content, mode, kind in files:
        home = _config(content, mode, kind)
        env = {"XDG_CONFIG_HOME": home}
        yield "_read", {"env": env}, _call(env, shortcuts._read)
        yield "current", {"pid": "lxp.vitals", "raw": None, "env": env}, \
            _call(env, lambda: shortcuts.current("lxp.vitals"))
    # the real machine
    yield "_read", {}, shortcuts._read
    real_pids = ["lxp.vitals", "omarchy.clock", "nothing.here"]
    try:
        import re
        real_pids += re.findall(r"-- almanac-shortcut-start: (\S+)", shortcuts._read()[0])
    except (OSError, ValueError):
        pass
    for pid in real_pids:
        yield "current", {"pid": pid, "raw": None}, (lambda p=pid: shortcuts.current(p))

    # -- conflict
    try:
        live = json.loads(subprocess.run(["hyprctl", "-j", "binds"], capture_output=True,
                                         text=True, timeout=10).stdout)
    except Exception:                                   # noqa: BLE001
        live = []
    combos = ["super+space", "super+alt+v", "super+return", "super+q", "super+shift+q",
              "super+1", "super+alt+space", "ctrl+alt+delete", "super+ctrl+f12", "nope"]
    for combo in combos:
        yield "conflict", {"binds": live, "combo": combo, "pid": "lxp.vitals"}, \
            (lambda c=combo: shortcuts.conflict(live, c, "lxp.vitals"))
    synth = [
        BINDS,
        [{"modmask": 66, "key": "space", "description": "caps"}],
        [{"modmask": 72, "key": "V", "description": "Almanac: lxp.vitals"}],
        [{"modmask": 72, "key": "V", "description": "Almanac: other"}],
        [{"modmask": 72, "key": "V", "arg": "kitty"}],
        [{"modmask": 72, "key": "V", "dispatcher": "exec"}],
        [{"modmask": 72, "key": "V"}],
        [{"modmask": 72, "key": "V", "description": "", "arg": "", "dispatcher": ""}],
        [{"modmask": 72, "key": "V", "mouse": True, "description": "m"}],
        [{"modmask": 72, "key": "W", "keycode": 55, "description": "kc"}],
        [{"modmask": 72, "key": "code:55", "description": "code"}],
        [{"modmask": 72, "key": "", "catch_all": True, "description": "catch"}],
        [{"modmask": 72, "key": "V", "submap": "resize", "description": "sub"}],
        [{"modmask": 72, "key": "V", "keycode": 55, "description": "Almanac: lxp.vitals"}],
        [{"modmask": "72", "key": "v", "description": "str mask"}],
        [{"modmask": 72.9, "key": "v", "description": "float mask"}],
        [{"modmask": True, "key": "v", "description": "bool mask"}],
        [{"modmask": None, "key": "v"}],
        [{"modmask": "x", "key": "v"}],
        [{"modmask": "1_0", "key": "v"}],
        [{"key": "v", "description": "no mask"}],
        [{"modmask": 72, "key": None, "catch_all": 1, "description": 5}],
        [{"modmask": 72, "key": 5, "keycode": 1, "arg": ["a"]}],
        [{"modmask": 72, "key": "ü", "description": "ü"}],
        [{"modmask": 72, "key": "V", "arg": {"k": "it's", "n\nl": "日"}}],
        [{"modmask": 72, "key": "V", "description": 0, "arg": [1.5, None, True, 'q"\'']}],
        [{"modmask": 72, "key": 1.0, "keycode": 2.5}],
        [{"modmask": 0, "key": "V"}, {"modmask": 72, "key": "V", "description": "second"}],
        [{"modmask": 74, "key": "V", "description": "caps too"}],
        [],
        ["notadict"],
        [{"modmask": 64, "key": "V"}, "late"],
    ]
    for binds in synth:
        for combo in ["super+alt+v", "alt+super+v", "super+space", "bad"]:
            yield "conflict", {"binds": binds, "combo": combo, "pid": "lxp.vitals"}, \
                (lambda b=binds, c=combo: shortcuts.conflict(b, c, "lxp.vitals"))

    # -- _hyprctl on the real machine: read-only queries
    for args in [["version"], ["-j", "version"], ["notacommand-almanac"], ["-j", "monitors"]]:
        if args == ["-j", "monitors"]:
            continue    # changes from moment to moment; kept out
        yield "_hyprctl", {"args": args}, (lambda a=args: shortcuts._hyprctl(*a))
    env = {"PATH": none_path}
    yield "_hyprctl", {"args": ["version"], "env": env}, _call(env, lambda: shortcuts._hyprctl("version"))

    # -- set_shortcut with the scripted hyprctl
    b = json.dumps(BINDS)
    yield _set(ORIGINAL, "lxp.vitals", "super+alt+v", {"binds": b})
    yield _set(ORIGINAL, "lxp.vitals", "super+space", {"binds": b})
    yield _set(ORIGINAL, "lxp.vitals", "super+alt+v", {"binds": b, "after": "line 3: bad bind\nline 4"})
    yield _set(ORIGINAL, "lxp.vitals", "super+alt+v", {"binds": b, "before": "old error"})
    yield _set(ORIGINAL, "lxp.vitals", None, {"binds": b})
    yield _set(good, "lxp.vitals", None, {"binds": b})
    yield _set(good, "lxp.vitals", None, {"binds": b, "after": "broken"})
    yield _set(good, "lxp.vitals", "super+b", {"binds": b})
    yield _set(good, "lxp.vitals", "super+alt+v", {"binds": json.dumps(
        [{"modmask": 72, "key": "V", "description": "Almanac: lxp.vitals"}])})
    yield _set(good + "\n\n\n", "other.plug", "super+o", {"binds": b})
    yield _set(good.replace("toggle", "summon"), "lxp.vitals", "super+b", {"binds": b})
    yield _set(good.replace("toggle", "summon"), "other.plug", "super+b", {"binds": b})
    yield _set(ORIGINAL, "lxp.vitals", "v", {"binds": b})
    yield _set(ORIGINAL, "../etc", "super+v", {"binds": b})
    yield _set(ORIGINAL, "a..b", "super+v", {"binds": b})
    yield _set(ORIGINAL, ".hidden", "super+v", {"binds": b})
    yield _set(ORIGINAL, "", "super+v", {"binds": b})
    yield _set(ORIGINAL, "ünï", "super+v", {"binds": b})
    yield _set(ORIGINAL, "ok-id_1.x", "super+v", {"binds": b})
    yield _set(ORIGINAL, "trail\n", None, {"binds": b})
    yield _set(ORIGINAL, "trail\n\n", None, {"binds": b})
    yield _set(ORIGINAL, "lxp.vitals", "super+v", {"binds": "{}"})
    yield _set(ORIGINAL, "lxp.vitals", "super+v", {"binds": '""'})
    yield _set(ORIGINAL, "lxp.vitals", "super+v", {"binds": "[]"})
    yield _set(ORIGINAL, "lxp.vitals", "super+v", {"binds": b, "fail": "-j"}, report=True)
    yield _set(ORIGINAL, "lxp.vitals", "super+v", {"binds": b, "fail": "reload"}, report=True)
    yield _set(ORIGINAL, "lxp.vitals", "super+v", {"binds": b, "fail": "configerrors"}, report=True)
    yield _set("ünï 日本\n" + ORIGINAL, "lxp.vitals", "super+v", {"binds": b})
    yield _set(ORIGINAL.rstrip("\n"), "lxp.vitals", "super+v", {"binds": b})
    yield _set("", "lxp.vitals", "super+v", {"binds": b})
    yield _set(ORIGINAL.replace("\n", "\r\n"), "lxp.vitals", "super+v", {"binds": b})
    yield _set(ORIGINAL, "lxp.vitals", "super+v", {"binds": b}, mode=0o600)
    # Raising outcomes compared as error-or-not.
    yield _set(ORIGINAL, "lxp.vitals", "super+v", {"binds": "not json"}, report=False)
    yield _set(ORIGINAL, "lxp.vitals", "super+v", {"binds": "5"}, report=False)
    yield _set(ORIGINAL, "lxp.vitals", "super+v", {"binds": '{"a": 1}'}, report=False)
    yield _set(b"\xff" + ORIGINAL.encode(), "lxp.vitals", "super+v", {"binds": b}, report=False)
    missing = {"XDG_CONFIG_HOME": str(_TMP / "nowhere")}
    yield "set_shortcut", {"pid": "a.b", "combo": "super+v", "fake": {"binds": b}, "env": missing}, \
        _call(missing, lambda: shortcuts.set_shortcut("a.b", "super+v"))

    # -- has_panel
    manifests = [
        {"entryPoints": {"panel": "Panel.qml"}},
        {"entryPoints": {"barWidget": "W.qml"}},
        {"entryPoints": {"overlay": "O.qml"}},
        {"entryPoints": {"panel": ""}},
        {"entryPoints": {"settings": "S.qml"}},
        {"entryPoints": {}},
        {"entryPoints": None},
        {"entryPoints": []},
        {"entryPoints": {"panel": 0, "overlay": [1]}},
        {"entryPoints": {"panel": False}},
        {},
        {"name": "ünï", "entryPoints": {"panel": "日本.qml"}},
    ]
    for i, m in enumerate(manifests):
        d = _TMP / f"plugin{i}"
        d.mkdir()
        (d / "manifest.json").write_text(json.dumps(m))
        yield "has_panel", {"dir": str(d)}, (lambda d=d: shortcuts.has_panel(d))
    for i, raw in enumerate([b"", b"not json", b"\xff\xfe", b'{"entryPoints": {"panel": "x",}}',
                             b'{"entryPoints": {"panel": "x"}}  ']):
        d = _TMP / f"rawplugin{i}"
        d.mkdir()
        (d / "manifest.json").write_bytes(raw)
        yield "has_panel", {"dir": str(d)}, (lambda d=d: shortcuts.has_panel(d))
    for d in [_TMP / "nope", _TMP, Path("/nonexistent/ünï")]:
        yield "has_panel", {"dir": str(d)}, (lambda d=d: shortcuts.has_panel(d))
    # real plugin checkouts, if any
    for base in [Path.home() / ".config/omarchy/plugins", Path.home() / ".local/share/omarchy-shell/plugins"]:
        if base.is_dir():
            for d in sorted(base.iterdir())[:20]:
                yield "has_panel", {"dir": str(d)}, (lambda d=d: shortcuts.has_panel(d))

    # ---- review round 1: messages the UI shows (str(exc)) compared exactly ----
    yield from _review_round1()

    # ---- review round 2 ----
    yield from _review_round2()


def _msg(fn):
    """fn() or, when it raises, {"error": str(exc)} -- the text app.py shows."""
    def run():
        try:
            return fn()
        except Exception as exc:                        # noqa: BLE001
            return {"error": str(exc)}
    return run


_FD_MASK = [r"Is a directory: \d+", "Is a directory: <fd>"]
_TMP_MASK = [r"\.almanac-shortcut-[a-z0-9_]{8}", ".almanac-shortcut-XXXXXXXX"]


def _masked(masks, fn):
    """fn() with each [regex, text] of `masks` replaced in its strings -- the
    Rust hook applies the same `mask` to its result."""
    def sub(v):
        if isinstance(v, str):
            for pat, to in masks:
                v = re.sub(pat, lambda _m, to=to: to, v)
            return v
        if isinstance(v, dict):
            return {k: sub(x) for k, x in v.items()}
        if isinstance(v, (list, tuple)):
            return [sub(x) for x in v]
        return v
    return lambda: sub(fn())


def _script(name, body, mode=0o755):
    d = _TMP / f"sbin-{name}"
    d.mkdir(exist_ok=True)
    f = d / "hyprctl"
    f.write_bytes(body)
    f.chmod(mode)
    return str(d)


def _review_round1():
    # _read: messages for a directory, bad UTF-8, a quote in the path
    for content, kind in [(ORIGINAL, "dir"), (b"\xff" + ORIGINAL.encode(), "file"),
                          (b"ok\n\xc3(", "file"), (ORIGINAL, "symlink")]:
        env = {"XDG_CONFIG_HOME": _config(content, kind=kind)}
        # os.fdopen on a directory names the descriptor, whose number
        # depends on what else each process has open: masked on both sides.
        yield "_read", {"env": env, "mask": [_FD_MASK]}, \
            _call(env, _masked([_FD_MASK], _msg(shortcuts._read)))
    q = _TMP / "it's"
    q.mkdir()
    env = {"XDG_CONFIG_HOME": str(q)}
    yield "_read", {"env": env}, _call(env, _msg(shortcuts._read))
    env = {"XDG_CONFIG_HOME": str(_TMP / "missing")}
    yield "_read", {"env": env}, _call(env, _msg(shortcuts._read))

    # conflict: Unicode digits and huge ints in modmask; repr of non-printables
    for binds in [[{"modmask": "٧٢", "key": "V", "description": "arabic digits"}],
                  [{"modmask": "99999999999999999999", "key": "V"}],
                  [{"modmask": 72, "key": "V", "arg": ["a b"]}],
                  [{"modmask": 72, "key": "V", "arg": ["z​w", "\x85"]}],
                  [{"modmask": 72, "key": "V", "arg": {"b": 1, "a": 2}}]]:
        yield "conflict", {"binds": binds, "combo": "super+alt+v", "pid": "p"}, \
            _msg(lambda b=binds: shortcuts.conflict(b, "super+alt+v", "p"))

    # set_shortcut: messages of json errors / non-standard JSON hyprctl could print
    for text in ["not json", "", '[{"modmask": 72, "key": "V", "x": NaN, "description": "d"}]',
                 '[{"modmask": 72, "key": "V", "description": "Infinity", "n": -Infinity}]']:
        home = _config(ORIGINAL)
        args = {"pid": "lxp.vitals", "combo": "super+alt+v", "fake": {"binds": text},
                "env": {"XDG_CONFIG_HOME": _config(ORIGINAL)}}

        def run(text=text, home=home):
            hypr, _calls = _fake({"binds": text})
            with mock.patch.object(shortcuts, "_hyprctl", side_effect=hypr):
                return shortcuts.set_shortcut("lxp.vitals", "super+alt+v")
        yield "set_shortcut", args, _call({"XDG_CONFIG_HOME": home}, _msg(run))
    # set_shortcut: bad UTF-8 file message
    home = _config(b"\xff" + ORIGINAL.encode())
    args = {"pid": "a.b", "combo": "super+v", "fake": {"binds": "[]"},
            "env": {"XDG_CONFIG_HOME": _config(b"\xff" + ORIGINAL.encode())}}
    yield "set_shortcut", args, _call({"XDG_CONFIG_HOME": home}, _msg(
        lambda: shortcuts.set_shortcut("a.b", "super+v")))
    # set_shortcut: hypr dir not writable -> mkstemp fails (message shape).
    # The failure writes nothing, so both sides share one home; mkstemp's
    # random name is masked.
    home = _config(ORIGINAL)
    os.chmod(Path(home) / "hypr", 0o555)
    atexit.register(lambda: os.chmod(Path(home) / "hypr", 0o755))

    def ro():
        hypr, _calls = _fake({"binds": "[]"})
        with mock.patch.object(shortcuts, "_hyprctl", side_effect=hypr):
            return shortcuts.set_shortcut("a.b", "super+v")
    yield "set_shortcut", {"pid": "a.b", "combo": "super+v", "fake": {"binds": "[]"},
                           "env": {"XDG_CONFIG_HOME": home}, "mask": [_TMP_MASK]}, \
        _call({"XDG_CONFIG_HOME": home}, _masked([_TMP_MASK], _msg(ro)))

    # has_panel: Python json accepts NaN; a non-object manifest raises
    for i, raw in enumerate([b'{"entryPoints": {"panel": "x"}, "v": NaN}',
                             b'{"entryPoints": {"panel": "x"}, "v": Infinity}',
                             b'[1]', b'{"entryPoints": "panel"}', b'{"entryPoints": [1]}']):
        d = _TMP / f"r1plugin{i}"
        d.mkdir()
        (d / "manifest.json").write_bytes(raw)
        yield "has_panel", {"dir": str(d)}, (lambda d=d: shortcuts.has_panel(d))

    # _hyprctl through fake binaries: decoding, newlines, exec errors, timeout
    fakes = [
        ("crlf", b"#!/bin/sh\nprintf 'a\\r\\nb\\rc\\n'\n", 0o755),
        ("badutf", b"#!/bin/sh\nprintf 'ok\\377'\n", 0o755),
        ("errutf", b"#!/bin/sh\nprintf 'bad\\377' >&2\nexit 1\n", 0o755),
        ("errcrlf", b"#!/bin/sh\nprintf 'e1\\r\\ne2' >&2\nexit 3\n", 0o755),
        ("noexec", b"#!/bin/sh\necho hi\n", 0o644),
        ("noshebang", b"echo hi\n", 0o755),
        ("sig", b"#!/bin/sh\nkill -9 $$\n", 0o755),
    ]
    for name, body, mode in fakes:
        env = {"PATH": _script(name, body, mode)}
        yield "_hyprctl", {"args": ["version"], "env": env}, \
            _call(env, _msg(lambda: shortcuts._hyprctl("version")))
    env = {"PATH": None}
    yield "_hyprctl", {"args": ["version"], "env": env}, \
        _call(env, _msg(lambda: shortcuts._hyprctl("version")))
    yield "available", {"env": {"PATH": None, "XDG_CONFIG_HOME": _config(ORIGINAL)}}, \
        _call({"PATH": None, "XDG_CONFIG_HOME": _config(ORIGINAL)}, shortcuts.available)
    if os.environ.get("SHORTCUTS_SLOW"):
        env = {"PATH": _script("slow", b"#!/bin/sh\nexec /usr/bin/sleep 12\n")}
        yield "_hyprctl", {"args": ["version"], "env": env}, \
            _call(env, _msg(lambda: shortcuts._hyprctl("version")))


def _set_msg(content, pid, combo, binds_text, mode=0o644):
    """set_shortcut with the scripted hyprctl; str(exc) on any exception.
    Returns (case, py_home, rs_home) so a follow-up case can inspect the file."""
    py_home, rs_home = _config(content, mode), _config(content, mode)

    def run():
        hypr, calls = _fake({"binds": binds_text})
        with mock.patch.object(shortcuts, "_hyprctl", side_effect=hypr):
            try:
                result, error = shortcuts.set_shortcut(pid, combo), None
            except Exception as e:                      # noqa: BLE001
                result, error = None, str(e)
        try:
            file = shortcuts.bindings_path().read_text()
        except OSError:
            file = None
        return {"result": result, "error": error, "file": file, "calls": calls}
    args = {"pid": pid, "combo": combo, "fake": {"binds": binds_text}, "report": True,
            "env": {"XDG_CONFIG_HOME": rs_home}}
    return ("set_shortcut", args, _call({"XDG_CONFIG_HOME": py_home}, run)), py_home, rs_home


def _review_round2():
    # available(): shutil.which uses os.access(X_OK) and "not a directory";
    # a hyprctl whose only x bit is group/other is not executable for its owner,
    # and a FIFO with x bits counts.
    for name, mode in [("gx", 0o010), ("ox", 0o001), ("gox", 0o611), ("ux", 0o100)]:
        d = _TMP / f"r2bin-{name}"
        d.mkdir()
        f = d / "hyprctl"
        f.write_text("#!/bin/sh\nexit 0\n")
        f.chmod(mode)
        env = {"XDG_CONFIG_HOME": _config(ORIGINAL), "PATH": str(d)}
        yield "available", {"env": env}, _call(env, shortcuts.available)
    d = _TMP / "r2bin-fifo"
    d.mkdir()
    os.mkfifo(d / "hyprctl")
    os.chmod(d / "hyprctl", 0o755)
    env = {"XDG_CONFIG_HOME": _config(ORIGINAL), "PATH": str(d)}
    yield "available", {"env": env}, _call(env, shortcuts.available)
    # unreadable bindings.lua
    env = {"XDG_CONFIG_HOME": _config(ORIGINAL, mode=0o000), "PATH": _bin("hyprctl")}
    yield "available", {"env": env}, _call(env, shortcuts.available)

    # set_shortcut keeps the file's permission bits (compared via _read after)
    for mode in (0o600, 0o640, 0o4755, 0o1644):
        case, py_home, rs_home = _set_msg(ORIGINAL, "lxp.vitals", "super+v", "[]", mode=mode)
        yield case
        yield "_read", {"env": {"XDG_CONFIG_HOME": rs_home}}, \
            _call({"XDG_CONFIG_HOME": py_home}, _msg(shortcuts._read))

    # a lone surrogate escape in hyprctl's JSON: Python keeps it in the str.
    # Inside a repr it is escaped ('\udc00') and compared exactly. As str()
    # it lands raw in the message, which a Rust String cannot hold: the port
    # gives U+FFFD there, so for those cases the Python message's surrogates
    # are mapped to U+FFFD before comparing (everything else exact).
    for text in [r'[{"modmask": 72, "key": "V", "description": "x\ud800y"}]',
                 r'[{"modmask": 72, "key": "V", "arg": {"k": "\udc00"}}]',
                 r'[{"modmask": 72, "key": "V", "arg": ["\udbff", {"\ud800": 1}]}]',
                 r'[{"modmask": "7\ud8002", "key": "V", "description": "m"}]',
                 r'[{"modmask": 72, "key": "V", "description": "Almanac: lxp.vitals\ud800"}]']:
        func, args, fn = _set_msg(ORIGINAL, "lxp.vitals", "super+alt+v", text)[0]
        yield func, args, _masked([[r"[\ud800-\udfff]", "\ufffd"]], fn)

    # float / int text in the conflict message, parsed by json.loads (no serde)
    for num in ["1e16", "1e-5", "0.1", "1e22", "5e-324", "123456789012345678.0",
                "1.7976931348623157e308", "1E400", "-1e-400", "12345678901234567890123",
                "-170141183460469231731687303715884105729", "2.5e-7", "100.0"]:
        text = '[{"modmask": 72, "key": "V", "description": %s}]' % num
        yield _set_msg(ORIGINAL, "lxp.vitals", "super+alt+v", text)[0]

    # Ints beyond i64/u64 cannot reach conflict() through its serde-valued
    # signature intact (serde_json has no arbitrary_precision: the parity
    # request is already f64 when the hook sees it). set_shortcut parses
    # hyprctl's text itself and keeps them exact; that is checked here.
    for num in ["100000000000000000000", "-9223372036854775809", "18446744073709551616",
                "1000000000000000000000000000000"]:
        text = '[{"modmask": 72, "key": "V", "description": %s}]' % num
        yield _set_msg(ORIGINAL, "lxp.vitals", "super+alt+v", text)[0]
    for mask in [2**64 + 72, -(2**63) - 2]:
        binds = [{"modmask": mask, "key": "V", "description": "big mask"}]
        yield "conflict", {"binds": binds, "combo": "super+alt+v", "pid": "p"}, \
            _msg(lambda b=binds: shortcuts.conflict(b, "super+alt+v", "p"))

    # deep JSON nesting: Python's json decodes ~50k levels fine
    deep = "[" * 3000 + "]" * 3000
    text = '[{"modmask": 0, "key": "X", "deep": %s}]' % deep
    yield _set_msg(ORIGINAL, "lxp.vitals", "super+alt+v", text)[0]
    d = _TMP / "r2deep3000"
    d.mkdir()
    (d / "manifest.json").write_text('{"entryPoints": {"panel": "x"}, "deep": %s}' % deep)
    yield "has_panel", {"dir": str(d)}, (lambda d=d: shortcuts.has_panel(d))
    # (These used to abort the Rust parity process: stack overflow, regex
    # size limit.) Depths stay well clear of where Python 3.14 gives up
    # (~52k levels decoding, ~37k lists / ~31k dicts in repr), which moves
    # with the stack already in use.
    for depth, kind in [(20000, "arr"), (20000, "obj"), (100000, "arr"), (100000, "obj")]:
        deep = ("[" * depth + "]" * depth if kind == "arr"
                else '{"a":' * depth + "1" + "}" * depth)
        d = _TMP / f"r2deep{depth}{kind}"
        d.mkdir()
        (d / "manifest.json").write_text('{"entryPoints": {"panel": "x"}, "deep": %s}' % deep)
        # Beyond the limit Python raises RecursionError (not caught).
        yield "has_panel", {"dir": str(d)}, (lambda d=d: shortcuts.has_panel(d))
    big = "a" * 300000
    yield "current", {"pid": big, "raw": "x"}, (lambda: shortcuts.current(big, "x"))
    raw = _block(big, "SUPER + V") + "tail"
    yield "current", {"pid": big, "raw": "\n" + raw}, (lambda: shortcuts.current(big, "\n" + raw))
    # repr of a deep value in the conflict message: fine at 20000 levels,
    # RecursionError at 45000 (raised: only error-ness is compared).
    for depth, kind in [(20000, "arr"), (20000, "obj"), (45000, "arr"), (45000, "obj"),
                        (100000, "arr")]:
        deep = ("[" * depth + "]" * depth if kind == "arr"
                else '{"a":' * depth + "1" + "}" * depth)
        text = '[{"modmask": 72, "key": "V", "description": %s}]' % deep
        home = _config(ORIGINAL)

        def run(text=text):
            hypr, _calls = _fake({"binds": text})
            with mock.patch.object(shortcuts, "_hyprctl", side_effect=hypr):
                return shortcuts.set_shortcut("lxp.vitals", "super+alt+v")
        yield "set_shortcut", {"pid": "lxp.vitals", "combo": "super+alt+v", "fake": {"binds": text},
                               "env": {"XDG_CONFIG_HOME": home}}, \
            _call({"XDG_CONFIG_HOME": home}, _msg(run) if depth == 20000 else run)

    # timeout after invalid UTF-8 was written: Python raises TimeoutExpired
    # without decoding what was collected
    if os.environ.get("SHORTCUTS_SLOW"):
        env = {"PATH": _script("slowbad", b"#!/bin/sh\nprintf 'x\\377'\nexec /usr/bin/sleep 12\n")}
        yield "_hyprctl", {"args": ["version"], "env": env}, \
            _call(env, _msg(lambda: shortcuts._hyprctl("version")))
        env = {"PATH": _script("slowbg", b"#!/bin/sh\n(/usr/bin/sleep 12; printf '\\377') &\nexec /usr/bin/sleep 30\n")}
        yield "_hyprctl", {"args": ["version"], "env": env}, \
            _call(env, _msg(lambda: shortcuts._hyprctl("version")))

    # json.loads error texts and odd-but-valid JSON from hyprctl
    for text in ['[1,]', '{"a":1,}', '[1 2]', '{"a" 1}', '{1:2}', '"\\x"', '"\\u12"', '"abc',
                 '[', '{', ' ', '[1]x', '﻿[]', '"\t"', '{"a":}', '[-]', '[01]', '[1.]',
                 '[1e]', '[.5]', '[-Infinity]', '[NaNx]', 'nul', '[true,false,null]',
                 '{"a":1 , "b" : 2 }', '[\n1,\n]', '{"a":1\n,\n}', '{ }', '[ ]',
                 '"\\ud800\\u0041"', '"\\ud800\\udc00"', '{"":1}', '[1,,2]', '{,}',
                 '{"a":1,,}', '[{"modmask":72,"key":"V"} , ]', '\n\n  [1,\n  2', '[1]\n\n x',
                 '[{"modmask":72,"key":"V","description":"a","description":"b"}]']:
        yield _set_msg(ORIGINAL, "lxp.vitals", "super+alt+v", text)[0]

    # int() of string modmasks: Unicode spaces and digits
    for mask in ["\x1c72", " 72", "72\x85", "７２", "\U0001d7d5\U0001d7d0",
                 "72.0", "+72", "-0", "0072", "7 2", "٧٢", "", " ", "7_2"]:
        binds = [{"modmask": mask, "key": "V", "description": "m"}]
        yield "conflict", {"binds": binds, "combo": "super+alt+v", "pid": "p"}, \
            _msg(lambda b=binds: shortcuts.conflict(b, "super+alt+v", "p"))
