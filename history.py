"""What changed on this machine, and when: one timeline.

"It worked last week" is the start of most troubleshooting, and the answer to
"what changed since?" is scattered: pacman.log for packages, the journal for
snapshots, `.bak` files for settings a tool reset, and nowhere at all for
what Almanac itself did. This puts them on one timeline, newest first,
grouped by day -- and marks the days that have a snapshot to go back to.

Almanac's own actions (an update that did not change anything, settings
restored, file types handed back) are appended to a small JSON-lines file in
the XDG state directory. Everything else is read from where it already is;
nothing is collected that the system was not already keeping.
"""
from __future__ import annotations

import json
import re
import subprocess
import time
from dataclasses import dataclass, field
from datetime import date, datetime

import snapshots

KEEP = 2000                     # recorded events kept; older ones drop off
DAYS = 30

PACKAGES, SNAPSHOT, RESET, ALMANAC = "packages", "snapshot", "reset", "almanac"


def _log_path():
    import xdg
    return xdg.state_home() / "almanac" / "history.jsonl"


@dataclass
class Event:
    ts: float
    kind: str
    title: str
    detail: str = ""
    items: list = field(default_factory=list)
    severity: str = ""          # "warn" for things that went wrong


@dataclass
class Day:
    day: date
    events: list
    snapshot: bool = False      # a snapshot was taken this day

    @property
    def label(self) -> str:
        today = date.today()
        if self.day == today:
            return "Today"
        if (today - self.day).days == 1:
            return "Yesterday"
        return self.day.strftime("%A %d %B")


# ---- what Almanac did --------------------------------------------------------

def record(kind: str, title: str, detail: str = "", severity: str = "",
           items: list | None = None) -> None:
    """Append one of Almanac's own actions. Never raises."""
    path = _log_path()
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        line = json.dumps({"ts": time.time(), "kind": kind, "title": title,
                           "detail": detail, "severity": severity,
                           "items": items or []})
        with path.open("a") as fh:
            fh.write(line + "\n")
        # Trim occasionally rather than on every write.
        if path.stat().st_size > 512 * 1024:
            lines = path.read_text().splitlines()[-KEEP:]
            path.write_text("\n".join(lines) + "\n")
    except OSError:
        pass


def recorded(since: float) -> list[Event]:
    try:
        lines = _log_path().read_text().splitlines()
    except OSError:
        return []
    out = []
    for line in lines:
        try:
            d = json.loads(line)
        except ValueError:
            continue
        if d.get("ts", 0) >= since:
            out.append(Event(d["ts"], ALMANAC, d.get("title", ""),
                             d.get("detail", ""), d.get("items") or [],
                             d.get("severity", "")))
    return out


# ---- what the system recorded ------------------------------------------------

_LINE = re.compile(r"^\[(?P<ts>[^\]]+)\] \[(?P<who>[^\]]+)\] (?P<msg>.*)$")
_CHANGE = re.compile(r"^(installed|upgraded|removed|downgraded|reinstalled) (\S+)")


def package_events(since: float, log_text: str | None = None) -> list[Event]:
    """One event per pacman transaction that changed something."""
    if log_text is None:
        try:
            with snapshots.PACMAN_LOG.open("rb") as fh:
                fh.seek(0, 2)
                fh.seek(max(0, fh.tell() - 4 * 1024 * 1024))
                log_text = fh.read().decode("utf-8", "replace")
        except OSError:
            return []
    out, current = [], None
    for line in log_text.splitlines():
        m = _LINE.match(line)
        if not m:
            continue
        msg = m.group("msg")
        if msg == "transaction started":
            current = (snapshots._ts(m.group("ts")), [])
        elif current is None:
            continue
        elif m.group("who") == "ALPM" and _CHANGE.match(msg):
            current[1].append(msg)
        elif msg == "transaction completed":
            started, changes = current
            current = None
            if changes and started >= since:
                out.append(Event(started, PACKAGES, _summarise(changes),
                                 items=changes))
    return out


def _summarise(changes: list[str]) -> str:
    """'Updated 5 packages · installed wordcraft' -- verbs in a fixed order."""
    verbs = {}
    for c in changes:
        m = _CHANGE.match(c)
        if m:
            verbs.setdefault(m.group(1), []).append(m.group(2))
    parts = []
    for verb, label in (("upgraded", "Updated"), ("installed", "Installed"),
                        ("removed", "Removed"), ("downgraded", "Downgraded"),
                        ("reinstalled", "Reinstalled")):
        names = verbs.get(verb)
        if not names:
            continue
        what = names[0] if len(names) == 1 else f"{len(names)} packages"
        parts.append(f"{label if not parts else label.lower()} {what}")
    return " · ".join(parts) or "Package change"


def snapshot_events(since: float, journal_text: str | None = None) -> list[Event]:
    """`snapper create` runs recorded by sudo or pkexec in the journal."""
    if journal_text is None:
        try:
            r = subprocess.run(
                ["journalctl", "--no-pager", "-q", "-o", "json",
                 "--since", f"@{int(since)}", "_COMM=sudo", "+", "_COMM=pkexec"],
                capture_output=True, text=True, timeout=30)
            journal_text = r.stdout if r.returncode == 0 else ""
        except (OSError, subprocess.SubprocessError):
            return []
    out = []
    for line in journal_text.splitlines():
        try:
            rec = json.loads(line)
        except ValueError:
            continue
        msg = rec.get("MESSAGE") or ""
        if not isinstance(msg, str) or not re.search(
                r"COMMAND=\S*snapper\b.*\bcreate\b", msg):
            continue
        desc = re.search(r"-d (.+?)(?:$| -)", msg)
        ts = int(rec.get("__REALTIME_TIMESTAMP", 0)) / 1e6
        out.append(Event(ts, SNAPSHOT, "Snapshot taken",
                         f"“{desc.group(1).strip()}”" if desc else ""))
    return out


def reset_events(since: float) -> list[Event]:
    """Config files a tool replaced, from the backups it left behind."""
    import guard
    out = []
    home = guard.config_home()
    for f in guard._walk(home, guard.MAX_DEPTH):
        m = guard.BAK.match(f.name)
        if not m:
            continue
        ts = float(m.group("ts"))
        if ts < since:
            continue
        name = str(f.with_name(m.group("base")).relative_to(home))
        out.append(Event(ts, RESET, f"{name} replaced",
                         f"Your previous version was kept as {f.name}"))
    return out


# ---- the timeline ------------------------------------------------------------

def timeline(days: int = DAYS) -> list[Day]:
    import demo
    if demo.active():
        return demo.history()
    since = time.time() - days * 86400
    events = (package_events(since) + snapshot_events(since)
              + reset_events(since) + recorded(since))
    events.sort(key=lambda e: e.ts, reverse=True)
    by_day: dict[date, list] = {}
    for e in events:
        by_day.setdefault(datetime.fromtimestamp(e.ts).date(), []).append(e)
    return [Day(d, evs, any(e.kind == SNAPSHOT for e in evs))
            for d, evs in sorted(by_day.items(), reverse=True)]
