"""Installed Omarchy shell plugins: what state each is in, and acting on it.

Counting commits behind upstream is not enough to decide whether a plugin can
be updated. Before anything touches one, it is classified -- the same states
omaplug (github.com/fross100/omaplug) settled on, which match what Omarchy's
own updater will and will not do:

* CURRENT        a clean checkout at its upstream's head
* UPDATE         a clean checkout behind its upstream: safe to fast-forward
* LOCAL_CHANGES  edited, ahead of upstream, diverged from it, or detached --
                 an update would have to decide what happens to that work
* LOCAL          not Omarchy's to update: a plain folder, a checkout with no
                 origin, or a **symlink**. Omarchy installs a plugin being
                 developed as a symlink to its author's workspace, and
                 `omarchy plugin update` would happily fast-forward that
                 workspace; this never offers to.
* ERROR          the state could not be read (fetch failed, git broke)

Updates go through `omarchy plugin update <id> --yes`, which validates the
result and rolls back a plugin that fails validation -- something a bare
`git pull` never did.
"""
from __future__ import annotations

import json
import os
import subprocess
import time
from dataclasses import dataclass
from pathlib import Path

import xdg

CURRENT, UPDATE, LOCAL_CHANGES, LOCAL, ERROR = (
    "current", "update", "local-changes", "local", "error")
FETCH_EVERY = 3600

# Bulk-update scopes, by the marketplace provenance a plugin carries.
SCOPES = (
    ("verified", "Verified only",
     {"official", "first-party", "reviewed", "verified"}),
    ("changed", "Verified, and changed since review",
     {"official", "first-party", "reviewed", "verified", "reviewed-stale",
      "stale"}),
    ("all", "All plugins", None),           # None: anything, listed or not
)


def plugins_dir() -> Path:
    return xdg.config_home() / "omarchy" / "plugins"


@dataclass
class State:
    id: str
    kind: str
    detail: str = ""            # why: "symlink", "modified", "ahead", "3 commits"…
    behind: int = 0
    url: str = ""

    @property
    def updatable(self) -> bool:
        return self.kind == UPDATE

    def describe(self) -> str:
        return {
            CURRENT: "Up to date",
            UPDATE: f"{self.behind} commit(s) behind",
            LOCAL_CHANGES: {
                "modified": "Has local edits",
                "ahead": "Ahead of upstream — local commits",
                "diverged": "Diverged from upstream",
                "detached": "Not on a branch",
            }.get(self.detail, "Local changes"),
            LOCAL: {
                "symlink": "Development plugin (a link to a workspace) — "
                           "never updated here",
                "not-git": "Not a git checkout — nothing to update from",
                "no-origin": "No upstream to update from",
                "no-commits": "No commits yet",
            }.get(self.detail, "Managed locally"),
            ERROR: f"Could not check: {self.detail}",
        }[self.kind]


def _git(d: Path, *args, timeout=25):
    return subprocess.run(["git", "-C", str(d), *args], capture_output=True,
                          text=True, timeout=timeout,
                          env={**os.environ, "GIT_TERMINAL_PROMPT": "0",
                               "GIT_SSH_COMMAND": os.environ.get(
                                   "GIT_SSH_COMMAND", "ssh -oBatchMode=yes")})


