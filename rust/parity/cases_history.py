"""Parity cases for history.py.

`env` in the args is applied on both sides for the duration of the call
(None removes the variable). Writers (`record`) get a fresh temp state dir
on each side, so neither touches the real ~/.local/state; the log they leave
is compared with each "ts" (the current time) masked. Readers run against
fixtures in temp dirs and against the real machine as it is.
"""
import atexit
import dataclasses
import json
import math
import os
import re
import shutil
import tempfile
from datetime import date, timedelta
from pathlib import Path

import history

_TMP = tempfile.mkdtemp(prefix="almanac-parity-history-")
atexit.register(shutil.rmtree, _TMP, True)
_n = [0]


def _dir(tag):
    _n[0] += 1
    d = Path(_TMP, f"{tag}-{_n[0]}")
    d.mkdir(parents=True)
    return str(d)


def _with_env(env, fn):
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


_TS = re.compile(r'^\{"ts": [0-9.e+-]+, ')


def _masked_log():
    try:
        text = history._log_path().read_text()
    except OSError:
        return []
    return [_TS.sub('{"ts": TS, ', l) for l in text.split("\n")]


def _wire(v):
    """What JSON cannot carry, spelled out (the Rust hook does the same with
    `_wire`): a non-finite float as {"__float__": "inf"}, a str holding
    surrogates as {"__str__": [code points]}."""
    if dataclasses.is_dataclass(v) and not isinstance(v, type):
        return {f.name: _wire(getattr(v, f.name)) for f in dataclasses.fields(v)}
    if isinstance(v, dict):
        return {k: _wire(x) for k, x in v.items()}
    if isinstance(v, (list, tuple)):
        return [_wire(x) for x in v]
    if isinstance(v, float) and not math.isfinite(v):
        return {"__float__": repr(v)}
    if isinstance(v, str) and any(0xD800 <= ord(c) <= 0xDFFF for c in v):
        return {"__str__": [ord(c) for c in v]}
    return v


def _record_case(kind, title, detail="", severity="", items=None, times=1,
                 summary=False, prefill=None):
    """Python and Rust each record into their own fresh state dir."""
    py, rs = _dir("rec-py"), _dir("rec-rs")
    if prefill is not None:
        for d in (py, rs):
            p = Path(d, "almanac", "history.jsonl")
            p.parent.mkdir(parents=True)
            p.write_text(prefill)

    def fn():
        for _ in range(times):
            history.record(kind, title, detail, severity, items)
        log = _masked_log()
        if summary:
            return [len(log), log[0] if log else None,
                    log[max(0, len(log) - 2)] if log else None]
        return log
    args = {"env": {"XDG_STATE_HOME": rs}, "kind": kind, "title": title,
            "detail": detail, "severity": severity, "items": items or [],
            "times": times, "summary": summary}
    return "record", args, _with_env({"XDG_STATE_HOME": py}, fn)


def _state(text):
    d = _dir("state")
    p = Path(d, "almanac", "history.jsonl")
    p.parent.mkdir(parents=True)
    p.write_bytes(text.encode("utf-8") if isinstance(text, str) else text)
    return d


LOG = (
    "[2026-10-16T23:02:32-0700] [ALPM] transaction started\n"
    "[2026-10-16T23:02:33-0700] [ALPM] upgraded a (1-1 -> 2-1)\n"
    "[2026-10-16T23:02:33-0700] [ALPM] upgraded b (1-1 -> 2-1)\n"
    "[2026-10-16T23:02:33-0700] [ALPM] installed c (1-1)\n"
    "[2026-10-16T23:02:34-0700] [ALPM] transaction completed\n"
    "[2026-10-16T23:05:00-0700] [ALPM] transaction started\n"
    "[2026-10-16T23:05:01-0700] [ALPM] transaction completed\n")

KEYRING = ("[2026-10-16T23:02:32-0700] [ALPM] transaction started\n"
           "[2026-10-16T23:02:33-0700] [ALPM] reinstalled archlinux-keyring "
           "(20261001-1)\n"
           "[2026-10-16T23:02:34-0700] [ALPM] transaction completed\n")

