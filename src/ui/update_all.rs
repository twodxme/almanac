//! "Update all": every row with an update waiting, each through the tool
//! that owns it.
//!
//! `omarchy update` covers pacman, the AUR and mise tools, and nothing else.
//! Plugins, AppImages, agent CLIs and local checkouts each have their own
//! updater; leaving them out meant the button's count promised updates it
//! never ran, and those rows stayed listed after every "successful" update.
//!
//! The system update runs first -- on Omarchy inside the window, as a step
//! timeline with the password asked in place (`start_in_window`), elsewhere
//! in a terminal -- then everything else as one confirmed batch. What needs a person's decision --
//! a major agent release, a checkout with local edits, a plugin outside the
//! update scope -- is held back and named, and keeps its own button.
//!
//! Before it starts, what would make it fail is looked at (free space,
//! another package manager, the battery); after it, whether it went through
//! (`postupdate`), with a repair offered for what can be repaired.

use std::cell::{Cell, RefCell};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::{gio, glib};
use serde_json::Value;

use crate::history::CommandRun;
use crate::i18n::tr;
use crate::inventory::{self, App};
use crate::inwindow::run::{Event, UpdateRun};
use crate::inwindow::tracker::{AppRow, AppState, Changed, Fix, StepId, StepState, Timeline};
use crate::postupdate::{self, Inputs, Issue, Level, Repair, Wait};
use crate::tasks::{self, LogFn, Step};
use crate::ui::actions::{appimage_update_work, popen_new_session, raised, truthy, update_cmd};
use crate::ui::plugins::{plan_plugin_updates, plugin_step, plugin_update_states};
use crate::ui::shell::{spawn_bg, WeakWin, Win};
use crate::ui::update_report::{BatchDone, BatchResult, Run, SharedRun};
use crate::ui::update_view::{step_label, UpdateView};
use crate::verify::{self, Outcome, ProbeValue};
use crate::{agents, patches, plugins, selfupdate, xdg};

thread_local! {
    /// An Update all is on, from its start to its history entry.
    static RUNNING: Cell<bool> = const { Cell::new(false) };
}

/// One Update all at a time. Taken as a run starts and held by whatever
/// carries it on (the preflight dialog, the terminal's watcher, the report)
/// until its history entry is written, or until it is dropped on the way: a
/// dialog cancelled, a terminal that would not start, the window closed.
/// While it is held, the button and Ctrl+U are refused with a toast.
pub struct Busy(());

impl Busy {
    pub fn take() -> Option<Busy> {
        RUNNING.with(|r| (!r.replace(true)).then_some(Busy(())))
    }

    pub fn held() -> bool {
        RUNNING.with(Cell::get)
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        RUNNING.with(|r| r.set(false));
    }
}

/// What starts the run once preflight lets it.
type Go = Rc<dyn Fn(&Win, Busy)>;

/// Which updater a row with an update goes to: the same choice its own
/// Update button makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// `omarchy update`: repo, AUR, mise tools
    System,
    Flatpak,
    Plugin,
    /// a local checkout that is not a plugin
    Git,
    AppImage,
    Agent,
    Almanac,
}

pub fn route(app: &App) -> Route {
    let extra = |k: &str| app.extra.get(k).is_some_and(truthy);
    if extra("self") {
        Route::Almanac
    } else if app.source == inventory::LOCAL {
        if extra("plugin_id") {
            Route::Plugin
        } else {
            Route::Git
        }
    } else if app.source == inventory::APPIMAGE {
        Route::AppImage
    } else if extra("update_cmd") {
        Route::Agent
    } else if app.source == inventory::FLATPAK {
        Route::Flatpak
    } else {
        Route::System
    }
}

/// By path: the terminal's login shell puts user-writable folders first.
const FLATPAK: &str = "/usr/bin/flatpak";

/// The terminal half: `None` when nothing waiting needs it. With both, the
/// exit status is the system update's when it failed: a flatpak update
/// that went fine must not hide it.
pub fn system_command(routes: &[Route], command: &str) -> Option<String> {
    system_command_for(routes, command, false)
}

/// `system_command`, for the terminal or for the window. In the window
/// flatpak is not part of it: it is one of the Other apps (`in_rest`).
pub fn system_command_for(routes: &[Route], command: &str, in_window: bool) -> Option<String> {
    let system = routes.contains(&Route::System);
    let flatpak = routes.contains(&Route::Flatpak) && !in_window;
    match (system, flatpak) {
        (false, false) => None,
        (true, false) => Some(command.to_string()),
        (false, true) => Some(format!("{FLATPAK} update -y")),
        (true, true) => Some(format!(
            "{command}; rc=$?; {FLATPAK} update -y || exit; exit $rc"
        )),
    }
}

/// Whether a row waiting goes with the batch after the system half. In the
/// window flatpak is one of them; in the terminal it rides along there.
pub fn in_rest(r: Route, in_window: bool) -> bool {
    match r {
        Route::System => false,
        Route::Flatpak => in_window,
        _ => true,
    }
}

/// The system half runs in the window only for `omarchy update` (the steps
/// it shows are Omarchy's), and never in a demo, which updates nothing.
pub fn use_in_window(system_command: &str, demo: bool) -> bool {
    system_command == "omarchy update" && !demo
}

/// What runs on the window's terminal.
fn update_argv() -> Vec<String> {
    update_argv_from(std::env::var_os("ALMANAC_UPDATE_COMMAND"))
}

/// `omarchy update -y`, or in a debug build the script `ALMANAC_UPDATE_COMMAND`
/// names: a fake updater for trying the view without updating anything.
fn update_argv_from(seam: Option<std::ffi::OsString>) -> Vec<String> {
    #[cfg(debug_assertions)]
    if let Some(p) = seam.filter(|p| !p.is_empty()) {
        return vec![p.to_string_lossy().into_owned()];
    }
    #[cfg(not(debug_assertions))]
    let _ = seam;
    ["omarchy", "update", "-y"].map(String::from).to_vec()
}

/// The Other apps' steps, or in a debug build the script
/// `ALMANAC_UPDATE_REST_COMMAND` names in place of each (given the step's
/// label): for trying the window without updating any app.
fn rest_steps_from(steps: Vec<Step>, seam: Option<std::ffi::OsString>) -> Vec<Step> {
    #[cfg(debug_assertions)]
    if let Some(p) = seam.filter(|p| !p.is_empty()) {
        let p = p.to_string_lossy().into_owned();
        return steps
            .into_iter()
            .map(|s| Step::argv(s.label.clone(), vec![p.clone(), s.label]))
            .collect();
    }
    #[cfg(not(debug_assertions))]
    let _ = seam;
    steps
}

/// What Quit does while an update runs in the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitDecision {
    Allow,
    /// it stops the update: ask first
    Confirm,
    /// pacman is under way: stopping it part-way can leave the system broken
    Refuse,
}

pub fn quit_decision(t: &Timeline) -> QuitDecision {
    let running = |s: &StepState| matches!(s, StepState::Running { .. });
    if t.steps
        .iter()
        .any(|(id, s)| *id == StepId::SystemPackages && running(s))
    {
        return QuitDecision::Refuse;
    }
    let failed = t
        .steps
        .iter()
        .any(|(_, s)| matches!(s, StepState::Failed { .. }));
    let under_way = t
        .steps
        .iter()
        .any(|(_, s)| running(s) || (!failed && matches!(s, StepState::Waiting)));
    if under_way {
        QuitDecision::Confirm
    } else {
        QuitDecision::Allow
    }
}

/// The fix for "Another Omarchy update is running", from who holds the
/// lock now: stopped for you only when nobody can be answering it (no
/// terminal) and pacman is not at work under it, as the preflight decides.
/// Otherwise it is left to finish, said in the preflight's words, and the
/// way on is to try again after.
fn busy_fix(holders: &[postupdate::Holder], pacman_running: bool) -> (Option<String>, Fix) {
    let live = holders.iter().any(|h| h.terminal) || pacman_running;
    if !live || holders.is_empty() {
        return (
            None,
            Fix::StopStuckUpdate(holders.iter().map(|h| h.pid).collect()),
        );
    }
    let who = holders
        .iter()
        .map(|h| format!("{} ({})", h.name, h.pid))
        .collect::<Vec<_>>()
        .join(", ");
    let reason =
        tr("{who} holds the update lock. Finish it in its terminal, or wait for it, then update.")
            .replace("{who}", &who);
    (Some(reason), Fix::RetryUpdate)
}

/// What Stop does while the update runs in the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopDecision {
    Now,
    /// pacman is under way: say what stopping then means, then stop
    Confirm,
}

pub fn stop_decision(t: &Timeline) -> StopDecision {
    let pacman = t
        .steps
        .iter()
        .any(|(id, s)| *id == StepId::SystemPackages && matches!(s, StepState::Running { .. }));
    if pacman {
        StopDecision::Confirm
    } else {
        StopDecision::Now
    }
}

/// The failed step's fix, if it has one.
fn failed_fix(t: &Timeline) -> Option<Fix> {
    t.steps.iter().find_map(|(_, s)| match s {
        StepState::Failed { fix, .. } => fix.clone(),
        _ => None,
    })
}

/// A failure that changed nothing waits on the timeline with its fix: there
/// is nothing for the checks after an update to look at.
fn pauses(t: &Timeline) -> bool {
    t.steps.iter().any(|(_, s)| {
        matches!(
            s,
            StepState::Failed {
                changed: Changed::Nothing,
                ..
            }
        )
    })
}

/// Why Almanac itself stopped the update before it was over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stopped {
    /// The password card was cancelled (Ctrl-C went to sudo).
    Cancelled,
    /// The terminal would have echoed the password, so it was not sent and
    /// the update was interrupted.
    PasswordRefused,
    /// The password could not be sent for another reason (the terminal
    /// could not be read or written), and the update was interrupted.
    PasswordUnsent,
    /// Stop was pressed.
    ByUser,
}

/// What a refused password makes of the run once Almanac stops it.
fn stopped_after(e: &crate::inwindow::run::PasswordError) -> Stopped {
    match e {
        crate::inwindow::run::PasswordError::WouldEcho => Stopped::PasswordRefused,
        _ => Stopped::PasswordUnsent,
    }
}

/// The update stopped because of `why` (sudo is asked before anything
/// else): the failure says so, and that nothing changed when no step had
/// finished.
fn mark_stopped(t: &mut Timeline, why: Stopped) {
    let none_done = !t
        .steps
        .iter()
        .any(|(_, s)| matches!(s, StepState::Done { .. }));
    for (_, s) in &mut t.steps {
        if let StepState::Failed {
            reason,
            changed,
            fix,
        } = s
        {
            *reason = match why {
                Stopped::Cancelled => tr("The password was cancelled, so the update stopped."),
                Stopped::PasswordRefused => tr(
                    "Almanac stopped the update without sending the password: the terminal \
                     would have shown your password as you typed it.",
                ),
                Stopped::ByUser => tr("You stopped the update."),
                Stopped::PasswordUnsent => tr(
                    "Almanac stopped the update: the password was not sent, because the \
                     update's terminal could not be reached.",
                ),
            };
            if none_done {
                *changed = Changed::Nothing;
            }
            if fix.is_none() {
                *fix = Some(Fix::RetryUpdate);
            }
        }
    }
}

/// The Other apps step under way, a row for each (name, new version).
fn other_apps_start(t: &mut Timeline, apps: &[(String, String)]) {
    match t.steps.iter_mut().find(|(i, _)| *i == StepId::OtherApps) {
        Some((_, s)) => {
            *s = StepState::Running {
                detail: String::new(),
            }
        }
        None => t.steps.push((
            StepId::OtherApps,
            StepState::Running {
                detail: String::new(),
            },
        )),
    }
    for (name, to) in apps {
        t.apps.push(AppRow {
            name: name.clone(),
            to: to.clone(),
            step: StepId::OtherApps,
            state: AppState::Waiting,
        });
    }
}

/// One step of the Other apps has started: the rows it updates are under
/// way.
fn other_apps_working(t: &mut Timeline, name: &str) {
    for a in &mut t.apps {
        if a.step == StepId::OtherApps && a.name == name && a.state == AppState::Waiting {
            a.state = AppState::Installing;
        }
    }
}

