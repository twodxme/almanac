//! Update one and remove: the commands, the AppImage update and removal,
//! processes in a new session, and the window's handlers for them.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gio, glib};
use serde_json::{Map, Value};

use crate::{agents, appimage, inventory, patches, plugins, snapshots, tasks, verify};
use crate::i18n::tr;
use crate::inventory::App;
use crate::patches::PatchState;
use crate::tasks::{LogFn, Step, StepAction};
use crate::ui::shell::{Win, spawn_bg};

use super::pyisms::{extra_str, not_a_path, py_oserror, py_path, py_str, py_str_repr, py_type, strerror, truthy};
use super::raised;
use super::verification::{after_pacman_again, after_pacman_cb, app_judge, app_judge_again, UpdateLog};

/// The `yay` argv that installs or updates one package (`on_update_one`,
/// `_install_option`).
pub fn yay_argv(name: &str) -> Vec<String> {
    tasks::yay_install(&[name])
}

/// The test of `on_update_one`: a release only checkupdates knows about
/// cannot be installed alone. `synced` is `inventory.synced_version`, asked
/// only for a package that is not from the AUR.
pub fn needs_full_upgrade(app: &App, synced: impl FnOnce(&str) -> String) -> bool {
    app.source != inventory::AUR && {
        let s = synced(&app.name);
        !s.is_empty() && s != app.update
    }
}

/// What `on_update_one` does with an app.
#[derive(Debug, PartialEq)]
pub enum UpdateOne {
    /// `self._update_agent(app)`
    Agent,
    /// Ask to run `on_update_all` instead.
    FullUpgrade,
    /// Ask, then run this argv after a snapshot.
    Single(Vec<String>),
}

/// The branches of `on_update_one`.
pub fn update_one(app: &App, synced: impl FnOnce(&str) -> String) -> UpdateOne {
    if app.extra.get("update_cmd").is_some_and(truthy) {
        return UpdateOne::Agent;
    }
    if needs_full_upgrade(app, synced) {
        return UpdateOne::FullUpgrade;
    }
    UpdateOne::Single(yay_argv(&app.name))
}

/// `app.extra.get("update_cmd") or []` as `' '.join(cmd)` and `tasks.run`
/// see it: a string is its characters, a dict its keys. `Err` is the
/// TypeError the join raises.
pub(in crate::ui) fn update_cmd(app: &App) -> Result<Vec<String>, String> {
    match app.extra.get("update_cmd") {
        Some(v) if truthy(v) => match v {
            Value::String(s) => Ok(s.chars().map(String::from).collect()),
            Value::Array(a) => a
                .iter()
                .enumerate()
                .map(|(i, x)| match x {
                    Value::String(s) => Ok(s.clone()),
                    other => Err(format!(
                        "TypeError: sequence item {i}: expected str instance, {} found",
                        py_type(other)
                    )),
                })
                .collect(),
            Value::Object(o) => Ok(o.keys().cloned().collect()),
            _ => Err("TypeError: can only join an iterable".to_string()),
        },
        _ => Ok(Vec::new()),
    }
}

/// Download `app`'s newer AppImage over the current one and record it,
/// checked against the release's published sha256 when it has one.
/// `Err`: network, disk, permissions, a download that is not whole.
pub(in crate::ui) fn appimage_update_work(app: &App) -> Result<(), String> {
    let url = app.extra.get("download").cloned().unwrap_or_default();
    let sha256 = app.extra.get("sha256").and_then(Value::as_str).unwrap_or_default();
    let dest = py_path(&app.exec_path);
    // urllib's Request takes str(url).
    appimage::download_verified(&py_str(&url), &dest, None, sha256)?;
    let key = registry_key(app);
    let update = app.update.clone();
    let k = key_str(&key)?.map(str::to_string);
    // Read afresh under the registry's lock: only this entry changes.
    inventory::update_registry(|reg| {
        let Some(entry) = k.and_then(|k| reg.get_mut(&k)) else { return Ok(false) };
        let Some(entry) = entry.as_object_mut() else {
            return Err(match entry {
                Value::Array(_) => "list indices must be integers or slices, not str".to_string(),
                other => format!("'{}' object does not support item assignment", py_type(other)),
            });
        };
        entry.insert("version".to_string(), Value::String(update));
        // Force a re-check rather than trusting the stale entry.
        entry.insert("checked_at".to_string(), Value::from(0));
        entry.insert("latest".to_string(), Value::Object(Default::default()));
        Ok(true)
    })?;
    appimage::install_desktop_entry(&dest, &py_str(&key))?;
    Ok(())
}

