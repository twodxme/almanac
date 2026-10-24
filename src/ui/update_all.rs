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

use std::sync::Arc;
use std::time::{Duration, Instant};

use gtk::{gio, glib};
use serde_json::Value;

use crate::i18n::tr;
use crate::inventory::{self, App};
use crate::tasks::{self, LogFn, Step};
use crate::ui::actions::{appimage_update_work, popen_new_session, raised, truthy, update_all_script, update_cmd};
use crate::ui::plugins::{plan_plugin_updates, plugin_step, plugin_update_states};
use crate::ui::shell::{spawn_bg, Win};
use crate::verify::{self, Probe};
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

/// The terminal half: `None` when nothing waiting needs it.
pub fn system_command(routes: &[Route], command: &str) -> Option<String> {
    let system = routes.contains(&Route::System);
    let flatpak = routes.contains(&Route::Flatpak);
    match (system, flatpak) {
        (false, false) => None,
        (true, false) => Some(command.to_string()),
        (false, true) => Some("flatpak update -y".to_string()),
        (true, true) => Some(format!("{command}; flatpak update -y")),
    }
}

/// What follows the terminal closing.
#[derive(Debug, PartialEq)]
pub enum AfterTerminal {
    /// it ran to the end: the rest of the batch next
    Rest,
    /// nothing else is waiting
    Done,
    /// it returned at once, so the update may still be running
    Say(&'static str),
}

/// A launcher that hands the window to a terminal server (ghostty, kitty's
/// single instance, footclient) returns at once, so there is no "finished"
/// to wait for. Starting the batch then would run it beside the system
/// update -- both upgrading the same mise tool, say.
pub fn after_terminal(elapsed: Duration, rest_waiting: bool) -> AfterTerminal {
    match (elapsed < Duration::from_secs(5), rest_waiting) {
        (true, true) => AfterTerminal::Say("When the system update finishes, press Update all again for the rest"),
        (true, false) => AfterTerminal::Say("Rescan when the update finishes"),
        (false, true) => AfterTerminal::Rest,
        (false, false) => AfterTerminal::Done,
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
    /// root. The list rescans itself when the terminal closes. Everything
    /// else follows as one batch, so nothing counted is left behind.
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
        let Some(command) = system_command(&routes, &xdg::system_update_command()) else {
            self.update_rest(rest);
            return;
        };
        self.run_system_update(&command, routes.contains(&Route::System), rest);
    }

    fn run_system_update(&self, command: &str, judge: bool, rest: Vec<App>) {
        let script = update_all_script(command);
        let Some(argv) = xdg::terminal_argv(&["bash", "-lc", script.as_str()]) else {
            self.say(&tr("No terminal found to run the update in"), 5);
            return;
        };
        if argv.is_empty() {
            self.say(&tr("No terminal found to run the update in"), 5);
            return;
        }
        let started = Instant::now();
        let proc = match popen_new_session(&argv) {
            Ok(p) => p,
            Err(e) => {
                self.say(&format!("Could not start the update: {e}"), 5);
                return;
            }
        };
        self.say(&tr("Update running in a terminal — the list refreshes when it closes"), 5);
        let probe: Probe = verify::pending_updates();

        // The `wait` thread, as a main-loop future: a GSubprocess is waited
        // on there, and the probes still run off the UI thread.
        let w = self.downgrade();
        glib::spawn_future_local(async move {
            // Asked while the user types their password, which is longer
            // than checkupdates takes.
            let p = probe.clone();
            let Ok(before) = gio::spawn_blocking(move || p()).await else { return };
            let _ = proc.wait_future().await;
            let next = after_terminal(started.elapsed(), !rest.is_empty());
            if let Some(win) = w.upgrade() {
                win.after_pacman(true);
                match next {
                    AfterTerminal::Rest => win.update_rest(rest),
                    AfterTerminal::Say(msg) => {
                        win.say(&tr(msg), 10);
                        return;
                    }
                    AfterTerminal::Done => {}
                }
            }
            if !judge {
                return;
            }
            let judged = gio::spawn_blocking(move || verify::try_judge_pending(before.as_ref(), probe().as_ref()));
            let Ok(out) = judged.await else { return };
            // judge_pending raising ends the Python's `wait` thread there.
            let out = match out {
                Ok(out) => out,
                Err(e) => return raised(&e),
            };
            if let Some(win) = w.upgrade() {
                win.report_outcome(&out);
            }
        });
    }

    /// Everything the terminal does not reach: planned off the UI thread,
    /// asked about once, then run as one task that keeps going past a
    /// failure so one broken download does not strand the rest.
    pub fn update_rest(&self, rest: Vec<App>) {
        if rest.is_empty() {
            return;
        }
        let have = self.shared.borrow().plugins.clone();
        self.say(&tr("Checking what else needs updating…"), 5);
        let w = self.downgrade();
        spawn_bg(
            move || plan(rest, std::sync::Arc::unwrap_or_clone(have)),
            move |batch| {
                let Some(win) = w.upgrade() else { return };
                match batch {
                    Ok(batch) => win.confirm_batch(batch),
                    Err(e) => raised(&e),
                }
            },
        );
    }

    fn confirm_batch(&self, batch: Batch) {
        let held = batch.held.join("\n");
        if batch.steps.is_empty() {
            if !held.is_empty() {
                self.say(&format!("Held back:\n{held}"), 10);
            }
            return;
        }
        let mut detail = batch.lines.join("\n");
        if !held.is_empty() {
            detail += &format!("\n\nHeld back\n{held}");
        }
        let n = batch.lines.len();
        let steps = batch.steps;
        let w = self.downgrade();
        tasks::confirm(
            &self.window,
            &format!("Update {n} more?"),
            "These are not covered by the system update. Each goes through its own updater; \
             one that fails does not stop the others.",
            &detail,
            &tr("Update"),
            false,
            move || {
                let Some(win) = w.upgrade() else { return };
                let w2 = win.downgrade();
                tasks::run_each(
                    &win.window,
                    "Updating everything else",
                    steps.clone(),
                    Some(Box::new(move |ok| {
                        if let Some(win) = w2.upgrade() {
                            win.say(
                                &if ok { tr("Everything updated") } else { tr("Some updates failed — see the log") },
                                8,
                            );
                            win.after_pacman(ok);
                        }
                    })),
                );
            },
        );
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
            Some("omarchy update; flatpak update -y")
        );
        assert_eq!(system_command(&[], c), None);
    }

    /// A terminal that returns at once may still be updating: the batch
    /// must not start beside it.
    #[test]
    fn the_batch_waits_for_the_terminal_to_finish() {
        let (quick, slow) = (Duration::from_secs(1), Duration::from_secs(90));
        assert!(matches!(after_terminal(quick, true), AfterTerminal::Say(m) if m.contains("Update all again")));
        assert_eq!(after_terminal(quick, false), AfterTerminal::Say("Rescan when the update finishes"));
        assert_eq!(after_terminal(slow, true), AfterTerminal::Rest);
        assert_eq!(after_terminal(slow, false), AfterTerminal::Done);
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
