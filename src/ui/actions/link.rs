//! Install from a link: every way a project ships, then the chosen one.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;

use crate::i18n::tr;
use crate::{sources, tasks, verify};
use crate::sources::{Option as SourceOption, Resolution};
use crate::tasks::{LogFn, OnDone, Step};
use crate::ui::shell::{Win, spawn_bg};
use crate::verify::Outcome;

use super::pyisms::{probe_text, probe_truthy, py_strip, thousands};
use super::raised;
use super::update::yay_argv;

impl Win {
    /// Paste a GitHub link; see every way it ships before choosing one.
    pub fn on_install_link(&self, link: &str) {
        let entry = gtk::Entry::builder()
            .placeholder_text("https://github.com/owner/project")
            .activates_default(true)
            .hexpand(true)
            .build();
        entry.set_text(link);
        let dialog = adw::AlertDialog::builder()
            .heading(tr("Install from a link"))
            .body(tr(
                "Paste a GitHub project link. Every way it is published is compared, \
                 and one is recommended, before anything is downloaded.",
            ))
            .build();
        dialog.set_extra_child(Some(&entry));
        dialog.add_response("cancel", &tr("Cancel"));
        dialog.add_response("find", &tr("Look it up"));
        dialog.set_response_appearance("find", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("find"));
        dialog.set_close_response("cancel");

        let w = self.downgrade();
        dialog.connect_response(None, move |_d, resp| {
            if resp != "find" {
                return;
            }
            let Some(win) = w.upgrade() else { return };
            let text = py_strip(&entry.text()).to_string();
            if sources::parse(&text).is_none() {
                win.say(&tr("That is not a GitHub repository link"), 5);
                return;
            }
            win.say(&tr("Looking it up…"), 5);

            let w2 = win.downgrade();
            // What resolve raises ends the Python's worker: no options shown.
            spawn_bg(
                move || sources::try_resolve(&text),
                move |res| match res {
                    Ok(res) => {
                        if let Some(win) = w2.upgrade() {
                            win.show_link_options(res);
                        }
                    }
                    Err(e) => raised(&e),
                },
            );
        });
        dialog.present(Some(&self.window));
    }

    pub fn show_link_options(&self, res: Resolution) {
        if res.options.is_empty() {
            let msg = if res.error.is_empty() { "Nothing installable found".to_string() } else { res.error.clone() };
            self.say(&msg, 8);
            return;
        }
        let chosen: Rc<RefCell<Option<SourceOption>>> = Rc::new(RefCell::new(res.best().cloned()));
        let rows = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).build();
        rows.add_css_class("boxed-list");
        let mut group: Option<gtk::CheckButton> = None;
        for (i, o) in res.options.iter().enumerate() {
            // Name and version on the title; the file or package name, then
            // each reason and caveat on its own line, so none is cut off.
            let mut lines = vec![o.name.clone()];
            lines.extend(o.reasons.iter().map(|r| format!("✓ {r}")));
            lines.extend(o.warnings.iter().map(|w| format!("⚠ {w}")));
            let title = if !o.version.is_empty() {
                format!("{} · {}", o.title, o.version)
            } else {
                o.title.clone()
            };
            let row: adw::ActionRow = Win::plain(&title, &lines.join("\n"));
            row.set_subtitle_lines(0);
            let check = gtk::CheckButton::builder().valign(gtk::Align::Center).build();
            match &group {
                None => group = Some(check.clone()),
                Some(g) => check.set_group(Some(g)),
            }
            check.set_active(i == 0);
            let chosen_c = chosen.clone();
            let o_c = o.clone();
            check.connect_toggled(move |b| {
                if b.is_active() {
                    *chosen_c.borrow_mut() = Some(o_c.clone());
                }
            });
            row.add_prefix(&check);
            row.set_activatable_widget(Some(&check));
            if i == 0 {
                let pill = gtk::Label::builder()
                    .label(tr("Recommended"))
                    .valign(gtk::Align::Center)
                    .build();
                pill.add_css_class("pill-ok");
                row.add_suffix(&pill);
            }
            rows.append(&row);
        }
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_propagate_natural_height(true);
        scroll.set_max_content_height(420);
        scroll.set_child(Some(&rows));

