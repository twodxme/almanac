"""Scheduled updates: the settings, and the systemd timer behind them.

The timer and its service already exist on disk; this is the part that lets
the app change them without the user editing unit files. Everything that is a
*policy* decision lives in one JSON file that both this module and the shell
script read, so the checkbox in the window and the behaviour at 11am on Sunday
cannot drift apart.

What is deliberately not configurable: whether risky batches are held when the
machine would need a human (kernel, bootloader, fresh Arch news). That is a
safety property, not a preference -- see omarchy-auto-update.
"""
from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

CONFIG_DIR = Path(os.environ.get("XDG_CONFIG_HOME", Path.home() / ".config")) / "almanac"
CONFIG = CONFIG_DIR / "auto-update.json"
UNIT = "omarchy-auto-update.timer"
TIMER_FILE = (Path(os.environ.get("XDG_CONFIG_HOME", Path.home() / ".config"))
              / "systemd/user" / UNIT)

# label -> OnCalendar
SCHEDULES = (
    ("daily", "Every day at 11:00", "*-*-* 11:00:00"),
    ("weekly", "Every Sunday at 11:00", "Sun 11:00"),
    ("fortnightly", "Every other Sunday", "Sun *-*-1..7,15..21 11:00:00"),
    ("monthly", "First Sunday of the month", "Sun *-*-1..7 11:00:00"),
)

DEFAULTS = {
    "schedule": "weekly",
    # What the unattended run is allowed to touch. Packages default on
    # because that is what the timer was created for; plugins default off
    # because a shell plugin update can change the desktop under you.
    "packages": True,
    "agents": True,
    "appimages": True,
    "plugins": False,
    "notify": True,
}


def load() -> dict:
    cfg = dict(DEFAULTS)
    try:
        cfg.update(json.loads(CONFIG.read_text()))
    except (OSError, ValueError):
        pass
    return cfg


def save(cfg: dict) -> None:
    CONFIG_DIR.mkdir(parents=True, exist_ok=True)
    tmp = CONFIG.with_suffix(".tmp")
    tmp.write_text(json.dumps(cfg, indent=2) + "\n")
    tmp.replace(CONFIG)


def _systemctl(*args, timeout=30) -> tuple[int, str]:
    try:
        out = subprocess.run(["systemctl", "--user", *args],
                             capture_output=True, text=True, timeout=timeout)
        return out.returncode, (out.stdout + out.stderr).strip()
    except (OSError, subprocess.SubprocessError) as exc:
        return 1, str(exc)


def is_enabled() -> bool:
    code, out = _systemctl("is-enabled", UNIT)
    return code == 0 and out.startswith("enabled")


def set_enabled(on: bool) -> tuple[bool, str]:
    args = (["enable", "--now", UNIT] if on else ["disable", "--now", UNIT])
    code, out = _systemctl(*args)
    return code == 0, out


def status() -> dict:
    """Next and last run, read from systemd rather than remembered here."""
    code, out = _systemctl("show", UNIT, "--property=NextElapseUSecRealtime",
                           "--property=LastTriggerUSec", "--property=Unit")
    info = {}
    for line in out.splitlines():
        if "=" in line:
            k, v = line.split("=", 1)
            info[k] = v
    return {
        "enabled": is_enabled(),
        "next": info.get("NextElapseUSecRealtime", "") or "not scheduled",
        "last": info.get("LastTriggerUSec", "") or "never",
    }


def set_schedule(key: str) -> tuple[bool, str]:
    """Rewrite OnCalendar in the timer unit and reload.

    The whole unit is not regenerated: it carries Persistent= and a randomised
    delay that were chosen for a laptop, and rewriting the file would quietly
    drop them.
    """
    calendar = dict((k, cal) for k, _l, cal in SCHEDULES).get(key)
    if not calendar:
        return False, f"unknown schedule {key!r}"
    try:
        text = TIMER_FILE.read_text()
    except OSError as exc:
        return False, str(exc)

    out, replaced = [], False
    for line in text.splitlines():
        if line.startswith("OnCalendar=") and not replaced:
            out.append(f"OnCalendar={calendar}")
            replaced = True
        else:
            out.append(line)
    if not replaced:
        return False, "no OnCalendar= line in the timer unit"

    try:
        TIMER_FILE.write_text("\n".join(out) + "\n")
    except OSError as exc:
        return False, str(exc)

    _systemctl("daemon-reload")
    if is_enabled():
        _systemctl("restart", UNIT)
    return True, calendar


def current_schedule() -> str:
    try:
        text = TIMER_FILE.read_text()
    except OSError:
        return "weekly"
    for line in text.splitlines():
        if line.startswith("OnCalendar="):
            cal = line.split("=", 1)[1].strip()
            for key, _label, c in SCHEDULES:
                if c == cal:
                    return key
    return "weekly"


def run_now_argv() -> list[str]:
    """How to trigger a run immediately, for the task runner."""
    return ["systemd-run", "--user", "--wait", "--pipe", "--collect",
            "--unit=almanac-update",
            str(Path.home() / ".local/bin/omarchy-auto-update")]
