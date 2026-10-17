"""System maintenance checks.

Each check answers one question with a number, and offers the one command that
acts on it. That shape is deliberate: "your package cache is 4.5 GB across 528
files" is actionable, "run paccache occasionally" is not.

Three rules the checks follow:

* Never report a finding without saying what would be done about it. A warning
  with no fix is just anxiety.
* Never propose a fix that needs judgement the check has not done. Orphan
  removal keeps build tooling; cache trimming keeps the last two versions so a
  downgrade is still possible.
* Reading is free, acting is not. Every check here is read-only; the fixes run
  only when asked, through the same confirm-then-run path as everything else.
"""
from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import time
from dataclasses import dataclass, field
from pathlib import Path

import xdg

HOME = Path.home()
PACMAN_CACHE = Path("/var/cache/pacman/pkg")
YAY_CACHE = xdg.cache_home() / "yay"
TRASH = xdg.data_home() / "Trash"


def _appimage_store() -> Path:
    """Where Almanac keeps AppImages -- beside its registry, in XDG state."""
    import inventory
    return Path(inventory.REGISTRY).parent / "apps"

OK, INFO, WARN, CRIT = "ok", "info", "warn", "crit"


@dataclass
class Finding:
    id: str
    title: str
    detail: str
    severity: str = INFO
    count: int = 0
    size_mb: float = 0.0
    fix_label: str = ""
    # argv to run, or a callable taking a log function. Empty means there is
    # nothing to do but look.
    fix: object = None
    fix_note: str = ""              # what the fix actually does, for the dialog
    items: list = field(default_factory=list)
    url: str = ""

    @property
    def actionable(self) -> bool:
        return self.fix is not None and self.severity != OK


def _run(cmd, timeout=60) -> str:
    try:
        return subprocess.run(cmd, capture_output=True, text=True,
                              timeout=timeout).stdout
    except (OSError, subprocess.SubprocessError):
        return ""


def _du_mb(path: Path) -> tuple[float, int]:
    """(megabytes, file count). Walks rather than shelling out to du."""
    total = count = 0
    if not path.exists():
        return 0.0, 0
    for root, _dirs, files in os.walk(path, onerror=lambda _e: None):
        for f in files:
            try:
                total += os.lstat(os.path.join(root, f)).st_size
                count += 1
            except OSError:
                pass
    return total / 1048576, count


def fmt_size(mb: float) -> str:
    if mb >= 1024:
        return f"{mb / 1024:.1f} GB"
    if mb >= 1:
        return f"{mb:.0f} MB"
    return f"{mb * 1024:.0f} KB"


# ---- checks ------------------------------------------------------------------

_SAVED = re.compile(r"disk space saved:\s*([\d.]+)\s*([KMGT]?)i?B")


def _paccache_reclaimable(*flags: str) -> float:
    """Megabytes a paccache run with `flags` would free, from its dry run."""
    m = _SAVED.search(_run(["paccache", "-d", *flags]))
    if not m:
        return 0.0                      # "no candidate packages found"
    mult = {"": 1 / 1048576, "K": 1 / 1024, "M": 1, "G": 1024, "T": 1048576}
    return float(m.group(1)) * mult[m.group(2)]


def pacman_cache() -> Finding:
    mb, n = _du_mb(PACMAN_CACHE)
    # Judged on what the fix would free, not on the cache's size. A trimmed
    # cache is still gigabytes -- two versions of every installed package --
    # and rating it by size kept it critical after a trim that had nothing
    # left to remove.
    free = _paccache_reclaimable("-k2") + _paccache_reclaimable("-uk0")
    if free < 256:
        return Finding(
            id="pacman-cache",
            title="Package cache",
            detail=(f"{fmt_size(mb)} in {n} downloaded packages, already "
                    f"trimmed to the two newest versions of each installed "
                    f"package. That much is what makes a downgrade possible."),
            severity=OK, count=n, size_mb=mb)
    sev = CRIT if free > 4096 else WARN if free > 1024 else INFO
    return Finding(
        id="pacman-cache",
        title="Package cache",
        detail=(f"{fmt_size(mb)} in {n} downloaded packages, {fmt_size(free)} "
                f"of it older versions and packages no longer installed. "
                f"Keeping a couple of versions makes a downgrade possible; "
                f"keeping all of them since install does not."),
        severity=sev, count=n, size_mb=free,
        fix_label="Trim to 2 versions",
        # -rk2 keeps the two newest of each installed package; -ruk0 drops
        # every cached version of packages no longer installed, which can
        # never be downgraded to anyway.
        fix=["paccache", "-rk2", "-ruk0"],
        fix_note="Keeps the 2 most recent versions of installed packages and "
                 "removes cached packages that are no longer installed.")


