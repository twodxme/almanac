"""Parity cases for verify.py.

Probes are called and their answer compared; `for_app` is compared as
{"probe": whether one was chosen, "value": what it answers}. Probe values go
to Rust as JSON (a set as a list of strings, a (size, mtime) identity as a
two-item list) and to Python as the real types. The real machine is read for
pacman, git, mise and binary versions, file identities, pending updates and
the health checks -- every one of them read-only.
"""
import atexit
import os
import shutil
import tempfile
from pathlib import Path

import inventory
import maintenance as m
import verify

_TMP = tempfile.mkdtemp(prefix="almanac-parity-verify-")
atexit.register(shutil.rmtree, _TMP, True)
_FILE = Path(_TMP, "app.AppImage")
_FILE.write_text("x" * 1234)
os.utime(_FILE, (1_700_000_000.75, 1_700_000_000.75))
_UNI = Path(_TMP, "ünï cødé ✓.AppImage")
_UNI.write_bytes(b"\0" * 7)
_OLD = Path(_TMP, "old")
_OLD.write_text("")
os.utime(_OLD, (-5.5, -5.5))
_REPO = str(Path(__file__).resolve().parent.parent)        # the rust checkout


def _py(v):
    """JSON form -> what a Python probe would answer."""
    if isinstance(v, list):
        if len(v) == 2 and all(isinstance(x, int) and not isinstance(x, bool) for x in v):
            return tuple(v)
        return set(v)
    return v


def _outcome(changed, message):
    o = verify.Outcome(changed, message)
    return {"changed": o.changed, "message": o.message, "ok": o.ok}


def _for_app(d):
    def run():
        p = verify.for_app(inventory.App(**d))
        return {"probe": p is not None, "value": p() if p else None}
    return run


def _finding(d):
    return None if d is None else m.Finding(**d)


def _recheck(fid):
    def run():
        f = verify.recheck(fid)
        if f is not None and callable(f.fix):
            f.fix = "<callable>"
        return f
    return run


