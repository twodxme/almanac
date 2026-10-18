"""Parity cases for plugin_setup: READMEs written into a temp dir, plus every
plugin really installed on this machine (read only)."""
import dataclasses
import atexit
import os
import shlex
import shutil
import tempfile
from pathlib import Path

import plugin_setup
import plugins

README = """# Plugin
## Requirements
You need an API key from the service.
```bash
sudo pacman -S --needed jq socat
yay -S some-helper-bin
```
## Installation
```bash
omarchy plugin add https://github.com/a/x --enable
make install PREFIX=~/.local
./scripts/setup.sh
curl -fsSL https://example.com/i.sh | sh
```
## Development
```bash
cargo test
make build
```
## Uninstall
```bash
./uninstall.sh
```
"""

# name -> (README file name or None, contents, extra files)
FIXTURES = {
    "basic": ("README.md", README, {"scripts/setup.sh": "echo configuring\n",
                                    "uninstall.sh": "rm -rf everything\n"}),
    "none": (None, "", {}),
    "lower": ("readme.md", "## Install\n```\nmake\n```\n", {}),
    "plain": ("README", "# Setup\n```sh\n$ cargo build --release\n$ cargo test\n```\n", {}),
    "crlf": ("README.md", "# Install\r\n```bash\r\nmake \\\r\n  install\r\nnpm i && npm run build\r\n```\r\nlog in first\r\n", {}),
    "nested": ("README.md", """# Thing
## Installation
### From source
```
paru -Syu foo-git bar++ "quoted thing"
pacman -S Foo baz
pacman -S 'a b' c
pacman -S -- -x y@1 z.z
go build ./... && ./install.sh --user && cd build && rm -rf ~/x
sudo make install
systemctl --user enable --now thing.service
bash scripts/run.sh
sh ../outside.sh
./missing.sh
cargo fmt
cargo build
cargo build
python3 -m pip install --user .
```
#### Configuration
Edit your hyprland.conf and add a keybind. Put your API_KEY in config.toml.
```toml
[thing]
key = "x"
```
## Testing
```
make
```
Your password goes here.
## Usage ##
Sign in with OAuth.
""", {"scripts/run.sh": "#!/bin/sh\n" + "\n".join(f"echo {i}" for i in range(100)) + "\n",
      "install.sh": "é" * 7000}),
    "unicode": ("README.md", "# Instalação — Setup ✓\n```\nnpm install ünïcode\nyay -S café pkg\n```\nUse sign-in ünder Setup\n## Requirements 🔑\napi key\n", {}),
    "unclosed": ("README.md", "# Install\n```\npacman -S 'oops\n```\n", {}),
    "badescape": ("README.md", "# Install\n```\npacman -S foo\\\\\n```\n", {}),
    "nofence": ("README.md", "# Install\nmake install\n```\nunterminated make\n", {}),
    "empty": ("README.md", "", {}),
    "invalid_utf8": ("README.md", b"# Install\n```\nmake \xff\xfe install\n```\n", {}),
    "byhand_only": ("README.md", "# Setup\nYou need an access token.\n## Configure\nEdit bindings.lua\n", {}),
    "fences_only_outside": ("README.md", "```\nmake\n```\n# Usage\n```\nmake run\n```\n", {}),
    # Round 1 review: Python's \w / \s / \b / re.I are not Rust regex's.
    "uni_word": ("README.md", "# Install\n```\nmake test\u0301\nmake test\u00bd\n./\u00bd\nmake cd\x1fx\npacman\x1f-S foo\npacman -S\u00bd bar\nsudo\x1cmake\n```\n", {}),
    "uni_case": ("README.md", "# \u0130nstall\n```\nmake\n```\nLOG \u0130N first\n# \u0131nstall\n```\ncargo build\n```\n# Build ci\u0301\n```\nnpm i\n```\n", {}),
    "uni_heading": ("README.md", "#\x1fSetup\x1f\n```\nmake\n```\nYour api key\n", {}),
    "uni_bound": ("README.md", "# Install\nsecrets\u00bd and xpassword then api\u0301key oauth\u0301\nconfig.yml\u00bd keybinds\n## Setup\nHYPRLAND.CONF and LOG\u0130N\n```\n./x-\u00e9\n./y-\n./z/\u0301\nmake x\u0301test\nmake check\u00bd\nmake\u2003check\nbash  c.sh\u0085\nsh d.sh-\npacman -S\u0301 baz\nmake\u3000all && npm\x1c&&\x1dcargo build\n```\n", {"c.sh": "c\n", "d.sh": "d\n"}),
    "uni_script": ("README.md", "# Install\n```\n./a.sh\u00bd\nbash b.sh\u0301\n```\n", {"a.sh": "secret a\n", "b.sh": "secret b\n"}),
    "badutf": ("README.md", b"# Install\n```\nmake \xf0\x9f\x98 \xed\xa0\x80 \xc0\xaf \xf4\x90\x80\x80 \xe0\x80 x\xf0\x9f\n```\n", {}),
    "unreadable_script": ("README.md", "# Install\n```\n./s.sh\n```\n", {"s.sh": "x\n"}),
    "unreadable_readme": ("README.md", "# Install\n```\nmake\n```\n", {}),
    # Round 2 review.
    "fold_case": ("README.md", "# \u017fetup\n```\nmake\n```\nYour pa\u017f\u017fword\n# Quic\u212a start\n```\ncargo build\n```\nedit the \u212aeybind\n# Te\u017ft\n```\nnpm i\n```\n", {}),
    "longchain": ("README.md", "# Install\n```\n./l0.sh\n```\n", {}),
    "selfloop": ("README.md", "# Install\n```\n./loop.sh\nbash ./a/../loop2.sh\n```\n", {}),
    "loop3": ("README.md", "# Install\n```\nbash ./a/../x.sh\n```\n", {"x.sh": "top\n", "d1/x.sh": "inner\n", "d2/keep": ""}),
    "nonutf8_target": ("README.md", "# Install\n```\n./a.sh\n```\n", {}),
    "chained": ("README.md", "## Getting started\n```\nsudo pacman -S --noconfirm a b && make && make check && ./a.sh\n```\n", {"a.sh": "x\r\ny\n"}),
}

