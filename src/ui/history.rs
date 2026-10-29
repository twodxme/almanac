//! UI section `history` (port of part of `app.py`): maintenance glue and
//! History (`app.py` lines 3675-3695 and 4221-4486).

use adw::prelude::*;

use crate::history;
use crate::i18n::tr;
use crate::inventory;
use crate::maintenance;
use crate::tasks::{self, OnDone, Step};
use crate::ui::shell::{spawn_bg, Win};
use crate::verify::{self, Outcome};

/// Widgets and data only the History and Health check pages use.
///
/// `_findings` is not here: Updates, the store header and Today read it too,
/// so it lives on `shell::Shared` as `findings`.
#[derive(Default)]
pub struct State {
    /// `self.maint_list`: the rows box of the one Health check section.
    pub maint_list: Option<gtk::ListBox>,
    /// `self.history_body`: one section per day goes in here.
    pub history_body: Option<gtk::Box>,
}

// ---- history ----------------------------------------------------------

/// `_HISTORY_ICONS`: the icon for each kind of history event.
pub const HISTORY_ICONS: &[(&str, &str)] = &[
    (history::PACKAGES, "package-x-generic-symbolic"),
    (history::SNAPSHOT, "camera-photo-symbolic"),
    (history::RESET, "edit-undo-symbolic"),
    (history::ALMANAC, "object-select-symbolic"),
    (history::AUTOUPDATE, "software-update-available-symbolic"),
];

/// `_HISTORY_ICONS.get(kind, "object-select-symbolic")`.
pub fn history_icon(kind: &str) -> &'static str {
    HISTORY_ICONS
        .iter()
        .find(|(k, _)| *k == kind)
        .map(|(_, icon)| *icon)
        .unwrap_or("object-select-symbolic")
}

