//! UI section `shell` (port of `app.py` lines 1–1454 and 6481–end).
//!
//! almanac — one window for everything installed on this machine.
//!
//! Design rule: this app owns the VIEW and the AppImage registry. It does not
//! own package state: installs and updates are handed to the tool that owns
//! them (yay, pacman, mise, git), with root requested through polkit, and a
//! full upgrade runs in a real terminal through the system's own updater.
//! Every result is then checked rather than assumed -- see verify.py.
//!
//! This file holds the helpers, the stylesheet, the list items, the window
//! itself (`Win` / `Inner` / `Shared`), the sidebar, header and columns,
//! persisted window state, external change detection, `AlmanacApp` and the
//! CLI. Every other section adds its own `impl Win` block (ARCHITECTURE.md).

use std::cell::{Ref, RefCell};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::Deref;
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use serde_json::{Map, Value};

use crate::i18n::tr;
use crate::inventory;

// ---------------------------------------------------------------------------
// Module-level helpers and tables
// ---------------------------------------------------------------------------

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

/// `_CSS`: the app's stylesheet, verbatim, in `data/style.css`.
pub const CSS: &str = include_str!("../../data/style.css");

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
fn truthy(v: Option<&Value>) -> bool {
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
fn as_float(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        Some(Value::String(s)) => s.trim().parse::<f64>().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// `int(e.get(key, 0) or 0)`, falling back to 0 where Python would raise.
fn as_int(v: Option<&Value>) -> i64 {
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
pub(super) fn as_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

/// `e.get("behind", (0, ""))`: the (count, kind) pair, stored as a JSON array.
fn as_behind(e: &Map<String, Value>) -> (i64, String) {
    match e.get("behind") {
        Some(Value::Array(a)) => (as_int(a.first()), as_text(a.get(1))),
        _ => (0, String::new()),
    }
}

// ---------------------------------------------------------------------------
// List items
// ---------------------------------------------------------------------------

/// Wrapper so `inventory::App` can live in a `gio::ListStore` (as a
/// `glib::BoxedAnyObject`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AppItem {
    pub app: inventory::App,
    // Flattened for the sorters, which must be cheap: they run on every
    // comparison during a sort.
    pub name: String,
    pub category: String,
    pub size_mb: f64,
    pub installed_ts: f64,
    pub source: String,
    pub has_update: bool,
    pub critical: String,
    pub stale: String,
    pub advice: String,
    pub behind: i64,
    pub behind_kind: String,
    pub git_behind: i64,
    pub git_dirty: bool,
    pub priority: i32,
    pub votes: i64,
    pub popularity: f64,
    pub added: String,
    pub verified: bool,
    pub haystack: String,
}

impl AppItem {
    pub fn new(app: inventory::App) -> AppItem {
        let e = &app.extra;
        let mut category = primary_category(&as_text(e.get("categories")));
        if category.is_empty() && app.source == inventory::LOCAL {
            category = "Local build".to_string();
        }
        let critical = as_text(e.get("critical"));
        let stale = as_text(e.get("stale"));
        let (mut behind, behind_kind) = as_behind(e);
        let git_behind = as_int(e.get("git_behind"));
        if git_behind != 0 {
            // Sorting treats commits-behind as its own magnitude; mixing it
            // with semver counts would compare unlike things.
            behind = git_behind;
        }
        let has_update = app.has_update();
        // Critical first, then any update, then the rest -- so the default
        // sort already puts the rows that matter at the top.
        let priority = if !critical.is_empty() || !stale.is_empty() {
            0
        } else if has_update && app.source == inventory::SYSTEM {
            1
        } else if has_update {
            2
        } else {
            3
        };
        AppItem {
            name: app.name.clone(),
            size_mb: as_float(e.get("size_mb")),
            installed_ts: as_float(e.get("installed_ts")),
            source: app.source.clone(),
            has_update,
            advice: as_text(e.get("advice")),
            behind,
            behind_kind,
            git_behind,
            git_dirty: truthy(e.get("git_dirty")),
            priority,
            votes: as_int(e.get("votes")),
            popularity: as_float(e.get("popularity")),
            added: as_text(e.get("added")),
            verified: truthy(e.get("verified")),
            haystack: format!("{} {} {}", app.name, app.description, category).to_lowercase(),
            category,
            critical,
            stale,
            app,
        }
    }

    /// Box it for a `gio::ListStore`.
    pub fn boxed(self) -> glib::BoxedAnyObject {
        glib::BoxedAnyObject::new(self)
    }

    /// The `AppItem` inside a list-model row, or None if the row holds
    /// something else.
    pub fn of(obj: &glib::Object) -> Option<Ref<'_, AppItem>> {
        obj.downcast_ref::<glib::BoxedAnyObject>()?.try_borrow::<AppItem>().ok()
    }
}

/// A marketplace entry, boxed so a `gio::ListStore` can hold it.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginItem {
    pub plugin: crate::store::Plugin,
}

impl PluginItem {
    pub fn new(plugin: crate::store::Plugin) -> PluginItem {
        PluginItem { plugin }
    }

    /// Box it for a `gio::ListStore`.
    pub fn boxed(self) -> glib::BoxedAnyObject {
        glib::BoxedAnyObject::new(self)
    }

    /// The `PluginItem` inside a list-model row, or None.
    pub fn of(obj: &glib::Object) -> Option<Ref<'_, PluginItem>> {
        obj.downcast_ref::<glib::BoxedAnyObject>()?.try_borrow::<PluginItem>().ok()
    }
}

// ---------------------------------------------------------------------------
// The window's tables (AlmanacWindow class attributes)
// ---------------------------------------------------------------------------

/// (key, label, icon) of one sidebar destination. Labels are msgids:
/// translate with `i18n::tr` where they are shown.
pub type GroupItem = (&'static str, &'static str, &'static str);

/// Grouped by the question each section answers: what can I get, what
/// wants my attention, what do I have, and where did it come from. Eleven
/// peers in one list made all of them equally weighted, which none are.
pub const GROUPS: &[(&str, Option<&str>, &[GroupItem])] = &[
    // The two primary destinations, ungrouped: getting software and
    // keeping it current are what the app is for, and a header above them
    // would rank them as peers of "Sources".
    ("primary", None, &[
        // "featured" stays the key: saved state and --page featured keep
        // working. Nothing here is featured by anyone; it is what is true
        // today.
        ("featured", "Today", "weather-clear-symbolic"),
        ("discover", "App Store", "system-software-install-symbolic"),
        ("themes", "Themes", "preferences-desktop-wallpaper-symbolic"),
        ("updates", "Updates", "software-update-available-symbolic"),
        (inventory::AGENT, "AI", "applications-science-symbolic"),
    ]),
    ("library", Some("Library"), &[
        ("apps", "Applications", "view-grid-symbolic"),
        ("plugins", "Plugins", "application-x-addon-symbolic"),
        ("switchboard", "Switchboard", "almanac-switchboard-symbolic"),
        ("all", "Everything", "view-list-symbolic"),
    ]),
    ("sources", Some("Sources"), &[
        (inventory::SYSTEM, "System", "preferences-system-symbolic"),
        (inventory::REPO, "Repositories", "package-x-generic-symbolic"),
        (inventory::AUR, "AUR", "package-x-generic-symbolic"),
        (inventory::APPIMAGE, "AppImages", "application-x-executable-symbolic"),
        (inventory::FLATPAK, "Flatpak", "application-x-executable-symbolic"),
        (inventory::LOCAL, "Local builds", "applications-engineering-symbolic"),
        (inventory::TOOL, "Dev tools", "utilities-terminal-symbolic"),
    ]),
    ("maintenance", Some("Maintenance"), &[
        ("maintenance", "Health check", "object-select-symbolic"),
        ("history", "History", "document-open-recent-symbolic"),
        (inventory::ORPHAN, "Orphans", "user-trash-symbolic"),
    ]),
];

/// Sources collapse by default: they answer "where did this come from",
/// which is a question you ask occasionally, not on every launch.
pub const COLLAPSED_BY_DEFAULT: &[&str] = &["sources"];

/// One entry of `PAGES`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageSpec {
    pub key: &'static str,
    /// which stack child to show
    pub view: &'static str,
    /// msgid; translate where shown
    pub title: &'static str,
    /// placeholder, or None when the box does nothing here
    pub search: Option<&'static str>,
    /// whether the Python wraps `search` in `_()` (some placeholders are not)
    pub search_translated: bool,
    /// show the "Update all" button
    pub update: bool,
    /// show the sort menu
    pub sort: bool,
    pub filters: bool,
    pub market: Option<&'static str>,
}

/// Every page, described once. Before this the header's buttons were shown
/// and hidden from seventeen places across six methods, which is how a page
/// ends up with another page's toolbar.
pub const PAGES: &[PageSpec] = &[
    PageSpec { key: "featured", view: "featured", title: "Today", search: None,
               search_translated: false, update: false, sort: false, filters: false,
               market: None },
    PageSpec { key: "discover", view: "store", title: "App Store",
               search: Some("Search plugins and apps…"), search_translated: false,
               update: false, sort: false, filters: true, market: None },
    // The same view, pinned to one marketplace. Without this themes were
    // reachable only through the filter drawer, and the first one did not
    // appear until the thirtieth card of the default ordering.
    PageSpec { key: "themes", view: "store", title: "Themes",
               search: Some("Search Omarchy themes…"), search_translated: false,
               update: false, sort: false, filters: true, market: Some("themes") },
    PageSpec { key: "packages", view: "list", title: "Software",
               search: Some("Search installed software"), search_translated: false,
               update: true, sort: true, filters: false, market: None },
    PageSpec { key: "maintenance", view: "maintenance", title: "Health check", search: None,
               search_translated: false, update: false, sort: false, filters: false,
               market: None },
    PageSpec { key: "plugins", view: "plugins", title: "Plugins",
               search: Some("Search plugins"), search_translated: true,
               update: false, sort: false, filters: false, market: None },
    PageSpec { key: "switchboard", view: "switchboard", title: "Switchboard",
               search: Some("Search the switchboard"), search_translated: true,
               update: false, sort: false, filters: false, market: None },
    PageSpec { key: "history", view: "history", title: "History", search: None,
               search_translated: false, update: false, sort: false, filters: false,
               market: None },
    PageSpec { key: inventory::AGENT, view: "ai", title: "AI", search: None,
               search_translated: false, update: false, sort: false, filters: false,
               market: None },
];

/// `PAGES.get(key)`.
pub fn page_spec(key: &str) -> Option<&'static PageSpec> {
    PAGES.iter().find(|p| p.key == key)
}

/// Ctrl+1 … Ctrl+0, in sidebar order. At most ten: there is no Ctrl+10, and
/// an invalid accelerator makes the window fail to open at all.
pub const PAGE_KEYS: &[&str] = &[
    "featured", "discover", "themes", "updates", inventory::AGENT, "apps", "plugins",
    "switchboard", "maintenance", "history",
];

/// The accelerator of the `n`th page key (1-based): `f"<Control>{n % 10}"`.
pub fn page_accel(n: usize) -> String {
    format!("<Control>{}", n % 10)
}

/// `self.SORTS_INSTALLED`: (key, label) of the package list's sort menu.
pub fn sorts_installed() -> Vec<(&'static str, String)> {
    use crate::i18n::tr;
    vec![
        ("default", tr("Priority (default)")),
        ("name", tr("Name A → Z")),
        ("name_desc", tr("Name Z → A")),
        ("size", tr("Size")),
        ("installed", tr("Install date")),
        ("category", tr("Category")),
        ("behind", tr("Versions behind")),
    ]
}

/// `self.SORTS_DISCOVER`. Only sorts the registry data can actually support:
/// there is no category for 5,576 of the 5,578 plugins, so offering one would
/// be a menu item that silently does nothing.
pub fn sorts_discover() -> Vec<(&'static str, String)> {
    use crate::i18n::tr;
    vec![
        ("default", tr("Best match (default)")),
        ("popular", tr("Most popular")),
        ("trending", tr("Trending")),
        ("newest", tr("Recently added")),
        ("name", tr("Name A → Z")),
        ("name_desc", tr("Name Z → A")),
    ]
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

/// `threading.Thread(target=work) + GLib.idle_add(done, result)`: run `work`
/// off the main thread, then `done` with its result on the main thread. If
/// `work` panics, `done` is not called (as a Python thread that raised never
/// reached its idle_add).
pub fn spawn_bg<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(T) + 'static,
) {
    // On a stack of its own, sized for values as deep as Python's json
    // reads (a cache or registry nested 52000 levels): the pool thread's
    // is whatever `ulimit -s` gave, and an overflow there aborts the app.
    let handle = gio::spawn_blocking(move || crate::util::with_stack(crate::util::JSON_STACK, work));
    glib::spawn_future_local(async move {
        if let Ok(value) = handle.await {
            done(value);
        }
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
            if let Ok(value) = result {
                done(value);
            }
        });
    });
    if pushed.is_err() {
        if let Some((work, done)) = job.lock().ok().and_then(|mut j| j.take()) {
            spawn_bg(work, done.into_inner());
        }
    }
}