def classify(d: Path, cache: dict | None = None, fetch: bool = True) -> State:
    """The state of the plugin in `d`. `cache` holds fetch results by path,
    so the network is asked at most once an hour per plugin; with `fetch`
    False it is not asked at all, and an unknown upstream reads as CURRENT
    with the detail "unchecked"."""
    pid = d.name
    if d.is_symlink():
        return State(pid, LOCAL, "symlink")
    if not (d / ".git").is_dir():
        return State(pid, LOCAL, "not-git")
    try:
        url = _git(d, "remote", "get-url", "origin").stdout.strip()
        if not url:
            return State(pid, LOCAL, "no-origin")
        head = _git(d, "rev-parse", "--verify", "HEAD").stdout.strip()
        if not head:
            return State(pid, LOCAL, "no-commits", url=url)
        if _git(d, "symbolic-ref", "--quiet", "HEAD").returncode != 0:
            return State(pid, LOCAL_CHANGES, "detached", url=url)
        if _git(d, "status", "--porcelain", "--untracked-files=normal").stdout.strip():
            # Still worth knowing how far behind: patches.py weighs the edit
            # against the update, and needs there to be one.
            remote = _remote(d, cache, fetch)
            behind = _count(d, head, remote) if remote else 0
            return State(pid, LOCAL_CHANGES, "modified", behind, url)
        remote = _remote(d, cache, fetch)
        if not remote:
            return (State(pid, ERROR, "fetch failed", url=url) if fetch
                    else State(pid, CURRENT, "unchecked", url=url))
        if remote == head:
            return State(pid, CURRENT, url=url)
        if _git(d, "merge-base", "--is-ancestor", head, remote).returncode == 0:
            return State(pid, UPDATE, "behind", _count(d, head, remote), url)
        if _git(d, "merge-base", "--is-ancestor", remote, head).returncode == 0:
            return State(pid, LOCAL_CHANGES, "ahead", url=url)
        return State(pid, LOCAL_CHANGES, "diverged", url=url)
    except (OSError, subprocess.SubprocessError) as exc:
        return State(pid, ERROR, str(exc)[:80])


def _remote(d: Path, cache: dict | None, fetch: bool = True) -> str:
    """The upstream head, fetched -- as Omarchy fetches it, `origin HEAD` --
    at most once an hour; counted against locally between fetches."""
    key = str(d)
    entry = (cache or {}).get(key) or {}
    fresh = time.time() - float(entry.get("fetched_at") or 0) < FETCH_EVERY
    sha = entry.get("remote", "")
    if (fresh or not fetch) and sha and _git(d, "cat-file", "-e", sha).returncode == 0:
        return sha
    if not fetch:
        return ""
    if _git(d, "fetch", "--quiet", "origin", "HEAD", timeout=45).returncode != 0:
        return ""
    sha = _git(d, "rev-parse", "--verify", "FETCH_HEAD").stdout.strip()
    if cache is not None and sha:
        cache[key] = {"remote": sha, "fetched_at": time.time()}
    return sha


def _count(d: Path, head: str, remote: str) -> int:
    out = _git(d, "rev-list", "--count", f"{head}..{remote}").stdout.strip()
    return int(out) if out.isdigit() else 0


@dataclass
class Installed:
    """One plugin on this system, built in or added, for the Plugins page."""
    id: str
    name: str
    description: str = ""
    version: str = ""
    kinds: tuple = ()
    enabled: bool = False
    can_disable: bool = True
    built_in: bool = False
    folder: Path | None = None
    state: State | None = None      # only for plugins the user added

    @property
    def has_panel(self) -> bool:
        return bool({"panel", "bar-widget", "overlay"} & set(self.kinds))


def _manifests(root: Path) -> dict[str, tuple[dict, Path]]:
    """id -> (manifest, folder) for every manifest under `root`."""
    out: dict[str, tuple[dict, Path]] = {}
    for m in root.rglob("manifest.json") if root.is_dir() else []:
        try:
            d = json.loads(m.read_text())
        except (OSError, ValueError):
            continue
        if isinstance(d, dict) and d.get("id"):
            out.setdefault(d["id"], (d, m.parent))
    return out


