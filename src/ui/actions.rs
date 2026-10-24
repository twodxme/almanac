//! UI section `actions` (port of `app.py` lines 5530-5636 and 5999-6481):
//! row actions, self-update and About, verification of outcomes, file-type
//! takeovers, install from a link, update one/all, AppImage add/remove.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::{gio, glib};
use serde_json::{Map, Value};

use crate::i18n::tr;
use crate::inventory::{self, App};
use crate::patches::{self, PatchState};
use crate::sources::{self, Option as SourceOption, Resolution};
use crate::tasks::{self, LogFn, OnDone, Step, StepAction};
use crate::ui::shell::{spawn_bg, Win, WeakWin};
use crate::verify::{self, Outcome, ProbeValue, TryProbe};
use crate::{agents, appimage, ghcache, history, mimeguard, plugins, selfupdate, settings,
            snapshots, theming};

#[derive(Default)]
pub struct State {}

/// `judge` of `_checked`: turns the before and after probe answers into what
/// the user is told. Runs on a worker thread.
pub type Judge = Box<dyn FnOnce(Option<ProbeValue>, Option<ProbeValue>) -> Outcome + Send>;

/// A `Judge` that can be asked again.
type JudgeAgain = Arc<dyn Fn(Option<ProbeValue>, Option<ProbeValue>) -> Outcome + Send + Sync>;

/// The `holder` dict of `_watch_defaults`: every file type's default app,
/// read on a worker thread before an install.
pub struct DefaultsWatch {
    /// `holder["before"]`: `mimeguard.snapshot()`, filled in by the thread.
    pub before: Arc<Mutex<Option<BTreeMap<String, String>>>>,
    /// `holder["thread"]`: disconnects when the thread is done, so
    /// `recv_timeout` is `join(timeout=...)`.
    pub thread: Receiver<()>,
}

/// A window handle a worker thread can carry and hand back to the main
/// thread in `glib::idle_add_once` (`GLib.idle_add`).
struct MainGuard(Option<glib::thread_guard::ThreadGuard<WeakWin>>);

impl MainGuard {
    fn new(win: &Win) -> Guard {
        Arc::new(MainGuard(Some(glib::thread_guard::ThreadGuard::new(win.downgrade()))))
    }

    fn upgrade(&self) -> Option<Win> {
        self.0.as_ref().and_then(|g| g.get_ref().upgrade())
    }
}

impl Drop for MainGuard {
    fn drop(&mut self) {
        // Dropped on the worker -- it had nothing to hand back, or it
        // panicked -- the handle is let go of without being touched:
        // ThreadGuard panics off its own thread, and a panic while unwinding
        // aborts the app, where a Python thread's exception only prints.
        if let Some(g) = self.0.take() {
            if g.is_owner() {
                drop(g);
            } else {
                std::mem::forget(g);
            }
        }
    }
}

type Guard = Arc<MainGuard>;

/// `GLib.idle_add(fn, ...)` from a worker thread.
fn idle(guard: Guard, f: impl FnOnce(&Win) + Send + 'static) {
    glib::idle_add_once(move || {
        if let Some(win) = guard.upgrade() {
            f(&win);
        }
    });
}

/// `threading.Thread(target=work, daemon=True).start()`; `Err` is the
/// RuntimeError `start()` raises.
fn start_thread(work: impl FnOnce() + Send + 'static) -> Result<(), String> {
    std::thread::Builder::new()
        .spawn(work)
        .map(|_| ())
        .map_err(|_| "RuntimeError: can't start new thread".to_string())
}

/// An exception the Python lets out of a GTK handler: PyGObject prints it
/// to stderr and the handler stops there.
pub(super) fn raised(exc: &str) {
    eprintln!("{exc}");
}

// ---- small helpers -----------------------------------------------------------

/// `app.extra.get(key, "")` as text: a missing key or null is "".
fn extra_str(app: &App, key: &str) -> String {
    match app.extra.get(key) {
        Some(Value::String(s)) => s.clone(),
        None | Some(Value::Null) => String::new(),
        Some(other) => other.to_string(),
    }
}

/// The TypeError subprocess raises for an argv item that is not a path.
fn not_a_path(v: &Value) -> String {
    format!("TypeError: expected str, bytes or os.PathLike object, not {}", py_type(v))
}

/// Python truthiness of a JSON value.
pub(super) fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `type(v).__name__` of a JSON value as Python loads it.
fn py_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// Python's `repr()` of a str.
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
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python's `repr()` of a JSON value as Python loads it.
fn py_repr(v: &Value) -> String {
    match v {
        Value::String(s) => py_str_repr(s),
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, x)| format!("{}: {}", py_str_repr(k), py_repr(x)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => py_str(other),
    }
}

/// `f"{v}"` of a JSON value as Python loads it.
fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".to_string(),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        Value::Number(n) => match n.as_f64() {
            Some(f) if n.is_f64() => crate::util::py_float_repr(f),
            _ => n.to_string(),
        },
        Value::String(s) => s.clone(),
        other => py_repr(other),
    }
}

/// `str(Path(s))`: "" is ".", and "." components and trailing slashes go.
fn py_path(s: &str) -> PathBuf {
    let p: PathBuf = Path::new(s).components().filter(|c| *c != Component::CurDir).collect();
    if p.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        p
    }
}

/// `repr()` of a `pathlib.Path`.
fn path_repr(p: &Path) -> String {
    format!("PosixPath({})", py_str_repr(&p.to_string_lossy()))
}

/// The strerror part of an io::Error, which prints "<strerror> (os error N)".
fn strerror(e: &std::io::Error) -> String {
    let text = e.to_string();
    match e.raw_os_error() {
        Some(n) => text.strip_suffix(&format!(" (os error {n})")).unwrap_or(&text).to_string(),
        None => text,
    }
}

/// `str(OSError)`: `[Errno 13] Permission denied: '/path'`.
fn py_oserror(e: &std::io::Error, path: &Path) -> String {
    match e.raw_os_error() {
        Some(n) => format!("[Errno {n}] {}: {}", strerror(e), py_str_repr(&path.to_string_lossy())),
        None => e.to_string(),
    }
}

/// `str(OSError)` with a second filename: `[Errno 28] ...: 'a' -> 'b'`.
fn py_oserror2(e: &std::io::Error, a: &Path, b: &Path) -> String {
    match e.raw_os_error() {
        Some(n) => format!("[Errno {n}] {}: {} -> {}", strerror(e),
                           py_str_repr(&a.to_string_lossy()), py_str_repr(&b.to_string_lossy())),
        None => e.to_string(),
    }
}

