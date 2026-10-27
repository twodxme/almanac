//! Did it work? Outcomes checked against the machine, not exit codes.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use crate::{history, settings, snapshots, verify};
use crate::history::{ActionLog, Change, CheckResult, CommandRun};
use crate::i18n::tr;
use crate::inventory::App;
use crate::tasks::{OnDone, Step};
use crate::ui::shell::{Win, spawn_bg};
use crate::verify::{Outcome, ProbeValue, TryProbe};

use super::{Judge, JudgeAgain, raised};
use super::pyisms::truthy;
use super::update::with_snapshot;

impl Win {
    /// An on_done that reports what changed, not what a command exited.
    ///
    /// `probe` is asked now and again once the task is over, and `judge`
    /// turns the two answers into what the user is told (see verify.py).
    /// Every silent failure this app has had passed an exit-status check.
    ///
    /// The first asking runs off the main thread (`<agent> --version` took
    /// 4.5 s for one agent, the window frozen meanwhile): `start` is handed
    /// the on_done once it has answered, and starts the action with it, so
    /// the action still begins after the first answer is in.
    ///
    /// Where the first asking raises, as `_checked` itself then does,
    /// `on_err` reports it and the action never starts. A raise on the
    /// second asking ends the worker: nothing is reported.
    pub fn checked(
        &self,
        probe: Option<TryProbe>,
        judge: Judge,
        then: Option<OnDone>,
        on_err: fn(&str),
        start: impl FnOnce(OnDone) + 'static,
    ) {
        self.checked_logged(probe, judge, then, on_err, None, start);
    }

    /// `checked` for an update of one thing: its history entry is the
    /// update's whole account (`UpdateLog`), not just the verdict.
    pub(super) fn checked_logged(
        &self,
        probe: Option<TryProbe>,
        judge: Judge,
        then: Option<OnDone>,
        on_err: fn(&str),
        log: Option<UpdateLog>,
        start: impl FnOnce(OnDone) + 'static,
    ) {
        let Some(first) = probe.clone() else {
            start(self.checked_from(None, None, judge, then, log));
            return;
        };
        let w = self.downgrade();
        spawn_bg(
            move || first(),
            move |before| match before {
                Ok(before) => {
                    if let Some(win) = w.upgrade() {
                        start(win.checked_from(before, probe, judge, then, log));
                    }
                }
                Err(e) => on_err(&e),
            },
        );
    }

    /// `checked`'s on_done, once the first answer is `before`.
    fn checked_from(
        &self,
        before: Option<ProbeValue>,
        probe: Option<TryProbe>,
        judge: Judge,
        then: Option<OnDone>,
        log: Option<UpdateLog>,
    ) -> OnDone {
        let w = self.downgrade();
        Box::new(move |ok| {
            if let Some(then) = then {
                then(ok);
            }
            let Some(probe) = probe else {
                if let (Some(log), Some(win)) = (log, w.upgrade()) {
                    win.record_update(&log, ok, None);
                }
                return;
            };
            spawn_bg(
                move || probe().map(|after| judge(before, after)),
                move |out| match out {
                    Ok(out) => {
                        if let Some(win) = w.upgrade() {
                            match log {
                                Some(log) => {
                                    win.record_update(&log, ok, Some(&out));
                                    win.show_outcome(&out);
                                }
                                None => win.report_outcome(&out),
                            }
                        }
                    }
                    Err(e) => raised(&e),
                },
            );
        })
    }

    /// One update's history entry: what ran, how long, what changed, and
    /// what the check found.
    pub(super) fn record_update(&self, log: &UpdateLog, ok: bool, out: Option<&Outcome>) {
        if let Err(e) = history::record_action(
            history::ALMANAC,
            &out.map(|o| o.message.clone()).unwrap_or_else(|| format!("Updated {}", log.app.name)),
            "",
            update_severity(ok, out),
            &update_action(log, ok, out),
        ) {
            raised(&e);
        }
    }

    /// `_checked` for a `done` that may run more than once (the Python
    /// reuses one `done` each time `tasks.confirm` answers yes). As with
    /// `checked`, the first asking is off the main thread and `start` gets
    /// the done once it has answered.
    pub(super) fn checked_again(
        &self,
        probe: TryProbe,
        judge: JudgeAgain,
        then: Rc<dyn Fn(bool)>,
        start: impl FnOnce(Rc<dyn Fn(bool)>) + 'static,
    ) {
        let w = self.downgrade();
        let first = probe.clone();
        spawn_bg(
            move || first(),
            move |before| match before {
                Ok(before) => {
                    if let Some(win) = w.upgrade() {
                        start(win.checked_again_from(before, probe, judge, then));
                    }
                }
                Err(e) => raised(&e),
            },
        );
    }

    /// `checked_again`'s done, once the first answer is `before`.
    fn checked_again_from(
        &self,
        before: Option<ProbeValue>,
        probe: TryProbe,
        judge: JudgeAgain,
        then: Rc<dyn Fn(bool)>,
    ) -> Rc<dyn Fn(bool)> {
        let w = self.downgrade();
        Rc::new(move |ok| {
            then(ok);
            let (probe, judge, before) = (probe.clone(), judge.clone(), before.clone());
            let w = w.clone();
            spawn_bg(
                move || probe().map(|after| judge(before, after)),
                move |out| match out {
                    Ok(out) => {
                        if let Some(win) = w.upgrade() {
                            win.report_outcome(&out);
                        }
                    }
                    Err(e) => raised(&e),
                },
            );
        })
    }

