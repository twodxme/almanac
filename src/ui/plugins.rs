//! UI section `plugins` (port of `app.py` lines 3695-3993 and 5636-5999):
//! the Plugins page; on/off, shortcuts, bulk updates, the shell — and the
//! row actions, install and remove that sit beside them in `app.py`.

use std::collections::BTreeMap;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use crate::history;
use crate::i18n::tr;
use crate::inventory;
use crate::plugins as plugins_mod;
use crate::shortcuts;
use crate::sources;
use crate::store as store_mod;
use crate::tasks;
use crate::ui::actions::Judge;
use crate::ui::shell::{name, source_label, spawn_bg, AppItem, WeakWin, Win};
use crate::util;
use crate::verify::{self, Outcome, ProbeValue, TryProbe};
use crate::xdg;

/// State only the Plugins section reads and writes.
/// What `plugin_logo` shows beside a plugin (see `plugin_logo_choice`).
/// The system icon shows first in every case; the others replace it when
/// their image arrives.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum LogoChoice {
    Vendor { key: String, domain: String },
    Art { thumb: String },
    Avatar { key: String, url: String },
    Symbol { name: String },
}


#[derive(Default)]
pub struct State {
    /// `self.plugins_body`: the page's column of sections.
    pub plugins_body: Option<gtk::Box>,
    /// `self._plugin_rows`: every row shown, with the plugin it stands for
    /// (searched by `plugins_filter`, looked up by `plugin_action`).
    /// Each row's plugin is the one object its switch updates, as in the
    /// Python.
    pub plugin_rows: Vec<(adw::ActionRow, std::rc::Rc<std::cell::RefCell<plugins_mod::Installed>>)>,
    // `self._plugin_updates` lives in `shell::Shared::plugin_updates`: the
    // header's primary action reads it too.
}

// ---- plugins -----------------------------------------------------------

/// `_PLUGIN_STATE_PILLS`: a plugin state key -> (pill label, css class).
pub const PLUGIN_STATE_PILLS: &[(&str, (&str, &str))] = &[
    ("update", ("Update waiting", "pill-accent")),
    ("modified", ("Local edits", "pill-warn")),
    ("ahead", ("Ahead of upstream", "pill-warn")),
    ("diverged", ("Diverged", "pill-warn")),
    ("detached", ("Not on a branch", "pill-warn")),
    ("symlink", ("Development link", "pill-quiet")),
    ("error", ("Could not check", "pill-warn")),
];

/// `key in _PLUGIN_STATE_PILLS` / `_PLUGIN_STATE_PILLS[key]`.
pub fn plugin_state_pill(key: &str) -> Option<(&'static str, &'static str)> {
    PLUGIN_STATE_PILLS.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// The pill key for a plugin state, as `_plugin_row` picks it: the kind for
/// "update" and "error", otherwise the detail ("modified", "symlink", ...).
pub fn plugin_state_key(st: Option<&plugins_mod::State>) -> String {
    match st {
        Some(st) if st.kind == "update" || st.kind == "error" => st.kind.clone(),
        Some(st) => st.detail.clone(),
        None => String::new(),
    }
}

/// Whether a row matches the search text, as `_plugins_filter` decides it.
pub fn plugin_matches(i: &plugins_mod::Installed, text: &str) -> bool {
    let q = py_strip(text).to_lowercase();
    if q.is_empty() {
        return true;
    }
    let hay = [i.id.as_str(), i.name.as_str(), i.description.as_str(), &i.kinds.join(" ")]
        .join(" ")
        .to_lowercase();
    hay.contains(&q)
}

/// The row menu `_plugin_row` builds: (label msgid, action name) in order.
/// Split out so the decision can be tested without a display.
///
/// `Err` where `i.has_panel` raises.
pub fn plugin_menu(
    i: &plugins_mod::Installed,
    shortcuts_available: bool,
) -> Result<Vec<(&'static str, &'static str)>, String> {
    let st = i.state.as_ref();
    let mut out = Vec::new();
    if let Some(st) = st {
        if st.updatable() || (st.detail == "modified" && st.behind != 0) {
            out.push(("Update…", "update"));
        }
    }
    if shortcuts_available && i.try_has_panel()? && i.folder.is_some() {
        out.push(("Keyboard shortcut…", "shortcut"));
    }
    if i.folder.is_some() {
        out.push(("Open folder", "folder"));
    }
    if st.is_some_and(|s| !s.url.is_empty()) {
        out.push(("Open project page", "page"));
    }
    if !i.built_in {
        out.push(("Remove…", "remove"));
    }
    Ok(out)
}

/// `f"{i.name} on" if on else f"{i.name} off"`: the switch's accessible name.
pub fn switch_name(plugin_name: &str, on: bool) -> String {
    if on {
        format!("{plugin_name} on")
    } else {
        format!("{plugin_name} off")
    }
}

/// The `_forget_shortcut` step body with the three `shortcuts` calls passed
/// in -- the seam the Python tests reach with `mock.patch.object`.
fn forget_shortcut_with(
    pid: &str,
    log: tasks::LogFn<'_>,
    available: &dyn Fn() -> bool,
    current: &dyn Fn(&str) -> Result<String, String>,
    set_shortcut: &dyn Fn(&str, Option<&str>) -> Result<String, String>,
) {
    let run = || -> Result<(), String> {
        if available() && !current(pid)?.is_empty() {
            log(&set_shortcut(pid, None)?);
        }
        Ok(())
    };
    if let Err(exc) = run() {
        log(&format!("its shortcut was left in bindings.lua: {exc}"));
    }
}

/// What `_confirm_plugin_updates` decides before it asks: the plugins to
/// update, those held back by the scope, those that need the user, and the
/// lines of the confirmation's detail.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UpdatePlan {
    pub ready: Vec<plugins_mod::State>,
    pub held: Vec<plugins_mod::State>,
    pub blocked: Vec<plugins_mod::State>,
    /// `None` for a scope `SCOPES` does not list: the Python's
    /// `dict(...)[scope]` raises KeyError there, once there is something to
    /// update.
    pub scope_label: Option<String>,
    pub lines: Vec<String>,
    /// What `s.describe()` raised building the "needs you" lines (the
    /// Python raises there, after the scope label is looked up).
    pub raised: Option<String>,
}

/// The planning half of `_confirm_plugin_updates`. `trust` maps plugin id
/// to its marketplace trust key.
pub fn plan_plugin_updates(
    states: &[plugins_mod::State],
    trust: &BTreeMap<String, String>,
    scope: &str,
) -> UpdatePlan {
    let ready: Vec<plugins_mod::State> = states
        .iter()
        .filter(|s| s.updatable() && plugins_mod::in_scope(trust.get(&s.id).map(String::as_str), scope))
        .cloned()
        .collect();
    let held: Vec<plugins_mod::State> =
        states.iter().filter(|s| s.updatable() && !ready.contains(s)).cloned().collect();
    let blocked: Vec<plugins_mod::State> = states
        .iter()
        .filter(|s| {
            (s.kind == plugins_mod::LOCAL_CHANGES || s.kind == plugins_mod::ERROR) && s.behind != 0
        })
        .cloned()
        .collect();
    let scope_label = plugins_mod::SCOPES
        .iter()
        .find(|(k, _l, _s)| *k == scope)
        .map(|(_k, l, _s)| (*l).to_string());
    let label = scope_label.as_deref().unwrap_or("");
    let mut lines: Vec<String> =
        ready.iter().map(|s| format!("{:<40} {} commit(s)", s.id, s.behind)).collect();
    if !held.is_empty() {
        lines.push(String::new());
        lines.push(format!("Held back (outside “{label}”):"));
        for s in &held {
            let t = trust.get(&s.id).map(String::as_str).unwrap_or("");
            lines.push(format!("  {} — {}", s.id, if t.is_empty() { "not listed" } else { t }));
        }
    }
    let mut raised = None;
    if !blocked.is_empty() {
        lines.push(String::new());
        lines.push("Not updated (needs you):".to_string());
        for s in &blocked {
            match s.try_describe() {
                Ok(d) => lines.push(format!("  {} — {d}", s.id)),
                Err(e) => {
                    raised = Some(e);
                    break;
                }
            }
        }
    }
    UpdatePlan { ready, held, blocked, scope_label, lines, raised }
}

/// The orphan split of `on_clean_orphans`: `(source, advice, name)` per row
/// -> (safe to remove, kept deliberately).
pub fn split_orphans<'a>(
    rows: impl IntoIterator<Item = (&'a str, &'a str, &'a str)>,
) -> (Vec<String>, Vec<String>) {
    let mut safe = Vec::new();
    let mut kept = Vec::new();
    for (source, advice, name) in rows {
        if source != inventory::ORPHAN {
            continue;
        }
        if advice == "keep" { &mut kept } else { &mut safe }.push(name.to_string());
    }
    (safe, kept)
}

/// What a removal asks before it runs, and the command it runs once the
/// user agrees -- the decision `on_remove` and `on_clean_orphans` make, with
/// `tasks.preview_removal` passed in (the seam the Python tests mock).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RemovalPlan {
    pub title: String,
    pub body: String,
    pub detail: String,
    pub destructive: bool,
    pub argv: Vec<String>,
}

type Preview<'a> = &'a dyn Fn(&[String]) -> (bool, String);

/// `on_remove` for a package; `Err` is what it says instead of asking.
pub fn plan_package_removal(name: &str, preview_removal: Preview<'_>) -> Result<RemovalPlan, String> {
    let (ok, preview) = preview_removal(&[name.to_string()]);
    if !ok {
        return Err(format!("Cannot remove {name}: {preview}"));
    }
    let count = py_splitlines(&preview).iter().filter(|l| !py_strip(l).is_empty()).count();
    Ok(RemovalPlan {
        title: format!("Remove {name}?"),
        body: format!(
            "-Rns also takes dependencies nothing else needs. \
             {count} package(s) would be removed, listed below. \
             Configuration files are not kept."
        ),
        detail: preview,
        destructive: true,
        argv: vec!["pacman".into(), "-Rns".into(), "--noconfirm".into(), name.to_string()],
    })
}

/// `on_clean_orphans`; `Err` is what it says instead of asking.
pub fn plan_orphan_cleanup<'a>(
    rows: impl IntoIterator<Item = (&'a str, &'a str, &'a str)>,
    preview_removal: Preview<'_>,
) -> Result<RemovalPlan, String> {
    let (safe, kept) = split_orphans(rows);

    if safe.is_empty() {
        let mut msg = "No orphans worth removing".to_string();
        if !kept.is_empty() {
            msg += &format!(" — {} kept deliberately (build tooling, debug packages)", kept.len());
        }
        return Err(msg);
    }

    let (ok, preview) = preview_removal(&safe);
    if !ok {
        return Err(format!("Could not work out what would be removed: {preview}"));
    }
    let kept_note = if kept.is_empty() {
        String::new()
    } else {
        format!("  {} kept deliberately (build tooling, debug packages)", kept.len())
    };
    let mut argv: Vec<String> = vec!["pacman".into(), "-Rns".into(), "--noconfirm".into()];
    argv.extend(safe.iter().cloned());
    Ok(RemovalPlan {
        title: format!("Remove {} orphaned package(s)?", safe.len()),
        body: tr("These are installed as dependencies but nothing needs them any \
                  more. Everything that would go is listed below.")
            + &kept_note,
        detail: preview,
        destructive: true,
        argv,
    })
}

/// `tasks.confirm`, as the removals reach it: the plan to show, and what to
/// do once the user agrees.
type Confirm<'a> = &'a dyn Fn(&RemovalPlan, Box<dyn Fn()>);