_tmp = tempfile.mkdtemp(prefix="almanac-parity-plugin-setup-")
atexit.register(shutil.rmtree, _tmp, True)
DIRS = {}
for name, (fname, body, extra) in FIXTURES.items():
    root = Path(_tmp, name, "plugin")
    root.mkdir(parents=True)
    if fname:
        p = root / fname
        if isinstance(body, bytes):
            p.write_bytes(body)
        else:
            p.write_bytes(body.encode())
    for rel, content in extra.items():
        f = root / rel
        f.parent.mkdir(parents=True, exist_ok=True)
        f.write_bytes(content.encode())
    DIRS[name] = root
(Path(_tmp, "nested", "outside.sh")).write_text("echo outside\n")
# Round 2: a 45-link symlink chain to a real script (Python's realpath has
# no hop limit), a symlink loop, and a script whose real name is not UTF-8.
_lc = DIRS["longchain"]
(_lc / "real.sh").write_text("echo real\n")
for _i in range(45):
    os.symlink(f"l{_i + 1}.sh" if _i < 44 else "real.sh", _lc / f"l{_i}.sh")
os.symlink("loop.sh", DIRS["selfloop"] / "loop.sh")
os.symlink("loop2b.sh", DIRS["selfloop"] / "loop2.sh")
os.symlink("loop2.sh", DIRS["selfloop"] / "loop2b.sh")
# A three-link loop across folders: Python stops where it re-meets `a`,
# then `..` lands in the plugin folder.
os.symlink("d1/b", DIRS["loop3"] / "a")
os.symlink("../d2/c", DIRS["loop3"] / "d1" / "b")
os.symlink("../a", DIRS["loop3"] / "d2" / "c")
_nu = DIRS["nonutf8_target"]
Path(os.fsdecode(bytes(_nu) + b"/\xff.sh")).write_text("echo nu\n")
os.symlink(os.fsdecode(b"\xff.sh"), _nu / "a.sh")
os.chmod(DIRS["unreadable_script"] / "s.sh", 0)
os.chmod(DIRS["unreadable_readme"] / "README.md", 0)
# A README that is a directory, not a file.
Path(_tmp, "dirreadme", "plugin", "README.md").mkdir(parents=True)
DIRS["dirreadme"] = Path(_tmp, "dirreadme", "plugin")
# A symlinked plugin folder.
os.symlink(DIRS["nested"], Path(_tmp, "linked"))
DIRS["linked"] = Path(_tmp, "linked")


def _real():
    pdir = plugins.plugins_dir()
    try:
        return sorted(p for p in pdir.iterdir() if p.is_dir())
    except OSError:
        return []


def _wire(v):
    """A str holding surrogate-escaped bytes (a file name that is not UTF-8)
    cannot travel as JSON to Rust and back; spell the bytes `\\xff`, as the
    Rust port does (see `esc` there)."""
    if dataclasses.is_dataclass(v) and not isinstance(v, type):
        return {f.name: _wire(getattr(v, f.name)) for f in dataclasses.fields(v)}
    if isinstance(v, str):
        return v.encode("utf-8", "surrogateescape").decode("utf-8", "backslashreplace")
    if isinstance(v, dict):
        return {_wire(k): _wire(x) for k, x in v.items()}
    if isinstance(v, (list, tuple)):
        return [_wire(x) for x in v]
    return v


