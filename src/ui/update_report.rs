//! After Update all: the report of what passed and what did not, the
//! repairs, the tidy-up, and the run's one history entry.
//!
//! Everything passing is a toast, as before. Anything failed, or that could
//! not be told, is a dialog that says which check and why in plain words,
//! with Fix when there is a repair for it. Fix runs the repairs after that
//! one click (root ones still ask polkit), then asks the checks again and
//! shows what they say now.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;

use adw::prelude::*;

use crate::history::{self, CommandRun};
use crate::i18n::tr;
use crate::inventory::App;
use crate::postupdate::{self, After, Before, CheckId, Repair, Report, RunFiles, RunRecord, Status};
use crate::tasks::{self, Step, StepAction};
use crate::ui::shell::{spawn_bg, Win};
use crate::verify::Outcome;

/// One Update all, from the click to its history entry.
pub struct Run {
    pub started: Instant,
    pub started_wall: f64,
    /// a pacman/AUR update is part of it
    pub pacman: bool,
    /// the terminal half ran
    pub terminal: bool,
    pub command: String,
    pub files: Option<RunFiles>,
    pub before: Before,
    pub report: Report,
    pub record: RunRecord,
}

pub type SharedRun = Rc<RefCell<Run>>;

impl Run {
    pub fn new(pacman: bool) -> SharedRun {
        Rc::new(RefCell::new(Run {
            started: Instant::now(),
            started_wall: crate::util::now(),
            pacman,
            terminal: false,
            command: String::new(),
            files: None,
            before: Before::default(),
            report: Report::default(),
            record: RunRecord::default(),
        }))
    }
}

/// What the batch after the terminal did, each row judged by its probe.
#[derive(Default)]
pub struct BatchResult {
    pub outcomes: Vec<(App, Outcome)>,
    pub held: Vec<String>,
    pub lines: Vec<String>,
    /// it ran (it was not empty, and the user said yes)
    pub ran: bool,
    pub ok: bool,
}

/// Told what the batch did, once it is over.
pub type BatchDone = Box<dyn FnOnce(&Win, BatchResult)>;

/// A step as the history keeps it.
fn step_text(s: &Step) -> String {
    match &s.action {
        StepAction::Argv(a) => tasks::elevate(a).join(" "),
        StepAction::Call(_) => s.label.clone(),
    }
}

fn ran(steps: &[Step], ok: bool) -> Vec<CommandRun> {
    // A task reports one result for all its steps: each is known to have
    // exited 0 only when the whole task did.
    steps.iter().map(|s| CommandRun { argv: step_text(s), exit: ok.then_some(0), secs: None }).collect()
}

fn icon(status: Status) -> &'static str {
    match status {
        Status::Fail => "dialog-error-symbolic",
        Status::Unknown => "dialog-question-symbolic",
        Status::Info => "dialog-information-symbolic",
        Status::Pass => "object-select-symbolic",
    }
}

impl Win {
    /// The checks are in: a toast when all passed, else the report.
    pub(in crate::ui) fn surface_update(&self, run: SharedRun) {
        let report = run.borrow().report.clone();
        if report.passed() {
            self.say(&report.summary(), 8);
            self.tidy_up(run);
        } else {
            self.show_update_report(run, report, true);
        }
    }

