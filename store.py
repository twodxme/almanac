"""Plugin marketplaces, as the marketplaces themselves publish them.

plugins.omarchy.org builds a catalog.json for its own front end: 5,600+ entries
with the name, description, author, category, tags, star count, verification
state, install command and card artwork already resolved. Reading that is what
makes the cards here look like the cards there.

The alternative -- walking the raw registry.json -- gives repo URLs and little
else; names have to be guessed from directory names and descriptions do not
exist at all. That is the difference between a storefront and a list of links.

Structure
---------
Everything is keyed on a `Market`, because more than one marketplace is coming.
A market owns its catalog URL, its cache file, and the function that maps its
raw entries onto `Plugin`. Adding one means adding a MARKETS entry and a
mapper; nothing in the UI has to learn about it, and plugins stay attributable
to where they came from -- which is what makes provenance sortable rather than
decorative.

Each catalog is stripped to the fields the cards use on the way into its cache.
The Omarchy one is 14MB raw and about a tenth of that once stripped.
"""
from __future__ import annotations

import hashlib
import json
import re
import os
import subprocess
import threading
import time
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path

UA = {"User-Agent": "almanac"}

CACHE = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")) / "almanac"
THUMBS = CACHE / "thumbs"
TTL = 6 * 3600                          # the sites regenerate a few times a day
# Bumped whenever a mapper starts keeping a new field, so existing caches are
# refetched instead of silently serving entries that are missing it.
SCHEMA = 4

# The six accents the Omarchy catalog assigns. Used for the initials tile on
# the ~800 entries with no preview image, so a card without artwork still reads
# as a card rather than as a blank.
ACCENTS = {
    "cyan":   ("#2dd4bf", "#0e7490"),
    "coral":  ("#fb7185", "#9f1239"),
    "lime":   ("#a3e635", "#3f6212"),
    "rose":   ("#f472b6", "#9d174d"),
    "amber":  ("#fbbf24", "#b45309"),
    "violet": ("#a78bfa", "#5b21b6"),
}
DEFAULT_ACCENT = ("#94a3b8", "#334155")


@dataclass(frozen=True)
class Trust:
    """What is actually known about an entry's origin.

    `rank` orders the provenance sort: 0 is the strongest claim. The labels
    are deliberately specific -- "Reviewed · changed since" and "Verified"
    are different promises, and collapsing both to a tick would overstate one
    of them.
    """
    key: str
    label: str
    css: str
    why: str
    rank: int


TRUST_ORDER = ("official", "first-party", "reviewed", "verified",
               "reviewed-stale", "stale", "unverified", "unreachable")


