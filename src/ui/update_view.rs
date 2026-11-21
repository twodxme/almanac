//! The in-window update: a step timeline, with the apps of the step under
//! way inside it, the password and any question asked in place, and the
//! failure with its fix where it happened.
//!
//! The view only shows; Update all (`update_all.rs`) owns the run, feeds
//! `render` and answers through the replies it is given. The pure helpers at
//! the top choose every word and icon, so they are tested without GTK.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use adw::prelude::*;

use crate::i18n::tr;
use crate::inwindow::prompt::{Answer, Prompt};
use crate::inwindow::tracker::{AppRow, AppState, Changed, Fix, StepId, StepState, Timeline};
use crate::ui::shell::{AppItem, WeakWin, Win};

/// Lines the Details log keeps; the run's own log file has everything.
const LOG_LINES: i32 = 2000;
/// The content width of the approved mockup.
const WIDTH: i32 = 560;

// --- the words and icons, without GTK ---

fn failure(t: &Timeline) -> Option<(StepId, &Changed)> {
    t.steps.iter().find_map(|(id, s)| match s {
        StepState::Failed { changed, .. } => Some((*id, changed)),
        _ => None,
    })
}

fn all_done(t: &Timeline) -> bool {
    !t.steps.is_empty() && t.steps.iter().all(|(_, s)| matches!(s, StepState::Done { .. }))
}

/// The heading: what the update is doing, or how it ended.
pub fn title(t: &Timeline) -> String {
    if let Some((_, changed)) = failure(t) {
        let none_done = !t.steps.iter().any(|(_, s)| matches!(s, StepState::Done { .. }));
        return if none_done && *changed == Changed::Nothing {
            tr("The update didn't start")
        } else {
            tr("The update stopped")
        };
    }
    if all_done(t) {
        return tr("Update finished");
    }
    tr("Updating your system")
}

/// The line under the heading: `Step n of total`, or `Updating…` when the
/// steps cannot be told apart; after a failure, the step it stopped at
/// (what it changed is said once, on the failure card).
pub fn subtitle(t: &Timeline) -> String {
    if let Some((id, _)) = failure(t) {
        return tr("Stopped at: {step}").replace("{step}", &tr(id.label()));
    }
    if t.generic {
        return tr("Updating…");
    }
    if all_done(t) {
        return tr("All steps done");
    }
    match t.position() {
        (0, _) => tr("Starting…"),
        (n, total) => tr("Step {n} of {total}")
            .replace("{n}", &n.to_string())
            .replace("{total}", &total.to_string()),
    }
}

/// Whether anything changed, in words; empty when it is not known.
pub fn changed_line(c: &Changed) -> String {
    match c {
        Changed::Nothing => tr("Nothing was changed."),
        Changed::Some => tr("Some changes were made."),
        Changed::Unknown => String::new(),
    }
}

pub fn fix_label(f: &Fix) -> String {
    match f {
        Fix::StopStuckUpdate(_) => tr("Stop it and update"),
        Fix::Resolve => tr("Resolve"),
        Fix::RetryUpdate => tr("Try again"),
    }
}

/// The bar: steps done out of all of them.
pub fn fraction(t: &Timeline) -> f64 {
    if t.steps.is_empty() {
        return 0.0;
    }
    let done = t
        .steps
        .iter()
        .filter(|(_, s)| matches!(s, StepState::Done { .. }))
        .count();
    done as f64 / t.steps.len() as f64
}

/// What sits at the start of a step's row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    Spinner,
    /// (icon name, style class)
    Icon(&'static str, &'static str),
}

pub fn step_look(s: &StepState) -> Look {
    match s {
        StepState::Waiting => Look::Icon("radio-symbolic", "dim-label"),
        StepState::Running { .. } => Look::Spinner,
        StepState::Done { .. } => Look::Icon("object-select-symbolic", "success"),
        StepState::Failed { .. } => Look::Icon("dialog-error-symbolic", "error"),
    }
}

/// The text at the end of an app's row and its style class.
pub fn app_suffix(a: &AppRow) -> (String, &'static str) {
    match &a.state {
        AppState::Done { version } => (format!("{version} ✓"), "dim-label"),
        AppState::Installing => (tr("installing…"), "accent"),
        AppState::Downloading => (tr("downloading…"), "accent"),
        AppState::Waiting => (a.to.clone(), "dim-label"),
        AppState::Failed => (tr("not updated"), "error"),
    }
}

/// `2 of 5` for a step that carries app rows.
pub fn apps_count(t: &Timeline, id: StepId) -> Option<String> {
    let mine: Vec<&AppRow> = t.apps.iter().filter(|a| a.step == id).collect();
    if mine.is_empty() {
        return None;
    }
    let done = mine
        .iter()
        .filter(|a| matches!(a.state, AppState::Done { .. }))
        .count();
    Some(
        tr("{done} of {total}")
            .replace("{done}", &done.to_string())
            .replace("{total}", &mine.len().to_string()),
    )
}

/// The answers shown as buttons: never one that agrees to a reboot.
pub fn buttons(answers: &[Answer]) -> Vec<&Answer> {
    answers.iter().filter(|a| !a.is_reboot_yes).collect()
}

/// A button on a question card.
#[derive(Debug, PartialEq, Eq)]
pub enum Choice<'a> {
    Answer(&'a Answer),
    /// Every answer would agree to a reboot: the only way on is to stop.
    Stop,
}

