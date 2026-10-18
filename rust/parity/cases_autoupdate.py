"""Parity cases for autoupdate.py.

Every case that touches a file gets its own temp config home; the module's
paths (CONFIG_DIR, CONFIG, TIMER_FILE) are pointed into it for the call, and
the Rust side derives the same paths from `config_home` and rewrites the
same starting files before it runs. systemctl is never run with a verb that
changes anything: set_enabled / set_schedule (and fake variants of
is_enabled / status) go through a stand-in that records its calls. The real
machine is read only: the path constants, load(), is_enabled(), status(),
current_schedule() and read-only systemctl verbs.
"""
import json
import os
import stat
import tempfile
from pathlib import Path

import autoupdate

_ROOT = Path(tempfile.mkdtemp(prefix="almanac-parity-autoupdate-"))
_n = 0

TIMER = (
    "[Unit]\nDescription=Run the weekly update check\n\n[Timer]\n"
    "OnCalendar=Sun 11:00\n# A laptop is often asleep at the scheduled time; run on next wake instead of\n"
    "# skipping the week entirely.\nPersistent=true\nRandomizedDelaySec=30m\n\n"
    "[Install]\nWantedBy=timers.target\n"
)


def _home():
    global _n
    _n += 1
    d = _ROOT / str(_n)
    d.mkdir()
    return str(d)


def _put(p, v):
    p.parent.mkdir(parents=True, exist_ok=True)
    if isinstance(v, bytes):
        p.write_bytes(v)
    elif isinstance(v, str):
        p.write_text(v)
    elif p.exists():
        p.unlink()


def _args(h, config="__skip__", timer="__skip__", **kw):
    a = {"config_home": h, **kw}
    for name, v in (("config", config), ("timer", timer)):
        if v == "__skip__":
            continue
        a[name] = list(v) if isinstance(v, bytes) else v
    return a


def _with(h, fn, config="__skip__", timer="__skip__", fake=None, timer_readonly=False, blocker=False):
    def run():
        saved = (autoupdate.CONFIG_DIR, autoupdate.CONFIG, autoupdate.TIMER_FILE, autoupdate._systemctl)
        autoupdate.CONFIG_DIR = Path(h) / "almanac"
        autoupdate.CONFIG = autoupdate.CONFIG_DIR / "auto-update.json"
        autoupdate.TIMER_FILE = Path(h) / "systemd/user" / autoupdate.UNIT
        calls = []
        if fake is not None:
            def stand_in(*args, timeout=30):
                calls.append([list(args), timeout])
                code, out = fake.get(args[0], [0, ""]) if args else [0, ""]
                return code, out
            autoupdate._systemctl = stand_in
        try:
            if config != "__skip__":
                _put(autoupdate.CONFIG, config)
            if timer != "__skip__":
                _put(autoupdate.TIMER_FILE, timer)
            if timer_readonly:
                os.chmod(autoupdate.TIMER_FILE, stat.S_IRUSR | stat.S_IRGRP | stat.S_IROTH)
            if blocker:
                autoupdate.CONFIG_DIR.write_text("x")
            return fn(calls)
        finally:
            (autoupdate.CONFIG_DIR, autoupdate.CONFIG, autoupdate.TIMER_FILE, autoupdate._systemctl) = saved
    return run


CONFIGS = [
    None, "", "{}", "not json", '{"plugins": true',
    '{"plugins": true, "schedule": "daily"}',
    '{"packages": "yes", "agents": 0, "notify": null, "extra": [1, 2.5, {"x": "é"}]}',
    '{"zzz": 1, "aaa": 2, "schedule": "monthly"}',                 # unknown keys keep file order
    '{"schedule": "a", "schedule": "b"}',                          # duplicate: last wins
    '  \n{"notify": false}\n\n',
    '{"schedule": "\\u00e9\\ud83d\\ude00 日本 ☃"}',
    "﻿{\"notify\": false}",                                   # BOM
    b'{"schedule": "\xff\xfe"}',                                   # not UTF-8
    '{"notify": false} trailing',
    "[]", '[["plugins", true], ["schedule", "daily"]]',
    '[["plugins", true], "abc"]',                                  # ValueError after one pair: partial
    '[["plugins", true], 5]',                                      # TypeError: raises
    '["ab", "cd"]',                                                # two-char strings are pairs
    '[{"k": 1, "v": 2}]',                                          # dict item iterates its keys
    '[[1, "one"], [null, 2], [true, 3], [1.5, 4]]',                # non-string keys
    '[[0, "zero"], [false, "f"], [2.0, "two"], [2, "deux"]]',      # 0 == False, 2.0 == 2
    '[[["x"], 1]]',                                                # unhashable key
    '[["a", 1, 2]]', '[[]]',
    '"str"', '""', '"ab"', "null", "5", "2.5", "true", "false",
    '{"n": 1.0, "big": 12345678901234567890, "neg": -0.0, "e": 1e300}',
]

