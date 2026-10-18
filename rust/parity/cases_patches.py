"""Parity cases for patches.py: analyze against scratch git repos (and the
real plugin checkouts, read-only), steps and explain on fixed states."""
import atexit
import glob
import os
import shutil
import subprocess
import tempfile
from pathlib import Path

import patches

# Nothing here may write to a real checkout: no opportunistic index refresh.
os.environ["GIT_OPTIONAL_LOCKS"] = "0"

BASE = "one\ntwo\nthree\nfour\nfive\nsix\n"


def _git(*a, cwd):
    subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@t", *a],
                   cwd=cwd, check=True, capture_output=True)


def _repo(root, name):
    """A tiny upstream with one file, and a clone of it."""
    up = Path(root, name)
    _git("init", "-q", "-b", "main", str(up), cwd=root)
    (up / "f.txt").write_text(BASE)
    _git("add", ".", cwd=up)
    _git("commit", "-q", "-m", "first", cwd=up)
    clone = Path(root, name + "-clone")
    _git("clone", "-q", str(up), str(clone), cwd=root)
    return up, clone


def _upstream(up, clone, text, msg="upstream", fname="f.txt"):
    (up / fname).write_text(text)
    _git("add", ".", cwd=up)
    _git("commit", "-qm", msg, cwd=up)
    _git("fetch", "-q", cwd=clone)


