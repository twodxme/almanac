//! UI section `actions` (port of `app.py` lines 5530-5636 and 5999-6481):
//! row actions, self-update and About, verification of outcomes, file-type
//! takeovers, install from a link, update one/all, AppImage add/remove.
//! One file per concern under `actions/`, re-exported here.

use std::collections::BTreeMap;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use gtk::glib;

use crate::ui::shell::{Win, WeakWin};
use crate::verify::{Outcome, ProbeValue};

mod appimage_add;
mod link;
mod pyisms;
mod rows;
mod takeovers;
mod update;
mod verification;

pub use appimage_add::{appimage_clash, appimage_name, AddClash};
pub(in crate::ui) use pyisms::truthy;
pub use rows::releases_url;
pub(in crate::ui) use update::{appimage_update_work, popen_new_session, update_cmd};
pub use update::{
    delete_appimage_files, needs_full_upgrade, remove_appimage_with, update_all_script, update_one,
    with_snapshot, yay_argv, Removal, UpdateOne,
};

#[derive(Default)]
pub struct State {}

/// `judge` of `_checked`: turns the before and after probe answers into what
/// the user is told. Runs on a worker thread.
pub type Judge = Box<dyn FnOnce(Option<ProbeValue>, Option<ProbeValue>) -> Outcome + Send>;

/// A `Judge` that can be asked again.
type JudgeAgain = Arc<dyn Fn(Option<ProbeValue>, Option<ProbeValue>) -> Outcome + Send + Sync>;

/// The `holder` dict of `_watch_defaults`: every file type's default app,
/// read on a worker thread before an install.
pub struct DefaultsWatch {
    /// `holder["before"]`: `mimeguard.snapshot()`, filled in by the thread.
    pub before: Arc<Mutex<Option<BTreeMap<String, String>>>>,
    /// `holder["thread"]`: disconnects when the thread is done, so
    /// `recv_timeout` is `join(timeout=...)`.
    pub thread: Receiver<()>,
}

/// A window handle a worker thread can carry and hand back to the main
/// thread in `glib::idle_add_once` (`GLib.idle_add`).
struct MainGuard(Option<glib::thread_guard::ThreadGuard<WeakWin>>);

impl MainGuard {
    fn new(win: &Win) -> Guard {
        Arc::new(MainGuard(Some(glib::thread_guard::ThreadGuard::new(win.downgrade()))))
    }

    fn upgrade(&self) -> Option<Win> {
        self.0.as_ref().and_then(|g| g.get_ref().upgrade())
    }
}

impl Drop for MainGuard {
    fn drop(&mut self) {
        // Dropped on the worker -- it had nothing to hand back, or it
        // panicked -- the handle is let go of without being touched:
        // ThreadGuard panics off its own thread, and a panic while unwinding
        // aborts the app, where a Python thread's exception only prints.
        if let Some(g) = self.0.take() {
            if g.is_owner() {
                drop(g);
            } else {
                std::mem::forget(g);
            }
        }
    }
}

type Guard = Arc<MainGuard>;

/// `GLib.idle_add(fn, ...)` from a worker thread.
fn idle(guard: Guard, f: impl FnOnce(&Win) + Send + 'static) {
    glib::idle_add_once(move || {
        if let Some(win) = guard.upgrade() {
            f(&win);
        }
    });
}

/// `threading.Thread(target=work, daemon=True).start()`; `Err` is the
/// RuntimeError `start()` raises.
fn start_thread(work: impl FnOnce() + Send + 'static) -> Result<(), String> {
    std::thread::Builder::new()
        .spawn(work)
        .map(|_| ())
        .map_err(|_| "RuntimeError: can't start new thread".to_string())
}

