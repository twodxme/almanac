//! "Update all": every row with an update waiting, each through the tool
//! that owns it.
//!
//! `omarchy update` covers pacman, the AUR and mise tools, and nothing else.
//! Plugins, AppImages, agent CLIs and local checkouts each have their own
//! updater; leaving them out meant the button's count promised updates it
//! never ran, and those rows stayed listed after every "successful" update.
//!
//! The system update runs first, in a terminal (it needs a password), then
//! everything else as one confirmed batch. What needs a person's decision --
//! a major agent release, a checkout with local edits, a plugin outside the
//! update scope -- is held back and named, and keeps its own button.
//!
//! Before it starts, what would make it fail is looked at (free space,
//! another package manager, the battery); after it, whether it went through
//! (`postupdate`), with a repair offered for what can be repaired.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::{gio, glib};
use serde_json::Value;

use crate::history::CommandRun;
use crate::i18n::tr;
use crate::inventory::{self, App};
use crate::postupdate::{self, Inputs, Issue, Level, Wait};
use crate::tasks::{self, LogFn, Step};
use crate::ui::actions::{appimage_update_work, popen_new_session, raised, truthy, update_cmd};
use crate::ui::plugins::{plan_plugin_updates, plugin_step, plugin_update_states};
use crate::ui::shell::{spawn_bg, Win};
use crate::ui::update_report::{BatchDone, BatchResult, Run, SharedRun};
use crate::verify::{self, Outcome, ProbeValue};
use crate::{agents, patches, plugins, selfupdate, xdg};

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
        if extra("plugin_id") { Route::Plugin } else { Route::Git }
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

