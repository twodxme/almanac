//! Almanac's own preferences, in one file.
//!
//! Everything a person can switch about Almanac itself lives here, with its
//! default -- including each thing it reaches over the network, so that "what
//! leaves your machine" in the README is something a user can turn off, not
//! just read about. Stored as JSON in the XDG state directory; a missing or
//! broken file means the defaults, never a crash.
//!
//! Scheduled updates keep their own file (autoupdate.py), because a shell
//! script reads it too.
//!
//! Port of `settings.py`.

use std::path::PathBuf;

use serde_json::Value;

/// A default value in `DEFAULTS`: every setting is a bool or a string.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DefaultValue {
    Bool(bool),
    Str(&'static str),
}

impl DefaultValue {
    /// As the JSON value the settings file would hold.
    pub fn to_value(self) -> Value {
        match self {
            DefaultValue::Bool(b) => Value::Bool(b),
            DefaultValue::Str(s) => Value::String(s.to_owned()),
        }
    }
}

/// Every setting and its default, in the Python dict's order.
pub const DEFAULTS: &[(&str, DefaultValue)] = &[
    // General
    ("start_page", DefaultValue::Str("last")),           // last | featured | updates | plugins
    // Notifications
    ("notify_plugin_updates", DefaultValue::Bool(true)), // from the background watcher
    ("notify_resets", DefaultValue::Bool(true)),         // a config file put back to its defaults
    ("notify_no_change", DefaultValue::Bool(true)),      // an action that ran but changed nothing
    // Updates
    ("hold_majors", DefaultValue::Bool(true)),           // major agent releases wait for a person
    ("plugin_scope", DefaultValue::Str("all")),          // see plugins.SCOPES
    // Network: each thing Almanac fetches, by what it is for
    ("net_vendor_icons", DefaultValue::Bool(true)),      // logos from each vendor's own domain
    ("net_security_tracker", DefaultValue::Bool(true)),  // security.archlinux.org advisories
    ("net_market_omarchy", DefaultValue::Bool(true)),    // plugins.omarchy.org catalogue
    ("net_market_artcraft", DefaultValue::Bool(true)),   // getartcraft.com + GitHub releases
    ("net_market_themes", DefaultValue::Bool(true)),     // the omarchy-theme topic on GitHub
    ("net_gh_token", DefaultValue::Bool(true)),          // use `gh auth token` for GitHub requests
    ("net_daily_picks", DefaultValue::Bool(true)),       // GitHub stats for Today's picks
];

pub const START_PAGES: &[(&str, &str)] = &[
    ("last", "Where you left off"),
    ("featured", "Today"),
    ("updates", "Updates"),
    ("plugins", "Plugins"),
];

/// `DEFAULTS.get(key)`: the default, or None for a key that is not a setting.
pub fn default_for(key: &str) -> Option<DefaultValue> {
    DEFAULTS.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

pub fn path() -> PathBuf {
    crate::xdg::state_home().join("almanac").join("settings.json")
}

/// The file as a JSON object; anything unreadable or not an object is `{}`.
#[allow(dead_code)]
fn load() -> serde_json::Map<String, Value> {
    todo!("port: settings::_load")
}

/// The setting, or its default. Unknown keys are a programming error
/// (`Err("KeyError: <key>")`, as the Python raises `KeyError`).
pub fn get(key: &str) -> Result<Value, String> {
    let _ = key;
    todo!("port: settings::get")
}

/// `bool(settings.get(key))` for a bool setting; an unknown key is false.
pub fn get_bool(key: &str) -> bool {
    let _ = key;
    todo!("port: settings::get (bool)")
}

/// `settings.get(key)` for a string setting; an unknown key is "".
pub fn get_str(key: &str) -> String {
    let _ = key;
    todo!("port: settings::get (str)")
}

/// Store one setting. `Err` for an unknown key (Python's KeyError) or when
/// the file cannot be written (OSError).
pub fn set(key: &str, value: Value) -> Result<(), String> {
    let _ = (key, value);
    todo!("port: settings::set")
}

/// Parity hook: see ARCHITECTURE.md.
pub fn parity(_func: &str, _args: &serde_json::Value) -> Option<serde_json::Value> {
    None
}
