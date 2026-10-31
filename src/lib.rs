//! Almanac, in Rust: a line-for-line port of the Python app.
//!
//! Each module below is the port of the Python module of the same name and
//! must give the same answers: `parity/run.py` runs both on the same inputs
//! and fails on any difference.

pub mod agents;
pub mod ai;
pub mod appimage;
pub mod autorun;
pub mod autostart;
pub mod autoupdate;
pub mod demo;
pub mod featured;
pub mod ghcache;
pub mod guard;
pub mod history;
pub mod housekeeping;
pub mod i18n;
pub mod icons;
pub mod inventory;
pub mod maintenance;
pub mod mimeguard;
pub mod mirror;
pub mod news;
pub mod patches;
pub mod picks;
pub mod plugin_setup;
pub mod plugins;
pub mod postupdate;
pub mod restart;
pub mod security;
pub mod selfupdate;
pub mod services;
pub mod settings;
pub mod shortcuts;
pub mod snapshots;
pub mod sources;
pub mod store;
pub mod tasks;
pub mod themeart;
pub mod theming;
pub mod verify;
pub mod watcher;
pub mod xdg;

pub mod ui;
pub mod util;

/// GTK for a widget test, or false to skip: a package build's check() has
/// no display, and GTK belongs to whichever test thread took it first.
///
/// The lock makes taking it one step. gtk-rs marks GTK taken only once
/// `gtk_init_check` returns, so two tests starting together both ran it,
/// and GTK's init racing itself aborted the test binary (an assertion in
/// the fcitx5 input-method module as GIO loaded it). Behind the lock, a
/// later thread sees GTK taken and gets the panic `catch_unwind` expects.
#[cfg(test)]
pub(crate) fn test_gtk() -> bool {
    static INIT: std::sync::Mutex<()> = std::sync::Mutex::new(());
    // Looking first keeps adw::init's NULL-display criticals out of the
    // build log; CI's Broadway sets GDK_BACKEND and neither variable.
    let set = |var| std::env::var_os(var).is_some_and(|v| !v.is_empty());
    let broadway = std::env::var("GDK_BACKEND").is_ok_and(|b| b.contains("broadway"));
    if !(set("WAYLAND_DISPLAY") || set("DISPLAY") || broadway) {
        return false;
    }
    let _taking = INIT.lock().unwrap_or_else(|e| e.into_inner());
    matches!(std::panic::catch_unwind(adw::init), Ok(Ok(())))
        && gtk::gdk::Display::default().is_some()
}
