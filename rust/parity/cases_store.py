"""Parity cases for store.

Everything that writes goes under a private XDG_CACHE_HOME set here, before
store (and ghcache, themeart) are imported -- their CACHE paths are read at
import time -- and inherited by the Rust process. Settings are read from the
real XDG_STATE_HOME and never written.

The Python side runs every case before the Rust side runs any, so the real
network cases are ordered so both see the same caches: `catalog` fills the
market caches first, and every GitHub answer the Python side fetches is in
the shared ghcache directory when the Rust side asks for it.
"""
import io
import json
import os
import tempfile
import time
from pathlib import Path
from unittest import mock

_TMP = Path(tempfile.mkdtemp(prefix="almanac-parity-store-"))
os.environ["XDG_CACHE_HOME"] = str(_TMP / "cache")

import ghcache  # noqa: E402
import store  # noqa: E402

CACHE = _TMP / "cache" / "almanac"
CACHE.mkdir(parents=True, exist_ok=True)

# When run.py checks every module, an earlier cases_* file has already
# imported store/ghcache/themeart with the real XDG_CACHE_HOME, so the env
# set above came too late for their import-time paths: point them here
# explicitly (the Rust side reads the env at call time), and drop any
# catalog an earlier module loaded from the real cache.
store.CACHE = CACHE
store.THUMBS = CACHE / "thumbs"
store._cached = None
ghcache.CACHE = CACHE
ghcache.DIR = CACHE / "github"
ghcache.STATE = CACHE / "github-rate.json"
import sys  # noqa: E402
if "themeart" in sys.modules:
    sys.modules["themeart"].CACHE = CACHE
    sys.modules["themeart"].ART = CACHE / "themeart"


# ---- plugins -------------------------------------------------------------------

def row(**kw):
    r = {"id": kw.pop("id", "x"), "name": kw.pop("name", "X")}
    r.update(kw)
    return r


ROWS = [
    row(),
    row(id="clock", name="Clock", stars=12, author="ann", kind="Widget",
        repo="https://github.com/ann/omarchy-clock", category="Utilities",
        coverage="snapshot-verified", validated_commit="abcdef1234567",
        upstream_commit="abcdef1234567", updated="2026-03-01T10:00:00Z",
        added="2025-01-01T00:00:00Z", thumb="previews/clock.png",
        preview="previews/clock-large.png", accent="cyan", tags=["time", "bar"],
        installed=True),
    row(id="clocky", name="Clocky", stars=12, repo="https://github.com/bob/clocky",
        coverage="snapshot-verified", validated_commit="abcdef1234567",
        upstream_commit="1234567abcdef", updated="2026-03-01T10:00:00+00:00",
        category="utilities"),
    row(id="alarm", name="Alarm clock", stars=40, description="Wakes you",
        review_method="maintainer-reviewed", reviewed_by="dhh",
        reviewed_at="2026-01-02T03:04:05Z", repo="https://gitlab.com/z/alarm",
        updated="garbage", added="", market="omarchy", accent="nope"),
    row(id="alarm2", name="Second", description="an alarm CLOCK", stars=40,
        review_method="maintainer-reviewed", validated_commit="a", upstream_commit="b",
        repo="https://github.com/z/alarm2"),
    row(id="omarchy.battery", name="Battery", built_in=True, stars=0,
        installable=True, install_cmd="omarchy plugin enable battery"),
    row(id="wordcraft", name="WordCraft", market="artcraft", install_kind="appimage",
        installable=True, version="1.2.0", repo="https://github.com/storytold/wordcraft",
        stars=15000, thumb="images/apps/wordcraft/hero.webp",
        preview="images/apps/wordcraft/hero.webp", category="Productivity"),
    row(id="fake", name="Fakecraft", market="artcraft", install_kind="appimage",
        installable=False, repo="https://github.com/mallory/fakecraft", stars=3),
    row(id="theme.ann.omarchy-lumon-theme", name="Lumon", market="themes",
        install_kind="theme", repo="https://github.com/ann/omarchy-lumon-theme",
        thumb="1/ann/omarchy-lumon-theme", preview="1/ann/omarchy-lumon-theme",
        category="Appearance", stars=77, upstream_status="passed"),
    row(id="gone", name="Gone", upstream_status="failed",
        upstream_checked_at="2026-05-05T05:05:05Z", stars=1),
    row(id="gone2", name="Gone too", upstream_status="unreachable", stars=1),
    row(id="stale", name="Stale", coverage="update-unverified",
        validated_commit="0123456789", upstream_commit="9876543210", stars=5),
    row(id="unk", name="Unknown market", market="nowhere", repo="no/slashes",
        stars=-3, thumb="t.png"),
    row(id="uni", name="Ünïcödé Σίσυφος", description="ÉMOJI 🎉 straße",
        author="ÅSA", kind="İstanbul", tags=["ß", "Ω"], stars=2,
        repo="https://github.com/ü/ñ", updated="2026-01-01", added="2026-01-01T12:00"),
    row(id="CAPS", name="caps", stars=2, updated="2026-01-01T00:00:00.123456Z"),
    row(id="dup1", name="Same", stars=9),
    row(id="dup2", name="same", stars=9),
    row(id="neg", name="", stars=0, market="themes", install_kind="plugin"),
]


