"""What a plugin still needs after it is installed, read from its README.

The marketplace marks some plugins "requires additional setup" and stops
there. Installing them is the same `omarchy plugin add` as any other; the
setup is whatever their README asks for next. This reads the README from the
installed checkout -- local, nothing fetched -- and sorts what it finds:

* **packages**  `pacman -S` / `yay -S` / `paru -S` lines: installed by
  Almanac itself, snapshotted and verified like any other install.
* **commands**  build steps, setup scripts and `systemctl --user` lines from
  the README's code blocks, run in a terminal in the plugin's directory --
  after the user has seen every line, and the scripts' own contents.
* **by hand**   what cannot be done for the user: API keys and logins,
  config and keybinding edits. Listed, with the README heading to read.

Only code blocks are read for commands: prose is not an instruction. A line
is taken only when it starts with a known tool, so an example output block
or a snippet of QML is never run. `sudo` lines are kept as written -- the
terminal shows the password prompt, and nothing is elevated silently.
"""
from __future__ import annotations

import re
import shlex
from dataclasses import dataclass, field
from pathlib import Path

_FENCE = re.compile(r"^```[^\n]*\n(.*?)^```", re.M | re.S)
_PKG = re.compile(r"^(?:sudo\s+)?(?:pacman|yay|paru)\s+-S\w*\s+(.+)$")
_PKG_NAME = re.compile(r"^[a-z0-9@][a-z0-9@._+-]*$")
# Commands worth running for the user, by the tool they start with.
_RUNNABLE = re.compile(
    r"^(?:sudo\s+)?(?:make|cmake|meson|ninja|cargo|go|npm|pnpm|yarn|bun|uv|pip|"
    r"pipx|python3?|gcc|cc|systemctl\s+--user|bash\s+\S+\.sh|sh\s+\S+\.sh|"
    r"\./[\w./-]+)\b")
# Never run, whatever the README says.
_NEVER = re.compile(r"(curl|wget)[^|]*\|\s*(sudo\s+)?(ba|z)?sh|rm\s+-rf\s+[/~]|"
                    r"omarchy[- ]plugin|git\s+clone|cd\s")
_BY_HAND = (
    (re.compile(r"\b(api[ _-]?key|access token|secret|password|log ?in|sign ?in|"
                r"credential|oauth)\b", re.I), "Credentials or a sign-in"),
    (re.compile(r"\b(keybind|bindings?\.(conf|lua)|hyprland\.(conf|lua)|"
                r"config\.(toml|json|ya?ml))\b", re.I), "Configuration you edit"),
)
_HEADING = re.compile(r"^(#{1,6})\s+(.+?)\s*#*$", re.M)
# Only these parts of a README are instructions for a user installing it.
# Everything else -- development, tests, uninstalling -- is not setup, and a
# README's `cargo clippy` or `uninstall.sh` must never be offered as such.
_SETUP_SECTION = re.compile(r"install|setup|set up|getting started|quick ?start|"
                            r"requirement|dependenc|prerequisite|build|configur|"
                            r"usage", re.I)
_OTHER_SECTION = re.compile(r"develop|contribut|test|uninstall|remov|troubleshoot|"
                            r"faq|changelog|release|licen|hack|debug|ci\b|lint", re.I)
# Commands that are development chores even inside an install section.
_CHORE = re.compile(r"\b(test|fmt|clippy|lint|check|bench|uninstall)\b")


@dataclass
class SetupPlan:
    plugin_dir: Path
    readme: str = ""                  # file name, '' when there is none
    packages: list = field(default_factory=list)
    commands: list = field(default_factory=list)
    scripts: dict = field(default_factory=dict)   # path -> contents, shown first
    by_hand: list = field(default_factory=list)   # (what, README heading)

    @property
    def automatable(self) -> bool:
        return bool(self.packages or self.commands)

    @property
    def empty(self) -> bool:
        return not (self.automatable or self.by_hand)


def _readme(plugin_dir: Path) -> Path | None:
    for name in ("README.md", "readme.md", "Readme.md", "README.MD", "README"):
        if (plugin_dir / name).is_file():
            return plugin_dir / name
    return None