/// `app.extra.get("key", app.name)`, as Python holds it.
fn registry_key(app: &App) -> Value {
    app.extra.get("key").cloned().unwrap_or_else(|| Value::String(app.name.clone()))
}

/// The registry entry `key` names, if a str: JSON keys are strings, so
/// nothing else matches. `Err` is the TypeError of an unhashable key.
fn key_str(key: &Value) -> Result<Option<&str>, String> {
    match key {
        Value::String(s) => Ok(Some(s)),
        Value::Array(_) | Value::Object(_) => {
            let t = py_type(key);
            Err(format!("cannot use '{t}' as a dict key (unhashable type: '{t}')"))
        }
        _ => Ok(None),
    }
}

/// The body of `_snapshot_first` once the snapshot step is known.
pub fn with_snapshot(step: Option<(String, snapshots::StepFn)>, steps: Vec<Step>) -> Vec<Step> {
    match step {
        Some((label, f)) => {
            let call: tasks::StepFn = Arc::new(move |log: LogFn<'_>| f(log));
            let mut out = vec![Step::call(label, call)];
            out.extend(steps);
            out
        }
        None => steps,
    }
}

/// The deleting part of `_remove_appimage`: the AppImage and the previous
/// copy kept beside it, nothing else. A file already gone is fine. `Err` is
/// the str() of the OSError.
pub fn delete_appimage_files(path: &Path) -> Result<(), String> {
    let gone = |p: &Path| match std::fs::remove_file(p) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(py_oserror(&e, p)),
        Ok(()) => Ok(()),
    };
    gone(path)?;
    gone(&path.with_extension("previous"))
}

/// What `_remove_appimage` ended with.
#[derive(Debug, PartialEq)]
pub enum Removal {
    /// "Could not delete ..." was said.
    NotDeleted(String),
    Removed,
}

/// `_remove_appimage` with the registry and desktop-entry calls passed in.
/// `Err` is what raised out of it.
pub fn remove_appimage_with(
    app: &App,
    load: impl FnOnce() -> Map<String, Value>,
    save: impl FnOnce(&Map<String, Value>) -> Result<(), String>,
    remove_entry: impl FnOnce(&str) -> Result<(), String>,
) -> Result<Removal, String> {
    let mut reg = load();
    let key = registry_key(app);
    let path = py_path(&app.exec_path);
    if let Err(e) = delete_appimage_files(&path) {
        let name = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        return Ok(Removal::NotDeleted(format!("Could not delete {name}: {e}")));
    }
    if let Some(k) = key_str(&key).map_err(|e| format!("TypeError: {e}"))? {
        reg.remove(k);
    }
    save(&reg)?;
    remove_entry(&py_str(&key))?;
    Ok(Removal::Removed)
}

/// `subprocess.Popen(argv, start_new_session=True)`. A GSubprocess, so the
/// child can call setsid(2) before its exec without unsafe code; like
/// Popen it resets SIGPIPE, closes other descriptors and reports a failed
/// exec as an error. The OSError Popen would raise when the program cannot
/// be run is found first, the way its execvp searches.
pub(in crate::ui) fn popen_new_session(argv: &[String]) -> Result<gio::Subprocess, String> {
    let Some(prog) = argv.first() else {
        return Err("[Errno 2] No such file or directory: ''".to_string());
    };
    if let Some(n) = exec_errno(prog) {
        return Err(errno_text(n, prog));
    }
    let launcher = gio::SubprocessLauncher::new(gio::SubprocessFlags::STDIN_INHERIT);
    launcher.set_child_setup(|| {
        let _ = rustix::process::setsid();
    });
    let args: Vec<&std::ffi::OsStr> = argv.iter().map(std::ffi::OsStr::new).collect();
    launcher.spawn(&args).map_err(|e| match e.kind::<SpawnErrno>() {
        Some(SpawnErrno(Some(n))) => errno_text(n, prog),
        _ => e.message().to_string(),
    })
}

/// `str(OSError)` of a failed exec: `[Errno 2] No such file or directory: 'x'`.
fn errno_text(n: i32, prog: &str) -> String {
    let e = std::io::Error::from_raw_os_error(n);
    format!("[Errno {n}] {}: {}", strerror(&e), py_str_repr(prog))
}

