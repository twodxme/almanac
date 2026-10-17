"""Background services of the user session: systemd user units.

Only units that can actually be switched -- `enabled` or `disabled` in
`systemctl --user list-unit-files`, services and timers. Static, generated,
transient and alias units have no switch and are left out.

Some of these are the session's plumbing. Switching off PipeWire takes all
sound with it; switching off the portal breaks file pickers and screen
sharing. Those are marked essential, and the page asks first and says what
stops working. Omarchy's own units are marked as such, since a later
`omarchy update` may expect them.
"""
from __future__ import annotations

import json
import shutil
import subprocess
from dataclasses import dataclass

# Exact unit names -> what stops working without it. Names, not prefixes:
# hyprsunset is a blue-light filter and xdg-desktop-portal-rewrite-launchers
# a helper, and a prefix match called both essential.
_SOUND = "All sound, and screen sharing"
_PORTAL = "File pickers, screen sharing and links from apps"
ESSENTIAL = {
    "pipewire.service": _SOUND, "pipewire.socket": _SOUND,
    "pipewire-pulse.service": _SOUND, "pipewire-pulse.socket": _SOUND,
    "wireplumber.service": "All sound",
    "xdg-desktop-portal.service": _PORTAL,
    "xdg-desktop-portal-hyprland.service": _PORTAL,
    "xdg-desktop-portal-gtk.service": _PORTAL,
    "dbus.service": "Almost everything in the session",
    "dbus-broker.service": "Almost everything in the session",
    "gnome-keyring-daemon.service": "Saved passwords and SSH keys",
    "at-spi-dbus-bus.service": "Screen readers and accessibility tools",
    "gvfs-daemon.service": "Network locations and devices in the file manager",
    "hyprpolkitagent.service": "Password prompts for system changes",
}


@dataclass
class Service:
    unit: str
    description: str
    enabled: bool
    active: bool
    essential: str              # what stops working, '' if nothing much
    omarchy: bool


def available() -> bool:
    return bool(shutil.which("systemctl"))


def _run(*args, timeout=20) -> str:
    try:
        r = subprocess.run(["systemctl", "--user", *args], capture_output=True,
                           text=True, timeout=timeout)
        return r.stdout
    except (OSError, subprocess.SubprocessError):
        return ""


def essential_for(unit: str) -> str:
    return ESSENTIAL.get(unit, "")


def services() -> list[Service]:
    import demo
    if demo.active():
        return demo.user_services()
    if not available():
        return []
    raw = _run("list-unit-files", "--type=service,timer",
               "--state=enabled,disabled", "--no-legend", "--output=json")
    try:
        units = json.loads(raw) if raw.strip() else []
    except ValueError:
        units = []
    names = [u["unit_file"] for u in units
             if isinstance(u, dict) and "@" not in u.get("unit_file", "")]
    if not names:
        return []
    states = {u["unit_file"]: u.get("state") for u in units if isinstance(u, dict)}
    shown = _run("show", "--property=Id,Description,ActiveState", *names)
    info: dict[str, dict[str, str]] = {}
    for block in shown.strip().split("\n\n"):
        d = dict(line.split("=", 1) for line in block.splitlines() if "=" in line)
        if d.get("Id"):
            info[d["Id"]] = d
    out = []
    for name in names:
        d = info.get(name, {})
        enabled = states.get(name) == "enabled"
        active = d.get("ActiveState") == "active"
        # Disabled yet running: something else starts it on demand (a socket,
        # D-Bus, another unit), so a switch would show "off" for a service
        # that is plainly on, and flipping it would not mean what it says.
        if active and not enabled:
            continue
        out.append(Service(name, d.get("Description") or name, enabled, active,
                           essential_for(name), name.startswith("omarchy")))
    out.sort(key=lambda s: (bool(s.essential), s.unit))
    return out


def toggle_argv(unit: str, on: bool) -> list[str]:
    """Enable and start, or disable and stop: the switch means both."""
    return ["systemctl", "--user", "enable" if on else "disable", "--now", unit]


def is_enabled(unit: str) -> bool:
    return _run("is-enabled", unit).strip() == "enabled"