CFGS = [
    {},
    dict(autoupdate.DEFAULTS),
    {**autoupdate.DEFAULTS, "schedule": "daily", "plugins": True},
    {"schedule": "élan ☃ 日本", "x": [1, 2.5, None, {"y": False}], "q": "quote\"back\\slash\nnl\t\x01"},
    {"zzz": 1, "aaa": {"b": 2, "a": 1}},
    {"f": 1.0, "g": 0.1, "h": 1e20, "i": -3},
]

FAKE_SHOW = [
    "NextElapseUSecRealtime=Sun 2026-10-18 11:12:13 PDT\nLastTriggerUSec=Sun 2026-10-11 11:20:01 PDT\nUnit=omarchy-auto-update.service",
    "NextElapseUSecRealtime=\nLastTriggerUSec=\nUnit=x",
    "",
    "garbage without equals\nNextElapseUSecRealtime=a=b=c",
    "LastTriggerUSec=n/a\r\nNextElapseUSecRealtime=x Unit=y",
    "NextElapseUSecRealtime=first\nNextElapseUSecRealtime=second",
    "Failed to connect to bus: No medium found",
]

FAKE_ENABLED = [[0, "enabled"], [0, "enabled-runtime"], [1, "disabled"], [0, "static"],
                [1, "enabled"], [0, ""], [0, "Enabled"], [4, "Failed to get unit file state"]]

TIMERS = [
    TIMER, None, "", "OnCalendar=*-*-* 11:00:00\n", "OnCalendar=  Sun *-*-1..7 11:00:00  \n",
    "OnCalendar=hourly\nOnCalendar=Sun *-*-1..7,15..21 11:00:00\n",
    "[Timer]\nOnCalendar=Sun 11:00\nOnCalendar=daily\n",
    "[Timer]\n# OnCalendar=Sun 11:00\n", " OnCalendar=Sun 11:00\n",
    "[Timer]\r\nOnCalendar=Sun 11:00\r\nPersistent=true\r\n",
    "OnCalendar=Sun 11:00",                                        # no trailing newline
    "Description=日本 ☃\x0bOnCalendar=*-*-* 11:00:00\x1cX=1\n",
    "﻿OnCalendar=Sun 11:00\n",
    b"OnCalendar=Sun 11:00\nDescription=\xff\n",                   # not UTF-8: raises
    "oncalendar=Sun 11:00\n", "OnCalendar=\n",
]

KEYS = [k for k, _l, _c in autoupdate.SCHEDULES] + ["", "hourly", "Weekly", "日本", "it's", "a\"b'c", "tab\there\n", "\x00\x7f\xa0 "]


