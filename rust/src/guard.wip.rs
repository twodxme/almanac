//! Port of `guard.py`.
//!
//! Settings a tool quietly put back to their defaults.
//!
//! Omarchy's `refresh` commands copy the shipped default of a config file over
//! the user's own, keeping theirs as `<file>.bak.<epoch>`. That is a reasonable
//! repair tool and a terrible surprise: `omarchy-refresh-shell` replaces
//! shell.json, every custom bar widget disappears, and nothing says why. The
//! backup is sitting right there; nobody tells you it exists.
//!
//! A reset is recognised from the files alone, which makes it independent of
//! which command did it and of whether Almanac was running at the time:
//!
//! * `<file>.bak.<epoch>` exists, from the last week,
//! * `<file>` is now identical to the default the distribution ships, and
//! * the backup is not -- so something customised was replaced.
//!
//! When the user chooses the defaults on purpose, the finding still offers the
//! backup for a week, then lets it go. Restoring keeps the replaced defaults as
//! `<file>.defaults.<epoch>`, so the restore is reversible too.
//!
//! On a system without a shipped-defaults tree (anything that is not Omarchy)
//! there is nothing to compare against, and this finds nothing.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

pub static BAK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?P<base>.+)\.bak\.(?P<ts>\d{9,11})$").unwrap());
pub const WINDOW_DAYS: u64 = 7;
/// Deep enough for ~/.config/<app>/<sub>/<file>; a full walk of ~/.config can
/// be large (browser profiles) and nothing reset by a refresh lives that deep.
pub const MAX_DEPTH: u32 = 4;

pub fn config_home() -> PathBuf {
    crate::xdg::config_home()
}

/// Where the distribution keeps the shipped copies of user configs.
pub fn defaults_root() -> Option<PathBuf> {
    let base = std::env::var("OMARCHY_PATH")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/usr/share/omarchy".to_string());
    let root = Path::new(&base).join("config");
    if root.is_dir() {
        Some(root)
    } else {
        None
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Reset {
    /// the config file, now holding defaults
    pub path: PathBuf,
    /// the user's version
    pub backup: PathBuf,
    pub when: f64,
    /// what the defaults dropped
    pub lost: Vec<String>,
}

impl Reset {
    pub fn new(path: PathBuf, backup: PathBuf, when: f64, lost: Vec<String>) -> Self {
        Reset { path, backup, when, lost }
    }

    pub fn name(&self) -> String {
        match self.path.strip_prefix(config_home()) {
            Ok(rel) => rel.to_string_lossy().into_owned(),
            Err(_) => self.path.to_string_lossy().into_owned(),
        }
    }
}

/// Content for comparison: what `normal` returns.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq)]
enum Normal {
    /// Parsed JSON, so formatting is ignored.
    Json(serde_json::Value),
    /// The raw bytes, stripped of surrounding whitespace.
    Raw(Vec<u8>),
}

/// Content for comparison: parsed when JSON, so formatting is ignored.
///
/// `like` is the file whose type decides -- a backup is named
/// `shell.json.bak.<epoch>`, so its own suffix says nothing.
#[allow(dead_code)]
fn normal(path: &Path, like: Option<&Path>) -> Option<Normal> {
    let _ = (path, like);
    todo!("port: guard::_normal")
}

/// Every "id" in a JSON document, in order: widgets, plugins, entries.
#[allow(dead_code)]
fn ids(blob: &serde_json::Value) -> Vec<String> {
    let _ = blob;
    todo!("port: guard::_ids")
}

/// What the reset removed, in terms a person recognises.
#[allow(dead_code)]
fn lost(current: &Path, backup: &Path) -> Vec<String> {
    let _ = (current, backup);
    todo!("port: guard::_lost")
}

/// Every regular file under `root`, `depth` directory levels deep at most,
/// not following symlinks; unreadable entries are skipped. (Python yields
/// lazily; the order is `os.scandir`'s.) Also used by `history::reset_events`. (`_walk` in the Python.)
pub fn walk(root: &Path, depth: u32) -> Vec<PathBuf> {
    let _ = (root, depth);
    todo!("port: guard::_walk")
}

/// `now` defaults to the current time (`None`, or 0 as in Python's `now or`).
pub fn resets(now: Option<f64>) -> Vec<Reset> {
    let _ = now;
    todo!("port: guard::resets")
}

/// Put the user's version back, keeping the defaults it replaces.
///
/// Errors (a failed copy) go to the caller, as the Python raises.
pub fn restore(reset: &Reset, log: crate::tasks::LogFn<'_>) -> Result<(), String> {
    let _ = (reset, log);
    todo!("port: guard::restore")
}

/// Commands that make a running desktop pick a restored file up.
///
/// Hyprland watches its own config. Omarchy's shell does not re-read
/// shell.json, so it has to be restarted -- with the restart command, never
/// the refresh one, which is what reset it in the first place.
pub fn reload_for<I, P>(paths: I) -> Vec<Vec<String>>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    let _ = paths.into_iter().map(|p| p.as_ref().to_path_buf()).count();
    todo!("port: guard::reload_for")
}

/// The Health check row for this.
pub fn finding() -> crate::maintenance::Finding {
    todo!("port: guard::finding")
}

/// Parity hook: see ARCHITECTURE.md.
pub fn parity(_func: &str, _args: &serde_json::Value) -> Option<serde_json::Value> {
    None
}
