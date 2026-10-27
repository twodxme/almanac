//! Did it work? Outcomes checked against the machine, not exit codes.

use std::rc::Rc;
use std::sync::Arc;

use crate::{history, settings, snapshots, verify};
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
        let Some(first) = probe.clone() else {
            start(self.checked_from(None, None, judge, then));
            return;
        };
        let w = self.downgrade();
        spawn_bg(
            move || first(),
            move |before| match before {
                Ok(before) => {
                    if let Some(win) = w.upgrade() {
                        start(win.checked_from(before, probe, judge, then));
                    }
                }
                Err(e) => on_err(&e),
            },
        );
    }

    /// `checked`'s on_done, once the first answer is `before`.
    fn checked_from(&self, before: Option<ProbeValue>, probe: Option<TryProbe>, judge: Judge, then: Option<OnDone>) -> OnDone {
        let w = self.downgrade();
        Box::new(move |ok| {
            if let Some(then) = then {
                then(ok);
            }
            let Some(probe) = probe else {
                return;
            };
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
        self.announce_outcome(out);
    }

    /// `report_outcome` without the History entry, for a caller that has
    /// recorded the outcome with the rest of what it did.
    pub fn announce_outcome(&self, out: &Outcome) {
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
