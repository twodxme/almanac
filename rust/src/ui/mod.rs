//! The window: the port of `app.py`, one file per section.

#[cfg(not(feature = "wip-ui-shell"))]
#[path = "shell.rs"]
pub mod shell;
#[cfg(feature = "wip-ui-shell")]
#[path = "shell.wip.rs"]
pub mod shell;

#[cfg(not(feature = "wip-ui-packages"))]
#[path = "packages.rs"]
pub mod packages;
#[cfg(feature = "wip-ui-packages")]
#[path = "packages.wip.rs"]
pub mod packages;

#[cfg(not(feature = "wip-ui-store"))]
#[path = "store.rs"]
pub mod store;
#[cfg(feature = "wip-ui-store")]
#[path = "store.wip.rs"]
pub mod store;

#[cfg(not(feature = "wip-ui-store-actions"))]
#[path = "store_actions.rs"]
pub mod store_actions;
#[cfg(feature = "wip-ui-store-actions")]
#[path = "store_actions.wip.rs"]
pub mod store_actions;

#[cfg(not(feature = "wip-ui-plugins"))]
#[path = "plugins.rs"]
pub mod plugins;
#[cfg(feature = "wip-ui-plugins")]
#[path = "plugins.wip.rs"]
pub mod plugins;

#[cfg(not(feature = "wip-ui-switchboard"))]
#[path = "switchboard.rs"]
pub mod switchboard;
#[cfg(feature = "wip-ui-switchboard")]
#[path = "switchboard.wip.rs"]
pub mod switchboard;

#[cfg(not(feature = "wip-ui-history"))]
#[path = "history.rs"]
pub mod history;
#[cfg(feature = "wip-ui-history")]
#[path = "history.wip.rs"]
pub mod history;

#[cfg(not(feature = "wip-ui-ai"))]
#[path = "ai.rs"]
pub mod ai;
#[cfg(feature = "wip-ui-ai")]
#[path = "ai.wip.rs"]
pub mod ai;

#[cfg(not(feature = "wip-ui-preferences"))]
#[path = "preferences.rs"]
pub mod preferences;
#[cfg(feature = "wip-ui-preferences")]
#[path = "preferences.wip.rs"]
pub mod preferences;

#[cfg(not(feature = "wip-ui-today"))]
#[path = "today.rs"]
pub mod today;
#[cfg(feature = "wip-ui-today")]
#[path = "today.wip.rs"]
pub mod today;

#[cfg(not(feature = "wip-ui-actions"))]
#[path = "actions.rs"]
pub mod actions;
#[cfg(feature = "wip-ui-actions")]
#[path = "actions.wip.rs"]
pub mod actions;