/// The buttons a question gets. Never none: a card without a button would
/// leave the update waiting with no way to answer it.
pub fn choices(answers: &[Answer]) -> Vec<Choice<'_>> {
    let shown = buttons(answers);
    if shown.is_empty() {
        return vec![Choice::Stop];
    }
    shown.into_iter().map(Choice::Answer).collect()
}

/// Split off an escape sequence cut short at the end of `s`, to be put in
/// front of the next chunk: `clean_output` cannot remove half of one.
pub fn split_trailing_escape(s: &str) -> (&str, &str) {
    /// Longer than any sequence the update prints; past this it is output.
    const MAX: usize = 256;
    let Some(pos) = s.rfind('\x1b') else {
        return (s, "");
    };
    let rest = &s[pos..];
    if rest.len() > MAX {
        return (s, "");
    }
    let mut chars = rest.chars().skip(1);
    let complete = match chars.next() {
        None => false,
        // CSI: ends at a final byte.
        Some('[') => chars.any(|c| ('@'..='~').contains(&c)),
        // OSC: ends at BEL (ST is ESC \, whose ESC would be the last one).
        Some(']') => chars.any(|c| c == '\x07'),
        Some(_) => true,
    };
    if complete {
        (s, "")
    } else {
        (&s[..pos], rest)
    }
}

// --- the view ---

struct StepUi {
    id: StepId,
    row: adw::ActionRow,
    icon: gtk::Image,
    spinner: gtk::Spinner,
    count: gtk::Label,
    /// The row under the step: its apps, its failure, a question.
    slot_row: gtk::ListBoxRow,
    slot: gtk::Box,
    apps: gtk::ListBox,
    failure: gtk::Box,
    /// What `failure` was built from, so it is rebuilt only when that changes.
    failure_key: RefCell<Option<String>>,
}

struct AppUi {
    name: String,
    row: adw::ActionRow,
    suffix: gtk::Label,
}

pub struct Inner {
    win: WeakWin,
    dialog: adw::Dialog,
    toolbar: adw::ToolbarView,
    title: gtk::Label,
    subtitle: gtk::Label,
    bar: gtk::ProgressBar,
    steps_list: gtk::ListBox,
    steps: RefCell<Vec<StepUi>>,
    apps: RefCell<Vec<AppUi>>,
    notes_group: adw::PreferencesGroup,
    restart: adw::ActionRow,
    safe: gtk::Box,
    expander: gtk::Expander,
    log: gtk::TextBuffer,
    log_view: gtk::TextView,
    log_end: gtk::TextMark,
    /// The password or question card on screen.
    prompt: RefCell<Option<gtk::Widget>>,
    fix: RefCell<Option<(Fix, OnFix)>>,
    /// The end of the last log chunk when it stopped inside an escape.
    log_carry: RefCell<String>,
    last: RefCell<Option<Timeline>>,
}

/// What the failure card's fix button runs.
type OnFix = Rc<dyn Fn()>;

/// The update dialog. Cheap to clone.
#[derive(Clone)]
pub struct UpdateView(Rc<Inner>);

fn label(text: &str, classes: &[&str]) -> gtk::Label {
    let l = gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .build();
    for c in classes {
        l.add_css_class(c);
    }
    l
}

/// An inset card under a step: the failure, the password, a question.
fn inset_card() -> gtk::Box {
    let b = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .build();
    b.add_css_class("card");
    b.add_css_class("update-inset");
    b
}

fn button_row() -> gtk::Box {
    gtk::Box::builder()
        .spacing(8)
        .halign(gtk::Align::End)
        .margin_top(6)
        .build()
}

fn set_style(w: &impl IsA<gtk::Widget>, on: &[&str], all: &[&str]) {
    for c in all {
        if on.contains(c) {
            w.add_css_class(c);
        } else {
            w.remove_css_class(c);
        }
    }
}

const STYLES: [&str; 4] = ["dim-label", "success", "error", "accent"];
const ROW_STYLES: [&str; 2] = ["update-now", "update-waiting"];