// ---------------------------------------------------------------------------
// The window
// ---------------------------------------------------------------------------

/// A sidebar destination: the row plus what Python stashed on it
/// (`row._key`, `row._count`, `row._group`).
#[derive(Debug, Clone)]
pub struct SidebarRow {
    pub key: String,
    pub row: adw::ActionRow,
    pub count: gtk::Label,
    pub group: String,
}

/// A group's header row (`header._group`, `header._chevron`).
#[derive(Debug, Clone)]
pub struct GroupHeader {
    pub group: String,
    pub row: gtk::ListBoxRow,
    pub chevron: gtk::Image,
}

/// `self._primary`: (label, enabled, tooltip, handler) of the current page's
/// main action. One type for both sections: the packages section defines it,
/// since `_primary_actions` builds it.
pub use crate::ui::packages::PrimaryAction;

/// How much decoded image data `TexCache` keeps: about a hundred 640px
/// screenshots, or every icon on a page many times over.
pub const TEX_BUDGET: usize = 48 << 20;

/// Decoded textures by "path@width", so scrolling back is free.
///
/// Bounded by the bytes the pixels take (`tex_bytes`), not by a count: a
/// 72px icon is 20 KB, a 1200px hero image several MB. When it is full the
/// least recently used go first, one at a time, so what is on screen stays.
pub struct TexCache<T = gdk::Texture> {
    /// value, its size, and when it was last used
    entries: HashMap<String, (T, usize, u64)>,
    /// last use -> key, oldest first
    order: BTreeMap<u64, String>,
    tick: u64,
    bytes: usize,
    budget: usize,
}

impl<T: Clone> TexCache<T> {
    pub fn new(budget: usize) -> Self {
        TexCache { entries: HashMap::new(), order: BTreeMap::new(), tick: 0, bytes: 0, budget }
    }

    fn touch(&mut self) -> u64 {
        self.tick += 1;
        self.tick
    }

    /// The value for `key`, now the most recently used.
    pub fn get(&mut self, key: &str) -> Option<T> {
        let tick = self.touch();
        let (value, _, used) = self.entries.get_mut(key)?;
        self.order.remove(used);
        *used = tick;
        let value = value.clone();
        self.order.insert(tick, key.to_string());
        Some(value)
    }

    /// Keep `value` (`bytes` big) under `key`, dropping the least recently
    /// used until it fits. One bigger than the whole budget is not kept.
    pub fn insert(&mut self, key: String, value: T, bytes: usize) {
        self.remove(&key);
        if bytes > self.budget {
            return;
        }
        while self.bytes + bytes > self.budget {
            let Some((_, oldest)) = self.order.pop_first() else { break };
            if let Some((_, size, _)) = self.entries.remove(&oldest) {
                self.bytes -= size;
            }
        }
        let tick = self.touch();
        self.order.insert(tick, key.clone());
        self.entries.insert(key, (value, bytes, tick));
        self.bytes += bytes;
    }

