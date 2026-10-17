"""The landing page: what deserves attention right now.

A "featured" section is usually an editorial list someone has to keep writing.
This one is assembled from what is true on the machine and in the marketplace
at the moment it is opened, which means it is never stale and never a lie.

Four shelves, in the order a person actually cares about them:

1. **Needs you** — things that are wrong now: security updates, failed
   services, sources the marketplace can no longer reach. This is the only
   shelf that can be empty and that being empty is the good outcome.
2. **New in the marketplace** — listed in the last few weeks, and filtered to
   what can actually be installed and has been reviewed, because "new" plus
   "unverified" is the worst combination to put in front of someone.
3. **Trending** — upstream activity in the last week, ranked by stars. Recent
   commits on a popular plugin is the honest version of trending; a "views"
   counter is not something this app can see or should guess at.
4. **Because you run X** — the tags of what is already installed, matched
   against what is not. No model, no profile, no tracking: a set
   intersection over the local plugin list.
"""
from __future__ import annotations

import time
from dataclasses import dataclass, field


@dataclass
class Shelf:
    key: str
    title: str
    subtitle: str
    items: list = field(default_factory=list)
    empty: str = "Nothing here."


@dataclass
class Item:
    title: str
    detail: str
    severity: str = ""          # crit | warn | "" for neutral
    plugin: object = None       # a store.Plugin, when it is a listing
    app: object = None          # an inventory.App, when it is installed
    action: str = ""            # button label
    kind: str = ""


DAY = 86400
# No single marketplace may own a shelf. Star counts are not comparable
# across them -- a vendor's flagship app carries 15,000 stars where the
# busiest Omarchy plugin carries 2,151 -- so an uncapped ranking is really
# just "whichever catalogue has the biggest projects in it".
PER_MARKET = 3


def _diversify(ranked, limit: int, per_market: int = PER_MARKET):
    seen: dict[str, int] = {}
    out = []
    for p in ranked:
        market = getattr(p, "market", "")
        if seen.get(market, 0) >= per_market:
            continue
        seen[market] = seen.get(market, 0) + 1
        out.append(p)
        if len(out) >= limit:
            break
    return out


def _ts(value: str) -> float:
    if not value:
        return 0.0
    try:
        from datetime import datetime
        return datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp()
    except ValueError:
        return 0.0


def needs_you(apps, findings) -> Shelf:
    """Only genuine problems. A long list here would defeat the purpose."""
    items = []

    for a in apps or []:
        if a.extra.get("critical") == "security" and a.has_update:
            items.append(Item(
                title=a.name,
                detail=f"Security update available · {a.version} → {a.update}",
                severity="crit", app=a, action="Update", kind="update"))

    for a in apps or []:
        if a.extra.get("stale") and not a.has_update:
            items.append(Item(
                title=a.name,
                detail=str(a.extra.get("stale")),
                severity="warn", app=a, kind="stale"))

    for f in findings or []:
        if f.severity in ("crit", "warn") and f.id != "disk":
            items.append(Item(
                title=f.title, detail=f.detail, severity=f.severity,
                action=f.fix_label if f.actionable else "", kind="finding"))
        elif f.id == "disk" and f.severity == "crit":
            items.append(Item(title=f.title, detail=f.detail,
                              severity="crit", kind="finding"))

    rank = {"crit": 0, "warn": 1}
    items.sort(key=lambda i: rank.get(i.severity, 2))
    return Shelf("attention", "Needs you",
                 "Problems on this machine, worst first.",
                 items[:8],
                 "Nothing is wrong right now.")


def new_listings(plugins, days: int = 21, limit: int = 8) -> Shelf:
    cutoff = time.time() - days * DAY
    fresh = [p for p in plugins
             if _ts(p.added) > cutoff and p.installable and not p.installed
             and p.trust.key in ("official", "first-party", "reviewed", "verified")]
    fresh.sort(key=lambda p: (-_ts(p.added), -p.stars))
    return Shelf(
        "new", "New in the marketplace",
        f"Listed in the last {days} days, reviewed, and installable.",
        [Item(title=p.name, detail=p.description or p.subtitle,
              plugin=p, action="Install", kind="plugin")
         for p in _diversify(fresh, limit)],
        "Nothing new has been listed recently.")


def trending(plugins, days: int = 7, limit: int = 8) -> Shelf:
    cutoff = time.time() - days * DAY
    hot = [p for p in plugins
           if _ts(p.updated) > cutoff and p.installable and not p.installed
           and p.stars >= 3
           and p.trust.key not in ("unreachable", "unverified")]
    hot.sort(key=lambda p: (-p.stars, -_ts(p.updated)))
    return Shelf(
        "trending", "Trending",
        f"Worked on in the last {days} days, by how many people have starred it.",
        [Item(title=p.name,
              detail=f"★ {p.stars} · updated {p.updated[:10]} · "
                     f"{p.description or p.subtitle}",
              plugin=p, action="Install", kind="plugin")
         for p in _diversify(hot, limit)],
        "Nothing has been updated recently.")


def because_you_run(plugins, limit: int = 8) -> Shelf:
    """Match the tags of what is installed against what is not.

    Deliberately not a recommender: it is a set intersection over tags the
    marketplace already publishes, computed locally, with nothing sent
    anywhere and nothing remembered between runs.
    """
    mine = [p for p in plugins if p.installed]
    if not mine:
        return Shelf("affinity", "Suggested", "", [],
                     "Install something and this fills in.")

    weight: dict[str, int] = {}
    for p in mine:
        for tag in p.tags:
            weight[tag] = weight.get(tag, 0) + 1
    if not weight:
        return Shelf("affinity", "Suggested", "", [],
                     "Nothing installed carries a tag to match on.")

    top = sorted(weight, key=lambda t: -weight[t])[:3]
    named = ", ".join(top)

    scored = []
    for p in plugins:
        if p.installed or not p.installable:
            continue
        if p.trust.key in ("unreachable", "unverified"):
            continue
        overlap = sum(weight.get(t, 0) for t in p.tags)
        if overlap:
            scored.append((overlap, p.stars, p))
    scored.sort(key=lambda row: (-row[0], -row[1]))
    picked = _diversify([p for _o, _s, p in scored], limit)

    return Shelf(
        "affinity", "Because of what you run",
        f"Matched on the tags you already have most of: {named}.",
        [Item(title=p.name,
              detail=f"{', '.join(p.tags)} · {p.description or p.subtitle}",
              plugin=p, action="Install", kind="plugin")
         for p in picked],
        "Nothing new matches what you already run.")


def build(apps, findings, plugins) -> list[Shelf]:
    """Every shelf, in display order. Pure; safe to call off the UI thread."""
    plugins = plugins or []
    shelves = [needs_you(apps, findings)]
    if plugins:
        shelves += [new_listings(plugins), trending(plugins),
                    because_you_run(plugins)]
    return shelves