impl UpdateView {
    pub fn new(win: &Win) -> UpdateView {
        let dialog = adw::Dialog::builder()
            .title(tr("Update"))
            .content_width(WIDTH)
            .build();
        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        header.set_show_title(false);
        toolbar.add_top_bar(&header);

        let page = adw::PreferencesPage::new();

        let hero = adw::PreferencesGroup::new();
        let head = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .build();
        let title = label("", &["title-2"]);
        let subtitle = label("", &["dim-label"]);
        let bar = gtk::ProgressBar::builder().margin_top(12).build();
        head.append(&title);
        head.append(&subtitle);
        head.append(&bar);
        hero.add(&head);
        page.add(&hero);

        let steps_group = adw::PreferencesGroup::new();
        let steps_list = Win::note_list();
        steps_group.add(&steps_list);
        page.add(&steps_group);

        let notes_group = adw::PreferencesGroup::new();
        let notes = Win::note_list();
        let restart = Win::note_row("system-reboot-symbolic", &tr("Restart to finish"), "");
        notes.append(&restart);
        notes_group.add(&notes);
        notes_group.set_visible(false);
        page.add(&notes_group);

        let foot_group = adw::PreferencesGroup::new();
        let foot = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .build();
        let safe = gtk::Box::builder().spacing(8).build();
        let dot = gtk::Image::builder()
            .icon_name("media-record-symbolic")
            .pixel_size(8)
            .valign(gtk::Align::Center)
            .build();
        dot.add_css_class("success");
        dot.set_accessible_role(gtk::AccessibleRole::Presentation);
        let safe_text = label(&tr("Safe to keep working"), &["success"]);
        safe.append(&dot);
        safe.append(&safe_text);

        let log = gtk::TextBuffer::new(None);
        let log_end = log.create_mark(None, &log.end_iter(), false);
        let log_view = gtk::TextView::builder()
            .buffer(&log)
            .editable(false)
            .cursor_visible(false)
            .monospace(true)
            .wrap_mode(gtk::WrapMode::WordChar)
            .top_margin(10)
            .bottom_margin(10)
            .left_margin(12)
            .right_margin(12)
            .build();
        log_view.add_css_class("monospace");
        log_view.add_css_class("update-log");
        let scroll = gtk::ScrolledWindow::builder()
            .min_content_height(180)
            .max_content_height(320)
            .propagate_natural_height(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&log_view)
            .margin_top(8)
            .build();
        scroll.add_css_class("card");
        scroll.set_overflow(gtk::Overflow::Hidden);
        let expander = gtk::Expander::builder()
            .label(tr("Details"))
            .child(&scroll)
            .build();
        expander.add_css_class("dim-label");
        foot.append(&safe);
        foot.append(&expander);
        foot_group.add(&foot);
        page.add(&foot_group);

        toolbar.set_content(Some(&page));
        dialog.set_child(Some(&toolbar));

        UpdateView(Rc::new(Inner {
            win: win.downgrade(),
            dialog,
            toolbar,
            title,
            subtitle,
            bar,
            steps_list,
            steps: RefCell::new(Vec::new()),
            apps: RefCell::new(Vec::new()),
            notes_group,
            restart,
            safe,
            expander,
            log,
            log_view,
            log_end,
            prompt: RefCell::new(None),
            fix: RefCell::new(None),
            log_carry: RefCell::new(String::new()),
            last: RefCell::new(None),
        }))
    }

    fn downgrade(&self) -> Weak<Inner> {
        Rc::downgrade(&self.0)
    }

    fn upgrade(w: &Weak<Inner>) -> Option<UpdateView> {
        w.upgrade().map(UpdateView)
    }

    /// The dialog itself, for whoever decides what closing it means.
    pub fn dialog(&self) -> &adw::Dialog {
        &self.0.dialog
    }

    pub fn present(&self) {
        if let Some(win) = self.0.win.upgrade() {
            self.0.dialog.present(Some(&win.window));
        }
    }

    /// One row per step, made once: the steps of a run do not change.
    fn ensure_steps(&self, t: &Timeline) {
        let same = {
            let steps = self.0.steps.borrow();
            steps.len() == t.steps.len() && steps.iter().zip(&t.steps).all(|(u, (id, _))| u.id == *id)
        };
        if same {
            return;
        }
        while let Some(child) = self.0.steps_list.first_child() {
            self.0.steps_list.remove(&child);
        }
        self.0.apps.borrow_mut().clear();
        let mut steps = Vec::new();
        for (id, _) in &t.steps {
            let row = Win::note_row("radio-symbolic", &tr(id.label()), "");
            // Nothing in the list is clicked; focus belongs to the buttons.
            row.set_focusable(false);
            // note_row's prefix is the image; the spinner sits beside it and
            // only one of the two is shown.
            let icon = find_image(row.upcast_ref()).unwrap_or_default();
            let spinner = gtk::Spinner::builder()
                .width_request(24)
                .height_request(24)
                .valign(gtk::Align::Center)
                .visible(false)
                .build();
            spinner.add_css_class("accent");
            row.add_prefix(&spinner);
            let count = gtk::Label::builder()
                .valign(gtk::Align::Center)
                .visible(false)
                .build();
            count.add_css_class("dim-label");
            count.add_css_class("caption");
            row.add_suffix(&count);

            let slot = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(12)
                .margin_start(24)
                .margin_end(12)
                .margin_bottom(12)
                .build();
            let apps = Win::note_list();
            apps.add_css_class("update-apps");
            apps.set_visible(false);
            let failure = inset_card();
            failure.set_visible(false);
            slot.append(&apps);
            slot.append(&failure);
            let slot_row = gtk::ListBoxRow::builder()
                .activatable(false)
                .selectable(false)
                .child(&slot)
                .focusable(false)
                .visible(false)
                .build();
            slot_row.add_css_class("update-slot");
            self.0.steps_list.append(&row);
            self.0.steps_list.append(&slot_row);
            steps.push(StepUi {
                id: *id,
                row,
                icon,
                spinner,
                count,
                slot_row,
                slot,
                apps,
                failure,
                failure_key: RefCell::new(None),
            });
        }
        *self.0.steps.borrow_mut() = steps;
    }