def yay_cache() -> Finding:
    mb, n = _du_mb(YAY_CACHE)
    sev = WARN if mb > 512 else OK
    return Finding(
        id="yay-cache",
        title="AUR build cache",
        detail=(f"{fmt_size(mb)} of cloned PKGBUILDs and build artefacts in "
                f"~/.cache/yay. Rebuilt on demand; nothing installed depends "
                f"on it."),
        severity=sev, count=n, size_mb=mb,
        fix_label="Clear",
        fix=_clear_yay_cache,
        fix_note="Deletes ~/.cache/yay. The next AUR build re-clones what it "
                 "needs.")


def _clear_yay_cache(log):
    if not YAY_CACHE.exists():
        log("nothing to clear")
        return
    mb, n = _du_mb(YAY_CACHE)
    log(f"removing {YAY_CACHE} ({fmt_size(mb)}, {n} files)")
    shutil.rmtree(YAY_CACHE, ignore_errors=True)
    log("cleared")


def journal() -> Finding:
    out = _run(["journalctl", "--disk-usage"])
    m = re.search(r"([\d.]+)([KMGT])", out or "")
    mb = 0.0
    if m:
        mult = {"K": 1 / 1024, "M": 1, "G": 1024, "T": 1048576}[m.group(2)]
        mb = float(m.group(1)) * mult
    sev = WARN if mb > 800 else OK
    return Finding(
        id="journal",
        title="Systemd journal",
        detail=(f"{fmt_size(mb)} of logs. Useful for diagnosing the recent "
                f"past; months of it rarely is."),
        # What the vacuum frees, which is what the reclaimable total adds up.
        severity=sev, size_mb=max(mb - 500, 0.0),
        fix_label="Vacuum to 500 MB",
        fix=["journalctl", "--vacuum-size=500M"],
        fix_note="Deletes the oldest journal files until 500 MB remains.")


def trash() -> Finding:
    mb, n = _du_mb(TRASH / "files")
    sev = INFO if mb > 50 else OK
    return Finding(
        id="trash",
        title="Trash",
        detail=f"{fmt_size(mb)} in {n} deleted files still taking up space.",
        severity=sev, count=n, size_mb=mb,
        fix_label="Empty trash",
        fix=_empty_trash,
        fix_note="Permanently deletes everything in the trash. This cannot be "
                 "undone.")


def _empty_trash(log):
    for sub in ("files", "info"):
        d = TRASH / sub
        if not d.exists():
            continue
        for child in d.iterdir():
            try:
                if child.is_dir() and not child.is_symlink():
                    shutil.rmtree(child)
                else:
                    child.unlink()
                log(f"removed {child.name}")
            except OSError as exc:
                log(f"could not remove {child.name}: {exc}")
    log("trash emptied")


def rollbacks() -> Finding:
    """The .previous copies this app keeps after an AppImage update."""
    apps = _appimage_store()
    files = sorted(apps.glob("*.previous")) if apps.exists() else []
    mb = sum(f.stat().st_size for f in files) / 1048576 if files else 0.0
    return Finding(
        id="rollbacks",
        title="AppImage rollback copies",
        detail=(f"{len(files)} previous build(s) kept after an update, "
                f"{fmt_size(mb)}. They exist so a bad update can be reverted; "
                f"once the new version is known good they are dead weight."),
        severity=INFO if files else OK,
        count=len(files), size_mb=mb,
        items=[f.name for f in files],
        fix_label="Delete rollback copies",
        fix=_delete_rollbacks if files else None,
        fix_note="Deletes the .previous files. The installed AppImages are "
                 "untouched, but reverting an update will no longer be "
                 "possible without re-downloading.")


def _delete_rollbacks(log):
    apps = _appimage_store()
    for f in sorted(apps.glob("*.previous")):
        try:
            f.unlink()
            log(f"removed {f.name}")
        except OSError as exc:
            log(f"could not remove {f.name}: {exc}")


