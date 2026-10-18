"""Today's picks: the one or two listings standing out from the rest today.

"Most downloaded today" is a number a store front shows and a catalogue
almost never publishes. These catalogues publish a cumulative star count and
nothing else, so every figure here is measured from somewhere real, and
labelled with what it actually is:

* **Stars and forks today**: a repository's public event feed records each
  star (a `WatchEvent`) and fork with its time, so the last 24 hours can be
  counted exactly from the first day, with no history needed. (GitHub no
  longer serves stargazer lists to anyone but the owner.) The feed holds 300
  events at most, which on a busy project can be under a day; the figure is
  then labelled with the hours it really covers, never stretched to a day.
* **Downloads**: GitHub counts downloads per release asset, cumulatively.
  Almanac keeps one snapshot a day, so the daily figure is the difference
  from the last day's count ("since yesterday"). Until there is a yesterday,
  the figure is shown as what it is: downloads of the current release.
* **Comments this week**: the repository's issue and pull request comments
  from the last seven days. A project people are talking about is a project
  people are using.

Asking GitHub about all 5,600 listings is not possible, and not polite. A
shortlist is made locally first (recently worked on, installable, reviewed,
best known per marketplace), and only that shortlist is asked about. The
answers go through ghcache, so a second look in a day costs nothing.

The two with the best blend of those signals are picked. Yesterday's picks
are marked down, so the shelf turns over rather than showing the same
heavyweight every day, and no marketplace gets both places while another has
a contender.
"""
from __future__ import annotations

import json
import math
import time
from dataclasses import dataclass, field
from typing import Any
from datetime import date, datetime, timezone

import ghcache
import xdg

DAY = 86400
SHORTLIST = 10            # with a GitHub sign-in; 4 without (60 calls an hour)
KEEP_DAYS = 8             # of download snapshots and past picks
# How much each signal counts, after scaling each to the best on the shortlist.
WEIGHTS = {"stars_today": 0.30, "forks_today": 0.10, "downloads": 0.25,
           "comments": 0.15, "stars": 0.10, "fresh": 0.10}
REPEAT_PENALTY = 0.5      # picked in the last three days
EVENT_PAGES = 3           # GitHub serves 300 events at most


@dataclass
class Signals:
    stars_today: int = -1          # -1: not known
    forks_today: int = -1
    window_hours: int = 24         # what "today" covered; less on a busy repo
    downloads: int = -1
    downloads_today: bool = False  # a measured daily figure, not a total
    downloads_label: str = ""      # "v1.4" when it is a release total
    comments: int = -1
    comments_capped: bool = False  # 100 or more
    release_total: int = -1        # the raw cumulative count, for the snapshot
    release_tag: str = ""


@dataclass
class Pick:
    plugin: Any
    signals: Signals
    score: float
    reasons: list = field(default_factory=list)   # [(number, label)]


def _state_path():
    return xdg.state_home() / "almanac" / "picks.json"


def _load() -> dict:
    try:
        data = json.loads(_state_path().read_text())
        return data if isinstance(data, dict) else {}
    except (OSError, ValueError):
        return {}


def _save(data: dict) -> None:
    try:
        p = _state_path()
        p.parent.mkdir(parents=True, exist_ok=True)
        tmp = p.with_suffix(".tmp")
        tmp.write_text(json.dumps(data))
        tmp.replace(p)
    except OSError:
        pass


def _ts(value: str) -> float:
    if not value:
        return 0.0
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp()
    except ValueError:
        return 0.0


def _github(repo_url: str) -> str:
    """owner/name for a github.com repository, '' for anything else."""
    url = (repo_url or "").strip().rstrip("/")
    if url.endswith(".git"):
        url = url[:-4]
    prefix = "https://github.com/"
    if not url.startswith(prefix):
        return ""
    parts = url[len(prefix):].split("/")
    return "/".join(parts[:2]) if len(parts) >= 2 and all(parts[:2]) else ""


def eligible(p) -> bool:
    """Something that can be featured: installable from here, not installed,
    not a theme (themes have their own page), and from a source that has been
    looked at. A standout that turns out to be unreviewed is the worst thing
    to put at the top of the landing page."""
    return (p.can_install and not p.installed
            and p.install_kind != "theme"
            and p.trust.key in ("official", "first-party", "reviewed", "verified")
            and bool(_github(p.repo)))


