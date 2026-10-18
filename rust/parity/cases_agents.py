"""Parity cases for agents.py.

agents.py fixes its cache paths at import time (CACHE = $XDG_CACHE_HOME/...),
so the Python side patches agents.CACHE & co. to a temp dir per case, and the
Rust side gets XDG_CACHE_HOME pointing at a *different* temp dir seeded with
the same files (`files` in the args). Each side starts from the same state and
neither can see what the other wrote. The cache file written by the call is
returned alongside the result, with its `at` timestamp masked.

Fake binaries (a fake `mise`, fake agents, scripts printing versions) live in
a shared temp bin dir that both sides only read. The real machine is read
through `mise ls`, `mise_tools` and `detect` with the caches pointed at temp
dirs -- seeded from the real mise-outdated cache, so nothing hits the network
and nothing under ~/.cache is written.
"""
import contextlib
import hashlib
import json
import os
import shutil
import tempfile
import time
from pathlib import Path
from unittest import mock

import agents
import ghcache

_ROOT = Path(tempfile.mkdtemp(prefix="almanac-parity-agents-"))
_BIN = _ROOT / "bin"
_BIN.mkdir()
_n = 0
NOW = time.time()


def _script(name, body, where=_BIN):
    p = where / name
    p.write_text("#!/bin/sh\n" + body)
    p.chmod(0o755)
    return str(p)


V_FLAG = _script("v-flag", 'echo "tool version 1.2.3-beta.1 (build)"\n')
V_SUB = _script("v-sub", '[ "$1" = version ] && echo "v 4.5" || { echo usage; exit 1; }\n')
V_SHORT = _script("v-short", '[ "$1" = -v ] && echo "9.8.7"\nexit 0\n')
V_NONE = _script("v-none", 'echo "no version here"\n')
V_ERR = _script("v-err", 'echo "2.0.0+meta.7" >&2\n')
V_UNI = _script("v-uni", 'echo "版本 ٣.٤.٥ — ok"\n')
V_SINGLE = _script("v-single", 'echo "build 42"\n')
MISSING = str(_ROOT / "does-not-exist")

# A fake mise: prints $FAKE_MISE_DIR/<subcommand>.json, so each case picks
# what `mise ls --json` / `mise outdated --json` say.
_FAKE = _ROOT / "fakebin"
_FAKE.mkdir()
_script("mise", 'f="$FAKE_MISE_DIR/$1.json"\n[ -f "$f" ] && cat "$f"\nexit 0\n', _FAKE)
_script("claude", 'echo "2.0.5 (Claude Code)"\n', _FAKE)
_script("codex", 'echo "codex-cli 0.9.0"\n', _FAKE)
_script("opencode", 'echo "nothing"\n', _FAKE)
_script("gh", 'echo gh\n', _FAKE)
_EMPTY = _ROOT / "emptybin"
_EMPTY.mkdir()


def _mise_dir(ls=None, outdated=None):
    global _n
    _n += 1
    d = _ROOT / f"mise{_n}"
    d.mkdir()
    if ls is not None:
        (d / "ls.json").write_text(ls if isinstance(ls, str) else json.dumps(ls))
    if outdated is not None:
        (d / "outdated.json").write_text(outdated if isinstance(outdated, str) else json.dumps(outdated))
    return str(d)


def _pair():
    global _n
    _n += 1
    py, rs = _ROOT / f"py{_n}", _ROOT / f"rs{_n}"
    py.mkdir()
    rs.mkdir()
    return py, rs


def _read_back(p):
    try:
        v = json.loads(Path(p).read_text())
    except (OSError, ValueError):
        return None
    if isinstance(v, dict) and isinstance(v.get("at"), (int, float)) and not isinstance(v.get("at"), bool):
        v["at"] = "<number>"
    return v


@contextlib.contextmanager
def _patched(base, env):
    cache = base / "almanac"
    patches = [
        mock.patch.object(agents, "CACHE", cache),
        mock.patch.object(agents, "LATEST_CACHE", cache / "agent-latest.json"),
        mock.patch.object(agents, "VERSION_CACHE", cache / "agent-versions.json"),
        mock.patch.object(agents, "MISE_CACHE", cache / "mise-outdated.json"),
        mock.patch.object(ghcache, "CACHE", cache),
        mock.patch.object(ghcache, "DIR", cache / "github"),
        mock.patch.object(ghcache, "STATE", cache / "github-rate.json"),
        mock.patch.dict(os.environ, {k: v for k, v in env.items() if v is not None}),
    ]
    with contextlib.ExitStack() as st:
        for p in patches:
            st.enter_context(p)
        for k, v in env.items():
            if v is None:
                os.environ.pop(k, None)
        yield cache


def case(func, kw, fn, files=None, env=None, readback=None):
    """files: {path relative to the almanac cache dir: text | None}."""
    py, rs = _pair()
    env = dict(env or {})
    files = files or {}
    rs_files = {str(rs / "almanac" / k): v for k, v in files.items()}

    def run():
        with _patched(py, env) as cache:
            for k, v in files.items():
                p = cache / k
                p.parent.mkdir(parents=True, exist_ok=True)
                if v is not None:
                    p.write_text(v)
            out = fn()
            if readback:
                return [out, _read_back(cache / readback)]
            return out

    args = {**kw, "env": {**env, "XDG_CACHE_HOME": str(rs)}, "files": rs_files}
    return func, args, run


def _gh_file(repo, body):
    url = f"{ghcache.API}/repos/{repo}/releases/latest"
    return f"github/{hashlib.sha1(url.encode()).hexdigest()}.json", json.dumps({"at": NOW, "body": body})


def _real_mise_cache():
    """The real mise-outdated cache, re-stamped as fresh (read only)."""
    try:
        blob = json.loads((Path.home() / ".cache/almanac/mise-outdated.json").read_text())
        data = blob.get("data") or {}
    except (OSError, ValueError):
        data = {}
    return json.dumps({"at": NOW, "data": data})


