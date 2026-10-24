//! Shortcuts and persisted window state.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use serde_json::{Map, Value};

use crate::inventory;

use super::helpers::truthy;
use super::tables::{PAGE_KEYS, page_accel};
use super::window::Win;

impl Win {
    /// `_install_shortcuts`: Ctrl+F, Ctrl+R/F5, Ctrl+U, Ctrl+W, Ctrl+comma,
    /// Escape, and Ctrl+1…Ctrl+0 for `PAGE_KEYS`.
    pub fn install_shortcuts(&self) {
        let controller = gtk::ShortcutController::new();
        controller.set_scope(gtk::ShortcutScope::Global);

        let add = |accel: &str, f: Rc<dyn Fn(&Win)>| {
            let w = self.downgrade();
            let action = gtk::CallbackAction::new(move |_, _| {
                if let Some(w) = w.upgrade() {
                    f(&w);
                }
                glib::Propagation::Stop
            });
            controller.add_shortcut(gtk::Shortcut::new(
                gtk::ShortcutTrigger::parse_string(accel),
                Some(action),
            ));
        };

        add("<Control>f", Rc::new(|w| {
            w.search.grab_focus();
        }));
        // Refresh means "re-read this page", the same as the menu item. It
        // used to always re-scan packages, whatever you were looking at.
        add("<Control>r", Rc::new(|w| w.refresh_page()));
        add("F5", Rc::new(|w| w.refresh_page()));
        add("<Control>u", Rc::new(|w| w.on_update_all()));
        add("<Control>w", Rc::new(|w| w.window.close()));
        add("<Control>comma", Rc::new(|w| w.on_preferences())); // the GNOME convention
        add("Escape", Rc::new(|w| w.clear_search()));

        // The primary destinations are reachable without the mouse, in the
        // order they appear in the sidebar: Ctrl+1 to Ctrl+9, then Ctrl+0.
        // There is no Ctrl+10 -- an eleventh page needs another scheme, and
        // PAGE_KEYS is checked by a test because an invalid accelerator makes
        // the window fail to open at all.
        for (i, key) in PAGE_KEYS.iter().enumerate() {
            let key = *key;
            add(&page_accel(i + 1), Rc::new(move |w| w.select_category(key)));
        }
        self.window.add_controller(controller);
    }

    /// `_clear_search`: Escape empties the search box, or, when it is already
    /// empty, drops the focus.
    pub fn clear_search(&self) {
        if !self.search.text().is_empty() {
            self.search.set_text("");
        } else {
            GtkWindowExt::set_focus(&self.window, None::<&gtk::Widget>);
        }
    }

    /// `_state_path`: window-state.json beside the AppImage registry.
    pub fn state_path(&self) -> PathBuf {
        let reg = inventory::registry();
        reg.parent().map(|p| p.to_path_buf()).unwrap_or_default().join("window-state.json")
    }

    /// Window size, chip and sort survive a restart -- otherwise every
    /// launch dumps you back on an unsorted "All".
    pub fn restore_state(&self) {
        let st = match std::fs::read_to_string(self.state_path())
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        {
            Some(Value::Object(st)) => st,
            _ => return,
        };
        // isinstance(x, int): an integral JSON number (a bool is an int too,
        // but never > 400).
        let int = |k: &str| st.get(k).and_then(|v| v.as_i64().or_else(|| v.as_bool().map(i64::from)));
        if let (Some(w), Some(h)) = (int("width"), int("height")) {
            if w > 400 && h > 300 {
                self.window.set_default_size(w as i32, h as i32);
            }
        }
        if let Some(Value::Array(groups)) = st.get("collapsed") {
            let collapsed: BTreeSet<String> = {
                let sh = self.shared.borrow();
                groups
                    .iter()
                    .filter_map(|g| g.as_str())
                    .filter(|g| sh.has_group(g))
                    .map(str::to_string)
                    .collect()
            };
            self.shared.borrow_mut().collapsed = collapsed;
            self.apply_all_group_states();
        }
        let mut chip = st.get("chip").and_then(|v| v.as_str()).map(str::to_string);
        let start = crate::settings::get_str("start_page");
        let known = |k: &str| self.shared.borrow().sidebar_row(k).is_some();
        if start != "last" && known(&start) {
            chip = Some(start);
        }
        if let Some(chip) = chip {
            if known(&chip) {
                self.select_category(&chip);
            }
        }
        let sort_col = st.get("sort_col").and_then(|v| v.as_str()).map(str::to_string);
        let sort_desc = truthy(st.get("sort_desc"));
        self.shell.borrow_mut().wanted_sort = Some((sort_col, sort_desc));
    }

    /// `_save_state`: write size, chip, sort and collapsed groups.
    pub fn save_state(&self) {
        let (mut col_title, mut desc) = (String::new(), false);
        if let Some(sorter) = self.view.sorter().and_downcast::<gtk::ColumnViewSorter>() {
            if let Some(c) = sorter.primary_sort_column() {
                col_title = c.title().map(|t| t.to_string()).unwrap_or_default();
                desc = sorter.primary_sort_order() == gtk::SortType::Descending;
            }
        }
        let (chip, collapsed) = {
            let sh = self.shared.borrow();
            (sh.filter_key.clone(), sh.collapsed.iter().cloned().collect::<Vec<_>>())
        };
        let mut st = Map::new();
        st.insert("width".into(), self.window.width().into());
        st.insert("height".into(), self.window.height().into());
        st.insert("chip".into(), chip.into());
        st.insert("sort_col".into(), col_title.into());
        st.insert("sort_desc".into(), desc.into());
        st.insert("collapsed".into(), collapsed.into());
        // Path.write_text: no directory is made, and any failure is dropped.
        let text = crate::util::dumps(&Value::Object(st), Some(2), false);
        let _ = crate::util::atomic_write(self.state_path(), text); // losing window state is not worth an error
    }

    /// `AlmanacApp._on_close`: save the window state, then let the close
    /// proceed.
    pub fn on_close(&self) -> glib::Propagation {
        self.save_state();
        glib::Propagation::Proceed
    }
}
