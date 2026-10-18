"""Parity cases for picks.py.

GitHub is never asked: the functions that read it run either against a canned
feed (`"feed": {url: body}`, put in place of ghcache.get_json on the Python
side, as the tests do) or against a copy of this machine's real GitHub cache
with the recorded quota set to zero, so ghcache serves what it has and spends
nothing. Every case that writes picks.json gets its own XDG_STATE_HOME, one
for each side, seeded the same. `now` is always pinned.

`"eligible_stub": true` swaps in the tests' `eligible` (every candidate counts
as reviewed and installable), so shortlist/today can be checked without
store's trust port; the cases without it use the real one.
"""
from __future__ import annotations

import copy
import dataclasses
import json
import math
import os
import shutil
import tempfile
import time
from datetime import date, datetime, timedelta, timezone
from pathlib import Path

import ghcache
import picks
import store
from run import normalise

NOW = 1_800_000_000.0                 # 2027-01-15T08:00:00Z
NOW_REAL = float(int(time.time()))    # for the real cache: pinned once
API = ghcache.API
DAY = 86400


def iso(t: float) -> str:
    return datetime.fromtimestamp(t, timezone.utc).isoformat()


def ago_h(h: float, now: float = NOW) -> str:
    return iso(now - h * 3600)


def P(pid, market="omarchy", stars=50, days_ago=1.0, **kw):
    d = {"id": pid, "name": pid.title(), "stars": stars, "market": market,
         "repo": f"https://github.com/o/{pid}", "installable": True,
         "verified": True, "install_cmd": "x",
         "updated": iso(NOW - days_ago * DAY), "installed": False}
    d.update(kw)
    return d


def plugs(ds):
    return [store.Plugin(**d) for d in ds]


def S(**kw):
    return kw


def sig(d):
    return picks.Signals(**d)


def _stub(p):
    return not p.installed and bool(picks._github(p.repo))


def call(fn, feed=None, token=None, stub=False, env=None, cache=None):
    """Run fn with the same stand-ins the Rust side gets from its args."""
    saved = (ghcache.get_json, ghcache.token, picks.eligible,
             ghcache.CACHE, ghcache.DIR, ghcache.STATE)
    old_env = {k: os.environ.get(k) for k in (env or {})}
    try:
        if feed is not None:
            ghcache.get_json = lambda url, *a, **k: copy.deepcopy(feed.get(url))
        if token is not None:
            ghcache.token = lambda: "x" * 40 if token else ""
        if stub:
            picks.eligible = _stub
        if cache is not None:
            ghcache.CACHE = Path(cache) / "almanac"
            ghcache.DIR = ghcache.CACHE / "github"
            ghcache.STATE = ghcache.CACHE / "github-rate.json"
        for k, v in (env or {}).items():
            os.environ[k] = v
        return fn()
    finally:
        (ghcache.get_json, ghcache.token, picks.eligible,
         ghcache.CACHE, ghcache.DIR, ghcache.STATE) = saved
        for k, v in old_env.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v


def events_url(full, page):
    return f"{API}/repos/{full}/events?per_page=100&page={page}"


