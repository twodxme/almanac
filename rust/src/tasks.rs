//! Port of `tasks.py`.
//!
//! Running things inside the app instead of handing them to a terminal.
//!
//! Shelling out to a terminal window was the honest option while nothing here
//! could show progress or ask a question. It is not self-contained though: the
//! work happens somewhere the app cannot see, success is whatever the user
//! reports by closing the window, and the inventory has to be re-read on a timer
//! and hope.
//!
//! A task here runs as a child process with its output streamed into a dialog, so
//! the app knows what happened and when it finished. Two things follow from that:
//!
//! * Anything destructive is confirmed by a GTK dialog *first*, showing exactly
//!   what will be touched -- the preview that pacman's own y/N prompt gives. The
//!   prompt is not skipped, it is moved somewhere it can be read.
//! * Anything needing root runs under `pkexec`, which prompts through polkit.
//!   `sudo` is not usable here: it wants a TTY, and there is deliberately none.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use gtk::glib;

/// A step that is work done in this process rather than a command --
/// downloading and registering an AppImage, say. It takes the log function
/// and returns `Err` to fail (the Python raises).
///
/// `Arc` so a step can be cloned; `Send + Sync` because steps run on the
/// worker thread. The same type as `maintenance::FixFn`, so a finding's fix
/// is a step as it is.
pub type StepFn = Arc<dyn Fn(LogFn<'_>) -> Result<(), String> + Send + Sync>;

/// The log function a step is handed: one line of output per call. The same
/// type as `maintenance::LogFn`.
pub type LogFn<'a> = &'a (dyn Fn(&str) + Send + Sync);

/// What one step does: run a command (`argv`), or call a function.
#[derive(Clone)]
pub enum StepAction {
    /// A command line; elevated with `elevate` when it needs root.
    Argv(Vec<String>),
    /// In-process work, see `StepFn`.
    Call(StepFn),
}

impl std::fmt::Debug for StepAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StepAction::Argv(a) => f.debug_tuple("Argv").field(a).finish(),
            StepAction::Call(_) => f.write_str("Call(<fn>)"),
        }
    }
}

/// `Finding.fix` as a step: `[(f.fix_label, f.fix)]` in the Python.
impl From<crate::maintenance::Fix> for StepAction {
    fn from(fix: crate::maintenance::Fix) -> Self {
        match fix {
            crate::maintenance::Fix::Argv(a) => StepAction::Argv(a),
            crate::maintenance::Fix::Func(f) => StepAction::Call(f),
        }
    }
}

/// One `(label, argv)` entry of the Python `steps` list.
#[derive(Clone, Debug)]
pub struct Step {
    pub label: String,
    pub action: StepAction,
}

impl Step {
    /// `(label, [argv...])`.
    pub fn argv(label: impl Into<String>, argv: Vec<String>) -> Self {
        Step { label: label.into(), action: StepAction::Argv(argv) }
    }

    /// `(label, callable)`.
    pub fn call(label: impl Into<String>, f: StepFn) -> Self {
        Step { label: label.into(), action: StepAction::Call(f) }
    }
}

/// Called on the main thread once the task is over, with whether it worked.
pub type OnDone = Box<dyn FnOnce(bool) + 'static>;

pub fn needs_root(argv: &[String]) -> bool {
    if argv.is_empty() {
        return false;
    }
    // The system journal is root's. Vacuumed as the user, journalctl cannot
    // delete a single file, says "freed 0B" and still exits 0.
    if argv[0] == "journalctl" {
        return argv.iter().any(|a| a.starts_with("--vacuum"));
    }
    matches!(argv[0].as_str(), "pacman" | "paccache" | "pacman-key")
}

/// Wrap a command that needs root so polkit can ask for the password.
///
/// --keep-cwd is deliberate: pkexec otherwise switches to root's home, which
/// changes what a relative path means mid-task.
pub fn elevate(argv: &[String]) -> Vec<String> {
    if needs_root(argv) {
        let mut out = vec!["pkexec".to_string(), "--keep-cwd".to_string()];
        out.extend(argv.iter().cloned());
        out
    } else {
        argv.to_vec()
    }
}

