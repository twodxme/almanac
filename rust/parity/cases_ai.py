"""Parity cases for ai.py.

ai.py fixes HOME, CLAUDE, CACHE and MCP_CACHE at import time, so the Python
side patches them per case, and the Rust side gets HOME / XDG_CACHE_HOME in
its `env`. A fixture home (skills, plugins, settings.json, marketplaces) is
shared by both sides -- the readers only read it, and the paths they report
must be the same strings. The almanac cache is per side: Python writes into
one temp dir, Rust into another seeded with the same files (`files`), and the
MCP cache the call leaves behind is returned with its `at` masked.

`claude` is a fake on PATH for everything except the real-machine cases,
which read the real ~/.claude and the real `claude plugin list`, with every
cache pointed at temp dirs (the MCP list and agent caches seeded from the
real ones, re-stamped as fresh, so nothing hits the network and nothing under
~/.cache is written).
"""
import contextlib
import json
import os
import tempfile
import time
from datetime import date, timedelta
from pathlib import Path
from unittest import mock

import agents
import ai
import ghcache

_ROOT = Path(tempfile.mkdtemp(prefix="almanac-parity-ai-"))
_n = 0
NOW = time.time()
REAL_HOME = str(Path.home())

_FAKE = _ROOT / "fakebin"
_FAKE.mkdir()
_EMPTY = _ROOT / "emptybin"
_EMPTY.mkdir()
_claude = _FAKE / "claude"
_claude.write_text(
    "#!/bin/sh\n"
    'case "$1 $2" in\n'
    '  "mcp list") cat "$FAKE_CLAUDE_DIR/mcp.txt" 2>/dev/null ;;\n'
    '  "plugin list") cat "$FAKE_CLAUDE_DIR/plugins.txt" 2>/dev/null ;;\n'
    '  *) echo "2.0.5 (Claude Code)" ;;\n'
    "esac\n")
_claude.chmod(0o755)
FAKE_PATH = f"{_FAKE}:/usr/bin"


def _fake_dir(mcp=None, plugins=None):
    global _n
    _n += 1
    d = _ROOT / f"claude{_n}"
    d.mkdir()
    if mcp is not None:
        (d / "mcp.txt").write_text(mcp)
    if plugins is not None:
        (d / "plugins.txt").write_text(plugins)
    return str(d)


def _pair():
    global _n
    _n += 1
    py, rs = _ROOT / f"py{_n}", _ROOT / f"rs{_n}"
    py.mkdir()
    rs.mkdir()
    return py, rs


def _home(tree):
    """A fixture home: {relative path: text | bytes}."""
    global _n
    _n += 1
    h = _ROOT / f"home{_n}"
    h.mkdir()
    for rel, body in tree.items():
        p = h / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        if isinstance(body, bytes):
            p.write_bytes(body)
        elif body is None:
            p.mkdir(exist_ok=True)
        else:
            p.write_text(body)
    return str(h)


def _read_back(p):
    try:
        v = json.loads(Path(p).read_text())
    except (OSError, ValueError):
        return None
    if isinstance(v, dict) and isinstance(v.get("at"), (int, float)) and not isinstance(v.get("at"), bool):
        v["at"] = "<number>"
    return v


@contextlib.contextmanager
def _patched(base, home, env):
    cache = base / "almanac"
    patches = [
        mock.patch.object(ai, "CACHE", cache),
        mock.patch.object(ai, "MCP_CACHE", cache / "mcp-list.json"),
        mock.patch.object(agents, "CACHE", cache),
        mock.patch.object(agents, "LATEST_CACHE", cache / "agent-latest.json"),
        mock.patch.object(agents, "VERSION_CACHE", cache / "agent-versions.json"),
        mock.patch.object(agents, "MISE_CACHE", cache / "mise-outdated.json"),
        mock.patch.object(ghcache, "CACHE", cache),
        mock.patch.object(ghcache, "DIR", cache / "github"),
        mock.patch.object(ghcache, "STATE", cache / "github-rate.json"),
        mock.patch.dict(os.environ, {k: v for k, v in env.items() if v is not None}),
    ]
    if home is not None:
        patches += [mock.patch.object(ai, "HOME", Path(home)),
                    mock.patch.object(ai, "CLAUDE", Path(home) / ".claude")]
    with contextlib.ExitStack() as st:
        for p in patches:
            st.enter_context(p)
        for k, v in env.items():
            if v is None:
                os.environ.pop(k, None)
        yield cache


def case(func, kw, fn, home=None, env=None, files=None, readback=False):
    """files: {path relative to the almanac cache dir: text | None}."""
    py, rs = _pair()
    env = dict(env or {})
    if home is not None:
        env["HOME"] = home
    files = files or {}
    rs_files = {str(rs / "almanac" / k): v for k, v in files.items()}

    def run():
        with _patched(py, home, env) as cache:
            for k, v in files.items():
                p = cache / k
                p.parent.mkdir(parents=True, exist_ok=True)
                if v is not None:
                    p.write_text(v)
            out = fn()
            if readback:
                return [out, _read_back(cache / "mcp-list.json")]
            return out

    args = {**kw, "env": {**env, "XDG_CACHE_HOME": str(rs)}, "files": rs_files}
    return func, args, run