    pub fn render(&self, t: &Timeline) {
        self.ensure_steps(t);
        self.0.title.set_text(&title(t));
        self.0.subtitle.set_text(&subtitle(t));
        self.0.bar.set_fraction(fraction(t));

        for app in &t.apps {
            self.render_app(app);
        }

        {
            let steps = self.0.steps.borrow();
            for (ui, (id, state)) in steps.iter().zip(&t.steps) {
                match step_look(state) {
                    Look::Spinner => {
                        ui.icon.set_visible(false);
                        ui.spinner.set_visible(true);
                        ui.spinner.start();
                    }
                    Look::Icon(name, class) => {
                        ui.spinner.stop();
                        ui.spinner.set_visible(false);
                        ui.icon.set_icon_name(Some(name));
                        set_style(&ui.icon, &[class], &STYLES);
                        ui.icon.set_visible(true);
                    }
                }
                let row_style: &[&str] = match state {
                    StepState::Running { .. } | StepState::Failed { .. } => &["update-now"],
                    StepState::Waiting => &["update-waiting"],
                    StepState::Done { .. } => &[],
                };
                set_style(&ui.row, row_style, &ROW_STYLES);
                let detail = match state {
                    StepState::Running { detail } | StepState::Done { detail } => tr(detail),
                    _ => String::new(),
                };
                ui.row.set_subtitle(&detail);

                let count = apps_count(t, *id).filter(|_| *state != StepState::Waiting);
                ui.count.set_visible(count.is_some());
                ui.count.set_text(count.as_deref().unwrap_or(""));

                let has_apps = t.apps.iter().any(|a| a.step == *id);
                let started = t
                    .apps
                    .iter()
                    .any(|a| a.step == *id && a.state != AppState::Waiting);
                ui.apps.set_visible(match state {
                    StepState::Running { .. } => has_apps,
                    StepState::Failed { .. } => started,
                    _ => false,
                });
                self.render_failure(ui, state);
            }
        }

        match &t.restart_note {
            Some(note) => {
                self.0.restart.set_subtitle(note);
                self.0.notes_group.set_visible(true);
            }
            None => self.0.notes_group.set_visible(false),
        }
        self.0
            .safe
            .set_visible(failure(t).is_none() && !all_done(t));

        *self.0.last.borrow_mut() = Some(t.clone());
        self.place_prompt();
    }

    fn render_failure(&self, ui: &StepUi, state: &StepState) {
        let StepState::Failed { reason, changed, .. } = state else {
            ui.failure.set_visible(false);
            *ui.failure_key.borrow_mut() = None;
            return;
        };
        let fix = self.0.fix.borrow().clone();
        let key = format!(
            "{reason}\u{1}{changed:?}\u{1}{:?}",
            fix.as_ref().map(|(f, _)| f)
        );
        if ui.failure_key.borrow().as_deref() == Some(key.as_str()) {
            return;
        }
        while let Some(c) = ui.failure.first_child() {
            ui.failure.remove(&c);
        }
        ui.failure.append(&label(reason, &[]));
        let line = changed_line(changed);
        if !line.is_empty() {
            ui.failure.append(&label(&line, &["dim-label"]));
        }
        let buttons = button_row();
        let details = gtk::Button::with_label(&tr("Show details"));
        let exp = self.0.expander.downgrade();
        details.connect_clicked(move |_| {
            if let Some(e) = exp.upgrade() {
                e.set_expanded(true);
            }
        });
        buttons.append(&details);
        if let Some((f, on_fix)) = fix {
            let b = gtk::Button::with_label(&fix_label(&f));
            b.add_css_class("suggested-action");
            b.connect_clicked(move |_| on_fix());
            buttons.append(&b);
        }
        ui.failure.append(&buttons);
        ui.failure.set_visible(true);
        *ui.failure_key.borrow_mut() = Some(key);
    }

    fn render_app(&self, a: &AppRow) {
        let mut apps = self.0.apps.borrow_mut();
        let ui = match apps.iter().position(|u| u.name == a.name) {
            Some(i) => &apps[i],
            None => {
                let steps = self.0.steps.borrow();
                let Some(step) = steps.iter().find(|s| s.id == a.step) else {
                    return;
                };
                let row: adw::ActionRow = Win::plain(&a.name, "");
                row.set_focusable(false);
                row.add_css_class("update-app");
                let img = gtk::Image::builder()
                    .pixel_size(24)
                    .valign(gtk::Align::Center)
                    .build();
                img.set_accessible_role(gtk::AccessibleRole::Presentation);
                self.icon_for(&img, &a.name);
                row.add_prefix(&img);
                let suffix = gtk::Label::builder().valign(gtk::Align::Center).build();
                suffix.add_css_class("caption");
                row.add_suffix(&suffix);
                step.apps.append(&row);
                apps.push(AppUi {
                    name: a.name.clone(),
                    row,
                    suffix,
                });
                apps.last().expect("just pushed")
            }
        };
        let (text, class) = app_suffix(a);
        ui.suffix.set_text(&text);
        set_style(&ui.suffix, &[class], &STYLES);
        let row_style: &[&str] = match a.state {
            AppState::Installing | AppState::Downloading => &["update-now"],
            AppState::Waiting => &["update-waiting"],
            _ => &[],
        };
        set_style(&ui.row, row_style, &ROW_STYLES);
    }