/// A `G_SPAWN_ERROR` code, as the errno the exec failed with (`None` for
/// the codes that are not an exec errno: fork, read, chdir, failed).
#[derive(Clone, Copy)]
struct SpawnErrno(Option<i32>);

impl glib::error::ErrorDomain for SpawnErrno {
    fn domain() -> glib::Quark {
        glib::Quark::from_str("g-exec-error-quark")
    }

    fn code(self) -> i32 {
        -1
    }

    fn from(code: i32) -> Option<Self> {
        // GSpawnError order: FORK READ CHDIR ACCES PERM 2BIG NOEXEC
        // NAMETOOLONG NOENT NOMEM NOTDIR LOOP TXTBUSY IO NFILE MFILE INVAL
        // ISDIR LIBBAD FAILED.
        const ERRNO: [i32; 16] = [13, 1, 7, 8, 36, 2, 12, 20, 40, 26, 5, 23, 24, 22, 21, 80];
        Some(SpawnErrno(usize::try_from(code - 3).ok().and_then(|i| ERRNO.get(i).copied())))
    }
}

/// The errno `_posixsubprocess` would report for exec'ing `prog`, or
/// `None` when a candidate can be run: the first error that is not ENOENT
/// or ENOTDIR, else the last one.
fn exec_errno(prog: &str) -> Option<i32> {
    const ENOENT: i32 = 2;
    const EACCES: i32 = 13;
    const ENOTDIR: i32 = 20;
    let candidates: Vec<PathBuf> = if prog.contains('/') {
        vec![PathBuf::from(prog)]
    } else {
        let path = std::env::var("PATH").unwrap_or_else(|_| "/bin:/usr/bin".to_string());
        path.split(':').map(|d| Path::new(d).join(prog)).collect()
    };
    let (mut saved, mut last) = (None, ENOENT);
    for c in candidates {
        let err = match std::fs::metadata(&c) {
            Err(e) => e.raw_os_error().unwrap_or(ENOENT),
            Ok(m) if m.is_dir() => EACCES,
            Ok(_) => match rustix::fs::access(&c, rustix::fs::Access::EXEC_OK) {
                Ok(()) => return None,
                Err(e) => e.raw_os_error(),
            },
        };
        if err != ENOENT && err != ENOTDIR && saved.is_none() {
            saved = Some(err);
        }
        last = err;
    }
    Some(saved.unwrap_or(last))
}

/// `" ".join(argv)` of a step, for the confirmation preview; `None` for a
/// callable, where the join raises.
fn step_text(step: &Step) -> Option<String> {
    match &step.action {
        StepAction::Argv(argv) => Some(argv.join(" ")),
        StepAction::Call(_) => None,
    }
}

impl Win {
    /// AppImages are ours, so remove them directly -- no package manager is
    /// involved. Every caller has asked first: the file and its rollback
    /// copy are gone for good.
    pub fn remove_appimage(&self, app: &App) {
        self.remove_appimage_then(app, |_| {});
    }

    /// `remove_appimage`, then `then` once the removal has been tried
    /// (whatever came of it): it runs off the main thread, so what reads
    /// the registry after it waits for it here.
    pub fn remove_appimage_then(&self, app: &App, then: impl FnOnce(&Win) + 'static) {
        // Its read and its write of the registry as one, under the lock;
        // let go before anything (the reload) reads the registry again.
        // Off the main thread: the lock waits for whoever holds it (the
        // scheduled update holds it through its downloads).
        let (w, app) = (self.downgrade(), app.clone());
        let name = app.name.clone();
        crate::ui::shell::spawn_bg_checked(
            move || {
                inventory::lock_registry().and_then(|_lock| {
                    remove_appimage_with(&app, inventory::load_registry, inventory::save_registry, appimage::remove_desktop_entry)
                })
            },
            move |removed| {
                let Some(w) = w.upgrade() else { return };
                match removed.and_then(|r| r) {
                    Err(e) => raised(&e),
                    Ok(Removal::NotDeleted(msg)) => w.say(&msg, 5),
                    Ok(Removal::Removed) => {
                        w.say(&format!("Removed {name}"), 5);
                        w.reload();
                    }
                }
                then(&w);
            },
        );
    }