def cases():
    for name in ("UNIT", "SCHEDULES", "DEFAULTS"):
        yield name, {}, (lambda name=name: getattr(autoupdate, name))
    # Paths as their bytes: a home that is not UTF-8 survives (str carries
    # it in surrogateescape form, which JSON cannot).
    for name in ("CONFIG_DIR", "CONFIG", "TIMER_FILE"):
        yield name, {}, (lambda name=name: _os(getattr(autoupdate, name)))
    yield "run_now_argv", {}, lambda: [_os(a) for a in autoupdate.run_now_argv()]

    # The real machine, read only.
    yield "load", {}, autoupdate.load
    yield "is_enabled", {}, autoupdate.is_enabled
    yield "status", {}, autoupdate.status
    yield "current_schedule", {}, autoupdate.current_schedule
    for args in (["is-enabled", autoupdate.UNIT], ["is-enabled", "no-such-unit.timer"],
                 ["show", autoupdate.UNIT, "--property=Unit"], ["show", autoupdate.UNIT, "--property=Id,Description"],
                 ["no-such-verb"], ["cat", "no-such-unit.timer"], []):
        yield "_systemctl", {"args": args}, (lambda args=args: autoupdate._systemctl(*args))
    yield "_systemctl", {"args": ["is-enabled", autoupdate.UNIT], "path": ""}, _no_path

    for c in CONFIGS:
        h = _home()
        yield "load", _args(h, config=c), _with(h, lambda _c: autoupdate.load(), config=c)

    for cfg in CFGS:
        h = _home()
        yield "save", _args(h, cfg=cfg), _with(h, lambda _c, cfg=cfg: _save(cfg))
    h = _home()
    yield "save", _args(h, cfg=dict(autoupdate.DEFAULTS), blocker=True), \
        _with(h, lambda _c: _save(dict(autoupdate.DEFAULTS)), blocker=True)

    for out in FAKE_SHOW:
        for en in FAKE_ENABLED[:3]:
            fake = {"show": [0, out], "is-enabled": en}
            yield "status", {"fake": fake}, _with(_home(), lambda c: {"result": autoupdate.status(), "calls": c}, fake=fake)
    for en in FAKE_ENABLED:
        fake = {"is-enabled": en}
        yield "is_enabled", {"fake": fake}, _with(_home(), lambda c: {"result": autoupdate.is_enabled(), "calls": c}, fake=fake)
    for on in (True, False):
        for res in ([0, ""], [1, "Failed to enable unit: Unit file omarchy-auto-update.timer does not exist."],
                    [0, "Created symlink '/x' → '/y'."]):
            fake = {"enable": res, "disable": res}
            yield "set_enabled", {"on": on, "fake": fake}, \
                _with(_home(), lambda c, on=on: {"result": autoupdate.set_enabled(on), "calls": c}, fake=fake)

    for t in TIMERS:
        h = _home()
        yield "current_schedule", _args(h, timer=t), _with(h, lambda _c: autoupdate.current_schedule(), timer=t)
        for k in KEYS:
            for en in ([0, "enabled"], [1, "disabled"]):
                fake = {"is-enabled": en}
                h = _home()
                yield "set_schedule", _args(h, timer=t, key=k, fake=fake), \
                    _with(h, lambda c, k=k: _sched(k, c), timer=t, fake=fake)
    # The timer unit is a directory, and read-only: OSError messages.
    h = _home()
    (Path(h) / "systemd/user" / autoupdate.UNIT).mkdir(parents=True)
    yield "set_schedule", {"config_home": h, "key": "daily", "fake": {}}, _with(h, lambda c: _sched("daily", c), fake={})
    yield "current_schedule", {"config_home": h}, _with(h, lambda _c: autoupdate.current_schedule())
    h = _home()
    yield "set_schedule", _args(h, timer=TIMER, key="daily", fake={}, timer_readonly=True), \
        _with(h, lambda c: _sched("daily", c), timer=TIMER, fake={}, timer_readonly=True)
    yield from review_cases()
    yield from round2_exec_cases()
    yield from round2b_cases()
    yield from round2c_cases()
    yield from round2d_cases()


def _os(s):
    return os.fsencode(str(s)).decode("utf-8", "backslashreplace")


def _no_path():
    old = os.environ.get("PATH")
    os.environ["PATH"] = ""
    try:
        return autoupdate._systemctl("is-enabled", autoupdate.UNIT)
    finally:
        os.environ["PATH"] = old


def _save(cfg):
    autoupdate.save(cfg)
    return autoupdate.CONFIG.read_text()


def _sched(key, calls):
    r = autoupdate.set_schedule(key)
    try:
        file = autoupdate.TIMER_FILE.read_bytes().decode("utf-8", "replace")
    except OSError:
        file = None
    return {"result": r, "calls": calls, "file": file}


# --- Review round 1: cases that probe where the port can drift. ---

def _fake_bin(name, body, mode=0o755):
    """A directory holding a stand-in `systemctl` script, for PATH."""
    d = _ROOT / f"bin-{name}"
    d.mkdir()
    p = d / "systemctl"
    p.write_bytes(body)
    os.chmod(p, mode)
    return str(d)


def _with_path(path, args, timeout=30):
    def run():
        old = os.environ.get("PATH")
        os.environ["PATH"] = path
        try:
            return autoupdate._systemctl(*args, timeout=timeout)
        finally:
            os.environ["PATH"] = old
    return run


REVIEW_CONFIGS = [
    '{"x": NaN}', '{"x": Infinity}', '{"x": -Infinity}', '{"x": 1e400}',   # Python's json accepts these
    '{"schedule": "\\ud800"}',                                           # lone surrogate: Python accepts
    '[[1e16, "a"], [10000000000000000, "b"]]',                           # 1e16 == 10**16: one key in Python
    '[[1e300, "a"], [1e300, "b"]]',
    '{"big": 123456789012345678901234567890}',
]

REVIEW_CFGS = [
    {"big": 123456789012345678901234567890},
    {"neg": -9223372036854775809},
]

REVIEW_KEYS = ["͸", "؜", "᠎", "\U000e0001", "￿", "⁥", "؀"]