    pub fn report_outcome(&self, out: &Outcome) {
        if let Err(e) = history::record_checked(
            history::ALMANAC,
            &out.message,
            "",
            if out.changed == Some(false) { "warn" } else { "" },
            &[],
        ) {
            return raised(&e);
        }
        self.show_outcome(out);
    }

    /// `report_outcome` without the history entry: said, and notified when
    /// nothing changed.
    fn show_outcome(&self, out: &Outcome) {
        if out.changed == Some(false) {
            // Something the user asked for did not happen. A toast can be
            // missed; this is the one result that must not be.
            if settings::get("notify_no_change").is_ok_and(|v| truthy(&v)) {
                self.notify(&tr("Nothing changed"), &out.message, "normal");
            }
            self.say(&out.message, 10);
        } else {
            self.say(&out.message, 5);
        }
    }

    /// Put a Snapper snapshot ahead of a package change, when available.
    ///
    /// `Err` where `create_step` raises: the action it was for never starts.
    pub fn snapshot_first(&self, steps: Vec<Step>, what: &str) -> Result<Vec<Step>, String> {
        let step = snapshots::try_create_step(&format!("Almanac: before {what}"))?;
        Ok(with_snapshot(step, steps))
    }
}

/// What one update ran, for its history entry.
pub(super) struct UpdateLog {
    pub app: App,
    pub argv: String,
    pub started: Instant,
}

impl UpdateLog {
    pub fn new(app: &App, argv: &[String]) -> UpdateLog {
        UpdateLog { app: app.clone(), argv: argv.join(" "), started: Instant::now() }
    }
}

/// "warn" when the update failed or changed nothing.
pub(super) fn update_severity(ok: bool, out: Option<&Outcome>) -> &'static str {
    if !ok || out.is_some_and(|o| o.changed == Some(false)) {
        "warn"
    } else {
        ""
    }
}

/// The action record of one update. The task reports one result, so the
/// command is known to have exited 0 only when it succeeded.
pub(super) fn update_action(log: &UpdateLog, ok: bool, out: Option<&Outcome>) -> ActionLog {
    let secs = log.started.elapsed().as_secs_f64();
    let changed = out.and_then(|o| o.changed);
    ActionLog {
        commands: vec![CommandRun { argv: log.argv.clone(), exit: ok.then_some(0), secs: Some(secs) }],
        changes: if changed == Some(true) {
            vec![Change { name: log.app.name.clone(), from: log.app.version.clone(), to: log.app.update.clone() }]
        } else {
            Vec::new()
        },
        checks: vec![CheckResult {
            name: "Installed version changed".into(),
            ok: changed,
            detail: out.map(|o| o.message.clone()).unwrap_or_else(|| "could not be checked".into()),
        }],
        duration_secs: Some(secs),
        ..Default::default()
    }
}

/// `lambda b, a: verify.judge_app(app, b, a)`
pub(super) fn app_judge(app: &App) -> Judge {
    let app = app.clone();
    Box::new(move |b, a| verify::judge_app(&app, b.as_ref(), a.as_ref()))
}

/// `app_judge` for `checked_again`.
pub(super) fn app_judge_again(app: &App) -> JudgeAgain {
    let app = app.clone();
    Arc::new(move |b, a| verify::judge_app(&app, b.as_ref(), a.as_ref()))
}

/// `after_pacman_cb` for `checked_again`.
pub(super) fn after_pacman_again(win: &Win) -> Rc<dyn Fn(bool)> {
    let w = win.downgrade();
    Rc::new(move |ok| {
        if let Some(win) = w.upgrade() {
            win.after_pacman(ok);
        }
    })
}

/// `then=self._after_pacman`
pub(super) fn after_pacman_cb(win: &Win) -> OnDone {
    let w = win.downgrade();
    Box::new(move |ok| {
        if let Some(win) = w.upgrade() {
            win.after_pacman(ok);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A single update's history entry: the command, the version change
    /// only when the check saw it, and "warn" when it failed or did nothing.
    #[test]
    fn one_update_is_logged_with_what_its_check_found() {
        let mut app = App::new("ghostty", "1.0", crate::inventory::REPO);
        app.update = "1.1".into();
        let log = UpdateLog::new(&app, &["yay".to_string(), "-S".into(), "ghostty".into()]);
        let landed = Outcome::new(Some(true), "ghostty 1.0 → 1.1");
        let a = update_action(&log, true, Some(&landed));
        assert_eq!(a.commands[0].argv, "yay -S ghostty");
        assert_eq!(a.commands[0].exit, Some(0));
        assert_eq!(a.changes, vec![Change { name: "ghostty".into(), from: "1.0".into(), to: "1.1".into() }]);
        assert_eq!(a.checks[0].ok, Some(true));
        assert_eq!(update_severity(true, Some(&landed)), "");

        let nothing = Outcome::new(Some(false), "ghostty did not change");
        let a = update_action(&log, true, Some(&nothing));
        assert!(a.changes.is_empty());
        assert_eq!(update_severity(true, Some(&nothing)), "warn");
        let a = update_action(&log, false, None);
        assert_eq!((a.commands[0].exit, a.checks[0].ok), (None, None));
        assert_eq!(update_severity(false, None), "warn");
    }
}
