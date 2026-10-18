//! Almanac, in Rust: a line-for-line port of the Python app.
//!
//! Each module below is the port of the Python module of the same name and
//! must give the same answers: `parity/run.py` runs both on the same inputs
//! and fails on any difference.
//!
//! While porting, a module lives in two files: `x.rs` (the last version that
//! compiled and passed) and `x.wip.rs` (being worked on). The `wip-x` feature
//! switches the crate to the work-in-progress copy, so one half-finished
//! module never breaks the build for everyone else. See ARCHITECTURE.md.

#[cfg(not(feature = "wip-agents"))]
#[path = "agents.rs"]
pub mod agents;
#[cfg(feature = "wip-agents")]
#[path = "agents.wip.rs"]
pub mod agents;

#[cfg(not(feature = "wip-ai"))]
#[path = "ai.rs"]
pub mod ai;
#[cfg(feature = "wip-ai")]
#[path = "ai.wip.rs"]
pub mod ai;

#[cfg(not(feature = "wip-appimage"))]
#[path = "appimage.rs"]
pub mod appimage;
#[cfg(feature = "wip-appimage")]
#[path = "appimage.wip.rs"]
pub mod appimage;

#[cfg(not(feature = "wip-autorun"))]
#[path = "autorun.rs"]
pub mod autorun;
#[cfg(feature = "wip-autorun")]
#[path = "autorun.wip.rs"]
pub mod autorun;

#[cfg(not(feature = "wip-autostart"))]
#[path = "autostart.rs"]
pub mod autostart;
#[cfg(feature = "wip-autostart")]
#[path = "autostart.wip.rs"]
pub mod autostart;

#[cfg(not(feature = "wip-autoupdate"))]
#[path = "autoupdate.rs"]
pub mod autoupdate;
#[cfg(feature = "wip-autoupdate")]
#[path = "autoupdate.wip.rs"]
pub mod autoupdate;

#[cfg(not(feature = "wip-demo"))]
#[path = "demo.rs"]
pub mod demo;
#[cfg(feature = "wip-demo")]
#[path = "demo.wip.rs"]
pub mod demo;

#[cfg(not(feature = "wip-featured"))]
#[path = "featured.rs"]
pub mod featured;
#[cfg(feature = "wip-featured")]
#[path = "featured.wip.rs"]
pub mod featured;

#[cfg(not(feature = "wip-ghcache"))]
#[path = "ghcache.rs"]
pub mod ghcache;
#[cfg(feature = "wip-ghcache")]
#[path = "ghcache.wip.rs"]
pub mod ghcache;

#[cfg(not(feature = "wip-guard"))]
#[path = "guard.rs"]
pub mod guard;
#[cfg(feature = "wip-guard")]
#[path = "guard.wip.rs"]
pub mod guard;

#[cfg(not(feature = "wip-history"))]
#[path = "history.rs"]
pub mod history;
#[cfg(feature = "wip-history")]
#[path = "history.wip.rs"]
pub mod history;

#[cfg(not(feature = "wip-i18n"))]
#[path = "i18n.rs"]
pub mod i18n;
#[cfg(feature = "wip-i18n")]
#[path = "i18n.wip.rs"]
pub mod i18n;

#[cfg(not(feature = "wip-icons"))]
#[path = "icons.rs"]
pub mod icons;
#[cfg(feature = "wip-icons")]
#[path = "icons.wip.rs"]
pub mod icons;

#[cfg(not(feature = "wip-inventory"))]
#[path = "inventory.rs"]
pub mod inventory;
#[cfg(feature = "wip-inventory")]
#[path = "inventory.wip.rs"]
pub mod inventory;

#[cfg(not(feature = "wip-maintenance"))]
#[path = "maintenance.rs"]
pub mod maintenance;
#[cfg(feature = "wip-maintenance")]
#[path = "maintenance.wip.rs"]
pub mod maintenance;

#[cfg(not(feature = "wip-mimeguard"))]
#[path = "mimeguard.rs"]
pub mod mimeguard;
#[cfg(feature = "wip-mimeguard")]
#[path = "mimeguard.wip.rs"]
pub mod mimeguard;

