"""The AI estate: everything on this machine that is part of an agent setup.

A coding agent is not one program. It is a harness, the MCP servers it can
reach, the skills and plugins that change how it behaves, and the hooks that
fire around it. None of that is visible to pacman, and most of it is invisible
to the harness's own `--version` too, so it is the part of the system that
drifts furthest without anyone noticing.

Everything here is read from the tools themselves (`claude mcp list`,
`claude plugin list --json`) or from the files they own, never from a list
kept in this module. An agent configured after this was written still shows up.
"""
from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import time
from dataclasses import dataclass, field
from pathlib import Path

HOME = Path.home()
CLAUDE = HOME / ".claude"
CACHE = Path(os.environ.get("XDG_CACHE_HOME", HOME / ".cache")) / "almanac"
MCP_CACHE = CACHE / "mcp-list.json"
# `claude mcp list` opens a connection to every configured server, so its cost
# scales with how many are set up and how far away they are -- 13 to 22
# seconds here. Short enough to still catch a server going down, long enough
# that moving between pages does not re-run it.
MCP_TTL = 300

HARNESS, MCP, SKILL, PLUGIN, MARKET, HOOK = (
    "harness", "mcp", "skill", "plugin", "marketplace", "hook")


@dataclass
class Entry:
    kind: str
    name: str
    detail: str = ""
    status: str = ""                # connected | needs auth | update | ""
    severity: str = ""              # ok | warn | info
    version: str = ""
    update: str = ""
    path: str = ""
    url: str = ""
    harness: str = "Claude Code"
    action_label: str = ""
    action: object = None           # argv list, or callable(log)
    action_note: str = ""
    items: list = field(default_factory=list)
    # Where to get the vendor's own mark. `icon_key` is the cache key, so two
    # services from one company share a single download.
    icon_key: str = ""
    icon_domain: str = ""
    icon_url: str = ""
    fallback_icon: str = "application-x-executable-symbolic"


def _github_avatar(repo: str) -> str:
    """An org's avatar is the brand mark for most repositories."""
    owner = (repo or "").split("/")[0]
    return f"https://github.com/{owner}.png?size=128" if owner else ""


def _host(target: str) -> str:
    m = re.match(r"^[a-z]+://([^/:]+)", target or "")
    return m.group(1) if m else ""


def _run(cmd, timeout=60) -> str:
    try:
        out = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        return (out.stdout or "") + (out.stderr or "")
    except (OSError, subprocess.SubprocessError):
        return ""


def _frontmatter(path: Path) -> dict:
    """name/description from a SKILL.md header, without a YAML dependency.

    Only the two scalar keys are needed, and they may be folded onto several
    lines with `>`; a real parser would be more than this costs.
    """
    try:
        text = path.read_text(errors="replace")
    except OSError:
        return {}
    if not text.startswith("---"):
        return {}
    end = text.find("\n---", 3)
    block = text[3:end if end > 0 else 4000]

    out, key, buf = {}, None, []
    for line in block.splitlines():
        m = re.match(r"^([A-Za-z_-]+):\s*(.*)$", line)
        if m:
            if key:
                out[key] = " ".join(buf).strip()
            key = m.group(1).lower()
            val = m.group(2).strip()
            buf = [] if val in (">", "|", ">-", "|-") else [val]
        elif key and line.strip():
            buf.append(line.strip())
    if key:
        out[key] = " ".join(buf).strip()
    return out


# ---- harnesses ---------------------------------------------------------------

def harnesses() -> list[Entry]:
    import agents
    out = []
    for a in agents.detect():
        behind, kind = a.extra.get("behind", (0, ""))
        out.append(Entry(
            kind=HARNESS,
            name=a.name,
            detail=(f"{a.extra.get('manager')}-managed · {a.exec_path}"),
            version=a.version,
            update=a.update,
            status=(f"{behind} {kind} behind" if a.update else "current"),
            severity="warn" if a.update else "ok",
            path=a.exec_path,
            url=a.extra.get("url", ""),
            harness=a.name,
            action_label="Update" if a.update and a.extra.get("update_cmd") else "",
            action=a.extra.get("update_cmd") if a.update else None,
            action_note=f"Runs {' '.join(a.extra.get('update_cmd') or [])}.",
            icon_key=a.extra.get("domain") or a.extra.get("key", ""),
            icon_domain=a.extra.get("domain", ""),
            icon_url=("" if a.extra.get("domain")
                      else _github_avatar(a.extra.get("repo", ""))),
            fallback_icon="utilities-terminal-symbolic"))
    return out


