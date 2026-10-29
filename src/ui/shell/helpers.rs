//! Module-level helpers: icons, labels, formatting, the stylesheet, reading
//! the mixed-type `extra` dict, and background work.

use std::cmp::Ordering;

use adw::prelude::*;
use gtk::{gio, glib};
use serde_json::{Map, Value};

use crate::inventory;

use super::items::AppItem;

/// Install is an arrow into a tray, shipped in icons/: the theme's own
/// download icon is a folder, which reads as "the Downloads folder".
pub const INSTALL_ICON: &str = "almanac-install-symbolic";

/// 15419 -> '15k', 2151 -> '2.2k', 198 -> '198': a count that fits a chip.
pub fn compact(n: i64) -> String {
    if n >= 10_000 {
        return format!("{}k", n.div_euclid(1000));
    }
    if n >= 1_000 {
        return format!("{:.1}k", n as f64 / 1000.0).replace(".0k", "k");
    }
    n.to_string()
}

/// Tooltip and accessible name at once, for a control that is only an icon.
///
/// A tooltip helps a pointer; a screen reader needs the accessible label, and
/// an icon-only button without one is announced as just "button". Setting
/// both from one place keeps them from drifting apart.
pub fn name(widget: &impl IsA<gtk::Widget>, text: &str) {
    widget.set_tooltip_text(Some(text));
    let widget: &gtk::Widget = widget.upcast_ref();
    widget.update_property(&[gtk::accessible::Property::Label(text)]);
}

/// A switch whose `state-set` handler runs for the user's flips only.
///
/// In GTK 4 `set_active` emits `state-set` whenever the value changes, so a
/// handler that snaps the switch back to what really holds (or a dialog's
/// "Keep it on") would run itself again: apply the change a second time, or
/// ask again. Every switch that moves only when a change holds connects
/// through [`SwitchGuard::connect`] and moves through [`SwitchGuard::set`],
/// one rule for all of them.
#[derive(Clone, Default)]
pub struct SwitchGuard(std::rc::Rc<std::cell::Cell<bool>>);

impl SwitchGuard {
    /// `switch.connect_state_set(f)`, skipped while [`set`](Self::set) moves it.
    pub fn connect(
        &self,
        switch: &gtk::Switch,
        f: impl Fn(&gtk::Switch, bool) -> glib::Propagation + 'static,
    ) -> glib::SignalHandlerId {
        let quiet = self.0.clone();
        switch.connect_state_set(move |sw, on| {
            if quiet.get() {
                // Our own move: let the default handler settle the state.
                return glib::Propagation::Proceed;
            }
            f(sw, on)
        })
    }

    /// Put the switch at `on` (state and position) without running the handler.
    pub fn set(&self, switch: &gtk::Switch, on: bool) {
        // Restored even if it was already set: a nested `set` from a signal
        // handler leaves it as the outer one found it.
        let was = self.0.replace(true);
        switch.set_state(on);
        switch.set_active(on);
        self.0.set(was);
    }
}

/// `_CSS`: the app's stylesheet, verbatim, in `data/style.css`.
pub const CSS: &str = include_str!("../../../data/style.css");

/// `SOURCE_LABEL`: how each source is named in the table.
pub const SOURCE_LABEL: &[(&str, &str)] = &[
    (inventory::REPO, "Repo"),
    (inventory::AUR, "AUR"),
    (inventory::APPIMAGE, "AppImage"),
    (inventory::LOCAL, "Local"),
    (inventory::FLATPAK, "Flatpak"),
    (inventory::ORPHAN, "Orphan"),
    (inventory::SYSTEM, "System"),
    (inventory::AVAILABLE, "Install"),
    (inventory::PLUGIN, "Plugin"),
    (inventory::AGENT, "AI agent"),
    (inventory::TOOL, "Dev tool"),
];

/// `SOURCE_LABEL.get(source, source)`.
pub fn source_label(source: &str) -> String {
    SOURCE_LABEL
        .iter()
        .find(|(k, _)| *k == source)
        .map(|(_, v)| (*v).to_string())
        .unwrap_or_else(|| source.to_string())
}