#[cfg(not(feature = "wip-patches"))]
#[path = "patches.rs"]
pub mod patches;
#[cfg(feature = "wip-patches")]
#[path = "patches.wip.rs"]
pub mod patches;

#[cfg(not(feature = "wip-picks"))]
#[path = "picks.rs"]
pub mod picks;
#[cfg(feature = "wip-picks")]
#[path = "picks.wip.rs"]
pub mod picks;

#[cfg(not(feature = "wip-plugin-setup"))]
#[path = "plugin_setup.rs"]
pub mod plugin_setup;
#[cfg(feature = "wip-plugin-setup")]
#[path = "plugin_setup.wip.rs"]
pub mod plugin_setup;

#[cfg(not(feature = "wip-plugins"))]
#[path = "plugins.rs"]
pub mod plugins;
#[cfg(feature = "wip-plugins")]
#[path = "plugins.wip.rs"]
pub mod plugins;

#[cfg(not(feature = "wip-security"))]
#[path = "security.rs"]
pub mod security;
#[cfg(feature = "wip-security")]
#[path = "security.wip.rs"]
pub mod security;

#[cfg(not(feature = "wip-selfupdate"))]
#[path = "selfupdate.rs"]
pub mod selfupdate;
#[cfg(feature = "wip-selfupdate")]
#[path = "selfupdate.wip.rs"]
pub mod selfupdate;

#[cfg(not(feature = "wip-services"))]
#[path = "services.rs"]
pub mod services;
#[cfg(feature = "wip-services")]
#[path = "services.wip.rs"]
pub mod services;

#[cfg(not(feature = "wip-settings"))]
#[path = "settings.rs"]
pub mod settings;
#[cfg(feature = "wip-settings")]
#[path = "settings.wip.rs"]
pub mod settings;

#[cfg(not(feature = "wip-shortcuts"))]
#[path = "shortcuts.rs"]
pub mod shortcuts;
#[cfg(feature = "wip-shortcuts")]
#[path = "shortcuts.wip.rs"]
pub mod shortcuts;

#[cfg(not(feature = "wip-snapshots"))]
#[path = "snapshots.rs"]
pub mod snapshots;
#[cfg(feature = "wip-snapshots")]
#[path = "snapshots.wip.rs"]
pub mod snapshots;

#[cfg(not(feature = "wip-sources"))]
#[path = "sources.rs"]
pub mod sources;
#[cfg(feature = "wip-sources")]
#[path = "sources.wip.rs"]
pub mod sources;

#[cfg(not(feature = "wip-store"))]
#[path = "store.rs"]
pub mod store;
#[cfg(feature = "wip-store")]
#[path = "store.wip.rs"]
pub mod store;

#[cfg(not(feature = "wip-tasks"))]
#[path = "tasks.rs"]
pub mod tasks;
#[cfg(feature = "wip-tasks")]
#[path = "tasks.wip.rs"]
pub mod tasks;

#[cfg(not(feature = "wip-themeart"))]
#[path = "themeart.rs"]
pub mod themeart;
#[cfg(feature = "wip-themeart")]
#[path = "themeart.wip.rs"]
pub mod themeart;

#[cfg(not(feature = "wip-theming"))]
#[path = "theming.rs"]
pub mod theming;
#[cfg(feature = "wip-theming")]
#[path = "theming.wip.rs"]
pub mod theming;

#[cfg(not(feature = "wip-verify"))]
#[path = "verify.rs"]
pub mod verify;
#[cfg(feature = "wip-verify")]
#[path = "verify.wip.rs"]
pub mod verify;

#[cfg(not(feature = "wip-watcher"))]
#[path = "watcher.rs"]
pub mod watcher;
#[cfg(feature = "wip-watcher")]
#[path = "watcher.wip.rs"]
pub mod watcher;

#[cfg(not(feature = "wip-xdg"))]
#[path = "xdg.rs"]
pub mod xdg;
#[cfg(feature = "wip-xdg")]
#[path = "xdg.wip.rs"]
pub mod xdg;

pub mod util;
pub mod ui;