def failed_units() -> Finding:
    rows = []
    for scope in ([], ["--user"]):
        out = _run(["systemctl", *scope, "--failed", "--no-legend",
                    "--plain", "--no-pager"])
        for line in out.splitlines():
            name = line.split()[0] if line.split() else ""
            if name:
                rows.append(("user " if scope else "system ") + name)
    return Finding(
        id="failed-units",
        title="Failed services",
        detail=("Units that exited with an error and were not restarted. "
                "A failed unit is something that was supposed to be running."
                if rows else "No failed units."),
        severity=WARN if rows else OK,
        count=len(rows), items=rows,
        fix_label="Clear failed state",
        # reset-failed clears the flag; it does not hide a unit that fails
        # again, so the finding comes straight back if the cause remains.
        fix=["systemctl", "--user", "reset-failed"] if rows else None,
        fix_note="Clears the failed flag for user units. If the cause is still "
                 "there, the unit fails again and this reappears.")


def pacnew() -> Finding:
    files = []
    for pat in ("*.pacnew", "*.pacsave"):
        out = _run(["find", "/etc", "-name", pat, "-type", "f"], timeout=40)
        files.extend(f for f in out.splitlines() if f.strip())
    return Finding(
        id="pacnew",
        title="Unmerged config files",
        detail=("Package updates left new default configs beside yours. Until "
                "they are merged you are running old defaults, and this is "
                "where Arch upgrades usually bite."
                if files else "No .pacnew or .pacsave files in /etc."),
        severity=WARN if files else OK,
        count=len(files), items=files,
        # pacdiff is an interactive three-way merge; proposing to run it
        # unattended would be proposing to guess at the user's config.
        fix_label="", fix=None,
        fix_note="Merge these with `pacdiff` in a terminal — it needs you to "
                 "decide, file by file.")


def dangling_desktop() -> Finding:
    """Launcher entries pointing at programs that are no longer there."""
    bad = []
    for d in (xdg.data_home() / "applications",):
        if not d.exists():
            continue
        for f in sorted(d.glob("*.desktop")):
            target = ""
            try:
                for line in f.read_text(errors="replace").splitlines():
                    if line.startswith("TryExec="):
                        target = line.split("=", 1)[1]
                        break
                    if line.startswith("Exec=") and not target:
                        target = line.split("=", 1)[1].split()[0]
            except OSError:
                continue
            # Entries routinely quote the path; an unstripped quote would make
            # every single one look broken.
            target = target.strip().strip('"').strip("'")
            if not target:
                continue
            found = (shutil.which(target) if "/" not in target
                     else os.path.exists(target))
            if not found:
                bad.append(f"{f.name} → {target}")
    return Finding(
        id="dangling-desktop",
        title="Broken launcher entries",
        detail=(".desktop files whose program no longer exists. They show up "
                "in the launcher and do nothing when clicked."
                if bad else "Every launcher entry points at something real."),
        severity=INFO if bad else OK,
        count=len(bad), items=bad,
        fix_label="Remove broken entries",
        fix=_remove_dangling if bad else None,
        fix_note="Deletes only the .desktop files listed above, from "
                 "~/.local/share/applications.")


def _remove_dangling(log):
    found = dangling_desktop()
    d = xdg.data_home() / "applications"
    for row in found.items:
        name = row.split(" → ")[0]
        f = d / name
        try:
            f.unlink()
            log(f"removed {name}")
        except OSError as exc:
            log(f"could not remove {name}: {exc}")


def broken_symlinks() -> Finding:
    bad = []
    for d in (HOME / ".local/bin",):
        if not d.exists():
            continue
        for f in d.iterdir():
            if f.is_symlink() and not f.exists():
                bad.append(f"{f.name} → {os.readlink(f)}")
    return Finding(
        id="broken-symlinks",
        title="Broken symlinks on PATH",
        detail=("Links in ~/.local/bin whose target is gone. Running one is a "
                "command-not-found with a confusing name."
                if bad else "No broken links in ~/.local/bin."),
        severity=INFO if bad else OK,
        count=len(bad), items=bad,
        fix_label="Remove broken links",
        fix=_remove_broken_links if bad else None,
        fix_note="Deletes only the dangling symlinks listed above.")