    fn remove(&mut self, key: &str) {
        if let Some((_, size, used)) = self.entries.remove(key) {
            self.order.remove(&used);
            self.bytes -= size;
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The bytes held.
    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

/// What a texture holds decoded: four bytes a pixel.
pub fn tex_bytes(tex: &gdk::Texture) -> usize {
    tex.width().max(0) as usize * tex.height().max(0) as usize * 4
}

/// State read and written by more than one section.
pub struct Shared {
    pub filter_key: String,
    pub query: String,
    /// `self._page`: None until `_show_page` first sets it. The Python
    /// reads it with `getattr(self, "_page", <default>)` and a different
    /// default in different places ("packages", "discover", ""), so readers
    /// go through `page_or` with the default their Python line has.
    pub page: Option<String>,
    /// `self.store`: every row of the package tables, as boxed `AppItem`s.
    pub store: gio::ListStore,
    /// Filter -> sort -> selection. Filtering first means the sorter only
    /// ever walks the rows actually on screen. (`self.filter`; its function
    /// is `Win::match`, set when the window is built.)
    pub filter: gtk::CustomFilter,
    /// `self.sorted`
    pub sorted: gtk::SortListModel,
    /// Decoded textures, so scrolling back is free (see `TexCache`).
    pub tex_cache: TexCache,
    /// `self.sidebar_rows`, in sidebar order.
    pub sidebar_rows: Vec<SidebarRow>,
    /// `self.group_rows`: None for the ungrouped primary group.
    pub group_rows: Vec<(String, Option<GroupHeader>)>,
    pub group_members: BTreeMap<String, Vec<SidebarRow>>,
    /// `self._collapsed`
    pub collapsed: BTreeSet<String>,
    pub primary: Option<PrimaryAction>,
    pub manual_sorter: Option<gtk::Sorter>,
    pub narrow: bool,
    /// Row-menu actions resolve by name at activation time, because the row
    /// widgets themselves are recycled as the list scrolls.
    pub row_actions: HashMap<String, inventory::App>,
    pub discovering: bool,
    pub discover_timer: Option<glib::SourceId>,
    // Marketplace state. Images are fetched on a small pool -- 5,600 cards
    // would otherwise open 5,600 connections on the first scroll -- and
    // decoded textures are kept so scrolling back is free.
    pub plugins: Vec<crate::store::Plugin>,
    pub store_cats: Vec<String>,
    pub store_cat_key: String,
    pub store_timer: Option<glib::SourceId>,
    pub store_loaded_once: bool,
    pub pinning: bool,
    pub findings: Findings,
    pub maint_loaded_once: bool,
    pub ai_survey: Vec<crate::ai::Group>,
    pub ai_loaded_once: bool,
    pub shelves: Vec<crate::featured::Shelf>,
    pub feat_loaded_once: bool,
    /// Findings already announced, so a rescan does not re-notify.
    pub reported: BTreeSet<String>,
    /// `self._plugin_updates`: written by the Plugins page, read by the
    /// header's primary action (`getattr(self, "_plugin_updates", 0)`).
    pub plugin_updates: usize,
    /// `self.store_sort`: the App Store's sort dropdown. Built by the store
    /// section, reset by the AI page's filter reset, set by Today's shelf links.
    pub store_sort: Option<gtk::DropDown>,
    /// The rest of the store's filter drawer (`self.store_cat`,
    /// `store_trust`, `store_market`, `store_verified`, `store_installed`):
    /// built by the store section, reset by `store_reset_filters` (ai).
    pub store_cat: Option<gtk::DropDown>,
    pub store_trust: Option<gtk::DropDown>,
    pub store_market: Option<gtk::DropDown>,
    pub store_verified: Option<gtk::ToggleButton>,
    pub store_installed: Option<gtk::ToggleButton>,
    /// `self.store_drawer`: the filter drawer behind `filter_btn`, built by
    /// the store section and attached by `show_page`.
    pub store_drawer: Option<gtk::Popover>,
    /// `self.store_model`: the cards showing, as boxed `PluginItem`s. Filled
    /// by the store section, read by `on_card_activated` (store_actions).
    pub store_model: gio::ListStore,
    /// `self.store_grid`: its column count follows `set_narrow`.
    pub store_grid: Option<gtk::GridView>,
    /// `self._store_markets`: market ids parallel to the marketplace
    /// dropdown ("" = all). Read by `pin_market` / `retitle_store`.
    pub store_markets: Vec<String>,
    /// `self._filters_for`: whether the filter lists were last built for the
    /// Themes page (None: never built). Read by `pin_market`.
    pub filters_for: Option<bool>,
    /// `self._plugins is None`. The Python's `__init__` starts `_plugins` as
    /// `[]`, so this starts false; a Preferences marketplace switch
    /// (`reload_store`) sets it (and empties `plugins`, as None holds
    /// nothing). Whoever assigns `plugins` (`store_loaded`, `feat_reload`'s
    /// fetch) sets it back to false; `plugins` stays a `Vec` because every
    /// other reader treats None as an empty list.
    pub plugins_none: bool,
}

impl Default for Shared {
    fn default() -> Shared {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let filter = gtk::CustomFilter::new(|_| true);
        let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
        let sorted = gtk::SortListModel::new(Some(filtered), None::<gtk::Sorter>);
        Shared {
            filter_key: "all".to_string(),
            query: String::new(),
            page: None,
            store,
            filter,
            sorted,
            tex_cache: TexCache::new(TEX_BUDGET),
            sidebar_rows: Vec::new(),
            group_rows: Vec::new(),
            group_members: BTreeMap::new(),
            collapsed: COLLAPSED_BY_DEFAULT.iter().map(|s| (*s).to_string()).collect(),
            primary: None,
            manual_sorter: None,
            narrow: false,
            row_actions: HashMap::new(),
            discovering: false,
            discover_timer: None,
            plugins: Vec::new(),
            store_cats: vec![String::new()],
            store_cat_key: String::new(),
            store_timer: None,
            store_loaded_once: false,
            pinning: false,
            findings: Findings::default(),
            maint_loaded_once: false,
            ai_survey: Vec::new(),
            ai_loaded_once: false,
            shelves: Vec::new(),
            feat_loaded_once: false,
            reported: BTreeSet::new(),
            plugin_updates: 0,
            store_sort: None,
            store_cat: None,
            store_trust: None,
            store_market: None,
            store_verified: None,
            store_installed: None,
            store_drawer: None,
            store_model: gio::ListStore::new::<glib::BoxedAnyObject>(),
            store_grid: None,
            store_markets: vec![String::new()],
            filters_for: None,
            plugins_none: false,
        }
    }
}

/// `self._findings`. The Python assigns it from worker threads as well as
/// the main one (`_feat_reload`'s worker sets it the moment it has run the
/// checks, before its `idle_add`), and what reads it next -- the sidebar
/// stats' "Free-able", the hero -- sees it at once. A handle the workers can
/// hold keeps that: one list, behind a lock, shared by every clone.
#[derive(Clone, Default)]
pub struct Findings(std::sync::Arc<std::sync::Mutex<Vec<crate::maintenance::Finding>>>);

impl Findings {
    /// The list as it is now.
    pub fn get(&self) -> Vec<crate::maintenance::Finding> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// `self._findings = findings`
    pub fn set(&self, findings: Vec<crate::maintenance::Finding>) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = findings;
    }
}

impl Shared {
    /// `getattr(self, "_page", default)`.
    pub fn page_or(&self, default: &str) -> String {
        self.page.clone().unwrap_or_else(|| default.to_string())
    }

    /// `self.sidebar_rows.get(key)`.
    pub fn sidebar_row(&self, key: &str) -> Option<&SidebarRow> {
        self.sidebar_rows.iter().find(|r| r.key == key)
    }

    /// `key in self.group_rows`
    pub fn has_group(&self, key: &str) -> bool {
        self.group_rows.iter().any(|(k, _)| k == key)
    }
}

/// State only this section uses.
#[derive(Default)]
pub struct State {
    /// Kept so the theme watch keeps delivering (a dropped monitor stops).
    pub theme_monitors: Vec<gio::FileMonitor>,
    /// pacman.log watch; None when it could not be set up.
    pub monitor: Option<gio::FileMonitor>,
    pub pending_rescan: Option<glib::SourceId>,
    pub config_monitors: Vec<gio::FileMonitor>,
    pub config_pending: Option<glib::SourceId>,
    pub config_reported: BTreeSet<String>,
    /// (sort column title, descending) restored from window-state.json.
    pub wanted_sort: Option<(Option<String>, bool)>,
}

/// The window. Cheap to clone; callbacks hold a `WeakWin`.
#[derive(Clone)]
pub struct Win(Rc<Inner>);

/// A weak handle on the window, for signal handlers.
#[derive(Clone)]
pub struct WeakWin(Weak<Inner>);

impl WeakWin {
    pub fn upgrade(&self) -> Option<Win> {
        self.0.upgrade().map(Win)
    }
}

impl Deref for Win {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

/// Everything the window owns: the widgets shared across sections (created
/// once, never replaced), the shared state, and one state per section.
pub struct Inner {
    pub window: adw::ApplicationWindow,
    /// The Omarchy palette, applied over our own stylesheet.
    pub theme_provider: gtk::CssProvider,
    pub sidebar_toggle: gtk::ToggleButton,
    /// The sidebar's stats footer.
    pub stats: gtk::Grid,
    pub title: adw::WindowTitle,
    pub busy: gtk::Spinner,
    pub search: gtk::SearchEntry,
    pub search_bar: gtk::Box,
    pub update_btn: gtk::Button,
    pub sort_btn: gtk::MenuButton,
    /// The same control as `sort_btn`: a sort menu on the package list and a
    /// filter drawer in the store.
    pub filter_btn: gtk::MenuButton,
    pub store_back: gtk::Button,
    pub view: gtk::ColumnView,
    /// `self._priority_sorter`: urgency, then name.
    pub priority_sorter: gtk::CustomSorter,
    pub list_stack: gtk::Stack,
    pub list_empty: adw::StatusPage,
    /// Above the Updates table: why the count is what it is, and whether the
    /// batch needs a restart (`mirror::notes`).
    pub update_notes: gtk::ListBox,
    pub content_stack: gtk::Stack,
    pub toasts: adw::ToastOverlay,
    pub split: adw::OverlaySplitView,
    /// `self._thumb_pool`: six workers for images (see `thumb_bg`). None
    /// only if GLib could not make a pool; `thumb_bg` then uses `spawn_bg`.
    pub thumb_pool: Option<glib::ThreadPool>,

    pub shared: RefCell<Shared>,
    pub shell: RefCell<State>,
    pub packages: RefCell<crate::ui::packages::State>,
    pub store: RefCell<crate::ui::store::State>,
    pub store_actions: RefCell<crate::ui::store_actions::State>,
    pub plugins: RefCell<crate::ui::plugins::State>,
    pub switchboard: RefCell<crate::ui::switchboard::State>,
    pub history: RefCell<crate::ui::history::State>,
    pub ai: RefCell<crate::ui::ai::State>,
    pub preferences: RefCell<crate::ui::preferences::State>,
    pub today: RefCell<crate::ui::today::State>,
    pub actions: RefCell<crate::ui::actions::State>,
}

/// A `GtkLabel` that is centred vertically and hidden: a sidebar count.
fn count_label() -> gtk::Label {
    let count = gtk::Label::builder().valign(gtk::Align::Center).visible(false).build();
    count.add_css_class("count-badge");
    count
}

/// One sidebar destination row (the loop body shared by both kinds of group).
fn sidebar_entry(key: &str, label: &str, icon: &str, gkey: &str) -> SidebarRow {
    let row = adw::ActionRow::builder().title(tr(label)).build();
    row.set_title_lines(1);
    row.add_prefix(&gtk::Image::from_icon_name(icon));
    let count = count_label();
    row.add_suffix(&count);
    SidebarRow { key: key.to_string(), row, count, group: gkey.to_string() }
}


impl Win {
    /// `AlmanacWindow.__init__`: build the window, its sidebar, header,
    /// table and pages, restore saved state and start the first scan.
    pub fn new(app: &adw::Application) -> Win {
        let window = adw::ApplicationWindow::builder().application(app).build();
        window.set_title(Some(&tr("Almanac")));
        window.set_icon_name(Some("io.github.twodxme.Almanac"));
        window.set_default_size(1320, 800);
        // Nothing here should ever need a horizontal scrollbar, so the window
        // has to be able to shrink to a half-screen tile.
        window.set_size_request(480, 420);

        let shared = Shared::default();
        let selection = gtk::SingleSelection::new(Some(shared.sorted.clone()));
        selection.set_can_unselect(true);
        selection.set_autoselect(false);

        // The widgets every section reaches for, made here so `Inner` can
        // hold them; `init` configures and places them in __init__'s order.
        let sort_btn = gtk::MenuButton::builder().icon_name("view-sort-descending-symbolic").build();
        let busy = gtk::Spinner::builder().valign(gtk::Align::Center).build();
        let stats = gtk::Grid::builder()
            .column_spacing(12)
            .row_spacing(1)
            .column_homogeneous(true)
            .margin_start(12)
            .margin_end(12)
            .margin_top(6)
            .margin_bottom(8)
            .build();
        let search_bar = gtk::Box::builder()
            .margin_start(12)
            .margin_end(12)
            .margin_top(6)
            .margin_bottom(2)
            .build();
        // Gtk.Button(label=..., icon_name=...): properties are applied in
        // that order, as PyGObject does, so the result is the same button.
        let store_back = gtk::Button::builder()
            .label(tr("Back to the App Store"))
            .icon_name("go-previous-symbolic")
            .halign(gtk::Align::Start)
            .margin_start(12)
            .margin_bottom(6)
            .build();
        let list_empty = Win::status_page("object-select-symbolic", &tr("Nothing here"), "", None);
        // Six workers, as ThreadPoolExecutor(max_workers=6). A pool that
        // cannot be made leaves thumb_bg on spawn_bg.
        let thumb_pool = glib::ThreadPool::shared(Some(6)).ok();

        let inner = Inner {
            window,
            theme_provider: gtk::CssProvider::new(),
            sidebar_toggle: gtk::ToggleButton::builder().icon_name("sidebar-show-symbolic").build(),
            stats,
            title: adw::WindowTitle::new(&tr("Almanac"), ""),
            busy,
            search: gtk::SearchEntry::builder()
                .placeholder_text(tr("Search installed software"))
                .build(),
            search_bar,
            update_btn: gtk::Button::with_label(&tr("Update all")),
            filter_btn: sort_btn.clone(),
            sort_btn,
            store_back,
            view: gtk::ColumnView::new(Some(selection)),
            priority_sorter: gtk::CustomSorter::new(|a, b| {
                match (AppItem::of(a), AppItem::of(b)) {
                    (Some(a), Some(b)) => {
                        a.priority.cmp(&b.priority).then_with(|| lower_cmp(&a.name, &b.name)).into()
                    }
                    _ => gtk::Ordering::Equal,
                }
            }),
            list_stack: gtk::Stack::new(),
            list_empty,
            update_notes: Win::note_list(),
            content_stack: gtk::Stack::new(),
            toasts: adw::ToastOverlay::new(),
            split: adw::OverlaySplitView::new(),
            thumb_pool,
            shared: RefCell::new(shared),
            shell: RefCell::default(),
            packages: RefCell::default(),
            store: RefCell::default(),
            store_actions: RefCell::default(),
            plugins: RefCell::default(),
            switchboard: RefCell::default(),
            history: RefCell::default(),
            ai: RefCell::default(),
            preferences: RefCell::default(),
            today: RefCell::default(),
            actions: RefCell::default(),
        };
        let win = Win(Rc::new(inner));
        win.init();
        win
    }

    /// The body of `__init__` once the shared widgets exist.
    fn init(&self) {
        let display = WidgetExt::display(&self.window);
        let provider = gtk::CssProvider::new();
        provider.load_from_string(CSS);
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );

        // The Omarchy palette is applied over our own stylesheet, because it
        // redefines the named colours our stylesheet is written against.
        gtk::style_context_add_provider_for_display(
            &display,
            &self.theme_provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
        );
        // Where this raises the Python's window is never built; here it is
        // logged and GTK's own theme shows.
        if let Err(e) = self.apply_theme() {
            eprintln!("almanac: {e}");
        }
        let w = self.downgrade();
        let monitors = crate::theming::watch(move || {
            if let Some(w) = w.upgrade() {
                w.on_theme_changed();
            }
        });
        self.shell.borrow_mut().theme_monitors = monitors;
        let w = self.downgrade();
        adw::StyleManager::default().connect_dark_notify(move |_| {
            let w = w.clone();
            glib::idle_add_local_once(move || {
                if let Some(w) = w.upgrade() {
                    if let Err(e) = w.apply_theme() {
                        eprintln!("almanac: {e}");
                    }
                }
            });
        });

        // filter_key = "all", query = "", the store, the image cache and pool
        // and the filter -> sort -> selection chain are made with `Shared`
        // and `Inner`. The filter's function is the packages section's.
        let filter = self.shared.borrow().filter.clone();
        let w = self.downgrade();
        filter.set_filter_func(move |obj| {
            let Some(w) = w.upgrade() else { return true };
            // Read in place: copying every row on every keystroke was most
            // of the filter's cost.
            match AppItem::of(obj) {
                Some(item) => w.r#match(&item),
                None => false,
            }
        });

        // --- sidebar: categories with live counts -------------------------
        // Ten filter chips and five header buttons was most of the window's
        // chrome. A sidebar holds the same navigation with the counts visible
        // at rest, which is the question being asked ("what needs me?").
        let sidebar_list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::Single).build();
        sidebar_list.add_css_class("navigation-sidebar");
        sidebar_list.add_css_class("almanac-sidebar");
        let w = self.downgrade();
        sidebar_list.connect_row_selected(move |list, row| {
            if let Some(w) = w.upgrade() {
                w.on_sidebar_selected(list, row);
            }
        });

        let mut sidebar_rows = Vec::new();
        let mut group_rows = Vec::new();
        let mut group_members = BTreeMap::new();
        for (gkey, glabel, items) in GROUPS {
            let Some(glabel) = glabel else {
                let mut members = Vec::new();
                for (key, label, icon) in items.iter() {
                    let entry = sidebar_entry(key, label, icon, gkey);
                    sidebar_list.append(&entry.row);
                    sidebar_rows.push(entry.clone());
                    members.push(entry);
                }
                group_rows.push(((*gkey).to_string(), None));
                group_members.insert((*gkey).to_string(), members);
                continue;
            };

            let header = gtk::ListBoxRow::builder().selectable(false).activatable(true).build();
            let hb = gtk::Box::builder()
                .spacing(6)
                .margin_start(6)
                .margin_top(6)
                .margin_bottom(2)
                .margin_end(6)
                .build();
            let chevron = gtk::Image::from_icon_name("pan-down-symbolic");
            chevron.add_css_class("dim-label");
            let title = gtk::Label::builder()
                .label(tr(glabel).to_uppercase())
                .xalign(0.0)
                .hexpand(true)
                .build();
            title.add_css_class("caption");
            title.add_css_class("dim-label");
            hb.append(&title);
            hb.append(&chevron);
            header.set_child(Some(&hb));
            // A single-item group has nothing to fold away; its header is a
            // label, not a control.
            header.set_activatable(items.len() > 1);
            sidebar_list.append(&header);
            group_rows.push((
                (*gkey).to_string(),
                Some(GroupHeader { group: (*gkey).to_string(), row: header, chevron }),
            ));

            let mut members = Vec::new();
            for (key, label, icon) in items.iter() {
                let entry = sidebar_entry(key, label, icon, gkey);
                sidebar_list.append(&entry.row);
                sidebar_rows.push(entry.clone());
                members.push(entry);
            }
            group_members.insert((*gkey).to_string(), members);
        }
        {
            let mut sh = self.shared.borrow_mut();
            sh.sidebar_rows = sidebar_rows;
            sh.group_rows = group_rows;
            sh.group_members = group_members;
        }

        let w = self.downgrade();
        sidebar_list.connect_row_activated(move |list, row| {
            if let Some(w) = w.upgrade() {
                w.on_sidebar_activated(list, row);
            }
        });
        self.shared.borrow_mut().collapsed =
            COLLAPSED_BY_DEFAULT.iter().map(|s| (*s).to_string()).collect();

        let sidebar_scroll = gtk::ScrolledWindow::builder().vexpand(true).build();
        sidebar_scroll.set_child(Some(&sidebar_list));
        sidebar_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);

        // A stats footer. The sidebar's bottom half is empty on any normal
        // window height, and these are the numbers you would otherwise open a
        // terminal for.
        // Two columns: six numbers in three lines, not six. Each keeps its
        // explanation as a tooltip.
        self.stats.add_css_class("stats");
        sidebar_scroll.set_vexpand(true);

        let sidebar_page = adw::ToolbarView::new();
        let sidebar_header = adw::HeaderBar::new();
        // The app's own mark, where someone actually looks at it. It was
        // registered with the desktop file and the window manager and so
        // appeared only in a launcher and an alt-tab switcher.
        let brand = gtk::Box::builder().spacing(8).halign(gtk::Align::Center).build();
        let mark = gtk::Image::builder()
            .icon_name("io.github.twodxme.Almanac")
            .pixel_size(18)
            .build();
        let wordmark = gtk::Label::new(Some(&tr("Almanac")));
        wordmark.add_css_class("heading");
        brand.append(&mark);
        brand.append(&wordmark);
        // Clicking an app's own mark opens its About, as it does in most
        // desktop apps; the menu entry stays for keyboard users.
        let brand_btn = gtk::Button::builder()
            .child(&brand)
            .tooltip_text(tr("About Almanac"))
            .build();
        brand_btn.add_css_class("flat");
        brand_btn.update_property(&[gtk::accessible::Property::Label("About Almanac")]);
        let w = self.downgrade();
        brand_btn.connect_clicked(move |_| {
            if let Some(w) = w.upgrade() {
                w.on_about();
            }
        });
        sidebar_header.set_title_widget(Some(&brand_btn));
        sidebar_header.set_show_end_title_buttons(false);
        sidebar_page.add_top_bar(&sidebar_header);
        let sidebar_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        sidebar_box.append(&sidebar_scroll);
        sidebar_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        sidebar_box.append(&self.stats);
        sidebar_page.set_content(Some(&sidebar_box));

        // --- content header: one primary action, everything else in a menu --
        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();

        self.sidebar_toggle.set_active(true);
        name(&self.sidebar_toggle, &tr("Show categories"));
        let w = self.downgrade();
        self.sidebar_toggle.connect_toggled(move |b| {
            if let Some(w) = w.upgrade() {
                w.split.set_show_sidebar(b.is_active());
            }
        });
        header.pack_start(&self.sidebar_toggle);

        // The header names the page and summarises it. Previously the search
        // entry was the title widget, so no page had a name and the four
        // status labels were the only thing describing what you were looking
        // at -- each in a different place on the page.
        let title_box = gtk::Box::builder().spacing(8).halign(gtk::Align::Center).build();
        self.busy.set_visible(false);
        title_box.append(&self.busy);
        title_box.append(&self.title);
        header.set_title_widget(Some(&title_box));

        self.search.set_hexpand(true);
        let w = self.downgrade();
        self.search.connect_search_changed(move |entry| {
            if let Some(w) = w.upgrade() {
                w.on_search(entry);
            }
        });

        // One primary action, always in the same place. Each page says what
        // it is; before this, "Update all" lived in the header while the AI
        // and Maintenance pages had their own button in their own toolbar, in
        // a different position with different alignment.
        self.update_btn.add_css_class("suggested-action");
        let w = self.downgrade();
        self.update_btn.connect_clicked(move |b| {
            if let Some(w) = w.upgrade() {
                w.on_primary(b);
            }
        });
        header.pack_end(&self.update_btn);
        self.shared.borrow_mut().primary = None;

        let menu = gio::Menu::new();
        let actions = gio::Menu::new();
        actions.append(Some(&tr("Install what’s in the search box…")), Some("win.install"));
        actions.append(Some(&tr("Install from a link…")), Some("win.fromlink"));
        actions.append(Some(&tr("Track an AppImage file…")), Some("win.track"));
        menu.append_section(None, &actions);
        let maintenance = gio::Menu::new();
        maintenance.append(Some(&tr("Review orphaned packages…")), Some("win.clean"));
        maintenance.append(Some(&tr("Update plugins…")), Some("win.updateplugins"));
        if crate::util::which("omarchy-restart-shell").is_some() {
            maintenance.append(Some(&tr("Restart the shell…")), Some("win.restartshell"));
        }
        maintenance.append(Some(&tr("Refresh this page")), Some("win.rescan"));
        menu.append_section(None, &maintenance);
        let settings_menu = gio::Menu::new();
        settings_menu.append(Some(&tr("Preferences…")), Some("win.prefs"));
        menu.append_section(None, &settings_menu);
        let about_section = gio::Menu::new();
        about_section.append(Some(&tr("Update Almanac…")), Some("win.selfupdate"));
        about_section.append(Some(&tr("About Almanac")), Some("win.about"));
        menu.append_section(None, &about_section);

        // SORTS_INSTALLED / SORTS_DISCOVER: see sorts_installed().
        self.rebuild_sort_menu(&sorts_installed());
        name(&self.sort_btn, &tr("Sort by"));
        header.pack_end(&self.sort_btn);

        // The store has no second button: the same control in the same place
        // opens a sort menu on the package list and a filter drawer in the
        // store. Two adjacent buttons that both mean "options" is the thing
        // this is avoiding. (`filter_btn` is `sort_btn`; see `Win::new`.)

        let sort_action = gio::SimpleAction::new_stateful(
            "sort",
            Some(glib::VariantTy::STRING),
            &"default".to_variant(),
        );
        let w = self.downgrade();
        sort_action.connect_activate(move |action, param| {
            if let Some(w) = w.upgrade() {
                w.on_sort(action, param);
            }
        });
        self.window.add_action(&sort_action);

        let menu_btn = gtk::MenuButton::builder().icon_name("open-menu-symbolic").build();
        menu_btn.set_menu_model(Some(&menu));
        name(&menu_btn, &tr("More actions"));
        header.pack_end(&menu_btn);

        let handlers: [(&str, fn(&Win)); 10] = [
            ("install", |w| w.on_install()),
            ("track", |w| w.on_add_appimage()),
            // on_install_link(None): no link yet, so it asks for one.
            ("fromlink", |w| w.on_install_link("")),
            ("updateplugins", |w| w.on_update_plugins()),
            ("restartshell", |w| w.on_restart_shell()),
            ("clean", |w| w.on_clean_orphans()),
            ("prefs", |w| w.on_preferences()),
            ("selfupdate", |w| w.on_self_update()),
            ("about", |w| w.on_about()),
            ("rescan", |w| w.refresh_page()),
        ];
        for (act_name, f) in handlers {
            let act = gio::SimpleAction::new(act_name, None);
            let w = self.downgrade();
            act.connect_activate(move |_, _| {
                if let Some(w) = w.upgrade() {
                    f(&w);
                }
            });
            self.window.add_action(&act);
        }

        toolbar.add_top_bar(&header);

        // One search box for the whole window, in one place on every page,
        // rather than a title-bar widget that silently does nothing on two of
        // the four pages.
        self.search_bar.append(&self.search);
        toolbar.add_top_bar(&self.search_bar);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);

