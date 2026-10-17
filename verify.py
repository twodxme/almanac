"""Did it work? Checked, not assumed.

A command's exit status says the command ran. It does not say the thing the
user asked for happened. Every silent failure this app has had was that gap:
`yay -S --needed` skipping a package the system database did not know was
newer, `journalctl --vacuum-size` deleting nothing without root, `paccache`
trimming a cache that had nothing left to trim -- each exited 0, each was
reported as done, and the row stayed exactly where it was.

So every action that changes something is bracketed by a probe: the same
question asked before and after ("which version is installed?", "what does
this check find?"). The answer the user sees is the difference between the
two, never the exit status.

Probes are plain callables returning something comparable. They run off the
UI thread and must never raise; a probe that cannot answer returns None, and
"could not tell" is reported as that rather than as success or failure.
"""
from __future__ import annotations

import subprocess
from dataclasses import dataclass
from pathlib import Path


@dataclass
class Outcome:
    changed: bool | None        # None: the probe could not tell
    message: str

    @property
    def ok(self) -> bool:
        return bool(self.changed)


def _out(cmd, timeout=30) -> str:
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        return r.stdout.strip() if r.returncode == 0 else ""
    except (OSError, subprocess.SubprocessError):
        return ""


# ---- probes ------------------------------------------------------------------

def installed_package(name: str):
    """The installed version of a pacman package, or '' when it is absent."""
    def probe():
        out = _out(["pacman", "-Q", name])
        return out.split()[1] if len(out.split()) == 2 else ""
    return probe


def git_head(repo: str):
    def probe():
        return _out(["git", "-C", repo, "rev-parse", "HEAD"]) or None
    return probe


def mise_version(tool: str):
    def probe():
        import agents
        return agents._mise_installed().get(tool) or None
    return probe


def binary_version(binary: str):
    def probe():
        import agents
        return agents._version_of(binary) or None
    return probe


def file_identity(path: str):
    """Size and mtime: an AppImage swapped in place changes both."""
    def probe():
        try:
            st = Path(path).stat()
            return (st.st_size, int(st.st_mtime))
        except OSError:
            return None
    return probe


def pending_updates():
    """Every repository and AUR package with an update waiting."""
    def probe():
        import inventory
        return set(inventory._pending_repo()) | set(inventory._pending_aur())
    return probe


def for_app(app):
    """The probe that answers "did this app's update land?"."""
    import inventory
    extra = app.extra or {}
    if app.source in (inventory.REPO, inventory.AUR, inventory.SYSTEM,
                      inventory.ORPHAN):
        return installed_package(app.name)
    if app.source == inventory.LOCAL and extra.get("repo"):
        return git_head(extra["repo"])
    if app.source == inventory.APPIMAGE and app.exec_path:
        return file_identity(app.exec_path)
    if extra.get("manager") == "mise":
        return mise_version(extra.get("key") or app.name)
    if extra.get("binary"):
        return binary_version(extra["binary"])
    return None


# ---- judging -----------------------------------------------------------------

def judge_app(app, before, after) -> Outcome:
    """Compare an app before and after its update."""
    if before is None or after is None:
        return Outcome(None, f"{app.name}: the update ran, but its result "
                             f"could not be checked")
    if before == after:
        return Outcome(False, f"{app.name} did not change — the update ran, "
                              f"but {app.version} is still installed")
    if isinstance(after, str) and app.update and after != app.update \
            and not after.startswith(app.update.split("-")[0]):
        return Outcome(True, f"{app.name} is now {after} "
                             f"({app.update} was expected)")
    # A git HEAD or a file identity is not something to show a person.
    if isinstance(after, str) and len(after) < 24:
        return Outcome(True, f"{app.name} {app.version} → {after}")
    return Outcome(True, f"{app.name} updated")


def judge_pending(before, after) -> Outcome:
    """Compare the pending-update set around a full upgrade."""
    if before is None or after is None:
        return Outcome(None, "The update finished; what it installed could "
                             "not be checked")
    done = before - after
    left = before & after
    if not before:
        return Outcome(None, "There was nothing to update")
    if not done:
        return Outcome(False, f"Nothing was updated — all {len(before)} "
                              f"update(s) are still waiting")
    if left:
        names = ", ".join(sorted(left)[:4]) + ("…" if len(left) > 4 else "")
        return Outcome(True, f"{len(done)} updated · {len(left)} still "
                             f"waiting: {names}")
    return Outcome(True, f"All {len(done)} update(s) installed")


def judge_finding(before, after) -> Outcome:
    """Compare a health finding before and after its fix ran."""
    import maintenance as m
    if after is None:
        return Outcome(None, f"{before.title}: done, but the check could not "
                             f"be run again")
    if after.severity == m.OK or not after.actionable:
        freed = before.size_mb - after.size_mb
        tail = f" — freed {m.fmt_size(freed)}" if freed >= 1 else ""
        return Outcome(True, f"{before.title}: resolved{tail}")
    freed = before.size_mb - after.size_mb
    if freed >= 1:
        return Outcome(True, f"{before.title}: freed {m.fmt_size(freed)}, "
                             f"but it still needs attention")
    if (after.severity, after.count) == (before.severity, before.count) \
            and abs(after.size_mb - before.size_mb) < 1:
        return Outcome(False, f"{before.title}: the fix ran but nothing "
                              f"changed — {after.detail}")
    return Outcome(True, f"{before.title}: changed, but it still needs "
                         f"attention")


def recheck(finding_id: str):
    """Run the one health check that produced `finding_id` again."""
    import maintenance
    fn = maintenance.CHECK_BY_ID.get(finding_id)
    if fn is None:
        return None
    try:
        return fn()
    except Exception:                                   # noqa: BLE001
        return None
