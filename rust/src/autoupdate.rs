//! Scheduled updates: the settings, and the systemd timer behind them.
//!
//! The timer and its service already exist on disk; this is the part that lets
//! the app change them without the user editing unit files. Everything that is a
//! *policy* decision lives in one JSON file that both this module and the shell
//! script read, so the checkbox in the window and the behaviour at 11am on Sunday
//! cannot drift apart.
//!
//! What is deliberately not configurable: whether risky batches are held when the
//! machine would need a human (kernel, bootloader, fresh Arch news). That is a
//! safety property, not a preference -- see omarchy-auto-update.
//!
//! Port of `autoupdate.py`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

use crate::util;

/// `$XDG_CONFIG_HOME` as the Python reads it: set (even to "") wins, with
/// no absolute-path check, unlike `xdg.config_home()`.
fn config_root() -> PathBuf {
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(v) => PathBuf::from(v),
        None => util::home().join(".config"),
    }
}

pub static CONFIG_DIR: LazyLock<PathBuf> = LazyLock::new(|| config_root().join("almanac"));
pub static CONFIG: LazyLock<PathBuf> = LazyLock::new(|| CONFIG_DIR.join("auto-update.json"));
pub const UNIT: &str = "omarchy-auto-update.timer";
pub static TIMER_FILE: LazyLock<PathBuf> =
    LazyLock::new(|| config_root().join("systemd/user").join(UNIT));

/// label -> OnCalendar: `(key, label, OnCalendar)`.
pub const SCHEDULES: &[(&str, &str, &str)] = &[
    ("daily", "Every day at 11:00", "*-*-* 11:00:00"),
    ("weekly", "Every Sunday at 11:00", "Sun 11:00"),
    ("fortnightly", "Every other Sunday", "Sun *-*-1..7,15..21 11:00:00"),
    ("monthly", "First Sunday of the month", "Sun *-*-1..7 11:00:00"),
];

/// The auto-update.json policy file (the Python's `cfg` dict). Fields are in
/// `DEFAULTS` order so a save writes them in the Python's order; keys the
/// file has that Almanac does not know are kept in `extra` and written back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub schedule: String,
    // What the unattended run is allowed to touch. Packages default on
    // because that is what the timer was created for; plugins default off
    // because a shell plugin update can change the desktop under you.
    pub packages: bool,
    pub agents: bool,
    pub appimages: bool,
    pub plugins: bool,
    pub notify: bool,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            schedule: "weekly".to_owned(),
            packages: true,
            agents: true,
            appimages: true,
            plugins: false,
            notify: true,
            extra: BTreeMap::new(),
        }
    }
}

/// The Python's `DEFAULTS` dict.
pub static DEFAULTS: LazyLock<Config> = LazyLock::new(Config::default);

/// `next` / `last` as systemd reports them, and whether the timer is on.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Status {
    pub enabled: bool,
    pub next: String,
    pub last: String,
}

pub fn load() -> Config {
    todo!("port: autoupdate::load")
}

/// `Err` where the Python raises OSError.
pub fn save(cfg: &Config) -> Result<(), String> {
    let _ = cfg;
    todo!("port: autoupdate::save")
}

/// `systemctl --user <args>` -> (returncode, stdout+stderr stripped).
#[allow(dead_code)]
fn systemctl(args: &[&str], timeout: u64) -> (i32, String) {
    let _ = (args, timeout);
    todo!("port: autoupdate::_systemctl")
}

pub fn is_enabled() -> bool {
    todo!("port: autoupdate::is_enabled")
}

pub fn set_enabled(on: bool) -> (bool, String) {
    let _ = on;
    todo!("port: autoupdate::set_enabled")
}

/// Next and last run, read from systemd rather than remembered here.
pub fn status() -> Status {
    todo!("port: autoupdate::status")
}

/// Rewrite OnCalendar in the timer unit and reload.
///
/// The whole unit is not regenerated: it carries Persistent= and a randomised
/// delay that were chosen for a laptop, and rewriting the file would quietly
/// drop them.
pub fn set_schedule(key: &str) -> (bool, String) {
    let _ = key;
    todo!("port: autoupdate::set_schedule")
}

pub fn current_schedule() -> String {
    todo!("port: autoupdate::current_schedule")
}

/// How to trigger a run immediately, for the task runner.
pub fn run_now_argv() -> Vec<String> {
    vec![
        "systemd-run".into(), "--user".into(), "--wait".into(), "--pipe".into(),
        "--collect".into(), "--unit=almanac-update".into(),
        util::home().join(".local/bin/omarchy-auto-update").to_string_lossy().into_owned(),
    ]
}

/// Parity hook: see ARCHITECTURE.md.
pub fn parity(_func: &str, _args: &serde_json::Value) -> Option<serde_json::Value> {
    None
}