def _remove_broken_links(log):
    d = HOME / ".local/bin"
    for f in list(d.iterdir()):
        if f.is_symlink() and not f.exists():
            try:
                f.unlink()
                log(f"removed {f.name}")
            except OSError as exc:
                log(f"could not remove {f.name}: {exc}")


def orphan_packages() -> Finding:
    import inventory
    apps = inventory.orphans()
    safe = [a.name for a in apps if a.extra.get("advice") != "keep"]
    kept = [a.name for a in apps if a.extra.get("advice") == "keep"]
    return Finding(
        id="orphans",
        title="Orphaned packages",
        detail=(f"{len(apps)} installed as dependencies that nothing needs any "
                f"more. {len(kept)} of them are build tooling or debug symbols "
                f"and are kept deliberately — removing rust only means "
                f"downloading it again on the next AUR build."
                if apps else "Nothing is orphaned."),
        severity=INFO if safe else OK,
        count=len(safe), items=safe,
        fix_label="Review and remove",
        fix=(["pacman", "-Rns", "--noconfirm", *safe]) if safe else None,
        fix_note="Removes only the packages listed above.")


def unmaintained_aur() -> Finding:
    """Foreign packages the AUR no longer carries, or that are flagged."""
    import inventory
    names = [n for n in _run(["pacman", "-Qmq"]).split() if n]
    if not names:
        return Finding(id="aur-health", title="AUR packages",
                       detail="No foreign packages installed.", severity=OK)
    health = inventory.aur_health(names)
    gone, flagged = [], []
    for n in names:
        h = health.get(n)
        if h is None:
            gone.append(n)
        elif h.get("out_of_date"):
            flagged.append(n)
        elif not h.get("maintainer"):
            flagged.append(f"{n} (orphaned upstream)")
    rows = [f"{n} — no longer in the AUR" for n in gone] + flagged
    return Finding(
        id="aur-health",
        title="AUR package health",
        detail=(f"{len(gone)} package(s) are installed from the AUR but no "
                f"longer listed there, and {len(flagged)} are flagged "
                f"out-of-date or have no maintainer. Neither will ever "
                f"update again on its own."
                if rows else
                f"All {len(names)} AUR packages are still listed and "
                f"maintained."),
        severity=WARN if gone else (INFO if flagged else OK),
        count=len(rows), items=rows,
        fix_label="", fix=None,
        fix_note="Decide per package: find a replacement, or keep it knowing "
                 "it is frozen.")


def _usage(path: str):
    """(free MB, total MB, percent used) or None."""
    try:
        st = os.statvfs(path)
    except OSError:
        return None
    total = st.f_blocks * st.f_frsize / 1048576
    free = st.f_bavail * st.f_frsize / 1048576
    if total <= 0:
        return None
    return free, total, 100 * (1 - free / total)


def package_integrity() -> Finding:
    """Files a package owns that are no longer where it put them.

    `pacman -Qk` reports two completely different things in the same breath:
    a file that is genuinely gone, and a file this process is not allowed to
    look at. Most of /etc and all of /boot is root-only, so an unprivileged
    run cannot see a large part of what it is checking -- and counting those
    as faults reports a healthy system as broken. They are counted and
    stated separately, never as problems.
    """
    # pacman writes the per-file warnings to stderr and the per-package
    # summary to stdout. Reading only stdout silently reports full coverage
    # of a check that could not open a quarter of what it looked at.
    try:
        proc = subprocess.run(["pacman", "-Qk"], capture_output=True,
                              text=True, timeout=240)
        out = (proc.stdout or "") + (proc.stderr or "")
    except (OSError, subprocess.SubprocessError):
        out = ""
    if not out.strip():
        return Finding(id="integrity", title="Package files",
                       detail="Could not run the integrity check.",
                       severity=OK)

    missing, unreadable = [], 0
    for line in out.splitlines():
        if "warning:" not in line or "(" not in line:
            continue
        reason = line.rsplit("(", 1)[-1].rstrip(")").strip().lower()
        body = line.split("warning:", 1)[1].strip()
        path = body.rsplit("(", 1)[0].strip()
        if "permission denied" in reason:
            unreadable += 1
        elif "no such file" in reason or "missing" in reason:
            missing.append(path)
        else:
            missing.append(f"{path} — {reason}")

    checked = sum(int(m) for m in re.findall(r"(\d+) total files", out))
    note = (f" {unreadable} file(s) could not be read without root and were "
            f"not checked." if unreadable else "")

    if missing:
        return Finding(
            id="integrity", title="Package files",
            detail=(f"{len(missing)} file(s) a package owns are missing from "
                    f"disk, out of {checked:,} checked.{note} A package that "
                    f"has lost files usually needs reinstalling."),
            severity=WARN, count=len(missing), items=missing[:40],
            fix_label="", fix=None,
            fix_note="Reinstall the owning package with "
                     "`pacman -S <name>` once you know which it is.")

    return Finding(
        id="integrity", title="Package files",
        detail=f"All {checked:,} readable files are where their packages put "
               f"them.{note}",
        severity=OK)