def cases():
    # -- tables and paths -------------------------------------------------
    yield "KNOWN", {}, lambda: list(agents.KNOWN)
    yield "BY_ID", {}, lambda: agents.BY_ID
    yield "TOOL_DOMAINS", {}, lambda: agents.TOOL_DOMAINS
    rs_base = _ROOT / "paths-rs"
    yield ("paths", {"env": {"XDG_CACHE_HOME": str(rs_base)}},
           lambda: {"CACHE": rs_base / "almanac",
                    "LATEST_CACHE": rs_base / "almanac/agent-latest.json",
                    "VERSION_CACHE": rs_base / "almanac/agent-versions.json",
                    "MISE_CACHE": rs_base / "almanac/mise-outdated.json",
                    "TTL": agents.TTL, "UA": agents.UA, "AGENT": agents.AGENT})
    for text in ["claude 2.1.3", "v0.10.2-rc.1+x", "1.2", "7", "a1.b2", "", "2025.01.15-abc",
                 "١٢.٣", "x 1.2.3-", "1.2.3+", "ver 3.4.5_beta", "10.0.0-alpha.β"]:
        yield ("version_search", {"text": text},
               lambda t=text: (m.group(0) if (m := agents._VERSION.search(t)) else None))

    # -- _newer -----------------------------------------------------------
    pairs = [("0.10.2", "0.9.9"), ("0.9.9", "0.10.2"), ("1.0", "1.0"), ("1.0.1", "1.0"),
             ("1.0", "1.0.0"), ("1.0.0", "1.0"), ("2", "1.9"), ("", "1.0"), ("1.0", ""),
             ("abc", "1.0"), ("2025.01.15", "2025.01.02"), ("1.2.3.4.9", "1.2.3.4.1"),
             ("1.2.3.5", "1.2.3.4.9"), ("v2.0.0", "1.99.99"), ("007", "7"), ("08.1", "8.0"),
             ("99999999999999999999999.1", "99999999999999999999998.9"),
             ("٣.٠", "2.9"), ("١٠", "9"), ("𝟗.𝟏", "9.0"), ("1.0-rc2", "1.0-rc1"),
             ("2.0.30", "1.19.3")]
    for a, b in pairs:
        yield "newer", {"latest": a, "current": b}, lambda a=a, b=b: agents._newer(a, b)

    # -- major_jump / hold_major (inventory.versions_behind) --------------
    for cur, lat in [("1.19.3", "2.0.30"), ("2.1.265", "2.1.301"), ("1.0", ""), ("", ""),
                     ("1.0", "1.0"), ("0.9", "1.0"), ("abc", "2.0")]:
        yield "major_jump", {"current": cur, "latest": lat}, lambda c=cur, l=lat: agents.major_jump(c, l)
    for setting in [None, True, False]:
        st = _ROOT / f"state{setting}"
        f = st / "almanac/settings.json"
        f.parent.mkdir(parents=True, exist_ok=True)
        if setting is not None:
            f.write_text(json.dumps({"hold_majors": setting}))
        for cur, lat in [("1.0", "2.0"), ("1.0", "1.1"), ("1.0", "")]:
            yield case("hold_major", {"current": cur, "latest": lat},
                       lambda c=cur, l=lat: agents.hold_major(c, l),
                       env={"XDG_STATE_HOME": str(st)})

    # -- _binary_id / _version_of ------------------------------------------
    link = _ROOT / "link-to-v-flag"
    link.symlink_to(V_FLAG)
    broken = _ROOT / "broken-link"
    broken.symlink_to(_ROOT / "nowhere")
    for p in [V_FLAG, str(link), str(broken), MISSING, "", str(_BIN)]:
        yield "binary_id", {"path": p}, lambda p=p: agents._binary_id(p)
    for b in [V_FLAG, V_SUB, V_SHORT, V_NONE, V_ERR, V_UNI, V_SINGLE, MISSING, str(_BIN)]:
        yield "version_of", {"binary": b}, lambda b=b: agents._version_of(b)

    # -- _cached_version ----------------------------------------------------
    key = agents._binary_id(V_FLAG)
    vc = "agent-versions.json"
    yield case("cached_version", {"binary": V_FLAG, "path": V_FLAG},
               lambda: agents._cached_version(V_FLAG, V_FLAG), readback=vc)
    yield case("cached_version", {"binary": V_FLAG, "path": V_FLAG},
               lambda: agents._cached_version(V_FLAG, V_FLAG), readback=vc,
               files={vc: json.dumps({V_FLAG: [key, "7.7.7"], "other": ["1:2", "ü"]})})
    yield case("cached_version", {"binary": V_FLAG, "path": V_FLAG},
               lambda: agents._cached_version(V_FLAG, V_FLAG), readback=vc,
               files={vc: json.dumps({"zzz": ["1:2", "3"], V_FLAG: ["0:0", "7.7.7"]})})
    yield case("cached_version", {"binary": V_FLAG, "path": V_FLAG},
               lambda: agents._cached_version(V_FLAG, V_FLAG), readback=vc,
               files={vc: json.dumps({V_FLAG: [key, "7.7.7", "x"]})})
    yield case("cached_version", {"binary": V_FLAG, "path": V_FLAG},
               lambda: agents._cached_version(V_FLAG, V_FLAG), readback=vc,
               files={vc: "{not json"})
    yield case("cached_version", {"binary": V_NONE, "path": V_NONE},
               lambda: agents._cached_version(V_NONE, V_NONE), readback=vc)
    yield case("cached_version", {"binary": MISSING, "path": MISSING},
               lambda: agents._cached_version(MISSING, MISSING), readback=vc)
    yield case("cached_version", {"binary": "claude-ü", "path": V_UNI},
               lambda: agents._cached_version("claude-ü", V_UNI), readback=vc)

    # -- _mise_installed ----------------------------------------------------
    yield "mise_installed", {}, lambda: agents._mise_installed()
    yield case("mise_installed", {}, lambda: agents._mise_installed(),
               env={"PATH": str(_EMPTY)})
    fake_path = f"{_FAKE}:/usr/bin"
    ls_variants = [
        {"uv": [{"version": "0.1", "active": False}, {"version": "0.2", "active": True},
                {"version": "0.3", "active": True}],
         "gh": {"version": "2.0", "active": True},
         "node": "junk", "go": [], "deno": [{"active": True}], "bun": [{"version": "1", "active": 1}],
         "rust": [{"version": "1.80", "active": 0}], "ünï": [{"version": "ß", "active": "yes"}]},
        {},
        "",
        "not json",
        # "[1, 2]" (a list where mise prints an object) is left out: Python
        # raises AttributeError, the Rust port answers {} (infallible).
    ]
    for ls in ls_variants:
        d = _mise_dir(ls=ls)
        yield case("mise_installed", {}, lambda: agents._mise_installed(),
                   env={"PATH": fake_path, "FAKE_MISE_DIR": d})

    # -- _mise_outdated -----------------------------------------------------
    mc = "mise-outdated.json"
    od = {"uv": {"current": "0.1", "latest": "0.3", "release_url": "https://u"}}
    d = _mise_dir(outdated=od)
    fenv = {"PATH": fake_path, "FAKE_MISE_DIR": d}
    yield case("mise_outdated", {"ttl": agents.TTL}, lambda: agents._mise_outdated(),
               env=fenv, readback=mc)
    yield case("mise_outdated", {"ttl": agents.TTL}, lambda: agents._mise_outdated(),
               env=fenv, readback=mc,
               files={mc: json.dumps({"at": NOW, "data": {"gh": {"latest": "9"}}})})
    yield case("mise_outdated", {"ttl": agents.TTL}, lambda: agents._mise_outdated(),
               env=fenv, readback=mc,
               files={mc: json.dumps({"at": NOW - agents.TTL - 5, "data": {"gh": {"latest": "9"}}})})
    yield case("mise_outdated", {"ttl": 0}, lambda: agents._mise_outdated(0),
               env=fenv, readback=mc,
               files={mc: json.dumps({"at": NOW, "data": {"gh": {"latest": "9"}}})})
    yield case("mise_outdated", {"ttl": agents.TTL}, lambda: agents._mise_outdated(),
               env=fenv, readback=mc, files={mc: json.dumps({"at": NOW, "data": None})})
    yield case("mise_outdated", {"ttl": agents.TTL}, lambda: agents._mise_outdated(),
               env=fenv, readback=mc, files={mc: json.dumps({"data": {"gh": {}}})})
    yield case("mise_outdated", {"ttl": agents.TTL}, lambda: agents._mise_outdated(),
               env=fenv, readback=mc, files={mc: "garbage"})
    for out in ["", "nope", "{}"]:
        d2 = _mise_dir(outdated=out)
        yield case("mise_outdated", {"ttl": agents.TTL}, lambda: agents._mise_outdated(),
                   env={"PATH": fake_path, "FAKE_MISE_DIR": d2}, readback=mc)
    yield case("mise_outdated", {"ttl": agents.TTL}, lambda: agents._mise_outdated(),
               env={"PATH": str(_EMPTY)}, readback=mc)

    # -- _load_latest / _save_latest -----------------------------------------
    lc = "agent-latest.json"
    vers = {"a/b": ["1.0", "https://x"], "ü/ñ": ["2.0", ""]}
    for content in [None, json.dumps({"at": NOW, "versions": vers}),
                    json.dumps({"at": NOW - agents.TTL - 1, "versions": vers}),
                    json.dumps({"versions": vers}), json.dumps({"at": NOW}), "{bad"]:
        yield case("load_latest", {}, lambda: agents._load_latest(), files={lc: content})
    for v in [vers, {}]:
        yield case("save_latest", {"versions": v}, lambda v=v: (agents._save_latest(v), None)[1],
                   readback=lc)

    # -- _github_latest / release_notes (through a seeded ghcache) ------------
    body = "## What's new\n\n" + "\n".join(f"- change {i} ünïcödé" for i in range(300)) + "\n\n"
    rels = {
        "o/tagged": {"tag_name": "v1.2.3", "html_url": "https://gh/o/tagged", "body": body},
        "o/plain": {"tag_name": "vvnightly", "html_url": "https://gh/o/plain", "body": "  short \x1c\n"},
        "o/empty": {"tag_name": None, "body": None},
        "o/nolines": {"tag_name": "rust-v0.9.0", "html_url": "u", "body": "x" * 3000},
        "o/list": [1, 2],
    }
    gh_files = dict(_gh_file(r, b) for r, b in rels.items())
    for repo in rels:
        yield case("github_latest", {"repo": repo}, lambda r=repo: agents._github_latest(r),
                   files=gh_files)
    for repo, limit in [("o/tagged", 2000), ("o/tagged", 50), ("o/tagged", 0), ("o/plain", 2000),
                        ("o/empty", 2000), ("o/nolines", 2000), ("o/list", 2000), ("", 2000)]:
        yield case("release_notes", {"repo": repo, "limit": limit},
                   lambda r=repo, l=limit: agents.release_notes(r, l), files=gh_files)

    # -- mise_tools ------------------------------------------------------------
    have_out = [
        ({"uv": "0.13.1"}, {"uv": {"current": "0.12.13", "latest": "0.13.1"}}),
        ({"claude": "2.0", "gh": "2.1", "weird-ü": "1"}, {}),
        ({"gh": "2.1"}, {"gh": None, "node": {"latest": "99"}}),
        ({"node": "20.1.0"}, {"node": {"latest": "22.0.0", "release_url": "https://n"}}),
        ({}, {}),
    ]
    for have, outdated in have_out:
        def fn(h=have, o=outdated):
            with mock.patch.object(agents, "_mise_installed", return_value=h), \
                 mock.patch.object(agents, "_mise_outdated", return_value=o):
                return agents.mise_tools()
        yield "mise_tools_from", {"have": have, "outdated": outdated}, fn
    real_mc = _real_mise_cache()
    yield case("mise_tools", {"check_updates": False}, lambda: agents.mise_tools(False))
    yield case("mise_tools", {"check_updates": True}, lambda: agents.mise_tools(True),
               files={mc: real_mc})

    # -- detect -----------------------------------------------------------------
    # A fake machine: claude self-managed (on PATH, not in mise), codex and
    # opencode under mise.
    ls = {"codex": [{"version": "0.9.1", "active": True}],
          "opencode": [{"version": "1.0.0", "active": True}], "gh": [{"version": "2", "active": True}]}
    d = _mise_dir(ls=ls, outdated={})
    fenv = {"PATH": fake_path, "FAKE_MISE_DIR": d}
    no_update = json.dumps({"at": NOW, "versions": {"anthropics/claude-code": ["2.0.5", "https://rel"]}})
    update = json.dumps({"at": NOW, "versions": {"anthropics/claude-code": ["2.0.9", "https://rel"]}})
    mise_od = json.dumps({"at": NOW, "data": {"codex": {"current": "0.8", "latest": "0.9.1",
                                                        "release_url": "https://codex"},
                                              "opencode": {}}})
    mise_up = json.dumps({"at": NOW, "data": {"opencode": {"latest": "2.0.0"}}})
    yield case("detect", {"check_updates": False}, lambda: agents.detect(False),
               env=fenv, readback=lc)
    yield case("detect", {"check_updates": True}, lambda: agents.detect(True),
               env=fenv, readback=lc, files={lc: no_update, mc: mise_od})
    yield case("detect", {"check_updates": True}, lambda: agents.detect(True),
               env=fenv, readback=lc, files={lc: update, mc: mise_od})
    yield case("detect", {"check_updates": True}, lambda: agents.detect(True),
               env=fenv, readback=lc, files={lc: no_update, mc: mise_up})
    yield case("detect", {"check_updates": False}, lambda: agents.detect(False),
               env={"PATH": str(_EMPTY)}, readback=lc)
    # The real machine, caches in temp dirs.
    yield case("detect", {"check_updates": False}, lambda: agents.detect(False), readback=lc)
    yield case("detect", {"check_updates": True}, lambda: agents.detect(True),
               readback=lc, files={mc: real_mc})


