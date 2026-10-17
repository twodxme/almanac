"""Synthetic data for screenshots and demos.

Almanac's screenshots would otherwise be photographs of whoever took them:
their installed packages, their connected services, their home directory,
their uptime. None of that has anything to do with the project, and a public
repository indexes it permanently.

With `ALMANAC_DEMO=1` the local inventory is replaced by the fixtures below.
What is *not* replaced is the marketplace catalogue — those listings are
public data published by the marketplaces themselves, and showing the real
ones is the honest way to demonstrate the store.

The fixtures are deliberately ordinary: packages any Arch machine might have,
versions that are plausible rather than real, and a spread of states so one
screenshot can show a security update, a routine update, something several
majors behind, and something perfectly current.
"""
from __future__ import annotations

import os


def active() -> bool:
    return os.environ.get("ALMANAC_DEMO", "").strip() not in ("", "0", "false")


HOME = "/home/user"

# (name, version, update, source, description, size_mb, extra)
_PACKAGES = [
    ("chromium", "151.0.7620.9-1", "152.0.7700.1-1", "repo",
     "A web browser built for speed, simplicity, and security", 312.4,
     {"critical": "security"}),
    ("openssh", "10.2p1-1", "10.3p1-1", "repo",
     "Premier connectivity tool for remote login with the SSH protocol", 7.1,
     {"critical": "security"}),
    ("linux", "6.18.3.arch1-1", "6.18.5.arch1-1", "repo",
     "The Linux kernel and modules", 148.0, {"critical": "core"}),
    ("mesa", "25.3.1-1", "25.3.2-1", "repo",
     "An open-source implementation of the OpenGL specification", 94.6,
     {"critical": "core"}),
    ("firefox", "147.0-1", "148.0-1", "repo",
     "Standalone web browser from mozilla.org", 268.2, {}),
    ("git", "2.52.0-1", "", "repo",
     "The fast distributed version control system", 28.4, {}),
    ("ripgrep", "14.1.1-1", "", "repo",
     "A search tool that combines the usability of ag with the raw speed of grep",
     5.2, {}),
    ("fd", "10.3.0-1", "10.4.0-1", "repo",
     "Simple, fast and user-friendly alternative to find", 3.1, {}),
    ("bat", "0.26.1-2", "", "repo",
     "Cat clone with syntax highlighting and git integration", 6.0, {}),
    ("htop", "3.4.1-1", "", "repo", "Interactive process viewer", 0.4, {}),
    ("neovim", "0.11.4-1", "0.12.0-1", "repo",
     "Fork of Vim aiming to improve extensibility and usability", 32.8, {}),
    ("tmux", "3.6-1", "", "repo",
     "A terminal multiplexer", 1.2, {}),
    ("jq", "1.8.1-1", "", "repo",
     "Command-line JSON processor", 1.1, {}),
    ("ffmpeg", "8.0-3", "8.0-4", "repo",
     "Complete solution to record, convert and stream audio and video", 42.7, {}),
    ("imagemagick", "7.1.2.8-1", "", "repo",
     "An image viewing and manipulation program", 24.1, {}),
    ("wl-clipboard", "2.2.1-3", "", "repo",
     "Command-line copy/paste utilities for Wayland", 0.2, {}),
    ("starship", "1.24.0-1", "1.25.0-1", "aur",
     "The cross-shell prompt for astronauts", 9.8, {}),
    ("lazygit", "0.58.0-1", "", "aur",
     "Simple terminal UI for git commands", 18.3, {}),
    ("zoxide", "0.9.9-1", "", "aur",
     "A smarter cd command inspired by z and autojump", 2.4, {}),
    ("btop", "1.4.5-1", "1.4.6-1", "aur",
     "A monitor of system resources, bpytop ported to C++", 3.9, {}),
]

_APPIMAGES = [
    ("Inkscape", "1.4.2", "1.5.0", "Vector graphics editor", 142.0),
    ("Obsidian", "1.9.14", "", "Knowledge base that works on local Markdown", 118.5),
]

_FLATPAKS = [
    ("Blender", "4.5.3", "", "org.blender.Blender",
     "Free and open source 3D creation suite", 1180.0),
]

