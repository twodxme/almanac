"""Enumerate installed software across every source on this machine.

Deliberately read-only. Nothing here mutates package state -- the GUI delegates
every install/update to the tool that actually owns it (pacman, yay, omarchy
update). The one exception is AppImages, which no package manager tracks, so
this module reads a registry that the app itself owns.
"""
from __future__ import annotations

import json
import os
import re
import time
import subprocess
from dataclasses import dataclass, field
from typing import Any
from pathlib import Path

# Runtime data, deliberately not where the code lives.
#
# Almanac installs as a git checkout that updates itself, so anything it
# writes has to sit outside that checkout -- otherwise `git pull` is fighting
# the app's own state, and a fresh clone would have 180MB of someone else's
# AppImages in it. Everything else derives from this one path.
STATE = Path(
    os.environ.get("XDG_STATE_HOME", Path.home() / ".local/state")
) / "almanac"
REGISTRY = STATE / "appimages.json"

# Sources, in the order they should appear.
REPO, AUR, APPIMAGE, LOCAL, FLATPAK = "repo", "aur", "appimage", "local", "flatpak"
ORPHAN = "orphan"
SYSTEM = "system"
# AI coding agents: installed by their own scripts or a version manager, so no
# package manager reports them. See agents.py.
AGENT = "agent"
# Version-managed developer tools: mise, asdf and friends own software that
# no package manager reports on.
TOOL = "tool"


# Updates worth pulling out of the crowd. Two kinds:
#   security   — network-facing or credential-handling; a stale one is exposure
#   core       — can stop the machine booting, or needs a reboot to take effect
CRITICAL = {
    "security": (
        "chromium", "google-chrome", "firefox", "brave", "vivaldi", "opera",
        "openssl", "openssh", "gnutls", "curl", "wget", "nss", "ca-certificates",
        "sudo", "polkit", "1password", "bitwarden", "keepassxc", "gnupg",
        "wpa_supplicant", "networkmanager", "dnsmasq", "nginx", "openvpn",
        "wireguard-tools", "tailscale", "nordvpn-bin",
    ),
    "core": (
        "linux", "linux-lts", "linux-zen", "linux-firmware", "glibc", "systemd",
        "mkinitcpio", "limine", "grub", "mesa", "pacman", "btrfs-progs",
    ),
}


def criticality(name: str) -> str:
    """'security', 'core', or '' -- empty for the ordinary majority."""
    base = name.removesuffix("-bin").removesuffix("-git").lower()
    for kind, names in CRITICAL.items():
        if base in names or name.lower() in names:
            return kind
    return ""


STALE_DAYS = 180

# Things that legitimately stop changing. A font or an icon theme two years
# old is finished, not abandoned, and flagging it would train the user to
# ignore the warning that matters.
_FINISHED_PREFIXES = ("ttf-", "otf-", "noto-", "adobe-source-")
_FINISHED_SUFFIXES = ("-font", "-fonts", "-icon-theme", "-themes", "-cursor-theme")


def _staleness(name: str, source: str, aur: dict, desk: dict, meta: dict) -> str:
    """'orphaned' | 'flagged' | 'stale' | '' — strongest signal wins.

    Only the first two are authoritative: a human either abandoned the package
    or flagged it out of date. 'stale' is a build-date proxy and is deliberately
    limited to real applications, because libraries and fonts sit untouched for
    years as a matter of course.
    """
    rec = aur.get(name)
    if source == AUR and rec is not None and not rec["maintainer"]:
        return "orphaned"
    if rec and rec["out_of_date"]:
        return "flagged"

    lower = name.lower()
    if lower.startswith(_FINISHED_PREFIXES) or lower.endswith(_FINISHED_SUFFIXES):
        return ""
    if not (desk.get("icon") or desk.get("categories")):
        return ""                      # library or CLI tool, not an app
    built = date_ts(meta.get("built", ""))
    if built and (time.time() - built) / 86400 > STALE_DAYS:
        return "stale"
    return ""


