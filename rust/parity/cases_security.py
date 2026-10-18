"""Parity cases for security.py.

`env` in the args is applied on both sides for the duration of the call
(None removes the variable). XDG_CACHE_HOME and XDG_STATE_HOME always point
into a temp root, so the real ~/.cache/almanac/security.json and settings
are never written; `cache_file` / `settings_file` (text, or None to remove)
are written there just before the call on each side. The real machine is
read through `pacman -Q`, `vercmp` and the tracker itself.
"""
import atexit
import dataclasses
import json
import re
import os
import shutil
import tempfile
from pathlib import Path

import security
import settings

_ROOT = Path(tempfile.mkdtemp(prefix="almanac-parity-security-"))
atexit.register(shutil.rmtree, _ROOT, True)
_n = 0


def _dirs():
    global _n
    _n += 1
    d = _ROOT / str(_n)
    return {"XDG_CACHE_HOME": str(d / "cache"), "XDG_STATE_HOME": str(d / "state")}


def _put(p, v):
    if v is _KEEP:
        return
    if v is None:
        if p.exists():
            p.unlink()
        return
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(v)


_KEEP = object()


def _case(func, fn, env=None, cache_file=_KEEP, settings_file=_KEEP, **kw):
    env = {**_dirs(), **(env or {})}
    args = {"env": env, **kw}
    if cache_file is not _KEEP:
        args["cache_file"] = cache_file
    if settings_file is not _KEEP:
        args["settings_file"] = settings_file

    def run():
        old = {k: os.environ.get(k) for k in env}
        for k, v in env.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v
        # urllib builds its opener (and reads the proxy variables) once;
        # forget it so each case sees its own env.
        security.urllib.request.install_opener(None)
        try:
            _put(security._cache_path(), cache_file)
            _put(settings.path(), settings_file)
            return fn()
        finally:
            for k, v in old.items():
                if v is None:
                    os.environ.pop(k, None)
                else:
                    os.environ[k] = v
    return func, args, run


GROUPS = [
    {"name": "AVG-1", "packages": ["openssl"], "status": "Fixed",
     "severity": "High", "type": "arbitrary code execution",
     "affected": "3.0.0-1", "fixed": "3.0.2-1", "issues": ["CVE-1"]},
    {"name": "AVG-2", "packages": ["libxml2"], "status": "Vulnerable",
     "severity": "Medium", "type": "denial of service",
     "affected": "2.14.0-1", "fixed": None, "issues": ["CVE-2"]},
    {"name": "AVG-3", "packages": ["curl"], "status": "Not affected",
     "severity": "Low", "type": "", "affected": "1-1", "fixed": None,
     "issues": []},
    {"name": "AVG-4", "packages": ["openssl", "lib32-openssl", "zlib"], "status": "Vulnerable",
     "severity": "Critical", "type": "", "affected": "", "fixed": None,
     "issues": ["CVE-9", "CVE-3", "CVE-7", "CVE-5"]},
    {"name": "AVG-5", "packages": ["zlib"], "status": "Fixed",
     "severity": "Low", "type": "information disclosure",
     "affected": "1.2-1", "fixed": "1:1.0-1", "issues": ["CVE-10"]},
    {"name": "AVG-6", "packages": ["ünïcødé"], "status": "Vulnerable",
     "severity": "Weird", "type": "ëxploit", "affected": "1.0-1",
     "issues": ["CVE-ü"]},
    {"name": "AVG-7", "packages": ["openssl"], "status": "Unknown",
     "severity": "Medium", "type": "x", "affected": "3.0.0-1", "fixed": None},
    {"name": "AVG-8", "packages": None, "status": "Vulnerable"},
    {"name": "AVG-9", "packages": ["curl"], "status": "Vulnerable",
     "severity": "High", "affected": "not-a-version!", "fixed": "8.1-1",
     "issues": ["CVE-11", "CVE-11"]},
    {},
]

