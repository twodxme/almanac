"""Drawing what a theme actually looks like.

GitHub's social card shows a repository: its name, its description, its owner's
avatar. For a theme that is a picture of the wrong thing — it tells you nothing
about the only question you have, which is what your desktop will look like.

Every Omarchy theme ships `colors.toml`, and raw.githubusercontent.com serves
it without touching the API rate limit. So the preview is drawn here from the
theme's own palette: a miniature desktop with the theme's bar, window,
text and accent in their real colours, over its real background.

It is a mock, not a screenshot, and it is honest about that — but it is drawn
entirely from values the theme itself publishes, so the colours are exactly
the ones you would get.
"""
from __future__ import annotations

import hashlib
import math
import os
import re
import urllib.request
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:                             # Python < 3.11
    tomllib = None

CACHE = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")) / "almanac"
ART = CACHE / "themeart"
RAW = "https://raw.githubusercontent.com"
UA = {"User-Agent": "almanac"}

W, H = 480, 270
_HEX = re.compile(r"^#(?:[0-9a-fA-F]{3}|[0-9a-fA-F]{6})$")


def _rgb(value: str, fallback=(0.5, 0.5, 0.5)):
    if not value or not _HEX.match(value):
        return fallback
    h = value.lstrip("#")
    if len(h) == 3:
        h = "".join(c * 2 for c in h)
    return tuple(int(h[i:i + 2], 16) / 255 for i in (0, 2, 4))


def palette_url(repo: str) -> str:
    """repo is an https://github.com/owner/name URL."""
    parts = [p for p in (repo or "").split("/") if p]
    if len(parts) < 4:
        return ""
    return f"{RAW}/{parts[2]}/{parts[3]}/HEAD/colors.toml"


def fetch_palette(repo: str) -> dict:
    """The theme's own colours. Served raw, so it costs no API quota."""
    url = palette_url(repo)
    if not url or tomllib is None:
        return {}
    try:
        req = urllib.request.Request(url, headers=UA)
        with urllib.request.urlopen(req, timeout=20) as r:
            raw = tomllib.loads(r.read().decode("utf-8", "replace"))
    except Exception:
        return {}
    out = {}
    for key, value in raw.items():
        if isinstance(value, str):
            v = value.strip()
            if key == "mode":
                out["mode"] = v.lower()
            elif _HEX.match(v):
                out[key] = v
    return out


def art_path(repo: str) -> Path:
    key = hashlib.sha1((repo or "").encode()).hexdigest()[:16]
    return ART / f"{key}.png"


def _rounded(ctx, x, y, w, h, r):
    ctx.new_sub_path()
    ctx.arc(x + w - r, y + r, r, -math.pi / 2, 0)
    ctx.arc(x + w - r, y + h - r, r, 0, math.pi / 2)
    ctx.arc(x + r, y + h - r, r, math.pi / 2, math.pi)
    ctx.arc(x + r, y + r, r, math.pi, 3 * math.pi / 2)
    ctx.close_path()


def render(palette: dict, dest: Path) -> Path | None:
    """Draw a miniature desktop in the theme's colours."""
    if not palette:
        return None
    import cairo

    def pick(*names, default=""):
        for n in names:
            if palette.get(n):
                return palette[n]
        return default

    bg      = _rgb(pick("background", default="#1c1e24"))
    bg_dark = _rgb(pick("dark_background", "background", default="#17191f"))
    bg_dkr  = _rgb(pick("darker_background", "dark_background", default="#111318"))
    bg_lit  = _rgb(pick("lighter_background", "background", default="#2d3038"))
    fg      = _rgb(pick("foreground", default="#f2f3f5"))
    muted   = _rgb(pick("muted", "dark_foreground", "foreground", default="#adb0b9"))
    accent  = _rgb(pick("accent", "blue", default="#78b4ff"))

    try:
        surface = cairo.ImageSurface(cairo.FORMAT_ARGB32, W, H)
        ctx = cairo.Context(surface)

        # Desktop
        ctx.set_source_rgb(*bg)
        ctx.paint()

        # Bar across the top, the way Omarchy wears it
        bar_h = 26
        ctx.set_source_rgb(*bg_dkr)
        ctx.rectangle(0, 0, W, bar_h)
        ctx.fill()
        # Workspace pips: the active one takes the accent
        for i in range(5):
            ctx.set_source_rgb(*(accent if i == 1 else muted))
            _rounded(ctx, 14 + i * 18, bar_h / 2 - 3, 11 if i == 1 else 6, 6, 3)
            ctx.fill()
        # Right-hand indicators
        for i in range(3):
            ctx.set_source_rgb(*muted)
            _rounded(ctx, W - 30 - i * 26, bar_h / 2 - 3, 16, 6, 3)
            ctx.fill()

        # A window, with its own header
        wx, wy, ww, wh = 38, 52, W - 76, H - 108
        ctx.set_source_rgba(0, 0, 0, 0.22)
        _rounded(ctx, wx + 3, wy + 4, ww, wh, 10)
        ctx.fill()
        ctx.set_source_rgb(*bg_lit)
        _rounded(ctx, wx, wy, ww, wh, 10)
        ctx.fill()
        ctx.set_source_rgb(*bg_dark)
        ctx.save()
        _rounded(ctx, wx, wy, ww, wh, 10)
        ctx.clip()
        ctx.rectangle(wx, wy, ww, 24)
        ctx.fill()
        ctx.restore()
        for i in range(3):
            ctx.set_source_rgb(*muted)
            ctx.arc(wx + 14 + i * 13, wy + 12, 3.2, 0, 2 * math.pi)
            ctx.fill()

        # Text lines, long then short, the shape of a paragraph
        for i, frac in enumerate((0.74, 0.56, 0.66, 0.38)):
            ctx.set_source_rgb(*(fg if i == 0 else muted))
            _rounded(ctx, wx + 16, wy + 42 + i * 15, (ww - 32) * frac, 7, 3.5)
            ctx.fill()

        # The accent, as a button
        ctx.set_source_rgb(*accent)
        _rounded(ctx, wx + ww - 92, wy + wh - 32, 76, 20, 10)
        ctx.fill()

        # The ANSI row: the colours a terminal will actually use
        chips = [pick("red"), pick("yellow", "orange"), pick("green"),
                 pick("cyan"), pick("blue"), pick("magenta")]
        chips = [c for c in chips if c]
        if chips:
            cw = 34
            total = len(chips) * (cw + 8) - 8
            x0 = (W - total) / 2
            for i, colour in enumerate(chips):
                ctx.set_source_rgb(*_rgb(colour))
                _rounded(ctx, x0 + i * (cw + 8), H - 36, cw, 14, 7)
                ctx.fill()

        dest.parent.mkdir(parents=True, exist_ok=True)
        tmp = dest.with_suffix(".tmp.png")
        surface.write_to_png(str(tmp))
        tmp.replace(dest)
        return dest
    except Exception:
        return None


def thumbnail(repo: str) -> Path | None:
    """A cached preview for a theme repository, drawing it if needed."""
    dest = art_path(repo)
    if dest.exists() and dest.stat().st_size:
        return dest
    return render(fetch_palette(repo), dest)
