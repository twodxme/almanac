"""Parity cases for the icon choices the window makes (app.py, ported in
src/ui/ai.rs and src/ui/plugins.rs; `almanac-parity ui`).

The Python side calls the real `_app_icon` / `_icon_source` / `_plugin_logo`
methods with stand-ins for what needs a window: the icon theme answers from
the `has_icon` list in the args, the image records what it was told to show,
and the thread pool runs its job at once with `icons.fetch` /
`store.fetch_thumb` recording their arguments instead of downloading. What
they recorded is the choice: the Rust side returns the same decision from
`Win::app_icon_choice` / `Win::plugin_logo_choice`.

Nothing here reads or writes a cache: `icons.cached` is held at None (as on
a fresh cache), and the only file touched is a temp icon for the "icon is a
path" branch.
"""
import atexit
import dataclasses
import shutil
import tempfile
from pathlib import Path
from types import SimpleNamespace

import app as A
import inventory
import plugins
import store

_TMP = Path(tempfile.mkdtemp(prefix="almanac-parity-ui-"))
atexit.register(shutil.rmtree, str(_TMP), True)
ICON_FILE = _TMP / "some-icon.png"
ICON_FILE.write_bytes(b"\x89PNG\r\n\x1a\n")
(_TMP / "a-dir.png").mkdir()


class _Anything:
    """A permissive stand-in for the Gtk widgets `_plugin_logo` builds."""

    def __init__(self, *a, **k):
        pass

    def __call__(self, *a, **k):
        return _Anything()

    def __getattr__(self, name):
        return _Anything()


def _patched(fn, has_icon=()):
    """Run fn with the window-only parts swapped out; returns what it
    recorded: [("name"|"file", value) ...], [fetch args ...], [thumb ids ...]."""
    calls, fetches, thumbs = [], [], []
    theme = SimpleNamespace(has_icon=lambda n: n in has_icon)
    saved = (A.Gtk, A.GLib, A.icons_mod.fetch, A.icons_mod.cached, A.store_mod.fetch_thumb)
    A.Gtk = SimpleNamespace(IconTheme=SimpleNamespace(get_for_display=lambda d: theme),
                            Box=_Anything, Image=_Anything, Align=_Anything(),
                            Overflow=_Anything(), AccessibleRole=_Anything())
    A.GLib = SimpleNamespace(idle_add=lambda *a: None)
    A.icons_mod.fetch = lambda key, url="", domain="": fetches.append((key, url, domain))
    A.icons_mod.cached = lambda key: None
    A.store_mod.fetch_thumb = lambda listing, detail=False: thumbs.append(listing.id)
    try:
        ok = fn(calls)
    finally:
        A.Gtk, A.GLib, A.icons_mod.fetch, A.icons_mod.cached, A.store_mod.fetch_thumb = saved
    return ok, calls, fetches, thumbs


class _Img:
    def __init__(self, calls):
        self.calls = calls

    def set_from_icon_name(self, n):
        self.calls.append(("name", n))

    def set_from_file(self, p):
        self.calls.append(("file", p))


def _app_icon_choice(app, has_icon):
    fake = SimpleNamespace(get_display=lambda: None,
                           _icon_source=A.AlmanacWindow._icon_source,
                           _set_icon=lambda *a, **k: None,
                           _icon_arrived=lambda *a: False,
                           _thumb_pool=SimpleNamespace(submit=lambda fn: fn()))
    ok, calls, fetches, _ = _patched(
        lambda calls: A.AlmanacWindow._app_icon(fake, _Img(calls), app, 32), set(has_icon))
    if fetches:
        key, url, domain = fetches[0]
        return {"kind": "vendor", "key": key, "domain": domain, "url": url,
                "glyph": calls[-1][1]}
    kind, value = calls[-1]
    if kind == "file":
        return {"kind": "file", "path": value}
    return {"kind": "theme", "name": value} if ok else {"kind": "glyph", "name": value}


def _plugin_logo_choice(i, listing):
    fake = SimpleNamespace(_square=lambda *a, **k: None, _texture=lambda *a, **k: None,
                           _thumb_pool=SimpleNamespace(submit=lambda fn: fn()))
    _, _, fetches, thumbs = _patched(lambda calls: A.AlmanacWindow._plugin_logo(fake, i, listing))
    if fetches:
        key, url, domain = fetches[0]
        if domain:
            return {"kind": "vendor", "key": key, "domain": domain}
        return {"kind": "avatar", "key": key, "url": url}
    if listing is not None and listing.thumb:
        p = store.thumb_path(listing)
        return {"kind": "art", "thumb": str(p) if p else ""}
    return {"kind": "symbol", "name": plugins.symbol_for(i)}


def _app(name="x", source=inventory.REPO, **extra):
    return inventory.App(name=name, version="1", source=source, extra=extra)


def _asdict(obj):
    """A dataclass as JSON-ready args (Paths as strings, tuples as lists)."""
    import json
    return json.loads(json.dumps(dataclasses.asdict(obj), default=str))


