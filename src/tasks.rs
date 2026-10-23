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
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adw::prelude::*;
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
    /// `self._proc`: the running child, kept so Cancel can stop it.
    proc: Arc<Mutex<Option<Arc<Proc>>>>,
    cancelled: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    pulse: Rc<RefCell<Option<glib::SourceId>>>,
}

/// A `TaskDialog` held weakly, for what the dialog itself owns: its
/// buttons' handlers and its pulse. A strong handle there is a cycle (the
/// dialog owns the button, the button's handler owns the dialog), and every
/// task dialog, with its whole log, stayed alive after it was closed.
struct WeakTaskDialog {
    dialog: glib::WeakRef<adw::Dialog>,
    step_label: glib::WeakRef<gtk::Label>,
    progress: glib::WeakRef<gtk::ProgressBar>,
    buffer: glib::WeakRef<gtk::TextBuffer>,
    text: glib::WeakRef<gtk::TextView>,
    scroll: glib::WeakRef<gtk::ScrolledWindow>,
    cancel_btn: glib::WeakRef<gtk::Button>,
    close_btn: glib::WeakRef<gtk::Button>,
    steps: std::sync::Weak<Mutex<Vec<Step>>>,
    on_done: Weak<RefCell<Option<OnDone>>>,
    proc: std::sync::Weak<Mutex<Option<Arc<Proc>>>>,
    cancelled: std::sync::Weak<AtomicBool>,
    failed: std::sync::Weak<AtomicBool>,
    pulse: Weak<RefCell<Option<glib::SourceId>>>,
}

impl WeakTaskDialog {
    fn upgrade(&self) -> Option<TaskDialog> {
        Some(TaskDialog {
            dialog: self.dialog.upgrade()?,
            step_label: self.step_label.upgrade()?,
            progress: self.progress.upgrade()?,
            buffer: self.buffer.upgrade()?,
            text: self.text.upgrade()?,
            scroll: self.scroll.upgrade()?,
            cancel_btn: self.cancel_btn.upgrade()?,
            close_btn: self.close_btn.upgrade()?,
            steps: self.steps.upgrade()?,
            on_done: self.on_done.upgrade()?,
            proc: self.proc.upgrade()?,
            cancelled: self.cancelled.upgrade()?,
            failed: self.failed.upgrade()?,
            pulse: self.pulse.upgrade()?,
        })
    }
}

impl TaskDialog {
    fn downgrade(&self) -> WeakTaskDialog {
        WeakTaskDialog {
            dialog: self.dialog.downgrade(),
            step_label: self.step_label.downgrade(),
            progress: self.progress.downgrade(),
            buffer: self.buffer.downgrade(),
            text: self.text.downgrade(),
            scroll: self.scroll.downgrade(),
            cancel_btn: self.cancel_btn.downgrade(),
            close_btn: self.close_btn.downgrade(),
            steps: Arc::downgrade(&self.steps),
            on_done: Rc::downgrade(&self.on_done),
            proc: Arc::downgrade(&self.proc),
            cancelled: Arc::downgrade(&self.cancelled),
            failed: Arc::downgrade(&self.failed),
            pulse: Rc::downgrade(&self.pulse),
        }
    }

    /// Builds the dialog, presents it on `parent` and starts the work.
    pub fn new(
        parent: &impl gtk::glib::object::IsA<gtk::Widget>,
        title: &str,
        steps: Vec<Step>,
        on_done: Option<OnDone>,
    ) -> TaskDialog {
        TaskDialog::build(parent, title, steps, on_done, false)
    }