    fn show_update_report(&self, run: SharedRun, report: Report, first: bool) {
        let attention = report.attention().len();
        let repairs = if first { report.repairs() } else { Vec::new() };
        let has_yay = crate::util::which("yay").is_some();
        let lines: Vec<String> = repairs.iter().filter_map(|r| postupdate::describe(r, has_yay)).collect();
        let fixable = !postupdate::repair_steps(&repairs, has_yay).is_empty();

        let mut body = if first {
            format!("{} after the update did not pass.", if attention == 1 { "One check".to_string() } else { format!("{attention} checks") })
        } else {
            tr("The repairs ran, and this is what the checks say now.")
        };
        body.push(' ');
        body += &if fixable {
            tr("Fix runs the repairs listed below, then checks again.")
        } else {
            tr("These need a person; the log shows what happened.")
        };

        let list = Win::note_list();
        let dialog = adw::AlertDialog::builder().heading(if first {
            tr("The update needs a look")
        } else {
            tr("Not everything is right yet")
        })
        .body(body)
        .build();
        dialog.set_body_use_markup(false);
        for c in report.checks.iter().filter(|c| c.needs_attention() || c.status == Status::Info) {
            let mut text = c.detail.clone();
            let shown: Vec<&String> = c.items.iter().filter(|i| !c.detail.contains(i.as_str())).take(8).collect();
            if !shown.is_empty() {
                text += "\n";
                text += &shown.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n");
                if c.items.len() > 8 {
                    text += "\n…";
                }
            }
            let row = Win::note_row(icon(c.status), &c.title, &text);
            if c.id == CheckId::Pacnew {
                let open = gtk::Button::builder().label(tr("Open Health")).valign(gtk::Align::Center).build();
                let (w, d) = (self.downgrade(), dialog.downgrade());
                open.connect_clicked(move |_| {
                    if let Some(d) = d.upgrade() {
                        d.close();
                    }
                    if let Some(win) = w.upgrade() {
                        win.select_category("maintenance");
                    }
                });
                row.add_suffix(&open);
            }
            list.append(&row);
        }
        if fixable {
            list.append(&Win::note_row("emblem-system-symbolic", &tr("Fix will"), &lines.join("\n")));
        }
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_propagate_natural_height(true);
        scroll.set_max_content_height(380);
        scroll.set_child(Some(&list));
        let show_log = gtk::Button::builder().label(tr("Show log")).halign(gtk::Align::Start).build();
        show_log.add_css_class("flat");
        let log = report.log.clone();
        let w = self.downgrade();
        show_log.connect_clicked(move |_| {
            if let Some(win) = w.upgrade() {
                win.show_update_log(&log);
            }
        });
        let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
        content.append(&scroll);
        content.append(&show_log);
        dialog.set_extra_child(Some(&content));
        if dialog.find_property("prefer-wide-layout").is_some() {
            // libadwaita 1.6+
            dialog.set_property("prefer-wide-layout", true);
        }
        dialog.add_response("close", &tr("Close"));
        if fixable {
            dialog.add_response("fix", &tr("Fix"));
            dialog.set_response_appearance("fix", adw::ResponseAppearance::Suggested);
            dialog.set_default_response(Some("fix"));
        }
        dialog.set_close_response("close");
        let w = self.downgrade();
        let run = RefCell::new(Some(run));
        dialog.connect_response(None, move |_d, resp| {
            let (Some(win), Some(run)) = (w.upgrade(), run.borrow_mut().take()) else { return };
            if resp == "fix" {
                win.run_update_repairs(run, repairs.clone(), report.clone());
            } else {
                win.finish_update(run);
            }
        });
        dialog.present(Some(&self.window));
    }

    /// pacman's log section and the terminal's output, as they were.
    fn show_update_log(&self, text: &str) {
        let buf = gtk::TextBuffer::new(None);
        buf.set_text(if text.trim().is_empty() { "Nothing was logged." } else { text });
        let view = gtk::TextView::builder().buffer(&buf).editable(false).monospace(true).cursor_visible(false).build();
        view.set_wrap_mode(gtk::WrapMode::WordChar);
        view.set_left_margin(8);
        view.set_right_margin(8);
        view.set_top_margin(6);
        view.set_bottom_margin(6);
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_min_content_height(320);
        scroll.set_max_content_height(480);
        scroll.set_child(Some(&view));
        scroll.add_css_class("card");
        let dialog = adw::AlertDialog::new(Some(&tr("What the update did")), None);
        dialog.set_extra_child(Some(&scroll));
        if dialog.find_property("prefer-wide-layout").is_some() {
            dialog.set_property("prefer-wide-layout", true);
        }
        dialog.add_response("close", &tr("Close"));
        dialog.set_close_response("close");
        dialog.present(Some(&self.window));
    }

