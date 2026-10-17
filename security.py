"""Known vulnerabilities in what is installed, from the Arch Security Tracker.

Until this, a "security update" meant an update to a package whose *name*
suggested exposure -- a browser, OpenSSL, sudo. That flags a routine curl
release as urgent and misses a vulnerable library nobody thinks of as
network-facing. The Arch Security Tracker publishes every advisory group
(AVG) with the packages, the affected and fixed versions, the severity and
the CVEs, as one public JSON document. Matching it against the installed
versions answers the real question: *is what I am running vulnerable, and
does an update fix it?*

* Fetched from security.archlinux.org, cached for six hours. Nothing about
  this machine is sent; the whole list is downloaded and matched locally.
* Versions are compared with pacman's own `vercmp`, the only comparison that
  agrees with pacman about epochs and pkgrels.
* When the tracker cannot be reached, callers get None -- "unknown", which
  the inventory treats differently from "no vulnerabilities".
"""
from __future__ import annotations

import json
import subprocess
import time
import urllib.request
from dataclasses import dataclass, field

URL = "https://security.archlinux.org/issues/all.json"
TTL = 6 * 3600
SEVERITY_RANK = {"Critical": 0, "High": 1, "Medium": 2, "Low": 3, "Unknown": 4}


def _cache_path():
    import xdg
    return xdg.cache_home() / "almanac" / "security.json"


@dataclass
class Vuln:
    package: str
    installed: str
    group: str                      # AVG-1234
    severity: str                   # Critical | High | Medium | Low | Unknown
    kind: str                       # "arbitrary code execution", …
    fixed: str                      # '' when no fixed version exists yet
    cves: list = field(default_factory=list)

    @property
    def url(self) -> str:
        return f"https://security.archlinux.org/{self.group}"

    @property
    def rank(self) -> int:
        return SEVERITY_RANK.get(self.severity, 4)


def advisories(ttl: int = TTL):
    """The tracker's advisory groups, or None when it cannot be reached."""
    path = _cache_path()
    cached = None
    try:
        blob = json.loads(path.read_text())
        cached = blob.get("groups")
        if time.time() - blob.get("at", 0) < ttl:
            return cached
    except (OSError, ValueError, AttributeError):
        pass
    try:
        req = urllib.request.Request(URL, headers={"User-Agent": "almanac"})
        with urllib.request.urlopen(req, timeout=30) as r:
            groups = json.loads(r.read().decode())
    except Exception:                                   # noqa: BLE001
        return cached                   # stale beats nothing; None if neither
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        tmp = path.with_suffix(".tmp")
        tmp.write_text(json.dumps({"at": time.time(), "groups": groups}))
        tmp.replace(path)
    except OSError:
        pass
    return groups


def _vercmp_all(pairs):
    """{(a, b): sign} via one `vercmp` process per pair, run concurrently."""
    from concurrent.futures import ThreadPoolExecutor

    def one(pair):
        try:
            out = subprocess.run(["vercmp", *pair], capture_output=True,
                                 text=True, timeout=10).stdout.strip()
            return pair, int(out)
        except (OSError, subprocess.SubprocessError, ValueError):
            return pair, None
    with ThreadPoolExecutor(max_workers=8) as pool:
        return dict(pool.map(one, set(pairs)))


def installed_versions() -> dict[str, str]:
    try:
        out = subprocess.run(["pacman", "-Q"], capture_output=True, text=True,
                             timeout=30).stdout
    except (OSError, subprocess.SubprocessError):
        return {}
    return dict(l.split(None, 1) for l in out.splitlines() if " " in l)


def vulnerabilities(installed: dict | None = None, groups=None) -> list[Vuln] | None:
    """Every advisory affecting an installed version. None: tracker unknown."""
    groups = advisories() if groups is None else groups
    if groups is None:
        return None
    installed = installed_versions() if installed is None else installed

    candidates = []
    pairs = []
    for g in groups:
        if g.get("status") == "Not affected":
            continue
        for pkg in g.get("packages") or []:
            have = installed.get(pkg)
            if not have:
                continue
            candidates.append((g, pkg, have))
            if g.get("fixed"):
                pairs.append((have, g["fixed"]))
            if g.get("affected"):
                pairs.append((have, g["affected"]))
    signs = _vercmp_all(pairs)

    out = []
    for g, pkg, have in candidates:
        fixed = g.get("fixed") or ""
        if fixed:
            # Vulnerable when older than the version that fixes it.
            if signs.get((have, fixed)) is None or signs[(have, fixed)] >= 0:
                continue
        else:
            # No fix yet: vulnerable when at or past the first affected one,
            # and only while the tracker still calls it vulnerable.
            if g.get("status") != "Vulnerable":
                continue
            affected = g.get("affected") or ""
            if affected and (signs.get((have, affected)) or 0) < 0:
                continue
        out.append(Vuln(pkg, have, g.get("name", ""), g.get("severity", "Unknown"),
                        g.get("type", ""), fixed, list(g.get("issues") or [])))
    out.sort(key=lambda v: (v.rank, v.package))
    return out


def by_package(vulns) -> dict[str, list[Vuln]]:
    out: dict[str, list[Vuln]] = {}
    for v in vulns or []:
        out.setdefault(v.package, []).append(v)
    return out


def fixed_by(vulns: list[Vuln], pending: str) -> list[Vuln]:
    """The vulnerabilities a pending version resolves."""
    have = [v for v in vulns if v.fixed]
    if not pending or not have:
        return []
    signs = _vercmp_all([(pending, v.fixed) for v in have])
    # 0 means "is the fixed version": falsy, so compared explicitly.
    return [v for v in have
            if signs.get((pending, v.fixed)) is not None
            and signs[(pending, v.fixed)] >= 0]


def finding():
    """The Health check row."""
    import maintenance as m

    vulns = vulnerabilities()
    if vulns is None:
        return m.Finding(
            id="vulnerabilities", title="Known vulnerabilities",
            detail="The Arch Security Tracker could not be reached, so "
                   "installed versions could not be checked.",
            severity=m.INFO)
    if not vulns:
        return m.Finding(
            id="vulnerabilities", title="Known vulnerabilities",
            detail="No installed package version has a known vulnerability "
                   "in the Arch Security Tracker.", severity=m.OK)

    groups = by_package(vulns)
    fixable = {p for p, vs in groups.items() if any(v.fixed for v in vs)}
    worst = min(v.rank for v in vulns)
    items = []
    for pkg, vs in sorted(groups.items(), key=lambda kv: min(v.rank for v in kv[1])):
        top = min(vs, key=lambda v: v.rank)
        cves = sorted({c for v in vs for c in v.cves})
        fix = (f"fixed in {top.fixed}" if top.fixed else "no fix released yet")
        items.append(f"{pkg} {top.installed} — {top.severity.lower()}, "
                     f"{top.kind or 'vulnerability'}; {fix} · "
                     f"{', '.join(cves[:3])}{'…' if len(cves) > 3 else ''}")
    n = len(groups)
    detail = (f"{n} installed package(s) have known vulnerabilities; "
              f"{len(fixable)} can be fixed by updating.")
    unfixed = n - len(fixable)
    if unfixed:
        detail += f" {unfixed} have no fixed version released yet."
    return m.Finding(
        id="vulnerabilities", title="Known vulnerabilities", detail=detail,
        # Fixable is actionable; unfixed is only worth knowing about.
        severity=(m.CRIT if fixable and worst <= 1 else
                  m.WARN if fixable else m.INFO),
        count=n, items=items,
        fix_note="Update all installs every fixed version. Packages with no "
                 "fixed version are tracked at security.archlinux.org.")
