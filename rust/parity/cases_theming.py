"""Parity cases for theming.py.

Readers run against the real machine (no `current` arg) and against fixture
themes in a temp dir: the Python side patches `theming.CURRENT` for the call,
the Rust side receives the same path as `current`. Nothing outside the temp
dir is written.
"""
import atexit
import os
import shutil
import sys
import tempfile
from pathlib import Path

import theming

EVERFOREST = {"mode": "dark", "background": "#2d353b", "dark_background": "#21272c",
              "darker_background": "#181d20", "lighter_background": "#343f44",
              "foreground": "#d3c6aa", "dark_foreground": "#4f585e",
              "accent": "#7fbbb3", "red": "#e67e80", "yellow": "#dbbc7f",
              "green": "#a7c080"}
LIGHT = {"mode": "light", "background": "#fdf6e3", "dark_background": "#eee8d5",
         "darker_background": "#e4ddc8", "lighter_background": "#ffffff",
         "foreground": "#586e75", "accent": "#268bd2", "red": "#dc322f",
         "yellow": "#b58900", "green": "#859900"}


def _esc(v):
    """A lone surrogate (an undecodable byte b, surrogateescape) cannot cross
    to Rust as JSON text; both sides write it as chr(0x10FF00 + b) instead."""
    if isinstance(v, Path):
        v = str(v)
    if isinstance(v, str):
        return "".join(chr(0x10FF00 + ord(c) - 0xDC00) if 0xDC80 <= ord(c) <= 0xDCFF
                       else c for c in v)
    return v


APP_RECURSION_LIMIT = 1000  # sys.getrecursionlimit() in the app


def at(cur, fn):
    """Call `fn` as the app would with its state under `cur`'s root:
    theming.STATE and theming.CURRENT (both bound when theming was imported)
    pointed at the fixture, and Python's recursion limit at the app's own.

    The limit matters because tomllib recurses: a deeply nested colors.toml
    raises RecursionError in the app, and tomllib sized MAX_KEY_PARTS from
    the limit when it was imported. Other modules' cases raise the limit for
    the whole process (cases_ghcache sets 20000 at import), so without this
    the answer for those files depended on which modules ran first."""
    import tomllib._parser as tp

    def run():
        old_state, old_cur = theming.STATE, theming.CURRENT
        old_limit, old_parts = sys.getrecursionlimit(), tp.MAX_KEY_PARTS
        theming.CURRENT = Path(cur)
        theming.STATE = Path(cur).parent.parent.parent
        sys.setrecursionlimit(APP_RECURSION_LIMIT)
        tp.MAX_KEY_PARTS = APP_RECURSION_LIMIT
        try:
            return _esc(fn())
        finally:
            sys.setrecursionlimit(old_limit)
            tp.MAX_KEY_PARTS = old_parts
            theming.STATE, theming.CURRENT = old_state, old_cur
    return run


def fixtures():
    root = Path(tempfile.mkdtemp(prefix="almanac-theming-parity-"))
    atexit.register(shutil.rmtree, root, True)
    out = {}

    def theme(name, colors=None, raw=None, theme_name=None, background=None,
              symlink=False):
        state = root / name
        cur = state / "omarchy/current/theme"
        if symlink:
            real = state / "omarchy/themes" / name
            real.mkdir(parents=True)
            cur.parent.mkdir(parents=True)
            cur.symlink_to(real)
        else:
            cur.mkdir(parents=True)
        if colors is not None:
            (cur / "colors.toml").write_text(colors, encoding="utf-8")
        if raw is not None:
            (cur / "colors.toml").write_bytes(raw)
        if theme_name is not None:
            (cur.parent / "theme.name").write_bytes(theme_name)
        if background == "file":
            (cur / "bg.png").write_bytes(b"x")
            (cur.parent / "background").symlink_to(cur / "bg.png")
        elif background == "dir":
            (cur.parent / "background").symlink_to(cur)
        elif background == "dangling":
            (cur.parent / "background").symlink_to(cur / "missing.png")
        out[name] = str(cur)

    theme("good", colors="\n".join(f'{k} = "{v}"' for k, v in EVERFOREST.items())
          + '\nselection = "#3d484d"\n', theme_name=b"everforest\n", background="file")
    theme("light", colors="\n".join(f'{k} = "{v}"' for k, v in LIGHT.items()),
          theme_name=b"Catppuccin Latte", background="dir", symlink=True)
    theme("messy", colors='mode = "  DARK \\t"\naccent = " #ABC "\nred = "#12345"\n'
                          'green = "green"\nyellow = 7\nblue = "#zzzzzz"\n'
                          'orange = "#AbCdEf\\n"\n[table]\nx = "#ffffff"\n'
                          'list = ["#fff"]\nfg = "\\u0663\\u0664\\u0665"\n'
                          'sep = "\\u001c#123\\u001f"\nnbsp = "\\u00a0#123\\u00a0"\n',
          theme_name=b"apple-dark_mode", background="dangling")
    theme("broken", colors='mode = "dark"\naccent = \n', theme_name=b"   ")
    theme("notutf8", raw=b'accent = "#fff"\nx = "\xff"\n', theme_name=b"\xff\xfe")
    theme("bom", raw=b'\xef\xbb\xbfaccent = "#fff"\n', theme_name=b"MiXed-case")
    theme("empty", colors="", theme_name=b"\xc3\xa9t\xc3\xa9-nuit")
    theme("nocolors", theme_name=b"UPPER")
    theme("nomode", colors='accent = "#123456"\nbackground = "#000"\n')
    theme("toml11", colors='accent = "#123456"\nt = {a = 1,\n b = 2}\n')
    theme("toml11e", colors='accent = "#123456"\nx = "\\e"\n')
    theme("toml11comma", colors='accent = "#123456"\nt = {a = 1,}\n')
    theme("toml11time", colors='accent = "#123456"\nt = 07:32\n')
    theme("toml11hex", colors='accent = "#123456"\nx = "\\x41"\n')
    theme("dupkey", colors='accent = "#123456"\naccent = "#654321"\n')
    theme("crlf", raw=b'mode = "Light"\r\naccent = "#123456"\r\n')
    out["missing"] = str(root / "nowhere/omarchy/current/theme")
    return out


