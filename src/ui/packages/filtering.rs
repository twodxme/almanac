//! Sorting and filtering: the sort menu, the row filter, discover search, the
//! sidebar's selection and its folding groups.

use adw::prelude::*;
use gtk::{gio, glib};

use crate::i18n::tr;
use crate::inventory;
use crate::ui::shell::{AppItem, Win, spawn_bg};

use super::decisions::{SORT_KEYS, discover_total, group_state, match_item, sort_cmp};
use super::extra::py_strip;

impl Win {
    pub fn rebuild_sort_menu<K: AsRef<str>, L: AsRef<str>>(&self, options: &[(K, L)]) {
        let menu = gio::Menu::new();
        for (key, label) in options {
            menu.append(Some(label.as_ref()), Some(&format!("win.sort::{}", key.as_ref())));
        }
        self.sort_btn.set_menu_model(Some(&menu));
    }

    /// Sort by a field that is no longer its own column.
    ///
    /// Selecting one replaces the column sorter; clicking a column header
    /// afterwards takes precedence again, because the MultiSorter still has
    /// the view's sorter first.
    pub fn on_sort(&self, action: &gio::SimpleAction, param: Option<&glib::Variant>) {
        let Some(param) = param else { return };
        let key = param.str().unwrap_or_default().to_string();
        action.set_state(param);
        if key == "default" {
            self.shared.borrow_mut().manual_sorter = None;
        } else {
            // getters[key]: an unknown key raised KeyError, leaving the
            // sorter as it was.
            if !SORT_KEYS.contains(&key.as_str()) {
                return;
            }
            let sorter = gtk::CustomSorter::new(move |a, b| match (AppItem::of(a), AppItem::of(b)) {
                (Some(a), Some(b)) => sort_cmp(&key, &a, &b).into(),
                _ => gtk::Ordering::Equal,
            });
            self.shared.borrow_mut().manual_sorter = Some(sorter.upcast());
        }
        self.rebuild_sorter();
    }

    pub fn rebuild_sorter(&self) {
        let combined = gtk::MultiSorter::new();
        // combined.append(None) raised TypeError, leaving the sorter as it was.
        let Some(view_sorter) = self.view.sorter() else { return };
        combined.append(view_sorter); // a clicked header wins
        let (manual, sorted) = {
            let sh = self.shared.borrow();
            (sh.manual_sorter.clone(), sh.sorted.clone())
        };
        if let Some(manual) = manual {
            combined.append(manual);
        }
        combined.append(self.priority_sorter.clone()); // then urgency, then name
        sorted.set_sorter(Some(&combined));
    }