INSTALLED = [
    {},
    {"openssl": "3.0.1-1"},
    {"openssl": "3.0.2-1"},
    {"openssl": "1:2.0-1"},
    {"openssl": "4.0-1"},
    {"openssl": ""},
    {"libxml2": "2.15.0-1"},
    {"libxml2": "2.13.0-1"},
    {"libxml2": "2.14.0-1"},
    {"curl": "8-1"},
    {"curl": "8.1-1"},
    {"curl": "7.0-1"},
    {"zlib": "1.3-1", "lib32-openssl": "3-1", "ünïcødé": "0.9-1"},
    {"ünïcødé": "2.0-1", "zlib": "1:0.5-1"},
    {"openssl": "3.0.1-1", "libxml2": "2.15.0-1", "curl": "7-1",
     "zlib": "1.3-1", "lib32-openssl": "3-1", "ünïcødé": "1.0-1"},
    {"openssl": "garbage", "curl": "-1"},
]


def _vulns(installed, groups=GROUPS):
    return security.vulnerabilities(installed, groups) or []


def _ser(vs):
    return [dataclasses.asdict(v) for v in vs]


def _finding_with(vulns):
    """finding() with vulnerabilities() handing it `vulns` (the Python test's
    mock); its result, or the text of what it raised."""
    orig = security.vulnerabilities
    security.vulnerabilities = lambda *a, **k: vulns
    try:
        return _err(security.finding)
    finally:
        security.vulnerabilities = orig


DEAD = {"https_proxy": "http://127.0.0.1:9", "HTTPS_PROXY": "http://127.0.0.1:9",
        "no_proxy": None, "NO_PROXY": None}