/// The traceback's last line for an OSError: its subclass, then `str()`.
fn py_oserror_line(e: &std::io::Error, path: &Path) -> String {
    let class = match e.raw_os_error() {
        Some(1) | Some(13) => "PermissionError",
        Some(2) => "FileNotFoundError",
        Some(17) => "FileExistsError",
        Some(20) => "NotADirectoryError",
        Some(21) => "IsADirectoryError",
        _ => "OSError",
    };
    format!("{class}: {}", py_oserror(e, path))
}

/// `str.isspace()` of one character: Unicode whitespace, plus the
/// information separators \x1c-\x1f, which Python counts and Rust does not.
fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// `s.strip()`.
fn py_strip(s: &str) -> &str {
    s.trim_matches(py_isspace)
}

/// `s.splitlines()[0]` of a non-empty string.
fn py_first_line(s: &str) -> &str {
    const BREAKS: [char; 10] = ['\n', '\r', '\x0b', '\x0c', '\x1c', '\x1d', '\x1e', '\u{85}',
                                '\u{2028}', '\u{2029}'];
    s.split(BREAKS).next().unwrap_or(s)
}

/// `str(UnicodeDecodeError)` of `bytes.decode("utf-8")`.
fn utf8_error(bytes: &[u8], e: &std::str::Utf8Error) -> String {
    let pos = e.valid_up_to();
    let (count, reason) = match e.error_len() {
        Some(n) => {
            let start_ok = matches!(bytes[pos], 0xC2..=0xF4);
            (n, if start_ok { "invalid continuation byte" } else { "invalid start byte" })
        }
        None => (bytes.len() - pos, "unexpected end of data"),
    };
    if count == 1 {
        format!("'utf-8' codec can't decode byte 0x{:02x} in position {pos}: {reason}", bytes[pos])
    } else {
        format!("'utf-8' codec can't decode bytes in position {pos}-{}: {reason}", pos + count - 1)
    }
}

/// Python truthiness of a probe's answer.
fn probe_truthy(v: &Option<ProbeValue>) -> bool {
    match v {
        None => false,
        Some(ProbeValue::Str(s)) => !s.is_empty(),
        Some(ProbeValue::Identity(..)) => true,
        Some(ProbeValue::Set(s)) => !s.is_empty(),
        Some(ProbeValue::Bool(b)) => *b,
        Some(ProbeValue::Map(m)) => !m.is_empty(),
    }
}

/// `f"{a}"` of a probe's answer (the installed version, for a package).
fn probe_text(v: &Option<ProbeValue>) -> String {
    match v {
        None => "None".to_string(),
        Some(ProbeValue::Str(s)) => s.clone(),
        Some(ProbeValue::Bool(b)) => if *b { "True" } else { "False" }.to_string(),
        Some(other) => format!("{other:?}"),
    }
}

