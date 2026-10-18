//! Where things live, and how to reach a terminal, on anyone's desktop.
//!
//! Almanac is published for anyone's machine, so nothing here may assume the one
//! it was written on: base directories follow the XDG Base Directory spec (a
//! user can move any of them), and a terminal is found the way the desktop
//! would find one rather than by trying three favourites.
//!
//! Port of `xdg.py`.

use std::path::{Path, PathBuf};

use crate::util;

fn base(var: &str, default: &str) -> PathBuf {
    let value = std::env::var(var).unwrap_or_default();
    // The spec: a relative path is invalid and must be ignored.
    if !value.is_empty() && Path::new(&value).is_absolute() {
        PathBuf::from(value)
    } else {
        util::home().join(default)
    }
}

pub fn data_home() -> PathBuf {
    base("XDG_DATA_HOME", ".local/share")
}

pub fn config_home() -> PathBuf {
    base("XDG_CONFIG_HOME", ".config")
}

pub fn state_home() -> PathBuf {
    base("XDG_STATE_HOME", ".local/state")
}

pub fn cache_home() -> PathBuf {
    base("XDG_CACHE_HOME", ".cache")
}

/// ~/.local/bin: not in the spec's variables, but named by it.
pub fn bin_home() -> PathBuf {
    util::home().join(".local/bin")
}

/// How each terminal takes a command to run. Most use -e; several do not, and
/// guessing wrong opens an empty shell instead of running anything.
#[allow(dead_code)]
const TERMINALS: &[(&str, &[&str])] = &[
    ("foot", &[]),
    ("alacritty", &["-e"]),
    ("kitty", &[]),
    ("ghostty", &["-e"]),
    ("wezterm", &["start", "--"]),
    ("konsole", &["-e"]),
    ("gnome-terminal", &["--wait", "--"]),
    ("kgx", &["-e"]),
    ("xfce4-terminal", &["--disable-server", "-x"]),
    ("tilix", &["-e"]),
    ("xterm", &["-e"]),
];

/// argv that runs `command` in a terminal window and waits for it.
///
/// `xdg-terminal-exec` first: it is the freedesktop proposal for exactly
/// this, honours the user's chosen terminal, and Omarchy and several other
/// distributions ship it. Then $TERMINAL, then whatever is installed.
pub fn terminal_argv<S: AsRef<str>>(command: &[S]) -> Option<Vec<String>> {
    let _ = command;
    todo!("port: xdg::terminal_argv")
}

/// The full-upgrade command this system's own tooling expects.
pub fn system_update_command() -> String {
    todo!("port: xdg::system_update_command")
}

/// Parity hook: see ARCHITECTURE.md.
pub fn parity(_func: &str, _args: &serde_json::Value) -> Option<serde_json::Value> {
    None
}
