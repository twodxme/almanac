//! Programs that start when you log in: XDG autostart entries.
//!
//! Every desktop that follows the freedesktop autostart spec -- GNOME, KDE,
//! Xfce, and Hyprland sessions run through uwsm or systemd's autostart
//! generator -- starts the `.desktop` files in `$XDG_CONFIG_DIRS/autostart`
//! (usually /etc/xdg/autostart) and `$XDG_CONFIG_HOME/autostart`. A file in the
//! user's directory overrides a system one of the same name.
//!
//! Switching one off follows the spec exactly: a user-directory copy with
//! `Hidden=true`. The system file is never touched, and switching it back on
//! removes only an override Almanac itself wrote (marked `X-Almanac-Override`),
//! so a user's own hand-made entry is edited in place rather than deleted.
//!
//! Port of `autostart.py`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const MARK: &str = "X-Almanac-Override";
/// Almanac's own: shown elsewhere.
pub const OWN: &[&str] = &["io.github.twodxme.Almanac"];

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Entry {
    pub filename: String,
    pub name: String,
    pub comment: String,
    pub icon: String,
    pub enabled: bool,
    /// the file that decides (user copy if any)
    pub path: PathBuf,
    /// the shipped one, when there is one
    pub system: Option<PathBuf>,
    /// the user copy is an override Almanac wrote
    pub ours: bool,
}

pub fn user_dir() -> PathBuf {
    crate::xdg::config_home().join("autostart")
}

pub fn system_dirs() -> Vec<PathBuf> {
    let dirs = std::env::var("XDG_CONFIG_DIRS").ok().filter(|d| !d.is_empty())
        .unwrap_or_else(|| "/etc/xdg".to_owned());
    dirs.split(':').filter(|d| !d.is_empty()).map(|d| Path::new(d).join("autostart")).collect()
}

/// The [Desktop Entry] group's keys. Localised keys are skipped.
#[allow(dead_code)]
fn parse(path: &Path) -> BTreeMap<String, String> {
    let _ = path;
    todo!("port: autostart::_parse")
}

/// Whether this desktop would run it at all (OnlyShowIn / NotShowIn).
#[allow(dead_code)]
fn applies(d: &BTreeMap<String, String>) -> bool {
    let _ = d;
    todo!("port: autostart::_applies")
}

#[allow(dead_code)]
fn on(d: &BTreeMap<String, String>) -> bool {
    let _ = d;
    todo!("port: autostart::_on")
}

pub fn entries() -> Vec<Entry> {
    todo!("port: autostart::entries")
}

/// Switch an entry, the way the spec says to. `Err` where the Python raises
/// OSError.
pub fn set_enabled(e: &Entry, on: bool) -> Result<(), String> {
    let _ = (e, on);
    todo!("port: autostart::set_enabled")
}

#[allow(dead_code)]
fn set_key(path: &Path, key: &str, value: &str) -> Result<(), String> {
    let _ = (path, key, value);
    todo!("port: autostart::_set_key")
}

/// Parity hook: see ARCHITECTURE.md.
pub fn parity(_func: &str, _args: &serde_json::Value) -> Option<serde_json::Value> {
    None
}