/// `f"{n:,}"`: thousands separated by commas.
fn thousands(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// `github.com/owner/repo[/...]` -> `https://github.com/owner/repo/releases`;
/// any other non-empty URL is returned as is, an empty one as `None`.
/// (The URL logic of `_row_notes`.)
pub fn releases_url(url: &str) -> Option<String> {
    let tail = match url.split_once("github.com/") {
        Some((_, t)) => t,
        None => url,
    };
    let parts: Vec<&str> = tail.trim_matches('/').split('/').collect();
    if parts.len() >= 2 {
        Some(format!("https://github.com/{}/{}/releases", parts[0], parts[1]))
    } else if !url.is_empty() {
        Some(url.to_string())
    } else {
        None
    }
}

/// The name an added AppImage is tracked under: the file stem up to the
/// first `-`, or the whole stem when that is empty (`_install_appimage`).
pub fn appimage_name(src: &Path) -> String {
    let stem = src
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    match stem.split('-').next() {
        Some(first) if !first.is_empty() => first.to_string(),
        _ => stem,
    }
}

/// What adding `src` under the key `name` would do to an AppImage already
/// tracked (`reg` is the registry, `store` where tracked AppImages live).
#[derive(Debug, PartialEq)]
pub enum AddClash {
    /// Nothing is tracked under that key, or it is this very file again:
    /// go ahead.
    Free,
    /// Another AppImage is tracked under it (`existing`, its source file or
    /// path), or its file is there. Adding would replace it -- its file,
    /// its launcher and its registry entry -- so that is asked; `spare` is
    /// a key that keeps both.
    Taken { existing: String, spare: String },
}

/// See [`AddClash`]. The key is the file name up to its first `-`, so two
/// different apps (`my-app`, `my-tool`) meet at the same one, and the
/// second used to replace the first without a word.
pub fn appimage_clash(reg: &Map<String, Value>, name: &str, src: &Path, store: &Path) -> AddClash {
    let taken = |k: &str| reg.contains_key(k) || store.join(format!("{k}.AppImage")).exists();
    if !taken(name) {
        return AddClash::Free;
    }
    let field = |k: &str| reg.get(name).and_then(|e| e.get(k)).and_then(Value::as_str).map(str::to_string);
    let src_s = src.to_string_lossy();
    if field("source_file").as_deref() == Some(&*src_s) {
        return AddClash::Free;
    }
    let existing = field("source_file")
        .or_else(|| field("path"))
        .unwrap_or_else(|| store.join(format!("{name}.AppImage")).to_string_lossy().into_owned());
    let spare = (2..).map(|n| format!("{name}-{n}")).find(|k| !taken(k)).unwrap_or_default();
    AddClash::Taken { existing, spare }
}

/// The `yay` argv that installs or updates one package (`on_update_one`,
/// `_install_option`).
pub fn yay_argv(name: &str) -> Vec<String> {
    ["yay", "-S", "--needed", "--noconfirm", "--sudo", "pkexec", name]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// The shell script `on_update_all` runs in a terminal.
pub fn update_all_script(command: &str) -> String {
    format!("{command}; echo; read -n1 -r -p 'Done — press any key to close'")
}

/// The test of `on_update_one`: a release only checkupdates knows about
/// cannot be installed alone. `synced` is `inventory.synced_version`, asked
/// only for a package that is not from the AUR.
pub fn needs_full_upgrade(app: &App, synced: impl FnOnce(&str) -> String) -> bool {
    app.source != inventory::AUR && {
        let s = synced(&app.name);
        s != "" && s != app.update
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
pub(super) fn update_cmd(app: &App) -> Result<Vec<String>, String> {
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
pub(super) fn appimage_update_work(app: &App) -> Result<(), String> {
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

/// `shutil.copy2(src, dst)`: contents, then times, extended attributes and
/// permission bits (`copystat`). `Err` is the str() of the OSError raised.
fn copy2(src: &Path, dst: &Path) -> Result<(), String> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
    let (dst, dst_repr) = if dst.is_dir() {
        let d = dst.join(src.file_name().unwrap_or_default());
        let r = py_str_repr(&d.to_string_lossy());
        (d, r)
    } else {
        (dst.to_path_buf(), path_repr(dst))
    };
    // copyfile
    if let (Ok(a), Ok(b)) = (std::fs::metadata(src), std::fs::metadata(&dst)) {
        if a.dev() == b.dev() && a.ino() == b.ino() {
            return Err(format!("{} and {dst_repr} are the same file", path_repr(src)));
        }
    }
    for p in [src, dst.as_path()] {
        if std::fs::metadata(p).is_ok_and(|m| m.file_type().is_fifo()) {
            return Err(format!("`{}` is a named pipe", p.display()));
        }
    }
    let mut fsrc = std::fs::File::open(src).map_err(|e| py_oserror(&e, src))?;
    let mut fdst = std::fs::File::create(&dst).map_err(|e| py_oserror(&e, &dst))?;
    std::io::copy(&mut fsrc, &mut fdst).map_err(|e| py_oserror2(&e, src, &dst))?;
    drop(fdst);
    // copystat
    let st = std::fs::metadata(src).map_err(|e| py_oserror(&e, src))?;
    let times = rustix::fs::Timestamps {
        last_access: rustix::fs::Timespec { tv_sec: st.atime(), tv_nsec: st.atime_nsec() as _ },
        last_modification: rustix::fs::Timespec { tv_sec: st.mtime(), tv_nsec: st.mtime_nsec() as _ },
    };
    rustix::fs::utimensat(rustix::fs::CWD, &dst, &times, rustix::fs::AtFlags::empty())
        .map_err(|e| py_oserror(&e.into(), &dst))?;
    copy_xattr(src, &dst)?;
    std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(st.mode() & 0o7777))
        .map_err(|e| py_oserror(&e, &dst))
}

/// `shutil._copyxattr`: what the file systems cannot carry is skipped.
fn copy_xattr(src: &Path, dst: &Path) -> Result<(), String> {
    use rustix::io::Errno;
    use std::os::unix::ffi::OsStrExt;
    let list = || -> rustix::io::Result<Vec<u8>> {
        let n = rustix::fs::listxattr(src, &mut [0u8; 0][..])?;
        let mut buf = vec![0u8; n];
        let n = rustix::fs::listxattr(src, &mut buf[..])?;
        buf.truncate(n);
        Ok(buf)
    };
    let names = match list() {
        Ok(n) => n,
        Err(e) if [Errno::NOTSUP, Errno::NODATA, Errno::INVAL].contains(&e) => return Ok(()),
        Err(e) => return Err(py_oserror(&e.into(), src)),
    };
    for name in names.split(|b| *b == 0).filter(|n| !n.is_empty()) {
        let name = std::ffi::OsStr::from_bytes(name);
        let copied = (|| -> Result<(), (Errno, &Path)> {
            let n = rustix::fs::getxattr(src, name, &mut [0u8; 0][..]).map_err(|e| (e, src))?;
            let mut value = vec![0u8; n];
            let n = rustix::fs::getxattr(src, name, &mut value[..]).map_err(|e| (e, src))?;
            value.truncate(n);
            rustix::fs::setxattr(dst, name, &value, rustix::fs::XattrFlags::empty()).map_err(|e| (e, dst))
        })();
        if let Err((e, p)) = copied {
            if ![Errno::PERM, Errno::NOTSUP, Errno::NODATA, Errno::INVAL, Errno::ACCESS].contains(&e) {
                return Err(py_oserror(&e.into(), p));
            }
        }
    }
    Ok(())
}

/// `subprocess.run([str(dest), "--version"], capture_output=True,
/// text=True, timeout=12).stdout`: `Ok(None)` for the OSError or
/// SubprocessError (a timeout) the caller catches, `Err` for the
/// UnicodeDecodeError it does not. As in `communicate`, the timeout covers
/// reading both pipes to their end, not only the process exiting.
fn version_output(dest: &Path) -> Result<Option<String>, String> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> Option<Receiver<Vec<u8>>> {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .spawn(move || {
                let mut buf = Vec::new();
                if let Some(mut p) = pipe {
                    let _ = p.read_to_end(&mut buf);
                }
                let _ = tx.send(buf);
            })
            .ok()?;
        Some(rx)
    }

    let Ok(mut child) = crate::util::spawn_group(Command::new(dest)
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped()))
    else {
        return Ok(None);
    };
    let deadline = Instant::now() + Duration::from_secs(12);
    let left = || deadline.saturating_duration_since(Instant::now());
    let timeout = |child: &mut std::process::Child| {
        crate::util::kill_group(child);
        Ok(None)
    };
    let (Some(out_rx), Some(err_rx)) = (drain(child.stdout.take()), drain(child.stderr.take())) else {
        return timeout(&mut child);
    };
    let mut got = Vec::new();
    for rx in [out_rx, err_rx] {
        match rx.recv_timeout(left()) {
            Ok(buf) => got.push(buf),
            Err(mpsc::RecvTimeoutError::Disconnected) => got.push(Vec::new()),
            Err(mpsc::RecvTimeoutError::Timeout) => return timeout(&mut child),
        }
    }
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if left().is_zero() => return timeout(&mut child),
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => return Ok(None),
        }
    }
    let mut texts = Vec::new();
    for bytes in &got {
        match std::str::from_utf8(bytes) {
            Ok(t) => texts.push(t.replace("\r\n", "\n").replace('\r', "\n")),
            Err(e) => return Err(format!("UnicodeDecodeError: {}", utf8_error(bytes, &e))),
        }
    }
    Ok(texts.into_iter().next())
}