def cases():
    dead = DEAD
    # Readers of the real machine.
    yield "installed_versions", {}, lambda: security.installed_versions()
    yield _case("enabled", lambda: security.enabled())
    yield _case("enabled", lambda: security.enabled(),
                settings_file='{"net_security_tracker": false}')
    yield _case("enabled", lambda: security.enabled(),
                settings_file='{"net_security_tracker": "yes"}')
    yield _case("enabled", lambda: security.enabled(), settings_file="{broken")

    # vulnerabilities over pinned groups.
    for inst in INSTALLED:
        yield "vulnerabilities", {"installed": inst, "groups": GROUPS}, \
            (lambda i=inst: security.vulnerabilities(i, GROUPS))
    yield "vulnerabilities", {"installed": {"openssl": "1-1"}, "groups": []}, \
        lambda: security.vulnerabilities({"openssl": "1-1"}, [])
    # The real pacman -Q against pinned groups (installed=None).
    real = [{"name": "AVG-R1", "packages": ["pacman", "glibc", "bash"], "status": "Fixed",
             "severity": "High", "type": "t", "affected": "0-1", "fixed": "99999-1",
             "issues": ["CVE-R"]},
            {"name": "AVG-R2", "packages": ["linux", "coreutils"], "status": "Vulnerable",
             "severity": "Low", "type": "", "affected": "1-1", "issues": []}]
    yield "vulnerabilities", {"installed": None, "groups": real}, \
        lambda: security.vulnerabilities(None, real)

    # vulnerabilities / advisories through the cache.
    fresh = json.dumps({"at": 1e12, "groups": GROUPS})
    yield _case("vulnerabilities", lambda: security.vulnerabilities({"openssl": "3.0.1-1"}),
                cache_file=fresh, installed={"openssl": "3.0.1-1"})
    yield _case("vulnerabilities", lambda: security.vulnerabilities({"openssl": "3.0.1-1"}),
                cache_file=fresh, settings_file='{"net_security_tracker": false}',
                installed={"openssl": "3.0.1-1"})
    yield _case("vulnerabilities", lambda: security.vulnerabilities(),
                cache_file=json.dumps({"at": 1e12, "groups": real}))
    yield _case("advisories", lambda: security.advisories(), cache_file=fresh)
    yield _case("advisories", lambda: security.advisories(), cache_file=fresh,
                settings_file='{"net_security_tracker": false}')
    yield _case("advisories", lambda: security.advisories(),
                cache_file=json.dumps({"at": 1e12}))          # fresh, no groups
    yield _case("advisories", lambda: security.advisories(),
                cache_file=json.dumps({"at": 1e12, "groups": []}))
    yield _case("advisories", lambda: security.advisories(),
                cache_file=json.dumps({"groups": GROUPS[:1], "at": True}), env=dead)
    # Stale or broken cache with the tracker unreachable (a dead proxy):
    # stale beats nothing; None if neither.
    for cf in (json.dumps({"at": 0, "groups": GROUPS[:2]}), None, "", "not json",
               "[1, 2]", json.dumps({"at": 0}), "{\"at\": 0, \"groups\": [\"ü\"]}"):
        yield _case("advisories", lambda: security.advisories(), env=dead, cache_file=cf)
        yield _case("advisories", lambda: security.advisories(3600), env=dead,
                    cache_file=cf, ttl=3600)
    # ttl: a fresh-ish cache is stale under a ttl of 0 / negative.
    yield _case("advisories", lambda: security.advisories(0), env=dead,
                cache_file=json.dumps({"at": 1e12, "groups": GROUPS[:1]}), ttl=-10**13)
    # The real tracker, fetched and written to the temp cache.
    yield _case("cache_after", lambda: _cache_after(), cache_file=None)

    # by_package / fixed_by / Vuln properties.
    for inst in INSTALLED:
        vs = _vulns(inst)
        yield "by_package", {"vulns": _ser(vs)}, (lambda v=vs: security.by_package(v))
        yield "Vuln.url", {"vulns": _ser(vs)}, (lambda v=vs: [x.url for x in v])
        yield "Vuln.rank", {"vulns": _ser(vs)}, (lambda v=vs: [x.rank for x in v])
        for pending in ("", "3.0.2-1", "3.0.1-2", "1:0-1", "99-1", "garbage", "ü-1"):
            yield "fixed_by", {"vulns": _ser(vs), "pending": pending}, \
                (lambda v=vs, p=pending: security.fixed_by(v, p))
    yield "by_package", {"vulns": []}, lambda: security.by_package([])
    odd = [security.Vuln("b", "1", "AVG-X", "Low", "", "2", ["c"]),
           security.Vuln("a", "1", "AVG-Y", "High", "", "", []),
           security.Vuln("b", "1", "AVG-Z", "Critical", "", "", ["a"])]
    yield "by_package", {"vulns": _ser(odd)}, lambda: security.by_package(odd)
    # by_package keeps first-appearance order (same() is blind to key order).
    yield "by_package.keys", {"vulns": _ser(odd)}, lambda: list(security.by_package(odd))
    yield "fixed_by", {"vulns": _ser(odd), "pending": "3"}, lambda: security.fixed_by(odd, "3")
    yield "Vuln.rank", {"vulns": _ser(odd)}, lambda: [x.rank for x in odd]

    # finding: off, unreachable, clean, and each severity, via the cache and
    # a pinned installed set is not possible (finding reads pacman -Q), so the
    # groups name packages every Arch machine has.
    yield _case("finding", lambda: security.finding(),
                settings_file='{"net_security_tracker": false}')
    yield _case("finding", lambda: security.finding(), env=dead, cache_file=None)
    yield _case("finding", lambda: security.finding(),
                cache_file=json.dumps({"at": 1e12, "groups": []}))
    many = [
        {"name": "AVG-F1", "packages": ["pacman", "glibc"], "status": "Fixed",
         "severity": "High", "type": "arbitrary code execution", "affected": "0-1",
         "fixed": "99999-1", "issues": ["CVE-4", "CVE-1", "CVE-3", "CVE-2"]},
        {"name": "AVG-F2", "packages": ["bash"], "status": "Vulnerable",
         "severity": "Critical", "type": "", "affected": "0-1", "issues": ["CVE-9"]},
        {"name": "AVG-F3", "packages": ["glibc"], "status": "Vulnerable",
         "severity": "Low", "type": "ünïcødé", "affected": "", "issues": []},
        {"name": "AVG-F4", "packages": ["coreutils"], "status": "Fixed",
         "severity": "Medium", "type": "x", "affected": "0-1", "fixed": "99999-1"},
    ]
    for gs in (many, many[1:3], many[3:], many[:1], [dict(many[3], severity="Unknown")]):
        yield _case("finding", lambda: security.finding(),
                    cache_file=json.dumps({"at": 1e12, "groups": gs}))
    # The real machine against the real tracker (temp cache).
    yield _case("finding", lambda: security.finding(), cache_file=None)
    yield from _review_round1()
    yield from _review_round2()
    yield from _review_round3()