def cases():
    fx = fixtures()

    # The real machine, as it is.
    yield "theme_dir", {}, lambda: theming.theme_dir()
    yield "background", {}, lambda: theming.background()
    yield "colors_path", {}, lambda: theming.colors_path()
    yield "load_palette", {}, lambda: theming.load_palette()
    yield "theme_name", {}, lambda: theming.theme_name()
    yield "describe", {}, lambda: theming.describe()
    yield "css_for", {"palette": theming.load_palette()}, \
        lambda: theming.css_for(theming.load_palette())
    yield "is_dark", {"palette": theming.load_palette()}, \
        lambda: theming.is_dark(theming.load_palette())
    yield "current", {}, lambda: theming.CURRENT
    yield "state", {}, lambda: theming.STATE

    # Fixture themes.
    for name, cur in fx.items():
        for func in ("theme_dir", "background", "colors_path", "load_palette",
                     "theme_name", "describe"):
            yield func, {"current": cur}, at(cur, getattr(theming, func))
        # watch(): the Python's `Gio.FileMonitor.directory` / `.file` do not
        # exist in PyGObject (AttributeError, swallowed by its `except
        # Exception`), so it always returns [] and never watches; so does
        # the Rust.
        yield "watch", {"current": cur}, at(cur, lambda: len(theming.watch(lambda: None)))
        pal = at(cur, theming.load_palette)()
        yield "css_for", {"palette": pal}, (lambda p=pal: theming.css_for(p))

    # luminance / contrast / hex6 / readable_on: ordinary, short, malformed,
    # unicode, Python int() quirks (sign, whitespace, Unicode digits).
    colours = ["#000000", "#ffffff", "#fff", "#FFF", "fff", "#7fbbb3", "##7fbbb3",
               "", "#", "#12", "#1234", "#12345", "#1234567", "#zzzzzz", "#ggg",
               "#-f-f-f", "# f f f", "#+f+f+f", "#f_f_ff", "#0x0x0x",
               "#\u0663\u0664\u0665\u0666\u0667\u0668", "#\uff10\uff11\uff12",
               "#ＡＢＣ", "#éàü", "#İaaaaa", "#\u00a0f\u00a0f\u00a0f",
               "#\x1cf\x1df\x1ef", "#\x0bf\x0bf\x0bf", "#\tf\nf\rf", "#3584e4", "#c01c28", "#cd9309", "#2ec27e",
               "#808080", "#767676", "#777777", "#ff0000", "#00ff00", "#0000ff",
               "#ABCDEF", "#aBc", " #fff", "#fff "]
    for c in colours:
        yield "luminance", {"value": c}, (lambda c=c: theming._luminance(c))
        yield "hex6", {"value": c}, (lambda c=c: theming._hex6(c))
        yield "readable_on", {"background": c}, (lambda c=c: theming.readable_on(c))
    for a, b in [("#fff", "#000"), ("#000000", "#ffffff"), ("#7fbbb3", "#2d353b"),
                 ("", ""), ("#zzzzzz", "#fff"), ("#d3c6aa", "#343f44"),
                 ("#-f-f-f", "#000"), ("#808080", "#808080")]:
        yield "contrast", {"a": a, "b": b}, (lambda a=a, b=b: theming.contrast(a, b))

    # mix
    for a, b, t in [("#000000", "#ffffff", 0.5), ("#000", "#fff", 0.25),
                    ("#2d353b", "#7fbbb3", 0.12), ("#ffffff", "#000000", 0.02),
                    ("#010101", "#020202", 0.5), ("#030303", "#040404", 0.5),
                    ("", "#fff", 0.5), ("#fff", "", 0.5), ("", "", 0.5),
                    ("#12", "#ABCDEF", 0.3), ("#000000", "#ffffff", 1.5),
                    ("#000000", "#ffffff", -0.5), ("#808080", "#000000", -2.0),
                    ("#zzzzzz", "#ffffff", 0.5), ("#ffffff", "#gggggg", 0.5),
                    ("#-f-f-f", "#ffffff", 0.0), ("# f f f", "#000", 1.0),
                    ("#\u0663\u0664\u0665\u0666\u0667\u0668", "#000", 0.5),
                    ("#İaaaaa", "#000", 0.5), ("#ABC", "#DEF", 0.0),
                    ("#7fbbb3", "#d3c6aa", 0.98), ("#000000", "#ffffff", 0.0),
                    ("#000000", "#ffffff", 1.0)]:
        yield "mix", {"a": a, "b": b, "t": t}, (lambda a=a, b=b, t=t: theming.mix(a, b, t))
    for i in range(0, 51, 7):
        t = i / 50
        yield "mix", {"a": "#e67e80", "b": "#343f44", "t": t}, \
            (lambda t=t: theming.mix("#e67e80", "#343f44", t))

    # legible
    legs = [("#e67e80", "#d3c6aa", ["#343f44", "#2d353b"], 5.5),
            ("#d3c6aa", "#ffffff", ["#343f44", "#2d353b"], 7.0),
            ("#586e75", "#000000", ["#ffffff", "#fdf6e3"], 7.0),
            ("#268bd2", "#586e75", ["#ffffff", "#fdf6e3"], 5.5),
            ("", "#fff", ["#000"], 5.5),
            ("#fff", "#000", [], 5.5),
            ("#fff", "#000", ["", ""], 5.5),
            ("#808080", "#808080", ["#808080"], 21.0),
            ("#808080", "#909090", ["", "#ffffff"], 7.0),
            ("#808080", "", ["#ffffff"], 7.0),
            ("#808080", "#7f7f7f", ["#808080"], 4.5),
            ("#zzzzzz", "#000", ["#fff"], 5.5),
            ("#123", "#zzzzzz", ["#fff"], 5.5),
            ("#12", "#000", ["#fff"], 5.5),
            ("#fff", "#000", ["#fff"], 0.0)]
    for color, toward, surfaces, target in legs:
        yield "legible", {"color": color, "toward": toward, "surfaces": surfaces,
                          "target": target}, \
            (lambda c=color, w=toward, s=surfaces, t=target: theming.legible(c, w, s, t))

    # dim_opacity
    for fg, surfaces, target in [("#d3c6aa", ["#343f44", "#2d353b"], 5.5),
                                 ("#586e75", ["#ffffff", "#fdf6e3"], 5.5),
                                 ("#ffffff", ["#000000"], 5.5),
                                 ("#808080", ["#808080"], 5.5),
                                 ("#fff", [], 5.5), ("#fff", ["", "#000"], 5.5),
                                 ("#zzzzzz", ["#000"], 5.5), ("", ["#000"], 5.5),
                                 ("#ffffff", ["#000000"], 30.0)]:
        yield "dim_opacity", {"fg": fg, "surfaces": surfaces, "target": target}, \
            (lambda f=fg, s=surfaces, t=target: theming.dim_opacity(f, s, t))

    # pill_css
    stock = {"error": "#c01c28", "warning": "#cd9309", "success": "#2ec27e",
             "accent": "#3584e4"}
    for fills, prefer in [(stock, []), (stock, ["#181d20", "#d3c6aa"]),
                          ({"error": "#e67e80", "warning": "#dbbc7f", "success": "#a7c080",
                            "accent": "#7fbbb3"}, ["#181d20", "#d3c6aa"]),
                          ({}, []), ({"error": ""}, ["#000"]),
                          ({"error": "#12", "accent": "#FFF"}, []),
                          ({"warning": "#zzzzzz"}, []),
                          ({"accent": "#808080"}, ["", "#000000", "#000", "#ffffff"]),
                          ({"accent": "#ffffff"}, ["#000", "#000000"]),
                          ({"other": "#fff"}, [])]:
        yield "pill_css", {"fills": fills, "prefer": prefer}, \
            (lambda f=fills, p=prefer: theming.pill_css(f, p))

    # pick_css
    for card, fg, accent in [("#343f44", "#d3c6aa", "#7fbbb3"),
                             ("#ffffff", "#586e75", "#268bd2"),
                             ("#ffffff", "#000000", "#3584e4"),
                             ("#383838", "#ffffff", "#78aeed"),
                             ("#2e2e32", "#ffffff", "#78aeed"),
                             ("", "#fff", "#000"), ("#fff", "", "#000"),
                             ("#fff", "#000", ""), ("#12", "#000", "#fff"),
                             ("#zzzzzz", "#000", "#fff"), ("#FFF", "#000", "#ABC")]:
        yield "pick_css", {"card": card, "fg": fg, "accent": accent}, \
            (lambda c=card, f=fg, a=accent: theming.pick_css(c, f, a))

    # flatten
    for rgba, under in [((1, 1, 1, 0.08), (0x22 / 255, 0x22 / 255, 0x26 / 255)),
                        ((1, 1, 1, 1), (0, 0, 0)), ((0.5, 0.5, 0.5, 1), None),
                        ((0.5, 0.5, 0.5, 0.5), None), ((0.2, 0.4, 0.6, 0.3), (1, 1, 1)),
                        ((1 / 255 * 0.5, 2.5 / 255, 3.5 / 255, 1), None),
                        ((-0.1, 1.5, 0, 1), None), ((-1, -0.5, 2, 1), None),
                        ((0, 0, 0, 0), (1, 1, 1)), ((0.1, 0.2, 0.3, -1), (0.5, 0.5, 0.5))]:
        yield "flatten", {"rgba": list(rgba), "under": list(under) if under else None}, \
            (lambda r=rgba, u=under: theming.flatten(r, u))

    # css_for
    pals = [EVERFOREST, LIGHT, {}, {"mode": "dark"},
            {"background": "#000000"}, {"foreground": "#ffffff"},
            {"accent": "#3584e4"}, {"blue": "#3584e4", "selection": "#123456"},
            {"background": "#2d353b", "foreground": "#d3c6aa"},
            {"lighter_background": "#ffffff", "foreground": "#777777"},
            {"background": "#fff", "foreground": "#000", "orange": "#e09d7f",
             "red": "#f00", "green": "#0f0"},
            {"background": "#2d353b", "dark_background": "", "foreground": "#d3c6aa",
             "accent": "", "blue": "#7fbbb3"},
            {"background": "#zzzzzz", "foreground": "#fff"},
            {"background": "#000", "foreground": "#zzzzzz"},
            {"background": "#123", "foreground": "#456", "accent": "#12"}]
    for pal in pals:
        yield "css_for", {"palette": pal}, (lambda p=pal: theming.css_for(p))
        yield "is_dark", {"palette": pal}, (lambda p=pal: theming.is_dark(p))
    for mode in ["dark", "light", "Dark", "", "auto"]:
        yield "is_dark", {"palette": {"mode": mode}}, \
            (lambda m=mode: theming.is_dark({"mode": m}))