def installed(fetch: bool = True, cache: dict | None = None) -> list[Installed]:
    """Every plugin Omarchy knows about, with what each one is and its state.

    Omarchy's own list decides what exists and whether it is on; manifests
    supply the description, and the user's own plugins are classified so the
    page can say whether each can be updated.
    """
    import demo
    if demo.active():
        return demo.plugins()
    try:
        out = subprocess.run(["omarchy", "plugin", "list", "--json"],
                             capture_output=True, text=True, timeout=25).stdout
        listed = [i for i in json.loads(out) if isinstance(i, dict) and i.get("id")]
    except (OSError, subprocess.SubprocessError, ValueError):
        return []
    import os as _os
    shipped = _manifests(Path(_os.environ.get("OMARCHY_PATH") or "/usr/share/omarchy")
                         / "shell" / "plugins")
    mine = {}
    pdir = plugins_dir()
    for d in sorted(pdir.glob("*")) if pdir.is_dir() else []:
        try:
            m = json.loads((d / "manifest.json").read_text())
        except (OSError, ValueError):
            continue
        mine[m.get("id") or d.name] = (m, d)

    result = []
    for item in listed:
        pid = item["id"]
        manifest, folder = mine.get(pid) or shipped.get(pid) or ({}, None)
        own = pid in mine
        result.append(Installed(
            id=pid,
            name=item.get("name") or manifest.get("name") or pid,
            description=manifest.get("description", ""),
            version=manifest.get("version", ""),
            kinds=tuple(item.get("kinds") or manifest.get("kinds") or ()),
            enabled=bool(item.get("enabled")),
            can_disable=bool(item.get("canDisable", True)),
            built_in=bool(item.get("firstParty")) and not own,
            folder=folder,
            state=classify(folder, cache, fetch) if own and folder else None))
    result.sort(key=lambda i: (i.built_in, i.name.lower()))
    return result


# ---- what to show beside each plugin -------------------------------------------

# A plugin about a service carries that service's mark, fetched from the
# vendor's own domain (icons.py) -- matched on the plugin's id and name only,
# never its description, where "works with Slack" would mislabel it.
VENDORS = (
    ("tailscale", "tailscale.com"), ("slack", "slack.com"),
    ("spotify", "spotify.com"), ("github", "github.com"),
    ("dropbox", "dropbox.com"), ("1password", "1password.com"),
    ("docker", "docker.com"), ("obsidian", "obsidian.md"),
    ("discord", "discord.com"), ("youtube", "youtube.com"),
    ("google", "google.com"), ("todoist", "todoist.com"),
    ("notion", "notion.so"), ("proton", "proton.me"),
    ("steam", "steampowered.com"), ("twitch", "twitch.tv"),
    ("claude", "anthropic.com"), ("anthropic", "anthropic.com"),
    ("openai", "openai.com"), ("chatgpt", "openai.com"),
    ("home assistant", "home-assistant.io"), ("jellyfin", "jellyfin.org"),
)

# Otherwise, a system icon for what it does. Adwaita names only, each checked
# to exist, so the row looks right under any icon theme that follows it.
SYMBOLS = (
    ("bluetooth", "bluetooth-symbolic"),
    ("wifi", "network-wireless-symbolic"), ("network", "network-wireless-symbolic"),
    ("tailscale", "network-transmit-receive-symbolic"),
    ("vpn", "network-transmit-receive-symbolic"),
    ("audio", "audio-volume-high-symbolic"), ("volume", "audio-volume-high-symbolic"),
    ("sound", "audio-volume-high-symbolic"),
    ("microphone", "microphone-sensitivity-high-symbolic"),
    ("media", "media-playback-start-symbolic"), ("music", "media-playback-start-symbolic"),
    ("battery", "battery-full-symbolic"), ("power", "system-shutdown-symbolic"),
    ("clock", "preferences-system-time-symbolic"), ("time", "preferences-system-time-symbolic"),
    ("reminder", "alarm-symbolic"), ("alarm", "alarm-symbolic"),
    ("weather", "weather-few-clouds-symbolic"),
    ("clipboard", "edit-paste-symbolic"), ("emoji", "face-smile-symbolic"),
    ("image", "image-x-generic-symbolic"), ("screenshot", "camera-photo-symbolic"),
    ("lock", "system-lock-screen-symbolic"), ("idle", "system-lock-screen-symbolic"),
    ("polkit", "dialog-password-symbolic"), ("password", "dialog-password-symbolic"),
    ("menu", "open-menu-symbolic"), ("background", "preferences-desktop-wallpaper-symbolic"),
    ("wallpaper", "preferences-desktop-wallpaper-symbolic"),
    ("appearance", "preferences-desktop-appearance-symbolic"),
    ("theme", "preferences-desktop-appearance-symbolic"),
    ("monitor", "video-display-symbolic"), ("display", "video-display-symbolic"),
    ("nightlight", "night-light-symbolic"), ("night", "night-light-symbolic"),
    ("notification", "preferences-system-notifications-symbolic"),
    ("osd", "preferences-system-notifications-symbolic"),
    ("keyboard", "preferences-desktop-keyboard-symbolic"),
    ("update", "software-update-available-symbolic"),
    ("disk", "drive-harddisk-symbolic"), ("speedtest", "content-loading-symbolic"),
    ("workspace", "view-grid-symbolic"), ("window", "window-new-symbolic"),
    ("tray", "view-app-grid-symbolic"), ("bar", "view-app-grid-symbolic"),
    ("agent", "utilities-terminal-symbolic"), ("dev", "utilities-terminal-symbolic"),
    ("vitals", "emblem-system-symbolic"), ("system", "emblem-system-symbolic"),
    ("radar", "weather-few-clouds-symbolic"),
)
GENERIC = "application-x-addon-symbolic"