# ---------------------------------------------------------------------------
# Review round 1: adversarial cases. Each targets a suspected divergence.
# ---------------------------------------------------------------------------
import subprocess
import sys

# Prints "1.<crc>" of $RAWFILE with any `"at": <number>` masked, so the exact
# bytes each side wrote can be compared through version_of.
RAWSUM = _script("rawsum", 'f="$RAWFILE"; [ -f "$f" ] || { echo "0.0"; exit 0; }\n'
                 'echo "1.$(sed -E \'s/"at": [-0-9.eE+]+/"at": X/\' "$f" | cksum | cut -d" " -f1)"\n')
V_BADUTF = _script("v-badutf", "printf '\\377 1.2.3\\n'\n")
V_INT = _script("v-int", 'echo "1.0.0"\n')

_FAKE2 = _ROOT / "fakebin2"
_FAKE2.mkdir()
_script("mise", 'f="$FAKE_MISE_DIR/$1.json"\n[ -f "$f" ] && cat "$f"\nexit 0\n', _FAKE2)
_script("claude", 'echo "2.0.5 (Claude Code)"\n', _FAKE2)
# Executable for group/other but not for its owner: os.access(X_OK) says no.
_FAKE3 = _ROOT / "fakebin3"
_FAKE3.mkdir()
_noexec = _FAKE3 / "gemini"
_noexec.write_text("#!/bin/sh\necho 0.1.0\n")
_noexec.chmod(0o611)


