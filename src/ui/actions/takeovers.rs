//! Which files an install took over: file-type defaults watched across an
//! install, and offered back.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use adw::prelude::*;

use crate::{history, mimeguard};
use crate::i18n::tr;
use crate::ui::shell::Win;

use super::{DefaultsWatch, MainGuard, idle, raised, start_thread};

impl Win {
    /// Read every file type's default app now, before an install.
    ///
    /// `_watch_defaults`; `Err` is what raised out of it.
    pub fn try_watch_defaults(&self) -> Result<DefaultsWatch, String> {
        let before: Arc<Mutex<Option<BTreeMap<String, String>>>> = Arc::new(Mutex::new(None));
        let theirs = before.clone();
        let (done, thread) = mpsc::channel::<()>();
        start_thread(move || {
            let _done = done;
            let snap = mimeguard::snapshot();
            if let Ok(mut b) = theirs.lock() {
                *b = snap;
            }
        })?;
        Ok(DefaultsWatch { before, thread })
    }

    /// `_check_defaults`; `Err` is what raised out of it.
    pub fn try_check_defaults(&self, holder: DefaultsWatch) -> Result<(), String> {
        let guard = MainGuard::new(self);
        start_thread(move || {
            let _ = holder.thread.recv_timeout(Duration::from_secs(90));
            let before = holder.before.lock().ok().and_then(|b| b.clone());
            let moved = mimeguard::changes(before.as_ref(), mimeguard::snapshot().as_ref());
            if !moved.is_empty() {
                idle(guard, move |win| win.offer_defaults_back(moved));
            }
        })
    }

    pub fn offer_defaults_back(&self, moved: Vec<(String, String, String)>) {
        let returnable: Vec<(String, String, String)> =
            moved.iter().filter(|m| !m.1.is_empty()).cloned().collect();
        let summary = match mimeguard::try_summary(&moved) {
            Ok(summary) => summary,
            Err(e) => return raised(&e),
        };
        let body = summary.join("\n")
            + &if !returnable.is_empty() {
                tr("\n\nGive them back to what opened them before?")
            } else {
                String::new()
            };
        let dialog = adw::AlertDialog::builder()
            .heading(tr("Some files now open in a different app"))
            .body(body)
            .build();
        dialog.add_response("keep", &tr("Keep the new app"));
        if !returnable.is_empty() {
            dialog.add_response("restore", &tr("Give them back"));
            dialog.set_response_appearance("restore", adw::ResponseAppearance::Suggested);
        }
        dialog.set_default_response(Some("keep"));
        dialog.set_close_response("keep");

        let w = self.downgrade();
        // What raises here ends `_offer_defaults_back` before the dialog
        // is shown.
        let recorded = mimeguard::try_summary(&moved)
            .and_then(|items| history::record_checked(history::ALMANAC, "File types changed hands", "", "", &items));
        if let Err(e) = recorded {
            return raised(&e);
        }
        dialog.connect_response(None, move |_d, resp| {
            if resp == "restore" {
                let n = match mimeguard::try_restore(&returnable, None) {
                    Ok(n) => n,
                    Err(e) => return raised(&e),
                };
                if let Some(win) = w.upgrade() {
                    win.say(&format!("{n} file type(s) handed back"), 5);
                }
                let recorded = mimeguard::try_summary(&returnable).and_then(|items| {
                    history::record_checked(history::ALMANAC, &format!("{n} file type(s) handed back"), "", "", &items)
                });
                if let Err(e) = recorded {
                    raised(&e);
                }
            }
        });
        dialog.present(Some(&self.window));
    }
}