def _words(*texts: str) -> str:
    return " " + " ".join(t.lower().replace(".", " ").replace("-", " ")
                          .replace("_", " ") for t in texts if t) + " "


def _own_words(i: Installed) -> str:
    """The plugin's name and the last part of its id. The rest of an id is a
    reverse-DNS namespace ("io.github.someone"), and the kind ("bar-widget")
    says where it sits, not what it is -- matching either mislabels it."""
    return _words(i.id.rsplit(".", 1)[-1], i.name)


def vendor_for(i: Installed) -> str:
    """The domain of the service a plugin is about, or ''."""
    words = _own_words(i)
    for word, domain in VENDORS:
        if f" {word}" in words:
            return domain
    return ""


def symbol_for(i: Installed) -> str:
    """A system icon for what the plugin does, from its name and id."""
    words = _own_words(i)
    for word, icon in SYMBOLS:
        if f" {word}" in words:
            return icon
    return GENERIC


def github_owner(url: str) -> str:
    import re
    m = re.match(r"(?:https://github\.com/|git@github\.com:)([\w.-]+)/", url or "")
    return m.group(1) if m else ""


def enabled() -> dict[str, bool]:
    """id -> enabled, from Omarchy's own list."""
    try:
        out = subprocess.run(["omarchy", "plugin", "list", "--json"],
                             capture_output=True, text=True, timeout=25).stdout
        items = json.loads(out)
    except (OSError, subprocess.SubprocessError, ValueError):
        return {}
    return {i["id"]: bool(i.get("enabled")) for i in items
            if isinstance(i, dict) and i.get("id")}


def toggle_argv(pid: str, on: bool) -> list[str]:
    return ["omarchy", "plugin", "enable" if on else "disable", pid]


def update_argv(pid: str) -> list[str]:
    return ["omarchy", "plugin", "update", pid, "--yes"]


def in_scope(trust_key: str | None, scope: str) -> bool:
    """Whether a plugin with this provenance is covered by a bulk update.
    `trust_key` is None for a plugin the marketplace does not list."""
    allowed = dict((k, s) for k, _l, s in SCOPES).get(scope)
    if allowed is None:
        return True
    return trust_key in allowed


def restart_argv(clear_cache: bool = True) -> list[str]:
    """Restart the shell -- never `omarchy-refresh-shell`, which resets
    shell.json to its defaults. Clearing Quickshell's compiled QML cache
    first fixes a plugin acting on stale compiled code. Detached, since the
    shell being restarted may be what started it."""
    cache = xdg.cache_home() / "quickshell"
    script = "omarchy-restart-shell"
    if clear_cache:
        script = (f'rm -rf "{cache}/qmlcache" "{cache}"/qtpipelinecache-*; '
                  + script)
    return ["setsid", "-f", "bash", "-c", script]


# ---- settings ------------------------------------------------------------------

def _settings_path() -> Path:
    return xdg.state_home() / "almanac" / "plugins.json"


def scope() -> str:
    import settings
    value = settings.get("plugin_scope")
    old = _settings_path()
    if old.is_file():
        # 1.4.0 kept this in its own file: carry it over once.
        try:
            value = json.loads(old.read_text()).get("scope", value)
            settings.set("plugin_scope", value)
            old.unlink()
        except (OSError, ValueError, AttributeError):
            pass
    return value if value in {k for k, _l, _s in SCOPES} else "all"


def set_scope(value: str) -> None:
    import settings
    settings.set("plugin_scope", value)
