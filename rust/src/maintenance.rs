//! Port of `maintenance.py`.
//!
//! System maintenance checks.
//!
//! Each check answers one question with a number, and offers the one command that
//! acts on it. That shape is deliberate: "your package cache is 4.5 GB across 528
//! files" is actionable, "run paccache occasionally" is not.
//!
//! Three rules the checks follow:
//!
//! * Never report a finding without saying what would be done about it. A warning
//!   with no fix is just anxiety.
//! * Never propose a fix that needs judgement the check has not done. Orphan
//!   removal keeps build tooling; cache trimming keeps the last two versions so a
//!   downgrade is still possible.
//! * Reading is free, acting is not. Every check here is read-only; the fixes run
//!   only when asked, through the same confirm-then-run path as everything else.

#![allow(dead_code)]

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// `Path.home()`, evaluated once at import in the Python.
pub static HOME: LazyLock<PathBuf> = LazyLock::new(crate::util::home);
pub const PACMAN_CACHE: &str = "/var/cache/pacman/pkg";
/// `xdg.cache_home() / "yay"`, evaluated once at import in the Python.
pub static YAY_CACHE: LazyLock<PathBuf> = LazyLock::new(|| crate::xdg::cache_home().join("yay"));
/// `xdg.data_home() / "Trash"`, evaluated once at import in the Python.
pub static TRASH: LazyLock<PathBuf> = LazyLock::new(|| crate::xdg::data_home().join("Trash"));

/// Where Almanac keeps AppImages -- beside its registry, in XDG state.
fn _appimage_store() -> PathBuf {
    todo!("port: maintenance::_appimage_store")
}

pub const OK: &str = "ok";
pub const INFO: &str = "info";
pub const WARN: &str = "warn";
pub const CRIT: &str = "crit";

/// The log function a callable fix (and a `tasks` step) is handed: one line
/// of output per call.
pub type LogFn<'a> = &'a (dyn Fn(&str) + Send + Sync);

/// A callable fix: takes the log function, returns `Err` where the Python
/// would raise to fail the step.
pub type FixFn = Arc<dyn Fn(LogFn<'_>) -> Result<(), String> + Send + Sync>;

/// `Finding.fix`: argv to run, or a callable taking a log function. (Python
/// stores either in one `object` field; `None` is `Option::None` on the
/// field.)
#[derive(Clone)]
pub enum Fix {
    Argv(Vec<String>),
    Func(FixFn),
}

impl Fix {
    /// Build an argv fix from string literals.
    pub fn argv(args: &[&str]) -> Fix {
        Fix::Argv(args.iter().map(|s| s.to_string()).collect())
    }

    /// Wrap a closure as a callable fix.
    pub fn func<F>(f: F) -> Fix
    where
        F: Fn(LogFn<'_>) -> Result<(), String> + Send + Sync + 'static,
    {
        Fix::Func(Arc::new(f))
    }
}

impl fmt::Debug for Fix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Fix::Argv(a) => f.debug_tuple("Argv").field(a).finish(),
            Fix::Func(_) => f.write_str("Func(<callable>)"),
        }
    }
}