/// `tasks.run`, as the removals reach it: the task's title and steps.
type RunSteps = std::rc::Rc<dyn Fn(&str, Vec<tasks::Step>)>;

/// Ask with the plan; its command runs only from the confirmation.
fn ask_then_run(plan: RemovalPlan, title: String, label: String, confirm: Confirm<'_>, run: RunSteps) {
    let argv = plan.argv.clone();
    confirm(&plan, Box::new(move || run(&title, vec![tasks::Step::argv(label.clone(), argv.clone())])));
}

/// `on_remove` for a package, with `tasks.preview_removal`, `tasks.confirm`,
/// `tasks.run` and `_say` passed in -- the seams the Python tests mock.
fn remove_package(name: &str, preview: Preview<'_>, say: &dyn Fn(&str), confirm: Confirm<'_>, run: RunSteps) {
    match plan_package_removal(name, preview) {
        Ok(plan) => ask_then_run(plan, format!("Removing {name}"), format!("Removing {name}"), confirm, run),
        Err(msg) => say(&msg),
    }
}

/// `on_clean_orphans`, with the same seams as [`remove_package`].
fn clean_orphans<'a>(
    rows: impl IntoIterator<Item = (&'a str, &'a str, &'a str)>,
    preview: Preview<'_>,
    say: &dyn Fn(&str),
    confirm: Confirm<'_>,
    run: RunSteps,
) {
    match plan_orphan_cleanup(rows, preview) {
        Ok(plan) => ask_then_run(
            plan,
            "Removing orphans".to_string(),
            "Removing orphaned packages".to_string(),
            confirm,
            run,
        ),
        Err(msg) => say(&msg),
    }
}

/// Whether `classify` reached `_remote` for a plugin it gave this state:
/// every answer but the ones it settles from the checkout alone.
fn reached_remote(st: &plugins_mod::State) -> bool {
    !(st.kind == plugins_mod::LOCAL
        || st.detail == "detached"
        || (st.kind == plugins_mod::ERROR && st.detail != "fetch failed"))
}

/// The exception the Python's `_remote` raises when the git cache's
/// `"_plugins"` entry is `other`, neither a dict nor null: `found` and
/// `scratch` are what `installed` gave with an empty cache in its place.
/// A truthy value fails at `.get` the first time `_remote` runs; a falsy one
/// reads as `{}` and fails at the first fetch it stores.
fn foreign_cache_error(
    other: &serde_json::Value,
    found: &[plugins_mod::Installed],
    scratch: &plugins_mod::FetchCache,
) -> Option<(&'static str, String)> {
    use serde_json::Value as V;
    if crate::demo::active() {
        return None; // the demo list is returned before the cache is touched
    }
    let (tname, truthy) = match other {
        V::Bool(b) => ("bool", *b),
        V::Number(n) if n.is_i64() || n.is_u64() => ("int", n.as_f64().is_some_and(|f| f != 0.0)),
        V::Number(n) => ("float", n.as_f64().is_some_and(|f| f != 0.0)),
        V::String(t) => ("str", !t.is_empty()),
        V::Array(a) => ("list", !a.is_empty()),
        V::Null | V::Object(_) => return None,
    };
    if truthy {
        let reached = found.iter().filter_map(|i| i.state.as_ref()).any(reached_remote);
        reached.then(|| ("AttributeError", format!("'{tname}' object has no attribute 'get'")))
    } else if !scratch.is_empty() {
        let msg = if tname == "list" {
            "list indices must be integers or slices, not str".to_string()
        } else {
            format!("'{tname}' object does not support item assignment")
        };
        Some(("TypeError", msg))
    } else {
        None
    }
}

/// Switch a plugin through Omarchy, then show what actually holds.
///
/// The switch moves only once Omarchy's own list agrees; a refusal puts
/// it back and says why. `i` is the row's own plugin, updated in place.
fn plugin_switch_shared(
    w: &WeakWin,
    switch: &gtk::Switch,
    on: bool,
    i: std::rc::Rc<std::cell::RefCell<plugins_mod::Installed>>,
) -> glib::Propagation {
    switch.set_sensitive(false);

    let id = i.borrow().id.clone();
    let work = move || {
        let r = run_checked(&plugins_mod::toggle_argv(&id, on), 60).ok()?;
        // What `enabled()` raises ends the worker: the switch stays as it is.
        let now = match plugins_mod::try_enabled() {
            Ok(on) => on.get(&id).copied(),
            Err(e) => {
                uncaught_err(&e);
                return None;
            }
        };
        Some((r, now))
    };

    let w = w.clone();
    let switch = switch.clone();
    let done = move |res: Option<(util::Output, Option<bool>)>| {
        let Some((r, now)) = res else { return };
        let (nm, can_disable) = {
            let x = i.borrow();
            (x.name.clone(), x.can_disable)
        };
        let now_b = now.unwrap_or(false);
        switch.set_sensitive(can_disable);
        switch.set_state(now_b);
        switch.set_active(now_b);
        i.borrow_mut().enabled = now_b;
        name(&switch, &switch_name(&nm, now_b));
        let win = w.upgrade();
        let word = if on { "on" } else { "off" };
        if now == Some(on) {
            if let Some(w) = &win {
                w.say(&format!("{nm} {word}"), 5);
            }
            if let Err(e) = history::record_checked(history::ALMANAC, &format!("{nm} switched {word}"), "", "", &[]) {
                return uncaught_err(&e);
            }
        } else {
            let text = if r.stderr.is_empty() { &r.stdout } else { &r.stderr };
            let why: Vec<&str> = py_splitlines(py_strip(text));
            let body = why.last().map(|s| s.to_string()).unwrap_or_else(|| tr("Omarchy did not change it"));
            if let Some(w) = &win {
                w.notify(&format!("{nm} was not switched {word}"), &body, "normal");
            }
        }
        if let Some(w) = &win {
            // `if getattr(self, "_plugins", None): annotate(self._plugins)`;
            // what annotate raises ends this idle callback, logged.
            let mut shared = w.shared.borrow_mut();
            if !shared.plugins_none && !shared.plugins.is_empty() {
                if let Err(e) = store_mod::try_annotate(&mut shared.plugins) {
                    eprintln!("almanac: {e}");
                }
            }
        }
    };
    spawn_bg(work, done);
    glib::Propagation::Stop // the state is set when Omarchy answers
}

/// `str(OSError)` as Python prints it: `[Errno 2] No such file or
/// directory: 'x'`.
fn py_oserror(e: &std::io::Error, filename: &str) -> String {
    match e.raw_os_error() {
        Some(code) => {
            let text = e.to_string();
            let text = text.strip_suffix(&format!(" (os error {code})")).unwrap_or(&text);
            format!("[Errno {code}] {text}: {}", py_repr(filename))
        }
        None => e.to_string(),
    }
}

/// The OSError subclass Python raises for an errno, as its traceback names it.
fn py_oserror_class(e: &std::io::Error) -> &'static str {
    match e.raw_os_error() {
        Some(libc::ENOENT) => "FileNotFoundError",
        Some(libc::EACCES) | Some(libc::EPERM) => "PermissionError",
        Some(libc::ENOTDIR) => "NotADirectoryError",
        Some(libc::EISDIR) => "IsADirectoryError",
        _ => "OSError",
    }
}

/// What Python prints for an exception nothing catches: the traceback,
/// ending in `Class: message`. The frames are the interpreter's; only the
/// header and the last line can be reproduced.
fn uncaught(class: &str, message: &str) {
    eprintln!("Traceback (most recent call last):\n{class}: {message}");
}

/// `uncaught` for an error already spelled "Type: message".
fn uncaught_err(e: &str) {
    eprintln!("Traceback (most recent call last):\n{e}");
}

/// The `G_SPAWN_ERROR` domain, read back as the errno it was made from.
#[derive(Clone, Copy)]
struct SpawnCode(i32);

impl glib::error::ErrorDomain for SpawnCode {
    fn domain() -> glib::Quark {
        glib::Quark::from_str("g-exec-error-quark")
    }
    fn code(self) -> i32 {
        self.0
    }
    fn from(code: i32) -> Option<Self> {
        Some(SpawnCode(code))
    }
}

fn spawn_errno(e: &glib::Error) -> std::io::Error {
    use glib::ffi as g;
    let Some(SpawnCode(code)) = e.kind::<SpawnCode>() else {
        return std::io::Error::other(e.message().to_string());
    };
    let errno = match code {
        g::G_SPAWN_ERROR_ACCES => libc::EACCES,
        g::G_SPAWN_ERROR_PERM => libc::EPERM,
        g::G_SPAWN_ERROR_TOO_BIG => libc::E2BIG,
        g::G_SPAWN_ERROR_NOEXEC => libc::ENOEXEC,
        g::G_SPAWN_ERROR_NAMETOOLONG => libc::ENAMETOOLONG,
        g::G_SPAWN_ERROR_NOENT => libc::ENOENT,
        g::G_SPAWN_ERROR_NOMEM => libc::ENOMEM,
        g::G_SPAWN_ERROR_NOTDIR => libc::ENOTDIR,
        g::G_SPAWN_ERROR_LOOP => libc::ELOOP,
        g::G_SPAWN_ERROR_TXTBUSY => libc::ETXTBSY,
        g::G_SPAWN_ERROR_IO => libc::EIO,
        g::G_SPAWN_ERROR_NFILE => libc::ENFILE,
        g::G_SPAWN_ERROR_MFILE => libc::EMFILE,
        g::G_SPAWN_ERROR_INVAL => libc::EINVAL,
        g::G_SPAWN_ERROR_ISDIR => libc::EISDIR,
        g::G_SPAWN_ERROR_LIBBAD => libc::ELIBBAD,
        _ => return std::io::Error::other(e.message().to_string()),
    };
    std::io::Error::from_raw_os_error(errno)
}

/// `subprocess.Popen(argv, start_new_session=True)`.
fn popen_detached(argv: &[String]) -> std::io::Result<()> {
    if argv.is_empty() {
        return Err(std::io::Error::from_raw_os_error(libc::ENOENT));
    }
    let launcher = gio::SubprocessLauncher::new(gio::SubprocessFlags::STDIN_INHERIT);
    launcher.set_child_setup(|| {
        let _ = rustix::process::setsid();
    });
    let args: Vec<&std::ffi::OsStr> = argv.iter().map(std::ffi::OsStr::new).collect();
    launcher.spawn(&args).map(drop).map_err(|e| spawn_errno(&e))
}

/// Python's `str.isprintable()` for one character: everything but the
/// Unicode categories Other (Cc, Cf, Cs, Co, Cn) and Separator (Zl, Zp, Zs),
/// with the ASCII space let through.
fn py_printable(c: char) -> bool {
    static NON_PRINTABLE: std::sync::OnceLock<Option<regex::Regex>> = std::sync::OnceLock::new();
    if c == ' ' {
        return true;
    }
    match NON_PRINTABLE.get_or_init(|| regex::Regex::new(r"^[\p{C}\p{Z}]$").ok()) {
        Some(re) => !re.is_match(c.encode_utf8(&mut [0; 4])),
        None => !c.is_control(),
    }
}

/// Python's `repr(str)`.
fn py_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::from(quote);
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
            c if py_printable(c) => out.push(c),
            c if (c as u32) < 0x100 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if (c as u32) < 0x10000 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push_str(&format!("\\U{:08x}", c as u32)),
        }
    }
    out.push(quote);
    out
}

/// Python's `str(value)` of a JSON value as `json.loads` would hand it over.
fn py_str_value(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => py_repr_value(other),
    }
}