/// `_FALLBACK_ICON`: the icon for a row whose app has none of its own.
pub const FALLBACK_ICON: &[(&str, &str)] = &[
    (inventory::REPO, "package-x-generic"),
    (inventory::AUR, "package-x-generic"),
    (inventory::APPIMAGE, "application-x-executable"),
    (inventory::LOCAL, "applications-engineering"),
    (inventory::FLATPAK, "application-x-executable"),
    (inventory::ORPHAN, "user-trash-symbolic"),
    (inventory::SYSTEM, "preferences-system-symbolic"),
    (inventory::AVAILABLE, "system-software-install-symbolic"),
    (inventory::PLUGIN, "application-x-addon-symbolic"),
    (inventory::AGENT, "system-run-symbolic"),
    (inventory::TOOL, "applications-engineering-symbolic"),
];

/// `_FALLBACK_ICON.get(source)`.
pub fn fallback_icon(source: &str) -> Option<&'static str> {
    FALLBACK_ICON.iter().find(|(k, _)| *k == source).map(|(_, v)| *v)
}

/// Epoch -> "3d ago". An age answers "is this stale?"; a timestamp doesn't.
pub fn age(ts: f64) -> String {
    use chrono::TimeZone;
    if ts == 0.0 || !ts.is_finite() {
        return String::new();
    }
    let secs = ts.floor() as i64;
    let nanos = ((ts - ts.floor()) * 1e9) as u32;
    let then = match chrono::Local.timestamp_opt(secs, nanos).earliest() {
        Some(t) => t.naive_local(),
        None => return String::new(),
    };
    let now = chrono::Local::now().naive_local();
    // timedelta.days floors, so a moment in the future is -1 days ("today").
    let delta = now - then;
    let days = (delta.num_milliseconds() as f64 / 86_400_000.0).floor() as i64;
    if days < 1 {
        return "today".to_string();
    }
    if days < 30 {
        return format!("{days}d ago");
    }
    if days < 365 {
        return format!("{}mo ago", days.div_euclid(30));
    }
    format!("{:.1}y ago", days as f64 / 365.0)
}

/// One meaningful word from a Categories= list.
///
/// Generic buckets lead most .desktop files, so taking the first entry would
/// label nearly everything "Utility". Prefer whichever specific one appears.
pub fn primary_category(cats: &str) -> String {
    if cats.is_empty() {
        return String::new();
    }
    let parts: Vec<&str> = cats.split(';').filter(|c| !c.is_empty()).collect();
    const GENERIC: &[&str] = &[
        "Application", "Utility", "System", "Settings", "GTK", "Qt", "GNOME", "KDE",
        "ConsoleOnly",
    ];
    for c in &parts {
        if !GENERIC.contains(c) {
            return (*c).to_string();
        }
    }
    parts.first().map(|s| (*s).to_string()).unwrap_or_default()
}

/// `_fmt_size`: megabytes as the table shows them.
pub fn fmt_size(mb: f64) -> String {
    if mb == 0.0 {
        return String::new();
    }
    if mb >= 1024.0 {
        return format!("{:.1} GB", mb / 1024.0);
    }
    if mb >= 1.0 {
        return format!("{mb:.0} MB");
    }
    format!("{:.0} KB", mb * 1024.0)
}

/// `_reverse_text`: sorts as the reverse of the string it wraps.
///
/// Gtk.CustomSorter has no descending flag, and negating a string is not a
/// thing, so reversing the comparison is the way to get Z -> A.
pub fn reverse_text(a: &str, b: &str) -> Ordering {
    // Python compares str by code point; UTF-8 byte order is the same order.
    b.cmp(a)
}