    /// The icon the Updates list shows for this package, else a dim glyph.
    fn icon_for(&self, img: &gtk::Image, name: &str) {
        let found = self.0.win.upgrade().and_then(|win| {
            let store = win.shared.borrow().store.clone();
            let app = (0..store.n_items()).find_map(|i| {
                store
                    .item(i)
                    .as_ref()
                    .and_then(AppItem::of)
                    .filter(|it| it.app.name == name)
                    .map(|it| it.app.clone())
            })?;
            Some(win.app_icon(img, &app, 48))
        });
        match found {
            Some(true) => img.remove_css_class("dim-label"),
            Some(false) => img.add_css_class("dim-label"),
            None => {
                img.set_icon_name(Some("package-x-generic-symbolic"));
                img.add_css_class("dim-label");
            }
        }
    }

    /// The step a password or question belongs under: the one under way,
    /// else the first not yet done.
    fn prompt_step(&self) -> Option<usize> {
        let last = self.0.last.borrow();
        let t = last.as_ref()?;
        let cur = t.current();
        t.steps
            .iter()
            .position(|(id, _)| Some(*id) == cur)
            .or_else(|| {
                t.steps
                    .iter()
                    .position(|(_, s)| !matches!(s, StepState::Done { .. }))
            })
            .or(Some(0))
    }

    fn place_prompt(&self) {
        let steps = self.0.steps.borrow();
        let at = self.prompt_step();
        if let Some(w) = self.0.prompt.borrow().as_ref() {
            if let Some(slot) = at.and_then(|i| steps.get(i)).map(|s| &s.slot) {
                if w.parent().as_ref() != Some(slot.upcast_ref()) {
                    detach(w);
                    slot.append(w);
                }
            }
        }
        for s in steps.iter() {
            let prompt_here = self
                .0
                .prompt
                .borrow()
                .as_ref()
                .and_then(|w| w.parent())
                .is_some_and(|p| &p == s.slot.upcast_ref::<gtk::Widget>());
            s.slot_row
                .set_visible(s.apps.get_visible() || s.failure.get_visible() || prompt_here);
        }
    }

    fn set_prompt(&self, w: gtk::Widget) {
        if let Some(old) = self.0.prompt.replace(Some(w)) {
            detach(&old);
        }
        self.place_prompt();
    }

    /// Take the password or question card off the screen (it was answered,
    /// or the update moved on and the answer would no longer be taken).
    pub fn close_prompt(&self) {
        if let Some(old) = self.0.prompt.replace(None) {
            detach(&old);
        }
        self.place_prompt();
    }

    /// Ask for the password in place. `reply` gets the password, or None for
    /// Cancel; the card closes first, whatever the reply then does.
    pub fn ask_password(&self, prompt: &str, attempt: u32, reply: impl Fn(Option<String>) + 'static) {
        let _ = prompt; // sudo's own wording ("[sudo] password for …") is not shown
        let card = inset_card();
        card.append(&label(&tr("Your password to update"), &["heading"]));
        card.append(&label(
            &tr("Updating changes system files. Almanac asks once, then forgets it when the update ends."),
            &["dim-label"],
        ));
        if attempt > 1 {
            card.append(&label(&tr("Wrong password — try again"), &["error"]));
        }
        let list = Win::note_list();
        list.set_margin_top(6);
        let entry = adw::PasswordEntryRow::builder().title(tr("Password")).build();
        list.append(&entry);
        card.append(&list);

        let row = button_row();
        let cancel = gtk::Button::with_label(&tr("Cancel"));
        let update = gtk::Button::with_label(&tr("Update"));
        update.add_css_class("suggested-action");
        update.set_sensitive(false);
        row.append(&cancel);
        row.append(&update);
        card.append(&row);

        let reply: Rc<dyn Fn(Option<String>)> = Rc::new(reply);
        let view = self.downgrade();
        let send: Rc<dyn Fn(Option<String>)> = {
            let entry = entry.downgrade();
            Rc::new(move |secret| {
                if let Some(e) = entry.upgrade() {
                    e.set_text("");
                }
                if let Some(v) = UpdateView::upgrade(&view) {
                    v.close_prompt();
                }
                reply(secret);
            })
        };
        {
            let update = update.downgrade();
            entry.connect_changed(move |e| {
                if let Some(u) = update.upgrade() {
                    u.set_sensitive(!e.text().is_empty());
                }
            });
        }
        let submit = {
            let send = send.clone();
            move |e: &adw::PasswordEntryRow| {
                if !e.text().is_empty() {
                    send(Some(e.text().to_string()));
                }
            }
        };
        let submit = Rc::new(submit);
        {
            let submit = submit.clone();
            entry.connect_entry_activated(move |e| submit(e));
        }
        {
            let entry = entry.downgrade();
            update.connect_clicked(move |_| {
                if let Some(e) = entry.upgrade() {
                    submit(&e);
                }
            });
        }
        {
            let send = send.clone();
            cancel.connect_clicked(move |_| send(None));
        }
        // Typing goes straight in, also when the card is made before the
        // dialog is on screen.
        entry.connect_map(|e| {
            e.grab_focus();
        });
        self.set_prompt(card.upcast());
        entry.grab_focus();
    }