/// Python's `repr(value)` of a JSON value.
fn py_repr_value(v: &serde_json::Value) -> String {
    use serde_json::Value as V;
    match v {
        V::Null => "None".to_string(),
        V::Bool(true) => "True".to_string(),
        V::Bool(false) => "False".to_string(),
        V::Number(n) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
            (Some(i), _, _) => i.to_string(),
            (None, Some(u), _) => u.to_string(),
            (None, None, Some(f)) => util::py_float_repr(f),
            _ => n.to_string(),
        },
        V::String(s) => py_repr(s),
        V::Array(a) => format!("[{}]", a.iter().map(py_repr_value).collect::<Vec<_>>().join(", ")),
        V::Object(o) => format!(
            "{{{}}}",
            o.iter().map(|(k, v)| format!("{}: {}", py_repr(k), py_repr_value(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// Python's `str.splitlines()`.
fn py_splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        let end = match c {
            '\r' => {
                if let Some(&(_, '\n')) = it.peek() {
                    it.next();
                    i + 2
                } else {
                    i + 1
                }
            }
            '\n' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}' => {
                i + c.len_utf8()
            }
            _ => continue,
        };
        out.push(&s[start..i]);
        start = end;
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// Python's `str.strip()`.
fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c))
}

/// `subprocess.run(argv, capture_output=True, text=True, timeout=n)`, with
/// the two exceptions it raises as `Err` (TimeoutExpired, OSError).
///
/// Started here rather than through `util::run`, which reports a failed
/// start only as a flag: the OSError text carries the real errno
/// (`[Errno 13] Permission denied: 'omarchy'`), and it reaches the task log.
fn run_checked(argv: &[String], timeout: u64) -> Result<util::Output, String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let Some((prog, args)) = argv.split_first() else {
        // Popen([]) raises IndexError before anything starts.
        return Err("list index out of range".to_string());
    };
    let mut child = Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| py_oserror(&e, prog))?;
    // Drain both pipes on their own threads, or a chatty child fills one and
    // blocks forever while we wait on it.
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut s = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut s);
            }
            String::from_utf8_lossy(&s).into_owned()
        })
    };
    let out_t = drain(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err_t = drain(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let start = std::time::Instant::now();
    let limit = std::time::Duration::from_secs(timeout);
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) if start.elapsed() >= limit => {
                let _ = child.kill();
                let _ = child.wait();
                let list: Vec<String> = argv.iter().map(|a| py_repr(a)).collect();
                return Err(format!("Command '[{}]' timed out after {timeout} seconds", list.join(", ")));
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
            Err(_) => break None,
        }
    };
    let stdout = out_t.join().unwrap_or_default();
    let stderr = err_t.join().unwrap_or_default();
    Ok(util::Output { code: status.and_then(|s| s.code()), stdout, stderr, timed_out: false, failed_to_start: false })
}

/// Python's `bool(a)` of a probe answer.
fn truthy(v: &Option<ProbeValue>) -> bool {
    match v {
        None => false,
        Some(ProbeValue::Bool(b)) => *b,
        Some(ProbeValue::Str(s)) => !s.is_empty(),
        Some(ProbeValue::Set(s)) => !s.is_empty(),
        Some(ProbeValue::Map(m)) => !m.is_empty(),
        Some(ProbeValue::Identity(..)) => true,
    }
}

fn extra_str(app: &inventory::App, key: &str) -> String {
    app.extra.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string()
}

impl Win {
    /// Every plugin on the system, with a switch each. See plugins.py.
    pub fn build_plugins_page(&self) -> gtk::Widget {
        let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let plugins_body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_start(12)
            .margin_end(12)
            .margin_bottom(16)
            .build();
        let scroller = gtk::ScrolledWindow::builder().vexpand(true).build();
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroller.set_child(Some(&plugins_body));
        page.append(&scroller);
        {
            let mut st = self.plugins.borrow_mut();
            st.plugins_body = Some(plugins_body);
            st.plugin_rows = Vec::new();
        }
        self.shared.borrow_mut().plugin_updates = 0;
        page.upcast()
    }

    pub fn plugins_reload(&self) {
        self.summarize(&tr("Reading plugins…"), Some("plugins"));

        // `getattr(self, "_plugins", None)`, read here: the worker cannot
        // reach the window.
        let have = self.shared.borrow().plugins.clone();
        let w = self.downgrade();
        spawn_bg(
            move || {
                let mut cache = inventory::git_cache();
                let entry = cache
                    .entry("_plugins".to_string())
                    .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
                // What `installed` raises ends the worker: the page stays at
                // "Reading plugins…".
                let found = match entry {
                    serde_json::Value::Object(m) => plugins_mod::try_installed(true, Some(m)),
                    serde_json::Value::Null => plugins_mod::try_installed(true, None),
                    other => {
                        // `setdefault` hands back whatever is stored, and
                        // `_remote` then reads it with `.get` (or, when it is
                        // falsy, writes into it): on anything but a dict that
                        // raises, the worker dies, and the page stays at
                        // "Reading plugins…".
                        let mut scratch = plugins_mod::FetchCache::new();
                        let found = plugins_mod::try_installed(true, Some(&mut scratch));
                        if let Some(exc) = found.as_ref().ok().and_then(|f| foreign_cache_error(other, f, &scratch)) {
                            uncaught(exc.0, &exc.1);
                            return None;
                        }
                        found
                    }
                };
                let found = match found {
                    Ok(found) => found,
                    Err(e) => {
                        uncaught_err(&e);
                        return None;
                    }
                };
                // `except Exception: trust = {}`
                let catalog = if have.is_empty() { store_mod::try_catalog(false) } else { Ok(have) };
                let trust: BTreeMap<String, store_mod::Plugin> = match catalog {
                    Ok(catalog) => catalog.into_iter().map(|c| (c.id.clone(), c)).collect(),
                    Err(_) => BTreeMap::new(),
                };
                let raw = if shortcuts::available() {
                    shortcuts::_read().map(|r| r.0).unwrap_or_default()
                } else {
                    String::new()
                };
                let mut keys = BTreeMap::new();
                for i in &found {
                    // `i.has_panel` raising ends the worker.
                    let panel = !raw.is_empty()
                        && match i.try_has_panel() {
                            Ok(panel) => panel,
                            Err(e) => {
                                uncaught_err(&e);
                                return None;
                            }
                        };
                    if panel {
                        keys.insert(i.id.clone(), shortcuts::current(&i.id, Some(&raw)).unwrap_or_default());
                    }
                }
                Some((found, trust, keys))
            },
            move |res| {
                let Some((found, trust, keys)) = res else { return };
                if let Some(w) = w.upgrade() {
                    w.plugins_show(found, trust, keys);
                }
            },
        );
    }

    pub fn plugins_show(
        &self,
        found: Vec<plugins_mod::Installed>,
        trust: BTreeMap<String, store_mod::Plugin>,
        keys: BTreeMap<String, String>,
    ) {
        let Some(body) = self.plugins.borrow().plugins_body.clone() else { return };
        while let Some(child) = body.first_child() {
            body.remove(&child);
        }
        let old = std::mem::take(&mut self.plugins.borrow_mut().plugin_rows);
        drop(old);
        let mine: Vec<&plugins_mod::Installed> = found.iter().filter(|i| !i.built_in).collect();
        let builtin: Vec<&plugins_mod::Installed> = found.iter().filter(|i| i.built_in).collect();
        for (title, note, group) in [
            (
                tr("Your plugins"),
                tr("Added from the marketplace or by hand. Updated through \
                    omarchy plugin update; a development link never is."),
                &mine,
            ),
            (
                tr("Built in"),
                tr("Ship with Omarchy and update with it. They can be switched \
                    on and off."),
                &builtin,
            ),
        ] {
            if group.is_empty() {
                continue;
            }
            let section = Win::section(&format!("{title} ({})", group.len()), &note, None);
            for i in group.iter() {
                let listing = trust.get(&i.id);
                // What raises building a row (`listing.trust`, the logo's
                // icon cache) ends `_plugins_show` there.
                let t = match listing.map(|l| l.try_trust()).transpose() {
                    Ok(t) => t,
                    Err(e) => return uncaught_err(&e),
                };
                let row = match self.plugin_row(
                    i,
                    t.as_ref(),
                    keys.get(&i.id).map(String::as_str).unwrap_or(""),
                    listing,
                ) {
                    Ok(row) => row,
                    Err(e) => return uncaught_err(&e),
                };
                section.rows.append(&row.0);
                self.plugins.borrow_mut().plugin_rows.push(row);
            }
            body.append(&section.root);
        }
        if found.is_empty() {
            body.append(&Win::status_page(
                "application-x-addon-symbolic",
                &tr("No plugins found"),
                &tr("The omarchy command did not list any plugins."),
                None,
            ));
        }
        let updates = mine.iter().filter(|i| i.state.as_ref().is_some_and(|s| s.updatable())).count();
        self.shared.borrow_mut().plugin_updates = updates;
        let on = found.iter().filter(|i| i.enabled).count();
        let text = format!("{} plugins · {on} on", found.len())
            + &if updates != 0 { format!(" · {updates} with updates") } else { String::new() };
        self.summarize(&text, Some("plugins"));
        self.plugins_filter(&self.search.text());
        self.sync_primary();
    }

    pub fn plugins_filter(&self, text: &str) {
        let rows = self.plugins.borrow().plugin_rows.clone();
        for (row, i) in rows {
            let hit = plugin_matches(&i.borrow(), text);
            row.set_visible(hit);
        }
    }

    pub fn plugin_row(
        &self,
        i: &plugins_mod::Installed,
        trust: Option<&store_mod::Trust>,
        shortcut: &str,
        listing: Option<&store_mod::Plugin>,
    ) -> Result<(adw::ActionRow, std::rc::Rc<std::cell::RefCell<plugins_mod::Installed>>), String> {
        let subtitle = if i.description.is_empty() { &i.id } else { &i.description };
        let row: adw::ActionRow = Win::plain(&i.name, subtitle);
        row.set_title_lines(1);
        row.set_subtitle_lines(1);
        row.set_tooltip_text(Some(subtitle));
        row.add_prefix(&self.plugin_logo(i, listing)?);

        let st = i.state.as_ref();
        let key = plugin_state_key(st);
        if let Some((label, css)) = plugin_state_pill(&key) {
            let pill = gtk::Label::builder().label(tr(label)).valign(gtk::Align::Center).build();
            pill.add_css_class(css);
            pill.set_tooltip_text(Some(&st.map(|s| s.try_describe()).transpose()?.unwrap_or_default()));
            row.add_suffix(&pill);
        }
        if let Some(trust) = trust {
            if !i.built_in {
                let chip = gtk::Label::builder().label(&trust.label).valign(gtk::Align::Center).build();
                chip.add_css_class(&trust.css);
                chip.set_tooltip_text(Some(&trust.why));
                row.add_suffix(&chip);
            }
        }
        if !shortcut.is_empty() {
            let keys = gtk::Label::builder()
                .label(shortcut.replace(" + ", "+"))
                .valign(gtk::Align::Center)
                .build();
            keys.add_css_class("dim-label");
            keys.add_css_class("caption");
            keys.set_tooltip_text(Some(&tr("Keyboard shortcut")));
            row.add_suffix(&keys);
        }

        // The actions, behind a menu; the switch beside it is the one control
        // that is always one click.
        let menu = gio::Menu::new();
        for (label, act) in plugin_menu(i, shortcuts::available())? {
            menu.append(Some(&tr(label)), Some(&format!("plugin.{act}::{}", i.id)));
        }
        if menu.n_items() != 0 {
            let more = gtk::MenuButton::builder()
                .icon_name("view-more-symbolic")
                .valign(gtk::Align::Center)
                .menu_model(&menu)
                .build();
            more.add_css_class("flat");
            name(&more, &format!("More actions for {}", i.name));
            row.add_suffix(&more);
        }

        let switch = gtk::Switch::builder()
            .active(i.enabled)
            .valign(gtk::Align::Center)
            .sensitive(i.can_disable)
            .build();
        name(&switch, &switch_name(&i.name, i.enabled));
        if !i.can_disable {
            switch.set_tooltip_text(Some(&tr("Omarchy needs this one; it cannot be \
                                              switched off")));
        }
        let shared = std::rc::Rc::new(std::cell::RefCell::new(i.clone()));
        let w = self.downgrade();
        let ic = shared.clone();
        switch.connect_state_set(move |sw, on| plugin_switch_shared(&w, sw, on, ic.clone()));
        row.add_suffix(&switch);
        row.set_activatable_widget(Some(&switch));
        Ok((row, shared))
    }

