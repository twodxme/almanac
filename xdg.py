"""Where things live, and how to reach a terminal, on anyone's desktop.

Almanac is published for anyone's machine, so nothing here may assume the one
it was written on: base directories follow the XDG Base Directory spec (a
user can move any of them), and a terminal is found the way the desktop
would find one rather than by trying three favourites.
"""
from __future__ import annotations

import os
import shutil
from pathlib import Path


def _base(var: str, default: str) -> Path:
    value = os.environ.get(var, "")
    # The spec: a relative path is invalid and must be ignored.
    return Path(value) if value and os.path.isabs(value) else Path.home() / default


def data_home() -> Path:
    return _base("XDG_DATA_HOME", ".local/share")


def config_home() -> Path:
    return _base("XDG_CONFIG_HOME", ".config")


def state_home() -> Path:
    return _base("XDG_STATE_HOME", ".local/state")


def cache_home() -> Path:
    return _base("XDG_CACHE_HOME", ".cache")


def bin_home() -> Path:
    """~/.local/bin: not in the spec's variables, but named by it."""
    return Path.home() / ".local/bin"


# How each terminal takes a command to run. Most use -e; several do not, and
# guessing wrong opens an empty shell instead of running anything.
_TERMINALS: tuple[tuple[str, list[str]], ...] = (
    ("foot", []),
    ("alacritty", ["-e"]),
    ("kitty", []),
    ("ghostty", ["-e"]),
    ("wezterm", ["start", "--"]),
    ("konsole", ["-e"]),
    ("gnome-terminal", ["--wait", "--"]),
    ("kgx", ["-e"]),
    ("xfce4-terminal", ["--disable-server", "-x"]),
    ("tilix", ["-e"]),
    ("xterm", ["-e"]),
)


def terminal_argv(command: list[str]) -> list[str] | None:
    """argv that runs `command` in a terminal window and waits for it.

    `xdg-terminal-exec` first: it is the freedesktop proposal for exactly
    this, honours the user's chosen terminal, and Omarchy and several other
    distributions ship it. Then $TERMINAL, then whatever is installed.
    """
    if shutil.which("xdg-terminal-exec"):
        return ["xdg-terminal-exec", *command]
    known = dict(_TERMINALS)
    chosen = os.environ.get("TERMINAL", "")
    if chosen and shutil.which(chosen):
        return [chosen, *known.get(Path(chosen).name, ["-e"]), *command]
    for name, flags in _TERMINALS:
        if shutil.which(name):
            return [name, *flags, *command]
    return None


def system_update_command() -> str:
    """The full-upgrade command this system's own tooling expects."""
    if shutil.which("omarchy"):
        return "omarchy update"         # snapshots, migrations, then pacman
    if shutil.which("yay"):
        return "yay -Syu"
    if shutil.which("paru"):
        return "paru -Syu"
    return "sudo pacman -Syu"