        let mut facts = vec![format!("{} stars", thousands(res.stars))];
        if !res.license.is_empty() {
            facts.push(res.license.clone());
        }
        if res.archived {
            facts.push("archived".to_string());
        }
        let body = if !res.description.is_empty() {
            format!("{}\n", res.description)
        } else {
            String::new()
        } + &facts.join(" · ");
        let dialog = adw::AlertDialog::builder()
            .heading(format!("Install {}?", res.repo))
            .body(body)
            .build();
        dialog.set_body_use_markup(false);
        if dialog.find_property("prefer-wide-layout").is_some() {
            // libadwaita 1.6+
            dialog.set_property("prefer-wide-layout", true);
        }
        dialog.set_extra_child(Some(&scroll));
        dialog.add_response("cancel", &tr("Cancel"));
        dialog.add_response("install", &tr("Install"));
        dialog.set_response_appearance("install", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("install"));
        dialog.set_close_response("cancel");
        let w = self.downgrade();
        dialog.connect_response(None, move |_d, r| {
            if r == "install" {
                let opt = chosen.borrow().clone();
                if let (Some(win), Some(opt)) = (w.upgrade(), opt) {
                    win.install_option(&res, &opt);
                }
            }
        });
        dialog.present(Some(&self.window));
    }

    pub fn install_option(&self, res: &Resolution, opt: &SourceOption) {
        let holder = match self.try_watch_defaults() {
            Ok(h) => h,
            Err(e) => return raised(&e),
        };

        let w = self.downgrade();
        let after: OnDone = Box::new(move |ok| {
            if let Some(win) = w.upgrade() {
                if ok {
                    if let Err(e) = win.try_check_defaults(holder) {
                        return raised(&e);
                    }
                }
                win.after_pacman(ok);
            }
        });

        if opt.kind == sources::APPIMAGE {
            let (res_c, opt_c) = (res.clone(), opt.clone());
            let call: tasks::StepFn = Arc::new(move |log: LogFn<'_>| {
                sources::install_appimage(&res_c, &opt_c, log).map(|_| ())
            });
            tasks::run(
                &self.window,
                &format!("Installing {}", res.repo),
                vec![Step::call(format!("Downloading {}", opt.name), call)],
                Some(after),
            );
            return;
        }
        let argv = yay_argv(&opt.name);
        let name = opt.name.clone();
        let judge_name = name.clone();
        let steps = match self.snapshot_first(vec![Step::argv(format!("Installing {name}"), argv)],
                                              &format!("installing {name}")) {
            Ok(steps) => steps,
            Err(e) => return raised(&e),
        };
        let w = self.downgrade();
        self.checked(
            Some(verify::installed_package_try(&name)),
            Box::new(move |b, a| {
                let landed = probe_truthy(&a) && a != b;
                Outcome::new(
                    Some(landed),
                    if landed {
                        format!("{judge_name} {} installed", probe_text(&a))
                    } else {
                        format!("{judge_name} was not installed")
                    },
                )
            }),
            Some(after),
            raised,
            move |done| {
                if let Some(win) = w.upgrade() {
                    tasks::run(&win.window, &format!("Installing {name}"), steps, Some(done));
                }
            },
        );
    }

    /// The app ran it, so it knows to re-read rather than guess.
    ///
    /// Whether or not it worked: a batch where one item failed still has
    /// the others done, and they must leave the Updates list. The pending
    /// updates are asked afresh, not taken from what the last scan kept.
    pub fn after_pacman(&self, _ok: bool) {
        crate::inventory::forget_pending();
        self.reload();
    }
}