def _hookable(v):
    """v as the Rust parity hook can report it: serde_json holds no NaN or
    infinity (null) and no lone surrogate (U+FFFD). Used only where the bytes
    written are compared exactly as well (rawsum)."""
    if isinstance(v, float) and (v != v or v in (float("inf"), float("-inf"))):
        return None
    if isinstance(v, str):
        return "".join("\ufffd" if 0xD800 <= ord(c) <= 0xDFFF else c for c in v)
    if isinstance(v, dict):
        return {_hookable(k): _hookable(x) for k, x in v.items()}
    if isinstance(v, (list, tuple)):
        return [_hookable(x) for x in v]
    return v


def case_raw(func, kw, fn, rel, files=None, env=None, hookable=False, call_kw=None):
    """The operation, then the raw bytes of the cache file it wrote.

    call_kw: extra args for the Rust side only (e.g. `pick`), with fn
    returning the matching part; the cache file is then not read back."""
    py, rs = _pair()
    env = dict(env or {})
    files = files or {}
    rs_files = {str(rs / "almanac" / k): v for k, v in files.items()}

    def run():
        with _patched(py, env) as cache:
            for k, v in files.items():
                p = cache / k
                p.parent.mkdir(parents=True, exist_ok=True)
                if v is not None:
                    p.write_text(v)
            if call_kw:
                return fn()
            out = [fn(), _read_back(cache / rel)]
            return _hookable(out) if hookable else out

    yield func, {**kw, **(call_kw or {}), "env": {**env, "XDG_CACHE_HOME": str(rs)}, "files": rs_files}, run
    yield ("version_of", {"binary": RAWSUM, "env": {"RAWFILE": str(rs / "almanac" / rel)}},
           lambda: _with_env({"RAWFILE": str(py / "almanac" / rel)}, lambda: agents._version_of(RAWSUM)))


def _with_env(env, fn):
    with mock.patch.dict(os.environ, env):
        return fn()


