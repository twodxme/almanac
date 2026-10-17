"""AI coding agents, which no package manager knows about.

These install themselves: a curl-to-shell script, a version manager, or their
own `update` subcommand. pacman has never heard of them, so without this they
are the one class of software on the machine that silently rots -- and they are
also the ones shipping a release most weeks.

Two things are deliberately not hardcoded:

* How an agent is managed. It is read from where the binary actually resolves
  to, so moving `claude` from mise to the native installer changes the update
  command here without an edit.
* Which agents exist. The table below is the known set, but detection is by
  what is on PATH; an agent that is not installed simply never appears.
"""
from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import time
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path

CACHE = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")) / "almanac"
LATEST_CACHE = CACHE / "agent-latest.json"
TTL = 6 * 3600
UA = {"User-Agent": "almanac"}

AGENT = "agent"                         # inventory source key


@dataclass
class Spec:
    id: str                             # the binary name on PATH
    name: str
    repo: str = ""                      # owner/name, for the latest release
    home: str = ""
    # The vendor's own domain, used only to fetch their icon from them.
    domain: str = ""
    # Its own updater, when it has one. Used only if the binary is not under
    # a version manager that owns it.
    self_update: list = field(default_factory=list)
    note: str = ""


KNOWN = (
    Spec("claude", "Claude Code", "anthropics/claude-code",
         "https://claude.com/claude-code", "claude.com", ["claude", "update"]),
    Spec("codex", "Codex CLI", "openai/codex",
         "https://github.com/openai/codex", "openai.com"),
    Spec("gemini", "Gemini CLI", "google-gemini/gemini-cli",
         "https://github.com/google-gemini/gemini-cli", "gemini.google.com"),
    Spec("opencode", "opencode", "anomalyco/opencode",
         "https://opencode.ai", "opencode.ai", ["opencode", "upgrade"]),
    Spec("copilot", "GitHub Copilot CLI", "github/copilot-cli",
         "https://github.com/github/copilot-cli", "github.com",
         ["copilot", "update"]),
    Spec("cursor-agent", "Cursor Agent", "", "https://cursor.com/cli",
         "cursor.com", ["cursor-agent", "update"],
         note="Versions are dated builds, not semantic versions."),
    Spec("crush", "Crush", "charmbracelet/crush",
         "https://github.com/charmbracelet/crush", "charm.sh"),
    Spec("goose", "Goose", "block/goose",
         "https://block.github.io/goose/", "block.xyz"),
    Spec("aider", "Aider", "Aider-AI/aider", "https://aider.chat",
         "aider.chat",
         ["python", "-m", "pip", "install", "--upgrade", "aider-chat"]),
    Spec("qwen", "Qwen Code", "QwenLM/qwen-code",
         "https://github.com/QwenLM/qwen-code", "qwen.ai"),
    Spec("amp", "Amp", "", "https://ampcode.com", "ampcode.com",
         ["amp", "update"]),
    Spec("droid", "Factory Droid", "", "https://factory.ai", "factory.ai",
         ["droid", "update"]),
)

BY_ID = {s.id: s for s in KNOWN}

_VERSION = re.compile(r"\d+(?:\.\d+)+(?:[-+][0-9A-Za-z.]+)?")


VERSION_CACHE = CACHE / "agent-versions.json"


def _binary_id(path: str) -> str:
    """Size and mtime of what the name resolves to.

    A CLI's version cannot change without its binary changing, so this is a
    sound cache key -- and asking a 215MB binary for its version costs a
    second of process start-up every time the inventory reloads.
    """
    try:
        st = os.stat(os.path.realpath(path))
        return f"{st.st_size}:{int(st.st_mtime)}"
    except OSError:
        return ""


def _cached_version(binary: str, path: str) -> str:
    key = _binary_id(path)
    if not key:
        return _version_of(binary)
    try:
        store = json.loads(VERSION_CACHE.read_text())
    except (OSError, ValueError):
        store = {}
    hit = store.get(binary)
    if isinstance(hit, list) and len(hit) == 2 and hit[0] == key:
        return hit[1]
    version = _version_of(binary)
    store[binary] = [key, version]
    try:
        CACHE.mkdir(parents=True, exist_ok=True)
        VERSION_CACHE.write_text(json.dumps(store))
    except OSError:
        pass
    return version


def _version_of(binary: str) -> str:
    for args in (["--version"], ["version"], ["-v"]):
        try:
            out = subprocess.run([binary, *args], capture_output=True, text=True,
                                 timeout=20)
        except (OSError, subprocess.SubprocessError):
            return ""
        text = (out.stdout or "") + (out.stderr or "")
        m = _VERSION.search(text)
        if m:
            return m.group(0)
    return ""