        // The way back from the repo/AUR search, which the sidebar cannot
        // offer: App Store is still the selected row, so re-selecting it
        // emits nothing.
        self.store_back.add_css_class("flat");
        self.store_back.set_tooltip_text(Some(&tr("Return to the plugin marketplace")));
        self.store_back.set_visible(false);
        let w = self.downgrade();
        self.store_back.connect_clicked(move |_| {
            if let Some(w) = w.upgrade() {
                w.store_return();
            }
        });
        content.append(&self.store_back);

        self.view.set_single_click_activate(true);
        let w = self.downgrade();
        self.view.connect_activate(move |view, position| {
            if let Some(w) = w.upgrade() {
                w.on_row_activated(view, position);
            }
        });
        self.view.set_show_row_separators(true);
        self.view.set_show_column_separators(false);
        self.build_columns();
        // Clicking a column header drives the sort.
        // A MultiSorter rather than swapping sorters: with no column selected
        // the column sorter reports every pair equal, so the priority sorter
        // decides; once a header is clicked it takes precedence and priority
        // becomes the tiebreak. Swapping on "changed" instead fired during
        // construction and silently replaced the default ordering.
        // (`priority_sorter` is made in `Win::new`.)
        self.shared.borrow_mut().manual_sorter = None;
        self.rebuild_sorter();

        let scroller = gtk::ScrolledWindow::builder().vexpand(true).build();
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroller.set_child(Some(&self.view));
        // The table sits on a card like every other page's lists -- same
        // background, corners and outline -- rather than on the darker view
        // background, edge to edge. One look for the whole app.
        self.view.add_css_class("almanac-table");
        scroller.add_css_class("card");
        scroller.set_overflow(gtk::Overflow::Hidden);
        // As tall as its rows, like a boxed list -- not a card running to the
        // bottom of the window under five updates. Past the window's height
        // it scrolls as before.
        scroller.set_propagate_natural_height(true);
        scroller.set_valign(gtk::Align::Start);
        scroller.set_margin_start(12);
        scroller.set_margin_end(12);
        scroller.set_margin_bottom(12);

        // A boxed list on the same 12px margins as the table's card, shown
        // only on the Updates page and only when it has something to say.
        self.update_notes.set_margin_start(12);
        self.update_notes.set_margin_end(12);
        self.update_notes.set_margin_bottom(12);
        self.update_notes.set_visible(false);
        content.append(&self.update_notes);

        self.list_stack.set_vexpand(true);
        self.list_stack.add_named(&scroller, Some("list"));
        self.list_stack.add_named(&self.list_empty, Some("empty"));
        content.append(&self.list_stack);

        self.content_stack.set_hhomogeneous(false);
        self.content_stack.set_vhomogeneous(false);
        self.content_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        self.content_stack.add_named(&content, Some("list"));
        self.content_stack.add_named(&self.build_store(), Some("store"));
        self.content_stack.add_named(&self.build_maintenance(), Some("maintenance"));
        self.content_stack.add_named(&self.build_history(), Some("history"));
        self.content_stack.add_named(&self.build_plugins_page(), Some("plugins"));
        self.content_stack.add_named(&self.build_switchboard(), Some("switchboard"));
        self.content_stack.add_named(&self.build_ai(), Some("ai"));
        self.content_stack.add_named(&self.build_featured(), Some("featured"));
        self.content_stack.set_visible_child_name("list");

        // Results and failures appear as toasts. They used to go to whichever
        // of four dim status labels the page happened to own, which is where
        // "Could not remove X" went to die.
        self.toasts.set_child(Some(&self.content_stack));
        toolbar.set_content(Some(&self.toasts));

        self.split.set_sidebar(Some(&sidebar_page));
        self.split.set_content(Some(&toolbar));
        self.split.set_sidebar_width_fraction(0.24);
        self.split.set_min_sidebar_width(205.0);
        self.split.set_max_sidebar_width(260.0);
        self.window.set_content(Some(&self.split));

        // Below ~760px the sidebar costs more than it gives: 205 of 480 pixels
        // is most of a half-screen tile. It folds into an overlay instead, and
        // the toggle in the header becomes the way back to it.
        if let Ok(condition) = adw::BreakpointCondition::parse("max-width: 760px") {
            let breakpoint = adw::Breakpoint::new(condition);
            breakpoint.add_setter(&self.split, "collapsed", Some(&true.to_value()));
            let w = self.downgrade();
            breakpoint.connect_apply(move |_| {
                if let Some(w) = w.upgrade() {
                    w.set_narrow(true);
                }
            });
            let w = self.downgrade();
            breakpoint.connect_unapply(move |_| {
                if let Some(w) = w.upgrade() {
                    w.set_narrow(false);
                }
            });
            self.window.add_breakpoint(breakpoint);
        }
        self.shared.borrow_mut().narrow = false;

        // Row-menu actions resolve by name at activation time, because the
        // row widgets themselves are recycled as the list scrolls. The
        // marketplace, findings, survey and shelves start empty (`Shared`).
        let group = gio::SimpleActionGroup::new();
        let row_handlers: [(&str, fn(&Win, &str)); 10] = [
            ("visit", |w, n| w.row_visit(n)),
            ("remove", |w, n| w.row_remove(n)),
            ("notes", |w, n| w.row_notes(n)),
            ("aur", |w, n| w.row_aur(n)),
            ("folder", |w, n| w.row_folder(n)),
            ("changes", |w, n| w.row_changes(n)),
            ("pull", |w, n| w.row_pull(n)),
            ("stash", |w, n| w.row_stash(n)),
            ("toggle", |w, n| w.row_toggle(n)),
            ("shortcut", |w, n| w.row_shortcut(n)),
        ];
        for (act_name, handler) in row_handlers {
            let a = gio::SimpleAction::new(act_name, Some(glib::VariantTy::STRING));
            let w = self.downgrade();
            a.connect_activate(move |_, param| {
                let arg = param.and_then(|p| p.str().map(str::to_string)).unwrap_or_default();
                if let Some(w) = w.upgrade() {
                    handler(&w, &arg);
                }
            });
            group.add_action(&a);
        }
        self.window.insert_action_group("row", Some(&group));
        let pgroup = gio::SimpleActionGroup::new();
        for act_name in ["update", "shortcut", "folder", "page", "remove"] {
            let a = gio::SimpleAction::new(act_name, Some(glib::VariantTy::STRING));
            let w = self.downgrade();
            a.connect_activate(move |_, param| {
                let arg = param.and_then(|p| p.str().map(str::to_string)).unwrap_or_default();
                if let Some(w) = w.upgrade() {
                    w.plugin_action(act_name, &arg);
                }
            });
            pgroup.add_action(&a);
        }
        self.window.insert_action_group("plugin", Some(&pgroup));

