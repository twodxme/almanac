"""Wearing whatever theme Omarchy is wearing.

Omarchy publishes the active theme as a symlink at
`~/.local/state/omarchy/current/theme`, and every theme ships a `colors.toml`
with a named palette — accent, three background depths, four foreground
weights, and the eight ANSI hues. Almanac reads that and restates it as
libadwaita's named colours, so the window re-skins with the rest of the
desktop instead of being the one app still wearing stock Adwaita.

Two design decisions worth stating:

* **Nothing here is Almanac-specific.** The palette is mapped onto
  `@accent_bg_color`, `@window_bg_color`, `@card_bg_color` and the rest, which
  every libadwaita widget already honours. Almanac's own CSS is written against
  those same names, so it follows for free and a new widget inherits the theme
  without anyone remembering to theme it.
* **Absence is a valid state.** On a machine with no Omarchy — which is most
  machines — there is no colours file, this does nothing, and GTK's own theme
  shows through. Theming is an enhancement, never a requirement.

The theme is watched, not sampled once: `omarchy theme set` swaps the symlink
and the window follows within a frame or two.
"""
from __future__ import annotations

import os
import re
from pathlib import Path

import tomllib

STATE = Path(os.environ.get("XDG_STATE_HOME", Path.home() / ".local/state"))
CURRENT = STATE / "omarchy/current/theme"
COLORS = "colors.toml"

_HEX = re.compile(r"^#(?:[0-9a-fA-F]{3}|[0-9a-fA-F]{6})$")


def theme_dir() -> Path | None:
    """The active theme's directory, or None when Omarchy is not in use."""
    try:
        if CURRENT.exists():
            return CURRENT.resolve()
    except OSError:
        pass
    return None


def background() -> Path | None:
    """The wallpaper Omarchy is currently showing, if there is one.

    Used as the hero on the Featured page: it is already on disk, it is
    already the thing the user is looking at behind this window, and it
    changes with the theme for free.
    """
    link = CURRENT.parent / "background"
    try:
        if link.exists():
            target = link.resolve()
            return target if target.is_file() else None
    except OSError:
        pass
    return None


def colors_path() -> Path | None:
    d = theme_dir()
    p = d / COLORS if d else None
    return p if (p and p.is_file()) else None


def load_palette() -> dict:
    """The theme's named colours, or {} when there is no theme to read.

    Values are validated as hex: a malformed colours file should leave the
    app looking like stock Adwaita, not inject broken CSS into every widget.
    """
    path = colors_path()
    if not path:
        return {}
    try:
        with open(path, "rb") as f:
            raw = tomllib.load(f)
    except (OSError, ValueError):
        return {}

    out = {}
    for key, value in raw.items():
        if isinstance(value, str):
            if key == "mode":
                out["mode"] = value.strip().lower()
            elif _HEX.match(value.strip()):
                out[key] = value.strip()
    return out


def _luminance(value: str) -> float:
    """WCAG relative luminance of a hex colour."""
    h = (value or "").lstrip("#")
    if len(h) == 3:
        h = "".join(c * 2 for c in h)
    if len(h) != 6:
        return 0.0
    try:
        chan = [int(h[i:i + 2], 16) / 255 for i in (0, 2, 4)]
    except ValueError:
        return 0.0
    conv = [c / 12.92 if c <= 0.03928 else ((c + 0.055) / 1.055) ** 2.4
            for c in chan]
    return 0.2126 * conv[0] + 0.7152 * conv[1] + 0.0722 * conv[2]


def contrast(a: str, b: str) -> float:
    la, lb = _luminance(a), _luminance(b)
    hi, lo = max(la, lb), min(la, lb)
    return (hi + 0.05) / (lo + 0.05)


# Contrast targets. WCAG AA asks 4.5:1 of body text; secondary text here is
# often small (captions, pills), so it is held to more than the minimum.
TEXT_TARGET = 5.5        # secondary and accent-coloured text on any surface
BODY_TARGET = 7.0        # primary text: WCAG AAA, the most legible tier


