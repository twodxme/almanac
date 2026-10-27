//! Data loading: the scan, notifications and health reports, the rows, the
//! status line and update notes.

use adw::prelude::*;
use gtk::{gio, glib};

use crate::i18n::tr;
use crate::inventory;
use crate::ui::shell::{AppItem, Win, spawn_bg};

use super::extra::text_of;
use super::SAY_TIMEOUT;

impl Win {
    /// `self.lookup_action(name)`, as the SimpleAction it always is here.
    fn win_action(&self, act_name: &str) -> Option<gio::SimpleAction> {
        self.window.lookup_action(act_name).and_downcast::<gio::SimpleAction>()
    }

    /// Every row of `self.store`, as the model's own objects (a reference
    /// each, not a copy of the row); read them in place with `AppItem::of`.
    pub(in crate::ui) fn store_objects(&self) -> Vec<glib::Object> {
        let store = self.shared.borrow().store.clone();
        (0..store.n_items()).filter_map(|i| store.item(i)).collect()
    }

    /// How many rows of `self.store` satisfy `pred`, read in place.
    pub(super) fn count_items(objects: &[glib::Object], pred: impl Fn(&AppItem) -> bool) -> usize {
        objects.iter().filter(|o| AppItem::of(o).is_some_and(|it| pred(&it))).count()
    }

    /// `self.filter.changed(Gtk.FilterChange.DIFFERENT)`.
    pub(super) fn filter_changed(&self) {
        let filter = self.shared.borrow().filter.clone();
        filter.changed(gtk::FilterChange::Different);
    }

    // ---- data ------------------------------------------------------------
    pub fn reload(&self) {
        {
            let mut st = self.packages.borrow_mut();
            if st.scanning {
                st.scan_again = true;
                return;
            }
            st.scanning = true;
            st.scan_again = false;
            st.scan_started = Some(std::time::SystemTime::now());
        }
        if let Some(act) = self.win_action("rescan") {
            act.set_enabled(false);
        }
        self.summarize(&tr("Scanning…"), None);

        // checkupdates and yay take seconds; on the UI thread that is a freeze.
        let w = self.downgrade();
        spawn_bg(
            || std::panic::catch_unwind(inventory::collect).ok(),
            move |apps| {
                if let Some(w) = w.upgrade() {
                    w.scan_done(apps);
                }
            },
        );
    }

    /// A scan landed: shown, unless `reload` was called meanwhile, in which
    /// case it is dropped for the one more scan that call asked for.
    fn scan_done(&self, apps: Option<Vec<inventory::App>>) {
        let again = {
            let mut st = self.packages.borrow_mut();
            st.scanning = false;
            std::mem::take(&mut st.scan_again)
        };
        if again {
            self.reload();
            return;
        }
        match apps {
            Some(apps) => {
                self.packages.borrow_mut().last_scan = Some(apps.clone());
                self.apply(apps);
            }
            // The scan panicked: the list stays as it was, rescanning allowed.
            None => {
                if let Some(act) = self.win_action("rescan") {
                    act.set_enabled(true);
                }
            }
        }
    }

    /// Whether a scan started after `changed`: the watcher's rescan for a
    /// change that old would find nothing new.
    pub fn scanned_since(&self, changed: std::time::SystemTime) -> bool {
        self.packages.borrow().scan_started.is_some_and(|started| started > changed)
    }

    /// Desktop notification, falling back to the status line.
    ///
    /// Infallible form of `try_notify` for callers with nothing left to do
    /// after it: the error Python would raise is reported on stderr.
    pub fn notify(&self, title: &str, body: &str, urgency: &str) {
        if let Err(e) = self.try_notify(title, body, urgency) {
            eprintln!("almanac: _notify: {e}");
        }
    }