MISE_CACHE = CACHE / "mise-outdated.json"
MISE_LS_CACHE = CACHE / "mise-installed.json"

# Tools mise manages that are not coding agents. They update the same way and
# were previously invisible: nothing on the machine reported that `gh` or
# `uv` had a newer release waiting.
TOOL_DOMAINS = {
    "gh": ("GitHub CLI", "github.com"),
    "node": ("Node.js", "nodejs.org"),
    "uv": ("uv", "astral.sh"),
    "python": ("Python", "python.org"),
    "go": ("Go", "go.dev"),
    "rust": ("Rust", "rust-lang.org"),
    "deno": ("Deno", "deno.com"),
    "bun": ("Bun", "bun.sh"),
    "terraform": ("Terraform", "terraform.io"),
    "kubectl": ("kubectl", "kubernetes.io"),
}


def _mise_installed(ttl: int = TTL) -> dict:
    """{tool: active version} for everything mise manages.

    Asked of mise rather than inferred from where a binary resolves to: a
    shimmed tool resolves to /usr/bin/mise, which looks nothing like a mise
    install path. Five of seven agents were misread as self-managed that
    way, and then had their updates checked against the wrong upstream.
    """
    if not shutil.which("mise"):
        return {}
    try:
        blob = json.loads(MISE_LS_CACHE.read_text())
        if time.time() - blob.get("at", 0) < ttl:
            return blob.get("data") or {}
    except (OSError, ValueError):
        pass
    try:
        out = subprocess.run(["mise", "ls", "--json"],
                             capture_output=True, text=True, timeout=90)
        raw = json.loads(out.stdout or "{}")
    except (OSError, subprocess.SubprocessError, ValueError):
        return {}
    data = {}
    for tool, entries in raw.items():
        if not isinstance(entries, list):
            entries = [entries]
        for e in entries:
            if isinstance(e, dict) and e.get("active"):
                data[tool] = e.get("version", "")
                break
    try:
        CACHE.mkdir(parents=True, exist_ok=True)
        MISE_LS_CACHE.write_text(json.dumps({"at": time.time(), "data": data}))
    except OSError:
        pass
    return data


def _mise_outdated(ttl: int = TTL) -> dict:
    """What mise already knows. One call covers every tool it manages.

    Cached because `mise outdated` queries every tool's upstream and takes
    seconds; the answer does not change between two refreshes a minute apart.
    """
    if not shutil.which("mise"):
        return {}
    try:
        blob = json.loads(MISE_CACHE.read_text())
        if time.time() - blob.get("at", 0) < ttl:
            return blob.get("data") or {}
    except (OSError, ValueError):
        pass
    try:
        out = subprocess.run(["mise", "outdated", "--json"],
                             capture_output=True, text=True, timeout=120)
        data = json.loads(out.stdout or "{}")
    except (OSError, subprocess.SubprocessError, ValueError):
        return {}
    try:
        CACHE.mkdir(parents=True, exist_ok=True)
        MISE_CACHE.write_text(json.dumps({"at": time.time(), "data": data}))
    except OSError:
        pass
    return data


def _load_latest() -> dict:
    try:
        blob = json.loads(LATEST_CACHE.read_text())
        if time.time() - blob.get("at", 0) < TTL:
            return blob.get("versions", {})
    except (OSError, ValueError):
        pass
    return {}


def _save_latest(versions: dict) -> None:
    try:
        CACHE.mkdir(parents=True, exist_ok=True)
        LATEST_CACHE.write_text(json.dumps({"at": time.time(),
                                            "versions": versions}))
    except OSError:
        pass


def _github_latest(repo: str) -> tuple[str, str]:
    """(version, release page) for the newest release, or ('','').

    Goes through the shared cache, so this competes with nothing else for the
    rate limit and costs nothing when the release has not moved.
    """
    import ghcache
    d = ghcache.latest_release(repo)
    if not isinstance(d, dict):
        return "", ""
    tag = d.get("tag_name") or ""
    m = _VERSION.search(tag)
    return (m.group(0) if m else tag.lstrip("v")), d.get("html_url", "")


