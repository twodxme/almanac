"""Whether the last update can be undone, and undoing it.

A system that snapshots before every upgrade can roll any of them back -- but
only if the snapshot was actually taken. Full upgrades through a wrapper like
`omarchy update` or a snap-pac hook take one; a one-off `yay -S` usually does
not, and nothing tells you which kind your last update was until the day you
need to undo it.

Snapshot listings are root-only, so this never reads them. Whether a snapshot
preceded a transaction is read from what any user can see:

* pacman.log, where snap-pac and similar hooks announce themselves inside the
  transaction they protect, and
* the journal, where `sudo`/`pkexec` record a `snapper ... create` command a
  minute before an update that ran it by hand.

Rolling back is not something a running system can do to itself. Snapshots
are booted from the boot menu; once booted into one, it is made permanent.
That second step is the one-click part, and it is offered only where the tool
for it exists (limine-snapper-restore), otherwise the steps are spelled out.

Everything here is optional. Without Snapper there is nothing to report
beyond saying so.
"""
from __future__ import annotations

import re
import shutil
import subprocess
import time
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path

PACMAN_LOG = Path("/var/log/pacman.log")
# How long before a transaction a manually created snapshot still counts as
# "taken for it". omarchy update snapshots, then syncs and downloads.
LEAD = 30 * 60
_LINE = re.compile(r"^\[(?P<ts>[^\]]+)\] \[(?P<who>[^\]]+)\] (?P<msg>.*)$")
_SNAPPER_HOOK = re.compile(r"snap-pac|snapper|timeshift|btrfs-autosnap", re.I)
_CMDLINE_SNAPSHOT = re.compile(r"subvol=\S*?/(\d+)/snapshot")


def _run(cmd, timeout=20) -> str:
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        return r.stdout if r.returncode == 0 else ""
    except (OSError, subprocess.SubprocessError):
        return ""


def snapper_configs() -> list[str]:
    """Configured snapper configs. Listing them needs no root."""
    if not shutil.which("snapper"):
        return []
    out = _run(["snapper", "--csvout", "list-configs"])
    rows = [l.split(",")[0] for l in out.splitlines()[1:] if l.strip()]
    return [r for r in rows if r]


def booted_snapshot(cmdline: str | None = None) -> str:
    """The snapshot number this system was booted from, or ''."""
    if cmdline is None:
        try:
            cmdline = Path("/proc/cmdline").read_text()
        except OSError:
            return ""
    m = _CMDLINE_SNAPSHOT.search(cmdline)
    return m.group(1) if m else ""


def _ts(text: str) -> float:
    try:
        return datetime.fromisoformat(text).timestamp()
    except ValueError:
        try:
            return datetime.strptime(text, "%Y-%m-%d %H:%M").timestamp()
        except ValueError:
            return 0.0


@dataclass
class Transaction:
    started: float
    changes: list = field(default_factory=list)     # "name old -> new"
    hooks_snapshot: bool = False                    # a snapshot hook ran in it


def last_transaction(log_text: str | None = None) -> Transaction | None:
    """The most recent pacman transaction that changed something."""
    if log_text is None:
        try:
            # The tail is enough; the log grows for the life of the install.
            with PACMAN_LOG.open("rb") as fh:
                fh.seek(0, 2)
                fh.seek(max(0, fh.tell() - 512 * 1024))
                log_text = fh.read().decode("utf-8", "replace")
        except OSError:
            return None
    current, last = None, None
    for line in log_text.splitlines():
        m = _LINE.match(line)
        if not m:
            continue
        msg = m.group("msg")
        if msg == "transaction started":
            current = Transaction(_ts(m.group("ts")))
        elif current is None:
            continue
        elif m.group("who") == "ALPM" and re.match(
                r"(upgraded|installed|removed|downgraded) ", msg):
            current.changes.append(msg)
        elif msg.startswith("running '") and _SNAPPER_HOOK.search(msg):
            current.hooks_snapshot = True
        elif msg == "transaction completed":
            if current.changes:
                last = current
            current = None
    # A hook that runs before the transaction ("pre" hooks) logs inside it,
    # so the loop above sees it. One that logs just before "transaction
    # started" is attributed by the journal check instead.
    return last