def _py_paths(env):
    code = ("import sys, json; sys.path.insert(0, %r); import agents; "
            "print(json.dumps({'CACHE': str(agents.CACHE), 'LATEST_CACHE': str(agents.LATEST_CACHE),"
            "'VERSION_CACHE': str(agents.VERSION_CACHE), 'MISE_CACHE': str(agents.MISE_CACHE),"
            "'TTL': agents.TTL, 'UA': agents.UA, 'AGENT': agents.AGENT}))") % str(Path(agents.__file__).parent)
    e = {**os.environ, **env}
    return json.loads(subprocess.run([sys.executable, "-c", code], env=e, capture_output=True,
                                     text=True, cwd=str(_ROOT)).stdout)


def review_cases():
    lc, mc, vc = "agent-latest.json", "mise-outdated.json", "agent-versions.json"
    # XDG_CACHE_HOME set but empty: Python's os.environ.get keeps "", so CACHE
    # is the relative path "almanac"; Rust falls back to ~/.cache.
    yield "paths", {"env": {"XDG_CACHE_HOME": ""}}, lambda: _py_paths({"XDG_CACHE_HOME": ""})

    # version_of: undecodable output raises UnicodeDecodeError in Python (text=True).
    yield "version_of", {"binary": V_BADUTF}, lambda: agents._version_of(V_BADUTF)
    yield "version_of", {"binary": ""}, lambda: agents._version_of("")

    # Unicode 16 digits (Python 3.14 knows them).
    for text in ["\U00016131.\U00016132", "\U0001CCF1.\U0001CCF2", "v\U00010D41.\U00010D40.1"]:
        yield ("version_search", {"text": text},
               lambda t=text: (m.group(0) if (m := agents._VERSION.search(t)) else None))
        yield "newer", {"latest": text, "current": "0.0"}, lambda t=text: agents._newer(t, "0.0")
    # int() refuses more than 4300 digits.
    huge = "1" * 5000 + ".1"
    yield "newer", {"latest": huge, "current": "1.0"}, lambda: agents._newer(huge, "1.0")

    # cached_version: a cached version that is not a string comes back as is.
    key = agents._binary_id(V_INT)
    for hit in [5, None, ["x"], 1.5]:
        yield case("cached_version", {"binary": V_INT, "path": V_INT},
                   lambda: agents._cached_version(V_INT, V_INT), readback=vc,
                   files={vc: json.dumps({V_INT: [key, hit]})})
    # A store that is a list: Python raises on .get.
    yield case("cached_version", {"binary": V_INT, "path": V_INT},
               lambda: agents._cached_version(V_INT, V_INT), readback=vc,
               files={vc: "[1, 2]"})
    # Bytes written: other entries kept verbatim (big ints, NaN, floats), unicode escaped.
    store = '{"zzz": ["1:2", 123456789012345678901234], "f": [1e5, 2.50], "ü": "é"}'
    yield from case_raw("cached_version", {"binary": "claude-ü", "path": V_INT},
                        lambda: agents._cached_version("claude-ü", V_INT), vc, files={vc: store})
    yield from case_raw("cached_version", {"binary": V_INT, "path": V_INT},
                        lambda: agents._cached_version(V_INT, V_INT), vc,
                        files={vc: '{"n": NaN, "i": Infinity}'}, hookable=True)

    # mise_installed: non-string versions, undecodable output, PATH unset.
    fake_path = f"{_FAKE}:/usr/bin"
    for ls in [{"uv": [{"version": None, "active": True}], "gh": [{"version": 2.5, "active": True}],
                "go": [{"version": ["1"], "active": True}], "bun": {"version": 7, "active": True}}]:
        d = _mise_dir(ls=ls)
        yield case("mise_installed", {}, lambda: agents._mise_installed(),
                   env={"PATH": fake_path, "FAKE_MISE_DIR": d})
    dbad = _mise_dir()
    (Path(dbad) / "ls.json").write_bytes(b'{"uv": [{"version": "1.\xff", "active": true}]}')
    yield case("mise_installed", {}, lambda: agents._mise_installed(),
               env={"PATH": fake_path, "FAKE_MISE_DIR": dbad})
    yield case("mise_installed", {}, lambda: agents._mise_installed(), env={"PATH": None})

    # mise_outdated: odd cache blobs and odd mise output.
    d = _mise_dir(outdated={"uv": {"latest": "0.3"}})
    fenv = {"PATH": fake_path, "FAKE_MISE_DIR": d}
    for blob in ["[1, 2]", json.dumps({"at": "x", "data": {}}), json.dumps({"at": None, "data": {}}),
                 json.dumps({"at": NOW, "data": [1, 2]}), json.dumps({"at": NOW, "data": "str"})]:
        yield case("mise_outdated", {"ttl": agents.TTL}, lambda: agents._mise_outdated(),
                   env=fenv, readback=mc, files={mc: blob})
    for out in ["[1, 2]", '"text"']:
        d2 = _mise_dir(outdated=out)
        yield case("mise_outdated", {"ttl": agents.TTL}, lambda: agents._mise_outdated(),
                   env={"PATH": fake_path, "FAKE_MISE_DIR": d2}, readback=mc)
    # NaN and inf: the hook reports them as null, so the Python side does too;
    # the bytes written are compared exactly through rawsum.
    for out in ['{"uv": {"latest": 1E400}}', '{"uv": {"latest": NaN}}']:
        d2 = _mise_dir(outdated=out)
        yield from case_raw("mise_outdated", {"ttl": agents.TTL}, lambda: agents._mise_outdated(), mc,
                            env={"PATH": fake_path, "FAKE_MISE_DIR": d2}, hookable=True)
    d3 = _mise_dir(outdated='{"ü": {"latest": "1.0", "n": 1.0, "e": 1e-7, "big": 123456789012345678901234, "neg": -0.0}}')
    yield from case_raw("mise_outdated", {"ttl": agents.TTL}, lambda: agents._mise_outdated(), mc,
                        env={"PATH": fake_path, "FAKE_MISE_DIR": d3})
    dbad2 = _mise_dir()
    (Path(dbad2) / "outdated.json").write_bytes(b'{"uv": {"latest": "1.\xff"}}')
    yield case("mise_outdated", {"ttl": agents.TTL}, lambda: agents._mise_outdated(),
               env={"PATH": fake_path, "FAKE_MISE_DIR": dbad2}, readback=mc)

    # load_latest: `versions` present but null / not a dict; blob not a dict.
    for content in [json.dumps({"at": NOW, "versions": None}), json.dumps({"at": NOW, "versions": [1]}),
                    "[1]", json.dumps({"at": "soon", "versions": {}})]:
        yield case("load_latest", {}, lambda: agents._load_latest(), files={lc: content})
    # save_latest: exact bytes.
    yield from case_raw("save_latest", {"versions": {"ü/ñ": ["2.0", "é"], "a/b": ["1", None]}},
                        lambda: agents._save_latest({"ü/ñ": ["2.0", "é"], "a/b": ["1", None]}), lc)

    # github_latest: html_url null / non-string; tag_name not a string.
    rels = {"o/nullurl": {"tag_name": "v3.0.0", "html_url": None},
            "o/numurl": {"tag_name": "v3.0.0", "html_url": 7},
            "o/numtag": {"tag_name": 300, "html_url": "u"},
            "o/numbody": {"tag_name": "v1.0", "body": 5}}
    gh_files = dict(_gh_file(r, b) for r, b in rels.items())
    for repo in ["o/nullurl", "o/numurl", "o/numtag"]:
        yield case("github_latest", {"repo": repo}, lambda r=repo: agents._github_latest(r), files=gh_files)
    yield case("release_notes", {"repo": "o/numbody", "limit": 2000},
               lambda: agents.release_notes("o/numbody", 2000), files=gh_files)

    # detect: cached latest entries that Python unpacks differently.
    fenv2 = {"PATH": f"{_FAKE2}:/usr/bin", "FAKE_MISE_DIR": _mise_dir(ls={}, outdated={})}
    for entry in [["2.0.9", "https://rel", "extra"], "ab", ["2.0.9"], [None, None], [3, 4]]:
        blob = json.dumps({"at": NOW, "versions": {"anthropics/claude-code": entry}})
        yield case("detect", {"check_updates": True}, lambda: agents.detect(True),
                   env=fenv2, readback=lc, files={lc: blob, mc: json.dumps({"at": NOW, "data": {}})})
    # detect: fetched through a seeded ghcache whose html_url is null -> written as null.
    gh = dict([_gh_file("anthropics/claude-code", {"tag_name": "v2.1.0", "html_url": None})])
    yield case("detect", {"check_updates": True}, lambda: agents.detect(True),
               env=fenv2, readback=lc, files={**gh, mc: json.dumps({"at": NOW, "data": {}})})
    # detect with a mise-managed agent whose outdated entry is not a dict.
    d4 = _mise_dir(ls={"claude": [{"version": "2.0.5", "active": True}]}, outdated={})
    for info in ["string", [1], {"latest": 3}, {"current": 2, "latest": "3.0"}]:
        yield case("detect", {"check_updates": True}, lambda: agents.detect(True),
                   env={"PATH": f"{_FAKE2}:/usr/bin", "FAKE_MISE_DIR": d4}, readback=lc,
                   files={mc: json.dumps({"at": NOW, "data": {"claude": info}}),
                          lc: json.dumps({"at": NOW, "versions": {}})})
    # detect: a binary only group/other may execute. shutil.which uses
    # os.access (owner bits for the owner); util::which any x bit.
    yield case("detect", {"check_updates": False}, lambda: agents.detect(False),
               env={"PATH": f"{_FAKE3}:{_FAKE2}", "FAKE_MISE_DIR": _mise_dir(ls={}, outdated={})}, readback=lc)
    # detect: key order of the rewritten agent-latest.json (Python keeps the
    # file's order and appends; Rust's BTreeMap sorts).
    gh2 = dict([_gh_file("anthropics/claude-code", {"tag_name": "v2.1.0", "html_url": "https://r"})])
    yield from case_raw("detect", {"check_updates": True}, lambda: agents.detect(True), lc,
                        env=fenv2, files={**gh2, mc: json.dumps({"at": NOW, "data": {}}),
                                          lc: json.dumps({"at": NOW, "versions": {"zz/top": ["1", "u"]}})})
    # binary_id: realpath normalises before stat; the kernel does not.
    for p in [V_FLAG + "/", V_FLAG + "/.", str(_ROOT / "nope" / ".." / "bin" / "v-flag")]:
        yield "binary_id", {"path": p}, lambda p=p: agents._binary_id(p)
    # detect with PATH unset: shutil.which falls back to CS_PATH.
    yield case("detect", {"check_updates": False}, lambda: agents.detect(False),
               env={"PATH": None}, readback=lc)

    # mise_tools through a fake mise with non-string versions / odd outdated entries.
    d5 = _mise_dir(ls={"uv": [{"version": None, "active": True}], "gh": [{"version": 2, "active": True}]},
                   outdated={"uv": {"latest": "1.0"}})
    yield case("mise_tools", {"check_updates": True}, lambda: agents.mise_tools(True),
               env={"PATH": fake_path, "FAKE_MISE_DIR": d5})
    for o in [{"gh": "str"}, {"gh": {"latest": 9}}, {"gh": {"latest": "3", "release_url": 5}}]:
        def fn(o=o):
            with mock.patch.object(agents, "_mise_installed", return_value={"gh": "2.1"}), \
                 mock.patch.object(agents, "_mise_outdated", return_value=o):
                return agents.mise_tools()
        yield "mise_tools_from", {"have": {"gh": "2.1"}, "outdated": o}, fn