    /// Put a question the update is waiting on under the step, its answers
    /// as buttons. An answer that agrees to a reboot is never offered; when
    /// that leaves none, the card offers `Stop the update` instead, which
    /// calls `on_stop` (Task 7: send Ctrl-C to the update's terminal). The
    /// card closes before either callback runs.
    pub fn ask(&self, p: &Prompt, reply: impl Fn(Answer) + 'static, on_stop: impl Fn() + 'static) {
        let Prompt::Question { text, answers } = p else {
            eprintln!("almanac: a password request reached the question card");
            return;
        };
        let card = inset_card();
        card.append(&label(&tr("The update asks"), &["heading"]));
        card.append(&label(text, &[]));
        let row = button_row();
        let reply: Rc<dyn Fn(Answer)> = Rc::new(reply);
        let on_stop: Rc<dyn Fn()> = Rc::new(on_stop);
        for choice in choices(answers) {
            let view = self.downgrade();
            let b = match choice {
                Choice::Answer(a) => {
                    let b = gtk::Button::with_label(&a.label);
                    let (a, reply) = (a.clone(), reply.clone());
                    b.connect_clicked(move |_| {
                        if let Some(v) = UpdateView::upgrade(&view) {
                            v.close_prompt();
                        }
                        reply(a.clone());
                    });
                    b
                }
                Choice::Stop => {
                    let b = gtk::Button::with_label(&tr("Stop the update"));
                    b.add_css_class("destructive-action");
                    let on_stop = on_stop.clone();
                    b.connect_clicked(move |_| {
                        if let Some(v) = UpdateView::upgrade(&view) {
                            v.close_prompt();
                        }
                        on_stop();
                    });
                    b
                }
            };
            row.append(&b);
        }
        card.append(&row);
        self.set_prompt(card.upcast());
    }

    /// Offer the fix on the failed step's card. The view keeps `on_fix`
    /// until `clear_fix`, so it must hold the view (and anything that holds
    /// the view) only weakly, or neither is ever freed.
    pub fn show_failure_fix(&self, fix: &Fix, on_fix: impl Fn() + 'static) {
        *self.0.fix.borrow_mut() = Some((fix.clone(), Rc::new(on_fix)));
        self.rerender();
    }

    /// Take the fix button away and drop its callback.
    pub fn clear_fix(&self) {
        self.0.fix.borrow_mut().take();
        self.rerender();
    }

    fn rerender(&self) {
        let last = self.0.last.borrow().clone();
        if let Some(t) = last {
            self.render(&t);
        }
    }

    /// The report takes the timeline's place in the same dialog.
    pub fn replace_with_report(&self, content: gtk::Widget) {
        self.close_prompt();
        self.0.toolbar.set_content(Some(&content));
    }

    /// Add output to Details. Control sequences are cleaned out here; one
    /// cut off at the end of a chunk waits for the next.
    pub fn append_log(&self, text: &str) {
        let follow = self.log_at_bottom();
        let joined = {
            let mut carry = self.0.log_carry.borrow_mut();
            let joined = std::mem::take(&mut *carry) + text;
            let (ready, rest) = split_trailing_escape(&joined);
            *carry = rest.to_string();
            ready.to_string()
        };
        let text = crate::postupdate::clean_output(&joined);
        let buf = &self.0.log;
        buf.insert(&mut buf.end_iter(), &text);
        self.trim_log(follow);
    }

    /// Replace Details with this output (e.g. `UpdateRun::log_tail`).
    pub fn set_log(&self, raw: &str) {
        let follow = self.log_at_bottom();
        self.0.log_carry.borrow_mut().clear();
        self.0.log.set_text(&crate::postupdate::clean_output(raw));
        self.trim_log(follow);
    }

    /// The log is scrolled to its end (or cannot scroll): new output keeps
    /// it there. Someone reading further up is left where they are.
    fn log_at_bottom(&self) -> bool {
        self.0
            .log_view
            .vadjustment()
            .is_none_or(|a| a.value() + a.page_size() >= a.upper() - 4.0)
    }

    fn trim_log(&self, follow: bool) {
        let buf = &self.0.log;
        let excess = buf.line_count() - LOG_LINES;
        if excess > 0 {
            if let Some(mut cut) = buf.iter_at_line(excess) {
                buf.delete(&mut buf.start_iter(), &mut cut);
            }
        }
        if follow {
            buf.move_mark(&self.0.log_end, &buf.end_iter());
            self.0.log_view.scroll_mark_onscreen(&self.0.log_end);
        }
    }
}

