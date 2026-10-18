"""Parity cases for autostart.py.

`env` in the args is applied on both sides for the duration of the call
(None removes the variable). Every case that reads or writes files gets its
own temp root, described in `fixture` = {"root": dir, "files": {relpath:
text | [bytes] | None (a directory) | {"link": target}}}; each side wipes the
root and rebuilds it from that description just before the call, so the
Python run (which goes first) cannot leak into the Rust one. XDG_CONFIG_HOME
always points into the temp root for anything that writes -- the real
machine is only ever read (user_dir, system_dirs, entries with no override).
"""
import atexit
import dataclasses
import os
import shutil
import tempfile
from pathlib import Path

import autostart

_ROOT = Path(tempfile.mkdtemp(prefix="almanac-parity-autostart-"))
atexit.register(shutil.rmtree, _ROOT, True)
_n = 0


def _root():
    global _n
    _n += 1
    return str(_ROOT / str(_n))


def _build(fixture):
    if not fixture:
        return
    root = Path(fixture["root"])
    shutil.rmtree(root, ignore_errors=True)
    root.mkdir(parents=True)
    for rel, v in fixture.get("files", {}).items():
        p = root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        if v is None:
            p.mkdir(parents=True, exist_ok=True)
        elif isinstance(v, str):
            p.write_bytes(v.encode())
        elif isinstance(v, list):
            p.write_bytes(bytes(v))
        elif isinstance(v, dict):
            os.symlink(v["link"], p)


def _tree(fixture):
    root = Path(fixture["root"])
    out = {}

    def walk(d):
        for p in sorted(d.iterdir()):
            rel = str(p.relative_to(root))
            if p.is_symlink():
                out[rel] = "-> " + os.readlink(p)
            elif p.is_dir():
                out[rel] = "<dir>"
                walk(p)
            else:
                out[rel] = p.read_bytes().decode("utf-8", "replace")
    walk(root)
    return out


def _call(env, fixture, fn):
    def run():
        saved = {k: os.environ.get(k) for k in env}
        try:
            for k, v in env.items():
                if v is None:
                    os.environ.pop(k, None)
                else:
                    os.environ[k] = v
            _build(fixture)
            return fn()
        finally:
            for k, v in saved.items():
                if v is None:
                    os.environ.pop(k, None)
                else:
                    os.environ[k] = v
    return run


def _case(func, fn, env=None, fixture=None, **kw):
    env = env or {}
    args = {"env": env, **kw}
    if fixture:
        args["fixture"] = fixture
    return func, args, _call(env, fixture, fn)


def _env(root, desktop="Hyprland", dirs=None):
    return {"XDG_CONFIG_HOME": root + "/home",
            "XDG_CONFIG_DIRS": dirs if dirs is not None else root + "/etc/xdg",
            "XDG_CURRENT_DESKTOP": desktop}


def _lookup(name):
    for e in autostart.entries():
        if _esc(e.filename) == name:
            return e
    raise LookupError(f"no entry {name}")



def _esc(v):
    """Names that are not UTF-8, the way the Rust side can show them: the
    surrogate-escaped bytes as `\\xNN` (JSON cannot carry lone surrogates)."""
    if dataclasses.is_dataclass(v) and not isinstance(v, type):
        return {f.name: _esc(getattr(v, f.name)) for f in dataclasses.fields(v)}
    if isinstance(v, Path):
        v = str(v)
    if isinstance(v, str):
        return v.encode("utf-8", "surrogateescape").decode("utf-8", "backslashreplace")
    if isinstance(v, dict):
        return {_esc(k): _esc(x) for k, x in v.items()}
    if isinstance(v, (list, tuple)):
        return [_esc(x) for x in v]
    return v


def _esc_fn(fn):
    return lambda: _esc(fn())


def _steps_fn(fixture, steps, after):
    def fn():
        for what, on in steps:
            e = _lookup(what) if isinstance(what, str) else autostart.Entry(
                what["filename"], what["name"], what["comment"], what["icon"],
                what["enabled"], Path(what["path"]),
                None if what["system"] is None else Path(what["system"]), what["ours"])
            autostart.set_enabled(e, on)
        out = {"result": None, "tree": _tree(fixture)}
        if after:
            out["entries"] = autostart.entries()
        return out
    return fn


