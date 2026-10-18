"""Parity cases for demo.py.

Everything here is fixed data except two things: `active()` reads
ALMANAC_DEMO (passed as `env`, applied on both sides), and `history()`
reads `time.time()`, pinned here by patching `time.time` on the Python side
and passing the same `now` to the Rust side. `picks` gets synthetic store
rows; `"eligible_stub": true` swaps in the tests' `picks.eligible` (not
installed and on GitHub), so the choosing can be checked without store's
trust port; the cases without it use the real one.
"""
from __future__ import annotations

import os
import time
from datetime import datetime
from unittest import mock

import demo
import picks
import store


def _env(env, fn):
    old = {k: os.environ.get(k) for k in env}
    try:
        for k, v in env.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v
        return fn()
    finally:
        for k, v in old.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v


def _days(days):
    """Day.day as ISO text: run.py's normalise() would repr() a date."""
    return [{"day": d.day.isoformat(), "events": d.events, "snapshot": d.snapshot}
            for d in days]


def _history(now):
    with mock.patch("time.time", return_value=now):
        return _days(demo.history())


def P(pid, market="omarchy", stars=50, **kw):
    d = {"id": pid, "name": pid.title(), "stars": stars, "market": market,
         "repo": f"https://github.com/o/{pid}", "installable": True,
         "verified": True, "install_cmd": "x", "installed": False}
    d.update(kw)
    return d


def _picks(ds, stub):
    saved = picks.eligible
    try:
        if stub:
            picks.eligible = lambda p: not p.installed and bool(picks._github(p.repo))
        return demo.picks([store.Plugin(**d) for d in ds])
    finally:
        picks.eligible = saved


def cases():
    # ---- active
    for v in (None, "", "0", "false", "1", " 1 ", "true", "yes", "False", " 0 ",
              "\t", "dÉmo"):
        env = {"ALMANAC_DEMO": v}
        yield "active", {"env": env}, (lambda env=env: _env(env, demo.active))

    # ---- constants
    yield "HOME", {}, lambda: demo.HOME
    yield "INSTALLED_PLUGIN_IDS", {}, lambda: demo.INSTALLED_PLUGIN_IDS
    yield "INSTALLED_THEMES", {}, lambda: demo.INSTALLED_THEMES

    # ---- the fixtures
    for f in ("packages", "appimages", "flatpaks", "local_builds", "agents",
              "collect", "findings", "ai_groups", "plugins", "startup_apps",
              "user_services"):
        yield f, {}, getattr(demo, f)

    # ---- history, with time pinned (plus around this zone's DST changes,
    # and midnight, where "days ago" crosses a date)
    nows = [1_800_000_000.0, float(int(time.time())),
            datetime(2026, 3, 8, 12, 0).timestamp(),
            datetime(2026, 3, 29, 12, 0).timestamp(),
            datetime(2026, 11, 1, 0, 30).timestamp(),
            datetime(2026, 10, 25, 23, 59).timestamp(),
            datetime(2027, 1, 1, 0, 0, 1).timestamp(),
            1_800_000_000.5]
    for now in nows:
        yield "history", {"now": now}, (lambda now=now: _history(now))

    # ---- picks
    sets = [
        [],
        [P("a", stars=10), P("b", stars=30), P("c", stars=20)],
        [P("a", stars=10, market="omarchy"), P("b", stars=30, market="omarchy"),
         P("c", stars=20, market="aether"), P("d", stars=99, market="aether")],
        [P("one", stars=5)],
        [P("x", stars=5, installed=True), P("y", stars=7, repo="https://gitlab.com/o/y"),
         P("z", stars=1, market="other")],
        [P("tie1", stars=10, market="m1"), P("tie2", stars=10, market="m2"),
         P("tie3", stars=10, market="m3")],
        [P("ünï", stars=1_500_000, market="ü", name="Ünïcode ✓"),
         P("big", stars=2_000_000, market="ü")],
        [P("theme", stars=80, install_kind="theme"),
         P("reviewed", stars=40, market="aether"),
         P("noinstall", stars=90, installable=False)],
    ]
    for ds in sets:
        yield "picks", {"plugins": ds, "eligible_stub": True}, \
            (lambda ds=ds: _picks(ds, True))
    for ds in sets:
        yield "picks", {"plugins": ds}, (lambda ds=ds: _picks(ds, False))