        self.apply_all_group_states();
        self.install_shortcuts();
        self.restore_state();
        // Land on a category, always: an unselected sidebar shows an empty
        // list that reads as a bug.
        let (rows, filter_key) = {
            let sh = self.shared.borrow();
            let rows: Vec<adw::ActionRow> = sh.sidebar_rows.iter().map(|r| r.row.clone()).collect();
            (rows, sh.filter_key.clone())
        };
        if !rows.is_empty() && !rows.iter().any(|r| r.is_selected()) {
            let key = if filter_key == "all" { "featured".to_string() } else { filter_key };
            self.select_category(&key);
        }
        self.watch_pacman();
        self.watch_configs();
        self.reload();
    }

    pub fn downgrade(&self) -> WeakWin {
        WeakWin(Rc::downgrade(&self.0))
    }

    /// `CATEGORIES`: every sidebar destination, in order. Labels are msgids.
    pub fn categories(&self) -> Vec<GroupItem> {
        GROUPS.iter().flat_map(|(_k, _l, items)| items.iter().copied()).collect()
    }

    /// `self._thumb_pool.submit(...)`: see `thumb_bg`.
    pub fn thumb_bg<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
        done: impl FnOnce(T) + 'static,
    ) {
        match &self.thumb_pool {
            Some(pool) => thumb_bg(pool, work, done),
            None => spawn_bg(work, done),
        }
    }

    // ---- columns ---------------------------------------------------------

    /// `_column`: append one column to the table.
    ///
    /// `fixed` is the STARTING width (0 for none); `resizable` lets the user
    /// drag the header edge from there.
    #[allow(clippy::too_many_arguments)]
    pub fn column(
        &self,
        title: &str,
        setup: impl Fn(&gtk::SignalListItemFactory, &glib::Object) + 'static,
        bind: impl Fn(&gtk::SignalListItemFactory, &glib::Object) + 'static,
        sorter: Option<gtk::Sorter>,
        expand: bool,
        fixed: i32,
        resizable: bool,
    ) -> gtk::ColumnViewColumn {
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(setup);
        factory.connect_bind(bind);
        let col = gtk::ColumnViewColumn::new(Some(title), Some(factory));
        col.set_expand(expand);
        if fixed != 0 {
            // fixed_width is the STARTING width; resizable lets the user drag
            // the header edge from there.
            col.set_fixed_width(fixed);
        }
        col.set_resizable(resizable);
        if let Some(s) = sorter {
            col.set_sorter(Some(&s));
        }
        self.view.append_column(&col);
        col
    }

    /// `_build_columns`: Name, Version, Source and the action buttons.
    pub fn build_columns(&self) {
        fn num_sorter(attr: fn(&AppItem) -> i64) -> gtk::Sorter {
            gtk::CustomSorter::new(move |a, b| match (AppItem::of(a), AppItem::of(b)) {
                (Some(a), Some(b)) => attr(&a).cmp(&attr(&b)).into(),
                _ => gtk::Ordering::Equal,
            })
            .upcast()
        }

        fn str_sorter(attr: fn(&AppItem) -> &str) -> gtk::Sorter {
            gtk::CustomSorter::new(move |a, b| {
                let (Some(a), Some(b)) = (AppItem::of(a), AppItem::of(b)) else {
                    return gtk::Ordering::Equal;
                };
                let (x, y) = (attr(&a), attr(&b));
                // Blanks last in either direction: an empty category is missing
                // data, not a value that deserves the top of the list.
                if x.is_empty() && !y.is_empty() {
                    return gtk::Ordering::Larger;
                }
                if !x.is_empty() && y.is_empty() {
                    return gtk::Ordering::Smaller;
                }
                lower_cmp(x, y).into()
            })
            .upcast()
        }

        // --- Name: icon + name + description ---
        let setup_name = |_f: &gtk::SignalListItemFactory, obj: &glib::Object| {
            let Some(item) = obj.downcast_ref::<gtk::ListItem>() else { return };
            let row = gtk::Box::builder().spacing(8).margin_top(4).margin_bottom(4).build();
            let img = gtk::Image::new();
            img.set_pixel_size(32);
            img.set_valign(gtk::Align::Center);
            let text = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .valign(gtk::Align::Center)
                .hexpand(true)
                .build();
            let title_row = gtk::Box::builder().spacing(6).build();
            let name_lbl = gtk::Label::builder().xalign(0.0).hexpand(true).build();
            name_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
            let pill = gtk::Label::builder().hexpand(false).build();
            pill.set_halign(gtk::Align::Start);
            pill.add_css_class("pill");
            pill.set_valign(gtk::Align::Center);
            pill.set_visible(false);
            title_row.append(&name_lbl);
            title_row.append(&pill);
            let desc = gtk::Label::builder().xalign(0.0).build();
            desc.add_css_class("dim-label");
            desc.add_css_class("caption");
            desc.set_ellipsize(gtk::pango::EllipsizeMode::End);
            text.append(&title_row);
            text.append(&desc);
            row.append(&img);
            row.append(&text);
            item.set_child(Some(&row));
        };

        let w = self.downgrade();
        let bind_name = move |_f: &gtk::SignalListItemFactory, obj: &glib::Object| {
            let Some(item) = obj.downcast_ref::<gtk::ListItem>() else { return };
            let Some(it) = item.item().and_then(|o| AppItem::of(&o).map(|i| i.clone())) else {
                return;
            };
            let Some(row) = item.child() else { return };
            let Some(img) = row.first_child().and_downcast::<gtk::Image>() else { return };
            let Some(text) = img.next_sibling() else { return };
            let Some(title_row) = text.first_child() else { return };
            let Some(desc_lbl) = title_row.next_sibling().and_downcast::<gtk::Label>() else {
                return;
            };
            let Some(name_lbl) = title_row.first_child().and_downcast::<gtk::Label>() else {
                return;
            };
            let Some(pill) = name_lbl.next_sibling().and_downcast::<gtk::Label>() else { return };

            name_lbl.set_text(&it.name);
            let installed = if it.installed_ts != 0.0 {
                format!("installed {}", age(it.installed_ts))
            } else {
                String::new()
            };
            let bits: Vec<String> = [it.category.clone(), fmt_size(it.size_mb), installed]
                .into_iter()
                .filter(|b| !b.is_empty())
                .collect();
            let desc: String = it.app.description.chars().take(70).collect();
            let meta = bits.join(" · ");
            desc_lbl.set_text(&if !desc.is_empty() && !meta.is_empty() {
                format!("{desc}  —  {meta}")
            } else if !desc.is_empty() {
                desc
            } else {
                meta
            });
            // Only ever shown on a row that actually has an update pending --
            // labelling an up-to-date browser "security" would be noise.
            for css in ["pill-crit", "pill-warn", "pill-ok"] {
                pill.remove_css_class(css);
            }
            if it.git_dirty {
                pill.set_text("local edits");
                pill.add_css_class("pill-warn");
                pill.set_tooltip_text(Some(&tr(
                    "This checkout has uncommitted changes. Pulling upstream \
                     would overwrite them — commit or stash first.",
                )));
                pill.set_visible(true);
            } else if it.source == inventory::SYSTEM && it.has_update {
                pill.set_text("system");
                pill.add_css_class("pill-warn");
                pill.set_tooltip_text(Some(&tr(
                    "Omarchy itself — update with the whole system (Update all), never on its own",
                )));
                pill.set_visible(true);
            } else if it.source == inventory::ORPHAN {
                let keep = it.advice == "keep";
                pill.set_text(if keep { "keep" } else { "safe to remove" });
                pill.add_css_class("pill-warn");
                pill.set_tooltip_text(Some(&if keep {
                    tr("Build tooling or a debug package — the next AUR build will want it back")
                } else {
                    tr("Nothing requires this any more")
                }));
                pill.set_visible(true);
            } else if !it.stale.is_empty() && it.critical.is_empty() {
                let (label, tip) = match it.stale.as_str() {
                    "orphaned" => (
                        "unmaintained",
                        "No maintainer in the AUR — nobody is packaging this any more".to_string(),
                    ),
                    "flagged" => (
                        "out of date",
                        "The AUR maintainer has flagged this as out of date".to_string(),
                    ),
                    "stale" => (
                        "stale",
                        format!(
                            "No new build in over {} months — upstream may have stopped",
                            inventory::STALE_DAYS.div_euclid(30)
                        ),
                    ),
                    // The Python's dict lookup raises on anything else; an
                    // unknown reason shows no pill rather than a crash.
                    _ => {
                        pill.set_visible(false);
                        return;
                    }
                };
                pill.set_text(label);
                pill.add_css_class("pill-warn");
                pill.set_tooltip_text(Some(&tip));
                pill.set_visible(true);
            } else if it.critical == "security" {
                pill.set_text("security");
                pill.set_tooltip_text(Some(&tr(
                    "Network-facing or credential-handling: a stale version is exposure",
                )));
                pill.set_visible(true);
            } else if it.critical == "core" {
                pill.set_text("core");
                pill.add_css_class("pill-warn");
                pill.set_tooltip_text(Some(&tr(
                    "Touches boot or needs a reboot — update with the full system, not alone",
                )));
                pill.set_visible(true);
            } else {
                pill.set_visible(false);
            }

            img.remove_css_class("dim-label");
            if let Some(w) = w.upgrade() {
                if !w.app_icon(&img, &it.app, 32) {
                    img.add_css_class("dim-label");
                }
            }
        };

        self.column("Name", setup_name, bind_name, Some(str_sorter(|i| &i.name)), true, 0, true);

        // --- plain text columns ---
        fn make_text(
            getter: fn(&AppItem) -> String,
            dim: bool,
            align: f32,
            accent_on_update: bool,
        ) -> (
            impl Fn(&gtk::SignalListItemFactory, &glib::Object) + 'static,
            impl Fn(&gtk::SignalListItemFactory, &glib::Object) + 'static,
        ) {
            let setup = move |_f: &gtk::SignalListItemFactory, obj: &glib::Object| {
                let Some(item) = obj.downcast_ref::<gtk::ListItem>() else { return };
                let lbl = gtk::Label::builder().xalign(align).build();
                lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
                lbl.add_css_class("caption");
                item.set_child(Some(&lbl));
            };
            let bind = move |_f: &gtk::SignalListItemFactory, obj: &glib::Object| {
                let Some(item) = obj.downcast_ref::<gtk::ListItem>() else { return };
                let Some(lbl) = item.child().and_downcast::<gtk::Label>() else { return };
                let Some((text, has_update)) = item
                    .item()
                    .and_then(|o| AppItem::of(&o).map(|it| (getter(&it), it.has_update)))
                else {
                    return;
                };
                lbl.set_text(&text);
                lbl.remove_css_class("accent");
                lbl.remove_css_class("dim-label");
                if accent_on_update && has_update {
                    // The one colour in the table: a pending update is the most
                    // actionable fact in the row.
                    lbl.add_css_class("accent");
                } else if dim {
                    lbl.add_css_class("dim-label");
                }
            };
            (setup, bind)
        }

        let (s, b) = make_text(version_text, true, 1.0, true);
        self.column("Version", s, b, Some(num_sorter(|i| i.behind)), false, 170, true);

        // Rows are recycled as the list scrolls, so a stale handler would
        // fire for whichever item later lands in this widget. Python
        // disconnects and reconnects on every bind; here each button has one
        // handler that runs whatever its current slot holds.
        let slots = ClickSlots::default();

        let setup_source = {
            let slots = slots.clone();
            move |_f: &gtk::SignalListItemFactory, obj: &glib::Object| {
                let Some(item) = obj.downcast_ref::<gtk::ListItem>() else { return };
                // A LinkButton would underline permanently and steal the row's
                // styling; a flat button reads as a label until hovered.
                let btn = gtk::Button::new();
                btn.add_css_class("flat");
                btn.add_css_class("caption");
                btn.set_halign(gtk::Align::Start);
                btn.set_valign(gtk::Align::Center);
                slots.attach(&btn);
                item.set_child(Some(&btn));
            }
        };

        let w = self.downgrade();
        let bind_source = {
            let slots = slots.clone();
            move |_f: &gtk::SignalListItemFactory, obj: &glib::Object| {
                let Some(item) = obj.downcast_ref::<gtk::ListItem>() else { return };
                let Some(btn) = item.child().and_downcast::<gtk::Button>() else { return };
                let Some((source, url)) = item.item().and_then(|o| {
                    AppItem::of(&o).map(|it| (it.source.clone(), as_text(it.app.extra.get("url"))))
                }) else {
                    return;
                };
                btn.set_label(&source_label(&source));
                slots.set(&btn, None);
                if !url.is_empty() {
                    btn.set_tooltip_text(Some(&format!("Open {url}")));
                    btn.set_sensitive(true);
                    btn.add_css_class("link-ish");
                    let w = w.clone();
                    slots.set(
                        &btn,
                        Some(Rc::new(move |_b: &gtk::Button| {
                            if let Some(w) = w.upgrade() {
                                w.open_url(&url);
                            }
                        })),
                    );
                } else {
                    // Local builds and AppImages often have nowhere to point.
                    btn.set_tooltip_text(Some(&tr("No upstream URL recorded")));
                    btn.set_sensitive(false);
                    btn.remove_css_class("link-ish");
                }
            }
        };

        self.column("Source", setup_source, bind_source, Some(str_sorter(|i| &i.source)), false, 74, true);

        // --- launch button ---
        let setup_act = {
            let slots = slots.clone();
            move |_f: &gtk::SignalListItemFactory, obj: &glib::Object| {
                let Some(item) = obj.downcast_ref::<gtk::ListItem>() else { return };
                let row = gtk::Box::builder()
                    .spacing(2)
                    .halign(gtk::Align::End)
                    .valign(gtk::Align::Center)
                    .build();
                let upd = gtk::Button::from_icon_name("software-update-available-symbolic");
                upd.add_css_class("flat");
                upd.add_css_class("circular");
                let play = gtk::Button::from_icon_name("media-playback-start-symbolic");
                play.add_css_class("flat");
                let menu = gtk::MenuButton::builder().icon_name("view-more-symbolic").build();
                menu.add_css_class("flat");
                slots.attach(&upd);
                slots.attach(&play);
                row.append(&upd);
                row.append(&play);
                row.append(&menu);
                item.set_child(Some(&row));
            }
        };

        let w = self.downgrade();
        let bind_act = {
            let slots = slots.clone();
            move |_f: &gtk::SignalListItemFactory, obj: &glib::Object| {
                let Some(item) = obj.downcast_ref::<gtk::ListItem>() else { return };
                let Some(it) = item.item().and_then(|o| AppItem::of(&o).map(|i| i.clone())) else {
                    return;
                };
                let Some(row) = item.child() else { return };
                let Some(upd) = row.first_child().and_downcast::<gtk::Button>() else { return };
                let Some(play) = upd.next_sibling().and_downcast::<gtk::Button>() else { return };
                let Some(menu) = play.next_sibling().and_downcast::<gtk::MenuButton>() else {
                    return;
                };
                let Some(win) = w.upgrade() else { return };
                name(&menu, &format!("More actions for {}", it.name));

                // Rows are recycled as the list scrolls, so a stale handler would
                // fire for whichever item later lands in this widget.
                slots.set(&upd, None);
                slots.set(&play, None);

                let extra = &it.app.extra;
                if it.source == inventory::AVAILABLE || it.source == inventory::PLUGIN {
                    let already = truthy(extra.get("already"));
                    upd.set_visible(!already);
                    play.set_visible(false);
                    menu.set_visible(truthy(extra.get("url")));
                    if !already {
                        upd.remove_css_class("error");
                        upd.add_css_class("accent");
                        upd.set_icon_name(INSTALL_ICON);
                        let suffix = if it.source == inventory::PLUGIN {
                            tr(" (Omarchy plugin)")
                        } else {
                            String::new()
                        };
                        name(&upd, &format!("Install {}{}", it.name, suffix));
                        let (w, app) = (w.clone(), it.app.clone());
                        slots.set(
                            &upd,
                            Some(Rc::new(move |_b: &gtk::Button| {
                                if let Some(w) = w.upgrade() {
                                    w.on_install_item(&app);
                                }
                            })),
                        );
                    }
                    if truthy(extra.get("url")) {
                        let m = gio::Menu::new();
                        m.append(Some("Open repository"), Some(&format!("row.visit::{}", it.name)));
                        menu.set_menu_model(Some(&m));
                        win.shared.borrow_mut().row_actions.insert(it.name.clone(), it.app.clone());
                    }
                    return;
                }

                upd.set_icon_name("software-update-available-symbolic");
                let mut can_update = it.has_update
                    && [inventory::REPO, inventory::AUR, inventory::APPIMAGE, inventory::TOOL]
                        .contains(&it.source.as_str());
                if it.source == inventory::SYSTEM {
                    // Almanac is a SYSTEM entry but is the one thing here that
                    // can genuinely update itself.
                    can_update = truthy(extra.get("self")) && it.has_update;
                } else if it.source == inventory::LOCAL {
                    // Never offer a pull over uncommitted work.
                    can_update = it.git_behind != 0 && !it.git_dirty;
                }
                upd.set_visible(can_update);
                if can_update {
                    name(
                        &upd,
                        &format!(
                            "Update {} alone, in a terminal.\nArch discourages partial upgrades: \
                             prefer Update all unless you need just this one.",
                            it.name
                        ),
                    );
                    // Colour only where it earns attention: a security update is
                    // worth a red glyph, routine ones stay quiet.
                    upd.remove_css_class("error");
                    upd.remove_css_class("accent");
                    upd.add_css_class(if it.critical == "security" { "error" } else { "accent" });
                    let (w, app) = (w.clone(), it.app.clone());
                    let handler: Rc<dyn Fn(&gtk::Button)> = if truthy(extra.get("self")) {
                        name(
                            &upd,
                            &format!("Update Almanac itself — {}", as_text(extra.get("note"))),
                        );
                        Rc::new(move |_b| {
                            if let Some(w) = w.upgrade() {
                                w.on_self_update();
                            }
                        })
                    } else if it.source == inventory::LOCAL {
                        let pid = as_text(extra.get("plugin_id"));
                        name(
                            &upd,
                            &if truthy(extra.get("plugin_id")) {
                                format!("omarchy plugin update {pid}")
                            } else {
                                format!(
                                    "git pull in {}\nLatest upstream: {}",
                                    as_text(extra.get("repo")),
                                    as_text(extra.get("git_subject"))
                                )
                            },
                        );
                        Rc::new(move |_b| {
                            if let Some(w) = w.upgrade() {
                                w.on_git_pull(&app);
                            }
                        })
                    } else if it.source == inventory::APPIMAGE {
                        name(
                            &upd,
                            &format!(
                                "Download {} {} and replace the current build (the old one is \
                                 kept as .previous)",
                                it.name, it.app.update
                            ),
                        );
                        Rc::new(move |b| {
                            if let Some(w) = w.upgrade() {
                                w.on_update_appimage(b, &app);
                            }
                        })
                    } else {
                        Rc::new(move |_b| {
                            if let Some(w) = w.upgrade() {
                                w.on_update_one(&app);
                            }
                        })
                    };
                    slots.set(&upd, Some(handler));
                }

                // extra.get("present", True)
                let present = extra.get("present").map(|v| truthy(Some(v))).unwrap_or(true);
                let missing = it.source == inventory::APPIMAGE && !present;
                let launchable = !it.app.exec_path.is_empty()
                    && (it.source == inventory::APPIMAGE || it.source == inventory::LOCAL);
                play.set_visible(launchable && !missing);
                if play.is_visible() {
                    name(&play, &format!("Launch {}", it.name));
                    let (w, app) = (w.clone(), it.app.clone());
                    slots.set(
                        &play,
                        Some(Rc::new(move |_b: &gtk::Button| {
                            if let Some(w) = w.upgrade() {
                                w.on_launch(&app);
                            }
                        })),
                    );
                }

                // Destructive actions sit behind a menu, never a bare button where
                // the pointer already is.
                let model = gio::Menu::new();
                let url = as_text(extra.get("url"));
                let n = &it.name;
                if !url.is_empty() {
                    model.append(Some("Open project page"), Some(&format!("row.visit::{n}")));
                }
                if it.has_update && url.contains("github.com/") {
                    model.append(Some("What’s new…"), Some(&format!("row.notes::{n}")));
                } else if it.source == inventory::AUR {
                    model.append(Some("View on the AUR"), Some(&format!("row.aur::{n}")));
                }
                if [inventory::REPO, inventory::AUR, inventory::APPIMAGE, inventory::FLATPAK]
                    .contains(&it.source.as_str())
                {
                    model.append(Some("Remove…"), Some(&format!("row.remove::{n}")));
                }

                // A local checkout with uncommitted work gets no pull button,
                // because the pull would be refused. Without these it got no
                // affordance at all and no explanation -- a row 209 commits
                // behind that simply does nothing.
                if it.source == inventory::LOCAL && truthy(extra.get("repo")) {
                    let pid = truthy(extra.get("plugin_id"));
                    let enabled = extra.get("enabled").filter(|v| !v.is_null());
                    if pid && enabled.is_some() {
                        model.append(
                            Some(&if truthy(enabled) { tr("Turn off") } else { tr("Turn on") }),
                            Some(&format!("row.toggle::{n}")),
                        );
                        // What has_panel raises ends this bind: the row
                        // keeps whatever menu it had.
                        let panel = crate::shortcuts::available()
                            && match crate::shortcuts::try_has_panel(std::path::Path::new(&as_text(
                                extra.get("repo"),
                            ))) {
                                Ok(panel) => panel,
                                Err(e) => {
                                    eprintln!("almanac: {e}");
                                    return;
                                }
                            };
                        if panel {
                            model.append(
                                Some(&tr("Keyboard shortcut…")),
                                Some(&format!("row.shortcut::{n}")),
                            );
                        }
                    }
                    model.append(Some("Open repository folder"), Some(&format!("row.folder::{n}")));
                    if it.git_dirty {
                        model.append(Some("Show local changes…"), Some(&format!("row.changes::{n}")));
                        if it.git_behind != 0 {
                            model.append(
                                Some("Stash changes and update…"),
                                Some(&format!("row.stash::{n}")),
                            );
                        }
                    } else if it.git_behind != 0 {
                        let label = if pid { tr("Update") } else { "Update (git pull)".to_string() };
                        model.append(Some(&label), Some(&format!("row.pull::{n}")));
                    }
                }

                let has_items = model.n_items() > 0;
                menu.set_menu_model(if has_items { Some(&model) } else { None });
                menu.set_visible(has_items);
                win.shared.borrow_mut().row_actions.insert(it.name.clone(), it.app.clone());
            }
        };

        self.column("", setup_act, bind_act, None, false, 104, false);
    }

    // ---- shortcuts and persisted state -----------------------------------

    /// `_install_shortcuts`: Ctrl+F, Ctrl+R/F5, Ctrl+U, Ctrl+W, Ctrl+comma,
    /// Escape, and Ctrl+1…Ctrl+0 for `PAGE_KEYS`.
    pub fn install_shortcuts(&self) {
        let controller = gtk::ShortcutController::new();
        controller.set_scope(gtk::ShortcutScope::Global);

        let add = |accel: &str, f: Rc<dyn Fn(&Win)>| {
            let w = self.downgrade();
            let action = gtk::CallbackAction::new(move |_, _| {
                if let Some(w) = w.upgrade() {
                    f(&w);
                }
                glib::Propagation::Stop
            });
            controller.add_shortcut(gtk::Shortcut::new(
                gtk::ShortcutTrigger::parse_string(accel),
                Some(action),
            ));
        };

        add("<Control>f", Rc::new(|w| {
            w.search.grab_focus();
        }));
        // Refresh means "re-read this page", the same as the menu item. It
        // used to always re-scan packages, whatever you were looking at.
        add("<Control>r", Rc::new(|w| w.refresh_page()));
        add("F5", Rc::new(|w| w.refresh_page()));
        add("<Control>u", Rc::new(|w| w.on_update_all()));
        add("<Control>w", Rc::new(|w| w.window.close()));
        add("<Control>comma", Rc::new(|w| w.on_preferences())); // the GNOME convention
        add("Escape", Rc::new(|w| w.clear_search()));

        // The primary destinations are reachable without the mouse, in the
        // order they appear in the sidebar: Ctrl+1 to Ctrl+9, then Ctrl+0.
        // There is no Ctrl+10 -- an eleventh page needs another scheme, and
        // PAGE_KEYS is checked by a test because an invalid accelerator makes
        // the window fail to open at all.
        for (i, key) in PAGE_KEYS.iter().enumerate() {
            let key = *key;
            add(&page_accel(i + 1), Rc::new(move |w| w.select_category(key)));
        }
        self.window.add_controller(controller);
    }

    /// `_clear_search`: Escape empties the search box, or, when it is already
    /// empty, drops the focus.
    pub fn clear_search(&self) {
        if !self.search.text().is_empty() {
            self.search.set_text("");
        } else {
            GtkWindowExt::set_focus(&self.window, None::<&gtk::Widget>);
        }
    }

    /// `_state_path`: window-state.json beside the AppImage registry.
    pub fn state_path(&self) -> PathBuf {
        let reg = inventory::registry();
        reg.parent().map(|p| p.to_path_buf()).unwrap_or_default().join("window-state.json")
    }

    /// Window size, chip and sort survive a restart -- otherwise every
    /// launch dumps you back on an unsorted "All".
    pub fn restore_state(&self) {
        let st = match std::fs::read_to_string(self.state_path())
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        {
            Some(Value::Object(st)) => st,
            _ => return,
        };
        // isinstance(x, int): an integral JSON number (a bool is an int too,
        // but never > 400).
        let int = |k: &str| st.get(k).and_then(|v| v.as_i64().or_else(|| v.as_bool().map(i64::from)));
        if let (Some(w), Some(h)) = (int("width"), int("height")) {
            if w > 400 && h > 300 {
                self.window.set_default_size(w as i32, h as i32);
            }
        }
        if let Some(Value::Array(groups)) = st.get("collapsed") {
            let collapsed: BTreeSet<String> = {
                let sh = self.shared.borrow();
                groups
                    .iter()
                    .filter_map(|g| g.as_str())
                    .filter(|g| sh.has_group(g))
                    .map(str::to_string)
                    .collect()
            };
            self.shared.borrow_mut().collapsed = collapsed;
            self.apply_all_group_states();
        }
        let mut chip = st.get("chip").and_then(|v| v.as_str()).map(str::to_string);
        let start = crate::settings::get_str("start_page");
        let known = |k: &str| self.shared.borrow().sidebar_row(k).is_some();
        if start != "last" && known(&start) {
            chip = Some(start);
        }
        if let Some(chip) = chip {
            if known(&chip) {
                self.select_category(&chip);
            }
        }
        let sort_col = st.get("sort_col").and_then(|v| v.as_str()).map(str::to_string);
        let sort_desc = truthy(st.get("sort_desc"));
        self.shell.borrow_mut().wanted_sort = Some((sort_col, sort_desc));
    }

    /// `_save_state`: write size, chip, sort and collapsed groups.
    pub fn save_state(&self) {
        let (mut col_title, mut desc) = (String::new(), false);
        if let Some(sorter) = self.view.sorter().and_downcast::<gtk::ColumnViewSorter>() {
            if let Some(c) = sorter.primary_sort_column() {
                col_title = c.title().map(|t| t.to_string()).unwrap_or_default();
                desc = sorter.primary_sort_order() == gtk::SortType::Descending;
            }
        }
        let (chip, collapsed) = {
            let sh = self.shared.borrow();
            (sh.filter_key.clone(), sh.collapsed.iter().cloned().collect::<Vec<_>>())
        };
        let mut st = Map::new();
        st.insert("width".into(), self.window.width().into());
        st.insert("height".into(), self.window.height().into());
        st.insert("chip".into(), chip.into());
        st.insert("sort_col".into(), col_title.into());
        st.insert("sort_desc".into(), desc.into());
        st.insert("collapsed".into(), collapsed.into());
        // Path.write_text: no directory is made, and any failure is dropped.
        let text = crate::util::dumps(&Value::Object(st), Some(2), false);
        let _ = std::fs::write(self.state_path(), text); // losing window state is not worth an error
    }

    // ---- external change detection ---------------------------------------

    /// Rescan when the package database changes, whoever changed it.
    ///
    /// pacman.log gets one line per transaction, so a single upgrade fires
    /// many events -- hence the debounce. Keeping a reference to the monitor
    /// matters: a GC'd FileMonitor silently stops delivering.
    pub fn watch_pacman(&self) {
        self.shell.borrow_mut().pending_rescan = None;
        let gfile = gio::File::for_path("/var/log/pacman.log");
        let monitor = match gfile.monitor_file(gio::FileMonitorFlags::NONE, None::<&gio::Cancellable>) {
            Ok(m) => {
                let w = self.downgrade();
                m.connect_changed(move |_, _, _, _| {
                    if let Some(w) = w.upgrade() {
                        w.on_pacman_changed();
                    }
                });
                Some(m)
            }
            Err(_) => None, // no watch is survivable; Rescan still works
        };
        self.shell.borrow_mut().monitor = monitor;
    }

    /// Notice the moment a tool resets a config file to its defaults.
    ///
    /// The Health check finds a reset whenever it runs; this makes it
    /// immediate while the window is open. Only the directories the
    /// distribution ships defaults for are watched, since nothing else can
    /// be "reset to defaults".
    pub fn watch_configs(&self) {
        {
            let mut st = self.shell.borrow_mut();
            st.config_monitors.clear();
            st.config_pending = None;
        }
        // Where `guard.resets()` raises the Python's window is never built;
        // here it is logged and nothing is watched.
        let resets = match crate::guard::try_resets_at(None, crate::guard::PyStack::WINDOW) {
            Ok(resets) => resets,
            Err(e) => {
                eprintln!("almanac: {e}");
                return;
            }
        };
        let reported: BTreeSet<String> = resets
            .iter()
            .map(|r| r.backup.to_string_lossy().into_owned())
            .collect();
        self.shell.borrow_mut().config_reported = reported;
        let Some(root) = crate::guard::defaults_root() else { return };
        let mut subs: Vec<PathBuf> = if root.is_dir() {
            std::fs::read_dir(&root)
                .map(|rd| rd.flatten().map(|e| e.path()).collect())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        subs.sort();
        for sub in subs {
            let Some(sub_name) = sub.file_name() else { continue };
            let target = crate::guard::config_home().join(sub_name);
            if !target.is_dir() {
                continue;
            }
            let mon = match gio::File::for_path(&target)
                .monitor_directory(gio::FileMonitorFlags::NONE, None::<&gio::Cancellable>)
            {
                Ok(m) => m,
                Err(_) => continue,
            };
            let w = self.downgrade();
            mon.connect_changed(move |_, file, _other, event| {
                if let Some(w) = w.upgrade() {
                    w.on_config_changed(file, event);
                }
            });
            self.shell.borrow_mut().config_monitors.push(mon);
        }
    }

    /// `_on_config_changed`: a CREATED backup file starts a 3 s debounce.
    pub fn on_config_changed(&self, file: &gio::File, event: gio::FileMonitorEvent) {
        if event != gio::FileMonitorEvent::Created {
            return;
        }
        let base = file.basename().map(|b| b.to_string_lossy().into_owned()).unwrap_or_default();
        if !crate::guard::BAK.is_match(&base) {
            return;
        }
        if let Some(old) = self.shell.borrow_mut().config_pending.take() {
            old.remove();
        }
        // The backup is written before the defaults are copied in; wait for both.
        let w = self.downgrade();
        let id = glib::timeout_add_seconds_local(3, move || match w.upgrade() {
            Some(w) => w.config_check(),
            None => glib::ControlFlow::Break,
        });
        self.shell.borrow_mut().config_pending = Some(id);
    }

    /// `_config_check`: look for resets off the main thread. One-shot timer.
    pub fn config_check(&self) -> glib::ControlFlow {
        self.shell.borrow_mut().config_pending = None;
        let w = self.downgrade();
        // What `guard.resets()` raises ends the Python's worker before its
        // idle_add: nothing is reported.
        spawn_bg(
            || crate::guard::try_resets_at(None, crate::guard::PyStack::WORKER),
            move |found| match found {
                Ok(found) => {
                    if let Some(w) = w.upgrade() {
                        w.config_report(found);
                    }
                }
                Err(e) => eprintln!("almanac: {e}"),
            },
        );
        glib::ControlFlow::Break
    }

    /// `_config_report`: announce resets not reported before.
    pub fn config_report(&self, found: Vec<crate::guard::Reset>) {
        let new: Vec<&crate::guard::Reset> = {
            let mut st = self.shell.borrow_mut();
            let new = found
                .iter()
                .filter(|r| !st.config_reported.contains(&*r.backup.to_string_lossy()))
                .collect();
            st.config_reported
                .extend(found.iter().map(|r| r.backup.to_string_lossy().into_owned()));
            new
        };
        if new.is_empty() {
            return;
        }
        let names = new.iter().map(|r| r.name()).collect::<Vec<_>>().join(", ");
        let lost: usize = new.iter().map(|r| r.lost.len()).sum();
        let body = format!(
            "{names} was replaced with the shipped defaults{}. Your version was kept — restore it \
             from Health check.",
            if lost != 0 { format!(", removing {lost} of your entries") } else { String::new() }
        );
        if crate::settings::get_bool("notify_resets") {
            self.notify(&tr("Your settings were reset"), &body, "critical");
        }
        self.say(&tr("Settings reset to defaults — see Health check"), 10);
        if self.content_stack.visible_child_name().as_deref() == Some("maintenance") {
            self.maint_reload();
        }
    }

    /// `_on_pacman_changed`: wait for the transaction to settle (6 s), then
    /// rescan once.
    pub fn on_pacman_changed(&self) {
        if let Some(old) = self.shell.borrow_mut().pending_rescan.take() {
            old.remove();
        }
        // Wait for the transaction to settle, then rescan once.
        let w = self.downgrade();
        let id = glib::timeout_add_seconds_local(6, move || match w.upgrade() {
            Some(w) => w.rescan_now(),
            None => glib::ControlFlow::Break,
        });
        self.shell.borrow_mut().pending_rescan = Some(id);
    }

    /// `_rescan_now`: the debounced rescan. One-shot timer.
    ///
    /// Skipped when a scan started after pacman.log last changed: the app's
    /// own actions rescan when they finish, and the watcher then saw the
    /// same transaction and asked for a second full scan six seconds later.
    pub fn rescan_now(&self) -> glib::ControlFlow {
        self.shell.borrow_mut().pending_rescan = None;
        let changed = std::fs::metadata(crate::snapshots::PACMAN_LOG).and_then(|m| m.modified());
        if changed.is_ok_and(|changed| self.scanned_since(changed)) {
            println!("pacman.log changed — already rescanned");
            return glib::ControlFlow::Break;
        }
        // Logged as well as shown, so the watch can be verified from a terminal
        // rather than only by staring at the window.
        println!("pacman.log changed — rescanning");
        self.summarize(&tr("Packages changed — rescanning…"), None);
        self.reload();
        glib::ControlFlow::Break // one-shot
    }

    /// `AlmanacApp._on_close`: save the window state, then let the close
    /// proceed.
    pub fn on_close(&self) -> glib::Propagation {
        self.save_state();
        glib::Propagation::Proceed
    }
}