def cases():
    # Outcome and its ok property
    for changed in (True, False, None):
        yield "outcome", {"changed": changed, "message": "m ✓"}, \
            (lambda c=changed: _outcome(c, "m ✓"))

    # _out
    yield "_out", {"cmd": ["echo", "  hi  "]}, lambda: verify._out(["echo", "  hi  "])
    yield "_out", {"cmd": ["false"]}, lambda: verify._out(["false"])
    yield "_out", {"cmd": ["/nonexistent/binary"]}, lambda: verify._out(["/nonexistent/binary"])
    yield "_out", {"cmd": ["sh", "-c", "printf 'ünï\\n\\n'"]}, \
        lambda: verify._out(["sh", "-c", "printf 'ünï\\n\\n'"])
    yield "_out", {"cmd": ["sleep", "3"], "timeout": 1}, lambda: verify._out(["sleep", "3"], timeout=1)

    # probes, against the real machine
    for name in ("pacman", "glibc", "no-such-package-xyz", "", "ünïcode", "pacman glibc"):
        yield "installed_package", {"name": name}, (lambda n=name: verify.installed_package(n)())
    for repo in (_REPO, "/nonexistent", _TMP, ""):
        yield "git_head", {"repo": repo}, (lambda r=repo: verify.git_head(r)())
    for tool in ("claude", "node", "no-such-tool", ""):
        yield "mise_version", {"tool": tool}, (lambda t=tool: verify.mise_version(t)())
    for binary in ("git", "pacman", "no-such-binary-xyz", ""):
        yield "binary_version", {"binary": binary}, (lambda b=binary: verify.binary_version(b)())
    # "" is the working directory (Path("") is "."), whose mtime other builds
    # keep moving between the two sides; the Rust test pins it to "." instead.
    for path in (str(_FILE), str(_UNI), str(_OLD), _TMP, "/nonexistent/x", "/usr/bin/pacman"):
        yield "file_identity", {"path": path}, (lambda p=path: verify.file_identity(p)())
    yield "pending_updates", {}, lambda: verify.pending_updates()()

    # for_app: which probe is chosen, and what it answers
    apps = [
        dict(name="pacman", version="1", source=inventory.REPO),
        dict(name="no-such-package-xyz", version="1", source=inventory.AUR),
        dict(name="glibc", version="1", source=inventory.SYSTEM),
        dict(name="no-such-package-xyz", version="1", source=inventory.ORPHAN),
        dict(name="almanac", version="1", source=inventory.LOCAL, extra={"repo": _REPO}),
        dict(name="almanac", version="1", source=inventory.LOCAL, extra={"repo": ""}),
        dict(name="x", version="1", source=inventory.LOCAL),
        dict(name="x", version="1", source=inventory.APPIMAGE, exec_path=str(_FILE)),
        dict(name="x", version="1", source=inventory.APPIMAGE, exec_path="/nonexistent"),
        dict(name="x", version="1", source=inventory.APPIMAGE),
        dict(name="claude", version="1", source="agent", extra={"manager": "mise"}),
        dict(name="x", version="1", source="agent", extra={"manager": "mise", "key": "claude"}),
        dict(name="no-such-tool", version="1", source="agent", extra={"manager": "mise", "key": ""}),
        dict(name="x", version="1", source="agent", extra={"binary": "git"}),
        dict(name="x", version="1", source="agent", extra={"binary": "no-such-binary-xyz"}),
        dict(name="x", version="1", source="agent", extra={"binary": ""}),
        dict(name="x", version="1", source="flatpak"),
        dict(name="x", version="1", source=""),
    ]
    for a in apps:
        yield "for_app", {"app": a}, _for_app(a)

    # judge_app
    base = dict(name="yaru", version="1.0", source=inventory.REPO, update="2.0")
    sha1, sha2 = "a" * 40, "b" * 40
    jcases = [
        (base, "1.0", "1.0"),
        (base, "1.0", "2.0"),
        (base, "1.0", "2.0-1"),
        (base, "1.0", "2.5"),
        (base, "1.0", "3.0"),
        (dict(base, update="2.0-3"), "1.0", "2.0-4"),
        (dict(base, update="2.0-3"), "1.0", "1.9"),
        (dict(base, update=""), "1.0", "1.1"),
        (dict(base, update="-1"), "1.0", "9"),
        (base, None, "2"),
        (base, "1.0", None),
        (base, None, None),
        (base, sha1, sha2),
        (dict(base, update=""), sha1, sha2),
        (base, "1.0", "x" * 23),
        (dict(base, update=""), "1.0", "x" * 24),
        (dict(base, update=""), "1.0", "ü" * 23),
        (base, "1.0", ""),
        (base, [10, 100], [10, 100]),
        (base, [10, 100], [11, 200]),
        (base, "1.0", [11, 200]),
        (base, ["a"], ["b"]),
        (base, True, False),
        (base, True, True),
        (base, {"p": "abc"}, {"p": "def"}),
        (base, {"p": None}, {"p": None}),
        (dict(name="ünï", version="β", source=inventory.AUR, update="γ"), "β", "δ"),
    ]
    for app, before, after in jcases:
        yield "judge_app", {"app": app, "before": before, "after": after}, \
            (lambda a=app, b=before, c=after: verify.judge_app(inventory.App(**a), _py(b), _py(c)))

    # judge_pending
    pcases = [
        (["a", "b", "c"], ["c"]),
        (["a"], ["a"]),
        ([], []),
        ([], ["a"]),
        (["a", "b"], []),
        (["a", "b"], ["c"]),
        (["e", "d", "c", "b", "a", "f"], ["f", "e", "d", "c", "b"]),
        (["e", "d", "c", "b", "a"], ["e", "d", "c", "b"]),
        (["Zed", "ünï", "abc", "x"], ["Zed", "ünï", "abc"]),
        (None, ["a"]),
        (["a"], None),
        (None, None),
    ]
    for before, after in pcases:
        yield "judge_pending", {"before": before, "after": after}, \
            (lambda b=before, c=after: verify.judge_pending(_py(b) if b is None else set(b),
                                                            _py(c) if c is None else set(c)))

    # judge_finding
    jb = dict(id="journal", title="Journal", detail="1 GB", severity=m.WARN, size_mb=500, fix=["true"])
    fcases = [
        (jb, dict(jb)),
        (jb, dict(id="journal", title="Journal", detail="ok", severity=m.OK)),
        (jb, None),
        (jb, dict(jb, severity=m.OK, size_mb=0)),
        (jb, dict(jb, size_mb=10)),
        (jb, dict(jb, size_mb=499.5)),
        (jb, dict(jb, size_mb=499.0)),
        (jb, dict(jb, size_mb=500.5, count=3)),
        (jb, dict(jb, size_mb=600)),
        (jb, dict(jb, fix=None)),
        (jb, dict(jb, fix=None, size_mb=0.5)),
        (jb, dict(jb, fix="<callable>")),
        (dict(jb, size_mb=5000), dict(jb, size_mb=100, severity=m.OK)),
        (dict(jb, size_mb=3000), dict(jb, size_mb=100)),
        (dict(jb, size_mb=1.5), dict(jb, size_mb=0.0, severity=m.OK)),
        (dict(jb, title="Jöurnal ✓", detail=""), dict(jb, title="other", detail="ünï")),
        (dict(jb, severity=m.INFO), dict(jb, severity=m.WARN)),
    ]
    for before, after in fcases:
        yield "judge_finding", {"before": before, "after": after}, \
            (lambda b=before, c=after: verify.judge_finding(_finding(b), _finding(c)))

    # recheck, against the real machine (every check is read-only)
    for fid in ("snapshot", "vulnerabilities", "config-reset", "journal", "disk", "trash",
                "failed-units", "pacnew", "", "nope", "Journal"):
        yield "recheck", {"finding_id": fid}, _recheck(fid)