def _raises(fn):
    """A reader whose exception the Rust side reports as {"error": ...}."""
    def run():
        try:
            return fn()
        except Exception as exc:                       # noqa: BLE001
            return {"error": str(exc)}
    return run


def _real_cache(name, key):
    """A real almanac cache file re-stamped as fresh (read only)."""
    try:
        blob = json.loads((Path(REAL_HOME) / ".cache/almanac" / name).read_text())
    except (OSError, ValueError):
        return None
    blob["at"] = NOW
    blob.setdefault(key, {} if key != "rows" else [])
    return json.dumps(blob)


MCP_OUT = """Checking MCP server health...

claude.ai Gmail: https://gmailmcp.googleapis.com/mcp/v1 - ✓ Connected
claude.ai Stripe: https://mcp.stripe.com - ! Needs authentication
claude.ai Uber Eats: https://mcp.ubereats.com/eats-claude/mcp (HTTP) - ✘ Failed to connect — HTTP 429: Error POSTing to endpoint: {"code":"rtapi.too_many_requests"}
local-tool: npx -y @acme/mcp-server --port 3000 - ✓ Connected
weird:name: http://10.0.0.1:8080/x - ✘ Failed to connect
plugin:context7:context7: https://mcp.context7.com/mcp (HTTP) - Connected
ünïcødé sërvér: https://例え.jp/mcp - ✓ Connected
broken line without separator
   spaced:    stdio-thing    -    Auth required
no-target: - x
a: b - c - d
"""