# --- Review round 1 (adversarial): cases that probe suspected differences. ---

def _fixtures_r1():
    root = Path(tempfile.mkdtemp(prefix="almanac-theming-r1-"))
    atexit.register(shutil.rmtree, root, True)
    out = {}

    def theme(name, colors=None, theme_name=None, real_name=None):
        state = root / name
        cur = state / "omarchy/current/theme"
        if real_name is not None:
            real = Path(os.fsdecode(bytes(state / "omarchy/themes") + b"/" + real_name))
            real.mkdir(parents=True)
            cur.parent.mkdir(parents=True)
            cur.symlink_to(real)
        else:
            cur.mkdir(parents=True)
        if colors is not None:
            (cur / "colors.toml").write_bytes(colors)
        if theme_name is not None:
            (cur.parent / "theme.name").write_bytes(theme_name)
        out[name] = str(cur)

    acc = b'mode = "dark"\naccent = "#123456"\n'
    # tomllib vs the toml crate (TOML 1.0 corner cases).
    theme("bigint", acc + b"x = 99999999999999999999\n")
    theme("leapday", acc + b"d = 1979-02-29\n")
    theme("badday", acc + b"d = 1979-04-31\n")
    theme("sec60", acc + b"t = 23:59:60\n")
    theme("hour24", acc + b"t = 24:00:00\n")
    theme("ctrlcomment", acc + b"# \x01 nope\n")
    theme("delcomment", acc + b"# \x7f nope\n")
    theme("delstring", acc + b'x = "a\x7fb"\n')
    theme("barecr", acc + b"x = 1\ry = 2\n")
    theme("dotted_then_table", acc + b"a.b = 1\n[a]\nc = 2\n")
    theme("table_then_dotted", acc + b"[a]\nb.c = 1\n[a.b]\nd = 2\n")
    theme("inf", acc + b"x = 1e1000\n")
    theme("emptykey", acc + b'"" = "#fff"\n')
    theme("nsfrac", acc + b"t = 07:32:00.1234567891\n")
    theme("utf8key", acc + "é = \"#abc\"\n".encode())
    # Numbers and dates past the crate's limits, or Python's.
    theme("hexbig", acc + b"x = 0xffffffffffffffffff\n")
    theme("negbig", acc + b"x = -9223372036854775809\n")
    theme("intmax", acc + b"x = 9223372036854775807\ny = -9223372036854775808\n")
    theme("bigs_nested", acc + b"a = [1e400, 99_999_999_999_999_999_999, -1e999]\n"
                               b"t = {b = 0o7777777777777777777777777, c = 0b" + b"1" * 70 + b"}\n"
                               b"[s]\nd = 1_000e1_000\n")
    theme("tiny", acc + b"x = 1e-400\n")
    theme("bigkey", acc + b"99999999999999999999 = \"#fff\"\n")
    theme("dupbigkey", acc + b"99999999999999999999 = 1\n99999999999999999999 = 2\n")
    theme("bigdup", acc + b"x = 99999999999999999999\nx = 1\n")
    theme("leadzero", acc + b"x = 099999999999999999999\n")
    theme("year0", acc + b"d = 0000-01-01\n")
    theme("year0dt", acc + b"d = 0000-01-01T00:00:00Z\n")
    theme("sec60dt", acc + b"d = 1979-05-27T23:59:60Z\n")
    theme("sec60arr", acc + b"d = [{t = 23:59:60.5}]\n")
    theme("off24", acc + b"d = 1979-05-27T00:00:00+24:00\n")
    theme("offneg0", acc + b"d = 1979-05-27T00:00:00-00:00\n")
    theme("year9999", acc + b"d = 9999-12-31T23:59:59.999999999Z\n")
    # theme.name: text-mode newline translation, Unicode title/islower.
    theme("cr_name", acc, theme_name=b"apple\rdark")
    theme("crlf_name", acc, theme_name=b"apple\r\ndark")
    theme("sharp_s", acc, theme_name="ßlue-x".encode())
    theme("digraph_lower", acc, theme_name="ǆa-b".encode())
    theme("digraph_title", acc, theme_name="ǅa-b".encode())
    theme("ligature", acc, theme_name="ﬁre-dark".encode())
    theme("bom_name", acc, theme_name=b"\xef\xbb\xbfapple-dark")
    # A theme directory whose name is not UTF-8 (theme.name absent, so the
    # name falls back to the directory's).
    theme("latin1dir", acc, real_name=b"caf\xe9")
    theme("latin1dir_lower", acc, real_name=b"caf\xe9-noir")
    # More of str.title(): final sigma, Lt in the middle, Georgian, ŉ.
    theme("sigma", acc, theme_name="οδοσ-σα".encode())
    theme("sigma2", acc, theme_name="aσ.σb σ".encode())
    theme("lt_mid", acc, theme_name="aǅb".encode())
    theme("georgian", acc, theme_name="ქართ-ული".encode())
    theme("napos", acc, theme_name="ŉx-ﬃ ﬆ".encode())
    theme("dotted_i", acc, theme_name="i̇x-y".encode())
    return out