def snapshot_before(started: float, journal_text: str | None = None) -> bool | None:
    """Did someone run `snapper create` in the run-up to `started`?

    None when the journal cannot be read, which is different from "no".
    """
    if journal_text is None:
        since = time.strftime("%Y-%m-%d %H:%M:%S",
                              time.localtime(started - LEAD))
        until = time.strftime("%Y-%m-%d %H:%M:%S",
                              time.localtime(started + 60))
        try:
            r = subprocess.run(
                ["journalctl", "--no-pager", "-q", "-o", "short-iso",
                 "--since", since, "--until", until,
                 "_COMM=sudo", "+", "_COMM=pkexec"],
                capture_output=True, text=True, timeout=20)
        except (OSError, subprocess.SubprocessError):
            return None
        if r.returncode != 0:
            return None
        journal_text = r.stdout
    return bool(re.search(r"COMMAND=\S*snapper\b.*\bcreate\b", journal_text))


def create_step(description: str):
    """A task step that snapshots the root config, or None without Snapper.

    The step never fails the task. A snapshot that could not be taken (the
    password prompt dismissed, the config broken) is reported in the log and
    the update goes ahead -- the same choice `omarchy update` makes, because
    refusing to update is not safer than updating without a net.
    """
    configs = snapper_configs()
    if not configs:
        return None
    # One config is the overwhelmingly common case; with several, snapshot
    # the root one, which is what an update changes.
    config = "root" if "root" in configs else configs[0]
    cmd = ["snapper", "-c", config, "create", "-c", "number",
           "-d", description[:60]]

    def step(log):
        import os
        argv = cmd if os.geteuid() == 0 else ["pkexec", *cmd]
        try:
            r = subprocess.run(argv, capture_output=True, text=True, timeout=120)
        except (OSError, subprocess.SubprocessError) as exc:
            log(f"no snapshot was taken ({exc}); continuing without one")
            return
        if r.returncode == 0:
            log(f"snapshot taken in the '{config}' config")
        else:
            why = (r.stderr or r.stdout).strip().splitlines()
            log(f"no snapshot was taken ({why[-1] if why else 'cancelled'}); "
                f"continuing without one")

    return ("Taking a snapshot first", step)


def finding():
    import maintenance as m

    number = booted_snapshot()
    if number:
        restore = shutil.which("limine-snapper-restore")
        return m.Finding(
            id="snapshot",
            title="Running from a snapshot",
            detail=(f"This session was started from snapshot #{number}. "
                    f"Changes made now are lost at the next normal restart "
                    f"unless the snapshot is made permanent."),
            severity=m.CRIT,
            fix_label="Keep this snapshot" if restore else "",
            fix=["limine-snapper-restore"] if restore else None,
            fix_note=("Makes this snapshot the running system, then asks you "
                      "to restart." if restore else
                      f"Make it permanent with `sudo snapper rollback "
                      f"{number}`, then restart."))

    configs = snapper_configs()
    if not configs:
        return m.Finding(
            id="snapshot", title="Update snapshots",
            detail=("No snapshot tool is set up, so an update cannot be "
                    "undone from the boot menu. Snapper on a btrfs root "
                    "provides that."),
            severity=m.INFO)

    tx = last_transaction()
    if tx is None:
        return m.Finding(id="snapshot", title="Update snapshots",
                         detail="Snapper is set up. No updates are logged yet.",
                         severity=m.OK)

    when = time.strftime("%a %d %b %H:%M", time.localtime(tx.started))
    names = [c.split()[1] for c in tx.changes if len(c.split()) > 1]
    what = ", ".join(names[:4]) + (f" and {len(names) - 4} more"
                                   if len(names) > 4 else "")
    covered = tx.hooks_snapshot or snapshot_before(tx.started)
    if covered:
        how = ("restart, choose the snapshot from the boot menu, then make it "
               "permanent here")
        return m.Finding(
            id="snapshot", title="Update snapshots",
            detail=f"The last update ({when}: {what}) can be undone — a "
                   f"snapshot was taken just before it. To undo it, {how}.",
            severity=m.OK, items=tx.changes[:40])
    if covered is None:
        return m.Finding(
            id="snapshot", title="Update snapshots",
            detail=f"Snapper is set up. Whether the last update ({when}) was "
                   f"snapshotted could not be read from the system journal.",
            severity=m.INFO, items=tx.changes[:40])
    return m.Finding(
        id="snapshot", title="Update snapshots",
        detail=(f"The last update ({when}: {what}) ran without a snapshot, so "
                f"it cannot be rolled back. Updates started from Almanac now "
                f"take one first."),
        severity=m.WARN, items=tx.changes[:40])