def _plugins(rows):
    return [store.Plugin(**r) for r in rows]


def _props(p):
    return {
        "market_name": p.market_name, "page": p.page, "asset_x": p.asset("x/y.png"),
        "thumb_url": p.thumb_url, "preview_url": p.preview_url, "colors": p.colors,
        "owner": p.owner, "host": p.host, "repo_name": p.repo_name,
        "publisher": p.publisher, "can_install": p.can_install, "drifted": p.drifted,
        "trust": p.trust, "haystack": p.haystack, "subtitle": p.subtitle,
    }


# ---- markets with hand-made caches -----------------------------------------------

def _seed(mid, content, age):
    path = CACHE / f"catalog-{mid}.json"
    path.write_text(content if isinstance(content, str) else json.dumps(content))
    t = time.time() - age
    os.utime(path, (t, t))


FRESH, MID, OLD = 0, 20 * 60, 7 * 3600
_R = [{"id": "a", "name": "A"}, {"id": "b", "name": "B", "unchecked": False}]
_U = [{"id": "a", "name": "A"}, {"id": "b", "name": "B", "unchecked": True}]
_seed("p-fresh", {"schema": store.SCHEMA, "rows": _R}, FRESH)
_seed("p-oldschema", {"schema": 4, "rows": _R}, FRESH)
_seed("p-boolschema", {"schema": True, "rows": _R}, FRESH)
_seed("p-floatschema", {"schema": 5.0, "rows": _R}, FRESH)
_seed("p-expired", {"schema": store.SCHEMA, "rows": _R}, OLD)
_seed("p-unchecked-mid", {"schema": store.SCHEMA, "rows": _U}, MID)
_seed("p-unchecked-new", {"schema": store.SCHEMA, "rows": _U}, FRESH)
_seed("p-list", [{"id": "l"}], FRESH)
_seed("p-broken", "{nope", FRESH)
_seed("p-junk-mid", {"schema": store.SCHEMA, "rows": [1, 2]}, MID)
_seed("p-junk-new", {"schema": store.SCHEMA, "rows": [1, 2]}, FRESH)
_seed("p-dictrows", {"schema": store.SCHEMA, "rows": {"a": 1}}, FRESH)
_seed("p-emptyrows", {"schema": store.SCHEMA, "rows": []}, FRESH)
_seed("p-nullrows", {"schema": store.SCHEMA, "rows": None}, FRESH)
_seed("p-norows", {"schema": store.SCHEMA}, FRESH)
_seed("p-unicode", {"schema": store.SCHEMA, "rows": [{"id": "ü", "name": "Ünï 🎉"}]}, FRESH)
MARKET_IDS = ["p-fresh", "p-oldschema", "p-boolschema", "p-floatschema", "p-expired",
              "p-unchecked-mid", "p-unchecked-new", "p-list", "p-broken", "p-junk-mid",
              "p-junk-new", "p-dictrows", "p-emptyrows", "p-nullrows", "p-norows",
              "p-unicode", "p-missing"]


def _market(mid):
    return store.Market(id=mid, name=mid.upper(), site="https://example.invalid/")


# A thumbnail already on disk is returned without a fetch.
_HAVE = store.Plugin(id="have", name="Have", thumb="cards/have.jpg", market="omarchy")
store.THUMBS.mkdir(parents=True, exist_ok=True)
store.thumb_path(_HAVE).write_bytes(b"jpeg")
_EMPTY = store.Plugin(id="empty", name="Empty", thumb="cards/empty.png",
                      market="nowhere-market")
store.thumb_path(_EMPTY).write_bytes(b"")


def _fetch_thumb(r, detail, cleanup):
    import hashlib
    got = store.fetch_thumb(store.Plugin(**r), detail)
    if got is None:
        return None
    out = {"path": str(got), "sha1": hashlib.sha1(got.read_bytes()).hexdigest()}
    if cleanup:
        got.unlink()
    return out


# ---- mocked marketplaces --------------------------------------------------------

def _artcraft(html, repos, releases, previous):
    with tempfile.TemporaryDirectory() as d:
        if previous is not None:
            (Path(d) / "catalog-artcraft.json").write_text(previous)
        if html is None:
            opener = mock.patch.object(store.urllib.request, "urlopen",
                                       side_effect=OSError("offline"))
        else:
            opener = mock.patch.object(store.urllib.request, "urlopen",
                                       return_value=io.BytesIO(html.encode()))
        with mock.patch.object(store, "CACHE", Path(d)), opener, \
             mock.patch.object(ghcache, "org_repos", return_value=repos), \
             mock.patch.object(ghcache, "latest_release",
                               side_effect=lambda r: releases.get(r)):
            return store._artcraft_rows()


def _themes(pages):
    def answer(url, *a, **k):
        n = int(url.rsplit("page=", 1)[1])
        return pages[n - 1] if n - 1 < len(pages) else None
    with mock.patch.object(ghcache, "get_json", side_effect=answer):
        return store._theme_rows()


HTML = ('<nav><a href="/apps/wordcraft" class="x">WordCraft</a>'
        '<a href="/apps/gridcraft">\n  Grid Craft  \n</a>'
        '<a href="/apps/photocraft"><img src="p.png"></a>'
        '<a href="/apps/wordcraft">Again</a>'
        '<a href="/apps/new-thing-2">A very very very long label that is over thirty</a>'
        '<a href="/apps/Upper">Nope</a></nav>')
