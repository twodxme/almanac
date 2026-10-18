"""Parity cases for settings.py.

Every case that touches a file gets its own temp XDG_STATE_HOME; the Rust
side rewrites the same starting file from the args before it runs, so the
Python run (which goes first) cannot leak into it. The real machine is read
only through `get`/`_load` with no override -- never written.
"""
import json
import os
import tempfile
from pathlib import Path

import settings

_ROOT = Path(tempfile.mkdtemp(prefix="almanac-parity-settings-"))
_n = 0


def _home():
    global _n
    _n += 1
    d = _ROOT / str(_n)
    d.mkdir()
    return str(d)


def _with(state_home, file, fn):
    """Run fn with XDG_STATE_HOME=state_home and the settings file set up."""
    def run():
        old = os.environ.get("XDG_STATE_HOME")
        os.environ["XDG_STATE_HOME"] = state_home
        try:
            p = settings.path()
            p.parent.mkdir(parents=True, exist_ok=True)
            if isinstance(file, bytes):
                p.write_bytes(file)
            elif isinstance(file, str):
                p.write_text(file)
            elif p.exists():
                p.unlink()
            return fn()
        finally:
            if old is None:
                os.environ.pop("XDG_STATE_HOME", None)
            else:
                os.environ["XDG_STATE_HOME"] = old
    return run


def _args(state_home, file, **kw):
    a = {"state_home": state_home, **kw}
    if isinstance(file, bytes):
        a["file_bytes"] = list(file)
    else:
        a["file"] = file
    return a


def _get_bool(key):
    try:
        return bool(settings.get(key))
    except KeyError:
        return False


def _get_str(key):
    try:
        v = settings.get(key)
    except KeyError:
        return ""
    return v if isinstance(v, str) else ""


def _set(key, value):
    settings.set(key, value)
    return settings.path().read_text()


FILES = [
    None,                                              # missing
    "",                                                # empty
    "{}",
    '{"net_vendor_icons": "nope"',                     # broken
    '{"net_vendor_icons": "nope"}',                    # wrong type
    '{"net_vendor_icons": false, "start_page": "plugins"}',
    '{"start_page": 3, "hold_majors": 0, "plugin_scope": null}',   # wrong types
    '{"hold_majors": 1, "notify_resets": "true"}',     # int is not a bool
    '[1, 2, 3]',                                       # not an object
    '"just a string"',
    'null',
    '{"start_page": "élan ☃ 日本", "extra": {"nested": [1, 2.5, true]}}',
    '{"start_page": "a", "start_page": "b"}',          # duplicate key: last wins
    '  \n{"hold_majors": false}\n\n',                  # whitespace around
    '{"plugin_scope": "\\u00e9\\ud83d\\ude00"}',       # escapes
    "﻿{\"hold_majors\": false}",                  # BOM
    b'{"start_page": "\xff\xfe"}',                     # not UTF-8
    '{"hold_majors": false} trailing',
]

KEYS = [k for k in settings.DEFAULTS] + ["no_such_setting", "", "START_PAGE", "ünï"]


