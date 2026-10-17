"""Almanac's own preferences, in one file.

Everything a person can switch about Almanac itself lives here, with its
default -- including each thing it reaches over the network, so that "what
leaves your machine" in the README is something a user can turn off, not
just read about. Stored as JSON in the XDG state directory; a missing or
broken file means the defaults, never a crash.

Scheduled updates keep their own file (autoupdate.py), because a shell
script reads it too.
"""
from __future__ import annotations

import json
from pathlib import Path

import xdg

DEFAULTS: dict = {
    # General
    "start_page": "last",           # last | featured | updates | plugins
    # Notifications
    "notify_plugin_updates": True,  # from the background watcher
    "notify_resets": True,          # a config file put back to its defaults
    "notify_no_change": True,       # an action that ran but changed nothing
    # Updates
    "hold_majors": True,            # major agent releases wait for a person
    "plugin_scope": "all",          # see plugins.SCOPES
    # Network: each thing Almanac fetches, by what it is for
    "net_vendor_icons": True,       # logos from each vendor's own domain
    "net_security_tracker": True,   # security.archlinux.org advisories
    "net_market_omarchy": True,     # plugins.omarchy.org catalogue
    "net_market_artcraft": True,    # getartcraft.com + GitHub releases
    "net_market_themes": True,      # the omarchy-theme topic on GitHub
    "net_gh_token": True,           # use `gh auth token` for GitHub requests
}

START_PAGES = (("last", "Where you left off"), ("featured", "Today"),
               ("updates", "Updates"), ("plugins", "Plugins"))


def path() -> Path:
    return xdg.state_home() / "almanac" / "settings.json"


def _load() -> dict:
    try:
        data = json.loads(path().read_text())
        return data if isinstance(data, dict) else {}
    except (OSError, ValueError):
        return {}


def get(key: str):
    """The setting, or its default. Unknown keys are a programming error."""
    if key not in DEFAULTS:
        raise KeyError(key)
    value = _load().get(key, DEFAULTS[key])
    # A hand-edited file with the wrong type falls back, not through.
    return value if isinstance(value, type(DEFAULTS[key])) else DEFAULTS[key]


def set(key: str, value) -> None:            # noqa: A001 - settings.set reads well
    if key not in DEFAULTS:
        raise KeyError(key)
    data = _load()
    data[key] = value
    p = path()
    p.parent.mkdir(parents=True, exist_ok=True)
    tmp = p.with_suffix(".tmp")
    tmp.write_text(json.dumps(data, indent=2, sort_keys=True))
    tmp.replace(p)