def aur_health(names: list[str]) -> dict[str, dict]:
    """Ask the AUR about its own packages: flagged out-of-date, and when the
    packaging was last touched.

    This is the only *authoritative* staleness signal available -- a human
    maintainer flags out-of-date, and LastModified is real packaging activity.
    Build age (below) is only a proxy. One RPC call covers every package.
    """
    if not names:
        return {}
    import urllib.parse
    import urllib.request
    query = "&".join(f"arg[]={urllib.parse.quote(n)}" for n in names)
    url = f"https://aur.archlinux.org/rpc/v5/info?{query}"
    try:
        with urllib.request.urlopen(url, timeout=15) as r:
            data = json.loads(r.read().decode())
    except Exception:
        return {}                  # offline is not a reason to show nothing
    out = {}
    for rec in data.get("results", []):
        out[rec.get("Name", "")] = {
            "out_of_date": bool(rec.get("OutOfDate")),
            "last_modified": float(rec.get("LastModified") or 0),
            "maintainer": rec.get("Maintainer") or "",
        }
    return out


def size_mb(value: str) -> float:
    """'1737.27 KiB' -> 1.70. Sorting needs a number: as text, '9 KiB' sorts
    above '10 MiB'."""
    try:
        num, unit = value.split()[:2]
        return {"B": float(num) / 1048576, "KiB": float(num) / 1024,
                "MiB": float(num), "GiB": float(num) * 1024}.get(unit, 0.0)
    except (ValueError, IndexError):
        return 0.0


def date_ts(value: str) -> float:
    """pacman's 'Fri 16 Oct 2026 04:08:05 PM PDT' -> epoch seconds, so the
    column sorts chronologically instead of by the weekday name."""
    if not value:
        return 0.0
    from datetime import datetime
    for fmt, count in (("%d %b %Y %I:%M:%S %p", 6), ("%d %b %Y", 4)):
        try:
            return datetime.strptime(" ".join(value.split()[1:count]), fmt).timestamp()
        except (ValueError, IndexError):
            continue
    return 0.0


@dataclass
class App:
    name: str
    version: str
    source: str
    description: str = ""
    update: str = ""              # new version if one is available
    exec_path: str = ""           # what to launch, when launchable
    extra: dict = field(default_factory=dict)

    @property
    def has_update(self) -> bool:
        return bool(self.update)


def _run(cmd, timeout=25) -> str:
    """Run a command, returning stdout. Never raises: a missing or failing
    helper should degrade one section of the list, not take the window down."""
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        return r.stdout
    except (OSError, subprocess.SubprocessError):
        return ""


# pacman -Qi field -> the key we keep it under.
_FIELDS = {
    "Description": "description",
    "URL": "url",
    "Groups": "groups",
    "Installed Size": "size",
    "Build Date": "built",
    "Install Date": "installed",
    "Licenses": "license",
}


def _package_info(names: list[str]) -> dict[str, dict]:
    """Full metadata for every package in ONE pacman call.

    pacman -Qi emits "Field : value" blocks separated by blank lines. Values
    can wrap onto continuation lines (long dependency lists), which is why a
    naive line-per-field parse drops data -- continuation lines are appended
    to the field already in progress.
    """
    if not names:
        return {}
    out = _run(["pacman", "-Qi", *names], timeout=60)
    info: dict[str, dict] = {}
    current, field = None, None
    for line in out.splitlines():
        if not line.strip():
            current, field = None, None
            continue
        if line.startswith(" ") and current and field:
            info[current][field] += " " + line.strip()
            continue
        if ":" not in line:
            continue
        key, _, value = line.partition(":")
        key, value = key.strip(), value.strip()
        if key == "Name":
            current = value
            info[current] = {}
            field = None
        elif current and key in _FIELDS:
            field = _FIELDS[key]
            info[current][field] = value
    return info


def _desktop_map(names: list[str]) -> dict[str, dict]:
    """package -> {icon, categories} by reading the .desktop files it owns.

    One `pacman -Ql` for the whole set, then read only the desktop files it
    points at. Looking each package up individually would be far slower.
    """
    if not names:
        return {}
    owns: dict[str, list[str]] = {}
    for line in _run(["pacman", "-Ql", *names], timeout=60).splitlines():
        pkg, _, path = line.partition(" ")
        if path.endswith(".desktop") and "/applications/" in path:
            owns.setdefault(pkg, []).append(path)

    result = {}
    for pkg, paths in owns.items():
        for path in paths:
            try:
                text = Path(path).read_text(errors="replace")
            except OSError:
                continue
            icon = cats = ""
            for line in text.splitlines():
                if line.startswith("Icon=") and not icon:
                    icon = line.split("=", 1)[1].strip()
                elif line.startswith("Categories=") and not cats:
                    cats = line.split("=", 1)[1].strip()
                elif line.startswith("NoDisplay=true"):
                    icon = cats = ""      # helper entry, not a real app
                    break
            if icon or cats:
                result[pkg] = {"icon": icon, "categories": cats}
                break                     # first real entry wins
    return result