    /// The mark beside a plugin, best source first (see plugins.py):
    /// the vendor's own logo for a plugin about a service, the marketplace
    /// artwork for a listed one, a system icon for what it does, the
    /// author's GitHub avatar, and a generic plugin icon last.
    ///
    /// The system icon shows at once; a logo replaces it when it arrives,
    /// so rows never jump.
    pub fn plugin_logo(
        &self,
        i: &plugins_mod::Installed,
        listing: Option<&store_mod::Plugin>,
    ) -> Result<gtk::Box, String> {
        let frame = gtk::Box::builder()
            .width_request(36)
            .height_request(36)
            .hexpand(false)
            .valign(gtk::Align::Center)
            .halign(gtk::Align::Center)
            .build();
        frame.add_css_class("plugin-logo");
        frame.set_overflow(gtk::Overflow::Hidden);
        let symbol = plugins_mod::symbol_for(i);
        let image = gtk::Image::builder()
            .icon_name(&symbol)
            .pixel_size(22)
            .hexpand(true)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .accessible_role(gtk::AccessibleRole::Presentation)
            .build();
        frame.append(&image);

        let show = {
            let w = self.downgrade();
            let frame = frame.clone();
            move |path: Option<std::path::PathBuf>, cover: bool| {
                let Some(path) = path else { return };
                let Some(w) = w.upgrade() else { return };
                let tex = if cover { w.square(&path, 72) } else { w.texture(&path, 72) };
                let Some(tex) = tex else { return };
                if let Some(first) = frame.first_child() {
                    frame.remove(&first);
                }
                if cover {
                    // Already square and small: an Image at a fixed pixel size
                    // cannot ask for more room than the tile has.
                    let pic = gtk::Image::builder()
                        .paintable(&tex)
                        .pixel_size(36)
                        .accessible_role(gtk::AccessibleRole::Presentation)
                        .build();
                    frame.append(&pic);
                    frame.add_css_class("has-art");
                } else {
                    let logo = gtk::Image::builder()
                        .paintable(&tex)
                        .pixel_size(26)
                        .hexpand(true)
                        .halign(gtk::Align::Center)
                        .valign(gtk::Align::Center)
                        .accessible_role(gtk::AccessibleRole::Presentation)
                        .build();
                    frame.append(&logo);
                }
            }
        };

        // `icons.cached` raising ends `_plugin_logo` (and the row with it);
        // `icons.fetch` raising on the pool is kept by its future, unseen,
        // and nothing is shown.
        let fetch = |key: String, url: String, domain: String| -> Result<(), String> {
            if let Some(hit) = crate::icons::cached_checked(&key)? {
                show(Some(hit), false);
                return Ok(());
            }
            let show = show.clone();
            self.thumb_bg(
                move || crate::icons::fetch_checked(&key, &url, &domain),
                move |path| {
                    if let Ok(path) = path {
                        show(path, false);
                    }
                },
            );
            Ok(())
        };

        match Win::plugin_logo_choice(i, listing) {
            LogoChoice::Vendor { key, domain } => fetch(key, String::new(), domain)?,
            LogoChoice::Avatar { key, url } => fetch(key, url, String::new())?,
            LogoChoice::Symbol { .. } => {}
            LogoChoice::Art { .. } => {
                let Some(listing) = listing else { return Ok(frame) };
                let cached = store_mod::thumb_path(listing, false);
            let present = cached
                .as_ref()
                .is_some_and(|c| c.exists() && std::fs::metadata(c).map(|m| m.len() != 0).unwrap_or(false));
                if present {
                    show(cached, true);
                } else {
                    let listing = listing.clone();
                    let show = show.clone();
                    self.thumb_bg(
                        move || store_mod::fetch_thumb(&listing, false),
                        move |path| show(path, true),
                    );
                }
            }
        }
        Ok(frame)
    }

    /// The mark `plugin_logo` puts beside a plugin, decided without a
    /// window: the vendor's logo, the marketplace art, the author's avatar
    /// (only where no system icon says more), or the system icon alone.
    pub fn plugin_logo_choice(i: &plugins_mod::Installed, listing: Option<&store_mod::Plugin>) -> LogoChoice {
        let symbol = plugins_mod::symbol_for(i);
        let domain = plugins_mod::vendor_for(i);
        let owner = plugins_mod::github_owner(i.state.as_ref().map(|s| s.url.as_str()).unwrap_or(""));
        if !domain.is_empty() {
            LogoChoice::Vendor { key: domain.clone(), domain }
        } else if let Some(listing) = listing.filter(|l| !l.thumb.is_empty()) {
            LogoChoice::Art {
                thumb: store_mod::thumb_path(listing, false)
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            }
        } else if symbol == plugins_mod::GENERIC && !owner.is_empty() {
            LogoChoice::Avatar { key: format!("github-{owner}"), url: crate::icons::github_avatar(&owner) }
        } else {
            LogoChoice::Symbol { name: symbol }
        }
    }