    /// `keep_going`: a failed step is logged and the rest still run, for a
    /// batch of unrelated work. The task still ends failed.
    fn build(
        parent: &impl gtk::glib::object::IsA<gtk::Widget>,
        title: &str,
        steps: Vec<Step>,
        on_done: Option<OnDone>,
        keep_going: bool,
    ) -> TaskDialog {
        let dialog = adw::Dialog::builder().title(c_str(title)).build();
        dialog.set_content_width(720);
        dialog.set_content_height(520);
        dialog.set_can_close(false);

        let view = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        header.set_show_end_title_buttons(false);
        view.add_top_bar(&header);

        let body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .margin_start(16)
            .margin_end(16)
            .margin_top(12)
            .margin_bottom(12)
            .build();

        let step_label = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        step_label.add_css_class("heading");
        body.append(&step_label);

        let progress = gtk::ProgressBar::new();
        progress.set_show_text(false);
        body.append(&progress);

        let buffer = gtk::TextBuffer::new(None);
        let text = gtk::TextView::builder()
            .buffer(&buffer)
            .editable(false)
            .monospace(true)
            .cursor_visible(false)
            .build();
        text.set_wrap_mode(gtk::WrapMode::WordChar);
        text.add_css_class("card");
        text.set_left_margin(8);
        text.set_right_margin(8);
        text.set_top_margin(6);

        let scroll = gtk::ScrolledWindow::builder().vexpand(true).build();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_child(Some(&text));
        body.append(&scroll);

        view.set_content(Some(&body));

        let bottom = gtk::Box::builder()
            .spacing(8)
            .margin_start(16)
            .margin_end(16)
            .margin_top(6)
            .margin_bottom(12)
            .halign(gtk::Align::End)
            .build();
        let cancel_btn = gtk::Button::with_label("Cancel");
        let close_btn = gtk::Button::with_label("Close");
        close_btn.add_css_class("suggested-action");
        close_btn.set_visible(false);
        bottom.append(&cancel_btn);
        bottom.append(&close_btn);
        view.add_bottom_bar(&bottom);

        dialog.set_child(Some(&view));

        let this = TaskDialog {
            dialog,
            step_label,
            progress,
            buffer,
            text,
            scroll,
            cancel_btn,
            close_btn,
            steps: Arc::new(Mutex::new(steps)),
            on_done: Rc::new(RefCell::new(on_done)),
            proc: Arc::new(Mutex::new(None)),
            cancelled: Arc::new(AtomicBool::new(false)),
            failed: Arc::new(AtomicBool::new(false)),
            pulse: Rc::new(RefCell::new(None)),
        };
        let _ = &this.failed;

        // The handlers and the pulse hold the dialog weakly: it owns them.
        {
            let t = this.downgrade();
            this.cancel_btn.connect_clicked(move |_b| {
                if let Some(t) = t.upgrade() {
                    t.cancel();
                }
            });
            let t = this.downgrade();
            this.close_btn.connect_clicked(move |_b| {
                if let Some(t) = t.upgrade() {
                    t.force_close();
                }
            });
        }

        let t = this.downgrade();
        let id = glib::timeout_add_local(Duration::from_millis(90), move || match t.upgrade() {
            Some(t) => t.tick(),
            None => glib::ControlFlow::Break,
        });
        *this.pulse.borrow_mut() = Some(id);
        this.dialog.present(Some(parent));

        // The worker thread cannot touch GTK; everything it shows goes back
        // through idle callbacks on the main thread, holding the dialog in a
        // ThreadGuard. The worker's own handle is moved into the last idle
        // callback (`finish`), so it is dropped on the main thread too.
        let guard = Arc::new(glib::thread_guard::ThreadGuard::new(this.clone()));
        let steps = std::mem::take(&mut *this.steps.lock().unwrap_or_else(|e| e.into_inner()));
        let cancelled = this.cancelled.clone();
        let proc = this.proc.clone();
        // threading names the thread Thread-N; N counts every thread the
        // process started, so here it only counts the task threads.
        static THREADS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = THREADS.fetch_add(1, Ordering::SeqCst) + 1;
        std::thread::spawn(move || {
            let result = {
                let set_label = |label: &str| {
                    let g = guard.clone();
                    let label = label.to_string();
                    glib::idle_add_once(move || g.get_ref().step_label.set_text(c_str(&label)));
                };
                let log = |line: &str| idle_append(&guard, line);
                // A step that panics ends the task as failed, like any other
                // error, instead of taking the worker down without a word.
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    work(&steps, &cancelled, &proc, &set_label, &log, keep_going)
                }))
                .unwrap_or_else(|_| Err("the task stopped unexpectedly".to_string()))
            };
            match result {
                Ok(ok) => {
                    glib::idle_add_once(move || guard.get_ref().finish(ok));
                }
                Err(exc) => {
                    // An exception the Python does not catch: the thread died
                    // with a traceback on stderr and `_finish` was never
                    // called, so the dialog pulsed on and could not be
                    // closed. It is in the log now, and the task fails.
                    eprintln!("Exception in thread Thread-{n} (_work):\nTraceback (most recent call last):\n{exc}");
                    idle_append(&guard, &format!("failed: {exc}"));
                    glib::idle_add_once(move || guard.get_ref().finish(false));
                }
            }
        });
        this
    }

    // ---- ui helpers ----

    pub fn log(&self, line: &str) {
        let line = with_newline(line);
        let t = self.clone();
        glib::idle_add_local_once(move || t.append(&line));
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.log("\n— cancelled —");
        let proc = self.proc.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(proc) = proc {
            if proc.poll().is_none() {
                use nix::errno::Errno;
                use nix::sys::signal::{killpg, Signal};
                // The child may itself have children (git, makepkg); signal the
                // whole group so cancelling actually stops the work.
                match nix::unistd::getpgid(Some(proc.pid)).and_then(|g| killpg(g, Signal::SIGTERM)) {
                    Ok(()) => {}
                    Err(Errno::ESRCH | Errno::EPERM) => {
                        if let Err(exc) = proc.terminate() {
                            // Raised out of the click handler, as in the Python.
                            eprintln!("Traceback: in cancel: {exc}");
                        }
                    }
                    Err(e) => eprintln!("Traceback: in cancel: {}", py_errno_str(e as i32)),
                }
            }
        }
    }

    pub fn force_close(&self) {
        self.dialog.set_can_close(true);
        self.dialog.close();
    }

    fn tick(&self) -> glib::ControlFlow {
        self.progress.pulse();
        glib::ControlFlow::Continue
    }

    fn append(&self, line: &str) {
        let mut end = self.buffer.end_iter();
        self.buffer.insert(&mut end, c_str(line));
        // Follow the tail, the way a terminal would.
        let mark = self.buffer.create_mark(None, &self.buffer.end_iter(), false);
        self.text.scroll_mark_onscreen(&mark);
        self.buffer.delete_mark(&mark);
    }

    // ---- the work ----

    fn finish(&self, ok: bool) {
        if let Some(id) = self.pulse.borrow_mut().take() {
            id.remove();
        }
        self.progress.set_fraction(if ok { 1.0 } else { 0.0 });
        self.step_label.set_text(if ok {
            "Done"
        } else if self.cancelled.load(Ordering::SeqCst) {
            "Cancelled"
        } else {
            "Failed"
        });
        self.cancel_btn.set_visible(false);
        self.close_btn.set_visible(true);
        self.dialog.set_can_close(true);
        let on_done = self.on_done.borrow_mut().take();
        if let Some(f) = on_done {
            f(ok);
        }
    }
}

/// `line if line.endswith("\n") else line + "\n"`, what `log` appends.
fn with_newline(line: &str) -> String {
    if line.ends_with('\n') {
        line.to_string()
    } else {
        format!("{line}\n")
    }
}

/// `log` from the worker thread: `GLib.idle_add(self._append, ...)`.
/// A Python string as GTK receives it through PyGObject: as a C string, so
/// it ends at the first NUL ('a\0b\n' shows as 'a', and the next line runs
/// on). gtk-rs would pass the whole length instead -- and panic on the NUL
/// in a debug build.
fn c_str(s: &str) -> &str {
    s.split('\0').next().unwrap_or_default()
}

fn idle_append(guard: &Arc<glib::thread_guard::ThreadGuard<TaskDialog>>, line: &str) {
    let g = guard.clone();
    let line = with_newline(line);
    glib::idle_add_once(move || g.get_ref().append(&line));
}

/// `shlex.quote`: a word the shell reads back as itself.
fn shlex_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    let safe = s.chars().all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c));
    if safe {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

/// Python's `str.isprintable()` for one character: false for the categories
/// `repr()` escapes (Cc, Cf, Cs, Co, Cn, Zl, Zp, and Zs other than space).
fn py_isprintable(c: char) -> bool {
    if c.is_ascii() {
        return (' '..='~').contains(&c);
    }
    static NONPRINT: std::sync::OnceLock<Option<regex::Regex>> = std::sync::OnceLock::new();
    let re = NONPRINT.get_or_init(|| {
        regex::Regex::new(r"^[\p{Cc}\p{Cf}\p{Co}\p{Cn}\p{Zl}\p{Zp}\p{Zs}]$").ok()
    });
    match re {
        Some(re) => !re.is_match(c.encode_utf8(&mut [0u8; 4])),
        None => true,
    }
}

/// Python's `repr()` of a str, as it shows up inside an OSError message.
fn py_str_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if py_isprintable(c) => out.push(c),
            c => {
                let n = c as u32;
                if n <= 0xff {
                    out.push_str(&format!("\\x{n:02x}"));
                } else if n <= 0xffff {
                    out.push_str(&format!("\\u{n:04x}"));
                } else {
                    out.push_str(&format!("\\U{n:08x}"));
                }
            }
        }
    }
    out.push(quote);
    out
}