# ---- MCP servers -------------------------------------------------------------

_MCP_LINE = re.compile(r"^(?P<name>.+?):\s+(?P<target>\S.*?)\s+-\s+(?P<state>.+)$")


def mcp_servers() -> list[Entry]:
    """What `claude mcp list` reports, including its health check.

    Parsed from the CLI rather than from ~/.claude.json because the connectors
    enabled through claude.ai never appear in that file -- reading the file
    would report zero servers on a machine with twenty.
    """
    if not shutil.which("claude"):
        return []
    try:
        blob = json.loads(MCP_CACHE.read_text())
        if time.time() - blob.get("at", 0) < MCP_TTL:
            return [Entry(**row) for row in blob.get("rows", [])]
    except (OSError, ValueError, TypeError):
        pass

    out = []
    for line in _run(["claude", "mcp", "list"], timeout=120).splitlines():
        line = line.strip()
        if not line or line.startswith("Checking"):
            continue
        m = _MCP_LINE.match(line)
        if not m:
            continue
        state = m.group("state").strip()
        connected = "Connect" in state and "✘" not in state and "!" not in state
        needs_auth = "auth" in state.lower()
        out.append(Entry(
            kind=MCP,
            name=m.group("name").strip(),
            detail=m.group("target").strip(),
            status="connected" if connected else
                   ("needs authentication" if needs_auth else state.strip("✘! ")),
            severity="ok" if connected else ("info" if needs_auth else "warn"),
            url=m.group("target").strip()
            if m.group("target").startswith("http") else "",
            icon_key=_host(m.group("target").strip()),
            icon_domain=_host(m.group("target").strip()),
            fallback_icon="network-server-symbolic",
            action_label="Authenticate" if needs_auth else "",
            action=None,
            action_note="Authentication happens inside the agent session: "
                        "run the server's authenticate tool there."))
    try:
        CACHE.mkdir(parents=True, exist_ok=True)
        MCP_CACHE.write_text(json.dumps(
            {"at": time.time(), "rows": [vars(e) for e in out]}))
    except (OSError, TypeError):
        pass
    return out


# ---- skills ------------------------------------------------------------------

def skills() -> list[Entry]:
    out = []
    root = CLAUDE / "skills"
    if root.exists():
        for d in sorted(root.iterdir()):
            f = d / "SKILL.md"
            if not f.is_file():
                continue
            fm = _frontmatter(f)
            out.append(Entry(
                kind=SKILL, name=fm.get("name") or d.name,
                detail=(fm.get("description") or "")[:300],
                status="user skill", severity="ok", path=str(d),
                fallback_icon="text-x-generic-symbolic"))

    # Skills that arrive with a plugin are listed separately: they are not
    # yours to edit, and they come and go with the plugin.
    for f in sorted((CLAUDE / "plugins").rglob("skills/*/SKILL.md")):
        fm = _frontmatter(f)
        plugin = f.parents[2].name
        out.append(Entry(
            kind=SKILL, name=fm.get("name") or f.parent.name,
            detail=(fm.get("description") or "")[:300],
            status=f"from {plugin}", severity="ok", path=str(f.parent),
            fallback_icon="text-x-generic-symbolic"))
    return out


# ---- plugins and marketplaces ------------------------------------------------

def plugins() -> list[Entry]:
    if not shutil.which("claude"):
        return []
    try:
        rows = json.loads(_run(["claude", "plugin", "list", "--json"]) or "[]")
    except ValueError:
        return []
    out = []
    for r in rows if isinstance(rows, list) else []:
        name = r.get("name") or r.get("id") or "?"
        out.append(Entry(
            kind=PLUGIN, name=name,
            detail=r.get("description") or "",
            version=r.get("version") or "",
            status="enabled" if r.get("enabled", True) else "disabled",
            severity="ok" if r.get("enabled", True) else "info",
            action_label="Disable" if r.get("enabled", True) else "Enable",
            fallback_icon="application-x-addon-symbolic",
            action=["claude", "plugin",
                    "disable" if r.get("enabled", True) else "enable", name]))
    return out


