//! External change detection: pacman.log and the watched configs.

use std::collections::BTreeSet;
use std::path::PathBuf;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::i18n::tr;

use super::helpers::spawn_bg;
use super::window::Win;

impl Win {
    /// Rescan when the package database changes, whoever changed it.
    ///
    /// pacman.log gets one line per transaction, so a single upgrade fires
    /// many events -- hence the debounce. Keeping a reference to the monitor
    /// matters: a GC'd FileMonitor silently stops delivering.
    pub fn watch_pacman(&self) {
        self.shell.borrow_mut().pending_rescan = None;
        let gfile = gio::File::for_path("/var/log/pacman.log");
        let monitor = match gfile.monitor_file(gio::FileMonitorFlags::NONE, None::<&gio::Cancellable>) {
            Ok(m) => {
                let w = self.downgrade();
                m.connect_changed(move |_, _, _, _| {
                    if let Some(w) = w.upgrade() {
                        w.on_pacman_changed();
                    }
                });
                Some(m)
            }
            Err(_) => None, // no watch is survivable; Rescan still works
        };
        self.shell.borrow_mut().monitor = monitor;
    }

    /// Notice the moment a tool resets a config file to its defaults.
    ///
    /// The Health check finds a reset whenever it runs; this makes it
    /// immediate while the window is open. Only the directories the
    /// distribution ships defaults for are watched, since nothing else can
    /// be "reset to defaults".
    pub fn watch_configs(&self) {
        let pending = {
            let mut st = self.shell.borrow_mut();
            st.config_monitors.clear();
            st.config_pending.take()
        };
        // A debounce still waiting would otherwise fire after the re-watch.
        // Live by construction: `config_check` clears the id before it
        // returns Break, so a stored id never names a source already gone.
        if let Some(old) = pending {
            old.remove();
        }
        // Where `guard.resets()` raises the Python's window is never built;
        // here it is logged and nothing is watched.
        let resets = match crate::guard::try_resets_at(None, crate::guard::PyStack::WINDOW) {
            Ok(resets) => resets,
            Err(e) => {
                eprintln!("almanac: {e}");
                return;
            }
        };
        let reported: BTreeSet<String> = resets
            .iter()
            .map(|r| r.backup.to_string_lossy().into_owned())
            .collect();
        self.shell.borrow_mut().config_reported = reported;
        let Some(root) = crate::guard::defaults_root() else { return };
        let mut subs: Vec<PathBuf> = if root.is_dir() {
            std::fs::read_dir(&root)
                .map(|rd| rd.flatten().map(|e| e.path()).collect())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        subs.sort();
        for sub in subs {
            let Some(sub_name) = sub.file_name() else { continue };
            let target = crate::guard::config_home().join(sub_name);
            if !target.is_dir() {
                continue;
            }
            let mon = match gio::File::for_path(&target)
                .monitor_directory(gio::FileMonitorFlags::NONE, None::<&gio::Cancellable>)
            {
                Ok(m) => m,
                Err(_) => continue,
            };
            let w = self.downgrade();
            mon.connect_changed(move |_, file, _other, event| {
                if let Some(w) = w.upgrade() {
                    w.on_config_changed(file, event);
                }
            });
            self.shell.borrow_mut().config_monitors.push(mon);
        }
    }

    /// `_on_config_changed`: a CREATED backup file starts a 3 s debounce.
    pub fn on_config_changed(&self, file: &gio::File, event: gio::FileMonitorEvent) {
        if event != gio::FileMonitorEvent::Created {
            return;
        }
        let base = file.basename().map(|b| b.to_string_lossy().into_owned()).unwrap_or_default();
        if !crate::guard::BAK.is_match(&base) {
            return;
        }
        if let Some(old) = self.shell.borrow_mut().config_pending.take() {
            old.remove();
        }
        // The backup is written before the defaults are copied in; wait for both.
        let w = self.downgrade();
        let id = glib::timeout_add_seconds_local(3, move || match w.upgrade() {
            Some(w) => w.config_check(),
            None => glib::ControlFlow::Break,
        });
        self.shell.borrow_mut().config_pending = Some(id);
    }

    /// `_config_check`: look for resets off the main thread. One-shot timer.
    pub fn config_check(&self) -> glib::ControlFlow {
        self.shell.borrow_mut().config_pending = None;
        let w = self.downgrade();
        // What `guard.resets()` raises ends the Python's worker before its
        // idle_add: nothing is reported.
        spawn_bg(
            || crate::guard::try_resets_at(None, crate::guard::PyStack::WORKER),
            move |found| match found {
                Ok(found) => {
                    if let Some(w) = w.upgrade() {
                        w.config_report(found);
                    }
                }
                Err(e) => eprintln!("almanac: {e}"),
            },
        );
        glib::ControlFlow::Break
    }

    /// `_config_report`: announce resets not reported before.
    pub fn config_report(&self, found: Vec<crate::guard::Reset>) {
        let new: Vec<&crate::guard::Reset> = {
            let mut st = self.shell.borrow_mut();
            let new = found
                .iter()
                .filter(|r| !st.config_reported.contains(&*r.backup.to_string_lossy()))
                .collect();
            st.config_reported
                .extend(found.iter().map(|r| r.backup.to_string_lossy().into_owned()));
            new
        };
        if new.is_empty() {
            return;
        }
        let names = new.iter().map(|r| r.name()).collect::<Vec<_>>().join(", ");
        let lost: usize = new.iter().map(|r| r.lost.len()).sum();
        let body = format!(
            "{names} was replaced with the shipped defaults{}. Your version was kept — restore it \
             from Health check.",
            if lost != 0 { format!(", removing {lost} of your entries") } else { String::new() }
        );
        if crate::settings::get_bool("notify_resets") {
            self.notify(&tr("Your settings were reset"), &body, "critical");
        }
        self.say(&tr("Settings reset to defaults — see Health check"), 10);
        if self.content_stack.visible_child_name().as_deref() == Some("maintenance") {
            self.maint_reload();
        }
    }

    /// `_on_pacman_changed`: wait for the transaction to settle (6 s), then
    /// rescan once.
    pub fn on_pacman_changed(&self) {
        if let Some(old) = self.shell.borrow_mut().pending_rescan.take() {
            old.remove();
        }
        // Wait for the transaction to settle, then rescan once.
        let w = self.downgrade();
        let id = glib::timeout_add_seconds_local(6, move || match w.upgrade() {
            Some(w) => w.rescan_now(),
            None => glib::ControlFlow::Break,
        });
        self.shell.borrow_mut().pending_rescan = Some(id);
    }

    /// `_rescan_now`: the debounced rescan. One-shot timer.
    ///
    /// Skipped when a scan started after pacman.log last changed: the app's
    /// own actions rescan when they finish, and the watcher then saw the
    /// same transaction and asked for a second full scan six seconds later.
    pub fn rescan_now(&self) -> glib::ControlFlow {
        self.shell.borrow_mut().pending_rescan = None;
        let changed = std::fs::metadata(crate::snapshots::PACMAN_LOG).and_then(|m| m.modified());
        if changed.is_ok_and(|changed| self.scanned_since(changed)) {
            println!("pacman.log changed — already rescanned");
            return glib::ControlFlow::Break;
        }
        // Logged as well as shown, so the watch can be verified from a terminal
        // rather than only by staring at the window.
        println!("pacman.log changed — rescanning");
        self.summarize(&tr("Packages changed — rescanning…"), None);
        self.reload();
        glib::ControlFlow::Break // one-shot
    }
}