/// The batch is over. Each row its check could judge ticks or turns red;
/// the others (held back, not checkable) leave the list, the report names
/// them. Nothing run, nothing claimed: the step goes. A row that did not
/// update has its check's reason in the step's failure.
fn other_apps_done(t: &mut Timeline, outcomes: &[(String, Outcome)], ran: bool, ok: bool) {
    if !ran {
        t.steps.retain(|(i, _)| *i != StepId::OtherApps);
        t.apps.retain(|a| a.step != StepId::OtherApps);
        return;
    }
    let mut why = Vec::new();
    t.apps.retain_mut(|a| {
        if a.step != StepId::OtherApps {
            return true;
        }
        let out = outcomes.iter().find(|(n, _)| *n == a.name).map(|(_, o)| o);
        match out.and_then(|o| o.changed) {
            Some(true) => {
                a.state = AppState::Done {
                    version: a.to.clone(),
                };
                true
            }
            Some(false) => {
                a.state = AppState::Failed;
                if let Some(o) = out.filter(|o| !o.message.is_empty()) {
                    why.push(o.message.clone());
                }
                true
            }
            None => false,
        }
    });
    let failed = !ok
        || t.apps
            .iter()
            .any(|a| a.step == StepId::OtherApps && a.state == AppState::Failed);
    let some_done = t
        .apps
        .iter()
        .any(|a| a.step == StepId::OtherApps && matches!(a.state, AppState::Done { .. }));
    if let Some((_, s)) = t.steps.iter_mut().find(|(i, _)| *i == StepId::OtherApps) {
        *s = if failed {
            StepState::Failed {
                reason: if why.is_empty() {
                    tr("Not everything updated; the report says which.")
                } else {
                    why.join("\n")
                },
                changed: if some_done {
                    Changed::Some
                } else {
                    Changed::Unknown
                },
                fix: None,
            }
        } else {
            StepState::Done {
                detail: String::new(),
            }
        };
    }
}

/// The notification at the end of a run in the window.
pub fn finished_note(r: &postupdate::Report) -> String {
    if !r.passed() {
        return tr("Update needs a look");
    }
    let n = r.changes.iter().filter(|c| !c.to.is_empty()).count()
        + r.checks
            .iter()
            .filter(|c| c.id == postupdate::CheckId::Item && c.status == postupdate::Status::Pass)
            .count();
    tr("Update finished — {n} updated").replace("{n}", &n.to_string())
}

/// The notification's body: a restart to finish comes first.
pub fn finished_body(r: &postupdate::Report) -> String {
    let restart = r.checks.iter().any(|c| {
        matches!(
            c.id,
            postupdate::CheckId::Restart | postupdate::CheckId::Kernel
        )
    });
    if restart {
        tr("Restart to finish. Open Almanac to see the report.")
    } else {
        tr("Open Almanac to see the report.")
    }
}

/// The batch after the terminal: what will run, and what is held back with
/// the reason.
#[derive(Default)]
pub struct Batch {
    pub steps: Vec<Step>,
    /// one preview line per item that will run
    pub lines: Vec<String>,
    pub held: Vec<String>,
    /// the rows that will run, each judged by its own probe afterwards
    pub apps: Vec<App>,
    /// for each step, the rows (indexes into `apps`) it updates
    pub owners: Vec<Vec<usize>>,
}

impl Batch {
    fn hold(&mut self, name: &str, why: &str) {
        self.held.push(format!("{name} — {why}"));
    }

    /// The steps added since the last claim update the rows added since.
    fn claim(&mut self, rows_before: usize) {
        let rows: Vec<usize> = (rows_before..self.apps.len()).collect();
        self.owners.resize(self.steps.len(), rows);
    }
}

/// Each probe asked now, in order: None where there is none or it failed.
fn ask_probes(probes: &[Option<verify::TryProbe>]) -> Vec<Option<ProbeValue>> {
    probes
        .iter()
        .map(|p| p.as_ref().and_then(|p| p().ok().flatten()))
        .collect()
}

/// The batch is over: each row judged by its probe, asked again now.
fn judge_batch(
    apps: Vec<App>,
    probes: &[Option<verify::TryProbe>],
    before: &[Option<ProbeValue>],
) -> Vec<(App, Outcome)> {
    let after = ask_probes(probes);
    apps.into_iter()
        .zip(probes.iter().zip(before.iter().zip(after.iter())))
        .map(|(app, (p, (b, a)))| {
            let out = if p.is_some() {
                verify::judge_app(&app, b.as_ref(), a.as_ref())
            } else {
                Outcome::new(
                    None,
                    format!(
                        "{}: the update ran, but its result could not be checked",
                        app.name
                    ),
                )
            };
            (app, out)
        })
        .collect()
}

/// An agent CLI: its own update command, unless the release is a major one
/// (shown with its notes, asked about on its own).
pub fn plan_agent(app: &App, batch: &mut Batch) -> Result<(), String> {
    let cmd = update_cmd(app)?;
    if cmd.is_empty() {
        batch.hold(&app.name, "has no self-update command");
    } else if agents::major_jump(&app.version, &app.update) {
        batch.hold(
            &app.name,
            &format!("{} is a major release; update it on its own", app.update),
        );
    } else {
        batch.lines.push(format!(
            "{:22} {} → {}   $ {}",
            app.name,
            app.version,
            app.update,
            cmd.join(" ")
        ));
        let rows = batch.apps.len();
        batch
            .steps
            .push(Step::argv(format!("Updating {}", app.name), cmd));
        batch.apps.push(app.clone());
        batch.claim(rows);
    }
    Ok(())
}