def _review_round1():
    """Adversarial review, round 1: inputs where the port diverges."""
    dead = DEAD
    g1 = [{"name": "AVG-C", "packages": ["openssl"], "status": "Fixed",
           "severity": "High", "fixed": "3.0.2-1"}]
    # advisories: a fresh cache whose "groups" is not a list is returned as-is.
    for groups in ({"a": 1}, "groups", 5):
        yield _case("advisories", lambda: security.advisories(),
                    cache_file=json.dumps({"at": 1e12, "groups": groups}))
    # advisories: Python's json accepts NaN / Infinity / 1e400; serde does not.
    for at in ("1e400", "Infinity", "NaN", "-Infinity"):
        yield _case("advisories", lambda: security.advisories(), env=dead,
                    cache_file='{"at": %s, "groups": %s}' % (at, json.dumps(g1)))
    # advisories: a non-numeric "at" is a TypeError in Python.
    for at in ('"x"', "null", "[]", "{}"):
        yield _case("advisories", lambda: security.advisories(), env=dead,
                    cache_file='{"at": %s, "groups": %s}' % (at, json.dumps(g1)))
    # advisories: an integer "at" too large for a float (OverflowError).
    yield _case("advisories", lambda: security.advisories(), env=dead,
                cache_file='{"at": 1%s, "groups": %s}' % ("0" * 400, json.dumps(g1)))

    inst = {"openssl": "3.0.1-1"}
    odd_groups = [
        # null name / severity / type keep None in Python
        [{"name": None, "packages": ["openssl"], "status": "Fixed",
          "severity": None, "type": None, "fixed": "3.0.2-1"}],
        # severity of another type
        [{"name": "AVG-N", "packages": ["openssl"], "status": "Fixed",
          "severity": 1, "type": 7, "fixed": "3.0.2-1"}],
        # issues as a string: list("CVE-1") is its characters
        [{"name": "AVG-S", "packages": ["openssl"], "status": "Fixed",
          "severity": "High", "fixed": "3.0.2-1", "issues": "CVE-1"}],
        # issues holding non-strings
        [{"name": "AVG-I", "packages": ["openssl"], "status": "Fixed",
          "severity": "High", "fixed": "3.0.2-1", "issues": [5, None, "CVE-1"]}],
        # issues as an object: list() of its keys
        [{"name": "AVG-D", "packages": ["openssl"], "status": "Fixed",
          "severity": "High", "fixed": "3.0.2-1", "issues": {"CVE-1": 1}}],
        # fixed of another type: Python hands it to vercmp (TypeError)
        [{"name": "AVG-F", "packages": ["openssl"], "status": "Vulnerable",
          "severity": "High", "fixed": 2}],
        # a non-object group: Python's g.get is an AttributeError
        ["AVG-X"],
        # a NUL byte in a version
        [{"name": "AVG-0", "packages": ["openssl"], "status": "Fixed",
          "severity": "High", "fixed": "3.0.2\u0000-1"}],
    ]
    for gs in odd_groups:
        yield "vulnerabilities", {"installed": inst, "groups": gs}, \
            (lambda g=gs: security.vulnerabilities(inst, g))
    # packages as a string: Python iterates its characters.
    gs = [{"name": "AVG-P", "packages": "zq", "status": "Vulnerable", "severity": "Low"}]
    yield "vulnerabilities", {"installed": {"z": "1-1"}, "groups": gs}, \
        (lambda g=gs: security.vulnerabilities({"z": "1-1"}, g))
    # packages as an object: Python iterates its keys.
    gs2 = [{"name": "AVG-Q", "packages": {"z": 1}, "status": "Vulnerable", "severity": "Low"}]
    yield "vulnerabilities", {"installed": {"z": "1-1"}, "groups": gs2}, \
        (lambda g=gs2: security.vulnerabilities({"z": "1-1"}, g))

    # finding over the real pacman -Q: unicode lowercasing and a null severity.
    for sev in ("ΑΣ", "İ", "ẞ", None):
        gs = [{"name": "AVG-U", "packages": ["pacman"], "status": "Fixed",
               "severity": sev, "type": "t", "fixed": "99999-1", "issues": ["CVE-1"]}]
        yield _case("finding", lambda: security.finding(),
                    cache_file=json.dumps({"at": 1e12, "groups": gs}))
    # finding with integer CVE ids: Python sorts and joins (TypeError on join).
    gs = [{"name": "AVG-V", "packages": ["pacman"], "status": "Fixed",
           "severity": "High", "type": "t", "fixed": "99999-1", "issues": [2, 1]}]
    yield _case("finding", lambda: security.finding(),
                cache_file=json.dumps({"at": 1e12, "groups": gs}))


