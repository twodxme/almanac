#!/usr/bin/env python3
"""The non-pacman half of the scheduled update.

omarchy-auto-update handles repo and AUR packages, with holds for anything
that needs a human. This handles the three kinds of software it cannot see:
agent CLIs, tracked AppImages and Omarchy shell plugins.

It is a separate program rather than more shell because each of these already
has working logic in this app -- agents.detect() knows what is behind,
appimage.check() knows what a release contains -- and reimplementing that in
bash would give two answers that drift.

Run with no arguments it does only what auto-update.json enables.
"""
from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))

import autoupdate                       # noqa: E402


def log(msg: str) -> None:
    print(msg, flush=True)


def _run(argv, timeout=1800) -> bool:
    log(f"$ {' '.join(argv)}")
    try:
        out = subprocess.run(argv, capture_output=True, text=True, timeout=timeout)
    except (OSError, subprocess.SubprocessError) as exc:
        log(f"  failed to start: {exc}")
        return False
    for line in (out.stdout or "").splitlines()[-20:]:
        log(f"  {line}")
    if out.returncode != 0:
        for line in (out.stderr or "").splitlines()[-10:]:
            log(f"  {line}")
        log(f"  exited {out.returncode}")
    return out.returncode == 0


def update_agents() -> int:
    import agents

    done = 0
    for a in agents.detect():
        if not a.update:
            continue
        cmd = a.extra.get("update_cmd")
        if not cmd:
            log(f"{a.name}: {a.version} → {a.update} available, no self-updater")
            continue
        if agents.major_jump(a.version, a.update):
            # A new major version waits for a person: it is the kind of
            # update that changes flags and config formats.
            log(f"{a.name}: {a.version} → {a.update} is a major release, held")
            continue
        log(f"{a.name}: {a.version} → {a.update}")
        if _run(list(cmd)):
            done += 1
    if not done:
        log("agents: nothing to update")
    return done


def update_appimages() -> int:
    import appimage
    import inventory

    reg = inventory._load_registry()
    done = 0
    for key, rec in list(reg.items()):
        try:
            latest = appimage.check(rec)
        except Exception as exc:                        # noqa: BLE001
            log(f"{key}: check failed: {exc}")
            continue
        if not latest.get("url"):
            continue
        log(f"{key}: {rec.get('version')} → {latest['version']}")
        try:
            dest = Path(rec["path"])
            appimage.download(latest["url"], dest)
            rec["version"] = latest["version"]
            rec["latest"] = {}
            rec["checked_at"] = 0
            reg[key] = rec
            inventory.save_registry(reg)
            appimage.install_desktop_entry(dest, key)
            done += 1
            log(f"  installed {latest['version']}")
        except Exception as exc:                        # noqa: BLE001
            # download() swaps atomically, so a failure here leaves the
            # working build in place rather than a half-written file.
            log(f"  failed: {exc}")
    if not done:
        log("appimages: nothing to update")
    return done


def update_plugins() -> int:
    """Omarchy shell plugins. Off by default: these change the desktop."""
    ok = _run(["omarchy", "plugin", "update", "--yes"], timeout=900)
    return 1 if ok else 0


def main(argv=None) -> int:
    cfg = autoupdate.load()
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--agents", action="store_true")
    ap.add_argument("--appimages", action="store_true")
    ap.add_argument("--plugins", action="store_true")
    args = ap.parse_args(argv)

    # An explicit flag overrides the config; no flags means "do what the
    # settings say", which is how the timer invokes it.
    explicit = args.agents or args.appimages or args.plugins
    want = {
        "agents": args.agents if explicit else cfg.get("agents", True),
        "appimages": args.appimages if explicit else cfg.get("appimages", True),
        "plugins": args.plugins if explicit else cfg.get("plugins", False),
    }

    changed = 0
    if want["agents"]:
        log("== agent CLIs ==")
        changed += update_agents()
    if want["appimages"]:
        log("== AppImages ==")
        changed += update_appimages()
    if want["plugins"]:
        log("== Omarchy plugins ==")
        changed += update_plugins()
    if not any(want.values()):
        log("nothing enabled in auto-update.json")
    log(f"== {changed} item(s) updated ==")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