def _plan_cases(d, label=None):
    s = str(d)
    yield "plan", {"dir": s}, lambda: _wire(plugin_setup.plan(Path(s)))
    for f in ("terminal_script", "describe"):
        yield f, {"dir": s}, (lambda f=f: _wire(getattr(plugin_setup, f)(plugin_setup.plan(Path(s)))))
    yield "automatable", {"dir": s}, lambda: plugin_setup.plan(Path(s)).automatable
    yield "empty", {"dir": s}, lambda: plugin_setup.plan(Path(s)).empty


def cases():
    for d in DIRS.values():
        yield from _plan_cases(d)
    # Path spellings str(Path) normalises.
    for s in (str(DIRS["basic"]) + "/", str(DIRS["basic"]) + "/./", "/nonexistent/x/", ""):
        yield "plan", {"dir": s}, (lambda s=s: plugin_setup.plan(Path(s)))
    # The real machine, as it is.
    for d in _real():
        yield from _plan_cases(d)

    # Hand-made plans.
    plans = [
        plugin_setup.SetupPlan(Path("/tmp/a b/it's")),
        plugin_setup.SetupPlan(Path("/x"), readme="README.md", packages=["a", "b"]),
        plugin_setup.SetupPlan(Path("/x"), commands=["make", "echo 'hi' $HOME", "ünï"],
                               scripts={"b.sh": "1\n2\r\n3", "a.sh": ""},
                               by_hand=[("Credentials or a sign-in", "Setup"), ("Configuration you edit", "")]),
        plugin_setup.SetupPlan(Path("/x"), by_hand=[("Configuration you edit", "Ünï “x”")]),
        plugin_setup.SetupPlan(Path("rel//dir/."), scripts={"s.sh": "\n".join(str(i) for i in range(80))}),
    ]
    for p in plans:
        js = {"plugin_dir": str(p.plugin_dir), "readme": p.readme, "packages": p.packages,
              "commands": p.commands, "scripts": p.scripts, "by_hand": [list(x) for x in p.by_hand]}
        yield "terminal_script", {"plan": js}, (lambda p=p: plugin_setup.terminal_script(p))
        yield "describe", {"plan": js}, (lambda p=p: plugin_setup.describe(p))
        yield "automatable", {"plan": js}, (lambda p=p: p.automatable)
        yield "empty", {"plan": js}, (lambda p=p: p.empty)

    # Internals the plan is built from.
    for block in ("", "a\nb", "$ make \\\n  install\n# comment\n\n  ./x  ", "x \\", "a\x0bb\x1cc d\r\ne\rf",
                  "\x1f make \x1f", "$ $ x", "\\\n\\\ny", "ü \\\nñ"):
        yield "_logical_lines", {"block": block}, (lambda b=block: list(plugin_setup._logical_lines(b)))
    for text in ("", "# A\n## B\n### C\n## D\n# E", "#NoSpace\n####### seven\n#  Spaced  ##  \n",
                 "#\nnext line", "## Ünï #\n# x ###", "  # indented\n# real",
                 "#\x1fTitle\n## Install\x1f\n", "# A \u0085\n"):
        yield "_sections", {"text": text}, (lambda t=text: [p for _s, p in plugin_setup._sections(t)])
    for path in ([], ["Install"], ["Install", "Testing"], ["Usage"], ["CI"], ["Quickstart"],
                 ["Build", "Debug builds"], ["INSTALLATION"], ["Licence"], ["Prerequisites"], ["Overview"],
                 ["\u0130nstall"], ["\u0131nstall"], ["SET\u0130P"], ["Build ci\u0301"], ["Build ci\u00bd"],
                 ["\u017fetup"], ["Quic\u212a start"], ["Te\u017ft"], ["In\u017ftall / CI\u212a"]):
        yield "_is_setup", {"path": path}, (lambda p=path: plugin_setup._is_setup(p))
    for s in ("a b  c", "", "  ", "'a b' c", "a\\ b", "''", "a'b'c", "'x", "x\\", "\\'", "tab\there",
              'a "b c" d', '"a\\"b"', '"a\\b"', "ünï 'ç d'"):
        yield "shlex_split", {"s": s}, (lambda s=s: shlex.split(s, posix=True))
    for s in ("", "abc", "a b", "it's", "ünï", "a@b%c+d=e:f,g./-h", "$HOME", "~"):
        yield "shlex_quote", {"s": s}, (lambda s=s: shlex.quote(s))