/// `subprocess.Popen(argv, start_new_session=True)`. A GSubprocess, so the
/// child can call setsid(2) before its exec without unsafe code; like
/// Popen it resets SIGPIPE, closes other descriptors and reports a failed
/// exec as an error. The OSError Popen would raise when the program cannot
/// be run is found first, the way its execvp searches.
pub(super) fn popen_new_session(argv: &[String]) -> Result<gio::Subprocess, String> {
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
    // ---- actions ---------------------------------------------------------
    pub fn row_visit(&self, name: &str) {
        let app = self.shared.borrow().row_actions.get(name).cloned();
        if let Some(app) = app {
            if let Some(url) = app.extra.get("url").filter(|v| truthy(v)) {
                self.open_url_value(url);
            }
        }
    }

    /// `self.open_url(app.extra["url"])` with the raw value: PyGObject
    /// refuses anything but a str.
    fn open_url_value(&self, url: &Value) {
        match url {
            Value::String(s) => self.open_url(s),
            other => raised(&format!("TypeError: Must be string, not {}", py_type(other))),
        }
    }

    /// Jump to the releases page, where the real changelog is.
    pub fn row_notes(&self, name: &str) {
        let app = self.shared.borrow().row_actions.get(name).cloned();
        let Some(app) = app else {
            return;
        };
        let url = match app.extra.get("url") {
            None => String::new(),
            Some(Value::String(s)) => s.clone(),
            Some(other) => {
                raised(&format!("AttributeError: '{}' object has no attribute 'split'", py_type(other)));
                return;
            }
        };
        // github.com/owner/repo[/...] -> github.com/owner/repo/releases
        if let Some(target) = releases_url(&url) {
            self.open_url(&target);
        }
    }

    /// Update Almanac, then offer to restart into the new code.
    ///
    /// The check is a `git fetch` (up to 90 s): it runs off the main thread,
    /// and the question is asked when it answers.
    pub fn on_self_update(&self) {
        let w = self.downgrade();
        spawn_bg(
            || selfupdate::try_check(true),
            move |info| match info {
                Ok(info) => {
                    if let Some(w) = w.upgrade() {
                        w.offer_self_update(info);
                    }
                }
                Err(e) => raised(&e),
            },
        );
    }

    fn offer_self_update(&self, info: selfupdate::Info) {
        if !info.can_update && info.kind == selfupdate::PACKAGE && !info.latest.is_empty() {
            // A newer release than this package: pacman installs it, not
            // Almanac, but the download is one click away.
            let toast = adw::Toast::builder().title(info.note.as_str()).use_markup(false).timeout(0).build();
            toast.set_button_label(Some(&tr("Open the release")));
            let w = self.downgrade();
            toast.connect_button_clicked(move |_| {
                if let Some(w) = w.upgrade() {
                    w.open_url(selfupdate::RELEASES_URL);
                }
            });
            self.toasts.add_toast(toast);
            return;
        }
        if !info.can_update {
            let note = if info.note.is_empty() { "Almanac is up to date".to_string() } else { info.note };
            self.say(&note, 8);
            return;
        }
        let w = self.downgrade();
        tasks::confirm(
            &self.window,
            "Update Almanac?",
            &format!(
                "{} commit(s) behind. The update is a fast-forward pull, so it cannot \
                 overwrite local work — if there were any, this would refuse instead.",
                info.behind
            ),
            &format!(
                "Checkout  {}\nRemote    {}\nNewest    {}\n\n$ git pull --ff-only",
                selfupdate::ROOT.display(),
                info.remote,
                info.latest
            ),
            &tr("Update"),
            false,
            move || {
                let Some(win) = w.upgrade() else { return };
                let steps = selfupdate::update_steps()
                    .into_iter()
                    .map(|(label, argv)| Step::argv(label, argv))
                    .collect();
                let w2 = win.downgrade();
                tasks::run(
                    &win.window,
                    "Updating Almanac",
                    steps,
                    Some(Box::new(move |ok| {
                        if let Some(win) = w2.upgrade() {
                            win.after_self_update(ok);
                        }
                    })),
                );
            },
        );
    }

    pub fn after_self_update(&self, ok: bool) {
        if !ok {
            self.say(&tr("Almanac was not updated"), 8);
            return;
        }
        // The running process cannot adopt the new source, so say so rather
        // than letting the version in the list disagree with what is running.
        let toast = adw::Toast::builder()
            .title(tr("Almanac updated — restart to use the new version"))
            .timeout(0)
            .build();
        toast.set_button_label(Some("Restart"));
        toast.connect_button_clicked(|_| selfupdate::restart());
        self.toasts.add_toast(toast);
        self.reload();
    }

    /// The About dialog. What it shows takes a handful of git calls and the
    /// theme and GitHub summaries: gathered off the main thread, then shown.
    pub fn on_about(&self) {
        let w = self.downgrade();
        spawn_bg(
            || -> Result<_, String> {
                // What raises in here ends on_about before the dialog is
                // presented, in the Python's order.
                let info = selfupdate::try_check(false)?;
                let theme = theming::try_describe()?;
                let github = ghcache::try_describe()?;
                let notes = selfupdate::try_release_notes()?;
                Ok((info, theme, github, notes))
            },
            move |gathered| match gathered {
                Ok((info, theme, github, notes)) => {
                    if let Some(w) = w.upgrade() {
                        w.show_about(info, &theme, &github, notes);
                    }
                }
                Err(e) => raised(&e),
            },
        );
    }

    fn show_about(&self, info: selfupdate::Info, theme: &str, github: &str, notes: Option<(String, String)>) {
        let about = adw::AboutDialog::builder()
            .application_name("Almanac")
            .application_icon("io.github.twodxme.Almanac")
            .version(info.version.as_str())
            .developer_name("twodxme")
            .comments(tr(
                "A catalogue of everything installed on this machine: where each thing \
                 came from, how old it is, whether it still has a maintainer, and what \
                 needs attention.",
            ))
            .website("https://github.com/twodxme/almanac")
            .issue_url("https://github.com/twodxme/almanac/issues")
            .license_type(gtk::License::MitX11)
            .build();
        about.add_credit_section(
            Some("Catalogues"),
            &[
                "Omarchy plugin marketplace https://plugins.omarchy.org",
                "ArtCraft https://getartcraft.com",
            ],
        );
        about.set_debug_info(&format!(
            "Installed as: {}\n{}\n{}\n{}",
            info.kind,
            info.note,
            theme,
            github
        ));
        if let Some((version, markup)) = notes {
            about.set_release_notes_version(&version);
            about.set_release_notes(&markup);
        }
        about.present(Some(&self.window));
    }

    pub fn row_folder(&self, name: &str) {
        let app = self.shared.borrow().row_actions.get(name).cloned();
        if let Some(app) = app {
            if let Some(repo) = app.extra.get("repo").filter(|v| truthy(v)) {
                self.open_url(&format!("file://{}", py_str(repo)));
            }
        }
    }

    /// Show what is uncommitted, read-only, before offering to move it.
    pub fn row_changes(&self, name: &str) {
        let app = self.shared.borrow().row_actions.get(name).cloned();
        let repo = match app.as_ref().and_then(|a| a.extra.get("repo")).filter(|v| truthy(v)) {
            None => return,
            Some(Value::String(r)) => r.clone(),
            Some(other) => return raised(&not_a_path(other)),
        };
        tasks::run(
            &self.window,
            &format!("Local changes in {name}"),
            vec![
                Step::argv("Working tree", strs(&["git", "-C", &repo, "status", "--short"])),
                Step::argv("Diff summary", strs(&["git", "-C", &repo, "diff", "--stat"])),
            ],
            None,
        );
    }

    pub fn row_pull(&self, name: &str) {
        let app = self.shared.borrow().row_actions.get(name).cloned();
        if let Some(app) = app {
            self.on_git_pull(&app);
        }
    }

    /// Update a checkout that has local edits -- see on_git_pull.
    pub fn row_stash(&self, name: &str) {
        let app = self.shared.borrow().row_actions.get(name).cloned();
        if let Some(app) = app {
            self.on_git_pull(&app);
        }
    }

    // ---- did it work? ------------------------------------------------------
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
    fn checked_again(
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

    // ---- which files an install took over ----------------------------------
    /// Read every file type's default app now, before an install.
    ///
    /// `_watch_defaults`; `Err` is what raised out of it.
    pub fn try_watch_defaults(&self) -> Result<DefaultsWatch, String> {
        let before: Arc<Mutex<Option<BTreeMap<String, String>>>> = Arc::new(Mutex::new(None));
        let theirs = before.clone();
        let (done, thread) = mpsc::channel::<()>();
        start_thread(move || {
            let _done = done;
            let snap = mimeguard::snapshot();
            if let Ok(mut b) = theirs.lock() {
                *b = snap;
            }
        })?;
        Ok(DefaultsWatch { before, thread })
    }

    /// `_check_defaults`; `Err` is what raised out of it.
    pub fn try_check_defaults(&self, holder: DefaultsWatch) -> Result<(), String> {
        let guard = MainGuard::new(self);
        start_thread(move || {
            let _ = holder.thread.recv_timeout(Duration::from_secs(90));
            let before = holder.before.lock().ok().and_then(|b| b.clone());
            let moved = mimeguard::changes(before.as_ref(), mimeguard::snapshot().as_ref());
            if !moved.is_empty() {
                idle(guard, move |win| win.offer_defaults_back(moved));
            }
        })
    }

    pub fn offer_defaults_back(&self, moved: Vec<(String, String, String)>) {
        let returnable: Vec<(String, String, String)> =
            moved.iter().filter(|m| !m.1.is_empty()).cloned().collect();
        let summary = match mimeguard::try_summary(&moved) {
            Ok(summary) => summary,
            Err(e) => return raised(&e),
        };
        let body = summary.join("\n")
            + &if !returnable.is_empty() {
                tr("\n\nGive them back to what opened them before?")
            } else {
                String::new()
            };
        let dialog = adw::AlertDialog::builder()
            .heading(tr("Some files now open in a different app"))
            .body(body)
            .build();
        dialog.add_response("keep", &tr("Keep the new app"));
        if !returnable.is_empty() {
            dialog.add_response("restore", &tr("Give them back"));
            dialog.set_response_appearance("restore", adw::ResponseAppearance::Suggested);
        }
        dialog.set_default_response(Some("keep"));
        dialog.set_close_response("keep");

        let w = self.downgrade();
        // What raises here ends `_offer_defaults_back` before the dialog
        // is shown.
        let recorded = mimeguard::try_summary(&moved)
            .and_then(|items| history::record_checked(history::ALMANAC, "File types changed hands", "", "", &items));
        if let Err(e) = recorded {
            return raised(&e);
        }
        dialog.connect_response(None, move |_d, resp| {
            if resp == "restore" {
                let n = match mimeguard::try_restore(&returnable, None) {
                    Ok(n) => n,
                    Err(e) => return raised(&e),
                };
                if let Some(win) = w.upgrade() {
                    win.say(&format!("{n} file type(s) handed back"), 5);
                }
                let recorded = mimeguard::try_summary(&returnable).and_then(|items| {
                    history::record_checked(history::ALMANAC, &format!("{n} file type(s) handed back"), "", "", &items)
                });
                if let Err(e) = recorded {
                    raised(&e);
                }
            }
        });
        dialog.present(Some(&self.window));
    }

    // ---- install from a link -----------------------------------------------
    /// Paste a GitHub link; see every way it ships before choosing one.
    pub fn on_install_link(&self, link: &str) {
        let entry = gtk::Entry::builder()
            .placeholder_text("https://github.com/owner/project")
            .activates_default(true)
            .hexpand(true)
            .build();
        entry.set_text(link);
        let dialog = adw::AlertDialog::builder()
            .heading(tr("Install from a link"))
            .body(tr(
                "Paste a GitHub project link. Every way it is published is compared, \
                 and one is recommended, before anything is downloaded.",
            ))
            .build();
        dialog.set_extra_child(Some(&entry));
        dialog.add_response("cancel", &tr("Cancel"));
        dialog.add_response("find", &tr("Look it up"));
        dialog.set_response_appearance("find", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("find"));
        dialog.set_close_response("cancel");

        let w = self.downgrade();
        dialog.connect_response(None, move |_d, resp| {
            if resp != "find" {
                return;
            }
            let Some(win) = w.upgrade() else { return };
            let text = py_strip(&entry.text()).to_string();
            if sources::parse(&text).is_none() {
                win.say(&tr("That is not a GitHub repository link"), 5);
                return;
            }
            win.say(&tr("Looking it up…"), 5);

            let w2 = win.downgrade();
            // What resolve raises ends the Python's worker: no options shown.
            spawn_bg(
                move || sources::try_resolve(&text),
                move |res| match res {
                    Ok(res) => {
                        if let Some(win) = w2.upgrade() {
                            win.show_link_options(res);
                        }
                    }
                    Err(e) => raised(&e),
                },
            );
        });
        dialog.present(Some(&self.window));
    }

    pub fn show_link_options(&self, res: Resolution) {
        if res.options.is_empty() {
            let msg = if res.error.is_empty() { "Nothing installable found".to_string() } else { res.error.clone() };
            self.say(&msg, 8);
            return;
        }
        let chosen: Rc<RefCell<Option<SourceOption>>> = Rc::new(RefCell::new(res.best().cloned()));
        let rows = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).build();
        rows.add_css_class("boxed-list");
        let mut group: Option<gtk::CheckButton> = None;
        for (i, o) in res.options.iter().enumerate() {
            // Name and version on the title; the file or package name, then
            // each reason and caveat on its own line, so none is cut off.
            let mut lines = vec![o.name.clone()];
            lines.extend(o.reasons.iter().map(|r| format!("✓ {r}")));
            lines.extend(o.warnings.iter().map(|w| format!("⚠ {w}")));
            let title = if !o.version.is_empty() {
                format!("{} · {}", o.title, o.version)
            } else {
                o.title.clone()
            };
            let row: adw::ActionRow = Win::plain(&title, &lines.join("\n"));
            row.set_subtitle_lines(0);
            let check = gtk::CheckButton::builder().valign(gtk::Align::Center).build();
            match &group {
                None => group = Some(check.clone()),
                Some(g) => check.set_group(Some(g)),
            }
            check.set_active(i == 0);
            let chosen_c = chosen.clone();
            let o_c = o.clone();
            check.connect_toggled(move |b| {
                if b.is_active() {
                    *chosen_c.borrow_mut() = Some(o_c.clone());
                }
            });
            row.add_prefix(&check);
            row.set_activatable_widget(Some(&check));
            if i == 0 {
                let pill = gtk::Label::builder()
                    .label(tr("Recommended"))
                    .valign(gtk::Align::Center)
                    .build();
                pill.add_css_class("pill-ok");
                row.add_suffix(&pill);
            }
            rows.append(&row);
        }
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_propagate_natural_height(true);
        scroll.set_max_content_height(420);
        scroll.set_child(Some(&rows));

        let mut facts = vec![format!("{} stars", thousands(res.stars))];
        if !res.license.is_empty() {
            facts.push(res.license.clone());
        }
        if res.archived {
            facts.push("archived".to_string());
        }
        let body = if !res.description.is_empty() {
            format!("{}\n", res.description)
        } else {
            String::new()
        } + &facts.join(" · ");
        let dialog = adw::AlertDialog::builder()
            .heading(format!("Install {}?", res.repo))
            .body(body)
            .build();
        dialog.set_body_use_markup(false);
        if dialog.find_property("prefer-wide-layout").is_some() {
            // libadwaita 1.6+
            dialog.set_property("prefer-wide-layout", true);
        }
        dialog.set_extra_child(Some(&scroll));
        dialog.add_response("cancel", &tr("Cancel"));
        dialog.add_response("install", &tr("Install"));
        dialog.set_response_appearance("install", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("install"));
        dialog.set_close_response("cancel");
        let w = self.downgrade();
        dialog.connect_response(None, move |_d, r| {
            if r == "install" {
                let opt = chosen.borrow().clone();
                if let (Some(win), Some(opt)) = (w.upgrade(), opt) {
                    win.install_option(&res, &opt);
                }
            }
        });
        dialog.present(Some(&self.window));
    }

    pub fn install_option(&self, res: &Resolution, opt: &SourceOption) {
        let holder = match self.try_watch_defaults() {
            Ok(h) => h,
            Err(e) => return raised(&e),
        };

        let w = self.downgrade();
        let after: OnDone = Box::new(move |ok| {
            if let Some(win) = w.upgrade() {
                if ok {
                    if let Err(e) = win.try_check_defaults(holder) {
                        return raised(&e);
                    }
                }
                win.after_pacman(ok);
            }
        });

        if opt.kind == sources::APPIMAGE {
            let (res_c, opt_c) = (res.clone(), opt.clone());
            let call: tasks::StepFn = Arc::new(move |log: LogFn<'_>| {
                sources::install_appimage(&res_c, &opt_c, log).map(|_| ())
            });
            tasks::run(
                &self.window,
                &format!("Installing {}", res.repo),
                vec![Step::call(format!("Downloading {}", opt.name), call)],
                Some(after),
            );
            return;
        }
        let argv = yay_argv(&opt.name);
        let name = opt.name.clone();
        let judge_name = name.clone();
        let steps = match self.snapshot_first(vec![Step::argv(format!("Installing {name}"), argv)],
                                              &format!("installing {name}")) {
            Ok(steps) => steps,
            Err(e) => return raised(&e),
        };
        let w = self.downgrade();
        self.checked(
            Some(verify::installed_package_try(&name)),
            Box::new(move |b, a| {
                let landed = probe_truthy(&a) && a != b;
                Outcome::new(
                    Some(landed),
                    if landed {
                        format!("{judge_name} {} installed", probe_text(&a))
                    } else {
                        format!("{judge_name} was not installed")
                    },
                )
            }),
            Some(after),
            raised,
            move |done| {
                if let Some(win) = w.upgrade() {
                    tasks::run(&win.window, &format!("Installing {name}"), steps, Some(done));
                }
            },
        );
    }

    /// The app ran it, so it knows to re-read rather than guess.
    pub fn after_pacman(&self, ok: bool) {
        if ok {
            self.reload();
        }
    }

    /// AppImages are ours, so remove them directly -- no package manager
    /// is involved and there is nothing to confirm against.
    pub fn remove_appimage(&self, app: &App) {
        // Its read and its write of the registry as one, under the lock;
        // let go before anything (the reload) reads the registry again.
        let removed = inventory::lock_registry().and_then(|_lock| {
            remove_appimage_with(app, inventory::load_registry, inventory::save_registry, appimage::remove_desktop_entry)
        });
        match removed {
            Err(e) => raised(&e),
            Ok(Removal::NotDeleted(msg)) => self.say(&msg, 5),
            Ok(Removal::Removed) => {
                self.say(&format!("Removed {}", app.name), 5);
                self.reload();
            }
        }
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
                win.checked(verify::for_app_try(&app), app_judge(&app), Some(after_pacman_cb(&win)), raised, move |done| {
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
            win.checked(verify::for_app_try(&app), app_judge(&app), Some(after_pacman_cb(&win)), raised, move |done| {
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

    pub fn on_add_appimage(&self) {
        let dialog = gtk::FileDialog::builder().title(tr("Choose an AppImage to track")).build();
        let w = self.downgrade();
        dialog.open(Some(&self.window), None::<&gio::Cancellable>, move |result| {
            if let Some(win) = w.upgrade() {
                win.on_appimage_chosen(result);
            }
        });
    }

    pub fn on_appimage_chosen(&self, result: Result<gio::File, glib::Error>) {
        let Ok(gfile) = result else {
            return; // cancelled
        };
        match gfile.path() {
            Some(path) => self.install_appimage(&path),
            None => raised("TypeError: argument should be a str or an os.PathLike object where \
                            __fspath__ returns a str, not 'NoneType'"),
        }
    }

    /// Copy into our own store and register it.
    ///
    /// Copying rather than referencing the download means a later cleanup of
    /// ~/Downloads cannot quietly break an installed app.
    ///
    /// A different AppImage already tracked under the same name is not
    /// replaced without asking (see [`AddClash`]).
    pub fn install_appimage(&self, src: &Path) {
        let registry = inventory::registry();
        let store = registry.parent().map(Path::to_path_buf).unwrap_or_default().join("apps");
        let name = appimage_name(src);
        match appimage_clash(&inventory::load_registry(), &name, src, &store) {
            AddClash::Free => self.install_appimage_as(src, &name),
            AddClash::Taken { existing, spare } => self.ask_appimage_clash(src, &name, &existing, &spare),
        }
    }

    /// Ask whether an added AppImage replaces the one tracked under `name`,
    /// or is kept beside it as `spare`.
    fn ask_appimage_clash(&self, src: &Path, name: &str, existing: &str, spare: &str) {
        let body = format!(
            "{}\n\n{}\n{existing}\n\n{}\n{}",
            tr("Another AppImage is already tracked under this name. Adding this one \
                as it is would replace it: its file, its launcher and what Almanac \
                knows about it."),
            tr("Tracked now:"),
            tr("Adding:"),
            src.display(),
        );
        let dialog = adw::AlertDialog::new(Some(&format!("“{name}” is already tracked")), Some(&body));
        dialog.add_response("cancel", &tr("Cancel"));
        dialog.add_response("both", &format!("Keep both, as “{spare}”"));
        dialog.add_response("replace", &tr("Replace"));
        dialog.set_response_appearance("both", adw::ResponseAppearance::Suggested);
        dialog.set_response_appearance("replace", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let w = self.downgrade();
        let (src, name, spare) = (src.to_path_buf(), name.to_string(), spare.to_string());
        dialog.connect_response(None, move |_d, resp| {
            let Some(win) = w.upgrade() else { return };
            match resp {
                "both" => win.install_appimage_as(&src, &spare),
                "replace" => win.install_appimage_as(&src, &name),
                _ => {}
            }
        });
        dialog.present(Some(&self.window));
    }

    /// `install_appimage` once the key it is tracked under is settled.
    ///
    /// The copy, the `--version` probe (up to 12 s) and the desktop entry
    /// (extracting the icon, up to 180 s) run off the main thread; the
    /// window says what it is doing meanwhile, and the outcome when done.
    fn install_appimage_as(&self, src: &Path, name: &str) {
        self.say(&format!("Adding {name}…"), 5);
        let (src, name) = (src.to_path_buf(), name.to_string());
        let w = self.downgrade();
        spawn_bg(
            move || add_appimage(&src, &name),
            move |res| {
                let Some(win) = w.upgrade() else { return };
                match res {
                    Ok(name) => {
                        win.say(&format!("Tracking {name}"), 5);
                        win.reload();
                    }
                    Err(AddFailed::Say(msg)) => win.say(&msg, 5),
                    Err(AddFailed::Raised(exc)) => raised(&exc),
                }
            },
        );
    }
}

/// How adding an AppImage stopped short: told to the user, or raised (logged).
enum AddFailed {
    Say(String),
    Raised(String),
}

/// The slow half of `install_appimage_as`, off the main thread: copy into
/// our store, ask the version, register, and give it a launcher entry.
/// Returns the name it is tracked under.
fn add_appimage(src: &Path, name: &str) -> Result<String, AddFailed> {
    use std::os::unix::fs::PermissionsExt;
    let registry = inventory::registry();
    let store = registry.parent().map(Path::to_path_buf).unwrap_or_default().join("apps");
    std::fs::create_dir_all(&store).map_err(|e| AddFailed::Raised(py_oserror_line(&e, &store)))?;
    let name = name.to_string();
    let dest = store.join(format!("{name}.AppImage"));
    let copied = copy2(src, &dest).and_then(|()| {
        let mode = std::fs::metadata(&dest).map_err(|e| py_oserror(&e, &dest))?.permissions().mode();
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(mode | 0o111))
            .map_err(|e| py_oserror(&e, &dest))
    });
    if let Err(e) = copied {
        return Err(AddFailed::Say(format!("Could not install: {e}")));
    }

    let mut version = "?".to_string();
    match version_output(&dest) {
        Ok(Some(out)) => {
            let out = py_strip(&out);
            if !out.is_empty() && out.chars().count() < 60 {
                version = py_first_line(out).to_string();
            }
        }
        Ok(None) => {} // plenty of AppImages ignore --version
        Err(e) => return Err(AddFailed::Raised(e)),
    }

    let dest_s = dest.to_string_lossy().into_owned();
    let src_name = src.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    let entry = serde_json::json!({
        "name": name,
        "path": dest_s,
        "version": version,
        "description": format!("AppImage · {src_name}"),
        "source_file": src.to_string_lossy(),
    });
    // Read afresh under the registry's lock: what changed meanwhile stays.
    inventory::update_registry(|reg| {
        reg.insert(name.clone(), entry);
        Ok(true)
    })
    .map_err(AddFailed::Raised)?;
    // A tracked AppImage should appear in the launcher like anything else,
    // not just inside this window.
    appimage::install_desktop_entry(&dest, &name).map_err(AddFailed::Raised)?;
    Ok(name)
}

/// `lambda b, a: verify.judge_app(app, b, a)`
fn app_judge(app: &App) -> Judge {
    let app = app.clone();
    Box::new(move |b, a| verify::judge_app(&app, b.as_ref(), a.as_ref()))
}

/// `app_judge` for `checked_again`.
fn app_judge_again(app: &App) -> JudgeAgain {
    let app = app.clone();
    Arc::new(move |b, a| verify::judge_app(&app, b.as_ref(), a.as_ref()))
}

/// `after_pacman_cb` for `checked_again`.
fn after_pacman_again(win: &Win) -> Rc<dyn Fn(bool)> {
    let w = win.downgrade();
    Rc::new(move |ok| {
        if let Some(win) = w.upgrade() {
            win.after_pacman(ok);
        }
    })
}

/// `then=self._after_pacman`
fn after_pacman_cb(win: &Win) -> OnDone {
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

    const CSS: &str = include_str!("../../data/style.css");

    #[test]
    fn releases_url_from_github() {
        assert_eq!(releases_url("https://github.com/a/b/tree/main").as_deref(),
                   Some("https://github.com/a/b/releases"));
        assert_eq!(releases_url("example.com").as_deref(), Some("example.com"));
        assert_eq!(releases_url(""), None);
    }

    #[test]
    fn appimage_name_takes_stem_prefix() {
        assert_eq!(appimage_name(Path::new("/d/Foo-1.2-x86_64.AppImage")), "Foo");
        assert_eq!(appimage_name(Path::new("/d/-x.AppImage")), "-x");
    }

    /// Two different AppImages whose names start alike meet at one key:
    /// the second is not added over the first without asking.
    #[test]
    fn a_different_appimage_under_the_same_name_is_asked_about() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("apps");
        std::fs::create_dir_all(&store).unwrap();
        let tool = Path::new("/home/u/Downloads/my-tool-2.0.AppImage");
        assert_eq!(appimage_name(tool), "my");
        let mut reg = Map::new();
        assert_eq!(appimage_clash(&reg, "my", tool, &store), AddClash::Free);
        reg.insert("my".into(), serde_json::json!({"path": "/s/my.AppImage",
                                                    "source_file": "/home/u/Downloads/my-app-1.0.AppImage"}));
        reg.insert("my-2".into(), serde_json::json!({}));
        std::fs::write(store.join("my-3.AppImage"), b"").unwrap();
        assert_eq!(
            appimage_clash(&reg, "my", tool, &store),
            AddClash::Taken { existing: "/home/u/Downloads/my-app-1.0.AppImage".into(), spare: "my-4".into() }
        );
        // The same file added again refreshes its own entry.
        let app = Path::new("/home/u/Downloads/my-app-1.0.AppImage");
        assert_eq!(appimage_clash(&reg, "my", app, &store), AddClash::Free);
        // A file in the store that nothing tracks is in the way too.
        std::fs::write(store.join("solo.AppImage"), b"").unwrap();
        assert!(matches!(appimage_clash(&Map::new(), "solo", app, &store), AddClash::Taken { .. }));
    }

    #[test]
    fn stars_have_thousands_separators() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(1234567), "1,234,567");
    }

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
    fn copying_an_appimage_onto_itself_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("a.AppImage");
        std::fs::write(&f, b"keep me").unwrap();
        let err = copy2(&f, &f).unwrap_err();
        assert!(err.ends_with("are the same file"), "{err}");
        assert_eq!(std::fs::read(&f).unwrap(), b"keep me");
        let g = tmp.path().join("b.AppImage");
        copy2(&f, &g).unwrap();
        assert_eq!(std::fs::read(&g).unwrap(), b"keep me");
        let missing = tmp.path().join("nope");
        assert_eq!(copy2(&missing, &g).unwrap_err(),
                   format!("[Errno 2] No such file or directory: '{}'", missing.display()));
    }

    #[test]
    fn python_text_rules() {
        assert_eq!(py_strip(" \x1c\x1fx\x1f"), "x");
        assert_eq!(py_first_line("a\x1cb"), "a");
        assert_eq!(py_first_line("a\u{85}b"), "a");
        assert_eq!(py_first_line("a"), "a");
        let b: &[u8] = &[b'a', b'b', 0xe2, 0x82];
        let e = std::str::from_utf8(b).unwrap_err();
        assert_eq!(utf8_error(b, &e), "'utf-8' codec can't decode bytes in position 2-3: unexpected end of data");
        let b: &[u8] = &[0xff];
        let e = std::str::from_utf8(b).unwrap_err();
        assert_eq!(utf8_error(b, &e), "'utf-8' codec can't decode byte 0xff in position 0: invalid start byte");
        assert_eq!(py_str(&serde_json::json!(true)), "True");
        assert_eq!(py_str(&serde_json::json!([1, "a"])), "[1, 'a']");
        assert_eq!(py_path("./a//b/"), PathBuf::from("a/b"));
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
    fn update_all_waits_for_a_key() {
        assert_eq!(update_all_script("omarchy update"),
                   "omarchy update; echo; read -n1 -r -p 'Done — press any key to close'");
        assert_eq!(yay_argv("p").join(" "), "yay -S --needed --noconfirm --sudo pkexec p");
    }

    /// The code under test, without this test module.
    fn code() -> String {
        // file!() is this file, from the crate root.
        let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/", file!()))
            .unwrap_or_default();
        src.split("#[cfg(test)]").next().unwrap_or("").to_string()
    }

    /// test_every_pill_class_is_defined_in_the_stylesheet (this section)
    #[test]
    fn every_pill_class_is_defined_in_the_stylesheet() {
        const PILLS: [&str; 6] = ["pill", "pill-crit", "pill-warn", "pill-ok", "pill-accent", "pill-quiet"];
        let re = regex::Regex::new(r#""(pill(?:-[a-z]+)?)""#).unwrap();
        let code = code();
        assert!(!code.is_empty());
        let used: Vec<String> = re.captures_iter(&code).map(|c| c[1].to_string()).collect();
        assert!(!used.is_empty(), "no pill classes found at all");
        for name in &used {
            assert!(CSS.contains(&format!(".{name}")), "{name} is applied but never styled");
            assert!(PILLS.contains(&name.as_str()), "unknown pill variant: {name}");
        }
    }

    /// test_spacing_comes_from_the_scale (this section)
    #[test]
    fn spacing_comes_from_the_scale() {
        const SCALE: [i64; 8] = [2, 4, 6, 8, 12, 16, 20, 24];
        let re = regex::Regex::new(
            r"\b(?:set_)?(margin_(?:start|end|top|bottom)|spacing|child_spacing|line_spacing)\((\d+)\)").unwrap();
        let off: Vec<String> = re
            .captures_iter(&code())
            .filter(|c| !SCALE.contains(&c[2].parse::<i64>().unwrap_or(-1)))
            .map(|c| format!("{}={}", &c[1], &c[2]))
            .collect();
        assert!(off.is_empty(), "spacing off the scale: {off:?}");
    }

    /// test_every_icon_only_button_is_named (this section): any icon-only
    /// button made here must reach `name(` within a few lines.
    #[test]
    fn every_icon_only_button_is_named() {
        let code = code();
        let lines: Vec<&str> = code.lines().collect();
        let mut unnamed = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            if line.contains("Button::from_icon_name") || line.contains(".icon_name(") {
                let window = lines[i..(i + 12).min(lines.len())].join("\n");
                if !window.contains("name(&") && !window.contains("Property::Label") {
                    unnamed.push(format!("actions.rs:{}", i + 1));
                }
            }
        }
        assert!(unnamed.is_empty(), "{unnamed:?}");
    }

    /// test_every_icon_name_is_in_adwaita_or_shipped_here (this section)
    #[test]
    fn every_icon_name_is_in_adwaita_or_shipped_here() {
        let adwaita = Path::new("/usr/share/icons/Adwaita");
        if !adwaita.is_dir() {
            return;
        }
        let re = regex::Regex::new(r#""([a-z0-9-]+-symbolic)""#).unwrap();
        for c in re.captures_iter(&code()) {
            let n = &c[1];
            let shipped = Path::new(env!("CARGO_MANIFEST_DIR")).join("icons").join(format!("{n}.svg"));
            let found = shipped.exists()
                || glob::glob(&format!("{}/**/{n}.*", adwaita.display()))
                    .map(|mut g| g.next().is_some())
                    .unwrap_or(false);
            assert!(found || n.starts_with("adw-"), "missing icon {n}");
        }
    }

    /// test_no_personal_paths_in_the_source (this section)
    #[test]
    fn no_personal_paths_in_the_source() {
        let home = format!("{}/", crate::util::home().display());
        assert!(!code().contains(&home));
        assert!(!code().to_lowercase().contains("streamdock"));
    }
}