    /// `_match`: the row filter (`match` is a Rust keyword, hence `r#`).
    ///
    /// Borrow hazard: GTK runs this synchronously, once per row, from inside
    /// `filter.changed()`, `store.append()`, `store.remove_all()` and
    /// `set_model()` on anything fed by `self.filter`. It takes
    /// `shared.borrow()`, so calling any of those while holding
    /// `shared.borrow_mut()` panics with a BorrowError. Clone the store or
    /// filter out of `shared`, drop the borrow, then call (as
    /// `filter_changed`, `apply` and `show_discover` do).
    pub fn r#match(&self, item: &AppItem) -> bool {
        // Read in place: this runs once per row on every filter pass.
        let sh = self.shared.borrow();
        match_item(&sh.filter_key, &sh.query, item)
    }

    /// Query the repos and the AUR for what the search box says.
    ///
    /// Debounced: every keystroke would otherwise be a pacman -Ss plus an AUR
    /// round trip.
    pub fn run_discover(&self) {
        let old = {
            let mut sh = self.shared.borrow_mut();
            sh.discovering = true;
            sh.discover_timer.take()
        };
        if let Some(old) = old {
            old.remove();
        }
        let w = self.downgrade();
        let id = glib::timeout_add_local(std::time::Duration::from_millis(450), move || {
            match w.upgrade() {
                Some(w) => w.discover_now(),
                None => glib::ControlFlow::Break,
            }
        });
        self.shared.borrow_mut().discover_timer = Some(id);
    }

    pub fn discover_now(&self) -> glib::ControlFlow {
        self.shared.borrow_mut().discover_timer = None;
        let query = py_strip(&self.search.text()).to_string();
        let browsing = query.chars().count() < 2;
        self.summarize(
            &if browsing {
                tr("Loading the plugin marketplace…")
            } else {
                format!("Searching for “{query}”…")
            },
            None,
        );

        let q = query.clone();
        // What the searches raise ends the Python's worker before its
        // idle_add: the results are never shown.
        type Found = (Vec<inventory::App>, Option<Result<usize, String>>);
        let work = move || -> Result<Found, String> {
            if browsing {
                // Nothing typed: show the marketplace itself. Packages are
                // excluded here -- 15,669 of them is a database dump, not a
                // storefront, and there is no popularity signal to rank them.
                let plugins = inventory::try_search_plugins("", 200)?;
                // The summary's total, counted here and not on the main
                // thread: reading the registry is 8 MB of JSON, and a fetch
                // when the cache is a day old.
                let total = q.is_empty().then(|| discover_total(&inventory::plugin_registry_shared(false)));
                Ok((plugins, total))
            } else {
                let mut plugins = inventory::try_search_plugins(&q, 25)?;
                let packages = inventory::try_search_available(&q, 60)?;
                // Plugins lead: on an Omarchy box they are the likelier intent,
                // and the package list is long enough to bury them otherwise.
                plugins.extend(packages);
                Ok((plugins, None))
            }
        };
        let w = self.downgrade();
        spawn_bg(work, move |results| match results {
            Ok((results, total)) => {
                if let Some(w) = w.upgrade() {
                    w.show_discover(&query, results, total);
                }
            }
            Err(e) => eprintln!("almanac: {e}"),
        });
        glib::ControlFlow::Break
    }

    /// `total`: how many plugins the registry lists, counted by the worker
    /// (`discover_total`) when the query is empty.
    pub fn show_discover(&self, query: &str, results: Vec<inventory::App>, total: Option<Result<usize, String>>) {
        let discovering = self.shared.borrow().discovering;
        if !discovering {
            return; // user navigated away mid-search
        }
        self.set_rows(&results);
        self.filter_changed();
        if query.is_empty() {
            let total = match total {
                Some(Ok(n)) => n,
                Some(Err(e)) => {
                    eprintln!("almanac: _show_discover: {e}");
                    return;
                }
                None => 0,
            };
            self.summarize(
                &format!(
                    "Showing {} of {total} Omarchy plugins · search to include repo and AUR packages",
                    results.len()
                ),
                None,
            );
        } else {
            self.summarize(
                &if !results.is_empty() {
                    format!("{} installable result(s) for “{query}”", results.len())
                } else {
                    format!("Nothing found for “{query}”")
                },
                None,
            );
        }
        self.update_btn.set_sensitive(false);
    }

    pub fn on_search(&self, entry: &gtk::SearchEntry) {
        let (page, filter_key) = {
            let sh = self.shared.borrow();
            (sh.page_or(""), sh.filter_key.clone())
        };
        if page == "plugins" {
            self.plugins_filter(&entry.text());
            return;
        }
        if page == "switchboard" {
            self.switchboard_filter(&entry.text());
            return;
        }
        if filter_key == "discover" {
            if self.content_stack.visible_child_name().as_deref() == Some("store") {
                self.store_queue();
            } else {
                self.run_discover();
            }
            return;
        }
        // Debounced: each pass re-filters every row and re-sorts what is
        // left, and typing a word asked for one per letter.
        let query = py_strip(&entry.text()).to_lowercase();
        if let Some(old) = self.packages.borrow_mut().search_timer.take() {
            old.remove();
        }
        let w = self.downgrade();
        let id = glib::timeout_add_local_once(std::time::Duration::from_millis(150), move || {
            if let Some(w) = w.upgrade() {
                w.packages.borrow_mut().search_timer = None;
                w.search_now(query);
            }
        });
        self.packages.borrow_mut().search_timer = Some(id);
    }

    /// The search box's filter pass. A query that only grew keeps the rows
    /// already hidden hidden (GTK re-checks just the shown ones), and one
    /// that only shrank re-checks just the hidden ones.
    fn search_now(&self, query: String) {
        let old = std::mem::replace(&mut self.shared.borrow_mut().query, query.clone());
        let change = if query != old && query.contains(&old) {
            gtk::FilterChange::MoreStrict
        } else if query != old && old.contains(&query) {
            gtk::FilterChange::LessStrict
        } else {
            gtk::FilterChange::Different
        };
        let filter = self.shared.borrow().filter.clone();
        filter.changed(change);
        if let Err(e) = self.update_status() {
            eprintln!("almanac: on_search: {e}");
        }
    }

    /// Header rows fold their group; everything else is a category.
    pub fn on_sidebar_activated(&self, _list: &gtk::ListBox, row: &gtk::ListBoxRow) {
        let group = {
            let sh = self.shared.borrow();
            sh.group_rows
                .iter()
                .find_map(|(_k, h)| h.as_ref().filter(|h| h.row == *row).map(|h| h.group.clone()))
        };
        let Some(group) = group else { return };
        self.toggle_group(&group);
    }

    pub fn toggle_group(&self, gkey: &str) {
        {
            let mut sh = self.shared.borrow_mut();
            let collapsed = sh.collapsed.contains(gkey);
            if collapsed {
                sh.collapsed.remove(gkey);
            } else {
                sh.collapsed.insert(gkey.to_string());
            }
        }
        self.apply_group_state(gkey);
    }

    pub fn apply_group_state(&self, gkey: &str) {
        let plan = {
            let sh = self.shared.borrow();
            group_state(&sh.group_rows, &sh.group_members, &sh.collapsed, &sh.filter_key, gkey, |r| {
                r.key.as_str()
            })
        };
        let Some(plan) = plan else {
            return; // ungrouped primaries never fold
        };
        plan.header.chevron.set_icon_name(Some(plan.icon));
        for (row, visible) in plan.members {
            row.row.set_visible(visible);
        }
    }

    pub fn apply_all_group_states(&self) {
        let keys: Vec<String> =
            self.shared.borrow().group_rows.iter().map(|(k, _)| k.clone()).collect();
        for gkey in keys {
            self.apply_group_state(&gkey);
        }
    }

    /// Go to a category, whether or not its row is already selected.
    ///
    /// GtkListBox emits nothing when you select the row that is already
    /// selected, so the page never got applied in that case -- which is
    /// exactly what happens when a restored category and `--page` name the
    /// same destination, or when another path has already moved the
    /// selection. Applying it directly makes the call mean what it says.
    pub fn select_category(&self, key: &str) {
        let row = self.shared.borrow().sidebar_row(key).cloned();
        let Some(row) = row else { return };
        self.shared.borrow_mut().collapsed.remove(&row.group);
        self.apply_all_group_states();
        // _on_sidebar_selected ignores its list argument, so a selected row
        // is applied whatever its parent is.
        if row.row.is_selected() {
            self.sidebar_selected(row.row.upcast_ref());
        } else if let Some(list) = row.row.parent().and_downcast::<gtk::ListBox>() {
            list.select_row(Some(&row.row));
        }
    }

    pub fn on_sidebar_selected(&self, _list: &gtk::ListBox, row: Option<&gtk::ListBoxRow>) {
        let Some(row) = row else {
            return; // selection cleared during a rebuild
        };
        self.sidebar_selected(row);
    }

    /// The body of `_on_sidebar_selected`, which never reads its list.
    fn sidebar_selected(&self, row: &gtk::ListBoxRow) {
        let key = {
            let sh = self.shared.borrow();
            sh.sidebar_rows
                .iter()
                .find(|r| r.row.upcast_ref::<gtk::ListBoxRow>() == row)
                .map(|r| r.key.clone())
        };
        let Some(key) = key else { return };
        self.shared.borrow_mut().filter_key = key.clone();
        // A folded group keeps only the selected row showing; once the
        // selection moves on, that row folds away too. Without this a row
        // kept visible because it was selected stayed out after leaving it.
        self.apply_all_group_states();
        if self.shared.borrow().narrow {
            let split = self.split.clone();
            glib::idle_add_local_once(move || split.set_show_sidebar(false));
        }
        if key == "featured" {
            self.leave_store();
            self.show_page("featured", "");
            let first = !self.shared.borrow().feat_loaded_once;
            if first {
                self.shared.borrow_mut().feat_loaded_once = true;
                self.feat_reload();
            } else {
                let shelves = self.shared.borrow().shelves.clone();
                self.feat_show(shelves);
            }
            return;
        }

        if key == inventory::AGENT {
            self.leave_store();
            self.show_page(inventory::AGENT, "");
            let first = !self.shared.borrow().ai_loaded_once;
            if first {
                self.shared.borrow_mut().ai_loaded_once = true;
                self.ai_reload();
            } else {
                let survey = self.shared.borrow().ai_survey.clone();
                self.ai_show(&survey, true);
            }
            return;
        }

        if key == "history" {
            self.leave_store();
            self.show_page("history", "");
            self.history_reload();
            return;
        }

        if key == "plugins" {
            self.leave_store();
            self.show_page("plugins", "");
            self.plugins_reload();
            return;
        }

        if key == "switchboard" {
            self.leave_store();
            self.show_page("switchboard", "");
            self.switchboard_reload();
            return;
        }

        if key == "maintenance" {
            self.leave_store();
            self.show_page("maintenance", "");
            let first = !self.shared.borrow().maint_loaded_once;
            if first {
                self.shared.borrow_mut().maint_loaded_once = true;
                // Today ran the same checks moments ago: show its answer at
                // once, saying how old it is, and ask again only once it is
                // a few minutes old (and then with the old answer showing
                // meanwhile).
                match crate::maintenance::last_run() {
                    Some((age, found)) => {
                        self.maint_show(found);
                        let said = self.title.subtitle();
                        let when = crate::maintenance::checked_ago(age);
                        self.summarize(&format!("{said} · {when}"), Some("maintenance"));
                        if age >= crate::maintenance::FINDINGS_FRESH {
                            self.maint_reload();
                        }
                    }
                    None => self.maint_reload(),
                }
            } else {
                let findings = self.shared.borrow().findings.get();
                self.maint_show(findings);
            }
            return;
        }

        if key == "discover" || key == "themes" {
            self.shared.borrow_mut().discovering = true;
            self.show_page(&key, "");
            self.search.grab_focus();
            let first = !self.shared.borrow().store_loaded_once;
            if first {
                self.shared.borrow_mut().store_loaded_once = true;
                self.store_reload(false);
            } else {
                self.pin_market(&key);
                self.store_refresh();
            }
            return;
        }
        if self.leave_store() {
            self.title.set_title(&self.page_title(&key, "Software"));
            return;
        }
        self.show_page("packages", &key);
        self.filter_changed();
        if let Err(e) = self.update_status() {
            eprintln!("almanac: _on_sidebar_selected: {e}");
        }
    }
}