def cases_r1():
    fx = _fixtures_r1()
    for name, cur in fx.items():
        for func in ("theme_dir", "load_palette", "theme_name", "describe"):
            yield func, {"current": cur}, at(cur, getattr(theming, func))
    # The Python watch(): Gio.FileMonitor.directory/.file do not exist, so
    # every target hits `except Exception: continue` and none is watched.
    for cur in fx.values():
        yield "watch", {"current": cur}, at(cur, lambda: len(theming.watch(lambda: None)))
    # Python ints are unbounded; the Rust rounding helper caps at 1.7e38.
    yield "mix", {"a": "#000000", "b": "#ffffff", "t": 1e40}, \
        lambda: theming.mix("#000000", "#ffffff", 1e40)
    yield "flatten", {"rgba": [1e40, 0, 0, 1], "under": None}, \
        lambda: theming.flatten((1e40, 0, 0, 1), None)
    for t in (1e19, -1e19, 9.3e18, 2.0**63, 2.0**64 + 2**12, 1e300, -1e300, 1.7976931348623157e308):
        yield "mix", {"a": "#000000", "b": "#ffffff", "t": t}, \
            (lambda t=t: theming.mix("#000000", "#ffffff", t))
        yield "flatten", {"rgba": [t, -t, 0.5, 1], "under": None}, \
            (lambda t=t: theming.flatten((t, -t, 0.5, 1), None))
    yield "flatten", {"rgba": [1e300, 0, 0, 0.5], "under": [1e300, 0, 0]}, \
        lambda: theming.flatten((1e300, 0, 0, 0.5), (1e300, 0, 0))
    # Final sigma and other lowercasing in _hex6.
    for c in ["#aaaaaΣ", "#ΣΣΣ", "#ẞẞẞ", "#Ωbc"]:
        yield "hex6", {"value": c}, (lambda c=c: theming._hex6(c))