/// `a.lower()` compared with `b.lower()`, without building either: the
/// sorters run this on every comparison of a sort over thousands of rows.
/// ASCII (nearly every package name) is lowered byte by byte; anything
/// else is lowered as `str::to_lowercase` does it, final sigma included.
pub fn lower_cmp(a: &str, b: &str) -> Ordering {
    if a.is_ascii() && b.is_ascii() {
        return a.bytes().map(|c| c.to_ascii_lowercase()).cmp(b.bytes().map(|c| c.to_ascii_lowercase()));
    }
    a.to_lowercase().cmp(&b.to_lowercase())
}

/// `_version_text`: the Version column's text for one row.
pub fn version_text(it: &AppItem) -> String {
    if it.git_behind != 0 {
        let suffix = if it.git_dirty { " · local edits" } else { "" };
        return format!("{} · {} commits behind{}", it.app.version, it.git_behind, suffix);
    }
    if !it.has_update {
        return it.app.version.clone();
    }
    let behind = it.behind;
    let kind = it.behind_kind.as_str();
    let short = match kind {
        "major" => Some("maj"),
        "minor" => Some("min"),
        "patch" | "point" => Some("pt"),
        _ => None,
    };
    let mut suffix = String::new();
    if let (true, Some(s)) = (behind > 0, short) {
        suffix = format!("  ·{behind} {s}");
    } else if kind == "rel" {
        suffix = "  ·rebuild".to_string();
    }
    format!("{} → {}{}", it.app.version, it.app.update, suffix)
}

// --- reading the mixed-type `extra` dict the way Python's builtins do ------

/// Python truthiness of a JSON value.
pub(super) fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// `float(e.get(key, 0) or 0)`, falling back to 0 where Python would raise.
pub(super) fn as_float(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        Some(Value::String(s)) => s.trim().parse::<f64>().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// `int(e.get(key, 0) or 0)`, falling back to 0 where Python would raise.
pub(super) fn as_int(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f.trunc() as i64))
            .unwrap_or(0),
        Some(Value::Bool(b)) => i64::from(*b),
        Some(Value::String(s)) => s.trim().parse::<i64>().unwrap_or(0),
        _ => 0,
    }
}

/// `e.get(key, "")` for a string field.
pub(in crate::ui) fn as_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

/// `e.get("behind", (0, ""))`: the (count, kind) pair, stored as a JSON array.
pub(super) fn as_behind(e: &Map<String, Value>) -> (i64, String) {
    match e.get("behind") {
        Some(Value::Array(a)) => (as_int(a.first()), as_text(a.get(1))),
        _ => (0, String::new()),
    }
}

