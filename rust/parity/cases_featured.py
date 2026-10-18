"""Parity cases for featured.py: every shelf, with fixed listings whose dates
are pinned relative to now (days away from any cutoff, so the two sides
calling time.time() a moment apart cannot disagree)."""
from datetime import datetime, timedelta, timezone

import featured
import inventory
import maintenance
import store


def ago(days: float) -> str:
    return (datetime.now(timezone.utc) - timedelta(days=days)).strftime("%Y-%m-%dT%H:%M:%SZ")


def P(**kw):
    d = {"id": kw.get("name", "x"), "name": "x", "installable": True}
    d.update(kw)
    return d


def apps(ds):
    return [inventory.App(**d) for d in ds]


def finds(ds):
    return [maintenance.Finding(**d) for d in ds]


def plugs(ds):
    return [store.Plugin(**d) for d in ds]


APPS = [
    {"name": "firefox", "version": "1.0", "source": "pacman", "update": "1.1",
     "extra": {"critical": "security"}},
    {"name": "openssl", "version": "3.0", "source": "pacman", "update": "",
     "extra": {"critical": "security"}},
    {"name": "oldtool", "version": "0.1", "source": "aur",
     "extra": {"stale": "Not updated upstream in 3 years"}},
    {"name": "numstale", "version": "0.1", "source": "aur", "extra": {"stale": 42}},
    {"name": "boolstale", "version": "0.1", "source": "aur", "extra": {"stale": True}},
    {"name": "falsy", "version": "0.1", "source": "aur", "extra": {"stale": ""}},
    {"name": "zero", "version": "0.1", "source": "aur", "extra": {"stale": 0}},
    {"name": "stale-but-updating", "version": "1", "source": "aur", "update": "2",
     "extra": {"stale": "old"}},
    {"name": "ünïcødé 名前", "version": "β", "source": "flatpak", "update": "γ",
     "extra": {"critical": "security"}},
    {"name": "other-critical", "version": "1", "source": "pacman", "update": "2",
     "extra": {"critical": "kernel"}},
]

FINDINGS = [
    {"id": "svc", "title": "Failed service", "detail": "foo.service failed",
     "severity": "crit", "fix_label": "Restart", "fix": ["systemctl", "restart", "foo"]},
    {"id": "cache", "title": "Cache", "detail": "2 GB", "severity": "warn",
     "fix_label": "Clean", "fix": None},
    {"id": "orph", "title": "Orphans", "detail": "3", "severity": "warn",
     "fix_label": "Remove", "fix": ["pacman", "-Rns"]},
    {"id": "info", "title": "FYI", "detail": "", "severity": "info"},
    {"id": "okish", "title": "Fine", "detail": "", "severity": "ok", "fix": ["true"]},
    {"id": "disk", "title": "Disk full", "detail": "95%", "severity": "crit",
     "fix_label": "Clean", "fix": ["x"]},
    {"id": "disk", "title": "Disk getting full", "detail": "85%", "severity": "warn"},
    {"id": "ü", "title": "Ünïcødé ⚠", "detail": "名前", "severity": "warn"},
]

MANY_FINDINGS = [{"id": f"f{i}", "title": f"T{i}", "detail": "d",
                  "severity": "warn" if i % 2 else "crit"} for i in range(12)]

# Plugins that never reach `trust` (filtered earlier by date, installable or
# installed) -- these do not depend on store's trust port.
UNTRUSTED_PATH = [
    P(name="old", added=ago(60), updated=ago(60), stars=50, market="omarchy"),
    P(name="noinstall", added=ago(2), updated=ago(2), stars=50, installable=False),
    P(name="mine", added=ago(2), updated=ago(2), stars=50, installed=True, tags=["bar"]),
    P(name="nodate", added="", updated="", stars=5),
    P(name="baddate", added="yesterday", updated="not a date", stars=5),
]

# Plugins that do reach `trust` (blocked until store::Plugin::trust is ported).
FULL = [
    P(name=f"p{i}", added=ago(i + 1), updated=ago(i % 5 + 0.5), stars=100 - i * 7,
      market=["omarchy", "artcraft"][i % 2], tags=[["bar", "theme"], ["bar"], ["clock"]][i % 3],
      built_in=(i % 4 == 0),
      review_method=("maintainer-reviewed" if i % 4 == 1 else ""),
      coverage=("snapshot-verified" if i % 4 == 2 else ""),
      description=("" if i % 3 == 0 else f"desc {i} ✓"),
      author=("au" if i % 2 else ""), kind="plugin",
      installed=(i in (1, 5)))
    for i in range(20)
] + [
    P(name="dup-tags", added=ago(1), updated=ago(1), stars=10, tags=["bar", "bar"],
      built_in=True, market="omarchy"),
    P(name="ties-a", added=ago(3), updated=ago(3), stars=40, tags=["clock", "bar"],
      coverage="snapshot-verified", market="omarchy", description="Ünïcødé ★"),
    P(name="ties-b", added=ago(3), updated=ago(3), stars=40, tags=["bar", "clock"],
      coverage="snapshot-verified", market="omarchy"),
    P(name="few-stars", added=ago(1), updated=ago(1), stars=2, tags=["bar"],
      coverage="snapshot-verified", market="omarchy"),
    P(name="unreach", added=ago(1), updated=ago(1), stars=99, tags=["bar"],
      upstream_status="failed", market="x"),
]