ASSET = {"name": "gridcraft-0.7.0-linux-x86_64.AppImage",
         "browser_download_url": "https://example.invalid/g.AppImage", "size": 123456789}
FOUND = {"tag_name": "v0.7.0", "published_at": "2026-02-03T04:05:06Z", "assets": [
    {"name": "gridcraft-0.7.0-mac.dmg"}, ASSET]}
REPOS = [{"name": "WordCraft", "description": "Words", "license": {"spdx_id": "NOASSERTION"},
          "stargazers_count": 1500, "pushed_at": "2026-01-01T00:00:00Z",
          "html_url": "https://github.com/storytold/wordcraft"},
         {"name": "gridcraft", "license": None, "stargazers_count": "12"},
         "not a dict", {"name": ""}, {"description": "no name"}]
PREVIOUS = json.dumps({"schema": 5, "rows": [
    {"id": "photocraft", "version": "0.9", "download_url": "https://example.invalid/p",
     "asset_name": "p.AppImage", "size_mb": 55.5, "added": "2025-12-12T00:00:00"},
    {"id": 7}, {"name": "no id"}]})

ARTCRAFT_CASES = [
    dict(html=HTML, repos=REPOS, releases={"storytold/gridcraft": FOUND}, previous=None),
    dict(html=HTML, repos=REPOS, releases={"storytold/gridcraft": FOUND}, previous=PREVIOUS),
    dict(html=HTML, repos=None, releases={
        "storytold/gridcraft": {"tag_name": "1.0", "assets": []},
        "storytold/wordcraft": ["truthy", "list"],
        "storytold/photocraft": {"tag_name": "", "assets": [ASSET]},
        "storytold/new-thing-2": {"tag_name": "vv2", "published_at": "x" * 40,
                                  "assets": [dict(ASSET, size=0)]}},
         previous=PREVIOUS),
    dict(html="<p>nothing here</p>", repos=REPOS, releases={}, previous=None),
    dict(html="", repos=REPOS, releases={}, previous="{broken"),
    dict(html=None, repos=REPOS, releases={}, previous=None),
    dict(html=HTML, repos=REPOS, releases={}, previous="[1, 2]"),
    dict(html=HTML, repos=REPOS, releases={}, previous=json.dumps({"rows": [1]})),
    dict(html=HTML, repos=REPOS, releases={}, previous=json.dumps({"rows": None})),
    dict(html=HTML, repos={"message": "rate limited"}, releases={
        "storytold/gridcraft": {"tag_name": "v3", "assets": [dict(ASSET, size=None)]}},
         previous=None),
    dict(html=HTML, repos=REPOS, releases={
        "storytold/gridcraft": {"tag_name": "v3", "assets": None}}, previous=None),
]

THEME_ITEM = {"full_name": "ann/omarchy-lumon-theme", "name": "omarchy-lumon-theme",
              "owner": {"login": "Ann"}, "description": "Severance",
              "topics": ["omarchy", "omarchy-theme", "hyprland", "dark", "extra"],
              "html_url": "https://github.com/ann/omarchy-lumon-theme",
              "clone_url": "https://github.com/ann/omarchy-lumon-theme.git",
              "stargazers_count": 42, "license": {"spdx_id": "MIT"},
              "pushed_at": "2026-04-04T00:00:00Z", "created_at": "2025-04-04T00:00:00Z"}
THEME_PAGES_CASES = [
    [{"items": [THEME_ITEM,
                dict(THEME_ITEM),                                   # duplicate
                dict(THEME_ITEM, full_name="b/awesome-omarchy-themes", name="awesome-omarchy-themes"),
                dict(THEME_ITEM, full_name="c/tokyo_night-omarchy", name="Tokyo_Night-Omarchy",
                     owner=None, topics=None, license=None, clone_url=None, stargazers_count=None),
                dict(THEME_ITEM, full_name="d/omarchy-theme", name="omarchy-theme"),
                dict(THEME_ITEM, full_name="e/dotfiles-theme", name="dotfiles-theme"),
                dict(THEME_ITEM, full_name="f/plain", name="plain"),
                dict(THEME_ITEM, full_name="g/ÜBER-theme", name="ÜBER-theme",
                     description=None, pushed_at=None)]},
     {"items": [dict(THEME_ITEM, full_name="h/omarchy_rose_pine_theme",
                     name="omarchy_rose_pine_theme")]},
     {"items": []},
     {"items": [dict(THEME_ITEM, full_name="never/reached-theme", name="reached-theme")]}],
    [None],
    [{"message": "API rate limit exceeded"}],
    [{"items": [dict(THEME_ITEM, name=None, full_name="x/none")]}],
    [{"items": [dict(THEME_ITEM, stargazers_count="nope")]}],
    [{"items": [THEME_ITEM]}] * 5,
]


# ---- raw catalogue entries ------------------------------------------------------

SETUP_NOTE = ("This plugin requires additional setup before it can be enabled. "
              "Follow the upstream installation instructions.")