_cases_orig = cases


def cases():                                         # noqa: F811
    yield from _cases_orig()
    yield from cases_r1()


# Opt-in (THEMING_FAKE_OMARCHY=1): shadow `omarchy` on PATH with a script
# whose `theme current` prints bytes that are not UTF-8. The Python's
# subprocess.run(text=True) raises UnicodeDecodeError (neither OSError nor
# SubprocessError), so theme_name() raises; Rust's util::run decodes lossily.
# Opt-in because it changes PATH for every case in the run (both sides
# inherit it), which would otherwise mask the real omarchy fallback.
if os.environ.get("THEMING_FAKE_OMARCHY") == "1":
    _fake = Path(tempfile.mkdtemp(prefix="almanac-theming-fakebin-"))
    atexit.register(shutil.rmtree, _fake, True)
    (_fake / "omarchy").write_text("#!/bin/sh\nprintf '\\377\\376Name\\n'\n")
    (_fake / "omarchy").chmod(0o755)
    os.environ["PATH"] = f"{_fake}:{os.environ.get('PATH', '')}"
    _cases_r1_inner = cases

    def cases():                                     # noqa: F811
        yield from _cases_r1_inner()
        root = Path(tempfile.mkdtemp(prefix="almanac-theming-fake-"))
        atexit.register(shutil.rmtree, root, True)
        cur = root / "omarchy/current/theme"
        cur.mkdir(parents=True)
        (cur / "colors.toml").write_text('mode = "dark"\n')
        for func in ("theme_name", "describe"):
            yield func, {"current": str(cur)}, at(str(cur), getattr(theming, func))


