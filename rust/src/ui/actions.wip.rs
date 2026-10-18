//! UI section `actions` (port of `app.py` lines 5530-5636 and 5999-6481):
//! row actions, self-update and About, verification of outcomes, file-type
//! takeovers, install from a link, update one/all, AppImage add/remove.
//!
//! STUBS: every method has its real signature and a body that does nothing.

#![allow(dead_code, unused_variables)]

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::inventory::App;
use crate::patches::PatchState;
use crate::sources::{Option as SourceOption, Resolution};
use crate::tasks::{OnDone, Step};
use crate::ui::shell::Win;
use crate::verify::{Outcome, Probe, ProbeValue};

/// This section's own state. Every attribute these methods touch
/// (`_row_actions`, `toasts`, `reload`) belongs to the shell or is shared,
/// so there is nothing of its own to keep.
#[derive(Default)]
pub struct State {}

/// `judge` of `_checked`: turns the before and after probe answers into what
/// the user is told. Runs on a worker thread.
pub type Judge = Box<dyn FnOnce(Option<ProbeValue>, Option<ProbeValue>) -> Outcome + Send>;

/// The `holder` dict of `_watch_defaults`: every file type's default app,
/// read on a worker thread before an install.
#[derive(Default)]
pub struct DefaultsWatch {
    /// `holder["before"]`: `mimeguard.snapshot()`, filled in by the thread.
    pub before: Arc<Mutex<Option<BTreeMap<String, String>>>>,
    /// `holder["thread"]`.
    pub thread: Option<JoinHandle<()>>,
}

/// `github.com/owner/repo[/...]` -> `https://github.com/owner/repo/releases`;
/// any other non-empty URL is returned as is, an empty one as `None`.
/// (The URL logic of `_row_notes`.)
pub fn releases_url(url: &str) -> Option<String> {
    let tail = match url.split_once("github.com/") {
        Some((_, t)) => t,
        None => url,
    };
    let parts: Vec<&str> = tail.trim_matches('/').split('/').collect();
    if parts.len() >= 2 {
        Some(format!("https://github.com/{}/{}/releases", parts[0], parts[1]))
    } else if !url.is_empty() {
        Some(url.to_string())
    } else {
        None
    }
}

/// The name an added AppImage is tracked under: the file stem up to the
/// first `-`, or the whole stem when that is empty (`_install_appimage`).
pub fn appimage_name(src: &Path) -> String {
    let stem = src
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    match stem.split('-').next() {
        Some(first) if !first.is_empty() => first.to_string(),
        _ => stem,
    }
}