FULL = {
    "id": "omarchy.clock", "name": "Clock", "description": "A clock", "author": "ann",
    "version": "1.0.0", "category": "Utilities", "kind": "Widget", "status": "Listed",
    "tags": ["time"], "accent": "cyan", "initials": "CL",
    "repo": "https://github.com/ann/clock", "stars": 12,
    "verificationStatus": "verified", "installAvailable": True,
    "installCommand": "omarchy plugin add https://github.com/ann/clock",
    "installNote": "", "license": "MIT", "repositoryUpdatedAt": "2026-01-01T00:00:00Z",
    "listedAt": "2025-01-01T00:00:00Z", "addedAt": "2024-01-01T00:00:00Z",
    "previewThumbnail": "t.png", "previewImage": "i.png", "builtIn": False,
    "sourceType": "community", "sourceUrl": "https://github.com/ann/clock",
    "manifestPath": "plugin.json", "verificationCoverage": "snapshot-verified",
    "verificationMethod": "automated", "verificationReviewedBy": "",
    "verificationReviewedAt": "", "listingValidatedCommit": "abc",
    "listingValidatedAt": "2026-01-01", "listingValidatedBranch": "main",
    "upstreamObservedCommit": "abc", "upstreamCheckedAt": "2026-01-02",
    "upstreamCheckStatus": "passed", "extraField": "dropped",
}
SETUP = {"id": "x", "repo": "https://github.com/a/x", "installAvailable": False,
         "repositoryLayout": "root-plugin", "installCommand": "", "installNote": SETUP_NOTE}
ENTRIES = [
    FULL, {}, {"id": "only-id"}, {"name": "No id"},
    dict(FULL, stars="12"), dict(FULL, stars=" 7 "), dict(FULL, stars="abc"),
    dict(FULL, stars=3.9), dict(FULL, stars=-2.5), dict(FULL, stars=True),
    dict(FULL, stars=None), dict(FULL, stars=[]), dict(FULL, stars=[1]),
    dict(FULL, stars="1_000"), dict(FULL, stars="+5"), dict(FULL, stars="1__0"),
    dict(FULL, builtIn=True, installAvailable=False, installCommand="",
         officialCommand="omarchy plugin enable clock"),
    dict(FULL, tags=None, name="", previewImage="", listedAt=None),
    dict(FULL, verificationStatus="Verified", id=5),
    SETUP, dict(SETUP, repo="https://github.com/a/x;rm -rf ~"),
    dict(SETUP, repo="https://github.com/o'neil/x y"), dict(SETUP, repo="ssh://evil/x"),
    dict(SETUP, repositoryLayout="monorepo"), dict(SETUP, installAvailable=True),
    dict(SETUP, builtIn=1), dict(SETUP, installNote=7), dict(SETUP, installNote=None),
    dict(SETUP, installCommand="custom"), dict(SETUP, status="Manual"),
    dict(SETUP, repo=None), dict(SETUP, repo="https://ünï.example/x"),
    {"id": "ü", "name": "Ünïcödé 🎉", "description": "straße", "stars": 0},
    "not a dict", None, [1, 2],
]

TS = ["", "2026-10-17T00:00:00Z", "2026-10-17T00:00:00+00:00", "2026-10-17",
      "2026-10-17T10:20", "2026-10-17 10:20:30.123456", "2026-10-17T10:20:30.123",
      "2026-10-17T10:20:30+05:30", "2026-10-17T10:20:30-07:00", "2026-10-17T10",
      "20261017", "2026-W42-6", "garbage", " 2026-10-17", "2026-10-17 ",
      "2026-02-30", "2026-10-17T10:20:30.1234567", "2026-10-17T10:20:30+0530",
      "2026-10-17T10:20:30-07", "1970-01-01T00:00:00Z", "2026-10-17t10:20:30",
      "2026-10-17X10:20:30", "2026-10-17T10:20:30ZZ", "２０２６-01-01",
      "2026-10-17T10:20:30x", "2026-10-17T10:20:30.", "2026-10-17T10:20x", "2026-10-17T1020",
      "2026-10-17T102030", "2026-10-17T10:2030", "2026-10-17T24:00", "2026-10-17T24:00:01",
      "2026-10-17T10:20:30+24:00", "2026-10-17T10:20:30+23:59:59.5", "2026-10-17é10:20",
      "2026-W53-1", "2025-W53-1", "2026W427", "2026W42", "2026-W42", "2026-W4", "2026-1-17",
      "2026-10-17T10:20:30,5", "2026-10-17T10:20:30.5Z", "2026-10-17T10Z",
      "2026-10-17T10:20:30Z+01", "0000-01-01", "9999-12-31T23:59:59", "9999-12-31T24:00",
      "2026-10-17T10:20:30-00:00", "2026-10-17T10:20:30+05", "2026-10-17T10:20:30+0",
      "2026-10-17T10:20:30+053", "2026-10-17TT", "2026-10-17T10:20:30.123456789+01:00",
      "2026-10-17T1", "2026-10-1", "2026-10", "2026", "2026W4271", "2026W42-7", "2026-W427",
      "2026-W42-7T10", "2026-W42-10", "2026-W42-7", "20261017T1020Z", "2026-10-17T10:20:60",
      "2026-10-17T10:60", "2026-13-01", "2026-02-29", "2024-02-29T23:59:59.999999Z",
      "2026-10-17T10:20:30+05:30:15", "2026-10-17T10:20:30-0530", "2026-10-17T10:20:30+05:3",
      "2026-03-08T02:30:00", "2026-11-01T01:30:00", "2026-10-17T10:20:30.000001-12:00",
      "2026-10-17\n10:20", "2026-10-17T10:20:30+05:30Z", "2026-10-17T-05:00"]