/// Python's `str.isspace()` for one character: Unicode White_Space plus the
/// four separators \x1c-\x1f.
fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// Python's `str.strip()`.
fn py_strip(s: &str) -> &str {
    s.trim_matches(py_isspace)
}

/// `os.strerror(n)`: io::Error prints "<strerror> (os error N)".
fn py_strerror(n: i32) -> String {
    let msg = std::io::Error::from_raw_os_error(n).to_string();
    msg.strip_suffix(&format!(" (os error {n})")).unwrap_or(&msg).to_string()
}

/// `str(OSError(n, strerror))`: `[Errno 2] No such file or directory`.
fn py_errno_str(n: i32) -> String {
    format!("[Errno {n}] {}", py_strerror(n))
}

/// The text one command step puts in the log before its output.
fn cmdline(cmd: &[String]) -> String {
    let words: Vec<String> = cmd.iter().map(|c| shlex_quote(c)).collect();
    format!("$ {}", words.join(" "))
}

/// Why `Popen` did not give a process.
#[derive(Debug)]
enum SpawnError {
    /// An `OSError`; its `str()`. Callers catch it.
    Os(String),
    /// Anything else (IndexError, ValueError), as `Type: message`. The
    /// Python callers do not catch these.
    Other(String),
}

/// `Popen.__init__` as far as exec, the way CPython's `_posixsubprocess`
/// does it: `args[0]` is the program; with no `/` in it each directory of
/// PATH is tried in turn (os.path.join, so `""` becomes `dir/`), and the
/// first error that is not ENOENT/ENOTDIR is the one reported. stdout and
/// stderr go to the given fds, stdin too when one is given (else inherited).
/// `start_new_session` makes the child the leader of a new session, so it
/// has no controlling terminal.
///
/// posix_spawn rather than fork+exec: glibc's execvp and GLib's spawn both
/// run a file without a `#!` line through /bin/sh, where Python's execve
/// fails with ENOEXEC; and a safe pre-exec hook does not exist.
fn py_popen(
    cmd: &[String],
    stdin: Option<std::os::fd::BorrowedFd<'_>>,
    stdout: std::os::fd::BorrowedFd<'_>,
    stderr: std::os::fd::BorrowedFd<'_>,
    start_new_session: bool,
) -> Result<nix::unistd::Pid, SpawnError> {
    use nix::spawn::{posix_spawn, PosixSpawnAttr, PosixSpawnFileActions, PosixSpawnFlags};
    use std::ffi::CString;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;

    let Some(program) = cmd.first() else {
        return Err(SpawnError::Other("IndexError: list index out of range".into()));
    };
    let nul = || SpawnError::Other("ValueError: embedded null byte".into());
    let argv: Vec<CString> =
        cmd.iter().map(|a| CString::new(a.as_bytes())).collect::<Result<_, _>>().map_err(|_| nul())?;
    let envp: Vec<CString> = std::env::vars_os()
        .filter_map(|(k, v)| {
            let mut kv = k.as_bytes().to_vec();
            kv.push(b'=');
            kv.extend_from_slice(v.as_bytes());
            CString::new(kv).ok()
        })
        .collect();

    let candidates: Vec<Vec<u8>> = if program.contains('/') {
        vec![program.as_bytes().to_vec()]
    } else {
        // os.get_exec_path(): PATH, or os.defpath when it is unset.
        let path = std::env::var_os("PATH")
            .map(|p| p.as_bytes().to_vec())
            .unwrap_or_else(|| b"/bin:/usr/bin".to_vec());
        path.split(|&b| b == b':')
            .map(|dir| {
                let mut c = dir.to_vec();
                if !c.is_empty() && !c.ends_with(b"/") {
                    c.push(b'/');
                }
                c.extend_from_slice(program.as_bytes());
                c
            })
            .collect()
    };

    let os_err = |e: nix::Error| SpawnError::Os(py_errno_str(e as i32));
    let mut actions = PosixSpawnFileActions::init().map_err(os_err)?;
    if let Some(fd) = stdin {
        actions.add_dup2(fd.as_raw_fd(), 0).map_err(os_err)?;
    }
    actions.add_dup2(stdout.as_raw_fd(), 1).map_err(os_err)?;
    actions.add_dup2(stderr.as_raw_fd(), 2).map_err(os_err)?;
    // close_fds=True: every descriptor above 2 is closed in the child, not
    // only the ones opened with O_CLOEXEC. (A descriptor that is gone by the
    // time the child runs is skipped: glibc ignores EBADF for a close
    // action.)
    for fd in open_fds() {
        if fd > 2 {
            actions.add_close(fd).map_err(os_err)?;
        }
    }
    let mut attr = PosixSpawnAttr::init().map_err(os_err)?;
    // restore_signals: what the parent ignores (SIGPIPE, SIGXFSZ) is back
    // to the default in the child.
    let mut sigdef = nix::sys::signal::SigSet::empty();
    sigdef.add(nix::sys::signal::Signal::SIGPIPE);
    sigdef.add(nix::sys::signal::Signal::SIGXFSZ);
    attr.set_sigdefault(&sigdef).map_err(os_err)?;
    let mut flags = PosixSpawnFlags::POSIX_SPAWN_SETSIGDEF;
    if start_new_session {
        flags |= PosixSpawnFlags::from_bits_retain(libc::POSIX_SPAWN_SETSID as libc::c_int);
    }
    attr.set_flags(flags).map_err(os_err)?;

    let mut saved: Option<nix::Error> = None;
    let mut last = nix::Error::ENOENT;
    for cand in &candidates {
        let Ok(path) = CString::new(cand.clone()) else { return Err(nul()) };
        match posix_spawn(path.as_c_str(), &actions, &attr, &argv, &envp) {
            Ok(pid) => return Ok(pid),
            Err(e) => {
                if e != nix::Error::ENOENT && e != nix::Error::ENOTDIR && saved.is_none() {
                    saved = Some(e);
                }
                last = e;
            }
        }
    }
    // Report the first exec error, not the last.
    let e = saved.unwrap_or(last);
    Err(SpawnError::Os(format!("{}: {}", py_errno_str(e as i32), py_str_repr(program))))
}

/// The descriptors open in this process, as `_posixsubprocess` finds them
/// for close_fds: the entries of /proc/self/fd (or /dev/fd).
fn open_fds() -> Vec<i32> {
    for dir in ["/proc/self/fd", "/dev/fd"] {
        if let Ok(rd) = std::fs::read_dir(dir) {
            return rd
                .filter_map(|e| e.ok()?.file_name().to_str()?.parse::<i32>().ok())
                .collect();
        }
    }
    Vec::new()
}