@dataclass
class Plugin:
    id: str
    name: str
    description: str = ""
    author: str = ""
    version: str = ""
    category: str = "Other"
    kind: str = ""
    status: str = ""
    tags: list = field(default_factory=list)
    accent: str = ""
    initials: str = ""
    repo: str = ""
    stars: int = 0
    verified: bool = False
    installable: bool = False
    install_cmd: str = ""
    install_note: str = ""
    license: str = ""
    updated: str = ""
    added: str = ""
    thumb: str = ""
    preview: str = ""
    built_in: bool = False

    # How this entry is installed. Marketplaces do not all ship the same kind
    # of thing: an Omarchy listing is a git plugin, an ArtCraft listing is a
    # signed-off AppImage release.
    install_kind: str = "plugin"        # plugin | appimage
    download_url: str = ""
    asset_name: str = ""
    size_mb: float = 0.0

    # Provenance: which marketplace listed it, who publishes it, what was
    # reviewed, and whether what you would install today is still what was
    # reviewed.
    market: str = "omarchy"
    source_type: str = ""
    source_url: str = ""
    manifest_path: str = ""
    coverage: str = ""
    review_method: str = ""
    reviewed_by: str = ""
    reviewed_at: str = ""
    validated_commit: str = ""
    validated_at: str = ""
    validated_branch: str = ""
    upstream_commit: str = ""
    upstream_checked_at: str = ""
    upstream_status: str = ""

    # filled in from the local system, not the catalog
    installed: bool = False
    enabled: bool = False
    local_version: str = ""

    # ---- marketplace ----
    @property
    def market_name(self) -> str:
        m = MARKETS.get(self.market)
        return m.name if m else self.market

    @property
    def page(self) -> str:
        m = MARKETS.get(self.market)
        return m.listing_url(self) if m else self.repo

    def asset(self, rel: str) -> str:
        m = MARKETS.get(self.market)
        return (m.site + rel) if (m and rel) else ""

    @property
    def thumb_url(self) -> str:
        return self.asset(self.thumb)

    @property
    def preview_url(self) -> str:
        return self.asset(self.preview)

    @property
    def colors(self) -> tuple[str, str]:
        return ACCENTS.get(self.accent, DEFAULT_ACCENT)

    # ---- provenance ----
    @property
    def owner(self) -> str:
        """The account that publishes it -- the thing to actually trust.

        The `author` field is self-declared in the plugin's own manifest; the
        repository owner is not. Where they disagree, this is the one that
        says where the code comes from.
        """
        parts = [x for x in self.repo.split("/") if x]
        return parts[2] if len(parts) > 2 else ""

    @property
    def host(self) -> str:
        parts = [x for x in self.repo.split("/") if x]
        return parts[1] if len(parts) > 1 else ""

    @property
    def repo_name(self) -> str:
        parts = [x for x in self.repo.split("/") if x]
        return "/".join(parts[2:4]) if len(parts) > 3 else ""

    @property
    def publisher(self) -> str:
        """One line naming where the code comes from, for the card."""
        if self.built_in:
            return "Omarchy · built in"
        if self.repo_name:
            return f"{self.host or 'github.com'}/{self.repo_name}"
        return "unknown source"

    @property
    def drifted(self) -> bool:
        """Upstream has moved past the commit the marketplace validated."""
        return bool(self.validated_commit and self.upstream_commit
                    and self.validated_commit != self.upstream_commit)

    @property
    def trust(self) -> Trust:
        def t(key, label, css, why):
            return Trust(key, label, css, why, TRUST_ORDER.index(key))

        if self.built_in:
            return t("official", "Official", "pill-accent",
                     f"Ships with Omarchy itself, listed by {self.market_name}")
        m = MARKETS.get(self.market)
        if m and m.first_party_owner and self.owner == m.first_party_owner:
            # Verified, not assumed: the listing is only first-party if the
            # repository it points at is owned by the account the marketplace
            # publishes under. A vendor catalogue listing somebody else's
            # repository is exactly the case this must not wave through.
            rel = f", release {self.version}" if self.version else ""
            return t("first-party", "First-party", "pill-accent",
                     f"Published by {m.name} itself, from "
                     f"github.com/{self.owner}{rel}")
        if self.upstream_status in ("failed", "unreachable"):
            return t("unreachable", "Source unreachable", "pill-warn",
                     f"{self.market_name} could not reach the repository on "
                     f"its last check ({self.upstream_checked_at[:10]})")
        if self.review_method == "maintainer-reviewed":
            who = f" by {self.reviewed_by}" if self.reviewed_by else ""
            when = f" on {self.reviewed_at[:10]}" if self.reviewed_at else ""
            base = f"Source reviewed{who}{when} for {self.market_name}"
            if self.drifted:
                return t("reviewed-stale", "Reviewed · changed since",
                         "pill-warn",
                         base + ", but upstream has committed since then")
            return t("reviewed", "Reviewed", "pill-ok", base)
        if self.coverage == "snapshot-verified" and not self.drifted:
            return t("verified", "Verified", "pill-ok",
                     f"{self.market_name} automated checks passed against "
                     f"commit {self.validated_commit[:7]}")
        if self.coverage in ("snapshot-verified", "update-unverified"):
            return t("stale", "Changed since review", "pill-warn",
                     f"Validated at {self.validated_commit[:7]}, but upstream "
                     f"is now at {self.upstream_commit[:7]} -- the newer "
                     f"commits have not been checked")
        return t("unverified", "Unverified", "pill-quiet",
                 f"{self.market_name} records no automated or human review")

    @property
    def haystack(self) -> str:
        return " ".join((self.name, self.id, self.description, self.author,
                         self.category, self.kind, self.repo_name,
                         self.market_name, " ".join(self.tags))).lower()

    @property
    def subtitle(self) -> str:
        bits = [self.author] if self.author else []
        if self.kind:
            bits.append(self.kind)
        return " · ".join(bits)