def cases():
    # constants
    yield "accents", {}, lambda: {
        "accents": [[k, list(v)] for k, v in store.ACCENTS.items()],
        "default": store.DEFAULT_ACCENT, "trust_order": store.TRUST_ORDER,
        "schema": store.SCHEMA, "ttl": store.TTL, "retry": store.RETRY,
        "sorts": [[k, label] for k, label, _f in store.SORTS]}

    # Plugin(**row) and its properties
    for r in ROWS:
        yield "Plugin", {"row": r}, lambda r=r: store.Plugin(**r)
        yield "props", {"row": r}, lambda r=r: _props(store.Plugin(**r))
    for bad in ({"id": "x"}, {"name": "x"}, {}, {"id": "x", "name": "y", "bogus": 1}):
        yield "Plugin", {"row": bad}, lambda r=bad: store.Plugin(**r)

    # raw entries
    for e in ENTRIES:
        yield "_needs_setup", {"e": e}, lambda e=e: store._needs_setup(e)
        yield "_omarchy_entry", {"e": e}, lambda e=e: store._omarchy_entry(e)

    for repo in ({"name": "omarchy-lumon-theme"}, {"name": "Tokyo-Night"}, {"name": "rose-pine-theme"},
                 {"name": "awesome-omarchy"}, {"name": "omarchy-theme-hook"},
                 {"name": "omarchy-theme-manager"}, {"name": "my-dotfiles-omarchy"},
                 {"name": "OMARCHY"}, {"name": ""}, {"name": None}, {}, {"name": 5},
                 {"name": "ÜBER-THEME"}, "nope"):
        yield "_looks_like_theme", {"repo": repo}, lambda r=repo: store._looks_like_theme(r)

    for v in TS:
        yield "_ts", {"value": v}, lambda v=v: store._ts(v)

    # sorting and filtering
    for key, _label, fn in store.SORTS:
        yield "sort", {"plugins": ROWS, "key": key}, \
            lambda fn=fn: [p.id for p in sorted(_plugins(ROWS), key=fn)]
    browse_args = [
        {}, {"sort": "az"}, {"sort": "za"}, {"sort": "bogus"}, {"sort": "new"},
        {"sort": "trending"}, {"sort": "provenance"}, {"sort": "market"},
        {"sort": "publisher"}, {"sort": "category"},
        {"query": "clock"}, {"query": "  CLOCK "}, {"query": "c"}, {"query": "c", "sort": "za"},
        {"query": "c", "sort": "bogus"}, {"query": "zzz"}, {"query": "ünï"}, {"query": "σίσυφος"},
        {"query": "storytold"}, {"query": "artcraft"}, {"query": "time bar"}, {"query": "caps"},
        {"query": "same"}, {"query": "   "},
        {"market": "artcraft"}, {"market": "nowhere"}, {"trust": "verified"},
        {"trust": "first-party"}, {"trust": "official"}, {"category": "Utilities"},
        {"category": "utilities"}, {"verified_only": True}, {"installed_only": True},
        {"verified_only": True, "query": "a", "sort": "az"},
        {"market": "omarchy", "trust": "unverified", "category": "Other", "sort": "za"},
    ]
    for a in browse_args:
        yield "browse", dict(a, plugins=ROWS), \
            lambda a=a: [p.id for p in store.browse(_plugins(ROWS), **a)]
    for rows in (ROWS, [], ROWS[:3]):
        yield "categories", {"plugins": rows}, lambda rows=rows: store.categories(_plugins(rows))
        yield "market_counts", {"plugins": rows}, lambda rows=rows: store.market_counts(_plugins(rows))
        yield "trust_counts", {"plugins": rows}, lambda rows=rows: store.trust_counts(_plugins(rows))

    # card artwork
    for r in ROWS + [row(thumb="a/b.tar.gz"), row(thumb="noext"), row(thumb="a/.hidden"),
                     row(thumb="trailing."), row(thumb="dir.d/"), row(thumb="ü/ñ.PNG?x=1"),
                     row(thumb="a/..", preview="x/./."), row(thumb="/abs//x.jpg")]:
        for detail in (False, True):
            yield "thumb_path", {"row": r, "detail": detail}, \
                lambda r=r, d=detail: store.thumb_path(store.Plugin(**r), d)
    for r, detail, cleanup in (
            (vars(_HAVE), False, False), (vars(_EMPTY), False, False),
            (row(), False, False), (row(thumb="x.png", market="nowhere"), False, False),
            (row(id="wordcraft", market="artcraft", thumb="images/apps/wordcraft/hero.webp",
                 preview="images/apps/wordcraft/hero.webp"), True, True),
            (row(id="nope", market="artcraft", thumb="images/apps/no-such-app/hero.webp"),
             False, True)):
        r = {k: v for k, v in r.items() if not k.startswith("_")}
        yield "fetch_thumb", {"row": r, "detail": detail, "cleanup": cleanup}, \
            lambda r=r, d=detail, c=cleanup: _fetch_thumb(r, d, c)

    # markets
    for mid in ("omarchy", "artcraft", "themes"):
        yield "Market.cache", {"market": mid}, lambda mid=mid: store.MARKETS[mid].cache
        for r in ROWS[:3] + [row(id="a b/ü?&")]:
            yield "Market.listing_url", {"market": mid, "row": r}, \
                lambda mid=mid, r=r: store.MARKETS[mid].listing_url(store.Plugin(**r))
    for mid in MARKET_IDS:
        spec = {"id": mid, "name": mid.upper(), "site": "https://example.invalid/"}
        for force in (False, True):
            yield "_load_market", {"market": spec, "force": force}, \
                lambda mid=mid, f=force: store._load_market(_market(mid), f)
    yield "Market.fetch", {"market": {"id": "nourl", "name": "N", "site": ""}}, \
        lambda: _market("nourl").fetch()

    for c in ARTCRAFT_CASES:
        yield "_artcraft_rows_with", c, lambda c=c: _artcraft(**c)
    for pages in THEME_PAGES_CASES:
        yield "_theme_rows_with", {"pages": pages}, lambda p=pages: _themes(p)

    # the real machine and the real marketplaces (read-only; caches go to _TMP)
    yield "enabled_markets", {}, lambda: [m.id for m in store.enabled_markets()]
    yield "installed_ids", {}, store.installed_ids
    yield "installed_themes", {}, store.installed_themes
    yield "removable_themes", {}, store.removable_themes
    yield "installed_appimages", {}, store.installed_appimages
    yield "catalog", {"force": False}, lambda: store.catalog(False)
    yield "_artcraft_rows", {}, store._artcraft_rows
    yield "_theme_rows", {}, store._theme_rows
    for mid in ("artcraft", "themes", "omarchy"):
        yield "Market.fetch", {"market": mid}, lambda mid=mid: store.MARKETS[mid].fetch()
    for mid in ("artcraft", "themes", "omarchy"):
        yield "_load_market", {"market": mid, "force": False}, \
            lambda mid=mid: store._load_market(store.MARKETS[mid], False)
    sample = ROWS + [row(id="omarchy.clock", name="Clock"),
                     row(id="theme.x.solitude", name="Solitude", install_kind="theme",
                         market="themes", version="1"),
                     row(id="theme.x.tokyo", name="Tokyo Night", install_kind="theme",
                         market="themes", built_in=True)]
    yield "annotate", {"plugins": sample}, lambda: _annotated(sample)

    yield from _review_round1()
    yield from _review_round2()
    yield from _review2_verify()