def _pending_repo() -> dict[str, str]:
    """name -> new version, from checkupdates (which never touches the db)."""
    pending = {}
    for line in _run(["checkupdates"], timeout=60).splitlines():
        parts = line.split()
        if len(parts) >= 4:
            pending[parts[0]] = parts[3]
    return pending


def synced_version(name: str) -> str:
    """The version pacman's own sync database offers for `name`, or ''.

    checkupdates reads a private, freshly synced copy of the database, so it
    can see a release the system database has not heard of yet. Installing
    one package without -y uses the system copy, and against that copy the
    package is already current -- pacman skips it and reports success.
    """
    for line in _run(["env", "LC_ALL=C", "pacman", "-Si", name]).splitlines():
        if line.startswith("Version"):
            return line.split(":", 1)[1].strip()
    return ""


def _pending_aur() -> dict[str, str]:
    pending = {}
    for line in _run(["yay", "-Qua"], timeout=90).splitlines():
        parts = line.split()
        if len(parts) >= 4:
            pending[parts[0]] = parts[3]
    return pending


# Omarchy's own packages: the distro updating itself, which deserves to be
# visible rather than buried as somebody's dependency.
_OMARCHY_PREFIX = "omarchy"


def packages() -> list[App]:
    """Explicitly-installed packages, plus anything with an update waiting.

    Listing only explicit installs keeps the dependency closure out of the
    way -- nobody installed libjpeg on purpose. But a dependency with an
    update pending is still something the user has to act on, and omarchy
    itself arrives that way, so pending updates are always included.
    """
    repo_names = _run(["pacman", "-Qentq"]).split()
    aur_names = _run(["pacman", "-Qemq"]).split()
    versions = dict(
        line.split()[:2] for line in _run(["pacman", "-Q"]).splitlines() if line.split()
    )
    # Pull in any non-explicit package that has an update waiting, so the
    # list can never disagree with "Update all".
    up_repo_pre, up_aur_pre = _pending_repo(), _pending_aur()
    known = set(repo_names) | set(aur_names)
    foreign = set(_run(["pacman", "-Qmq"]).split())
    for name in (set(up_repo_pre) | set(up_aur_pre)) - known:
        (aur_names if name in foreign else repo_names).append(name)

    all_names = repo_names + aur_names
    info = _package_info(all_names)
    desktops = _desktop_map(all_names)
    aur = aur_health(aur_names)
    up_repo, up_aur = up_repo_pre, up_aur_pre
    explicit = set(_run(["pacman", "-Qentq"]).split()) | set(_run(["pacman", "-Qemq"]).split())

    # "Security update" means the update fixes a known vulnerability in the
    # installed version, per the Arch Security Tracker. Only when the tracker
    # cannot be reached does the old guess-by-name stand in.
    import security
    vulns = security.vulnerabilities(installed=versions)
    vuln_map = security.by_package(vulns) if vulns is not None else None

    def critical_for(name: str, pend: str) -> str:
        if not pend:
            return ""
        if vuln_map is None:
            return criticality(name)
        if security.fixed_by(vuln_map.get(name, []), pend):
            return "security"
        return "core" if criticality(name) == "core" else ""

    apps = []
    for names, source, pending in ((repo_names, REPO, up_repo), (aur_names, AUR, up_aur)):
        for n in names:
            meta = info.get(n, {})
            desk = desktops.get(n, {})
            # Omarchy's own packages are the distro updating itself; grouping
            # them under System stops them reading as ordinary apps.
            row_source = SYSTEM if n.startswith(_OMARCHY_PREFIX) else source
            apps.append(App(
                name=n,
                version=versions.get(n, "?"),
                source=row_source,
                description=meta.get("description", ""),
                update=pending.get(n, ""),
                exec_path=n,
                extra={
                    "icon": desk.get("icon", ""),
                    "categories": desk.get("categories", ""),
                    "size": meta.get("size", ""),
                    "installed": meta.get("installed", ""),
                    "built": meta.get("built", ""),
                    "url": meta.get("url", ""),
                    "license": meta.get("license", ""),
                    # A package with a .desktop entry is something the user
                    # actually launches, rather than a toolchain or library
                    # pulled in explicitly. Worth distinguishing in the list.
                    "is_app": bool(desk.get("icon") or desk.get("categories")),
                    "dependency": n not in explicit,
                    "real_source": source,
                    # Numeric twins of size/installed, purely so the columns
                    # can sort on magnitude rather than on their text form.
                    "critical": critical_for(n, pending.get(n, "")),
                    "vulns": [{"group": v.group, "severity": v.severity,
                               "kind": v.kind, "fixed": v.fixed,
                               "cves": v.cves}
                              for v in (vuln_map or {}).get(n, [])],
                    "behind": versions_behind(versions.get(n, ""), pending.get(n, ""))
                              if pending.get(n) else (0, ""),
                    "maintainer": aur.get(n, {}).get("maintainer", ""),
                    "orphaned": source == AUR and n in aur
                                and not aur[n]["maintainer"],
                    "flagged": aur.get(n, {}).get("out_of_date", False),
                    "stale": _staleness(n, source, aur, desk, meta),
                    "size_mb": size_mb(meta.get("size", "")),
                    "installed_ts": date_ts(meta.get("installed", "")),
                    "built_ts": date_ts(meta.get("built", "")),
                },
            ))
    return apps