PLAIN = [{"name": f"q{i}", "market": ["a", "b", "a", "a", "a", "c"][i % 6]} for i in range(15)]


def cases():
    yield "_ts", {"value": ""}, lambda: featured._ts("")
    yield "_ts", {"value": "2025-01-02T03:04:05Z"}, lambda: featured._ts("2025-01-02T03:04:05Z")
    yield "_ts", {"value": "2025-01-02T03:04:05+02:00"}, lambda: featured._ts("2025-01-02T03:04:05+02:00")
    yield "_ts", {"value": "garbage"}, lambda: featured._ts("garbage")
    yield "_ts", {"value": "2025-13-40"}, lambda: featured._ts("2025-13-40")

    for limit, per in [(8, 3), (2, 3), (20, 1), (20, 100), (0, 3)]:
        yield ("_diversify", {"plugins": [P(**d) for d in PLAIN], "limit": limit, "per_market": per},
               lambda limit=limit, per=per: featured._diversify(plugs([P(**d) for d in PLAIN]), limit, per))
    yield "_diversify", {"plugins": [], "limit": 8, "per_market": 3}, lambda: featured._diversify([], 8, 3)

    yield "needs_you", {"apps": [], "findings": []}, lambda: featured.needs_you([], [])
    yield "needs_you", {"apps": APPS, "findings": []}, lambda: featured.needs_you(apps(APPS), [])
    yield "needs_you", {"apps": [], "findings": FINDINGS}, lambda: featured.needs_you([], finds(FINDINGS))
    yield ("needs_you", {"apps": APPS, "findings": FINDINGS},
           lambda: featured.needs_you(apps(APPS), finds(FINDINGS)))
    yield ("needs_you", {"apps": [], "findings": MANY_FINDINGS},
           lambda: featured.needs_you([], finds(MANY_FINDINGS)))

    yield "build", {"apps": [], "findings": [], "plugins": []}, lambda: featured.build([], [], [])
    yield ("build", {"apps": APPS, "findings": FINDINGS, "plugins": []},
           lambda: featured.build(apps(APPS), finds(FINDINGS), []))

    # because_you_run's early returns.
    yield "because_you_run", {"plugins": [], "limit": 8}, lambda: featured.because_you_run([], 8)
    noinst = [P(name="a", tags=["x"]), P(name="b")]
    yield "because_you_run", {"plugins": noinst, "limit": 8}, lambda: featured.because_you_run(plugs(noinst), 8)
    notags = [P(name="a", installed=True), P(name="b", tags=["x"])]
    yield "because_you_run", {"plugins": notags, "limit": 8}, lambda: featured.because_you_run(plugs(notags), 8)

    for name, ds in [("untrusted", UNTRUSTED_PATH), ("empty", [])]:
        yield "new_listings", {"plugins": ds, "days": 21, "limit": 8}, lambda ds=ds: featured.new_listings(plugs(ds), 21, 8)
        yield "trending", {"plugins": ds, "days": 7, "limit": 8}, lambda ds=ds: featured.trending(plugs(ds), 7, 8)

    # Below here every case reaches store.Plugin.trust / .subtitle.
    for days, limit in [(21, 8), (5, 3), (365, 20)]:
        yield ("new_listings", {"plugins": FULL, "days": days, "limit": limit},
               lambda days=days, limit=limit: featured.new_listings(plugs(FULL), days, limit))
        yield ("trending", {"plugins": FULL, "days": days, "limit": limit},
               lambda days=days, limit=limit: featured.trending(plugs(FULL), days, limit))
    for limit in (8, 2):
        yield ("because_you_run", {"plugins": FULL, "limit": limit},
               lambda limit=limit: featured.because_you_run(plugs(FULL), limit))
    yield ("build", {"apps": APPS, "findings": FINDINGS, "plugins": FULL},
           lambda: featured.build(apps(APPS), finds(FINDINGS), plugs(FULL)))
    mixed = FULL + UNTRUSTED_PATH
    yield ("build", {"apps": [], "findings": [], "plugins": mixed},
           lambda: featured.build([], [], plugs(mixed)))


# ---- review round 1: edge cases ------------------------------------------