/// (icon, title, body)
pub type EmptyState = (&'static str, String, String);

/// `EMPTY_STATES`: (icon, title, body) for a filter that shows nothing.
/// Read by the packages section when a table comes up empty.
///
/// Python runs `_()` on these once, when the class is defined, so they are
/// translated once here too and kept.
pub fn empty_states(key: &str) -> Option<EmptyState> {
    thread_local! {
        static CACHE: std::cell::RefCell<Vec<(String, Option<EmptyState>)>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }
    if let Some(hit) = CACHE.with(|c| {
        c.borrow().iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }) {
        return hit;
    }
    let state = empty_states_uncached(key);
    CACHE.with(|c| c.borrow_mut().push((key.to_string(), state.clone())));
    state
}

fn empty_states_uncached(key: &str) -> Option<(&'static str, String, String)> {
    let state = match key {
        "updates" => (
            "object-select-symbolic",
            tr("Everything is up to date"),
            tr("Nothing installed has a newer version waiting."),
        ),
        k if k == inventory::ORPHAN => (
            "object-select-symbolic",
            tr("No orphans"),
            tr("Every installed package is either asked for directly or needed by something that is."),
        ),
        k if k == inventory::FLATPAK => (
            "package-x-generic-symbolic",
            tr("No Flatpaks"),
            tr("Nothing is installed through Flatpak on this machine."),
        ),
        k if k == inventory::APPIMAGE => (
            "application-x-executable-symbolic",
            tr("No AppImages tracked"),
            tr("Install one from the App Store, or add an existing file with ☰ → Track an AppImage."),
        ),
        k if k == inventory::LOCAL => (
            "applications-engineering-symbolic",
            tr("No local builds"),
            tr("Nothing here is running from a git checkout."),
        ),
        k if k == inventory::AUR => (
            "package-x-generic-symbolic",
            tr("No AUR packages"),
            tr("Nothing foreign is installed."),
        ),
        _ => return None,
    };
    Some(state)
}

/// (pill label, pill css class)
pub type SeverityPill = (String, &'static str);

/// `_SEVERITY_CSS`: (pill label, pill css class) per finding severity.
/// Read by the Today section too.
///
/// Translated once and kept, as the class-level `_()` in Python is.
pub fn severity_css(severity: &str) -> Option<SeverityPill> {
    thread_local! {
        static CACHE: std::cell::RefCell<Vec<(String, Option<SeverityPill>)>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }
    if let Some(hit) = CACHE.with(|c| {
        c.borrow().iter().find(|(k, _)| k == severity).map(|(_, v)| v.clone())
    }) {
        return hit;
    }
    let css = severity_css_uncached(severity);
    CACHE.with(|c| c.borrow_mut().push((severity.to_string(), css.clone())));
    css
}

fn severity_css_uncached(severity: &str) -> Option<(String, &'static str)> {
    match severity {
        s if s == maintenance::CRIT => Some((tr("Critical"), "pill-crit")),
        s if s == maintenance::WARN => Some((tr("Attention"), "pill-warn")),
        s if s == maintenance::INFO => Some((tr("Optional"), "pill")),
        s if s == maintenance::OK => Some((tr("OK"), "pill-ok")),
        s if s == maintenance::UNKNOWN => Some((tr("Couldn't check"), "pill")),
        _ => None,
    }
}

/// `datetime.fromtimestamp(e.ts).strftime("%H:%M")`; `None` where Python raises.
fn clock_text(ts: f64) -> Option<String> {
    use chrono::Datelike;
    if !ts.is_finite() {
        return None;
    }
    let (mut secs, frac) = (ts.trunc(), ts.fract());
    let us = crate::util::py_round(frac * 1e6);
    if us >= 1e6 {
        secs += 1.0;
    } else if us < 0.0 {
        secs -= 1.0;
    }
    let d = chrono::DateTime::from_timestamp(secs as i64, 0)?.with_timezone(&chrono::Local);
    if !(1..=9999).contains(&d.year()) {
        return None;
    }
    Some(d.format("%H:%M").to_string())
}

/// `(label, f.fix)`: a step for each command the fix runs.
fn fix_steps(label: String, f: &maintenance::Finding) -> Vec<Step> {
    tasks::fix_steps(&label, f.fix.clone())
}

fn panic_text(err: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = err.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = err.downcast_ref::<String>() {
        s.clone()
    } else {
        // str(exc) of a bare exception is "".
        String::new()
    }
}

/// A pass, a failure or "could not tell" as an icon, named for a screen
/// reader since the icon is all there is.
fn verdict_icon(ok: Option<bool>, words: &str) -> gtk::Image {
    let (icon, class) = match ok {
        Some(true) => ("object-select-symbolic", "store-verified"),
        Some(false) => ("dialog-error-symbolic", "error"),
        None => ("dialog-question-symbolic", "dim-label"),
    };
    let image = gtk::Image::builder().icon_name(icon).valign(gtk::Align::Center).build();
    image.add_css_class(class);
    crate::ui::shell::name(&image, words);
    image
}

/// A dim caption at the end of a row: an exit status, a verdict, versions.
fn caption(text: &str) -> gtk::Label {
    let label = gtk::Label::builder().label(text).valign(gtk::Align::Center).build();
    label.add_css_class("dim-label");
    label.add_css_class("caption");
    label.add_css_class("numeric");
    label
}

/// The rows an open event shows of what its action did: each command and
/// how it ended, what changed, what was checked and repaired, and the
/// snapshot, space freed and time taken.
fn action_rows(a: &history::ActionLog) -> Vec<adw::ActionRow> {
    let mut rows = Vec::new();
    for c in &a.commands {
        let ok = history::exit_ok(c.exit);
        let row: adw::ActionRow = Win::plain(&c.argv, "");
        // The command as it ran, in the face a terminal would show it in.
        row.add_css_class("monospace");
        row.set_title_selectable(true);
        row.add_prefix(&verdict_icon(ok, history::verdict_text(ok)));
        let mut end = history::exit_text(c.exit);
        if let Some(s) = c.secs {
            end += &format!(" · {}", history::duration_text(s));
        }
        row.add_suffix(&caption(&end));
        rows.push(row);
    }
    for c in &a.changes {
        let row: adw::ActionRow = Win::plain(&c.name, "");
        let icon = gtk::Image::from_icon_name(history_icon(history::PACKAGES));
        icon.set_accessible_role(gtk::AccessibleRole::Presentation);
        row.add_prefix(&icon);
        row.add_suffix(&caption(&history::change_text(c)));
        rows.push(row);
    }
    let checked = |r: &history::CheckResult, verdict: &str| {
        let row: adw::ActionRow = Win::plain(&r.name, &r.detail);
        row.set_subtitle_lines(3);
        row.add_prefix(&verdict_icon(r.ok, verdict));
        row.add_suffix(&caption(verdict));
        row
    };
    for r in &a.checks {
        let verdict = match r.ok {
            Some(true) => tr("Passed"),
            Some(false) => tr("Failed"),
            None => tr("Could not tell"),
        };
        rows.push(checked(r, &verdict));
    }
    for r in &a.repairs {
        let verdict = match r.ok {
            Some(true) => tr("Repaired"),
            Some(false) => tr("Not repaired"),
            None => tr("Could not tell"),
        };
        rows.push(checked(r, &verdict));
    }
    let facts = a.facts();
    if !facts.is_empty() {
        let row: adw::ActionRow = Win::plain(&facts, "");
        row.add_css_class("dim-label");
        // In line with the rows above, which all lead with an icon.
        let icon = gtk::Image::from_icon_name("document-open-recent-symbolic");
        icon.set_accessible_role(gtk::AccessibleRole::Presentation);
        row.add_prefix(&icon);
        rows.push(row);
    }
    rows
}

/// Free space on the root filesystem, in bytes: measured either side of a
/// cleanup, so History says what it actually freed.
fn free_bytes() -> Option<u64> {
    let st = rustix::fs::statvfs("/").ok()?;
    Some(st.f_bavail.saturating_mul(st.f_frsize))
}

/// What a Health check fix is recorded with: what it runs, and the clock
/// and free space before it started.
struct FixRun {
    /// the History title for a run of several fixes
    title: String,
    commands: Vec<String>,
    free_before: Option<u64>,
    started: std::time::Instant,
}

impl FixRun {
    /// The command lines `steps` will run, elevated as the task runs them.
    fn of(title: &str, steps: &[Step], free_before: Option<u64>) -> Self {
        let commands = steps
            .iter()
            .filter_map(|s| match &s.action {
                tasks::StepAction::Argv(a) if !a.is_empty() => Some(history::command_line(&tasks::elevate(a))),
                _ => None,
            })
            .collect();
        FixRun { title: title.to_string(), commands, free_before, started: std::time::Instant::now() }
    }

    /// The History entry for the run, once each fixed check has been asked
    /// again. `ok` is the task's: every command exited 0. When it is not,
    /// which one failed is not known, and the record says so.
    fn record(&self, ok: bool, fixed: &[maintenance::Finding], outs: &[Outcome], free_after: Option<u64>) -> Result<(), String> {
        let action = history::ActionLog {
            commands: self
                .commands
                .iter()
                .map(|argv| history::CommandRun { argv: argv.clone(), exit: ok.then_some(0), secs: None })
                .collect(),
            checks: fixed
                .iter()
                .zip(outs)
                .map(|(f, o)| history::CheckResult { name: f.title.clone(), ok: o.changed, detail: o.message.clone() })
                .collect(),
            freed_bytes: match (self.free_before, free_after) {
                (Some(b), Some(a)) => Some(a as i64 - b as i64),
                _ => None,
            },
            duration_secs: Some(self.started.elapsed().as_secs_f64()),
            ..Default::default()
        };
        let title = match outs {
            [one] => one.message.clone(),
            _ => self.title.clone(),
        };
        let detail = fixed.iter().map(|f| f.fix_label.as_str()).collect::<Vec<_>>().join(" · ");
        let failed = !ok || outs.iter().any(|o| o.changed == Some(false));
        history::record_action(history::ALMANAC, &title, &detail, if failed { "warn" } else { "" }, &action)
    }
}

enum Row {
    Action(adw::ActionRow),
    Expander(adw::ExpanderRow),
}

impl Row {
    fn add_prefix(&self, w: &impl IsA<gtk::Widget>) {
        match self {
            Row::Action(r) => r.add_prefix(w),
            Row::Expander(r) => r.add_prefix(w),
        }
    }

    fn add_suffix(&self, w: &impl IsA<gtk::Widget>) {
        match self {
            Row::Action(r) => r.add_suffix(w),
            Row::Expander(r) => r.add_suffix(w),
        }
    }

    fn set_subtitle_lines(&self, lines: i32) {
        match self {
            Row::Action(r) => r.set_subtitle_lines(lines),
            Row::Expander(r) => r.set_subtitle_lines(lines),
        }
    }

    fn row(&self) -> adw::PreferencesRow {
        match self {
            Row::Action(r) => r.clone().upcast(),
            Row::Expander(r) => r.clone().upcast(),
        }
    }
}

impl Win {
    // ---- maintenance ------------------------------------------------------

    /// One page per question the system can answer about its own health.
    pub fn build_maintenance(&self) -> gtk::Box {
        let page = gtk::Box::new(gtk::Orientation::Vertical, 0);

        let body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_start(12)
            .margin_end(12)
            .margin_bottom(16)
            .build();
        // No heading: the page is already called Health check.
        let section = Win::section(
            "",
            &tr("Each check reads the system and offers the one command that acts on what it found. Nothing here runs until you ask it to."),
            None,
        );
        self.history.borrow_mut().maint_list = Some(section.rows.clone());
        body.append(&section.root);

        let scroller = gtk::ScrolledWindow::builder().vexpand(true).build();
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroller.set_child(Some(&body));
        page.append(&scroller);
        page
    }

    // ---- history ----------------------------------------------------------

    /// What changed on this machine, by day. See history.py.
    pub fn build_history(&self) -> gtk::Box {
        let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let history_body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_start(12)
            .margin_end(12)
            .margin_bottom(16)
            .build();
        self.history.borrow_mut().history_body = Some(history_body.clone());
        let scroller = gtk::ScrolledWindow::builder().vexpand(true).build();
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroller.set_child(Some(&history_body));
        page.append(&scroller);
        page
    }

    pub fn history_reload(&self) {
        self.summarize(&tr("Reading the last 30 days…"), Some("history"));

        let w = self.downgrade();
        spawn_bg(
            || history::timeline(history::DAYS),
            move |days| {
                if let Some(w) = w.upgrade() {
                    w.history_show(days);
                }
            },
        );
    }

    pub fn history_show(&self, days: Vec<history::Day>) {
        let Some(history_body) = self.history.borrow().history_body.clone() else {
            // Python raises AttributeError here and PyGObject prints it.
            eprintln!("AttributeError: 'Almanac' object has no attribute 'history_body'");
            return;
        };
        while let Some(child) = history_body.first_child() {
            history_body.remove(&child);
        }
        if days.is_empty() {
            history_body.append(&Win::status_page(
                "document-open-recent-symbolic",
                &tr("Nothing recorded yet"),
                &tr("Package changes, snapshots, settings resets and what Almanac did in the last 30 days appear here."),
                None,
            ));
            self.summarize(&tr("Nothing in the last 30 days"), Some("history"));
            return;
        }
        // The last 24 hours in one line, above the timeline: the same boxed
        // list and icon size as the notes above the Updates table.
        let recent = Win::note_list();
        recent.append(&Win::note_row(
            "document-open-recent-symbolic",
            &tr("In the last 24 hours"),
            &history::last_day(&days, crate::util::now()),
        ));
        // Under it, what Almanac keeps of its own and the way to clear it.
        // Only Almanac's log can be cleared: the package history and the
        // snapshots are the system's, read from where it keeps them.
        let own = Win::note_row(
            history_icon(history::ALMANAC),
            &tr("Almanac's log"),
            &tr("What Almanac did is kept for 30 days, then cleared automatically."),
        );
        let clear = gtk::Button::builder().label(tr("Clear…")).valign(gtk::Align::Center).build();
        crate::ui::shell::name(&clear, &tr("Clear Almanac's log"));
        clear.set_sensitive(
            days.iter()
                .flat_map(|d| &d.events)
                .any(|e| (e.kind == history::ALMANAC || e.kind == history::AUTOUPDATE) && !history::is_cleared_marker(e)),
        );
        let w = self.downgrade();
        clear.connect_clicked(move |_| {
            if let Some(w) = w.upgrade() {
                w.on_clear_log();
            }
        });
        own.add_suffix(&clear);
        recent.append(&own);
        history_body.append(&recent);

        for day in &days {
            let note = if day.snapshot {
                "A snapshot was taken this day — changes after it can be undone from the boot menu."
            } else {
                ""
            };
            let section = Win::section(&day.label(), note, None);
            for e in &day.events {
                let Some(row) = self.history_row(e) else {
                    // fromtimestamp raises in Python; PyGObject prints it and
                    // the idle callback stops here.
                    eprintln!("ValueError: timestamp {} out of range for datetime.fromtimestamp", e.ts);
                    return;
                };
                section.rows.append(&row);
            }
            history_body.append(&section.root);
        }
        let n: usize = days.iter().map(|d| d.events.len()).sum();
        self.summarize(&format!("{n} change(s) in the last 30 days"), Some("history"));
    }

    pub fn history_row(&self, e: &history::Event) -> Option<adw::PreferencesRow> {
        let when = clock_text(e.ts)?;
        let subtitle = &e.detail;
        // An entry's title can be an outcome that carries an error.
        let title = &crate::util::human_error(&e.title);
        let row = if let Some(action) = &e.action {
            // What the action did, recorded as it happened: open the row
            // to read it, or copy it whole for a bug report.
            let row: adw::ExpanderRow = Win::plain(title, subtitle);
            for entry in action_rows(action) {
                row.add_row(&entry);
            }
            row.add_row(&self.copy_details_row(e));
            Row::Expander(row)
        } else if !e.items.is_empty() {
            let row: adw::ExpanderRow = Win::plain(title, subtitle);
            for item in e.items.iter().take(80) {
                let entry: adw::ActionRow = Win::plain(item, "");
                entry.add_css_class("property");
                row.add_row(&entry);
            }
            row.add_row(&self.copy_details_row(e));
            Row::Expander(row)
        } else {
            Row::Action(Win::plain(title, subtitle))
        };
        let icon = gtk::Image::from_icon_name(if e.severity == "warn" {
            "dialog-warning-symbolic"
        } else {
            history_icon(&e.kind)
        });
        icon.set_accessible_role(gtk::AccessibleRole::Presentation);
        row.add_prefix(&icon);
        let clock = gtk::Label::builder().label(when).valign(gtk::Align::Center).build();
        clock.add_css_class("dim-label");
        clock.add_css_class("caption");
        clock.add_css_class("numeric");
        row.add_suffix(&clock);
        Some(row.row())
    }

    /// The last row of an open event: copies it as plain text.
    fn copy_details_row(&self, e: &history::Event) -> adw::ActionRow {
        let row: adw::ActionRow = Win::plain(&tr("Copy details"), "");
        row.set_activatable(true);
        let icon = gtk::Image::from_icon_name("edit-copy-symbolic");
        icon.set_accessible_role(gtk::AccessibleRole::Presentation);
        row.add_prefix(&icon);
        let text = history::details_text(e);
        let w = self.downgrade();
        row.connect_activated(move |r| {
            r.clipboard().set_text(&text);
            if let Some(w) = w.upgrade() {
                w.toast(&tr("Details copied"), 3);
            }
        });
        row
    }

    /// Ask, then clear Almanac's own log; History reads itself again after.
    pub fn on_clear_log(&self) {
        let dialog = adw::AlertDialog::new(
            Some(&tr("Clear Almanac's log?")),
            Some(&tr("Removes the record of what Almanac did. Package history from pacman and your \
                      snapshots are not affected.")),
        );
        dialog.add_response("cancel", &tr("Cancel"));
        dialog.add_response("clear", &tr("Clear log"));
        dialog.set_response_appearance("clear", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let w = self.downgrade();
        dialog.connect_response(None, move |_d, resp| {
            if resp != "clear" {
                return;
            }
            let w = w.clone();
            spawn_bg(
                || history::clear().map_err(|e| e.to_string()),
                move |result| {
                    let Some(w) = w.upgrade() else { return };
                    match result {
                        Ok(()) => {
                            w.toast(&tr("Almanac's log cleared"), 5);
                            w.history_reload();
                        }
                        Err(e) => w.say(&format!("{}: {e}", tr("Could not clear Almanac's log")), 8),
                    }
                },
            );
        });
        dialog.present(Some(&self.window));
    }

    pub fn maint_reload(&self) {
        self.busy(true);
        self.summarize(&tr("Checking the system…"), Some("maintenance"));

        let w = self.downgrade();
        spawn_bg(
            || std::panic::catch_unwind(maintenance::run_all).map_err(|e| panic_text(&*e)),
            move |result| {
                let Some(w) = w.upgrade() else { return };
                match result {
                    Ok(findings) => w.maint_show(findings),
                    Err(exc) => w.say(&format!("Health checks failed: {exc}"), 8),
                }
            },
        );
    }

    pub fn maint_show(&self, findings: Vec<maintenance::Finding>) {
        self.busy(false);
        self.shared.borrow().findings.set(findings.clone());
        let Some(maint_list) = self.history.borrow().maint_list.clone() else {
            // Python raises AttributeError here and PyGObject prints it.
            eprintln!("AttributeError: 'Almanac' object has no attribute 'maint_list'");
            return;
        };
        while let Some(row) = maint_list.first_child() {
            maint_list.remove(&row);
        }

        // Problems first, each in full; everything that passed folds into one
        // row. Fifteen "OK" rows of two or three lines each were most of the
        // page, and pushed the one that mattered down with them.
        let passed: Vec<&maintenance::Finding> =
            findings.iter().filter(|f| f.severity == maintenance::OK).collect();
        for f in &findings {
            if f.severity != maintenance::OK {
                maint_list.append(&self.maint_row(f));
            }
        }
        if !passed.is_empty() {
            let titles: Vec<&str> = passed.iter().take(4).map(|f| f.title.as_str()).collect();
            let fold: adw::ExpanderRow = Win::plain(
                &format!("{} checks passed", passed.len()),
                &(titles.join(", ") + if passed.len() > 4 { "…" } else { "" }),
            );
            fold.set_subtitle_lines(1);
            let tick = gtk::Image::builder()
                .icon_name("object-select-symbolic")
                .valign(gtk::Align::Center)
                .build();
            tick.add_css_class("store-verified");
            fold.add_prefix(&tick);
            // Open when nothing else is listed, so the page is never blank.
            fold.set_expanded(passed.len() == findings.len());
            for f in &passed {
                fold.add_row(&self.maint_row(f));
            }
            maint_list.append(&fold);
        }

        let issues = findings
            .iter()
            .filter(|f| f.severity == maintenance::CRIT || f.severity == maintenance::WARN)
            .count();
        let free = maintenance::reclaimable_mb(&findings);
        self.sync_primary();
        // "0 KB reclaimable" is not news; say it only when there is some.
        let room = if free >= 1.0 {
            format!(" · {} reclaimable", maintenance::fmt_size(free))
        } else {
            String::new()
        };
        self.summarize(
            &(if issues > 0 {
                format!("{issues} thing(s) worth attention{room}")
            } else {
                format!("Nothing needs attention{room}")
            }),
            Some("maintenance"),
        );

        // Keep the sidebar badge honest about what the page says.
        let count = self.shared.borrow().sidebar_row("maintenance").map(|r| r.count.clone());
        if let Some(count) = count {
            let crit = findings.iter().any(|f| f.severity == maintenance::CRIT);
            Win::badge(&count, issues, if crit { "crit" } else { "warn" });
        }
    }

    pub fn maint_row(&self, f: &maintenance::Finding) -> adw::PreferencesRow {
        let (label, css) = severity_css(&f.severity).unwrap_or((String::new(), "pill"));

        let row = if !f.items.is_empty() {
            let row: adw::ExpanderRow = Win::plain(&f.title, &f.detail);
            for item in f.items.iter().take(60) {
                let entry: adw::ActionRow = Win::plain(item, "");
                entry.add_css_class("property");
                entry.set_subtitle_selectable(true);
                row.add_row(&entry);
            }
            if f.items.len() > 60 {
                let more: adw::ActionRow =
                    Win::plain(&format!("… and {} more", f.items.len() - 60), "");
                row.add_row(&more);
            }
            Row::Expander(row)
        } else {
            Row::Action(Win::plain(&f.title, &f.detail))
        };
        // A problem gets room to explain itself; a pass gets one line.
        row.set_subtitle_lines(if f.severity == maintenance::OK { 1 } else { 3 });
        row.row().set_tooltip_text(Some(&crate::util::human_error(&f.detail)));

        let pill = gtk::Label::builder().label(label).valign(gtk::Align::Center).build();
        pill.add_css_class(css);
        row.add_prefix(&pill);

        if f.size_mb != 0.0 && f.id != "disk" {
            let size = gtk::Label::builder()
                .label(maintenance::fmt_size(f.size_mb))
                .valign(gtk::Align::Center)
                .build();
            size.add_css_class("dim-label");
            size.add_css_class("caption");
            row.add_suffix(&size);
        }

        if f.actionable() && !f.fix_label.is_empty() {
            let btn = gtk::Button::builder()
                .label(f.fix_label.as_str())
                .valign(gtk::Align::Center)
                .build();
            if f.severity == maintenance::CRIT {
                btn.add_css_class("suggested-action");
            }
            let w = self.downgrade();
            let f = f.clone();
            btn.connect_clicked(move |_b| {
                if let Some(w) = w.upgrade() {
                    w.maint_fix(&f);
                }
            });
            row.add_suffix(&btn);
        } else if !f.fix_note.is_empty()
            && !matches!(&f.fix, Some(maintenance::Fix::Func(_)))
            && !f.fix.as_ref().is_some_and(|x| x.commands().iter().any(|a| !a.is_empty()))
            && f.severity != maintenance::OK
        {
            // No button, but say what the human would have to do.
            let hint = gtk::Label::builder()
                .label(tr("manual"))
                .valign(gtk::Align::Center)
                .build();
            hint.add_css_class("dim-label");
            hint.add_css_class("caption");
            hint.set_tooltip_text(Some(&f.fix_note));
            row.add_suffix(&hint);
        }
        row.row()
    }

    pub fn maint_fix(&self, f: &maintenance::Finding) {
        let mut detail = f.fix_note.clone();
        if !f.items.is_empty() {
            let items: Vec<&str> = f.items.iter().take(40).map(String::as_str).collect();
            detail += "\n\n";
            detail += &items.join("\n");
            if f.items.len() > 40 {
                detail += &format!("\n… and {} more", f.items.len() - 40);
            }
        }
        let destructive = ["trash", "orphans", "dangling-desktop", "broken-symlinks", "rollbacks"]
            .contains(&f.id.as_str());
        let w = self.downgrade();
        let fixed = f.clone();
        tasks::confirm(
            &self.window,
            &f.title,
            &f.detail,
            &detail,
            &f.fix_label,
            destructive,
            move || {
                if let Some(w) = w.upgrade() {
                    let steps = fix_steps(fixed.fix_label.clone(), &fixed);
                    w.run_fixes(&fixed.title, &fixed.title, steps, vec![fixed.clone()]);
                }
            },
        );
    }

    /// Run fix `steps` in a task dialog, then check `fixed` again and record
    /// what the run did (`summary` titles a run of several fixes there).
    /// The free space is read first, off the main thread, so History can
    /// say what the run actually freed.
    fn run_fixes(&self, title: &str, summary: &str, steps: Vec<Step>, fixed: Vec<maintenance::Finding>) {
        let w = self.downgrade();
        let (title, summary) = (title.to_string(), summary.to_string());
        spawn_bg(free_bytes, move |free_before| {
            let Some(w) = w.upgrade() else { return };
            let run = FixRun::of(&summary, &steps, free_before);
            let on_done = w.findings_done(fixed, run);
            let _ = tasks::run(&w.window, &title, steps, Some(on_done));
        });
    }

    /// After fixes: ask each check again, say what actually changed, and
    /// record the run -- its commands, each check's answer, the space it
    /// freed and how long it took -- as one History entry.
    fn findings_done(&self, fixed: Vec<maintenance::Finding>, run: FixRun) -> OnDone {
        let w = self.downgrade();
        Box::new(move |ok: bool| {
            let Some(win) = w.upgrade() else { return };
            win.maint_after(ok);

            let w = win.downgrade();
            spawn_bg(
                move || {
                    let outs: Vec<Outcome> = fixed
                        .iter()
                        .map(|f| verify::judge_finding(f, verify::recheck(&f.id).as_ref()))
                        .collect();
                    let recorded = run.record(ok, &fixed, &outs, free_bytes());
                    let failed: Vec<Outcome> =
                        outs.iter().filter(|o| o.changed == Some(false)).cloned().collect();
                    let message = (failed.is_empty() && !outs.is_empty()).then(|| {
                        outs.iter().map(|o| o.message.as_str()).collect::<Vec<_>>().join(" · ")
                    });
                    (failed, message, recorded)
                },
                // spawn_bg's done already runs from the main loop, the one hop
                // Python's GLib.idle_add makes; deliver straight from it.
                move |(failed, message, recorded)| {
                    let Some(w) = w.upgrade() else { return };
                    if let Err(e) = recorded {
                        eprintln!("almanac: history: {e}");
                    }
                    // Already in History, with the rest of the run.
                    for o in failed {
                        w.announce_outcome(&o);
                    }
                    if let Some(message) = message {
                        w.say(&message, 8);
                    }
                },
            );
        })
    }

    pub fn maint_after(&self, ok: bool) {
        self.maint_reload();
        if ok {
            self.reload();
        }
    }

    /// The three cleanups that only ever free cached or logged data.
    pub fn reclaim_space(&self) {
        let wanted = ["pacman-cache", "yay-cache", "journal"];
        let (mut steps, mut detail, mut total) = (Vec::new(), Vec::new(), 0.0);
        let findings = self.shared.borrow().findings.get();
        for f in &findings {
            if wanted.contains(&f.id.as_str()) && f.actionable() {
                steps.extend(fix_steps(format!("{}: {}", f.title, f.fix_label), f));
                detail.push(format!(
                    "{:<22} {:>9}   {}",
                    f.title,
                    maintenance::fmt_size(f.size_mb),
                    f.fix_note
                ));
                total += f.size_mb;
            }
        }
        if steps.is_empty() {
            self.say(&tr("Nothing to reclaim"), 5);
            return;
        }
        let w = self.downgrade();
        tasks::confirm(
            &self.window,
            &format!("Reclaim about {}?", maintenance::fmt_size(total)),
            &tr("These free cached downloads, build artefacts and old logs. Nothing you created is touched, and nothing installed is removed."),
            &detail.join("\n\n"),
            &tr("Reclaim"),
            false,
            move || {
                let Some(w) = w.upgrade() else { return };
                let fixed: Vec<maintenance::Finding> = w
                    .shared
                    .borrow()
                    .findings
                    .get()
                    .iter()
                    .filter(|f| wanted.contains(&f.id.as_str()) && f.actionable())
                    .cloned()
                    .collect();
                w.run_fixes("Reclaiming space", "Space reclaimed", steps.clone(), fixed);
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PILLS: &[&str] = &["pill", "pill-crit", "pill-warn", "pill-ok", "pill-accent", "pill-quiet"];
    const SCALE: &[i64] = &[2, 4, 6, 8, 12, 16, 20, 24];

    fn source() -> String {
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/", file!())).unwrap()
    }

    /// The code above the tests: what the app runs.
    fn code() -> String {
        let src = source();
        src.split("#[cfg(test)]").next().unwrap_or("").to_string()
    }

    fn css() -> &'static str {
        include_str!("../../data/style.css")
    }

    #[test]
    fn history_icon_falls_back() {
        assert_eq!(history_icon(history::SNAPSHOT), "camera-photo-symbolic");
        assert_eq!(history_icon("nope"), "object-select-symbolic");
    }

    #[test]
    fn every_source_category_explains_itself() {
        for key in [
            inventory::ORPHAN,
            inventory::FLATPAK,
            inventory::APPIMAGE,
            inventory::LOCAL,
            inventory::AUR,
            "updates",
        ] {
            let (icon, title, body) =
                empty_states(key).unwrap_or_else(|| panic!("{key} has no empty state"));
            assert!(
                !icon.is_empty() && !title.is_empty() && !body.is_empty(),
                "{key}'s empty state is incomplete"
            );
            assert!(body.chars().count() > 20, "{key}'s empty state does not explain anything");
        }
    }

    #[test]
    fn an_empty_updates_list_is_good_news_not_an_error() {
        let (icon, title, _body) = empty_states("updates").unwrap();
        // A check mark: a finished state should not look like a fault.
        assert_eq!(icon, "object-select-symbolic");
        assert!(!icon.contains("error"));
        assert!(title.to_lowercase().contains("up to date"));
    }

    #[test]
    fn orphan_empty_state_does_not_imply_something_is_wrong() {
        let (_icon, title, body) = empty_states(inventory::ORPHAN).unwrap();
        let text = (title + &body).to_lowercase();
        assert!(!text.contains("error"));
        assert!(!text.contains("fail"));
    }

    #[test]
    fn severity_pills_and_fallback() {
        assert_eq!(severity_css(maintenance::CRIT).unwrap().1, "pill-crit");
        assert_eq!(severity_css(maintenance::OK).unwrap().1, "pill-ok");
        assert!(severity_css("bogus").is_none());
    }

    #[test]
    fn a_fix_run_is_recorded_with_its_commands_checks_and_space() {
        let _g = crate::util::TEST_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        let old = std::env::var_os("XDG_STATE_HOME");
        std::env::set_var("XDG_STATE_HOME", d.path());
        let steps = vec![
            Step::argv("Trim", vec!["paccache".into(), "-rk2".into()]),
            Step::argv("Nothing", Vec::new()),
        ];
        let run = FixRun::of("Space reclaimed", &steps, Some(10 << 20));
        let fixed = vec![maintenance::Finding { title: "Package cache".into(), fix_label: "Trim to 2 versions".into(),
                                                ..Default::default() }];
        let worked = vec![Outcome::new(Some(true), "Package cache: resolved")];
        run.record(true, &fixed, &worked, Some(13 << 20)).unwrap();
        let stuck = vec![Outcome::new(Some(false), "Package cache: the fix ran but nothing changed")];
        run.record(false, &fixed, &stuck, None).unwrap();
        let evs = history::recorded(0.0).unwrap();
        match old {
            Some(v) => std::env::set_var("XDG_STATE_HOME", v),
            None => std::env::remove_var("XDG_STATE_HOME"),
        }
        assert_eq!(evs.len(), 2);
        let (good, bad) = (evs[0].action.clone().unwrap(), evs[1].action.clone().unwrap());
        assert_eq!(evs[0].title, "Package cache: resolved");
        assert_eq!(evs[0].detail, "Trim to 2 versions");
        assert_eq!(evs[0].severity, "");
        // The command as it ran: elevated, by path.
        assert_eq!(good.commands.len(), 1);
        assert_eq!(good.commands[0].argv, "/usr/bin/pkexec --keep-cwd /usr/bin/paccache -rk2");
        assert_eq!(good.commands[0].exit, Some(0));
        assert_eq!(good.freed_bytes, Some(3 << 20));
        assert_eq!(good.checks[0].ok, Some(true));
        assert!(good.duration_secs.is_some());
        // A task that failed does not say which command did.
        assert_eq!(bad.commands[0].exit, None);
        assert_eq!(bad.freed_bytes, None);
        assert_eq!(bad.checks[0].ok, Some(false));
        assert_eq!(evs[1].severity, "warn");
    }

    #[test]
    fn clearing_says_what_it_does_not_touch_and_asks_first() {
        let code = code();
        assert!(code.contains("Package history from pacman and your \\\n                      snapshots are not affected."));
        assert!(code.contains("set_response_appearance(\"clear\", adw::ResponseAppearance::Destructive)"));
        assert!(code.contains("set_default_response(Some(\"cancel\"))"));
    }

    #[test]
    fn clock_is_hours_and_minutes() {
        let t = clock_text(1_700_000_000.7).unwrap();
        assert_eq!(t.len(), 5);
        assert_eq!(&t[2..3], ":");
        // fromtimestamp raises on these.
        assert_eq!(clock_text(f64::NAN), None);
        assert_eq!(clock_text(1e20), None);
    }

    // VisualSystem: spacing and badge styling, asserted against this file.
    #[test]
    fn spacing_comes_from_the_scale() {
        let re = regex::Regex::new(
            r"\.(margin_(?:start|end|top|bottom)|spacing|child_spacing|line_spacing)\((\d+)\)",
        )
        .unwrap();
        let code = code();
        let offenders: Vec<String> = re
            .captures_iter(&code)
            .filter(|c| !SCALE.contains(&c[2].parse::<i64>().unwrap_or(-1)))
            .map(|c| format!("{}={}", &c[1], &c[2]))
            .collect();
        assert!(offenders.is_empty(), "spacing off the scale {SCALE:?}: {offenders:?}");
    }

    #[test]
    fn every_pill_class_is_defined_in_the_stylesheet() {
        let re = regex::Regex::new(r#""(pill(?:-[a-z]+)?)""#).unwrap();
        let code = code();
        let used: Vec<String> = re.captures_iter(&code).map(|c| c[1].to_string()).collect();
        assert!(!used.is_empty(), "no pill classes found at all");
        for name in &used {
            assert!(css().contains(&format!(".{name}")), "{name} is applied but never styled");
            assert!(PILLS.contains(&name.as_str()), "unknown pill variant: {name}");
        }
    }

    #[test]
    fn every_css_class_used_here_is_defined_or_stock() {
        // Classes libadwaita itself styles.
        let stock = ["dim-label", "caption", "numeric", "property", "suggested-action"];
        {
            let class = "store-verified";
            assert!(css().contains(&format!(".{class}")), "{class} is never styled");
        }
        for class in stock {
            assert!(code().contains(&format!("\"{class}\"")));
        }
    }

    // Accessibility: an icon-only button must be named.
    #[test]
    fn every_icon_only_button_is_named() {
        let code = code();
        let lines: Vec<&str> = code.lines().collect();
        let mut unnamed = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            if line.contains("Button::builder()") || line.contains("Button::from_icon_name(") {
                let window = lines[i..(i + 12).min(lines.len())].join("\n");
                if (window.contains(".icon_name(") || line.contains("from_icon_name("))
                    && !window.contains("name(&") && !window.contains("AccessibleProperty::Label") {
                        unnamed.push(i + 1);
                    }
            }
        }
        assert!(unnamed.is_empty(), "unnamed icon buttons at lines {unnamed:?}");
    }

    // IconsExist: a theme-specific name renders as a blank square elsewhere.
    #[test]
    fn every_icon_name_is_in_adwaita_or_shipped_here() {
        let adwaita = std::path::Path::new("/usr/share/icons/Adwaita");
        if !adwaita.is_dir() {
            return; // Adwaita icon theme not installed
        }
        let re = regex::Regex::new(r#""([a-z0-9-]+-symbolic)""#).unwrap();
        let code = code();
        let mut have = std::collections::BTreeSet::new();
        let pattern_a = format!("{}/**/*.svg", adwaita.display());
        let pattern_b = format!("{}/**/*.png", adwaita.display());
        let here = concat!(env!("CARGO_MANIFEST_DIR"), "/icons/*.svg").to_string();
        for pattern in [pattern_a, pattern_b, here] {
            for p in glob::glob(&pattern).into_iter().flatten().flatten() {
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    have.insert(stem.to_string());
                }
            }
        }
        let missing: Vec<String> = re
            .captures_iter(&code)
            .map(|c| c[1].to_string())
            .filter(|n| !have.contains(n) && !n.starts_with("adw-"))
            .collect();
        assert!(missing.is_empty(), "missing icons: {missing:?}");
    }

    // The Python's widget structure, asserted on the source: same classes,
    // same limits, same order.
    #[test]
    fn rows_keep_the_python_limits_and_classes() {
        let code = code();
        assert!(code.contains("e.items.iter().take(80)"));
        assert!(code.contains("f.items.iter().take(60)"));
        assert!(code.contains("set_subtitle_selectable(true)"));
        assert!(code.contains("set_accessible_role(gtk::AccessibleRole::Presentation)"));
        assert!(code.contains("tr(\"manual\")"));
    }
}