def shortlist(plugins, size: int, now: float | None = None) -> list:
    """The few worth asking GitHub about. Worked on in the last fortnight,
    then the best known of each marketplace in turn, so the vendor with the
    15,000-star flagship cannot fill the list by itself."""
    now = now or time.time()
    pool = [p for p in plugins if eligible(p) and now - _ts(p.updated) < 14 * DAY]
    by_market: dict[str, list] = {}
    for p in sorted(pool, key=lambda p: -p.stars):
        by_market.setdefault(p.market, []).append(p)
    out: list = []
    while len(out) < size and any(by_market.values()):
        for market in sorted(by_market):
            if by_market[market] and len(out) < size:
                out.append(by_market[market].pop(0))
    return out


def activity_today(full: str, now: float, pages: int = EVENT_PAGES):
    """Stars and forks in the last 24 hours, from the repository's event feed,
    and how many hours of it the feed actually reached back."""
    stars = forks = 0
    oldest = now
    exhausted = False                  # the feed ended: nothing older exists
    for page in range(1, pages + 1):
        rows = ghcache.get_json(
            f"{ghcache.API}/repos/{full}/events?per_page=100&page={page}",
            ttl=3 * 3600)
        if not isinstance(rows, list):
            if page == 1:
                return -1, -1, 24
            break
        for r in rows:
            at = _ts(r.get("created_at", ""))
            oldest = min(oldest, at)
            if now - at < DAY:
                stars += r.get("type") == "WatchEvent"
                forks += r.get("type") == "ForkEvent"
        if len(rows) < 100:
            exhausted = True
            break
        if now - oldest >= DAY:
            break
    if exhausted or now - oldest >= DAY:
        return stars, forks, 24
    return stars, forks, max(1, int((now - oldest) / 3600))


def comments_this_week(full: str, now: float) -> tuple[int, bool]:
    since = datetime.fromtimestamp(now - 7 * DAY, timezone.utc).strftime(
        "%Y-%m-%dT%H:%M:%SZ")
    rows = ghcache.get_json(
        f"{ghcache.API}/repos/{full}/issues/comments?since={since}&per_page=100",
        ttl=6 * 3600)
    if not isinstance(rows, list):
        return -1, False
    return len(rows), len(rows) >= 100


def release_downloads(full: str) -> tuple[int, str]:
    """Downloads of the current release's assets, and its tag."""
    rel = ghcache.latest_release(full, ttl=6 * 3600)
    if not isinstance(rel, dict) or not rel.get("assets"):
        return -1, ""
    return (sum(int(a.get("download_count") or 0) for a in rel["assets"]),
            rel.get("tag_name") or "")


def measure(p, now: float, history: dict) -> Signals:
    full = _github(p.repo)
    s = Signals()
    pages = EVENT_PAGES if ghcache.token() else 1
    s.stars_today, s.forks_today, s.window_hours = activity_today(full, now, pages)
    s.comments, s.comments_capped = comments_this_week(full, now)
    total, tag = release_downloads(full)
    s.release_total, s.release_tag = total, tag
    if total >= 0:
        # Yesterday's count of the same release, when there is one.
        today = date.fromtimestamp(now).isoformat()
        past = sorted(d for d in history if d < today)
        before = (history.get(past[-1], {}) if past else {}).get(p.id)
        if before and before[1] == tag and total >= before[0]:
            s.downloads, s.downloads_today = total - before[0], True
        else:
            s.downloads, s.downloads_label = total, tag
    return s


def _scaled(values: list[float]) -> list[float]:
    top = max(values) if values else 0
    return [v / top if top > 0 else 0.0 for v in values]


def rank(candidates: list, signals: list[Signals], now: float,
         recent_picks: set) -> list[Pick]:
    """Blend the signals. Each is scaled to the best in the same marketplace,
    so a plugin is judged against plugins and a vendor's app against the
    vendor's apps: 60 stars is a crowd for an Omarchy plugin and a quiet day
    for a 15,000-star app. A signal nobody has counts for nothing."""
    # Log scales: one viral repository should not make every other number zero.
    cols = {
        "stars_today": [math.log1p(max(s.stars_today, 0)) for s in signals],
        "forks_today": [math.log1p(max(s.forks_today, 0)) for s in signals],
        "downloads": [math.log1p(max(s.downloads, 0)) for s in signals],
        "comments": [math.log1p(max(s.comments, 0)) for s in signals],
        "stars": [math.log1p(p.stars) for p in candidates],
        "fresh": [max(0.0, 1 - (now - _ts(p.updated)) / (14 * DAY))
                  for p in candidates],
    }
    scaled: dict[str, list[float]] = {k: [0.0] * len(candidates) for k in cols}
    for market in {p.market for p in candidates}:
        idx = [i for i, p in enumerate(candidates) if p.market == market]
        for k, v in cols.items():
            for i, x in zip(idx, _scaled([v[i] for i in idx])):
                scaled[k][i] = x
    out = []
    for i, (p, s) in enumerate(zip(candidates, signals)):
        score = sum(WEIGHTS[k] * scaled[k][i] for k in WEIGHTS)
        if p.id in recent_picks:
            score *= REPEAT_PENALTY
        out.append(Pick(p, s, score, reasons(p, s)))
    out.sort(key=lambda pk: -pk.score)
    return out