def _logical_lines(block: str):
    """Code-block lines, with backslash continuations joined and prompts cut."""
    buf = ""
    for raw in block.splitlines():
        line = raw.strip()
        if line.startswith("$ "):
            line = line[2:]
        if line.endswith("\\"):
            buf += line[:-1] + " "
            continue
        line = (buf + line).strip()
        buf = ""
        if line and not line.startswith("#"):
            yield line


def _sections(text: str):
    """(start, heading path) for every heading, outermost first."""
    path: list[tuple[int, str]] = []
    out = []
    for m in _HEADING.finditer(text):
        level = len(m.group(1))
        path = [(lv, h) for lv, h in path if lv < level] + [(level, m.group(2))]
        out.append((m.start(), [h for _lv, h in path]))
    return out


def _is_setup(path: list[str]) -> bool:
    """A section is setup when its heading path says so and nothing in it
    says it is about something else."""
    joined = " / ".join(path)
    return bool(_SETUP_SECTION.search(joined)) and not _OTHER_SECTION.search(joined)


def plan(plugin_dir: Path) -> SetupPlan:
    out = SetupPlan(plugin_dir)
    path = _readme(plugin_dir)
    if path is None:
        return out
    out.readme = path.name
    text = path.read_text(errors="replace")
    sections = _sections(text)

    def heading_at(pos: int) -> list[str]:
        above = [p for start, p in sections if start <= pos]
        return above[-1] if above else []

    for fence in _FENCE.finditer(text):
        if not _is_setup(heading_at(fence.start())):
            continue
        block = fence.group(1)
        for line in _logical_lines(block):
            for part in (p.strip() for p in re.split(r"\s*&&\s*", line)):
                m = _PKG.match(part)
                if m:
                    for tok in shlex.split(m.group(1), posix=True) if '"' not in part else []:
                        if tok.startswith("-"):
                            continue
                        if _PKG_NAME.match(tok) and tok not in out.packages:
                            out.packages.append(tok)
                    continue
                if (_NEVER.search(part) or _CHORE.search(part)
                        or not _RUNNABLE.match(part)):
                    continue
                if part not in out.commands:
                    out.commands.append(part)
                script = re.match(r"^(?:sudo\s+)?(?:bash\s+|sh\s+)?(\.?/?[\w./-]+\.sh)\b", part)
                if script:
                    target = (plugin_dir / script.group(1)).resolve()
                    # Only scripts the plugin itself ships, never outside it.
                    if target.is_file() and plugin_dir.resolve() in target.parents:
                        out.scripts[str(target.relative_to(plugin_dir.resolve()))] = \
                            target.read_text(errors="replace")[:6000]

    # Things only a person can do, found in setup sections' prose, with the
    # heading to read under.
    prose = _FENCE.sub(lambda m: " " * len(m.group(0)), text)
    for pattern, label in _BY_HAND:
        for m in pattern.finditer(prose):
            where = heading_at(m.start())
            if _is_setup(where):
                out.by_hand.append((label, where[-1]))
                break
    return out


def terminal_script(p: SetupPlan) -> str:
    """The commands, as one shell script that stops at the first failure."""
    lines = [f"cd {shlex.quote(str(p.plugin_dir))}", "set -e"]
    for cmd in p.commands:
        lines.append(f"echo; echo '$ '{shlex.quote(cmd)}")
        lines.append(cmd)
    lines.append("echo; echo 'Setup finished.'")
    return "\n".join(lines)


def describe(p: SetupPlan) -> str:
    """Everything the plan would do, for the confirmation dialog."""
    parts = []
    if p.packages:
        parts.append("Packages, installed by Almanac\n  " + " ".join(p.packages))
    if p.commands:
        parts.append("Run in a terminal, in the plugin's folder\n"
                     + "\n".join(f"  $ {c}" for c in p.commands))
    for name, body in p.scripts.items():
        parts.append(f"Contents of {name}\n" + "\n".join(
            "  " + l for l in body.splitlines()[:60]))
    if p.by_hand:
        parts.append("Left for you\n" + "\n".join(
            f"  • {what}" + (f" — see “{h}” in the README" if h else "")
            for what, h in p.by_hand))
    return "\n\n".join(parts) or "The README asks for nothing beyond the install."