/// `Popen.returncode` from a wait status: the exit code, or -N for death
/// by signal N.
fn returncode(st: nix::sys::wait::WaitStatus) -> Option<i32> {
    use nix::sys::wait::WaitStatus;
    match st {
        WaitStatus::Exited(_, code) => Some(code),
        WaitStatus::Signaled(_, sig, _) => Some(-(sig as i32)),
        _ => None,
    }
}

/// A started child: the parts of `subprocess.Popen` the task uses.
pub struct Proc {
    pid: nix::unistd::Pid,
    /// `returncode`; the lock is `_waitpid_lock` too: `wait()` holds it
    /// while it blocks, and `poll()` answers "still running" when it is
    /// taken.
    returncode: Mutex<Option<i32>>,
    /// `proc.stdout`, open for as long as the Popen is referenced.
    stdout: std::io::PipeReader,
}

impl Proc {
    /// `Popen.poll()`.
    fn poll(&self) -> Option<i32> {
        use nix::sys::wait::{waitpid, WaitPidFlag};
        let mut rc = match self.returncode.try_lock() {
            Ok(rc) => rc,
            Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return None,
        };
        if rc.is_none() {
            match waitpid(self.pid, Some(WaitPidFlag::WNOHANG)) {
                Ok(st) => {
                    if let Some(code) = returncode(st) {
                        *rc = Some(code);
                    }
                }
                Err(nix::Error::ECHILD) => *rc = Some(0),
                Err(_) => {}
            }
        }
        *rc
    }

    /// `Popen.wait()`.
    fn wait(&self) -> i32 {
        use nix::sys::wait::waitpid;
        let mut rc = self.returncode.lock().unwrap_or_else(|e| e.into_inner());
        while rc.is_none() {
            match waitpid(self.pid, None) {
                Ok(st) => *rc = returncode(st),
                Err(nix::Error::EINTR) => {}
                // _try_wait: ChildProcessError reads as status 0.
                Err(_) => *rc = Some(0),
            }
        }
        rc.unwrap_or(0)
    }

    /// `Popen.terminate()`: SIGTERM unless it has already been reaped;
    /// ProcessLookupError is swallowed, anything else raised.
    fn terminate(&self) -> Result<(), String> {
        if self.poll().is_some() {
            return Ok(());
        }
        match nix::sys::signal::kill(self.pid, nix::sys::signal::Signal::SIGTERM) {
            Ok(()) | Err(nix::Error::ESRCH) => Ok(()),
            Err(e) => Err(py_errno_str(e as i32)),
        }
    }
}

/// `subprocess.Popen(cmd, stdout=PIPE, stderr=STDOUT, stdin=DEVNULL,
/// text=True, bufsize=1, start_new_session=True)`.
fn spawn(cmd: &[String]) -> Result<Proc, SpawnError> {
    use std::os::fd::AsFd;
    let io_err = |e: std::io::Error| SpawnError::Os(py_errno_str(e.raw_os_error().unwrap_or(0)));
    // DEVNULL is os.open(os.devnull, os.O_RDWR): a child may write to fd 0.
    // The OSError names the file.
    let devnull = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")
        .map_err(|e| SpawnError::Os(format!("{}: '/dev/null'", py_errno_str(e.raw_os_error().unwrap_or(0)))))?;
    let (reader, writer) = std::io::pipe().map_err(io_err)?;
    let pid = py_popen(cmd, Some(devnull.as_fd()), writer.as_fd(), writer.as_fd(), true)?;
    // Our copy of the write end closes here, so the read end sees EOF once
    // the child and its children are done.
    drop(writer);
    Ok(Proc { pid, returncode: Mutex::new(None), stdout: reader })
}

/// The codeset Python's `text=True` decodes with:
/// `locale.getpreferredencoding(False)`, as the interpreter would have set it
/// up at startup from this environment.
///
/// * `PYTHONUTF8=1` is UTF-8 mode: always UTF-8.
/// * Unset, UTF-8 mode switches itself on when the LC_CTYPE locale is "C" or
///   "POSIX" -- including a locale that is named but not installed, since
///   `setlocale` then fails and leaves "C" (PEP 540).
/// * `PYTHONUTF8=0` keeps the locale's own codeset, except that a "C" locale
///   is coerced to C.UTF-8 unless LC_ALL is set or `PYTHONCOERCECLOCALE=0`
///   (PEP 538); an uncoerced "C" is ASCII.
///
/// Python asks `nl_langinfo(CODESET)`; that needs `unsafe` here, so the same
/// answer comes from `locale charmap` under the LC_CTYPE locale. Worked out
/// once, as Python does at startup. (The same rule as `patches`.)
fn preferred_encoding() -> &'static str {
    static ENC: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ENC.get_or_init(|| {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let utf8_mode = var("PYTHONUTF8");
        if utf8_mode.as_deref() == Some("1") {
            return "UTF-8".to_string();
        }
        let name = var("LC_ALL").or_else(|| var("LC_CTYPE")).or_else(|| var("LANG"))
            .unwrap_or_else(|| "C".to_string());
        if name == "C" || name == "POSIX" {
            if utf8_mode.as_deref() != Some("0") {
                return "UTF-8".to_string();
            }
        }
        let out = crate::util::run_with(&["locale", "charmap"], &crate::util::RunOpts {
            timeout: Some(Duration::from_secs(5)),
            env: vec![("LC_ALL".to_string(), name.clone())],
            ..Default::default()
        });
        let charmap = out.stdout.trim().to_string();
        let is_c = name == "C" || name == "POSIX" || !out.ok() || !out.stderr.trim().is_empty()
            || charmap.is_empty() || charmap == "ANSI_X3.4-1968";
        if !is_c {
            return charmap;
        }
        if utf8_mode.as_deref() != Some("0") {
            return "UTF-8".to_string();
        }
        let coerce = var("LC_ALL").is_none() && var("PYTHONCOERCECLOCALE").as_deref() != Some("0");
        if coerce { "UTF-8" } else { "ANSI_X3.4-1968" }.to_string()
    })
}

fn is_utf8_codeset(enc: &str) -> bool {
    let e = enc.to_ascii_lowercase().replace(['-', '_'], "");
    e == "utf8"
}

/// Decodes the next chunk of a strict text stream the way Python's
/// incremental decoder does, in the locale's codeset (see
/// `preferred_encoding`): `pending` holds the incomplete sequence the
/// previous chunk ended in. Returns the complete text as UTF-8 bytes, or the
/// UnicodeDecodeError.
fn decode_chunk(pending: &mut Vec<u8>, chunk: &[u8], final_: bool) -> Result<Vec<u8>, String> {
    decode_chunk_as(preferred_encoding(), pending, chunk, final_)
}