/// The `yay` argv that installs or updates one package (`on_update_one`,
/// `_install_option`).
pub fn yay_argv(name: &str) -> Vec<String> {
    ["yay", "-S", "--needed", "--noconfirm", "--sudo", "pkexec", name]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// The shell script `on_update_all` runs in a terminal.
pub fn update_all_script(command: &str) -> String {
    format!("{command}; echo; read -n1 -r -p 'Done — press any key to close'")
}

impl Win {
    // ---- actions ---------------------------------------------------------
    pub fn row_visit(&self, name: &str) {}

    /// Jump to the releases page, where the real changelog is.
    pub fn row_notes(&self, name: &str) {}

    /// Update Almanac, then offer to restart into the new code.
    pub fn on_self_update(&self) {}

    pub fn after_self_update(&self, ok: bool) {}

    pub fn on_about(&self) {}

    pub fn row_folder(&self, name: &str) {}

    /// Show what is uncommitted, read-only, before offering to move it.
    pub fn row_changes(&self, name: &str) {}

    pub fn row_pull(&self, name: &str) {}

    /// Update a checkout that has local edits -- see on_git_pull.
    pub fn row_stash(&self, name: &str) {}

    // ---- did it work? ------------------------------------------------------
    /// An on_done that reports what changed, not what a command exited.
    ///
    /// `probe` is asked now and again once the task is over, and `judge`
    /// turns the two answers into what the user is told (see verify.py).
    /// Every silent failure this app has had passed an exit-status check.
    pub fn checked(&self, probe: Option<Probe>, judge: Judge, then: Option<OnDone>) -> OnDone {
        Box::new(move |ok| {
            if let Some(then) = then {
                then(ok);
            }
        })
    }

    pub fn report_outcome(&self, out: &Outcome) {}

    /// Put a Snapper snapshot ahead of a package change, when available.
    pub fn snapshot_first(&self, steps: Vec<Step>, what: &str) -> Vec<Step> {
        steps
    }

    // ---- which files an install took over ----------------------------------
    /// Read every file type's default app now, before an install.
    pub fn watch_defaults(&self) -> DefaultsWatch {
        DefaultsWatch::default()
    }

    pub fn check_defaults(&self, holder: DefaultsWatch) {}

    /// `moved` is `mimeguard.changes(...)`: (mime, old id, new id).
    pub fn offer_defaults_back(&self, moved: Vec<(String, String, String)>) {}

    // ---- install from a link -----------------------------------------------
    /// Paste a GitHub link; see every way it ships before choosing one.
    pub fn on_install_link(&self, link: &str) {}

    pub fn show_link_options(&self, res: Resolution) {}

    pub fn install_option(&self, res: &Resolution, opt: &SourceOption) {}

    /// The app ran it, so it knows to re-read rather than guess.
    pub fn after_pacman(&self, ok: bool) {}

    /// AppImages are ours, so remove them directly -- no package manager
    /// is involved and there is nothing to confirm against.
    pub fn remove_appimage(&self, app: &App) {}

    /// Update one thing, with whatever tool owns it.
    pub fn on_update_one(&self, app: &App) {}

    /// Agents update themselves, or their version manager does it.
    pub fn update_agent(&self, app: &App) {}

    pub fn confirm_agent(&self, app: &App, cmd: &[String], notes: &str) {}

    /// Update a local checkout, taking any local edits into account.
    ///
    /// A clean checkout is a fast-forward. One with edits is first held up
    /// against the update (patches.py): an edit the update already contains
    /// is dropped, one that still applies is carried over, and one that
    /// conflicts is set aside as a stash. Edits are never discarded.
    pub fn on_git_pull(&self, app: &App) {}

    pub fn confirm_pull(&self, app: &App, repo: &str, state: &PatchState) {}

    /// Download the newer AppImage and swap it in, off the UI thread.
    pub fn on_update_appimage(&self, button: &gtk::Button, app: &App) {}

    /// `error` is `None` on success, else what went wrong.
    pub fn after_appimage(&self, name: &str, version: &str, error: Option<String>) {}

    /// Hand the full upgrade to the system's own updater, in a terminal.
    ///
    /// The terminal is the point: `omarchy update` needs a password, and a GUI
    /// cannot answer that prompt. Running it where the user can type is the
    /// honest solution; the alternative is passwordless root.
    ///
    /// The list rescans itself when the terminal closes. Leaving that to the
    /// user meant everything just updated stayed listed as needing an update.
    pub fn on_update_all(&self) {}

    pub fn on_add_appimage(&self) {}

    /// `Gtk.FileDialog.open` callback: `file` is `None` when cancelled.
    pub fn on_appimage_chosen(&self, file: Option<gtk::gio::File>) {}

    /// Copy into our own store and register it.
    ///
    /// Copying rather than referencing the download means a later cleanup of
    /// ~/Downloads cannot quietly break an installed app.
    pub fn install_appimage(&self, src: &Path) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn releases_url_from_github() {
        assert_eq!(releases_url("https://github.com/a/b/tree/main").as_deref(),
                   Some("https://github.com/a/b/releases"));
        assert_eq!(releases_url("example.com").as_deref(), Some("example.com"));
        assert_eq!(releases_url(""), None);
    }

    #[test]
    fn appimage_name_takes_stem_prefix() {
        assert_eq!(appimage_name(Path::new("/d/Foo-1.2-x86_64.AppImage")), "Foo");
        assert_eq!(appimage_name(Path::new("/d/-x.AppImage")), "-x");
    }
}