def marketplaces() -> list[Entry]:
    path = CLAUDE / "plugins/known_marketplaces.json"
    try:
        data = json.loads(path.read_text())
    except (OSError, ValueError):
        return []
    out = []
    for name, info in (data or {}).items():
        loc = Path(info.get("installLocation") or "")
        count = len(list((loc / "plugins").glob("*/"))) if loc.exists() else 0
        src = info.get("source") or {}
        repo = src.get("repo") or src.get("source") or ""
        updated = (info.get("lastUpdated") or "")[:10]
        # A marketplace that has not been synced in weeks is listing plugin
        # versions that no longer exist, which is the quiet failure mode here.
        stale = False
        if updated:
            from datetime import date
            try:
                y, m, d = (int(x) for x in updated.split("-"))
                stale = (date.today() - date(y, m, d)).days > 14
            except ValueError:
                stale = False
        out.append(Entry(
            kind=MARKET, name=name,
            detail=f"{count} plugins available · {repo}",
            status=(f"synced {updated} — stale" if stale else
                    f"synced {updated}") if updated else "never synced",
            severity="warn" if (stale or not updated) else "info",
            path=str(loc),
            url=f"https://github.com/{repo}" if repo else "",
            icon_key=f"gh-{repo.split('/')[0]}" if repo else "",
            icon_url=_github_avatar(repo),
            fallback_icon="software-store-symbolic",
            action_label="Sync",
            action=["claude", "plugin", "marketplace", "update", name],
            action_note="Re-fetches the marketplace catalogue from its source."))
    return out


# ---- hooks and other extensions ----------------------------------------------

def hooks() -> list[Entry]:
    try:
        settings = json.loads((CLAUDE / "settings.json").read_text())
    except (OSError, ValueError):
        return []
    out = []
    for event, groups in (settings.get("hooks") or {}).items():
        for group in groups if isinstance(groups, list) else []:
            for h in group.get("hooks") or []:
                cmd = h.get("command", "")
                # A hook whose script has been deleted fails silently on every
                # session start, which is exactly the kind of rot this exists
                # to surface.
                m = re.search(r"(/[^\s'\"]+)", cmd)
                missing = bool(m) and not os.path.exists(m.group(1))
                out.append(Entry(
                    kind=HOOK, name=event,
                    detail=cmd,
                    status="script missing" if missing else
                           f"matches {group.get('matcher', '*')}",
                    severity="warn" if missing else "ok",
                    fallback_icon="system-run-symbolic",
                    path=m.group(1) if m else ""))
    sl = (settings.get("statusLine") or {}).get("command")
    if sl:
        m = re.search(r"(\$HOME|~)?(/[^\s'\"]+)", sl)
        target = (m.group(2) if m else "")
        full = str(HOME) + target if target else ""
        out.append(Entry(
            kind=HOOK, name="statusLine", detail=sl,
            status="script missing" if full and not os.path.exists(full)
                   else "configured",
            severity="warn" if full and not os.path.exists(full) else "ok",
            fallback_icon="system-run-symbolic",
            path=full))
    return out


GROUPS = (
    ("Harnesses", HARNESS, harnesses,
     "The agent CLIs themselves."),
    ("MCP servers", MCP, mcp_servers,
     "Tools and data the agents can reach."),
    ("Skills", SKILL, skills,
     "Instructions loaded on demand to change how an agent works."),
    ("Plugins", PLUGIN, plugins,
     "Bundles of skills, commands, agents and hooks."),
    ("Marketplaces", MARKET, marketplaces,
     "Where plugins are installed from."),
    ("Hooks", HOOK, hooks,
     "Commands that run around an agent session."),
)


def group_specs():
    """(title, description, loader) for each group, in display order."""
    return [(title, desc, fn) for title, _kind, fn, desc in GROUPS]


def load_group(fn, title: str) -> list[Entry]:
    try:
        return fn()
    except Exception as exc:                            # noqa: BLE001
        return [Entry(kind="error", name=title,
                      detail=f"Could not read: {exc}", severity="warn")]


def survey() -> list[tuple[str, str, list[Entry]]]:
    """(title, description, entries) for each group. Read-only.

    `claude mcp list` health-checks 28 servers and takes most of the time
    here; the file scans should not be queued behind it.
    """
    import demo
    if demo.active():
        return demo.ai_groups()

    from concurrent.futures import ThreadPoolExecutor

    with ThreadPoolExecutor(max_workers=len(GROUPS)) as pool:
        futures = [(title, desc, pool.submit(fn))
                   for title, _kind, fn, desc in GROUPS]
        out = []
        for title, desc, future in futures:
            try:
                entries = future.result()
            except Exception as exc:                    # noqa: BLE001
                entries = [Entry(kind="error", name=title,
                                 detail=f"Could not read: {exc}",
                                 severity="warn")]
            out.append((title, desc, entries))
    return out


def counts(groups) -> dict:
    return {title: len(entries) for title, _d, entries in groups}


def needs_attention(groups) -> int:
    return sum(1 for _t, _d, entries in groups
               for e in entries if e.severity == "warn")