fn decode_chunk_as(enc: &str, pending: &mut Vec<u8>, chunk: &[u8], final_: bool) -> Result<Vec<u8>, String> {
    let mut data = std::mem::take(pending);
    data.extend_from_slice(chunk);
    if !is_utf8_codeset(enc) {
        return decode_legacy(enc, pending, data, final_).map_err(|(_, msg)| msg);
    }
    match std::str::from_utf8(&data) {
        Ok(_) => Ok(data),
        Err(e) => {
            let at = e.valid_up_to();
            match e.error_len() {
                None if !final_ => {
                    *pending = data.split_off(at);
                    Ok(data)
                }
                None => Err(format!(
                    "UnicodeDecodeError: 'utf-8' codec can't decode byte 0x{:02x} in position {at}: unexpected end of data",
                    data[at]
                )),
                Some(_) => {
                    let b = data[at];
                    let why = if (0x80..0xc2).contains(&b) || b >= 0xf5 {
                        "invalid start byte"
                    } else {
                        "invalid continuation byte"
                    };
                    Err(format!(
                        "UnicodeDecodeError: 'utf-8' codec can't decode byte 0x{b:02x} in position {at}: {why}"
                    ))
                }
            }
        }
    }
}

/// A codeset other than UTF-8: ASCII and Latin-1 exactly as Python's codecs,
/// anything else through iconv (glib's `g_convert`), which has the same
/// tables. Python names a single-byte table's failure the 'charmap' codec.
/// The error carries the offset in `data` of the byte that would not decode
/// (None for an encoding iconv does not know) beside the message.
fn decode_legacy(
    enc: &str,
    pending: &mut Vec<u8>,
    data: Vec<u8>,
    final_: bool,
) -> Result<Vec<u8>, (Option<usize>, String)> {
    let lower = enc.to_ascii_lowercase();
    if lower == "ansi_x3.4-1968" || lower == "ascii" || lower == "us-ascii" {
        if let Some(at) = data.iter().position(|&b| b >= 0x80) {
            return Err((Some(at), format!(
                "UnicodeDecodeError: 'ascii' codec can't decode byte 0x{:02x} in position {at}: ordinal not in range(128)",
                data[at]
            )));
        }
        return Ok(data);
    }
    if matches!(lower.as_str(), "iso-8859-1" | "iso8859-1" | "latin1" | "latin-1") {
        return Ok(data.iter().map(|&b| b as char).collect::<String>().into_bytes());
    }
    let single_byte = lower.starts_with("iso-8859") || lower.starts_with("iso8859")
        || lower.starts_with("koi8") || lower.starts_with("cp") || lower.starts_with("windows-");
    let bad = |at: usize| {
        let b = data.get(at).copied().unwrap_or(0);
        if single_byte {
            format!("UnicodeDecodeError: 'charmap' codec can't decode byte 0x{b:02x} in position {at}: character maps to <undefined>")
        } else {
            format!("UnicodeDecodeError: '{lower}' codec can't decode byte 0x{b:02x} in position {at}: illegal multibyte sequence")
        }
    };
    // g_convert stops at an incomplete sequence at the end and says how far
    // it read: the rest waits for the next chunk, or is an error at the end
    // of the stream.
    match glib::convert(&data, "UTF-8", enc) {
        Ok((out, read)) if read < data.len() => {
            if final_ {
                return Err((Some(read), format!(
                    "UnicodeDecodeError: '{lower}' codec can't decode byte 0x{:02x} in position {read}: incomplete multibyte sequence",
                    data[read]
                )));
            }
            *pending = data[read..].to_vec();
            Ok(out.to_vec())
        }
        Ok((out, _)) => Ok(out.to_vec()),
        Err(glib::CvtError::IllegalSequence { offset, .. }) => Err((Some(offset), bad(offset))),
        Err(glib::CvtError::Convert(_)) => Err((None, format!("LookupError: unknown encoding: {enc}"))),
    }
}

/// `decode_chunk_as`, forgiving: a byte that does not decode becomes U+FFFD
/// and the text after it goes on, the way `errors="replace"` reads. A task's
/// log is for reading; one stray byte from a build tool must not stop it
/// being read (strict decoding used to end the worker there, leaving the
/// child unreaped and the dialog unable to close). An incomplete sequence at
/// the end of a chunk still waits for the next one.
fn decode_chunk_lossy(enc: &str, pending: &mut Vec<u8>, chunk: &[u8], final_: bool) -> Vec<u8> {
    const REPLACEMENT: &[u8] = "\u{FFFD}".as_bytes();
    let mut data = std::mem::take(pending);
    data.extend_from_slice(chunk);
    let mut out = Vec::with_capacity(data.len());
    if !is_utf8_codeset(enc) {
        loop {
            match decode_legacy(enc, pending, data.clone(), final_) {
                Ok(text) => {
                    out.extend(text);
                    return out;
                }
                Err((Some(at), _)) if at < data.len() => {
                    // What comes before the bad byte decodes on its own.
                    if let Ok(text) = decode_legacy(enc, &mut Vec::new(), data[..at].to_vec(), true) {
                        out.extend(text);
                    }
                    out.extend_from_slice(REPLACEMENT);
                    data.drain(..=at);
                }
                // A codeset iconv does not know: read it as UTF-8.
                Err(_) => {
                    out.extend_from_slice(String::from_utf8_lossy(&data).as_bytes());
                    return out;
                }
            }
        }
    }
    let mut rest = data.as_slice();
    loop {
        match std::str::from_utf8(rest) {
            Ok(_) => {
                out.extend_from_slice(rest);
                return out;
            }
            Err(e) => {
                let at = e.valid_up_to();
                out.extend_from_slice(&rest[..at]);
                match e.error_len() {
                    Some(n) => {
                        out.extend_from_slice(REPLACEMENT);
                        rest = &rest[at + n..];
                    }
                    None if !final_ => {
                        *pending = rest[at..].to_vec();
                        return out;
                    }
                    None => {
                        out.extend_from_slice(REPLACEMENT);
                        return out;
                    }
                }
            }
        }
    }
}