# ---- review round 1 (adversarial): edges the cases above do not reach


# DST transitions at the fixtures' own hours (POSIX TZ rules), so mktime's
# handling of a skipped / repeated wall-clock time is exercised. chrono's Local
# caches the zone on first use, so these cannot be switched per case: they run
# only when the whole parity run is started under the rule, e.g.
#   TZ='XST0XDT-0:30,M3.2.0/9,M11.1.0/10' python3 parity/run.py demo
# - a 30-minute DST gap at 09:00 on 2026-03-08 (09:13 / 09:14 do not exist)
# - a 1-hour fall-back at 10:00 on 2026-11-01 (09:13 / 09:14 happen twice)
# - a 1-hour gap at 10:00 on 2026-03-08 (10:28 / 10:30 do not exist)
_TZ_RULES = {
    "XST0XDT-0:30,M3.2.0/9,M11.1.0/10": 1772990000.0,
    "XST0XDT,M3.2.0/9,M11.1.0/10": 1793560000.0,
    "XST0XDT,M3.2.0/10,M11.1.0/2": 1773259200.0,
}


def review_cases():
    # str.strip() also strips the ASCII separators \x1c-\x1f; Rust's trim()
    # does not.
    for v in ("\x1c", "\x1f", " \x1d0\x1e ", "\x85", "　", " false"):
        env = {"ALMANAC_DEMO": v}
        yield "active", {"env": env}, (lambda env=env: _env(env, demo.active))

    now = _TZ_RULES.get(os.environ.get("TZ", ""))
    if now is not None:
        yield "history", {"now": now}, (lambda now=now: _history(now))

    # Out of datetime's range: Python raises, Rust must report an error.
    for now in (3e11, 1e20, -1e12):
        yield "history", {"now": now}, (lambda now=now: _history(now))

    # -p.stars on the smallest int: Python has no overflow.
    ds = [P("lo", stars=-2**63, market="m1"), P("hi", stars=3, market="m2")]
    yield "picks", {"plugins": ds, "eligible_stub": True}, \
        (lambda ds=ds: _picks(ds, True))


_orig_cases = cases


def cases():  # noqa: F811
    yield from _orig_cases()
    yield from review_cases()


# ---- review round 2 (adversarial)


def review2_cases():
    # datetime.fromtimestamp() probes 24 h back for a fold; on 0001-01-01
    # local that probe is year 0 and raises ValueError. Rust's year check
    # only looks at the event's own date, so it returns days instead.
    for d in (1, 4, 5):
        now = time.mktime((1, 1, d, 12, 0, 0, 0, 0, -1))
        yield "history", {"now": now}, (lambda now=now: _history(now))

    # Negative fractional now: localtime() floors (ROUND_FLOOR).
    for now in (-1_000_000_000.5, -0.5):
        yield "history", {"now": now}, (lambda now=now: _history(now))

    # An unknown TZ name: glibc (Python) falls back to UTC, chrono's Local
    # falls back to /etc/localtime. Run the whole module under it to see:
    #   TZ=Europe/Nowhere python3 parity/run.py demo   (every history case)

    # stars beyond i64: Python sorts it; the Rust parity hook's
    # serde_json::from_value(...).ok() drops the row silently.
    ds = [P("huge", stars=2**64, market="m1"), P("small", stars=3, market="m2"),
          P("mid", stars=5, market="m3")]
    yield "picks", {"plugins": ds, "eligible_stub": True}, \
        (lambda ds=ds: _picks(ds, True))


_orig_cases2 = cases


def cases():  # noqa: F811
    yield from _orig_cases2()
    yield from review2_cases()