# ---------------------------------------------------------------- appimages --
def _load_registry() -> dict:
    try:
        return json.loads(REGISTRY.read_text())
    except (OSError, ValueError):
        return {}


def save_registry(data: dict) -> None:
    REGISTRY.parent.mkdir(parents=True, exist_ok=True)
    tmp = REGISTRY.with_suffix(".tmp")
    tmp.write_text(json.dumps(data, indent=2) + "\n")
    tmp.replace(REGISTRY)          # atomic: a crash mid-write cannot truncate it


APPIMAGE_CHECK_SECONDS = 3600


def appimages(check_updates: bool = True) -> list[App]:
    import appimage as _ai

    registry = _load_registry()
    apps, dirty = [], False
    for key, rec in sorted(registry.items()):
        path = Path(rec.get("path", ""))

        # Ask GitHub at most hourly: a rescan happens on every package change,
        # and the release feed does not move nearly that fast.
        cached = rec.get("latest") or {}
        age = time.time() - float(rec.get("checked_at") or 0)
        if check_updates and path.exists() and age > APPIMAGE_CHECK_SECONDS:
            found = _ai.check(rec)
            rec["latest"] = found
            rec["checked_at"] = time.time()
            if found.get("spec"):
                rec["update_spec"] = found["spec"]
            registry[key] = rec
            cached, dirty = found, True

        apps.append(App(
            name=rec.get("name", key),
            version=rec.get("version", "?"),
            source=APPIMAGE,
            description=rec.get("description", "AppImage"),
            update=cached.get("version", "") if cached else "",
            exec_path=str(path),
            extra={
                "present": path.exists(),
                "url": rec.get("url", "") or (cached.get("page", "") if cached else ""),
                "key": key,
                "download": cached.get("url", "") if cached else "",
                "published": cached.get("published", "") if cached else "",
                "installed_ts": path.stat().st_mtime if path.exists() else 0,
                "size_mb": path.stat().st_size / 1048576 if path.exists() else 0,
                "is_app": True,
            },
        ))
    if dirty:
        save_registry(registry)
    return apps


# Orphans that are usually wanted back. Build tooling gets pulled in by the
# next AUR package and removing it just means downloading it again; a debug
# package was almost certainly a deliberate choice.
_KEEP_ORPHANS = ("rust", "cargo", "go", "python-build", "python-installer",
                 "python-wheel", "python-setuptools", "meson", "ninja", "cmake")


def versions_behind(current: str, latest: str) -> tuple[int, str]:
    """How far behind, and at which level.

    Returns (count, kind) where kind is 'major'/'minor'/'patch'/'rel'. Only the
    first differing component is counted: 1.2.9 -> 3.0.0 is "2 major behind",
    not "2 major and 2 minor", because once the major moves the lower numbers
    restart and differencing them is meaningless.
    """
    def nums(v: str) -> list[int]:
        core = re.split(r"[-+~]", (v or "").split(":")[-1])[0]
        out = []
        for part in core.split("."):
            m = re.match(r"\d+", part)
            if not m:
                break
            out.append(int(m.group()))
        return out

    a, b = nums(current), nums(latest)
    if not a or not b:
        return (0, "")
    # Calendar versions are not release counters. Both the packed form
    # (20181225 -> 20261002, which differenced to "79777 major behind") and
    # the dotted form (2026.10.16 -> 2026.10.17, which read as "1 patch")
    # describe when a build was cut, not how many releases were missed.
    def calendar(v: list[int]) -> bool:
        if v[0] > 100000:                       # 20261002
            return True
        return (1980 <= v[0] <= 2100           # 2026.10.17
                and len(v) >= 2 and 1 <= v[1] <= 12
                and (len(v) < 3 or 1 <= v[2] <= 31))

    if calendar(a) or calendar(b):
        return (0, "")
    names = ("major", "minor", "patch")
    for i in range(max(len(a), len(b))):
        x = a[i] if i < len(a) else 0
        y = b[i] if i < len(b) else 0
        if y != x:
            return (y - x, names[i] if i < 3 else "point")
    # Same version numbers: the difference is only the pkgrel (e.g. -1 -> -2),
    # which is a rebuild, not a new upstream release.
    return (0, "rel")