# ---- marketplaces ------------------------------------------------------------

def _omarchy_entry(e: dict) -> dict:
    """plugins.omarchy.org catalog.json -> the fields the cards render.

    Keeping the other 30 fields would mean caching 14MB to display none of it.
    """
    return {
        "id": e.get("id") or "",
        "name": e.get("name") or e.get("id") or "",
        "description": e.get("description") or "",
        "author": e.get("author") or "",
        "version": e.get("version") or "",
        "category": e.get("category") or "Other",
        "kind": e.get("kind") or "",
        "status": e.get("status") or "",
        "tags": e.get("tags") or [],
        "accent": e.get("accent") or "",
        "initials": e.get("initials") or "",
        "repo": e.get("repo") or "",
        "stars": int(e.get("stars") or 0),
        "verified": e.get("verificationStatus") == "verified",
        # A built-in is "installable" in the sense that it can be switched on;
        # the catalog expresses that as officialCommand rather than as an add.
        "installable": bool(e.get("installAvailable") or e.get("builtIn")),
        "install_cmd": e.get("installCommand") or e.get("officialCommand") or "",
        "install_note": e.get("installNote") or "",
        "license": e.get("license") or "",
        "updated": e.get("repositoryUpdatedAt") or "",
        "added": e.get("listedAt") or e.get("addedAt") or "",
        "thumb": e.get("previewThumbnail") or "",
        "preview": e.get("previewImage") or e.get("previewThumbnail") or "",
        "built_in": bool(e.get("builtIn")),
        "source_type": e.get("sourceType") or "",
        "source_url": e.get("sourceUrl") or "",
        "manifest_path": e.get("manifestPath") or "",
        "coverage": e.get("verificationCoverage") or "",
        "review_method": e.get("verificationMethod") or "",
        "reviewed_by": e.get("verificationReviewedBy") or "",
        "reviewed_at": e.get("verificationReviewedAt") or "",
        "validated_commit": e.get("listingValidatedCommit") or "",
        "validated_at": e.get("listingValidatedAt") or "",
        "validated_branch": e.get("listingValidatedBranch") or "",
        "upstream_commit": e.get("upstreamObservedCommit") or "",
        "upstream_checked_at": e.get("upstreamCheckedAt") or "",
        "upstream_status": e.get("upstreamCheckStatus") or "",
    }


@dataclass
class Market:
    id: str
    name: str
    site: str
    catalog_url: str = ""
    mapper: object = None               # raw entry -> dict of Plugin fields
    loader: object = None               # or: build the rows itself
    root: str = "plugins"               # key holding the entry list
    listing: str = "?plugin={id}"       # how the site addresses one entry
    # The account this marketplace publishes under, when it is a vendor
    # shipping its own software. Left empty for open marketplaces, where no
    # listing is first-party.
    first_party_owner: str = ""
    enabled: bool = True

    @property
    def cache(self) -> Path:
        return CACHE / f"catalog-{self.id}.json"

    def listing_url(self, p: Plugin) -> str:
        if not self.listing:
            return p.repo
        return self.site + self.listing.format(id=p.id)

    def fetch(self) -> list[dict]:
        if self.loader is not None:
            out = []
            for row in self.loader():
                if row.get("id"):
                    row["market"] = self.id
                    out.append(row)
            return out
        req = urllib.request.Request(self.catalog_url, headers=UA)
        with urllib.request.urlopen(req, timeout=60) as r:
            data = json.loads(r.read().decode())
        rows = data.get(self.root) if isinstance(data, dict) else data
        out = []
        for e in rows or []:
            row = self.mapper(e)
            if row.get("id"):
                row["market"] = self.id
                out.append(row)
        return out


