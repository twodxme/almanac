"""Parity cases for xdg.py.

`env` in the args is applied on both sides for the duration of the call
(None removes the variable); PATH is pointed at a temp dir of fake
executables so which() sees a chosen set of programs, never runs them.
"""
import atexit
import os
import shutil
import stat
import tempfile
from pathlib import Path
from unittest import mock

import xdg

_TMP = tempfile.mkdtemp(prefix="almanac-parity-xdg-")
atexit.register(shutil.rmtree, _TMP, True)


def _bin(*names):
    """A directory holding empty executables called `names`."""
    d = Path(_TMP, "-".join(names) or "empty")
    d.mkdir(exist_ok=True)
    for n in names:
        f = d / n
        f.write_text("#!/bin/sh\nexit 0\n")
        f.chmod(f.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    return str(d)


def _call(env, fn):
    def run():
        saved = {k: os.environ.get(k) for k in env}
        try:
            for k, v in env.items():
                if v is None:
                    os.environ.pop(k, None)
                else:
                    os.environ[k] = v
            return fn()
        finally:
            for k, v in saved.items():
                if v is None:
                    os.environ.pop(k, None)
                else:
                    os.environ[k] = v
    return run


def cases():
    homes = [("data_home", "XDG_DATA_HOME"), ("config_home", "XDG_CONFIG_HOME"),
             ("state_home", "XDG_STATE_HOME"), ("cache_home", "XDG_CACHE_HOME")]
    values = [None, "", "/srv/data", "relative/ignored", "./dot", "/srv//data/",
              "/srv/./data/.", "//net/share", "///triple", "/", "/tmp/ünï cødé/日本",
              "~/notexpanded", "/a/../b", " /leading-space"]
    for func, var in homes:
        yield func, {}, getattr(xdg, func)          # the real machine
        for v in values:
            env = {var: v}
            yield func, {"env": env}, _call(env, getattr(xdg, func))
        env = {var: None, "HOME": "/home/someone"}
        yield func, {"env": env}, _call(env, getattr(xdg, func))
        env = {var: None, "HOME": "/home/trailing/"}
        yield func, {"env": env}, _call(env, getattr(xdg, func))
    yield "bin_home", {}, xdg.bin_home
    for h in ["/home/someone", "/home/ünï/", "/"]:
        env = {"HOME": h}
        yield "bin_home", {"env": env}, _call(env, xdg.bin_home)

    # terminal_argv on the real machine
    for cmd in [["true"], [], ["sh", "-c", "echo hi; read"], ["ünï", "日本 語", ""]]:
        yield "terminal_argv", {"command": cmd}, (lambda c=cmd: xdg.terminal_argv(c))

    full = _bin("xdg-terminal-exec", "foot", "kitty", "alacritty")
    some = _bin("foot", "alacritty", "wezterm")
    gnome = _bin("gnome-terminal")
    xfce = _bin("xfce4-terminal", "xterm")
    custom = _bin("myterm", "kitty")
    empty = _bin()
    scenarios = [
        {"PATH": full, "TERMINAL": None},
        {"PATH": full, "TERMINAL": "kitty"},
        {"PATH": some, "TERMINAL": None},
        {"PATH": some, "TERMINAL": ""},
        {"PATH": some, "TERMINAL": "wezterm"},
        {"PATH": some, "TERMINAL": "notinstalled"},
        {"PATH": gnome, "TERMINAL": None},
        {"PATH": xfce, "TERMINAL": None},
        {"PATH": custom, "TERMINAL": "myterm"},
        {"PATH": custom, "TERMINAL": "kitty"},
        {"PATH": custom, "TERMINAL": custom + "/kitty"},
        {"PATH": custom, "TERMINAL": custom + "/myterm"},
        {"PATH": empty, "TERMINAL": custom + "/kitty"},
        {"PATH": empty, "TERMINAL": None},
        {"PATH": empty, "TERMINAL": "kitty"},
        {"PATH": "", "TERMINAL": None},
        {"PATH": some + ":" + gnome, "TERMINAL": "gnome-terminal"},
        {"PATH": some, "TERMINAL": "ünïterm"},
    ]
    for env in scenarios:
        for cmd in [["true"], ["a b", "ü"]]:
            yield "terminal_argv", {"command": cmd, "env": env}, \
                _call(env, lambda c=cmd: xdg.terminal_argv(c))

    yield "system_update_command", {}, xdg.system_update_command
    for names in [("omarchy", "yay", "paru"), ("yay", "paru"), ("paru",), ("yay",), ()]:
        env = {"PATH": _bin(*names)}
        yield "system_update_command", {"env": env}, _call(env, xdg.system_update_command)

    # --- review round 1 ---
    # HOME empty / unset / unnormalised: Path.home() is a Path, so it falls
    # back to the passwd entry when unset, to "/" when empty, and normalises.
    for h in ["", None, "/home//x/./", "//net", "/home/x/.."]:
        for func, var in homes:
            env = {var: None, "HOME": h}
            yield func, {"env": env}, _call(env, getattr(xdg, func))
        env = {"HOME": h}
        yield "bin_home", {"env": env}, _call(env, xdg.bin_home)

    # PATH unset: shutil.which falls back to os.confstr("CS_PATH") (/bin:/usr/bin).
    env = {"PATH": None}
    yield "system_update_command", {"env": env}, _call(env, xdg.system_update_command)
    env = {"PATH": None, "TERMINAL": None}
    yield "terminal_argv", {"command": ["true"], "env": env}, \
        _call(env, lambda: xdg.terminal_argv(["true"]))

    # Executable only for group/other, not for the owning user: os.access(X_OK)
    # says no, a "any x bit" check says yes.
    def _bin_mode(tag, mode, *names):
        d = Path(_TMP, "mode-" + tag)
        d.mkdir(exist_ok=True)
        for n in names:
            f = d / n
            f.write_text("#!/bin/sh\nexit 0\n")
            f.chmod(mode)
        return str(d)
    grp = _bin_mode("g", 0o654, "yay", "foot", "xdg-terminal-exec")
    oth = _bin_mode("o", 0o645, "paru", "kitty")
    for p in [grp, oth, grp + ":" + oth]:
        env = {"PATH": p, "TERMINAL": None}
        yield "system_update_command", {"env": env}, _call(env, xdg.system_update_command)
        yield "terminal_argv", {"command": ["true"], "env": env}, \
            _call(env, lambda: xdg.terminal_argv(["true"]))
    env = {"PATH": _bin(), "TERMINAL": oth + "/kitty"}
    yield "terminal_argv", {"command": ["true"], "env": env}, \
        _call(env, lambda: xdg.terminal_argv(["true"]))
    # A directory named like a terminal, and a TERMINAL with a trailing slash.
    dird = Path(_TMP, "dirs"); (dird / "foot").mkdir(parents=True, exist_ok=True)
    env = {"PATH": str(dird) + ":" + gnome, "TERMINAL": None}
    yield "terminal_argv", {"command": ["true"], "env": env}, \
        _call(env, lambda: xdg.terminal_argv(["true"]))
    env = {"PATH": empty, "TERMINAL": custom + "/kitty/"}
    yield "terminal_argv", {"command": ["true"], "env": env}, \
        _call(env, lambda: xdg.terminal_argv(["true"]))

    # --- review round 2 ---
    # HOME that expanduser hands back as "~" (Path.home() raises RuntimeError),
    # and tilde/relative HOMEs that it does not reject.
    for h in ["~", "~/", "~//", "~abc", "~/x", ".", "relhome/", "./rel/./x", "//", "///x//"]:
        for func, var in homes:
            env = {var: None, "HOME": h}
            yield func, {"env": env}, _call(env, getattr(xdg, func))
        env = {"HOME": h}
        yield "bin_home", {"env": env}, _call(env, xdg.bin_home)
    # XDG values made only of slashes and dots.
    for v in ["//", "//.", "//./x/", "/.", "/./", "////"]:
        env = {"XDG_DATA_HOME": v}
        yield "data_home", {"env": env}, _call(env, xdg.data_home)
    # TERMINAL spelled with redundant path pieces: Path(chosen).name decides flags.
    for t in [custom + "//kitty", custom + "/./kitty", custom + "/../" + Path(custom).name + "/kitty",
              "kitty/", "./kitty", custom + "/kitty/."]:
        env = {"PATH": empty, "TERMINAL": t}
        yield "terminal_argv", {"command": ["true"], "env": env}, \
            _call(env, lambda: xdg.terminal_argv(["true"]))