def _hex6(value: str) -> str:
    h = (value or "").lstrip("#")
    if len(h) == 3:
        h = "".join(c * 2 for c in h)
    return "#" + h.lower() if len(h) == 6 else ""


def mix(a: str, b: str, t: float) -> str:
    """`a` moved a fraction `t` of the way to `b`."""
    a, b = _hex6(a), _hex6(b)
    if not a or not b:
        return a or b
    ca = [int(a[i:i + 2], 16) for i in (1, 3, 5)]
    cb = [int(b[i:i + 2], 16) for i in (1, 3, 5)]
    return "#" + "".join(f"{round(x + (y - x) * t):02x}" for x, y in zip(ca, cb))


def legible(color: str, toward: str, surfaces: list[str], target: float) -> str:
    """`color`, moved toward `toward` only as far as it takes to reach
    `target` contrast on every surface -- the theme's hue, kept wherever it
    already reads."""
    if not color:
        return ""
    for step in range(0, 51):
        c = mix(color, toward, step / 50)
        if all(contrast(c, s) >= target for s in surfaces if s):
            return c
    # The theme's own text is not enough (a light theme's foreground can
    # itself be ~5:1): go on toward black or white, whichever the surface
    # wants.
    extreme = readable_on(next((s for s in surfaces if s), "#000000"))
    for step in range(1, 51):
        c = mix(toward, extreme, step / 50)
        if all(contrast(c, s) >= target for s in surfaces if s):
            return c
    return extreme


def dim_opacity(fg: str, surfaces: list[str], target: float) -> int:
    """The faintest libadwaita can draw dimmed text (an opacity of the
    foreground) while it still reaches `target` on every surface, in percent.
    Never fainter than libadwaita's own 55%."""
    for pct in range(55, 101):
        if all(contrast(mix(s, fg, pct / 100), s) >= target for s in surfaces if s):
            return pct
    return 100


# Status pills are solid: the status colour as the fill, text measured against
# it. Tinted pills kept the colour in the text, and on a low-contrast palette
# that text could not be both coloured and readable (Everforest's red read at
# 3.6:1); solid fills carry the colour and leave the text free to be legible.
PILL_FILLS = (
    ("pill-crit", "error"), ("pill-warn", "warning"),
    ("pill-ok", "success"), ("pill-accent", "accent"),
    ("count-badge.is-warn", "warning"),
)


def pill_css(fills: dict[str, str], prefer: tuple[str, ...] | list[str] = ()) -> str:
    """Solid pill rules: each fill with the text that reads best on it.

    `prefer` are the theme's own dark and light text colours; one is used
    when it reaches 7:1, so the pill still looks like the theme, and black or
    white otherwise.
    """
    lines = []
    for cls, role in PILL_FILLS:
        fill = _hex6(fills.get(role, ""))
        if not fill:
            continue
        own = [c for c in prefer if c and contrast(c, fill) >= 7]
        text = (max(own, key=lambda c: contrast(c, fill)) if own
                else readable_on(fill))
        lines.append(f".{cls} {{ background: {fill}; color: {text}; }}")
    return "\n".join(lines) + ("\n" if lines else "")


# Today's picks sit on the card colour washed toward the accent. That wash
# lowers the contrast of everything on it, so the card's text is measured
# against the strongest point of it rather than borrowed from a plain card.
PICK_WASH = 0.12


def pick_css(card: str, fg: str, accent: str) -> str:
    """Text colours for the picks' accent-washed card."""
    card, fg, accent = _hex6(card), _hex6(fg), _hex6(accent)
    if not (card and fg and accent):
        return ""
    wash = mix(card, accent, PICK_WASH)
    text = legible(fg, readable_on(wash), [wash], BODY_TARGET)
    eyebrow = legible(accent, text, [wash], TEXT_TARGET)
    return (f".pick-card {{ color: {text}; }}\n"
            f".pick-eyebrow {{ color: {eyebrow}; }}\n")