_LOCAL = [
    ("example.bar-clock", "2.1.0", 0, False,
     "A clock for the bar, built from a local checkout"),
    ("example.weather", "1.4.2", 23, False,
     "Current conditions and the next few hours, in the bar"),
    ("example.scratchpad", "0.9.1", 7, True,
     "A scratch buffer that follows you between workspaces"),
]

# (display name, binary, installed, latest, manager, domain)
_AGENTS = [
    ("Claude Code", "claude", "2.1.301", "2.1.344", "mise", "claude.com"),
    ("Codex CLI", "codex", "0.162.1", "0.166.0", "mise", "openai.com"),
    ("Gemini CLI", "gemini", "0.64.0", "0.64.0", "self", "gemini.google.com"),
    ("opencode", "opencode", "1.19.3", "1.19.3", "self", "opencode.ai"),
    ("GitHub Copilot CLI", "copilot", "1.0.98", "1.0.98", "self", "github.com"),
]

_MCP = [
    ("Documentation", "https://mcp.example.com/docs", "connected", "ok"),
    ("Issue tracker", "https://mcp.example.com/issues", "connected", "ok"),
    ("Design system", "https://mcp.example.com/design", "connected", "ok"),
    ("Analytics", "https://mcp.example.com/analytics",
     "needs authentication", "info"),
]

_SKILLS = [
    ("release-notes", "Draft release notes from the commits since the last tag"),
    ("review-checklist", "The team's review conventions, loaded on demand"),
]


def packages():
    import inventory
    out = []
    for name, version, update, source, desc, size, extra in _PACKAGES:
        e: dict = dict(extra)
        e.update({"size": f"{size:.2f} MiB", "is_app": name in
                  ("chromium", "firefox", "neovim"),
                  "url": f"https://example.org/{name}",
                  "installed": "1mo ago"})
        if update:
            e["behind"] = inventory.versions_behind(version, update)
        out.append(inventory.App(name=name, version=version, source=source,
                                 description=desc, update=update, extra=e))
    return out


def appimages():
    import inventory
    out = []
    for name, version, update, desc, size in _APPIMAGES:
        extra = {"key": name.lower(), "size": f"{size:.2f} MiB",
                 "is_app": True, "present": True,
                 "url": f"https://example.org/{name.lower()}"}
        if update:
            extra["behind"] = inventory.versions_behind(version, update)
            extra["download"] = "https://example.org/download"
        out.append(inventory.App(
            name=name, version=version, source=inventory.APPIMAGE,
            description=desc, update=update,
            exec_path=f"{HOME}/.local/state/almanac/apps/{name.lower()}.AppImage",
            extra=extra))
    return out


def flatpaks():
    import inventory
    return [inventory.App(
        name=name, version=version, source=inventory.FLATPAK,
        description=desc, update=update,
        extra={"appid": appid, "size": f"{size:.2f} MiB", "is_app": True})
        for name, version, update, appid, desc, size in _FLATPAKS]


def local_builds():
    import inventory
    out = []
    for name, version, behind, dirty, desc in _LOCAL:
        out.append(inventory.App(
            name=name, version=version, source=inventory.LOCAL,
            description=desc,
            exec_path="",
            extra={"repo": f"{HOME}/.config/omarchy/plugins/{name}",
                   "git_behind": behind, "git_dirty": dirty,
                   "git_subject": "Tidy up the panel layout",
                   "url": f"https://github.com/example/{name}"}))
    return out


def agents():
    import inventory
    out = []
    for name, binary, version, latest, manager, domain in _AGENTS:
        update = latest if latest != version else ""
        extra = {"key": binary, "binary": binary, "manager": manager,
                 "domain": domain, "is_app": False,
                 "url": f"https://{domain}",
                 "update_cmd": (["mise", "upgrade", binary] if manager == "mise"
                                else [binary, "update"]),
                 "behind": (inventory.versions_behind(version, latest)
                            if update else (0, ""))}
        out.append(inventory.App(
            name=name, version=version, source=inventory.AGENT,
            description=f"AI coding agent · managed by {manager}",
            update=update,
            exec_path=f"{HOME}/.local/bin/{binary}", extra=extra))
    return out