# ---- review round 1: suspected divergences ------------------------------------

_NS = Path(_TMP, "ns")
_NS.write_text("ns")
# st_mtime is a float: 20_000_000.999999999 rounds to 20_000_001.0 (a
# small epoch, so the 1-second gap is outside run.py's 1e-9 relative tolerance)
os.utime(_NS, ns=(20_000_000_999_999_999, 20_000_000_999_999_999))


def review_cases():
    # text=True decodes strictly: invalid UTF-8 raises UnicodeDecodeError,
    # which (OSError, SubprocessError) does not catch.
    yield "_out", {"cmd": ["printf", "\\377ok"]}, lambda: verify._out(["printf", "\\377ok"])
    # text=True is universal-newlines mode: \r\n and \r become \n.
    yield "_out", {"cmd": ["printf", "a\\r\\nb\\rc"]}, lambda: verify._out(["printf", "a\\r\\nb\\rc"])
    # str.strip() also strips \x1c-\x1f (str.isspace); Rust trim() does not.
    yield "_out", {"cmd": ["printf", "\\037hi\\034"]}, lambda: verify._out(["printf", "\\037hi\\034"])
    # A grandchild holding stdout open: communicate() waits for EOF and times out.
    yield "_out", {"cmd": ["sh", "-c", "echo hi; sleep 3 &"], "timeout": 1}, \
        lambda: verify._out(["sh", "-c", "echo hi; sleep 3 &"], timeout=1)
    # subprocess.run([]) raises IndexError, which is not caught.
    yield "_out", {"cmd": []}, lambda: verify._out([])
    # Path() drops a trailing slash and "." components; stat() then succeeds.
    for p in (str(_FILE) + "/", str(_FILE) + "/.", _TMP + "//app.AppImage"):
        yield "file_identity", {"path": p}, (lambda p=p: verify.file_identity(p)())
    # int(st_mtime) of a float that rounded up at nanosecond precision.
    yield "file_identity", {"path": str(_NS)}, lambda: verify.file_identity(str(_NS))()
    # An embedded NUL raises ValueError, not OSError.
    yield "file_identity", {"path": "a\0b"}, lambda: verify.file_identity("a\0b")()
    # for_app: non-string repo/binary reach subprocess and raise TypeError.
    for a in (dict(name="x", version="1", source=inventory.LOCAL, extra={"repo": 5}),
              dict(name="x", version="1", source="agent", extra={"binary": 5}),
              dict(name="pacman", version="1", source=inventory.REPO, extra=None),
              dict(name="x", version="1", source="agent", extra={"manager": "mise", "key": 0})):
        yield "for_app", {"app": a}, _for_app(a)
    # judge_app: probe values ProbeValue cannot hold (a dict of ints).
    base = dict(name="yaru", version="1.0", source=inventory.REPO, update="2.0")
    yield "judge_app", {"app": base, "before": {"p": 1}, "after": {"p": 2}}, \
        lambda: verify.judge_app(inventory.App(**base), {"p": 1}, {"p": 2})
    # judge_app: a list of two numbers that is not an (int, int) identity
    yield "judge_app", {"app": base, "before": [1.5, 2], "after": [1.5, 3]}, \
        lambda: verify.judge_app(inventory.App(**base), (1.5, 2), (1.5, 3))

    # review round 2: sets of non-strings and other probe answers
    for before, after in (([1, 2], [2]), ([1, 2], []), (["a", 1], ["a"]), ([1.0, True], [1]),
                          ([3, 1, 2], [3]), ("ab", "b"), ([[1]], ["a"])):
        yield "judge_pending", {"before": before, "after": after}, \
            (lambda b=before, c=after: verify.judge_pending(set(b), set(c)))
    for before, after in (({"p": 1}, {"p": 1.0}), ([1, 2], [1, 2.5]), (1, True), ("1", 1)):
        yield "judge_app", {"app": base, "before": before, "after": after}, \
            (lambda b=before, c=after: verify.judge_app(inventory.App(**base), _py(b), _py(c)))
    for key in ([1], {"a": 1}, 5):
        a = dict(name="x", version="1", source="agent", extra={"manager": "mise", "key": key})
        yield "for_app", {"app": a}, _for_app(a)
    yield "_out", {"cmd": ["sh", "-c", "printf 'x\\377' >&2; echo ok"]}, \
        lambda: verify._out(["sh", "-c", "printf 'x\\377' >&2; echo ok"])
    yield "_out", {"cmd": ["sh", "-c", "echo hi; sleep 3 & exit 1"], "timeout": 1}, \
        lambda: verify._out(["sh", "-c", "echo hi; sleep 3 & exit 1"], timeout=1)
    yield "installed_package", {"name": "pacman\x1f"}, lambda: verify.installed_package("pacman\x1f")()