def _cache_after():
    r = security.advisories()
    p = security._cache_path()
    try:
        blob = json.loads(p.read_text())
    except (OSError, ValueError):
        blob = None
    return {"result": r,
            "groups": blob.get("groups") if isinstance(blob, dict) else None,
            "has_at": isinstance(blob, dict) and isinstance(blob.get("at"), (int, float))}


# ---- review round 2 -----------------------------------------------------------

def _fakebin(name, script_body):
    """A directory holding one executable `name` (a shell script), for PATH."""
    global _n
    _n += 1
    d = _ROOT / f"bin{_n}"
    d.mkdir(parents=True)
    f = d / name
    f.write_text("#!/bin/sh\n" + script_body)
    f.chmod(0o755)
    return str(d) + ":" + os.environ.get("PATH", "")


def _fake_pacman(data: bytes):
    """A `pacman` that prints exactly `data` (raw bytes)."""
    global _n
    _n += 1
    blob = _ROOT / f"pacman-out{_n}"
    blob.write_bytes(data)
    return _fakebin("pacman", f"cat '{blob}'\n")


def _fake_vercmp(out: str):
    """A `vercmp` that prints `out` (UTF-8) whatever it is asked."""
    global _n
    _n += 1
    blob = _ROOT / f"vercmp-out{_n}"
    blob.write_bytes(out.encode())
    return _fakebin("vercmp", f"cat '{blob}'\n")


NOPROXY = {"https_proxy": None, "HTTPS_PROXY": None, "http_proxy": None,
           "HTTP_PROXY": None, "all_proxy": None, "ALL_PROXY": None,
           "no_proxy": None, "NO_PROXY": None}