# Review round 2: what JSON transport cannot carry (NaN, ints past 64 bits,
# int keys beside str keys, lone surrogates) is compared as text: the
# json.dumps of what load() gave, and the file a load-then-save writes.
ROUND2_CONFIGS = [
    '{"z": -0}', '{"z": -0.0}', '{"z": 0.0, "w": -0e5}',
    '[[1, "a"], ["1", "b"]]', '[[true, 1], [1, 2], [1.0, 3], ["True", 4]]',
    '[[NaN, 1], [NaN, 2]]', '[[1e400, 1], [Infinity, 2], [-Infinity, 3], [-1e999, 4]]',
    '[[123456789012345678901234567890, 1], [1.2345678901234568e+29, 2]]',
    '[[9007199254740993, 1], [9007199254740992.0, 2], [9007199254740992, 3]]',
    '[[0, "a"], [-0.0, "b"], [false, "c"], [null, "d"], [0.5, "e"], [5e-1, "f"]]',
    '{"s": "\\udc00\\ud800", "t": "\\ud800\\u0041", "u": "\\uD83D\\uDE00"}',
    '{"s": "\\ud800\\udc0"}', '{"s": "\\ud800"', '"\\ud800x"',
    '{"n": 1E5, "m": 1e+5, "o": 1.5e-7, "p": 0.1, "q": 1e22, "r": 123456789.125}',
    '[1,]', '{"a": 1,}', '1.', '01', '-', '1e', '.5', '+1', 'nan', '-NaN', 'Infinityx',
    '{"a": "\x7f"}', '{"a": "\x1f"}', '\f{}', ' \t\r\n{"a": 1} \r\n',
    '{"a": "\\x41"}', '{"a": "\\u00e9\\/\\b\\f\\n\\r\\t\\"\\\\"}',
    '{"k": ' + "1" * 4300 + '}', '{"k": ' + "1" * 4301 + '}', '{"k": -' + "1" * 4300 + '}',
    '{"a": [[[{"b": [{}, [], {"c": null}]}]]]}',
    '{"a": ' + "[" * 600 + "]" * 600 + '}',
    '{"\\ud800": 1, "x": {"\\udfff": [NaN]}}',
]


def _dumps_load(_c):
    return json.dumps(autoupdate.load(), indent=2)


def _load_save(_c):
    autoupdate.save(autoupdate.load())
    return autoupdate.CONFIG.read_text()


def review_cases():
    for c in REVIEW_CONFIGS:
        h = _home()
        yield "load", _args(h, config=c, dumps=True), _with(h, _dumps_load, config=c)
    for c in CONFIGS + REVIEW_CONFIGS + ROUND2_CONFIGS:
        h = _home()
        yield "load", _args(h, config=c, dumps=True), _with(h, _dumps_load, config=c)
        h = _home()
        yield "load_save", _args(h, config=c), _with(h, _load_save, config=c)
    for c in ROUND2_CONFIGS:
        if "NaN" in c or "Infinity" in c or "1e400" in c or "1e999" in c or "\\ud8" in c or "\\udc" in c \
                or "1" * 400 in c:
            continue          # the plain load result cannot travel as JSON
        h = _home()
        yield "load", _args(h, config=c), _with(h, lambda _c: autoupdate.load(), config=c)
    for cfg in REVIEW_CFGS + [{"x": float("nan"), "y": float("inf"), "s": "\ud800", "big": -10 ** 40}]:
        h = _home()
        yield "save", _args(h, cfg_json=json.dumps(cfg)), _with(h, lambda _c, cfg=cfg: _save(cfg))
    # Too deep for json: RecursionError escapes load(). (Python's limits are
    # C-stack measurements, ~52130 levels to load and ~52120 to save, so
    # nothing is tried near them; a save that deep writes gigabytes.)
    for n in (50000, 52200):
        c = '{"a": ' + "[" * n + "]" * n + '}'
        h = _home()
        yield "load", _args(h, config=c, len=True), _with(h, lambda _c: len(autoupdate.load()), config=c)
    for k in REVIEW_KEYS:
        h = _home()
        fake = {"is-enabled": [0, "enabled"]}
        yield "set_schedule", _args(h, timer=TIMER, key=k, fake=fake), \
            _with(h, lambda c, k=k: _sched(k, c), timer=TIMER, fake=fake)

    # A stand-in systemctl on PATH: text-mode decoding, signals, permissions,
    # and a child that leaves its pipes open.
    unit = ["is-enabled", autoupdate.UNIT]
    for name, body, mode, timeout in (
        ("crlf", b"#!/bin/sh\nprintf 'enabled\\r\\nline2\\rx\\r\\n'\n", 0o755, 30),
        ("badutf8", b"#!/bin/sh\nprintf 'enabled\\377\\n'\n", 0o755, 30),
        ("signal", b"#!/bin/sh\nkill -TERM $$\n", 0o755, 30),
        ("noexec", b"#!/bin/sh\necho enabled\n", 0o644, 30),
        ("sleep", b"#!/bin/sh\nexec /usr/bin/sleep 3\n", 0o755, 1),
        ("orphan", b"#!/bin/sh\n(/usr/bin/sleep 3 &)\necho enabled\n", 0o755, 1),
    ):
        p = _fake_bin(name, body, mode)
        yield "_systemctl", {"args": unit, "path": p, "timeout": timeout}, _with_path(p, unit, timeout)


