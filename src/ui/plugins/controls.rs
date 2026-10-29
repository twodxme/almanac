//! On/off, shortcuts and restarting the shell.

use adw::prelude::*;
use gtk::{gdk, glib};

use crate::{history, plugins as plugins_mod, shortcuts, tasks};
use crate::i18n::tr;
use crate::ui::actions::Judge;
use crate::ui::shell::{Win, spawn_bg};
use crate::verify::{Outcome, ProbeValue, TryProbe};

use super::pyisms::{extra_str, popen_detached, py_oserror, py_oserror_class, uncaught, uncaught_err};

impl Win {
    pub fn row_toggle(&self, name: &str) {
        let app = self.shared.borrow().row_actions.get(name).cloned();
        if let Some(app) = app {
            let pid = extra_str(&app, "plugin_id");
            if !pid.is_empty() {
                let enabled = app.extra.get("enabled").is_some_and(|v| match v {
                    serde_json::Value::Bool(b) => *b,
                    serde_json::Value::Null => false,
                    serde_json::Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
                    serde_json::Value::String(s) => !s.is_empty(),
                    serde_json::Value::Array(a) => !a.is_empty(),
                    serde_json::Value::Object(o) => !o.is_empty(),
                });
                self.toggle_plugin(&pid, !enabled, &app.name);
            }
        }
    }

    /// Switch a plugin on or off through Omarchy, then check it took.
    pub fn toggle_plugin(&self, pid: &str, on: bool, label: &str) {
        if self.demo_refuses() {
            return;
        }
        let label = if label.is_empty() { pid.to_string() } else { label.to_string() };
        let p = pid.to_string();
        let probe: TryProbe =
            std::sync::Arc::new(move || Ok(plugins_mod::try_enabled()?.get(&p).map(|b| ProbeValue::Bool(*b))));
        let lb = label.clone();
        let judge: Judge = Box::new(move |_b, a| {
            let a = match a {
                Some(ProbeValue::Bool(b)) => Some(b),
                _ => None,
            };
            Outcome::new(
                Some(a == Some(on)),
                match a {
                    Some(a) => format!("{lb} is now {}", if a { "on" } else { "off" }),
                    None => format!("{lb}: state unknown"),
                },
            )
        });
        let w = self.downgrade();
        let then: tasks::OnDone = Box::new(move |_ok| {
            if let Some(w) = w.upgrade() {
                w.reload();
                let have = !w.shared.borrow().plugins.is_empty();
                if have {
                    w.store_refresh();
                }
            }
        });
        // `_checked` asks the probe first; what it raises ends this
        // before the task starts.
        let title = format!("{} {label}", if on { "Turning on" } else { "Turning off" });
        let steps = vec![tasks::Step::argv(
            format!("omarchy plugin {} {pid}", if on { "enable" } else { "disable" }),
            plugins_mod::toggle_argv(pid, on),
        )];
        let w = self.downgrade();
        self.checked(Some(probe), judge, Some(then), uncaught_err, move |on_done| {
            if let Some(w) = w.upgrade() {
                w.run_task(&title, steps, Some(on_done));
            }
        });
    }

    pub fn row_shortcut(&self, name: &str) {
        let app = self.shared.borrow().row_actions.get(name).cloned();
        if let Some(app) = app {
            let pid = extra_str(&app, "plugin_id");
            if !pid.is_empty() {
                self.edit_shortcut(&pid, &app.name);
            }
        }
    }