    /// Update one thing, with whatever tool owns it.
    pub fn on_update_one(&self, app: &App) {
        let argv = match update_one(app, inventory::synced_version) {
            UpdateOne::Agent => {
                self.update_agent(app);
                return;
            }
            // A release only checkupdates knows about cannot be installed on
            // its own: the system database would need -y first, and syncing
            // for one package is the partial upgrade Arch warns against.
            // Running yay anyway does nothing and reports success, so say
            // what will work.
            UpdateOne::FullUpgrade => {
                let w = self.downgrade();
                tasks::confirm(
                    &self.window,
                    &format!("{} needs a full upgrade", app.name),
                    &format!(
                        "{} was released after this system's package database was last \
                         refreshed, so it cannot be installed on its own. “Update all” \
                         refreshes the database and installs it along with everything it \
                         depends on.",
                        app.update
                    ),
                    "",
                    &tr("Update all"),
                    false,
                    move || {
                        if let Some(win) = w.upgrade() {
                            win.on_update_all();
                        }
                    },
                );
                return;
            }
            // `yay -S <pkg>` without -y installs against the database as it
            // stands. That is a partial upgrade, which Arch warns against, so
            // the warning is in the dialog and Update all remains the safe
            // default.
            UpdateOne::Single(argv) => argv,
        };
        let w = self.downgrade();
        let app_c = app.clone();
        tasks::confirm(
            &self.window,
            &format!("Update {} only?", app.name),
            &tr("Arch prefers full system upgrades. Updating one package against a \
                 database that has moved on is a partial upgrade, which can break \
                 linking. “Update all” is the safer choice."),
            &format!("{}  {} → {}\n\n$ {}", app.name, app.version, app.update, argv.join(" ")),
            &tr("Update anyway"),
            false,
            move || {
                let Some(win) = w.upgrade() else { return };
                let app = app_c.clone();
                let steps = win.snapshot_first(
                    vec![Step::argv(format!("Updating {}", app.name), argv.clone())],
                    &format!("updating {}", app.name),
                );
                let steps = match steps {
                    Ok(steps) => steps,
                    Err(e) => return raised(&e),
                };
                let w = win.downgrade();
                let title = format!("Updating {}", app.name);
                let log = Some(UpdateLog::new(&app, &argv));
                win.checked_logged(verify::for_app_try(&app), app_judge(&app), Some(after_pacman_cb(&win)), raised, log, move |done| {
                    if let Some(win) = w.upgrade() {
                        tasks::run(&win.window, &title, steps, Some(done));
                    }
                });
            },
        );
    }

    /// Agents update themselves, or their version manager does it.
    pub fn update_agent(&self, app: &App) {
        let cmd = update_cmd(app);
        if cmd.as_ref().is_ok_and(|c| c.is_empty()) {
            self.say(
                &format!(
                    "{} has no self-update command — {} is installed, {} is out",
                    app.name, app.version, app.update
                ),
                5,
            );
            if let Some(url) = app.extra.get("url").filter(|v| truthy(v)) {
                self.open_url_value(url);
            }
            return;
        }
        if agents::major_jump(&app.version, &app.update) {
            // A new major version is shown with what changed, and asked
            // about on its own: it can rename flags or move config.
            let repo = extra_str(app, "repo");
            let w = self.downgrade();
            let app_c = app.clone();
            // What release_notes raises ends the Python's worker: nothing
            // is asked.
            spawn_bg(
                move || agents::try_release_notes(&repo, 2000),
                move |notes| match notes {
                    Ok(notes) => {
                        if let Some(win) = w.upgrade() {
                            win.confirm_agent_cmd(&app_c, cmd, &notes);
                        }
                    }
                    Err(e) => raised(&e),
                },
            );
            return;
        }
        self.confirm_agent_cmd(app, cmd, "");
    }