def flatten(rgba: tuple, under: tuple | None) -> str:
    """A colour with alpha (0-1 channels), as the opaque hex it shows as
    over `under` (an opaque rgb, 0-1). Contrast is only defined for what is
    actually on screen."""
    r, g, b, a = rgba
    if under is not None and a < 1:
        r, g, b = (c * a + u * (1 - a) for c, u in zip((r, g, b), under))
    return "#" + "".join(f"{round(c * 255):02x}" for c in (r, g, b))


def readable_on(background: str) -> str:
    """Black or white, whichever actually reads better on this colour.

    Measured, not guessed from a luminance cutoff: a mid-blue accent sits
    close enough to any threshold you pick that the wrong side of it costs
    four times the contrast. Comparing both candidates is exact and cannot
    be off by one theme.

    The theme's own darkest background was used here before, which is only
    right when the accent happens to be light.
    """
    if not background:
        return ""
    return max(("#000000", "#ffffff"),
               key=lambda candidate: contrast(candidate, background))


def _pick(p: dict, *names: str) -> str:
    for n in names:
        if p.get(n):
            return p[n]
    return ""


def css_for(palette: dict) -> str:
    """Restate an Omarchy palette as libadwaita's named colours.

    Only names the theme actually defines are emitted. A theme that ships no
    `green` keeps libadwaita's success colour rather than getting a guess.
    """
    if not palette:
        return ""

    bg      = _pick(palette, "background")
    bg_dark = _pick(palette, "dark_background", "background")
    bg_dkr  = _pick(palette, "darker_background", "dark_background", "background")
    bg_lit  = _pick(palette, "lighter_background", "background")
    fg      = _pick(palette, "foreground")
    # Primary text held to 7:1 on the lightest surface it is drawn on (the
    # cards); a theme that already reads that well is left exactly as it is.
    if fg:
        fg = legible(fg, readable_on(bg_lit or bg), [bg_lit, bg], BODY_TARGET)
    fg_dim  = _pick(palette, "dark_foreground", "muted", "foreground")
    accent  = _pick(palette, "accent", "blue")
    select  = _pick(palette, "selection")

    # (libadwaita name, value). Order matters only for readability.
    pairs = [
        ("accent_bg_color", accent),
        ("accent_color", accent),
        ("accent_fg_color", readable_on(accent) if accent else ""),

        ("window_bg_color", bg),
        ("window_fg_color", fg),
        ("view_bg_color", bg_dark),
        ("view_fg_color", fg),
        ("headerbar_bg_color", bg_dark),
        ("headerbar_fg_color", fg),
        ("sidebar_bg_color", bg_dkr),
        ("sidebar_fg_color", fg),
        ("secondary_sidebar_bg_color", bg_dkr),
        ("card_bg_color", bg_lit),
        ("card_fg_color", fg),
        ("dialog_bg_color", bg_lit),
        ("dialog_fg_color", fg),
        ("popover_bg_color", bg_lit),
        ("popover_fg_color", fg),

        ("success_color", _pick(palette, "green")),
        ("success_bg_color", _pick(palette, "green")),
        ("warning_color", _pick(palette, "yellow", "orange")),
        ("warning_bg_color", _pick(palette, "yellow", "orange")),
        ("error_color", _pick(palette, "red")),
        ("error_bg_color", _pick(palette, "red")),
        ("destructive_color", _pick(palette, "red")),
        ("destructive_bg_color", _pick(palette, "red")),

        # Foregrounds for the solid semantic fills, measured against each
        # fill rather than assumed to be white.
        ("error_fg_color", readable_on(_pick(palette, "red"))),
        ("warning_fg_color", readable_on(_pick(palette, "yellow", "orange"))),
        ("success_fg_color", readable_on(_pick(palette, "green"))),
    ]

    # Text colours, measured. A theme's accent and semantic colours are made
    # for fills; as text on its own backgrounds they can fall short (in
    # Everforest red text read at 3.6:1 in a pill). libadwaita keeps the text
    # variants (accent_color, success_color…) apart from the fills
    # (accent_bg_color…), so only the text is lifted, toward the foreground,
    # and only as far as it needs to go.
    surfaces = [bg_lit, bg]
    css_pairs_raw = list(pairs)
    if fg:
        text = ("accent_color", "success_color", "warning_color",
                "error_color", "destructive_color")
        pairs = [(name, legible(value, fg, surfaces, TEXT_TARGET)
                  if name in text and value else value)
                 for name, value in pairs]

    lines = [f"@define-color {name} {value};"
             for name, value in pairs if value]

    # Dimmed text in libadwaita is the foreground at --dim-opacity (55%).
    # On a low-contrast palette that is unreadable -- 3.1:1 on Everforest's
    # cards -- so the opacity is computed from the palette instead. (The
    # theme's "dark_foreground" used to be painted here as dim text; it is a
    # terminal colour, and read at 1.5:1.)
    if fg and bg:
        lines.append(f":root {{ --dim-opacity: "
                     f"{dim_opacity(fg, surfaces, TEXT_TARGET)}%; }}")
        lines.append(pick_css(bg_lit or bg, fg, accent).rstrip("\n"))
        raw = dict(css_pairs_raw)
        lines.append(pill_css({"error": raw.get("error_bg_color", ""),
                               "warning": raw.get("warning_bg_color", ""),
                               "success": raw.get("success_bg_color", ""),
                               "accent": accent},
                              prefer=[bg_dkr, fg]).rstrip("\n"))
    if select:
        lines.append(f"@define-color accent_hover_color {select};")
    return "\n".join(lines) + "\n"