_orig_cases = cases


def cases():  # noqa: F811
    yield from _orig_cases()
    yield from review_cases()


# ---------------------------------------------------------------------------
# Review round 2: adversarial cases. Each targets a suspected divergence.
# ---------------------------------------------------------------------------
# A mise whose stdout is fine but whose stderr is not UTF-8: Python decodes
# both (text=True) and its UnicodeDecodeError is a ValueError -> {}.
_FAKE4 = _ROOT / "fakebin4"
_FAKE4.mkdir()
_script("mise", 'f="$FAKE_MISE_DIR/$1.json"\n[ -f "$f" ] && cat "$f"\nprintf \'\\377\\n\' >&2\nexit 0\n', _FAKE4)
# Exits at once but leaves a grandchild holding stdout open past the 20s
# timeout: Python's communicate() waits for EOF and times out.
V_BG = _script("v-bg", 'echo 1.2.3\n(sleep 23) &\nexit 0\n')


def _py_paths2(set_env, unset):
    code = ("import sys, json; sys.path.insert(0, %r); import agents; "
            "print(json.dumps({'CACHE': str(agents.CACHE), 'LATEST_CACHE': str(agents.LATEST_CACHE),"
            "'VERSION_CACHE': str(agents.VERSION_CACHE), 'MISE_CACHE': str(agents.MISE_CACHE),"
            "'TTL': agents.TTL, 'UA': agents.UA, 'AGENT': agents.AGENT}))") % str(Path(agents.__file__).parent)
    e = {k: v for k, v in os.environ.items() if k not in unset}
    e.update(set_env)
    return json.loads(subprocess.run([sys.executable, "-c", code], env=e, capture_output=True,
                                     text=True, cwd=str(_ROOT)).stdout)