# ---- ArtCraft -----------------------------------------------------------------
# getartcraft.com publishes no catalog endpoint, so the listing is assembled
# from the three places the facts actually live: the vendor's own /apps page
# (which apps they ship, and the hero artwork), the GitHub organisation (what
# each one is, how it is licensed, how active it is) and each repository's
# latest release (the version and the Linux AppImage to install).
ARTCRAFT_SITE = "https://getartcraft.com/"
ARTCRAFT_ORG = "storytold"

# The vendor groups these only by name, so the grouping is ours. It exists so
# ArtCraft entries land in the same category filter as everything else rather
# than in a bucket of their own.
_ARTCRAFT_CATEGORY = {
    "photocraft": "Graphics", "vectorcraft": "Graphics",
    "lightcraft": "Graphics", "designcraft": "Graphics",
    "figcraft": "Design", "cadcraft": "Design",
    "filmcraft": "Video", "effectcraft": "Video",
    "soundcraft": "Audio",
    "wordcraft": "Productivity", "gridcraft": "Productivity",
    "deckcraft": "Productivity", "pdfcraft": "Productivity",
}
_ACCENT_CYCLE = ("violet", "cyan", "coral", "lime", "amber", "rose")


def _artcraft_rows() -> list[dict]:
    import re
    from concurrent.futures import ThreadPoolExecutor

    import ghcache

    # What the last successful run found, keyed by slug. The GitHub API allows
    # 60 unauthenticated calls an hour and this costs 14 of them, so a refresh
    # that runs into the limit must keep the version it already knew rather
    # than caching a blank one for the next six hours.
    previous = {}
    try:
        blob = json.loads(MARKETS["artcraft"].cache.read_text())
        for row in (blob.get("rows") or []):
            previous[row.get("id")] = row
    except (OSError, ValueError, KeyError):
        pass

    # 1. Which apps the vendor actually ships, and what they call them.
    req = urllib.request.Request(ARTCRAFT_SITE + "apps", headers=UA)
    with urllib.request.urlopen(req, timeout=30) as r:
        html = r.read().decode("utf-8", "replace")
    names: dict[str, str] = {}
    for slug, label in re.findall(
            r'href="/apps/([a-z0-9-]+)"[^>]*>\s*([A-Za-z][A-Za-z ]{1,30}?)\s*<', html):
        names.setdefault(slug, label.strip())
    for slug in re.findall(r'href="/apps/([a-z0-9-]+)"', html):
        names.setdefault(slug, slug.title())
    if not names:
        return []

    # 2. What each repository is. One call for the whole organisation.
    repos = {}
    for r in (ghcache.org_repos(ARTCRAFT_ORG) or []):
        if isinstance(r, dict) and r.get("name"):
            repos[r["name"].lower()] = r

    # 3. The release to install, for each app at once. These are independent
    #    HTTP calls, and doing them one after another was most of the load
    #    time; cached and conditional ones return in milliseconds.
    slugs = sorted(names)
    with ThreadPoolExecutor(max_workers=8) as pool:
        releases = dict(zip(slugs, pool.map(
            lambda sl: ghcache.latest_release(f"{ARTCRAFT_ORG}/{sl}"), slugs)))

    rows = []
    for i, slug in enumerate(slugs):
        label = names[slug]
        repo = repos.get(slug, {})
        tag, published, url, asset, size = "", "", "", "", 0
        rel = releases.get(slug) or {}
        if isinstance(rel, dict):
            tag = (rel.get("tag_name") or "").lstrip("v")
            published = (rel.get("published_at") or "")[:19]
            for a in rel.get("assets", []):
                nm = a.get("name", "")
                if nm.endswith("-linux-x86_64.AppImage"):
                    url = a.get("browser_download_url", "")
                    asset = nm
                    size = round((a.get("size") or 0) / 1048576, 1)
                    break

        if not url:
            # Fall back to what was known before, so a rate-limited refresh
            # degrades to stale rather than to empty.
            old = previous.get(slug) or {}
            tag = tag or old.get("version", "")
            url = old.get("download_url", "")
            asset = old.get("asset_name", "")
            size = old.get("size_mb", 0)
            published = published or old.get("added", "")

        lic = (repo.get("license") or {}).get("spdx_id") or ""
        rows.append({
            "id": slug,
            "name": label,
            "description": repo.get("description") or "",
            "author": "ArtCraft",
            "version": tag,
            "category": _ARTCRAFT_CATEGORY.get(slug, "Graphics"),
            "kind": "Desktop app",
            "status": "Available" if url else "No Linux build",
            "tags": ["appimage", "desktop"],
            "accent": _ACCENT_CYCLE[i % len(_ACCENT_CYCLE)],
            "initials": label[:2].upper(),
            "repo": repo.get("html_url") or f"https://github.com/{ARTCRAFT_ORG}/{slug}",
            "stars": int(repo.get("stargazers_count") or 0),
            "verified": bool(url),
            "installable": bool(url),
            "install_cmd": "",          # handled in-app, not by a shell command
            "install_note": "" if url else
                            "No x86_64 AppImage in the latest release.",
            "license": "NOASSERTION" if lic == "NOASSERTION" else lic,
            "updated": repo.get("pushed_at") or "",
            "added": published,
            "thumb": f"images/apps/{slug}/hero.webp",
            "preview": f"images/apps/{slug}/hero.webp",
            "built_in": False,
            "install_kind": "appimage",
            "download_url": url,
            "asset_name": asset,
            "size_mb": size,
            "source_type": "vendor",
            "source_url": ARTCRAFT_SITE + "apps/" + slug,
            "upstream_status": "passed" if url else "",
            "upstream_checked_at": published,
        })
    return rows