    /// `_confirm_agent` with `cmd` as `update_cmd` reads it.
    fn confirm_agent_cmd(&self, app: &App, cmd: Result<Vec<String>, String>, notes: &str) {
        let manager = match app.extra.get("manager") {
            None => "self".to_string(),
            Some(v) => py_str(v),
        };
        let major = agents::major_jump(&app.version, &app.update);
        let heading = if major {
            format!("{} {} is a major release", app.name, app.update)
        } else {
            format!("Update {}?", app.name)
        };
        let body = if major {
            format!(
                "Major versions can change commands, flags and config formats. Read what \
                 changed first. Managed by {manager}; nothing here needs root."
            )
        } else {
            format!("Managed by {manager}. It replaces its own binary, so nothing here needs root.")
        };
        let cmd = match cmd {
            Ok(c) => c,
            Err(e) => return raised(&e),
        };
        let mut detail = format!("{}  {} → {}\n\n$ {}", app.name, app.version, app.update, cmd.join(" "));
        if !notes.is_empty() {
            detail += &format!("\n\nWhat changed\n{notes}");
        }
        let action = if !major { tr("Update") } else { format!("Update to {}", app.update) };
        let w = self.downgrade();
        let app_c = app.clone();
        tasks::confirm(&self.window, &heading, &body, &detail, &action, false, move || {
            let Some(win) = w.upgrade() else { return };
            let app = app_c.clone();
            let w = win.downgrade();
            let (title, cmd) = (format!("Updating {}", app.name), cmd.clone());
            let log = Some(UpdateLog::new(&app, &cmd));
            win.checked_logged(verify::for_app_try(&app), app_judge(&app), Some(after_pacman_cb(&win)), raised, log, move |done| {
                if let Some(win) = w.upgrade() {
                    tasks::run(&win.window, &title, vec![Step::argv(title.clone(), cmd)], Some(done));
                }
            });
        });
    }

    /// Update a local checkout, taking any local edits into account.
    ///
    /// A clean checkout is a fast-forward. One with edits is first held up
    /// against the update (patches.py): an edit the update already contains
    /// is dropped, one that still applies is carried over, and one that
    /// conflicts is set aside as a stash. Edits are never discarded.
    pub fn on_git_pull(&self, app: &App) {
        let Some(repo) = app.extra.get("repo").filter(|v| truthy(v)).cloned() else {
            return;
        };
        if extra_str(app, "plugin_state") == plugins::LOCAL {
            let detail = app.extra.get("plugin_detail").map(py_str).unwrap_or_default();
            self.say(&format!("{}: {}", app.name, detail), 8);
            return;
        }

        let w = self.downgrade();
        let app_c = app.clone();
        // patches._git runs `git -C str(repo)`.
        let repo_c = py_str(&repo);
        // What analyze raises ends the Python's worker: nothing is asked.
        spawn_bg(
            move || patches::try_analyze(&repo_c, None),
            move |state| {
                let state = match state {
                    Ok(state) => state,
                    Err(e) => return raised(&e),
                };
                if let Some(win) = w.upgrade() {
                    match &repo {
                        Value::String(repo) => win.confirm_pull(&app_c, repo, &state),
                        // `verify.git_head(repo)`'s argv, run by `_checked`
                        // at once, raises before anything is shown.
                        other => raised(&not_a_path(other)),
                    }
                }
            },
        );
    }

    pub fn confirm_pull(&self, app: &App, repo: &str, state: &PatchState) {
        let pull = match app.extra.get("plugin_id").filter(|v| truthy(v)) {
            None => None,
            Some(Value::String(pid)) => Some(plugins::update_argv(pid)),
            // update_argv(pid) puts it in an argv: joining it for the preview
            // raises, and so does quoting it for the log once the task runs
            // (port note: there the Python's dialog opens and stalls).
            Some(other) => {
                return raised(&format!(
                    "TypeError: sequence item 3: expected str instance, {} found",
                    py_type(other)
                ))
            }
        };
        let steps = patches::steps(repo, state, &app.name, pull.as_deref());
        let w = self.downgrade();
        let (app, state) = (app.clone(), state.clone());
        self.checked_again(verify::git_head_try(repo), app_judge_again(&app), after_pacman_again(self), move |done| {
            if let Some(win) = w.upgrade() {
                win.confirm_pull_with(&app, &state, steps, done);
            }
        });
    }

    /// `confirm_pull`, once the checkout's head before the pull is known.
    fn confirm_pull_with(&self, app: &App, state: &PatchState, steps: Vec<Step>, done: Rc<dyn Fn(bool)>) {
        if state.kind == patches::CLEAN || state.kind == patches::UNKNOWN {
            tasks::run(&self.window, &format!("Updating {}", app.name), steps, Some(Box::new(move |ok| done(ok))));
            return;
        }
        let (heading, body, action) = patches::explain(state, &app.name);
        let Some(lines) = steps.iter().map(|s| step_text(s).map(|t| format!("$ {t}"))).collect::<Option<Vec<_>>>() else {
            return raised("TypeError: can only join an iterable");
        };
        let mut detail = lines.join("\n");
        if !state.upstream_commits.is_empty() {
            detail += &format!(
                "\n\nUpstream changes to the same files\n{}",
                state.upstream_commits.iter().take(15).cloned().collect::<Vec<_>>().join("\n")
            );
        }
        let w = self.downgrade();
        let title = format!("Updating {}", app.name);
        tasks::confirm(&self.window, &heading, &body, &detail, &action, false, move || {
            let Some(win) = w.upgrade() else { return };
            let done = done.clone();
            tasks::run(&win.window, &title, steps.clone(), Some(Box::new(move |ok| done(ok))));
        });
    }

