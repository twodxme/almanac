"""`almanac --guard`: notice settings resets while the window is closed.

The window watches for a config file being reset to its defaults, but only
while it is open -- and the reset that prompted this happened with it
closed. This is the same check as a small background process: no window, no
GTK, a few directory monitors on a GLib main loop, idle until a tool writes a
`<file>.bak.<epoch>`.

It starts with the session through XDG autostart, the one mechanism every
desktop honours (systemd-based sessions run it through their autostart
generator; others read the directory directly). Turning it on and off is a
switch in Preferences, which writes or removes the autostart entry.

A reset is announced once. The backups already reported are remembered in
the XDG state directory, so a restart does not repeat old news.
"""
from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

import xdg

ENTRY_NAME = "io.github.twodxme.Almanac.Guard.desktop"


def autostart_path() -> Path:
    return xdg.config_home() / "autostart" / ENTRY_NAME


def _reported_path() -> Path:
    return xdg.state_home() / "almanac" / "guard-reported.json"


def launcher() -> list[str]:
    """How to start Almanac from an autostart entry, however it is installed."""
    found = shutil.which("almanac")
    if found:
        return [found]
    return [sys.executable, str(Path(__file__).resolve().parent / "app.py")]


def enabled() -> bool:
    return autostart_path().is_file()


def enable(start_now: bool = True) -> None:
    path = autostart_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    exec_line = " ".join(_quote(a) for a in [*launcher(), "--guard"])
    path.write_text(
        "[Desktop Entry]\n"
        "Type=Application\n"
        "Name=Almanac settings guard\n"
        "Comment=Notices when a config file is reset to its defaults\n"
        f"Exec={exec_line}\n"
        "Icon=io.github.twodxme.Almanac\n"
        "NoDisplay=true\n"
        "Terminal=false\n"
        "X-GNOME-Autostart-enabled=true\n")
    if start_now and not running():
        subprocess.Popen([*launcher(), "--guard"], start_new_session=True,
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def disable() -> None:
    autostart_path().unlink(missing_ok=True)
    pid = running()
    if pid:
        try:
            os.kill(pid, 15)
        except OSError:
            pass


def _quote(arg: str) -> str:
    """Desktop Entry Exec quoting: reserved characters need double quotes."""
    if any(c in arg for c in ' \t\n"\'\\><~|&;$*?#()`'):
        return '"' + arg.replace("\\", "\\\\").replace('"', '\\"') \
            .replace("`", "\\`").replace("$", "\\$") + '"'
    return arg


def _pid_path() -> Path:
    runtime = os.environ.get("XDG_RUNTIME_DIR") or str(xdg.state_home() / "almanac")
    return Path(runtime) / "almanac-guard.pid"


def running() -> int:
    """PID of a guard that is running, or 0."""
    try:
        pid = int(_pid_path().read_text().strip())
        os.kill(pid, 0)
        cmdline = Path(f"/proc/{pid}/cmdline").read_bytes()
        return pid if b"--guard" in cmdline else 0
    except (OSError, ValueError):
        return 0


def _load_reported() -> set:
    try:
        return set(json.loads(_reported_path().read_text()))
    except (OSError, ValueError, TypeError):
        return set()


def _save_reported(seen: set) -> None:
    path = _reported_path()
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(sorted(seen)))
    except OSError:
        pass


def announce(found, seen: set) -> set:
    """Notify about resets not already reported. Returns the updated set."""
    new = [r for r in found if str(r.backup) not in seen]
    for r in new:
        n = len(r.lost)
        lost = (f" {n} of your entries {'was' if n == 1 else 'were'} removed."
                if n else "")
        body = (f"{r.name} was replaced with the shipped defaults.{lost} "
                f"Your version was kept — open Almanac to restore it.")
        _notify("Your settings were reset", body)
    return seen | {str(r.backup) for r in found}


def _notify(title: str, body: str) -> None:
    """A notification whose action opens Almanac on the Health check."""
    tool = shutil.which("notify-send")
    if not tool:
        return

    def run():
        try:
            out = subprocess.run(
                [tool, "-u", "critical", "-i", "io.github.twodxme.Almanac",
                 "-A", "open=Open Almanac", "--wait", title, body],
                capture_output=True, text=True, timeout=3600).stdout.strip()
        except (OSError, subprocess.SubprocessError):
            return
        if out == "open":
            subprocess.Popen([*launcher(), "--page", "health"],
                             start_new_session=True)
    import threading
    threading.Thread(target=run, daemon=True).start()


def main() -> int:
    import gi
    gi.require_version("Gio", "2.0")
    from gi.repository import Gio, GLib

    import guard

    if running():
        return 0                    # one per session
    try:
        _pid_path().parent.mkdir(parents=True, exist_ok=True)
        _pid_path().write_text(str(os.getpid()))
    except OSError:
        pass

    seen = announce(guard.resets(), _load_reported())
    _save_reported(seen)

    root = guard.defaults_root()
    if root is None:
        return 0                    # nothing ships defaults; nothing to guard

    loop = GLib.MainLoop()
    state = {"seen": seen, "pending": 0}

    def check():
        state["pending"] = 0
        state["seen"] = announce(guard.resets(), state["seen"])
        _save_reported(state["seen"])
        return False

    def changed(_mon, gfile, _other, event):
        if event != Gio.FileMonitorEvent.CREATED:
            return
        if not guard.BAK.match(gfile.get_basename() or ""):
            return
        if state["pending"]:
            GLib.source_remove(state["pending"])
        state["pending"] = GLib.timeout_add_seconds(3, check)

    monitors = []
    for sub in sorted(root.iterdir()):
        target = guard.config_home() / sub.name
        if not target.is_dir():
            continue
        try:
            mon = Gio.File.new_for_path(str(target)).monitor_directory(
                Gio.FileMonitorFlags.NONE, None)
        except GLib.Error:
            continue
        mon.connect("changed", changed)
        monitors.append(mon)        # a collected monitor stops delivering

    GLib.unix_signal_add(GLib.PRIORITY_DEFAULT, 15, loop.quit)
    GLib.unix_signal_add(GLib.PRIORITY_DEFAULT, 2, loop.quit)
    loop.run()
    try:
        _pid_path().unlink()
    except OSError:
        pass
    return 0