/// `str(exc)` for an error the core modules spell "TypeError: message" (the
/// form their `Err`s take, so parity can compare them): the message alone,
/// which is what the Python shows when it formats a caught exception.
pub fn exc_str(e: &str) -> String {
    match e.split_once(": ") {
        Some((t, m))
            if t.ends_with("Error")
                && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.') =>
        {
            m.to_string()
        }
        _ => e.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Background work
// ---------------------------------------------------------------------------

/// The stack [`spawn_bg`] runs its jobs on: 8 MiB, what a Python thread had
/// (glibc's default) and what the depth limits copied from Python were
/// measured on -- sources' 2000 levels, security's 1000, inventory's 900,
/// which those modules walk recursively. Nothing deeper reaches a job's own
/// frames: ghcache refuses a body past `ghcache::VALUE_DEPTH`, the readers
/// that keep Python's 52 000 levels clone, compare, convert and drop
/// without recursion, and the few that still recurse that deep (agents,
/// picks, ghcache's printer) start a `util::JSON_STACK` thread of their
/// own. The inventory collectors run on `inventory::COLLECTOR_STACK`, 8 MiB
/// like this one. Reserved, not used.
const BG_STACK: usize = 8 << 20;

/// What a panic says: its message, or "" for a payload that is not text
/// (as `str(exc)` of a bare exception).
pub fn panic_text(err: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = err.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = err.downcast_ref::<String>() {
        s.clone()
    } else {
        String::new()
    }
}

/// A window's handler for failed background work (see `on_bg_failure`).
type BgFailed = Box<dyn Fn(&str)>;

thread_local! {
    /// What the window does when background work handed to [`spawn_bg`]
    /// panics: its `done` cannot be called without a value, so this stands
    /// in for it (the window clears its busy state and says so).
    static BG_FAILED: std::cell::RefCell<Option<BgFailed>> = const { std::cell::RefCell::new(None) };
}

/// Set what [`spawn_bg`] does on the main thread when its work panics.
pub fn on_bg_failure(f: impl Fn(&str) + 'static) {
    BG_FAILED.with(|h| *h.borrow_mut() = Some(Box::new(f)));
}

fn bg_failed(msg: &str) {
    eprintln!("almanac: background work failed: {msg}");
    // Taken out while it runs, so a handler that starts more work (and has
    // it fail) cannot borrow it twice.
    let handler = BG_FAILED.with(|h| h.borrow_mut().take());
    if let Some(f) = handler {
        f(msg);
        BG_FAILED.with(|h| {
            let mut slot = h.borrow_mut();
            if slot.is_none() {
                *slot = Some(f);
            }
        });
    }
}

/// `threading.Thread(target=work) + GLib.idle_add(done, result)`: run `work`
/// off the main thread, then `done` with its result on the main thread.
///
/// If `work` panics there is no result for `done`; the window's failure
/// handler ([`on_bg_failure`]) runs instead, so a page that set itself busy
/// for the work is not left spinning forever with nothing said. Work whose
/// caller has its own way to report a failure uses [`spawn_bg_checked`].
pub fn spawn_bg<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(T) + 'static,
) {
    spawn_bg_checked(work, move |result| match result {
        Ok(value) => done(value),
        Err(msg) => bg_failed(&msg),
    });
}

/// [`spawn_bg`], with `done` always called: `Err` with the panic's message
/// when `work` panicked.
pub fn spawn_bg_checked<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(Result<T, String>) + 'static,
) {
    // On a stack of its own, so its size does not hang on `ulimit -s` (the
    // pool thread's, and an overflow there aborts the app).
    let handle = gio::spawn_blocking(move || {
        crate::util::with_stack(BG_STACK, move || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).map_err(|e| panic_text(&*e))
        })
    });
    glib::spawn_future_local(async move {
        let result = match handle.await {
            Ok(result) => result,
            Err(e) => Err(panic_text(&*e)),
        };
        done(result);
    });
}