def collect():
    return packages() + appimages() + flatpaks() + local_builds() + agents()


def findings():
    """A believable spread of health results, with real-looking sizes."""
    import maintenance as m
    return [
        m.Finding(id="pacman-cache", title="Package cache",
                  detail="3.1 GB in 412 downloaded packages. Keeping a couple "
                         "of versions makes a downgrade possible; keeping all "
                         "of them since install does not.",
                  severity=m.WARN, count=412, size_mb=3174,
                  fix_label="Trim to 2 versions", fix=["paccache", "-rk2"],
                  fix_note="Keeps the 2 most recent versions of installed "
                           "packages."),
        m.Finding(id="journal", title="Systemd journal",
                  detail="820 MB of logs. Useful for diagnosing the recent "
                         "past; months of it rarely is.",
                  severity=m.WARN, size_mb=820, fix_label="Vacuum to 500 MB",
                  fix=["journalctl", "--vacuum-size=500M"],
                  fix_note="Deletes the oldest journal files."),
        m.Finding(id="orphans", title="Orphaned packages",
                  detail="3 installed as dependencies that nothing needs any "
                         "more. Build tooling and debug symbols are kept "
                         "deliberately.",
                  severity=m.INFO, count=3,
                  items=["libexample", "libexample-utils", "oldcodec"],
                  fix_label="Review and remove",
                  fix=["pacman", "-Rns", "--noconfirm", "libexample"],
                  fix_note="Removes only the packages listed above."),
        m.Finding(id="yay-cache", title="AUR build cache",
                  detail="410 MB of cloned PKGBUILDs and build artefacts. "
                         "Rebuilt on demand; nothing installed depends on it.",
                  severity=m.INFO, size_mb=410, count=96, fix_label="Clear",
                  fix=["true"], fix_note="Deletes the AUR build cache."),
        m.Finding(id="rollbacks", title="AppImage rollback copies",
                  detail="1 previous build kept after an update, 118 MB. It "
                         "exists so a bad update can be reverted.",
                  severity=m.INFO, count=1, size_mb=118,
                  items=["inkscape.previous"],
                  fix_label="Delete rollback copies", fix=["true"],
                  fix_note="Deletes the .previous files."),
        m.Finding(id="vulnerabilities", title="Known vulnerabilities",
                  detail="2 installed package(s) have known vulnerabilities; "
                         "2 can be fixed by updating.",
                  severity=m.CRIT, count=2,
                  items=["chromium 151.0.7620.9-1 — high, arbitrary code "
                         "execution; fixed in 152.0.7700.1-1 · CVE-2026-0001",
                         "openssh 10.2p1-1 — medium, information disclosure; "
                         "fixed in 10.3p1-1 · CVE-2026-0002"],
                  fix_note="Update all installs every fixed version."),
        m.Finding(id="snapshot", title="Update snapshots",
                  detail="The last update (today 09:13: fd) can be undone — a "
                         "snapshot was taken just before it.",
                  severity=m.OK),
        m.Finding(id="pacnew", title="Unmerged config files",
                  detail="No .pacnew or .pacsave files in /etc.",
                  severity=m.OK),
        m.Finding(id="failed-units", title="Failed services",
                  detail="No failed units.", severity=m.OK),
        m.Finding(id="dangling-desktop", title="Broken launcher entries",
                  detail="Every launcher entry points at something real.",
                  severity=m.OK),
        m.Finding(id="broken-symlinks", title="Broken symlinks on PATH",
                  detail="No broken links in ~/.local/bin.", severity=m.OK),
        m.Finding(id="disk", title="Disk space",
                  detail="286.4 GB free of 474.9 GB on / (40% used).",
                  severity=m.OK, size_mb=293273),
    ]