    /// `_notify` as Python runs it: Err where Popen raises something other
    /// than OSError -- a NUL in an argument is a ValueError, which escaped
    /// to the caller and skipped the rest of it. Only a failed spawn (the
    /// OSError) falls back to the status line.
    pub fn try_notify(&self, title: &str, body: &str, urgency: &str) -> Result<(), String> {
        // In plain words once, here: the status line below is `toast`, not
        // `say`, which would put them in plain words again.
        let (title, body) = (&crate::util::human_error(title), &crate::util::human_error(body));
        let tool = crate::util::which("omarchy-notification-send")
            .or_else(|| crate::util::which("notify-send"));
        let Some(tool) = tool else {
            self.toast(&format!("{title}: {body}"), SAY_TIMEOUT);
            return Ok(());
        };
        if [urgency, title, body].iter().any(|a| a.contains('\0'))
            || tool.as_os_str().as_encoded_bytes().contains(&0)
        {
            return Err("ValueError: embedded null byte".to_string());
        }
        // start_new_session=True: the child calls setsid() before exec, so
        // it leaves this session and its controlling terminal and outlives a
        // SIGHUP when the launching terminal closes. GLib spawns through an
        // intermediate child, so nothing here has to wait for it; the
        // environment and stdin are inherited, as Popen's are.
        let env: Vec<std::path::PathBuf> = std::env::vars_os()
            .map(|(k, v)| {
                let mut kv = k;
                kv.push("=");
                kv.push(v);
                std::path::PathBuf::from(kv)
            })
            .collect();
        let envp: Vec<&std::path::Path> = env.iter().map(|p| p.as_path()).collect();
        let argv: Vec<&std::path::Path> = vec![
            tool.as_path(),
            std::path::Path::new("-u"),
            std::path::Path::new(urgency),
            std::path::Path::new(title),
            std::path::Path::new(body),
        ];
        let spawned = glib::spawn_async(
            None::<&std::path::Path>,
            &argv,
            &envp,
            glib::SpawnFlags::CHILD_INHERITS_STDIN,
            Some(Box::new(|| {
                let _ = rustix::process::setsid();
            })),
        );
        if spawned.is_err() {
            self.toast(&format!("{title}: {body}"), SAY_TIMEOUT);
        }
        Ok(())
    }

    /// Tell the user once about things that need a decision, not a click.
    ///
    /// Only new findings are announced: a notification repeated on every
    /// rescan is one the user learns to dismiss without reading.
    pub fn report_health(&self, apps: &[inventory::App]) {
        let abandoned: Vec<&inventory::App> = apps
            .iter()
            .filter(|a| matches!(text_of(a.extra.get("stale")).as_str(), "orphaned" | "flagged"))
            .collect();
        let dupes = inventory::duplicates(apps);

        let mut findings = std::collections::BTreeSet::new();
        for a in &abandoned {
            findings.insert(format!("stale:{}:{}", a.name, text_of(a.extra.get("stale"))));
        }
        for (k, _v) in &dupes {
            findings.insert(format!("dupe:{k}"));
        }

        let new: Vec<String> = {
            let mut sh = self.shared.borrow_mut();
            let new = findings.difference(&sh.reported).cloned().collect();
            sh.reported.extend(findings.iter().cloned());
            new
        };
        if new.is_empty() {
            return;
        }

        if !abandoned.is_empty() && new.iter().any(|f| f.starts_with("stale:")) {
            let lines: Vec<String> = abandoned
                .iter()
                .map(|a| {
                    format!(
                        "{} — {}",
                        a.name,
                        if text_of(a.extra.get("stale")) == "orphaned" {
                            "no maintainer in the AUR"
                        } else {
                            "flagged out of date by its maintainer"
                        }
                    )
                })
                .collect();
            let body = lines.iter().take(5).cloned().collect::<Vec<_>>().join("\n");
            // A _notify that raised ended _report_health here.
            if let Err(e) = self.try_notify(&tr("Software no longer maintained"), &body, "critical") {
                eprintln!("almanac: _report_health: {e}");
                return;
            }
        }

        if !dupes.is_empty() && new.iter().any(|f| f.starts_with("dupe:")) {
            let lines: Vec<String> = dupes
                .iter()
                .take(5)
                .map(|(k, v)| format!("{k}: {}", v.join(", ")))
                .collect();
            self.notify(&tr("Duplicate software installed"), &lines.join("\n"), "normal");
        }
    }

    /// Replace every row of `self.store` with `apps`.
    ///
    /// Row-menu actions find their app by name in `row_actions`, which rows
    /// fill as they are bound. Names no longer in the table are forgotten
    /// here, so it holds what the table holds rather than every row ever
    /// shown (each AUR search added its results); a row still listed keeps
    /// its entry, and gets a fresh one when it is bound again.
    pub(super) fn set_rows(&self, apps: &[inventory::App]) {
        let store = self.shared.borrow().store.clone();
        store.remove_all();
        for a in apps {
            store.append(&AppItem::new(a.clone()).boxed());
        }
        let names: std::collections::HashSet<&str> = apps.iter().map(|a| a.name.as_str()).collect();
        self.shared.borrow_mut().row_actions.retain(|name, _| names.contains(name.as_str()));
    }