MIXED = (
    "garbage line\n"
    "[2026-10-01T10:00:00+0200] [ALPM] upgraded orphan (1 -> 2)\n"   # no transaction yet
    "[2026-10-01T10:00:00+0200] [PACMAN] Running 'pacman -Syu'\n"
    "[2026-10-01T10:00:01+0200] [ALPM] transaction started\n"
    "[2026-10-01T10:00:02+0200] [ALPM] removed old (1-1)\n"
    "[2026-10-01T10:00:02+0200] [PACMAN] removed fake (1-1)\n"       # not ALPM
    "[2026-10-01T10:00:02+0200] [ALPM] downgraded d (2-1 -> 1-1)\n"
    "[2026-10-01T10:00:02+0200] [ALPM] downgraded e (2-1 -> 1-1)\n"
    "[2026-10-01T10:00:02+0200] [ALPM] reinstalled archlinux-keyring (1)\n"
    "[2026-10-01T10:00:02+0200] [ALPM] installed ünïcødé-pkg (1-1)\n"
    "[2026-10-01T10:00:02+0200] [ALPM-SCRIPTLET] upgraded x (1 -> 2)\n"
    "[2026-10-01T10:00:02+0200] [ALPM] upgraded\n"
    "[2026-10-01T10:00:03+0200] [ALPM] transaction completed\n"
    "[2026-10-02 11:00] [ALPM] transaction started\n"                # old format
    "[2026-10-02 11:00] [ALPM] installed oldfmt (1)\r\n"
    "[2026-10-02 11:01] [ALPM] transaction completed\r\n"
    "[2026-10-03T09:00:00Z] [ALPM] transaction started\n"
    "[2026-10-03T09:00:00Z] [ALPM] upgraded z (1 -> 2)\n"            # never completed
    "[2026-10-03T09:00:00Z] [ALPM] transaction started\n"
    "[2026-10-03T09:00:01Z] [ALPM] upgraded y (1 -> 2)\n"
    "[2026-10-03T09:00:02Z] [ALPM] transaction completed\n"
    "[bad-date] [ALPM] transaction started\n"
    "[bad-date] [ALPM] installed baddate (1)\n"
    "[bad-date] [ALPM] transaction completed\n"
    "[2026-10-04T09:00:00+0000] [ALPM] transaction completed\n")


def _jrec(msg, ts="1792216942000000", **extra):
    r = {"MESSAGE": msg, "__REALTIME_TIMESTAMP": ts}
    r.update(extra)
    return json.dumps(r)


JOURNAL = "\n".join([
    _jrec("tim : TTY=pts/4 ; COMMAND=/usr/bin/snapper -c root create -c number -d 4.0.4-1"),
    _jrec("tim : TTY=pts/4 ; COMMAND=/usr/bin/snapper -c root create -d before update -c number",
          ts="1792216000123456"),
    _jrec("tim : COMMAND=/usr/bin/snapper create"),
    _jrec("tim : COMMAND=/usr/bin/snapper list"),
    _jrec("tim : COMMAND=/usr/bin/snapperx create -d nope"),
    _jrec("tim : COMMAND=snapper -c home create --description x -d  spaced desc  "),
    _jrec("tim : COMMAND=/usr/bin/snapper create -d “ünï” 日本"),
    _jrec("tim : COMMAND=/usr/bin/ls ; snapper create"),
    json.dumps({"MESSAGE": [116, 105, 109], "__REALTIME_TIMESTAMP": "1"}),
    json.dumps({"MESSAGE": None}),
    json.dumps({"MESSAGE": "COMMAND=/usr/bin/snapper create -d nots"}),
    json.dumps({"MESSAGE": "COMMAND=/usr/bin/snapper create -d intts",
                "__REALTIME_TIMESTAMP": 1792216942000001}),
    "not json",
    "",
    "",
    _jrec("x COMMAND=/usr/bin/snapper create -d trailing\n"),
    _jrec("x COMMAND=/usr/bin/snapper create -d line\nsecond -d more"),
])