    /// Fix: the repairs, then the checks that had them, asked again.
    fn run_update_repairs(&self, run: SharedRun, repairs: Vec<Repair>, report: Report) {
        let steps = postupdate::repair_steps(&repairs, crate::util::which("yay").is_some());
        let w = self.downgrade();
        let done_steps = steps.clone();
        tasks::run_each(
            &self.window,
            &tr("Repairing the update"),
            steps,
            Some(Box::new(move |ok| {
                let Some(win) = w.upgrade() else { return };
                let (pacman, before) = {
                    let mut r = run.borrow_mut();
                    r.record.commands.extend(ran(&done_steps, ok));
                    (r.pacman, r.before.clone())
                };
                win.say(&tr("Checking again…"), 5);
                let w = win.downgrade();
                spawn_bg(
                    move || postupdate::probe_after(pacman),
                    move |after: After| {
                        let Some(win) = w.upgrade() else { return };
                        let again = report.recheck(&before, &after);
                        run.borrow_mut().record.recheck = Some(again.clone());
                        win.mark_updated(&postupdate::installed_since(&before, &after));
                        win.after_pacman(true);
                        if again.passed() {
                            win.say(&tr("Repaired — every check passes now"), 8);
                            win.tidy_up(run);
                        } else {
                            win.show_update_report(run, again, false);
                        }
                    },
                );
            })),
        );
    }

    /// After a system update that went through: the package cache trimmed
    /// and the build cache of what was just built cleared, with the space
    /// that freed measured, not added up. Skipped without a word when
    /// there is nothing to do it with.
    fn tidy_up(&self, run: SharedRun) {
        let (eligible, updated) = {
            let r = run.borrow();
            let passed = r.record.last().is_some_and(Report::passed);
            let names: Vec<String> =
                r.report.changes.iter().filter(|c| !c.to.is_empty()).map(|c| c.name.clone()).collect();
            (r.pacman && r.terminal && passed, names)
        };
        if !eligible {
            return self.finish_update(run);
        }
        let steps = postupdate::cleanup_steps(crate::util::which("paccache").is_some(), &updated);
        if steps.is_empty() {
            return self.finish_update(run);
        }
        let free = || {
            let paths: Vec<PathBuf> = postupdate::cache_paths();
            let refs: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
            postupdate::free_total(&refs)
        };
        let w = self.downgrade();
        spawn_bg(free, move |before| {
            let Some(win) = w.upgrade() else { return };
            let w = win.downgrade();
            let done_steps = steps.clone();
            tasks::run(
                &win.window,
                &tr("Tidying up after the update"),
                steps,
                Some(Box::new(move |ok| {
                    run.borrow_mut().record.commands.extend(ran(&done_steps, ok));
                    spawn_bg(free, move |after| {
                        let Some(win) = w.upgrade() else { return };
                        let freed = before.zip(after).map(|(b, a)| a as i64 - b as i64);
                        run.borrow_mut().record.freed = freed;
                        if let Some(b) = freed.filter(|b| *b >= 1048576) {
                            win.say(&format!("Freed {}", crate::maintenance::fmt_size(b as f64 / 1048576.0)), 5);
                        }
                        win.finish_update(run);
                    });
                })),
            );
        });
    }

    /// The run's one history entry, and its files let go of.
    pub(in crate::ui) fn finish_update(&self, run: SharedRun) {
        let r = run.borrow();
        let action = r.record.action(r.started.elapsed().as_secs_f64());
        if let Err(e) = history::record_action(history::ALMANAC, "Update all", &r.record.detail(), r.record.severity(), &action) {
            eprintln!("almanac: {e}");
        }
        if let Some(f) = &r.files {
            f.remove();
        }
    }
}