def cases():
    # -- constants and the dataclass ------------------------------------------
    yield ("constants", {},
           lambda: {"HOME": str(ai.HOME), "CLAUDE": str(ai.CLAUDE), "CACHE": str(ai.CACHE),
                    "MCP_CACHE": str(ai.MCP_CACHE), "MCP_TTL": ai.MCP_TTL,
                    "kinds": [ai.HARNESS, ai.MCP, ai.SKILL, ai.PLUGIN, ai.MARKET, ai.HOOK]})
    for fields in [{"kind": "mcp", "name": "x"},
                   {"kind": "hook", "name": "ü", "detail": "d", "status": "s", "severity": "warn",
                    "action": ["a", "b"], "items": ["i"], "harness": "Codex",
                    "fallback_icon": "f", "icon_key": "k"}]:
        yield "Entry", {"fields": fields}, lambda f=fields: ai.Entry(**f)

    # -- small helpers ----------------------------------------------------------
    for repo in ["anthropics/claude-code", "", "/x", "solo", "ü/ñ", None, "a/b/c"]:
        yield "github_avatar", {"repo": repo}, lambda r=repo: ai._github_avatar(r)
    for t in ["https://mcp.example.org/x", "http://10.0.0.1:8080/x", "HTTPS://upper.org",
              "npx foo", "", "ws://h", "https://", "https:///x", "git+ssh://a.b/c",
              "https://例え.jp/mcp", "https://a b/c", "x https://later.org", "h2://x\ny"]:
        yield "host", {"target": t}, lambda t=t: ai._host(t)
    for cmd, t in [(["echo", "hi"], 60), (["sh", "-c", "echo out; echo err >&2"], 60),
                   (["sh", "-c", "printf 'ünï\\nx'; exit 3"], 60),
                   (["/does/not/exist"], 60), (["sleep", "5"], 1), ([str(_ROOT)], 60)]:
        yield "run", {"cmd": cmd, "timeout": t}, lambda c=cmd, t=t: ai._run(c, t)

    # -- _frontmatter -------------------------------------------------------------
    long_tail = "ü" * 5000
    fm_home = _home({
        "plain.md": "---\nname: my-skill\ndescription: Does a thing.\n---\nbody\n",
        "folded.md": "---\nName: x\ndescription: >\n  one\n  two\n\n  three\nother: |-\n  kept\n---\n",
        "literal.md": "---\ndescription: |\n  a\n  b\nname:   spaced   \n---",
        "none.md": "no frontmatter here\nname: x\n",
        "unclosed.md": "---\nname: open\ndescription: never closed\n" + long_tail,
        "unclosed2.md": "---\n" + "x" * 3990 + "\nname: cut\n",
        "crlf.md": "---\r\nname: crlf\r\ndescription: >\r\n  a\r\n  b\r\n---\r\n",
        "cr.md": "---\rname: cr\rdescription: old mac\r---\r",
        "uni.md": "---\nname: ünïcødé\ndescription: 日本語の説明 — ok\nnäme: no\n---\n",
        "seps.md": "---\nname: a\x0bdescription: b c\x1cd: e\n---\n",
        "dashes.md": "----\nname: four\n---\n",
        "empty.md": "",
        "only.md": "---",
        "immediate.md": "---\n---\nname: after\n",
        "bad-utf8.md": b"---\nname: caf\xe9\ndescription: \xff\xfe ok\n---\n",
        "keys.md": "---\nfoo_bar-baz: 1\n9bad: no\n  name: indented\nname:\n  folded on next\n---\n",
        "tabs.md": "---\nname:\tTabbed\t\ndescription:  \x1f trailing sep \x1f\n---\n",
        "dir.md": None,
    })
    for f in ["plain.md", "folded.md", "literal.md", "none.md", "unclosed.md", "unclosed2.md",
              "crlf.md", "cr.md", "uni.md", "seps.md", "dashes.md", "empty.md", "only.md",
              "immediate.md", "bad-utf8.md", "keys.md", "tabs.md", "dir.md", "missing.md"]:
        p = str(Path(fm_home) / f)
        yield "frontmatter", {"path": p}, lambda p=p: ai._frontmatter(Path(p))

    # -- _tilde -----------------------------------------------------------------------
    for p in [REAL_HOME + "/.local/bin/claude", REAL_HOME, REAL_HOME + "x/y", "/usr/bin/x",
              "", "~/x", REAL_HOME + "/", REAL_HOME + "/ü"]:
        yield "tilde", {"path": p}, lambda p=p: ai._tilde(p)
    for h, p in [("/home/tim/", "/home/tim/a"), ("/", "/usr/bin"), ("/home/ü", "/home/ü/b"),
                 ("/home/tim//", "/home/tim/c")]:
        yield case("tilde", {"path": p}, lambda p=p: ai._tilde(p), env={"HOME": h})

    # -- harnesses ----------------------------------------------------------------------
    real_latest = _real_cache("agent-latest.json", "versions")
    real_mise = _real_cache("mise-outdated.json", "data")
    real_files = {k: v for k, v in {"agent-latest.json": real_latest,
                                    "mise-outdated.json": real_mise}.items() if v}
    fenv = {"PATH": FAKE_PATH, "FAKE_CLAUDE_DIR": _fake_dir()}
    no_update = json.dumps({"at": NOW, "versions": {"anthropics/claude-code": ["2.0.5", "https://rel"]}})
    update = json.dumps({"at": NOW, "versions": {"anthropics/claude-code": ["2.1.9", "https://rel"]}})
    yield case("harnesses", {}, _raises(ai.harnesses), env=fenv, files={"agent-latest.json": no_update})
    yield case("harnesses", {}, _raises(ai.harnesses), env=fenv, files={"agent-latest.json": update})
    # mise owns claude here, so its update comes from the mise-outdated cache.
    mise_up = json.dumps({"at": NOW, "data": {"claude": {"current": "2.0.5", "latest": "3.1.0",
                                                         "release_url": "https://rel/ü"}}})
    yield case("harnesses", {}, _raises(ai.harnesses), env=fenv,
               files={"agent-latest.json": update, "mise-outdated.json": mise_up})
    yield case("harnesses", {}, _raises(ai.harnesses), env={"PATH": str(_EMPTY)})
    # The real machine.
    yield case("harnesses", {}, _raises(ai.harnesses), files=real_files)

    # -- mcp_servers ------------------------------------------------------------------------
    menv = {"PATH": FAKE_PATH, "FAKE_CLAUDE_DIR": _fake_dir(mcp=MCP_OUT)}
    mc = "mcp-list.json"
    row = {"kind": "mcp", "name": "cached", "detail": "https://c.org", "status": "connected"}
    for files in [
        {},
        {mc: json.dumps({"at": NOW, "rows": [row]})},
        {mc: json.dumps({"at": NOW - ai.MCP_TTL - 5, "rows": [row]})},
        {mc: json.dumps({"rows": [row]})},
        {mc: json.dumps({"at": NOW})},
        {mc: json.dumps({"at": NOW, "rows": []})},
        {mc: json.dumps({"at": NOW, "rows": ""})},
        {mc: json.dumps({"at": NOW, "rows": {}})},
        {mc: json.dumps({"at": NOW, "rows": "abc"})},
        {mc: json.dumps({"at": NOW, "rows": None})},
        {mc: json.dumps({"at": NOW, "rows": [{"kind": "mcp"}]})},
        {mc: json.dumps({"at": NOW, "rows": [{**row, "bogus": 1}]})},
        {mc: json.dumps({"at": NOW, "rows": [row, 5]})},
        {mc: json.dumps({"at": "yesterday", "rows": [row]})},
        {mc: json.dumps({"at": True, "rows": [row]})},
        {mc: json.dumps([1, 2])},
        {mc: "not json"},
    ]:
        yield case("mcp_servers", {}, _raises(ai.mcp_servers), env=menv, files=files, readback=True)
    for out in ["", "Checking MCP server health...\n", "No MCP servers configured.\n"]:
        yield case("mcp_servers", {}, _raises(ai.mcp_servers),
                   env={"PATH": FAKE_PATH, "FAKE_CLAUDE_DIR": _fake_dir(mcp=out)}, readback=True)
    yield case("mcp_servers", {}, _raises(ai.mcp_servers), env={"PATH": str(_EMPTY)}, readback=True)
    # The real machine, from its own cache.
    real_mcp = _real_cache("mcp-list.json", "rows")
    if real_mcp:
        yield case("mcp_servers", {}, _raises(ai.mcp_servers), files={mc: real_mcp}, readback=True)

    # -- skills --------------------------------------------------------------------------------
    sk_home = _home({
        ".claude/skills/beta/SKILL.md": "---\nname: beta-skill\ndescription: " + "d" * 400 + "\n---\n",
        ".claude/skills/alpha/SKILL.md": "---\ndescription: no name, ünï " + "é" * 400 + "\n---\n",
        ".claude/skills/Zed/SKILL.md": "no header",
        ".claude/skills/empty/README.md": "x",
        ".claude/skills/loose.md": "x",
        ".claude/skills/.hidden/SKILL.md": "---\nname: hidden\n---\n",
        ".claude/plugins/cache/mkt/plug/1.0/skills/one/SKILL.md": "---\nname: one\ndescription: from a plugin\n---\n",
        ".claude/plugins/cache/mkt/plug/1.0/skills/two/SKILL.md": "---\ndescription: two\n---\n",
        ".claude/plugins/marketplaces/m/plugins/p/skills/a/SKILL.md": "---\nname: pa\n---\n",
        ".claude/plugins/skills/top/SKILL.md": "---\nname: top\n---\n",
        ".claude/plugins/x/skills/notdir": "x",
        ".claude/plugins/x/skills/dirskill/SKILL.md/inner": "x",
        ".claude/plugins/x/skills/deep/sub/SKILL.md": "not matched",
        ".claude/plugins/.h/skills/hid/SKILL.md": "---\nname: hid\n---\n",
    })
    yield case("skills", {}, _raises(ai.skills), home=sk_home)
    yield case("skills", {}, _raises(ai.skills), home=_home({"x": "y"}))
    yield case("skills", {}, _raises(ai.skills), home=_home({".claude/skills": "a file"}))
    yield case("skills", {}, _raises(ai.skills), home=REAL_HOME)

    # -- plugins ---------------------------------------------------------------------------------
    pl = [
        {"id": "a@m", "name": "alpha", "version": "1.0", "description": "Alpha ü", "enabled": True},
        {"id": "b@m", "enabled": False},
        {"name": "", "id": "", "enabled": 0},
        {"name": "c", "enabled": None},
        {"name": "d", "version": None, "description": None},
        {},
    ]
    for out in [json.dumps(pl), "", "not json", json.dumps({"a": 1}), "[]", json.dumps([pl[0], 5]),
                "  " + json.dumps(pl[:1]) + "\n"]:
        yield case("plugins", {}, _raises(ai.plugins),
                   env={"PATH": FAKE_PATH, "FAKE_CLAUDE_DIR": _fake_dir(plugins=out)})
    yield case("plugins", {}, _raises(ai.plugins), env={"PATH": str(_EMPTY)})
    yield case("plugins", {}, _raises(ai.plugins))     # the real machine

    # -- marketplaces -------------------------------------------------------------------------------
    loc = _home({"plugins/a": None, "plugins/b": None, "plugins/.c": None, "plugins/file": "x"})
    fresh = (date.today() - timedelta(days=3)).isoformat() + "T01:02:03Z"
    old = (date.today() - timedelta(days=30)).isoformat() + "T01:02:03Z"
    edge = (date.today() - timedelta(days=14)).isoformat()
    km = ".claude/plugins/known_marketplaces.json"
    variants = [
        {"official": {"source": {"source": "github", "repo": "anthropics/claude-plugins-official"},
                      "installLocation": loc, "lastUpdated": fresh},
         "old": {"source": {"source": "git"}, "installLocation": loc + "/", "lastUpdated": old},
         "edge": {"source": {}, "installLocation": "/does/not/exist", "lastUpdated": edge},
         "never": {"installLocation": "", "source": None},
         "badday": {"lastUpdated": "2026-02-30"},
         "words": {"lastUpdated": "yesterday"},
         "spaces": {"lastUpdated": " 2020-1-1 "},
         "under": {"lastUpdated": "2_020-01-01"},
         "ünï": {"source": {"repo": "ü/ñ"}, "lastUpdated": "2026"}},
        {},
        [],
        None,
        [1],
        {"m": "notadict"},
        {"m": {"source": "github"}},
    ]
    for v in variants:
        h = _home({km: json.dumps(v)})
        yield case("marketplaces", {}, _raises(ai.marketplaces), home=h)
    yield case("marketplaces", {}, _raises(ai.marketplaces), home=_home({km: "{nope"}))
    yield case("marketplaces", {}, _raises(ai.marketplaces), home=_home({}))
    yield case("marketplaces", {}, _raises(ai.marketplaces), home=REAL_HOME)

    # -- hooks -----------------------------------------------------------------------------------------
    hk_home = _home({".claude/hooks/ok.sh": "#!/bin/sh\n", "status.sh": "x"})
    st = ".claude/settings.json"
    hook_variants = [
        {"hooks": {
            "SessionStart": [{"matcher": "*", "hooks": [
                {"type": "command", "command": f"bash '{hk_home}/.claude/hooks/ok.sh' session"},
                {"type": "command", "command": "bash /nowhere/gone.sh"},
                {"type": "command", "command": "echo no path"},
                {"type": "command"}]}],
            "PreToolUse": [{"hooks": [{"command": f"\"{hk_home}/.claude/hooks/ok.sh\""}]},
                           {"matcher": "", "hooks": []}, {"matcher": None, "hooks": None},
                           {"matcher": "Bash|Edit", "hooks": [{"command": "/ünï/x"}]}],
            "Stop": "notalist"},
         "statusLine": {"type": "command", "command": 'bash "$HOME/status.sh"'}},
        {"statusLine": {"command": "~/missing-status.sh --x"}},
        {"statusLine": {"command": "echo plain"}},
        {"statusLine": {"command": "/usr/bin/env true"}},
        {"statusLine": {"command": ""}},
        {"statusLine": None},
        {"hooks": None},
        {},
        [],
        {"hooks": [1]},
        {"hooks": {"E": ["g"]}},
        {"hooks": {"E": [{"hooks": ["h"]}]}},
        {"hooks": {"E": [{"hooks": {"k": 1}}]}},
        {"hooks": {"E": [{"hooks": 5}]}},
        {"hooks": {"E": [{"hooks": [{"command": 5}]}]}},
        {"statusLine": "cmd"},
        {"statusLine": {"command": 7}},
    ]
    for v in hook_variants:
        h = _home({st: json.dumps(v)})
        # The hook scripts live in hk_home; status scripts resolve against $HOME.
        for rel in [".claude/hooks/ok.sh", "status.sh"]:
            p = Path(h) / rel
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text("x")
        yield case("hooks", {}, _raises(ai.hooks), home=h)
    yield case("hooks", {}, _raises(ai.hooks), home=_home({st: "garbage"}))
    yield case("hooks", {}, _raises(ai.hooks), home=_home({}))
    yield case("hooks", {}, _raises(ai.hooks), home=REAL_HOME)

    # -- groups --------------------------------------------------------------------------------------------
    yield "group_specs", {}, lambda: [(t, d, f.__name__) for t, d, f in ai.group_specs()]
    lg_home = _home({st: json.dumps([1]),
                     ".claude/skills/s/SKILL.md": "---\nname: s\n---\n"})
    for name in ["skills", "hooks", "marketplaces", "plugins", "mcp_servers"]:
        yield case("load_group", {"loader": name, "title": "T"},
                   lambda n=name: ai.load_group(getattr(ai, n), "T"),
                   home=lg_home, env={"PATH": str(_EMPTY)})

    def boom():
        raise RuntimeError("boom")
    yield "load_group", {"loader": "boom", "title": "Hooks"}, lambda: ai.load_group(boom, "Hooks")
    yield "load_group", {"loader": "panics", "title": "ü"}, lambda: ai.load_group(boom, "ü")

    sv_env = {"PATH": FAKE_PATH,
              "FAKE_CLAUDE_DIR": _fake_dir(mcp=MCP_OUT, plugins=json.dumps(pl))}
    yield case("survey", {}, ai.survey, home=sk_home, env=sv_env,
               files={"agent-latest.json": update})
    yield case("survey", {}, ai.survey, home=lg_home, env={"PATH": str(_EMPTY)})
    real_sv = dict(real_files)
    if real_mcp:
        real_sv[mc] = real_mcp
        yield case("survey", {}, ai.survey, files=real_sv)   # the real machine

    groups = [
        ("A", "a", [ai.Entry(kind="mcp", name="x", severity="warn"),
                    ai.Entry(kind="mcp", name="y", severity="ok")]),
        ("B", "b", []),
        ("C", "c", [ai.Entry(kind="hook", name="z", severity="warn")]),
        ("A", "dup", [ai.Entry(kind="mcp", name="q")]),
    ]
    import dataclasses
    as_json = [[t, d, [dataclasses.asdict(e) for e in es]] for t, d, es in groups]
    for gs, js in [(groups, as_json), ([], [])]:
        yield "counts", {"groups": js}, lambda g=gs: ai.counts(g)
        yield "needs_attention", {"groups": js}, lambda g=gs: ai.needs_attention(g)
    yield from _review_round1()
    yield from _review_round2()