def _compact(n: int) -> str:
    if n >= 1_000_000:
        return f"{n / 1_000_000:.1f}M".replace(".0M", "M")
    if n >= 1000:
        return f"{n / 1000:.1f}k".replace(".0k", "k")
    return str(n)


def reasons(p, s: Signals) -> list[tuple[str, str]]:
    """What made it stand out, as (number, what it counts), each one a fact."""
    out = []
    if s.stars_today > 0:
        noun = "star" if s.stars_today == 1 else "stars"
        out.append((f"+{_compact(s.stars_today)}",
                    f"{noun} today" if s.window_hours >= 24
                    else f"{noun} in {s.window_hours}h"))
    if s.downloads > 0:
        out.append((_compact(s.downloads),
                    "downloads since yesterday" if s.downloads_today
                    else f"downloads of {s.downloads_label}" if s.downloads_label
                    else "downloads"))
    if s.forks_today > 0 and len(out) < 2:
        noun = "fork" if s.forks_today == 1 else "forks"
        out.append((f"+{_compact(s.forks_today)}",
                    f"{noun} today" if s.window_hours >= 24
                    else f"{noun} in {s.window_hours}h"))
    if s.comments > 0:
        out.append((_compact(s.comments) + ("+" if s.comments_capped else ""),
                    "comment this week" if s.comments == 1
                    else "comments this week"))
    if p.stars:
        out.append((_compact(p.stars), "stars"))
    return out[:3]


def choose(ranked: list[Pick], limit: int = 2) -> list[Pick]:
    """The top pick, and a second from another marketplace when one is close
    enough to deserve the place; the same marketplace only as a last resort."""
    if not ranked:
        return []
    first = ranked[0]
    picked = [first]
    rest = [pk for pk in ranked[1:] if pk.score >= 0.5 * first.score]
    other = [pk for pk in rest if pk.plugin.market != first.plugin.market]
    for pk in other + [pk for pk in rest if pk not in other]:
        if len(picked) >= limit:
            break
        picked.append(pk)
    return picked


def today(plugins, now: float | None = None) -> list[Pick]:
    """Today's picks. Blocking (it may ask GitHub); call off the UI thread."""
    import demo
    import settings
    now = now or time.time()
    if demo.active():
        return demo.picks(plugins)
    if not settings.get("net_daily_picks"):
        return []
    size = SHORTLIST if ghcache.token() else 4
    candidates = shortlist(plugins, size, now)
    if not candidates:
        return []

    data = _load()
    history = data.get("downloads", {})
    signals = [measure(p, now, history) for p in candidates]

    key = date.fromtimestamp(now).isoformat()
    snap = history.setdefault(key, {})
    for p, s in zip(candidates, signals):
        # Every day, the first count seen: tomorrow's figure is measured
        # from it, whether or not today's was.
        if s.release_total >= 0 and p.id not in snap:
            snap[p.id] = [s.release_total, s.release_tag]
    past = data.get("picked", {})
    recent = {pid for day, ids in past.items()
              if day != key and now - _ts(day + "T00:00:00+00:00") < 3 * DAY
              for pid in ids}

    picked = choose(rank(candidates, signals, now, recent))
    # Today's picks stay today's picks: a reload in the afternoon must not
    # reshuffle them because a number moved.
    kept = past.get(key)
    if kept:
        by_id = {pk.plugin.id: pk for pk in rank(candidates, signals, now, set())}
        again = [by_id[i] for i in kept if i in by_id]
        if again:
            picked = again
    past[key] = [pk.plugin.id for pk in picked]

    data["downloads"] = {d: v for d, v in history.items()
                         if now - _ts(d + "T00:00:00+00:00") < KEEP_DAYS * DAY}
    data["picked"] = {d: v for d, v in past.items()
                      if now - _ts(d + "T00:00:00+00:00") < KEEP_DAYS * DAY}
    _save(data)
    return picked