/// `almanac --preview-update-view[=running|failure|password|question|reboot|none]` (debug
/// builds only): the dialog fed a fixed timeline, for screenshots. Nothing
/// runs and nothing is answered.
#[cfg(debug_assertions)]
pub fn preview(win: &Win, which: &str) {
    if which == "none" {
        return;
    }
    use crate::inventory::{AUR, REPO};
    let store = win.shared.borrow().store.clone();
    let mut apps: Vec<(String, String, bool)> = (0..store.n_items())
        .filter_map(|i| {
            store
                .item(i)
                .as_ref()
                .and_then(AppItem::of)
                .filter(|it| it.has_update && (it.app.source == REPO || it.app.source == AUR))
                .map(|it| (it.app.name.clone(), it.app.update.clone(), false))
        })
        .take(5)
        .collect();
    if apps.len() < 5 {
        apps = [
            ("cursor-bin", "3.31.15"),
            ("mise-bin", "2026.11.11"),
            ("slack-desktop", "4.47.69"),
            ("voxtype-bin", "1.3.0"),
            ("omarchy-speedtest", "0.2.2"),
        ]
        .iter()
        .map(|(n, v)| (n.to_string(), v.to_string(), false))
        .collect();
    }
    let mut t = Timeline::new(&apps, false);
    let view = UpdateView::new(win);
    if which == "failure" {
        t.steps[0].1 = StepState::Failed {
            reason: "Another Omarchy update has been waiting since Thu 17:51 with no window \
                     to answer it in. It never touched your packages."
                .into(),
            changed: Changed::Nothing,
            fix: Some(Fix::StopStuckUpdate(vec![4242])),
        };
        view.render(&t);
        view.show_failure_fix(&Fix::StopStuckUpdate(vec![4242]), || {
            eprintln!("preview: fix clicked")
        });
    } else {
        for i in 0..2 {
            t.steps[i].1 = StepState::Done { detail: String::new() };
        }
        t.steps[2].1 = StepState::Running { detail: "Installing".into() };
        for (i, a) in t.apps.iter_mut().enumerate() {
            a.state = match i {
                0 | 1 => AppState::Done { version: a.to.clone() },
                2 => AppState::Installing,
                _ => AppState::Waiting,
            };
        }
        view.render(&t);
        match which {
            "password" => view.ask_password("[sudo] password for you: ", 2, |s| {
                eprintln!("preview: password given: {}", s.is_some())
            }),
            "question" | "reboot" => {
                let reboot = which == "reboot";
                let no = Answer { label: "No".into(), send: b"n\r".to_vec(), is_reboot_yes: false };
                let yes = Answer { label: "Yes".into(), send: b"y\r".to_vec(), is_reboot_yes: reboot };
                view.ask(
                    &Prompt::Question {
                        text: if reboot {
                            "Reboot now to finish the update?".into()
                        } else {
                            ":: Replace iptables with extra/iptables-nft? [Y/n]".into()
                        },
                        answers: if reboot { vec![yes] } else { vec![yes, no] },
                    },
                    |a| eprintln!("preview: answered {}", a.label),
                    || eprintln!("preview: stop"),
                );
            }
            _ => {}
        }
    }
    view.set_log(
        ":: Synchronizing package databases...\n core is up to date\n extra is up to date\n\
         :: Starting full system upgrade...\n:: Retrieving packages...\n\
         (1/5) upgrading cursor-bin\n(2/5) upgrading mise-bin\n(3/5) upgrading slack-desktop\n",
    );
    view.present();
    // The caller of a real run keeps the view; here nothing else would.
    std::mem::forget(view);
}

fn detach(w: &gtk::Widget) {
    if let Some(parent) = w.parent().and_then(|p| p.downcast::<gtk::Box>().ok()) {
        parent.remove(w);
    }
}

