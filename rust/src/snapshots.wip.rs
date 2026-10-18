//! Port of `snapshots.py`.
//!
//! Whether the last update can be undone, and undoing it.
//!
//! A system that snapshots before every upgrade can roll any of them back -- but
//! only if the snapshot was actually taken. Full upgrades through a wrapper like
//! `omarchy update` or a snap-pac hook take one; a one-off `yay -S` usually does
//! not, and nothing tells you which kind your last update was until the day you
//! need to undo it.
//!
//! Snapshot listings are root-only, so this never reads them. Whether a snapshot
//! preceded a transaction is read from what any user can see:
//!
//! * pacman.log, where snap-pac and similar hooks announce themselves inside the
//!   transaction they protect, and
//! * the journal, where `sudo`/`pkexec` record a `snapper ... create` command a
//!   minute before an update that ran it by hand.
//!
//! Rolling back is not something a running system can do to itself. Snapshots
//! are booted from the boot menu; once booted into one, it is made permanent.
//! That second step is the one-click part, and it is offered only where the tool
//! for it exists (limine-snapper-restore), otherwise the steps are spelled out.
//!
//! Everything here is optional. Without Snapper there is nothing to report
//! beyond saying so.

use serde::{Deserialize, Serialize};

/// `Path("/var/log/pacman.log")`.
pub const PACMAN_LOG: &str = "/var/log/pacman.log";
/// How long before a transaction a manually created snapshot still counts as
/// "taken for it". omarchy update snapshots, then syncs and downloads.
pub const LEAD: i64 = 30 * 60;

/// The log function a task step is handed: one line at a time.
pub type Log<'a> = &'a dyn Fn(&str);

/// A task step done in this process rather than a command (Python: a callable
/// taking the log function, which raises to fail). `create_step`'s step never
/// fails, so it always returns `Ok(())`.
pub type StepFn = Box<dyn Fn(Log<'_>) -> Result<(), String> + Send + Sync>;

/// Configured snapper configs. Listing them needs no root.
pub fn snapper_configs() -> Vec<String> {
    todo!("port: snapshots::snapper_configs")
}

/// The snapshot number this system was booted from, or ''.
///
/// `cmdline` None reads `/proc/cmdline`.
pub fn booted_snapshot(cmdline: Option<&str>) -> String {
    let _ = cmdline;
    todo!("port: snapshots::booted_snapshot")
}

/// A pacman.log timestamp as epoch seconds; 0.0 when it cannot be parsed.
/// (`datetime.fromisoformat`, falling back to `"%Y-%m-%d %H:%M"`.)
/// Also used by `history::package_events`.
pub fn _ts(text: &str) -> f64 {
    let _ = text;
    todo!("port: snapshots::_ts")
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Transaction {
    pub started: f64,
    /// "name old -> new"
    pub changes: Vec<String>,
    /// a snapshot hook ran in it
    pub hooks_snapshot: bool,
}

/// The most recent pacman transaction that changed something.
///
/// `log_text` None reads the tail of `PACMAN_LOG`; None when it cannot be read.
pub fn last_transaction(log_text: Option<&str>) -> Option<Transaction> {
    let _ = log_text;
    todo!("port: snapshots::last_transaction")
}

/// Did someone run `snapper create` in the run-up to `started`?
///
/// None when the journal cannot be read, which is different from "no".
pub fn snapshot_before(started: f64, journal_text: Option<&str>) -> Option<bool> {
    let _ = (started, journal_text);
    todo!("port: snapshots::snapshot_before")
}

/// A task step that snapshots the root config, or None without Snapper.
///
/// The step never fails the task. A snapshot that could not be taken (the
/// password prompt dismissed, the config broken) is reported in the log and
/// the update goes ahead -- the same choice `omarchy update` makes, because
/// refusing to update is not safer than updating without a net.
///
/// Returns `(label, step)`; the label is "Taking a snapshot first".
pub fn create_step(description: &str) -> Option<(String, StepFn)> {
    let _ = description;
    todo!("port: snapshots::create_step")
}

/// The Health check row for update snapshots.
pub fn finding() -> crate::maintenance::Finding {
    todo!("port: snapshots::finding")
}

/// Parity hook: see ARCHITECTURE.md.
pub fn parity(_func: &str, _args: &serde_json::Value) -> Option<serde_json::Value> {
    None
}