/// Click handlers for recycled buttons: one permanent handler per button,
/// running whatever closure the button's current row put in its slot.
type ClickFn = Rc<dyn Fn(&gtk::Button)>;

#[derive(Clone, Default)]
struct ClickSlots(Rc<RefCell<HashMap<usize, Rc<RefCell<Option<ClickFn>>>>>>);

impl ClickSlots {
    fn key(btn: &gtk::Button) -> usize {
        btn.as_ptr() as usize
    }

    /// Give a freshly made button its slot and its one handler.
    fn attach(&self, btn: &gtk::Button) {
        let slot: Rc<RefCell<Option<ClickFn>>> = Rc::default();
        let mine = slot.clone();
        btn.connect_clicked(move |b| {
            let f = mine.borrow().clone();
            if let Some(f) = f {
                f(b);
            }
        });
        self.0.borrow_mut().insert(ClickSlots::key(btn), slot);
        let table = Rc::downgrade(&self.0);
        btn.connect_destroy(move |b| {
            if let Some(t) = table.upgrade() {
                t.borrow_mut().remove(&ClickSlots::key(b));
            }
        });
    }

    /// `btn.disconnect(btn._handler)` / `btn._handler = btn.connect(...)`.
    fn set(&self, btn: &gtk::Button, f: Option<ClickFn>) {
        let slot = self.0.borrow().get(&ClickSlots::key(btn)).cloned();
        if let Some(slot) = slot {
            *slot.borrow_mut() = f;
        }
    }
}