    /// Download the newer AppImage and swap it in, off the UI thread.
    pub fn on_update_appimage(&self, button: &gtk::Button, app: &App) {
        if !app.extra.get("download").is_some_and(truthy) {
            self.say(&format!("No download URL for {}", app.name), 5);
            return;
        }
        button.set_sensitive(false);
        self.summarize(&format!("Downloading {} {}…", app.name, app.update), None);

        let app_c = app.clone();
        let work = move || appimage_update_work(&app_c);
        let w = self.downgrade();
        let (name, version) = (app.name.clone(), app.update.clone());
        let url = app.extra.get("download").map(py_str).unwrap_or_default();
        let log = UpdateLog::new(app, &[format!("download {url}")]);
        spawn_bg(
            move || {
                // A panic is a bug, the Rust form of the TypeErrors and
                // KeyErrors `except Exception` also catches.
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)) {
                    Ok(Ok(())) => None,
                    Ok(Err(e)) => Some(e), // network, disk, perms
                    Err(p) => Some(
                        p.downcast_ref::<&str>().map(|s| s.to_string())
                            .or_else(|| p.downcast_ref::<String>().cloned())
                            .unwrap_or_default(),
                    ),
                }
            },
            move |error| {
                if let Some(win) = w.upgrade() {
                    // The download is checked against the release's sha256
                    // where it publishes one; a failure is said as is.
                    let out = match &error {
                        None => verify::Outcome::new(Some(true), format!("{name} updated to {version}")),
                        Some(e) => verify::Outcome::new(Some(false), format!("{name} update failed: {e}")),
                    };
                    win.record_update(&log, error.is_none(), Some(&out));
                    win.after_appimage(&name, &version, error);
                }
            },
        );
    }

    pub fn after_appimage(&self, name: &str, version: &str, error: Option<String>) {
        if let Some(error) = error {
            self.say(&format!("{name} update failed: {error}"), 5);
        } else {
            self.say(&format!("{name} updated to {version}"), 5);
            self.reload();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::pyisms::strs;

    /// test_a_release_the_system_db_lacks_is_sent_to_update_all: checkupdates
    /// saw 2.0 in a fresh private db; the system db still offers 1.0, so
    /// `yay -S --needed` would skip it and report success.
    #[test]
    fn a_release_the_system_db_lacks_is_sent_to_update_all() {
        let mut pkg = App::new("somepkg", "1.0", inventory::REPO);
        pkg.update = "2.0".into();
        assert_eq!(update_one(&pkg, |_| "1.0".to_string()), UpdateOne::FullUpgrade,
                   "a no-op install must not be run");
        assert_eq!(update_one(&pkg, |_| "2.0".to_string()), UpdateOne::Single(yay_argv("somepkg")));
        assert_eq!(update_one(&pkg, |_| String::new()), UpdateOne::Single(yay_argv("somepkg")));
        // The AUR is never asked about the system database.
        pkg.source = inventory::AUR.into();
        assert_eq!(update_one(&pkg, |_| panic!("asked")), UpdateOne::Single(yay_argv("somepkg")));
    }

    /// test_package_update_takes_a_snapshot_first
    #[test]
    fn package_update_takes_a_snapshot_first() {
        let mut pkg = App::new("somepkg", "1", inventory::REPO);
        pkg.update = "2".into();
        let UpdateOne::Single(argv) = update_one(&pkg, |_| "2".to_string()) else {
            panic!("a single package update was expected");
        };
        let main = vec![Step::argv("Updating somepkg", argv)];
        // The real step, where Snapper is set up on this machine (the Python
        // patches snapper_configs, which is private here).
        if let Ok(Some(step)) = snapshots::try_create_step("Almanac: before updating somepkg") {
            let steps = with_snapshot(Some(step), main.clone());
            assert_eq!(steps[0].label, "Taking a snapshot first");
            assert!(step_text(&steps[1]).unwrap_or_default().contains("yay -S"));
        }
        let snap: snapshots::StepFn = Box::new(|_log| Ok(()));
        let steps = with_snapshot(Some(("Taking a snapshot first".to_string(), snap)), main.clone());
        assert!(step_text(&steps[1]).unwrap_or_default().contains("yay -S"));
        // Without Snapper the steps are left as they were.
        assert_eq!(with_snapshot(None, main).len(), 1);
    }

    /// test_appimage_removal_deletes_only_its_own_files
    #[test]
    fn appimage_removal_deletes_only_its_own_files() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("registry.json");
        let live = tmp.path().join("thing.AppImage");
        let prev = tmp.path().join("thing.previous");
        let other = tmp.path().join("unrelated.AppImage");
        for f in [&live, &prev, &other] {
            std::fs::write(f, b"x").unwrap();
        }
        let mut reg = Map::new();
        reg.insert("thing".into(), serde_json::json!({"name": "thing", "path": live.to_string_lossy()}));
        crate::util::write_json(&target, &Value::Object(reg), Some(2), false).unwrap();

        let mut entry = App::new("thing", "1", inventory::APPIMAGE);
        entry.exec_path = live.to_string_lossy().into_owned();
        entry.extra.insert("key".into(), Value::from("thing"));
        let removed = std::cell::RefCell::new(Vec::new());
        let out = remove_appimage_with(
            &entry,
            || crate::util::read_json(&target).and_then(|v| v.as_object().cloned()).unwrap_or_default(),
            |r| crate::util::write_json(&target, &Value::Object(r.clone()), Some(2), false)
                .map_err(|e| e.to_string()),
            |k| {
                removed.borrow_mut().push(k.to_string());
                Ok(())
            },
        );
        assert_eq!(out, Ok(Removal::Removed));
        assert!(!live.exists());
        assert!(!prev.exists());
        assert!(other.exists(), "removal must not touch other files");
        assert_eq!(crate::util::read_json(&target), Some(serde_json::json!({})));
        assert_eq!(*removed.borrow(), vec!["thing".to_string()]);
    }

    #[test]
    fn an_empty_exec_path_is_the_current_directory() {
        let mut entry = App::new("thing", "1", inventory::APPIMAGE);
        entry.exec_path = String::new();
        let out = remove_appimage_with(&entry, Map::new, |_| panic!("saved"), |_| panic!("removed"));
        assert_eq!(out, Ok(Removal::NotDeleted("Could not delete : [Errno 21] Is a directory: '.'".into())));
    }

    #[test]
    fn update_cmd_is_read_as_python_joins_it() {
        let mut a = App::new("x", "1", inventory::LOCAL);
        assert_eq!(update_cmd(&a), Ok(vec![]));
        a.extra.insert("update_cmd".into(), serde_json::json!("ab"));
        assert_eq!(update_cmd(&a), Ok(vec!["a".to_string(), "b".to_string()]));
        a.extra.insert("update_cmd".into(), serde_json::json!(["a", 5]));
        assert_eq!(update_cmd(&a), Err("TypeError: sequence item 1: expected str instance, int found".into()));
        a.extra.insert("update_cmd".into(), serde_json::json!(5));
        assert_eq!(update_cmd(&a), Err("TypeError: can only join an iterable".into()));
    }

    #[test]
    fn exec_errors_are_found_before_starting() {
        assert_eq!(exec_errno("almanac-no-such-terminal"), Some(2));
        assert_eq!(exec_errno("sh"), None);
        assert_eq!(exec_errno("/"), Some(13));
    }

    #[test]
    fn update_runs_in_a_new_session() {
        // start_new_session=True: the child leads its own session.
        let argv = strs(&["sh", "-c", "test \"$(cut -d' ' -f6 /proc/$$/stat)\" = $$"]);
        let proc = popen_new_session(&argv).expect("sh starts");
        assert!(proc.wait_check(None::<&gio::Cancellable>).is_ok());
        assert_eq!(
            popen_new_session(&strs(&["almanac-no-such-terminal"])).err().as_deref(),
            Some("[Errno 2] No such file or directory: 'almanac-no-such-terminal'")
        );
    }

    #[test]
    fn one_package_is_installed_with_root_by_path() {
        assert_eq!(
            yay_argv("p").join(" "),
            "yay -S --needed --noconfirm --sudo /usr/bin/pkexec --pacman /usr/bin/pacman p"
        );
    }
}