def orphan_advice(name: str) -> str:
    """'keep' when removing it is likely a false economy, else 'remove'."""
    if name in _KEEP_ORPHANS or name.endswith(("-debug", "-docs")):
        return "keep"
    return "remove"


def orphans() -> list[App]:
    """Packages installed as dependencies that nothing requires any more.

    Listed separately rather than mixed into the main inventory: they were
    never chosen, so they are a cleanup decision, not software the user owns.
    """
    names = _run(["pacman", "-Qdtq"], timeout=30).split()
    if not names:
        return []
    info = _package_info(names)
    versions = dict(
        line.split()[:2] for line in _run(["pacman", "-Q", *names]).splitlines()
        if line.split())
    apps = []
    for n in names:
        meta = info.get(n, {})
        advice = orphan_advice(n)
        apps.append(App(
            name=n,
            version=versions.get(n, "?"),
            source=ORPHAN,
            description=meta.get("description", ""),
            exec_path="",
            extra={
                "size": meta.get("size", ""),
                "size_mb": size_mb(meta.get("size", "")),
                "installed_ts": date_ts(meta.get("installed", "")),
                "url": meta.get("url", ""),
                "advice": advice,
                "is_app": False,
            },
        ))
    return apps


def duplicates(apps: list[App]) -> dict[str, list[str]]:
    """Same software present twice -- an AppImage plus its packaged build, or
    a -bin and a source package side by side."""
    seen: dict[str, list[App]] = {}
    for a in apps:
        if a.source == ORPHAN:
            continue
        base = (a.name.lower().removesuffix("-bin").removesuffix("-git")
                .removesuffix("-appimage").removesuffix("-desktop"))
        seen.setdefault(base, []).append(a)
    return {k: [f"{a.name} ({a.source})" for a in v]
            for k, v in seen.items() if len(v) > 1}


def flatpaks() -> list[App]:
    """Installed Flatpak apps. Silent no-op when flatpak is not installed, so
    the section simply does not appear rather than erroring."""
    out = _run(["flatpak", "list", "--app", "--columns=application,name,version"],
               timeout=30)
    if not out.strip():
        return []
    pending = set()
    for line in _run(["flatpak", "remote-ls", "--updates", "--columns=application"],
                     timeout=60).splitlines():
        if line.strip():
            pending.add(line.strip())

    apps = []
    for line in out.splitlines():
        parts = line.split("\t")
        if not parts or not parts[0].strip():
            continue
        appid = parts[0].strip()
        name = parts[1].strip() if len(parts) > 1 else appid
        version = parts[2].strip() if len(parts) > 2 else "?"
        apps.append(App(
            name=name or appid,
            version=version or "?",
            source=FLATPAK,
            description=appid,
            update="available" if appid in pending else "",
            exec_path=appid,
            extra={"icon": appid, "is_app": True, "appid": appid},
        ))
    return apps


