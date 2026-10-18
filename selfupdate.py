"""Almanac updating Almanac.

An app whose whole purpose is telling you what is out of date should not be
the one thing on the machine that quietly isn't. It appears in its own Updates
list like anything else.

How it updates depends on how it was installed, and that is detected rather
than assumed:

* **git checkout** -- the normal case. `git fetch` then a fast-forward pull.
  --ff-only is the point: if local edits or a diverged history would make the
  pull a merge, it refuses and says so instead of creating a conflict inside a
  running application.
* **a distribution package** -- pacman owns it, so this stays out of the way
  and lets the package appear in the ordinary update list.
* **neither** -- a plain directory someone copied. There is nothing to pull,
  so it says that rather than inventing a mechanism.

The running process is never swapped underneath itself: new code takes effect
on the next launch, and the app offers to restart rather than pretending the
update already applied.
"""
from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent
NAME = "Almanac"
# Bumped by hand at release. git describe supersedes it in a checkout, so this
# is only what a tarball install reports.
VERSION = "1.7.0"

GIT, PACKAGE, PLAIN = "git", "package", "plain"


METAINFO = "io.github.twodxme.Almanac.metainfo.xml"


def release_notes() -> tuple[str, str] | None:
    """(version, AppStream description markup) of the newest release.

    Read from the same AppStream file software centres show, so the About
    window's "What's New" never disagrees with them. A checkout has it under
    docs/; a package installs it to the system metainfo directory.
    """
    import xml.etree.ElementTree as ET
    for path in (ROOT / "docs" / METAINFO, Path("/usr/share/metainfo") / METAINFO):
        try:
            release = ET.parse(path).getroot().find("releases/release")
        except (OSError, ET.ParseError):
            continue
        if release is None:
            continue
        desc = release.find("description")
        if desc is None:
            return None
        # libadwaita takes the description's children as AppStream markup.
        body = "".join(ET.tostring(child, encoding="unicode") for child in desc)
        return release.get("version", ""), " ".join(body.split())
    return None


def _git(*args, timeout=60) -> tuple[int, str]:
    try:
        out = subprocess.run(["git", "-C", str(ROOT), *args],
                             capture_output=True, text=True, timeout=timeout)
        return out.returncode, (out.stdout + out.stderr).strip()
    except (OSError, subprocess.SubprocessError) as exc:
        return 1, str(exc)


def install_kind() -> str:
    code, _ = _git("rev-parse", "--is-inside-work-tree")
    if code == 0:
        return GIT
    try:
        out = subprocess.run(["pacman", "-Qoq", str(ROOT / "app.py")],
                             capture_output=True, text=True, timeout=20)
        if out.returncode == 0 and out.stdout.strip():
            return PACKAGE
    except (OSError, subprocess.SubprocessError):
        pass
    return PLAIN


def owning_package() -> str:
    try:
        out = subprocess.run(["pacman", "-Qoq", str(ROOT / "app.py")],
                             capture_output=True, text=True, timeout=20)
        return out.stdout.strip()
    except (OSError, subprocess.SubprocessError):
        return ""


def version() -> str:
    """What is running, preferring what git actually says over the constant."""
    if install_kind() == GIT:
        code, described = _git("describe", "--tags", "--always", "--dirty")
        if code == 0 and described:
            return described
    return VERSION


def remote_url() -> str:
    code, url = _git("remote", "get-url", "origin")
    return url if code == 0 else ""


def branch() -> str:
    code, name = _git("rev-parse", "--abbrev-ref", "HEAD")
    return name if code == 0 else ""


def dirty() -> bool:
    code, out = _git("status", "--porcelain", "--untracked-files=no")
    return code == 0 and bool(out)


def check(fetch: bool = True) -> dict:
    """What an update would consist of, without performing one.

    Returns a dict with: kind, version, behind, latest, note, can_update.
    """
    kind = install_kind()
    info = {"kind": kind, "version": version(), "behind": 0, "latest": "",
            "note": "", "can_update": False, "remote": "", "dirty": False}

    if kind == PACKAGE:
        pkg = owning_package()
        info["note"] = (f"Installed as the {pkg} package — it updates with "
                        f"the rest of the system.")
        return info

    if kind == PLAIN:
        info["note"] = ("Running from a plain directory with no git "
                        "checkout, so there is nothing to pull from.")
        return info

    info["remote"] = remote_url()
    info["dirty"] = dirty()
    if not info["remote"]:
        info["note"] = ("This checkout has no 'origin' remote yet, so there "
                        "is nowhere to update from.")
        return info

    if fetch:
        code, out = _git("fetch", "--quiet", "origin", timeout=90)
        if code != 0:
            info["note"] = f"Could not reach the remote: {out.splitlines()[-1] if out else ''}"
            return info

    br = branch()
    code, counts = _git("rev-list", "--left-right", "--count",
                        f"HEAD...origin/{br}")
    if code != 0:
        info["note"] = f"No upstream branch for {br}."
        return info
    try:
        ahead, behind = (int(x) for x in counts.split())
    except ValueError:
        ahead = behind = 0

    info["behind"] = behind
    if behind:
        code, latest = _git("log", "-1", "--format=%h %s",
                            f"origin/{br}")
        info["latest"] = latest if code == 0 else ""
    if ahead and behind:
        info["note"] = (f"This checkout has {ahead} local commit(s) the "
                        f"remote does not, so it cannot fast-forward. "
                        f"Rebase or reset it by hand.")
        return info
    if info["dirty"]:
        info["note"] = ("There are uncommitted changes here. Commit or stash "
                        "them before updating, or they would be in the way "
                        "of a fast-forward.")
        return info
    if behind:
        info["can_update"] = True
        info["note"] = f"{behind} commit(s) behind origin/{br}."
    else:
        info["note"] = f"Up to date with origin/{br}."
    return info


def update_steps() -> list:
    """Steps for the task runner. Fast-forward only, deliberately."""
    return [("Updating Almanac",
             ["git", "-C", str(ROOT), "pull", "--ff-only", "--stat"])]


def restart_argv() -> list:
    """How to start the new code once it is on disk."""
    launcher = Path.home() / ".local/bin/almanac"
    if launcher.exists():
        return [str(launcher)]
    return [sys.executable, str(ROOT / "app.py")]


def restart() -> None:
    """Hand off to a fresh process and let this one go.

    A running Python process cannot adopt new source, so the only honest way
    to apply an update is to start again.
    """
    argv = restart_argv()
    try:
        subprocess.Popen(argv, start_new_session=True,
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    except OSError:
        return
    os._exit(0)


def as_app(check_updates: bool = True):
    """Almanac as an entry in its own inventory."""
    import inventory

    info = check(fetch=check_updates)
    update = ""
    if info["can_update"]:
        update = f"{info['behind']} commit(s)"
    return inventory.App(
        name=NAME,
        version=info["version"],
        source=inventory.SYSTEM,
        description=f"This application · {info['note']}",
        update=update,
        exec_path=str(Path.home() / ".local/bin/almanac"),
        extra={
            "key": "almanac",
            "self": True,
            "kind": info["kind"],
            "repo": str(ROOT),
            "url": info["remote"].replace(".git", "")
                   if info["remote"].startswith("http") else "",
            "note": info["note"],
            "latest": info["latest"],
            "git_behind": info["behind"],
            "git_dirty": info["dirty"],
            "is_app": True,
        },
    )