def _resets_home():
    d = Path(_dir("config"))
    files = ["hypr/hyprland.conf.bak.1792216942", "hypr/hyprland.conf",
             "waybar/config.jsonc.bak.1700000000", "waybar/style.css.bak.12345678",
             "a/b/c/deep.conf.bak.1792216000", "a/b/c/d/toodeep.conf.bak.1792216000",
             "top.bak.1792216100", "ünï/cødé.ini.bak.1792216200",
             "x.bak.17922161000", "y.bak.1792216100a", ".bak.1792216100"]
    for f in files:
        p = d / f
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text("x")
    (d / "linkdir").symlink_to(d / "hypr")
    (d / "link.bak.1792216300").symlink_to(d / "top.bak.1792216100")
    return str(d)


def cases():
    # ---- Day.label
    today = date.today()
    for delta in (0, 1, 2, 6, 7, 400, -1, -2):
        d = today - timedelta(days=delta)
        yield "Day.label", {"day": d.isoformat()}, (lambda d=d: history.Day(d, []).label)
    for iso in ("2026-01-01", "2024-02-29", "1999-12-31"):
        d = date.fromisoformat(iso)
        yield "Day.label", {"day": iso}, (lambda d=d: history.Day(d, []).label)

    # ---- record (temp state dirs)
    yield _record_case(history.ALMANAC, "yaru did not change", severity="warn")
    yield _record_case(history.ALMANAC, "Restored settings", "3 files",
                       items=["a.conf", "b ünï.json", 'q"uote\\'])
    yield _record_case("custom", "ünïcødé 日本 “quotes” \U0001F600 \t\n\x00\x7f",
                       "line sep", "warn", times=3)
    yield _record_case("", "")
    yield _record_case(history.ALMANAC, "appended", prefill='{"ts": 1, "title": "old"}\n')
    big = "".join(json.dumps({"ts": i, "kind": "almanac", "title": "x" * 200}) + "\n"
                  for i in range(3000))
    yield _record_case(history.ALMANAC, "trimmed", prefill=big, summary=True)

    # ---- recorded (fixture state dirs, then the real machine)
    lines = "\n".join([
        json.dumps({"ts": 100, "kind": "x", "title": "int ts", "detail": "d",
                    "severity": "warn", "items": ["a", "b"]}),
        json.dumps({"ts": 1792216942.5, "title": "float ts"}),
        json.dumps({"ts": 50.0, "title": "old"}),
        json.dumps({"ts": 200, "items": None, "severity": "warn"}),
        json.dumps({"ts": 300, "title": "ünï 日本"}, ensure_ascii=False),
        "not json", "", "{broken",
        json.dumps({"ts": 400, "title": "after\rcr"}),
    ]) + "\r\n" + json.dumps({"ts": 500, "title": "crlf"}) + "\x0c" + \
        json.dumps({"ts": 600, "title": "formfeed"})
    st = _state(lines)
    for since in (0, 99.5, 100, 1792216942.5, 1e12, -1):
        yield "recorded", {"env": {"XDG_STATE_HOME": st}, "since": since}, \
            _with_env({"XDG_STATE_HOME": st}, lambda s=since: history.recorded(s))
    empty = _dir("nostate")
    yield "recorded", {"env": {"XDG_STATE_HOME": empty}, "since": 0}, \
        _with_env({"XDG_STATE_HOME": empty}, lambda: history.recorded(0))
    yield "recorded", {"since": 0}, lambda: history.recorded(0)          # the real machine

    # ---- package_events (fixtures, then the real pacman.log)
    for name, text in (("log", LOG), ("keyring", KEYRING), ("mixed", MIXED),
                       ("empty", ""), ("newlines", "\n\n\n"),
                       ("unicode", "[2026-10-01T10:00:00+0200] [ALPM] transaction started "
                                   "[2026-10-01T10:00:00+0200] [ALPM] installed 日本 (1) "
                                   "[2026-10-01T10:00:00+0200] [ALPM] transaction completed")):
        for since in (0, 1792216952, 1792216953, 1e12):
            yield "package_events", {"since": since, "log_text": text}, \
                (lambda s=since, t=text: history.package_events(s, t))
    yield "package_events", {"since": 0}, lambda: history.package_events(0)
    yield "package_events", {"since": 1790000000}, lambda: history.package_events(1790000000)

    # ---- _summarise
    for changes in ([], ["upgraded a (1 -> 2)"], ["installed x (1)", "installed y (1)"],
                    ["removed r (1)", "upgraded u (1 -> 2)", "reinstalled k (1)",
                     "downgraded d (2 -> 1)", "installed i (1)"],
                    ["nonsense", "upgraded"], ["reinstalled a (1)", "reinstalled b (1)"],
                    ["installed ünï (1)", "installed a\x1fb (1)"]):
        yield "_summarise", {"changes": changes}, lambda c=changes: history._summarise(c)

    # ---- snapshot_events (fixtures, then the real journal)
    for since in (0, 1792216942):
        yield "snapshot_events", {"since": since, "journal_text": JOURNAL}, \
            (lambda s=since: history.snapshot_events(s, JOURNAL))
    yield "snapshot_events", {"since": 0, "journal_text": ""}, \
        lambda: history.snapshot_events(0, "")
    for since in (0, 1790000000, 1e10):
        yield "snapshot_events", {"since": since}, (lambda s=since: history.snapshot_events(s))

    # ---- reset_events (a fixture config home, then the real one)
    home = _resets_home()
    for since in (0, 1792216100, 1792216300, 1e12):
        yield "reset_events", {"env": {"XDG_CONFIG_HOME": home}, "since": since}, \
            _with_env({"XDG_CONFIG_HOME": home}, lambda s=since: history.reset_events(s))
    missing = str(Path(_TMP, "does-not-exist"))
    yield "reset_events", {"env": {"XDG_CONFIG_HOME": missing}, "since": 0}, \
        _with_env({"XDG_CONFIG_HOME": missing}, lambda: history.reset_events(0))
    yield "reset_events", {"since": 0}, lambda: history.reset_events(0)

    yield from _review_round1()
    yield from _review_round2()

    # ---- timeline (the real machine; demo mode under --demo)
    for days in (30, 1, 0, 365):
        yield "timeline", {"days": days}, (lambda d=days: _days(history.timeline(d)))