_orig_cases = cases


def cases():  # noqa: F811
    yield from _orig_cases()
    yield from review_cases()


# ---- review round 2 (second reviewer): suspected divergences --------------------

_BADVER = Path(_TMP, "badver")
# A binary whose --version output is not UTF-8: agents._version_of decodes
# with text=True and raises UnicodeDecodeError, which it does not catch.
_BADVER.write_bytes(b"#!/bin/sh\nprintf '\\377 tool 1.2.3\\n'\n")
_BADVER.chmod(0o755)


def review2_cases():
    # binary_version: _version_of lets ValueError / UnicodeDecodeError through,
    # so the probe raises; agents::version_of swallows both into "".
    yield "binary_version", {"binary": "a\0b"}, lambda: verify.binary_version("a\0b")()
    yield "binary_version", {"binary": str(_BADVER)}, lambda: verify.binary_version(str(_BADVER))()
    a = dict(name="x", version="1", source="agent", extra={"binary": str(_BADVER)})
    yield "for_app", {"app": a}, _for_app(a)
    # None where the dataclass annotation says str: Python just finds it falsy.
    for a in (dict(name="x", version="1", source=inventory.APPIMAGE, exec_path=None),
              dict(name="x", version="1", source=None)):
        yield "for_app", {"app": a}, _for_app(a)
    base = dict(name="yaru", version="1.0", source=inventory.REPO, update=None)
    yield "judge_app", {"app": base, "before": "1.0", "after": "2.0"}, \
        lambda: verify.judge_app(inventory.App(**base), "1.0", "2.0")
    vbase = dict(name="yaru", version=None, source=inventory.REPO, update="2.0")
    yield "judge_app", {"app": vbase, "before": "1.0", "after": "1.0"}, \
        lambda: verify.judge_app(inventory.App(**vbase), "1.0", "1.0")
    # A (size, mtime) identity with an int beyond 64 bits: still a tuple to Python.
    rb = dict(name="yaru", version="1.0", source=inventory.REPO, update="2.0")
    yield "judge_app", {"app": rb, "before": [2**64, 1], "after": [1, 2**64]}, \
        lambda: verify.judge_app(inventory.App(**rb), (2**64, 1), (1, 2**64))
    # judge_finding: Finding fields that are not the annotated type.
    jb = dict(id="journal", title="Journal", detail="1 GB", severity=m.WARN, size_mb=500, fix=["true"])
    for before, after in ((jb, dict(jb, count=None)), (jb, dict(jb, count=0.0)),
                          (dict(jb, count=None), dict(jb, count=None)),
                          (jb, dict(jb, detail=None)),
                          # fmt_size at exact ties: round-half-even
                          (dict(jb, size_mb=502.5), dict(jb, size_mb=500, severity=m.OK)),
                          (dict(jb, size_mb=1780), dict(jb, size_mb=500, severity=m.OK)),
                          (dict(jb, size_mb=503.5), dict(jb, size_mb=500)),
                          (dict(jb, size_mb=500 + 1024 * 2.25), dict(jb, size_mb=500))):
        yield "judge_finding", {"before": before, "after": after}, \
            (lambda b=before, c=after: verify.judge_finding(_finding(b), _finding(c)))