    /// A task step: drop a removed plugin's shortcut. Otherwise the
    /// binding outlives the plugin -- the key stays taken and opens nothing.
    /// Never fails the removal; a binding it cannot remove is reported.
    pub fn forget_shortcut(pid: &str) -> tasks::Step {
        let pid = pid.to_string();
        let f: tasks::StepFn = std::sync::Arc::new(move |log: tasks::LogFn<'_>| {
            forget_shortcut_with(
                &pid,
                log,
                &shortcuts::available,
                &|p| shortcuts::current(p, None),
                &shortcuts::set_shortcut,
            );
            Ok(())
        });
        tasks::Step::call(tr("Removing its keyboard shortcut"), f)
    }

    /// Switch a plugin through Omarchy, then show what actually holds.
    ///
    /// The switch moves only once Omarchy's own list agrees; a refusal puts
    /// it back and says why.
    pub fn plugin_switch(
        &self,
        switch: &gtk::Switch,
        on: bool,
        i: std::rc::Rc<std::cell::RefCell<plugins_mod::Installed>>,
    ) -> glib::Propagation {
        // `i` is the caller's own object (the store dialog's `info`), shared,
        // so what `done` writes to it the caller sees on the next toggle.
        plugin_switch_shared(&self.downgrade(), switch, on, i)
    }

    pub fn plugin_action(&self, action: &str, pid: &str) {
        let found = self
            .plugins
            .borrow()
            .plugin_rows
            .iter()
            .find(|(_r, x)| x.borrow().id == pid)
            .map(|(_r, x)| x.borrow().clone());
        let Some(i) = found else { return };
        let st = i.state.clone();
        if action == "shortcut" {
            self.edit_shortcut(&i.id, &i.name);
        } else if action == "folder" && i.folder.is_some() {
            if let Some(folder) = &i.folder {
                self.open_url(&format!("file://{}", folder.display()));
            }
        } else if action == "page" && st.as_ref().is_some_and(|s| !s.url.is_empty()) {
            let url = st.as_ref().map(|s| s.url.clone()).unwrap_or_default();
            let page = inventory::github_page(&url);
            self.open_url(if page.is_empty() { &url } else { &page });
        } else if action == "update" && i.folder.is_some() {
            let mut extra = serde_json::Map::new();
            let folder = i.folder.as_ref().map(|f| f.display().to_string()).unwrap_or_default();
            extra.insert("repo".into(), folder.into());
            extra.insert("plugin_id".into(), i.id.clone().into());
            extra.insert("plugin_state".into(), st.as_ref().map(|s| s.kind.clone()).unwrap_or_default().into());
            let detail = match st.as_ref().map(|s| s.try_describe()).transpose() {
                Ok(detail) => detail.unwrap_or_default(),
                Err(e) => return uncaught_err(&e),
            };
            extra.insert("plugin_detail".into(), detail.into());
            let app = inventory::App {
                update: st.as_ref().map(|s| format!("{} commits", s.behind)).unwrap_or_default(),
                extra: extra.into(),
                ..inventory::App::new(&i.id, &i.version, inventory::LOCAL)
            };
            self.on_git_pull(&app);
        } else if action == "remove" {
            let argv: Vec<String> =
                ["omarchy", "plugin", "remove", &i.id, "--yes"].iter().map(|s| s.to_string()).collect();
            let folder = i.folder.as_ref().map(|f| f.display().to_string()).unwrap_or_else(|| "None".into());
            let w = self.downgrade();
            tasks::confirm(
                &self.window,
                &format!("Remove {}?", i.name),
                &tr("Deletes the plugin's folder and takes it off the bar. Its \
                     settings in shell.json are removed with it."),
                &format!("{folder}\n\n$ {}", argv.join(" ")),
                &tr("Remove"),
                true,
                move || {
                    let Some(w) = w.upgrade() else { return };
                    let id = i.id.clone();
                    let probe: TryProbe = std::sync::Arc::new(move || {
                        Ok(Some(ProbeValue::Bool(plugins_mod::try_enabled()?.contains_key(&id))))
                    });
                    let nm = i.name.clone();
                    let judge: Judge = Box::new(move |_b, a| {
                        let a = truthy(&a);
                        Outcome::new(
                            Some(!a),
                            if !a { format!("{nm} removed") } else { format!("{nm} is still installed") },
                        )
                    });
                    let wt = w.downgrade();
                    let then: tasks::OnDone = Box::new(move |ok| {
                        if let Some(w) = wt.upgrade() {
                            w.plugins_reload();
                            w.after_pacman(ok);
                        }
                    });
                    let on_done = match w.checked(Some(probe), judge, Some(then)) {
                        Ok(on_done) => on_done,
                        Err(e) => return uncaught_err(&e),
                    };
                    tasks::run(
                        &w.window,
                        &format!("Removing {}", i.name),
                        vec![
                            tasks::Step::argv(format!("Removing {}", i.name), argv.clone()),
                            Win::forget_shortcut(&i.id),
                        ],
                        Some(on_done),
                    );
                },
            );
        }
    }

    // ---- plugins: on/off, shortcuts, bulk updates, the shell ----------------

    pub fn row_toggle(&self, name: &str) {
        let app = self.shared.borrow().row_actions.get(name).cloned();
        if let Some(app) = app {
            let pid = extra_str(&app, "plugin_id");
            if !pid.is_empty() {
                let enabled = app.extra.get("enabled").is_some_and(|v| match v {
                    serde_json::Value::Bool(b) => *b,
                    serde_json::Value::Null => false,
                    serde_json::Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
                    serde_json::Value::String(s) => !s.is_empty(),
                    serde_json::Value::Array(a) => !a.is_empty(),
                    serde_json::Value::Object(o) => !o.is_empty(),
                });
                self.toggle_plugin(&pid, !enabled, &app.name);
            }
        }
    }

    /// Switch a plugin on or off through Omarchy, then check it took.
    pub fn toggle_plugin(&self, pid: &str, on: bool, label: &str) {
        let label = if label.is_empty() { pid.to_string() } else { label.to_string() };
        let p = pid.to_string();
        let probe: TryProbe =
            std::sync::Arc::new(move || Ok(plugins_mod::try_enabled()?.get(&p).map(|b| ProbeValue::Bool(*b))));
        let lb = label.clone();
        let judge: Judge = Box::new(move |_b, a| {
            let a = match a {
                Some(ProbeValue::Bool(b)) => Some(b),
                _ => None,
            };
            Outcome::new(
                Some(a == Some(on)),
                match a {
                    Some(a) => format!("{lb} is now {}", if a { "on" } else { "off" }),
                    None => format!("{lb}: state unknown"),
                },
            )
        });
        let w = self.downgrade();
        let then: tasks::OnDone = Box::new(move |_ok| {
            if let Some(w) = w.upgrade() {
                w.reload();
                let have = !w.shared.borrow().plugins.is_empty();
                if have {
                    w.store_refresh();
                }
            }
        });
        // `_checked` asks the probe at once; what it raises ends this
        // before the task starts.
        let on_done = match self.checked(Some(probe), judge, Some(then)) {
            Ok(on_done) => on_done,
            Err(e) => return uncaught_err(&e),
        };
        tasks::run(
            &self.window,
            &format!("{} {label}", if on { "Turning on" } else { "Turning off" }),
            vec![tasks::Step::argv(
                format!("omarchy plugin {} {pid}", if on { "enable" } else { "disable" }),
                plugins_mod::toggle_argv(pid, on),
            )],
            Some(on_done),
        );
    }

    pub fn row_shortcut(&self, name: &str) {
        let app = self.shared.borrow().row_actions.get(name).cloned();
        if let Some(app) = app {
            let pid = extra_str(&app, "plugin_id");
            if !pid.is_empty() {
                self.edit_shortcut(&pid, &app.name);
            }
        }
    }

    /// Record a key combination and bind it to opening the plugin.
    pub fn edit_shortcut(&self, pid: &str, label: &str) {
        let existing = match shortcuts::current(pid, None) {
            Ok(e) => e,
            Err(exc) => {
                self.say(&exc, 8);
                return;
            }
        };
        let entry = gtk::Entry::builder()
            .placeholder_text(tr("Press the keys, e.g. Super + Alt + V"))
            .hexpand(true)
            .build();
        entry.set_text(&existing);
        let keys = gtk::EventControllerKey::new();

        let weak_entry = entry.downgrade();
        keys.connect_key_pressed(move |_c, keyval, _code, state| {
            let Some(entry) = weak_entry.upgrade() else { return glib::Propagation::Proceed };
            let name = keyval.name().map(|n| n.to_uppercase()).unwrap_or_default();
            if matches!(
                name.as_str(),
                "SUPER_L" | "SUPER_R" | "CONTROL_L" | "CONTROL_R" | "ALT_L" | "ALT_R" | "SHIFT_L"
                    | "SHIFT_R" | "META_L" | "META_R"
            ) {
                return glib::Propagation::Proceed; // wait for the actual key
            }
            let mut mods: Vec<String> = [
                ("SUPER", gdk::ModifierType::SUPER_MASK),
                ("CTRL", gdk::ModifierType::CONTROL_MASK),
                ("ALT", gdk::ModifierType::ALT_MASK),
                ("SHIFT", gdk::ModifierType::SHIFT_MASK),
            ]
            .iter()
            .filter(|(_m, mask)| state.intersects(*mask))
            .map(|(m, _mask)| m.to_string())
            .collect();
            if mods.is_empty() {
                return glib::Propagation::Proceed; // plain typing still edits the text
            }
            mods.push(name);
            entry.set_text(&mods.join(" + "));
            entry.set_position(-1);
            glib::Propagation::Stop
        });
        entry.add_controller(keys);

        let dialog = adw::AlertDialog::new(
            Some(&format!("Keyboard shortcut for {label}")),
            Some(&tr("Opens the plugin's panel. Checked against every binding \
                      Hyprland has now; written to its own block in \
                      bindings.lua, and put back if Hyprland rejects it.")),
        );
        dialog.set_extra_child(Some(&entry));
        dialog.add_response("cancel", &tr("Cancel"));
        if !existing.is_empty() {
            dialog.add_response("remove", &tr("Remove shortcut"));
            dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
        }
        dialog.add_response("save", &tr("Save"));
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("save"));
        dialog.set_close_response("cancel");

        let w = self.downgrade();
        let pid = pid.to_string();
        let entry_c = entry.clone();
        dialog.connect_response(None, move |_d, resp| {
            if resp != "save" && resp != "remove" {
                return;
            }
            let combo = if resp == "remove" { None } else { Some(entry_c.text().to_string()) };
            let pid = pid.clone();
            let w = w.clone();
            spawn_bg(
                // What record raises (a log that is not UTF-8) is a
                // ValueError: caught with the rest.
                move || match shortcuts::set_shortcut(&pid, combo.as_deref()) {
                    Ok(msg) => match history::record_checked(history::ALMANAC, &msg, "", "", &[]) {
                        Ok(()) => (msg, true),
                        Err(exc) => (format!("Shortcut not saved: {}", crate::ui::shell::exc_str(&exc)), false),
                    },
                    Err(exc) => (format!("Shortcut not saved: {exc}"), false),
                },
                move |(msg, saved)| {
                    let Some(w) = w.upgrade() else { return };
                    let on_page = w.shared.borrow().page_or("") == "plugins";
                    if saved && on_page {
                        w.plugins_reload();
                    }
                    w.say(&msg, 8);
                },
            );
        });
        dialog.present(Some(&self.window));
        entry.grab_focus();
    }

    /// Update every plugin with a clean update waiting, within the scope
    /// chosen in Preferences; say which were held back and why.
    pub fn on_update_plugins(&self) {
        let have = self.shared.borrow().plugins.clone();
        // What raises in here ends the Python's worker: nothing is asked.
        let work = move || -> Result<_, String> {
            let plugins = if have.is_empty() { store_mod::try_catalog(false)? } else { have };
            let mut trust: BTreeMap<String, String> = BTreeMap::new();
            for p in &plugins {
                trust.insert(p.id.clone(), p.try_trust()?.key);
            }
            let mut cache = plugins_mod::FetchCache::new();
            let mut states = Vec::new();
            let pdir = plugins_mod::plugins_dir();
            if pdir.is_dir() {
                let mut dirs: Vec<std::path::PathBuf> = std::fs::read_dir(&pdir)
                    .map(|rd| {
                        // pathlib's glob("*") matches hidden entries too.
                        rd.filter_map(|e| e.ok()).map(|e| e.path()).collect()
                    })
                    .unwrap_or_default();
                dirs.sort();
                for d in dirs {
                    if d.join("manifest.json").is_file() {
                        states.push(plugins_mod::try_classify(&d, Some(&mut cache), true)?);
                    }
                }
            }
            Ok((states, trust))
        };
        self.say(&tr("Checking plugins…"), 5);
        let w = self.downgrade();
        spawn_bg(work, move |res| match res {
            Ok((states, trust)) => {
                if let Some(w) = w.upgrade() {
                    w.confirm_plugin_updates(states, trust);
                }
            }
            Err(e) => uncaught_err(&e),
        });
    }

    pub fn confirm_plugin_updates(
        &self,
        states: Vec<plugins_mod::State>,
        trust: BTreeMap<String, String>,
    ) {
        let scope = match plugins_mod::try_scope() {
            Ok(scope) => scope,
            Err(e) => return uncaught_err(&e),
        };
        let plan = plan_plugin_updates(&states, &trust, &scope);
        if plan.ready.is_empty() {
            let held = if plan.held.is_empty() {
                String::new()
            } else {
                format!(" — {} held back by the update scope", plan.held.len())
            };
            self.say(&(tr("No plugin updates to install") + &held), 8);
            return;
        }
        let Some(scope_label) = plan.scope_label.clone() else {
            // `dict(...)[scope]` raises in the idle callback: logged, nothing
            // asked.
            uncaught("KeyError", &py_repr(&scope));
            return;
        };
        if let Some(e) = &plan.raised {
            return uncaught_err(e);
        }
        let ready = plan.ready.clone();

        fn step(pid: String) -> tasks::StepFn {
            std::sync::Arc::new(move |log: tasks::LogFn<'_>| {
                // Checked again now: the dialog may have stayed open while the
                // plugin was edited, or swapped for a development link.
                // What classify raises fails the step with str(exc).
                let now = plugins_mod::try_classify(
                    &plugins_mod::plugins_dir().join(&pid),
                    Some(&mut plugins_mod::FetchCache::new()),
                    true,
                )
                .map_err(|e| crate::ui::shell::exc_str(&e))?;
                if !now.updatable() {
                    let why = now.try_describe().map_err(|e| crate::ui::shell::exc_str(&e))?;
                    log(&format!("{pid}: skipped — {why}"));
                    return Err(format!("{pid} changed since the check"));
                }
                let r = run_checked(&plugins_mod::update_argv(&pid), 300)?;
                let both = r.stdout.clone() + &r.stderr;
                let lines: Vec<&str> = py_splitlines(&both);
                for line in &lines[lines.len().saturating_sub(6)..] {
                    log(line);
                }
                if !r.ok() {
                    return Err(format!("{pid} was not updated"));
                }
                Ok(())
            })
        }

        let heads: Vec<(String, String)> = ready
            .iter()
            .map(|s| (s.id.clone(), plugins_mod::plugins_dir().join(&s.id).display().to_string()))
            .collect();

        let w = self.downgrade();
        tasks::confirm(
            &self.window,
            &format!("Update {} plugin(s)?", ready.len()),
            &format!(
                "Scope: {scope_label} (change it in Preferences). Each goes \
                 through `omarchy plugin update`, which validates it and rolls \
                 back one that fails."
            ),
            &plan.lines.join("\n"),
            &tr("Update"),
            false,
            move || {
                let Some(w) = w.upgrade() else { return };
                let probe_heads = heads.clone();
                let probe: TryProbe = std::sync::Arc::new(move || {
                    let mut out = BTreeMap::new();
                    for (pid, path) in &probe_heads {
                        let head = match verify::git_head_try(path)()? {
                            Some(ProbeValue::Str(s)) => Some(s),
                            _ => None,
                        };
                        out.insert(pid.clone(), head);
                    }
                    Ok(Some(ProbeValue::Map(out)))
                });
                let judge_heads = heads.clone();
                let judge: Judge = Box::new(move |before, after| {
                    let as_map = |v: Option<ProbeValue>| match v {
                        Some(ProbeValue::Map(m)) => m,
                        _ => BTreeMap::new(),
                    };
                    let (before, after) = (as_map(before), as_map(after));
                    let moved: Vec<&String> = judge_heads
                        .iter()
                        .map(|(p, _)| p)
                        .filter(|p| before.get(*p) != after.get(*p))
                        .collect();
                    let stuck: Vec<&str> = judge_heads
                        .iter()
                        .map(|(p, _)| p)
                        .filter(|p| !moved.contains(p))
                        .map(String::as_str)
                        .collect();
                    Outcome::new(
                        Some(!moved.is_empty() && stuck.is_empty()),
                        format!("{} plugin(s) updated", moved.len())
                            + &if stuck.is_empty() {
                                String::new()
                            } else {
                                format!("; not updated: {}", stuck.join(", "))
                            },
                    )
                });
                let wt = w.downgrade();
                let then: tasks::OnDone = Box::new(move |ok| {
                    if let Some(w) = wt.upgrade() {
                        w.after_pacman(ok);
                    }
                });
                let steps: Vec<tasks::Step> = ready
                    .iter()
                    .map(|s| tasks::Step::call(format!("Updating {}", s.id), step(s.id.clone())))
                    .collect();
                let on_done = match w.checked(Some(probe), judge, Some(then)) {
                    Ok(on_done) => on_done,
                    Err(e) => return uncaught_err(&e),
                };
                tasks::run(
                    &w.window,
                    &tr("Updating plugins"),
                    steps,
                    Some(on_done),
                );
            },
        );
    }

    pub fn on_restart_shell(&self) {
        let restart = || {
            let argv = plugins_mod::restart_argv(true);
            match popen_detached(&argv) {
                Ok(()) => {
                    if let Err(e) = history::record_checked(history::ALMANAC, "Shell restarted", "", "", &[]) {
                        uncaught_err(&e);
                    }
                }
                Err(e) => {
                    // Not caught in the Python: the OSError escapes the
                    // confirm callback, so its traceback goes to stderr and
                    // nothing is recorded.
                    let prog = argv.first().map(String::as_str).unwrap_or("");
                    uncaught(py_oserror_class(&e), &py_oserror(&e, prog));
                }
            }
        };

        let argv = plugins_mod::restart_argv(true);
        tasks::confirm(
            &self.window,
            &tr("Restart the shell?"),
            &tr("Restarts the Omarchy shell and clears its compiled QML cache. \
                 Use it when a plugin behaves as if it had not been updated. \
                 Your bar and settings are not touched."),
            &format!("$ {}", argv.get(3..).unwrap_or(&[]).join(" ")),
            &tr("Restart"),
            false,
            restart,
        );
    }

    pub fn row_aur(&self, name: &str) {
        self.open_url(&format!("https://aur.archlinux.org/packages/{name}"));
    }

    pub fn row_remove(&self, name: &str) {
        let app = self.shared.borrow().row_actions.get(name).cloned();
        if let Some(app) = app {
            self.on_remove(&app);
        }
    }

    /// Hand the URL to the desktop's default browser.
    pub fn open_url(&self, url: &str) {
        if let Err(e) = gio::AppInfo::launch_default_for_uri(url, None::<&gio::AppLaunchContext>) {
            self.say(&format!("Could not open {url}: {}", e.message()), 5);
        }
    }

    pub fn on_launch(&self, app: &inventory::App) {
        if let Err(e) = popen_detached(std::slice::from_ref(&app.exec_path)) {
            self.say(&format!("Could not launch {}: {}", app.name, py_oserror(&e, &app.exec_path)), 5);
        }
    }

    /// The one remaining escape to a terminal: see on_update_all.
    pub fn in_terminal(&self, script: &str, note: &str) -> bool {
        let Some(argv) = xdg::terminal_argv(&[
            "bash".to_string(),
            "-lc".to_string(),
            script.to_string() + "; echo; read -n1 -r -p 'Done — press any key to close'",
        ])
        .filter(|a| !a.is_empty()) else {
            self.say(&tr("No terminal found to run that in"), 5);
            return false;
        };
        match popen_detached(&argv) {
            Ok(()) => {
                self.say(note, 5);
                true
            }
            Err(e) => {
                let prog = argv.first().cloned().unwrap_or_default();
                self.say(&format!("Could not start a terminal: {}", py_oserror(&e, &prog)), 5);
                false
            }
        }
    }

    /// Remove only the orphans judged safe, with pacman asking first.
    ///
    /// Build tooling and debug packages are excluded: the next AUR build pulls
    /// rust straight back, so removing it is churn rather than cleanup.
    pub fn on_clean_orphans(&self) {
        let store = self.shared.borrow().store.clone();
        let mut rows: Vec<(String, String, String)> = Vec::new();
        for i in 0..store.n_items() {
            let Some(obj) = store.item(i) else { continue };
            let row = AppItem::of(&obj).map(|it| (it.source.clone(), it.advice.clone(), it.name.clone()));
            rows.extend(row);
        }
        clean_orphans(
            rows.iter().map(|(s, a, n)| (s.as_str(), a.as_str(), n.as_str())),
            &tasks::preview_removal,
            &|m| self.say(m, 5),
            &|plan, yes| self.confirm_removal(plan, yes),
            self.removal_runner(),
        );
    }

    /// `tasks.confirm` for a removal plan.
    fn confirm_removal(&self, plan: &RemovalPlan, yes: Box<dyn Fn()>) {
        tasks::confirm(
            &self.window,
            &plan.title,
            &plan.body,
            &plan.detail,
            &tr("Remove"),
            plan.destructive,
            move || yes(),
        );
    }

    /// `tasks.run` for a removal, then `_after_pacman`.
    fn removal_runner(&self) -> RunSteps {
        let w = self.downgrade();
        std::rc::Rc::new(move |title: &str, steps: Vec<tasks::Step>| {
            let Some(w) = w.upgrade() else { return };
            let wt = w.downgrade();
            tasks::run(
                &w.window,
                title,
                steps,
                Some(Box::new(move |ok| {
                    if let Some(w) = wt.upgrade() {
                        w.after_pacman(ok);
                    }
                })),
            );
        })
    }

    /// Install one result, through the tool that owns that kind of thing.
    pub fn on_install_item(&self, app: &inventory::App) {
        if app.source == inventory::PLUGIN {
            let url = extra_str(app, "repo");
            if url.is_empty() {
                return;
            }
            let w = self.downgrade();
            let nm = app.name.clone();
            let u = url.clone();
            tasks::confirm(
                &self.window,
                &format!("Install the {} plugin?", app.name),
                &tr("Omarchy shell plugins run inside the shell process with your \
                     permissions — there is no sandbox."),
                &format!("Source: {url}\n\n$ omarchy plugin add {url} --enable --yes"),
                &tr("Install"),
                false,
                move || {
                    let Some(w) = w.upgrade() else { return };
                    let wt = w.downgrade();
                    tasks::run(
                        &w.window,
                        &format!("Installing {nm}"),
                        vec![tasks::Step::argv(
                            format!("Installing {nm}"),
                            ["omarchy", "plugin", "add", &u, "--enable", "--yes"]
                                .iter()
                                .map(|s| s.to_string())
                                .collect(),
                        )],
                        Some(Box::new(move |ok| {
                            if let Some(w) = wt.upgrade() {
                                w.after_pacman(ok);
                            }
                        })),
                    );
                },
            );
            return;
        }

        // yay builds as this user and only needs root for the final pacman
        // call; --sudo pkexec is what lets that happen without a terminal.
        let argv: Vec<String> = ["yay", "-S", "--needed", "--noconfirm", "--sudo", "pkexec", &app.name]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let w = self.downgrade();
        let nm = app.name.clone();
        let detail = format!("{} {}\n{}\n\n$ {}", app.name, app.version, app.description, argv.join(" "));
        tasks::confirm(
            &self.window,
            &format!("Install {}?", app.name),
            &format!(
                "From {}. AUR packages are built from source on this machine.",
                source_label(&app.source)
            ),
            &detail,
            &tr("Install"),
            false,
            move || {
                if let Some(w) = w.upgrade() {
                    w.install_package(&nm, argv.clone());
                }
            },
        );
    }

    /// Search and install, handed to yay in a terminal.
    ///
    /// yay's own interactive picker is better than anything reimplemented
    /// here, and it is the tool that owns the transaction.
    pub fn on_install(&self) {
        let text = py_strip(&self.search.text()).to_string();
        if text.is_empty() {
            self.search.grab_focus();
            self.say(&tr("Type what to search for, then press Install"), 5);
            return;
        }
        if sources::parse(&text).is_some() {
            self.on_install_link(&text);
            return;
        }
        let argv: Vec<String> = ["yay", "-S", "--needed", "--noconfirm", "--sudo", "pkexec", &text]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let w = self.downgrade();
        let detail = format!("$ {}", argv.join(" "));
        let t = text.clone();
        tasks::confirm(
            &self.window,
            &format!("Install “{text}”?"),
            &tr("This installs a package by exact name. To browse what matches \
                 first, use the App Store and search there."),
            &detail,
            &tr("Install"),
            false,
            move || {
                if let Some(w) = w.upgrade() {
                    w.install_package(&t, argv.clone());
                }
            },
        );
    }

    /// Install through yay: snapshot first, then check what changed.
    pub fn install_package(&self, name: &str, argv: Vec<String>) {
        let holder = match self.try_watch_defaults() {
            Ok(holder) => holder,
            Err(e) => return uncaught_err(&e),
        };

        let w = self.downgrade();
        let after: tasks::OnDone = Box::new(move |ok| {
            let Some(w) = w.upgrade() else { return };
            if ok {
                if let Err(e) = w.try_check_defaults(holder) {
                    return uncaught_err(&e);
                }
            }
            w.after_pacman(ok);
        });

        let nm = name.to_string();
        let judge: Judge = Box::new(move |b, a| {
            let a_s = match &a {
                Some(ProbeValue::Str(s)) => s.clone(),
                _ => String::new(),
            };
            let changed = truthy(&a) && a != b;
            Outcome::new(
                Some(changed),
                if changed { format!("{nm} {a_s} installed") } else { format!("{nm} was not installed") },
            )
        });
        // What the snapshot or the first probe raises ends this before the
        // task starts.
        let steps = match self.snapshot_first(
            vec![tasks::Step::argv(format!("Installing {name}"), argv)],
            &format!("installing {name}"),
        ) {
            Ok(steps) => steps,
            Err(e) => return uncaught_err(&e),
        };
        let on_done = match self.checked(Some(verify::installed_package_try(name)), judge, Some(after)) {
            Ok(on_done) => on_done,
            Err(e) => return uncaught_err(&e),
        };
        tasks::run(&self.window, &format!("Installing {name}"), steps, Some(on_done));
    }

    /// Remove a package, with its orphaned dependencies.
    ///
    /// -Rns is what a user means by "uninstall", but it can take more than
    /// expected, so the terminal shows the full list before confirming --
    /// no --noconfirm here, deliberately.
    pub fn on_remove(&self, app: &inventory::App) {
        if app.source == inventory::APPIMAGE {
            self.remove_appimage(app);
            return;
        }
        if app.source == inventory::FLATPAK {
            // `app.extra.get("appid", app.name)`: whatever is stored, as
            // str() prints it; the name only when the key is missing.
            let appid = app.extra.get("appid").map(py_str_value).unwrap_or_else(|| app.name.clone());
            let w = self.downgrade();
            let nm = app.name.clone();
            let id = appid.clone();
            tasks::confirm(
                &self.window,
                &format!("Remove {}?", app.name),
                &tr("The Flatpak and its data are removed."),
                &format!("$ flatpak uninstall --assumeyes {appid}"),
                &tr("Remove"),
                true,
                move || {
                    let Some(w) = w.upgrade() else { return };
                    let wt = w.downgrade();
                    tasks::run(
                        &w.window,
                        &format!("Removing {nm}"),
                        vec![tasks::Step::argv(
                            format!("Removing {nm}"),
                            vec!["flatpak".into(), "uninstall".into(), "--assumeyes".into(), id.clone()],
                        )],
                        Some(Box::new(move |ok| {
                            if let Some(w) = wt.upgrade() {
                                w.after_pacman(ok);
                            }
                        })),
                    );
                },
            );
            return;
        }

        remove_package(
            &app.name,
            &tasks::preview_removal,
            &|m| self.say(m, 5),
            &|plan, yes| self.confirm_removal(plan, yes),
            self.removal_runner(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::Mutex;

    #[test]
    fn plugin_logo_choice_vendor_art_avatar_symbol() {
        let state = |url: &str| {
            Some(plugins_mod::State::new("p", plugins_mod::CURRENT, "", 0, url))
        };
        let mut i = plugins_mod::Installed { id: "tim.tailscale".into(), name: "My Tailscale".into(), ..Default::default() };
        assert_eq!(
            Win::plugin_logo_choice(&i, None),
            LogoChoice::Vendor { key: "tailscale.com".into(), domain: "tailscale.com".into() }
        );
        i = plugins_mod::Installed { id: "a.thing".into(), name: "Thing".into(), state: state("https://github.com/someone/thing"), ..Default::default() };
        assert_eq!(
            Win::plugin_logo_choice(&i, None),
            LogoChoice::Avatar { key: "github-someone".into(), url: "https://github.com/someone.png?size=128".into() }
        );
        let listing = store_mod::Plugin { id: "a.thing".into(), thumb: "img/t.webp".into(), ..Default::default() };
        assert!(matches!(Win::plugin_logo_choice(&i, Some(&listing)), LogoChoice::Art { .. }));
        // A system icon that says what it does wins over the author's face.
        i = plugins_mod::Installed { id: "lxp.vitals".into(), name: "Vitals".into(), state: state("https://github.com/lxp/vitals"), ..Default::default() };
        assert_eq!(Win::plugin_logo_choice(&i, None), LogoChoice::Symbol { name: "emblem-system-symbolic".into() });
    }

    fn src() -> &'static str {
        include_str!("plugins.rs")
    }

    fn css() -> &'static str {
        include_str!("../../data/style.css")
    }

    fn log_into(lines: &Mutex<Vec<String>>) -> impl Fn(&str) + Send + Sync + '_ {
        move |l: &str| lines.lock().unwrap().push(l.to_string())
    }

    // PluginRemovalAndRecheck.test_removing_a_plugin_removes_its_shortcut
    #[test]
    fn removing_a_plugin_removes_its_shortcut() {
        let lines = Mutex::new(Vec::new());
        let calls = Mutex::new(Vec::new());
        let log = log_into(&lines);
        forget_shortcut_with(
            "lxp.vitals",
            &log,
            &|| true,
            &|_p| Ok("SUPER + ALT + V".to_string()),
            &|p, c| {
                calls.lock().unwrap().push((p.to_string(), c.map(str::to_string)));
                Ok("removed".to_string())
            },
        );
        assert_eq!(*calls.lock().unwrap(), vec![("lxp.vitals".to_string(), None)]);
    }

    // PluginRemovalAndRecheck.test_a_shortcut_that_cannot_be_removed_does_not_fail_the_removal
    #[test]
    fn a_shortcut_that_cannot_be_removed_does_not_fail_the_removal() {
        let lines = Mutex::new(Vec::new());
        let log = log_into(&lines);
        forget_shortcut_with(
            "x",
            &log,
            &|| true,
            &|_p| Err("edited by hand".to_string()),
            &|_p, _c| panic!("must not be called"),
        );
        let lines = lines.lock().unwrap();
        assert!(lines.last().unwrap().contains("left in bindings.lua"));
    }

    #[test]
    fn forget_shortcut_step_label() {
        assert_eq!(Win::forget_shortcut("x").label, "Removing its keyboard shortcut");
    }

    // RemovalCommands.test_package_removal_shows_the_list_before_removing_anything
    #[test]
    fn package_removal_shows_the_list_before_removing_anything() {
        let asked = Mutex::new(Vec::new());
        let preview = |names: &[String]| {
            asked.lock().unwrap().push(names.to_vec());
            (true, "somepkg 1\nleftover-dep 2".to_string())
        };
        let plan = plan_package_removal("somepkg", &preview).expect("removal must ask first");
        // The preview is the whole point: -Rns takes dependencies too, and
        // the user has to be able to see which ones.
        assert!(plan.detail.contains("leftover-dep"));
        assert!(plan.destructive);
        assert!(plan.body.contains("2 package(s) would be removed"));
        assert_eq!(plan.argv.join(" "), "pacman -Rns --noconfirm somepkg");
        assert_eq!(*asked.lock().unwrap(), vec![vec!["somepkg".to_string()]]);
    }

    /// `_Confirmed`: stands in for tasks.confirm/tasks.run.
    #[derive(Default)]
    struct Confirmed {
        confirm_args: std::cell::RefCell<Option<RemovalPlan>>,
        on_yes: std::cell::RefCell<Option<Box<dyn Fn()>>>,
        ran: std::rc::Rc<std::cell::RefCell<Vec<tasks::Step>>>,
    }

    impl Confirmed {
        fn confirm(&self, plan: &RemovalPlan, yes: Box<dyn Fn()>) {
            *self.confirm_args.borrow_mut() = Some(plan.clone());
            *self.on_yes.borrow_mut() = Some(yes);
        }
        fn run(&self) -> RunSteps {
            let ran = self.ran.clone();
            std::rc::Rc::new(move |_title: &str, steps: Vec<tasks::Step>| ran.borrow_mut().extend(steps))
        }
        fn say_yes(&self) {
            let yes = self.on_yes.borrow_mut().take().expect("nothing asked for confirmation");
            yes();
        }
        fn commands(&self) -> Vec<String> {
            self.ran
                .borrow()
                .iter()
                .map(|s| match &s.action {
                    tasks::StepAction::Argv(argv) => argv.join(" "),
                    _ => "<callable>".to_string(),
                })
                .collect()
        }
    }

    // The same test, through `remove_package`, the body of `on_remove`.
    #[test]
    fn on_remove_runs_nothing_before_the_user_confirms() {
        let fake = Confirmed::default();
        let said = std::cell::RefCell::new(Vec::new());
        let preview = |_n: &[String]| (true, "somepkg 1\nleftover-dep 2".to_string());
        remove_package(
            "somepkg",
            &preview,
            &|m| said.borrow_mut().push(m.to_string()),
            &|p, y| fake.confirm(p, y),
            fake.run(),
        );
        let args = fake.confirm_args.borrow().clone().expect("removal must ask first");
        assert!(fake.ran.borrow().is_empty(), "nothing may run before the user confirms");
        assert!(args.detail.contains("leftover-dep"));
        assert!(args.destructive);
        fake.say_yes();
        assert!(fake.commands().contains(&"pacman -Rns --noconfirm somepkg".to_string()));
        assert!(said.borrow().is_empty());
    }

    #[test]
    fn on_remove_asks_nothing_when_the_preview_fails() {
        let fake = Confirmed::default();
        let said = std::cell::RefCell::new(Vec::new());
        let preview = |_n: &[String]| (false, "target not found".to_string());
        remove_package(
            "somepkg",
            &preview,
            &|m| said.borrow_mut().push(m.to_string()),
            &|p, y| fake.confirm(p, y),
            fake.run(),
        );
        assert!(fake.confirm_args.borrow().is_none(), "must not offer to remove what it cannot describe");
        assert!(fake.ran.borrow().is_empty());
        assert_eq!(*said.borrow(), vec!["Cannot remove somepkg: target not found".to_string()]);
    }

    #[test]
    fn on_clean_orphans_runs_only_the_safe_ones_after_yes() {
        let fake = Confirmed::default();
        let asked = Mutex::new(Vec::new());
        let preview = |names: &[String]| {
            asked.lock().unwrap().push(names.to_vec());
            (true, "libfoo 1".to_string())
        };
        clean_orphans(
            [(inventory::ORPHAN, "keep", "rust"), (inventory::ORPHAN, "remove", "libfoo")],
            &preview,
            &|_m| panic!("must ask"),
            &|p, y| fake.confirm(p, y),
            fake.run(),
        );
        assert_eq!(*asked.lock().unwrap(), vec![vec!["libfoo".to_string()]]);
        assert!(fake.ran.borrow().is_empty());
        fake.say_yes();
        let joined = fake.commands().join(" ");
        assert!(joined.contains("libfoo"));
        assert!(!joined.contains("rust"), "rust is build tooling and must be kept");
        assert_eq!(fake.ran.borrow()[0].label, "Removing orphaned packages");
    }

    // RemovalCommands.test_removal_is_abandoned_when_the_preview_fails
    #[test]
    fn removal_is_abandoned_when_the_preview_fails() {
        let preview = |_n: &[String]| (false, "target not found".to_string());
        // Err: nothing to confirm, so nothing can run.
        assert_eq!(
            plan_package_removal("somepkg", &preview),
            Err("Cannot remove somepkg: target not found".to_string()),
            "must not offer to remove what it cannot describe"
        );
    }

    // RemovalCommands.test_orphan_cleanup_excludes_the_keep_list
    #[test]
    fn orphan_cleanup_excludes_the_keep_list() {
        let asked = Mutex::new(Vec::new());
        let preview = |names: &[String]| {
            asked.lock().unwrap().push(names.to_vec());
            (true, "libfoo 1".to_string())
        };
        let plan = plan_orphan_cleanup(
            [(inventory::ORPHAN, "keep", "rust"), (inventory::ORPHAN, "remove", "libfoo")],
            &preview,
        )
        .expect("asks first");
        // rust must not even reach the preview, let alone the removal.
        assert_eq!(*asked.lock().unwrap(), vec![vec!["libfoo".to_string()]]);
        let joined = plan.argv.join(" ");
        assert!(joined.contains("libfoo"));
        assert!(!joined.contains("rust"));
    }

    #[test]
    fn split_orphans_ignores_other_sources() {
        let (safe, kept) = split_orphans([
            (inventory::ORPHAN, "keep", "rust"),
            (inventory::ORPHAN, "remove", "libfoo"),
            (inventory::REPO, "", "firefox"),
        ]);
        assert_eq!(safe, vec!["libfoo".to_string()]);
        assert_eq!(kept, vec!["rust".to_string()]);
    }

    #[test]
    fn python_string_helpers() {
        assert_eq!(py_splitlines("a\r\nb\rc\n\nd\x0ce\u{2028}f\n"), vec!["a", "b", "c", "", "d", "e", "f"]);
        assert_eq!(py_splitlines(""), Vec::<&str>::new());
        assert_eq!(py_repr("omarchy"), "'omarchy'");
        assert_eq!(py_repr("it's"), "\"it's\"");
        assert_eq!(py_strip(" \x1cx\n"), "x");
        assert_eq!(py_strip("\u{85}\u{a0}x\u{3000}"), "x");
        assert_eq!(py_strip("\u{200b}x"), "\u{200b}x");
        // repr escapes what str.isprintable() rejects, in the width Python uses.
        assert_eq!(py_repr("a\u{85}b\u{a0}c"), "'a\\x85b\\xa0c'");
        assert_eq!(py_repr("\u{2028}\u{feff}é"), "'\\u2028\\ufeffé'");
        assert_eq!(py_repr("\u{e0001}"), "'\\U000e0001'");
        assert_eq!(py_str_value(&serde_json::Value::Null), "None");
        assert_eq!(py_str_value(&serde_json::json!("org.x")), "org.x");
        assert_eq!(py_str_value(&serde_json::json!([1, "a", true])), "[1, 'a', True]");
    }

    #[test]
    fn run_checked_reports_the_real_errno() {
        let dir = std::env::temp_dir().join(format!("almanac-noexec-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let f = dir.join("omarchy");
        std::fs::write(&f, "#!/bin/sh\n").unwrap();
        let prog = f.display().to_string();
        let err = run_checked(&[prog.clone()], 5).unwrap_err();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(err, format!("[Errno 13] Permission denied: '{prog}'"));
        let err = run_checked(&["/nonexistent/almanac-test".to_string()], 5).unwrap_err();
        assert_eq!(err, "[Errno 2] No such file or directory: '/nonexistent/almanac-test'");
        let r = run_checked(&["sh".into(), "-c".into(), "echo out; echo err >&2; exit 3".into()], 5).unwrap();
        assert_eq!((r.stdout.as_str(), r.stderr.as_str(), r.code), ("out\n", "err\n", Some(3)));
        let err = run_checked(&["sleep".into(), "5".into()], 0).unwrap_err();
        assert_eq!(err, "Command '['sleep', '5']' timed out after 0 seconds");
    }

    #[test]
    fn a_plugins_cache_that_is_not_a_dict_fails_like_python() {
        let reached = plugins_mod::Installed {
            state: Some(plugins_mod::State::new("a", "update", "behind", 1, "u")),
            ..Default::default()
        };
        let local = plugins_mod::Installed {
            state: Some(plugins_mod::State::new("a", "local", "symlink", 0, "")),
            ..Default::default()
        };
        let empty = plugins_mod::FetchCache::new();
        let mut wrote = plugins_mod::FetchCache::new();
        wrote.insert("/p".into(), serde_json::json!({}));
        assert_eq!(
            foreign_cache_error(&serde_json::json!([1]), std::slice::from_ref(&reached), &empty),
            Some(("AttributeError", "'list' object has no attribute 'get'".to_string()))
        );
        assert_eq!(foreign_cache_error(&serde_json::json!("x"), std::slice::from_ref(&local), &empty), None);
        assert_eq!(foreign_cache_error(&serde_json::json!([]), std::slice::from_ref(&reached), &empty), None);
        assert_eq!(
            foreign_cache_error(&serde_json::json!(0), &[], &wrote),
            Some(("TypeError", "'int' object does not support item assignment".to_string()))
        );
        assert_eq!(
            foreign_cache_error(&serde_json::json!([]), &[], &wrote),
            Some(("TypeError", "list indices must be integers or slices, not str".to_string()))
        );
    }

    #[test]
    fn popen_starts_a_new_session() {
        let dir = std::env::temp_dir().join(format!("almanac-popen-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let out = dir.join("sid");
        let script = format!("ps -o sid= -p $$ > {}.tmp && mv {0}.tmp {0}", out.display());
        popen_detached(&["sh".into(), "-c".into(), script]).unwrap();
        let mut text = String::new();
        for _ in 0..200 {
            if let Ok(t) = std::fs::read_to_string(&out) {
                text = t;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = std::fs::remove_dir_all(&dir);
        let mine = rustix::process::getsid(None).unwrap().as_raw_nonzero().get();
        let theirs: i32 = text.trim().parse().unwrap();
        assert_ne!(theirs, mine);
    }

    #[test]
    fn popen_reports_the_os_error_like_python() {
        let e = popen_detached(&["/nonexistent/almanac-test".to_string()]).unwrap_err();
        assert_eq!(py_oserror(&e, "/nonexistent/almanac-test"), "[Errno 2] No such file or directory: '/nonexistent/almanac-test'");
    }

    #[test]
    fn state_pills() {
        assert_eq!(plugin_state_pill("update"), Some(("Update waiting", "pill-accent")));
        assert_eq!(plugin_state_pill("symlink"), Some(("Development link", "pill-quiet")));
        assert_eq!(plugin_state_pill("current"), None);
        let st = plugins_mod::State::new("a", "update", "3 commits", 3, "");
        assert_eq!(plugin_state_key(Some(&st)), "update");
        let st = plugins_mod::State::new("a", "local-changes", "modified", 0, "");
        assert_eq!(plugin_state_key(Some(&st)), "modified");
        assert_eq!(plugin_state_key(None), "");
    }

    #[test]
    fn filter_matches_id_name_description_and_kinds() {
        let i = plugins_mod::Installed {
            id: "lxp.vitals".into(),
            name: "Vitals".into(),
            description: "CPU and memory".into(),
            kinds: vec!["bar-widget".into()],
            ..Default::default()
        };
        assert!(plugin_matches(&i, ""));
        assert!(plugin_matches(&i, "  VITALS "));
        assert!(plugin_matches(&i, "memory"));
        assert!(plugin_matches(&i, "bar-widget"));
        assert!(!plugin_matches(&i, "weather"));
    }

    #[test]
    fn row_menu_matches_the_python() {
        let mut i = plugins_mod::Installed {
            id: "x".into(),
            kinds: vec!["panel".into()],
            folder: Some("/tmp/x".into()),
            state: Some(plugins_mod::State::new("x", "update", "", 2, "https://github.com/a/x")),
            ..Default::default()
        };
        let acts: Vec<&str> = plugin_menu(&i, true).expect("menu").iter().map(|(_l, a)| *a).collect();
        assert_eq!(acts, ["update", "shortcut", "folder", "page", "remove"]);
        i.built_in = true;
        i.folder = None;
        i.state = None;
        assert!(plugin_menu(&i, true).expect("menu").is_empty());
        // Local edits with upstream commits waiting can still be updated.
        i.state = Some(plugins_mod::State::new("x", "local-changes", "modified", 1, ""));
        let acts: Vec<&str> = plugin_menu(&i, false).expect("menu").iter().map(|(_l, a)| *a).collect();
        assert_eq!(acts, ["update"]);
    }

    #[test]
    fn update_plan_holds_back_out_of_scope_and_lists_blocked() {
        let states = vec![
            plugins_mod::State::new("a", "update", "", 2, ""),
            plugins_mod::State::new("b", "update", "", 1, ""),
            plugins_mod::State::new("c", "local-changes", "modified", 3, ""),
            plugins_mod::State::new("d", "current", "", 0, ""),
        ];
        let trust: BTreeMap<String, String> = [("a".to_string(), "official".to_string())].into();
        let plan = plan_plugin_updates(&states, &trust, "verified");
        assert_eq!(plan.ready.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["a"]);
        assert_eq!(plan.held.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["b"]);
        assert_eq!(plan.blocked.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["c"]);
        assert_eq!(plan.scope_label.as_deref(), Some("Verified only"));
        assert_eq!(plan.lines[0], format!("{:<40} 2 commit(s)", "a"));
        assert_eq!(plan.lines[0].len(), 40 + " 2 commit(s)".len());
        assert!(plan.lines.contains(&"Held back (outside “Verified only”):".to_string()));
        assert!(plan.lines.contains(&"  b — not listed".to_string()));
        assert!(plan.lines.contains(&"Not updated (needs you):".to_string()));
        assert!(plan.lines.contains(&"  c — Has local edits".to_string()));
        let all = plan_plugin_updates(&states, &trust, "all");
        assert_eq!(all.ready.len(), 2);
        assert!(all.held.is_empty());
        // `dict(...)[scope]` raises for a scope SCOPES does not list.
        assert_eq!(plan_plugin_updates(&states, &trust, "nonsense").scope_label, None);
    }

    #[test]
    fn switch_names() {
        assert_eq!(switch_name("Vitals", true), "Vitals on");
        assert_eq!(switch_name("Vitals", false), "Vitals off");
    }

    #[test]
    fn oserror_reads_like_python() {
        let e = std::io::Error::from_raw_os_error(2);
        assert_eq!(py_oserror(&e, "x"), "[Errno 2] No such file or directory: 'x'");
    }

    // Accessibility.test_every_icon_only_button_is_named, for this file.
    #[test]
    fn every_icon_only_button_is_named() {
        let lines: Vec<&str> = src().lines().collect();
        let made = regex::Regex::new(r"let (\w+) = gtk::(?:Menu|Toggle)?Button::builder\(\)").unwrap();
        let mut unnamed = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            let Some(m) = made.captures(line) else { continue };
            let var = &m[1];
            let window = lines[i..(i + 12).min(lines.len())].join("\n");
            if !window.contains(".icon_name(") {
                continue;
            }
            if !window.contains(&format!("name(&{var}")) && !window.contains("Property::Label") {
                unnamed.push(format!("plugins.rs:{} {var}", i + 1));
            }
        }
        assert_eq!(unnamed, Vec::<String>::new());
    }

    // VisualSystem.test_spacing_comes_from_the_scale, for this file.
    #[test]
    fn spacing_comes_from_the_scale() {
        const SCALE: [i64; 8] = [2, 4, 6, 8, 12, 16, 20, 24];
        let re = regex::Regex::new(
            r"\.(margin_(?:start|end|top|bottom)|spacing|child_spacing|line_spacing)\((\d+)\)",
        )
        .unwrap();
        let boxed = regex::Regex::new(r"gtk::Box::new\(gtk::Orientation::\w+, (\d+)\)").unwrap();
        let mut offenders = Vec::new();
        for c in re.captures_iter(src()) {
            let v: i64 = c[2].parse().unwrap();
            if !SCALE.contains(&v) {
                offenders.push(format!("{}={v}", &c[1]));
            }
        }
        for c in boxed.captures_iter(src()) {
            let v: i64 = c[1].parse().unwrap();
            if v != 0 && !SCALE.contains(&v) {
                offenders.push(format!("spacing={v}"));
            }
        }
        assert_eq!(offenders, Vec::<String>::new());
    }

    // VisualSystem.test_every_pill_class_is_defined_in_the_stylesheet
    #[test]
    fn every_pill_class_is_defined_in_the_stylesheet() {
        const PILLS: [&str; 6] = ["pill", "pill-crit", "pill-warn", "pill-ok", "pill-accent", "pill-quiet"];
        let used: std::collections::BTreeSet<String> = regex::Regex::new(r#""(pill(?:-[a-z]+)?)""#)
            .unwrap()
            .captures_iter(src())
            .map(|c| c[1].to_string())
            .collect();
        assert!(!used.is_empty(), "no pill classes found at all");
        for name in &used {
            assert!(css().contains(&format!(".{name}")), "{name} is applied but never styled");
            assert!(PILLS.contains(&name.as_str()), "unknown pill variant: {name}");
        }
    }

    #[test]
    fn plugin_logo_classes_are_styled() {
        assert!(css().contains(".plugin-logo"));
        assert!(css().contains(".plugin-logo.has-art"));
    }

    // IconsExist.test_every_icon_name_is_in_adwaita_or_shipped_here
    #[test]
    fn every_icon_name_is_in_adwaita_or_shipped_here() {
        let adwaita = Path::new("/usr/share/icons/Adwaita");
        if !adwaita.is_dir() {
            return; // Adwaita icon theme not installed
        }
        fn walk(d: &Path, out: &mut std::collections::BTreeSet<String>) {
            let Ok(rd) = std::fs::read_dir(d) else { return };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if matches!(p.extension().and_then(|x| x.to_str()), Some("svg" | "png")) {
                    if let Some(s) = p.file_stem().and_then(|s| s.to_str()) {
                        out.insert(s.to_string());
                    }
                }
            }
        }
        let mut have = std::collections::BTreeSet::new();
        walk(adwaita, &mut have);
        walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("icons"), &mut have);
        let names: std::collections::BTreeSet<String> = regex::Regex::new(r#""([a-z0-9-]+-symbolic)""#)
            .unwrap()
            .captures_iter(src())
            .map(|c| c[1].to_string())
            .collect();
        let missing: Vec<&String> =
            names.iter().filter(|n| !have.contains(*n) && !n.starts_with("adw-")).collect();
        assert_eq!(missing, Vec::<&String>::new());
    }
}