# --- Review round 2 (adversarial). ---

def _fixtures_r2():
    import unicodedata
    root = Path(tempfile.mkdtemp(prefix="almanac-theming-r2-"))
    atexit.register(shutil.rmtree, root, True)
    out = {}

    def theme(name, colors=None, theme_name=None):
        cur = root / name / "omarchy/current/theme"
        cur.mkdir(parents=True)
        if colors is not None:
            (cur / "colors.toml").write_bytes(colors)
        if theme_name is not None:
            (cur.parent / "theme.name").write_bytes(theme_name)
        out[name] = str(cur)

    acc = b'mode = "dark"\naccent = "#123456"\n'
    # Nesting: tomllib recurses (RecursionError, not caught by load_palette);
    # the toml crate has its own recursion limit.
    for d in (20, 60, 100, 130, 200, 300, 400, 700):
        theme(f"nest_arr{d}", acc + b"x = " + b"[" * d + b"]" * d + b"\n")
        theme(f"nest_tbl{d}", acc + b"x = " + b"{a=" * d + b"1" + b"}" * d + b"\n")
    theme("nest_keys", acc + b"x." + b".".join(b"k%d" % i for i in range(300)) + b" = 1\n")
    theme("nest_hdr", acc + b"[" + b".".join(b"k%d" % i for i in range(300)) + b"]\nz = 1\n")
    # Assorted TOML 1.0 corners.
    misc = {
        "surrogate_esc": b'x = "\\uD800"\n',
        "big_esc": b'x = "\\U00110000"\n',
        "off99": b"d = 1979-05-27T00:00:00+99:00\n",
        "off2360": b"d = 1979-05-27T00:00:00+23:60\n",
        "off2359": b"d = 1979-05-27T00:00:00+23:59\n",
        "dt_nosec": b"d = 1979-05-27T07:32Z\n",
        "dt_space": b"d = 1979-05-27 07:32:00\n",
        "dt_lower": b"d = 1979-05-27t07:32:00z\n",
        "dt_frac_empty": b"t = 07:32:00.\n",
        "month13": b"d = 1979-13-01\n",
        "exp_lead0": b"x = 1e05\n",
        "exp_us": b"x = 1e_5\n",
        "float_lead0": b"x = 00.5\n",
        "keys_special": b'inf = "#fff"\nnan = "#000"\ntrue = "#111"\n1.2 = 3\n',
        "inline_extend": b"a = {b = 1}\na.c = 2\n",
        "aot_then_tbl": b"[[a]]\nb = 1\n[a]\n",
        "tbl_dup": b"[a]\n[a]\n",
        "super_after": b"[a.b.c]\n[a]\nd = 1\n",
        "aot_dotted": b"[[a]]\nb.c = 1\n[a.b]\nd = 1\n",
        "ml_quotes": b'x = """a""""\ny = """a"""""\n',
        "ml_six": b'x = """a""""""\n',
        "ml_bs": b'x = """a \\   \n   b"""\n',
        "lit_ml_cr": b"x = '''a\rb'''\n",
        "tab_in_str": b'x = "a\tb"\n',
        "nul_esc_key": b'"a\\u0000b" = "#abc"\n',
        "ws_header": b"[ a . b ]\nc = 1\n",
        "quoted_dotted": b'"a".b = 1\n',
        "empty_quoted_tbl": b'[""]\nx = 1\n',
        "neg_zero": b"x = -0\ny = +0\nz = -0.0\nw = -nan\n",
        "hex_upper": b"x = 0XFF\n",
        "oct_neg": b"x = -0o7\n",
        "esc_slash": b'x = "\\/"\n',
        "esc_e": b'x = "\\e"\n',
        "long_float": b"x = " + b"1" * 400 + b".5\n",
        "long_int_digits": b"x = " + b"9" * 5000 + b"\n",
        "deep_dt_arr": b"d = [[[1979-02-29]]]\n",
        # Python 3.11+ caps int() at 4300 decimal digits (ValueError, so
        # load_palette gives {}); powers-of-two bases are exempt.
        "int_4300": b"x = " + b"9" * 4300 + b"\n",
        "int_4301": b"x = " + b"9" * 4301 + b"\n",
        "int_4301_us": b"x = " + b"_".join([b"9"] * 4301) + b"\n",
        "hex_5000": b"x = 0x" + b"f" * 5000 + b"\n",
    }
    for d in range(70, 100, 3):
        theme(f"nest_arr{d}", acc + b"x = " + b"[" * d + b"]" * d + b"\n")
    for n in (60, 100, 150):
        theme(f"nest_keys{n}", acc + b"x." + b".".join(b"k%d" % i for i in range(n)) + b" = 1\n")
    # Unicode 17 case pairs (Python 3.14 is Unicode 16: unassigned there).
    for cp in (0xA7CE, 0xA7CF, 0xA7D2, 0xA7D3, 0x16EA0, 0x16EBB):
        theme(f"u17_{cp:x}", f'mode = "\\U{cp:08X}"\naccent = "#123456"\n'.encode(),
              theme_name=("x-" + chr(cp) + chr(cp)).encode())
    for k, v in misc.items():
        theme("m_" + k, acc + v)

    # Unicode tables: Python 3.14 is Unicode 16.0; Rust's std may be newer.
    # mode = "<chunk>" exercises strip()+lower(); theme.name exercises
    # islower()+title().
    cps = [c for c in range(0x80, 0x110000) if not 0xD800 <= c <= 0xDFFF]
    step = 8192
    for i in range(0, len(cps), step):
        chunk = cps[i:i + step]
        esc = "".join(f"\\U{c:08X}" for c in chunk)
        theme(f"lower_{i}", f'mode = "a{esc}a"\naccent = "#123456"\n'.encode())
        chars = [chr(c) for c in chunk
                 if not chr(c).isupper() and unicodedata.category(chr(c)) != "Lt"]
        name = "a " + " ".join(c + c for c in chars) + " a"
        theme(f"title_{i}", acc, theme_name=name.encode("utf-8", "surrogatepass"))
    return out


