"""Programs that start when you log in: XDG autostart entries.

Every desktop that follows the freedesktop autostart spec -- GNOME, KDE,
Xfce, and Hyprland sessions run through uwsm or systemd's autostart
generator -- starts the `.desktop` files in `$XDG_CONFIG_DIRS/autostart`
(usually /etc/xdg/autostart) and `$XDG_CONFIG_HOME/autostart`. A file in the
user's directory overrides a system one of the same name.

Switching one off follows the spec exactly: a user-directory copy with
`Hidden=true`. The system file is never touched, and switching it back on
removes only an override Almanac itself wrote (marked `X-Almanac-Override`),
so a user's own hand-made entry is edited in place rather than deleted.
"""
from __future__ import annotations

import os
from dataclasses import dataclass
from pathlib import Path

import xdg

MARK = "X-Almanac-Override"
OWN = ("io.github.twodxme.Almanac",)        # Almanac's own: shown elsewhere


@dataclass
class Entry:
    filename: str
    name: str
    comment: str
    icon: str
    enabled: bool
    path: Path                  # the file that decides (user copy if any)
    system: Path | None         # the shipped one, when there is one
    ours: bool                  # the user copy is an override Almanac wrote


def user_dir() -> Path:
    return xdg.config_home() / "autostart"


def system_dirs() -> list[Path]:
    dirs = os.environ.get("XDG_CONFIG_DIRS") or "/etc/xdg"
    return [Path(d) / "autostart" for d in dirs.split(":") if d]


def _parse(path: Path) -> dict[str, str]:
    """The [Desktop Entry] group's keys. Localised keys are skipped."""
    out: dict[str, str] = {}
    group = ""
    try:
        lines = path.read_text(errors="replace").splitlines()
    except OSError:
        return out
    for line in lines:
        line = line.strip()
        if line.startswith("["):
            group = line
        elif group == "[Desktop Entry]" and "=" in line and not line.startswith("#"):
            k, v = line.split("=", 1)
            if "[" not in k:
                out.setdefault(k.strip(), v.strip())
    return out


def _applies(d: dict[str, str]) -> bool:
    """Whether this desktop would run it at all (OnlyShowIn / NotShowIn)."""
    here = {x for x in (os.environ.get("XDG_CURRENT_DESKTOP") or "").split(":") if x}
    only = {x for x in d.get("OnlyShowIn", "").split(";") if x}
    never = {x for x in d.get("NotShowIn", "").split(";") if x}
    if only and here and not (only & here):
        return False
    return not (never & here)


def _on(d: dict[str, str]) -> bool:
    return (d.get("Hidden", "false").lower() != "true"
            and d.get("X-GNOME-Autostart-enabled", "true").lower() != "false")


def entries() -> list[Entry]:
    import demo
    if demo.active():
        return demo.startup_apps()
    found: dict[str, Path] = {}
    for folder in reversed(system_dirs()):     # earlier dirs take precedence
        for f in sorted(folder.glob("*.desktop")) if folder.is_dir() else []:
            found[f.name] = f
    systems = dict(found)
    for f in sorted(user_dir().glob("*.desktop")) if user_dir().is_dir() else []:
        found[f.name] = f
    out = []
    for name, path in sorted(found.items()):
        if any(name.startswith(o) for o in OWN):
            continue
        d = _parse(path)
        base = _parse(systems[name]) if name in systems else d
        if not _applies(base) or d.get("Type", "Application") != "Application":
            continue
        out.append(Entry(name, base.get("Name") or d.get("Name") or name,
                         base.get("Comment") or d.get("Comment", ""),
                         base.get("Icon") or d.get("Icon", ""),
                         _on(d), path, systems.get(name),
                         d.get(MARK, "").lower() == "true"))
    out.sort(key=lambda e: e.name.lower())
    return out


def set_enabled(e: Entry, on: bool) -> None:
    """Switch an entry, the way the spec says to. Raises OSError."""
    target = user_dir() / e.filename
    if on:
        if target.exists() and _parse(target).get(MARK, "").lower() == "true":
            target.unlink()                     # our override: just remove it
            return
        if target.exists():                     # the user's own: edit in place
            _set_key(target, "Hidden", "false")
            _set_key(target, "X-GNOME-Autostart-enabled", "true")
        return
    if target.exists():
        _set_key(target, "Hidden", "true")
        return
    user_dir().mkdir(parents=True, exist_ok=True)
    source = e.system or e.path
    text = source.read_text(errors="replace")
    target.write_text(text)
    _set_key(target, "Hidden", "true")
    _set_key(target, MARK, "true")


def _set_key(path: Path, key: str, value: str) -> None:
    lines = path.read_text(errors="replace").splitlines()
    out, group, done = [], "", False
    for line in lines:
        stripped = line.strip()
        if stripped.startswith("["):
            if group == "[Desktop Entry]" and not done:
                out.append(f"{key}={value}")
                done = True
            group = stripped
        elif group == "[Desktop Entry]" and stripped.split("=", 1)[0].strip() == key:
            if not done:
                out.append(f"{key}={value}")
                done = True
            continue
        out.append(line)
    if not done:
        out.append(f"{key}={value}")
    tmp = path.with_suffix(".tmp")
    tmp.write_text("\n".join(out) + "\n")
    tmp.replace(path)