def _review_round2():
    """Adversarial review, round 2."""
    # vercmp output parsed by Python's int() vs Rust's i64 parse.
    g_fix = [{"name": "AVG-V1", "packages": ["openssl"], "status": "Fixed",
              "severity": "High", "fixed": "2-1"}]
    g_aff = [{"name": "AVG-V2", "packages": ["openssl"], "status": "Vulnerable",
              "severity": "High", "affected": "2-1"}]
    inst = {"openssl": "1-1"}
    for out in ("-99999999999999999999\n", "-1_0\n", "-١\n", "\x1c-1\x1c\n",
                "99999999999999999999\n", "1_0\n", "١\n"):
        path = _fake_vercmp(out)
        for gs in (g_fix, g_aff):
            yield _case("vulnerabilities",
                        (lambda g=gs: security.vulnerabilities(inst, g)),
                        env={"PATH": path}, installed=inst, groups=gs)
        vs = [security.Vuln("openssl", "1-1", "AVG-V1", "High", "", "2-1", [])]
        yield _case("fixed_by", (lambda v=vs: security.fixed_by(v, "3-1")),
                    env={"PATH": path}, vulns=_ser(vs), pending="3-1")

    # pacman -Q output read through a fake pacman.
    for data in ("a �\n".encode(),            # a real U+FFFD is valid UTF-8
                 b"a 1\x85b 2\n",                   # \x85 (as UTF-8) splits lines
                 "a 1 b 2　c\n".encode(),
                 b"a 1\x1cb 2\n",
                 b"a\x1f 1\n",
                 b"a  1  2 \n",
                 b"x 1\r\ny 2\rz 3\x0bw 4\x0cv 5\n",
                 b"",
                 b"onlyname\nq 1\n"):
        yield _case("installed_versions", lambda: security.installed_versions(),
                    env={"PATH": _fake_pacman(data)})

    # Values Python's json keeps but serde_json cannot hold, read from the cache.
    def raw(groups_text):
        return '{"at": 1e12, "groups": %s}' % groups_text
    for gt in ('[{"name": NaN, "packages": ["pacman"], "status": "Fixed", '
               '"severity": "High", "type": Infinity, "fixed": "99999-1"}]',
               '[{"name": 123456789012345678901234567890, "packages": ["pacman"], '
               '"status": "Fixed", "severity": "High", '
               '"type": 123456789012345678901234567890, "fixed": "99999-1"}]',
               '[{"name": "AVG-S", "packages": ["pacman"], "status": "Fixed", '
               '"severity": "High", "type": "a\\ud800b", "fixed": "99999-1"}]',
               '[{"name": "AVG-R", "packages": ["pacman"], "status": "Fixed", '
               '"severity": "High", "type": ["a\\u200bb", "\\u0085", 1e16], "fixed": "99999-1"}]',
               '[{"name": "AVG-N", "packages": ["pacman"], "status": "Vulnerable", '
               '"severity": "High", "type": "t", "fixed": NaN}]',
               '[{"name": "AVG-M", "packages": ["pacman"], "status": "Vulnerable", '
               '"severity": NaN, "type": "t", "affected": "0-1"}]'):
        # NaN cannot cross JSON (and nan != nan): compared as json.dumps text.
        yield _case("advisories.dumps", lambda: json.dumps(security.advisories()),
                    cache_file=raw(gt))
        yield _case("vulnerabilities.dumps", lambda: _dumps_vulns(security.vulnerabilities()),
                    cache_file=raw(gt))
        # finding(): the hook's Finding cannot carry a lone surrogate, so the
        # row is compared with U+FFFD in its place, and the items exactly as
        # json.dumps text.
        yield _case("finding", lambda: _no_surrogates(security.finding()), cache_file=raw(gt))
        yield _case("finding.items.dumps", lambda: json.dumps(security.finding().items),
                    cache_file=raw(gt))

    # Proxy environment: Python's urllib uses only https_proxy/HTTPS_PROXY for
    # an https URL and matches no_proxy by domain suffix; ureq also takes
    # ALL_PROXY / HTTP_PROXY and matches "archlinux.org" literally.
    stale = json.dumps({"at": 0, "groups": [{"name": "AVG-STALE"}]})
    for env in ({"ALL_PROXY": "http://127.0.0.1:9"},
                {"all_proxy": "http://127.0.0.1:9"},
                {"HTTP_PROXY": "http://127.0.0.1:9"},
                {"http_proxy": "http://127.0.0.1:9"},
                {"https_proxy": "http://127.0.0.1:9", "no_proxy": "archlinux.org"},
                {"https_proxy": "http://127.0.0.1:9", "no_proxy": "*"},
                {"HTTPS_PROXY": "http://127.0.0.1:9"},
                {"https_proxy": "http://127.0.0.1:9"}):
        e = {**NOPROXY, **env}
        yield _case("advisories", lambda: security.advisories(),
                    env=e, cache_file=stale)


# ---- review round 3 -----------------------------------------------------------