# ---- review round 1: suspected differences -----------------------------------

def _review_round1():
    # Plugin(**row) does not type-check its fields; a cached row with a
    # non-str id or a null field still loads in Python.
    for r in (row(id=5), row(tags=None), row(stars="12"), row(description=None),
              row(size_mb=None), row(installed=1)):
        yield "Plugin", {"row": r}, lambda r=r: store.Plugin(**r)
    # int() of non-ASCII digits and of integers beyond i64.
    for stars in ("\u0663", "\uff11\uff12", "99999999999999999999",
                  18446744073709551615, 99999999999999999999, "-0", " \u00a012\u2003"):
        e = dict(FULL, stars=stars)
        yield "_omarchy_entry", {"e": e}, lambda e=e: store._omarchy_entry(e)
    # naive dates far in the past go through the local zone (LMT before 1883).
    for v in ("0001-01-01", "0001-01-01T00:00:00", "0001-01-02T00:00:00", "1800-01-01T00:00:00",
              "1883-11-18T12:00:00", "0001-01-01T00:00:00+00:00", "0001-01-01T00:00:00+01:00",
              "9999-12-31T23:59:59", "9999-12-31T23:59:59-12:00", "1969-12-31T23:59:59.5"):
        yield "_ts", {"value": v}, lambda v=v: store._ts(v)
    # ArtCraft: shapes the Python handles differently from the port.
    html = '<a href="/apps/gridcraft">GridCraft</a>'
    bad_artcraft = [
        dict(html=html, repos=REPOS, releases={},
             previous=json.dumps({"rows": [{"id": [1], "version": "x"}]})),
        dict(html=html, repos=REPOS, releases={},
             previous=json.dumps({"rows": [{"id": {"a": 1}}]})),
        dict(html=html, repos=REPOS, releases={"storytold/gridcraft": dict(
            FOUND, published_at=["2026", "02"])}, previous=None),
        dict(html=html, repos=REPOS, releases={"storytold/gridcraft": dict(
            FOUND, assets=[dict(ASSET, size=True)])}, previous=None),
        dict(html=html, repos=[{"name": "gridcraft", "license": "MIT"}], releases={},
             previous=None),
        dict(html=html, repos=[{"name": "gridcraft", "license": ["MIT"]}], releases={},
             previous=None),
    ]
    for c in bad_artcraft:
        yield "_artcraft_rows_with", c, lambda c=c: _artcraft(**c)
    bad_themes = [
        [{"items": [dict(THEME_ITEM, license="MIT")]}],
        [{"items": [dict(THEME_ITEM, name="omarchy-x-theme\n", full_name="a/x")]}],
        [{"items": [dict(THEME_ITEM, clone_url=["a", "b"])]}],
        [{"items": [dict(THEME_ITEM, clone_url={"a": 1})]}],
        [{"items": [dict(THEME_ITEM, full_name=["a"])]}],
        [{"items": [dict(THEME_ITEM, full_name=1), dict(THEME_ITEM, full_name=True)]}],
        [{"items": [dict(THEME_ITEM, html_url=None)]}],
    ]
    for pages in bad_themes:
        yield "_theme_rows_with", {"pages": pages}, lambda p=pages: _themes(p)