def _days(days):
    """Day.day as ISO text: run.py's normalise() would repr() a date."""
    return [{"day": d.day.isoformat(), "events": d.events, "snapshot": d.snapshot}
            for d in days]


def _review_round1():
    """Adversarial review, round 1: inputs where the port is suspected to differ."""
    # recorded: non-string fields come back as they are in Python.
    for name, line in (
            ("title-null", '{"ts": 100, "title": null, "detail": 5, "severity": ["w"]}'),
            ("items-str", '{"ts": 100, "title": "t", "items": "abc"}'),
            ("items-mixed", '{"ts": 100, "title": "t", "items": [1, null, "s", {"a": 1}]}'),
            ("ts-infinity", '{"ts": Infinity, "title": "inf"}\n{"ts": 100, "title": "ok"}'),
            ("ts-nan-literal", '{"ts": 100, "title": NaN}'),
            ("lone-surrogate", '{"ts": 100, "title": "\\ud800x"}\n{"ts": 101, "title": "ok"}'),
            ("ts-bool", '{"ts": true, "title": "bool ts"}'),
            ("ts-missing", '{"title": "no ts"}'),
            ("ts-string", '{"ts": "5", "title": "str ts"}'),
            ("not-object", '[1, 2]'),
            ("ts-1e400", '{"ts": 1e400, "title": "huge"}'),
            ("ts-nan", '{"ts": NaN, "title": "nan ts"}\n{"ts": 1, "title": "ok"}'),
            ("ts-bigint", '{"ts": 1' + '0' * 30 + ', "title": "big"}'),
            ("items-falsy", '{"ts": 1, "items": 0}\n{"ts": 2, "items": ""}\n{"ts": 3, "items": {}}'),
            ("items-dict", '{"ts": 1, "items": {"a": [1.5, -Infinity]}}'),
            ("dup-key", '{"ts": 1, "title": "a", "title": "b"}'),
            ("bom", '\ufeff{"ts": 1, "title": "bom"}\n{"ts": 2, "title": "ok"}'),
            ("deep", '{"ts": 1, "items": ' + "[" * 300 + "]" * 300 + '}'),
            ("scalar", '5'),
            ("null-line", 'null')):
        st = _state(line)
        yield "recorded", {"env": {"XDG_STATE_HOME": st}, "since": 0, "_case": name, "_wire": True}, \
            _with_env({"XDG_STATE_HOME": st}, lambda: _wire(history.recorded(0)))
    # A missing or uncomparable ts only raises when it is reached and compared.
    for name, line, since in (("ts-missing-later", '{"title": "no ts"}', 1),
                              ("ts-bool-false", '{"ts": false}', 0),
                              ("ts-bigint-since", '{"ts": 9007199254740993}', 9007199254740992.0)):
        st = _state(line)
        yield "recorded", {"env": {"XDG_STATE_HOME": st}, "since": since, "_case": name}, \
            _with_env({"XDG_STATE_HOME": st}, lambda s=since: history.recorded(s))
    # recorded: a log that is not valid UTF-8 (read_text raises UnicodeDecodeError).
    st = _state(b'{"ts": 100, "title": "ok"}\n\xff\xfe\n')
    yield "recorded", {"env": {"XDG_STATE_HOME": st}, "since": 0, "_case": "bad-utf8"}, \
        _with_env({"XDG_STATE_HOME": st}, lambda: history.recorded(0))

    # record: trimming a big log that is not valid UTF-8 raises in Python.
    py, rs = _dir("rec-py"), _dir("rec-rs")
    blob = b"\xff" + b"x" * (600 * 1024) + b"\n"
    for d in (py, rs):
        p = Path(d, "almanac", "history.jsonl")
        p.parent.mkdir(parents=True)
        p.write_bytes(blob)

    def rec_bad():
        history.record(history.ALMANAC, "t")
        log = _masked_log()
        return [len(log), log[0] if log else None, log[max(0, len(log) - 2)] if log else None]
    yield "record", {"env": {"XDG_STATE_HOME": rs}, "kind": "almanac", "title": "t",
                     "detail": "", "severity": "", "items": [], "times": 1, "summary": True,
                     "_case": "trim-bad-utf8"}, \
        _with_env({"XDG_STATE_HOME": py}, rec_bad)

    # snapshot_events: regex \b and int() differences.
    for name, rec in (
            ("combining-mark", _jrec("x COMMAND=/usr/bin/snapper\u0301 create -d cm")),
            ("superscript", _jrec("x COMMAND=/usr/bin/snapper\u00b2 create -d sup")),
            ("undertie", _jrec("x COMMAND=/usr/bin/snapper\u203f create -d pc")),
            ("fullwidth-ts", _jrec("x COMMAND=/usr/bin/snapper create -d fw",
                                   ts="\uff11\uff17\uff19\uff12\uff12\uff11\uff16\uff19\uff14\uff12\uff10\uff10\uff10\uff10\uff10\uff10")),
            ("huge-ts", _jrec("x COMMAND=/usr/bin/snapper create -d big", ts="1" + "0" * 45)),
            ("float-str-ts", _jrec("x COMMAND=/usr/bin/snapper create -d f", ts="1.5")),
            ("null-ts", _jrec("x COMMAND=/usr/bin/snapper create -d n", ts=None)),
            ("underscore-ts", _jrec("x COMMAND=/usr/bin/snapper create -d u", ts="_1")),
            ("not-object", "[1, 2]"),
            ("float-ts", json.dumps({"MESSAGE": "COMMAND=snapper create", "__REALTIME_TIMESTAMP": 1792216942.9e6})),
            ("bool-ts", json.dumps({"MESSAGE": "COMMAND=snapper create", "__REALTIME_TIMESTAMP": True})),
            ("inf-ts", '{"MESSAGE": "COMMAND=snapper create", "__REALTIME_TIMESTAMP": Infinity}'),
            ("int-overflow-ts", _jrec("x COMMAND=snapper create", ts="1" + "0" * 400)),
            ("space-ts", _jrec("x COMMAND=snapper create", ts="\u2003 17_92 \n")),
            ("nan-message", '{"MESSAGE": NaN}'),
            ("surrogate-desc", '{"MESSAGE": "COMMAND=snapper create -d a\\udc80b", "__REALTIME_TIMESTAMP": "5"}'),
            ("word-digit", _jrec("x COMMAND=/usr/bin/snapper create\u0663 -d x")),
            ("newline-between", _jrec("x COMMAND=/usr/bin/snapper\ncreate"))):
        yield "snapshot_events", {"since": 0, "journal_text": rec, "_case": name, "_wire": True}, \
            (lambda r=rec: _wire(history.snapshot_events(0, r)))
    # Far past i64 (Python's int() does not saturate); the journal is real.
    yield "snapshot_events", {"since": 1e20}, lambda: history.snapshot_events(1e20)

    # reset_events: Unicode digits, a non-UTF-8 name, a "." base.
    for name, fname in (("arabic-digits", "u.conf.bak.\u0661\u0667\u0669\u0662\u0662\u0661\u0666\u0661\u0660\u0660"),
                        ("non-utf8", b"\xff.conf.bak.1792216100"),
                        ("dot-base", "..bak.1792216100"),
                        ("dotdot-base", "...bak.1792216100"),
                        ("thai-digits", "t.ini.bak.\u0e51\u0e57\u0e59\u0e52\u0e52\u0e51\u0e56\u0e51\u0e50\u0e50"),
                        ("non-utf8-dir", b"\xfe/x.conf.bak.1792216100")):
        home = Path(_dir("config"))
        target = os.path.join(os.fsencode(str(home)), fname) if isinstance(fname, bytes) \
            else str(home / fname)
        os.makedirs(os.path.dirname(target), exist_ok=True)
        with open(target, "w") as fh:
            fh.write("x")
        yield "reset_events", {"env": {"XDG_CONFIG_HOME": str(home)}, "since": 0, "_case": name,
                               "_wire": True}, \
            _with_env({"XDG_CONFIG_HOME": str(home)}, lambda: _wire(history.reset_events(0)))


