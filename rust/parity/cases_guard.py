"""Parity cases for guard: the shipped-defaults lookup, the content
comparison (JSON parsed the way Python parses it), the walk, reset detection
on fixture trees and on the real machine, restore on a scratch copy, the
reload commands and the Health check row. Nothing on the machine changes:
every write goes to a temp dir named in XDG_CONFIG_HOME / XDG_STATE_HOME."""
import json
import os
import re
import shutil
import sys
import tempfile
import time
from pathlib import Path
from unittest import mock

import guard
import pystack

NOW = 1_800_000_000.0
RECENT = int(NOW) - 3600
OLD = int(NOW) - 8 * 86400


def _env(env):
    """mock.patch.dict that also unsets the keys given as None."""
    patch = mock.patch.dict(os.environ, {k: v for k, v in env.items() if v is not None})

    class _Ctx:
        def __enter__(self):
            patch.start()
            for k, v in env.items():
                if v is None:
                    os.environ.pop(k, None)

        def __exit__(self, *exc):
            patch.stop()
    return _Ctx()


def _dumps(v):
    """The exact JSON text of a result, for strs JSON from Rust cannot carry."""
    from run import normalise
    return json.dumps(normalise(v))


def _call(env, fn):
    with _env(env):
        return fn()


def _w(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    if isinstance(data, str):
        data = data.encode()
    path.write_bytes(data)


def _tree(cfg, shipped):
    """A config tree with one candidate per comparison rule."""
    D = {"bar": [{"id": "omarchy.clock"}, {"id": "omarchy.tray"}]}
    M = {"bar": [{"id": "omarchy.clock"}, {"id": "my.widget"}, {"id": "my.tray"},
                 {"id": "my.widget"}, {"nested": {"id": "deep.one", "x": [{"id": "ü-widget"}]}}]}
    files = {
        # the plain reset
        "omarchy/shell.json": (json.dumps(D, indent=2), json.dumps(D), json.dumps(M)),
        # backup is the defaults, formatted differently: not a reset
        "a/same.json": (json.dumps(D), json.dumps(D), json.dumps(D, indent=4)),
        # customised: not a reset
        "b/custom.json": (json.dumps(M), json.dumps(D), json.dumps(M)),
        # numbers as Python compares them: 1 == 1.0 == true
        "c/nums.json": ('{"a": 1, "b": [1.0], "c": true}', '{"c": 1, "b": [true], "a": 1.0}', '{"a": 2}'),
        # NaN: equal inside a list (one shared object), not at the top
        "c/nanlist.json": ("[NaN]", "[NaN]", '["x"]'),
        "c/nantop.json": ("NaN", "NaN", '"x"'),
        # duplicate keys: the last wins
        "c/dup.json": ('{"a": 1, "a": 2}', '{"a": 2}', '{"a": 1}'),
        # a BOM, and UTF-16 with and without one
        "c/bom.json": (b"\xef\xbb\xbf" + json.dumps(D).encode(), json.dumps(D), json.dumps(M)),
        "c/utf16.json": (json.dumps(D).encode("utf-16"), json.dumps(D), json.dumps(M)),
        "c/utf16le.json": (json.dumps(D).encode("utf-16-le"), json.dumps(D), json.dumps(M)),
        "c/utf32.json": (json.dumps(D).encode("utf-32"), json.dumps(D), json.dumps(M)),
        # not JSON at all although named .json: compared as stripped bytes
        "c/broken.json": ("{not json\n", "  {not json\t", '{"id": "x"}'),
        "c/trailing.json": ("[1,]", "[1,]", "[1]"),
        "c/badutf8.json": (b'{"a": "\xff"}', b'{"a": "\xff"}', b'{"a": 1}'),
        # null: Python's None, skipped like an unreadable file
        "c/null.json": ("null", "null", "[]"),
        "c/nullbak.json": ("[]", "[]", "null"),
        # big numbers and unicode
        "c/big.json": ("123456789012345678901234567890123456789012", "123456789012345678901234567890123456789012", "1"),
        "c/bigf.json": ("1e400", "Infinity", "1"),
        "c/uni.json": ('{"id": "ünï"}', '{"id": "\\u00fcn\\u00ef"}', '[{"id": "日本"}, {"id": "\\ud83d\\ude00"}]'),
        "c/escapes.json": ('"a\\/b\\n"', '"a/b\\u000a"', '"z"'),
        # plain config files: whitespace at the ends does not count
        "hypr/hyprland.conf": ("\n\nsource = x\n\x0b", "source = x", "source = mine\n"),
        "hypr/same.conf": ("x\n", "x", " x \n"),
        # a hidden file named .json has no suffix in Python
        "d/.json": ("{}", "{ }", "{}"),
        "d/x.json.": ('{"a":1}', '{"a":1}', '{"a":1.0}'),
        # four levels deep is walked, five is not
        "e/f/g/deep.json": (json.dumps(D), json.dumps(D), json.dumps(M)),
        "e/f/g/h/deeper.json": (json.dumps(D), json.dumps(D), json.dumps(M)),
        # nothing shipped for it
        "f/unshipped.json": (json.dumps(D), None, json.dumps(M)),
        # unicode file name
        "ü/ñame.json": (json.dumps(D), json.dumps(D), json.dumps(M)),
    }
    for rel, (current, stock, theirs) in files.items():
        _w(cfg / rel, current)
        if stock is not None:
            _w(shipped / "config" / rel, stock)
        _w(cfg / f"{rel}.bak.{RECENT}", theirs)
    # several backups: the newest wins
    _w(cfg / "omarchy/shell.json.bak.1799990000", json.dumps(D))
    # an old one only: let go
    _w(cfg / "g/old.json", json.dumps(D))
    _w(shipped / "config/g/old.json", json.dumps(D))
    _w(cfg / f"g/old.json.bak.{OLD}", json.dumps(M))
    # unicode digits in the epoch (Python's \d and float() take them)
    _w(cfg / "h/arabic.json", json.dumps(D))
    _w(shipped / "config/h/arabic.json", json.dumps(D))
    _w(cfg / "h/arabic.json.bak.١٧٩٩٩٩٩٩٩٩", json.dumps(M))
    # malformed backup names
    for n in ["x.json.bak.12345678", "x.json.bak.123456789012", "x.json.bak.17999999a9",
              ".bak.1799999999", "x.json.bak.1799999999.gz", "x.json.BAK.1799999999"]:
        _w(cfg / "i" / n, json.dumps(M))
    _w(cfg / "i/x.json", json.dumps(D))
    _w(shipped / "config/i/x.json", json.dumps(D))
    # a trailing newline in the name: Python's `$` allows it
    _w(cfg / "j/nl.json", json.dumps(D))
    _w(shipped / "config/j/nl.json", json.dumps(D))
    _w(cfg / f"j/nl.json.bak.{RECENT}\n", json.dumps(M))
    # the backup is a symlink to a directory / the config is a symlink
    (cfg / "k").mkdir()
    _w(cfg / "k/real.json", json.dumps(D))
    _w(shipped / "config/k/link.json", json.dumps(D))
    os.symlink(cfg / "k/real.json", cfg / "k/link.json")
    _w(cfg / f"k/link.json.bak.{RECENT}", json.dumps(M))
    os.symlink(cfg / "k", cfg / f"k/dirlink.json.bak.{RECENT}")
    # an empty backup and an empty current file
    _w(cfg / "l/empty.conf", "")
    _w(shipped / "config/l/empty.conf", "  ")
    _w(cfg / f"l/empty.conf.bak.{RECENT}", "")
    _w(cfg / "l/empty2.conf", "")
    _w(shipped / "config/l/empty2.conf", "")
    _w(cfg / f"l/empty2.conf.bak.{RECENT}", "mine")


# The app runs at Python's default recursion limit, and `_ids`' depth (which
# the Rust side models from the frames above it) depends on it. Another
# module's cases raise the limit for the whole runner process (cases_ghcache:
# 20000), so each case here runs at the default and puts it back after.
# On the main thread the wrapper is itself one frame more than the frame
# counts the cases give the Rust side, so it allows one more there; a case on
# its own thread starts a fresh frame count and gets the limit as it is.
APP_RECURSION_LIMIT = 1000


def _at_app_limit(fn, thread):
    def go():
        old = sys.getrecursionlimit()
        sys.setrecursionlimit(APP_RECURSION_LIMIT + (0 if thread else 1))
        try:
            return fn()
        finally:
            sys.setrecursionlimit(old)
    return go


def cases():
    for func, args, fn in _cases():
        yield func, args, _at_app_limit(fn, bool(args.get("thread")))


def _cases():
    real = {"XDG_CONFIG_HOME": None, "OMARCHY_PATH": None}

    # config_home / defaults_root
    yield "config_home", {"env": real}, lambda: _call(real, guard.config_home)
    for env in [{"XDG_CONFIG_HOME": "/tmp/x//y/"}, {"XDG_CONFIG_HOME": "rel/ative"},
                {"XDG_CONFIG_HOME": "/tmp/ünï"}]:
        yield "config_home", {"env": env}, (lambda e=env: _call(e, guard.config_home))
    for env in [real, {"OMARCHY_PATH": ""}, {"OMARCHY_PATH": "/nonexistent"},
                {"OMARCHY_PATH": "/usr/share/omarchy/"}, {"OMARCHY_PATH": "/usr/share//omarchy"}]:
        yield "defaults_root", {"env": env}, (lambda e=env: _call(e, guard.defaults_root))

    # Reset.name
    home = str(guard.config_home())
    for p in [home + "/omarchy/shell.json", "/elsewhere/shell.json", home, home + "/ü/x.conf", "", "rel/x"]:
        r = guard.Reset(Path(p), Path(p + ".bak.1"), 1.0, [])
        yield "name", {"env": real, "path": p, "backup": p + ".bak.1", "when": 1.0, "lost": []}, \
            (lambda r=r: _call(real, lambda: r.name))

    # the fixture tree
    root = Path(tempfile.mkdtemp(prefix="almanac-parity-guard-"))
    cfg, shipped = root / "config", root / "omarchy"
    cfg.mkdir()
    _tree(cfg, shipped)
    fx = {"XDG_CONFIG_HOME": str(cfg), "OMARCHY_PATH": str(shipped)}

    # _normal, _lost on every fixture file
    files = sorted(p for p in cfg.rglob("*") if p.is_file() and ".bak." not in p.name)
    for f in files:
        bak = f.with_name(f.name + f".bak.{RECENT}")
        if "nan" in f.name or "bigf" in f.name:
            continue        # NaN and inf do not cross JSON
        yield "normal", {"path": str(f)}, (lambda f=f: guard._normal(f))
        if bak.exists():
            yield "normal", {"path": str(bak), "like": str(f)}, (lambda f=f, b=bak: guard._normal(b, f))
            yield "lost", {"current": str(f), "backup": str(bak)}, (lambda f=f, b=bak: guard._lost(f, b))
    yield "normal", {"path": str(root / "missing.json")}, lambda: guard._normal(root / "missing.json")
    yield "normal", {"path": str(cfg)}, lambda: guard._normal(cfg)
    yield "lost", {"current": str(root / "no"), "backup": str(root / "no2")}, \
        lambda: guard._lost(root / "no", root / "no2")

    # _ids
    for blob in [{"id": "a", "z": [{"id": "b"}, {"id": 3}, {"id": None}], "y": {"id": "c", "k": {"id": "d"}}},
                 [], {}, "id", [[[{"id": "deep"}]]], {"id": {"id": "inner"}}, {"ID": "x", "id": "ü"},
                 {"b": {"id": "2"}, "a": {"id": "1"}}, None, 5]:
        yield "ids", {"blob": blob}, (lambda b=blob: guard._ids(b))

    # _walk
    for d in [0, 1, 2, 4, 5, -1]:
        yield "walk", {"root": str(cfg), "depth": d}, (lambda d=d: list(guard._walk(cfg, d)))
    yield "walk", {"root": str(root / "missing"), "depth": 4}, lambda: list(guard._walk(root / "missing", 4))
    yield "walk", {"root": str(cfg / "omarchy/shell.json"), "depth": 4}, \
        lambda: list(guard._walk(cfg / "omarchy/shell.json", 4))

    # resets on the fixture, at several clocks
    for now in [NOW, NOW + 7 * 86400 - 3600, NOW + 7 * 86400, NOW - 10 ** 9, 1.0]:
        e = dict(fx)
        yield "resets", {"env": e, "now": now}, (lambda n=now: _call(fx, lambda: guard.resets(n)))
    yield "resets", {"env": {**fx, "OMARCHY_PATH": str(root / "none")}, "now": NOW}, \
        lambda: _call({**fx, "OMARCHY_PATH": str(root / "none")}, lambda: guard.resets(NOW))
    yield "resets", {"env": {**fx, "XDG_CONFIG_HOME": str(root / "nohome")}, "now": NOW}, \
        lambda: _call({**fx, "XDG_CONFIG_HOME": str(root / "nohome")}, lambda: guard.resets(NOW))
    # the real machine as it is
    yield "resets", {"env": real}, lambda: _call(real, guard.resets)

    # finding: fixture (with the clock the files say) and the real machine
    def _finding(env):
        def go():
            f = _call(env, guard.finding)
            if callable(f.fix):
                f.fix = "<callable>"
            return f
        return go
    recent = root / "recent"
    rcfg, rshipped = recent / "config", recent / "omarchy"
    D = {"bar": [{"id": "omarchy.clock"}]}
    many = {"bar": [{"id": "omarchy.clock"}] + [{"id": f"w{i}"} for i in range(40)]}
    ts = int(time.time()) - 120
    for rel, mine in [("omarchy/shell.json", many), ("waybar/config.json", {"x": 1}),
                      ("ü/ñ.conf", None)]:
        _w(rcfg / rel, json.dumps(D) if mine is not None else "a=1")
        _w(rshipped / "config" / rel, json.dumps(D) if mine is not None else "a=1\n")
        _w(rcfg / f"{rel}.bak.{ts}", json.dumps(mine) if mine is not None else "a=2")
    renv = {"XDG_CONFIG_HOME": str(rcfg), "OMARCHY_PATH": str(rshipped)}
    yield "finding", {"env": renv}, _finding(renv)
    yield "finding", {"env": fx}, _finding(fx)
    yield "finding", {"env": real}, _finding(real)

    # restore: each side gets its own copy, since the Python side runs first
    def _restore_fixture(tag):
        base = root / f"restore-{tag}"
        c, s = base / "config", base / "omarchy"
        _w(s / "config/omarchy/shell.json", json.dumps(D))
        _w(c / "omarchy/shell.json", json.dumps(D, indent=2))
        _w(c / f"omarchy/shell.json.bak.{ts}", json.dumps(many))
        (base / "state").mkdir()
        env = {"XDG_CONFIG_HOME": str(c), "OMARCHY_PATH": str(s), "XDG_STATE_HOME": str(base / "state")}
        return env, c / "omarchy/shell.json", c / f"omarchy/shell.json.bak.{ts}"

    def _restore(env, path, bak, lost):
        def go():
            lines = []
            with _env(env):
                r = guard.Reset(path, bak, float(ts), lost)
                out = guard.restore(r, log=lambda m: lines.append(re.sub(r"\.defaults\.\d+", ".defaults.<t>", m)))
            names = sorted(re.sub(r"\.defaults\.\d+", ".defaults.<t>", n) for n in os.listdir(path.parent))
            return [out, lines, path.read_text(), names]
        return go
    py_env, py_path, py_bak = _restore_fixture("py")
    rs_env, rs_path, rs_bak = _restore_fixture("rs")
    yield "restore", {"env": rs_env, "path": str(rs_path), "backup": str(rs_bak), "when": float(ts), "lost": ["w1"]}, \
        _restore(py_env, py_path, py_bak, ["w1"])
    # a missing backup: the copy raises
    py_env2, py_path2, _ = _restore_fixture("py2")
    rs_env2, rs_path2, _ = _restore_fixture("rs2")
    yield "restore", {"env": rs_env2, "path": str(rs_path2), "backup": str(rs_path2) + ".bak.0", "when": 1.0, "lost": []}, \
        _restore(py_env2, py_path2, Path(str(py_path2) + ".bak.0"), [])

    # reload_for, with and without the restart command on PATH
    fake = root / "bin"
    fake.mkdir()
    (fake / "omarchy-restart-shell").write_text("#!/bin/sh\nexit 0\n")
    (fake / "omarchy-restart-shell").chmod(0o755)
    empty = root / "emptybin"
    empty.mkdir()
    for path in [str(fake), str(empty), os.environ.get("PATH", "")]:
        for paths in [["/c/omarchy/shell.json"], ["shell.json"], ["/c/shell.json/"], ["/c/hypr/hyprland.conf"],
                      [], ["/a/shell.json", "/b/shell.json", "x"], ["/c/Shell.json"], ["/c/shell.json.bak.1"], [""]]:
            env = {"PATH": path}
            yield "reload_for", {"env": env, "paths": paths}, \
                (lambda e=env, p=paths: _call(e, lambda: guard.reload_for(p)))

    yield from _adversarial(root, fx)
    yield from _round2(root)


def _adversarial(root, fx):
    """Review round 1: cases that probe where the port may part from Python."""
    D = {"bar": [{"id": "omarchy.clock"}]}
    M = {"bar": [{"id": "omarchy.clock"}, {"id": "my.widget"}]}

    def tree(name, files):
        base = root / f"adv-{name}"
        c, s = base / "config", base / "omarchy"
        c.mkdir(parents=True)
        for rel, (current, stock, theirs, ts) in files.items():
            _w(c / rel, current)
            if stock is not None:
                _w(s / "config" / rel, stock)
            if theirs is not None:
                _w(c / f"{rel}.bak.{ts}", theirs)
        return {"XDG_CONFIG_HOME": str(c), "OMARCHY_PATH": str(s)}, c, s

    # 1. lone surrogates: Python keeps "\ud800" and "\udbff" distinct
    env, c, _ = tree("sur", {
        "a/sur.json": ('"\\ud800"', '"\\udbff"', '"mine"', RECENT),
        "a/sur8.json": (b'"\xed\xa0\x80"', b'"\xed\xaf\xbf"', b'"mine"', RECENT),
        "a/sur16.json": ('"\ud800"'.encode("utf-16-le", "surrogatepass"),
                         '"\udbff"'.encode("utf-16-le", "surrogatepass"), '"mine"', RECENT),
        "a/ids.json": ('[{"id": "\\ud800"}]', '[{"id": "\\ud800"}]', '[{"id": "\\udbff"}]', RECENT),
    })
    # (a lone surrogate cannot cross the JSON the Rust side answers in, so
    # these compare the exact text json.dumps gives for the result)
    yield "resets_json", {"env": env, "now": NOW}, \
        (lambda e=env: _dumps(_call(e, lambda: guard.resets(NOW))))
    yield "lost_json", {"current": str(c / "a/ids.json"), "backup": str(c / f"a/ids.json.bak.{RECENT}")}, \
        (lambda c=c: _dumps(guard._lost(c / "a/ids.json", c / f"a/ids.json.bak.{RECENT}")))

    # 2. nesting Python's json parses (it goes to ~50000 deep), Rust stops at 900
    env, c, _ = tree("deep", {
        "a/d950.json": ("[" * 950 + "]" * 950, "[ " * 950 + "]" * 950, "[1]", RECENT),
        "a/d3000.json": ("[" * 3000 + "]" * 3000, "[ " * 3000 + "]" * 3000, "[1]", RECENT),
    })
    yield "resets", {"env": env, "now": NOW}, (lambda e=env: _call(e, lambda: guard.resets(NOW)))
    # 950 deep, the backup the defaults reformatted: Python parses all three
    # and sees no reset; Rust falls back to raw bytes and reports one
    env, c, _ = tree("deep2", {
        "a/d950.json": ("[" * 950 + "]" * 950, "[" * 950 + "]" * 950, "[ " * 950 + "]" * 950, RECENT),
    })
    yield "resets", {"env": env, "now": NOW}, (lambda e=env: _call(e, lambda: guard.resets(NOW)))
    # deeper than Python can: RecursionError is not a ValueError, so it escapes
    env, c, _ = tree("deepraise", {
        "a/huge.json": ("[" * 100000 + "]" * 100000, "[]", "[1]", RECENT),
    })
    yield "normal", {"path": str(c / "a/huge.json")}, (lambda c=c: guard._normal(c / "a/huge.json"))
    yield "resets", {"env": env, "now": NOW}, (lambda e=env: _call(e, lambda: guard.resets(NOW)))

    # 3. a backup named `..bak.<epoch>`: Python's with_name('.') raises out of resets
    env, c, _ = tree("dot", {"a/x.json": (json.dumps(D), json.dumps(D), json.dumps(M), RECENT)})
    _w(c / f"a/..bak.{RECENT}", "x")
    yield "resets", {"env": env, "now": NOW}, (lambda e=env: _call(e, lambda: guard.resets(NOW)))

    # 4. Reset.name with a `//` root, which pathlib keeps distinct from `/`
    for home, p in [("/tmp/cfg", "//tmp/cfg/omarchy/shell.json"),
                    ("//tmp/cfg", "/tmp/cfg/omarchy/shell.json"),
                    ("//tmp/cfg", "//tmp/cfg/omarchy/shell.json")]:
        e = {"XDG_CONFIG_HOME": home}
        r = guard.Reset(Path(p), Path(p + ".bak.1"), 1.0, [])
        yield "name", {"env": e, "path": p, "backup": p + ".bak.1", "when": 1.0, "lost": []}, \
            (lambda r=r, e=e: _call(e, lambda: r.name))

    # 5. which(): os.access(X_OK) is about *our* execute bit, not anyone's
    for mode in (0o010, 0o001, 0o644):
        d = root / f"adv-bin-{mode:o}"
        d.mkdir()
        (d / "omarchy-restart-shell").write_text("#!/bin/sh\n")
        (d / "omarchy-restart-shell").chmod(mode)
        env = {"PATH": str(d)}
        yield "reload_for", {"env": env, "paths": ["/c/shell.json"]}, \
            (lambda e=env: _call(e, lambda: guard.reload_for(["/c/shell.json"])))
    # PATH unset: shutil.which falls back to os.confstr("CS_PATH")
    yield "reload_for", {"env": {"PATH": None}, "paths": ["/c/shell.json"]}, \
        (lambda: _call({"PATH": None}, lambda: guard.reload_for(["/c/shell.json"])))

    # 6. restore when the backup is read-only: copy2 sets times by path
    ts = int(time.time()) - 120

    def ro_fixture(tag):
        base = root / f"adv-ro-{tag}"
        c, s = base / "config", base / "omarchy"
        _w(s / "config/omarchy/shell.json", json.dumps(D))
        _w(c / "omarchy/shell.json", json.dumps(D))
        bak = c / f"omarchy/shell.json.bak.{ts}"
        _w(bak, json.dumps(M))
        bak.chmod(0o444)
        (base / "state").mkdir()
        env = {"XDG_CONFIG_HOME": str(c), "OMARCHY_PATH": str(s), "XDG_STATE_HOME": str(base / "state")}
        return env, c / "omarchy/shell.json", bak

    def ro_go(env, path, bak):
        def go():
            lines = []
            with _env(env):
                out = guard.restore(guard.Reset(path, bak, float(ts), []),
                                    log=lambda m: lines.append(re.sub(r"\.defaults\.\d+", ".defaults.<t>", m)))
            names = sorted(re.sub(r"\.defaults\.\d+", ".defaults.<t>", n) for n in os.listdir(path.parent))
            return [out, lines, path.read_text(), names]
        return go
    pe, pp, pb = ro_fixture("py")
    re_, rp, rb = ro_fixture("rs")
    yield "restore", {"env": re_, "path": str(rp), "backup": str(rb), "when": float(ts), "lost": []}, \
        ro_go(pe, pp, pb)

    # 7. a file name that is not UTF-8 (Python: surrogateescape), and its sort
    # order against a character above U+DCxx
    base = root / "adv-bytes"
    c, s = base / "config", base / "omarchy"
    for d in (b"\xff", "豈".encode()):
        cb, sb = os.fsencode(c) + b"/" + d, os.fsencode(s) + b"/config/" + d
        os.makedirs(cb, exist_ok=True)
        os.makedirs(sb, exist_ok=True)
        for p, data in [(cb + b"/a.json", json.dumps(D)), (sb + b"/a.json", json.dumps(D)),
                        (cb + f"/a.json.bak.{RECENT}".encode(), json.dumps(M))]:
            with open(p, "w") as fh:
                fh.write(data)
    env = {"XDG_CONFIG_HOME": str(c), "OMARCHY_PATH": str(s)}
    # (exact text, as above: the names carry surrogateescape'd bytes)
    yield "resets_json", {"env": env, "now": NOW}, \
        (lambda e=env: _dumps(_call(e, lambda: guard.resets(NOW))))
    yield "walk_json", {"root": str(c), "depth": 4}, (lambda c=c: _dumps(list(guard._walk(c, 4))))

    # 8. epochs in digits Unicode 16 added (Garay, Kirat Rai, Outlined)
    env, c, _ = tree("u16", {"a/x.json": (json.dumps(D), json.dumps(D), None, RECENT),
                             "b/x.json": (json.dumps(D), json.dumps(D), None, RECENT),
                             "c/x.json": (json.dumps(D), json.dumps(D), None, RECENT)})
    for sub, zero in [("a", 0x10D40), ("b", 0x16D70), ("c", 0x1CCF0)]:
        digits = "".join(chr(zero + int(ch)) for ch in str(RECENT))
        _w(c / sub / f"x.json.bak.{digits}", json.dumps(M))
    yield "resets", {"env": env, "now": NOW}, (lambda e=env: _call(e, lambda: guard.resets(NOW)))


def _round2(root):
    """Review round 2: error texts, recursion limits under other call stacks,
    time zones in the Health row."""
    import resource
    import threading
    import maintenance
    D = {"bar": [{"id": "omarchy.clock"}]}
    M = {"bar": [{"id": "omarchy.clock"}, {"id": "my.widget"}]}

    def tree(name, files):
        base = root / f"r2-{name}"
        c, s = base / "config", base / "omarchy"
        c.mkdir(parents=True)
        for rel, (current, stock, theirs, ts) in files.items():
            _w(c / rel, current)
            if stock is not None:
                _w(s / "config" / rel, stock)
            if theirs is not None:
                _w(c / f"{rel}.bak.{ts}", theirs)
        return {"XDG_CONFIG_HOME": str(c), "OMARCHY_PATH": str(s)}, c, s

    def caught(fn):
        """The exception's text, as the Rust side reports it."""
        def go():
            try:
                return fn()
            except Exception as exc:             # noqa: BLE001
                return {"error": str(exc)}
        return go

    def in_thread(fn):
        """As the app runs it: maintenance.run_all and guard.resets are called
        from a worker threading.Thread."""
        def go():
            box = {}

            def work():
                try:
                    box["v"] = fn()
                except BaseException as exc:      # noqa: BLE001
                    box["e"] = exc
            t = threading.Thread(target=work)
            t.start()
            t.join()
            if "e" in box:
                raise box["e"]
            return box["v"]
        return go

    # 1. restore's error texts. A backup that is the config itself: shutil's
    # SameFileError names both as PosixPath(...), not as bare strings.
    ts = int(time.time()) - 120
    base = root / "r2-same"
    c = base / "config"
    _w(c / "omarchy/shell.json", json.dumps(D))
    (base / "state").mkdir()
    env = {"XDG_CONFIG_HOME": str(c), "XDG_STATE_HOME": str(base / "state")}
    p = c / "omarchy/shell.json"
    yield "restore", {"env": env, "path": str(p), "backup": str(p), "when": float(ts), "lost": []}, \
        caught(lambda: _call(env, lambda: guard.restore(guard.Reset(p, p, float(ts), []), log=lambda m: None)))
    # the config itself gone: open() names the path as a str
    q = c / "omarchy/gone.json"
    yield "restore", {"env": env, "path": str(q), "backup": str(p), "when": float(ts), "lost": []}, \
        caught(lambda: _call(env, lambda: guard.restore(guard.Reset(q, p, float(ts), []), log=lambda m: None)))

    # a named pipe as the backup or as the config: copyfile refuses it before
    # opening anything (opening one would wait for a writer)
    fifo = c / "omarchy/pipe.json.bak.1"
    os.mkfifo(fifo)
    yield "restore", {"env": env, "path": str(p), "backup": str(fifo), "when": float(ts), "lost": []}, \
        caught(lambda: _call(env, lambda: guard.restore(guard.Reset(p, fifo, float(ts), []), log=lambda m: None)))
    fifo2 = c / "omarchy/pipe.json"
    os.mkfifo(fifo2)
    yield "restore", {"env": env, "path": str(fifo2), "backup": str(p), "when": float(ts), "lost": []}, \
        caught(lambda: _call(env, lambda: guard.restore(guard.Reset(fifo2, p, float(ts), []), log=lambda m: None)))

    # 2. `_ids` recursion: Python's limit counts the caller's frames, so the
    # depth that fails depends on who asked. The Health check goes through
    # maintenance.config_resets (one frame more than guard.finding). The Rust
    # side is told where the call comes from: `frames` above the function
    # (run.py's main thread: <module>, main, run_module, the case's lambda,
    # _call, lambda -> 6 above resets) and `thread`.
    for n in (988, 989, 990, 991, 992):
        env, c, _ = tree(f"ids{n}", {"a/x.json": ("[" * n + "]" * n, "[ " * n + "]" * n, "[1]", RECENT)})
        yield "resets", {"env": env, "now": NOW}, (lambda e=env: _call(e, lambda: guard.resets(NOW)))

        def via_maintenance(e=env):
            f = _call(e, maintenance.config_resets)
            f.fix = "<callable>" if callable(f.fix) else f.fix
            return f
        yield "finding", {"env": env, "frames": 6}, via_maintenance

        def via_thread(e=env):
            return in_thread(lambda: _call(e, lambda: guard.resets(NOW)))()
        # _bootstrap, _bootstrap_inner, run, work, the lambda, _call, lambda
        yield "resets", {"env": env, "now": NOW, "frames": 7, "thread": True}, via_thread

    # 3. json's nesting limit is the C stack left, which differs between the
    # main thread and a worker thread (where the app parses). Not a reset
    # (their version is the defaults reformatted), so only the parse decides.
    # A worker thread's stack is a fresh RLIMIT_STACK, so its limit is exact
    # for one process (52154 under 8 MB here; see pystack for why another
    # machine's differs by a level). The main thread's starts at an address
    # ASLR moves, so Python's own limit there varies run to run
    # (~52100..52200): only depths clear of that band are deterministic.
    for n in (*pystack.thread_depths(), 51500, 52700):
        deep, spaced = "[" * n + "]" * n, "[ " * n + "]" * n
        env, c, _ = tree(f"json{n}", {"a/x.json": (deep, spaced, spaced, RECENT)})
        if n in (51500, 52700):
            yield "resets", {"env": env, "now": NOW}, (lambda e=env: _call(e, lambda: guard.resets(NOW)))
        yield "resets", {"env": env, "now": NOW, "frames": 7, "thread": True}, \
            in_thread(lambda e=env: _call(e, lambda: guard.resets(NOW)))

    # 4. the Health row's "%a %H:%M" under various time zones
    env0, c, _ = tree("tz", {"omarchy/shell.json": (json.dumps(D), json.dumps(D), json.dumps(M), ts)})
    for tz in ["UTC", "Asia/Kolkata", "America/St_Johns", "<+0530>-5:30", "EST5EDT,M3.2.0,M11.1.0",
               "Nowhere/Fake", "", ":UTC", "UTC+3", "/etc/localtime", "Etc/GMT-14"]:
        env = {**env0, "TZ": tz}

        def go(e=env):
            # time.localtime reads TZ once; tzset() makes it read the new one,
            # as a process started with that TZ would.
            def run():
                time.tzset()
                return guard.finding()
            try:
                f = _call(e, run)
            finally:
                time.tzset()
            f.fix = "<callable>" if callable(f.fix) else f.fix
            return f
        yield "finding", {"env": env}, go