MARKETS: dict[str, Market] = {
    "omarchy": Market(
        id="omarchy",
        name="Omarchy Plugins",
        site="https://plugins.omarchy.org/",
        catalog_url="https://plugins.omarchy.org/catalog.json",
        mapper=_omarchy_entry,
    ),
    "artcraft": Market(
        id="artcraft",
        name="ArtCraft",
        site=ARTCRAFT_SITE,
        loader=_artcraft_rows,
        listing="apps/{id}",
        first_party_owner=ARTCRAFT_ORG,
    ),
}


def enabled_markets() -> list[Market]:
    return [m for m in MARKETS.values() if m.enabled]


# ---- loading -----------------------------------------------------------------

_lock = threading.Lock()
_cached: list[Plugin] | None = None


def _load_market(m: Market, force: bool) -> list[dict]:
    rows = None
    fresh = m.cache.exists() and time.time() - m.cache.stat().st_mtime < TTL
    if fresh and not force:
        try:
            blob = json.loads(m.cache.read_text())
            if isinstance(blob, dict) and blob.get("schema") == SCHEMA:
                rows = blob.get("rows")
        except (OSError, ValueError):
            rows = None
    if rows is not None:
        return rows

    try:
        rows = m.fetch()
        CACHE.mkdir(parents=True, exist_ok=True)
        tmp = m.cache.with_suffix(".tmp")
        tmp.write_text(json.dumps({"schema": SCHEMA, "rows": rows}))
        tmp.replace(m.cache)
        return rows
    except Exception:
        # Offline, or the site moved. An expired cache still fills the store;
        # an empty one makes the app look broken. One market being down must
        # not take the others with it.
        try:
            blob = json.loads(m.cache.read_text())
            rows = blob.get("rows") if isinstance(blob, dict) else blob
        except (OSError, ValueError):
            rows = None
        return rows or []


def catalog(force: bool = False) -> list[Plugin]:
    """Every listed plugin from every enabled marketplace."""
    global _cached
    with _lock:
        if _cached is not None and not force:
            return _cached
        out = []
        for m in enabled_markets():
            out.extend(Plugin(**r) for r in _load_market(m, force))
        _cached = out
        return _cached