/// Reads `reader` the way Python iterates a text-mode pipe: chunks of up to
/// 8192 bytes, decoded in the locale's codeset (a byte that does not decode
/// shows as U+FFFD, see `decode_chunk_lossy`), universal newlines (`\r\n`,
/// `\r` and `\n` all end a line), each line handed over without its
/// terminator, a last unterminated line included. A read error is an `Err`.
fn for_each_line(mut reader: impl std::io::Read, mut f: impl FnMut(&str)) -> Result<(), String> {
    let mut buf = [0u8; 8192];
    let mut pending: Vec<u8> = Vec::new();
    let mut line: Vec<u8> = Vec::new();
    let mut after_cr = false;
    let mut emit = |line: &mut Vec<u8>| {
        // Whole lines of decoded UTF-8: \r and \n never sit inside a
        // multi-byte sequence.
        f(&String::from_utf8_lossy(line));
        line.clear();
    };
    loop {
        let n = match reader.read(&mut buf) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("OSError: {}", py_errno_str(e.raw_os_error().unwrap_or(0)))),
        };
        let data = decode_chunk_lossy(preferred_encoding(), &mut pending, &buf[..n], n == 0);
        for &b in &data {
            if after_cr {
                after_cr = false;
                if b == b'\n' {
                    continue;
                }
            }
            match b {
                b'\n' | b'\r' => {
                    after_cr = b == b'\r';
                    emit(&mut line);
                }
                _ => line.push(b),
            }
        }
        if n == 0 {
            break;
        }
    }
    if !line.is_empty() {
        emit(&mut line);
    }
    Ok(())
}