/// An exception the Python lets out of a GTK handler: PyGObject prints it
/// to stderr and the handler stops there.
pub(super) fn raised(exc: &str) {
    eprintln!("{exc}");
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    const CSS: &str = include_str!("../../data/style.css");

    /// The code under test, without the test modules: this file and the
    /// section's files under actions/.
    fn code() -> String {
        // file!() is this file, from the crate root.
        let root = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/", file!()));
        let dir = root.with_extension("");
        std::iter::once(root.to_path_buf())
            .chain(
                ["rows.rs", "verification.rs", "takeovers.rs", "link.rs", "update.rs", "appimage_add.rs", "pyisms.rs"]
                    .iter()
                    .map(|f| dir.join(f)),
            )
            .map(|p| {
                let src = std::fs::read_to_string(p).unwrap_or_default();
                src.split("#[cfg(test)]").next().unwrap_or("").to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// test_every_pill_class_is_defined_in_the_stylesheet (this section)
    #[test]
    fn every_pill_class_is_defined_in_the_stylesheet() {
        const PILLS: [&str; 6] = ["pill", "pill-crit", "pill-warn", "pill-ok", "pill-accent", "pill-quiet"];
        let re = regex::Regex::new(r#""(pill(?:-[a-z]+)?)""#).unwrap();
        let code = code();
        assert!(!code.is_empty());
        let used: Vec<String> = re.captures_iter(&code).map(|c| c[1].to_string()).collect();
        assert!(!used.is_empty(), "no pill classes found at all");
        for name in &used {
            assert!(CSS.contains(&format!(".{name}")), "{name} is applied but never styled");
            assert!(PILLS.contains(&name.as_str()), "unknown pill variant: {name}");
        }
    }

    /// test_spacing_comes_from_the_scale (this section)
    #[test]
    fn spacing_comes_from_the_scale() {
        const SCALE: [i64; 8] = [2, 4, 6, 8, 12, 16, 20, 24];
        let re = regex::Regex::new(
            r"\b(?:set_)?(margin_(?:start|end|top|bottom)|spacing|child_spacing|line_spacing)\((\d+)\)").unwrap();
        let off: Vec<String> = re
            .captures_iter(&code())
            .filter(|c| !SCALE.contains(&c[2].parse::<i64>().unwrap_or(-1)))
            .map(|c| format!("{}={}", &c[1], &c[2]))
            .collect();
        assert!(off.is_empty(), "spacing off the scale: {off:?}");
    }

    /// test_every_icon_only_button_is_named (this section): any icon-only
    /// button made here must reach `name(` within a few lines.
    #[test]
    fn every_icon_only_button_is_named() {
        let code = code();
        let lines: Vec<&str> = code.lines().collect();
        let mut unnamed = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            if line.contains("Button::from_icon_name") || line.contains(".icon_name(") {
                let window = lines[i..(i + 12).min(lines.len())].join("\n");
                if !window.contains("name(&") && !window.contains("Property::Label") {
                    unnamed.push(format!("actions.rs:{}", i + 1));
                }
            }
        }
        assert!(unnamed.is_empty(), "{unnamed:?}");
    }

    /// test_every_icon_name_is_in_adwaita_or_shipped_here (this section)
    #[test]
    fn every_icon_name_is_in_adwaita_or_shipped_here() {
        let adwaita = Path::new("/usr/share/icons/Adwaita");
        if !adwaita.is_dir() {
            return;
        }
        let re = regex::Regex::new(r#""([a-z0-9-]+-symbolic)""#).unwrap();
        for c in re.captures_iter(&code()) {
            let n = &c[1];
            let shipped = Path::new(env!("CARGO_MANIFEST_DIR")).join("icons").join(format!("{n}.svg"));
            let found = shipped.exists()
                || glob::glob(&format!("{}/**/{n}.*", adwaita.display()))
                    .map(|mut g| g.next().is_some())
                    .unwrap_or(false);
            assert!(found || n.starts_with("adw-"), "missing icon {n}");
        }
    }

    /// test_no_personal_paths_in_the_source (this section)
    #[test]
    fn no_personal_paths_in_the_source() {
        let home = format!("{}/", crate::util::home().display());
        assert!(!code().contains(&home));
        assert!(!code().to_lowercase().contains("streamdock"));
    }
}