def disk_space() -> Finding:
    """Every filesystem that matters, not just the root one.

    Checking only / misses the ways a machine actually runs out of room: a
    full /boot stops a kernel update, and a full XDG runtime directory --
    1.6 GB of tmpfs that one runaway log can fill -- breaks logins, Wayland
    clients and system updates while `df /` still reports hundreds of
    gigabytes free.
    """
    import subprocess

    watched = [("/", "/")]
    for extra in ("/boot", "/var", "/home",
                  os.environ.get("XDG_RUNTIME_DIR") or f"/run/user/{os.getuid()}"):
        if extra and os.path.isdir(extra):
            try:
                if os.stat(extra).st_dev != os.stat("/").st_dev:
                    watched.append((extra, extra))
            except OSError:
                pass

    rows, worst, worst_pct, seen = [], OK, 0.0, set()
    for label, path in watched:
        u = _usage(path)
        if not u:
            continue
        free, total, pct = u
        # btrfs subvolumes report distinct device ids while sharing one pool,
        # so /, /home and /var would otherwise be listed three times with
        # identical figures.
        fingerprint = (round(total), round(free))
        if fingerprint in seen:
            continue
        seen.add(fingerprint)
        rows.append(f"{label}  {fmt_size(free)} free of {fmt_size(total)} "
                    f"({pct:.0f}% used)")
        # A small tmpfs fills fast and breaks more than it looks like it
        # should, so it is judged on headroom rather than percentage alone.
        tight = (total < 4096 and free < 128) or pct > 92
        level = CRIT if tight else (WARN if pct > 85 else OK)
        if (CRIT, WARN, INFO, OK).index(level) < (CRIT, WARN, INFO, OK).index(worst):
            worst = level
        worst_pct = max(worst_pct, pct)

    root = _usage("/") or (0, 0, 0)
    return Finding(
        id="disk",
        title="Disk space",
        detail=(" · ".join(rows) if rows else "Could not read filesystem stats.")
               + ("  A filesystem is nearly full; an update will fail before "
                  "it starts." if worst == CRIT else ""),
        severity=worst,
        items=rows if len(rows) > 1 else [],
        size_mb=root[0])


def runtime_logs() -> Finding:
    """Session logs in the runtime directory, which is a small tmpfs.

    Hyprland and Quickshell write per-session logs into XDG_RUNTIME_DIR. A
    display that flaps produces a hotplug event per cycle and several log
    lines per event, and the directory it lands in is typically 1.6 GB. The
    failure mode is not "logs are large" but "the next system update dies
    with No space left on device".
    """
    runtime = os.environ.get("XDG_RUNTIME_DIR") or f"/run/user/{os.getuid()}"
    if not os.path.isdir(runtime):
        return Finding(id="runtime-logs", title="Session logs",
                       detail="No runtime directory to check.", severity=OK)

    biggest, total_mb, items = [], 0.0, []
    for sub in ("hypr", "quickshell"):
        d = Path(runtime) / sub
        if not d.exists():
            continue
        mb, _n = _du_mb(d)
        total_mb += mb
        if mb >= 1:
            items.append(f"{sub}  {fmt_size(mb)}")
    for f in Path(runtime).rglob("*.log"):
        try:
            size = f.stat().st_size / 1048576
        except OSError:
            continue
        if size > 100:
            biggest.append(f"{fmt_size(size)}  {f}")

    u = _usage(runtime)
    pct = u[2] if u else 0
    sev = CRIT if pct > 90 else (WARN if (total_mb > 400 or biggest) else OK)
    return Finding(
        id="runtime-logs",
        title="Session logs",
        detail=(f"{fmt_size(total_mb)} of session logs in {runtime}, which is "
                f"{pct:.0f}% full. These are logs from this and previous "
                f"desktop sessions; nothing reads them after the session ends."
                if total_mb >= 1 else
                f"Session logs are negligible ({runtime} is {pct:.0f}% full)."),
        severity=sev, size_mb=total_mb,
        items=items + biggest,
        fix_label="Clear old session logs" if total_mb >= 1 else "",
        fix=_clear_runtime_logs if total_mb >= 1 else None,
        fix_note="Truncates the running session's log and removes log "
                 "directories from sessions that have ended. The desktop is "
                 "not restarted and nothing else is touched.")