/// Work that reads git and the network: run off the UI thread.
fn plan(apps: Vec<App>, have: Vec<crate::store::Plugin>) -> Result<Batch, String> {
    let mut batch = Batch::default();
    let by = |r: Route| apps.iter().filter(move |a| route(a) == r);

    if by(Route::Plugin).next().is_some() {
        let (states, trust) = plugin_update_states(have)?;
        let plan = plan_plugin_updates(&states, &trust, &plugins::try_scope()?);
        if let Some(e) = plan.raised {
            return Err(e);
        }
        for s in &plan.ready {
            batch
                .lines
                .push(format!("{:22} omarchy plugin update", s.id));
            let rows = batch.apps.len();
            batch.steps.push(Step::call(
                format!("Updating {}", s.id),
                plugin_step(s.id.clone(), plan.pins.get(&s.id).cloned()),
            ));
            let row = by(Route::Plugin)
                .find(|a| crate::ui::shell::as_text(a.extra.get("plugin_id")) == s.id);
            batch.apps.extend(row.cloned());
            batch.claim(rows);
        }
        for (s, why) in plan.held.iter().zip(&plan.held_why) {
            batch.hold(
                &s.id,
                &format!("outside the plugin update scope set in Preferences ({why})"),
            );
        }
        // Rows counted as updates that are not clean fast-forwards now.
        for a in by(Route::Plugin) {
            let pid = crate::ui::shell::as_text(a.extra.get("plugin_id"));
            let known = plan.ready.iter().chain(&plan.held).any(|s| s.id == pid);
            if !known {
                let detail = crate::ui::shell::as_text(a.extra.get("plugin_detail"));
                batch.hold(
                    &a.name,
                    if detail.is_empty() {
                        "cannot be updated as it is"
                    } else {
                        &detail
                    },
                );
            }
        }
    }

    for a in by(Route::Git) {
        let Some(Value::String(repo)) = a.extra.get("repo") else {
            batch.hold(&a.name, "no checkout to update");
            continue;
        };
        let state = patches::try_analyze(repo, None)?;
        if state.kind == patches::CLEAN || state.kind == patches::UNKNOWN {
            batch
                .lines
                .push(format!("{:22} git pull in {repo}", a.name));
            let rows = batch.apps.len();
            batch
                .steps
                .extend(patches::steps(repo, &state, &a.name, None));
            batch.apps.push(a.clone());
            batch.claim(rows);
        } else {
            batch.hold(
                &a.name,
                "has local edits; update it on its own to see what happens to them",
            );
        }
    }

    for a in by(Route::AppImage) {
        if !a.extra.get("download").is_some_and(truthy) {
            batch.hold(&a.name, "no download URL");
            continue;
        }
        batch.lines.push(format!(
            "{:22} {} → {}   download",
            a.name, a.version, a.update
        ));
        let app = a.clone();
        let rows = batch.apps.len();
        batch.steps.push(Step::call(
            format!("Downloading {} {}", a.name, a.update),
            Arc::new(move |_log: LogFn<'_>| appimage_update_work(&app)),
        ));
        batch.apps.push(a.clone());
        batch.claim(rows);
    }

    for a in by(Route::Agent) {
        plan_agent(a, &mut batch)?;
    }

    // In the window flatpak is not part of the system half: one step for
    // every Flatpak app, each judged by its own probe after it.
    let flatpaks: Vec<&App> = by(Route::Flatpak).collect();
    if !flatpaks.is_empty() {
        batch
            .lines
            .push(format!("{:22} {FLATPAK} update -y", "Flatpak"));
        let rows = batch.apps.len();
        batch.steps.push(Step::argv(
            "Updating Flatpak apps",
            vec![FLATPAK.into(), "update".into(), "-y".into()],
        ));
        batch.apps.extend(flatpaks.into_iter().cloned());
        batch.claim(rows);
    }

    if let Some(a) = by(Route::Almanac).next() {
        let info = selfupdate::try_check(true)?;
        if let Some((line, steps)) = almanac_update(&info) {
            batch.lines.push(format!("{:22} {line}", a.name));
            let rows = batch.apps.len();
            batch.steps.extend(steps);
            batch.apps.push(a.clone());
            batch.claim(rows);
        } else {
            batch.hold(
                &a.name,
                if info.note.is_empty() {
                    "cannot update itself here"
                } else {
                    &info.note
                },
            );
        }
    }
    Ok(batch)
}

/// How Almanac updates itself, as a line for the plan and its steps: a
/// checkout pulls, the package downloaded from GitHub installs the newer
/// release. None when neither can.
pub fn almanac_update(info: &selfupdate::Info) -> Option<(String, Vec<Step>)> {
    if info.can_update {
        let steps = selfupdate::update_steps()
            .into_iter()
            .map(|(label, argv)| Step::argv(label, argv))
            .collect();
        return Some(("git pull --ff-only".into(), steps));
    }
    if selfupdate::offers_download(info) {
        let steps = selfupdate::package::update_steps(&info.latest);
        if !steps.is_empty() {
            let line = format!(
                "{} → {}   download, check, pacman -U",
                info.version, info.latest
            );
            return Some((line, steps));
        }
    }
    None
}

impl Win {
    /// Update everything listed as having an update.
    ///
    /// The terminal is the point for the system half: `omarchy update` needs
    /// a password, and a GUI cannot answer that prompt. Running it where the
    /// user can type is the honest solution; the alternative is passwordless
    /// root. When it finishes it is checked, and the list re-read.
    /// Everything else follows as one batch, so nothing counted is left
    /// behind.
    pub fn on_update_all(&self) {
        // Demo mode never touches the machine: no terminal, no updaters.
        if crate::demo::active() {
            self.say(&tr("Demo mode: nothing is updated"), 5);
            return;
        }
        if Busy::held() {
            self.say(&tr("Update all is already running"), 5);
            return;
        }
        if self.confirm_news_then(|w| w.update_all_now()) {
            return;
        }
        self.update_all_now();
    }

    /// Past the news: the run starts here, and holds `Busy` from now on.
    fn update_all_now(&self) {
        let Some(busy) = Busy::take() else {
            self.say(&tr("Update all is already running"), 5);
            return;
        };
        let pending: Vec<App> = self
            .store_objects()
            .iter()
            .filter_map(crate::ui::shell::AppItem::of)
            .filter(|i| i.has_update)
            .map(|i| i.app.clone())
            .collect();
        let routes: Vec<Route> = pending.iter().map(route).collect();
        let system = xdg::system_update_command();
        let in_window = use_in_window(&system, crate::demo::active());
        // The rows the timeline starts with: (name, new version, from the
        // AUR or mise rather than the repos).
        let apps: Vec<(String, String, bool)> = pending
            .iter()
            .filter(|a| route(a) == Route::System)
            .map(|a| {
                (
                    a.name.clone(),
                    a.update.clone(),
                    a.source != inventory::REPO,
                )
            })
            .collect();
        let rest: Vec<App> = pending
            .into_iter()
            .filter(|a| in_rest(route(a), in_window))
            .collect();
        let command = system_command_for(&routes, &system, in_window);
        let pacman = routes.contains(&Route::System);
        // What would make it fail part-way is looked at first; /proc and
        // statvfs are read off the UI thread.
        let w = self.downgrade();
        spawn_bg(postupdate::probe_preflight, move |p| {
            let Some(win) = w.upgrade() else { return };
            let issues = postupdate::preflight_issues(&p, pacman);
            win.preflight(
                issues,
                busy,
                Rc::new(move |win: &Win, busy: Busy| {
                    let shown = in_window.then(|| apps.clone());
                    win.start_update_all(command.clone(), pacman, rest.clone(), shown, busy)
                }),
            );
        });
    }

    /// Say what stands in the way, in the app rather than as a terminal
    /// that fails: a blocking problem has no "Continue anyway".
    fn preflight(&self, issues: Vec<Issue>, busy: Busy, go: Go) {
        if issues.is_empty() {
            return go(self, busy);
        }
        // A block with a fix of its own (a stuck update to stop) can still be
        // got past from here; one without cannot, and then nothing is offered.
        let blocked = issues
            .iter()
            .any(|i| i.level == Level::Block && i.fix.is_none());
        let fix = if blocked {
            None
        } else {
            issues.iter().find_map(|i| i.fix.clone())
        };
        let (fix_label, fix_title, fix_failed) = match &fix {
            Some(Repair::StopUpdate(_)) => (
                tr("Stop it and continue"),
                tr("Stopping the stuck update"),
                tr("The earlier update is still running, so this one did not start"),
            ),
            _ => (
                tr("Remove it and continue"),
                tr("Removing pacman’s lock file"),
                tr("The lock file is still there, so the update did not start"),
            ),
        };
        let any_block = issues.iter().any(|i| i.level == Level::Block);
        let dialog = adw::AlertDialog::builder()
            .heading(if any_block {
                tr("The update cannot start yet")
            } else {
                tr("Before updating")
            })
            .body(if any_block {
                tr("Starting now would fail part-way.")
            } else {
                tr("The update can go ahead, but this may get in its way.")
            })
            .build();
        let list = Win::note_list();
        for i in &issues {
            let icon = if i.level == Level::Block {
                "dialog-error-symbolic"
            } else {
                "dialog-warning-symbolic"
            };
            list.append(&Win::note_row(icon, &i.title, &i.detail));
        }
        dialog.set_extra_child(Some(&list));
        dialog.add_response("cancel", &tr("Cancel"));
        if !any_block {
            dialog.add_response("continue", &tr("Continue anyway"));
        }
        if fix.is_some() {
            dialog.add_response("fix", &fix_label);
            dialog.set_response_appearance("fix", adw::ResponseAppearance::Suggested);
        }
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let w = self.downgrade();
        // Cancel lets the run go, and with it `Busy`.
        let busy = RefCell::new(Some(busy));
        dialog.connect_response(None, move |_d, resp| {
            let (Some(win), Some(busy)) = (w.upgrade(), busy.borrow_mut().take()) else {
                return;
            };
            match (resp, &fix) {
                ("continue", _) => go(&win, busy),
                ("fix", Some(f)) => {
                    let steps = postupdate::repair_steps(std::slice::from_ref(f), false);
                    let (w, go, fix_failed) = (win.downgrade(), go.clone(), fix_failed.clone());
                    tasks::run(
                        &win.window,
                        &fix_title,
                        steps,
                        Some(Box::new(move |ok| {
                            let Some(win) = w.upgrade() else { return };
                            if ok {
                                go(&win, busy);
                            } else {
                                win.say(&fix_failed, 8);
                            }
                        })),
                    );
                }
                _ => {}
            }
        });
        dialog.present(Some(&self.window));
    }

    /// `in_window`: the timeline's rows when the system half runs in the
    /// window, None for the terminal.
    fn start_update_all(
        &self,
        command: Option<String>,
        pacman: bool,
        rest: Vec<App>,
        in_window: Option<Vec<(String, String, bool)>>,
        busy: Busy,
    ) {
        let run = Run::new(pacman && command.is_some());
        run.borrow_mut().busy = Some(busy);
        match (command, in_window) {
            (Some(command), Some(apps)) if pacman => self.start_in_window(command, run, apps, rest),
            (Some(command), _) => self.run_system_update(&command, pacman, rest, run),
            (None, _) => {
                let r = run.clone();
                self.update_rest_then(rest, Box::new(move |win, res| win.batch_done(r, res)));
            }
        }
    }

    fn run_system_update(&self, command: &str, pacman: bool, rest: Vec<App>, run: SharedRun) {
        // Where the script reports back. Without it the update still runs;
        // how it ended is then not known.
        let (files, files_error) =
            match postupdate::run_dir().and_then(|d| postupdate::new_run_files_in(&d)) {
                Ok(f) => (Some(f), None),
                Err(e) => {
                    eprintln!("almanac: {e}");
                    (None, Some(e))
                }
            };
        let recorder = Path::new("/usr/bin/script");
        let unrecorded = postupdate::unrecorded_why(files_error.as_deref(), recorder.exists());
        let script = postupdate::terminal_script(
            command,
            files.as_ref(),
            recorder.exists().then_some(recorder),
        );
        let Some(argv) = xdg::terminal_argv(&["bash", "-lc", script.as_str()]) else {
            self.say(&tr("No terminal found to run the update in"), 5);
            return;
        };
        if argv.is_empty() {
            self.say(&tr("No terminal found to run the update in"), 5);
            return;
        }
        let command = command.to_string();
        let w = self.downgrade();
        glib::spawn_future_local(async move {
            // How things were, asked before the update starts: asked while
            // it runs, what it breaks would count as how things were, and
            // the checks after it would pass.
            let before = gio::spawn_blocking(move || postupdate::probe_before(pacman))
                .await
                .unwrap_or_default();
            let Some(win) = w.upgrade() else {
                if let Some(f) = &files {
                    f.remove();
                }
                return;
            };
            // Where this update's lines in pacman.log will start.
            let offset = postupdate::log_offset(Path::new(postupdate::PACMAN_LOG));
            let started = Instant::now();
            let proc = match popen_new_session(&argv) {
                Ok(p) => p,
                Err(e) => {
                    if let Some(f) = &files {
                        f.remove();
                    }
                    win.say(&format!("Could not start the update: {e}"), 5);
                    return;
                }
            };
            {
                let mut r = run.borrow_mut();
                r.terminal = true;
                r.command = command;
                r.files = files.clone();
                r.before = before.clone();
            }
            win.say(
                &tr("Update running in a terminal — it is checked when it finishes"),
                5,
            );
            drop(win);

            // The `wait` thread, as a main-loop future: a GSubprocess is
            // waited on there. A single-instance terminal returns at once,
            // so its end is only one of the signs the update is over.
            let exited: Rc<Cell<Option<Duration>>> = Rc::new(Cell::new(None));
            let e = exited.clone();
            glib::spawn_future_local(async move {
                let _ = proc.wait_future().await;
                e.set(Some(started.elapsed()));
            });
            let status_file = files.as_ref().map(|f| f.status.clone());
            let status = loop {
                glib::timeout_future(Duration::from_secs(1)).await;
                let exited_at = exited.get();
                let st = match status_file.clone() {
                    Some(p) => gio::spawn_blocking(move || postupdate::read_status(&p))
                        .await
                        .unwrap_or_default(),
                    None => postupdate::RunStatus::default(),
                };
                match postupdate::wait_step(
                    started.elapsed(),
                    exited_at,
                    &st,
                    status_file.is_some(),
                ) {
                    Wait::Keep => continue,
                    Wait::Done => break st,
                    Wait::GiveUp(msg) => {
                        if let Some(win) = w.upgrade() {
                            win.say(&tr(msg), 10);
                            win.after_pacman(true);
                            let argv = run.borrow().command.clone();
                            run.borrow_mut().record.commands.push(CommandRun {
                                argv,
                                exit: None,
                                secs: None,
                            });
                            win.finish_update(run);
                        }
                        return;
                    }
                }
            };
            // Read the list again now; the checks follow.
            let Some(win) = w.upgrade() else { return };
            win.after_pacman(true);
            drop(win);
            let started_wall = run.borrow().started_wall;
            let exit = status.exit;
            let checked = gio::spawn_blocking(move || {
                let after = postupdate::probe_after(pacman);
                let section = offset
                    .and_then(|o| postupdate::read_log_since(Path::new(postupdate::PACMAN_LOG), o));
                let output = files
                    .as_ref()
                    .and_then(|f| std::fs::read(&f.output).ok())
                    .map(|b| String::from_utf8_lossy(&b).into_owned())
                    .unwrap_or_default();
                let log = section.as_deref().map(|t| {
                    let mut scan = postupdate::scan_log(t);
                    postupdate::scan_output(&output, &mut scan);
                    scan
                });
                let inputs = Inputs {
                    system: pacman,
                    exit,
                    log,
                    before: before.clone(),
                    after: after.clone(),
                };
                let mut report = postupdate::build_report(&inputs);
                report.log =
                    postupdate::log_text(section.as_deref(), &output, unrecorded.as_deref());
                // The snapshot is looked for over the whole run, the
                // password prompt included.
                let snapshot = if pacman {
                    postupdate::snapshot_since(started_wall, crate::util::now())
                } else {
                    None
                };
                (before, after, report, snapshot)
            });
            let Ok((before, after, report, snapshot)) = checked.await else {
                return;
            };
            let Some(win) = w.upgrade() else { return };
            {
                let mut r = run.borrow_mut();
                let argv = r.command.clone();
                r.record.commands.push(CommandRun {
                    argv,
                    exit,
                    secs: status.secs(),
                });
                r.record.snapshot = snapshot;
                r.before = before.clone();
                r.report = report;
            }
            // Rows verified updated leave the list now; the rescan confirms.
            win.mark_updated(&postupdate::installed_since(&before, &after));
            let r = run.clone();
            win.update_rest_then(rest, Box::new(move |win, res| win.batch_done(r, res)));
        });
    }

    /// Everything the terminal does not reach: planned off the UI thread,
    /// asked about once, then run as one task that keeps going past a
    /// failure so one broken download does not strand the rest. `then` is
    /// told what happened, each row judged by its own probe.
    pub fn update_rest_then(&self, rest: Vec<App>, then: BatchDone) {
        if rest.is_empty() {
            return then(self, BatchResult::default());
        }
        let have = self.shared.borrow().plugins.clone();
        self.say(&tr("Checking what else needs updating…"), 5);
        let w = self.downgrade();
        spawn_bg(
            move || plan(rest, std::sync::Arc::unwrap_or_clone(have)),
            move |batch| {
                let Some(win) = w.upgrade() else { return };
                match batch {
                    Ok(batch) => win.confirm_batch(batch, then),
                    Err(e) => {
                        raised(&e);
                        then(&win, BatchResult::default());
                    }
                }
            },
        );
    }

    fn confirm_batch(&self, batch: Batch, then: BatchDone) {
        let held = batch.held.clone();
        if batch.steps.is_empty() {
            return then(
                self,
                BatchResult {
                    held,
                    ..Default::default()
                },
            );
        }
        let mut detail = batch.lines.join("\n");
        if !held.is_empty() {
            detail += &format!("\n\nHeld back\n{}", held.join("\n"));
        }
        let n = batch.lines.len();
        let Batch {
            steps, lines, apps, ..
        } = batch;
        let then = RefCell::new(Some(then));
        let w = self.downgrade();
        tasks::confirm_then(
            &self.window,
            &format!("Update {n} more?"),
            "These are not covered by the system update. Each goes through its own updater; \
             one that fails does not stop the others.",
            &detail,
            &tr("Update"),
            false,
            move |yes| {
                let (Some(win), Some(then)) = (w.upgrade(), then.borrow_mut().take()) else {
                    return;
                };
                if yes {
                    win.run_batch(
                        apps.clone(),
                        steps.clone(),
                        lines.clone(),
                        held.clone(),
                        then,
                    );
                } else {
                    then(
                        &win,
                        BatchResult {
                            held: held.clone(),
                            ..Default::default()
                        },
                    );
                }
            },
        );
    }

    /// The batch, with each row's probe asked before and after.
    fn run_batch(
        &self,
        apps: Vec<App>,
        steps: Vec<Step>,
        lines: Vec<String>,
        held: Vec<String>,
        then: BatchDone,
    ) {
        let probes: Vec<Option<verify::TryProbe>> = apps.iter().map(verify::for_app_try).collect();
        let first = probes.clone();
        let w = self.downgrade();
        spawn_bg(
            move || ask_probes(&first),
            move |before| {
                let Some(win) = w.upgrade() else { return };
                let w = win.downgrade();
                tasks::run_each(
                    &win.window,
                    "Updating everything else",
                    steps,
                    Some(Box::new(move |ok| {
                        spawn_bg(
                            move || judge_batch(apps, &probes, &before),
                            move |outcomes| {
                                if let Some(win) = w.upgrade() {
                                    then(
                                        &win,
                                        BatchResult {
                                            outcomes,
                                            held,
                                            lines,
                                            ran: true,
                                            ok,
                                        },
                                    );
                                }
                            },
                        );
                    })),
                );
            },
        );
    }

    /// The batch is over (or there was none): folded into the run's report,
    /// which is then shown.
    fn batch_done(&self, run: SharedRun, res: BatchResult) {
        if !res.ran && !run.borrow().terminal {
            // Nothing ran at all: say what was held back, as before.
            if !res.held.is_empty() {
                self.say(&format!("Held back:\n{}", res.held.join("\n")), 10);
            }
            return;
        }
        let verified: Vec<String> = res
            .outcomes
            .iter()
            .filter(|(_, o)| o.changed == Some(true))
            .map(|(a, _)| a.name.clone())
            .collect();
        {
            let mut r = run.borrow_mut();
            let named: Vec<(String, Outcome)> = res
                .outcomes
                .iter()
                .map(|(a, o)| (a.name.clone(), o.clone()))
                .collect();
            r.report.add_items(&named, &res.held);
            for line in &res.lines {
                // Per item, a batch only knows whether all of it went fine;
                // each item's own verdict is its check.
                let argv = line.split_whitespace().collect::<Vec<_>>().join(" ");
                r.record.commands.push(CommandRun {
                    argv,
                    exit: (res.ok).then_some(0),
                    secs: None,
                });
            }
            for (a, o) in &res.outcomes {
                if o.changed == Some(true) {
                    r.record.changes.push(crate::history::Change {
                        name: a.name.clone(),
                        from: a.version.clone(),
                        to: a.update.clone(),
                    });
                }
            }
        }
        self.mark_updated(&verified);
        if res.ran {
            self.after_pacman(res.ok);
        }
        self.surface_update(run);
    }
}

// ---- the system half in the window ---------------------------------------------

/// What a run in the window starts from, kept to start it again (after the
/// stuck update is stopped, or to Resolve).
#[derive(Clone)]
struct Job {
    argv: Vec<String>,
    /// the timeline's rows: (name, new version, from the AUR or mise)
    apps: Vec<(String, String, bool)>,
    rest: Vec<App>,
    /// where this update's lines in pacman.log start
    offset: Option<u64>,
    /// the run's own log (every attempt appends to it)
    log: PathBuf,
}

/// `omarchy update` on the window's terminal, from its start to `Finished`.
/// Owned by the timer that pumps it; everything the view is handed holds
/// it weakly.
struct Live {
    win: WeakWin,
    view: UpdateView,
    run: SharedRun,
    job: Job,
    /// None once it ended (or was taken to be stopped)
    update: RefCell<Option<UpdateRun>>,
    /// the log file, read on from where this attempt began, for Details;
    /// with the bytes of a character cut in two at the end of the last read
    reader: RefCell<Option<(std::fs::File, Vec<u8>)>>,
    /// where this attempt's output starts in the log
    log_from: u64,
    started: Instant,
    /// Almanac stopped the update itself, and why
    stopped: Cell<Option<Stopped>>,
    /// Stop sent Ctrl-C: pressed again, it hangs up
    interrupted: Cell<bool>,
}

thread_local! {
    /// The run in the window while its terminal is open, for Quit.
    static LIVE: RefCell<Option<Weak<Live>>> = const { RefCell::new(None) };
    /// Its timeline, from its start until its report is up or it stopped.
    static SHOWN: RefCell<Option<Timeline>> = const { RefCell::new(None) };
    /// The Other apps under way in the window, for Quit to stop.
    static BATCH: RefCell<Option<tasks::QuietTask>> = const { RefCell::new(None) };
    /// Stop and quit chose to quit once the Other apps have stopped.
    static QUIT_PENDING: Cell<bool> = const { Cell::new(false) };
    /// The run whose view is in the window, until its history entry is
    /// written: Quit writes it then.
    static IN_WINDOW: RefCell<Option<Weak<RefCell<Run>>>> = const { RefCell::new(None) };
    /// Held while the window is hidden with an update running in it.
    static BACKGROUND: RefCell<Option<gio::ApplicationHoldGuard>> = const { RefCell::new(None) };
}

/// `run`'s entry is written: Quit has nothing left to write for it.
pub(in crate::ui) fn forget_window_run(run: &SharedRun) {
    IN_WINDOW.with(|r| {
        let mut r = r.borrow_mut();
        if r.as_ref().is_some_and(|w| w.as_ptr() == Rc::as_ptr(run)) {
            *r = None;
        }
    });
}

/// The update was stopped before it ended (Quit): its command goes into
/// the entry as not known to have finished, and the entry says so.
fn record_stopped(run: &SharedRun, secs: Option<f64>) {
    let mut r = run.borrow_mut();
    let argv = r.command.clone();
    r.record.commands.push(CommandRun {
        argv,
        exit: None,
        secs,
    });
    r.record.stopped = true;
}

/// An update runs in the window: closing the window hides it instead, and
/// Quit asks first.
pub fn update_running() -> bool {
    SHOWN.with(|s| s.borrow().is_some())
}

fn set_shown(t: Option<Timeline>) {
    SHOWN.with(|s| *s.borrow_mut() = t);
}

/// The file from `from` on, as text.
fn read_from(path: &Path, from: u64) -> String {
    let Ok(mut f) = std::fs::File::open(path) else {
        return String::new();
    };
    let mut buf = Vec::new();
    if f.seek(SeekFrom::Start(from)).is_ok() {
        let _ = f.read_to_end(&mut buf);
    }
    String::from_utf8_lossy(&buf).into_owned()
}

impl Live {
    /// One beat of the 150 ms timer.
    fn tick(self: &Rc<Live>) -> glib::ControlFlow {
        let events = match self.update.borrow_mut().as_mut() {
            Some(u) => u.poll(),
            None => return glib::ControlFlow::Break,
        };
        self.pull_log();
        for e in events {
            match e {
                Event::Changed => self.render(),
                Event::NeedsPassword { prompt, attempt } => {
                    self.raise();
                    let me = Rc::downgrade(self);
                    self.view.ask_password(&prompt, attempt, move |secret| {
                        if let Some(l) = me.upgrade() {
                            l.password(secret);
                        }
                    });
                }
                Event::Question(p) => {
                    self.raise();
                    let (a, b) = (Rc::downgrade(self), Rc::downgrade(self));
                    self.view.ask(
                        &p,
                        move |answer| {
                            if let Some(l) = a.upgrade() {
                                l.reply(&answer);
                            }
                        },
                        move || {
                            if let Some(l) = b.upgrade() {
                                l.stop();
                            }
                        },
                    );
                }
                Event::PromptGone => self.view.close_prompt(),
                Event::Finished { code } => {
                    self.finished(code);
                    return glib::ControlFlow::Break;
                }
            }
        }
        // Quiet for a minute on something with no card: say so, and where
        // to look; Stop is beside it.
        let waiting = self
            .update
            .borrow()
            .as_ref()
            .filter(|u| u.seems_waiting(Instant::now()))
            .map(|u| u.last_line());
        self.view.set_waiting(waiting.as_deref());
        glib::ControlFlow::Continue
    }

    fn render(&self) {
        let t = self.update.borrow().as_ref().map(|u| u.timeline().clone());
        if let Some(t) = t {
            self.view.render(&t);
            set_shown(Some(t));
        }
    }

    /// A question while the window is hidden brings it back with the question.
    fn raise(&self) {
        if let Some(win) = self.win.upgrade() {
            if !win.window.is_visible() {
                win.window.present();
            }
        }
    }

    /// New output in the log, into Details.
    fn pull_log(&self) {
        let mut reader = self.reader.borrow_mut();
        let Some((file, carry)) = reader.as_mut() else {
            return;
        };
        if file.read_to_end(carry).unwrap_or(0) == 0 && carry.is_empty() {
            return;
        }
        let valid = match std::str::from_utf8(carry) {
            Ok(_) => carry.len(),
            Err(e) if e.error_len().is_none() => e.valid_up_to(),
            Err(_) => carry.len(),
        };
        let rest = carry.split_off(valid);
        let text = String::from_utf8_lossy(carry).into_owned();
        *carry = rest;
        if !text.is_empty() {
            self.view.append_log(&text);
        }
    }

    /// The password (or Cancel) to sudo. A request no longer on screen
    /// takes nothing: the card goes. A password the terminal would show is
    /// not sent, and the update is stopped so it ends as a failure that
    /// says why, rather than sudo waiting on with no card.
    fn password(&self, secret: Option<String>) {
        let mut secret = secret;
        let cancel = secret.is_none();
        let mut slot = self.update.borrow_mut();
        let r = slot.as_mut().map(|u| u.answer_password(secret.as_deref()));
        if let Some(s) = secret.as_mut() {
            // SAFETY: zero bytes are valid UTF-8.
            unsafe { s.as_bytes_mut() }.fill(0);
        }
        match r {
            Some(Ok(())) => {
                // Only a Ctrl-C that went out makes this a cancelled run.
                if cancel {
                    self.stopped.set(Some(Stopped::Cancelled));
                }
            }
            Some(Err(e)) => {
                eprintln!("almanac: {e}");
                // Still asked for: refused, not stale. Stop it.
                if let Some(u) = slot.as_mut().filter(|u| u.asks_password()) {
                    match u.interrupt() {
                        Ok(()) => self.stopped.set(Some(stopped_after(&e))),
                        Err(e) => eprintln!("almanac: {e}"),
                    }
                }
                self.view.close_prompt();
            }
            None => self.view.close_prompt(),
        }
    }

    fn reply(&self, a: &crate::inwindow::prompt::Answer) {
        let r = self.update.borrow_mut().as_mut().map(|u| u.answer(a));
        if !matches!(r, Some(Ok(()))) {
            self.view.close_prompt();
        }
    }

    /// Stop the update (the card's Stop button, or Stop under the
    /// timeline): Ctrl-C on its terminal.
    fn stop(&self) {
        self.stop_now();
    }

    /// Ctrl-C on the update's terminal; a second time, the terminal is hung
    /// up on (an update that ignores Ctrl-C still ends). Whatever card was
    /// up is no longer answered.
    fn stop_now(&self) {
        self.view.close_prompt();
        let mut slot = self.update.borrow_mut();
        let Some(u) = slot.as_mut() else { return };
        self.stopped.set(Some(Stopped::ByUser));
        if self.interrupted.get() {
            u.hang_up();
            return;
        }
        match u.interrupt() {
            Ok(()) => {
                self.interrupted.set(true);
                self.view.stop_sent();
            }
            Err(e) => eprintln!("almanac: {e}"),
        }
    }

    /// `omarchy update` ended. Its terminal is already released, so the run
    /// is let go of here at once.
    fn finished(&self, code: i32) {
        let t = self
            .update
            .borrow_mut()
            .take()
            .map(|u| u.timeline().clone());
        self.pull_log();
        self.view.set_waiting(None);
        LIVE.with(|l| *l.borrow_mut() = None);
        let Some(mut t) = t else { return };
        if let Some(why) = self.stopped.get() {
            mark_stopped(&mut t, why);
        }
        {
            let mut r = self.run.borrow_mut();
            let argv = r.command.clone();
            r.record.commands.push(CommandRun {
                argv,
                exit: Some(code),
                secs: Some(self.started.elapsed().as_secs_f64()),
            });
        }
        if let Some(win) = self.win.upgrade() {
            win.system_half_over(
                self.view.clone(),
                self.run.clone(),
                self.job.clone(),
                self.log_from,
                t,
                code,
            );
        }
    }
}

impl Win {
    /// The system half in the window: `omarchy update -y` on a hidden
    /// terminal, shown as a step timeline in the update view, then the
    /// checks, the Other apps, and the report in the same view. When it
    /// cannot start there, today's terminal runs it instead.
    fn start_in_window(
        &self,
        command: String,
        run: SharedRun,
        apps: Vec<(String, String, bool)>,
        rest: Vec<App>,
    ) {
        let files = match postupdate::run_dir().and_then(|d| postupdate::new_run_files_in(&d)) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("almanac: {e}");
                return self.run_system_update(&command, true, rest, run);
            }
        };
        let w = self.downgrade();
        glib::spawn_future_local(async move {
            // How things were, asked before the update starts (see
            // `run_system_update`).
            let before = gio::spawn_blocking(|| postupdate::probe_before(true))
                .await
                .unwrap_or_default();
            let Some(win) = w.upgrade() else {
                files.remove();
                return;
            };
            let job = Job {
                argv: update_argv(),
                apps,
                rest,
                offset: postupdate::log_offset(Path::new(postupdate::PACMAN_LOG)),
                log: files.output.clone(),
            };
            {
                let mut r = run.borrow_mut();
                r.before = before;
                r.files = Some(files.clone());
                r.command = job.argv.join(" ");
                r.terminal = true;
                r.in_window = true;
            }
            let view = UpdateView::new(&win);
            if let Err(e) = win.drive(&view, &run, job.clone(), false) {
                eprintln!("almanac: the update could not start in the window: {e}");
                files.remove();
                {
                    let mut r = run.borrow_mut();
                    r.files = None;
                    r.terminal = false;
                    r.in_window = false;
                }
                return win.run_system_update(&command, true, job.rest, run);
            }
            run.borrow_mut().view = Some(view.clone());
            IN_WINDOW.with(|r| *r.borrow_mut() = Some(Rc::downgrade(&run)));
            win.own_view(&view, run);
            view.present();
        });
    }

    /// The view's dialog holds the run from now on. It cannot be closed
    /// while the update runs (that would leave it with nowhere to ask its
    /// questions); once it can, closing it writes the history entry and
    /// lets Update all start again.
    fn own_view(&self, view: &UpdateView, run: SharedRun) {
        let d = view.dialog();
        // libadwaita turns the window's close into this dialog's close
        // attempt (its close-request handler runs before Almanac's): while
        // the update runs, either one sends the window to the background.
        let w = self.downgrade();
        d.connect_close_attempt(move |_| {
            if let Some(win) = w.upgrade() {
                if update_running() {
                    let _ = win.on_close();
                    win.to_background();
                }
            }
        });
        let w = self.downgrade();
        // Taken on close: the run holds the view, which holds this handler.
        let slot = RefCell::new(Some(run));
        d.connect_closed(move |_| {
            let Some(run) = slot.borrow_mut().take() else {
                return;
            };
            run.borrow_mut().view = None;
            let retry = run.borrow().then_retry;
            if let Some(win) = w.upgrade() {
                win.finish_update(run);
                if retry {
                    win.on_update_all();
                }
            }
        });
    }

    /// Start (or start again) `omarchy update` on a terminal of its own, fed
    /// to `view` every 150 ms.
    fn drive(
        &self,
        view: &UpdateView,
        run: &SharedRun,
        job: Job,
        resolve: bool,
    ) -> Result<(), String> {
        let log_from = std::fs::metadata(&job.log).map(|m| m.len()).unwrap_or(0);
        let t = Timeline::new(&job.apps, !job.rest.is_empty());
        let update = if resolve {
            UpdateRun::resolve(job.argv.clone(), Vec::new(), t, job.log.clone())?
        } else {
            UpdateRun::start(job.argv.clone(), Vec::new(), t, job.log.clone())?
        };
        let reader = std::fs::File::open(&job.log).ok().and_then(|mut f| {
            f.seek(SeekFrom::Start(log_from)).ok()?;
            Some((f, Vec::new()))
        });
        let first = update.timeline().clone();
        // A banner about the attempt before ("still running…") is over.
        view.clear_notice();
        view.clear_fix();
        view.close_prompt();
        view.set_log("");
        view.render(&first);
        view.dialog().set_can_close(false);
        set_shown(Some(first));
        let live = Rc::new(Live {
            win: self.downgrade(),
            view: view.clone(),
            run: run.clone(),
            job,
            update: RefCell::new(Some(update)),
            reader: RefCell::new(reader),
            log_from,
            started: Instant::now(),
            stopped: Cell::new(None),
            interrupted: Cell::new(false),
        });
        LIVE.with(|l| *l.borrow_mut() = Some(Rc::downgrade(&live)));
        let w = self.downgrade();
        view.set_on_stop(move || {
            if let Some(win) = w.upgrade() {
                win.stop_clicked();
            }
        });
        glib::timeout_add_local(Duration::from_millis(150), move || live.tick());
        Ok(())
    }

    /// `omarchy update` is over. A failure that changed nothing waits on the
    /// timeline with its fix; anything else is checked, then the Other apps
    /// run, then the report takes the view.
    fn system_half_over(
        &self,
        view: UpdateView,
        run: SharedRun,
        job: Job,
        log_from: u64,
        t: Timeline,
        code: i32,
    ) {
        // "Quitting now would stop pacman part-way" no longer holds.
        view.clear_notice();
        view.render(&t);
        let fix = failed_fix(&t);
        run.borrow_mut().retry_offered = fix == Some(Fix::RetryUpdate);
        if pauses(&t) {
            match fix {
                Some(Fix::StopStuckUpdate(_)) => self.offer_busy_fix(&run, job, Some(t.clone())),
                Some(fix) => self.offer_fix(&view, &run, job, fix),
                None => {}
            }
            view.dialog().set_can_close(true);
            self.in_window_over(
                &tr("Update needs a look"),
                &tr("Open Almanac to see the report."),
            );
            return;
        }
        set_shown(Some(t.clone()));
        // What the timeline knew goes into the report that replaces it.
        let restart = t.restart_note.clone();
        let warnings: Vec<(String, String)> = t
            .steps
            .iter()
            .filter_map(|(id, _)| t.warning(*id).map(|w| (step_label(*id), w)))
            .collect();
        let (before, started_wall) = {
            let r = run.borrow();
            (r.before.clone(), r.started_wall)
        };
        let (offset, log) = (job.offset, job.log.clone());
        let w = self.downgrade();
        spawn_bg(
            move || {
                let after = postupdate::probe_after(true);
                let section = offset
                    .and_then(|o| postupdate::read_log_since(Path::new(postupdate::PACMAN_LOG), o));
                let output = read_from(&log, log_from);
                let scan = section.as_deref().map(|t| {
                    let mut scan = postupdate::scan_log(t);
                    postupdate::scan_output(&output, &mut scan);
                    scan
                });
                let inputs = Inputs {
                    system: true,
                    exit: Some(code),
                    log: scan,
                    before: before.clone(),
                    after: after.clone(),
                };
                let mut report = postupdate::build_report(&inputs);
                report.add_notes(restart.as_deref(), &warnings);
                report.log = postupdate::log_text_as(
                    section.as_deref(),
                    &output,
                    None,
                    postupdate::WINDOW_OUTPUT,
                );
                let snapshot = postupdate::snapshot_since(started_wall, crate::util::now());
                (before, after, report, snapshot)
            },
            move |(before, after, report, snapshot)| {
                let Some(win) = w.upgrade() else { return };
                {
                    let mut r = run.borrow_mut();
                    r.record.snapshot = snapshot;
                    r.before = before.clone();
                    r.report = report;
                }
                win.mark_updated(&postupdate::installed_since(&before, &after));
                win.after_pacman(true);
                win.other_apps_in_window(view, run, job.rest, t);
            },
        );
    }

    /// The rest of Update all, as the timeline's last step. Update all was
    /// the go-ahead: no "Update N more?" and no task dialog; each app is a
    /// row, its output goes to Details, and it is judged by its own check.
    fn other_apps_in_window(
        &self,
        view: UpdateView,
        run: SharedRun,
        rest: Vec<App>,
        mut t: Timeline,
    ) {
        if rest.is_empty() {
            return self.batch_done(run, BatchResult::default());
        }
        let rows: Vec<(String, String)> = rest
            .iter()
            .map(|a| (a.name.clone(), a.update.clone()))
            .collect();
        other_apps_start(&mut t, &rows);
        view.render(&t);
        set_shown(Some(t.clone()));
        let have = self.shared.borrow().plugins.clone();
        let w = self.downgrade();
        spawn_bg(
            move || {
                let mut batch = plan(rest, std::sync::Arc::unwrap_or_clone(have))?;
                batch.steps = rest_steps_from(
                    std::mem::take(&mut batch.steps),
                    std::env::var_os("ALMANAC_UPDATE_REST_COMMAND"),
                );
                let probes: Vec<Option<verify::TryProbe>> =
                    batch.apps.iter().map(verify::for_app_try).collect();
                let before = ask_probes(&probes);
                Ok::<_, String>((batch, probes, before))
            },
            move |planned| {
                let Some(win) = w.upgrade() else { return };
                match planned {
                    Ok((batch, probes, before)) => {
                        win.run_other_apps(view, run, t, batch, probes, before)
                    }
                    Err(e) => {
                        other_apps_done(&mut t, &[], false, false);
                        view.render(&t);
                        let held = vec![format!(
                            "{} — {}",
                            tr("Other apps"),
                            crate::util::human_error(&e)
                        )];
                        win.batch_done(
                            run,
                            BatchResult {
                                held,
                                ..Default::default()
                            },
                        );
                    }
                }
            },
        );
    }

    fn run_other_apps(
        &self,
        view: UpdateView,
        run: SharedRun,
        mut t: Timeline,
        batch: Batch,
        probes: Vec<Option<verify::TryProbe>>,
        before: Vec<Option<ProbeValue>>,
    ) {
        let Batch {
            steps,
            lines,
            held,
            apps,
            owners,
        } = batch;
        // Held back, or nothing to do: those rows leave, the report says why.
        t.apps
            .retain(|a| a.step != StepId::OtherApps || apps.iter().any(|b| b.name == a.name));
        if steps.is_empty() {
            other_apps_done(&mut t, &[], false, false);
            view.render(&t);
            return self.batch_done(
                run,
                BatchResult {
                    held,
                    ..Default::default()
                },
            );
        }
        view.render(&t);
        set_shown(Some(t.clone()));
        let t = Rc::new(RefCell::new(t));
        let labels: Vec<String> = steps.iter().map(|s| s.label.clone()).collect();
        let names: Vec<String> = apps.iter().map(|a| a.name.clone()).collect();
        let on_step = {
            let (t, view) = (t.clone(), view.clone());
            Box::new(move |i: usize| {
                let mut t = t.borrow_mut();
                for &row in owners.get(i).into_iter().flatten() {
                    if let Some(name) = names.get(row) {
                        other_apps_working(&mut t, name);
                    }
                }
                view.render(&t);
                set_shown(Some(t.clone()));
                if let Some(label) = labels.get(i) {
                    view.append_log(&format!("\n— {label} —\n"));
                }
            })
        };
        let on_line = {
            let view = view.clone();
            Box::new(move |line: &str| view.append_log(&format!("{line}\n")))
        };
        let w = self.downgrade();
        let on_done: tasks::OnDone = {
            let view = view.clone();
            Box::new(move |ok| {
                BATCH.with(|b| b.borrow_mut().take());
                if QUIT_PENDING.get() {
                    if let Some(win) = w.upgrade() {
                        // What the Other apps ran, not known to have finished.
                        {
                            let mut r = run.borrow_mut();
                            for line in &lines {
                                let argv = line.split_whitespace().collect::<Vec<_>>().join(" ");
                                r.record.commands.push(CommandRun {
                                    argv,
                                    exit: None,
                                    secs: None,
                                });
                            }
                            r.record.stopped = true;
                        }
                        win.quit_now();
                    }
                    return;
                }
                spawn_bg(
                    move || judge_batch(apps, &probes, &before),
                    move |outcomes| {
                        let Some(win) = w.upgrade() else { return };
                        let named: Vec<(String, Outcome)> = outcomes
                            .iter()
                            .map(|(a, o)| (a.name.clone(), o.clone()))
                            .collect();
                        let mut t = t.borrow_mut();
                        other_apps_done(&mut t, &named, true, ok);
                        view.render(&t);
                        set_shown(Some(t.clone()));
                        drop(t);
                        win.batch_done(
                            run,
                            BatchResult {
                                outcomes,
                                held,
                                lines,
                                ran: true,
                                ok,
                            },
                        );
                    },
                );
            })
        };
        let task = self.run_quiet_task(steps, on_step, on_line, on_done);
        BATCH.with(|b| *b.borrow_mut() = task);
    }

    /// The fix on the failed step's card. Its button holds the run weakly:
    /// the view's dialog holds the run, and the view holds the button.
    fn offer_fix(&self, view: &UpdateView, run: &SharedRun, job: Job, fix: Fix) {
        let (w, r) = (self.downgrade(), Rc::downgrade(run));
        let chosen = fix.clone();
        view.show_failure_fix(&fix, move || {
            let (Some(win), Some(run)) = (w.upgrade(), r.upgrade()) else {
                return;
            };
            let Some(view) = run.borrow().view.clone() else {
                return;
            };
            match &chosen {
                Fix::StopStuckUpdate(_) => win.stop_stuck_then_restart(&view, &run, job.clone()),
                Fix::Resolve => win.restart_in_window(&view, &run, job.clone(), true),
                Fix::RetryUpdate => {
                    // Update all from the start, preflight and all, once
                    // this run's entry is written.
                    run.borrow_mut().then_retry = true;
                    view.dialog().close();
                }
            }
        });
    }

    /// "Another Omarchy update is running": who holds the lock decides the
    /// fix (`busy_fix`). One that someone may be answering in its terminal
    /// is not offered to be stopped; the card says to finish it there.
    fn offer_busy_fix(&self, run: &SharedRun, job: Job, t: Option<Timeline>) {
        let (w, r) = (self.downgrade(), Rc::downgrade(run));
        spawn_bg(
            || {
                let proc_root = Path::new("/proc");
                let holders =
                    postupdate::lock_holders_in(proc_root, &postupdate::omarchy_update_lock());
                let pacman = !postupdate::lockers_in(proc_root).is_empty();
                (holders, pacman)
            },
            move |(holders, pacman)| {
                let (Some(win), Some(run)) = (w.upgrade(), r.upgrade()) else {
                    return;
                };
                let Some(view) = run.borrow().view.clone() else {
                    return;
                };
                let (reason, fix) = busy_fix(&holders, pacman);
                if let (Some(reason), Some(mut t)) = (reason, t) {
                    for (_, s) in &mut t.steps {
                        if let StepState::Failed { reason: r, .. } = s {
                            *r = reason.clone();
                        }
                    }
                    view.render(&t);
                }
                if fix == Fix::RetryUpdate {
                    run.borrow_mut().retry_offered = true;
                }
                win.offer_fix(&view, &run, job, fix);
            },
        );
    }

    /// Stop it: the processes holding Omarchy's update lock now (the
    /// output cannot say which they are), stopped as the preflight's fix
    /// stops them, then the update again.
    fn stop_stuck_then_restart(&self, view: &UpdateView, run: &SharedRun, job: Job) {
        view.clear_fix();
        let (w, r) = (self.downgrade(), Rc::downgrade(run));
        spawn_bg(
            || {
                postupdate::lock_holders_in(Path::new("/proc"), &postupdate::omarchy_update_lock())
                    .iter()
                    .map(|h| h.pid)
                    .collect::<Vec<u32>>()
            },
            move |pids| {
                let (Some(win), Some(run)) = (w.upgrade(), r.upgrade()) else {
                    return;
                };
                let Some(view) = run.borrow().view.clone() else {
                    return;
                };
                win.stop_pids_then_restart(&view, &run, job, pids);
            },
        );
    }

    fn stop_pids_then_restart(&self, view: &UpdateView, run: &SharedRun, job: Job, pids: Vec<u32>) {
        if pids.is_empty() {
            // It ended meanwhile: nothing to stop.
            return self.restart_in_window(view, run, job, false);
        }
        let steps = postupdate::repair_steps(&[Repair::StopUpdate(pids)], false);
        let (w, r) = (self.downgrade(), Rc::downgrade(run));
        self.run_task(
            &tr("Stopping the stuck update"),
            steps,
            Some(Box::new(move |ok| {
                let (Some(win), Some(run)) = (w.upgrade(), r.upgrade()) else {
                    return;
                };
                let Some(view) = run.borrow().view.clone() else {
                    return;
                };
                if ok {
                    win.restart_in_window(&view, &run, job, false);
                } else {
                    view.notice(&tr(
                        "The earlier update is still running, so this one did not start",
                    ));
                    win.offer_busy_fix(&run, job, view.timeline());
                }
            })),
        );
    }

    fn restart_in_window(&self, view: &UpdateView, run: &SharedRun, job: Job, resolve: bool) {
        if let Err(e) = self.drive(view, run, job, resolve) {
            view.notice(&format!("{}: {e}", tr("Could not start the update")));
        }
    }

    /// The run in the window has stopped, or its report is up: the window
    /// may close again, and a hidden one is told how it ended.
    pub(in crate::ui) fn in_window_over(&self, note: &str, body: &str) {
        set_shown(None);
        if !self.window.is_visible() || !self.window.is_active() {
            if let Some(app) = self.window.application() {
                let n = gio::Notification::new(note);
                n.set_body(Some(body));
                app.send_notification(Some("update-finished"), &n);
            }
        }
        BACKGROUND.with(|b| b.borrow_mut().take());
    }

    /// The window's close button while an update runs in it: the window
    /// goes, Almanac stays until the update is over.
    pub fn to_background(&self) {
        self.window.set_visible(false);
        if let Some(app) = self.window.application() {
            BACKGROUND.with(|b| {
                let mut b = b.borrow_mut();
                if b.is_none() {
                    *b = Some(app.hold());
                }
            });
        }
    }

    /// Quit (Ctrl+Q). With an update in the window: refused while pacman
    /// works, else asked about first, since it stops the update.
    pub fn on_quit(&self) {
        if QUIT_PENDING.get() {
            // Asked twice: the step under way is not waited for.
            return self.quit_now();
        }
        let decision = SHOWN
            .with(|s| s.borrow().as_ref().map(quit_decision))
            .unwrap_or(QuitDecision::Allow);
        match decision {
            QuitDecision::Allow => self.quit_now(),
            QuitDecision::Refuse => {
                self.window.present();
                let msg = tr("Quitting now would stop pacman part-way.");
                match LIVE.with(|l| l.borrow().as_ref().and_then(Weak::upgrade)) {
                    Some(live) => live.view.notice(&msg),
                    // No update view to show it in.
                    None => self.say(&msg, 8),
                }
            }
            QuitDecision::Confirm => {
                self.window.present();
                let dialog = adw::AlertDialog::builder()
                    .heading(tr("Stop the update and quit?"))
                    .body(tr(
                        "The update is still running. Quitting stops it where it is.",
                    ))
                    .build();
                dialog.add_response("cancel", &tr("Keep updating"));
                dialog.add_response("quit", &tr("Stop and quit"));
                dialog.set_response_appearance("quit", adw::ResponseAppearance::Destructive);
                dialog.set_default_response(Some("cancel"));
                dialog.set_close_response("cancel");
                let w = self.downgrade();
                dialog.connect_response(None, move |_, resp| {
                    if resp != "quit" {
                        return;
                    }
                    if let Some(win) = w.upgrade() {
                        win.stop_then_quit();
                    }
                });
                dialog.present(Some(&self.window));
            }
        }
    }

    /// Stop under the timeline. While pacman works it asks first (stopping
    /// it then can leave packages half installed), and still stops. In the
    /// Other apps, the step under way stops and the rest are skipped.
    fn stop_clicked(&self) {
        let Some(live) = LIVE.with(|l| l.borrow().as_ref().and_then(Weak::upgrade)) else {
            BATCH.with(|b| {
                if let Some(t) = b.borrow().as_ref().filter(|t| !t.finished()) {
                    t.cancel();
                }
            });
            return;
        };
        let t = live.update.borrow().as_ref().map(|u| u.timeline().clone());
        match t.as_ref().map(stop_decision) {
            Some(StopDecision::Now) => live.stop_now(),
            Some(StopDecision::Confirm) => {
                let dialog = adw::AlertDialog::builder()
                    .heading(tr("Stop the update?"))
                    .body(tr("Quitting now would stop pacman part-way."))
                    .build();
                dialog.add_response("cancel", &tr("Keep updating"));
                dialog.add_response("stop", &tr("Stop the update"));
                dialog.set_response_appearance("stop", adw::ResponseAppearance::Destructive);
                dialog.set_default_response(Some("cancel"));
                dialog.set_close_response("cancel");
                let l = Rc::downgrade(&live);
                dialog.connect_response(None, move |_, resp| {
                    if resp == "stop" {
                        if let Some(live) = l.upgrade() {
                            live.stop_now();
                        }
                    }
                });
                dialog.present(Some(&self.window));
            }
            None => {}
        }
    }

    fn quit_now(&self) {
        // The run in the window gets its history entry whichever way it
        // ends: this is the last chance.
        if let Some(run) = IN_WINDOW.with(|r| r.borrow().as_ref().and_then(Weak::upgrade)) {
            let live = LIVE.with(|l| l.borrow().as_ref().and_then(Weak::upgrade));
            if let Some(live) = live.filter(|l| l.update.borrow().is_some()) {
                record_stopped(&run, Some(live.started.elapsed().as_secs_f64()));
            }
            self.finish_update(run);
        }
        let _ = self.on_close();
        BACKGROUND.with(|b| b.borrow_mut().take());
        if let Some(app) = self.window.application() {
            app.quit();
        }
    }

    /// Ctrl-C to the update, then its terminal let go of off the main
    /// thread (that waits for it to leave), then quit.
    fn stop_then_quit(&self) {
        // The Other apps: the step under way is stopped and the rest
        // skipped; Almanac quits once it has (see `run_other_apps`).
        let stopping = BATCH.with(|b| {
            b.borrow()
                .as_ref()
                .filter(|t| !t.finished())
                .map(|t| t.cancel())
                .is_some()
        });
        if stopping {
            QUIT_PENDING.set(true);
            return;
        }
        let live = LIVE.with(|l| l.borrow().as_ref().and_then(Weak::upgrade));
        let update = live.as_ref().and_then(|l| l.update.borrow_mut().take());
        let Some(mut update) = update else {
            return self.quit_now();
        };
        if let Some(l) = &live {
            record_stopped(&l.run, Some(l.started.elapsed().as_secs_f64()));
        }
        let _ = update.interrupt();
        let w = self.downgrade();
        spawn_bg(
            move || drop(update),
            move |()| {
                if let Some(win) = w.upgrade() {
                    win.quit_now();
                }
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn update_all_installs_a_newer_github_release_of_almanac() {
        let info = selfupdate::Info {
            kind: selfupdate::PACKAGE.into(),
            version: "2.2.0".into(),
            latest: "2.2.1".into(),
            origin: Some(selfupdate::PackageOrigin::Foreign { aur: None }),
            ..Default::default()
        };
        let (line, steps) = almanac_update(&info).expect("not planned");
        assert!(line.contains("2.2.0 → 2.2.1"), "{line}");
        assert_eq!(steps.len(), 2, "download and check, then install");
        let current = selfupdate::Info {
            latest: String::new(),
            ..info
        };
        assert!(almanac_update(&current).is_none());
    }

    fn app(source: &str, extra: Value) -> App {
        App {
            name: "x".into(),
            version: "1.0".into(),
            source: source.into(),
            update: "1.1".into(),
            extra: extra.as_object().cloned().unwrap_or_default().into(),
            ..Default::default()
        }
    }

    #[test]
    fn each_row_goes_to_the_updater_its_own_button_uses() {
        assert_eq!(route(&app(inventory::REPO, json!({}))), Route::System);
        assert_eq!(route(&app(inventory::AUR, json!({}))), Route::System);
        assert_eq!(route(&app(inventory::TOOL, json!({}))), Route::System);
        assert_eq!(route(&app(inventory::FLATPAK, json!({}))), Route::Flatpak);
        assert_eq!(
            route(&app(inventory::LOCAL, json!({"plugin_id": "p"}))),
            Route::Plugin
        );
        assert_eq!(
            route(&app(inventory::LOCAL, json!({"repo": "/r"}))),
            Route::Git
        );
        assert_eq!(route(&app(inventory::APPIMAGE, json!({}))), Route::AppImage);
        assert_eq!(
            route(&app(inventory::AGENT, json!({"update_cmd": ["a", "up"]}))),
            Route::Agent
        );
        assert_eq!(
            route(&app(inventory::LOCAL, json!({"self": true}))),
            Route::Almanac
        );
    }

    /// Only plugins waiting: no terminal, so no password prompt.
    #[test]
    fn the_terminal_runs_only_for_what_it_covers() {
        let c = "omarchy update";
        assert_eq!(system_command(&[Route::Plugin, Route::AppImage], c), None);
        assert_eq!(
            system_command(&[Route::System, Route::Plugin], c).as_deref(),
            Some(c)
        );
        assert_eq!(
            system_command(&[Route::Flatpak], c).as_deref(),
            Some("/usr/bin/flatpak update -y")
        );
        assert_eq!(
            system_command(&[Route::Flatpak, Route::System], c).as_deref(),
            Some("omarchy update; rc=$?; /usr/bin/flatpak update -y || exit; exit $rc")
        );
        assert_eq!(system_command(&[], c), None);
    }

    /// Both halves in one shell: the system update's failure is the exit
    /// status even when flatpak's update went fine, and flatpak's own
    /// failure shows when the system update did not fail.
    #[test]
    fn the_combined_command_keeps_the_first_failure() {
        let status = |cmd: &str| {
            std::process::Command::new("/usr/bin/bash")
                .arg("-c")
                .arg(cmd)
                .status()
                .unwrap()
                .code()
        };
        let both = |sys: &str, flat: &str| {
            system_command(&[Route::System, Route::Flatpak], sys)
                .unwrap()
                .replace("/usr/bin/flatpak update -y", flat)
        };
        assert_eq!(status(&both("false", "true")), Some(1));
        assert_eq!(status(&both("true", "exit 3")), Some(3));
        assert_eq!(status(&both("true", "true")), Some(0));
    }

    /// The source of the rows that stayed listed: a batch where one item
    /// failed never reloaded, so the items that did update stayed under
    /// Updates until a manual rescan.
    #[test]
    fn an_update_attempt_always_reads_the_list_again() {
        let code = include_str!("actions/link.rs");
        let body = code
            .split("pub fn after_pacman(")
            .nth(1)
            .and_then(|s| s.split("\n    }").next())
            .unwrap_or("");
        assert!(
            body.contains("self.reload()"),
            "the split found after_pacman"
        );
        assert!(
            !body.contains("if ok"),
            "a failed item must not keep the others listed"
        );
        assert!(
            body.contains("forget_pending()"),
            "and the pending updates are asked again"
        );
    }

    /// The "before" picture was taken after the terminal had started, so a
    /// service the update broke counted as already broken and the checks
    /// after it passed.
    #[test]
    fn how_things_were_is_asked_before_the_update_starts() {
        let code = include_str!("update_all.rs");
        let body = code
            .split("fn run_system_update(")
            .nth(1)
            .and_then(|s| s.split("\n    }\n").next())
            .unwrap_or("");
        let before = body
            .find("probe_before(")
            .expect("the split found run_system_update");
        let start = body.find("popen_new_session(").unwrap();
        assert!(before < start);
        assert!(
            body[before..start].contains(".await"),
            "and it has answered before the start"
        );
    }

    /// One Update all at a time: a second start is refused until the first
    /// lets go, however it ends.
    #[test]
    fn a_second_update_all_waits_for_the_first() {
        let first = Busy::take().expect("free at first");
        assert!(Busy::held());
        assert!(Busy::take().is_none());
        drop(first);
        assert!(!Busy::held());
        let again = Busy::take();
        assert!(again.is_some());
    }

    /// ALMANAC_DEMO: Update all says so and stops, before any terminal,
    /// probe or updater.
    #[test]
    fn demo_mode_updates_nothing() {
        let code = include_str!("update_all.rs");
        let body = code
            .split("pub fn on_update_all(")
            .nth(1)
            .and_then(|s| s.split("\n    }\n").next())
            .unwrap_or("");
        let demo = body
            .find("crate::demo::active()")
            .expect("demo mode is checked");
        assert!(
            body[demo..].find("return;").unwrap() < body[demo..].find("update_all_now").unwrap()
        );
    }

    /// In the window, flatpak is not tacked onto `omarchy update`: it is one
    /// of the Other apps, run by Almanac after the system half.
    #[test]
    fn system_command_leaves_flatpak_out_in_window() {
        let c = "omarchy update";
        assert_eq!(
            system_command_for(&[Route::Flatpak, Route::System], c, true).as_deref(),
            Some(c)
        );
        assert_eq!(system_command_for(&[Route::Flatpak], c, true), None);
        assert_eq!(
            system_command_for(&[Route::Flatpak, Route::System], c, false).as_deref(),
            Some("omarchy update; rc=$?; /usr/bin/flatpak update -y || exit; exit $rc")
        );
        // And so the flatpak rows go with the rest of the batch.
        assert!(in_rest(Route::Flatpak, true));
        assert!(!in_rest(Route::Flatpak, false));
        assert!(!in_rest(Route::System, true));
        assert!(in_rest(Route::Plugin, false));
    }

    /// The tasks the window's run starts (stopping a stuck update) go
    /// through the demo gate, like every other action.
    #[test]
    fn the_window_run_starts_tasks_through_the_demo_gate() {
        let code = include_str!("update_all.rs");
        let section = code
            .split("// ---- the system half in the window")
            .nth(1)
            .and_then(|s| s.split("#[cfg(test)]").next())
            .expect("the section is there");
        assert!(section.contains("self.run_task("));
        assert!(!section.contains("tasks::run("));
        assert!(!section.contains("tasks::run_each("));
        assert!(!section.contains("tasks::run_quiet("));
    }

    /// While the update dialog is up a toast is behind it: what the window
    /// run has to say goes to the dialog's banner.
    #[test]
    fn the_window_run_speaks_in_its_dialog() {
        let code = include_str!("update_all.rs");
        let section = code
            .split("// ---- the system half in the window")
            .nth(1)
            .and_then(|s| s.split("#[cfg(test)]").next())
            .expect("the section is there");
        let says: Vec<&str> = section
            .lines()
            .filter(|l| l.contains(".say("))
            .map(str::trim)
            .collect();
        assert_eq!(
            says,
            vec!["None => self.say(&msg, 8),"],
            "only with no view open"
        );
        assert!(section.matches(".notice(").count() >= 3);
    }

    #[test]
    fn in_window_is_chosen_only_for_omarchy() {
        assert!(use_in_window("omarchy update", false));
        assert!(!use_in_window("omarchy update", true), "never in a demo");
        assert!(!use_in_window("yay -Syu", false));
        assert!(!use_in_window("sudo pacman -Syu", false));
        assert!(!use_in_window("paru -Syu", false));
    }

    /// `ALMANAC_UPDATE_COMMAND` (debug builds): a script run in place of
    /// `omarchy update -y`, for trying the view without updating.
    #[test]
    fn the_updater_is_omarchy_unless_a_debug_build_is_told_otherwise() {
        assert_eq!(update_argv_from(None), vec!["omarchy", "update", "-y"]);
        assert_eq!(
            update_argv_from(Some("".into())),
            vec!["omarchy", "update", "-y"]
        );
        let fake = update_argv_from(Some("/x/fake.sh".into()));
        if cfg!(debug_assertions) {
            assert_eq!(fake, vec!["/x/fake.sh"]);
        } else {
            assert_eq!(fake, vec!["omarchy", "update", "-y"]);
        }
    }

    #[test]
    fn the_other_apps_run_their_own_steps_unless_a_debug_build_is_told_otherwise() {
        let steps = || {
            vec![Step::argv(
                "Updating tuck",
                vec!["omarchy".into(), "plugin".into()],
            )]
        };
        let argv = |s: &[Step]| match &s[0].action {
            tasks::StepAction::Argv(a) => a.clone(),
            _ => panic!("argv"),
        };
        assert_eq!(
            argv(&rest_steps_from(steps(), None)),
            vec!["omarchy", "plugin"]
        );
        let fake = rest_steps_from(steps(), Some("/x/rest.sh".into()));
        assert_eq!(fake[0].label, "Updating tuck");
        if cfg!(debug_assertions) {
            assert_eq!(argv(&fake), vec!["/x/rest.sh", "Updating tuck"]);
        } else {
            assert_eq!(argv(&fake), vec!["omarchy", "plugin"]);
        }
    }

    fn timeline() -> Timeline {
        Timeline::new(&[("cursor-bin".into(), "3.1".into(), false)], true)
    }

    fn set(t: &mut Timeline, id: StepId, s: StepState) {
        let at = t.steps.iter().position(|(i, _)| *i == id).unwrap();
        t.steps[at].1 = s;
    }

    fn running() -> StepState {
        StepState::Running {
            detail: String::new(),
        }
    }

    fn done() -> StepState {
        StepState::Done {
            detail: String::new(),
        }
    }

    #[test]
    fn quit_is_refused_during_system_packages() {
        let mut t = timeline();
        set(&mut t, StepId::Snapshot, done());
        set(&mut t, StepId::SigningKeys, done());
        set(&mut t, StepId::SystemPackages, running());
        assert_eq!(quit_decision(&t), QuitDecision::Refuse);
        set(&mut t, StepId::SystemPackages, done());
        set(&mut t, StepId::Migrations, running());
        assert_eq!(quit_decision(&t), QuitDecision::Confirm);
        // Not yet started counts as under way: the run is still to come.
        assert_eq!(quit_decision(&timeline()), QuitDecision::Confirm);
        for (id, _) in t.steps.clone() {
            set(&mut t, id, done());
        }
        assert_eq!(quit_decision(&t), QuitDecision::Allow);
    }

    /// Stop is always there while the update runs. While pacman works it
    /// asks first, saying what stopping then means; it still stops.
    #[test]
    fn stop_asks_first_only_while_pacman_works() {
        let mut t = timeline();
        assert_eq!(stop_decision(&t), StopDecision::Now);
        set(&mut t, StepId::SystemPackages, running());
        assert_eq!(stop_decision(&t), StopDecision::Confirm);
        set(&mut t, StepId::SystemPackages, done());
        set(&mut t, StepId::Migrations, running());
        assert_eq!(stop_decision(&t), StopDecision::Now);
        let code = include_str!("update_all.rs");
        let body = code
            .split("fn stop_clicked(")
            .nth(1)
            .and_then(|s| s.split("\n    }\n").next())
            .expect("stop_clicked");
        assert!(body.contains("tr(\"Quitting now would stop pacman part-way.\")"));
        let drive = code.split("fn drive(").nth(1).unwrap();
        assert!(drive
            .split("\n    }\n")
            .next()
            .unwrap()
            .contains("set_on_stop("));
    }

    #[test]
    fn a_stopped_update_says_who_stopped_it() {
        let mut t = timeline();
        set(&mut t, StepId::Snapshot, done());
        set(
            &mut t,
            StepId::SigningKeys,
            StepState::Failed {
                reason: "It stopped with an error (exit status 130).".into(),
                changed: Changed::Unknown,
                fix: Some(Fix::RetryUpdate),
            },
        );
        mark_stopped(&mut t, Stopped::ByUser);
        let StepState::Failed {
            reason,
            changed,
            fix,
        } = &t.steps[1].1
        else {
            panic!("still failed")
        };
        assert_eq!(reason, "You stopped the update.");
        assert_eq!(*changed, Changed::Unknown, "a step had finished");
        assert_eq!(*fix, Some(Fix::RetryUpdate));
    }

    /// "Stop it and update" only for an update nobody can be answering: one
    /// with a terminal (someone may be at it) or with pacman at work is
    /// left to finish, in the preflight's words.
    #[test]
    fn a_live_update_elsewhere_is_not_offered_to_be_stopped() {
        use crate::postupdate::Holder;
        let h = |pid, terminal| Holder {
            pid,
            name: "omarchy-update".into(),
            terminal,
        };
        let (reason, fix) = busy_fix(&[h(41, true)], false);
        let reason = reason.expect("reworded");
        assert!(reason.contains("Finish it in its terminal"), "{reason}");
        assert!(reason.contains("omarchy-update (41)"), "{reason}");
        assert_eq!(fix, Fix::RetryUpdate);
        let (reason, fix) = busy_fix(&[h(41, false)], true);
        assert!(reason.is_some());
        assert_eq!(fix, Fix::RetryUpdate);
        assert_eq!(
            busy_fix(&[h(41, false), h(42, false)], false),
            (None, Fix::StopStuckUpdate(vec![41, 42]))
        );
        // Gone meanwhile: nothing to stop, the update just starts again.
        assert_eq!(busy_fix(&[], false), (None, Fix::StopStuckUpdate(vec![])));
    }

    #[test]
    fn a_new_attempt_clears_the_earlier_notice() {
        let code = include_str!("update_all.rs");
        let drive = code.split("fn drive(").nth(1).unwrap();
        assert!(drive
            .split("\n    }\n")
            .next()
            .unwrap()
            .contains("view.clear_notice();"));
    }

    /// A failure that changed nothing stays on the timeline with its fix
    /// (there is nothing for the checks to look at); anything else goes on
    /// to the checks and the report.
    #[test]
    fn only_a_failure_that_changed_nothing_waits_on_the_timeline() {
        let mut t = timeline();
        assert!(!pauses(&t));
        set(
            &mut t,
            StepId::Snapshot,
            StepState::Failed {
                reason: "Another Omarchy update is running.".into(),
                changed: Changed::Nothing,
                fix: Some(Fix::StopStuckUpdate(vec![])),
            },
        );
        assert!(pauses(&t));
        set(
            &mut t,
            StepId::Snapshot,
            StepState::Failed {
                reason: "x".into(),
                changed: Changed::Unknown,
                fix: Some(Fix::RetryUpdate),
            },
        );
        assert!(!pauses(&t));
    }

    /// A password cancelled is said as that, not as "exit status 1".
    #[test]
    fn a_cancelled_password_says_nothing_changed() {
        let mut t = timeline();
        set(
            &mut t,
            StepId::Snapshot,
            StepState::Failed {
                reason: "It stopped with an error (exit status 1).".into(),
                changed: Changed::Unknown,
                fix: Some(Fix::RetryUpdate),
            },
        );
        mark_stopped(&mut t, Stopped::Cancelled);
        let StepState::Failed { changed, fix, .. } = &t.steps[0].1 else {
            panic!("still failed")
        };
        assert_eq!(*changed, Changed::Nothing);
        assert_eq!(*fix, Some(Fix::RetryUpdate));
        assert!(pauses(&t));
        // A run that went through is left as it is.
        let mut ok = timeline();
        mark_stopped(&mut ok, Stopped::Cancelled);
        assert!(!pauses(&ok));
    }

    /// The terminal would have shown the password, so Almanac did not send
    /// it and stopped the update: a visible failure with Try again, never a
    /// card that went away while sudo still waits.
    #[test]
    fn a_password_the_terminal_would_show_stops_the_run_visibly() {
        let mut t = timeline();
        set(
            &mut t,
            StepId::Snapshot,
            StepState::Failed {
                reason: "It stopped with an error (exit status 130).".into(),
                changed: Changed::Unknown,
                fix: Some(Fix::RetryUpdate),
            },
        );
        mark_stopped(&mut t, Stopped::PasswordRefused);
        let StepState::Failed {
            reason,
            changed,
            fix,
        } = &t.steps[0].1
        else {
            panic!("still failed")
        };
        assert!(
            reason.contains("would have shown your password"),
            "{reason}"
        );
        assert_eq!(*changed, Changed::Nothing);
        assert_eq!(*fix, Some(Fix::RetryUpdate));
        assert!(pauses(&t));
    }

    /// Only an echoing terminal is blamed on echo: a password that could
    /// not be sent for another reason (the terminal could not be read)
    /// says so, and still ends as a visible failure with Try again.
    #[test]
    fn a_password_not_sent_for_another_reason_does_not_blame_echo() {
        use crate::inwindow::run::PasswordError;
        assert_eq!(
            stopped_after(&PasswordError::WouldEcho),
            Stopped::PasswordRefused
        );
        assert_eq!(
            stopped_after(&PasswordError::Terminal("tcgetattr: EIO".into())),
            Stopped::PasswordUnsent
        );
        let mut t = timeline();
        set(
            &mut t,
            StepId::Snapshot,
            StepState::Failed {
                reason: "It stopped with an error (exit status 130).".into(),
                changed: Changed::Unknown,
                fix: Some(Fix::RetryUpdate),
            },
        );
        mark_stopped(&mut t, Stopped::PasswordUnsent);
        let StepState::Failed { reason, fix, .. } = &t.steps[0].1 else {
            panic!("still failed")
        };
        assert!(!reason.contains("shown your password"), "{reason}");
        assert!(reason.contains("not sent"), "{reason}");
        assert_eq!(*fix, Some(Fix::RetryUpdate));
    }

    /// In the window the rest of Update all is the Other apps step: no
    /// "Update N more?" confirm, no task dialog of its own.
    #[test]
    fn other_apps_run_in_the_timeline_without_dialogs() {
        let code = include_str!("update_all.rs");
        let section = code
            .split("// ---- the system half in the window")
            .nth(1)
            .and_then(|s| s.split("#[cfg(test)]").next())
            .expect("the section is there");
        assert!(!section.contains("update_rest_then("));
        assert!(!section.contains("confirm_batch("));
        assert!(!section.contains("run_batch("));
        assert!(section.contains("self.run_quiet_task("));
    }

    #[test]
    fn an_other_app_row_runs_while_its_steps_do() {
        let mut t = timeline();
        other_apps_start(
            &mut t,
            &[("tuck".into(), "1.2".into()), ("zed".into(), "9".into())],
        );
        other_apps_working(&mut t, "tuck");
        let row =
            |t: &Timeline, n: &str| t.apps.iter().find(|a| a.name == n).map(|a| a.state.clone());
        assert_eq!(row(&t, "tuck"), Some(AppState::Installing));
        assert_eq!(row(&t, "zed"), Some(AppState::Waiting));
        // Quit while the Other apps run asks first.
        assert_eq!(quit_decision(&t), QuitDecision::Confirm);
    }

    /// Each batch step knows which row it updates (a checkout pulls in
    /// several steps; Almanac itself in two).
    #[test]
    fn every_batch_step_has_its_row() {
        let mut b = Batch::default();
        plan_agent(
            &app(
                inventory::AGENT,
                json!({"update_cmd": ["claude", "update"]}),
            ),
            &mut b,
        )
        .unwrap();
        assert_eq!(b.owners.len(), b.steps.len());
        assert_eq!(b.owners, vec![vec![0]]);
    }

    #[test]
    fn other_apps_rows_end_as_their_checks_say() {
        let mut t = timeline();
        other_apps_start(
            &mut t,
            &[("tuck".into(), "1.2".into()), ("zed".into(), "9".into())],
        );
        let at = t
            .steps
            .iter()
            .position(|(i, _)| *i == StepId::OtherApps)
            .unwrap();
        assert!(matches!(t.steps[at].1, StepState::Running { .. }));
        assert_eq!(
            t.apps
                .iter()
                .filter(|a| a.step == StepId::OtherApps)
                .count(),
            2
        );
        other_apps_done(
            &mut t,
            &[("tuck".into(), Outcome::new(Some(true), "tuck: 1.2"))],
            true,
            true,
        );
        let row = |n: &str| t.apps.iter().find(|a| a.name == n).map(|a| a.state.clone());
        assert_eq!(
            row("tuck"),
            Some(AppState::Done {
                version: "1.2".into()
            })
        );
        // zed did not run (held back): its row goes, it is in the report.
        assert_eq!(row("zed"), None);
        assert!(matches!(t.steps[at].1, StepState::Done { .. }));

        let mut t = timeline();
        other_apps_start(&mut t, &[("tuck".into(), "1.2".into())]);
        other_apps_done(
            &mut t,
            &[("tuck".into(), Outcome::new(Some(false), "tuck: still 1.1"))],
            true,
            false,
        );
        let at = t
            .steps
            .iter()
            .position(|(i, _)| *i == StepId::OtherApps)
            .unwrap();
        let StepState::Failed { reason, .. } = &t.steps[at].1 else {
            panic!("failed")
        };
        assert_eq!(reason, "tuck: still 1.1", "the row's own reason");
        assert_eq!(
            t.apps.last().map(|a| a.state.clone()),
            Some(AppState::Failed)
        );

        // Nothing ran (declined, or nothing to do): the step is not claimed.
        let mut t = timeline();
        other_apps_start(&mut t, &[("tuck".into(), "1.2".into())]);
        other_apps_done(&mut t, &[], false, false);
        assert!(!t.steps.iter().any(|(i, _)| *i == StepId::OtherApps));
        assert!(!t.apps.iter().any(|a| a.step == StepId::OtherApps));
    }

    #[test]
    fn the_notification_says_how_it_ended() {
        let mut r = postupdate::Report::default();
        r.changes.push(crate::history::Change {
            name: "a".into(),
            from: "1".into(),
            to: "2".into(),
        });
        assert_eq!(finished_note(&r), "Update finished — 1 updated");
        r.add_items(&[("b".into(), Outcome::new(Some(false), "did not"))], &[]);
        assert_eq!(finished_note(&r), "Update needs a look");
    }

    /// A hidden window's notification says a restart is needed, not only
    /// that the update finished.
    #[test]
    fn the_notification_body_carries_the_restart() {
        let mut r = postupdate::Report::default();
        assert_eq!(finished_body(&r), "Open Almanac to see the report.");
        r.add_notes(Some("A restart is needed."), &[]);
        assert_eq!(
            finished_body(&r),
            "Restart to finish. Open Almanac to see the report."
        );
    }

    /// Every way a run in the window ends writes its history entry: closing
    /// the view, Quit (also while the update or the Other apps still ran).
    #[test]
    fn every_end_of_a_run_in_the_window_is_recorded() {
        let code = include_str!("update_all.rs");
        let body = |f: &str| {
            code.split(f)
                .nth(1)
                .and_then(|s| s.split("\n    }\n").next())
                .unwrap_or_else(|| panic!("{f}"))
                .to_string()
        };
        assert!(body("fn quit_now(").contains("finish_update("));
        let stq = body("fn stop_then_quit(");
        assert!(
            stq.contains("record_stopped("),
            "the stopped update is in the entry"
        );
        let rec = body("fn record_stopped(");
        assert!(rec.contains("exit: None") && rec.contains("stopped = true"));
        let other = code.split("if QUIT_PENDING.get() {").nth(1).unwrap();
        assert!(other
            .split("return;")
            .next()
            .unwrap()
            .contains("exit: None"));
        let report = include_str!("update_report.rs");
        let fin = report.split("fn finish_update(").nth(1).unwrap();
        assert!(
            fin.split("\n    }\n").next().unwrap().contains("recorded"),
            "written once"
        );
    }

    #[test]
    fn a_major_agent_release_is_held_back() {
        let mut b = Batch::default();
        let mut a = app(
            inventory::AGENT,
            json!({"update_cmd": ["claude", "update"]}),
        );
        plan_agent(&a, &mut b).unwrap();
        assert_eq!(b.steps.len(), 1);
        a.update = "2.0".into();
        plan_agent(&a, &mut b).unwrap();
        assert_eq!(b.steps.len(), 1);
        assert_eq!(
            b.held,
            vec!["x — 2.0 is a major release; update it on its own"]
        );
        plan_agent(&app(inventory::AGENT, json!({})), &mut b).unwrap();
        assert_eq!(b.held[1], "x — has no self-update command");
    }
}