TS_EDGE = [
    " 2025-01-02T03:04:05Z", "2025-01-02T03:04:05Z ", "\t2025-01-02",
    "2025-01-02T03:04:05+0200", "2025-01-02T03:04:05+02", "20250102", "20250102T030405Z",
    "2025-01-02T03", "2025-01-02T03Z", "2025-01-02T0304", "2025-01-02T03:04:05,5Z",
    "2025-01-02T23:59:60Z", "2025-01-02t03:04:05Z", "2025-01-02 03:04:05Z",
    "2025-01-02X03:04:05", "2025-01-02T03:04:05+02:00:30", "2025-W01-1", "2025-002",
    "2025-01-02T03:04:05z", "2025-01-02T03:04:05.123", "2025-01-02T03:04:05.1234567Z",
    "2025-01-02T24:00:00", "2025-01-02T03:04:05ZZ", "2025-01-02Z", "2025-01",
    "２０２５-01-02", "2025-01-02T03:04:05-00:00", "2025-01-02T03:04:05.5",
    "2025-01-02T03:04", "2025-01-02T03:04Z",
]

STALE_EDGE = [
    {"name": f"s{i}", "version": "1", "source": "aur", "extra": {"stale": v}}
    # Integers up to u64::MAX: serde_json (no arbitrary_precision) reads a
    # wider JSON integer such as 10**20 as a float, so the parity transport
    # cannot carry one -- and nor can App.extra in the Rust app.
    for i, v in enumerate([1.5, 1.0, 2**64 - 1, 2**63, -2**63, ["a", "b"], {"k": "v"}, [], {}, -0.0,
                           None, "  ", 1e100, 0.1, ["it's", 'q"', "\\", "\n", "\x01é", "\u200b", 1, 2.5, None, True],
                           {"a": {"b": [1]}, "it's": False}])
]

UPD_EDGE = [
    {"name": "ws-update", "version": "1", "source": "aur", "update": " ",
     "extra": {"critical": "security", "stale": "x"}},
    {"name": "crit-nonstr", "version": "1", "source": "aur", "update": "2",
     "extra": {"critical": ["security"]}},
]

FIND_EDGE = [
    {"id": "e", "title": "empty fix", "detail": "", "severity": "warn",
     "fix_label": "Go", "fix": []},
    {"id": "DISK", "title": "upper", "detail": "", "severity": "warn"},
    {"id": "x", "title": "Crit upper", "detail": "", "severity": "CRIT"},
]

# Installed plugins only -> because_you_run never reaches trust; exercises the
# tie order of the top-3 tag names.
TAG_TIES = [
    P(name="i1", installed=True, tags=["d", "b", "a", "c"]),
    P(name="i2", installed=True, tags=["c", "a"]),
    P(name="i3", installed=True, tags=["e"]),
    P(name="n1", installable=False, tags=["a"]),
]


def cases_round1():
    for v in TS_EDGE:
        yield "_ts", {"value": v}, lambda v=v: featured._ts(v)
    yield "needs_you", {"apps": STALE_EDGE, "findings": []}, lambda: featured.needs_you(apps(STALE_EDGE), [])
    for one in STALE_EDGE:
        yield "needs_you", {"apps": [one], "findings": []}, lambda one=one: featured.needs_you(apps([one]), [])
    yield "needs_you", {"apps": UPD_EDGE, "findings": []}, lambda: featured.needs_you(apps(UPD_EDGE), [])
    yield ("needs_you", {"apps": [], "findings": FIND_EDGE},
           lambda: featured.needs_you([], finds(FIND_EDGE)))
    yield "because_you_run", {"plugins": TAG_TIES, "limit": 8}, lambda: featured.because_you_run(plugs(TAG_TIES), 8)
    # Parity-hook argument handling: Python accepts any number here.
    bad = [{"name": "x"}]
    yield "needs_you", {"apps": bad, "findings": []}, lambda: featured.needs_you(apps(bad), [])
    extra_kw = [{"name": "x", "version": "1", "source": "aur", "bogus": 1}]
    yield "needs_you", {"apps": extra_kw, "findings": []}, lambda: featured.needs_you(apps(extra_kw), [])
    badf = [{"id": "x", "title": "t"}]
    yield "needs_you", {"apps": [], "findings": badf}, lambda: featured.needs_you([], finds(badf))
    badp = [{"name": "noid"}]
    yield "trending", {"plugins": badp, "days": 7, "limit": 8}, lambda: featured.trending(plugs(badp), 7, 8)
    for lim in (-1, 0, 1):
        yield "because_you_run", {"plugins": FULL, "limit": lim}, lambda lim=lim: featured.because_you_run(plugs(FULL), lim)
    yield ("new_listings", {"plugins": FULL, "days": 2.5, "limit": 8},
           lambda: featured.new_listings(plugs(FULL), 2.5, 8))
    yield ("_diversify", {"plugins": [P(**d) for d in PLAIN], "limit": -1, "per_market": 3},
           lambda: featured._diversify(plugs([P(**d) for d in PLAIN]), -1, 3))
    yield ("new_listings", {"plugins": UNTRUSTED_PATH, "days": 1.5, "limit": 8},
           lambda: featured.new_listings(plugs(UNTRUSTED_PATH), 1.5, 8))
    yield ("trending", {"plugins": UNTRUSTED_PATH, "days": 0, "limit": 8},
           lambda: featured.trending(plugs(UNTRUSTED_PATH), 0, 8))