// ---------------------------------------------------------------------------
// The application and the CLI
// ---------------------------------------------------------------------------

/// Opening straight to a page makes Almanac scriptable -- a bar widget or a
/// keybinding can land on Updates without a click -- and makes a screenshot
/// of a given page reproducible.
pub const PAGE_ALIASES: &[(&str, &str)] = &[
    ("featured", "featured"), ("home", "featured"), ("today", "featured"),
    ("store", "discover"), ("app-store", "discover"), ("discover", "discover"),
    ("themes", "themes"), ("theme", "themes"),
    ("updates", "updates"), ("update", "updates"),
    ("ai", inventory::AGENT), ("agents", inventory::AGENT),
    ("health", "maintenance"), ("maintenance", "maintenance"),
    ("history", "history"), ("timeline", "history"),
    ("plugins", "plugins"), ("plugin", "plugins"),
    ("switchboard", "switchboard"), ("switches", "switchboard"),
    ("apps", "apps"), ("applications", "apps"),
    ("all", "all"), ("everything", "all"),
    ("orphans", inventory::ORPHAN),
];

/// `PAGE_ALIASES.get(wanted)`.
pub fn resolve_page(wanted: &str) -> Option<&'static str> {
    PAGE_ALIASES.iter().find(|(k, _)| *k == wanted).map(|(_, v)| *v)
}