GIT_CHECK_SECONDS = 3600
_GIT_CACHE = Path(
    os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")) / "almanac/git.json"


def _git_cache() -> dict:
    try:
        return json.loads(_GIT_CACHE.read_text())
    except (OSError, ValueError):
        return {}


def _git_behind(repo: Path, cache: dict) -> dict:
    """How far a checkout is behind its upstream.

    Only the `git fetch` is cached -- it is a network round trip per repo, so
    it runs at most once an hour. Counting commits against the fetched refs is
    local and cheap, so it is done every time: a cached count kept a plugin
    "901 commits behind" for an hour after it had been pulled. A repo with no
    upstream, or an unreachable remote, returns nothing rather than a
    misleading zero -- "up to date" and "could not check" are different answers.
    """
    key = str(repo)
    entry = cache.get(key) or {}
    fetched_at = float(entry.get("fetched_at") or 0)

    def git(*args, timeout=25):
        return subprocess.run(["git", "-C", str(repo), *args],
                              capture_output=True, text=True, timeout=timeout)

    try:
        result: dict[str, Any]
        if git("rev-parse", "--abbrev-ref", "@{upstream}").returncode != 0:
            result = {"tracked": False}
        else:
            if time.time() - fetched_at >= GIT_CHECK_SECONDS:
                git("fetch", "--quiet", timeout=45)
                fetched_at = time.time()
            behind = git("rev-list", "--count", "HEAD..@{upstream}").stdout.strip()
            subject = git("log", "-1", "--format=%s", "@{upstream}").stdout.strip()
            dirty = bool(git("status", "--porcelain").stdout.strip())
            result = {"tracked": True, "behind": int(behind or 0),
                      "latest_subject": subject[:90], "dirty": dirty}
    except (OSError, subprocess.SubprocessError, ValueError):
        result = dict(entry) or {"tracked": False}
        result["error"] = True

    result["fetched_at"] = fetched_at
    cache[key] = result
    return result


def _github_page(url: str) -> str:
    """A git remote as a page a browser can open, or ''."""
    url = (url or "").strip().removesuffix(".git")
    if url.startswith("git@github.com:"):
        url = "https://github.com/" + url.split(":", 1)[1]
    return url if url.startswith("https://") else ""


def local_builds(check_updates: bool = True) -> list[App]:
    """Software built from source here. Listed so the inventory is honest about
    what exists; these update through their own repos, not a package manager."""
    apps = []
    cache = _git_cache() if check_updates else {}
    dirty_cache = False

    def git_extra(repo: Path) -> dict:
        nonlocal dirty_cache
        if not check_updates or not (repo / ".git").exists():
            return {}
        info = _git_behind(repo, cache)
        dirty_cache = True
        behind = info.get("behind", 0)
        return {
            "git_behind": behind,
            "git_subject": info.get("latest_subject", ""),
            "git_dirty": info.get("dirty", False),
            "git_tracked": info.get("tracked", False),
        }

    # Anything cloned straight into the data directory and built there --
    # a driver, a tool installed with `make install PREFIX=~/.local`. Found
    # by being a git checkout, not by name; Almanac's own checkout is left
    # to its self-update.
    import xdg
    own = Path(__file__).resolve().parent
    for repo in sorted(xdg.data_home().glob("*/.git")):
        repo = repo.parent
        if repo.resolve() == own:
            continue
        rev = _run(["git", "-C", str(repo), "describe", "--always", "--dirty"]).strip()
        origin = _run(["git", "-C", str(repo), "remote", "get-url", "origin"]).strip()
        g = git_extra(repo)
        launcher = xdg.bin_home() / repo.name
        apps.append(App(
            name=repo.name,
            version=rev or "git",
            source=LOCAL,
            description=("Built from source" + (f" · {origin}" if origin else "")),
            update=(f"{g['git_behind']} commits" if g.get("git_behind") else ""),
            exec_path=str(launcher) if launcher.exists() else "",
            extra={**g, "repo": str(repo), "is_app": False},
        ))
    # Omarchy shell plugins: classified before anything offers to touch them
    # (plugins.py), with whether Omarchy currently has each one switched on.
    import plugins as plugins_mod
    pdir = plugins_mod.plugins_dir()
    switched_on = plugins_mod.enabled() if pdir.is_dir() else {}
    pcache = cache.setdefault("_plugins", {})
    for p in sorted(pdir.glob("*")) if pdir.is_dir() else []:
        if not p.is_dir():
            continue
        desc, ver, pid = "Omarchy shell plugin", "—", p.name
        try:
            m = json.loads((p / "manifest.json").read_text())
            desc = m.get("description", desc)
            ver = m.get("version", ver)
            pid = m.get("id") or pid
        except (OSError, ValueError):
            pass
        st = plugins_mod.classify(p, pcache, fetch=check_updates)
        # Only a run that checked has anything worth saving; without the
        # check the cache started empty, and saving it would erase the last
        # fetch results.
        dirty_cache = dirty_cache or check_updates
        on = switched_on.get(pid)
        # An edit with an update waiting is still offered, through the
        # patches.py weighing; ahead, diverged, detached and symlinked
        # plugins are not, because no update is right for them.
        behind = st.behind if (st.updatable or st.detail == "modified") else 0
        apps.append(App(
            name=p.name, version=ver, source=LOCAL,
            description=(desc if on is not False else f"Turned off · {desc}"),
            update=(f"{behind} commits" if behind else ""),
            extra={"repo": str(p), "is_app": False, "plugin_id": pid,
                   "plugin_state": st.kind, "plugin_detail": st.describe(),
                   "enabled": on, "url": _github_page(st.url),
                   "git_behind": behind, "git_dirty": st.detail == "modified",
                   "git_tracked": st.kind not in (plugins_mod.LOCAL,),
                   "git_subject": ""},
        ))

    if dirty_cache:
        try:
            _GIT_CACHE.parent.mkdir(parents=True, exist_ok=True)
            _GIT_CACHE.write_text(json.dumps(cache, indent=2))
        except OSError:
            pass                        # a lost cache only costs a refetch
    return apps


def collect() -> list[App]:
    """Everything, in one list.

    The six collectors are independent and each is dominated by waiting --
    checkupdates and yay on the network, git on disk, agents on process
    start-up. Run together the total is the slowest one rather than the sum.
    """
    import demo
    if demo.active():
        return demo.collect()

    import agents
    from concurrent.futures import ThreadPoolExecutor

    import selfupdate
    # Almanac lists itself. An app whose job is telling you what is stale
    # should not be the one thing that silently is.
    sources = (packages, appimages, flatpaks, local_builds,
               agents.detect, agents.mise_tools,
               lambda: [selfupdate.as_app()], orphans)
    out = []
    with ThreadPoolExecutor(max_workers=len(sources)) as pool:
        for future in [pool.submit(fn) for fn in sources]:
            try:
                out.extend(future.result())
            except Exception:
                # One collector failing must not empty the whole window; the
                # others still describe most of the system.
                continue
    return out


def details(app: App) -> dict:
    """Everything worth showing in a detail view, fetched on demand.

    Deliberately not part of collect(): dependency lists mean a pacman call
    per package, which would turn a 2-second scan into a minute for data that
    is only read one row at a time.
    """
    out = dict(app.extra)
    if app.source in (REPO, AUR, SYSTEM, ORPHAN):
        text = _run(["pacman", "-Qi", app.name], timeout=20)
        field, acc = None, {}
        for line in text.splitlines():
            if line.startswith(" ") and field:
                acc[field] += " " + line.strip()
                continue
            if ":" not in line:
                continue
            k, _, v = line.partition(":")
            field = k.strip()
            acc[field] = v.strip()
        for key in ("Depends On", "Required By", "Optional Deps", "Provides",
                    "Conflicts With", "Packager", "Architecture", "Validated By"):
            if acc.get(key) and acc[key] != "None":
                out[key] = acc[key]
        out["files"] = len(_run(["pacman", "-Qlq", app.name], timeout=20).splitlines())
    return out


# ---------------------------------------------------------------- discover --
AVAILABLE = "available"


def search_available(query: str, limit: int = 60) -> list[App]:
    """Search what could be installed, across the official repos and the AUR.

    Results are marked AVAILABLE so the UI can offer Install rather than
    Update. Anything already installed is dropped: it belongs in the main
    inventory, where its real version and update state live.
    """
    query = query.strip()
    if len(query) < 2:
        return []

    installed = set(_run(["pacman", "-Qq"]).split())
    found: list[App] = []

    # --- official repos ---
    out = _run(["pacman", "-Ss", query], timeout=40)
    name = version = repo = ""
    for line in out.splitlines():
        if not line.startswith(" "):
            head = line.split()
            if not head:
                continue
            repo_name = head[0]
            repo, _, name = repo_name.partition("/")
            version = head[1] if len(head) > 1 else ""
            continue
        if name and name not in installed:
            found.append(App(
                name=name, version=version, source=AVAILABLE,
                description=line.strip(),
                extra={"installable": True, "repo_name": repo, "origin": REPO,
                       "is_app": True},
            ))
        name = ""

    # --- AUR ---
    try:
        import urllib.parse
        import urllib.request
        url = ("https://aur.archlinux.org/rpc/v5/search/"
               + urllib.parse.quote(query) + "?by=name-desc")
        with urllib.request.urlopen(url, timeout=20) as r:
            results = json.loads(r.read().decode()).get("results", [])
    except Exception:
        results = []

    # Popularity first: on the AUR it is the only signal that separates a
    # maintained package from one person's abandoned experiment.
    results.sort(key=lambda r: -(r.get("Popularity") or 0))
    for rec in results:
        nm = rec.get("Name", "")
        if not nm or nm in installed:
            continue
        found.append(App(
            name=nm, version=rec.get("Version", ""), source=AVAILABLE,
            description=(rec.get("Description") or "").strip(),
            extra={
                "installable": True, "origin": AUR, "is_app": True,
                "votes": rec.get("NumVotes", 0),
                "popularity": round(rec.get("Popularity") or 0, 2),
                "maintainer": rec.get("Maintainer") or "",
                "orphaned": not rec.get("Maintainer"),
                "flagged": bool(rec.get("OutOfDate")),
                "url": rec.get("URL") or "",
            },
        ))
    return found[:limit]


# ------------------------------------------------- omarchy plugin registry --
PLUGIN = "plugin"
REGISTRY_URL = ("https://raw.githubusercontent.com/omacom/"
                "omarchy-plugin-marketplace/main/registry.json")
_REGISTRY_CACHE = Path(
    os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")) / "almanac/registry.json"
REGISTRY_TTL = 86400


def plugin_registry(force: bool = False) -> dict:
    """The official Omarchy plugin marketplace index.

    ~5,500 entries and a megabyte of JSON, so it is cached for a day. A failed
    fetch falls back to whatever is on disk rather than emptying the catalogue.
    """
    try:
        age = time.time() - _REGISTRY_CACHE.stat().st_mtime
        cached = json.loads(_REGISTRY_CACHE.read_text())
    except (OSError, ValueError):
        age, cached = 1e9, None

    if cached is not None and not force and age < REGISTRY_TTL:
        return cached

    try:
        import urllib.request
        with urllib.request.urlopen(REGISTRY_URL, timeout=30) as r:
            data = json.loads(r.read().decode())
        _REGISTRY_CACHE.parent.mkdir(parents=True, exist_ok=True)
        _REGISTRY_CACHE.write_text(json.dumps(data))
        return data
    except Exception:
        return cached or {}


def _plugin_name(entry: dict) -> tuple[str, str, str]:
    """(name, description, author) for a registry entry.

    Only a couple of entries ship catalog metadata, so for the rest the repo
    URL is all there is -- the last path segment is the plugin's identity.
    """
    cat = entry.get("catalog")
    if isinstance(cat, list) and cat:
        cat = cat[0]
    if isinstance(cat, dict) and cat.get("name"):
        return (cat["name"], cat.get("description", ""), cat.get("author", ""))

    repo = (entry.get("repo") or "").rstrip("/")
    parts = repo.split("/")
    name = parts[-1] if parts else repo
    author = parts[-2] if len(parts) >= 2 else ""
    return (name, "", author)


class _reverse(str):
    """Sorts as its own reverse, so a date field reads newest-first."""
    def __lt__(self, other): return str.__gt__(self, other)
    def __gt__(self, other): return str.__lt__(self, other)


def search_plugins(query: str, limit: int = 40) -> list[App]:
    """Search the official Omarchy plugin marketplace.

    Retired plugins are excluded: they were delisted, so offering them would
    be recommending something the registry has withdrawn.
    """
    reg = plugin_registry()
    sources = reg.get("sources") or []
    if not sources:
        return []

    retired = set(reg.get("retiredPluginIds") or [])
    import xdg
    installed = {d.name for d in (xdg.config_home() / "omarchy/plugins").glob("*")
                 if d.is_dir()}
    q = query.strip().lower()
    out = []
    for entry in sources:
        repo = entry.get("repo") or ""
        name, desc, author = _plugin_name(entry)
        hay = f"{name} {desc} {author} {repo}".lower()
        if q and q not in hay:
            continue
        if name in retired:
            continue
        out.append(App(
            name=name,
            version=entry.get("type", "plugin"),
            source=PLUGIN,
            description=desc or f"Omarchy plugin — {author}" if author else "Omarchy plugin",
            extra={
                "installable": True, "origin": PLUGIN, "is_app": True,
                "url": repo, "repo": repo, "author": author,
                # A validated listing has been checked by the marketplace;
                # worth showing, since anyone can submit a repo.
                "verified": bool(entry.get("listingValidatedAt")),
                "added": (entry.get("addedAt") or "")[:10],
                "suite": entry.get("type") == "suite",
                "already": name in installed,
            },
        ))
        # Browsing (no query) needs the whole set available to sort before
        # trimming, or "most recent" would only be the most recent of an
        # arbitrary first slice.
        if q and len(out) >= limit * 3:
            break

    # Verified listings first, then the ones with real metadata.
    out.sort(key=lambda a: (not a.extra["verified"], not a.extra["added"],
                            _reverse(a.extra["added"]), a.name.lower()))
    return out[:limit]