_cases_round0 = cases


def cases():
    yield from _cases_round0()
    yield from cases_round1()


# ---- review round 2 -------------------------------------------------------

# Naive values are local time in Python (this machine: America/Los_Angeles):
# DST gap/fold, LMT before 1883, and the year-1 underflow Python turns into
# ValueError -> 0.0.
TS_EDGE2 = [
    "0001-01-01", "0001-01-01T00:00:00", "0001-01-01T07:00:00", "0001-01-02",
    "9999-12-31T23:59:59", "9999-12-31", "0001-01-01T00:00:00+01:00",
    "0001-01-01T00:00:00-01:00", "9999-12-31T23:59:59-23:59", "9999-12-31T23:59:59+23:59",
    "2025-03-09T02:30:00", "2025-11-02T01:30:00", "1970-01-01", "1883-11-18T12:00:00",
    "1800-01-01", "2100-07-01T12:00:00", "2025-01-02T03:04:05.9999995",
    "2025-01-02T03:04:05.000001+05:30", "2025-01-02T03:04:05+05:30:00.5",
]

# str()/repr() of App.extra["stale"] values: printable rules beyond the
# common controls (unassigned, format, private-use planes), float reprs,
# both quote kinds in one string, dict insertion order.
STALE_EDGE2 = [
    {"name": f"r{i}", "version": "1", "source": "aur", "extra": {"stale": v}}
    for i, v in enumerate([
        ["͸"], ["؀"], ["\U000E0001"], ["\U000F0000"], ["\U00100000"], ["￿"],
        ["⁥"], ["\U0001F600"], ["؜"], ["᠎"], ["\U0001D173"], [" x"],
        ["　"], [" "], ["\u0085"], ["\U0001FFFF"], ["ࢠ"], ["﷐"],
        ["both'\""], {"z": 1, "a": 2}, [1e16, 1e-7, 5e-324, 1.7976931348623157e308, 123456789012345678.0],
        1e16, 1e-7, 1e15, 123456789.123456789, -1.5e-10, ["กำ"], ["‍"],
    ])
]


def cases_round2():
    for v in TS_EDGE2:
        yield "_ts", {"value": v}, lambda v=v: featured._ts(v)
    for one in STALE_EDGE2:
        yield "needs_you", {"apps": [one], "findings": []}, lambda one=one: featured.needs_you(apps([one]), [])
    # Hook arguments of other Python types.
    pl = [P(**d) for d in PLAIN]
    yield ("_diversify", {"plugins": pl, "limit": 2.5, "per_market": 3},
           lambda: featured._diversify(plugs(pl), 2.5, 3))
    yield ("_diversify", {"plugins": pl, "limit": 8, "per_market": 1.5},
           lambda: featured._diversify(plugs(pl), 8, 1.5))
    yield ("because_you_run", {"plugins": FULL, "limit": 1.5},
           lambda: featured.because_you_run(plugs(FULL), 1.5))
    yield ("new_listings", {"plugins": UNTRUSTED_PATH, "days": 2**63, "limit": 8},
           lambda: featured.new_listings(plugs(UNTRUSTED_PATH), 2**63, 8))
    yield ("trending", {"plugins": UNTRUSTED_PATH, "days": True, "limit": 8},
           lambda: featured.trending(plugs(UNTRUSTED_PATH), True, 8))
    yield ("trending", {"plugins": FULL, "days": 7, "limit": True},
           lambda: featured.trending(plugs(FULL), 7, True))
    # Not a case: float stars. Plugin.stars is `int`, and every path that
    # builds a Plugin in store.py runs `int(...)` on it (store.py:312, 507,
    # 898), so a float never reaches featured; store::Plugin.stars is i64.
    # Unicode in updated[:10] and in tags.
    uu = [P(name="uu", added=ago(1), updated=ago(1)[:4] + "é-01-02", stars=9, built_in=True)]
    yield "trending", {"plugins": uu, "days": 7, "limit": 8}, lambda: featured.trending(plugs(uu), 7, 8)


_cases_round1_all = cases


def cases():
    yield from _cases_round1_all()
    yield from cases_round2()