/// The first `gtk::Image` under `w` (note_row's prefix icon).
fn find_image(w: &gtk::Widget) -> Option<gtk::Image> {
    let mut child = w.first_child();
    while let Some(c) = child {
        if let Ok(img) = c.clone().downcast::<gtk::Image>() {
            return Some(img);
        }
        if let Some(found) = find_image(&c) {
            return Some(found);
        }
        child = c.next_sibling();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inwindow::tracker::{AppState, Changed, Fix, StepId, StepState, Timeline};

    fn five() -> Timeline {
        let apps: Vec<(String, String, bool)> = [
            ("cursor-bin", "3.31.15"),
            ("mise-bin", "2026.11.11"),
            ("slack-desktop", "4.47.69"),
        ]
        .iter()
        .map(|(n, v)| (n.to_string(), v.to_string(), false))
        .collect();
        Timeline::new(&apps, false)
    }

    fn set(t: &mut Timeline, id: StepId, s: StepState) {
        t.steps.iter_mut().find(|(i, _)| *i == id).unwrap().1 = s;
    }

    fn done() -> StepState {
        StepState::Done { detail: String::new() }
    }

    fn running() -> StepState {
        StepState::Running { detail: String::new() }
    }

    #[test]
    fn labels_and_positions() {
        let mut t = five();
        assert_eq!(subtitle(&t), "Starting…");
        for id in [StepId::Snapshot, StepId::SigningKeys, StepId::SystemPackages] {
            set(&mut t, id, done());
        }
        set(&mut t, StepId::Migrations, running());
        assert_eq!(subtitle(&t), "Step 4 of 6");
        assert_eq!(title(&t), "Updating your system");
        assert!((fraction(&t) - 0.5).abs() < 1e-9);

        t.generic = true;
        assert_eq!(subtitle(&t), "Updating…");

        assert_eq!(changed_line(&Changed::Nothing), "Nothing was changed.");
        assert_eq!(changed_line(&Changed::Some), "Some changes were made.");
        assert_eq!(changed_line(&Changed::Unknown), "");

        assert_eq!(fix_label(&Fix::StopStuckUpdate(vec![1])), "Stop it and update");
        assert_eq!(fix_label(&Fix::Resolve), "Resolve");
        assert_eq!(fix_label(&Fix::RetryUpdate), "Try again");
    }

    #[test]
    fn a_failure_names_what_changed() {
        let mut t = five();
        set(
            &mut t,
            StepId::Snapshot,
            StepState::Failed {
                reason: "Another Omarchy update is running.".into(),
                changed: Changed::Nothing,
                fix: Some(Fix::StopStuckUpdate(vec![7])),
            },
        );
        assert_eq!(title(&t), "The update didn't start");
        // What changed is said on the card only; the subtitle names the step.
        assert_eq!(subtitle(&t), "Stopped at: Snapshot");

        let mut t = five();
        set(&mut t, StepId::Snapshot, done());
        set(
            &mut t,
            StepId::SigningKeys,
            StepState::Failed {
                reason: "x".into(),
                changed: Changed::Unknown,
                fix: Some(Fix::RetryUpdate),
            },
        );
        assert_eq!(title(&t), "The update stopped");
        assert_eq!(subtitle(&t), "Stopped at: Signing keys");
    }

    #[test]
    fn every_step_done_is_finished() {
        let mut t = five();
        for (_, s) in t.steps.iter_mut() {
            *s = done();
        }
        assert_eq!(title(&t), "Update finished");
        assert_eq!(subtitle(&t), "All steps done");
        assert!((fraction(&t) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn steps_look_like_their_state() {
        assert_eq!(step_look(&StepState::Waiting), Look::Icon("radio-symbolic", "dim-label"));
        assert_eq!(step_look(&running()), Look::Spinner);
        assert_eq!(step_look(&done()), Look::Icon("object-select-symbolic", "success"));
        assert_eq!(
            step_look(&StepState::Failed {
                reason: String::new(),
                changed: Changed::Nothing,
                fix: None
            }),
            Look::Icon("dialog-error-symbolic", "error")
        );
    }

    #[test]
    fn app_rows_say_where_they_are() {
        let mut t = five();
        t.apps[0].state = AppState::Done { version: "3.31.15".into() };
        t.apps[1].state = AppState::Done { version: "2026.11.11".into() };
        t.apps[2].state = AppState::Installing;
        assert_eq!(app_suffix(&t.apps[0]), ("3.31.15 ✓".to_string(), "dim-label"));
        assert_eq!(app_suffix(&t.apps[2]), ("installing…".to_string(), "accent"));
        t.apps[2].state = AppState::Downloading;
        assert_eq!(app_suffix(&t.apps[2]), ("downloading…".to_string(), "accent"));
        t.apps[2].state = AppState::Waiting;
        assert_eq!(app_suffix(&t.apps[2]), ("4.47.69".to_string(), "dim-label"));
        t.apps[2].state = AppState::Failed;
        assert_eq!(app_suffix(&t.apps[2]), ("not updated".to_string(), "error"));

        // Two of three done, on the step they belong to; none elsewhere.
        assert_eq!(apps_count(&t, StepId::SystemPackages).as_deref(), Some("2 of 3"));
        assert_eq!(apps_count(&t, StepId::AurAndTools), None);
    }

    /// One visual system: spacing from the scale the other pages use.
    #[test]
    fn spacing_comes_from_the_scale() {
        const SCALE: [i64; 8] = [2, 4, 6, 8, 12, 16, 20, 24];
        let src = include_str!("update_view.rs");
        let code = src.split("#[cfg(test)]").next().unwrap_or(src);
        let re = regex::Regex::new(
            r"\.(margin_(?:start|end|top|bottom)|spacing)\((\d+)\)",
        )
        .unwrap();
        let off: Vec<String> = re
            .captures_iter(code)
            .filter(|c| !SCALE.contains(&c[2].parse::<i64>().unwrap_or(-1)))
            .map(|c| format!("{}={}", &c[1], &c[2]))
            .collect();
        assert!(off.is_empty(), "spacing off the scale: {off:?}");
    }

    #[test]
    fn a_reboot_yes_is_never_a_button() {
        use crate::inwindow::prompt::Answer;
        let a = |label: &str, reboot: bool| Answer {
            label: label.into(),
            send: b"y\r".to_vec(),
            is_reboot_yes: reboot,
        };
        let answers = vec![a("Yes", true), a("No", false)];
        let shown: Vec<&str> = buttons(&answers).iter().map(|a| a.label.as_str()).collect();
        assert_eq!(shown, ["No"]);
        assert_eq!(choices(&answers), vec![Choice::Answer(&answers[1])]);

        // Only a reboot "yes" on offer: the card still has a way on.
        let only_yes = vec![a("Yes", true)];
        assert!(buttons(&only_yes).is_empty());
        assert_eq!(choices(&only_yes), vec![Choice::Stop]);
    }

    #[test]
    fn an_escape_cut_at_the_end_waits_for_the_rest() {
        assert_eq!(split_trailing_escape("plain"), ("plain", ""));
        assert_eq!(split_trailing_escape("a\x1b[1;3"), ("a", "\x1b[1;3"));
        assert_eq!(split_trailing_escape("a\x1b"), ("a", "\x1b"));
        assert_eq!(split_trailing_escape("a\x1b[1mb"), ("a\x1b[1mb", ""));
        assert_eq!(split_trailing_escape("a\x1b]0;title"), ("a", "\x1b]0;title"));
        assert_eq!(split_trailing_escape("a\x1b]0;t\x07b"), ("a\x1b]0;t\x07b", ""));
        assert_eq!(split_trailing_escape("a\x1b(B"), ("a\x1b(B", ""));
        // Rejoined, the cut sequence cleans away entirely.
        let (ready, rest) = split_trailing_escape("x\x1b[3");
        let next = format!("{rest}2mred");
        assert_eq!(
            crate::postupdate::clean_output(ready) + &crate::postupdate::clean_output(&next),
            "xred"
        );
    }
}
