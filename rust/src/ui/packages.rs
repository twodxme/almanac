//! UI section `packages` (port of `app.py` lines 1454-2481).
//!
//! Data loading and filtering: the Updates, Applications, Everything and
//! Orphans tables, the detail dialog, the sidebar's selection and counts,
//! page chrome, the header's primary action, theming and the status line.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use serde_json::{Map, Value};

use crate::i18n::tr;
use crate::inventory;
use crate::ui::shell::{
    name, page_spec, reverse_text, sorts_installed, source_label, spawn_bg, AppItem, Win,
};

/// The header's primary action for one page: (label, enabled, tooltip,
/// handler), as `_primary_actions` returns it.
#[derive(Clone)]
pub struct PrimaryAction {
    pub label: String,
    pub enabled: bool,
    pub tooltip: String,
    pub handler: Rc<dyn Fn(&Win)>,
}

/// Rows `plain` can build: the Python passed the row class (`cls`), which
/// is `Adw.ActionRow` by default and `Adw.ExpanderRow` in a few places.
pub trait PlainRow: IsA<adw::PreferencesRow> {
    fn new_plain() -> Self;
    fn put_subtitle(&self, subtitle: &str);
}

impl PlainRow for adw::ActionRow {
    fn new_plain() -> Self {
        adw::ActionRow::new()
    }
    fn put_subtitle(&self, subtitle: &str) {
        self.set_subtitle(subtitle);
    }
}

impl PlainRow for adw::ExpanderRow {
    fn new_plain() -> Self {
        adw::ExpanderRow::new()
    }
    fn put_subtitle(&self, subtitle: &str) {
        self.set_subtitle(subtitle);
    }
}

/// State only this section uses. Everything else this section reads or
/// writes (`_discovering`, `_discover_timer`, `_narrow`, `_primary`,
/// `_reported`, the `*_loaded_once` flags, `_manual_sorter`) is read by other
/// sections too and lives in `shell::Shared`.
#[derive(Default)]
pub struct State {
    /// Coalesces theme-change bursts into one restyle (`_theme_timer`).
    pub theme_timer: Option<glib::SourceId>,
}

// ---------------------------------------------------------------------------
// Reading the mixed-type `extra` dict the way Python's builtins do
// ---------------------------------------------------------------------------

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

/// `str(value)` for a JSON value. A list or dict prints as its repr, as
/// Python's `str()` does.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(_) | Value::Object(_) => py_repr(v),
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(n) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
            (Some(i), _, _) => i.to_string(),
            (_, Some(u), _) => u.to_string(),
            (_, _, Some(f)) => crate::util::py_float_repr(f),
            _ => n.to_string(),
        },
    }
}

/// `type(value).__name__` for a JSON value, as Python decoded it.
fn py_type_name(v: &Value) -> &'static str {
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

/// `repr(value)` for a JSON value, as Python prints the decoded object.
fn py_repr(v: &Value) -> String {
    match v {
        Value::String(s) => py_repr_str(s),
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, x)| format!("{}: {}", py_repr_str(k), py_repr(x)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => py_str(other),
    }
}

