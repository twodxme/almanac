//! A local change to a checkout, held up against the update.
//!
//! Plugins and other git checkouts get patched in place: a fix that has not
//! been released yet, a setting the author did not expose. `git pull --ff-only`
//! then refuses to update -- correctly, since it will not touch uncommitted
//! work -- and the checkout sits behind forever, which is worse.
//!
//! What the user needs to know is how their change relates to the update, and
//! git can answer that exactly, without touching the working tree:
//!
//! * **absorbed** -- the update already contains the change, line for line. It
//!   can be dropped.
//! * **applies** -- the change still applies on top of the update. It can be
//!   carried forward.
//! * **conflicts** -- the update changed the same lines. The change is set aside
//!   (as a stash, so nothing is lost) and the update installed.
//!
//! Each is decided by applying the change to a scratch index built from the
//! upstream tree (`git apply --cached --check`), forwards and in reverse. The
//! working tree and the real index are never touched until the user chooses.
//!
//! "Absorbed" is deliberately exact. Upstream fixing the same bug a different
//! way is common -- and it lands in "conflicts", because deciding that two
//! different patches mean the same thing is a judgement git cannot make.
//!
//! Port of `patches.py`.

use serde::{Deserialize, Serialize};

pub const CLEAN: &str = "clean";
pub const ABSORBED: &str = "absorbed";
pub const APPLIES: &str = "applies";
pub const CONFLICTS: &str = "conflicts";
pub const UNKNOWN: &str = "unknown";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PatchState {
    pub kind: String,
    pub files: Vec<String>,
    /// touching `files`
    pub upstream_commits: Vec<String>,
    pub untracked: Vec<String>,
}

/// `git -C repo <args>` with an optional environment and stdin.
#[allow(dead_code)]
fn git(repo: &str, args: &[&str], env: &[(String, String)], input_text: Option<&str>,
       timeout: u64) -> crate::util::Output {
    let _ = (repo, args, env, input_text, timeout);
    todo!("port: patches::_git")
}

/// `upstream` None means the Python default, `"@{upstream}"`.
pub fn analyze(repo: &str, upstream: Option<&str>) -> PatchState {
    let _ = (repo, upstream);
    todo!("port: patches::analyze")
}

/// The task steps that update `repo` for this state. `pull` is the
/// command that fetches the update -- `git pull --ff-only` by default; an
/// Omarchy plugin passes `omarchy plugin update`, which also validates.
/// (None or an empty list means the default, as Python's `pull or [...]`.)
pub fn steps(repo: &str, state: &PatchState, label: &str, pull: Option<&[String]>)
             -> Vec<crate::tasks::Step> {
    let _ = (repo, state, label, pull);
    todo!("port: patches::steps")
}

/// (heading, body, action label) for the confirmation dialog.
pub fn explain(state: &PatchState, name: &str) -> (String, String, String) {
    let _ = (state, name);
    todo!("port: patches::explain")
}

/// Parity hook: see ARCHITECTURE.md.
pub fn parity(_func: &str, _args: &serde_json::Value) -> Option<serde_json::Value> {
    None
}
