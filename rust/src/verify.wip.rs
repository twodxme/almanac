//! Port of `verify.py`.
//!
//! Did it work? Checked, not assumed.
//!
//! A command's exit status says the command ran. It does not say the thing the
//! user asked for happened. Every silent failure this app has had was that gap:
//! `yay -S --needed` skipping a package the system database did not know was
//! newer, `journalctl --vacuum-size` deleting nothing without root, `paccache`
//! trimming a cache that had nothing left to trim -- each exited 0, each was
//! reported as done, and the row stayed exactly where it was.
//!
//! So every action that changes something is bracketed by a probe: the same
//! question asked before and after ("which version is installed?", "what does
//! this check find?"). The answer the user sees is the difference between the
//! two, never the exit status.
//!
//! Probes are plain callables returning something comparable. They run off the
//! UI thread and must never raise; a probe that cannot answer returns None, and
//! "could not tell" is reported as that rather than as success or failure.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Outcome {
    /// None: the probe could not tell
    pub changed: Option<bool>,
    pub message: String,
}

impl Outcome {
    pub fn new(changed: Option<bool>, message: impl Into<String>) -> Self {
        Outcome { changed, message: message.into() }
    }

    pub fn ok(&self) -> bool {
        self.changed.unwrap_or(false)
    }
}

/// "Something comparable": what a probe answers. Python probes return a str,
/// a `(size, mtime)` tuple, a set, or whatever an ad-hoc probe in the UI
/// returns (a bool, a dict of plugin HEADs); this enum covers those.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProbeValue {
    Str(String),
    /// `(st_size, int(st_mtime))` from `file_identity`.
    Identity(u64, i64),
    Set(BTreeSet<String>),
    Bool(bool),
    Map(BTreeMap<String, Option<String>>),
}

/// A probe: asked before and after, off the UI thread. `None` is "could not
/// tell".
pub type Probe = Arc<dyn Fn() -> Option<ProbeValue> + Send + Sync>;

#[allow(dead_code)]
fn out(cmd: &[&str], timeout: u64) -> String {
    let _ = (cmd, timeout);
    todo!("port: verify::_out")
}

// ---- probes ------------------------------------------------------------------

/// The installed version of a pacman package, or '' when it is absent.
pub fn installed_package(name: &str) -> Probe {
    let _ = name;
    todo!("port: verify::installed_package")
}

pub fn git_head(repo: &str) -> Probe {
    let _ = repo;
    todo!("port: verify::git_head")
}

pub fn mise_version(tool: &str) -> Probe {
    let _ = tool;
    todo!("port: verify::mise_version")
}

pub fn binary_version(binary: &str) -> Probe {
    let _ = binary;
    todo!("port: verify::binary_version")
}

/// Size and mtime: an AppImage swapped in place changes both.
pub fn file_identity(path: &str) -> Probe {
    let _ = path;
    todo!("port: verify::file_identity")
}

/// Every repository and AUR package with an update waiting.
pub fn pending_updates() -> Probe {
    todo!("port: verify::pending_updates")
}

/// The probe that answers "did this app's update land?".
pub fn for_app(app: &crate::inventory::App) -> Option<Probe> {
    let _ = app;
    todo!("port: verify::for_app")
}

// ---- judging -----------------------------------------------------------------

/// Compare an app before and after its update.
pub fn judge_app(
    app: &crate::inventory::App,
    before: Option<&ProbeValue>,
    after: Option<&ProbeValue>,
) -> Outcome {
    let _ = (app, before, after);
    todo!("port: verify::judge_app")
}

/// Compare the pending-update set around a full upgrade.
///
/// Both sides are what `pending_updates` answers (`ProbeValue::Set`); any
/// other value counts as "could not tell".
pub fn judge_pending(before: Option<&ProbeValue>, after: Option<&ProbeValue>) -> Outcome {
    let _ = (before, after);
    todo!("port: verify::judge_pending")
}

/// Compare a health finding before and after its fix ran.
pub fn judge_finding(
    before: &crate::maintenance::Finding,
    after: Option<&crate::maintenance::Finding>,
) -> Outcome {
    let _ = (before, after);
    todo!("port: verify::judge_finding")
}

/// Run the one health check that produced `finding_id` again.
pub fn recheck(finding_id: &str) -> Option<crate::maintenance::Finding> {
    let _ = finding_id;
    todo!("port: verify::recheck")
}

/// Parity hook: see ARCHITECTURE.md.
pub fn parity(_func: &str, _args: &serde_json::Value) -> Option<serde_json::Value> {
    None
}