def cases():
    yield "DEFAULTS", {}, lambda: settings.DEFAULTS
    yield "START_PAGES", {}, lambda: settings.START_PAGES
    h = _home()
    yield "path", {"state_home": h}, _with(h, None, settings.path)
    # The real machine, read only.
    yield "path", {}, lambda: settings.path()
    yield "_load", {}, lambda: settings._load()
    for k in KEYS:
        yield "get", {"key": k}, (lambda k=k: settings.get(k))

    for f in FILES:
        h = _home()
        yield "_load", _args(h, f), _with(h, f, settings._load)
        for k in KEYS:
            h = _home()
            yield "get", _args(h, f, key=k), _with(h, f, lambda k=k: settings.get(k))
            h = _home()
            yield "get_bool", _args(h, f, key=k), _with(h, f, lambda k=k: _get_bool(k))
            h = _home()
            yield "get_str", _args(h, f, key=k), _with(h, f, lambda k=k: _get_str(k))

    # Python's json takes NaN/Infinity; only the readers are compared (the
    # values themselves are not JSON).
    for f in ('{"hold_majors": NaN, "start_page": "x", "s": "NaN"}',
              '{"plugin_scope": -Infinity, "net_gh_token": false, "x": [Infinity]}'):
        for k in KEYS:
            h = _home()
            yield "get", _args(h, f, key=k), _with(h, f, lambda k=k: settings.get(k))
            h = _home()
            yield "get_bool", _args(h, f, key=k), _with(h, f, lambda k=k: _get_bool(k))

    values = [False, True, "featured", "", "日本 ☃", 0, 1.5, None, [1, "a"],
              {"b": 2, "a": [None, False]}, "quote\"back\\slash\nnl\t\x01"]
    for f in FILES:
        for k in ("hold_majors", "start_page", "no_such_setting"):
            for v in values:
                h = _home()
                yield "set", _args(h, f, key=k, value=v), _with(h, f, lambda k=k, v=v: _set(k, v))
    # A state home that is a file: mkdir fails (OSError).
    h = _home()
    blocker = Path(h) / "blocked"
    blocker.write_text("x")
    yield "set", {"state_home": str(blocker), "key": "hold_majors", "value": False}, \
        lambda: _with_raw(str(blocker), lambda: settings.set("hold_majors", False))
    yield from _round1()
    yield from _round2b()
    yield from _round2c()


# Review round 1: what Python's json reads that serde_json does not, or
# reads differently, and so writes back differently.
_DEEP = '{"hold_majors": false, "deep": ' + "[" * 200 + "]" * 200 + "}"
_FLOATS = ('{"hold_majors": false, "f": [0.10000000000000001942, 2.2250738585072011e-308, '
           '9007199254740993.0, 0.30000000000000004441, 1.0000000000000002220446, '
           '123456789.12345678901234567, 4.9406564584124654e-324, 1.7976931348623157e308, '
           '7.2057594037927933e16, 0.1e1, 1E5, 1e-7, 5e-324, 2.4703282292062328e-324]}')
ROUND1_FILES = [
    '{"hold_majors": false, "big": 123456789012345678901234567890}',   # int beyond u64
    '{"hold_majors": false, "neg": -123456789012345678901234567890}',
    '{"hold_majors": false, "huge": ' + "9" * 400 + '}',               # int beyond f64
    '{"hold_majors": false, "inf": 1e400}',                             # float overflow: inf
    '{"hold_majors": false, "z": -0}',                                  # negative zero int
    '{"hold_majors": false, "s": "\\ud800"}',                        # lone surrogate
    '{"hold_majors": false, "s": "\\udc00\\ud800"}',
    _DEEP,                                                              # nesting > 128
    '{"hold_majors": false, "n": NaN, "i": Infinity, "m": -Infinity}',  # written back by set
    _FLOATS,
]


# Review round 2: more of the same, plus the limits of CPython's own reader.
ROUND2_FILES = [
    '{"hold_majors": false, "n": ' + "1" * 4300 + '}',                  # int(): at the digit limit
    '{"hold_majors": false, "n": -' + "1" * 4300 + '}',
    '{"hold_majors": false, "n": ' + "1" * 4301 + '}',                  # past it: ValueError -> {}
    '{"hold_majors": false, "f": [-0.0, -1e400, 1E+2, 0e0, -0, 0.5e-400]}',
    '{"hold_majors": false, "\\ud83d\\ude00": 1, "\\uffff": 2, "\\ud800": 3, "a": 4}',  # sort by code point
    '{"hold_majors": false, "s": "\\ud800\\u0041\\uDBFF\\uDFFF\\/\x7f"}',
    '{"hold_majors": false, "s": "\\ud800\\uZZZZ"}',               # bad escape after a surrogate
    '{"hold_majors": false, "x": [1, {"y": NaN}, ], "z": 1}',          # trailing comma
    '{"hold_majors": false, "x": 01}',
    '{"hold_majors": false, "x": 1.}',
    '{"hold_majors": false, "x": 1e}',
    '{"hold_majors": false, "x": -}',
    '{"hold_majors": false, "x": -Inf}',
    '{"hold_majors": true, "hold_majors": false, "a": {"k": 1, "k": [2]}}',
    '{"hold_majors": false, "deep": ' + "[" * 1000 + "]" * 1000 + "}",
    '{"hold_majors": false, "deep": ' + '{"a": ' * 1000 + "1" + "}" * 1000 + "}",
    '\t\r\n {"hold_majors": false}\r\n',
]
# Nesting past CPython's limit (52105 on an 8 MiB stack): RecursionError,
# which _load does not catch, so get and set raise too.
_TOO_DEEP = '{"hold_majors": false, "deep": ' + "[" * 60000 + "]" * 60000 + "}"