APPS = [
    _app(),
    _app(icon="firefox"),                                   # in the theme
    _app(icon="firefox", domain="mozilla.org"),             # theme wins over vendor
    _app(icon="no-such-icon"),                               # neither: falls through
    _app(icon="no-such-icon", url="https://www.gimp.org/"),
    _app(icon=str(ICON_FILE)),                               # a file
    _app(icon=str(_TMP / "a-dir.png"), url="https://x.org"),  # a directory is not a file
    _app(icon=str(_TMP / "missing.png")),
    _app(domain="claude.com", url="https://github.com/anthropics/claude-code"),
    _app(domain="", url="https://firefox-source-docs.mozilla.org/security/nss/index.html"),
    _app(url="https://github.com/wesleygrimes/omastorm"),
    _app(url="https://github.com/WesleyGrimes"),
    _app(url="https://github.com/"),
    _app(url="https://github.com/sponsors/someone"),
    _app(url="https://github.com/orgs/x/people"),
    _app(url="https://gist.github.com/someone/abc"),
    _app(url="https://gitlab.com/inkscape/inkscape"),
    _app(url="https://codeberg.org/dnkl/foot"),
    _app(url="https://www.bbc.co.uk/sounds"),
    _app(url="https://mcp.stripe.com:443/x"),
    _app(url="http://localhost/x"),
    _app(url="ftp://example.org/pub"),
    _app(url="HTTPS://Example.org/"),
    _app(url="not a url"),
    _app(repo_url="https://github.com/crmne/omarchy-hyprmoncfg"),
    _app(url="", repo_url="https://example.net/r"),
    _app(appid="org.gnome.Calculator", source=inventory.FLATPAK),
    _app(appid="com.spotify.Client", url="https://spotify.com", source=inventory.FLATPAK),
    _app(appid="single", source=inventory.FLATPAK),
    _app(appid="io.github.foo.Bar", icon="io.github.foo.Bar", source=inventory.FLATPAK),
    _app(name="Ünï cødé", url="https://ünï.example/"),
]
for src in [inventory.REPO, inventory.AUR, inventory.APPIMAGE, inventory.LOCAL,
            inventory.FLATPAK, inventory.ORPHAN, inventory.SYSTEM, inventory.AVAILABLE,
            inventory.PLUGIN, inventory.AGENT, inventory.TOOL, "something-else", ""]:
    APPS.append(_app(source=src))
    APPS.append(_app(source=src, url="https://example.com/"))
HAS_ICON = ["firefox", "io.github.foo.Bar"]


def _installed(name, pid, url=None, kinds=()):
    state = plugins.State(id=pid, kind=plugins.CURRENT, url=url) if url is not None else None
    return plugins.Installed(id=pid, name=name, kinds=tuple(kinds), state=state)


def _listing(pid, thumb="", market="omarchy", install_kind="plugin", repo=""):
    return store.Plugin(id=pid, name=pid, description="", author="", version="1",
                        thumb=thumb, market=market, install_kind=install_kind, repo=repo)


PLUGINS = [
    (_installed("My Tailscale", "tim.tailscale"), None),                      # vendor
    (_installed("My Tailscale", "tim.tailscale"), _listing("tim.tailscale", "t.png")),
    (_installed("Omastorm", "com.omastorm.radar", "https://github.com/wesleygrimes/omastorm"), None),
    (_installed("Thing", "a.thing", "https://github.com/someone/thing"), None),   # avatar
    (_installed("Thing", "a.thing", "git@github.com:someone/thing.git"), None),
    (_installed("Thing", "a.thing", "https://gitlab.com/someone/thing"), None),
    (_installed("Thing", "a.thing", ""), None),
    (_installed("Thing", "a.thing"), None),
    (_installed("Thing", "a.thing", "https://github.com/someone/thing"),
     _listing("a.thing", "img/thing.webp")),                                    # art
    (_installed("Thing", "a.thing", "https://github.com/someone/thing"),
     _listing("a.thing", "")),
    (_installed("Thing", "a.thing"), _listing("a.thing", "img/x", market="other")),
    (_installed("Vitals", "lxp.vitals", "https://github.com/lxp/vitals"), None),  # a symbol: no avatar
    (_installed("Night light", "x.night"), None),
    (_installed("", ""), None),
]


def cases():
    for a in APPS:
        yield "_icon_source", {"app": _asdict(a)}, \
            (lambda a=a: list(A.AlmanacWindow._icon_source(a)))
        yield "app_icon_choice", {"app": _asdict(a), "has_icon": HAS_ICON}, \
            (lambda a=a: _app_icon_choice(a, HAS_ICON))
    # The real machine as it is: every row the tables show, theme looked up
    # in nothing (so every app reaches the vendor / glyph branches too).
    for a in inventory.packages()[:400] + inventory.local_builds(check_updates=False):
        yield "app_icon_choice", {"app": _asdict(a), "has_icon": []}, \
            (lambda a=a: _app_icon_choice(a, []))
    for i, listing in PLUGINS:
        yield "plugin_logo_choice", \
            {"i": _asdict(i), "listing": _asdict(listing) if listing else None}, \
            (lambda i=i, listing=listing: _plugin_logo_choice(i, listing))
    for i in plugins.installed(fetch=False):
        yield "plugin_logo_choice", {"i": _asdict(i), "listing": None}, \
            (lambda i=i: _plugin_logo_choice(i, None))