    /// Record a key combination and bind it to opening the plugin.
    pub fn edit_shortcut(&self, pid: &str, label: &str) {
        if self.demo_refuses() {
            return;
        }
        let existing = match shortcuts::current(pid, None) {
            Ok(e) => e,
            Err(exc) => {
                self.say(&exc, 8);
                return;
            }
        };
        let entry = gtk::Entry::builder()
            .placeholder_text(tr("Press the keys, e.g. Super + Alt + V"))
            .hexpand(true)
            .build();
        entry.set_text(&existing);
        let keys = gtk::EventControllerKey::new();

        let weak_entry = entry.downgrade();
        keys.connect_key_pressed(move |_c, keyval, _code, state| {
            let Some(entry) = weak_entry.upgrade() else { return glib::Propagation::Proceed };
            let name = keyval.name().map(|n| n.to_uppercase()).unwrap_or_default();
            if matches!(
                name.as_str(),
                "SUPER_L" | "SUPER_R" | "CONTROL_L" | "CONTROL_R" | "ALT_L" | "ALT_R" | "SHIFT_L"
                    | "SHIFT_R" | "META_L" | "META_R"
            ) {
                return glib::Propagation::Proceed; // wait for the actual key
            }
            let mut mods: Vec<String> = [
                ("SUPER", gdk::ModifierType::SUPER_MASK),
                ("CTRL", gdk::ModifierType::CONTROL_MASK),
                ("ALT", gdk::ModifierType::ALT_MASK),
                ("SHIFT", gdk::ModifierType::SHIFT_MASK),
            ]
            .iter()
            .filter(|(_m, mask)| state.intersects(*mask))
            .map(|(m, _mask)| m.to_string())
            .collect();
            if mods.is_empty() {
                return glib::Propagation::Proceed; // plain typing still edits the text
            }
            mods.push(name);
            entry.set_text(&mods.join(" + "));
            entry.set_position(-1);
            glib::Propagation::Stop
        });
        entry.add_controller(keys);

        let dialog = adw::AlertDialog::new(
            Some(&format!("Keyboard shortcut for {label}")),
            Some(&tr("Opens the plugin's panel. Checked against every binding \
                      Hyprland has now; written to its own block in \
                      bindings.lua, and put back if Hyprland rejects it.")),
        );
        dialog.set_extra_child(Some(&entry));
        dialog.add_response("cancel", &tr("Cancel"));
        if !existing.is_empty() {
            dialog.add_response("remove", &tr("Remove shortcut"));
            dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
        }
        dialog.add_response("save", &tr("Save"));
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("save"));
        dialog.set_close_response("cancel");

        let w = self.downgrade();
        let pid = pid.to_string();
        let entry_c = entry.clone();
        dialog.connect_response(None, move |_d, resp| {
            if resp != "save" && resp != "remove" {
                return;
            }
            let combo = if resp == "remove" { None } else { Some(entry_c.text().to_string()) };
            let pid = pid.clone();
            let w = w.clone();
            spawn_bg(
                // What record raises (a log that is not UTF-8) is a
                // ValueError: caught with the rest.
                move || match shortcuts::set_shortcut(&pid, combo.as_deref()) {
                    Ok(msg) => match history::record_checked(history::ALMANAC, &msg, "", "", &[]) {
                        Ok(()) => (msg, true),
                        Err(exc) => (format!("Shortcut not saved: {}", crate::ui::shell::exc_str(&exc)), false),
                    },
                    Err(exc) => (format!("Shortcut not saved: {exc}"), false),
                },
                move |(msg, saved)| {
                    let Some(w) = w.upgrade() else { return };
                    let on_page = w.shared.borrow().page_or("") == "plugins";
                    if saved && on_page {
                        w.plugins_reload();
                    }
                    w.say(&msg, 8);
                },
            );
        });
        dialog.present(Some(&self.window));
        entry.grab_focus();
    }

    pub fn on_restart_shell(&self) {
        if self.demo_refuses() {
            return;
        }
        let restart = || {
            let argv = plugins_mod::restart_argv(true);
            match popen_detached(&argv) {
                Ok(()) => {
                    if let Err(e) = history::record_checked(history::ALMANAC, "Shell restarted", "", "", &[]) {
                        uncaught_err(&e);
                    }
                }
                Err(e) => {
                    // Not caught in the Python: the OSError escapes the
                    // confirm callback, so its traceback goes to stderr and
                    // nothing is recorded.
                    let prog = argv.first().map(String::as_str).unwrap_or("");
                    uncaught(py_oserror_class(&e), &py_oserror(&e, prog));
                }
            }
        };

        let argv = plugins_mod::restart_argv(true);
        tasks::confirm(
            &self.window,
            &tr("Restart the shell?"),
            &tr("Restarts the Omarchy shell and clears its compiled QML cache. \
                 Use it when a plugin behaves as if it had not been updated. \
                 Your bar and settings are not touched."),
            &format!("$ {}", argv.get(3..).unwrap_or(&[]).join(" ")),
            &tr("Restart"),
            false,
            restart,
        );
    }
}