def _nest(n):
    return "[" * n + "]" * n


def review2_cases():
    lc, mc, vc = "agent-latest.json", "mise-outdated.json", "agent-versions.json"

    # CACHE: HOME unset -> Python's Path.home() asks pwd; HOME="" -> "/".
    yield ("paths", {"env": {"HOME": None, "XDG_CACHE_HOME": None}},
           lambda: _py_paths2({}, {"HOME", "XDG_CACHE_HOME"}))
    yield ("paths", {"env": {"HOME": "", "XDG_CACHE_HOME": None}},
           lambda: _py_paths2({"HOME": ""}, {"XDG_CACHE_HOME"}))
    # pathlib normalises "//" and "/./" in the middle; PathBuf::join does not.
    yield ("paths", {"env": {"XDG_CACHE_HOME": "/a//b/./c/"}},
           lambda: _py_paths2({"XDG_CACHE_HOME": "/a//b/./c/"}, set()))

    # NUL in a path: Python raises ValueError (not OSError, not caught).
    yield "binary_id", {"path": "/usr/bin/ls\x00x"}, lambda: agents._binary_id("/usr/bin/ls\x00x")
    yield "version_of", {"binary": "ls\x00x"}, lambda: agents._version_of("ls\x00x")

    # Timeout while a grandchild keeps the pipe open.
    yield "version_of", {"binary": V_BG}, lambda: agents._version_of(V_BG)

    # JSON nesting: Python 3.14 parses 2000 levels; the port gives up at 900.
    # (A value 2000 deep is too deep for the runner's own recursive
    # comparison: `readback: false` / `pick` keep it out of the result.)
    key = agents._binary_id(V_INT)
    store = '{"deep": %s, %s: [%s, "7.7.7"]}' % (_nest(2000), json.dumps(V_INT), json.dumps(key))
    yield case("cached_version", {"binary": V_INT, "path": V_INT, "readback": False},
               lambda: agents._cached_version(V_INT, V_INT), files={vc: store})
    # A miss with a deep store: the deep entry is written back as it was.
    store = '{"deep": %s, %s: ["0:0", "7.7.7"]}' % (_nest(2000), json.dumps(V_INT))
    yield from case_raw("cached_version", {"binary": V_INT, "path": V_INT},
                        lambda: agents._cached_version(V_INT, V_INT), vc, files={vc: store},
                        call_kw={"readback": False})
    # Deep, but within Python's limit (about 52000 levels) / past it.
    for n in [30000, 60000]:
        blob = '{"at": %r, "versions": {"a/b": ["1", "u"]}, "pad": %s}' % (NOW, _nest(n))
        yield case("load_latest", {}, lambda: agents._load_latest(), files={lc: blob})
        st = _ROOT / f"state-deep{n}"
        f = st / "almanac/settings.json"
        f.parent.mkdir(parents=True, exist_ok=True)
        f.write_text('{"hold_majors": true, "pad": %s}' % _nest(n))
        yield case("hold_major", {"current": "1.0", "latest": "2.0"},
                   lambda: agents.hold_major("1.0", "2.0"), env={"XDG_STATE_HOME": str(st)})
        d = _mise_dir(outdated='{"uv": {"latest": "2.0", "pad": %s}}' % _nest(n))
        yield from case_raw("mise_outdated", {"ttl": agents.TTL},
                            lambda: agents._mise_outdated()["uv"]["latest"], mc,
                            env={"PATH": f"{_FAKE}:/usr/bin", "FAKE_MISE_DIR": d},
                            call_kw={"pick": ["uv", "latest"]})
    blob = '{"at": %r, "versions": {"a/b": ["1", "u"]}, "pad": %s}' % (NOW, _nest(2000))
    yield case("load_latest", {}, lambda: agents._load_latest(), files={lc: blob})
    # ...and at 100000 levels Python raises RecursionError (not a ValueError).
    blob = '{"at": %r, "versions": {}, "pad": %s}' % (NOW, _nest(100000))
    yield case("load_latest", {}, lambda: agents._load_latest(), files={lc: blob})
    d = _mise_dir(outdated='{"uv": {"latest": "2.0", "pad": %s}}' % _nest(2000))
    yield from case_raw("mise_outdated", {"ttl": agents.TTL},
                        lambda: agents._mise_outdated()["uv"]["latest"], mc,
                        env={"PATH": f"{_FAKE}:/usr/bin", "FAKE_MISE_DIR": d},
                        call_kw={"pick": ["uv", "latest"]})
    # hold_major: settings.json too deep for Python's json -> RecursionError.
    st = _ROOT / "state-deep"
    f = st / "almanac/settings.json"
    f.parent.mkdir(parents=True, exist_ok=True)
    f.write_text(_nest(100000))
    yield case("hold_major", {"current": "1.0", "latest": "2.0"},
               lambda: agents.hold_major("1.0", "2.0"), env={"XDG_STATE_HOME": str(st)})

    # Lone surrogate escapes: Python keeps and re-writes "\ud800"; the port
    # turns it into U+FFFD, so the cache file the Python wrote changes.
    # (The hook cannot report a lone surrogate, so the readback is compared
    # with U+FFFD in its place; rawsum compares the bytes written exactly.)
    yield from case_raw("cached_version", {"binary": V_INT, "path": V_INT},
                        lambda: agents._cached_version(V_INT, V_INT), vc,
                        files={vc: '{"other": ["1:2", "a\\ud800b"], "\\udc00": 1}'}, hookable=True)
    # A lone surrogate where it is read: a cached version and a mise tool.
    yield case("cached_version", {"binary": V_INT, "path": V_INT, "readback": False},
               lambda: _hookable(agents._cached_version(V_INT, V_INT)),
               files={vc: '{%s: [%s, "1.\\udc00"]}' % (json.dumps(V_INT), json.dumps(key))})

    # mise with a non-UTF-8 stderr.
    d = _mise_dir(ls={"uv": [{"version": "0.2", "active": True}]}, outdated={"uv": {"latest": "0.3"}})
    fenv = {"PATH": f"{_FAKE4}:/usr/bin", "FAKE_MISE_DIR": d}
    yield case("mise_installed", {}, lambda: agents._mise_installed(), env=fenv)
    yield case("mise_outdated", {"ttl": agents.TTL}, lambda: agents._mise_outdated(),
               env=fenv, readback=mc)

    # shutil.which splits a tool name with a directory part and re-joins it.
    tool = f"{_BIN}//v-flag"
    def fn():
        with mock.patch.object(agents, "_mise_installed", return_value={tool: "1.0"}), \
             mock.patch.object(agents, "_mise_outdated", return_value={}):
            return agents.mise_tools()
    yield "mise_tools_from", {"have": {tool: "1.0"}, "outdated": {}}, fn

    # realpath: symlink loop and relative chains.
    loop_a, loop_b = _ROOT / "loop-a", _ROOT / "loop-b"
    loop_a.symlink_to("loop-b")
    loop_b.symlink_to("loop-a")
    (_ROOT / "sub").mkdir()
    (_ROOT / "sub" / "up").symlink_to("../bin")
    for p in [str(loop_a), str(loop_a) + "/x", str(_ROOT / "sub/up/v-flag"),
              str(_ROOT / "sub/up/../bin/v-flag")]:
        yield "binary_id", {"path": p}, lambda p=p: agents._binary_id(p)