# ---- review round 2: around the round-1 fixes --------------------------------

def _review_round2():
    for r in (row(id=None), row(tags=["a", 1, None]), row(tags="ab"), row(stars=3.0),
              row(stars=2**70), row(size_mb=3), row(size_mb="big"), row(verified="yes"),
              row(name={"a": [1, "b"]}), row(local_version=7)):
        yield "Plugin", {"row": r}, lambda r=r: store.Plugin(**r)
    for stars in ("\u0e53\u0e52", "1_\u0663", "-\uff19", "\u00b2", "\u2167", "1e3", "0x10",
                  1e20, -1e19, 2.5e18):
        e = dict(FULL, stars=stars)
        yield "_omarchy_entry", {"e": e}, lambda e=e: store._omarchy_entry(e)
    for v in ("0001-01-01T12:00:00", "0001-01-01T07:52:58", "0001-01-01T07:52:57",
              "0001-01-02", "9999-12-31", "2026-03-08T02:00:00", "2026-03-08T03:00:00",
              "2026-11-01T01:00:00", "2026-11-01T02:00:00", "1883-11-18T11:59:59"):
        yield "_ts", {"value": v}, lambda v=v: store._ts(v)
    html = '<a href="/apps/gridcraft">GridCraft</a>'
    for c in [
        dict(html=html, repos=REPOS, releases={},
             previous=json.dumps({"rows": [{"id": None}, {"id": 1.5}, {"id": True}]})),
        dict(html=html, repos=REPOS, releases={"storytold/gridcraft": dict(
            FOUND, published_at={"a": 1})}, previous=None),
        dict(html=html, repos=REPOS, releases={"storytold/gridcraft": dict(
            FOUND, published_at=5)}, previous=None),
        dict(html=html, repos=REPOS, releases={"storytold/gridcraft": dict(
            FOUND, published_at=list("abcdefghijklmnopqrstuvwxyz"))}, previous=None),
        dict(html=html, repos=REPOS, releases={"storytold/gridcraft": dict(
            FOUND, assets=[dict(ASSET, size=False)])}, previous=None),
        dict(html=html, repos=REPOS, releases={"storytold/gridcraft": dict(
            FOUND, assets=[dict(ASSET, size="5")])}, previous=None),
        dict(html=html, repos=[{"name": "gridcraft", "license": True}], releases={},
             previous=None),
        dict(html=html, repos=[{"name": "gridcraft", "license": 0}], releases={},
             previous=None),
        dict(html=html, repos=[{"name": "gridcraft", "stargazers_count": "\uff17"}],
             releases={}, previous=None),
    ]:
        yield "_artcraft_rows_with", c, lambda c=c: _artcraft(**c)
    for pages in [
        [{"items": [dict(THEME_ITEM, name="omarchy\n", full_name="a/1")]}],
        [{"items": [dict(THEME_ITEM, name="x-theme\n\n", full_name="a/2")]}],
        [{"items": [dict(THEME_ITEM, name="x\ntheme", full_name="a/3")]}],
        [{"items": [dict(THEME_ITEM, name="omarchy-x-omarchy\n", full_name="a/4")]}],
        [{"items": [dict(THEME_ITEM, full_name=1.0), dict(THEME_ITEM, full_name=1)]}],
        [{"items": [dict(THEME_ITEM, full_name=0), dict(THEME_ITEM, full_name=False),
                    dict(THEME_ITEM, full_name=None), dict(THEME_ITEM, full_name=None)]}],
        [{"items": [dict(THEME_ITEM, full_name={"a": 1})]}],
        [{"items": [dict(THEME_ITEM, clone_url=[None, True, 1.5, "it's", 'say "hi"', "\n\x01"])]}],
        [{"items": [dict(THEME_ITEM, clone_url=7)]}],
        [{"items": [dict(THEME_ITEM, license=["x"])]}],
        [{"items": [dict(THEME_ITEM, stargazers_count=2**64 + 1)]}],
    ]:
        yield "_theme_rows_with", {"pages": pages}, lambda p=pages: _themes(p)


# ---- review round 2 (adversarial verify): suspected differences ---------------

_seed("p-surrogate", '{"schema": 5, "rows": [{"id": "a", "name": "\\ud83d"}]}', FRESH)
_seed("p-nan", '{"schema": 5, "rows": [{"id": "a", "name": "A", "size_mb": NaN}]}', FRESH)


def _representable(v):
    """`v` with lone surrogates as U+FFFD and NaN/infinities as None."""
    import math
    if isinstance(v, dict):
        return {_representable(k): _representable(x) for k, x in v.items()}
    if isinstance(v, list):
        return [_representable(x) for x in v]
    if isinstance(v, str):
        return "".join("\ufffd" if 0xD800 <= ord(c) < 0xE000 else c for c in v)
    if isinstance(v, float) and not math.isfinite(v):
        return None
    return v