/// The terminal half: `None` when nothing waiting needs it. With both, the
/// exit status is the system update's when it failed: a flatpak update
/// that went fine must not hide it.
pub fn system_command(routes: &[Route], command: &str) -> Option<String> {
    let system = routes.contains(&Route::System);
    let flatpak = routes.contains(&Route::Flatpak);
    match (system, flatpak) {
        (false, false) => None,
        (true, false) => Some(command.to_string()),
        (false, true) => Some("flatpak update -y".to_string()),
        (true, true) => Some(format!("{command}; rc=$?; flatpak update -y || exit; exit $rc")),
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
}

impl Batch {
    fn hold(&mut self, name: &str, why: &str) {
        self.held.push(format!("{name} — {why}"));
    }
}

/// An agent CLI: its own update command, unless the release is a major one
/// (shown with its notes, asked about on its own).
pub fn plan_agent(app: &App, batch: &mut Batch) -> Result<(), String> {
    let cmd = update_cmd(app)?;
    if cmd.is_empty() {
        batch.hold(&app.name, "has no self-update command");
    } else if agents::major_jump(&app.version, &app.update) {
        batch.hold(&app.name, &format!("{} is a major release; update it on its own", app.update));
    } else {
        batch.lines.push(format!("{:22} {} → {}   $ {}", app.name, app.version, app.update, cmd.join(" ")));
        batch.steps.push(Step::argv(format!("Updating {}", app.name), cmd));
        batch.apps.push(app.clone());
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
            batch.lines.push(format!("{:22} omarchy plugin update", s.id));
            batch.steps.push(Step::call(format!("Updating {}", s.id), plugin_step(s.id.clone(), plan.pins.get(&s.id).cloned())));
            let row = by(Route::Plugin).find(|a| crate::ui::shell::as_text(a.extra.get("plugin_id")) == s.id);
            batch.apps.extend(row.cloned());
        }
        for (s, why) in plan.held.iter().zip(&plan.held_why) {
            batch.hold(&s.id, &format!("outside the plugin update scope set in Preferences ({why})"));
        }
        // Rows counted as updates that are not clean fast-forwards now.
        for a in by(Route::Plugin) {
            let pid = crate::ui::shell::as_text(a.extra.get("plugin_id"));
            let known = plan.ready.iter().chain(&plan.held).any(|s| s.id == pid);
            if !known {
                let detail = crate::ui::shell::as_text(a.extra.get("plugin_detail"));
                batch.hold(&a.name, if detail.is_empty() { "cannot be updated as it is" } else { &detail });
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
            batch.lines.push(format!("{:22} git pull in {repo}", a.name));
            batch.steps.extend(patches::steps(repo, &state, &a.name, None));
            batch.apps.push(a.clone());
        } else {
            batch.hold(&a.name, "has local edits; update it on its own to see what happens to them");
        }
    }

    for a in by(Route::AppImage) {
        if !a.extra.get("download").is_some_and(truthy) {
            batch.hold(&a.name, "no download URL");
            continue;
        }
        batch.lines.push(format!("{:22} {} → {}   download", a.name, a.version, a.update));
        let app = a.clone();
        batch.steps.push(Step::call(
            format!("Downloading {} {}", a.name, a.update),
            Arc::new(move |_log: LogFn<'_>| appimage_update_work(&app)),
        ));
        batch.apps.push(a.clone());
    }

    for a in by(Route::Agent) {
        plan_agent(a, &mut batch)?;
    }

    if let Some(a) = by(Route::Almanac).next() {
        let info = selfupdate::try_check(true)?;
        if info.can_update {
            batch.lines.push(format!("{:22} git pull --ff-only", a.name));
            for (label, argv) in selfupdate::update_steps() {
                batch.steps.push(Step::argv(label, argv));
            }
            batch.apps.push(a.clone());
        } else {
            batch.hold(&a.name, if info.note.is_empty() { "cannot update itself here" } else { &info.note });
        }
    }
    Ok(batch)
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
        let pending: Vec<App> = self
            .store_objects()
            .iter()
            .filter_map(crate::ui::shell::AppItem::of)
            .filter(|i| i.has_update)
            .map(|i| i.app.clone())
            .collect();
        let routes: Vec<Route> = pending.iter().map(route).collect();
        let rest: Vec<App> = pending
            .into_iter()
            .filter(|a| !matches!(route(a), Route::System | Route::Flatpak))
            .collect();
        let command = system_command(&routes, &xdg::system_update_command());
        let pacman = routes.contains(&Route::System);
        // What would make it fail part-way is looked at first; /proc and
        // statvfs are read off the UI thread.
        let w = self.downgrade();
        spawn_bg(postupdate::probe_preflight, move |p| {
            let Some(win) = w.upgrade() else { return };
            let issues = postupdate::preflight_issues(&p, pacman);
            win.preflight(
                issues,
                Rc::new(move |win: &Win| win.start_update_all(command.clone(), pacman, rest.clone())),
            );
        });
    }

    /// Say what stands in the way, in the app rather than as a terminal
    /// that fails: a blocking problem has no "Continue anyway".
    fn preflight(&self, issues: Vec<Issue>, go: Rc<dyn Fn(&Win)>) {
        if issues.is_empty() {
            return go(self);
        }
        let blocked = issues.iter().any(|i| i.level == Level::Block);
        let fix = if blocked { None } else { issues.iter().find_map(|i| i.fix.clone()) };
        let dialog = adw::AlertDialog::builder()
            .heading(if blocked { tr("The update cannot start yet") } else { tr("Before updating") })
            .body(if blocked {
                tr("Starting now would fail part-way.")
            } else {
                tr("The update can go ahead, but this may get in its way.")
            })
            .build();
        let list = Win::note_list();
        for i in &issues {
            let icon = if i.level == Level::Block { "dialog-error-symbolic" } else { "dialog-warning-symbolic" };
            list.append(&Win::note_row(icon, &i.title, &i.detail));
        }
        dialog.set_extra_child(Some(&list));
        dialog.add_response("cancel", &tr("Cancel"));
        if !blocked {
            dialog.add_response("continue", &tr("Continue anyway"));
        }
        if fix.is_some() {
            dialog.add_response("fix", &tr("Remove it and continue"));
            dialog.set_response_appearance("fix", adw::ResponseAppearance::Suggested);
        }
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let w = self.downgrade();
        dialog.connect_response(None, move |_d, resp| {
            let Some(win) = w.upgrade() else { return };
            match (resp, &fix) {
                ("continue", _) => go(&win),
                ("fix", Some(f)) => {
                    let steps = postupdate::repair_steps(std::slice::from_ref(f), false);
                    let (w, go) = (win.downgrade(), go.clone());
                    tasks::run(
                        &win.window,
                        &tr("Removing pacman’s lock file"),
                        steps,
                        Some(Box::new(move |ok| {
                            let Some(win) = w.upgrade() else { return };
                            if ok {
                                go(&win);
                            } else {
                                win.say(&tr("The lock file is still there, so the update did not start"), 8);
                            }
                        })),
                    );
                }
                _ => {}
            }
        });
        dialog.present(Some(&self.window));
    }

    fn start_update_all(&self, command: Option<String>, pacman: bool, rest: Vec<App>) {
        let run = Run::new(pacman && command.is_some());
        match command {
            Some(command) => self.run_system_update(&command, pacman, rest, run),
            None => {
                let r = run.clone();
                self.update_rest_then(rest, Box::new(move |win, res| win.batch_done(r, res)));
            }
        }
    }

    fn run_system_update(&self, command: &str, pacman: bool, rest: Vec<App>, run: SharedRun) {
        // Where the script reports back. Without it the update still runs;
        // how it ended is then not known.
        let files = match postupdate::run_dir().and_then(|d| postupdate::new_run_files_in(&d)) {
            Ok(f) => Some(f),
            Err(e) => {
                eprintln!("almanac: {e}");
                None
            }
        };
        let recorder = Path::new("/usr/bin/script");
        let script = postupdate::terminal_script(command, files.as_ref(), recorder.exists().then_some(recorder));
        let Some(argv) = xdg::terminal_argv(&["bash", "-lc", script.as_str()]) else {
            self.say(&tr("No terminal found to run the update in"), 5);
            return;
        };
        if argv.is_empty() {
            self.say(&tr("No terminal found to run the update in"), 5);
            return;
        }
        // Where this update's lines in pacman.log will start.
        let offset = postupdate::log_offset(Path::new(postupdate::PACMAN_LOG));
        let started = Instant::now();
        let proc = match popen_new_session(&argv) {
            Ok(p) => p,
            Err(e) => {
                if let Some(f) = &files {
                    f.remove();
                }
                self.say(&format!("Could not start the update: {e}"), 5);
                return;
            }
        };
        {
            let mut r = run.borrow_mut();
            r.terminal = true;
            r.command = command.to_string();
            r.files = files.clone();
        }
        self.say(&tr("Update running in a terminal — it is checked when it finishes"), 5);

        // The `wait` thread, as a main-loop future: a GSubprocess is waited
        // on there. A single-instance terminal returns at once, so its end
        // is only one of the signs the update is over.
        let exited: Rc<Cell<Option<Duration>>> = Rc::new(Cell::new(None));
        let e = exited.clone();
        glib::spawn_future_local(async move {
            let _ = proc.wait_future().await;
            e.set(Some(started.elapsed()));
        });
        let w = self.downgrade();
        glib::spawn_future_local(async move {
            // Asked while the user types their password, which is longer
            // than checkupdates takes.
            let before = gio::spawn_blocking(move || postupdate::probe_before(pacman));
            let status_file = files.as_ref().map(|f| f.status.clone());
            let status = loop {
                glib::timeout_future(Duration::from_secs(1)).await;
                let exited_at = exited.get();
                let st = match status_file.clone() {
                    Some(p) => gio::spawn_blocking(move || postupdate::read_status(&p)).await.unwrap_or_default(),
                    None => postupdate::RunStatus::default(),
                };
                match postupdate::wait_step(started.elapsed(), exited_at, &st, status_file.is_some()) {
                    Wait::Keep => continue,
                    Wait::Done => break st,
                    Wait::GiveUp(msg) => {
                        if let Some(win) = w.upgrade() {
                            win.say(&tr(msg), 10);
                            win.after_pacman(true);
                            let argv = run.borrow().command.clone();
                            run.borrow_mut().record.commands.push(CommandRun { argv, exit: None, secs: None });
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
            let before = before.await.unwrap_or_default();
            let started_wall = run.borrow().started_wall;
            let exit = status.exit;
            let checked = gio::spawn_blocking(move || {
                let after = postupdate::probe_after(pacman);
                let section = offset.and_then(|o| postupdate::read_log_since(Path::new(postupdate::PACMAN_LOG), o));
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
                let inputs = Inputs { system: pacman, exit, log, before: before.clone(), after: after.clone() };
                let mut report = postupdate::build_report(&inputs);
                report.log = postupdate::log_text(section.as_deref(), &output);
                let snapshot = if pacman { postupdate::snapshot_since(started_wall) } else { None };
                (before, after, report, snapshot)
            });
            let Ok((before, after, report, snapshot)) = checked.await else { return };
            let Some(win) = w.upgrade() else { return };
            {
                let mut r = run.borrow_mut();
                let argv = r.command.clone();
                r.record.commands.push(CommandRun { argv, exit, secs: status.secs() });
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
            return then(self, BatchResult { held, ..Default::default() });
        }
        let mut detail = batch.lines.join("\n");
        if !held.is_empty() {
            detail += &format!("\n\nHeld back\n{}", held.join("\n"));
        }
        let n = batch.lines.len();
        let Batch { steps, lines, apps, .. } = batch;
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
                let (Some(win), Some(then)) = (w.upgrade(), then.borrow_mut().take()) else { return };
                if yes {
                    win.run_batch(apps.clone(), steps.clone(), lines.clone(), held.clone(), then);
                } else {
                    then(&win, BatchResult { held: held.clone(), ..Default::default() });
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
        let ask = |probes: &[Option<verify::TryProbe>]| -> Vec<Option<ProbeValue>> {
            probes.iter().map(|p| p.as_ref().and_then(|p| p().ok().flatten())).collect()
        };
        let first = probes.clone();
        let w = self.downgrade();
        spawn_bg(
            move || ask(&first),
            move |before| {
                let Some(win) = w.upgrade() else { return };
                let w = win.downgrade();
                tasks::run_each(
                    &win.window,
                    "Updating everything else",
                    steps,
                    Some(Box::new(move |ok| {
                        spawn_bg(
                            move || {
                                let after = ask(&probes);
                                let outcomes: Vec<(App, Outcome)> = apps
                                    .into_iter()
                                    .zip(probes.iter().zip(before.iter().zip(after.iter())))
                                    .map(|(app, (p, (b, a)))| {
                                        let out = if p.is_some() {
                                            verify::judge_app(&app, b.as_ref(), a.as_ref())
                                        } else {
                                            Outcome::new(None, format!("{}: the update ran, but its result could not be checked", app.name))
                                        };
                                        (app, out)
                                    })
                                    .collect();
                                outcomes
                            },
                            move |outcomes| {
                                if let Some(win) = w.upgrade() {
                                    then(&win, BatchResult { outcomes, held, lines, ran: true, ok });
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
        let verified: Vec<String> =
            res.outcomes.iter().filter(|(_, o)| o.changed == Some(true)).map(|(a, _)| a.name.clone()).collect();
        {
            let mut r = run.borrow_mut();
            let named: Vec<(String, Outcome)> = res.outcomes.iter().map(|(a, o)| (a.name.clone(), o.clone())).collect();
            r.report.add_items(&named, &res.held);
            for line in &res.lines {
                // Per item, a batch only knows whether all of it went fine;
                // each item's own verdict is its check.
                let argv = line.split_whitespace().collect::<Vec<_>>().join(" ");
                r.record.commands.push(CommandRun { argv, exit: (res.ok).then_some(0), secs: None });
            }
            for (a, o) in &res.outcomes {
                if o.changed == Some(true) {
                    r.record.changes.push(crate::history::Change { name: a.name.clone(), from: a.version.clone(), to: a.update.clone() });
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
        assert_eq!(route(&app(inventory::LOCAL, json!({"plugin_id": "p"}))), Route::Plugin);
        assert_eq!(route(&app(inventory::LOCAL, json!({"repo": "/r"}))), Route::Git);
        assert_eq!(route(&app(inventory::APPIMAGE, json!({}))), Route::AppImage);
        assert_eq!(route(&app(inventory::AGENT, json!({"update_cmd": ["a", "up"]}))), Route::Agent);
        assert_eq!(route(&app(inventory::LOCAL, json!({"self": true}))), Route::Almanac);
    }

    /// Only plugins waiting: no terminal, so no password prompt.
    #[test]
    fn the_terminal_runs_only_for_what_it_covers() {
        let c = "omarchy update";
        assert_eq!(system_command(&[Route::Plugin, Route::AppImage], c), None);
        assert_eq!(system_command(&[Route::System, Route::Plugin], c).as_deref(), Some(c));
        assert_eq!(system_command(&[Route::Flatpak], c).as_deref(), Some("flatpak update -y"));
        assert_eq!(
            system_command(&[Route::Flatpak, Route::System], c).as_deref(),
            Some("omarchy update; rc=$?; flatpak update -y || exit; exit $rc")
        );
        assert_eq!(system_command(&[], c), None);
    }

    /// Both halves in one shell: the system update's failure is the exit
    /// status even when flatpak's update went fine, and flatpak's own
    /// failure shows when the system update did not fail.
    #[test]
    fn the_combined_command_keeps_the_first_failure() {
        let status = |cmd: &str| {
            std::process::Command::new("/usr/bin/bash").arg("-c").arg(cmd).status().unwrap().code()
        };
        let both = |sys: &str, flat: &str| {
            system_command(&[Route::System, Route::Flatpak], sys).unwrap().replace("flatpak update -y", flat)
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
        let body = code.split("pub fn after_pacman(").nth(1).and_then(|s| s.split("\n    }").next()).unwrap_or("");
        assert!(body.contains("self.reload()"), "the split found after_pacman");
        assert!(!body.contains("if ok"), "a failed item must not keep the others listed");
        assert!(body.contains("forget_pending()"), "and the pending updates are asked again");
    }

    #[test]
    fn a_major_agent_release_is_held_back() {
        let mut b = Batch::default();
        let mut a = app(inventory::AGENT, json!({"update_cmd": ["claude", "update"]}));
        plan_agent(&a, &mut b).unwrap();
        assert_eq!(b.steps.len(), 1);
        a.update = "2.0".into();
        plan_agent(&a, &mut b).unwrap();
        assert_eq!(b.steps.len(), 1);
        assert_eq!(b.held, vec!["x — 2.0 is a major release; update it on its own"]);
        plan_agent(&app(inventory::AGENT, json!({})), &mut b).unwrap();
        assert_eq!(b.held[1], "x — has no self-update command");
    }
}