def _review_round1():
    """Adversarial review, round 1: cases probing suspected divergences."""
    import subprocess
    import sys

    # `_run` is text=True: universal newlines, and strict UTF-8 decoding
    # (a UnicodeDecodeError is a ValueError, which `_run` does not catch).
    for cmd in [["printf", "a\\r\\nb\\rc\\n"], ["printf", "ok \\377 bad"]]:
        yield "run", {"cmd": cmd, "timeout": 60}, lambda c=cmd: ai._run(c, 60)

    # `claude mcp list` printing a byte that is not UTF-8: Python raises out
    # of mcp_servers (and load_group turns it into an error row).
    d = _fake_dir()
    (Path(d) / "mcp.txt").write_bytes(
        b"srv: https://a.org - \xe2\x9c\x93 Connected\nbad\xff: https://b.org - \xe2\x9c\x93 Connected\n")
    yield case("mcp_servers", {}, _raises(ai.mcp_servers),
               env={"PATH": FAKE_PATH, "FAKE_CLAUDE_DIR": d}, readback=True)
    # ...and `claude plugin list --json` with one: json.loads never sees it,
    # the ValueError is caught, the answer is [].
    d = _fake_dir()
    (Path(d) / "plugins.txt").write_bytes(b'[{"name": "caf\xe9", "enabled": true}]')
    yield case("plugins", {}, _raises(ai.plugins), env={"PATH": FAKE_PATH, "FAKE_CLAUDE_DIR": d})

    # The MCP cache: Entry(**row) takes any JSON type in any field.
    menv = {"PATH": FAKE_PATH, "FAKE_CLAUDE_DIR": _fake_dir(mcp=MCP_OUT)}
    mc = "mcp-list.json"
    for rows in [[{"kind": "mcp", "name": "n", "detail": None}],
                 [{"kind": "mcp", "name": 5}],
                 [{"kind": "mcp", "name": "n", "items": [1, 2]}],
                 [{"kind": "mcp", "name": "n", "action": "argv-as-string"}]]:
        yield case("mcp_servers", {}, _raises(ai.mcp_servers), env=menv,
                   files={mc: json.dumps({"at": NOW, "rows": rows})}, readback=True)
    # Python's json accepts Infinity / out-of-range floats: an `at` of inf
    # is always fresh.
    for at in ["Infinity", "1e400"]:
        yield case("mcp_servers", {}, _raises(ai.mcp_servers), env=menv,
                   files={mc: '{"at": %s, "rows": [{"kind": "mcp", "name": "inf"}]}' % at},
                   readback=True)

    # plugins whose fields are not strings keep their JSON type in Python.
    for pl in [[{"name": 5, "enabled": True}],
               [{"name": "v", "version": 1.5, "description": ["a", "b"]}]]:
        yield case("plugins", {}, _raises(ai.plugins),
                   env={"PATH": FAKE_PATH, "FAKE_CLAUDE_DIR": _fake_dir(plugins=json.dumps(pl))})

    # marketplaces: lastUpdated as a number (not subscriptable), an
    # installLocation that is not a string (Path() raises), and dates written
    # in non-ASCII digits (int() accepts them).
    km = ".claude/plugins/known_marketplaces.json"
    for v in [{"m": {"lastUpdated": 1700000000}},
              {"m": {"installLocation": 5}},
              {"m": {"lastUpdated": "２０２０-０１-０１"}},
              {"m": {"lastUpdated": "١٤٤٠-٠١-٠١"}},
              {"m": {"source": {"repo": 7}}}]:
        yield case("marketplaces", {}, _raises(ai.marketplaces), home=_home({km: json.dumps(v)}))

    # hooks: a matcher that is not a string goes through repr(), which
    # escapes what is not printable.
    st = ".claude/settings.json"
    for matcher in [["a b\x01c​"], {"k": "\x7f"}, 1e16, ["it's \"q\""]]:
        v = {"hooks": {"E": [{"matcher": matcher, "hooks": [{"command": "echo"}]}]}}
        yield case("hooks", {}, _raises(ai.hooks), home=_home({st: json.dumps(v)}))

    # Symlinked skill directories and symlinks along the rglob walk.
    h = Path(_home({"ext/real/SKILL.md": "---\nname: linked\n---\n",
                    "ext/tree/skills/t/SKILL.md": "---\nname: viatree\n---\n",
                    "ext/sk/q/SKILL.md": "---\nname: viaskills\n---\n",
                    ".claude/plugins/a/skills/.keep": "",
                    ".claude/plugins/b/skills/s/.keep": "",
                    ".claude/plugins/c/.keep": ""}))
    pl_ = h / ".claude/plugins"
    (pl_ / "a/skills/linked").symlink_to(h / "ext/real")
    (pl_ / "a/skills/broken").symlink_to("/nonexistent")
    (pl_ / "b/skills/s/SKILL.md").symlink_to("/nonexistent")
    (pl_ / "linktree").symlink_to(h / "ext/tree")
    (pl_ / "c/skills").symlink_to(h / "ext/sk")
    yield case("skills", {}, _raises(ai.skills), home=str(h))

    # Invalid UTF-8 of several shapes: errors="replace" vs from_utf8_lossy.
    fm = _home({"t.md": b"---\nname: a\xf0\x9f\x98\nb: \xed\xa0\x80|\xc0\xaf|\xf4\x90\x80\x80|\xe2\x82\n---\n"})
    yield "frontmatter", {"path": fm + "/t.md"}, lambda p=fm + "/t.md": ai._frontmatter(Path(p))
    # An installLocation with a NUL in it.
    yield case("marketplaces", {}, _raises(ai.marketplaces),
               home=_home({km: json.dumps({"m": {"installLocation": "/tmp/a\u0000b"}})}))

    # CACHE is fixed at import from XDG_CACHE_HOME; an empty value is used
    # as-is by os.environ.get (a relative "almanac"), and HOME="" makes
    # Path.home() "/".
    for env in [{"XDG_CACHE_HOME": ""}, {"XDG_CACHE_HOME": "rel/dir"},
                {"HOME": "", "XDG_CACHE_HOME": None}]:
        def consts(env=env):
            e = {**os.environ, **{k: v for k, v in env.items() if v is not None}}
            for k, v in env.items():
                if v is None:
                    e.pop(k, None)
            code = ("import json, ai; print(json.dumps({'HOME': str(ai.HOME), 'CLAUDE': str(ai.CLAUDE),"
                    " 'CACHE': str(ai.CACHE), 'MCP_CACHE': str(ai.MCP_CACHE), 'MCP_TTL': ai.MCP_TTL,"
                    " 'kinds': [ai.HARNESS, ai.MCP, ai.SKILL, ai.PLUGIN, ai.MARKET, ai.HOOK]}))")
            out = subprocess.run([sys.executable, "-c", code], env=e, capture_output=True, text=True,
                                 cwd=str(Path(ai.__file__).parent))
            return json.loads(out.stdout)
        yield "constants", {"env": env}, consts