def _review2_verify():
    # Python's re \s matches \x1c-\x1f (str.isspace); Rust regex's \s does not.
    for html in ('<a href="/apps/gridcraft">\x1fGridCraft</a>',
                 '<a href="/apps/gridcraft">GridCraft\x1c</a>'):
        c = dict(html=html, repos=REPOS, releases={}, previous=None)
        yield "_artcraft_rows_with", c, lambda c=c: _artcraft(**c)
    # str.title() uses the titlecase mapping (ǆ -> ǅ, ß -> Ss, ﬁ -> Fi).
    for name in ("\u01c6x-theme", "\u00df-theme", "\ufb01re-theme", "x\u01c5y-theme"):
        pages = [{"items": [dict(THEME_ITEM, name=name, full_name="t/" + name)]}]
        yield "_theme_rows_with", {"pages": pages}, lambda p=pages: _themes(p)
    # 1e19 == 10**19 in Python, so the second is a duplicate there.
    pages = [{"items": [dict(THEME_ITEM, full_name=1e19), dict(THEME_ITEM, full_name=10**19)]}]
    yield "_theme_rows_with", {"pages": pages}, lambda p=pages: _themes(p)
    # Caches Python's json reads and serde_json does not: lone surrogate, NaN.
    # Both load (the rows are served, not refetched). A Rust str cannot hold
    # a lone surrogate nor JSON a NaN, so the port holds U+FFFD and null in
    # their place; the Python answer is compared in those terms.
    for mid in ("p-surrogate", "p-nan"):
        spec = {"id": mid, "name": mid.upper(), "site": "https://example.invalid/"}
        yield "_load_market", {"market": spec, "force": False}, \
            lambda mid=mid: _representable(store._load_market(_market(mid), False))
    # Plugin(**row) keeps wrong-typed cached values; the properties then raise.
    for r in (row(tags=["a", 1]), row(name=None), row(repo=None),
              row(upstream_status="failed", upstream_checked_at=None)):
        yield "props", {"row": r}, lambda r=r: _props(store.Plugin(**r))
    rows = [row(id="s", stars="12"), row(id="t", stars=3)]
    yield "browse", {"plugins": rows}, lambda rows=rows: [p.id for p in store.browse(_plugins(rows))]
    # More wrong-typed values: what the properties make of them.
    for r in (row(market=["x"]), row(thumb=5), row(thumb=0), row(accent=["a"]), row(accent=3),
              row(validated_commit=1, upstream_commit="1", coverage="snapshot-verified"),
              row(validated_commit=0, upstream_commit="1", coverage="snapshot-verified"),
              row(validated_commit=["abcdefghij"], upstream_commit="x", coverage="update-unverified"),
              row(tags="ab"), row(tags=None), row(tags={"k": 1}), row(author=5), row(kind=7, author=""),
              row(id=5), row(market=5), row(repo="https://github.com/basecamp/x/y", market="omarchy",
                                             version=2, built_in=0),
              row(review_method="maintainer-reviewed", reviewed_by=3, reviewed_at=None),
              row(review_method="maintainer-reviewed", reviewed_at=["2026-01-01T00:00"]),
              row(installable=1, install_cmd=0, install_kind="appimage"), row(built_in="yes")):
        yield "props", {"row": r}, lambda r=r: _props(store.Plugin(**r))
    mixed = [
        [row(id="a", stars=1.5), row(id="b", stars=2), row(id="c", stars=True)],
        [row(id="a", updated=5), row(id="b")],
        [row(id="a", updated=0, added=None), row(id="b")],
        [row(id="a", name=3), row(id="b")],
        [row(id="a", category=None), row(id="b")],
        [row(id="a", market=["m"]), row(id="b")],
        [row(id="a", repo=None), row(id="b")],
        [row(id="a", stars=10**20), row(id="b", stars=1e20), row(id="c", stars=-(10**20))],
    ]
    for rows in mixed:
        for key, _l, fn in store.SORTS:
            yield "sort", {"plugins": rows, "key": key}, \
                lambda rows=rows, fn=fn: [p.id for p in sorted(_plugins(rows), key=fn)]
        for a in ({}, {"query": "a"}, {"query": "zz"}, {"installed_only": True}, {"trust": "unverified"},
                  {"category": "Other"}, {"market": "omarchy"}, {"verified_only": True}):
            yield "browse", dict(a, plugins=rows), \
                lambda rows=rows, a=a: [p.id for p in store.browse(_plugins(rows), **a)]
    for rows in ([row(id=1, name="One"), row(id="1", name="one")], [row(id=1.0, name=3)]):
        for a in ({"query": "one"}, {"query": "1"}, {"query": "x"}):
            yield "browse", dict(a, plugins=rows), \
                lambda rows=rows, a=a: [p.id for p in store.browse(_plugins(rows), **a)]
    yield "trust_counts", {"plugins": [row(repo=None, market="omarchy")]}, \
        lambda: store.trust_counts(_plugins([row(repo=None, market="omarchy")]))
    yield "trust_counts", {"plugins": [row(upstream_status="failed", upstream_checked_at=7)]}, \
        lambda: store.trust_counts(_plugins([row(upstream_status="failed", upstream_checked_at=7)]))


def _annotated(rows):
    ps = _plugins(rows)
    store.annotate(ps)
    return ps