_orig_cases2 = cases


def cases():  # noqa: F811
    yield from _orig_cases2()
    yield from review2_cases()


# ---- review round 2 (third reviewer): suspected divergences ---------------------

def review3_cases():
    jb = dict(id="journal", title="Journal", detail="1 GB", severity=m.WARN, size_mb=500, fix=["true"])
    # size_mb as Python int: before - after is exact integer arithmetic;
    # Rust's f64 rounds 2**53 + 1 to 2**53, so 1 MB freed becomes 0.
    for before, after in ((dict(jb, size_mb=2**53 + 1), dict(jb, size_mb=2**53)),
                          (dict(jb, size_mb=2**53 + 1), dict(jb, size_mb=2**53, severity=m.OK)),
                          # count beyond i64: Python compares it, Rust cannot hold it
                          (dict(jb, count=2**63), dict(jb, count=2**63))):
        yield "judge_finding", {"before": before, "after": after}, \
            (lambda b=before, c=after: verify.judge_finding(_finding(b), _finding(c)))
    # before=None: Python reads before.title / before.size_mb and raises.
    for after in (None, dict(jb)):
        yield "judge_finding", {"before": None, "after": after}, \
            (lambda c=after: verify.judge_finding(None, _finding(c)))
    # extra that is truthy but not a dict: a package source never calls .get on it.
    a = dict(name="pacman", version="1", source=inventory.REPO, extra="abc")
    yield "for_app", {"app": a}, _for_app(a)
    # a non-string argv item: subprocess raises TypeError.
    yield "_out", {"cmd": ["echo", 5]}, lambda: verify._out(["echo", 5])
    # timeout=None is "no timeout", not 30 seconds.
    yield "_out", {"cmd": ["sh", "-c", "sleep 31; echo hi"], "timeout": None}, \
        lambda: verify._out(["sh", "-c", "sleep 31; echo hi"], timeout=None)
    # timeout=0: TimeoutExpired before anything is read.
    yield "_out", {"cmd": ["echo", "hi"], "timeout": 0}, lambda: verify._out(["echo", "hi"], timeout=0)


_orig_cases3 = cases


def cases():  # noqa: F811
    yield from _orig_cases3()
    yield from review3_cases()