/// `repr(str)`: single quotes unless the text has one and no double quote;
/// control and non-space separator characters escaped.
fn py_repr_str(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::from(q);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == q => {
                out.push('\\');
                out.push(c);
            }
            c if c != ' ' && (c.is_control() || c.is_whitespace()) => {
                let n = c as u32;
                if n < 0x100 {
                    out.push_str(&format!("\\x{n:02x}"));
                } else if n < 0x10000 {
                    out.push_str(&format!("\\u{n:04x}"));
                } else {
                    out.push_str(&format!("\\U{n:08x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push(q);
    out
}

/// `e.get(key, "")` for a string field.
fn text_of(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

/// Python's `str.isspace()`: Unicode White_Space plus the four information
/// separators (U+001C-U+001F), which Python counts and Rust does not.
fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `text.strip()`.
pub fn py_strip(text: &str) -> &str {
    text.trim_matches(py_space)
}

/// An integer as Python's `int()` holds it: unbounded, so kept as a sign
/// and its decimal digits.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PyInt {
    neg: bool,
    /// No leading zeros; "0" for zero (which is never negative).
    mag: String,
}

impl PyInt {
    fn negated(&self) -> PyInt {
        PyInt { neg: !self.neg && self.mag != "0", mag: self.mag.clone() }
    }
}

impl Ord for PyInt {
    fn cmp(&self, other: &Self) -> Ordering {
        let by_mag = (self.mag.len(), &self.mag).cmp(&(other.mag.len(), &other.mag));
        match (self.neg, other.neg) {
            (false, false) => by_mag,
            (true, true) => by_mag.reverse(),
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
        }
    }
}

impl PartialOrd for PyInt {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The value of a Unicode decimal digit (category Nd), as Python's int()
/// reads it. Every Nd run in Unicode is a whole 0-9 set starting at its
/// zero, so the value is the distance from the run's start, mod 10.
fn decimal_value(c: char) -> Option<u32> {
    use glib::Unichar;
    if c.is_ascii_digit() {
        return c.to_digit(10);
    }
    let is_nd = |c: char| c.unicode_type() == glib::UnicodeType::DecimalNumber;
    if !is_nd(c) {
        return None;
    }
    let mut back = 0u32;
    let mut n = c as u32;
    while let Some(prev) = n.checked_sub(1).and_then(char::from_u32) {
        if !is_nd(prev) {
            break;
        }
        back += 1;
        n -= 1;
    }
    Some(back % 10)
}

/// `int(text)`, or None where it raises ValueError: surrounding whitespace,
/// an optional sign, decimal digits from any script, and single underscores
/// between digits.
fn py_int(text: &str) -> Option<PyInt> {
    let t = py_strip(text);
    let (neg, body) = match t.chars().next() {
        Some('-') => (true, &t[1..]),
        Some('+') => (false, &t[1..]),
        _ => (false, t),
    };
    let chars: Vec<char> = body.chars().collect();
    if chars.is_empty() {
        return None;
    }
    let mut digits = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' {
            let before = i > 0 && chars[i - 1] != '_';
            let after = chars.get(i + 1).is_some_and(|&n| n != '_' && decimal_value(n).is_some());
            if !before || !after {
                return None;
            }
            continue;
        }
        digits.push(char::from_digit(decimal_value(c)?, 10)?);
    }
    let mag = digits.trim_start_matches('0');
    let mag = if mag.is_empty() { "0".to_string() } else { mag.to_string() };
    let zero = mag == "0";
    Some(PyInt { neg: neg && !zero, mag })
}

/// `float(x)` where Python's datetime.fromtimestamp takes it: an int, a
/// bool or a float. Anything else is the TypeError it raises.
fn timestamp_of(v: Option<&Value>) -> Result<f64, String> {
    match v {
        Some(Value::Number(n)) => n.as_f64().ok_or_else(|| "timestamp out of range".to_string()),
        Some(Value::Bool(b)) => Ok(f64::from(u8::from(*b))),
        Some(other) => Err(format!("TypeError: not a timestamp: {}", py_repr(other))),
        None => Err("KeyError: 'built_ts'".to_string()),
    }
}

/// Insert a comma every three digits, from the right.
fn group_digits(digits: &str) -> String {
    let mut out = String::new();
    let n = digits.chars().count();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (n - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `f"{n:,}"`: an integer with thousands separators.
pub fn thousands(n: i64) -> String {
    let out = group_digits(&n.unsigned_abs().to_string());
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

/// `f"{x:,}"` for a JSON value: ints and bools group their digits, a float
/// groups the integer part of its repr ('1,234.0'); a str, list or dict is
/// the ValueError/TypeError Python raises.
fn py_format_thousands(v: &Value) -> Result<String, String> {
    match v {
        Value::Bool(b) => Ok(u8::from(*b).to_string()),
        Value::Number(n) if n.as_i64().is_some() || n.as_u64().is_some() => {
            let s = n.to_string();
            let (sign, digits) = s.strip_prefix('-').map_or(("", s.as_str()), |d| ("-", d));
            Ok(format!("{sign}{}", group_digits(digits)))
        }
        Value::Number(n) => {
            let r = crate::util::py_float_repr(n.as_f64().unwrap_or(0.0));
            if r.contains(['e', 'N', 'I']) {
                return Ok(r);
            }
            let (sign, body) = r.strip_prefix('-').map_or(("", r.as_str()), |d| ("-", d));
            let (int, frac) = body.split_once('.').unwrap_or((body, ""));
            Ok(format!("{sign}{}.{frac}", group_digits(int)))
        }
        Value::String(_) => Err("ValueError: Cannot specify ',' with 's'.".to_string()),
        other => Err(format!("TypeError: unsupported format string passed to {}", py_repr(other))),
    }
}

/// `datetime.fromtimestamp(ts).strftime(fmt)`, in the user's locale (the
/// month names follow LC_TIME, as Python's strftime does once i18n has
/// called setlocale). Err where fromtimestamp raises: NaN, infinity, or a
/// year outside 1-9999.
fn local_time(ts: f64, fmt: &str) -> Result<String, String> {
    if !ts.is_finite() {
        return Err(format!("ValueError: Invalid value {} for timestamp", crate::util::py_float_repr(ts)));
    }
    // fromtimestamp rounds to the microsecond, half to even.
    let micros = (ts * 1e6).round_ties_even();
    let secs = (micros / 1e6).floor();
    if !(-62_135_596_800.0..=253_402_300_799.0).contains(&secs) {
        return Err("OverflowError: timestamp out of range for platform time_t".to_string());
    }
    glib::DateTime::from_unix_local(secs as i64)
        .and_then(|t| t.format(fmt))
        .map(|s| s.to_string())
        .map_err(|e| format!("ValueError: {e}"))
}

/// What `_detail_row` accepts: any value. `None` and `""` mean "no row";
/// everything else is shown as `str(value)`, so 0 and False still show.
pub trait DetailValue {
    /// None for `value in (None, "")`, otherwise `str(value)`.
    fn detail_text(&self) -> Option<String>;
}

impl DetailValue for str {
    fn detail_text(&self) -> Option<String> {
        (!self.is_empty()).then(|| self.to_string())
    }
}

impl DetailValue for String {
    fn detail_text(&self) -> Option<String> {
        self.as_str().detail_text()
    }
}

impl DetailValue for glib::GString {
    fn detail_text(&self) -> Option<String> {
        self.as_str().detail_text()
    }
}

impl DetailValue for Value {
    fn detail_text(&self) -> Option<String> {
        match self {
            Value::Null => None,
            Value::String(s) => s.detail_text(),
            other => Some(py_str(other)),
        }
    }
}

impl DetailValue for i64 {
    fn detail_text(&self) -> Option<String> {
        Some(self.to_string())
    }
}

impl DetailValue for bool {
    fn detail_text(&self) -> Option<String> {
        Some(if *self { "True" } else { "False" }.to_string())
    }
}

impl<T: DetailValue> DetailValue for Option<T> {
    fn detail_text(&self) -> Option<String> {
        self.as_ref().and_then(DetailValue::detail_text)
    }
}

/// `_say`'s default timeout, in seconds. Rust has no default arguments, so
/// callers that relied on it pass this.
pub const SAY_TIMEOUT: u32 = 5;

// ---------------------------------------------------------------------------
// Pure decisions, kept apart from the widgets so they can be tested without
// a display
// ---------------------------------------------------------------------------

/// The body of `_match` for one row, given the window's filter and query.
pub fn match_item(filter_key: &str, query: &str, item: &AppItem) -> bool {
    if item.source == inventory::AVAILABLE || item.source == inventory::PLUGIN {
        return true;
    }
    if filter_key == "updates" && !item.has_update {
        return false;
    }
    if filter_key == "apps"
        && !(truthy(item.app.extra.get("is_app"))
            || item.source == inventory::APPIMAGE
            || item.source == inventory::LOCAL)
    {
        return false;
    }
    // Plugins have their own page. They stay in Everything and Updates.
    if truthy(item.app.extra.get("plugin_id"))
        && (filter_key == "apps" || filter_key == inventory::LOCAL)
    {
        return false;
    }
    let broad = matches!(filter_key, "all" | "updates" | "apps");
    if broad && item.source == inventory::ORPHAN {
        return false;
    }
    if !broad && item.source != filter_key {
        return false;
    }
    if !query.is_empty() && !item.haystack.contains(query) {
        return false;
    }
    true
}

/// What `_apply_group_state` does to one group, worked out from the
/// sidebar's tables: the header's chevron icon and each member row's
/// visibility. The widgets are generic so the wiring can be tested without
/// a display.
pub struct GroupState<H, M> {
    pub header: H,
    pub icon: &'static str,
    pub members: Vec<(M, bool)>,
}

pub fn group_state<H: Clone, M: Clone>(
    group_rows: &[(String, Option<H>)],
    group_members: &BTreeMap<String, Vec<M>>,
    collapsed: &std::collections::BTreeSet<String>,
    filter_key: &str,
    gkey: &str,
    key_of: impl Fn(&M) -> &str,
) -> Option<GroupState<H, M>> {
    // self.group_rows.get(gkey) is None: ungrouped primaries never fold.
    let header = group_rows.iter().find(|(k, _)| k == gkey).and_then(|(_, h)| h.clone())?;
    let folded = collapsed.contains(gkey);
    let members = group_members
        .get(gkey)
        .map(|rows| {
            rows.iter()
                // The selected category stays visible even when its group
                // folds, or the sidebar would show a selection that is not
                // there.
                .map(|r| (r.clone(), !folded || key_of(r) == filter_key))
                .collect()
        })
        .unwrap_or_default();
    Some(GroupState {
        header,
        icon: if folded { "pan-end-symbolic" } else { "pan-down-symbolic" },
        members,
    })
}

/// The badges `_detail_badges` puts under an app's name.
pub fn detail_badges_for(item: &AppItem) -> Vec<(String, &'static str)> {
    let mut out = Vec::new();
    if item.critical == "security" {
        out.push(("security update".to_string(), "pill-crit"));
    } else if item.critical == "core" {
        out.push(("core system".to_string(), "pill-crit"));
    }
    if item.stale == "orphaned" {
        out.push(("unmaintained".to_string(), "pill-warn"));
    } else if item.stale == "flagged" {
        out.push(("flagged out of date".to_string(), "pill-warn"));
    }
    if item.git_dirty {
        out.push(("local edits".to_string(), "pill-warn"));
    }
    if item.source == inventory::ORPHAN {
        out.push((format!("orphan · {}", item.advice), "pill-warn"));
    }
    out
}

/// The "Available" row of the detail sheet: the new version and how far
/// behind the installed one is.
pub fn available_label(item: &AppItem) -> String {
    let mut label = item.app.update.clone();
    if item.behind != 0 && matches!(item.behind_kind.as_str(), "major" | "minor" | "patch" | "point")
    {
        label += &format!("  ({} {}", item.behind, item.behind_kind);
        label += if item.behind > 1 { "s behind)" } else { " behind)" };
    } else if item.behind_kind == "rel" {
        label += "  (rebuild only)";
    }
    label
}

/// Keys `_on_sort` has a getter for.
pub const SORT_KEYS: &[&str] = &[
    "name", "name_desc", "size", "installed", "category", "behind", "popular", "trending",
    "newest",
];

/// `-int(i.added.replace("-", "") or 0)` when `added` is set, "" when not --
/// and None where Python's int() would raise.
#[derive(Debug, Clone, PartialEq)]
enum Added {
    Blank,
    Neg(PyInt),
}

fn added_key(added: &str) -> Option<Added> {
    if added.is_empty() {
        return Some(Added::Blank);
    }
    let digits = added.replace('-', "");
    if digits.is_empty() {
        return Some(Added::Neg(PyInt { neg: false, mag: "0".into() }));
    }
    py_int(&digits).map(|n| Added::Neg(n.negated()))
}

fn float_cmp(a: f64, b: f64) -> Ordering {
    a.partial_cmp(&b).unwrap_or(Ordering::Equal)
}

/// `(g(a) > g(b)) - (g(a) < g(b))` for the getter `key` names. A getter
/// that would raise in Python (an unparseable date, or "" compared with a
/// number) makes the pair compare equal, as the failed callback did.
pub fn sort_cmp(key: &str, a: &AppItem, b: &AppItem) -> Ordering {
    let (an, bn) = (a.name.to_lowercase(), b.name.to_lowercase());
    match key {
        "name" => an.cmp(&bn),
        "name_desc" => reverse_text(&an, &bn),
        "size" => float_cmp(-a.size_mb, -b.size_mb),
        "installed" => float_cmp(-a.installed_ts, -b.installed_ts),
        "category" => (a.category.is_empty(), a.category.to_lowercase())
            .cmp(&(b.category.is_empty(), b.category.to_lowercase())),
        // Negated in i128: Python ints are unbounded, so -i64::MIN is just a
        // bigger number there, not an overflow.
        "behind" => (-i128::from(a.behind)).cmp(&-i128::from(b.behind)),
        // All-time standing. Unverified plugins sink: anyone can list one.
        "popular" => (-i128::from(a.votes), !a.verified, an)
            .cmp(&(-i128::from(b.votes), !b.verified, bn)),
        // The AUR's popularity is already decay-weighted, which is what
        // "trending" means; plugins fall back to how recently they landed.
        "trending" => {
            let (Some(xa), Some(xb)) = (added_key(&a.added), added_key(&b.added)) else {
                return Ordering::Equal;
            };
            // The tuple compare stops at the first element that is not ==.
            // A NaN is never == anything, so it stops there, and NaN is
            // neither > nor < anything: the pair compares equal outright.
            let (pa, pb) = (-a.popularity, -b.popularity);
            if pa.is_nan() || pb.is_nan() {
                return Ordering::Equal;
            }
            let first = float_cmp(pa, pb);
            if first != Ordering::Equal {
                return first;
            }
            let second = match (&xa, &xb) {
                (Added::Blank, Added::Blank) => Ordering::Equal,
                (Added::Neg(x), Added::Neg(y)) => x.cmp(y),
                _ => return Ordering::Equal, // str vs int: TypeError
            };
            second.then_with(|| an.cmp(&bn))
        }
        "newest" => (a.added.is_empty())
            .cmp(&b.added.is_empty())
            .then_with(|| reverse_text(&a.added, &b.added))
            .then_with(|| an.cmp(&bn)),
        _ => Ordering::Equal,
    }
}

/// `" ".join(text.split())[:400]`.
fn squeeze(text: &str, limit: usize) -> String {
    text.split(py_space)
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(limit)
        .collect()
}

impl Win {
    /// `self.lookup_action(name)`, as the SimpleAction it always is here.
    fn win_action(&self, act_name: &str) -> Option<gio::SimpleAction> {
        self.window.lookup_action(act_name).and_downcast::<gio::SimpleAction>()
    }

    /// Every row of `self.store`, as the model's own objects (a reference
    /// each, not a copy of the row); read them in place with `AppItem::of`.
    fn store_objects(&self) -> Vec<glib::Object> {
        let store = self.shared.borrow().store.clone();
        (0..store.n_items()).filter_map(|i| store.item(i)).collect()
    }

    /// How many rows of `self.store` satisfy `pred`, read in place.
    fn count_items(objects: &[glib::Object], pred: impl Fn(&AppItem) -> bool) -> usize {
        objects.iter().filter(|o| AppItem::of(o).is_some_and(|it| pred(&it))).count()
    }

    /// `self.filter.changed(Gtk.FilterChange.DIFFERENT)`.
    fn filter_changed(&self) {
        let filter = self.shared.borrow().filter.clone();
        filter.changed(gtk::FilterChange::Different);
    }

    // ---- data ------------------------------------------------------------
    pub fn reload(&self) {
        if let Some(act) = self.win_action("rescan") {
            act.set_enabled(false);
        }
        self.summarize(&tr("Scanning…"), None);

        // checkupdates and yay take seconds; on the UI thread that is a freeze.
        let w = self.downgrade();
        spawn_bg(inventory::collect, move |apps| {
            if let Some(w) = w.upgrade() {
                w.apply(apps);
            }
        });
    }

    /// Desktop notification, falling back to the status line.
    ///
    /// Infallible form of `try_notify` for callers with nothing left to do
    /// after it: the error Python would raise is reported on stderr.
    pub fn notify(&self, title: &str, body: &str, urgency: &str) {
        if let Err(e) = self.try_notify(title, body, urgency) {
            eprintln!("almanac: _notify: {e}");
        }
    }

    /// `_notify` as Python runs it: Err where Popen raises something other
    /// than OSError -- a NUL in an argument is a ValueError, which escaped
    /// to the caller and skipped the rest of it. Only a failed spawn (the
    /// OSError) falls back to the status line.
    pub fn try_notify(&self, title: &str, body: &str, urgency: &str) -> Result<(), String> {
        let tool = crate::util::which("omarchy-notification-send")
            .or_else(|| crate::util::which("notify-send"));
        let Some(tool) = tool else {
            self.say(&format!("{title}: {body}"), SAY_TIMEOUT);
            return Ok(());
        };
        if [urgency, title, body].iter().any(|a| a.contains('\0'))
            || tool.as_os_str().as_encoded_bytes().contains(&0)
        {
            return Err("ValueError: embedded null byte".to_string());
        }
        // start_new_session=True: the child calls setsid() before exec, so
        // it leaves this session and its controlling terminal and outlives a
        // SIGHUP when the launching terminal closes. GLib spawns through an
        // intermediate child, so nothing here has to wait for it; the
        // environment and stdin are inherited, as Popen's are.
        let env: Vec<std::path::PathBuf> = std::env::vars_os()
            .map(|(k, v)| {
                let mut kv = k;
                kv.push("=");
                kv.push(v);
                std::path::PathBuf::from(kv)
            })
            .collect();
        let envp: Vec<&std::path::Path> = env.iter().map(|p| p.as_path()).collect();
        let argv: Vec<&std::path::Path> = vec![
            tool.as_path(),
            std::path::Path::new("-u"),
            std::path::Path::new(urgency),
            std::path::Path::new(title),
            std::path::Path::new(body),
        ];
        let spawned = glib::spawn_async(
            None::<&std::path::Path>,
            &argv,
            &envp,
            glib::SpawnFlags::CHILD_INHERITS_STDIN,
            Some(Box::new(|| {
                let _ = rustix::process::setsid();
            })),
        );
        if spawned.is_err() {
            self.say(&format!("{title}: {body}"), SAY_TIMEOUT);
        }
        Ok(())
    }

    /// Tell the user once about things that need a decision, not a click.
    ///
    /// Only new findings are announced: a notification repeated on every
    /// rescan is one the user learns to dismiss without reading.
    pub fn report_health(&self, apps: &[inventory::App]) {
        let abandoned: Vec<&inventory::App> = apps
            .iter()
            .filter(|a| matches!(text_of(a.extra.get("stale")).as_str(), "orphaned" | "flagged"))
            .collect();
        let dupes = inventory::duplicates(apps);

        let mut findings = std::collections::BTreeSet::new();
        for a in &abandoned {
            findings.insert(format!("stale:{}:{}", a.name, text_of(a.extra.get("stale"))));
        }
        for (k, _v) in &dupes {
            findings.insert(format!("dupe:{k}"));
        }

        let new: Vec<String> = {
            let mut sh = self.shared.borrow_mut();
            let new = findings.difference(&sh.reported).cloned().collect();
            sh.reported.extend(findings.iter().cloned());
            new
        };
        if new.is_empty() {
            return;
        }

        if !abandoned.is_empty() && new.iter().any(|f| f.starts_with("stale:")) {
            let lines: Vec<String> = abandoned
                .iter()
                .map(|a| {
                    format!(
                        "{} — {}",
                        a.name,
                        if text_of(a.extra.get("stale")) == "orphaned" {
                            "no maintainer in the AUR"
                        } else {
                            "flagged out of date by its maintainer"
                        }
                    )
                })
                .collect();
            let body = lines.iter().take(5).cloned().collect::<Vec<_>>().join("\n");
            // A _notify that raised ended _report_health here.
            if let Err(e) = self.try_notify(&tr("Software no longer maintained"), &body, "critical") {
                eprintln!("almanac: _report_health: {e}");
                return;
            }
        }

        if !dupes.is_empty() && new.iter().any(|f| f.starts_with("dupe:")) {
            let lines: Vec<String> = dupes
                .iter()
                .take(5)
                .map(|(k, v)| format!("{k}: {}", v.join(", ")))
                .collect();
            self.notify(&tr("Duplicate software installed"), &lines.join("\n"), "normal");
        }
    }

    pub fn apply(&self, apps: Vec<inventory::App>) {
        let store = self.shared.borrow().store.clone();
        store.remove_all();
        for a in &apps {
            store.append(&AppItem::new(a.clone()).boxed());
        }
        if let Some(act) = self.win_action("rescan") {
            act.set_enabled(true);
        }
        self.filter_changed();
        // An exception out of _update_status ended _apply before the health
        // report, as Python's traceback did.
        if let Err(e) = self.update_status() {
            eprintln!("almanac: _apply: {e}");
            return;
        }
        self.report_health(&apps);
    }

    /// Err where Python's `_update_status` raised (out of `_refresh_stats`);
    /// everything up to that point has been applied, the rest has not.
    pub fn update_status(&self) -> Result<(), String> {
        let items = self.store_objects();
        let total = items.len();
        let updates = Win::count_items(&items, |i| i.has_update);
        // Clone, then ask GTK: never hold the borrow across the call.
        let (sorted, filter_key, query) = {
            let sh = self.shared.borrow();
            (sh.sorted.clone(), sh.filter_key.clone(), sh.query.clone())
        };
        let shown = sorted.n_items() as usize;
        let crit = Win::count_items(&items, |i| !i.critical.is_empty());

        let orphan_n = Win::count_items(&items, |i| i.source == inventory::ORPHAN);
        let installed = total - orphan_n;
        let mut text = format!("{installed} installed · {updates} with updates");
        if crit != 0 {
            text += &format!(" · {crit} critical");
        }
        if orphan_n != 0 {
            text += &format!(" · {orphan_n} orphans");
        }
        if shown != total {
            text += &format!(" · showing {shown}");
        }
        // Say which kind of empty this is, rather than showing a blank table.
        if shown == 0 {
            let (mut icon, mut title, mut body) = match crate::ui::history::empty_states(&filter_key)
            {
                Some((icon, title, body)) => (icon, title, body),
                None => (
                    "system-search-symbolic",
                    "Nothing matches".to_string(),
                    "Try a different search.".to_string(),
                ),
            };
            if !query.is_empty() {
                icon = "system-search-symbolic";
                title = "Nothing matches".to_string();
                body = format!("No installed software matches “{query}”.");
            }
            self.list_empty.set_icon_name(Some(icon));
            self.list_empty.set_title(&title);
            self.list_empty.set_description(Some(&body));
            self.list_stack.set_visible_child_name("empty");
        } else {
            self.list_stack.set_visible_child_name("list");
        }

        self.summarize(&text, Some("packages"));
        self.sync_primary();
        self.update_sidebar_counts();
        self.refresh_stats()?;
        let removable =
            Win::count_items(&items, |i| i.source == inventory::ORPHAN && i.advice != "keep");
        if let Some(act) = self.win_action("clean") {
            act.set_enabled(removable > 0);
        }
        self.update_btn.set_label(&if updates != 0 {
            format!("Update all ({updates})")
        } else {
            tr("Update all")
        });
        Ok(())
    }

    pub fn on_row_activated(&self, _view: &gtk::ColumnView, position: u32) {
        let sorted = self.shared.borrow().sorted.clone();
        let item = sorted.item(position).and_then(|o| AppItem::of(&o).map(|it| it.clone()));
        if let Some(item) = item {
            self.show_details(&item);
        }
    }

    /// A card for one app: everything known about it, and what can be done.
    ///
    /// Details are fetched on open rather than during the scan -- dependency
    /// lists are a pacman call each, which would make the list slow for data
    /// nobody reads until they ask for it.
    pub fn show_details(&self, item: &AppItem) {
        // An entry whose details Python could not read (a missing advisory
        // field, a timestamp that is not a number) raised out of the click
        // handler: the traceback went to stderr and no dialog opened.
        if let Err(e) = self.build_details(item) {
            eprintln!("almanac: cannot show details for {}: {e}", item.app.name);
        }
    }

    fn build_details(&self, item: &AppItem) -> Result<(), String> {
        let app = &item.app;
        let extra: Map<String, Value> = inventory::try_details(app)?;
        let has = |key: &str| truthy(extra.get(key));

        let page = adw::PreferencesPage::new();

        // --- header: icon, name, what it is ---
        let hero = adw::PreferencesGroup::new();
        let head = gtk::Box::builder().spacing(12).margin_bottom(4).build();
        let img = gtk::Image::new();
        img.set_pixel_size(64);
        self.app_icon(&img, &item.app, 128);
        head.append(&img);

        let titles = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .valign(gtk::Align::Center)
            .spacing(2)
            .build();
        let t = gtk::Label::builder().xalign(0.0).label(&app.name).build();
        t.add_css_class("title-2");
        titles.append(&t);
        if !app.description.is_empty() {
            let d = gtk::Label::builder().xalign(0.0).label(&app.description).wrap(true).build();
            d.add_css_class("dim-label");
            titles.append(&d);
        }
        let badges = gtk::Box::builder().spacing(6).margin_top(4).build();
        for (text, css) in self.detail_badges(item) {
            let b = gtk::Label::new(Some(&text));
            b.add_css_class(css);
            badges.append(&b);
        }
        if badges.first_child().is_some() {
            titles.append(&badges);
        }
        head.append(&titles);
        hero.add(&head);
        page.add(&hero);

        // --- version ---
        let vg = adw::PreferencesGroup::builder().title(tr("Version")).build();
        Win::detail_row(&vg, &tr("Installed"), &app.version);
        if !app.update.is_empty() {
            Win::detail_row(&vg, &tr("Available"), &available_label(item));
        }
        if has("published") {
            Win::detail_row(&vg, &tr("Released"), &extra["published"]);
        }
        if has("git_subject") {
            Win::detail_row(&vg, &tr("Latest upstream commit"), &extra["git_subject"]);
        }
        if has("plugin_detail") {
            Win::detail_row(&vg, &tr("State"), &extra["plugin_detail"]);
            if extra.get("enabled").is_some_and(|v| !v.is_null()) {
                Win::detail_row(
                    &vg,
                    &tr("Switched"),
                    &if truthy(extra.get("enabled")) { tr("On") } else { tr("Off") },
                );
            }
        }
        page.add(&vg);

        // Advisories from the Arch Security Tracker, each linked to its page.
        if has("vulns") {
            let sg = adw::PreferencesGroup::builder()
                .title(tr("Known vulnerabilities"))
                .description(tr("From the Arch Security Tracker, for the installed version."))
                .build();
            // `for v in extra["vulns"]` with `v["cves"]`, `v["fixed"]` and
            // `v['group']`: anything but a list of dicts, or a dict missing a
            // key, raised before the dialog opened.
            let vulns = match &extra["vulns"] {
                Value::Array(a) => a.clone(),
                other => return Err(format!("TypeError: vulns is not a list: {}", py_repr(other))),
            };
            for v in &vulns {
                let Value::Object(v) = v else {
                    return Err(format!("TypeError: advisory is not a dict: {}", py_repr(v)));
                };
                let get = |k: &str| v.get(k).ok_or_else(|| format!("KeyError: {}", py_repr_str(k)));
                // ", ".join(v["cves"][:4]): a list of strings, or a string,
                // whose characters are then joined.
                // Only the four that are joined have to be strings; len()
                // counts them all.
                let (cve_list, cve_n): (Vec<String>, usize) = match get("cves")? {
                    Value::Array(a) => (
                        a.iter()
                        .take(4)
                        .map(|c| match c {
                            Value::String(s) => Ok(s.clone()),
                            other => Err(format!(
                                "TypeError: sequence item: expected str instance, got {}",
                                py_repr(other)
                            )),
                        })
                        .collect::<Result<_, _>>()?,
                        a.len(),
                    ),
                    Value::String(s) => {
                        (s.chars().take(4).map(String::from).collect(), s.chars().count())
                    }
                    other => return Err(format!("TypeError: cves cannot be sliced: {}", py_repr(other))),
                };
                let cves = cve_list.join(", ") + if cve_n > 4 { "…" } else { "" };
                let fixed = get("fixed")?;
                let fix = if truthy(Some(fixed)) {
                    format!("fixed in {}", py_str(fixed))
                } else {
                    "no fix released yet".to_string()
                };
                let group = py_str(get("group")?);
                let (severity, kind) = (py_str(get("severity")?), py_str(get("kind")?));
                let row: adw::ActionRow = Win::plain(
                    &format!("{group} · {severity} · {kind}"),
                    &format!("{fix} · {cves}"),
                );
                row.set_subtitle_lines(2);
                let link = gtk::Button::builder()
                    .icon_name("adw-external-link-symbolic")
                    .valign(gtk::Align::Center)
                    .tooltip_text(tr("Open the advisory"))
                    .build();
                link.add_css_class("flat");
                link.update_property(&[gtk::accessible::Property::Label(&format!(
                    "Open advisory {group}"
                ))]);
                let w = self.downgrade();
                link.connect_clicked(move |_b| {
                    if let Some(w) = w.upgrade() {
                        w.open_url(&format!("https://security.archlinux.org/{group}"));
                    }
                });
                row.add_suffix(&link);
                sg.add(&row);
            }
            page.add(&sg);
        }

        // --- where it came from ---
        let og = adw::PreferencesGroup::builder().title(tr("Origin")).build();
        Win::detail_row(&og, &tr("Source"), &source_label(&item.source));
        for (key, label) in [
            ("category", "Category"),
            ("size", "Installed size"),
            ("license", "Licence"),
            ("Packager", "Packager"),
            ("Architecture", "Architecture"),
            ("repo", "Checkout"),
        ] {
            let val = if has(key) {
                extra[key].clone()
            } else if key == "category" {
                Value::String(item.category.clone())
            } else {
                Value::String(String::new())
            };
            if truthy(Some(&val)) {
                Win::detail_row(&og, label, &py_str(&val));
            }
        }
        if item.installed_ts != 0.0 {
            Win::detail_row(&og, &tr("Installed"), &local_time(item.installed_ts, "%d %b %Y, %H:%M")?);
        }
        if has("built_ts") {
            Win::detail_row(&og, &tr("Built"), &local_time(timestamp_of(extra.get("built_ts"))?, "%d %b %Y")?);
        }
        if has("files") {
            Win::detail_row(&og, &tr("Files"), &py_format_thousands(&extra["files"])?);
        }
        if has("url") {
            // subtitle=extra["url"]: a GObject string property takes only a str.
            let url = match &extra["url"] {
                Value::String(u) => u.clone(),
                other => return Err(format!("TypeError: url must be a str: {}", py_repr(other))),
            };
            let r = adw::ActionRow::builder().title(tr("Project page")).subtitle(&url).build();
            r.set_subtitle_selectable(true);
            let btn = gtk::Button::builder()
                .icon_name("adw-external-link-symbolic")
                .valign(gtk::Align::Center)
                .build();
            btn.add_css_class("flat");
            name(&btn, &format!("Open {url}"));
            let w = self.downgrade();
            btn.connect_clicked(move |_b| {
                if let Some(w) = w.upgrade() {
                    w.open_url(&url);
                }
            });
            r.add_suffix(&btn);
            og.add(&r);
        }
        page.add(&og);

        // --- relationships ---
        let deps = adw::PreferencesGroup::builder().title(tr("Dependencies")).build();
        let mut added = false;
        for (key, label) in [
            ("Depends On", "Requires"),
            ("Required By", "Required by"),
            ("Optional Deps", "Optional"),
            ("Provides", "Provides"),
            ("Conflicts With", "Conflicts with"),
        ] {
            if has(key) {
                // extra[key].split(): only a str has one.
                let Value::String(text) = &extra[key] else {
                    return Err(format!("AttributeError: {key} has no attribute 'split'"));
                };
                let row = adw::ActionRow::builder()
                    .title(label)
                    .subtitle(squeeze(text, 400))
                    .build();
                row.set_subtitle_lines(4);
                row.set_subtitle_selectable(true);
                deps.add(&row);
                added = true;
            }
        }
        if added {
            page.add(&deps);
        }

        let dialog = adw::Dialog::new();
        dialog.set_title(&app.name);
        dialog.set_content_width(620);
        dialog.set_content_height(720);

        let tv = adw::ToolbarView::new();
        let hb = adw::HeaderBar::new();

        if !app.update.is_empty() && item.source != inventory::SYSTEM {
            let up = gtk::Button::with_label(&tr("Update"));
            up.add_css_class("suggested-action");
            let (w, d, it) = (self.downgrade(), dialog.downgrade(), item.clone());
            up.connect_clicked(move |_b| {
                if let Some(d) = d.upgrade() {
                    d.close();
                }
                if let Some(w) = w.upgrade() {
                    w.update_from_detail(&it);
                }
            });
            hb.pack_end(&up);
        }
        tv.add_top_bar(&hb);

        let scroll = gtk::ScrolledWindow::builder().vexpand(true).build();
        scroll.set_child(Some(&page));
        tv.set_content(Some(&scroll));
        dialog.set_child(Some(&tv));
        dialog.present(Some(&self.window));
        Ok(())
    }

    pub fn detail_badges(&self, item: &AppItem) -> Vec<(String, &'static str)> {
        detail_badges_for(item)
    }

    /// A row whose text is taken literally.
    ///
    /// Adw rows parse title and subtitle as Pango markup, and every string
    /// put in one here was written by someone else -- a skill description, a
    /// marketplace listing, a package summary. An unescaped "&" or "<name>"
    /// makes the row render empty.
    ///
    /// The order matters: the text is parsed when it is assigned, so markup
    /// has to be switched off before the title and subtitle go in, not after.
    pub fn plain<R: PlainRow>(title: &str, subtitle: &str) -> R {
        Win::plain_with(title, subtitle, |_row: &R| {})
    }

    /// `_plain(cls, title, subtitle, **kwargs)`: `kwargs` stands in for the
    /// constructor keywords (`activatable=True`, ...). It runs on the new row
    /// before anything else, as the constructor applied them.
    pub fn plain_with<R: PlainRow>(title: &str, subtitle: &str, kwargs: impl FnOnce(&R)) -> R {
        let row = R::new_plain();
        kwargs(&row);
        row.set_use_markup(false);
        if !title.is_empty() {
            row.set_title(title);
        }
        if !subtitle.is_empty() {
            row.put_subtitle(subtitle);
        }
        row
    }

    pub fn detail_row<V: DetailValue + ?Sized>(group: &adw::PreferencesGroup, title: &str, value: &V) {
        // A row reading "Reviewed by: (blank)" says less than no row at all,
        // and the provenance group has several fields that only some entries
        // carry.
        let Some(value) = value.detail_text() else {
            return;
        };
        let row: adw::ActionRow = Win::plain(title, &value);
        row.set_subtitle_selectable(true);
        group.add(&row);
    }

    pub fn update_from_detail(&self, item: &AppItem) {
        if item.source == inventory::APPIMAGE {
            self.on_update_appimage(&gtk::Button::new(), &item.app);
        } else if item.source == inventory::LOCAL {
            self.on_git_pull(&item.app);
        } else {
            self.on_update_one(&item.app);
        }
    }

    pub fn rebuild_sort_menu<K: AsRef<str>, L: AsRef<str>>(&self, options: &[(K, L)]) {
        let menu = gio::Menu::new();
        for (key, label) in options {
            menu.append(Some(label.as_ref()), Some(&format!("win.sort::{}", key.as_ref())));
        }
        self.sort_btn.set_menu_model(Some(&menu));
    }

    /// Sort by a field that is no longer its own column.
    ///
    /// Selecting one replaces the column sorter; clicking a column header
    /// afterwards takes precedence again, because the MultiSorter still has
    /// the view's sorter first.
    pub fn on_sort(&self, action: &gio::SimpleAction, param: Option<&glib::Variant>) {
        let Some(param) = param else { return };
        let key = param.str().unwrap_or_default().to_string();
        action.set_state(param);
        if key == "default" {
            self.shared.borrow_mut().manual_sorter = None;
        } else {
            // getters[key]: an unknown key raised KeyError, leaving the
            // sorter as it was.
            if !SORT_KEYS.contains(&key.as_str()) {
                return;
            }
            let sorter = gtk::CustomSorter::new(move |a, b| match (AppItem::of(a), AppItem::of(b)) {
                (Some(a), Some(b)) => sort_cmp(&key, &a, &b).into(),
                _ => gtk::Ordering::Equal,
            });
            self.shared.borrow_mut().manual_sorter = Some(sorter.upcast());
        }
        self.rebuild_sorter();
    }

    pub fn rebuild_sorter(&self) {
        let combined = gtk::MultiSorter::new();
        // combined.append(None) raised TypeError, leaving the sorter as it was.
        let Some(view_sorter) = self.view.sorter() else { return };
        combined.append(view_sorter); // a clicked header wins
        let (manual, sorted) = {
            let sh = self.shared.borrow();
            (sh.manual_sorter.clone(), sh.sorted.clone())
        };
        if let Some(manual) = manual {
            combined.append(manual);
        }
        combined.append(self.priority_sorter.clone()); // then urgency, then name
        sorted.set_sorter(Some(&combined));
    }

    /// Set a sidebar count and how loudly it should read.
    ///
    /// Three levels, and only one of them is red. Every badge used to turn
    /// red as soon as any single package anywhere was security-critical,
    /// which painted Featured, Updates and AI the same alarming colour and
    /// left the actual security item indistinguishable from the rest.
    pub fn badge(label: &gtk::Label, count: usize, level: &str) {
        label.set_text(&if count > 0 {
            count.to_string()
        } else {
            String::new()
        });
        for css in ["is-warn", "is-crit"] {
            label.remove_css_class(css);
        }
        label.set_visible(count > 0);
        if count == 0 {
            return;
        }
        if level == "crit" {
            label.add_css_class("is-crit");
        } else if level == "warn" {
            label.add_css_class("is-warn");
        }
    }

    pub fn stat_row(&self, label: &str, value: &str, tooltip: &str) -> gtk::Box {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let name = gtk::Label::builder()
            .label(label)
            .xalign(0.0)
            .hexpand(true)
            .build();
        name.add_css_class("stat-key");
        let val = gtk::Label::builder().label(value).xalign(1.0).build();
        val.add_css_class("stat-val");
        row.append(&name);
        row.append(&val);
        if !tooltip.is_empty() {
            row.set_tooltip_text(Some(tooltip));
        }
        row
    }

    /// The numbers you would otherwise open a terminal to get.
    ///
    /// Err where Python's raised: a non-string `size` (size_mb calls
    /// `.split()` on it) or an unreadable uptime. The grid is left as it was
    /// at that point -- emptied, nothing attached.
    pub fn refresh_stats(&self) -> Result<(), String> {
        while let Some(child) = self.stats.first_child() {
            self.stats.remove(&child);
        }

        let items = self.store_objects();
        let total = Win::count_items(&items, |i| i.source != inventory::ORPHAN);
        let mut size = 0.0_f64;
        for o in &items {
            let Some(i) = AppItem::of(o) else { continue };
            // extra.get("size", "") goes to size_mb as it is.
            size += match i.app.extra.get("size") {
                None => inventory::size_mb(""),
                Some(Value::String(text)) => inventory::size_mb(text),
                Some(other) => {
                    return Err(format!(
                        "AttributeError: '{}' object has no attribute 'split'",
                        py_type_name(other)
                    ))
                }
            };
        }
        let explicit =
            Win::count_items(&items, |i| i.source == inventory::REPO || i.source == inventory::AUR);
        let foreign = Win::count_items(&items, |i| i.source == inventory::AUR);
        // Counting packages, not releases: summing release deltas across
        // date-versioned and epoch-versioned packages produced numbers in the
        // tens of thousands, which described nothing.
        let majors = Win::count_items(&items, |i| i.behind != 0 && i.behind_kind == "major");
        let pending = Win::count_items(&items, |i| i.has_update);
        let security = Win::count_items(&items, |i| i.critical == "security");

        let mut rows: Vec<(String, String, String)> = vec![
            (
                "Installed".into(),
                format!("{total}"),
                "Everything Almanac can see, orphans aside".into(),
            ),
            (
                "Disk".into(),
                crate::maintenance::fmt_size(size),
                "Installed size reported by the package manager".into(),
            ),
            (
                "Updates".into(),
                format!("{pending}"),
                "Packages, AppImages, plugins and agents with a newer version".into(),
            ),
            (
                "AUR".into(),
                format!("{foreign}"),
                format!("{foreign} of {explicit} repo and AUR packages are foreign"),
            ),
        ];
        if majors != 0 {
            rows.push((
                "Majors".into(),
                format!("{majors}"),
                "Packages at least one major version behind upstream".into(),
            ));
        }
        if security != 0 {
            rows.push((
                "Security".into(),
                format!("{security}"),
                "Network-facing packages with an update waiting".into(),
            ));
        }

        let findings = self.shared.borrow().findings.get();
        let free = crate::maintenance::reclaimable_mb(&findings);
        if free != 0.0 {
            rows.push((
                "Free-able".into(),
                crate::maintenance::fmt_size(free),
                "Caches and logs that can be cleared".into(),
            ));
        }

        let kernel = Win::kernel_version();
        if !kernel.is_empty() {
            // The version, not the full release string: "7.2.5", not
            // "7.2.5-3-omarchy" -- the rest is in the tooltip.
            rows.push((
                "Kernel".into(),
                kernel.split('-').next().unwrap_or("").to_string(),
                format!("Running kernel release {kernel}"),
            ));
        }

        let up = Win::uptime()?;
        if !up.is_empty() {
            rows.push(("Up".into(), up, "Uptime, since last boot".into()));
        }

        for (n, (label, value, tip)) in rows.iter().enumerate() {
            let n = n as i32;
            self.stats.attach(&self.stat_row(label, value, tip), n % 2, n / 2, 1, 1);
        }
        Ok(())
    }

    pub fn kernel_version() -> String {
        Win::kernel_version_for(crate::demo::active())
    }

    /// `_kernel_version` with `demo.active()` passed in, so a test can ask
    /// for the demo answer without setting ALMANAC_DEMO under other tests.
    fn kernel_version_for(demo: bool) -> String {
        if demo {
            return "6.18.5".to_string(); // a screenshot must not name this machine's
        }
        let uts = rustix::system::uname();
        let release = uts.release().to_string_lossy();
        release.split("-arch").next().unwrap_or("").to_string()
    }

    /// Err where Python's `_uptime` raised past its `except (OSError,
    /// ValueError)`: an empty /proc/uptime (IndexError), or a NaN/infinite
    /// value reaching `int()` outside the try.
    pub fn uptime() -> Result<String, String> {
        Win::uptime_for(crate::demo::active())
    }

    /// `_uptime` with `demo.active()` passed in (see `kernel_version_for`).
    fn uptime_for(demo: bool) -> Result<String, String> {
        if demo {
            return Ok("6d 2h".to_string());
        }
        // A file that is not UTF-8 fails to decode: a ValueError, caught.
        let Ok(text) = std::fs::read_to_string("/proc/uptime") else {
            return Ok(String::new());
        };
        Win::uptime_from(&text)
    }

    /// The part of `_uptime` after the read, on the file's text.
    fn uptime_from(text: &str) -> Result<String, String> {
        let Some(first) = text.split(py_space).find(|p| !p.is_empty()) else {
            return Err("IndexError: list index out of range".to_string());
        };
        let Ok(seconds) = first.parse::<f64>() else {
            return Ok(String::new()); // float() ValueError, caught
        };
        if seconds.is_nan() {
            return Err("ValueError: cannot convert float NaN to integer".to_string());
        }
        if seconds.is_infinite() {
            return Err("OverflowError: cannot convert float infinity to integer".to_string());
        }
        let total = seconds.trunc() as i128;
        let (days, rem) = (total.div_euclid(86400), total.rem_euclid(86400));
        let (hours, minutes) = ((rem / 60).div_euclid(60), (rem / 60).rem_euclid(60));
        if days != 0 {
            return Ok(format!("{days}d {hours}h"));
        }
        if hours != 0 {
            return Ok(format!("{hours}h {minutes}m"));
        }
        Ok(format!("{minutes}m"))
    }

    /// Per-category totals, so the sidebar answers "what needs me?" at rest.
    pub fn update_sidebar_counts(&self) {
        let items = self.store_objects();
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        counts.insert("updates".into(), Win::count_items(&items, |i| i.has_update));
        counts.insert(
            "apps".into(),
            Win::count_items(&items, |i| {
                (truthy(i.app.extra.get("is_app"))
                    || i.source == inventory::APPIMAGE
                    || i.source == inventory::LOCAL)
                    && i.source != inventory::ORPHAN
            }),
        );
        counts.insert("all".into(), Win::count_items(&items, |i| i.source != inventory::ORPHAN));
        for src in [
            inventory::SYSTEM,
            inventory::REPO,
            inventory::AUR,
            inventory::APPIMAGE,
            inventory::FLATPAK,
            inventory::LOCAL,
            inventory::AGENT,
            inventory::TOOL,
            inventory::ORPHAN,
        ] {
            counts.insert(src.into(), Win::count_items(&items, |i| i.source == src));
        }
        let count = |k: &str| counts.get(k).copied().unwrap_or(0);

        let (headers, rows) = {
            let sh = self.shared.borrow();
            let headers: Vec<(gtk::Image, Option<usize>)> = sh
                .group_rows
                .iter()
                .filter_map(|(gkey, header)| {
                    let header = header.as_ref()?;
                    let members = sh.group_members.get(gkey).cloned().unwrap_or_default();
                    let total = if sh.collapsed.contains(gkey) && members.len() > 1 {
                        Some(members.iter().map(|r| count(&r.key)).sum())
                    } else {
                        None
                    };
                    Some((header.chevron.clone(), total))
                })
                .collect();
            (headers, sh.sidebar_rows.clone())
        };
        for (chevron, total) in headers {
            match total {
                Some(total) => chevron.set_tooltip_text(Some(&format!("{total} items"))),
                None => chevron.set_tooltip_text(None),
            }
        }
        let security = Win::count_items(&items, |i| i.critical == "security");
        for row in rows {
            let key = row.key.as_str();
            if ["maintenance", "history", "plugins", "switchboard", inventory::AGENT, "featured", "themes"]
                .contains(&key)
            {
                continue; // their badges count findings, not packages
            }
            let n = count(key);
            // An empty category stays visible but recedes, so the layout does
            // not reshuffle as things are installed and removed.
            // "discover" has no inventory count of its own and "all"/"updates"
            // must stay reachable when empty, so none of the three is gated on
            // a count. Greying out the App Store is what made it unclickable.
            row.row.set_sensitive(
                n > 0
                    || [
                        "all",
                        "updates",
                        "discover",
                        "themes",
                        "plugins",
                        "switchboard",
                        "maintenance",
                        "featured",
                        inventory::AGENT,
                    ]
                    .contains(&key),
            );
            // Red means "a security update is waiting", nothing else. Pending
            // updates are a number, not an emergency.
            Win::badge(&row.count, n, if key == "updates" && security != 0 { "crit" } else { "" });
        }
    }

    // ---- filtering -------------------------------------------------------
    /// `_match`: the row filter (`match` is a Rust keyword, hence `r#`).
    ///
    /// Borrow hazard: GTK runs this synchronously, once per row, from inside
    /// `filter.changed()`, `store.append()`, `store.remove_all()` and
    /// `set_model()` on anything fed by `self.filter`. It takes
    /// `shared.borrow()`, so calling any of those while holding
    /// `shared.borrow_mut()` panics with a BorrowError. Clone the store or
    /// filter out of `shared`, drop the borrow, then call (as
    /// `filter_changed`, `apply` and `show_discover` do).
    pub fn r#match(&self, item: &AppItem) -> bool {
        // Read in place: this runs once per row on every filter pass.
        let sh = self.shared.borrow();
        match_item(&sh.filter_key, &sh.query, item)
    }

    /// Query the repos and the AUR for what the search box says.
    ///
    /// Debounced: every keystroke would otherwise be a pacman -Ss plus an AUR
    /// round trip.
    pub fn run_discover(&self) {
        let old = {
            let mut sh = self.shared.borrow_mut();
            sh.discovering = true;
            sh.discover_timer.take()
        };
        if let Some(old) = old {
            old.remove();
        }
        let w = self.downgrade();
        let id = glib::timeout_add_local(std::time::Duration::from_millis(450), move || {
            match w.upgrade() {
                Some(w) => w.discover_now(),
                None => glib::ControlFlow::Break,
            }
        });
        self.shared.borrow_mut().discover_timer = Some(id);
    }

    pub fn discover_now(&self) -> glib::ControlFlow {
        self.shared.borrow_mut().discover_timer = None;
        let query = py_strip(&self.search.text()).to_string();
        let browsing = query.chars().count() < 2;
        self.summarize(
            &if browsing {
                tr("Loading the plugin marketplace…")
            } else {
                format!("Searching for “{query}”…")
            },
            None,
        );

        let q = query.clone();
        // What the searches raise ends the Python's worker before its
        // idle_add: the results are never shown.
        let work = move || -> Result<Vec<inventory::App>, String> {
            if browsing {
                // Nothing typed: show the marketplace itself. Packages are
                // excluded here -- 15,669 of them is a database dump, not a
                // storefront, and there is no popularity signal to rank them.
                inventory::try_search_plugins("", 200)
            } else {
                let mut plugins = inventory::try_search_plugins(&q, 25)?;
                let packages = inventory::try_search_available(&q, 60)?;
                // Plugins lead: on an Omarchy box they are the likelier intent,
                // and the package list is long enough to bury them otherwise.
                plugins.extend(packages);
                Ok(plugins)
            }
        };
        let w = self.downgrade();
        spawn_bg(work, move |results| match results {
            Ok(results) => {
                if let Some(w) = w.upgrade() {
                    w.show_discover(&query, results);
                }
            }
            Err(e) => eprintln!("almanac: {e}"),
        });
        glib::ControlFlow::Break
    }

    pub fn show_discover(&self, query: &str, results: Vec<inventory::App>) {
        let (discovering, store) = {
            let sh = self.shared.borrow();
            (sh.discovering, sh.store.clone())
        };
        if !discovering {
            return; // user navigated away mid-search
        }
        store.remove_all();
        for a in &results {
            store.append(&AppItem::new(a.clone()).boxed());
        }
        self.filter_changed();
        if query.is_empty() {
            // len(sources or []): a falsy value counts as the empty list; a
            // truthy one without a len() (a number, True) raised TypeError
            // out of _show_discover, before the summary.
            let sources = inventory::plugin_registry(false).get("sources").cloned();
            let total = match sources {
                Some(v) if !truthy(Some(&v)) => 0,
                None => 0,
                Some(Value::Array(a)) => a.len(),
                Some(Value::Object(o)) => o.len(),
                Some(Value::String(s)) => s.chars().count(),
                Some(other) => {
                    eprintln!(
                        "almanac: _show_discover: TypeError: object of type '{}' has no len()",
                        py_type_name(&other)
                    );
                    return;
                }
            };
            self.summarize(
                &format!(
                    "Showing {} of {total} Omarchy plugins · search to include repo and AUR packages",
                    results.len()
                ),
                None,
            );
        } else {
            self.summarize(
                &if !results.is_empty() {
                    format!("{} installable result(s) for “{query}”", results.len())
                } else {
                    format!("Nothing found for “{query}”")
                },
                None,
            );
        }
        self.update_btn.set_sensitive(false);
    }

    pub fn on_search(&self, entry: &gtk::SearchEntry) {
        let (page, filter_key) = {
            let sh = self.shared.borrow();
            (sh.page_or(""), sh.filter_key.clone())
        };
        if page == "plugins" {
            self.plugins_filter(&entry.text());
            return;
        }
        if page == "switchboard" {
            self.switchboard_filter(&entry.text());
            return;
        }
        if filter_key == "discover" {
            if self.content_stack.visible_child_name().as_deref() == Some("store") {
                self.store_queue();
            } else {
                self.run_discover();
            }
            return;
        }
        self.shared.borrow_mut().query = py_strip(&entry.text()).to_lowercase();
        self.filter_changed();
        if let Err(e) = self.update_status() {
            eprintln!("almanac: on_search: {e}");
        }
    }

    /// Header rows fold their group; everything else is a category.
    pub fn on_sidebar_activated(&self, _list: &gtk::ListBox, row: &gtk::ListBoxRow) {
        let group = {
            let sh = self.shared.borrow();
            sh.group_rows
                .iter()
                .find_map(|(_k, h)| h.as_ref().filter(|h| h.row == *row).map(|h| h.group.clone()))
        };
        let Some(group) = group else { return };
        self.toggle_group(&group);
    }

    pub fn toggle_group(&self, gkey: &str) {
        {
            let mut sh = self.shared.borrow_mut();
            let collapsed = sh.collapsed.contains(gkey);
            if collapsed {
                sh.collapsed.remove(gkey);
            } else {
                sh.collapsed.insert(gkey.to_string());
            }
        }
        self.apply_group_state(gkey);
    }

    pub fn apply_group_state(&self, gkey: &str) {
        let plan = {
            let sh = self.shared.borrow();
            group_state(&sh.group_rows, &sh.group_members, &sh.collapsed, &sh.filter_key, gkey, |r| {
                r.key.as_str()
            })
        };
        let Some(plan) = plan else {
            return; // ungrouped primaries never fold
        };
        plan.header.chevron.set_icon_name(Some(plan.icon));
        for (row, visible) in plan.members {
            row.row.set_visible(visible);
        }
    }

    pub fn apply_all_group_states(&self) {
        let keys: Vec<String> =
            self.shared.borrow().group_rows.iter().map(|(k, _)| k.clone()).collect();
        for gkey in keys {
            self.apply_group_state(&gkey);
        }
    }

    /// Go to a category, whether or not its row is already selected.
    ///
    /// GtkListBox emits nothing when you select the row that is already
    /// selected, so the page never got applied in that case -- which is
    /// exactly what happens when a restored category and `--page` name the
    /// same destination, or when another path has already moved the
    /// selection. Applying it directly makes the call mean what it says.
    pub fn select_category(&self, key: &str) {
        let row = self.shared.borrow().sidebar_row(key).cloned();
        let Some(row) = row else { return };
        self.shared.borrow_mut().collapsed.remove(&row.group);
        self.apply_all_group_states();
        // _on_sidebar_selected ignores its list argument, so a selected row
        // is applied whatever its parent is.
        if row.row.is_selected() {
            self.sidebar_selected(row.row.upcast_ref());
        } else if let Some(list) = row.row.parent().and_downcast::<gtk::ListBox>() {
            list.select_row(Some(&row.row));
        }
    }

    pub fn on_sidebar_selected(&self, _list: &gtk::ListBox, row: Option<&gtk::ListBoxRow>) {
        let Some(row) = row else {
            return; // selection cleared during a rebuild
        };
        self.sidebar_selected(row);
    }

    /// The body of `_on_sidebar_selected`, which never reads its list.
    fn sidebar_selected(&self, row: &gtk::ListBoxRow) {
        let key = {
            let sh = self.shared.borrow();
            sh.sidebar_rows
                .iter()
                .find(|r| r.row.upcast_ref::<gtk::ListBoxRow>() == row)
                .map(|r| r.key.clone())
        };
        let Some(key) = key else { return };
        self.shared.borrow_mut().filter_key = key.clone();
        // A folded group keeps only the selected row showing; once the
        // selection moves on, that row folds away too. Without this a row
        // kept visible because it was selected stayed out after leaving it.
        self.apply_all_group_states();
        if self.shared.borrow().narrow {
            let split = self.split.clone();
            glib::idle_add_local_once(move || split.set_show_sidebar(false));
        }
        if key == "featured" {
            self.leave_store();
            self.show_page("featured", "");
            let first = !self.shared.borrow().feat_loaded_once;
            if first {
                self.shared.borrow_mut().feat_loaded_once = true;
                self.feat_reload();
            } else {
                let shelves = self.shared.borrow().shelves.clone();
                self.feat_show(shelves);
            }
            return;
        }

        if key == inventory::AGENT {
            self.leave_store();
            self.show_page(inventory::AGENT, "");
            let first = !self.shared.borrow().ai_loaded_once;
            if first {
                self.shared.borrow_mut().ai_loaded_once = true;
                self.ai_reload();
            } else {
                let survey = self.shared.borrow().ai_survey.clone();
                self.ai_show(&survey, true);
            }
            return;
        }

        if key == "history" {
            self.leave_store();
            self.show_page("history", "");
            self.history_reload();
            return;
        }

        if key == "plugins" {
            self.leave_store();
            self.show_page("plugins", "");
            self.plugins_reload();
            return;
        }

        if key == "switchboard" {
            self.leave_store();
            self.show_page("switchboard", "");
            self.switchboard_reload();
            return;
        }

        if key == "maintenance" {
            self.leave_store();
            self.show_page("maintenance", "");
            let first = !self.shared.borrow().maint_loaded_once;
            if first {
                self.shared.borrow_mut().maint_loaded_once = true;
                self.maint_reload();
            } else {
                let findings = self.shared.borrow().findings.get();
                self.maint_show(findings);
            }
            return;
        }

        if key == "discover" || key == "themes" {
            self.shared.borrow_mut().discovering = true;
            self.show_page(&key, "");
            self.search.grab_focus();
            let first = !self.shared.borrow().store_loaded_once;
            if first {
                self.shared.borrow_mut().store_loaded_once = true;
                self.store_reload(false);
            } else {
                self.pin_market(&key);
                self.store_refresh();
            }
            return;
        }
        if self.leave_store() {
            self.title.set_title(&self.page_title(&key, "Software"));
            return;
        }
        self.show_page("packages", &key);
        self.filter_changed();
        if let Err(e) = self.update_status() {
            eprintln!("almanac: _on_sidebar_selected: {e}");
        }
    }

    /// Fold or unfold for the window's width.
    pub fn set_narrow(&self, narrow: bool) {
        self.shared.borrow_mut().narrow = narrow;
        // Collapsed, the sidebar is an overlay that should start hidden;
        // expanded, it is furniture and should be back.
        self.split.set_show_sidebar(!narrow);
        self.sidebar_toggle.set_active(!narrow);
        self.sidebar_toggle.set_tooltip_text(Some(&if narrow {
            tr("Show categories")
        } else {
            tr("Hide categories")
        }));
        // One column of cards is the right answer on a narrow window; four
        // would each be 110px wide.
        let grid = self.shared.borrow().store_grid.clone();
        if let Some(grid) = grid {
            grid.set_max_columns(if narrow { 2 } else { 4 });
        }
    }

    pub fn select_category_and_close(&self, key: &str) {
        self.select_category(key);
        if self.shared.borrow().narrow {
            self.split.set_show_sidebar(false);
        }
    }

    pub fn page_title(&self, key: &str, fallback: &str) -> String {
        let row = self.shared.borrow().sidebar_row(key).map(|r| r.row.clone());
        match row {
            Some(row) => row.title().to_string(),
            None => fallback.to_string(),
        }
    }

    /// Apply one page's chrome. The only place that touches it.
    pub fn show_page(&self, key: &str, category: &str) {
        let Some(spec) = page_spec(key).or_else(|| page_spec("packages")) else { return };
        self.shared.borrow_mut().page = Some(key.to_string());
        self.content_stack.set_visible_child_name(spec.view);
        let title = tr(spec.title);
        self.title.set_title(&if !category.is_empty() {
            self.page_title(category, &title)
        } else {
            title
        });
        let drawer = spec.filters;
        self.sort_btn.set_visible(spec.sort || drawer);
        name(
            &self.sort_btn,
            &if drawer { tr("Filter and sort the marketplace") } else { tr("Sort by") },
        );
        if drawer {
            self.sort_btn.set_menu_model(None::<&gio::MenuModel>);
            // Python builds the drawer in __init__, so the page change always
            // runs to the end; if the store section has not built it yet the
            // button simply has no popover until it does.
            let drawer_widget = self.shared.borrow().store_drawer.clone();
            self.sort_btn.set_popover(drawer_widget.as_ref());
        } else {
            self.sort_btn.set_popover(None::<&gtk::Popover>);
            self.rebuild_sort_menu(&sorts_installed());
        }
        self.search_bar.set_visible(spec.search.is_some());
        if let Some(search) = spec.search {
            if !search.is_empty() {
                let text = if spec.search_translated { tr(search) } else { search.to_string() };
                self.search.set_placeholder_text(Some(&text));
            }
        }
        self.store_back.set_visible(false);
        if spec.view == "store" {
            self.pin_market(key);
        }
        self.sync_primary();
    }

    /// Re-read whatever the current page shows. One action, every page.
    pub fn refresh_page(&self) {
        let page = self.shared.borrow().page_or("packages");
        match page.as_str() {
            "featured" => self.feat_reload(),
            "discover" => self.store_reload(true),
            "maintenance" => self.maint_reload(),
            "history" => self.history_reload(),
            "plugins" => self.plugins_reload(),
            "switchboard" => self.switchboard_reload(),
            p if p == inventory::AGENT => self.ai_reload(),
            _ => self.reload(),
        }
    }

    /// `getattr(self, "_store_markets", [""])`.
    ///
    /// The attribute always exists here: `Shared` starts it at `[""]`, the
    /// fallback's value, so it is read as it is (an empty list stays empty).
    fn store_market_keys(&self) -> Vec<String> {
        self.shared.borrow().store_markets.clone()
    }

    /// Point the marketplace filter at whatever this page is about.
    ///
    /// The App Store shows every catalogue; Themes is the same view with one
    /// of them selected. Changing the filter by hand afterwards re-titles the
    /// page, so the header never claims to be showing something it is not.
    pub fn pin_market(&self, page: &str) {
        let has_plugins = !self.shared.borrow().plugins.is_empty();
        let filters_for = self.shared.borrow().filters_for;
        if has_plugins && filters_for != Some(page == "themes") {
            self.store_filters();
        }
        let wanted = page_spec(page).and_then(|s| s.market).unwrap_or("");
        let keys = self.store_market_keys();
        let Some(market) = self.shared.borrow().store_market.clone() else { return };
        // Themes is one catalogue already; the App Store chooses among the rest.
        market.set_visible(page != "themes" && keys.len() > 2);
        let index = keys.iter().position(|k| k == wanted).unwrap_or(0) as u32;
        if market.selected() != index {
            self.shared.borrow_mut().pinning = true;
            market.set_selected(index);
            self.shared.borrow_mut().pinning = false;
        }
    }

    /// Say what the store is showing when the marketplace filter changes.
    ///
    /// This only edits the title. It used to also select the matching
    /// sidebar row, which re-entered the selection handler and rebuilt the
    /// page -- so opening Themes, then letting the catalogue finish loading,
    /// put you back on the App Store. Which page you are on is the sidebar's
    /// business and nothing else's.
    pub fn retitle_store(&self) {
        if self.content_stack.visible_child_name().as_deref() != Some("store") {
            return;
        }
        if self.shared.borrow().pinning {
            return;
        }
        let keys = self.store_market_keys();
        if keys.len() <= 1 {
            return;
        }
        let Some(store_market) = self.shared.borrow().store_market.clone() else { return };
        let i = store_market.selected() as usize;
        let market = keys.get(i).cloned().unwrap_or_default();
        let page = self.shared.borrow().page_or("discover");
        if market == "themes" {
            self.title.set_title(&tr("Themes"));
        } else if page == "themes" && market.is_empty() {
            // Un-pinned from the Themes page: it is the whole store now.
            self.title.set_title(&tr("App Store"));
        } else {
            let title = page_spec(&page).map(|s| tr(s.title)).unwrap_or_else(|| "App Store".into());
            self.title.set_title(&title);
        }
    }

    /// Put the current page's main action in the header button.
    pub fn sync_primary(&self) {
        let page = self.shared.borrow().page_or("packages");
        let action = self.primary_actions().remove(&page);
        self.shared.borrow_mut().primary = action.clone();
        let Some(action) = action else {
            self.update_btn.set_visible(false);
            return;
        };
        // A disabled primary button ("Nothing to reclaim", "Plugins current")
        // takes the header's most prominent place to say there is nothing to
        // do; the subtitle already says that.
        if !action.enabled {
            self.update_btn.set_visible(false);
            return;
        }
        self.update_btn.set_visible(true);
        self.update_btn.set_label(&action.label);
        self.update_btn.set_sensitive(action.enabled);
        self.update_btn.set_tooltip_text(Some(&action.tooltip));
    }

    /// (label, enabled, tooltip, handler) for each page that has one.
    pub fn primary_actions(&self) -> BTreeMap<String, PrimaryAction> {
        let pending = Win::count_items(&self.store_objects(), |i| i.has_update);
        let (harnesses, free, plugin_updates) = {
            let sh = self.shared.borrow();
            let harnesses = sh
                .ai_survey
                .iter()
                .flat_map(|(_t, _d, es)| es.iter())
                .filter(|e| {
                    e.kind == crate::ai::HARNESS
                        && !e.update.is_empty()
                        && e.action.as_ref().is_some_and(|a| !a.is_empty())
                })
                .count();
            (harnesses, crate::maintenance::reclaimable_mb(&sh.findings.get()), sh.plugin_updates)
        };
        let mut out = BTreeMap::new();
        out.insert(
            "packages".to_string(),
            PrimaryAction {
                label: if pending != 0 {
                    format!("Update all ({pending})")
                } else {
                    "Up to date".to_string()
                },
                enabled: pending != 0,
                tooltip: "Runs `omarchy update` in a terminal, which is where it can ask for \
                          your password"
                    .to_string(),
                handler: Rc::new(|w: &Win| w.on_update_all()),
            },
        );
        out.insert(
            inventory::AGENT.to_string(),
            PrimaryAction {
                label: if harnesses != 0 {
                    format!("Update {harnesses} agent CLI{}", if harnesses != 1 { "s" } else { "" })
                } else {
                    "Agents current".to_string()
                },
                enabled: harnesses != 0,
                tooltip: "Update every agent CLI with a newer release that knows how to update \
                          itself"
                    .to_string(),
                handler: Rc::new(|w: &Win| w.ai_update_all()),
            },
        );
        out.insert(
            "plugins".to_string(),
            PrimaryAction {
                label: if plugin_updates != 0 {
                    format!("Update plugins ({plugin_updates})")
                } else {
                    tr("Plugins current")
                },
                enabled: plugin_updates != 0,
                tooltip: tr(
                    "Update every plugin with a clean update waiting, within the scope set in \
                     Preferences",
                ),
                handler: Rc::new(|w: &Win| w.on_update_plugins()),
            },
        );
        out.insert(
            "maintenance".to_string(),
            PrimaryAction {
                label: if free != 0.0 {
                    format!("Reclaim {}", crate::maintenance::fmt_size(free))
                } else {
                    "Nothing to reclaim".to_string()
                },
                enabled: free > 0.0,
                tooltip: "Trim the package cache, clear the AUR build cache and vacuum the \
                          journal — none of it touches anything you created"
                    .to_string(),
                handler: Rc::new(|w: &Win| w.reclaim_space()),
            },
        );
        out
    }

    pub fn on_primary(&self, _button: &gtk::Button) {
        let primary = self.shared.borrow().primary.clone();
        if let Some(primary) = primary {
            (primary.handler)(self);
        }
    }

    /// A transient result. Replaces four separate dim status labels.
    ///
    /// `timeout` defaulted to 5 in Python (`SAY_TIMEOUT`).
    pub fn say(&self, message: &str, timeout: u32) {
        let toast = adw::Toast::builder().title(message).timeout(timeout).build();
        self.toasts.add_toast(toast);
    }

    /// Restate the active Omarchy palette as libadwaita's named colours.
    ///
    /// Absence is a valid state: with no Omarchy theme the provider is
    /// emptied and GTK's own theme shows through unmodified.
    ///
    /// `Err` where the Python raises (out of `theming`), before the
    /// provider is touched.
    pub fn apply_theme(&self) -> Result<(), String> {
        let palette = crate::theming::try_load_palette()?;
        let mut css = crate::theming::try_css_for(&palette)?;
        if palette.is_empty() {
            // No Omarchy palette: measure the pills against whatever theme GTK
            // is showing, so they read as well there as on an Omarchy one.
            let fills = self.theme_fills()?;
            css += &crate::theming::pill_css(&fills, &[]);
            let get = |k: &str| fills.get(k).cloned().unwrap_or_default();
            css += &crate::theming::try_pick_css(&get("card"), &get("fg"), &get("accent"))?;
        }
        // load_from_data raised GLib.Error on a parse failure in older GTK;
        // GTK 4.12+ reports parse errors through a signal and never fails.
        self.theme_provider.load_from_string(&css);
        let dark = crate::theming::is_dark(&palette);
        if let Some(dark) = dark {
            adw::StyleManager::default().set_color_scheme(if dark {
                adw::ColorScheme::ForceDark
            } else {
                adw::ColorScheme::ForceLight
            });
        }
        Ok(())
    }

    /// The active GTK theme's status fills, as hex. lookup_color is the
    /// only way GTK 4 exposes a named colour; it is deprecated, not gone.
    #[allow(deprecated)]
    pub fn theme_fills(&self) -> Result<BTreeMap<String, String>, String> {
        let mut out = BTreeMap::new();
        let ctx = self.window.style_context();
        // Translucent colours are flattened onto the window first:
        // libadwaita's dark card is white at 8%, and read as plain white
        // it measured "black text" onto a dark card.
        let base = ctx
            .lookup_color("window_bg_color")
            .map(|u| (f64::from(u.red()), f64::from(u.green()), f64::from(u.blue())));
        for (role, color) in [
            ("error", "error_bg_color"),
            ("warning", "warning_bg_color"),
            ("success", "success_bg_color"),
            ("accent", "accent_bg_color"),
            ("card", "card_bg_color"),
            ("fg", "window_fg_color"),
        ] {
            if let Some(rgba) = ctx.lookup_color(color) {
                out.insert(
                    role.to_string(),
                    crate::theming::try_flatten(
                        (
                            f64::from(rgba.red()),
                            f64::from(rgba.green()),
                            f64::from(rgba.blue()),
                            f64::from(rgba.alpha()),
                        ),
                        base,
                    )?,
                );
            }
        }
        Ok(out)
    }

    pub fn on_theme_changed(&self) {
        // The theme is swapped as several filesystem operations, so this
        // fires a handful of times; coalesce into one restyle.
        let old = self.packages.borrow_mut().theme_timer.take();
        if let Some(old) = old {
            old.remove();
        }
        let w = self.downgrade();
        let id = glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
            match w.upgrade() {
                Some(w) => w.theme_tick(),
                None => glib::ControlFlow::Break,
            }
        });
        self.packages.borrow_mut().theme_timer = Some(id);
    }

    pub fn theme_tick(&self) -> glib::ControlFlow {
        self.packages.borrow_mut().theme_timer = None;
        // What raises in here ends the Python's timeout callback, logged.
        if let Err(e) = self.apply_theme() {
            eprintln!("almanac: {e}");
            return glib::ControlFlow::Break;
        }
        let has_hero = self.today.borrow().hero.is_some();
        if has_hero {
            self.refresh_hero();
        }
        match crate::theming::try_describe() {
            Ok(text) => self.say(&text, 3),
            Err(e) => eprintln!("almanac: {e}"),
        }
        glib::ControlFlow::Break
    }

    /// The one place the window says it is doing something.
    pub fn busy(&self, working: bool) {
        self.busy.set_visible(working);
        if working {
            self.busy.start();
        } else {
            self.busy.stop();
        }
    }

    /// The standing one-line description of what is on screen.
    ///
    /// Scoped to a page because the loaders are asynchronous: the package
    /// scan finishing half a second after you opened the App Store used to
    /// overwrite the store's summary with "129 installed · showing 0".
    pub fn summarize(&self, text: &str, page: Option<&str>) {
        if let Some(page) = page {
            if page != self.shared.borrow().page_or("packages") {
                return;
            }
        }
        self.title.set_subtitle(text);
    }

    /// Put the installed inventory back. True if the store was showing.
    pub fn leave_store(&self) -> bool {
        if !self.shared.borrow().discovering {
            return false;
        }
        self.shared.borrow_mut().discovering = false;
        self.show_page("packages", "");
        self.rebuild_sort_menu(&sorts_installed());
        self.shared.borrow_mut().manual_sorter = None;
        self.rebuild_sorter();
        self.reload();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    const SRC: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/", file!()));
    const CSS: &str = include_str!("../../data/style.css");

    fn item(name: &str, source: &str, extra: Value) -> AppItem {
        let mut app = inventory::App::new(name, "1", source);
        if let Value::Object(m) = extra {
            app.extra = m;
        }
        AppItem::new(app)
    }

    /// The code part of this file, without its tests.
    fn code() -> &'static str {
        SRC.split("#[cfg(test)]").next().unwrap_or(SRC)
    }

    /// test_sidebar_stats_are_synthetic
    #[test]
    fn sidebar_stats_are_synthetic() {
        // demo.active() is passed in rather than set in the environment:
        // other tests in this binary read ALMANAC_DEMO concurrently.
        let kernel = Win::kernel_version_for(true);
        let uptime = Win::uptime_for(true).unwrap_or_default();
        let real = rustix::system::uname().release().to_string_lossy().to_string();
        let real = real.split('-').next().unwrap_or("").to_string();
        if !real.is_empty() {
            assert!(!kernel.contains(&real));
        }
        assert_eq!(uptime, "6d 2h");
    }

    /// test_plugins_leave_applications_and_local_builds
    #[test]
    fn plugins_leave_applications_and_local_builds() {
        let mut plugin = item("p", inventory::LOCAL, serde_json::json!({"plugin_id": "p"}));
        plugin.has_update = true;
        plugin.haystack = String::new();
        let mut build = item("b", inventory::LOCAL, serde_json::json!({}));
        build.haystack = String::new();
        for (key, want) in
            [("apps", false), (inventory::LOCAL, false), ("all", true), ("updates", true)]
        {
            assert_eq!(match_item(key, "", &plugin), want, "{key}");
        }
        assert!(match_item(inventory::LOCAL, "", &build));
    }

    #[test]
    fn installable_rows_always_match_and_orphans_stay_on_their_page() {
        let avail = item("x", inventory::AVAILABLE, serde_json::json!({}));
        assert!(match_item("updates", "nothing", &avail));
        let orphan = item("o", inventory::ORPHAN, serde_json::json!({}));
        assert!(!match_item("all", "", &orphan));
        assert!(match_item(inventory::ORPHAN, "", &orphan));
        let repo = item("Firefox", inventory::REPO, serde_json::json!({}));
        assert!(match_item("all", "firefox", &repo));
        assert!(!match_item("all", "chromium", &repo));
        assert!(!match_item(inventory::AUR, "", &repo));
    }

    /// test_a_folded_groups_selected_row_folds_once_left: drives
    /// `_apply_all_group_states` over group_rows and group_members, as the
    /// Python does with mocks, and records each set_visible.
    #[test]
    fn a_folded_groups_selected_row_folds_once_left() {
        #[derive(Clone, Debug, PartialEq)]
        struct Row(&'static str);
        let group_rows = vec![("primary".to_string(), None::<()>), ("library".to_string(), Some(()))];
        let mut group_members = BTreeMap::new();
        group_members.insert("library".to_string(), vec![Row("apps"), Row("plugins")]);
        let collapsed: BTreeSet<String> = ["library".to_string()].into_iter().collect();
        // _apply_all_group_states: every group in turn; returns the last
        // set_visible each row got.
        let apply_all = |filter_key: &str| {
            let mut seen: BTreeMap<&'static str, bool> = BTreeMap::new();
            for (gkey, _) in &group_rows {
                if let Some(plan) =
                    group_state(&group_rows, &group_members, &collapsed, filter_key, gkey, |r| r.0)
                {
                    assert_eq!(plan.icon, "pan-end-symbolic");
                    for (row, visible) in plan.members {
                        seen.insert(row.0, visible);
                    }
                }
            }
            seen
        };
        let seen = apply_all("plugins");
        assert_eq!(seen.get("plugins"), Some(&true)); // selected: shown
        assert_eq!(seen.get("apps"), Some(&false));
        let seen = apply_all("updates"); // navigated away
        assert_eq!(seen.get("plugins"), Some(&false)); // now folded
        // Ungrouped primaries never fold; an unfolded group shows everyone.
        assert!(group_state(&group_rows, &group_members, &collapsed, "x", "primary", |r| r.0).is_none());
        let open = group_state(&group_rows, &group_members, &BTreeSet::new(), "x", "library", |r| r.0)
            .map(|p| (p.icon, p.members.iter().all(|(_, v)| *v)));
        assert_eq!(open, Some(("pan-down-symbolic", true)));
        let body = code()
            .split("pub fn on_sidebar_selected")
            .nth(1)
            .and_then(|s| s.split("pub fn set_narrow").next())
            .unwrap_or("");
        assert!(body.contains("self.apply_all_group_states()"));
    }

    #[test]
    fn values_print_as_python_str_and_format() {
        assert_eq!(py_str(&serde_json::json!(["GPL", "MIT"])), "['GPL', 'MIT']");
        assert_eq!(py_str(&serde_json::json!({"a": 1, "b": [true, null]})), "{'a': 1, 'b': [True, None]}");
        assert_eq!(py_str(&serde_json::json!(["it's"])), "[\"it's\"]");
        assert_eq!(py_str(&serde_json::json!(1.5)), "1.5");
        assert_eq!(py_format_thousands(&serde_json::json!(1234)).as_deref(), Ok("1,234"));
        assert_eq!(py_format_thousands(&serde_json::json!(-1234567)).as_deref(), Ok("-1,234,567"));
        assert_eq!(py_format_thousands(&serde_json::json!(1234.0)).as_deref(), Ok("1,234.0"));
        assert_eq!(py_format_thousands(&serde_json::json!(1e16)).as_deref(), Ok("1e+16"));
        assert_eq!(py_format_thousands(&serde_json::json!(true)).as_deref(), Ok("1"));
        assert!(py_format_thousands(&serde_json::json!("12")).is_err());
        assert!(timestamp_of(Some(&serde_json::json!("1700000000"))).is_err());
        assert_eq!(timestamp_of(Some(&serde_json::json!(5))), Ok(5.0));
        assert!(local_time(f64::NAN, "%Y").is_err());
        assert!(local_time(1e300, "%Y").is_err());
        assert_eq!(Value::Null.detail_text(), None);
        assert_eq!(serde_json::json!("").detail_text(), None);
        assert_eq!(serde_json::json!(0).detail_text().as_deref(), Some("0"));
        assert_eq!(serde_json::json!(false).detail_text().as_deref(), Some("False"));
        assert_eq!("".detail_text(), None);
    }

    #[test]
    fn int_and_strip_follow_python() {
        let int = |s: &str| py_int(s).map(|n| (n.neg, n.mag));
        assert_eq!(int("20240102"), Some((false, "20240102".into())));
        assert_eq!(int("2024_01_02"), Some((false, "20240102".into())));
        assert_eq!(int(" +007 "), Some((false, "7".into())));
        assert_eq!(int("-0"), Some((false, "0".into())));
        assert_eq!(int("\u{0662}\u{0660}"), Some((false, "20".into()))); // Arabic-Indic
        assert_eq!(int("1__2"), None);
        assert_eq!(int("_1"), None);
        assert_eq!(int("1_"), None);
        assert_eq!(int(""), None);
        assert_eq!(int("1.0"), None);
        let big = "9".repeat(60);
        assert!(py_int(&big) > py_int("1"));
        assert!(py_int(&big).map(|n| n.negated()) < py_int("-1"));
        assert_eq!(py_strip("\u{1c} ab \u{1f}\n"), "ab");
        let mut a = AppItem { added: "2024_01_02".into(), ..Default::default() };
        let b = AppItem { added: "2024-03-01".into(), ..Default::default() };
        assert_eq!(sort_cmp("trending", &a, &b), Ordering::Greater);
        a.added = "\u{0662}\u{0660}\u{0662}\u{0665}\u{0660}\u{0661}\u{0660}\u{0661}".into(); // 20250101
        assert_eq!(sort_cmp("trending", &a, &b), Ordering::Less);
    }

    #[test]
    fn detail_badges_and_versions() {
        let mut it = item("x", inventory::ORPHAN, serde_json::json!({"critical": "security",
                                                                       "stale": "flagged",
                                                                       "advice": "keep"}));
        it.git_dirty = true;
        let got: Vec<(String, &str)> = detail_badges_for(&it);
        assert_eq!(
            got,
            vec![
                ("security update".to_string(), "pill-crit"),
                ("flagged out of date".to_string(), "pill-warn"),
                ("local edits".to_string(), "pill-warn"),
                ("orphan · keep".to_string(), "pill-warn"),
            ]
        );
        let mut it = AppItem::default();
        it.app.update = "2.0".into();
        it.behind = 2;
        it.behind_kind = "major".into();
        assert_eq!(available_label(&it), "2.0  (2 majors behind)");
        it.behind = 1;
        assert_eq!(available_label(&it), "2.0  (1 major behind)");
        it.behind_kind = "rel".into();
        assert_eq!(available_label(&it), "2.0  (rebuild only)");
    }

    #[test]
    fn files_get_thousands_separators() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1234), "1,234");
        assert_eq!(thousands(1234567), "1,234,567");
        assert_eq!(thousands(-1234), "-1,234");
        assert_eq!(squeeze("a  b\n c", 3), "a b");
    }

    #[test]
    fn sort_getters_order_like_the_python() {
        let mut a = AppItem { name: "Alpha".into(), ..Default::default() };
        let mut b = AppItem { name: "beta".into(), ..Default::default() };
        assert_eq!(sort_cmp("name", &a, &b), Ordering::Less);
        assert_eq!(sort_cmp("name_desc", &a, &b), Ordering::Greater);
        a.size_mb = 10.0;
        b.size_mb = 20.0;
        assert_eq!(sort_cmp("size", &a, &b), Ordering::Greater); // biggest first
        a.category = String::new();
        b.category = "Office".into();
        assert_eq!(sort_cmp("category", &a, &b), Ordering::Greater); // blanks last
        a.verified = true;
        assert_eq!(sort_cmp("popular", &a, &b), Ordering::Less);
        a.added = "2024-01-02".into();
        b.added = "2024-03-01".into();
        assert_eq!(sort_cmp("newest", &a, &b), Ordering::Greater);
        assert_eq!(sort_cmp("trending", &a, &b), Ordering::Greater);
        b.added = String::new();
        // "" against a number raised TypeError: the pair compares equal.
        assert_eq!(sort_cmp("trending", &a, &b), Ordering::Equal);
        assert!(SORT_KEYS.iter().all(|k| *k != "default"));
    }

    /// Review fixes: NaN popularity, unbounded negation, Python whitespace.
    #[test]
    fn sort_and_text_edge_cases_follow_python() {
        let mut a = AppItem { name: "a".into(), ..Default::default() };
        let b = AppItem { name: "b".into(), ..Default::default() };
        a.popularity = f64::NAN;
        assert_eq!(sort_cmp("trending", &a, &b), Ordering::Equal);
        assert_eq!(sort_cmp("trending", &b, &a), Ordering::Equal);
        a.popularity = 0.0;
        assert_eq!(sort_cmp("trending", &a, &b), Ordering::Less);
        a.behind = i64::MIN;
        assert_eq!(sort_cmp("behind", &a, &b), Ordering::Greater);
        a.votes = i64::MIN;
        assert_eq!(sort_cmp("popular", &a, &b), Ordering::Greater);
        assert_eq!(squeeze("x\u{1c}y  z\n", 400), "x y z");
        assert_eq!(squeeze("abcdef", 3), "abc");
    }

    #[test]
    fn uptime_raises_where_python_does() {
        assert_eq!(Win::uptime_from("90061.5 1.0\n").as_deref(), Ok("1d 1h"));
        assert_eq!(Win::uptime_from("3720 0").as_deref(), Ok("1h 2m"));
        assert_eq!(Win::uptime_from("59").as_deref(), Ok("0m"));
        assert_eq!(Win::uptime_from("junk 1").as_deref(), Ok(""));
        assert!(Win::uptime_from("").unwrap_err().starts_with("IndexError"));
        assert!(Win::uptime_from("inf").unwrap_err().starts_with("OverflowError"));
        assert!(Win::uptime_from("nan").unwrap_err().starts_with("ValueError"));
    }

    /// test_every_icon_only_button_is_named, against this file.
    #[test]
    fn every_icon_only_button_is_named() {
        let lines: Vec<&str> = code().lines().collect();
        let made = regex::Regex::new(
            r"let (\w+) = gtk::(?:Menu|Toggle)?Button::(?:builder\(\)|from_icon_name\()",
        )
        .unwrap();
        let mut unnamed = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            let Some(m) = made.captures(line) else { continue };
            let var = &m[1];
            let window = lines[i..(i + 12).min(lines.len())].join("\n");
            let is_icon = window.lines().take(3).any(|l| l.contains("icon_name"))
                || line.contains("from_icon_name");
            if !is_icon {
                continue;
            }
            if !window.contains(&format!("name(&{var}"))
                && !window.contains("Property::Label")
            {
                unnamed.push(format!("packages.rs:{} {var}", i + 1));
            }
        }
        assert_eq!(unnamed, Vec::<String>::new());
    }

    /// test_spacing_comes_from_the_scale, against this file.
    #[test]
    fn spacing_comes_from_the_scale() {
        let scale: BTreeSet<i64> = [2, 4, 6, 8, 12, 16, 20, 24].into_iter().collect();
        let prop = regex::Regex::new(
            r"\.(margin_(?:start|end|top|bottom)|spacing|child_spacing|line_spacing)\((\d+)\)",
        )
        .unwrap();
        let boxed = regex::Regex::new(r"Box::new\(gtk::Orientation::\w+, (\d+)\)").unwrap();
        let mut offenders = Vec::new();
        for c in prop.captures_iter(code()) {
            if !scale.contains(&c[2].parse::<i64>().unwrap()) {
                offenders.push(format!("{}={}", &c[1], &c[2]));
            }
        }
        for c in boxed.captures_iter(code()) {
            if !scale.contains(&c[1].parse::<i64>().unwrap()) {
                offenders.push(format!("spacing={}", &c[1]));
            }
        }
        assert_eq!(offenders, Vec::<String>::new());
    }

    /// test_badges_use_one_pill_system, against this file.
    #[test]
    fn badges_use_one_pill_system() {
        let retired = [
            "crit-pill", "stale-pill", "crit-core", "store-chip", "store-installed",
            "trust-official", "trust-ok", "trust-warn", "trust-none",
        ];
        let found: Vec<&str> =
            retired.iter().copied().filter(|n| code().contains(&format!("\"{n}\""))).collect();
        assert_eq!(found, Vec::<&str>::new());
    }

    /// test_every_pill_class_is_defined_in_the_stylesheet, against this file.
    #[test]
    fn every_pill_class_is_defined_in_the_stylesheet() {
        let pills: BTreeSet<&str> =
            ["pill", "pill-crit", "pill-warn", "pill-ok", "pill-accent", "pill-quiet"]
                .into_iter()
                .collect();
        let used: BTreeSet<String> = regex::Regex::new(r#""(pill(?:-[a-z]+)?)""#)
            .unwrap()
            .captures_iter(code())
            .map(|c| c[1].to_string())
            .collect();
        assert!(!used.is_empty(), "no pill classes found at all");
        for n in &used {
            assert!(CSS.contains(&format!(".{n}")), "{n} is applied but never styled");
            assert!(pills.contains(n.as_str()), "unknown pill variant {n}");
        }
        // The sidebar count's levels are styled too.
        for cls in ["count-badge.is-warn", "count-badge.is-crit", "stat-key", "stat-val"] {
            assert!(CSS.contains(&format!(".{cls}")), "{cls} is applied but never styled");
        }
    }

    /// test_pills_are_all_one_size
    #[test]
    fn pills_are_all_one_size() {
        let block = regex::Regex::new(r"\.pill,[^{]*\{([^}]*)\}").unwrap();
        let shared = block.captures(CSS).expect("the shared pill rule is missing");
        assert!(shared[1].contains("font-size"), "the pill size must be set once");
        for variant in ["pill-crit", "pill-warn", "pill-ok", "pill-accent", "pill-quiet"] {
            let rule = regex::Regex::new(&format!(r"(?m)^\.{variant}\s*\{{([^}}]*)\}}")).unwrap();
            if let Some(r) = rule.captures(CSS) {
                assert!(!r[1].contains("font-size"), ".{variant} must not set its own size");
            }
        }
    }

    fn stems(dir: &Path, out: &mut BTreeSet<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stems(&p, out);
            } else if matches!(p.extension().and_then(|x| x.to_str()), Some("svg" | "png")) {
                if let Some(s) = p.file_stem().and_then(|s| s.to_str()) {
                    out.insert(s.to_string());
                }
            }
        }
    }

    /// test_every_icon_name_is_in_adwaita_or_shipped_here, for this file.
    #[test]
    fn every_icon_name_is_in_adwaita_or_shipped_here() {
        let adwaita = Path::new("/usr/share/icons/Adwaita");
        if !adwaita.is_dir() {
            return; // Adwaita icon theme not installed
        }
        let mut have = BTreeSet::new();
        stems(adwaita, &mut have);
        stems(&PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../icons")), &mut have);
        let names: BTreeSet<String> = regex::Regex::new(r#""([a-z0-9-]+-symbolic)""#)
            .unwrap()
            .captures_iter(code())
            .map(|c| c[1].to_string())
            .collect();
        // adw-* icons ship inside libadwaita itself.
        let missing: Vec<&String> =
            names.iter().filter(|n| !have.contains(*n) && !n.starts_with("adw-")).collect();
        assert_eq!(missing, Vec::<&String>::new());
    }
}