def _review_round2():
    """Adversarial review, round 2."""
    import stat

    menv = {"PATH": FAKE_PATH, "FAKE_CLAUDE_DIR": _fake_dir(mcp=MCP_OUT)}
    mc = "mcp-list.json"
    km = ".claude/plugins/known_marketplaces.json"
    st = ".claude/settings.json"
    deep = "[" * 200 + "]" * 200

    # Python's json nests ~1000 deep; serde_json stops at 128. A deeply
    # nested value anywhere in a file Python reads fine.
    yield case("plugins", {}, _raises(ai.plugins),
               env={"PATH": FAKE_PATH, "FAKE_CLAUDE_DIR": _fake_dir(plugins=deep)})
    yield case("plugins", {}, _raises(ai.plugins),
               env={"PATH": FAKE_PATH, "FAKE_CLAUDE_DIR": _fake_dir(
                   plugins='[{"name": "p", "x": %s}]' % deep)})
    yield case("hooks", {}, _raises(ai.hooks),
               home=_home({st: '{"statusLine": {"command": "echo hi"}, "x": %s}' % deep}))
    yield case("marketplaces", {}, _raises(ai.marketplaces),
               home=_home({km: '{"m": {"lastUpdated": "2020-01-01", "x": %s}}' % deep}))
    yield case("mcp_servers", {}, _raises(ai.mcp_servers), env=menv,
               files={mc: '{"at": %r, "rows": [{"kind": "mcp", "name": "deep"}], "x": %s}' % (NOW, deep)},
               readback=True)

    # `_run`: subprocess.run's timeout covers reading the pipes; a
    # grandchild holding stdout open past the timeout makes Python give up.
    yield "run", {"cmd": ["sh", "-c", "echo hi; sleep 3 &"], "timeout": 1}, \
        lambda: ai._run(["sh", "-c", "echo hi; sleep 3 &"], 1)

    # shutil.which uses os.access(X_OK): a claude with only the group's
    # execute bit is not runnable by its owner, so Python never calls it
    # (and never writes the MCP cache).
    gx = _ROOT / "groupx"
    gx.mkdir()
    f = gx / "claude"
    f.write_text("#!/bin/sh\necho 'x: https://x.org - Connected'\n")
    f.chmod(0o654)
    yield case("mcp_servers", {}, _raises(ai.mcp_servers), env={"PATH": str(gx)}, readback=True)

    # harnesses: agents.detect raising (a malformed latest-version cache)
    # propagates out of the Python reader; and a release url that is not a
    # string is kept as is.
    fenv = {"PATH": FAKE_PATH, "FAKE_CLAUDE_DIR": _fake_dir()}
    for versions in [{"anthropics/claude-code": ["2.1.9"]},
                     5,
                     {"anthropics/claude-code": ["2.1.9", 5]}]:
        yield case("harnesses", {}, _raises(ai.harnesses), env=fenv,
                   files={"agent-latest.json": json.dumps({"at": NOW, "versions": versions})})

    # marketplaces: lastUpdated a dict (slicing a dict raises KeyError) or a
    # non-empty list (sliceable; .split raises).
    for v in [{"m": {"lastUpdated": {"a": 1}}}, {"m": {"lastUpdated": ["2020"]}}]:
        yield case("marketplaces", {}, _raises(ai.marketplaces), home=_home({km: json.dumps(v)}))

    # A skill directory whose name is not UTF-8 (surrogateescape in Python).
    h = Path(_home({".claude/skills/ok/SKILL.md": "---\nname: ok\n---\n"}))
    bad = os.path.join(os.fsencode(h / ".claude/skills"), b"caf\xe9")
    os.mkdir(bad)
    with open(os.path.join(bad, b"SKILL.md"), "w") as fh:
        fh.write("no header\n")
    # The runner's JSON cannot carry a lone surrogate, so this compares the
    # `json.dumps` text, where Python writes it as `\udce9`.
    import dataclasses
    yield case("skills_json", {}, lambda: json.dumps([dataclasses.asdict(e) for e in ai.skills()]),
               home=str(h))
    # ...and the same under a plugin directory that is not UTF-8 (status).
    h2 = Path(_home({".claude/skills/ok/SKILL.md": "---\nname: ok\n---\n"}))
    pd = os.path.join(os.fsencode(h2 / ".claude/plugins"), b"pl\xff", b"skills", b"s\xe9")
    os.makedirs(pd)
    with open(os.path.join(pd, b"SKILL.md"), "w") as fh:
        fh.write("---\ndescription: d\n---\n")
    yield case("skills_json", {}, lambda: json.dumps([dataclasses.asdict(e) for e in ai.skills()]),
               home=str(h2))

    # Nesting past serde_json's limit, near Python's, and past it (Python
    # raises RecursionError, which no reader catches).
    for n in [1000, 20000, 50000]:
        dn = "[" * n + "]" * n
        yield case("plugins", {}, _raises(ai.plugins),
                   env={"PATH": FAKE_PATH, "FAKE_CLAUDE_DIR": _fake_dir(
                       plugins='[{"name": "p", "description": "x", "x": %s}, 7]' % dn)})
        yield case("hooks", {}, _raises(ai.hooks),
                   home=_home({st: '{"statusLine": {"command": "echo hi"}, "x": %s}' % dn}))
    # The kB figure in the message depends on where the stack stood when the
    # decoder started; only the rest of it is compared.
    def too_deep(fn=_raises(ai.plugins)):
        import re
        out = fn()
        if isinstance(out, dict) and "error" in out:
            out["error"] = re.sub(r"used \d+ kB", "used 8148 kB", out["error"])
        return out
    yield case("plugins", {}, too_deep,
               env={"PATH": FAKE_PATH, "FAKE_CLAUDE_DIR": _fake_dir(
                   plugins='[{"name": "p", "x": %s}]' % ("[" * 60000 + "]" * 60000))})
    # A malformed deep value is still a ValueError.
    yield case("plugins", {}, _raises(ai.plugins),
               env={"PATH": FAKE_PATH, "FAKE_CLAUDE_DIR": _fake_dir(plugins="[" * 300 + "]" * 299)})
    yield case("plugins", {}, _raises(ai.plugins),
               env={"PATH": FAKE_PATH, "FAKE_CLAUDE_DIR": _fake_dir(
                   plugins='[{"name": "p", "x": %s}]' % ("[" * 300 + "}" + "]" * 299))})
    # A deep value inside a field the reader keeps (the description).
    yield case("plugins", {}, _raises(ai.plugins),
               env={"PATH": FAKE_PATH, "FAKE_CLAUDE_DIR": _fake_dir(
                   plugins='[{"name": "p", "description": %s}]' % ('[{"a": "]["}, ' * 150 + '1' + ']' * 150))})

    # HOME unset: expanduser("~") and Path.home() fall back to the passwd
    # entry, not "/".
    yield case("tilde", {"path": REAL_HOME + "/a"}, lambda: ai._tilde(REAL_HOME + "/a"),
               env={"HOME": None})
    env = {"HOME": None}
    def consts_nohome():
        import subprocess
        import sys
        e = {k: v for k, v in os.environ.items() if k != "HOME"}
        code = ("import json, ai; print(json.dumps({'HOME': str(ai.HOME), 'CLAUDE': str(ai.CLAUDE),"
                " 'CACHE': str(ai.CACHE), 'MCP_CACHE': str(ai.MCP_CACHE), 'MCP_TTL': ai.MCP_TTL,"
                " 'kinds': [ai.HARNESS, ai.MCP, ai.SKILL, ai.PLUGIN, ai.MARKET, ai.HOOK]}))")
        out = subprocess.run([sys.executable, "-c", code], env=e, capture_output=True, text=True,
                             cwd=str(Path(ai.__file__).parent))
        return json.loads(out.stdout)
    yield "constants", {"env": {"HOME": None, "XDG_CACHE_HOME": os.environ.get("XDG_CACHE_HOME")}}, consts_nohome