/// Runs a list of (label, argv) steps, streaming output into the window.
///
/// Cloning gives another handle to the same dialog.
#[derive(Clone)]
pub struct TaskDialog {
    pub dialog: adw::Dialog,
    pub step_label: gtk::Label,
    pub progress: gtk::ProgressBar,
    pub buffer: gtk::TextBuffer,
    pub text: gtk::TextView,
    pub scroll: gtk::ScrolledWindow,
    pub cancel_btn: gtk::Button,
    pub close_btn: gtk::Button,
    /// The steps, taken by the worker thread when it starts.
    steps: Arc<Mutex<Vec<Step>>>,
    on_done: Rc<RefCell<Option<OnDone>>>,
    /// PID of the running child (its process group leader), kept so Cancel
    /// can stop it.
    proc_pid: Arc<Mutex<Option<i32>>>,
    cancelled: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    pulse: Rc<RefCell<Option<glib::SourceId>>>,
}

impl TaskDialog {
    /// Builds the dialog, presents it on `parent` and starts the work.
    pub fn new(
        parent: &impl gtk::glib::object::IsA<gtk::Widget>,
        title: &str,
        steps: Vec<Step>,
        on_done: Option<OnDone>,
    ) -> TaskDialog {
        let _ = (parent, title, steps, on_done);
        todo!("port: tasks::TaskDialog::new")
    }

    // ---- ui helpers ----

    /// Append a line to the log; safe to call from the main thread. (The
    /// worker thread logs through a channel to the same place.)
    pub fn log(&self, line: &str) {
        let _ = line;
        todo!("port: tasks::TaskDialog::log")
    }

    pub fn cancel(&self) {
        todo!("port: tasks::TaskDialog::cancel")
    }

    pub fn force_close(&self) {
        todo!("port: tasks::TaskDialog::force_close")
    }

    #[allow(dead_code)]
    fn tick(&self) -> glib::ControlFlow {
        todo!("port: tasks::TaskDialog::_tick")
    }

    #[allow(dead_code)]
    fn append(&self, line: &str) {
        let _ = line;
        todo!("port: tasks::TaskDialog::_append")
    }

    // ---- the work ----

    #[allow(dead_code)]
    fn finish(&self, ok: bool) {
        let _ = ok;
        todo!("port: tasks::TaskDialog::_finish")
    }
}

/// Run steps in a dialog. `steps` is [(label, [argv...]), ...].
pub fn run(
    parent: &impl gtk::glib::object::IsA<gtk::Widget>,
    title: &str,
    steps: Vec<Step>,
    on_done: Option<OnDone>,
) -> TaskDialog {
    TaskDialog::new(parent, title, steps, on_done)
}

/// Ask before doing something that cannot be undone by clicking again.
///
/// `detail` is the preview -- the actual list of what will be removed or
/// installed. Showing it is the whole point: a y/N prompt with no list is
/// worse than this dialog, not better.
#[allow(clippy::too_many_arguments)]
pub fn confirm(
    parent: &impl gtk::glib::object::IsA<gtk::Widget>,
    heading: &str,
    body: &str,
    detail: &str,
    action: &str,
    destructive: bool,
    on_yes: impl Fn() + 'static,
) {
    let _ = (parent, heading, body, detail, action, destructive, on_yes);
    todo!("port: tasks::confirm")
}

/// What `pacman -Rs` would actually take out, without root.
///
/// -Rns cannot be previewed (--nosave and --print are mutually exclusive), so
/// the preview runs -Rs. The set of packages is the same; only the treatment
/// of config files differs, and that is stated in the dialog.
pub fn preview_removal(names: &[String]) -> (bool, String) {
    let _ = names;
    todo!("port: tasks::preview_removal")
}

/// Parity hook: see ARCHITECTURE.md.
pub fn parity(_func: &str, _args: &serde_json::Value) -> Option<serde_json::Value> {
    None
}
