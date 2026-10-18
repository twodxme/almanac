//! Port of `theming.py`.
//!
//! Wearing whatever theme Omarchy is wearing.
//!
//! Omarchy publishes the active theme as a symlink at
//! `~/.local/state/omarchy/current/theme`, and every theme ships a `colors.toml`
//! with a named palette — accent, three background depths, four foreground
//! weights, and the eight ANSI hues. Almanac reads that and restates it as
//! libadwaita's named colours, so the window re-skins with the rest of the
//! desktop instead of being the one app still wearing stock Adwaita.
//!
//! Two design decisions worth stating:
//!
//! * **Nothing here is Almanac-specific.** The palette is mapped onto
//!   `@accent_bg_color`, `@window_bg_color`, `@card_bg_color` and the rest, which
//!   every libadwaita widget already honours. Almanac's own CSS is written against
//!   those same names, so it follows for free and a new widget inherits the theme
//!   without anyone remembering to theme it.
//! * **Absence is a valid state.** On a machine with no Omarchy — which is most
//!   machines — there is no colours file, this does nothing, and GTK's own theme
//!   shows through. Theming is an enhancement, never a requirement.
//!
//! The theme is watched, not sampled once: `omarchy theme set` swaps the symlink
//! and the window follows within a frame or two.

use std::collections::BTreeMap;
use std::path::PathBuf;

use gtk::gio;

/// `STATE`: `$XDG_STATE_HOME`, or `~/.local/state`. A function rather than a
/// constant because Rust has no import-time evaluation; read at call time.
pub fn state() -> PathBuf {
    match std::env::var_os("XDG_STATE_HOME") {
        Some(v) => PathBuf::from(v),
        None => crate::util::home().join(".local/state"),
    }
}

/// `CURRENT`: the symlink Omarchy points at the active theme.
pub fn current() -> PathBuf {
    state().join("omarchy/current/theme")
}

pub const COLORS: &str = "colors.toml";

/// `_HEX` as a pattern string: `#rgb` or `#rrggbb`.
pub const HEX_PATTERN: &str = r"^#(?:[0-9a-fA-F]{3}|[0-9a-fA-F]{6})$";

/// The active theme's directory, or None when Omarchy is not in use.
pub fn theme_dir() -> Option<PathBuf> {
    todo!("port: theming::theme_dir")
}

/// The wallpaper Omarchy is currently showing, if there is one.
///
/// Used as the hero on the Featured page: it is already on disk, it is
/// already the thing the user is looking at behind this window, and it
/// changes with the theme for free.
pub fn background() -> Option<PathBuf> {
    todo!("port: theming::background")
}

pub fn colors_path() -> Option<PathBuf> {
    todo!("port: theming::colors_path")
}

/// The theme's named colours, or {} when there is no theme to read.
///
/// Values are validated as hex: a malformed colours file should leave the
/// app looking like stock Adwaita, not inject broken CSS into every widget.
pub fn load_palette() -> BTreeMap<String, String> {
    todo!("port: theming::load_palette")
}

/// WCAG relative luminance of a hex colour.
///
/// (`_luminance` in the Python; the tests call it, so it is public here.)
pub fn luminance(value: &str) -> f64 {
    let _ = value;
    todo!("port: theming::_luminance")
}

pub fn contrast(a: &str, b: &str) -> f64 {
    let _ = (a, b);
    todo!("port: theming::contrast")
}

// Contrast targets. WCAG AA asks 4.5:1 of body text; secondary text here is
// often small (captions, pills), so it is held to more than the minimum.
/// Secondary and accent-coloured text on any surface.
pub const TEXT_TARGET: f64 = 5.5;
/// Primary text: WCAG AAA, the most legible tier.
pub const BODY_TARGET: f64 = 7.0;

/// `_hex6`: a hex colour normalised to lowercase `#rrggbb`, or "" when it
/// is not one.
pub fn hex6(value: &str) -> String {
    let _ = value;
    todo!("port: theming::_hex6")
}

/// `a` moved a fraction `t` of the way to `b`.
pub fn mix(a: &str, b: &str, t: f64) -> String {
    let _ = (a, b, t);
    todo!("port: theming::mix")
}

/// `color`, moved toward `toward` only as far as it takes to reach
/// `target` contrast on every surface -- the theme's hue, kept wherever it
/// already reads.
pub fn legible(color: &str, toward: &str, surfaces: &[&str], target: f64) -> String {
    let _ = (color, toward, surfaces, target);
    todo!("port: theming::legible")
}