def _load_json():
    """_load() exactly: a serde value cannot hold every Python value."""
    return json.dumps(settings._load())


def _round1():
    for f in ROUND1_FILES + ROUND2_FILES + [_TOO_DEEP]:
        h = _home()
        yield "_load_json", _args(h, f), _with(h, f, _load_json)
        h = _home()
        yield "get", _args(h, f, key="hold_majors"), _with(h, f, lambda: settings.get("hold_majors"))
        h = _home()
        yield "set", _args(h, f, key="start_page", value="plugins"), \
            _with(h, f, lambda: _set("start_page", "plugins"))
    # XDG_STATE_HOME that the spec says to ignore (relative / empty): path only.
    for sh in ("relative/state", "", "~/state"):
        yield "path", {"state_home": sh}, (lambda sh=sh: _with_raw(sh, settings.path))


def _with_raw(state_home, fn):
    old = os.environ.get("XDG_STATE_HOME")
    os.environ["XDG_STATE_HOME"] = state_home
    try:
        return fn()
    finally:
        if old is None:
            os.environ.pop("XDG_STATE_HOME", None)
        else:
            os.environ["XDG_STATE_HOME"] = old


# Review round 2 (second reviewer).
def _round2b():
    # The RecursionError threshold is a C-stack byte budget in CPython 3.14,
    # not a fixed count: on the main thread it lands anywhere from ~52090 to
    # past 52106 from one run to the next (measured), so only a depth safely
    # under it is pinned here; MAX_DEPTH's exact boundary cannot be.
    for total in (52080,):
        n = total - 1
        f = '{"hold_majors": false, "deep": ' + "[" * n + "]" * n + "}"
        h = _home()
        yield "get", _args(h, f, key="hold_majors"), _with(h, f, lambda: settings.get("hold_majors"))
    # Called one Python frame deeper per level, as the app does from its
    # callbacks: the same file decodes or not depending on the caller.
    def nested(depth, fn):
        return fn() if depth == 0 else nested(depth - 1, fn)
    n = 52079
    f = '{"hold_majors": false, "deep": ' + "[" * n + "]" * n + "}"
    for frames in (0, 50, 200):
        h = _home()
        yield "get", _args(h, f, key="hold_majors"), \
            _with(h, f, lambda frames=frames: nested(frames, lambda: settings.get("hold_majors")))
    # A string setting holding a lone surrogate: Python returns it as is.
    # The runner's JSON cannot carry a lone surrogate from Rust (serde
    # strings are UTF-8), so both sides are compared as json.dumps text, the
    # surrogate escaped; Rust's get_exact keeps it, get/get_str cannot.
    for f in ('{"start_page": "\\ud800"}', '{"plugin_scope": "a\\udfffb"}'):
        for k in ("start_page", "plugin_scope"):
            h = _home()
            yield "get_json", _args(h, f, key=k), _with(h, f, lambda k=k: json.dumps(settings.get(k)))
            h = _home()
            yield "get_str_json", _args(h, f, key=k), _with(h, f, lambda k=k: json.dumps(_get_str(k)))
        # And written back unchanged by set, as plugins.scope does.
        h = _home()
        yield "set", _args(h, f, key="hold_majors", value=False), \
            _with(h, f, lambda: _set("hold_majors", False))
    # Values a caller may store: ints past u64/i64, floats in every repr form.
    for v in (2**64, -2**63 - 1, 2**63, 10**30, -0.0, 1e16, 1e22, 0.1 + 0.2, 1e-5, 1e-4,
              5e-324, 1.7976931348623157e308, 123456789.0):
        h = _home()
        yield "set", _args(h, None, key="start_page", value=v, value_json=json.dumps(v)), \
            _with(h, None, lambda v=v: _set("start_page", v))
    # settings.json is a directory: read is OSError -> {}, replace fails.
    h = _home()
    d = Path(h) / "almanac" / "settings.json"
    d.mkdir(parents=True)
    yield "get", {"state_home": h, "key": "hold_majors"}, \
        lambda h=h: _with_raw(h, lambda: settings.get("hold_majors"))
    yield "set", {"state_home": h, "key": "hold_majors", "value": False}, \
        lambda h=h: _with_raw(h, lambda: settings.set("hold_majors", False))
    # settings.tmp is a directory: write_text fails.
    h = _home()
    t = Path(h) / "almanac" / "settings.tmp"
    t.mkdir(parents=True)
    yield "set", {"state_home": h, "key": "hold_majors", "value": False}, \
        lambda h=h: _with_raw(h, lambda: settings.set("hold_majors", False))
    # CRLF / lone CR line endings (read_text uses universal newlines).
    for f in ('{\r\n"hold_majors": false,\r"start_page": "plugins"\r\n}\r',
              '{"start_page": "a\rb"}', '{"start_page": "a\r\nb"}'):
        for k in ("hold_majors", "start_page"):
            h = _home()
            yield "get", _args(h, f, key=k), _with(h, f, lambda k=k: settings.get(k))
        h = _home()
        yield "set", _args(h, f, key="plugin_scope", value="x"), \
            _with(h, f, lambda: _set("plugin_scope", "x"))