def _no_surrogates(v):
    """v (a dataclass, dict, list or str) with each lone surrogate as U+FFFD."""
    if dataclasses.is_dataclass(v) and not isinstance(v, type):
        v = dataclasses.asdict(v)
    if isinstance(v, str):
        return re.sub("[\ud800-\udfff]", "\ufffd", v)
    if isinstance(v, dict):
        return {k: _no_surrogates(x) for k, x in v.items()}
    if isinstance(v, (list, tuple)):
        return [_no_surrogates(x) for x in v]
    return v


def _dumps_vulns(vs):
    return json.dumps(None if vs is None else [dataclasses.asdict(v) for v in vs])


def _err(fn):
    """fn()'s result, or the text of what it raised (the Rust `.error` forms)."""
    try:
        return fn()
    except Exception as exc:                            # noqa: BLE001
        return f"{type(exc).__name__}: {exc}"


def _py_proxy():
    """What urlopen(URL) does about a proxy: None (direct), "error", or
    [credentials, host, port] of the CONNECT tunnel."""
    import http.client
    import urllib.parse
    import urllib.request as u
    proxies = u.getproxies_environment()
    p = proxies.get("https")
    if p is None:
        return None
    try:
        _, user, password, hostport = u._parse_proxy(p)
    except ValueError:
        return "error"
    if u.proxy_bypass("security.archlinux.org"):
        return None
    uq = urllib.parse.unquote
    creds = f"{uq(user)}:{uq(password)}" if user and password else ""
    try:
        host, port = http.client.HTTPSConnection("x")._get_hostport(uq(hostport), None)
    except http.client.InvalidURL:
        return "error"
    if not host or not 0 <= port <= 65535:
        return "error"
    return [creds, host, port]


def _sorted_set(items):
    return _err(lambda: sorted({c for c in items}))