/// `self._thumb_pool.submit(work)` + `GLib.idle_add(done, result)`: like
/// `spawn_bg`, but on the window's small image pool, so 5,600 cards cannot
/// open 5,600 connections on the first scroll. Falls back to `spawn_bg` if
/// the pool refuses the job.
pub fn thumb_bg<T: Send + 'static>(
    pool: &glib::ThreadPool,
    work: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(T) + 'static,
) {
    // `done` holds main-thread objects; the guard lets it ride along to the
    // worker and back without ever being touched there.
    let done = glib::thread_guard::ThreadGuard::new(done);
    let job = std::sync::Arc::new(std::sync::Mutex::new(Some((work, done))));
    let theirs = job.clone();
    let pushed = pool.push(move || {
        let Some((work, done)) = theirs.lock().ok().and_then(|mut j| j.take()) else {
            return;
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));
        glib::MainContext::default().invoke(move || {
            let done = done.into_inner();
            match result {
                Ok(value) => done(value),
                Err(e) => bg_failed(&panic_text(&*e)),
            }
        });
    });
    if pushed.is_err() {
        if let Some((work, done)) = job.lock().ok().and_then(|mut j| j.take()) {
            spawn_bg(work, done.into_inner());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtk::gdk;

    #[test]
    fn a_guarded_switch_runs_its_handler_for_user_flips_only() {
        // As `ui::packages`' widget test: skip with no display (a package
        // build's check()), or when GTK belongs to another test thread.
        let set = |var| std::env::var_os(var).is_some_and(|v| !v.is_empty());
        let broadway = std::env::var("GDK_BACKEND").is_ok_and(|b| b.contains("broadway"));
        if !(set("WAYLAND_DISPLAY") || set("DISPLAY") || broadway) {
            return;
        }
        if !matches!(std::panic::catch_unwind(adw::init), Ok(Ok(()))) || gdk::Display::default().is_none() {
            return;
        }
        let sw = gtk::Switch::new();
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let guard = SwitchGuard::default();
        let c = calls.clone();
        guard.connect(&sw, move |_, _| {
            c.set(c.get() + 1);
            glib::Propagation::Stop
        });
        sw.set_active(true); // as a click does
        assert_eq!(calls.get(), 1);
        assert!(!sw.state(), "the handler stopped it: the state waits");
        guard.set(&sw, false); // snapping back
        guard.set(&sw, true);
        assert_eq!(calls.get(), 1);
        assert!(sw.is_active() && sw.state());
        sw.set_active(false);
        assert_eq!(calls.get(), 2);
    }

    /// Work that panics still reaches the main thread: `spawn_bg_checked`'s
    /// `done` gets the panic as `Err`, and `spawn_bg` runs the window's
    /// failure handler in place of a `done` it has no value for -- before,
    /// both were skipped and a busy page spun forever.
    #[test]
    fn background_work_that_panics_still_reports_back() {
        let ctx = glib::MainContext::new();
        ctx.with_thread_default(|| {
            let got = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
            let g = got.clone();
            spawn_bg_checked(|| -> i32 { panic!("boom") }, move |r| g.borrow_mut().push(format!("{r:?}")));
            let g = got.clone();
            spawn_bg_checked(|| 7, move |r| g.borrow_mut().push(format!("{r:?}")));
            let g = got.clone();
            on_bg_failure(move |msg| g.borrow_mut().push(format!("failed: {msg}")));
            let g = got.clone();
            spawn_bg(|| -> i32 { panic!("bang") }, move |v| g.borrow_mut().push(format!("done {v}")));
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            while got.borrow().len() < 3 && std::time::Instant::now() < deadline {
                ctx.iteration(true);
            }
            let mut got = got.borrow().clone();
            got.sort();
            assert_eq!(got, ["Err(\"boom\")", "Ok(7)", "failed: bang"]);
        })
        .expect("a fresh main context");
    }

    #[test]
    fn lower_cmp_orders_as_lowering_first_does() {
        let words = [
            "", "a", "A", "ab", "aB", "Ab", "b", "B", "_x", "[x", "Zed", "zed", "zeta", "Émile", "émile", "eMile",
            "ΟΔΟΣ", "οδος", "οδοσ", "İstanbul", "istanbul", "ß", "SS", "ss", "Ä", "a\u{308}", "日本", "Z9", "z10",
        ];
        for a in words {
            for b in words {
                assert_eq!(lower_cmp(a, b), a.to_lowercase().cmp(&b.to_lowercase()), "{a:?} vs {b:?}");
            }
        }
    }

    /// test_star_counts_fit_a_chip
    #[test]
    fn star_counts_fit_a_chip() {
        let got: Vec<String> = [198, 2151, 2000, 15419, 66175].iter().map(|n| compact(*n)).collect();
        assert_eq!(got, ["198", "2.2k", "2k", "15k", "66k"]);
    }

    #[test]
    fn primary_category_skips_generic() {
        assert_eq!(primary_category("Utility;TextEditor;"), "TextEditor");
        assert_eq!(primary_category("Utility;GTK;"), "Utility");
        assert_eq!(primary_category(""), "");
    }

    #[test]
    fn version_text_shapes() {
        let mut it = AppItem::default();
        it.app.version = "1.0".into();
        assert_eq!(version_text(&it), "1.0");
        it.has_update = true;
        it.app.update = "2.0".into();
        it.behind = 1;
        it.behind_kind = "major".into();
        assert_eq!(version_text(&it), "1.0 → 2.0  ·1 maj");
        it.behind_kind = "rel".into();
        assert_eq!(version_text(&it), "1.0 → 2.0  ·rebuild");
        it.git_behind = 3;
        it.git_dirty = true;
        assert_eq!(version_text(&it), "1.0 · 3 commits behind · local edits");
    }
}