def _in_thread(fn):
    """Run fn on a worker thread, as the app's background loaders call get."""
    import threading
    out = {}

    def run():
        try:
            out["v"] = fn()
        except BaseException as e:            # noqa: BLE001
            out["e"] = e
    t = threading.Thread(target=run)
    t.start()
    t.join()
    if "e" in out:
        raise out["e"]
    return out["v"]


def _thread_limit():
    """The depth CPython's json reaches on a worker thread, whose stack is
    RLIMIT_STACK (2 MiB when unlimited): 160 bytes a level, 43920 before
    the first (measured; 52154 under 8 MiB). As guard.rs computes it."""
    import resource
    soft = resource.getrlimit(resource.RLIMIT_STACK)[0]
    size = 2 << 20 if soft == resource.RLIM_INFINITY else (max(soft, 16384) + 4095) // 4096 * 4096
    return (size - 43920) // 160


def _round2c():
    # On a worker thread CPython's limit is 52154 containers, every run
    # (measured), where the main thread's moves around 52098-52131; Rust
    # makes the call on a worker thread too (`thread: true`).
    for total in (52120, 52154, 52155, 60000):
        n = total - 1
        for kind in ("[]", ('{"a": ', "}")):
            if kind == "[]":
                f = '{"hold_majors": false, "deep": ' + "[" * n + "]" * n + "}"
            else:
                f = '{"hold_majors": false, "deep": ' + kind[0] * n + "1" + kind[1] * n + "}"
            h = _home()
            yield "get", _args(h, f, key="hold_majors", thread=True), \
                _with(h, f, lambda: _in_thread(lambda: settings.get("hold_majors")))
            if total > _thread_limit():
                # (Only past the limit: under it, indent=2 writes GBs. The
                # limit follows `ulimit -s`; 52154 is the 8 MiB one.)
                h = _home()
                yield "set", _args(h, f, key="start_page", value="x", thread=True), \
                    _with(h, f, lambda: _in_thread(lambda: _set("start_page", "x")))