/// The faintest libadwaita can draw dimmed text (an opacity of the
/// foreground) while it still reaches `target` on every surface, in percent.
/// Never fainter than libadwaita's own 55%.
pub fn dim_opacity(fg: &str, surfaces: &[&str], target: f64) -> u32 {
    let _ = (fg, surfaces, target);
    todo!("port: theming::dim_opacity")
}

// Status pills are solid: the status colour as the fill, text measured against
// it. Tinted pills kept the colour in the text, and on a low-contrast palette
// that text could not be both coloured and readable (Everforest's red read at
// 3.6:1); solid fills carry the colour and leave the text free to be legible.
/// (CSS class, fill role) pairs.
pub const PILL_FILLS: &[(&str, &str)] = &[
    ("pill-crit", "error"),
    ("pill-warn", "warning"),
    ("pill-ok", "success"),
    ("pill-accent", "accent"),
    ("count-badge.is-warn", "warning"),
];

/// Solid pill rules: each fill with the text that reads best on it.
///
/// `prefer` are the theme's own dark and light text colours; one is used
/// when it reaches 7:1, so the pill still looks like the theme, and black or
/// white otherwise. (Python's default `prefer=()` is `&[]`.)
pub fn pill_css(fills: &BTreeMap<String, String>, prefer: &[&str]) -> String {
    let _ = (fills, prefer);
    todo!("port: theming::pill_css")
}

// Today's picks sit on the card colour washed toward the accent. That wash
// lowers the contrast of everything on it, so the card's text is measured
// against the strongest point of it rather than borrowed from a plain card.
pub const PICK_WASH: f64 = 0.12;

/// Text colours for the picks' accent-washed card.
pub fn pick_css(card: &str, fg: &str, accent: &str) -> String {
    let _ = (card, fg, accent);
    todo!("port: theming::pick_css")
}

/// A colour with alpha (0-1 channels), as the opaque hex it shows as
/// over `under` (an opaque rgb, 0-1). Contrast is only defined for what is
/// actually on screen.
pub fn flatten(rgba: (f64, f64, f64, f64), under: Option<(f64, f64, f64)>) -> String {
    let _ = (rgba, under);
    todo!("port: theming::flatten")
}

/// Black or white, whichever actually reads better on this colour.
///
/// Measured, not guessed from a luminance cutoff: a mid-blue accent sits
/// close enough to any threshold you pick that the wrong side of it costs
/// four times the contrast. Comparing both candidates is exact and cannot
/// be off by one theme.
///
/// The theme's own darkest background was used here before, which is only
/// right when the accent happens to be light.
pub fn readable_on(background: &str) -> String {
    let _ = background;
    todo!("port: theming::readable_on")
}

/// Restate an Omarchy palette as libadwaita's named colours.
///
/// Only names the theme actually defines are emitted. A theme that ships no
/// `green` keeps libadwaita's success colour rather than getting a guess.
pub fn css_for(palette: &BTreeMap<String, String>) -> String {
    let _ = palette;
    todo!("port: theming::css_for")
}

/// True/False from the theme's own `mode`, or None when it says nothing.
pub fn is_dark(palette: &BTreeMap<String, String>) -> Option<bool> {
    match palette.get("mode").map(String::as_str) {
        Some("dark") => Some(true),
        Some("light") => Some(false),
        _ => None,
    }
}

/// The theme's display name.
///
/// `current/theme` is not always a symlink into the themes directory, so the
/// directory name can be the useless literal "theme". Omarchy writes the real
/// name next to it, which costs a file read instead of a subprocess.
pub fn theme_name() -> String {
    todo!("port: theming::theme_name")
}

/// Call `on_change()` whenever the active theme changes.
///
/// Both the pointer and the file it points at are watched: `omarchy theme
/// set` replaces the symlink, while editing a theme in place rewrites
/// colors.toml without the pointer moving.
///
/// The monitors are returned so the caller can keep them alive; a dropped
/// monitor stops emitting without any error.
pub fn watch(on_change: impl Fn() + 'static) -> Vec<gio::FileMonitor> {
    let _ = on_change;
    todo!("port: theming::watch")
}

pub fn describe() -> String {
    todo!("port: theming::describe")
}


/// Parity hook: see ARCHITECTURE.md.
pub fn parity(_func: &str, _args: &serde_json::Value) -> Option<serde_json::Value> {
    None
}