_orig_cases2 = cases


def cases():  # noqa: F811
    yield from _orig_cases2()
    yield from review2_cases()


# ---------------------------------------------------------------------------
# Review round 2 (second reviewer): more suspected divergences.
# ---------------------------------------------------------------------------
# An executable with no #! line: Python's execve gets ENOEXEC (an OSError ->
# ""); glibc's execvp/posix_spawnp may fall back to /bin/sh and run it.
V_NOSHEBANG = _BIN / "v-noshebang"
V_NOSHEBANG.write_text("echo 5.6.7\n")
V_NOSHEBANG.chmod(0o755)
V_NOSHEBANG = str(V_NOSHEBANG)
# A binary that lives under a */mise/installs/* path but that `mise ls` does
# not list: managed by mise through the path test alone.
_FAKE5 = _ROOT / "x" / "mise" / "installs" / "codex" / "bin"
_FAKE5.mkdir(parents=True)
_script("codex", 'echo "codex-cli 0.7.0"\n', _FAKE5)


def review2b_cases():
    lc, mc, vc = "agent-latest.json", "mise-outdated.json", "agent-versions.json"

    yield "version_of", {"binary": V_NOSHEBANG}, lambda: agents._version_of(V_NOSHEBANG)

    # release_notes with a negative limit: Python slices from the end.
    gh_files = dict([_gh_file("o/neg", {"tag_name": "v1", "body": "line one\nline two\nline three"})])
    for limit in [-5, -100]:
        yield case("release_notes", {"repo": "o/neg", "limit": limit},
                   lambda l=limit: agents.release_notes("o/neg", l), files=gh_files)

    # detect: a cached --version answer that is not a string lands in App.version as is.
    claude = str(_FAKE2 / "claude")
    key = agents._binary_id(claude)
    fenv2 = {"PATH": f"{_FAKE2}:/usr/bin", "FAKE_MISE_DIR": _mise_dir(ls={}, outdated={})}
    for hit in [5, None, 2.5]:
        yield case("detect", {"check_updates": False}, lambda: agents.detect(False),
                   env=fenv2, readback=lc, files={vc: json.dumps({"claude": [key, hit]})})

    # detect: managed by mise only through the */mise/installs/* path.
    d = _mise_dir(ls={}, outdated={})
    yield case("detect", {"check_updates": True}, lambda: agents.detect(True),
               env={"PATH": f"{_FAKE5}:{_FAKE}:/usr/bin", "FAKE_MISE_DIR": d}, readback=lc,
               files={mc: json.dumps({"at": NOW, "data": {"codex": {"current": "0.6", "latest": "0.8.0"}}}),
                      lc: json.dumps({"at": NOW, "versions": {}})})

    # mise_tools: a tool whose active version is null (Python keeps None).
    d6 = _mise_dir(ls={"uv": [{"version": None, "active": True}]}, outdated={})
    yield case("mise_tools", {"check_updates": False}, lambda: agents.mise_tools(False),
               env={"PATH": f"{_FAKE}:/usr/bin", "FAKE_MISE_DIR": d6})


_orig_cases3 = cases


def cases():  # noqa: F811
    yield from _orig_cases3()
    yield from review2b_cases()