def _clear_runtime_logs(log):
    import shutil as _sh
    runtime = Path(os.environ.get("XDG_RUNTIME_DIR") or f"/run/user/{os.getuid()}")
    live = set()
    try:
        out = subprocess.run(["hyprctl", "instances", "-j"],
                             capture_output=True, text=True, timeout=20)
        live = {i.get("instance") for i in json.loads(out.stdout or "[]")}
    except Exception:
        pass

    for d in sorted((runtime / "hypr").glob("*")):
        if not d.is_dir():
            continue
        for f in d.glob("*.log"):
            size = f.stat().st_size / 1048576 if f.exists() else 0
            if d.name in live:
                # The session is still running: truncate rather than unlink,
                # so the compositor keeps a valid file to write to.
                with open(f, "w"):
                    pass
                log(f"truncated {f.name} ({fmt_size(size)}, live session)")
            else:
                f.unlink(missing_ok=True)
                log(f"removed {f.name} ({fmt_size(size)})")
        if d.name not in live and not any(d.iterdir()):
            d.rmdir()

    qs = runtime / "quickshell/by-id"
    if qs.exists():
        cutoff = time.time() - 3 * 86400
        for d in sorted(qs.iterdir()):
            try:
                if d.is_dir() and d.stat().st_mtime < cutoff:
                    _sh.rmtree(d, ignore_errors=True)
                    log(f"removed quickshell session {d.name}")
            except OSError:
                pass
    log("done")


def config_resets() -> Finding:
    import guard
    return guard.finding()


def update_snapshots() -> Finding:
    import snapshots
    return snapshots.finding()


CHECKS = (disk_space, runtime_logs, package_integrity, pacman_cache,
          journal, yay_cache, orphan_packages,
          unmaintained_aur, failed_units, pacnew, dangling_desktop,
          broken_symlinks, rollbacks, trash, config_resets, update_snapshots)


# Which check produced which finding, so one can be asked again after its fix
# without re-running all of them (the integrity check alone reads every file
# pacman installed).
CHECK_BY_ID = {
    "disk": disk_space, "runtime-logs": runtime_logs,
    "integrity": package_integrity, "pacman-cache": pacman_cache,
    "journal": journal, "yay-cache": yay_cache, "orphans": orphan_packages,
    "aur-health": unmaintained_aur, "failed-units": failed_units,
    "pacnew": pacnew, "dangling-desktop": dangling_desktop,
    "broken-symlinks": broken_symlinks, "rollbacks": rollbacks, "trash": trash,
    "config-reset": config_resets, "snapshot": update_snapshots,
}


def run_all() -> list[Finding]:
    """Every check. Read-only; safe to run on a timer."""
    import demo
    if demo.active():
        return demo.findings()

    out = []
    for check in CHECKS:
        try:
            out.append(check())
        except Exception as exc:                        # noqa: BLE001
            out.append(Finding(id=getattr(check, "__name__", "check"),
                               title=check.__name__.replace("_", " ").title(),
                               detail=f"Check failed: {exc}", severity=INFO))
    rank = {CRIT: 0, WARN: 1, INFO: 2, OK: 3}
    out.sort(key=lambda f: (rank.get(f.severity, 3), -f.size_mb))
    return out


def reclaimable_mb(findings: list[Finding]) -> float:
    """How much space the actionable cleanups would actually free."""
    frees = {"pacman-cache", "yay-cache", "journal", "trash", "rollbacks",
             "runtime-logs"}
    return sum(f.size_mb for f in findings
               if f.id in frees and f.actionable)
