//! Port of `selfupdate.py`.
//!
//! Almanac updating Almanac.
//!
//! An app whose whole purpose is telling you what is out of date should not be
//! the one thing on the machine that quietly isn't. It appears in its own Updates
//! list like anything else.
//!
//! How it updates depends on how it was installed, and that is detected rather
//! than assumed:
//!
//! * **git checkout** -- the normal case. `git fetch` then a fast-forward pull.
//!   --ff-only is the point: if local edits or a diverged history would make the
//!   pull a merge, it refuses and says so instead of creating a conflict inside a
//!   running application.
//! * **a distribution package** -- pacman owns it, so this stays out of the way
//!   and lets the package appear in the ordinary update list.
//! * **neither** -- a plain directory someone copied. There is nothing to pull,
//!   so it says that rather than inventing a mechanism.
//!
//! The running process is never swapped underneath itself: new code takes effect
//! on the next launch, and the app offers to restart rather than pretending the
//! update already applied.

use std::path::PathBuf;
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

/// The Almanac checkout (Python: `Path(__file__).resolve().parent`).
///
/// The Rust crate lives in `<checkout>/rust`, so the checkout is the parent
/// of the crate's manifest directory.
pub static ROOT: LazyLock<PathBuf> = LazyLock::new(|| {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    crate_dir.parent().map(PathBuf::from).unwrap_or(crate_dir)
});

pub const NAME: &str = "Almanac";
/// Bumped by hand at release. git describe supersedes it in a checkout, so this
/// is only what a tarball install reports.
pub const VERSION: &str = "1.7.0";

pub const GIT: &str = "git";
pub const PACKAGE: &str = "package";
pub const PLAIN: &str = "plain";

pub const METAINFO: &str = "io.github.twodxme.Almanac.metainfo.xml";

/// What an update would consist of (the dict `check` returns).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Info {
    /// One of `GIT`, `PACKAGE`, `PLAIN`.
    pub kind: String,
    pub version: String,
    pub behind: i64,
    pub latest: String,
    pub note: String,
    pub can_update: bool,
    pub remote: String,
    pub dirty: bool,
}

/// (version, AppStream description markup) of the newest release.
///
/// Read from the same AppStream file software centres show, so the About
/// window's "What's New" never disagrees with them. A checkout has it under
/// docs/; a package installs it to the system metainfo directory.
pub fn release_notes() -> Option<(String, String)> {
    todo!("port: selfupdate::release_notes")
}

/// `_git`: run `git -C ROOT <args>`; (exit code, stdout+stderr stripped).
/// A failure to run git at all is (1, the error message).
pub fn git(args: &[&str], timeout: u64) -> (i32, String) {
    let _ = (args, timeout);
    todo!("port: selfupdate::_git")
}

/// How this copy was installed: `GIT`, `PACKAGE` or `PLAIN`.
pub fn install_kind() -> &'static str {
    todo!("port: selfupdate::install_kind")
}

/// The pacman package that owns this copy, or "".
pub fn owning_package() -> String {
    todo!("port: selfupdate::owning_package")
}

/// What is running, preferring what git actually says over the constant.
pub fn version() -> String {
    todo!("port: selfupdate::version")
}

/// URL of the `origin` remote, or "".
pub fn remote_url() -> String {
    todo!("port: selfupdate::remote_url")
}

/// The checked-out branch, or "".
pub fn branch() -> String {
    todo!("port: selfupdate::branch")
}

/// Whether tracked files have uncommitted changes.
pub fn dirty() -> bool {
    todo!("port: selfupdate::dirty")
}

/// What an update would consist of, without performing one.
///
/// Returns a dict with: kind, version, behind, latest, note, can_update.
pub fn check(fetch: bool) -> Info {
    let _ = fetch;
    todo!("port: selfupdate::check")
}

/// Steps for the task runner. Fast-forward only, deliberately.
///
/// Each step is (title, argv).
pub fn update_steps() -> Vec<(String, Vec<String>)> {
    todo!("port: selfupdate::update_steps")
}

/// How to start the new code once it is on disk.
pub fn restart_argv() -> Vec<String> {
    todo!("port: selfupdate::restart_argv")
}

/// Hand off to a fresh process and let this one go.
///
/// A running Python process cannot adopt new source, so the only honest way
/// to apply an update is to start again.
pub fn restart() {
    todo!("port: selfupdate::restart")
}

/// Almanac as an entry in its own inventory.
pub fn as_app(check_updates: bool) -> crate::inventory::App {
    let _ = check_updates;
    todo!("port: selfupdate::as_app")
}

/// Parity hook: see ARCHITECTURE.md.
pub fn parity(_func: &str, _args: &serde_json::Value) -> Option<serde_json::Value> {
    None
}