def installed_ids() -> dict[str, bool]:
    """id -> enabled, for everything the shell already knows about."""
    try:
        out = subprocess.run(["omarchy", "plugin", "list", "--json"],
                             capture_output=True, text=True, timeout=25).stdout
        items = json.loads(out)
    except Exception:
        return {}
    return {i["id"]: bool(i.get("enabled"))
            for i in items if isinstance(i, dict) and i.get("id")}


def installed_appimages() -> dict[str, str]:
    """registry key -> installed version, for the AppImages this app tracks."""
    try:
        import inventory
        return {k: (v.get("version") or "")
                for k, v in inventory._load_registry().items()}
    except Exception:
        return {}


def annotate(plugins: list[Plugin]) -> None:
    """Mark what is already on the system, so cards can say so."""
    import demo
    if demo.active():
        # The catalogue stays real -- it is public data -- but what counts as
        # installed comes from the fixtures, so a screenshot never discloses
        # what is actually on the machine taking it.
        for p in plugins:
            p.installed = (p.id in demo.INSTALLED_PLUGIN_IDS
                           or (p.install_kind == "theme"
                               and p.name.lower() in demo.INSTALLED_THEMES))
            p.enabled = p.installed
            p.local_version = p.version if p.installed else ""
            if p.install_kind == "theme":
                p.built_in = False
        return

    have = installed_ids()
    images = installed_appimages()
    has_themes = any(p.install_kind == "theme" for p in plugins)
    themes = installed_themes() if has_themes else set()
    mine = removable_themes() if has_themes else set()
    for p in plugins:
        if p.install_kind == "theme":
            p.installed = p.name.lower() in themes
            p.enabled = p.installed
            # A stock theme is "built in" in exactly the sense a built-in
            # plugin is: it is there, it works, and Remove does not apply.
            p.built_in = p.installed and p.name.lower() not in mine
        elif p.install_kind == "appimage":
            p.installed = p.id in images
            p.enabled = p.installed
            p.local_version = images.get(p.id, "")
        else:
            p.installed = p.id in have
            p.enabled = have.get(p.id, False)


# ---- sorting and filtering ---------------------------------------------------

def _ts(value: str) -> float:
    """ISO-8601 to a sortable number. Unknown dates sort oldest."""
    if not value:
        return 0.0
    try:
        from datetime import datetime
        return datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp()
    except ValueError:
        return 0.0


class _Rev(str):
    """Reverse string ordering without a second sort pass."""
    def __lt__(self, other):
        return str.__gt__(self, other)

    def __gt__(self, other):
        return str.__lt__(self, other)


SORTS = (
    # (key, label, sort function)
    ("popular",    "Most popular",  lambda p: (-p.stars, p.name.lower())),
    ("trending",   "Trending",      lambda p: (-_ts(p.updated), -p.stars)),
    ("new",        "Newest",        lambda p: (-_ts(p.added), p.name.lower())),
    ("az",         "Name (A → Z)",  lambda p: p.name.lower()),
    ("za",         "Name (Z → A)",  lambda p: _Rev(p.name.lower())),
    ("category",   "Category",      lambda p: (p.category.lower(), -p.stars)),
    # Provenance: strongest claim first, then by marketplace, then by the
    # publisher -- so everything from one account lands together.
    ("provenance", "Provenance",    lambda p: (p.trust.rank, p.market_name.lower(),
                                               p.owner.lower(), -p.stars)),
    ("market",     "Marketplace",   lambda p: (p.market_name.lower(),
                                               p.trust.rank, -p.stars)),
    ("publisher",  "Publisher",     lambda p: (p.owner.lower(), -p.stars)),
)

_SORT_FN = {k: f for k, _l, f in SORTS}


def categories(plugins: list[Plugin]) -> list[tuple[str, int]]:
    counts: dict[str, int] = {}
    for p in plugins:
        counts[p.category] = counts.get(p.category, 0) + 1
    return sorted(counts.items(), key=lambda kv: (-kv[1], kv[0]))