def comments_url(full, now):
    since = datetime.fromtimestamp(now - 7 * DAY, timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    return f"{API}/repos/{full}/issues/comments?since={since}&per_page=100"


def release_url(full):
    return f"{API}/repos/{full}/releases/latest"


def ev(kind, h, now=NOW):
    return {"type": kind, "created_at": ago_h(h, now)}


def real_cache() -> str:
    """A copy of this machine's GitHub cache, with no quota left to spend."""
    tmp = tempfile.mkdtemp(prefix="almanac-picks-cache-")
    src = Path.home() / ".cache" / "almanac" / "github"
    dst = Path(tmp) / "almanac"
    dst.mkdir(parents=True)
    if src.is_dir():
        shutil.copytree(src, dst / "github")
    (dst / "github-rate.json").write_text(json.dumps(
        {"remaining": 0, "limit": 5000, "reset": 4102444800, "at": NOW_REAL}))
    return tmp


def state_pair(seed=None, settings=None):
    """(python dir, rust dir), both seeded with the same picks.json / settings."""
    dirs = []
    for _ in range(2):
        d = Path(tempfile.mkdtemp(prefix="almanac-picks-state-"))
        (d / "almanac").mkdir()
        if seed is not None:
            (d / "almanac" / "picks.json").write_text(
                seed if isinstance(seed, str) else json.dumps(seed))
        if settings is not None:
            (d / "almanac" / "settings.json").write_text(json.dumps(settings))
        dirs.append(str(d))
    return dirs


def today_result(plugin_ds, now):
    out = picks.today(plugs(plugin_ds), now)
    p = picks._state_path()
    # the file as written, byte for byte (NaN, big ints and all)
    state = p.read_text() if p.exists() else None
    return {"picks": _finite(normalise(out)), "state": state}


def _finite(v):
    """JSON between the two sides cannot carry NaN, the infinities or a lone
    surrogate: both send a float as its repr, such a str as its json.dumps."""
    if isinstance(v, float) and not math.isfinite(v):
        return repr(v)
    if isinstance(v, str) and any(0xD800 <= ord(c) < 0xE000 for c in v):
        return json.dumps(v)
    if isinstance(v, dict):
        return {k: _finite(x) for k, x in v.items()}
    if isinstance(v, list):
        return [_finite(x) for x in v]
    return v


def catalog_rows():
    rows = []
    for f in ("catalog-artcraft.json", "catalog-omarchy.json"):
        try:
            rows += json.loads((Path.home() / ".cache" / "almanac" / f).read_text())["rows"]
        except (OSError, ValueError, KeyError):
            pass
    fields = {f.name for f in dataclasses.fields(store.Plugin) if f.init}
    return [r for r in rows if isinstance(r, dict) and set(r) <= fields]


def cases():
    # ---- _github -------------------------------------------------------------
    for u in ["https://github.com/o/r", "https://github.com/o/r/", "https://github.com/o/r.git",
              "https://github.com/o/r.git/", "  https://github.com/o/r  \n", "https://github.com/o/r///",
              "https://github.com/o/r/tree/main/x", "https://github.com/o", "https://github.com/o/",
              "https://github.com//r", "https://github.com/", "http://github.com/o/r",
              "https://gitlab.com/o/r", "git@github.com:o/r.git", "", "https://github.com/o/r.git.git",
              "https://github.com/o/r/.git", "https://github.com/ö/ü名前", " https://github.com/o/r ",
              "https://GitHub.com/o/r", "https://github.com/.git"]:
        yield "_github", {"repo_url": u}, lambda u=u: picks._github(u)

    # ---- _compact ------------------------------------------------------------
    for n in [0, 1, -5, -2000, 999, 1000, 1049, 1050, 1149, 1150, 1250, 1350, 2450, 9949, 9950, 9999,
              15300, 999_949, 999_999, 1_000_000, 1_050_000, 1_250_000, 1_350_000, 9_999_999,
              2_500_000_000, 10**12]:
        yield "_compact", {"n": n}, lambda n=n: picks._compact(n)

    # ---- _ts / _scaled ---------------------------------------------------------
    for v in ["", "2027-01-15T07:00:00+00:00", "2027-01-15T07:00:00Z", "2027-01-15T07:00:00.123456+00:00",
              "2027-01-15T00:00:00+02:00", "garbage", "2027-13-01T00:00:00+00:00", "ünï"]:
        yield "_ts", {"value": v}, lambda v=v: picks._ts(v)
    for vals in [[], [0.0, 0.0], [1.0, 2.0, 4.0], [-1.0, -2.0], [0.5], [3.0, 3.0, 0.0]]:
        yield "_scaled", {"values": vals}, lambda vals=vals: picks._scaled(vals)

    # ---- reasons ---------------------------------------------------------------
    sigs = [
        S(), S(stars_today=1), S(stars_today=100, window_hours=2), S(stars_today=1, window_hours=23),
        S(stars_today=2500, window_hours=24), S(downloads=5000, downloads_label="v2.0"),
        S(downloads=40, downloads_today=True), S(downloads=1, downloads_label=""),
        S(downloads=1_250_000, downloads_label="v1.0-ß"),
        S(forks_today=1), S(forks_today=3, window_hours=5),
        S(stars_today=4, downloads=10, forks_today=2),             # forks squeezed out
        S(stars_today=4, forks_today=2, comments=1),
        S(comments=1), S(comments=100, comments_capped=True), S(comments=7),
        S(stars_today=4, downloads=10, forks_today=2, comments=3),
        S(stars_today=-1, forks_today=-1, downloads=-1, comments=-1),
        S(stars_today=0, downloads=0, comments=0, forks_today=0),
    ]
    for stars in [0, 50, 15300, -3]:
        p = P("a", stars=stars)
        for s in sigs:
            yield ("reasons", {"plugin": p, "s": s},
                   lambda p=p, s=s: picks.reasons(store.Plugin(**p), sig(s)))

    # ---- rank ----------------------------------------------------------------
    big = [P(f"app{i}", "artcraft", stars=15000 + i) for i in range(3)]
    small = [P(f"plug{i}", "omarchy", stars=40 + i) for i in range(3)]
    judged = (big + small, [S(stars_today=150, comments=100)] * 3 + [S(stars_today=3, comments=4)] * 3)
    two = ([P("a"), P("b")], [S(stars_today=10), S(stars_today=8)])
    mixed = ([P("x1", "omarchy", stars=10, days_ago=0.5), P("x2", "omarchy", stars=0, days_ago=13),
              P("x3", "artcraft", stars=900, days_ago=20), P("x4", "themes", stars=3, days_ago=-2),
              P("x5", "artcraft", stars=901, days_ago=2, updated="not a date"),
              P("x6", "ünï", stars=7, days_ago=3)],
             [S(stars_today=5, forks_today=1, downloads=100, comments=3),
              S(stars_today=0, downloads=40, downloads_today=True),
              S(stars_today=-1, forks_today=-1, downloads=-1, comments=-1),
              S(stars_today=2),
              S(comments=100, comments_capped=True, downloads=9),
              S()])
    ties = ([P(f"t{i}", ["a", "b"][i % 2], stars=10) for i in range(6)], [S(stars_today=1)] * 6)
    rank_sets = [("judged", judged), ("two", two), ("mixed", mixed), ("ties", ties), ("empty", ([], []))]
    for name, (cands, ss) in rank_sets:
        for recent in [[], ["a"], ["plug0", "app1", "x3"], ["t0", "t1", "nobody"]]:
            yield ("rank", {"plugins": cands, "signals": ss, "now": NOW, "recent_picks": recent},
                   lambda cands=cands, ss=ss, recent=recent:
                   picks.rank(plugs(cands), [sig(s) for s in ss], NOW, set(recent)))

    # ---- choose ----------------------------------------------------------------
    for name, (cands, ss) in rank_sets + [
        ("close-same", ([P("c1", "a"), P("c2", "a"), P("c3", "b", stars=1)],
                        [S(stars_today=10), S(stars_today=9), S()])),
        ("lone", ([P("solo")], [S(stars_today=1)])),
    ]:
        ranked = normalise(picks.rank(plugs(cands), [sig(s) for s in ss], NOW, set()))
        for limit in [2, 1, 3, 0]:
            yield ("choose", {"ranked": ranked, "limit": limit},
                   lambda ranked=ranked, limit=limit: picks.choose(
                       [picks.Pick(store.Plugin(**d["plugin"]), sig(d["signals"]), d["score"],
                                   [tuple(r) for r in d["reasons"]]) for d in ranked], limit))

    # ---- shortlist (the tests' eligible) ---------------------------------------
    pool = ([P(f"app{i}", "artcraft", stars=9000 - i) for i in range(6)]
            + [P(f"plug{i}", "omarchy", stars=9 + i) for i in range(6)]
            + [P("th", "themes", stars=1), P("mine", installed=True), P("norepo", repo="https://gitlab.com/o/x"),
               P("stale", days_ago=14.5), P("edge", days_ago=13.9), P("nodate", updated=""),
               P("ünï", "ünï-market", stars=3)])
    for size in [0, 1, 4, 10, 100]:
        yield ("shortlist", {"plugins": pool, "size": size, "now": NOW, "eligible_stub": True},
               lambda size=size: call(lambda: picks.shortlist(plugs(pool), size, NOW), stub=True))
    yield ("shortlist", {"plugins": [P("old", days_ago=30)], "size": 4, "now": NOW, "eligible_stub": True},
           lambda: call(lambda: picks.shortlist(plugs([P("old", days_ago=30)]), 4, NOW), stub=True))
    yield ("shortlist", {"plugins": [], "size": 4, "now": NOW, "eligible_stub": True},
           lambda: call(lambda: picks.shortlist([], 4, NOW), stub=True))

    # ---- eligible / shortlist with the real eligible (reaches store trust) ----
    elig = [P("plain"), P("off", installable=False), P("nocmd", install_cmd=""),
            P("ai", install_cmd="", install_kind="appimage"), P("theme", install_kind="theme"),
            P("inst", installed=True), P("gl", repo="https://gitlab.com/o/r"),
            P("builtin", built_in=True), P("rev", review_method="maintainer-reviewed"),
            P("snap", coverage="snapshot-verified"), P("unreach", upstream_status="failed"),
            P("unver", verified=False)]
    for p in elig:
        yield "eligible", {"plugin": p}, lambda p=p: picks.eligible(store.Plugin(**p))
    yield ("shortlist", {"plugins": elig, "size": 10, "now": NOW},
           lambda: picks.shortlist(plugs(elig), 10, NOW))

    # ---- activity_today (canned feed) ------------------------------------------
    busy = [ev("WatchEvent", i / 50) for i in range(100)]
    feeds = {
        "busy-1page": ({events_url("o/busy", 1): busy}, 1),
        "busy-3pages": ({events_url("o/busy", 1): busy,
                         events_url("o/busy", 2): [ev("WatchEvent" if i % 3 else "ForkEvent", 2 + i / 10) for i in range(100)],
                         events_url("o/busy", 3): [ev("ForkEvent", 12 + i / 100) for i in range(100)]}, 3),
        "busy-reaches-a-day": ({events_url("o/busy", 1): busy,
                                events_url("o/busy", 2): [ev("WatchEvent", 2 + i / 4) for i in range(100)]}, 3),
        "page2-missing": ({events_url("o/busy", 1): busy}, 3),
        "page2-not-a-list": ({events_url("o/busy", 1): busy, events_url("o/busy", 2): {"message": "x"}}, 3),
        "quiet": ({events_url("o/busy", 1): [ev("WatchEvent", 2), ev("ForkEvent", 30)]}, 3),
        "empty": ({events_url("o/busy", 1): []}, 3),
        "nothing": ({}, 3),
        "dict": ({events_url("o/busy", 1): {"message": "Not Found"}}, 3),
        "odd-rows": ({events_url("o/busy", 1): [{"type": "WatchEvent"}, {"created_at": ago_h(1)},
                                                {"type": "WatchEvent", "created_at": "garbage"},
                                                {"type": "ForkEvent", "created_at": ago_h(23.99)},
                                                {"type": "WatchEvent", "created_at": ago_h(-5)},
                                                {"type": "PushEvent", "created_at": ago_h(1)}]}, 3),
        "zero-pages": ({events_url("o/busy", 1): busy}, 0),
    }
    for name, (feed, pages) in feeds.items():
        yield ("activity_today", {"full": "o/busy", "now": NOW, "pages": pages, "feed": feed},
               lambda feed=feed, pages=pages: call(lambda: picks.activity_today("o/busy", NOW, pages), feed=feed))

    # ---- comments_this_week ---------------------------------------------------
    for n in [0, 1, 5, 99, 100, 150]:
        feed = {comments_url("o/r", NOW): [{"id": i} for i in range(n)]}
        yield ("comments_this_week", {"full": "o/r", "now": NOW, "feed": feed},
               lambda feed=feed: call(lambda: picks.comments_this_week("o/r", NOW), feed=feed))
    for feed in [{}, {comments_url("o/r", NOW): {"message": "x"}}, {comments_url("o/r", NOW): None}]:
        yield ("comments_this_week", {"full": "o/r", "now": NOW, "feed": feed},
               lambda feed=feed: call(lambda: picks.comments_this_week("o/r", NOW), feed=feed))
    odd_now = 1_800_000_000.75
    feed = {comments_url("o/r", odd_now): [{}] * 3}
    yield ("comments_this_week", {"full": "o/r", "now": odd_now, "feed": feed},
           lambda feed=feed: call(lambda: picks.comments_this_week("o/r", odd_now), feed=feed))

    # ---- release_downloads ----------------------------------------------------
    rels = [
        {"tag_name": "v1.4", "assets": [{"download_count": 10}, {"download_count": 32}]},
        {"tag_name": "v1.4", "assets": []},
        {"tag_name": "v1.4"},
        {"tag_name": None, "assets": [{"download_count": None}, {}]},
        {"assets": [{"download_count": "12"}, {"download_count": 3.7}, {"download_count": 0}]},
        {"tag_name": "ünï-β", "assets": [{"download_count": 1}]},
        ["not", "a", "dict"],
        None,
    ]
    for rel in rels:
        feed = {release_url("o/r"): rel}
        yield ("release_downloads", {"full": "o/r", "feed": feed},
               lambda feed=feed: call(lambda: picks.release_downloads("o/r"), feed=feed))

    # ---- measure --------------------------------------------------------------
    p = P("m")
    full = "o/m"
    today_key = date.fromtimestamp(NOW).isoformat()
    yday = (date.fromtimestamp(NOW) - timedelta(days=1)).isoformat()
    older = (date.fromtimestamp(NOW) - timedelta(days=3)).isoformat()
    base_feed = {
        events_url(full, 1): [ev("WatchEvent", 1), ev("WatchEvent", 3), ev("ForkEvent", 5), ev("WatchEvent", 30)],
        comments_url(full, NOW): [{}] * 7,
        release_url(full): {"tag_name": "v2.0", "assets": [{"download_count": 5000}, {"download_count": 250}]},
    }
    histories = [
        {},
        {yday: {"m": [5000, "v2.0"]}},
        {yday: {"m": [6000, "v2.0"]}},               # went down: a total, not today's
        {yday: {"m": [5000, "v1.9"]}},               # another release
        {yday: {"m": [5250, "v2.0"]}},               # nothing new: 0 since yesterday
        {yday: {"other": [1, "v1"]}},
        {older: {"m": [100, "v2.0"]}, yday: {}},     # the latest past day decides
        {older: {"m": [100, "v2.0"]}},
        {today_key: {"m": [10, "v2.0"]}},            # today is not "yesterday"
        {"2099-01-01": {"m": [1, "v2.0"]}, older: {"m": [5200, "v2.0"]}},
    ]
    for hist in histories:
        for token in [False, True]:
            yield ("measure", {"plugin": p, "now": NOW, "history": hist, "feed": base_feed, "token": token},
                   lambda hist=hist, token=token: call(lambda: picks.measure(store.Plugin(**p), NOW, hist),
                                                       feed=base_feed, token=token))
    for name, feed in [("nothing", {}), ("no-release", {k: v for k, v in base_feed.items() if "releases" not in k})]:
        yield ("measure", {"plugin": p, "now": NOW, "history": {yday: {"m": [1, "v2.0"]}}, "feed": feed, "token": False},
               lambda feed=feed: call(lambda: picks.measure(store.Plugin(**p), NOW, {yday: {"m": [1, "v2.0"]}}),
                                      feed=feed, token=False))
    np = P("norepo", repo="https://gitlab.com/x/y")
    yield ("measure", {"plugin": np, "now": NOW, "history": {}, "feed": {}, "token": False},
           lambda: call(lambda: picks.measure(store.Plugin(**np), NOW, {}), feed={}, token=False))

    # ---- today (canned feed, the tests' eligible, own state dirs) --------------
    tp = [P("a"), P("b"), P("c", "artcraft", stars=900), P("d", "artcraft", stars=5, days_ago=3),
          P("e", "themes", stars=0, days_ago=0.2)]
    tfeed = {}
    for i, x in enumerate(tp):
        f = f"o/{x['id']}"
        tfeed[events_url(f, 1)] = [ev("WatchEvent", 1 + j) for j in range([1, 9, 4, 0, 2][i])]
        tfeed[comments_url(f, NOW)] = [{}] * [0, 3, 100, 1, 0][i]
        if i in (0, 2, 3):
            tfeed[release_url(f)] = {"tag_name": f"v{i}", "assets": [{"download_count": 100 * (i + 1)}]}
    seeds = {
        "fresh": None,
        "broken": "{not json",
        "a-list": "[1, 2]",
        "kept": {"picked": {today_key: ["d", "a"]}},
        "kept-unknown": {"picked": {today_key: ["gone"]}},
        "kept-empty": {"picked": {today_key: []}},
        "recent": {"picked": {yday: ["b", "c"], older: ["a"], "2020-01-01": ["d"], "junk": ["e"]}},
        "history": {"downloads": {yday: {"a": [50, "v0"], "c": [400, "v2"], "d": [1, "other"]},
                                  "2020-01-01": {"a": [1, "v0"]}, "junk": {}},
                    "picked": {}, "extra": {"kept": True}},
        "today-snap": {"downloads": {today_key: {"a": [7, "v0"]}}},
        "snap-1-item": {"downloads": {yday: {"a": [50]}}},
        "odd-shapes": {"downloads": {yday: {"a": "nope", "c": "zz", "d": []}}, "picked": {yday: "bc"}},
        "odd-kept": {"picked": {today_key: "ab", yday: {"c": 1}}},
    }
    for name, seed in seeds.items():
        for tok in [False, True]:
            py, rs = state_pair(seed)
            yield ("today", {"plugins": tp, "now": NOW, "feed": tfeed, "token": tok, "eligible_stub": True,
                             "env": {"XDG_STATE_HOME": rs}},
                   lambda py=py, tok=tok: call(lambda: today_result(tp, NOW), feed=tfeed, token=tok, stub=True,
                                               env={"XDG_STATE_HOME": py}))
    # Later the same day: the morning's picks hold.
    for later in [3600.0, 20 * 3600.0]:
        py, rs = state_pair({"picked": {today_key: ["e"]}})
        yield ("today", {"plugins": tp, "now": NOW + later, "feed": {}, "token": False, "eligible_stub": True,
                         "env": {"XDG_STATE_HOME": rs}},
               lambda py=py, later=later: call(lambda: today_result(tp, NOW + later), feed={}, token=False,
                                               stub=True, env={"XDG_STATE_HOME": py}))
    py, rs = state_pair(None, settings={"net_daily_picks": False})
    yield ("today", {"plugins": tp, "now": NOW, "feed": tfeed, "token": False, "eligible_stub": True,
                     "env": {"XDG_STATE_HOME": rs}},
           lambda py=py: call(lambda: today_result(tp, NOW), feed=tfeed, token=False, stub=True,
                              env={"XDG_STATE_HOME": py}))
    py, rs = state_pair(None)
    nobody = [P("x", installed=True), P("y", days_ago=40)]
    yield ("today", {"plugins": nobody, "now": NOW, "feed": tfeed, "token": False, "eligible_stub": True,
                     "env": {"XDG_STATE_HOME": rs}},
           lambda py=py: call(lambda: today_result(nobody, NOW), feed=tfeed, token=False, stub=True,
                              env={"XDG_STATE_HOME": py}))

    # ---- the real machine: this GitHub cache, nothing spent -------------------
    cache = real_cache()
    cenv = {"XDG_CACHE_HOME": cache}
    real = ["storytold/filmcraft", "storytold/photocraft", "storytold/wordcraft", "storytold/cadcraft",
            "mryll/claudebar", "mtolhuys/fathom", "tornikegomareli/omarchy-spaces",
            "fasi96/omarchy-liquid-glass", "fross100/omaplug", "nobody/not-cached"]
    for full in real:
        for pages in [1, 3]:
            yield ("activity_today", {"full": full, "now": NOW_REAL, "pages": pages, "env": cenv},
                   lambda full=full, pages=pages: call(lambda: picks.activity_today(full, NOW_REAL, pages),
                                                       cache=cache, env=cenv))
        yield ("comments_this_week", {"full": full, "now": NOW_REAL, "env": cenv},
               lambda full=full: call(lambda: picks.comments_this_week(full, NOW_REAL), cache=cache, env=cenv))
        yield ("release_downloads", {"full": full, "env": cenv},
               lambda full=full: call(lambda: picks.release_downloads(full), cache=cache, env=cenv))
    rows = catalog_rows()
    by_id = {r.get("id"): r for r in rows}
    real_hist = {}
    try:
        real_hist = json.loads((Path.home() / ".local/state/almanac/picks.json").read_text()).get("downloads", {})
    except (OSError, ValueError, AttributeError):
        pass
    for pid in ["filmcraft", "photocraft", "io.github.mtolhuys.fathom", "mryll.claudebar"]:
        if pid in by_id:
            row = by_id[pid]
            yield ("measure", {"plugin": row, "now": NOW_REAL, "history": real_hist, "env": cenv},
                   lambda row=row: call(lambda: picks.measure(store.Plugin(**row), NOW_REAL, real_hist),
                                        cache=cache, env=cenv))
    if rows:
        yield ("shortlist", {"plugins": rows, "size": 10, "now": NOW_REAL},
               lambda: picks.shortlist(plugs(rows), 10, NOW_REAL))
        py, rs = state_pair(None)
        yield ("today", {"plugins": rows, "now": NOW_REAL, "env": {**cenv, "XDG_STATE_HOME": rs}},
               lambda py=py: call(lambda: today_result(rows, NOW_REAL), cache=cache,
                                  env={**cenv, "XDG_STATE_HOME": py}))

    # ---- review round 1: edges where the port may part ways ---------------------
    # _ts: Python's fromisoformat neither trims nor takes a leap second, but it
    # takes every other ISO 8601 shape (basic, week dates, comma, hour only).
    for v in [" 2027-01-15T07:00:00+00:00", "2027-01-15T07:00:00+00:00 ", "2027-01-15T23:59:60+00:00",
              "2027-01-15T07", "2027-01-15T07:00:00+0000", "20270115T070000Z", "2027-W03-5",
              "2027-01-15x07:00:00+00:00", "2027-01-15T07:00:00,5+00:00",
              "2027-01-15T07:00:00.1234567+00:00", "2027-01-15T24:00:00+00:00", "2027-01-15T07:00:00+05",
              "2027-01-15t07:00:00z", "2027-W02-4T00:00:00+00:00", "2027-01-15T07:00:00.5Z",
              "2027-01-15"]:
        yield "_ts", {"value": v}, lambda v=v: picks._ts(v)
    # _github: str.strip() also strips the ASCII separators \x1c-\x1f.
    for u in ["\x1chttps://github.com/o/r", "https://github.com/o/r\x1f", " https://github.com/o/r　"]:
        yield "_github", {"repo_url": u}, lambda u=u: picks._github(u)
    # comments_this_week: fromtimestamp rounds to the microsecond (half-even),
    # so .9999998 of a second is the next second in the URL.
    for odd in [1_800_000_000.9999998, 1_800_000_000.9999995]:
        feed = {comments_url("o/r", odd): [{}] * 2}
        yield ("comments_this_week", {"full": "o/r", "now": odd, "feed": feed},
               lambda feed=feed, odd=odd: call(lambda: picks.comments_this_week("o/r", odd), feed=feed))
    # a now past datetime's range: Python raises
    yield ("comments_this_week", {"full": "o/r", "now": 1e20, "feed": {}},
           lambda: call(lambda: picks.comments_this_week("o/r", 1e20), feed={}))
    # release_downloads: int() of odd counts, assets that are not a list of dicts
    for rel in [
        {"tag_name": "v1", "assets": [{"download_count": "1_000"}]},
        {"tag_name": "v1", "assets": [{"download_count": "١٢"}]},
        {"tag_name": "v1", "assets": [{"download_count": "12.5"}]},
        {"tag_name": "v1", "assets": [{"download_count": 1e20}]},
        {"tag_name": "v1", "assets": [{"download_count": [1]}]},
        {"tag_name": "v1", "assets": {"x": 1}},
        {"tag_name": "v1", "assets": "abc"},
        {"tag_name": "v1", "assets": [5]},
        {"tag_name": 5, "assets": [{"download_count": 1}]},
        {"tag_name": ["x"], "assets": [{"download_count": 1}]},
    ]:
        feed = {release_url("o/r"): rel}
        yield ("release_downloads", {"full": "o/r", "feed": feed},
               lambda feed=feed: call(lambda: picks.release_downloads("o/r"), feed=feed))
    # activity_today: rows that are not dicts, created_at that is not a string
    for erows in [["x"], [5], [{"type": "WatchEvent", "created_at": 1800000000}],
                  [{"type": "WatchEvent", "created_at": None}]]:
        feed = {events_url("o/busy", 1): erows}
        yield ("activity_today", {"full": "o/busy", "now": NOW, "pages": 3, "feed": feed},
               lambda feed=feed: call(lambda: picks.activity_today("o/busy", NOW, 3), feed=feed))
    # rank: a negative star count is a math domain error in log1p
    for st in [-3, -1]:
        cands = [P("n1", stars=st), P("n2", stars=5)]
        ss = [S(stars_today=1), S()]
        yield ("rank", {"plugins": cands, "signals": ss, "now": NOW, "recent_picks": []},
               lambda cands=cands, ss=ss: picks.rank(plugs(cands), [sig(s) for s in ss], NOW, set()))
    # today: snapshots and past picks of shapes Almanac does not write, a picks.json
    # Python's json reads and serde does not, day keys only one side parses
    r1 = {
        "snap-3-items": {"downloads": {yday: {"a": [50, "v0", "x"]}}},
        "snap-float": {"downloads": {yday: {"a": [50.0, "v0"]}}},
        "snap-half": {"downloads": {yday: {"a": [50.5, "v0"]}}},
        "snap-bool": {"downloads": {yday: {"a": [True, "v0"]}}},
        "snap-str-count": {"downloads": {yday: {"a": ["50", "v0"]}}},
        "snap-day-list": {"downloads": {older: {"a": [50, "v0"]}, yday: []}},
        "snap-day-null": {"downloads": {older: {"a": [50, "v0"]}, yday: None}},
        "downloads-list": {"downloads": []},
        "downloads-null": {"downloads": None},
        "today-snap-list": {"downloads": {today_key: []}},
        "picked-list": {"picked": []},
        "picked-null": {"picked": None},
        "picked-number": {"picked": {yday: 5}},
        "kept-nested": {"picked": {today_key: [["a"]]}},
        "kept-number": {"picked": {today_key: 7}},
        "key-space": {"picked": {" " + yday: ["b", "c"]}, "downloads": {" " + yday: {"a": [50, "v0"]}}},
        "key-week": {"picked": {"2027-W02-4": ["b", "c"]}, "downloads": {"2027-W02-4": {"a": [50, "v0"]}}},
        "key-basic": {"picked": {yday.replace("-", ""): ["b", "c"]}},
        "nan": '{"picked": {"%s": ["d", "a"]}, "x": NaN}' % today_key,
        "infinity": '{"picked": {"%s": ["d", "a"]}, "x": -Infinity}' % today_key,
        "bigint": '{"extra": 123456789012345678901234567890, "f": [1E5, -0, -0.0, 1e16, 1.5, 1e-7], '
                  '"picked": {"%s": ["d", "a"]}}' % today_key,
        "nan-snap": '{"downloads": {"%s": {"a": [NaN, "v0"], "c": [-Infinity, "v2"]}}}' % yday,
        "snap-big": '{"downloads": {"%s": {"a": [123456789012345678901234567890, "v0"], "c": [1e400, "v2"]}}}' % yday,
        "snap-dict": {"downloads": {yday: {"a": {"x": 1}}}},
        "snap-str2": {"downloads": {yday: {"a": "v0v0"}}},
        "history-str": {"downloads": "1"},
        "unicode-keys": r'{"downloads": {"%s": {"a": [1, "v\u00e9\ud83d\ude00"]}}, "\u00fc": "\u2028"}' % yday,
        "deep": '{"x": ' + "[" * 900 + "]" * 900 + "}",
        "dup-keys": '{"picked": {"%s": ["d"]}, "picked": {"%s": ["a", "c"]}}' % (today_key, today_key),
    }
    for name, seed in r1.items():
        py, rs = state_pair(seed)
        yield ("today", {"plugins": tp, "now": NOW, "feed": tfeed, "token": False, "eligible_stub": True,
                         "env": {"XDG_STATE_HOME": rs}},
               lambda py=py: call(lambda: today_result(tp, NOW), feed=tfeed, token=False, stub=True,
                                  env={"XDG_STATE_HOME": py}))
    # settings of the wrong type fall back to the default (on) in Python
    for val in [0, 1, "no", "", None, [], {}]:
        py, rs = state_pair(None, settings={"net_daily_picks": val})
        yield ("today", {"plugins": tp, "now": NOW, "feed": tfeed, "token": False, "eligible_stub": True,
                         "env": {"XDG_STATE_HOME": rs}},
               lambda py=py: call(lambda: today_result(tp, NOW), feed=tfeed, token=False, stub=True,
                                  env={"XDG_STATE_HOME": py}))
    # a number serde cannot hold: Python reads it as inf and keeps the file
    py, rs = state_pair('{"picked": {"%s": ["d", "a"]}, "x": 1e400}' % today_key)
    yield ("today", {"plugins": tp, "now": NOW, "feed": tfeed, "token": False, "eligible_stub": True,
                     "env": {"XDG_STATE_HOME": rs}},
           lambda py=py: call(lambda: today_result(tp, NOW), feed=tfeed, token=False, stub=True,
                              env={"XDG_STATE_HOME": py}))
    # rank: fewer signals than candidates is an IndexError in Python
    for cands, ss in [([P("a"), P("b")], [S(stars_today=1)]), ([P("a")], [S(stars_today=1), S()])]:
        yield ("rank", {"plugins": cands, "signals": ss, "now": NOW, "recent_picks": []},
               lambda cands=cands, ss=ss: picks.rank(plugs(cands), [sig(s) for s in ss], NOW, set()))
    # review round 2: a tag that is not a string, and a release total past
    # i64, carried through measure, the reasons and the snapshot on disk
    for name, rel, hist in [
        ("tag-int", {"tag_name": 5, "assets": [{"download_count": 120}]}, {yday: {"a": [100, 5]}}),
        ("tag-int-vs-float", {"tag_name": 5, "assets": [{"download_count": 120}]}, {yday: {"a": [100, 5.0]}}),
        ("tag-list", {"tag_name": ["x"], "assets": [{"download_count": 120}]}, {yday: {"a": [100, "x"]}}),
        ("tag-true", {"tag_name": True, "assets": [{"download_count": 120}]}, {yday: {"a": [100, 1]}}),
        ("huge", {"tag_name": "v0", "assets": [{"download_count": 1e20}, {"download_count": "1_000"}]},
         {yday: {"a": [50, "v0"]}}),
    ]:
        feed = dict(tfeed)
        feed[release_url("o/a")] = rel
        py, rs = state_pair({"downloads": hist})
        yield ("today", {"plugins": tp, "now": NOW, "feed": feed, "token": False, "eligible_stub": True,
                         "env": {"XDG_STATE_HOME": rs}},
               lambda py=py, feed=feed: call(lambda: today_result(tp, NOW), feed=feed, token=False, stub=True,
                                             env={"XDG_STATE_HOME": py}))
    for rel in [{"tag_name": 5, "assets": [{"download_count": 120}]},
                {"tag_name": {"a": [1, None]}, "assets": [{"download_count": 3}]},
                {"tag_name": "v0", "assets": [{"download_count": 1e20}]}]:
        feed = dict(base_feed)
        feed[release_url("o/m")] = rel
        yield ("measure", {"plugin": p, "now": NOW, "history": {}, "feed": feed, "token": False},
               lambda feed=feed: call(lambda: picks.measure(store.Plugin(**p), NOW, {}), feed=feed, token=False))

    # ---- adversarial review round 2 -------------------------------------------
    # NaN scores (a snapshot of -Infinity makes a download count of inf, and
    # inf/inf in _scaled is NaN): list.sort's binary insertion and the Rust
    # stable sort's linear insertion order NaN differently, so the top pick
    # itself can differ.
    spec = [("p0", "a", 54, "2027-01-08T01:47:43.352350+00:00", 4, 1, None),
            ("p1", "b", 70, "2027-01-13T00:03:35.034977+00:00", 1, 5, 236),
            ("p2", "a", 53, "2027-01-06T22:58:52.629028+00:00", 4, 5, 323),
            ("p3", "b", 15, "2027-01-07T21:38:41.285395+00:00", 5, 5, 496),
            ("p4", "a", 38, "2027-01-12T11:48:06.307176+00:00", 3, 4, 437),
            ("p5", "a", 95, "2027-01-07T08:37:22.730113+00:00", 3, 5, 281)]
    nan_plugins, nan_feed = [], {}
    for pid, market, stars, upd, nev, ncom, dl in spec:
        nan_plugins.append(P(pid, market, stars=stars, updated=upd))
        f = f"o/{pid}"
        nan_feed[events_url(f, 1)] = [{"type": "WatchEvent", "created_at": "2027-01-15T07:00:00+00:00"}] * nev
        nan_feed[comments_url(f, NOW)] = [{}] * ncom
        if dl is not None:
            nan_feed[release_url(f)] = {"tag_name": "v", "assets": [{"download_count": dl}]}
    py, rs = state_pair('{"downloads": {"%s": {"p3": [-Infinity, "v"], "p4": [-Infinity, "v"]}}}' % yday)
    yield ("today", {"plugins": nan_plugins, "now": NOW, "feed": nan_feed, "token": True, "eligible_stub": True,
                     "env": {"XDG_STATE_HOME": rs}},
           lambda py=py: call(lambda: today_result(nan_plugins, NOW), feed=nan_feed, token=True, stub=True,
                              env={"XDG_STATE_HOME": py}))
    # picks.json shapes json.loads takes and the hand-written parser does not
    # (or reads differently): nesting past 1,000 (3.14 goes to ~50,000), a
    # negative int of exactly 4,300 digits (the limit counts digits, not the
    # sign), and lone surrogates, which Python keeps and writes back as \udXXX.
    r2 = {
        "deep-1500": '{"x": ' + "[" * 1500 + "]" * 1500 + "}",
        "deep-40000": '{"x": ' + "[" * 40000 + "]" * 40000 + "}",
        "deep-70000": '{"x": ' + "[" * 70000 + "]" * 70000 + "}",
        "neg-4300-digits": '{"x": -' + "1" * 4300 + "}",
        "lone-high": r'{"x": "\ud800A"}',
        "lone-low": r'{"x": "\udc00"}',
        "pair-then-lone": r'{"x": "😀\ud83d"}',
    }
    for name, seed in r2.items():
        py, rs = state_pair(seed)
        yield ("today", {"plugins": tp, "now": NOW, "feed": tfeed, "token": False, "eligible_stub": True,
                         "env": {"XDG_STATE_HOME": rs}},
               lambda py=py: call(lambda: today_result(tp, NOW), feed=tfeed, token=False, stub=True,
                                  env={"XDG_STATE_HOME": py}))
    # The real ghcache path: a cached body is read by ghcache as Python json
    # (NaN, ints past u64, lone surrogates) but reaches picks as a serde
    # Value, and get_json's own exceptions (an entry that is not a dict, an
    # "at" that is not a number) are swallowed into None on the Rust side.
    import hashlib

    def crafted_cache(entries: dict) -> str:
        """A GitHub cache holding these raw entry texts, no quota left."""
        tmp = tempfile.mkdtemp(prefix="almanac-picks-cache-")
        d = Path(tmp) / "almanac" / "github"
        d.mkdir(parents=True)
        for url, text in entries.items():
            (d / f"{hashlib.sha1(url.encode()).hexdigest()}.json").write_text(text)
        (Path(tmp) / "almanac" / "github-rate.json").write_text(json.dumps(
            {"remaining": 0, "limit": 5000, "reset": 4102444800, "at": NOW_REAL}))
        return tmp

    def entry(body_text: str) -> str:
        return '{"at": 0, "body": %s, "etag": "", "modified": ""}' % body_text

    rel_bodies = {
        "nan-count": '{"tag_name": "v1", "assets": [{"download_count": NaN}]}',
        "bigint-count": '{"tag_name": "v1", "assets": [{"download_count": 123456789012345678901234567891}]}',
        "lone-surrogate-tag": r'{"tag_name": "v\ud800", "assets": [{"download_count": 3}]}',
        "inf-count": '{"tag_name": "v1", "assets": [{"download_count": Infinity}]}',
    }
    for name, body in rel_bodies.items():
        c = crafted_cache({release_url("o/r"): entry(body)})
        cenv2 = {"XDG_CACHE_HOME": c}
        yield ("release_downloads", {"full": "o/r", "env": cenv2},
               lambda c=c, cenv2=cenv2: _finite(normalise(
                   call(lambda: picks.release_downloads("o/r"), cache=c, env=cenv2))))
    ev_entries = {
        "entry-not-a-dict": "[1]",
        "entry-at-a-string": '{"at": "x", "body": [], "etag": ""}',
        "created-at-nan": entry('[{"type": "WatchEvent", "created_at": NaN}]'),
    }
    for name, text in ev_entries.items():
        c = crafted_cache({events_url("o/busy", 1): text})
        cenv2 = {"XDG_CACHE_HOME": c}
        yield ("activity_today", {"full": "o/busy", "now": NOW, "pages": 1, "env": cenv2},
               lambda c=c, cenv2=cenv2: call(lambda: picks.activity_today("o/busy", NOW, 1), cache=c, env=cenv2))
    # an int past u64 in a cached release: the float on the Rust side is off
    # by billions, so "1 download since yesterday" becomes a release total
    # (the history goes through picks.json, which Rust reads with its own
    # parser, so only the cached body is lossy)
    c = crafted_cache({release_url("o/a"): entry(
        '{"tag_name": "v1", "assets": [{"download_count": 123456789012345678901234567891}]}')})
    py, rs = state_pair('{"downloads": {"%s": {"a": [123456789012345678901234567890, "v1"]}}}' % yday)
    yield ("today", {"plugins": tp, "now": NOW, "token": False, "eligible_stub": True,
                     "env": {"XDG_CACHE_HOME": c, "XDG_STATE_HOME": rs}},
           lambda py=py, c=c: call(lambda: today_result(tp, NOW), token=False, stub=True, cache=c,
                                   env={"XDG_CACHE_HOME": c, "XDG_STATE_HOME": py}))
    # sum() of floats is compensated (Neumaier, 3.12+), the Rust fold is not:
    # 0.3+0.15+0.1+0.1 is exactly 0.65 in Python and 0.6499999999999999 in a
    # plain left fold, so a tie Python breaks by candidate order goes the
    # other way, and the top pick changes.
    tie = ([P("ta", "a", stars=10), P("tb", "b", stars=0)],
           [S(stars_today=1, comments=1), S(stars_today=1, downloads=5)])
    yield ("rank", {"plugins": tie[0], "signals": tie[1], "now": NOW, "recent_picks": []},
           lambda: picks.rank(plugs(tie[0]), [sig(s) for s in tie[1]], NOW, set()))