def detect(check_updates: bool = True) -> list:
    """Every known agent that is actually installed, with its update state.

    Returns inventory.App objects so agents sit in the same list, the same
    Updates count and the same sorting as everything else -- being unmanaged
    by pacman is a packaging detail, not a reason for a separate screen.
    """
    import inventory

    from concurrent.futures import ThreadPoolExecutor

    present = [(spec, shutil.which(spec.id)) for spec in KNOWN]
    present = [(spec, path) for spec, path in present if path]
    if not present:
        return []

    with ThreadPoolExecutor(max_workers=8) as pool:
        mise_future = pool.submit(_mise_outdated) if check_updates else None
        installed_future = pool.submit(_mise_installed)
        versions = dict(zip(
            (spec.id for spec, _p in present),
            pool.map(lambda sp: _cached_version(sp[0].id, sp[1]), present)))
        mise = mise_future.result() if mise_future else {}
        mise_have = installed_future.result()

    cached = _load_latest() if check_updates else {}
    fetched = dict(cached)
    out = []

    for spec, path in present:
        real = os.path.realpath(path)
        current = versions.get(spec.id, "")

        # Who owns the update. Asked of mise, because a shimmed binary
        # resolves to the mise executable itself and tells you nothing.
        managed_by_mise = spec.id in mise_have or "/mise/installs/" in real
        latest, release_url, update_cmd, manager = "", "", [], "self"

        if managed_by_mise:
            manager = "mise"
            update_cmd = ["mise", "upgrade", spec.id]
            info = mise.get(spec.id) or {}
            latest = info.get("latest") or ""
            release_url = info.get("release_url") or ""
            # mise reports the version it has *installed*, which is the honest
            # comparison -- `claude --version` can be an older linked build.
            current = info.get("current") or mise_have.get(spec.id) or current
        else:
            update_cmd = list(spec.self_update)
            if spec.repo and check_updates:
                if spec.repo in cached:
                    latest, release_url = cached[spec.repo]
                else:
                    latest, release_url = _github_latest(spec.repo)
                    if latest:
                        fetched[spec.repo] = [latest, release_url]

        update = ""
        if latest and current and _newer(latest, current):
            update = latest

        # The rest of the inventory stores this as one (count, kind) tuple;
        # matching that is what lets agents use the same row renderer.
        behind = (inventory.versions_behind(current, latest)
                  if update else (0, ""))

        out.append(inventory.App(
            name=spec.name,
            version=current or "unknown",
            source=AGENT,
            description=f"AI coding agent · managed by {manager}",
            update=update,
            exec_path=path,
            extra={
                "key": spec.id,
                "binary": spec.id,
                "manager": manager,
                "update_cmd": update_cmd,
                "repo": spec.repo,
                "url": release_url or spec.home,
                "home": spec.home,
                "note": spec.note,
                "domain": spec.domain,
                "behind": behind,
                "is_app": False,
                "real_path": real,
            },
        ))

    if fetched != cached:
        _save_latest(fetched)
    return out


def mise_tools(check_updates: bool = True) -> list:
    """Everything mise manages that is not one of the agents above.

    Same update mechanism, same staleness problem, no reason to leave them
    out just because they are not AI tooling.
    """
    import inventory

    have = _mise_installed()
    if not have:
        return []
    outdated = _mise_outdated() if check_updates else {}

    out = []
    for tool, version in sorted(have.items()):
        if tool in BY_ID:
            continue                    # already reported as an agent
        info = outdated.get(tool) or {}
        latest = info.get("latest") or ""
        update = latest if (latest and _newer(latest, version)) else ""
        name, domain = TOOL_DOMAINS.get(tool, (tool, ""))
        out.append(inventory.App(
            name=name,
            version=info.get("current") or version,
            source=inventory.TOOL,
            description=f"Developer tool · managed by mise",
            update=update,
            exec_path=shutil.which(tool) or "",
            extra={
                "key": tool,
                "manager": "mise",
                "domain": domain,
                "update_cmd": ["mise", "upgrade", tool],
                "url": info.get("release_url") or (f"https://{domain}" if domain else ""),
                "behind": (inventory.versions_behind(version, latest)
                           if update else (0, "")),
                "is_app": False,
            }))
    return out


def _newer(latest: str, current: str) -> bool:
    """True when `latest` is a higher version than `current`.

    A string compare says 0.9.9 > 0.10.2, which is the bug this exists to
    avoid. Unparseable pairs (Cursor's dated builds vs a tag) compare as
    equal rather than claiming an update that may not exist.
    """
    def key(v):
        parts = re.findall(r"\d+", v or "")
        return tuple(int(x) for x in parts[:4])

    a, b = key(latest), key(current)
    if not a or not b:
        return False
    return a > b