def market_counts(plugins: list[Plugin]) -> list[tuple[str, str, int]]:
    """(market id, name, count), busiest first."""
    counts: dict[str, int] = {}
    for p in plugins:
        counts[p.market] = counts.get(p.market, 0) + 1
    rows = [(mid, MARKETS[mid].name if mid in MARKETS else mid, n)
            for mid, n in counts.items()]
    return sorted(rows, key=lambda r: (-r[2], r[1]))


def trust_counts(plugins: list[Plugin]) -> list[tuple[str, str, int]]:
    """(trust key, label, count) in provenance order."""
    counts: dict[str, tuple[str, int]] = {}
    for p in plugins:
        t = p.trust
        label, n = counts.get(t.key, (t.label, 0))
        counts[t.key] = (label, n + 1)
    return [(k, counts[k][0], counts[k][1])
            for k in TRUST_ORDER if k in counts]


def browse(plugins: list[Plugin], query: str = "", category: str = "",
           sort: str = "popular", installed_only: bool = False,
           verified_only: bool = False, market: str = "",
           trust: str = "") -> list[Plugin]:
    q = query.strip().lower()
    out = plugins
    if market:
        out = [p for p in out if p.market == market]
    if trust:
        out = [p for p in out if p.trust.key == trust]
    if category:
        out = [p for p in out if p.category == category]
    if verified_only:
        # "Trustworthy" here means a review of some kind exists and still
        # applies to what you would install -- not merely that a tick was set.
        out = [p for p in out if p.trust.key in ("official", "reviewed", "verified")]
    if installed_only:
        out = [p for p in out if p.installed]

    fn = _SORT_FN.get(sort)
    if q:
        # Rank name matches above description matches: searching "clock" should
        # not bury the plugin called Clock under everything that mentions one.
        exact, starts, named, loose = [], [], [], []
        for p in out:
            name = p.name.lower()
            if name == q or p.id.lower() == q:
                exact.append(p)
            elif name.startswith(q):
                starts.append(p)
            elif q in name:
                named.append(p)
            elif q in p.haystack:
                loose.append(p)
        if fn:
            for bucket in (exact, starts, named, loose):
                bucket.sort(key=fn)
        return exact + starts + named + loose

    return sorted(out, key=fn) if fn else list(out)


# ---- card artwork ------------------------------------------------------------

def thumb_path(p: Plugin, detail: bool = False) -> Path | None:
    """Local path for a card image, or None when the entry has no artwork."""
    if p.install_kind == "theme":
        import themeart
        return themeart.art_path(p.repo)
    rel = p.preview if detail else p.thumb
    if not rel:
        return None
    # Keyed on the market too: two marketplaces can name an asset the same way.
    key = hashlib.sha1(f"{p.market}/{rel}".encode()).hexdigest()[:16]
    return THUMBS / f"{key}{Path(rel).suffix or '.webp'}"


def fetch_thumb(p: Plugin, detail: bool = False) -> Path | None:
    """Download a card image if it is not already cached. Blocking."""
    if p.install_kind == "theme":
        import themeart
        return themeart.thumbnail(p.repo)
    dest = thumb_path(p, detail)
    if dest is None:
        return None
    if dest.exists() and dest.stat().st_size:
        return dest
    url = p.preview_url if detail else p.thumb_url
    if not url:
        return None
    try:
        THUMBS.mkdir(parents=True, exist_ok=True)
        req = urllib.request.Request(url, headers=UA)
        with urllib.request.urlopen(req, timeout=30) as r:
            data = r.read()
        if not data:
            return None
        tmp = dest.with_suffix(dest.suffix + ".tmp")
        tmp.write_bytes(data)
        tmp.replace(dest)
        return dest
    except Exception:
        return None                     # a missing image is not an error


# ---- Omarchy themes ----------------------------------------------------------
# Themes have no catalogue of their own: omarchythemes.com renders client-side
# and ships no JSON, so the authoritative list is the `omarchy-theme` topic on
# GitHub, which is what theme authors already tag their repositories with.
#
# A theme is installed with `omarchy theme install <git-url>` and applied with
# `omarchy theme set <name>`, so unlike a plugin the install and the switch are
# two different actions -- which is why `install_kind` distinguishes them.
THEME_TOPIC = "omarchy-theme"
THEME_PAGES = 4                         # 100 per page, stars-ordered