def ai_groups():
    """The AI estate, with generic services rather than anyone's real ones."""
    import ai

    harnesses = []
    for name, binary, version, latest, manager, domain in _AGENTS:
        update = latest if latest != version else ""
        behind = "2 minor behind" if update else "current"
        harnesses.append(ai.Entry(
            kind=ai.HARNESS, name=name,
            detail=f"{manager}-managed · {HOME}/.local/bin/{binary}",
            version=version, update=update, status=behind,
            severity="warn" if update else "ok",
            icon_key=domain, icon_domain=domain,
            fallback_icon="utilities-terminal-symbolic",
            action_label="Update" if update else "",
            action=["mise", "upgrade", binary] if update else None,
            url=f"https://{domain}"))

    servers = [ai.Entry(kind=ai.MCP, name=name, detail=url, status=status,
                        severity=sev, url=url,
                        fallback_icon="network-server-symbolic",
                        action_label="Authenticate" if sev == "info" else "")
               for name, url, status, sev in _MCP]

    skills = [ai.Entry(kind=ai.SKILL, name=name, detail=desc,
                       status="user skill", severity="ok",
                       fallback_icon="text-x-generic-symbolic")
              for name, desc in _SKILLS]

    market = [ai.Entry(
        kind=ai.MARKET, name="example-marketplace",
        detail="24 plugins available · example/claude-plugins",
        status="synced 2026-10-14", severity="info",
        fallback_icon="software-store-symbolic",
        action_label="Sync", action=["true"],
        url="https://github.com/example/claude-plugins")]

    hooks = [ai.Entry(kind=ai.HOOK, name="SessionStart",
                      detail=f"bash {HOME}/.claude/hooks/session.sh",
                      status="matches *", severity="ok",
                      fallback_icon="system-run-symbolic")]

    specs = ai.group_specs()
    data = {"Harnesses": harnesses, "MCP servers": servers, "Skills": skills,
            "Plugins": [], "Marketplaces": market, "Hooks": hooks}
    return [(title, desc, data.get(title, [])) for title, desc, _fn in specs]


# Marketplace entries to show as already installed, so cards demonstrate both
# states without touching what is really on the machine.
INSTALLED_PLUGIN_IDS = {"omarchy.clock", "omarchy.battery"}
INSTALLED_THEMES = {"solitude"}


def history():
    """A believable week, so the History page can be shown without anyone's."""
    import time
    import history as h
    now = time.time()
    day = 86400

    def at(days_ago, hour, minute):
        t = time.localtime(now - days_ago * day)
        return time.mktime((t.tm_year, t.tm_mon, t.tm_mday, hour, minute, 0,
                            0, 0, -1))

    events = [
        h.Event(at(0, 9, 14), h.ALMANAC, "fd 10.3.0-1 → 10.4.0-1"),
        h.Event(at(0, 9, 13), h.PACKAGES, "Updated fd",
                items=["upgraded fd (10.3.0-1 -> 10.4.0-1)"]),
        h.Event(at(0, 9, 13), h.SNAPSHOT, "Snapshot taken",
                "“Almanac: before updating fd”"),
        h.Event(at(1, 18, 2), h.ALMANAC, "File types changed hands",
                items=["Inkscape now opens .svg — before: Image Viewer"]),
        h.Event(at(1, 17, 40), h.ALMANAC, "Installed inkscape 1.4.2",
                "AppImage from https://github.com/example/inkscape · "
                "checksum verified"),
        h.Event(at(2, 21, 7), h.ALMANAC, "omarchy/shell.json restored",
                "From shell.json.bak.1; the defaults were kept"),
        h.Event(at(2, 21, 5), h.RESET, "omarchy/shell.json replaced",
                "Your previous version was kept as a backup"),
        h.Event(at(3, 10, 30), h.PACKAGES,
                "Updated 14 packages · installed 2 packages",
                items=["upgraded firefox (147.0-1 -> 148.0-1)",
                       "upgraded mesa (25.3.1-1 -> 25.3.2-1)",
                       "installed wl-clipboard (2.2.1-3)"]),
        h.Event(at(3, 10, 28), h.SNAPSHOT, "Snapshot taken", "“4.0.4-1”"),
    ]
    by_day: dict = {}
    from datetime import datetime
    for e in sorted(events, key=lambda e: e.ts, reverse=True):
        by_day.setdefault(datetime.fromtimestamp(e.ts).date(), []).append(e)
    return [h.Day(d, evs, any(e.kind == h.SNAPSHOT for e in evs))
            for d, evs in sorted(by_day.items(), reverse=True)]
