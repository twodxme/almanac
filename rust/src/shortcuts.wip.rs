//! Port of `shortcuts.py`.
//!
//! Keyboard shortcuts that open a plugin's panel, in Hyprland's bindings.lua.
//!
//! The approach is omaplug's (github.com/fross100/omaplug, MIT): check the live
//! bindings for a conflict before writing, write only a block Almanac owns and
//! can recognise again, and undo the change if Hyprland reports a config error
//! after reloading.
//!
//! Only Omarchy's Lua configuration is supported -- ~/.config/hypr/bindings.lua
//! with `o.bind(...)` -- since that is the one file a user is told to put their
//! own bindings in. Anywhere else, `available()` is False and the feature is
//! not offered.
//!
//! A block looks like this, and is the only text this module ever edits:
//!
//! ```text
//!     -- almanac-shortcut-start: lxp.vitals
//!     o.bind("SUPER + ALT + V", "Almanac: lxp.vitals", "omarchy-shell shell toggle lxp.vitals")
//!     -- almanac-shortcut-end: lxp.vitals
//! ```

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

/// Hyprland modifier masks, in the order a combination is written.
pub const MODIFIERS: &[(&str, u32)] = &[("SUPER", 64), ("CTRL", 4), ("ALT", 8), ("SHIFT", 1)];

/// Python `_KEY.fullmatch`: anchored here.
#[allow(dead_code)]
static KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"^(?:[A-Z0-9]|F(?:[1-9]|1[0-2])|SLASH|SPACE|RETURN|TAB|HOME|END|",
        r"INSERT|DELETE|PAGEUP|PAGEDOWN|UP|DOWN|LEFT|RIGHT|COMMA|PERIOD|",
        r"SEMICOLON|APOSTROPHE|MINUS|EQUAL|GRAVE|BACKSLASH|",
        r"BRACKETLEFT|BRACKETRIGHT)$"
    ))
    .unwrap()
});
#[allow(dead_code)]
static ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._-]*$").unwrap());

pub fn bindings_path() -> PathBuf {
    todo!("port: shortcuts::bindings_path")
}

/// Omarchy's Lua bindings file, and Hyprland to ask about live binds.
pub fn available() -> bool {
    todo!("port: shortcuts::available")
}

/// 'super+alt+v' -> ('SUPER + ALT + V', modmask, 'V'). Err (Python ValueError)
/// with the message to show.
pub fn normalise(_value: &str) -> Result<(String, u32, String), String> {
    todo!("port: shortcuts::normalise")
}

#[allow(dead_code)]
fn _block(_pid: &str, _combo: &str) -> String {
    todo!("port: shortcuts::_block")
}

#[allow(dead_code)]
fn _pattern(_pid: &str) -> Regex {
    todo!("port: shortcuts::_pattern")
}

/// The combination Almanac bound for this plugin, or ''. `raw` is the file's
/// text when the caller already read it (None: read it now). Err when the
/// block was edited by hand, or the file cannot be read.
pub fn current(_pid: &str, _raw: Option<&str>) -> Result<String, String> {
    todo!("port: shortcuts::current")
}

/// Why `combo` cannot be used, from Hyprland's live bindings (`hyprctl -j
/// binds`, one JSON object each), or ''. Err when `combo` does not normalise.
pub fn conflict(_binds: &[serde_json::Value], _combo: &str, _pid: &str) -> Result<String, String> {
    todo!("port: shortcuts::conflict")
}

/// (app.py calls this.) bindings.lua's text and permission bits,
/// opened without following a symlink. Err (OSError / ValueError) when it is
/// missing, not a regular file the user owns, or over 1 MiB.
pub fn _read() -> Result<(String, u32), String> {
    todo!("port: shortcuts::_read")
}

/// Atomically replace bindings.lua with `raw`, keeping `mode`.
#[allow(dead_code)]
fn _write(_raw: &str, _mode: u32) -> Result<(), String> {
    todo!("port: shortcuts::_write")
}

/// `hyprctl *args`, stripped stdout; Err with its message when it fails.
pub fn _hyprctl(_args: &[&str]) -> Result<String, String> {
    todo!("port: shortcuts::_hyprctl")
}

/// Bind (or with None, unbind) a plugin's shortcut. Returns the result.
///
/// Reloads Hyprland and checks for config errors; on any, the previous file
/// is restored and Hyprland reloaded again, and Err (ValueError) says why.
pub fn set_shortcut(pid: &str, combo: Option<&str>) -> Result<String, String> {
    set_shortcut_with(pid, combo, &mut |args| _hyprctl(args))
}

/// `set_shortcut` with `hyprctl` supplied by the caller -- the seam the
/// Python tests reach with `mock.patch.object(shortcuts, "_hyprctl")`.
pub fn set_shortcut_with(
    _pid: &str,
    _combo: Option<&str>,
    _hyprctl: &mut dyn FnMut(&[&str]) -> Result<String, String>,
) -> Result<String, String> {
    todo!("port: shortcuts::set_shortcut")
}

/// Only a plugin with something to open can be given a shortcut to it.
pub fn has_panel(_plugin_dir: &Path) -> bool {
    todo!("port: shortcuts::has_panel")
}

/// Parity hook: see ARCHITECTURE.md.
pub fn parity(_func: &str, _args: &serde_json::Value) -> Option<serde_json::Value> {
    None
}
