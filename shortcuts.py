"""Keyboard shortcuts that open a plugin's panel, in Hyprland's bindings.lua.

The approach is omaplug's (github.com/fross100/omaplug, MIT): check the live
bindings for a conflict before writing, write only a block Almanac owns and
can recognise again, and undo the change if Hyprland reports a config error
after reloading.

Only Omarchy's Lua configuration is supported -- ~/.config/hypr/bindings.lua
with `o.bind(...)` -- since that is the one file a user is told to put their
own bindings in. Anywhere else, `available()` is False and the feature is
not offered.

A block looks like this, and is the only text this module ever edits:

    -- almanac-shortcut-start: lxp.vitals
    o.bind("SUPER + ALT + V", "Almanac: lxp.vitals", "omarchy-shell shell toggle lxp.vitals")
    -- almanac-shortcut-end: lxp.vitals
"""
from __future__ import annotations

import json
import os
import re
import stat
import subprocess
import tempfile
from pathlib import Path

import xdg

MODIFIERS = {"SUPER": 64, "CTRL": 4, "ALT": 8, "SHIFT": 1}
_KEY = re.compile(r"[A-Z0-9]|F(?:[1-9]|1[0-2])|SLASH|SPACE|RETURN|TAB|HOME|END|"
                  r"INSERT|DELETE|PAGEUP|PAGEDOWN|UP|DOWN|LEFT|RIGHT|COMMA|PERIOD|"
                  r"SEMICOLON|APOSTROPHE|MINUS|EQUAL|GRAVE|BACKSLASH|"
                  r"BRACKETLEFT|BRACKETRIGHT")
_ID = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*$")


def bindings_path() -> Path:
    return xdg.config_home() / "hypr" / "bindings.lua"


def available() -> bool:
    """Omarchy's Lua bindings file, and Hyprland to ask about live binds."""
    import shutil
    path = bindings_path()
    try:
        return (path.is_file() and not path.is_symlink()
                and "o.bind(" in path.read_text(errors="replace")
                and bool(shutil.which("hyprctl")))
    except OSError:
        return False


def normalise(value: str) -> tuple[str, int, str]:
    """'super+alt+v' -> ('SUPER + ALT + V', modmask, 'V'). Raises ValueError."""
    parts = [p.strip().upper().replace("CONTROL", "CTRL") for p in value.split("+")]
    keys = [p for p in parts if p not in MODIFIERS]
    mods = [p for p in parts if p in MODIFIERS]
    if (len(keys) != 1 or not mods or len(set(parts)) != len(parts)
            or not _KEY.fullmatch(keys[0])):
        raise ValueError("Use one or more modifiers (Super, Ctrl, Alt, Shift) "
                         "and one key, such as Super + Alt + V.")
    ordered = [m for m in MODIFIERS if m in mods]
    return " + ".join(ordered + keys), sum(MODIFIERS[m] for m in ordered), keys[0]


def _block(pid: str, combo: str) -> str:
    return (f"\n-- almanac-shortcut-start: {pid}\n"
            f"o.bind({json.dumps(combo)}, {json.dumps('Almanac: ' + pid)}, "
            f"{json.dumps('omarchy-shell shell toggle ' + pid)})\n"
            f"-- almanac-shortcut-end: {pid}\n")


def _pattern(pid: str):
    return re.compile(r"\n-- almanac-shortcut-start: " + re.escape(pid)
                      + r"\n.*?-- almanac-shortcut-end: " + re.escape(pid) + r"\n", re.S)


def current(pid: str, raw: str | None = None) -> str:
    """The combination Almanac bound for this plugin, or ''."""
    raw = _read()[0] if raw is None else raw
    m = _pattern(pid).search(raw)
    if not m:
        return ""
    found = re.search(r'o\.bind\("([^"\n]+)"', m.group(0))
    if not found or m.group(0) != _block(pid, found.group(1)):
        raise ValueError("The shortcut block in bindings.lua was edited by hand; "
                         "change it there.")
    return found.group(1)


def conflict(binds: list, combo: str, pid: str) -> str:
    """Why `combo` cannot be used, from Hyprland's live bindings, or ''."""
    _c, mask, key = normalise(combo)
    for b in binds:
        if int(b.get("modmask", 0)) & ~2 != mask or b.get("mouse"):
            continue                        # 2 is Caps Lock, never chosen
        bound = str(b.get("key", "")).upper()
        unsure = bool(b.get("keycode")) or bound.startswith("CODE:") or b.get("catch_all")
        if bound != key and not unsure:
            continue
        if bound == key and b.get("description") == "Almanac: " + pid:
            continue                        # our own, being replaced
        what = b.get("description") or b.get("arg") or b.get("dispatcher") or "another action"
        if unsure or b.get("submap"):
            return f"Cannot be sure it is free ({what}). Choose another."
        return f"Already used by {what}."
    return ""


def _read() -> tuple[str, int]:
    path = bindings_path()
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(fd, encoding="utf-8") as fh:
        info = os.fstat(fh.fileno())
        if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid()
                or info.st_size > 1024 * 1024):
            raise ValueError("bindings.lua is not a regular file you own.")
        return fh.read(), stat.S_IMODE(info.st_mode)


def _write(raw: str, mode: int) -> None:
    path = bindings_path()
    fd, tmp = tempfile.mkstemp(prefix=".almanac-shortcut-", dir=path.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as fh:
            os.fchmod(fh.fileno(), mode)
            fh.write(raw)
            fh.flush()
            os.fsync(fh.fileno())
        os.replace(tmp, path)
    finally:
        if os.path.exists(tmp):
            os.unlink(tmp)


def _hyprctl(*args) -> str:
    r = subprocess.run(["hyprctl", *args], capture_output=True, text=True, timeout=10)
    if r.returncode:
        raise ValueError((r.stderr or r.stdout).strip() or "hyprctl failed")
    return r.stdout.strip()


def set_shortcut(pid: str, combo: str | None) -> str:
    """Bind (or with None, unbind) a plugin's shortcut. Returns the result.

    Reloads Hyprland and checks for config errors; on any, the previous file
    is restored and Hyprland reloaded again, and ValueError says why.
    """
    if not _ID.match(pid) or ".." in pid:
        raise ValueError("Not a plugin id.")
    raw, mode = _read()
    old = current(pid, raw)
    stripped = _pattern(pid).sub("", raw)
    if combo is None:
        if not old:
            return "No shortcut was set."
        new = stripped
    else:
        combo = normalise(combo)[0]
        why = conflict(json.loads(_hyprctl("-j", "binds")), combo, pid)
        if why:
            raise ValueError(why)
        new = stripped.rstrip("\n") + "\n" + _block(pid, combo)
    if _hyprctl("configerrors"):
        raise ValueError("Hyprland already reports config errors; fix those first.")
    _write(new, mode)
    _hyprctl("reload")
    errors = _hyprctl("configerrors")
    if errors:
        _write(raw, mode)
        _hyprctl("reload")
        raise ValueError("Hyprland rejected it, so bindings.lua was put back: "
                         + errors.splitlines()[0])
    return (f"{combo} opens {pid}" if combo else f"Shortcut for {pid} removed")


def has_panel(plugin_dir: Path) -> bool:
    """Only a plugin with something to open can be given a shortcut to it."""
    try:
        m = json.loads((plugin_dir / "manifest.json").read_text())
    except (OSError, ValueError):
        return False
    points = m.get("entryPoints") or {}
    return bool(points.get("panel") or points.get("barWidget")
                or points.get("overlay"))