/// The D-Bus name, which the desktop file and metadata use too.
pub fn application_id() -> &'static str {
    "io.github.twodxme.Almanac"
}

/// Almanac's own glyphs: `Path(__file__).resolve().parent / "icons"`, the
/// icons/ folder of the checkout this crate lives in.
pub fn icons_dir() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/icons"))
}

/// `AlmanacApp(Adw.Application)`.
#[derive(Clone)]
pub struct AlmanacApp {
    pub app: adw::Application,
    pub start_page: Rc<RefCell<String>>,
    /// The one window, kept alive here (`self.props.active_window`).
    pub win: Rc<RefCell<Option<Win>>>,
}

impl AlmanacApp {
    pub fn new() -> AlmanacApp {
        let app = adw::Application::builder()
            .application_id(application_id())
            .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
            .build();
        app.add_main_option(
            "page",
            glib::Char::from(b'p'),
            glib::OptionFlags::NONE,
            glib::OptionArg::String,
            "Open on a specific page: store, updates, ai, health, apps, all",
            Some("PAGE"),
        );
        let this = AlmanacApp {
            app,
            start_page: Rc::new(RefCell::new(String::new())),
            win: Rc::new(RefCell::new(None)),
        };
        let t = this.clone();
        this.app.connect_handle_local_options(move |_app, opts| t.local_options(opts));
        let t = this.clone();
        this.app.connect_command_line(move |_app, cmd| t.command_line(cmd));
        let t = this.clone();
        this.app.connect_startup(move |_app| t.startup());
        let t = this.clone();
        this.app.connect_activate(move |_app| t.activate());
        this
    }

    /// `do_command_line`: `--page NAME` picks the first page.
    pub fn command_line(&self, cmd: &gio::ApplicationCommandLine) -> glib::ExitCode {
        let wanted = wanted_page(&cmd.options_dict());
        if !wanted.is_empty() {
            match resolve_page(&wanted) {
                None => {
                    // Normally caught by `local_options` first; kept for a
                    // command line that reaches the primary instance some
                    // other way.
                    eprint!("{}", unknown_page_message(&wanted));
                    return glib::ExitCode::from(2);
                }
                Some(resolved) => *self.start_page.borrow_mut() = resolved.to_string(),
            }
        }
        self.app.activate();
        glib::ExitCode::SUCCESS
    }

    /// The `--page` check of `do_command_line`, made in the invoking process.
    ///
    /// The Python reports an unknown page with `printerr_literal`, which
    /// prints on the stderr of whoever ran the command -- also when another
    /// Almanac is already running and receives the command line. That call
    /// needs gio's v2_80 feature, which the crate does not enable, and a plain
    /// `eprint!` in `command_line` lands on the *running* instance's stderr
    /// instead. Checking here, before the command line is handed on, prints
    /// in the right terminal with the same text and exit status 2 -- and,
    /// like the Python, still does so where no display can be opened (GTK is
    /// not started yet).
    pub fn local_options(&self, opts: &glib::VariantDict) -> std::ops::ControlFlow<glib::ExitCode> {
        let wanted = wanted_page(opts);
        if !wanted.is_empty() && resolve_page(&wanted).is_none() {
            eprint!("{}", unknown_page_message(&wanted));
            return std::ops::ControlFlow::Break(glib::ExitCode::from(2));
        }
        std::ops::ControlFlow::Continue(())
    }

    /// `do_startup`: Almanac's own glyphs, shipped beside the code so they
    /// look the same under any icon theme. (The Adw.Application default
    /// handler runs first: "startup" is a run-first signal.)
    pub fn startup(&self) {
        if let Some(display) = gdk::Display::default() {
            gtk::IconTheme::for_display(&display).add_search_path(icons_dir());
        }
    }

    /// `do_activate`: create the window once, land on the requested page,
    /// present it.
    pub fn activate(&self) {
        let existing = self.win.borrow().clone();
        let win = match existing {
            Some(w) => w,
            None => {
                let w = Win::new(&self.app);
                let weak = w.downgrade();
                w.window.connect_close_request(move |_| match weak.upgrade() {
                    Some(w) => AlmanacApp::on_close(&w),
                    None => glib::Propagation::Proceed,
                });
                *self.win.borrow_mut() = Some(w.clone());
                w
            }
        };
        let start = self.start_page.borrow().clone();
        if !start.is_empty() {
            // After the restored category and the first inventory pass, so a
            // late callback cannot pull the window off the requested page.
            let weak = win.downgrade();
            glib::idle_add_local_full(glib::Priority::LOW, move || {
                if let Some(w) = weak.upgrade() {
                    w.select_category(&start);
                }
                glib::ControlFlow::Break
            });
        }
        win.window.present();
    }

    /// `_on_close`: save the window state; let the close proceed.
    pub fn on_close(win: &Win) -> glib::Propagation {
        win.on_close()
    }
}

impl Default for AlmanacApp {
    fn default() -> Self {
        AlmanacApp::new()
    }
}

/// `(opts.get("page") or "").strip().lower()`.
fn wanted_page(opts: &glib::VariantDict) -> String {
    let page = opts.lookup::<String>("page").ok().flatten().unwrap_or_default();
    py_strip(&page).to_lowercase()
}

/// The text `do_command_line` prints for a page it does not know.
fn unknown_page_message(wanted: &str) -> String {
    let keys: BTreeSet<&str> = PAGE_ALIASES.iter().map(|(k, _)| *k).collect();
    format!(
        "almanac: unknown page {}. Try one of: {}\n",
        py_repr_str(wanted),
        keys.into_iter().collect::<Vec<_>>().join(", ")
    )
}

/// `str.isspace()` for one character: Unicode White_Space plus the ASCII
/// separators U+001C..U+001F, which Python counts as whitespace.
fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// `str.strip()`.
fn py_strip(s: &str) -> &str {
    s.trim_matches(py_isspace)
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

/// `repr(str)` for the error message: single quotes unless the text holds one
/// and no double quote; what is not printable escaped as Python escapes it.
fn py_repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(quote);
    for c in s.chars() {
        let n = c as u32;
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
            _ if n < 0x100 => out.push_str(&format!("\\x{n:02x}")),
            _ if n < 0x10000 => out.push_str(&format!("\\u{n:04x}")),
            _ => out.push_str(&format!("\\U{n:08x}")),
        }
    }
    out.push(quote);
    out
}

/// The entry point for `main.rs`: `sys.exit(AlmanacApp().run(sys.argv))`.
///
/// `almanac --guard` is the background settings watcher: no window, so it is
/// dispatched before any GTK work.
pub fn run() -> glib::ExitCode {
    // `sys.argv` holds an argument that is not UTF-8 too (surrogate-escaped);
    // `env::args` would panic on it, so the bytes are compared.
    if std::env::args_os().skip(1).any(|a| a == "--guard") {
        let code = crate::watcher::main();
        return glib::ExitCode::from(u8::try_from(code).unwrap_or(1));
    }
    AlmanacApp::new().app.run()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_texture_cache_drops_the_least_recently_used_by_size() {
        let mut c: TexCache<u32> = TexCache::new(100);
        c.insert("a".into(), 1, 40);
        c.insert("b".into(), 2, 40);
        assert_eq!(c.get("a"), Some(1)); // a is now newer than b
        c.insert("c".into(), 3, 40); // over budget: b goes, not a
        assert_eq!((c.get("b"), c.get("a"), c.get("c")), (None, Some(1), Some(3)));
        assert_eq!((c.len(), c.bytes()), (2, 80));
        // replacing a key does not count it twice
        c.insert("c".into(), 4, 50);
        assert_eq!((c.len(), c.bytes()), (2, 90));
        // one big entry evicts as many as it needs
        c.insert("big".into(), 5, 100);
        assert_eq!((c.len(), c.bytes(), c.get("big")), (1, 100, Some(5)));
        // and one bigger than the whole budget is not kept at all
        c.insert("huge".into(), 6, 101);
        assert_eq!((c.get("huge"), c.get("big")), (None, Some(5)));
        // many small ones stay within the budget
        for i in 0..1000 {
            c.insert(format!("i{i}"), i, 7);
        }
        assert!(c.bytes() <= 100 && c.len() == 14);
        assert_eq!(c.get("i999"), Some(999));
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

    /// test_every_page_shortcut_is_a_valid_accelerator: Ctrl+10 does not
    /// exist; an invalid accelerator stops the window from being constructed
    /// at all.
    #[test]
    fn every_page_shortcut_is_a_valid_accelerator() {
        assert!(PAGE_KEYS.len() <= 10);
        // `Gtk.ShortcutTrigger.parse_string` needs GTK initialised on the
        // main thread, which a test thread is not; the accelerator is
        // checked the way GTK reads it instead: a modifier GTK knows, then a
        // key name GDK resolves (gdk_keyval_from_name needs no GTK).
        let mut seen = std::collections::BTreeSet::new();
        for i in 1..=PAGE_KEYS.len() {
            let accel = page_accel(i);
            let key = accel.strip_prefix("<Control>").expect("a <Control> accelerator");
            assert!(gdk::Key::from_name(key).is_some(), "{accel}: no such key");
            assert!(seen.insert(accel.clone()), "{accel} used twice");
        }
        // The check would catch the mistake it is for.
        assert!(gdk::Key::from_name("10").is_none());
    }

    #[test]
    fn page_aliases_resolve() {
        assert_eq!(resolve_page("health"), Some("maintenance"));
        assert_eq!(resolve_page("store"), Some("discover"));
        assert_eq!(resolve_page("orphans"), Some(inventory::ORPHAN));
        assert_eq!(resolve_page("nope"), None);
    }

    /// `str.strip().lower()` as `do_command_line` reads `--page`.
    #[test]
    fn page_name_is_stripped_like_python() {
        assert_eq!(py_strip("\x1c updates\u{a0}\x1f"), "updates");
        assert_eq!(py_strip("\u{200b}x"), "\u{200b}x"); // not whitespace to Python either
        let msg = unknown_page_message("nope");
        assert!(msg.starts_with("almanac: unknown page 'nope'. Try one of: agents, ai, all, app-store,"));
        assert!(msg.ends_with("today, update, updates\n"));
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

    #[test]
    fn py_repr_quotes() {
        assert_eq!(py_repr_str("abc"), "'abc'");
        assert_eq!(py_repr_str("it's"), "\"it's\"");
        assert_eq!(py_repr_str("a\u{200b}\u{a0}\x1c\u{e000}\u{10ffff}é日"), "'a\\u200b\\xa0\\x1c\\ue000\\U0010ffffé日'");
        assert_eq!(py_repr_str("a\"b'c"), "'a\"b\\'c'");
    }
}