def cases_r2():
    fx = _fixtures_r2()
    for name, cur in fx.items():
        funcs = ("load_palette",) if name.startswith("lower_") else \
                ("theme_name",) if name.startswith("title_") else \
                ("load_palette", "theme_name", "describe") if name.startswith("u17_") else \
                ("load_palette", "describe")
        for func in funcs:
            yield func, {"current": cur}, at(cur, getattr(theming, func))
    # Python int() accepts every Unicode decimal digit (Nd); the Rust keeps a
    # static table of block zeros.
    import unicodedata
    for c in range(0x80, 0x110000):
        ch = chr(c)
        if not 0xD800 <= c <= 0xDFFF and unicodedata.decimal(ch, None) is not None:
            yield "luminance", {"value": "#" + ch * 2 + "ffff"}, \
                (lambda ch=ch: theming._luminance("#" + ch * 2 + "ffff"))
    for v in ["#\ua7ce\ua7ce\ua7ce", "#\U00016ea0" * 3, "#\ua7d2bc"]:
        yield "hex6", {"value": v}, (lambda v=v: theming._hex6(v))
    # Whitespace accepted by int().
    for c in [0x85, 0xA0, 0x1680, 0x180E, 0x2000, 0x200B, 0x2028, 0x202F, 0x205F,
              0x3000, 0xFEFF, 0x1C, 0x1F]:
        v = "#" + chr(c) + "f" * 5
        yield "luminance", {"value": v}, (lambda v=v: theming._luminance(v))


_cases_r1_outer = cases


def cases():                                         # noqa: F811
    yield from _cases_r1_outer()
    yield from cases_r2()


# --- Round 3: the tomllib port, at the edges of Python's recursion limit. ---