# Repositories that carry the topic but are not themes: tooling, lists, and
# collections for other bars. Checked against the name, not the description,
# because descriptions mention "theme" constantly and legitimately.
_NOT_A_THEME = ("awesome-", "-hook", "theme-manager", "theme-switcher",
                "waybar-themes", "-templates", "themes-collection",
                "theme-builder", "-generator", "dotfiles")


def _looks_like_theme(repo: dict) -> bool:
    name = (repo.get("name") or "").lower()
    if any(bad in name for bad in _NOT_A_THEME):
        return False
    # The convention is omarchy-<something>-theme or <something>-omarchy-theme.
    return "theme" in name or "omarchy" in name


def _theme_rows() -> list[dict]:
    import ghcache

    seen, rows = set(), []
    for page in range(1, THEME_PAGES + 1):
        data = ghcache.get_json(
            f"{'https://api.github.com'}/search/repositories"
            f"?q=topic:{THEME_TOPIC}&sort=stars&order=desc"
            f"&per_page=100&page={page}")
        items = (data or {}).get("items") or []
        if not items:
            break
        for r in items:
            full = r.get("full_name", "")
            if full in seen or not _looks_like_theme(r):
                continue
            seen.add(full)
            owner = (r.get("owner") or {}).get("login", "")
            slug = (r.get("name") or "").lower()
            # omarchy-lumon-theme -> Lumon
            pretty = re.sub(r"^omarchy[-_]?|[-_]?theme$|[-_]?omarchy$", "", slug)
            pretty = pretty.replace("-", " ").replace("_", " ").strip().title()
            rows.append({
                "id": f"theme.{owner.lower()}.{slug}",
                "name": pretty or slug,
                "description": r.get("description") or "",
                "author": owner,
                "version": "",
                "category": "Appearance",
                "kind": "Theme",
                "status": "Available",
                "tags": ["theme"] + [t for t in (r.get("topics") or [])
                                     if t not in ("omarchy", THEME_TOPIC)][:2],
                "accent": "", "initials": (pretty or slug)[:2].upper(),
                "repo": r.get("html_url", ""),
                "stars": int(r.get("stargazers_count") or 0),
                "verified": False,
                "installable": True,
                "install_cmd": f"omarchy theme install {r.get('clone_url', '')}",
                "install_note": "",
                "license": ((r.get("license") or {}).get("spdx_id") or ""),
                "updated": r.get("pushed_at") or "",
                "added": r.get("created_at") or "",
                # GitHub renders a social card per repository; for a theme
                # that is usually the author's own screenshot.
                "thumb": f"1/{owner}/{r.get('name')}",
                "preview": f"1/{owner}/{r.get('name')}",
                "built_in": False,
                "install_kind": "theme",
                "source_type": "community",
                "source_url": r.get("html_url", ""),
                "upstream_status": "passed",
                "upstream_checked_at": r.get("pushed_at") or "",
            })
    return rows


MARKETS["themes"] = Market(
    id="themes",
    name="Omarchy Themes",
    site="https://opengraph.githubassets.com/",
    loader=_theme_rows,
    listing="",
)


def removable_themes() -> set:
    """Themes the user installed, as opposed to the ones Omarchy ships.

    `omarchy theme remove` only accepts user-installed themes, so a stock
    theme is treated exactly like a built-in plugin: present, applicable,
    and not yours to delete.
    """
    try:
        out = subprocess.run(["omarchy", "theme", "extras"],
                             capture_output=True, text=True, timeout=20)
        return {line.strip().lower() for line in out.stdout.splitlines()
                if line.strip()}
    except Exception:
        return set()


def installed_themes() -> set:
    """Theme names omarchy already knows about, lowercased for matching."""
    try:
        out = subprocess.run(["omarchy", "theme", "list"],
                             capture_output=True, text=True, timeout=20)
        return {line.strip().lower() for line in out.stdout.splitlines()
                if line.strip()}
    except Exception:
        return set()