def _fixtures():
    root = tempfile.mkdtemp(prefix="almanac-parity-patches-")
    atexit.register(shutil.rmtree, root, True)
    r = {}

    up, c = _repo(root, "absorbed")
    _upstream(up, c, "one\nTWO\nthree\nfour\nfive\nsix\n", "fix")
    (c / "f.txt").write_text("one\nTWO\nthree\nfour\nfive\nsix\n")
    r["absorbed"] = c

    up, c = _repo(root, "applies")
    _upstream(up, c, "ONE\ntwo\nthree\nfour\nfive\nsix\n")
    (c / "f.txt").write_text("one\ntwo\nthree\nfour\nfive\nSIX\n")
    r["applies"] = c

    up, c = _repo(root, "conflicts")
    _upstream(up, c, "one\nUPSTREAM\nthree\nfour\nfive\nsix\n")
    _upstream(up, c, "one\nUPSTREAM2\nthree\nfour\nfive\nsix\n", "second — ünïcode")
    (c / "f.txt").write_text("one\nMINE\nthree\nfour\nfive\nsix\n")
    r["conflicts"] = c

    up, c = _repo(root, "clean")
    r["clean"] = c

    up, c = _repo(root, "untracked")
    (c / "new file.txt").write_text("x")
    (c / "ñandú.md").write_text("y")
    r["untracked_clean"] = c

    up, c = _repo(root, "untracked2")
    (c / "f.txt").write_text(BASE + "seven\n")
    (c / "extra.txt").write_text("x")
    r["untracked_dirty"] = c

    # A clone with no upstream configured: @{upstream} does not resolve.
    up, c = _repo(root, "noupstream")
    _git("checkout", "-q", "-b", "local", cwd=c)
    (c / "f.txt").write_text(BASE + "x\n")
    r["no_upstream"] = c

    # Several files, one deleted, one binary, one unicode name.
    up, c = _repo(root, "many")
    for n in ("a.txt", "b.txt", "c.txt", "ü.txt"):
        (up / n).write_text("x\n")
    (up / "bin.dat").write_bytes(bytes(range(256)))
    _git("add", ".", cwd=up)
    _git("commit", "-qm", "more", cwd=up)
    _git("pull", "-q", cwd=c)
    _upstream(up, c, "y\n", "touch a", "a.txt")
    for n in ("a.txt", "b.txt", "c.txt", "ü.txt"):
        (c / n).write_text("mine\n")
    (c / "bin.dat").write_bytes(bytes(reversed(range(256))))
    (c / "f.txt").unlink()
    r["many"] = c

    # Staged-only change still counts (diff against HEAD).
    up, c = _repo(root, "staged")
    (c / "f.txt").write_text(BASE.replace("three", "3"))
    _git("add", ".", cwd=c)
    r["staged"] = c

    # Whitespace-only diff is still a change.
    up, c = _repo(root, "ws")
    (c / "f.txt").write_text(BASE + "   \n")
    r["whitespace"] = c

    # --- review round 1: bytes Python's text-mode pipes treat differently ---
    # CRLF file, upstream made the same edit: Python's universal newlines turn
    # the diff's \r\n into \n before it is fed to `git apply`.
    crlf = "one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix\r\n"
    up, c = _repo(root, "crlf_absorbed")
    (up / "f.txt").write_bytes(crlf.encode()); _git("commit", "-qam", "crlf", cwd=up)
    _git("pull", "-q", cwd=c)
    (up / "f.txt").write_bytes(crlf.replace("two", "TWO").encode())
    _git("commit", "-qam", "fix", cwd=up); _git("fetch", "-q", cwd=c)
    (c / "f.txt").write_bytes(crlf.replace("two", "TWO").encode())
    r["crlf_absorbed"] = c

    up, c = _repo(root, "crlf_applies")
    (up / "f.txt").write_bytes(crlf.encode()); _git("commit", "-qam", "crlf", cwd=up)
    _git("pull", "-q", cwd=c)
    (up / "f.txt").write_bytes(crlf.replace("one", "ONE").encode())
    _git("commit", "-qam", "up", cwd=up); _git("fetch", "-q", cwd=c)
    (c / "f.txt").write_bytes(crlf.replace("six", "SIX").encode())
    r["crlf_applies"] = c

    # Old-Mac line endings: lone \r.
    cr = "one\rtwo\rthree\rfour\rfive\rsix\r"
    up, c = _repo(root, "cr_absorbed")
    (up / "f.txt").write_bytes(cr.encode()); _git("commit", "-qam", "cr", cwd=up)
    _git("pull", "-q", cwd=c)
    (up / "f.txt").write_bytes(cr.replace("two", "TWO").encode())
    _git("commit", "-qam", "fix", cwd=up); _git("fetch", "-q", cwd=c)
    (c / "f.txt").write_bytes(cr.replace("two", "TWO").encode())
    r["cr_absorbed"] = c

    # A latin-1 text file: the diff is not UTF-8.
    up, c = _repo(root, "latin1")
    (c / "f.txt").write_bytes(BASE.replace("two", "caf\xe9").encode("latin-1"))
    r["latin1_diff"] = c

    # A commit subject in latin-1 bytes (no encoding header) touching f.txt.
    up, c = _repo(root, "latin1msg")
    (up / "f.txt").write_text(BASE.replace("two", "UP"))
    msgf = Path(root, "msg-latin1")
    msgf.write_bytes("caf\xe9 fix\n".encode("latin-1"))
    _git("-c", "i18n.commitEncoding=UTF-8", "commit", "-qa", "-F", str(msgf), cwd=up)
    _git("fetch", "-q", cwd=c)
    (c / "f.txt").write_text(BASE.replace("two", "MINE"))
    r["latin1_subject"] = c

    # Commit subjects with characters str.splitlines() treats as line breaks.
    up, c = _repo(root, "oddsubject")
    (up / "f.txt").write_text(BASE.replace("two", "UP"))
    msgf = Path(root, "msg-odd")
    msgf.write_text("a\x0cb\x1cc\u2028d\x85e\rf\x0bg fix\n")
    _git("commit", "-qa", "-F", str(msgf), cwd=up)
    _git("fetch", "-q", cwd=c)
    (c / "f.txt").write_text(BASE.replace("two", "MINE"))
    r["odd_subject"] = c

    # --- review round 2 ---
    # A truncated 3-byte sequence mid-line: Python names the byte *range*
    # ("bytes in position 2-3"), the message the Rust only approximates.
    up, c = _repo(root, "badutf8")
    (c / "f.txt").write_bytes(BASE.replace("two", "ab").encode() .replace(b"ab", b"a\xe2\x82b"))
    r["truncated_utf8_diff"] = c

    r["empty_repo"] = Path(root, "empty")
    _git("init", "-q", str(r["empty_repo"]), cwd=root)
    r["not_git"] = Path(root, "plain")
    r["not_git"].mkdir()
    r["missing"] = Path(root, "does-not-exist")
    return r