def _fixtures_r3():
    root = Path(tempfile.mkdtemp(prefix="almanac-theming-r3-"))
    atexit.register(shutil.rmtree, root, True)
    out = {}

    def theme(name, colors):
        cur = root / name / "omarchy/current/theme"
        cur.mkdir(parents=True)
        (cur / "colors.toml").write_bytes(colors)
        out[name] = str(cur)

    acc = b'mode = "dark"\naccent = "#123456"\n'
    inner = {
        "empty": b"", "int": b"1", "str": b'"a"', "esc": b'"\\u0041"',
        "bigesc": b'"\\U0001F600"', "badesc": b'"\\q"', "mlbs": b'"""a\\\n  b"""',
        "lit": b"'a'", "mllit": b"'''a'''", "dt": b"1979-05-27T00:00:00+01:00",
        "baddt": b"1979-02-30", "lt": b"07:32:00", "true": b"true",
        "tbl": b"{a=1}", "dotted": b"{a.b.c=1}", "cmt": b"# c\n", "bad": b"?",
        "inf": b"-inf",
    }
    for k, v in inner.items():
        for d in range(490, 499):
            theme(f"a_{k}_{d}", acc + b"x = " + b"[" * d + v + b"]" * d + b"\n")
        for d in range(325, 334):
            theme(f"t_{k}_{d}", acc + b"x = " + b"{a=" * d + (v or b"1").replace(b"# c\n", b"1") + b"}" * d + b"\n")
    for d in range(195, 202):
        theme(f"mix_{d}", acc + b"x = " + b"[{a=" * d + b"1" + b"}]" * d + b"\n")
        theme(f"mixarr_{d}", acc + b"x = " + b"[1, [" * d + b"]" * d * 1 + b"]" * d + b"\n")
    for n in (998, 999, 1000, 1001):
        parts = b".".join(b"k%d" % i for i in range(n))
        theme(f"keys_{n}", acc + parts + b" = 1\n")
        theme(f"hdr_{n}", acc + b"[" + parts + b"]\nz = 1\n")
        theme(f"aot_{n}", acc + b"[[" + parts + b"]]\nz = 1\n")
        theme(f"inl_{n}", acc + b"x = {" + parts + b" = 1}\n")
    # Error before / after a too-deep value.
    theme("err_first", acc + b"y = ?\nx = " + b"[" * 700 + b"]" * 700 + b"\n")
    theme("deep_first", acc + b"x = " + b"[" * 700 + b"]" * 700 + b"\ny = ?\n")
    # Order of keys, and tables after the palette.
    theme("order", b'red = "#ff0000"\nmode = "Light"\naccent = "#00ff00"\n[t]\nblue = "#0000ff"\n')
    theme("aot_mix", acc + b"[[a]]\nb = 1\n[[a]]\n[a.c]\nd = 1\n")
    theme("crlf", b'mode = "dark"\r\naccent = "#123456"\r\n')
    theme("lone_cr", b'mode = "dark"\raccent = "#123456"\n')
    return out


def cases_r3():
    for name, cur in _fixtures_r3().items():
        for func in ("load_palette", "describe"):
            yield func, {"current": cur}, at(cur, getattr(theming, func))


_cases_r2_outer = cases


def cases():                                         # noqa: F811
    yield from _cases_r2_outer()
    yield from cases_r3()


# Opt-in (THEMING_NO_PATH=1): PATH unset for the Rust run. shutil.which()
# then searches os.confstr("CS_PATH") (/bin:/usr/bin) and finds omarchy;
# util::which returns None when PATH is unset. (run.py builds before it
# imports this module, so the build still has PATH.)
if os.environ.get("THEMING_NO_PATH") == "1":
    os.environ.pop("PATH", None)
    _cases_r2_np = cases

    def cases():                                     # noqa: F811
        root = Path(tempfile.mkdtemp(prefix="almanac-theming-nopath-"))
        atexit.register(shutil.rmtree, root, True)
        cur = root / "omarchy/current/theme"
        cur.mkdir(parents=True)
        (cur / "colors.toml").write_text('mode = "dark"\n')
        for func in ("theme_name", "describe"):
            yield func, {"current": str(cur)}, at(str(cur), getattr(theming, func))


# Opt-in (THEMING_SLOW_OMARCHY=1): an `omarchy` that prints its name and
# exits at once, leaving a background child holding stdout for 12 s.
# Python's communicate(timeout=10) waits for EOF, so it times out and falls
# back to the directory name; the Rust joins its reader thread with no
# deadline once the child has exited, and returns the printed name.
if os.environ.get("THEMING_SLOW_OMARCHY") == "1":
    _slow = Path(tempfile.mkdtemp(prefix="almanac-theming-slowbin-"))
    atexit.register(shutil.rmtree, _slow, True)
    (_slow / "omarchy").write_text("#!/bin/sh\necho 'Slow Name'\nsleep 12 &\nexit 0\n")
    (_slow / "omarchy").chmod(0o755)
    os.environ["PATH"] = f"{_slow}:{os.environ.get('PATH', '')}"

    def cases():                                     # noqa: F811
        root = Path(tempfile.mkdtemp(prefix="almanac-theming-slow-"))
        atexit.register(shutil.rmtree, root, True)
        cur = root / "omarchy/current/theme"
        cur.mkdir(parents=True)
        yield "theme_name", {"current": str(cur)}, at(str(cur), theming.theme_name)
