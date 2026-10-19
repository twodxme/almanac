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
pub mod i18n;
pub mod icons;
pub mod inventory;
pub mod maintenance;
pub mod mirror;
pub mod mimeguard;
pub mod patches;
pub mod picks;
pub mod plugin_setup;
pub mod plugins;
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

pub mod util;
pub mod ui;
