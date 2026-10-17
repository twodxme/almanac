"""Settings a tool quietly put back to their defaults.

Omarchy's `refresh` commands copy the shipped default of a config file over
the user's own, keeping theirs as `<file>.bak.<epoch>`. That is a reasonable
repair tool and a terrible surprise: `omarchy-refresh-shell` replaces
shell.json, every custom bar widget disappears, and nothing says why. The
backup is sitting right there; nobody tells you it exists.

A reset is recognised from the files alone, which makes it independent of
which command did it and of whether Almanac was running at the time:

* `<file>.bak.<epoch>` exists, from the last week,
* `<file>` is now identical to the default the distribution ships, and
* the backup is not -- so something customised was replaced.

When the user chooses the defaults on purpose, the finding still offers the
backup for a week, then lets it go. Restoring keeps the replaced defaults as
`<file>.defaults.<epoch>`, so the restore is reversible too.

On a system without a shipped-defaults tree (anything that is not Omarchy)
there is nothing to compare against, and this finds nothing.
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

BAK = re.compile(r"^(?P<base>.+)\.bak\.(?P<ts>\d{9,11})$")
WINDOW_DAYS = 7
# Deep enough for ~/.config/<app>/<sub>/<file>; a full walk of ~/.config can
# be large (browser profiles) and nothing reset by a refresh lives that deep.
MAX_DEPTH = 4


def config_home() -> Path:
    import xdg
    return xdg.config_home()


def defaults_root() -> Path | None:
    """Where the distribution keeps the shipped copies of user configs."""
    base = os.environ.get("OMARCHY_PATH") or "/usr/share/omarchy"
    root = Path(base) / "config"
    return root if root.is_dir() else None


@dataclass
class Reset:
    path: Path                  # the config file, now holding defaults
    backup: Path                # the user's version
    when: float
    lost: list = field(default_factory=list)   # what the defaults dropped

    @property
    def name(self) -> str:
        try:
            return str(self.path.relative_to(config_home()))
        except ValueError:
            return str(self.path)


def _normal(path: Path, like: Path | None = None):
    """Content for comparison: parsed when JSON, so formatting is ignored.

    `like` is the file whose type decides -- a backup is named
    `shell.json.bak.<epoch>`, so its own suffix says nothing.
    """
    try:
        raw = path.read_bytes()
    except OSError:
        return None
    if (like or path).suffix == ".json":
        try:
            return json.loads(raw)
        except ValueError:
            pass
    return raw.strip()


def _ids(blob) -> list:
    """Every "id" in a JSON document, in order: widgets, plugins, entries."""
    out = []

    def walk(node):
        if isinstance(node, dict):
            if isinstance(node.get("id"), str):
                out.append(node["id"])
            for v in node.values():
                walk(v)
        elif isinstance(node, list):
            for v in node:
                walk(v)
    walk(blob)
    return out


def _lost(current: Path, backup: Path) -> list:
    """What the reset removed, in terms a person recognises."""
    a, b = _normal(backup, current), _normal(current)
    if isinstance(a, (dict, list)) and isinstance(b, (dict, list)):
        kept = set(_ids(b))
        seen, out = set(), []
        for i in _ids(a):
            if i not in kept and i not in seen:
                seen.add(i)
                out.append(i)
        return out
    return []


def _walk(root: Path, depth: int):
    try:
        entries = list(os.scandir(root))
    except OSError:
        return
    for e in entries:
        try:
            if e.is_dir(follow_symlinks=False):
                if depth > 1:
                    yield from _walk(Path(e.path), depth - 1)
            elif e.is_file(follow_symlinks=False):
                yield Path(e.path)
        except OSError:
            continue


def resets(now: float | None = None) -> list[Reset]:
    defaults = defaults_root()
    if defaults is None:
        return []
    home = config_home()
    now = now or time.time()
    cutoff = now - WINDOW_DAYS * 86400

    newest: dict[Path, tuple[float, Path]] = {}
    for f in _walk(home, MAX_DEPTH):
        m = BAK.match(f.name)
        if not m:
            continue
        ts = float(m.group("ts"))
        if ts < cutoff:
            continue
        base = f.with_name(m.group("base"))
        if base not in newest or ts > newest[base][0]:
            newest[base] = (ts, f)

    out = []
    for base, (ts, bak) in sorted(newest.items()):
        shipped = defaults / base.relative_to(home)
        if not (base.is_file() and shipped.is_file()):
            continue
        current, theirs, stock = (_normal(base), _normal(bak, base),
                                  _normal(shipped, base))
        if current is None or theirs is None:
            continue
        if current == stock and theirs != stock:
            out.append(Reset(base, bak, ts, _lost(base, bak)))
    return out


def restore(reset: Reset, log=print) -> None:
    """Put the user's version back, keeping the defaults it replaces."""
    keep = reset.path.with_name(f"{reset.path.name}.defaults.{int(time.time())}")
    shutil.copy2(reset.path, keep)
    shutil.copy2(reset.backup, reset.path)
    log(f"restored {reset.name} from {reset.backup.name}")
    log(f"the defaults it replaced are in {keep.name}")


def reload_for(paths) -> list[list[str]]:
    """Commands that make a running desktop pick a restored file up.

    Hyprland watches its own config. Omarchy's shell does not re-read
    shell.json, so it has to be restarted -- with the restart command, never
    the refresh one, which is what reset it in the first place.
    """
    cmds = []
    names = {Path(p).name for p in paths}
    if "shell.json" in names and shutil.which("omarchy-restart-shell"):
        cmds.append(["omarchy-restart-shell"])
    return cmds


def finding():
    """The Health check row for this."""
    import maintenance as m

    found = resets()
    if not found:
        return m.Finding(
            id="config-reset", title="Settings reset to defaults",
            detail="No config file has been put back to its defaults in the "
                   "last week.", severity=m.OK)

    parts = []
    items = []
    for r in found:
        when = time.strftime("%a %H:%M", time.localtime(r.when))
        lost = f", dropping {len(r.lost)} entries" if r.lost else ""
        parts.append(f"{r.name} at {when}{lost}")
        items.append(f"{r.name} — your version is {r.backup.name}")
        items.extend(f"    removed: {i}" for i in r.lost[:30])

    def fix(log):
        for r in found:
            restore(r, log)
        for cmd in reload_for(r.path for r in found):
            log("$ " + " ".join(cmd))
            subprocess.run(cmd, capture_output=True, timeout=60)

    return m.Finding(
        id="config-reset",
        title="Settings reset to defaults",
        detail=("Replaced with the shipped defaults: " + "; ".join(parts)
                + ". Your own version was kept as a backup."),
        severity=m.WARN, count=len(found), items=items,
        fix_label="Restore my settings", fix=fix,
        fix_note="Copies your version back from the backup. The defaults it "
                 "replaces are kept alongside, so this can be undone. To keep "
                 "the defaults instead, leave this; it stops being offered a "
                 "week after the reset.")