def _in_tz(tz, env, expr):
    """Evaluate `expr` (history imported) in a fresh Python with TZ set: the C
    library reads TZ once per process, so os.environ cannot change it here."""
    import subprocess
    import sys
    code = ("import json, sys; sys.path.insert(0, %r); import history\n"
            "def _days(days):\n"
            "    import dataclasses\n"
            "    return [{'day': d.day.isoformat(), 'events': [dataclasses.asdict(e) for e in d.events],"
            " 'snapshot': d.snapshot} for d in days]\n"
            "print(json.dumps(%s))" % (str(Path(history.__file__).parent), expr))
    full = dict(os.environ, TZ=tz, **env)

    def run():
        r = subprocess.run([sys.executable, "-c", code], env=full, capture_output=True,
                           text=True, timeout=120)
        if r.returncode:
            raise RuntimeError(r.stderr[-500:])
        return json.loads(r.stdout)
    return run


def _review_round2():
    """Adversarial review, round 2."""
    from datetime import datetime, timezone
    # An unknown TZ: glibc (Python's localtime) falls back to UTC; chrono's
    # Local falls back to /etc/localtime instead. 03:00 UTC is the evening
    # before in America/Los_Angeles (this machine), so the day differs.
    st = _state("\n".join(json.dumps({"ts": t, "title": f"at {t}"})
                          for t in (1792465200, 1792465200 + 86400 * 2)))
    cfg = _dir("tz-config")
    env = {"XDG_STATE_HOME": st, "XDG_CONFIG_HOME": cfg}
    for tz in ("America/Los_Angeles", "Nowhere/Invalid"):    # the first: a control
        yield "timeline", {"env": dict(env, TZ=tz), "days": 30, "_case": "tz-" + tz}, \
            _in_tz(tz, env, "_days(history.timeline(30))")
        utc_today = datetime.now(timezone.utc).date().isoformat()
        yield "Day.label", {"env": {"TZ": tz}, "day": utc_today, "_case": "tz-" + tz}, \
            _in_tz(tz, {}, "history.Day(__import__('datetime').date.fromisoformat(%r), []).label"
                   % utc_today)
