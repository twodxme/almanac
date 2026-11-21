//! The step timeline for an update run, from what its processes and output show.
//!
//! No GTK here. The rule it keeps: a step is never shown Done unless a
//! later step's process has started or the run exited 0. When nothing is
//! known, `generic` is set and the view says "Updating…".

use std::time::{Duration, Instant};

use crate::postupdate::clean_output;

/// How long without a recognised process before the view goes generic.
const GENERIC_AFTER: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepId {
    Snapshot,
    SigningKeys,
    SystemPackages,
    Migrations,
    AurAndTools,
    Checking,
    OtherApps,
}

impl StepId {
    pub fn label(&self) -> &'static str {
        match self {
            StepId::Snapshot => "Snapshot",
            StepId::SigningKeys => "Signing keys",
            StepId::SystemPackages => "System packages",
            StepId::Migrations => "Omarchy migrations",
            StepId::AurAndTools => "AUR and tools",
            StepId::Checking => "Checking everything works",
            StepId::OtherApps => "Other apps",
        }
    }

    /// The step an omarchy script belongs to.
    fn of_process(name: &str) -> Option<StepId> {
        Some(match name {
            "omarchy-update-pkg-prune" | "omarchy-snapshot" => StepId::Snapshot,
            "omarchy-update-keyring" => StepId::SigningKeys,
            "omarchy-update-system-pkgs"
            | "omarchy-update-pacman"
            | "omarchy-update-system-pkgs-when-conflicted" => StepId::SystemPackages,
            "omarchy-migrate"
            | "omarchy-update-orphan-pkgs"
            | "omarchy-update-restart"
            | "omarchy-hook" => StepId::Migrations,
            "omarchy-update-mise" | "omarchy-update-aur-pkgs" => StepId::AurAndTools,
            "omarchy-update-boot" => StepId::Checking,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Changed {
    Nothing,
    Some,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fix {
    StopStuckUpdate(Vec<u32>),
    Resolve,
    RetryUpdate,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StepState {
    Waiting,
    Running {
        detail: String,
    },
    Done {
        detail: String,
    },
    Failed {
        reason: String,
        changed: Changed,
        fix: Option<Fix>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppState {
    Waiting,
    Downloading,
    Installing,
    Done { version: String },
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppRow {
    pub name: String,
    pub to: String,
    pub step: StepId,
    pub state: AppState,
}

#[derive(Clone, Debug)]
pub struct Timeline {
    pub steps: Vec<(StepId, StepState)>,
    pub apps: Vec<AppRow>,
    pub restart_note: Option<String>,
    pub generic: bool,
}

impl Timeline {
    /// `apps` is (name, new version, from the AUR).
    pub fn new(apps: &[(String, String, bool)], other_apps: bool) -> Timeline {
        let mut ids = vec![
            StepId::Snapshot,
            StepId::SigningKeys,
            StepId::SystemPackages,
            StepId::Migrations,
            StepId::AurAndTools,
            StepId::Checking,
        ];
        if other_apps {
            ids.push(StepId::OtherApps);
        }
        Timeline {
            steps: ids.into_iter().map(|i| (i, StepState::Waiting)).collect(),
            apps: apps
                .iter()
                .map(|(name, to, aur)| AppRow {
                    name: name.clone(),
                    to: to.clone(),
                    step: if *aur {
                        StepId::AurAndTools
                    } else {
                        StepId::SystemPackages
                    },
                    state: AppState::Waiting,
                })
                .collect(),
            restart_note: None,
            generic: false,
        }
    }

    /// The step under way: the last one running.
    pub fn current(&self) -> Option<StepId> {
        self.steps
            .iter()
            .rev()
            .find(|(_, s)| matches!(s, StepState::Running { .. }))
            .map(|(i, _)| *i)
    }

    /// (1-based position of the current step, number of steps).
    pub fn position(&self) -> (usize, usize) {
        let n = self.steps.len();
        let at = self
            .current()
            .and_then(|c| self.steps.iter().position(|(i, _)| *i == c))
            .map_or(0, |i| i + 1);
        (at, n)
    }

    fn index(&self, id: StepId) -> Option<usize> {
        self.steps.iter().position(|(i, _)| *i == id)
    }

    fn failed(&self) -> bool {
        self.steps
            .iter()
            .any(|(_, s)| matches!(s, StepState::Failed { .. }))
    }
}

pub struct Tracker {
    t: Timeline,
    pub(crate) started: Instant,
    /// The end of a chunk that has no line ending yet.
    pending: String,
    /// The row pacman is on.
    open_row: Option<usize>,
}

impl Tracker {
    pub fn new(t: Timeline) -> Tracker {
        Tracker {
            t,
            started: Instant::now(),
            pending: String::new(),
            open_row: None,
        }
    }

    pub fn timeline(&self) -> &Timeline {
        &self.t
    }

    /// Record that a restart is needed but was left to the user.
    pub fn set_restart_note(&mut self, note: String) {
        self.t.restart_note = Some(note);
    }

    /// The names of the programs now running in the update's session.
    pub fn on_procs(&mut self, names: &[String]) {
        self.on_procs_at(names, Instant::now());
    }

    pub(crate) fn on_procs_at(&mut self, names: &[String], now: Instant) {
        let reached = names
            .iter()
            .filter_map(|n| StepId::of_process(n))
            .filter_map(|id| self.t.index(id))
            .max();
        if let Some(at) = reached {
            if !self.t.failed() {
                for i in 0..at {
                    if !matches!(self.t.steps[i].1, StepState::Failed { .. }) {
                        self.t.steps[i].1 = StepState::Done {
                            detail: String::new(),
                        };
                        let id = self.t.steps[i].0;
                        self.close_rows(id);
                    }
                }
                // Only move forward: an earlier step's process after a later
                // one started changes nothing.
                let ahead = self.t.index_of_current().is_some_and(|c| c > at);
                if !ahead && matches!(self.t.steps[at].1, StepState::Waiting) {
                    self.t.steps[at].1 = StepState::Running {
                        detail: String::new(),
                    };
                }
            }
            self.t.generic = false;
        } else if self.t.current().is_none()
            && !self.t.failed()
            && now.saturating_duration_since(self.started) >= GENERIC_AFTER
        {
            self.t.generic = true;
        }
    }

    pub fn on_output(&mut self, chunk: &str) {
        self.pending.push_str(chunk);
        let mut rest = std::mem::take(&mut self.pending);
        while let Some(at) = rest.find(['\r', '\n']) {
            let seg: String = rest.drain(..=at).collect();
            self.segment(seg.trim_end_matches(['\r', '\n']));
        }
        self.pending = rest;
    }

    pub fn on_exit(&mut self, code: i32) {
        let tail = std::mem::take(&mut self.pending);
        self.segment(&tail);
        if code == 0 {
            // Exit 0 means omarchy update finished all its own steps, sampled
            // or not. "Other apps" runs after it, in Almanac: not ours to close.
            let mut closed = Vec::new();
            for (id, s) in &mut self.t.steps {
                if *id != StepId::OtherApps
                    && matches!(s, StepState::Running { .. } | StepState::Waiting)
                {
                    *s = StepState::Done {
                        detail: String::new(),
                    };
                    closed.push(*id);
                }
            }
            for id in closed {
                self.close_rows(id);
            }
            self.t.generic = false;
        } else if !self.t.failed() {
            let at = self
                .t
                .current()
                .or_else(|| {
                    self.t
                        .steps
                        .iter()
                        .find(|(_, s)| !matches!(s, StepState::Done { .. }))
                        .map(|(i, _)| *i)
                })
                .unwrap_or(StepId::Snapshot);
            self.fail(
                at,
                format!("It stopped with an error (exit status {code})."),
                Changed::Unknown,
                Some(Fix::RetryUpdate),
            );
        }
    }

    fn fail(&mut self, id: StepId, reason: String, changed: Changed, fix: Option<Fix>) {
        if self.t.failed() {
            return;
        }
        if let Some(i) = self.t.index(id) {
            self.t.steps[i].1 = StepState::Failed {
                reason,
                changed,
                fix,
            };
            self.t.generic = false;
        }
        // Only the failing step's rows: other steps' rows are what they were.
        for r in &mut self.t.apps {
            if r.step == id && matches!(r.state, AppState::Installing | AppState::Downloading) {
                r.state = AppState::Failed;
            }
        }
    }

    /// A step is over: its rows that were under way are done.
    fn close_rows(&mut self, id: StepId) {
        for (i, r) in self.t.apps.iter_mut().enumerate() {
            if r.step == id && matches!(r.state, AppState::Installing | AppState::Downloading) {
                r.state = AppState::Done {
                    version: r.to.clone(),
                };
                if self.open_row == Some(i) {
                    self.open_row = None;
                }
            }
        }
    }

    fn any_row_done(&self) -> bool {
        self.t
            .apps
            .iter()
            .any(|r| matches!(r.state, AppState::Done { .. }))
    }

    fn segment(&mut self, raw: &str) {
        let clean = clean_output(raw);
        let line = clean.trim();
        if line.is_empty() {
            return;
        }
        if line.contains("An Omarchy update is already running") {
            // The output cannot say who holds it; the caller fills the ids.
            self.fail(
                StepId::Snapshot,
                "Another Omarchy update is running.".into(),
                Changed::Nothing,
                Some(Fix::StopStuckUpdate(Vec::new())),
            );
        } else if line.contains("This upgrade needs an answer") {
            self.fail(
                StepId::SystemPackages,
                "This update needs an answer only you can give.".into(),
                Changed::Nothing,
                Some(Fix::Resolve),
            );
        } else if line.contains("incorrect password attempt") {
            self.fail(
                StepId::Snapshot,
                "Password not accepted. Nothing was changed.".into(),
                Changed::Nothing,
                None,
            );
        } else if line.starts_with("error: failed retrieving file")
            || line.contains("failed to commit transaction")
        {
            let changed = if self.any_row_done() {
                Changed::Some
            } else {
                Changed::Nothing
            };
            self.fail(
                StepId::SystemPackages,
                line.to_string(),
                changed,
                Some(Fix::RetryUpdate),
            );
        } else if line.contains("Run omarchy-system reboot when ready") {
            self.t.restart_note = Some(line.to_string());
        } else if line.starts_with(":: Retrieving packages") {
            if !self.t.failed() {
                if let Some(i) = self.t.index(StepId::SystemPackages) {
                    if matches!(
                        self.t.steps[i].1,
                        StepState::Waiting | StepState::Running { .. }
                    ) {
                        self.t.steps[i].1 = StepState::Running {
                            detail: "Downloading".into(),
                        };
                    }
                }
            }
        } else if let Some((name, verb_done)) = parse_row(line) {
            self.row(name, verb_done);
        }
    }

    /// A row seen in the output; `finished` is the version of an
    /// "upgraded NAME (a -> b)" line, `None` for a row that is starting.
    fn row(&mut self, name: &str, finished: Option<String>) {
        let found = self.t.apps.iter().position(|r| r.name == name);
        let at = match found {
            Some(i) => i,
            None => {
                let step = self.t.current().unwrap_or(StepId::SystemPackages);
                self.t.apps.push(AppRow {
                    name: name.to_string(),
                    to: finished.clone().unwrap_or_default(),
                    step,
                    state: AppState::Waiting,
                });
                self.t.apps.len() - 1
            }
        };
        match finished {
            Some(v) => {
                self.t.apps[at].state = AppState::Done { version: v };
                if self.open_row == Some(at) {
                    self.open_row = None;
                }
            }
            None => {
                if let Some(prev) = self.open_row.take() {
                    if prev != at && !matches!(self.t.apps[prev].state, AppState::Done { .. }) {
                        let to = self.t.apps[prev].to.clone();
                        self.t.apps[prev].state = AppState::Done { version: to };
                    }
                }
                self.t.apps[at].state = AppState::Installing;
                self.open_row = Some(at);
            }
        }
    }
}

/// `(i/n) upgrading NAME` gives (NAME, None); `upgraded NAME (a -> b)` gives
/// (NAME, Some(b)).
fn parse_row(line: &str) -> Option<(&str, Option<String>)> {
    if let Some(rest) = line.strip_prefix('(') {
        let (counter, rest) = rest.split_once(") ")?;
        let (i, n) = counter.split_once('/')?;
        if i.is_empty() || !i.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let (verb, rest) = rest.split_once(' ')?;
        if !matches!(verb, "upgrading" | "installing" | "reinstalling") {
            return None;
        }
        let name = rest.split_whitespace().next()?;
        return Some((name, None));
    }
    let rest = line.strip_prefix("upgraded ")?;
    let (name, v) = rest.split_once(' ')?;
    let v = v.trim().strip_prefix('(')?.strip_suffix(')')?;
    let to = v.split_once(" -> ").map_or(v, |(_, b)| b);
    Some((name, Some(to.to_string())))
}

impl Timeline {
    fn index_of_current(&self) -> Option<usize> {
        self.current().and_then(|c| self.index(c))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }
    fn apps5() -> Vec<(String, String, bool)> {
        [
            ("cursor", "1.2"),
            ("mise", "2.0"),
            ("slack-desktop", "4.1"),
            ("a", "1"),
            ("b", "2"),
        ]
        .iter()
        .map(|(n, v)| (n.to_string(), v.to_string(), false))
        .collect()
    }
    fn tr() -> Tracker {
        Tracker::new(Timeline::new(&apps5(), false))
    }
    fn state(t: &Tracker, id: StepId) -> StepState {
        t.timeline()
            .steps
            .iter()
            .find(|(i, _)| *i == id)
            .unwrap()
            .1
            .clone()
    }
    fn running(t: &Tracker, id: StepId) -> bool {
        matches!(state(t, id), StepState::Running { .. })
    }
    fn done(t: &Tracker, id: StepId) -> bool {
        matches!(state(t, id), StepState::Done { .. })
    }
    fn row(t: &Tracker, n: &str) -> AppState {
        t.timeline()
            .apps
            .iter()
            .find(|r| r.name == n)
            .unwrap()
            .state
            .clone()
    }

    #[test]
    fn labels_and_other_apps() {
        let t = Timeline::new(&[], true);
        let l: Vec<_> = t.steps.iter().map(|(i, _)| i.label()).collect();
        assert_eq!(
            l,
            [
                "Snapshot",
                "Signing keys",
                "System packages",
                "Omarchy migrations",
                "AUR and tools",
                "Checking everything works",
                "Other apps"
            ]
        );
        assert_eq!(Timeline::new(&[], false).steps.len(), 6);
    }

    #[test]
    fn processes_tick_steps_in_order() {
        let mut t = tr();
        assert_eq!(t.timeline().current(), None);
        t.on_procs(&s(&["omarchy-snapshot"]));
        assert_eq!(t.timeline().current(), Some(StepId::Snapshot));
        assert_eq!(t.timeline().position(), (1, 6));
        t.on_procs(&s(&["omarchy-update-keyring"]));
        assert!(done(&t, StepId::Snapshot));
        assert!(running(&t, StepId::SigningKeys));
        t.on_procs(&s(&["omarchy-update-system-pkgs"]));
        assert!(done(&t, StepId::SigningKeys));
        assert_eq!(t.timeline().current(), Some(StepId::SystemPackages));
        assert_eq!(t.timeline().position(), (3, 6));
        // A process of an earlier step does not move us back.
        t.on_procs(&s(&["omarchy-snapshot"]));
        assert_eq!(t.timeline().current(), Some(StepId::SystemPackages));
    }

    #[test]
    fn a_step_is_never_done_early() {
        let mut t = tr();
        t.on_procs(&s(&["omarchy-update-system-pkgs"]));
        assert!(running(&t, StepId::SystemPackages));
        t.on_procs(&s(&["omarchy-update-aur-pkgs"]));
        assert!(done(&t, StepId::SystemPackages));
        assert!(running(&t, StepId::AurAndTools));
        t.on_procs(&s(&["omarchy-update-boot"]));
        assert!(done(&t, StepId::AurAndTools));
        assert!(running(&t, StepId::Checking));
        assert!(!done(&t, StepId::Checking));
    }

    #[test]
    fn pacman_lines_move_app_rows() {
        let mut t = tr();
        t.on_procs(&s(&["omarchy-update-system-pkgs"]));
        t.on_output(":: Retrieving packages...\n");
        assert_eq!(
            state(&t, StepId::SystemPackages),
            StepState::Running {
                detail: "Downloading".into()
            }
        );
        t.on_output("(1/5) upgrading cursor                [####] 100%\r");
        t.on_output("(2/5) upgrading mise                  [####] 100%\r");
        t.on_output("(3/5) upgrading slack-desktop         [####] 100%\r");
        assert_eq!(
            row(&t, "cursor"),
            AppState::Done {
                version: "1.2".into()
            }
        );
        assert_eq!(
            row(&t, "mise"),
            AppState::Done {
                version: "2.0".into()
            }
        );
        assert_eq!(row(&t, "slack-desktop"), AppState::Installing);
        assert_eq!(row(&t, "a"), AppState::Waiting);
    }

    #[test]
    fn upgraded_line_finishes_a_row_with_its_version() {
        let mut t = tr();
        t.on_output("(1/5) upgrading cursor\n");
        t.on_output("upgraded cursor (1.1 -> 1.3)\n");
        assert_eq!(
            row(&t, "cursor"),
            AppState::Done {
                version: "1.3".into()
            }
        );
    }

    #[test]
    fn unknown_packages_get_rows() {
        let mut t = tr();
        t.on_procs(&s(&["omarchy-update-system-pkgs"]));
        t.on_output("(1/9) upgrading libfoo\n");
        let r = t
            .timeline()
            .apps
            .iter()
            .find(|r| r.name == "libfoo")
            .unwrap();
        assert_eq!(r.step, StepId::SystemPackages);
        assert_eq!(r.state, AppState::Installing);
    }

    #[test]
    fn carriage_returns_split_segments() {
        let mut t = tr();
        t.on_output("(1/5) upgrading a\r(2/5) upgrading b\n");
        assert!(matches!(row(&t, "a"), AppState::Done { .. }));
        assert_eq!(row(&t, "b"), AppState::Installing);
    }

    #[test]
    fn lines_split_across_chunks_are_joined() {
        let mut t = tr();
        t.on_output("(1/5) upgr");
        assert_eq!(row(&t, "cursor"), AppState::Waiting);
        t.on_output("ading cursor\n");
        assert_eq!(row(&t, "cursor"), AppState::Installing);
    }

    #[test]
    fn ansi_is_stripped() {
        let mut t = tr();
        t.on_output("\x1b[1m(1/5) upgrading cursor\x1b[0m\n");
        assert_eq!(row(&t, "cursor"), AppState::Installing);
    }

    #[test]
    fn already_running_fails_before_anything() {
        let mut t = tr();
        t.on_output("An Omarchy update is already running.\n");
        match state(&t, StepId::Snapshot) {
            StepState::Failed {
                reason,
                changed,
                fix,
            } => {
                assert_eq!(reason, "Another Omarchy update is running.");
                assert_eq!(changed, Changed::Nothing);
                assert_eq!(fix, Some(Fix::StopStuckUpdate(vec![])));
            }
            o => panic!("{o:?}"),
        }
        t.on_exit(1);
        assert!(matches!(
            state(&t, StepId::Snapshot),
            StepState::Failed {
                fix: Some(Fix::StopStuckUpdate(_)),
                ..
            }
        ));
    }

    #[test]
    fn conflict_offers_resolve() {
        let mut t = tr();
        t.on_procs(&s(&["omarchy-update-system-pkgs"]));
        t.on_output("This upgrade needs an answer. Run omarchy update interactively to give it.\n");
        assert!(matches!(
            state(&t, StepId::SystemPackages),
            StepState::Failed {
                changed: Changed::Nothing,
                fix: Some(Fix::Resolve),
                ..
            }
        ));
    }

    #[test]
    fn download_failure_fails_system_packages() {
        let mut t = tr();
        t.on_procs(&s(&["omarchy-update-system-pkgs"]));
        t.on_output("error: failed retrieving file 'x' from mirror : Could not resolve host\n");
        match state(&t, StepId::SystemPackages) {
            StepState::Failed {
                reason, changed, ..
            } => {
                assert!(reason.starts_with("error: failed retrieving file"));
                assert_eq!(changed, Changed::Nothing);
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn commit_failure_after_a_done_row_says_some() {
        let mut t = tr();
        t.on_procs(&s(&["omarchy-update-system-pkgs"]));
        t.on_output("(1/5) upgrading cursor\n(2/5) upgrading mise\n");
        t.on_output("error: failed to commit transaction (conflicting files)\n");
        assert!(matches!(
            state(&t, StepId::SystemPackages),
            StepState::Failed {
                changed: Changed::Some,
                ..
            }
        ));
    }

    #[test]
    fn sudo_failure_fails_first_step() {
        let mut t = tr();
        t.on_output("sudo: 3 incorrect password attempts\n");
        t.on_exit(1);
        assert_eq!(
            state(&t, StepId::Snapshot),
            StepState::Failed {
                reason: "Password not accepted. Nothing was changed.".into(),
                changed: Changed::Nothing,
                fix: None
            }
        );
    }

    #[test]
    fn nonzero_exit_fails_the_current_step() {
        let mut t = tr();
        t.on_procs(&s(&["omarchy-migrate"]));
        t.on_exit(3);
        assert_eq!(
            state(&t, StepId::Migrations),
            StepState::Failed {
                reason: "It stopped with an error (exit status 3).".into(),
                changed: Changed::Unknown,
                fix: Some(Fix::RetryUpdate)
            }
        );
    }

    #[test]
    fn exit_zero_completes() {
        let mut t = Tracker::new(Timeline::new(&apps5(), true));
        t.on_procs(&s(&["omarchy-update-boot"]));
        t.on_exit(0);
        for (id, st) in &t.timeline().steps {
            if *id == StepId::OtherApps {
                assert_eq!(*st, StepState::Waiting, "Almanac runs it after the exit");
            } else {
                assert!(matches!(st, StepState::Done { .. }), "{id:?}");
            }
        }
    }

    #[test]
    fn a_finished_step_closes_its_last_row() {
        let mut t = tr();
        t.on_procs(&s(&["omarchy-update-system-pkgs"]));
        t.on_output("(5/5) upgrading b\n");
        assert_eq!(row(&t, "b"), AppState::Installing);
        t.on_procs(&s(&["omarchy-migrate"]));
        assert!(done(&t, StepId::SystemPackages));
        assert_eq!(
            row(&t, "b"),
            AppState::Done {
                version: "2".into()
            }
        );
    }

    #[test]
    fn a_later_failure_leaves_installed_rows_alone() {
        let mut t = tr();
        t.on_procs(&s(&["omarchy-update-system-pkgs"]));
        t.on_output("(1/5) upgrading cursor\n(2/5) upgrading mise\n");
        t.on_procs(&s(&["omarchy-migrate"]));
        t.on_exit(1);
        assert!(matches!(
            state(&t, StepId::Migrations),
            StepState::Failed { .. }
        ));
        assert_eq!(
            row(&t, "mise"),
            AppState::Done {
                version: "2.0".into()
            }
        );
        assert_eq!(
            row(&t, "cursor"),
            AppState::Done {
                version: "1.2".into()
            }
        );
    }

    #[test]
    fn a_failure_fails_only_its_own_steps_open_row() {
        let mut t = tr();
        t.on_procs(&s(&["omarchy-update-system-pkgs"]));
        t.on_output("(1/5) upgrading cursor\n");
        t.on_output("error: failed to commit transaction (x)\n");
        assert_eq!(row(&t, "cursor"), AppState::Failed);
    }

    #[test]
    fn fail_leaves_rows_of_other_steps_alone() {
        let mut t = Tracker::new(Timeline::new(
            &[
                ("pkg".into(), "1".into(), false),
                ("tool".into(), "2".into(), true),
            ],
            false,
        ));
        t.t.apps[0].state = AppState::Installing;
        t.t.apps[1].state = AppState::Installing;
        t.fail(StepId::AurAndTools, "x".into(), Changed::Unknown, None);
        assert_eq!(row(&t, "pkg"), AppState::Installing, "other step's row");
        assert_eq!(row(&t, "tool"), AppState::Failed);
    }

    #[test]
    fn restart_note_is_kept() {
        let mut t = tr();
        t.on_output("Update complete. Run omarchy-system reboot when ready.\n");
        assert!(t
            .timeline()
            .restart_note
            .as_deref()
            .unwrap()
            .contains("reboot"));
    }

    #[test]
    fn no_process_for_five_seconds_goes_generic() {
        let mut t = tr();
        let t0 = t.started;
        t.on_procs_at(&[], t0 + Duration::from_secs(2));
        assert!(!t.timeline().generic);
        t.on_procs_at(&s(&["bash"]), t0 + Duration::from_secs(6));
        assert!(t.timeline().generic);
        t.on_procs_at(&s(&["omarchy-snapshot"]), t0 + Duration::from_secs(7));
        assert!(!t.timeline().generic);
    }
}