KEYS = "[Desktop Entry]\nType=Application\nName=Keys\nExec=keys\n"
GNOME = "[Desktop Entry]\nType=Application\nName=G\nExec=g\nOnlyShowIn=GNOME;\n"

PARSE_TEXTS = [
    KEYS,
    "",
    "\n\n\n",
    "Name=outside any group\n[Desktop Entry]\nName=Inside\n",
    "[Desktop Entry]\nName=First\nName=Second\n",
    "[Desktop Entry]\nName[de]=Schlüssel\nName=Keys\nComment[fr]=x\n",
    "[Desktop Entry]\n# Name=commented\n  #Icon=also\nIcon=real\n",
    "[Desktop Entry]\n  Name  =  Spaced Out  \n\tExec\t=\tx y \t\n",
    "[Desktop Entry]\nExec=a=b=c\nEmpty=\n=novalue\nnoequals\n",
    "[Desktop Entry]\nName=A\n[Desktop Action new]\nName=Action\nExec=b\n",
    "[Other]\nName=Other\n[Desktop Entry]\nName=Later\n[Desktop Entry]\nIcon=again\n",
    "[desktop entry]\nName=wrong case\n",
    " [Desktop Entry] \nName=indented header\n",
    "[Desktop Entry]\r\nName=CRLF\r\nComment=dos\r\n",
    "[Desktop Entry]\rName=CR only\rIcon=mac\r",
    "[Desktop Entry]\nName=Ünïcødé 日本語 🎉\nComment=Ελληνικά\n",
    "[Desktop Entry]\nName=line\x0bsplit\nIcon=form\x0cfeed\n",
    "[Desktop Entry]\nName=uni\u2028sep\nComment=para\u2029graph\nIcon=nel\x85x\n",
    "[Desktop Entry]\nName=\x1fstripped\x1f\nComment=\u3000ideo\u3000\nIcon=\xa0nb\xa0\n",
    "[Desktop Entry]\nName=fs\x1cgs\x1drs\x1eend\n",
    "\ufeff[Desktop Entry]\nName=BOM\n",
    "[Desktop Entry]\nName=no trailing newline",
    "[Desktop Entry]\nHidden=TRUE\nX-GNOME-Autostart-enabled=False\nX-Almanac-Override=True\n",
]
PARSE_BYTES = [
    b"[Desktop Entry]\nName=bad \xff\xfe bytes\nIcon=\xc3\n",
    b"[Desktop Entry]\nName=\xe6\x97\xa5\xe6\x9c\nComment=\xed\xa0\x80surrogate\n",
    b"[Desktop Entry]\nName=nul\x00inside\n",
    b"\xf0\x9f\x8e[Desktop Entry]\nName=x\n",
]

DICTS = [
    {}, {"OnlyShowIn": "GNOME;"}, {"OnlyShowIn": "Hyprland;"}, {"OnlyShowIn": "GNOME;KDE;"},
    {"OnlyShowIn": ";;"}, {"NotShowIn": "Hyprland;"}, {"NotShowIn": "GNOME;"},
    {"OnlyShowIn": "Hyprland", "NotShowIn": "Hyprland"}, {"OnlyShowIn": "hyprland;"},
    {"OnlyShowIn": "GNOME;", "NotShowIn": "KDE;"}, {"OnlyShowIn": "ünï;"},
    {"OnlyShowIn": " Hyprland;"}, {"NotShowIn": ""},
]
ON_DICTS = [
    {}, {"Hidden": "true"}, {"Hidden": "TRUE"}, {"Hidden": "false"}, {"Hidden": "yes"},
    {"Hidden": "True "}, {"Hidden": ""}, {"X-GNOME-Autostart-enabled": "false"},
    {"X-GNOME-Autostart-enabled": "FALSE"}, {"X-GNOME-Autostart-enabled": "0"},
    {"X-GNOME-Autostart-enabled": "true"}, {"Hidden": "false", "X-GNOME-Autostart-enabled": "False"},
    {"Hidden": "true", "X-GNOME-Autostart-enabled": "true"}, {"Hidden": "TRÜE"},
]