def _states():
    files5 = ["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"]
    return {
        "clean": patches.PatchState("clean"),
        "unknown": patches.PatchState("unknown"),
        "absorbed": patches.PatchState("absorbed", ["f.txt"], ["abc fix"]),
        "applies": patches.PatchState("applies", ["f.txt", "g.txt"]),
        "conflicts": patches.PatchState("conflicts", ["f.txt"], ["abc one", "def two"]),
        "conflicts0": patches.PatchState("conflicts", ["f.txt"], []),
        "conflicts_many": patches.PatchState("conflicts", files5, ["a"]),
        "three": patches.PatchState("applies", files5[:3]),
        "four": patches.PatchState("absorbed", files5[:4]),
        "nofiles": patches.PatchState("absorbed"),
        "unicode": patches.PatchState("applies", ["ünï/çødé.txt", "日本.md"]),
        "weird": patches.PatchState("bogus", ["x"]),
        "empty_kind": patches.PatchState(""),
    }


def _dump(state):
    return {"kind": state.kind, "files": state.files,
            "upstream_commits": state.upstream_commits, "untracked": state.untracked}


def cases():
    fx = _fixtures()
    for name, path in fx.items():
        yield "analyze", {"repo": str(path)}, (lambda p=path: patches.analyze(str(p)))
    for name in ("applies", "conflicts", "many"):
        p = fx[name]
        yield "analyze", {"repo": str(p), "upstream": "origin/main"}, \
            (lambda p=p: patches.analyze(str(p), "origin/main"))
        yield "analyze", {"repo": str(p), "upstream": "HEAD"}, \
            (lambda p=p: patches.analyze(str(p), "HEAD"))
        yield "analyze", {"repo": str(p), "upstream": "no-such-ref"}, \
            (lambda p=p: patches.analyze(str(p), "no-such-ref"))
    yield "analyze", {"repo": ""}, lambda: patches.analyze("")

    # --- review round 2: a NUL in an argument makes subprocess raise
    # ValueError("embedded null byte"), which analyze() does not catch.
    yield "analyze", {"repo": "/tmp/a\x00b"}, lambda: patches.analyze("/tmp/a\x00b")
    for name in ("applies", "clean"):
        p = fx[name]
        yield "analyze", {"repo": str(p), "upstream": "origin/ma\x00in"}, \
            (lambda p=p: patches.analyze(str(p), "origin/ma\x00in"))

    # The real machine: every git checkout Almanac might update (read-only).
    real = sorted(glob.glob(os.path.expanduser("~/.config/omarchy/plugins/*/.git"))
                  + glob.glob(os.path.expanduser("~/.local/share/*/.git")))
    for g in real:
        repo = os.path.dirname(g)
        yield "analyze", {"repo": repo}, (lambda r=repo: patches.analyze(r))

    labels = ["x", "Radar", "ünïcode “quoted”", ""]
    pulls = [None, [], ["omarchy", "plugin", "update", "radar"]]
    for sname, st in _states().items():
        for label in labels:
            for pull in pulls:
                for repo in ("/tmp/repo", "/home/u/ñ dir"):
                    yield "steps", {"repo": repo, "state": _dump(st), "label": label,
                                    "pull": pull}, \
                        (lambda r=repo, s=st, l=label, p=pull: patches.steps(r, s, l, p))
        for name in ("Radar", "", "ünï “x”"):
            yield "explain", {"state": _dump(st), "name": name}, \
                (lambda s=st, n=name: patches.explain(s, n))