def is_dark(palette: dict) -> bool | None:
    """True/False from the theme's own `mode`, or None when it says nothing."""
    mode = palette.get("mode")
    if mode in ("dark", "light"):
        return mode == "dark"
    return None


def theme_name() -> str:
    """The theme's display name.

    `current/theme` is not always a symlink into the themes directory, so the
    directory name can be the useless literal "theme". Omarchy writes the real
    name next to it, which costs a file read instead of a subprocess.
    """
    try:
        name = (CURRENT.parent / "theme.name").read_text().strip()
        if name:
            # The file holds the slug ("apple-dark"); the display name is
            # what the theme picker shows ("Apple Dark").
            return name.replace("-", " ").replace("_", " ").title() \
                if name.islower() else name
    except OSError:
        pass
    import shutil
    import subprocess
    if shutil.which("omarchy"):
        try:
            out = subprocess.run(["omarchy", "theme", "current"],
                                 capture_output=True, text=True, timeout=10)
            lines = (out.stdout or "").strip().splitlines()
            if out.returncode == 0 and lines and lines[0]:
                return lines[0]
        except (OSError, subprocess.SubprocessError):
            pass
    d = theme_dir()
    return d.name if d else ""


def watch(on_change) -> list:
    """Call `on_change()` whenever the active theme changes.

    Both the pointer and the file it points at are watched: `omarchy theme
    set` replaces the symlink, while editing a theme in place rewrites
    colors.toml without the pointer moving.
    """
    from gi.repository import Gio

    monitors = []
    targets = [CURRENT.parent, CURRENT]
    colors = colors_path()
    if colors:
        targets.append(colors)

    for target in targets:
        try:
            gfile = Gio.File.new_for_path(str(target))
            kind = (Gio.FileMonitor.directory if target.is_dir()
                    else Gio.FileMonitor.file)
            monitor = kind(gfile, Gio.FileMonitorFlags.WATCH_MOVES, None)
        except Exception:                               # noqa: BLE001
            continue
        monitor.connect("changed", lambda *_a: on_change())
        monitors.append(monitor)
    # Returned so the caller can keep them alive; a dropped monitor stops
    # emitting without any error.
    return monitors


def describe() -> str:
    d = theme_dir()
    if not d:
        return "No Omarchy theme detected — using the system GTK theme"
    palette = load_palette()
    name = theme_name() or d.name
    if not palette:
        return f"{name} (no readable colors.toml — using the system theme)"
    return f"Following the Omarchy theme: {name} ({palette.get('mode', '?')})"