def cases():
    # The real machine, read only.
    yield "user_dir", {}, autostart.user_dir
    yield "system_dirs", {}, autostart.system_dirs
    yield "entries", {}, autostart.entries
    for desk in ["GNOME", "KDE", "", None, "Hyprland:GNOME"]:
        env = {"XDG_CURRENT_DESKTOP": desk}
        yield _case("entries", autostart.entries, env)

    for v in [None, "", "/srv/cfg", "relative", "/tmp/ünï cødé", "/a/../b/"]:
        env = {"XDG_CONFIG_HOME": v, "HOME": "/home/someone"}
        yield _case("user_dir", autostart.user_dir, env)
    for v in [None, "", "/etc/xdg", "/a:/b", "/a::/b:", ":", "rel:/abs", "/ünï:/日本",
              "/a/:/b//", " /space", "//net:/./x/.:///t:./rel:.", "~/x"]:
        env = {"XDG_CONFIG_DIRS": v}
        yield _case("system_dirs", autostart.system_dirs, env)

    # _parse
    for text in PARSE_TEXTS + PARSE_BYTES:
        r = _root()
        fx = {"root": r, "files": {"x.desktop": list(text) if isinstance(text, bytes) else text}}
        p = r + "/x.desktop"
        yield _case("_parse", lambda p=p: autostart._parse(Path(p)), fixture=fx, path=p)
    r = _root()
    fx = {"root": r, "files": {"dir.desktop": None, "dangling.desktop": {"link": r + "/nowhere"}}}
    for p in [r + "/missing.desktop", r + "/dir.desktop", r + "/dangling.desktop", ""]:
        yield _case("_parse", lambda p=p: autostart._parse(Path(p)), fixture=fx, path=p)

    # _applies / _on
    for desk in ["Hyprland", "GNOME", "GNOME:KDE", "", None, "Hyprland::", "ünï"]:
        env = {"XDG_CURRENT_DESKTOP": desk}
        for d in DICTS:
            yield _case("_applies", lambda d=d: autostart._applies(d), env, d=d)
    for d in ON_DICTS:
        yield _case("_on", lambda d=d: autostart._on(d), d=d)

    # _set_key
    set_texts = [
        KEYS, "", "[Desktop Entry]\n", "Name=no group\n",
        "[Desktop Entry]\nHidden=false\nName=K\nHidden=false\n",
        "[Desktop Entry]\nName=K\n[Desktop Action x]\nHidden=keep\n",
        "[Other]\nHidden=keep\n[Desktop Entry]\nName=K\n",
        "[Desktop Entry]\n  Hidden  = false \nName=K\n",
        "[Desktop Entry]\nHidden[de]=x\nName=K\n",
        "[Desktop Entry]\r\nName=CRLF\r\nHidden=false\r\n",
        "[Desktop Entry]\nName=Ünïcødé 日本\n# comment\n\n",
        "[Desktop Entry]\nName=no newline",
        "[Desktop Entry]\nName=K\n[Desktop Entry]\nHidden=dupe group\n",
    ]
    for text in set_texts + [b"[Desktop Entry]\nName=\xff\xfe\n"]:
        for key, value in [("Hidden", "true"), (autostart.MARK, "true"), ("Name", "Ü 新")]:
            r = _root()
            fx = {"root": r, "files": {"a.desktop": list(text) if isinstance(text, bytes) else text}}
            p = r + "/a.desktop"
            yield _case("_set_key", (lambda p=p, k=key, v=value, fx=fx:
                                     {"result": autostart._set_key(Path(p), k, v), "tree": _tree(fx)}),
                        fixture=fx, path=p, key=key, value=value)
    for name in ["plain", "two.dots.desktop", ".hidden"]:
        r = _root()
        fx = {"root": r, "files": {name: KEYS}}
        p = r + "/" + name
        yield _case("_set_key", (lambda p=p, fx=fx:
                                 {"result": autostart._set_key(Path(p), "Hidden", "true"), "tree": _tree(fx)}),
                    fixture=fx, path=p, key="Hidden", value="true")
    r = _root()
    fx = {"root": r, "files": {}}
    yield _case("_set_key", lambda p=r + "/missing.desktop": autostart._set_key(Path(p), "Hidden", "true"),
                fixture=fx, path=r + "/missing.desktop", key="Hidden", value="true")

    # entries over fixture trees
    scenes = [
        {"etc/xdg/autostart/keys.desktop": KEYS, "etc/xdg/autostart/gnome-only.desktop": GNOME},
        {},
        {"etc/xdg/autostart": None},
        {"etc/xdg/autostart/b.desktop": "[Desktop Entry]\nName=beta\n",
         "etc/xdg/autostart/a.desktop": "[Desktop Entry]\nName=Alpha\n",
         "etc/xdg/autostart/c.desktop": "[Desktop Entry]\nName=alpha\n",
         "etc/xdg/autostart/Z.desktop": "[Desktop Entry]\nName=Émile\n",
         "etc/xdg/autostart/noname.desktop": "[Desktop Entry]\nExec=x\n",
         "etc/xdg/autostart/emptyname.desktop": "[Desktop Entry]\nName=\nComment=c\n",
         "etc/xdg/autostart/README": "not a desktop file",
         "etc/xdg/autostart/x.DESKTOP": KEYS,
         "etc/xdg/autostart/.desktop": KEYS,
         "etc/xdg/autostart/.hidden.desktop": KEYS},
        {"etc/xdg/autostart/keys.desktop": KEYS,
         "home/autostart/keys.desktop": "[Desktop Entry]\nHidden=true\nX-Almanac-Override=true\n"},
        {"etc/xdg/autostart/keys.desktop": "[Desktop Entry]\nName=Sys\nComment=sys c\nIcon=sys-i\n",
         "home/autostart/keys.desktop": "[Desktop Entry]\nName=User\nComment=user c\nIcon=user-i\nType=Link\n"},
        {"etc/xdg/autostart/keys.desktop": "[Desktop Entry]\nName=Sys\n",
         "home/autostart/keys.desktop": "[Desktop Entry]\nName=User\nComment=only user\nX-GNOME-Autostart-enabled=false\n"},
        {"etc/xdg/autostart/g.desktop": GNOME,
         "home/autostart/g.desktop": "[Desktop Entry]\nName=Mine\nOnlyShowIn=Hyprland;\n"},
        {"home/autostart/mine.desktop": "[Desktop Entry]\nName=Mine\nOnlyShowIn=GNOME;\n",
         "home/autostart/two.desktop": "[Desktop Entry]\nName=Two\nNotShowIn=Hyprland;\n",
         "home/autostart/svc.desktop": "[Desktop Entry]\nType=Service\nName=S\n",
         "home/autostart/notype.desktop": "Name=no group at all\n"},
        {"etc/xdg/autostart/io.github.twodxme.Almanac.desktop": KEYS,
         "home/autostart/io.github.twodxme.Almanac-guard.desktop": KEYS,
         "home/autostart/io.github.twodxme.desktop": KEYS},
        {"etc/xdg/autostart/dir.desktop": None,
         "etc/xdg/autostart/dangling.desktop": {"link": "/nonexistent/x"},
         "etc/xdg/autostart/linked.desktop": {"link": "keys.real"},
         "etc/xdg/autostart/keys.real": KEYS,
         "etc/xdg/autostart/bad.desktop": list(b"[Desktop Entry]\nName=\xff\xfebad\n"),
         "home/autostart/ünïcødé 日本.desktop": "[Desktop Entry]\nName=Ünï 🎉\n"},
        {"home/autostart": "a file, not a dir"},
    ]
    for files in scenes:
        for desk in ["Hyprland", "GNOME", ""]:
            r = _root()
            yield _case("entries", autostart.entries, _env(r, desk), {"root": r, "files": files})
    # several system dirs: earlier ones take precedence
    r = _root()
    files = {"one/autostart/a.desktop": "[Desktop Entry]\nName=from one\n",
             "two/autostart/a.desktop": "[Desktop Entry]\nName=from two\n",
             "two/autostart/b.desktop": "[Desktop Entry]\nName=only two\n"}
    for dirs in [f"{r}/one:{r}/two", f"{r}/two:{r}/one", f"{r}/two::{r}/missing:", ""]:
        r2 = _root()
        f2 = {k: v for k, v in files.items()}
        d2 = dirs.replace(r, r2)
        yield _case("entries", autostart.entries, _env(r2, dirs=d2), {"root": r2, "files": f2})

    # set_enabled: the StartupApps scenarios and the edges around them
    sys_keys = {"etc/xdg/autostart/keys.desktop": KEYS, "etc/xdg/autostart/gnome-only.desktop": GNOME}
    mine = "[Desktop Entry]\nType=Application\nName=Mine\nExec=m\n"
    flows = [
        (sys_keys, [["keys.desktop", False]]),
        (sys_keys, [["keys.desktop", False], ["keys.desktop", True]]),
        (sys_keys, [["keys.desktop", True]]),
        (sys_keys, [["keys.desktop", False], ["keys.desktop", False]]),
        ({**sys_keys, "home/autostart/mine.desktop": mine}, [["mine.desktop", False]]),
        ({**sys_keys, "home/autostart/mine.desktop": mine},
         [["mine.desktop", False], ["mine.desktop", True]]),
        ({**sys_keys, "home/autostart/keys.desktop": "[Desktop Entry]\nName=K\nHidden=true\n"},
         [["keys.desktop", True]]),
        ({**sys_keys, "home/autostart/keys.desktop":
          "[Desktop Entry]\nName=K\nX-GNOME-Autostart-enabled=false\n[Desktop Action a]\nExec=z\n"},
         [["keys.desktop", True], ["keys.desktop", False]]),
        ({"etc/xdg/autostart/u.desktop":
          list("[Desktop Entry]\nName=Ünï 日本\r\nComment=c\n".encode() + b"Icon=\xff\n")},
         [["u.desktop", False], ["u.desktop", True]]),
        ({"etc/xdg/autostart/x.desktop": "[Desktop Entry]\nName=X\nX-Almanac-Override=TRUE\n"},
         [["x.desktop", False]]),
        ({**sys_keys, "home": "a file where the config home should be"}, [["keys.desktop", False]]),
    ]
    for files, steps in flows:
        r = _root()
        fx = {"root": r, "files": files}
        yield _case("set_enabled", _steps_fn(fx, steps, True), _env(r), fx, steps=steps, after=True)
    # explicit entries, so these run in demo mode too
    r = _root()
    base = {"filename": "keys.desktop", "name": "Keys", "comment": "", "icon": "",
            "enabled": True, "path": r + "/etc/xdg/autostart/keys.desktop",
            "system": r + "/etc/xdg/autostart/keys.desktop", "ours": False}
    explicit = [
        [[base, False]],
        [[base, False], [base, True]],
        [[{**base, "system": None}, False]],
        [[{**base, "system": r + "/etc/xdg/autostart/missing.desktop"}, False]],
        [[{**base, "system": None, "path": r + "/nowhere.desktop"}, False]],
        [[{**base, "filename": "ünï 日本.desktop"}, False]],
        [[{**base, "system": r + "/etc/xdg/autostart"}, False]],
    ]
    for steps in explicit:
        r2 = _root()
        st = [[{k: (v.replace(r, r2) if isinstance(v, str) else v) for k, v in e.items()}, on]
              for e, on in steps]
        fx = {"root": r2, "files": dict(sys_keys)}
        yield _case("set_enabled", _steps_fn(fx, st, False), _env(r2), fx, steps=st, after=False)

    # Review round 1: differences found by the adversarial review.
    # 1. `path.with_suffix(".tmp")` vs `Path::with_extension("tmp")` on names
    #    with leading dots: Python keeps leading dots in the stem ("..desktop"
    #    -> "..desktop.tmp"), Rust splits at the last dot ("..desktop" ->
    #    ".." -- the parent directory, so the write fails).
    for name, extra in [("..desktop", {}), ("...desktop", {"...tmp": "keep me\n"}),
                        ("..x", {})]:
        r = _root()
        fx = {"root": r, "files": {name: KEYS, **extra}}
        p = r + "/" + name
        yield _case("_set_key", (lambda p=p, fx=fx:
                                 {"result": autostart._set_key(Path(p), "Hidden", "true"), "tree": _tree(fx)}),
                    fixture=fx, path=p, key="Hidden", value="true")
    r = _root()
    fx = {"root": r, "files": {"etc/xdg/autostart/..desktop": KEYS}}
    steps = [["..desktop", False]]
    yield _case("set_enabled", _steps_fn(fx, steps, True), _env(r), fx, steps=steps, after=True)
    # 2. Sorting by `name.lower()`: Python 3.14 has Unicode 16, Rust 1.98 has
    #    Unicode 17, where U+A7D2 gained a lowercase (U+A7D3). Python orders
    #    b (A7D2) before a (A7D3); Rust sees a tie and keeps filename order.
    for names in [("ꟓ", "꟒"), ("꟏", "꟎"), ("\U00016ebb", "\U00016ea0")]:
        r = _root()
        files = {"etc/xdg/autostart/a.desktop": f"[Desktop Entry]\nName={names[0]}\n",
                 "etc/xdg/autostart/b.desktop": f"[Desktop Entry]\nName={names[1]}\n"}
        yield _case("entries", autostart.entries, _env(r), {"root": r, "files": files})
    # 3. A file name that is not UTF-8: Python keeps it (surrogateescape),
    #    Rust's `filename: String` is lossy, so the override it writes has a
    #    different name from the system file. The system dir lives outside the
    #    fixture root (fixtures are JSON, which cannot carry such a name), so
    #    both sides read the same one.
    sysd = _ROOT / "nonutf8" / "autostart"
    sysd.mkdir(parents=True, exist_ok=True)
    (Path(os.fsdecode(bytes(sysd))) / os.fsdecode(b"caf\xe9.desktop")).write_text(KEYS)
    r = _root()
    yield _case("entries", _esc_fn(autostart.entries), _env(r, dirs=str(_ROOT / "nonutf8")),
                {"root": r, "files": {}})
    # Switching it off writes the override under the very same bytes, and
    # entries() then sees it off; on again removes it.
    for steps in [[["caf\\xe9.desktop", False]],
                  [["caf\\xe9.desktop", False], ["caf\\xe9.desktop", True]]]:
        r = _root()
        fx = {"root": r, "files": {}}
        yield _case("set_enabled", _esc_fn(_steps_fn(fx, steps, True)),
                    _env(r, dirs=str(_ROOT / "nonutf8")), fx, steps=steps, after=True)
    # No Name: the display name is the file name itself, and it sorts by its
    # code points (U+DCxx for the stray bytes) against names around it.
    sysd = _ROOT / "nonutf8b" / "autostart"
    sysd.mkdir(parents=True, exist_ok=True)
    for raw, name in [(b"\xe9t\xe9.desktop", None), (b"z.desktop", "\ud7fb"),
                      (b"y.desktop", "\ue000"), (b"x.desktop", "caf"), (b"w.desktop", "\u00e9")]:
        body = "[Desktop Entry]\nExec=x\n" + (f"Name={name}\n" if name else "")
        (Path(os.fsdecode(bytes(sysd))) / os.fsdecode(raw)).write_text(body)
    r = _root()
    yield _case("entries", _esc_fn(autostart.entries), _env(r, dirs=str(_ROOT / "nonutf8b")),
                {"root": r, "files": {}})

    # Review round 2.
    def _catch(fn):
        def run():
            try:
                return fn()
            except Exception as exc:                   # noqa: BLE001
                return {"error": str(exc)}
        return run

    # a. glib's g_get_charset() honours $CHARSET; Python's read_text/write_text
    #    use the locale fixed at interpreter start-up and ignore it.
    for cs in ["ISO-8859-1", "ASCII"]:
        r = _root()
        fx = {"root": r, "files": {"x.desktop": "[Desktop Entry]\nName=Ünï 日本\n"}}
        p = r + "/x.desktop"
        yield _case("_parse", lambda p=p: autostart._parse(Path(p)), {"CHARSET": cs}, fx, path=p)
        r = _root()
        fx = {"root": r, "files": {"etc/xdg/autostart/u.desktop": "[Desktop Entry]\nName=Ünï 日本\n"}}
        steps = [["u.desktop", False]]
        yield _case("set_enabled", _catch(_steps_fn(fx, steps, True)), {**_env(r), "CHARSET": cs},
                    fx, steps=steps, after=True)

    # b. Explicit file names pathlib normalises but PathBuf::join keeps:
    #    "" and "." are the user dir itself, "a/" is "a".
    r = _root()
    base = {"filename": "keys.desktop", "name": "Keys", "comment": "", "icon": "",
            "enabled": True, "path": r + "/etc/xdg/autostart/keys.desktop",
            "system": r + "/etc/xdg/autostart/keys.desktop", "ours": False}
    for fname, on, extra in [("", False, {}), (".", False, {}), ("", False, {"home/autostart": None}),
                             ("", True, {"home/autostart": None}), ("a/", False, {}),
                             ("sub//k.desktop", False, {"home/autostart/sub": None}),
                             ("./k.desktop", False, {})]:
        r2 = _root()
        e = {k: (v.replace(r, r2) if isinstance(v, str) else v) for k, v in base.items()}
        e["filename"] = fname
        st = [[e, on]]
        fx = {"root": r2, "files": {**sys_keys, **extra}}
        yield _case("set_enabled", _catch(_steps_fn(fx, st, False)), _env(r2), fx, steps=st, after=False)
    # system / path given with a trailing slash or empty: Path() drops the
    # slash and reads "" as "."
    for sysv in [r + "/etc/xdg/autostart/keys.desktop/", ""]:
        r2 = _root()
        e = {k: (v.replace(r, r2) if isinstance(v, str) else v) for k, v in base.items()}
        e["system"] = sysv.replace(r, r2)
        st = [[e, False]]
        fx = {"root": r2, "files": dict(sys_keys)}
        yield _case("set_enabled", _catch(_steps_fn(fx, st, False)), _env(r2), fx, steps=st, after=False)

    # c. read_text() translates \r\n and \r (universal newlines) before the
    #    copy is written; visible when the first _set_key then fails (a
    #    directory sits where the .tmp goes).
    r = _root()
    fx = {"root": r, "files": {"etc/xdg/autostart/keys.desktop": "[Desktop Entry]\r\nName=K\rExec=k\r\n",
                               "etc/xdg/autostart/g.desktop": KEYS,
                               "home/autostart/keys.tmp": None}}
    steps = [["keys.desktop", False]]
    # tree_on_error: the error comes with the tree, so the left-behind copy
    # (LF only) is compared too.
    def _with_tree(fn, fx):
        def run():
            try:
                return fn()
            except Exception as exc:                   # noqa: BLE001
                return {"error": str(exc), "tree": _tree(fx)}
        return run
    yield _case("set_enabled", _with_tree(_steps_fn(fx, steps, False), fx), _env(r), fx,
                steps=steps, after=False, tree_on_error=True)

    # d. repr() of a path in an error message: unassigned and format
    #    characters Python escapes.
    for ch in ["͸", "\U000e0041", "\U0001d173", "࢐", "࣢", "\U000110bd",
               "\U00013430", "﷐", "⿿", "\U0001f8ff"]:
        r = _root()
        fx = {"root": r, "files": {}}
        p = r + "/x" + ch + ".desktop"
        yield _case("_set_key", _catch(lambda p=p: autostart._set_key(Path(p), "Hidden", "true")),
                    fixture=fx, path=p, key="Hidden", value="true")

    # e. names glob must still match: a newline, a star, brackets
    for name in ["a\nb.desktop", "*.desktop", "[x].desktop", "a.desktop\n"]:
        r = _root()
        files = {"etc/xdg/autostart/" + name: "[Desktop Entry]\nExec=x\n"}
        yield _case("entries", autostart.entries, _env(r), {"root": r, "files": files})

    # f. error texts, compared exactly
    for files, steps in [
        ({**sys_keys, "home/autostart": "a file"}, [["keys.desktop", False]]),
        ({**sys_keys, "home": "a file"}, [["keys.desktop", False]]),
        ({**sys_keys, "home/autostart/keys.desktop": None}, [["keys.desktop", False]]),
        ({**sys_keys, "home/autostart/keys.desktop": None}, [["keys.desktop", True]]),
        ({**sys_keys, "home/autostart/keys.desktop": {"link": "/nonexistent/x"}}, [["keys.desktop", False]]),
    ]:
        r = _root()
        fx = {"root": r, "files": files}
        yield _case("set_enabled", _catch(_steps_fn(fx, steps, True)), _env(r), fx, steps=steps, after=True)
