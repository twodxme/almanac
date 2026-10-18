//! Port of `watcher.py`.
//!
//! `almanac --guard`: notice settings resets while the window is closed.
//!
//! The window watches for a config file being reset to its defaults, but only
//! while it is open -- and the reset that prompted this happened with it
//! closed. This is the same check as a small background process: no window, no
//! GTK, a few directory monitors on a GLib main loop, idle until a tool writes a
//! `<file>.bak.<epoch>`.
//!
//! It starts with the session through XDG autostart, the one mechanism every
//! desktop honours (systemd-based sessions run it through their autostart
//! generator; others read the directory directly). Turning it on and off is a
//! switch in Preferences, which writes or removes the autostart entry.
//!
//! A reset is announced once. The backups already reported are remembered in
//! the XDG state directory, so a restart does not repeat old news.

use std::collections::BTreeSet;
use std::path::PathBuf;

pub const ENTRY_NAME: &str = "io.github.twodxme.Almanac.Guard.desktop";

pub fn autostart_path() -> PathBuf {
    crate::xdg::config_home().join("autostart").join(ENTRY_NAME)
}

#[allow(dead_code)]
fn reported_path() -> PathBuf {
    crate::xdg::state_home().join("almanac").join("guard-reported.json")
}

/// How to start Almanac from an autostart entry, however it is installed.
pub fn launcher() -> Vec<String> {
    todo!("port: watcher::launcher")
}

pub fn enabled() -> bool {
    autostart_path().is_file()
}

/// Writes the autostart entry and, with `start_now`, starts a guard unless
/// one is running. A failed write is an `Err` (the Python raises OSError,
/// which the Preferences switch reports).
pub fn enable(start_now: bool) -> Result<(), String> {
    let _ = start_now;
    todo!("port: watcher::enable")
}

/// Removes the autostart entry and stops a running guard.
pub fn disable() -> Result<(), String> {
    todo!("port: watcher::disable")
}

/// Desktop Entry Exec quoting: reserved characters need double quotes.
/// (`_quote` in the Python.)
pub fn quote(arg: &str) -> String {
    const RESERVED: &str = " \t\n\"'\\><~|&;$*?#()`";
    if arg.chars().any(|c| RESERVED.contains(c)) {
        let inner = arg
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('`', "\\`")
            .replace('$', "\\$");
        return format!("\"{inner}\"");
    }
    arg.to_string()
}

#[allow(dead_code)]
fn pid_path() -> PathBuf {
    todo!("port: watcher::_pid_path")
}

/// PID of a guard that is running, or 0.
pub fn running() -> i32 {
    todo!("port: watcher::running")
}

#[allow(dead_code)]
fn load_reported() -> BTreeSet<String> {
    todo!("port: watcher::_load_reported")
}

#[allow(dead_code)]
fn save_reported(seen: &BTreeSet<String>) {
    let _ = seen;
    todo!("port: watcher::_save_reported")
}

/// Notify about resets not already reported. Returns the updated set.
pub fn announce(found: &[crate::guard::Reset], seen: &BTreeSet<String>) -> BTreeSet<String> {
    let _ = (found, seen);
    todo!("port: watcher::announce")
}

// ---- plugin updates --------------------------------------------------------

pub const PLUGIN_CHECK_EVERY: u64 = 6 * 3600;

#[allow(dead_code)]
fn plugins_seen_path() -> PathBuf {
    crate::xdg::state_home().join("almanac").join("plugin-updates-notified.json")
}

/// Plugins with a clean update waiting, fetched as Omarchy fetches.
pub fn plugin_updates() -> Vec<String> {
    todo!("port: watcher::plugin_updates")
}

/// Notify only when something new is waiting -- not the same list again,
/// and not when the count merely fell because some were updated.
pub fn announce_plugin_updates(waiting: &[String], last: &BTreeSet<String>) -> BTreeSet<String> {
    let _ = (waiting, last);
    todo!("port: watcher::announce_plugin_updates")
}

/// A notification whose action opens Almanac on the relevant page.
///
/// Python defaults: `page="health"`, `urgency="critical"`.
#[allow(dead_code)]
fn notify(title: &str, body: &str, page: &str, urgency: &str) {
    let _ = (title, body, page, urgency);
    todo!("port: watcher::_notify")
}

/// `almanac --guard`. Returns the process exit status.
pub fn main() -> i32 {
    todo!("port: watcher::main")
}

/// Parity hook: see ARCHITECTURE.md.
pub fn parity(_func: &str, _args: &serde_json::Value) -> Option<serde_json::Value> {
    None
}