impl PartialEq for Fix {
    fn eq(&self, other: &Fix) -> bool {
        match (self, other) {
            (Fix::Argv(a), Fix::Argv(b)) => a == b,
            (Fix::Func(a), Fix::Func(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

/// An argv serialises as its list; a callable as the string `"<callable>"`.
impl Serialize for Fix {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Fix::Argv(a) => a.serialize(s),
            Fix::Func(_) => s.serialize_str("<callable>"),
        }
    }
}

/// A list deserialises as an argv; anything else (a serialised callable)
/// becomes a callable that does nothing.
impl<'de> Deserialize<'de> for Fix {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Fix, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        match v {
            serde_json::Value::Array(items) => Ok(Fix::Argv(
                items
                    .into_iter()
                    .map(|i| match i {
                        serde_json::Value::String(s) => s,
                        other => other.to_string(),
                    })
                    .collect(),
            )),
            _ => Ok(Fix::func(|_log| Ok(()))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Finding {
    pub id: String,
    pub title: String,
    pub detail: String,
    pub severity: String,
    pub count: i64,
    pub size_mb: f64,
    pub fix_label: String,
    /// argv to run, or a callable taking a log function. Empty means there is
    /// nothing to do but look.
    pub fix: Option<Fix>,
    /// what the fix actually does, for the dialog
    pub fix_note: String,
    pub items: Vec<String>,
    pub url: String,
}

impl Default for Finding {
    fn default() -> Finding {
        Finding {
            id: String::new(),
            title: String::new(),
            detail: String::new(),
            severity: INFO.to_string(),
            count: 0,
            size_mb: 0.0,
            fix_label: String::new(),
            fix: None,
            fix_note: String::new(),
            items: Vec::new(),
            url: String::new(),
        }
    }
}

impl Finding {
    pub fn actionable(&self) -> bool {
        self.fix.is_some() && self.severity != OK
    }
}

fn _run(cmd: &[&str], timeout: u64) -> String {
    todo!("port: maintenance::_run")
}

/// (megabytes, file count). Walks rather than shelling out to du.
fn _du_mb(path: &Path) -> (f64, i64) {
    todo!("port: maintenance::_du_mb")
}

pub fn fmt_size(mb: f64) -> String {
    if mb >= 1024.0 {
        return format!("{:.1} GB", mb / 1024.0);
    }
    if mb >= 1.0 {
        return format!("{:.0} MB", mb);
    }
    format!("{:.0} KB", mb * 1024.0)
}

// ---- checks ------------------------------------------------------------------

/// `_SAVED = re.compile(r"disk space saved:\s*([\d.]+)\s*([KMGT]?)i?B")`
const _SAVED: &str = r"disk space saved:\s*([\d.]+)\s*([KMGT]?)i?B";

/// Megabytes a paccache run with `flags` would free, from its dry run.
fn _paccache_reclaimable(flags: &[&str]) -> f64 {
    todo!("port: maintenance::_paccache_reclaimable")
}

pub fn pacman_cache() -> Finding {
    todo!("port: maintenance::pacman_cache")
}

pub fn yay_cache() -> Finding {
    todo!("port: maintenance::yay_cache")
}

fn _clear_yay_cache(log: LogFn<'_>) -> Result<(), String> {
    todo!("port: maintenance::_clear_yay_cache")
}

pub fn journal() -> Finding {
    todo!("port: maintenance::journal")
}

pub fn trash() -> Finding {
    todo!("port: maintenance::trash")
}

fn _empty_trash(log: LogFn<'_>) -> Result<(), String> {
    todo!("port: maintenance::_empty_trash")
}

/// The .previous copies this app keeps after an AppImage update.
pub fn rollbacks() -> Finding {
    todo!("port: maintenance::rollbacks")
}

fn _delete_rollbacks(log: LogFn<'_>) -> Result<(), String> {
    todo!("port: maintenance::_delete_rollbacks")
}

pub fn failed_units() -> Finding {
    todo!("port: maintenance::failed_units")
}

pub fn pacnew() -> Finding {
    todo!("port: maintenance::pacnew")
}

/// Launcher entries pointing at programs that are no longer there.
pub fn dangling_desktop() -> Finding {
    todo!("port: maintenance::dangling_desktop")
}

fn _remove_dangling(log: LogFn<'_>) -> Result<(), String> {
    todo!("port: maintenance::_remove_dangling")
}

pub fn broken_symlinks() -> Finding {
    todo!("port: maintenance::broken_symlinks")
}

fn _remove_broken_links(log: LogFn<'_>) -> Result<(), String> {
    todo!("port: maintenance::_remove_broken_links")
}

pub fn orphan_packages() -> Finding {
    todo!("port: maintenance::orphan_packages")
}

/// Foreign packages the AUR no longer carries, or that are flagged.
pub fn unmaintained_aur() -> Finding {
    todo!("port: maintenance::unmaintained_aur")
}

/// (free MB, total MB, percent used) or None.
fn _usage(path: &str) -> Option<(f64, f64, f64)> {
    todo!("port: maintenance::_usage")
}

/// Files a package owns that are no longer where it put them.
///
/// `pacman -Qk` reports two completely different things in the same breath:
/// a file that is genuinely gone, and a file this process is not allowed to
/// look at. Most of /etc and all of /boot is root-only, so an unprivileged
/// run cannot see a large part of what it is checking -- and counting those
/// as faults reports a healthy system as broken. They are counted and
/// stated separately, never as problems.
pub fn package_integrity() -> Finding {
    todo!("port: maintenance::package_integrity")
}

/// Every filesystem that matters, not just the root one.
///
/// Checking only / misses the ways a machine actually runs out of room: a
/// full /boot stops a kernel update, and a full XDG runtime directory --
/// 1.6 GB of tmpfs that one runaway log can fill -- breaks logins, Wayland
/// clients and system updates while `df /` still reports hundreds of
/// gigabytes free.
pub fn disk_space() -> Finding {
    todo!("port: maintenance::disk_space")
}

/// Session logs in the runtime directory, which is a small tmpfs.
///
/// Hyprland and Quickshell write per-session logs into XDG_RUNTIME_DIR. A
/// display that flaps produces a hotplug event per cycle and several log
/// lines per event, and the directory it lands in is typically 1.6 GB. The
/// failure mode is not "logs are large" but "the next system update dies
/// with No space left on device".
pub fn runtime_logs() -> Finding {
    todo!("port: maintenance::runtime_logs")
}

fn _clear_runtime_logs(log: LogFn<'_>) -> Result<(), String> {
    todo!("port: maintenance::_clear_runtime_logs")
}

pub fn config_resets() -> Finding {
    crate::guard::finding()
}

pub fn update_snapshots() -> Finding {
    crate::snapshots::finding()
}

pub fn known_vulnerabilities() -> Finding {
    crate::security::finding()
}

/// A health check: Python's `CHECKS` holds the bare functions and reads
/// `check.__name__` when one fails; here each carries that name beside it.
pub type Check = fn() -> Finding;

pub const CHECKS: &[(&str, Check)] = &[
    ("disk_space", disk_space),
    ("runtime_logs", runtime_logs),
    ("package_integrity", package_integrity),
    ("pacman_cache", pacman_cache),
    ("journal", journal),
    ("yay_cache", yay_cache),
    ("orphan_packages", orphan_packages),
    ("unmaintained_aur", unmaintained_aur),
    ("failed_units", failed_units),
    ("pacnew", pacnew),
    ("dangling_desktop", dangling_desktop),
    ("broken_symlinks", broken_symlinks),
    ("rollbacks", rollbacks),
    ("trash", trash),
    ("config_resets", config_resets),
    ("update_snapshots", update_snapshots),
    ("known_vulnerabilities", known_vulnerabilities),
];

/// Which check produced which finding, so one can be asked again after its fix
/// without re-running all of them (the integrity check alone reads every file
/// pacman installed).
pub const CHECK_BY_ID: &[(&str, Check)] = &[
    ("disk", disk_space),
    ("runtime-logs", runtime_logs),
    ("integrity", package_integrity),
    ("pacman-cache", pacman_cache),
    ("journal", journal),
    ("yay-cache", yay_cache),
    ("orphans", orphan_packages),
    ("aur-health", unmaintained_aur),
    ("failed-units", failed_units),
    ("pacnew", pacnew),
    ("dangling-desktop", dangling_desktop),
    ("broken-symlinks", broken_symlinks),
    ("rollbacks", rollbacks),
    ("trash", trash),
    ("config-reset", config_resets),
    ("snapshot", update_snapshots),
    ("vulnerabilities", known_vulnerabilities),
];

/// `CHECK_BY_ID.get(finding_id)`.
pub fn check_by_id(finding_id: &str) -> Option<Check> {
    CHECK_BY_ID.iter().find(|(id, _)| *id == finding_id).map(|(_, f)| *f)
}

/// Every check. Read-only; safe to run on a timer.
pub fn run_all() -> Vec<Finding> {
    todo!("port: maintenance::run_all")
}

/// How much space the actionable cleanups would actually free.
pub fn reclaimable_mb(findings: &[Finding]) -> f64 {
    const FREES: [&str; 6] = ["pacman-cache", "yay-cache", "journal", "trash", "rollbacks", "runtime-logs"];
    findings
        .iter()
        .filter(|f| FREES.contains(&f.id.as_str()) && f.actionable())
        .map(|f| f.size_mb)
        .sum()
}

/// Parity hook: see ARCHITECTURE.md.
pub fn parity(_func: &str, _args: &serde_json::Value) -> Option<serde_json::Value> {
    None
}