    /// Rows whose update was verified installed show it at once, ahead of
    /// the rescan that confirms it: they leave Updates and the header's
    /// count, rather than waiting seconds for checkupdates.
    pub fn mark_updated(&self, names: &[String]) {
        if names.is_empty() {
            return;
        }
        let store = self.shared.borrow().store.clone();
        let mut changed = false;
        for i in 0..store.n_items() {
            let Some(obj) = store.item(i) else { continue };
            let done = match AppItem::of(&obj) {
                Some(it) if it.has_update && names.contains(&it.name) => Some(it.app.as_updated()),
                _ => None,
            };
            if let Some(app) = done {
                store.splice(i, 1, &[AppItem::new(app).boxed()]);
                changed = true;
            }
        }
        if changed {
            self.filter_changed();
            if let Err(e) = self.update_status() {
                eprintln!("almanac: mark_updated: {e}");
            }
        }
    }

    pub fn apply(&self, apps: Vec<inventory::App>) {
        self.set_rows(&apps);
        if let Some(act) = self.win_action("rescan") {
            act.set_enabled(true);
        }
        self.filter_changed();
        // An exception out of _update_status ended _apply before the health
        // report, as Python's traceback did.
        if let Err(e) = self.update_status() {
            eprintln!("almanac: _apply: {e}");
            return;
        }
        self.report_health(&apps);
    }

    /// Err where Python's `_update_status` raised (out of `_refresh_stats`);
    /// everything up to that point has been applied, the rest has not.
    pub fn update_status(&self) -> Result<(), String> {
        let items = self.store_objects();
        let total = items.len();
        let updates = Win::count_items(&items, |i| i.has_update);
        // Clone, then ask GTK: never hold the borrow across the call.
        let (sorted, filter_key, query) = {
            let sh = self.shared.borrow();
            (sh.sorted.clone(), sh.filter_key.clone(), sh.query.clone())
        };
        let shown = sorted.n_items() as usize;
        let crit = Win::count_items(&items, |i| !i.critical.is_empty());

        let orphan_n = Win::count_items(&items, |i| i.source == inventory::ORPHAN);
        let installed = total - orphan_n;
        let mut text = format!("{installed} installed · {updates} with updates");
        if crit != 0 {
            text += &format!(" · {crit} critical");
        }
        if orphan_n != 0 {
            text += &format!(" · {orphan_n} orphans");
        }
        if shown != total {
            text += &format!(" · showing {shown}");
        }
        // Say which kind of empty this is, rather than showing a blank table.
        if shown == 0 {
            let (mut icon, mut title, mut body) = match crate::ui::history::empty_states(&filter_key)
            {
                Some((icon, title, body)) => (icon, title, body),
                None => (
                    "system-search-symbolic",
                    "Nothing matches".to_string(),
                    "Try a different search.".to_string(),
                ),
            };
            if !query.is_empty() {
                icon = "system-search-symbolic";
                title = "Nothing matches".to_string();
                body = format!("No installed software matches “{query}”.");
            }
            self.list_empty.set_icon_name(Some(icon));
            self.list_empty.set_title(&title);
            self.list_empty.set_description(Some(&body));
            self.list_stack.set_visible_child_name("empty");
        } else {
            self.list_stack.set_visible_child_name("list");
        }

        self.refresh_update_notes(&filter_key, &query);
        self.summarize(&text, Some("packages"));
        self.sync_primary();
        self.update_sidebar_counts();
        self.refresh_stats()?;
        let removable =
            Win::count_items(&items, |i| i.source == inventory::ORPHAN && i.advice != "keep");
        if let Some(act) = self.win_action("clean") {
            act.set_enabled(removable > 0);
        }
        // The header button's label is sync_primary's alone: it belongs to
        // the page on show, and a scan finishing under Health or Plugins must
        // not relabel their button "Update all" while it still runs theirs.
        Ok(())
    }

    /// The notes above the Updates table: on that page only, not while a
    /// search narrows it, and only when `mirror::notes` has something to say.
    pub fn refresh_update_notes(&self, filter_key: &str, query: &str) {
        while let Some(child) = self.update_notes.first_child() {
            self.update_notes.remove(&child);
        }
        let notes = if filter_key == "updates" && query.is_empty() {
            // Pending repository packages: what a mirror batch brings.
            // Omarchy's own packages (System) come from its own repository,
            // not the Arch batch.
            let pending: Vec<String> = self
                .store_objects()
                .iter()
                .filter_map(|o| AppItem::of(o).map(|i| i.clone()))
                .filter(|i| i.has_update && i.source == inventory::REPO)
                .map(|i| i.app.name)
                .collect();
            let all = Win::count_items(&self.store_objects(), |i| i.has_update);
            crate::mirror::notes(&crate::mirror::read(), &pending, all)
        } else {
            Vec::new()
        };
        for note in &notes {
            self.update_notes.append(&Win::note_row(note.icon, &note.title, &note.body));
        }
        self.update_notes.set_visible(!notes.is_empty());
    }
}