/// The body of `TaskDialog._work`, without the window: runs the steps in
/// order, reporting each label through `set_label` and every line through
/// `log` (which adds the newline). Returns `ok and not cancelled`, what
/// `_finish` is called with; `Err` is an exception the Python does not
/// catch, which ends the thread without calling `_finish`.
fn work(
    steps: &[Step],
    cancelled: &AtomicBool,
    proc_slot: &Mutex<Option<Arc<Proc>>>,
    set_label: &dyn Fn(&str),
    log: LogFn<'_>,
    keep_going: bool,
) -> Result<bool, String> {
    let mut ok = true;
    for step in steps {
        if cancelled.load(Ordering::SeqCst) {
            ok = false;
            break;
        }
        set_label(&step.label);

        let argv = match &step.action {
            // A step can be work done in this process rather than a command --
            // downloading and registering an AppImage, say. It takes the log
            // function and raises to fail.
            StepAction::Call(f) => {
                if let Err(exc) = f(log) {
                    log(&format!("failed: {exc}"));
                    ok = false;
                    if !keep_going {
                        break;
                    }
                }
                log("");
                continue;
            }
            StepAction::Argv(a) => a,
        };

        let cmd = elevate(argv);
        log(&cmdline(&cmd));
        let proc = match spawn(&cmd) {
            Ok(p) => Arc::new(p),
            Err(SpawnError::Os(exc)) => {
                log(&format!("could not start: {exc}"));
                ok = false;
                if !keep_going {
                    break;
                }
                log("");
                continue;
            }
            Err(SpawnError::Other(exc)) => return Err(exc),
        };
        // kept so Cancel can stop it
        *proc_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(proc.clone());
        let read = for_each_line(&proc.stdout, |line| log(line));
        if let Err(exc) = &read {
            // Nobody reads its output any more: stop it (it leads a session
            // of its own, so its group is its pid) rather than leave it to
            // block on a full pipe, and still reap it below.
            log(&format!("could not read its output: {exc}"));
            let _ = nix::sys::signal::killpg(proc.pid, nix::sys::signal::Signal::SIGKILL);
        }
        let code = proc.wait();
        *proc_slot.lock().unwrap_or_else(|e| e.into_inner()) = None;
        if read.is_err() {
            ok = false;
            if !keep_going {
                break;
            }
        } else if code != 0 {
            log(&format!("— exited {code} —"));
            ok = false;
            if !keep_going {
                break;
            }
        }
        log("");
    }
    Ok(ok && !cancelled.load(Ordering::SeqCst))
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

/// [`run`], but one failed step does not stop the others.
pub fn run_each(
    parent: &impl gtk::glib::object::IsA<gtk::Widget>,
    title: &str,
    steps: Vec<Step>,
    on_done: Option<OnDone>,
) -> TaskDialog {
    TaskDialog::build(parent, title, steps, on_done, true)
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
    let dialog = adw::AlertDialog::new(Some(c_str(heading)), Some(c_str(body)));
    if !detail.is_empty() {
        let buf = gtk::TextBuffer::new(None);
        buf.set_text(c_str(detail));
        let text = gtk::TextView::builder()
            .buffer(&buf)
            .editable(false)
            .monospace(true)
            .cursor_visible(false)
            .build();
        text.set_wrap_mode(gtk::WrapMode::WordChar);
        text.set_left_margin(8);
        text.set_right_margin(8);
        text.set_top_margin(6);
        text.set_bottom_margin(6);
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_min_content_height(170);
        scroll.set_max_content_height(300);
        scroll.set_child(Some(&text));
        scroll.add_css_class("card");
        dialog.set_extra_child(Some(&scroll));
    }

    dialog.add_response("cancel", "Cancel");
    dialog.add_response("go", c_str(action));
    dialog.set_response_appearance(
        "go",
        if destructive {
            adw::ResponseAppearance::Destructive
        } else {
            adw::ResponseAppearance::Suggested
        },
    );
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.connect_response(None, move |_d, resp| {
        if resp == "go" {
            on_yes();
        }
    });
    dialog.present(Some(parent));
}

/// What `pacman -Rs` would actually take out, without root.
///
/// -Rns cannot be previewed (--nosave and --print are mutually exclusive), so
/// the preview runs -Rs. The set of packages is the same; only the treatment
/// of config files differs, and that is stated in the dialog.
///
/// (An exception the Python lets through to its caller -- a NUL byte in a
/// name, output that is not UTF-8 -- comes back as `(false, "Type:
/// message")` here; `preview_removal_checked` reports it as `Err`.)
pub fn preview_removal(names: &[String]) -> (bool, String) {
    preview_removal_checked(names).unwrap_or_else(|exc| (false, exc))
}

/// `preview_removal`, with what the Python raises to its caller as `Err`.
pub fn preview_removal_checked(names: &[String]) -> Result<(bool, String), String> {
    let mut argv: Vec<String> =
        ["pacman", "-Rs", "--print", "--print-format", "%n %v"].iter().map(|s| s.to_string()).collect();
    argv.extend(names.iter().cloned());
    // subprocess.run(argv, capture_output=True, text=True, timeout=60):
    // stdin is the app's own.
    let out = match py_run_capture(&argv, Duration::from_secs(60)) {
        Ok(out) => out,
        // except (OSError, subprocess.SubprocessError) as exc: str(exc)
        Err(SpawnError::Os(exc)) => return Ok((false, exc)),
        Err(SpawnError::Other(exc)) => return Err(exc),
    };
    let Some((code, stdout, stderr)) = out else {
        // subprocess.TimeoutExpired
        let list: Vec<String> = argv.iter().map(|a| py_str_repr(a)).collect();
        return Ok((false, format!("Command '[{}]' timed out after 60 seconds", list.join(", "))));
    };
    let stdout = py_text(stdout)?;
    let stderr = py_text(stderr)?;
    if code != 0 {
        let text = if stderr.is_empty() { &stdout } else { &stderr };
        return Ok((false, py_strip(text).to_string()));
    }
    Ok((true, py_strip(&stdout).to_string()))
}

/// What `text=True` makes of captured bytes: strict UTF-8 (a bad byte
/// raises UnicodeDecodeError) with universal newlines.
fn py_text(bytes: Vec<u8>) -> Result<String, String> {
    let mut pending = Vec::new();
    let data = decode_chunk(&mut pending, &bytes, true)?;
    let s = String::from_utf8_lossy(&data);
    Ok(s.replace("\r\n", "\n").replace('\r', "\n"))
}

/// `subprocess.run(argv, capture_output=True, timeout=...)` without the
/// decoding: `Some((returncode, stdout, stderr))`, or `None` when it timed
/// out (the child is killed and reaped, as `run` does).
#[allow(clippy::type_complexity)]
fn py_run_capture(argv: &[String], timeout: Duration) -> Result<Option<(i32, Vec<u8>, Vec<u8>)>, SpawnError> {
    use std::io::Read;
    use std::os::fd::AsFd;
    use std::sync::mpsc;
    let io_err = |e: std::io::Error| SpawnError::Os(py_errno_str(e.raw_os_error().unwrap_or(0)));
    let (out_r, out_w) = std::io::pipe().map_err(io_err)?;
    let (err_r, err_w) = std::io::pipe().map_err(io_err)?;
    let pid = py_popen(argv, None, out_w.as_fd(), err_w.as_fd(), false)?;
    drop(out_w);
    drop(err_w);
    let proc = Proc { pid, returncode: Mutex::new(None), stdout: out_r };
    let start = std::time::Instant::now();

    // communicate(timeout=...): both pipes to EOF, then the exit status, all
    // within the timeout.
    let (tx, rx) = mpsc::channel::<(usize, std::io::Result<Vec<u8>>)>();
    let readers: Vec<Box<dyn Read + Send>> =
        vec![Box::new(proc.stdout.try_clone().map_err(io_err)?), Box::new(err_r)];
    for (i, mut r) in readers.into_iter().enumerate() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let res = r.read_to_end(&mut buf).map(|_| buf);
            let _ = tx.send((i, res));
        });
    }
    drop(tx);
    let mut got: [Option<Vec<u8>>; 2] = [None, None];
    let timed_out = |proc: &Proc| {
        let _ = nix::sys::signal::kill(proc.pid, nix::sys::signal::Signal::SIGKILL);
        proc.wait();
    };
    while got.iter().any(Option::is_none) {
        let left = timeout.saturating_sub(start.elapsed());
        match rx.recv_timeout(left) {
            Ok((i, Ok(buf))) => got[i] = Some(buf),
            Ok((_, Err(e))) => {
                timed_out(&proc);
                return Err(io_err(e));
            }
            Err(_) => {
                timed_out(&proc);
                return Ok(None);
            }
        }
    }
    loop {
        if let Some(code) = proc.poll() {
            let [out, err] = got;
            return Ok(Some((code, out.unwrap_or_default(), err.unwrap_or_default())));
        }
        if start.elapsed() >= timeout {
            timed_out(&proc);
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    /// A batch of unrelated updates runs every step; the first failure
    /// still fails the whole, and a plain task still stops at it.
    #[test]
    fn keep_going_runs_past_a_failed_step() {
        let ran = Arc::new(Mutex::new(Vec::new()));
        let step = |name: &'static str, ok: bool| {
            let ran = ran.clone();
            Step::call(name, Arc::new(move |_log: LogFn<'_>| {
                ran.lock().unwrap().push(name);
                if ok { Ok(()) } else { Err(format!("{name} broke")) }
            }))
        };
        let steps = vec![step("a", true), step("b", false), step("c", true)];
        let lines = Mutex::new(Vec::new());
        let log = |l: &str| lines.lock().unwrap().push(l.to_string());
        let run = |keep_going| {
            ran.lock().unwrap().clear();
            work(&steps, &AtomicBool::new(false), &Mutex::new(None), &|_| {}, &log, keep_going)
        };
        assert_eq!(run(true), Ok(false));
        assert_eq!(*ran.lock().unwrap(), vec!["a", "b", "c"]);
        assert!(lines.lock().unwrap().contains(&"failed: b broke".to_string()));
        assert_eq!(run(false), Ok(false));
        assert_eq!(*ran.lock().unwrap(), vec!["a", "b"]);
    }

    #[test]
    fn test_journal_vacuum_runs_as_root() {
        // As the user it deletes nothing, prints "freed 0B" and exits 0.
        assert_eq!(elevate(&v(&["journalctl", "--vacuum-size=500M"]))[0], "pkexec");
        assert!(!needs_root(&v(&["journalctl", "--disk-usage"])));
    }

    #[test]
    fn universal_newlines() {
        let mut got = Vec::new();
        for_each_line(&b"a\r\nb\rc\n\nd"[..], |l| got.push(l.to_string())).unwrap();
        assert_eq!(got, ["a", "b", "c", "", "d"]);
    }

    #[test]
    fn repr_like_python() {
        assert_eq!(py_str_repr("no\u{2028}x"), "'no\\u2028x'");
        assert_eq!(py_str_repr("a\u{a0}b\u{85}"), "'a\\xa0b\\x85'");
        assert_eq!(py_str_repr("it's"), "\"it's\"");
        assert_eq!(py_str_repr("é ü 日本"), "'é ü 日本'");
    }

    #[test]
    fn strip_like_python() {
        assert_eq!(py_strip("\x1c x\x1f\n"), "x");
        assert_eq!(py_strip("\u{3000}x\u{a0}"), "x");
    }

    /// Output that is not UTF-8 is read with the bad bytes replaced; it
    /// used to end the worker before the child was reaped or the dialog
    /// could close.
    #[test]
    fn bad_utf8_is_replaced_not_raised() {
        let mut got = Vec::new();
        for_each_line(&b"ok\n\xff\xfe\nnext"[..], |l| got.push(l.to_string())).unwrap();
        assert_eq!(got, ["ok", "\u{FFFD}\u{FFFD}", "next"]);
        got.clear();
        for_each_line(&b"ok\n\xc3"[..], |l| got.push(l.to_string())).unwrap();
        assert_eq!(got, ["ok", "\u{FFFD}"]);
    }

    /// A character split across two reads is still one character.
    #[test]
    fn a_sequence_split_between_reads_decodes() {
        struct OneByte<'a>(&'a [u8]);
        impl std::io::Read for OneByte<'_> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let Some((&b, rest)) = self.0.split_first() else { return Ok(0) };
                buf[0] = b;
                self.0 = rest;
                Ok(1)
            }
        }
        let mut got = Vec::new();
        for_each_line(OneByte("é日本\n".as_bytes()), |l| got.push(l.to_string())).unwrap();
        assert_eq!(got, ["é日本"]);
        let mut p = Vec::new();
        assert_eq!(decode_chunk_lossy("ISO-8859-1", &mut p, b"\xe9", true), "é".as_bytes());
        assert_eq!(decode_chunk_lossy("ANSI_X3.4-1968", &mut p, b"a\xe9b", true), "a\u{FFFD}b".as_bytes());
        assert_eq!(decode_chunk_lossy("EUC-JP", &mut p, b"a\xa4", false), b"a");
        assert_eq!(decode_chunk_lossy("EUC-JP", &mut p, b"\xa2\xff", true), "あ\u{FFFD}".as_bytes());
    }

    /// A command that prints bytes that are not UTF-8 runs to its end: its
    /// output is logged, it is waited for (no zombie), and the task reports
    /// its exit status rather than dying silently.
    #[test]
    fn a_command_printing_invalid_utf8_finishes() {
        let steps = vec![
            Step::argv("bad bytes", v(&["sh", "-c", "printf 'a\\377b\\n'; exit 3"])),
            Step::argv("after", v(&["true"])),
        ];
        let lines = Mutex::new(Vec::new());
        let log = |l: &str| lines.lock().unwrap().push(l.to_string());
        let slot = Mutex::new(None);
        assert_eq!(work(&steps, &AtomicBool::new(false), &slot, &|_| {}, &log, true), Ok(false));
        let lines = lines.into_inner().unwrap();
        assert!(lines.contains(&"a\u{FFFD}b".to_string()), "{lines:?}");
        assert!(lines.contains(&"— exited 3 —".to_string()), "{lines:?}");
        assert!(lines.contains(&"$ true".to_string()), "the next step still ran: {lines:?}");
        assert!(slot.lock().unwrap().is_none());
    }

    #[test]
    fn gtk_strings_end_at_nul() {
        assert_eq!(c_str("a\0b\n"), "a");
        assert_eq!(c_str("ab\n"), "ab\n");
        assert_eq!(c_str("\0"), "");
    }

    #[test]
    fn legacy_codesets() {
        let mut p = Vec::new();
        assert_eq!(decode_chunk_as("ISO-8859-1", &mut p, b"\xe9\xff", true).unwrap(), "\u{e9}\u{ff}".as_bytes());
        assert!(decode_chunk_as("ANSI_X3.4-1968", &mut p, b"ok\xe9", true).unwrap_err().contains("'ascii' codec"));
        assert_eq!(decode_chunk_as("ISO-8859-15", &mut p, b"\xa4", true).unwrap(), "\u{20ac}".as_bytes());
        // Multi-byte UTF-8 is not Latin-1 text: it decodes byte by byte.
        assert_eq!(decode_chunk_as("ISO-8859-1", &mut p, "é".as_bytes(), true).unwrap(), "Ã©".as_bytes());
        // An incomplete sequence waits for the next chunk.
        assert_eq!(decode_chunk_as("EUC-JP", &mut p, b"a\xa4", false).unwrap(), b"a");
        assert_eq!(decode_chunk_as("EUC-JP", &mut p, b"\xa2", true).unwrap(), "あ".as_bytes());
        assert!(decode_chunk_as("EUC-JP", &mut p, b"\xa4", true).is_err());
    }

    /// GTK for a widget test, or None to skip: a package build's check()
    /// has no display, and GTK belongs to whichever test thread took it
    /// first (as in `ui::packages`' widget test).
    fn gtk_here() -> Option<()> {
        let set = |var| std::env::var_os(var).is_some_and(|v| !v.is_empty());
        let broadway = std::env::var("GDK_BACKEND").is_ok_and(|b| b.contains("broadway"));
        if !(set("WAYLAND_DISPLAY") || set("DISPLAY") || broadway) {
            return None;
        }
        if !matches!(std::panic::catch_unwind(adw::init), Ok(Ok(()))) || gtk::gdk::Display::default().is_none() {
            return None;
        }
        Some(())
    }

    /// Spin the main loop until `done` says so, for at most `secs`.
    fn spin(secs: u64, done: impl Fn() -> bool) -> bool {
        let ctx = glib::MainContext::default();
        let end = std::time::Instant::now() + Duration::from_secs(secs);
        while !done() {
            if std::time::Instant::now() > end {
                return false;
            }
            while ctx.iteration(false) {}
            std::thread::sleep(Duration::from_millis(5));
        }
        true
    }

    /// A closed task dialog is freed, log and all: its buttons' handlers
    /// used to hold it strongly, a cycle that kept every one forever. A
    /// task whose worker ends in an error still finishes (it used to pulse
    /// on, unable to close).
    #[test]
    fn a_closed_task_dialog_is_freed() {
        if gtk_here().is_none() {
            return;
        }
        let parent = adw::Window::new();
        parent.present();
        // An argv with a NUL byte: the worker ends in an error.
        for steps in [
            vec![Step::call("ok", Arc::new(|log: LogFn<'_>| {
                log("working");
                Ok(())
            }))],
            vec![Step::argv("nul", v(&["true", "a\0b"]))],
        ] {
            let result = Rc::new(std::cell::Cell::new(None));
            let r = result.clone();
            let t = run(&parent, "Test", steps, Some(Box::new(move |ok| r.set(Some(ok)))));
            assert!(spin(10, || result.get().is_some()), "the task never finished");
            let (dialog, on_done, pulse) = (t.dialog.downgrade(), Rc::downgrade(&t.on_done), Rc::downgrade(&t.pulse));
            assert!(t.close_btn.is_visible());
            t.close_btn.emit_clicked();
            drop(t);
            assert!(
                spin(10, || dialog.upgrade().is_none() && on_done.upgrade().is_none() && pulse.upgrade().is_none()),
                "the closed dialog is still alive"
            );
        }
        parent.destroy();
    }

    #[test]
    fn quote_like_shlex() {
        assert_eq!(shlex_quote(""), "''");
        assert_eq!(shlex_quote("--vacuum-size=500M"), "--vacuum-size=500M");
        assert_eq!(shlex_quote("it's"), "'it'\"'\"'s'");
    }
}