def _review_round3():
    """Fixes for review round 2, checked on the values themselves."""
    # Proxy choice, by the variables urllib reads.
    P = "http://127.0.0.1:9"
    for env in ({}, {"ALL_PROXY": P}, {"all_proxy": P}, {"HTTP_PROXY": P},
                {"http_proxy": P}, {"https_proxy": P}, {"HTTPS_PROXY": P},
                {"Https_Proxy": P}, {"HTTPS_PROXY": P, "https_proxy": ""},
                {"HTTPS_PROXY": "", "https_proxy": P},
                {"https_proxy": P, "no_proxy": "archlinux.org"},
                {"https_proxy": P, "no_proxy": ".ArchLinux.ORG"},
                {"https_proxy": P, "NO_PROXY": "x.org, security.archlinux.org "},
                {"https_proxy": P, "no_proxy": "linux.org"},
                {"https_proxy": P, "no_proxy": "*.archlinux.org"},
                {"https_proxy": P, "no_proxy": "*"},
                {"https_proxy": P, "no_proxy": ""},
                {"https_proxy": "127.0.0.1:3128"},
                {"https_proxy": "socks5://h:1080"},
                {"https_proxy": "http://user:p%40ss:w@proxy:8080/path"},
                {"https_proxy": "http://user@proxy:8080"},
                {"https_proxy": "http://:pw@proxy:8080"},
                {"https_proxy": "http://proxy"},
                {"https_proxy": "http://proxy:"},
                {"https_proxy": "http://[::1]:3128"},
                {"https_proxy": "http://proxy:x"},
                {"https_proxy": "http://proxy:99999"},
                {"https_proxy": "http:/proxy"},
                {"https_proxy": "http:proxy:1"},
                {"https_proxy": "http://"},
                {"https_proxy": "http://pr%6fxy:8_0"},
                {"https_proxy": "http:/x", "no_proxy": "*"},
                {"https_proxy": P, "REQUEST_METHOD": "GET"}):
        e = {**NOPROXY, "Https_Proxy": None, "REQUEST_METHOD": None, **env}
        yield _case("_tracker_proxy", _py_proxy, env=e)

    # int(out.strip()) of vercmp's output, by sign.
    for out in ("-1\n", "0\n", "1\n", "-0", "+1", "-99999999999999999999", "-1_0",
                "1_0", "-\u0661", "\u0661", "\u0967", "\x1c-1\x1c\n", " \u3000-1\u2028",
                "\x85 1", "- 1", "_1", "1_", "1__0", "", "-", "x", "1.0", "\ufffd",
                "1 2", "0x1", "\U0001d7d9", "\uff11"):
        def sign(o=out):
            try:
                n = int(o.strip())
            except ValueError:
                return None
            return (n > 0) - (n < 0)
        yield "_py_int_sign", {"s": out}, sign

    # Set order and sort errors over hashable non-strings (strings hash with
    # a per-process salt, so their order is Python's chance).
    import random
    rnd = random.Random(7)
    pool = [None, True, False, 0, 1, 2, -1, 7, 8, 9, 16, 17, 2.5, 0.5, -3.25, 1e16, 1.0,
            12345678901234567890, -1234567890123456789, 2**61 - 1, 2**61, 1e300, 3.0]
    for _ in range(60):
        items = [rnd.choice(pool) for _ in range(rnd.randint(0, 12))]
        yield "_py_set", {"items": items}, (lambda i=items: list({c for c in i}))
        yield "_py_sorted_set.error", {"items": items}, (lambda i=items: _sorted_set(i))
    for items in ([None, 1], [2.5, 1], [1, None], [None], [3, 2, 1, None], list(range(20)) + [None],
                  [1.5, 0.5, 2.5], list(range(70, 0, -1)), [True, 1, 1.0, 0, False]):
        yield "_py_sorted_set.error", {"items": items}, (lambda i=items: _sorted_set(i))

    # finding() with those as CVE ids, by message.
    def fv(cves, kind="t"):
        return [security.Vuln("pacman", "1-1", "AVG-1", "High", kind, "99999-1", cves)]
    for cves in ([None, 1], [2.5, 1], [1, 2], [3], [None], ["CVE-1", "CVE-2"],
                 [1, 2.5, None], [True, 1.0]):
        vs = fv(cves)
        yield _case("finding_for.error", (lambda v=vs: _finding_with(v)), vulns=_ser(vs))
    for kind in (["a\u200bb", "\x85", "\t", "'", "\"'", 1e16], {"k": [None, True]},
                 1e16, 0.1, "", 0, None):
        vs = fv(["CVE-1"], kind)
        yield _case("finding_for.error", (lambda v=vs: _finding_with(v)), vulns=_ser(vs))

    # pacman -Q output Python cannot decode, by message.
    for data in (b"a \xff\n", b"ab\x80c 1\n", b"x 1\n\xe2\x82", b"\xed\xa0\x80",
                 b"y 2\n\xf0\x90\x28", b"\xc0\xaf"):
        yield _case("installed_versions.error",
                    lambda: _err(security.installed_versions),
                    env={"PATH": _fake_pacman(data)})

    # Values serde_json cannot carry, read from the cache, by message.
    def raw(groups_text):
        return '{"at": 1e12, "groups": %s}' % groups_text
    for gt in ('[{"name": "AVG-N", "packages": ["pacman"], "status": "Vulnerable", '
               '"severity": "High", "type": "t", "fixed": NaN}]',
               '[{"name": "AVG-N", "packages": ["pacman"], "status": "Vulnerable", '
               '"severity": "High", "type": "t", "affected": Infinity}]',
               '[{"name": "AVG-N", "packages": ["pacman"], "status": "Fixed", '
               '"severity": "High", "type": NaN, "fixed": "99999-1", "issues": [NaN, NaN]}]'):
        if "NaN]" in gt:
            # no error: the NaNs come back, compared as json.dumps text
            yield _case("vulnerabilities.dumps", lambda: _dumps_vulns(security.vulnerabilities()),
                        cache_file=raw(gt))
        else:
            yield _case("vulnerabilities.error", lambda: _err(security.vulnerabilities),
                        cache_file=raw(gt))
        yield _case("finding.error", lambda: _err(security.finding), cache_file=raw(gt))