# --- Review round 2 (adversarial): exec edge cases on PATH. ---

def _fake_dir_bin():
    """A PATH directory whose `systemctl` is itself a directory (EACCES)."""
    d = _ROOT / "bin-isdir"
    (d / "systemctl").mkdir(parents=True)
    return str(d)


def round2_exec_cases():
    unit = ["is-enabled", autoupdate.UNIT]
    # No shebang: execve gives ENOEXEC. Python's _posixsubprocess reports it
    # as OSError; execvp-style lookups retry the file with /bin/sh.
    p = _fake_bin("noshebang", b"echo enabled\n", 0o755)
    yield "_systemctl", {"args": unit, "path": p, "timeout": 30}, _with_path(p, unit, 30)
    # Empty file, executable: also ENOEXEC for execve.
    p = _fake_bin("empty", b"", 0o755)
    yield "_systemctl", {"args": unit, "path": p, "timeout": 30}, _with_path(p, unit, 30)
    # ENOEXEC first on PATH, a working one second.
    p2 = _fake_bin("good", b"#!/bin/sh\necho enabled\n", 0o755)
    p = _ROOT / "bin-noshebang"
    path = f"{p}:{p2}"
    yield "_systemctl", {"args": unit, "path": path, "timeout": 30}, _with_path(path, unit, 30)
    p = _fake_dir_bin()
    yield "_systemctl", {"args": unit, "path": p, "timeout": 30}, _with_path(p, unit, 30)
    path = f"{p}:{p2}"
    yield "_systemctl", {"args": unit, "path": path, "timeout": 30}, _with_path(path, unit, 30)
    # Timeout 0: Python times out at once.
    p = _fake_bin("zero", b"#!/bin/sh\necho enabled\n", 0o755)
    yield "_systemctl", {"args": unit, "path": p, "timeout": 0}, _with_path(p, unit, 0)


ROUND2B_CONFIGS = [
    '{"f": [5e-324, 2.2250738585072014e-308, 1e16, 1e-5, 0.0001, 1e15, 9999999999999998.0, '
    '123456789012345680000, 1.7976931348623157e308, 0.30000000000000004, 1e-7, 100.0, 1.5e300, 4.35, 2.675]}',
    '{"g": [1.0e0, 10e-1, 0.1e1, -0.0e-0, 1E-400, 9.999999999999999e22, 1e23]}',
    '[["k", 1], ["k", 2], [1, 3], [1.0, 4], [true, 5]]',
    '{"\\u0000": "\\u001f\\u007f\\u0080\\u00ff\\u0100\\uffff", "\\ud83d\\ude00": "\\ud83d\\ude00x"}',
    '{"a": "\\uDBFF\\uDFFF", "b": "\\udbff\\u0020", "c": "\\ud800\\ud800\\udc00"}',
    '[[2e0, "a"], [2, "b"], [1e0, "c"], ["1", "d"]]',
]


def round2b_cases():
    for c in ROUND2B_CONFIGS:
        h = _home()
        yield "load", _args(h, config=c, dumps=True), _with(h, _dumps_load, config=c)
        h = _home()
        yield "load_save", _args(h, config=c), _with(h, _load_save, config=c)


def round2c_cases():
    # Every code point (no surrogates) through repr() in the "unknown
    # schedule" message, a plane per case: str.isprintable() per character.
    for lo in range(0, 0x110000, 0x10000):
        k = "".join(chr(c) for c in range(lo, lo + 0x10000) if not 0xD800 <= c <= 0xDFFF)
        h = _home()
        yield "set_schedule", {"key": k, "fake": {}, "config_home": h}, \
            _with(h, lambda c, k=k: _sched(k, c), fake={})


def round2d_cases():
    # An argument with a NUL byte: Python's subprocess raises ValueError
    # ("embedded null byte"), which _systemctl does not catch.
    for args in (["is-enabled", "a\x00b"], ["\x00"]):
        yield "_systemctl", {"args": args}, (lambda args=args: autoupdate._systemctl(*args))
