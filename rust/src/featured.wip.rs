//! Port of `featured.py`.
//!
//! The landing page: what deserves attention right now.
//!
//! A "featured" section is usually an editorial list someone has to keep writing.
//! This one is assembled from what is true on the machine and in the marketplace
//! at the moment it is opened, which means it is never stale and never a lie.
//!
//! Four shelves, in the order a person actually cares about them:
//!
//! 1. **Needs you** — things that are wrong now: security updates, failed
//!    services, sources the marketplace can no longer reach. This is the only
//!    shelf that can be empty and that being empty is the good outcome.
//! 2. **New in the marketplace** — listed in the last few weeks, and filtered to
//!    what can actually be installed and has been reviewed, because "new" plus
//!    "unverified" is the worst combination to put in front of someone.
//! 3. **Trending** — upstream activity in the last week, ranked by stars. Recent
//!    commits on a popular plugin is the honest version of trending; a "views"
//!    counter is not something this app can see or should guess at.
//! 4. **Because you run X** — the tags of what is already installed, matched
//!    against what is not. No model, no profile, no tracking: a set
//!    intersection over the local plugin list.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::inventory::App;
use crate::maintenance::Finding;
use crate::store::Plugin;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Shelf {
    pub key: String,
    pub title: String,
    pub subtitle: String,
    pub items: Vec<Item>,
    pub empty: String,
}

impl Default for Shelf {
    fn default() -> Self {
        Shelf {
            key: String::new(),
            title: String::new(),
            subtitle: String::new(),
            items: Vec::new(),
            empty: "Nothing here.".to_string(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Item {
    pub title: String,
    pub detail: String,
    /// crit | warn | "" for neutral
    pub severity: String,
    /// a store.Plugin, when it is a listing
    pub plugin: Option<Plugin>,
    /// an inventory.App, when it is installed
    pub app: Option<App>,
    /// button label
    pub action: String,
    pub kind: String,
}

pub const DAY: i64 = 86400;
/// No single marketplace may own a shelf. Star counts are not comparable
/// across them -- a vendor's flagship app carries 15,000 stars where the
/// busiest Omarchy plugin carries 2,151 -- so an uncapped ranking is really
/// just "whichever catalogue has the biggest projects in it".
pub const PER_MARKET: usize = 3;

// `limit` and `per_market` are Python ints: a negative one is compared as is
// (`len(out) >= -1` stops after the first pick), never wrapped.
fn _diversify(ranked: &[Plugin], limit: i64, per_market: i64) -> Vec<Plugin> {
    _diversify_n(ranked, limit as f64, per_market as f64)
}

/// `_diversify` for any Python number: the parity hook passes floats and
/// bools through, as Python compares them as they are (`len(out) >= 2.5`).
fn _diversify_n(ranked: &[Plugin], limit: f64, per_market: f64) -> Vec<Plugin> {
    // Insertion-ordered like the Python dict; only counts are read back.
    let mut seen: Vec<(String, i64)> = Vec::new();
    let mut out: Vec<Plugin> = Vec::new();
    for p in ranked {
        let market = p.market.as_str();
        let n = seen.iter().find(|(m, _)| m == market).map(|(_, n)| *n).unwrap_or(0);
        if n as f64 >= per_market {
            continue;
        }
        match seen.iter_mut().find(|(m, _)| m == market) {
            Some(entry) => entry.1 = n + 1,
            None => seen.push((market.to_string(), n + 1)),
        }
        out.push(p.clone());
        if out.len() as f64 >= limit {
            break;
        }
    }
    out
}

fn _ts(value: &str) -> f64 {
    // Same body as `store._ts` in the Python (`datetime.fromisoformat` after
    // `Z` -> `+00:00`, 0.0 on ValueError), so it shares store's port of the
    // full Python 3.14 grammar rather than `util::iso_ts`, which trims and
    // accepts RFC 3339 forms fromisoformat rejects.
    let ts = crate::store::_ts(value);
    if ts != 0.0 && _naive(value) && _local_to_seconds_fails(ts) {
        return 0.0;
    }
    ts
}

/// Whether a value `fromisoformat` accepted carries no UTC offset. A date
/// never holds `+`, and its `-` sit before index 9 (`YYYY-Www-D`), where the
/// time part cannot start; any later `-` or a `+` (or `Z`) opens an offset.
fn _naive(value: &str) -> bool {
    let s = value.replace('Z', "+00:00");
    !s.contains('+') && !s.chars().skip(9).any(|c| c == '-')
}

/// A naive datetime's `.timestamp()` solves `local(u) == t` the way
/// CPython's `local_to_seconds` does, and each `local()` probe -- localtime
/// converted back to a datetime -- raises ValueError when it lands outside
/// years 1..=9999. That happens on 0001-01-01 west of Greenwich (the
/// `u - 24h` probe falls in year 0), which `_ts` turns into 0.0. `ts` is the
/// timestamp store computed for the same naive value.
fn _local_to_seconds_fails(ts: f64) -> bool {
    use chrono::{Datelike, TimeZone};
    let local = |u: i64| -> Option<i64> {
        let d = chrono::Local.timestamp_opt(u, 0).earliest()?.naive_local();
        (1..=9999).contains(&d.year()).then(|| d.and_utc().timestamp())
    };
    let Some(n) = chrono::Local.timestamp_opt(ts.floor() as i64, 0).earliest() else {
        return false;
    };
    // The naive fields, read as UTC.
    let t = n.naive_local().and_utc().timestamp();
    const MAX_FOLD_SECONDS: i64 = 24 * 3600;
    let run = || -> Option<()> {
        let lt = local(t)?;
        let a = lt - t;
        let u1 = t - a;
        let t1 = local(u1)?;
        let b = if t1 == t {
            // fold is 0: look for an earlier solution.
            let u2 = u1 - MAX_FOLD_SECONDS;
            let b = local(u2)? - u2;
            if a == b {
                return Some(());
            }
            b
        } else {
            t1 - u1
        };
        local(t - b)?;
        Some(())
    };
    run().is_none()
}

/// Python truthiness of a JSON value from `App.extra`.
fn _truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// `str(value)` for a JSON value from `App.extra`.
fn _py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(n) => match (n.as_i64(), n.as_u64()) {
            (Some(i), _) => i.to_string(),
            (_, Some(u)) => u.to_string(),
            _ => crate::util::py_float_repr(n.as_f64().unwrap_or_default()),
        },
        Value::String(s) => s.clone(),
        other => _py_repr(other),
    }
}

/// `repr(value)` for a JSON value: what `str()` prints for the list or dict
/// it decodes to, and for the strings inside one.
fn _py_repr(v: &Value) -> String {
    match v {
        Value::String(s) => {
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
                    c if !_printable(c) => {
                        let n = c as u32;
                        if n <= 0xff {
                            out.push_str(&format!("\\x{n:02x}"));
                        } else if n <= 0xffff {
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
        Value::Array(a) => format!("[{}]", a.iter().map(_py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, x)| format!("{}: {}", _py_repr(&Value::String(k.clone())), _py_repr(x)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => _py_str(other),
    }
}

/// `str.isprintable()` for one character: false in the Unicode categories
/// Cc, Cf, Cs, Co, Cn, Zl, Zp and Zs other than the space. `repr()` escapes
/// these.
fn _printable(c: char) -> bool {
    let n = c as u32;
    NONPRINTABLE
        .binary_search_by(|&(lo, hi)| {
            if hi < n {
                std::cmp::Ordering::Less
            } else if lo > n {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_err()
}

/// `s[:n]`, by characters as Python slices.
fn _head(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// `p.description or p.subtitle` -- the subtitle is only computed when the
/// description is empty, as Python's `or` short-circuits.
fn _blurb(p: &Plugin) -> String {
    if p.description.is_empty() {
        p.subtitle()
    } else {
        p.description.clone()
    }
}

/// Only genuine problems. A long list here would defeat the purpose.
pub fn needs_you(apps: &[App], findings: &[Finding]) -> Shelf {
    let mut items: Vec<Item> = Vec::new();

    for a in apps {
        if a.extra.get("critical").and_then(|v| v.as_str()) == Some("security") && a.has_update() {
            items.push(Item {
                title: a.name.clone(),
                detail: format!("Security update available · {} → {}", a.version, a.update),
                severity: "crit".to_string(),
                app: Some(a.clone()),
                action: "Update".to_string(),
                kind: "update".to_string(),
                ..Default::default()
            });
        }
    }

    for a in apps {
        if _truthy(a.extra.get("stale")) && !a.has_update() {
            items.push(Item {
                title: a.name.clone(),
                detail: a.extra.get("stale").map(_py_str).unwrap_or_default(),
                severity: "warn".to_string(),
                app: Some(a.clone()),
                kind: "stale".to_string(),
                ..Default::default()
            });
        }
    }

    for f in findings {
        if (f.severity == "crit" || f.severity == "warn") && f.id != "disk" {
            items.push(Item {
                title: f.title.clone(),
                detail: f.detail.clone(),
                severity: f.severity.clone(),
                action: if f.actionable() { f.fix_label.clone() } else { String::new() },
                kind: "finding".to_string(),
                ..Default::default()
            });
        } else if f.id == "disk" && f.severity == "crit" {
            items.push(Item {
                title: f.title.clone(),
                detail: f.detail.clone(),
                severity: "crit".to_string(),
                kind: "finding".to_string(),
                ..Default::default()
            });
        }
    }

    let rank = |sev: &str| match sev {
        "crit" => 0,
        "warn" => 1,
        _ => 2,
    };
    items.sort_by_key(|i| rank(&i.severity));
    items.truncate(8);
    Shelf {
        key: "attention".to_string(),
        title: "Needs you".to_string(),
        subtitle: "Problems on this machine, worst first.".to_string(),
        items,
        empty: "Nothing is wrong right now.".to_string(),
    }
}

/// Python defaults: `days=21`, `limit=8`.
pub fn new_listings(plugins: &[Plugin], days: i64, limit: i64) -> Shelf {
    _new_listings(plugins, days as f64, &days.to_string(), limit as f64)
}

/// `new_listings` for any number of days: Python takes a float as well, and
/// the subtitle prints it as `str()` does (`days_text`).
fn _new_listings(plugins: &[Plugin], days: f64, days_text: &str, limit: f64) -> Shelf {
    let cutoff = crate::util::now() - days * DAY as f64;
    let mut fresh: Vec<Plugin> = plugins
        .iter()
        .filter(|p| {
            _ts(&p.added) > cutoff
                && p.installable
                && !p.installed
                && ["official", "first-party", "reviewed", "verified"].contains(&p.trust().key.as_str())
        })
        .cloned()
        .collect();
    fresh.sort_by(|a, b| {
        let (ka, kb) = ((-_ts(&a.added), -a.stars), (-_ts(&b.added), -b.stars));
        ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
    });
    Shelf {
        key: "new".to_string(),
        title: "New in the marketplace".to_string(),
        subtitle: format!("Listed in the last {days_text} days, reviewed, and installable."),
        items: _diversify_n(&fresh, limit, PER_MARKET as f64)
            .into_iter()
            .map(|p| Item {
                title: p.name.clone(),
                detail: _blurb(&p),
                plugin: Some(p),
                action: "Install".to_string(),
                kind: "plugin".to_string(),
                ..Default::default()
            })
            .collect(),
        empty: "Nothing new has been listed recently.".to_string(),
    }
}

/// Python defaults: `days=7`, `limit=8`.
pub fn trending(plugins: &[Plugin], days: i64, limit: i64) -> Shelf {
    _trending(plugins, days as f64, &days.to_string(), limit as f64)
}

/// `trending` for any number of days (see `_new_listings`).
fn _trending(plugins: &[Plugin], days: f64, days_text: &str, limit: f64) -> Shelf {
    let cutoff = crate::util::now() - days * DAY as f64;
    let mut hot: Vec<Plugin> = plugins
        .iter()
        .filter(|p| {
            _ts(&p.updated) > cutoff
                && p.installable
                && !p.installed
                && p.stars >= 3
                && !["unreachable", "unverified"].contains(&p.trust().key.as_str())
        })
        .cloned()
        .collect();
    hot.sort_by(|a, b| {
        let (ka, kb) = ((-a.stars, -_ts(&a.updated)), (-b.stars, -_ts(&b.updated)));
        ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
    });
    Shelf {
        key: "trending".to_string(),
        title: "Trending".to_string(),
        subtitle: format!("Worked on in the last {days_text} days, by how many people have starred it."),
        items: _diversify_n(&hot, limit, PER_MARKET as f64)
            .into_iter()
            .map(|p| Item {
                title: p.name.clone(),
                detail: format!("★ {} · updated {} · {}", p.stars, _head(&p.updated, 10), _blurb(&p)),
                plugin: Some(p),
                action: "Install".to_string(),
                kind: "plugin".to_string(),
                ..Default::default()
            })
            .collect(),
        empty: "Nothing has been updated recently.".to_string(),
    }
}

/// Match the tags of what is installed against what is not.
///
/// Deliberately not a recommender: it is a set intersection over tags the
/// marketplace already publishes, computed locally, with nothing sent
/// anywhere and nothing remembered between runs.
///
/// Python default: `limit=8`.
pub fn because_you_run(plugins: &[Plugin], limit: i64) -> Shelf {
    _because_you_run(plugins, limit as f64)
}

/// `because_you_run` for any Python number as `limit` (see `_diversify_n`).
fn _because_you_run(plugins: &[Plugin], limit: f64) -> Shelf {
    let mine: Vec<&Plugin> = plugins.iter().filter(|p| p.installed).collect();
    if mine.is_empty() {
        return Shelf {
            key: "affinity".to_string(),
            title: "Suggested".to_string(),
            subtitle: String::new(),
            items: Vec::new(),
            empty: "Install something and this fills in.".to_string(),
        };
    }

    // Insertion-ordered, like the Python dict: ties in the ranking below keep
    // the order the tags were first seen in.
    let mut weight: Vec<(String, i64)> = Vec::new();
    for p in &mine {
        for tag in &p.tags {
            match weight.iter_mut().find(|(t, _)| t == tag) {
                Some(entry) => entry.1 += 1,
                None => weight.push((tag.clone(), 1)),
            }
        }
    }
    if weight.is_empty() {
        return Shelf {
            key: "affinity".to_string(),
            title: "Suggested".to_string(),
            subtitle: String::new(),
            items: Vec::new(),
            empty: "Nothing installed carries a tag to match on.".to_string(),
        };
    }
    let w = |t: &str| weight.iter().find(|(k, _)| k == t).map(|(_, n)| *n).unwrap_or(0);

    let mut ranked: Vec<&(String, i64)> = weight.iter().collect();
    ranked.sort_by_key(|(_, n)| -*n);
    let top: Vec<&str> = ranked.iter().take(3).map(|(t, _)| t.as_str()).collect();
    let named = top.join(", ");

    let mut scored: Vec<(i64, i64, &Plugin)> = Vec::new();
    for p in plugins {
        if p.installed || !p.installable {
            continue;
        }
        let key = p.trust().key;
        if key == "unreachable" || key == "unverified" {
            continue;
        }
        let overlap: i64 = p.tags.iter().map(|t| w(t)).sum();
        if overlap != 0 {
            scored.push((overlap, p.stars, p));
        }
    }
    scored.sort_by_key(|row| (-row.0, -row.1));
    let ranked_plugins: Vec<Plugin> = scored.iter().map(|(_o, _s, p)| (*p).clone()).collect();
    let picked = _diversify_n(&ranked_plugins, limit, PER_MARKET as f64);

    Shelf {
        key: "affinity".to_string(),
        title: "Because of what you run".to_string(),
        subtitle: format!("Matched on the tags you already have most of: {named}."),
        items: picked
            .into_iter()
            .map(|p| Item {
                title: p.name.clone(),
                detail: format!("{} · {}", p.tags.join(", "), _blurb(&p)),
                plugin: Some(p),
                action: "Install".to_string(),
                kind: "plugin".to_string(),
                ..Default::default()
            })
            .collect(),
        empty: "Nothing new matches what you already run.".to_string(),
    }
}

/// Every shelf, in display order. Pure; safe to call off the UI thread.
pub fn build(apps: &[App], findings: &[Finding], plugins: &[Plugin]) -> Vec<Shelf> {
    let mut shelves = vec![needs_you(apps, findings)];
    if !plugins.is_empty() {
        shelves.push(new_listings(plugins, 21, 8));
        shelves.push(trending(plugins, 7, 8));
        shelves.push(because_you_run(plugins, 8));
    }
    shelves
}

/// A list of dataclass instances from parity args. Like the Python
/// constructor, a missing required field or an unknown one is a TypeError,
/// and a value of the wrong shape an error too -- never silently dropped.
fn _arg_list<T>(args: &Value, key: &str, cls: &str, required: &[&str]) -> Result<Vec<T>, String>
where
    T: serde::de::DeserializeOwned + serde::Serialize + Default,
{
    let Some(items) = args.get(key).and_then(|v| v.as_array()) else {
        return Ok(Vec::new());
    };
    let known = serde_json::to_value(T::default()).ok();
    let mut out = Vec::new();
    for x in items {
        if let Some(obj) = x.as_object() {
            let missing: Vec<&str> = required.iter().copied().filter(|k| !obj.contains_key(*k)).collect();
            if !missing.is_empty() {
                return Err(format!("TypeError: {cls}.__init__() missing required arguments: {}", missing.join(", ")));
            }
            if let Some(Value::Object(k)) = &known {
                if let Some(bad) = obj.keys().find(|f| !k.contains_key(*f)) {
                    return Err(format!("TypeError: {cls}.__init__() got an unexpected keyword argument '{bad}'"));
                }
            }
        }
        out.push(serde_json::from_value(x.clone()).map_err(|e| format!("TypeError: {cls}: {e}"))?);
    }
    Ok(out)
}

/// A Python number from parity args, as Python compares it: a bool is 0 or 1.
fn _arg_or(args: &Value, key: &str, default: i64) -> f64 {
    match args.get(key) {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(default as f64),
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        _ => default as f64,
    }
}

/// A Python number from parity args, with its `str()`: `21`, `1.5`, `True`.
fn _arg_num(args: &Value, key: &str, default: i64) -> (f64, String) {
    match args.get(key) {
        Some(Value::Number(n)) => match (n.as_i64(), n.as_u64()) {
            (Some(i), _) => (i as f64, i.to_string()),
            (_, Some(u)) => (u as f64, u.to_string()),
            _ => {
                let f = n.as_f64().unwrap_or_default();
                (f, crate::util::py_float_repr(f))
            }
        },
        Some(Value::Bool(b)) => (f64::from(u8::from(*b)), if *b { "True" } else { "False" }.to_string()),
        _ => (default as f64, default.to_string()),
    }
}

/// Parity hook: see ARCHITECTURE.md.
pub fn parity(func: &str, args: &Value) -> Option<Value> {
    // A case that panics inside another module must fail on its own, not
    // take every other case in the run down with it.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let out = std::panic::catch_unwind(|| _parity(func, args));
    std::panic::set_hook(hook);
    match out {
        Ok(v) => v,
        Err(e) => {
            let msg = e
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            Some(serde_json::json!({"error": format!("panicked: {msg}")}))
        }
    }
}

fn _parity(func: &str, args: &Value) -> Option<Value> {
    let parsed = (|| {
        Ok::<_, String>((
            _arg_list::<App>(args, "apps", "App", &["name", "version", "source"])?,
            _arg_list::<Finding>(args, "findings", "Finding", &["id", "title", "detail"])?,
            _arg_list::<Plugin>(args, "plugins", "Plugin", &["id", "name"])?,
        ))
    })();
    let (apps, findings, plugins) = match parsed {
        Ok(t) => t,
        Err(e) => return Some(serde_json::json!({ "error": e })),
    };
    let v = match func {
        "_diversify" => serde_json::to_value(_diversify_n(
            &plugins,
            _arg_or(args, "limit", 8),
            _arg_or(args, "per_market", PER_MARKET as i64),
        )),
        "_ts" => serde_json::to_value(_ts(crate::util::arg_str(args, "value"))),
        "needs_you" => serde_json::to_value(needs_you(&apps, &findings)),
        "new_listings" => {
            let (days, text) = _arg_num(args, "days", 21);
            serde_json::to_value(_new_listings(&plugins, days, &text, _arg_or(args, "limit", 8)))
        }
        "trending" => {
            let (days, text) = _arg_num(args, "days", 7);
            serde_json::to_value(_trending(&plugins, days, &text, _arg_or(args, "limit", 8)))
        }
        "because_you_run" => serde_json::to_value(_because_you_run(&plugins, _arg_or(args, "limit", 8))),
        "build" => serde_json::to_value(build(&apps, &findings, &plugins)),
        _ => return None,
    };
    Some(v.unwrap_or_else(|e| serde_json::json!({"error": e.to_string()})))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Featured and the AI page carry differently-shaped collections.
    ///
    /// Featured yields Shelf objects; the AI survey yields
    /// (title, description, entries) tuples. Unpacking one as the other raised
    /// only at runtime, on a page that had already rendered.
    #[test]
    fn test_featured_build_returns_shelf_objects() {
        let shelves = build(&[], &[], &[]);
        assert!(!shelves.is_empty());
        for shelf in &shelves {
            // a shelf must expose .items, not unpack as a tuple
            let _items: &Vec<Item> = &shelf.items;
            let _title: &String = &shelf.title;
        }
    }
}

/// Code points `str.isprintable()` rejects (Python 3.14.7, Unicode 16.0.0), as
/// inclusive ranges, generated from `unicodedata`.
#[rustfmt::skip]
const NONPRINTABLE: &[(u32, u32)] = &[
    (0x0, 0x1f), (0x7f, 0xa0), (0xad, 0xad), (0x378, 0x379), (0x380, 0x383), (0x38b, 0x38b),
    (0x38d, 0x38d), (0x3a2, 0x3a2), (0x530, 0x530), (0x557, 0x558), (0x58b, 0x58c), (0x590, 0x590),
    (0x5c8, 0x5cf), (0x5eb, 0x5ee), (0x5f5, 0x605), (0x61c, 0x61c), (0x6dd, 0x6dd), (0x70e, 0x70f),
    (0x74b, 0x74c), (0x7b2, 0x7bf), (0x7fb, 0x7fc), (0x82e, 0x82f), (0x83f, 0x83f), (0x85c, 0x85d),
    (0x85f, 0x85f), (0x86b, 0x86f), (0x88f, 0x896), (0x8e2, 0x8e2), (0x984, 0x984), (0x98d, 0x98e),
    (0x991, 0x992), (0x9a9, 0x9a9), (0x9b1, 0x9b1), (0x9b3, 0x9b5), (0x9ba, 0x9bb), (0x9c5, 0x9c6),
    (0x9c9, 0x9ca), (0x9cf, 0x9d6), (0x9d8, 0x9db), (0x9de, 0x9de), (0x9e4, 0x9e5), (0x9ff, 0xa00),
    (0xa04, 0xa04), (0xa0b, 0xa0e), (0xa11, 0xa12), (0xa29, 0xa29), (0xa31, 0xa31), (0xa34, 0xa34),
    (0xa37, 0xa37), (0xa3a, 0xa3b), (0xa3d, 0xa3d), (0xa43, 0xa46), (0xa49, 0xa4a), (0xa4e, 0xa50),
    (0xa52, 0xa58), (0xa5d, 0xa5d), (0xa5f, 0xa65), (0xa77, 0xa80), (0xa84, 0xa84), (0xa8e, 0xa8e),
    (0xa92, 0xa92), (0xaa9, 0xaa9), (0xab1, 0xab1), (0xab4, 0xab4), (0xaba, 0xabb), (0xac6, 0xac6),
    (0xaca, 0xaca), (0xace, 0xacf), (0xad1, 0xadf), (0xae4, 0xae5), (0xaf2, 0xaf8), (0xb00, 0xb00),
    (0xb04, 0xb04), (0xb0d, 0xb0e), (0xb11, 0xb12), (0xb29, 0xb29), (0xb31, 0xb31), (0xb34, 0xb34),
    (0xb3a, 0xb3b), (0xb45, 0xb46), (0xb49, 0xb4a), (0xb4e, 0xb54), (0xb58, 0xb5b), (0xb5e, 0xb5e),
    (0xb64, 0xb65), (0xb78, 0xb81), (0xb84, 0xb84), (0xb8b, 0xb8d), (0xb91, 0xb91), (0xb96, 0xb98),
    (0xb9b, 0xb9b), (0xb9d, 0xb9d), (0xba0, 0xba2), (0xba5, 0xba7), (0xbab, 0xbad), (0xbba, 0xbbd),
    (0xbc3, 0xbc5), (0xbc9, 0xbc9), (0xbce, 0xbcf), (0xbd1, 0xbd6), (0xbd8, 0xbe5), (0xbfb, 0xbff),
    (0xc0d, 0xc0d), (0xc11, 0xc11), (0xc29, 0xc29), (0xc3a, 0xc3b), (0xc45, 0xc45), (0xc49, 0xc49),
    (0xc4e, 0xc54), (0xc57, 0xc57), (0xc5b, 0xc5c), (0xc5e, 0xc5f), (0xc64, 0xc65), (0xc70, 0xc76),
    (0xc8d, 0xc8d), (0xc91, 0xc91), (0xca9, 0xca9), (0xcb4, 0xcb4), (0xcba, 0xcbb), (0xcc5, 0xcc5),
    (0xcc9, 0xcc9), (0xcce, 0xcd4), (0xcd7, 0xcdc), (0xcdf, 0xcdf), (0xce4, 0xce5), (0xcf0, 0xcf0),
    (0xcf4, 0xcff), (0xd0d, 0xd0d), (0xd11, 0xd11), (0xd45, 0xd45), (0xd49, 0xd49), (0xd50, 0xd53),
    (0xd64, 0xd65), (0xd80, 0xd80), (0xd84, 0xd84), (0xd97, 0xd99), (0xdb2, 0xdb2), (0xdbc, 0xdbc),
    (0xdbe, 0xdbf), (0xdc7, 0xdc9), (0xdcb, 0xdce), (0xdd5, 0xdd5), (0xdd7, 0xdd7), (0xde0, 0xde5),
    (0xdf0, 0xdf1), (0xdf5, 0xe00), (0xe3b, 0xe3e), (0xe5c, 0xe80), (0xe83, 0xe83), (0xe85, 0xe85),
    (0xe8b, 0xe8b), (0xea4, 0xea4), (0xea6, 0xea6), (0xebe, 0xebf), (0xec5, 0xec5), (0xec7, 0xec7),
    (0xecf, 0xecf), (0xeda, 0xedb), (0xee0, 0xeff), (0xf48, 0xf48), (0xf6d, 0xf70), (0xf98, 0xf98),
    (0xfbd, 0xfbd), (0xfcd, 0xfcd), (0xfdb, 0xfff), (0x10c6, 0x10c6), (0x10c8, 0x10cc), (0x10ce, 0x10cf),
    (0x1249, 0x1249), (0x124e, 0x124f), (0x1257, 0x1257), (0x1259, 0x1259), (0x125e, 0x125f), (0x1289, 0x1289),
    (0x128e, 0x128f), (0x12b1, 0x12b1), (0x12b6, 0x12b7), (0x12bf, 0x12bf), (0x12c1, 0x12c1), (0x12c6, 0x12c7),
    (0x12d7, 0x12d7), (0x1311, 0x1311), (0x1316, 0x1317), (0x135b, 0x135c), (0x137d, 0x137f), (0x139a, 0x139f),
    (0x13f6, 0x13f7), (0x13fe, 0x13ff), (0x1680, 0x1680), (0x169d, 0x169f), (0x16f9, 0x16ff), (0x1716, 0x171e),
    (0x1737, 0x173f), (0x1754, 0x175f), (0x176d, 0x176d), (0x1771, 0x1771), (0x1774, 0x177f), (0x17de, 0x17df),
    (0x17ea, 0x17ef), (0x17fa, 0x17ff), (0x180e, 0x180e), (0x181a, 0x181f), (0x1879, 0x187f), (0x18ab, 0x18af),
    (0x18f6, 0x18ff), (0x191f, 0x191f), (0x192c, 0x192f), (0x193c, 0x193f), (0x1941, 0x1943), (0x196e, 0x196f),
    (0x1975, 0x197f), (0x19ac, 0x19af), (0x19ca, 0x19cf), (0x19db, 0x19dd), (0x1a1c, 0x1a1d), (0x1a5f, 0x1a5f),
    (0x1a7d, 0x1a7e), (0x1a8a, 0x1a8f), (0x1a9a, 0x1a9f), (0x1aae, 0x1aaf), (0x1acf, 0x1aff), (0x1b4d, 0x1b4d),
    (0x1bf4, 0x1bfb), (0x1c38, 0x1c3a), (0x1c4a, 0x1c4c), (0x1c8b, 0x1c8f), (0x1cbb, 0x1cbc), (0x1cc8, 0x1ccf),
    (0x1cfb, 0x1cff), (0x1f16, 0x1f17), (0x1f1e, 0x1f1f), (0x1f46, 0x1f47), (0x1f4e, 0x1f4f), (0x1f58, 0x1f58),
    (0x1f5a, 0x1f5a), (0x1f5c, 0x1f5c), (0x1f5e, 0x1f5e), (0x1f7e, 0x1f7f), (0x1fb5, 0x1fb5), (0x1fc5, 0x1fc5),
    (0x1fd4, 0x1fd5), (0x1fdc, 0x1fdc), (0x1ff0, 0x1ff1), (0x1ff5, 0x1ff5), (0x1fff, 0x200f), (0x2028, 0x202f),
    (0x205f, 0x206f), (0x2072, 0x2073), (0x208f, 0x208f), (0x209d, 0x209f), (0x20c1, 0x20cf), (0x20f1, 0x20ff),
    (0x218c, 0x218f), (0x242a, 0x243f), (0x244b, 0x245f), (0x2b74, 0x2b75), (0x2b96, 0x2b96), (0x2cf4, 0x2cf8),
    (0x2d26, 0x2d26), (0x2d28, 0x2d2c), (0x2d2e, 0x2d2f), (0x2d68, 0x2d6e), (0x2d71, 0x2d7e), (0x2d97, 0x2d9f),
    (0x2da7, 0x2da7), (0x2daf, 0x2daf), (0x2db7, 0x2db7), (0x2dbf, 0x2dbf), (0x2dc7, 0x2dc7), (0x2dcf, 0x2dcf),
    (0x2dd7, 0x2dd7), (0x2ddf, 0x2ddf), (0x2e5e, 0x2e7f), (0x2e9a, 0x2e9a), (0x2ef4, 0x2eff), (0x2fd6, 0x2fef),
    (0x3000, 0x3000), (0x3040, 0x3040), (0x3097, 0x3098), (0x3100, 0x3104), (0x3130, 0x3130), (0x318f, 0x318f),
    (0x31e6, 0x31ee), (0x321f, 0x321f), (0xa48d, 0xa48f), (0xa4c7, 0xa4cf), (0xa62c, 0xa63f), (0xa6f8, 0xa6ff),
    (0xa7ce, 0xa7cf), (0xa7d2, 0xa7d2), (0xa7d4, 0xa7d4), (0xa7dd, 0xa7f1), (0xa82d, 0xa82f), (0xa83a, 0xa83f),
    (0xa878, 0xa87f), (0xa8c6, 0xa8cd), (0xa8da, 0xa8df), (0xa954, 0xa95e), (0xa97d, 0xa97f), (0xa9ce, 0xa9ce),
    (0xa9da, 0xa9dd), (0xa9ff, 0xa9ff), (0xaa37, 0xaa3f), (0xaa4e, 0xaa4f), (0xaa5a, 0xaa5b), (0xaac3, 0xaada),
    (0xaaf7, 0xab00), (0xab07, 0xab08), (0xab0f, 0xab10), (0xab17, 0xab1f), (0xab27, 0xab27), (0xab2f, 0xab2f),
    (0xab6c, 0xab6f), (0xabee, 0xabef), (0xabfa, 0xabff), (0xd7a4, 0xd7af), (0xd7c7, 0xd7ca), (0xd7fc, 0xf8ff),
    (0xfa6e, 0xfa6f), (0xfada, 0xfaff), (0xfb07, 0xfb12), (0xfb18, 0xfb1c), (0xfb37, 0xfb37), (0xfb3d, 0xfb3d),
    (0xfb3f, 0xfb3f), (0xfb42, 0xfb42), (0xfb45, 0xfb45), (0xfbc3, 0xfbd2), (0xfd90, 0xfd91), (0xfdc8, 0xfdce),
    (0xfdd0, 0xfdef), (0xfe1a, 0xfe1f), (0xfe53, 0xfe53), (0xfe67, 0xfe67), (0xfe6c, 0xfe6f), (0xfe75, 0xfe75),
    (0xfefd, 0xff00), (0xffbf, 0xffc1), (0xffc8, 0xffc9), (0xffd0, 0xffd1), (0xffd8, 0xffd9), (0xffdd, 0xffdf),
    (0xffe7, 0xffe7), (0xffef, 0xfffb), (0xfffe, 0xffff), (0x1000c, 0x1000c), (0x10027, 0x10027), (0x1003b, 0x1003b),
    (0x1003e, 0x1003e), (0x1004e, 0x1004f), (0x1005e, 0x1007f), (0x100fb, 0x100ff), (0x10103, 0x10106), (0x10134, 0x10136),
    (0x1018f, 0x1018f), (0x1019d, 0x1019f), (0x101a1, 0x101cf), (0x101fe, 0x1027f), (0x1029d, 0x1029f), (0x102d1, 0x102df),
    (0x102fc, 0x102ff), (0x10324, 0x1032c), (0x1034b, 0x1034f), (0x1037b, 0x1037f), (0x1039e, 0x1039e), (0x103c4, 0x103c7),
    (0x103d6, 0x103ff), (0x1049e, 0x1049f), (0x104aa, 0x104af), (0x104d4, 0x104d7), (0x104fc, 0x104ff), (0x10528, 0x1052f),
    (0x10564, 0x1056e), (0x1057b, 0x1057b), (0x1058b, 0x1058b), (0x10593, 0x10593), (0x10596, 0x10596), (0x105a2, 0x105a2),
    (0x105b2, 0x105b2), (0x105ba, 0x105ba), (0x105bd, 0x105bf), (0x105f4, 0x105ff), (0x10737, 0x1073f), (0x10756, 0x1075f),
    (0x10768, 0x1077f), (0x10786, 0x10786), (0x107b1, 0x107b1), (0x107bb, 0x107ff), (0x10806, 0x10807), (0x10809, 0x10809),
    (0x10836, 0x10836), (0x10839, 0x1083b), (0x1083d, 0x1083e), (0x10856, 0x10856), (0x1089f, 0x108a6), (0x108b0, 0x108df),
    (0x108f3, 0x108f3), (0x108f6, 0x108fa), (0x1091c, 0x1091e), (0x1093a, 0x1093e), (0x10940, 0x1097f), (0x109b8, 0x109bb),
    (0x109d0, 0x109d1), (0x10a04, 0x10a04), (0x10a07, 0x10a0b), (0x10a14, 0x10a14), (0x10a18, 0x10a18), (0x10a36, 0x10a37),
    (0x10a3b, 0x10a3e), (0x10a49, 0x10a4f), (0x10a59, 0x10a5f), (0x10aa0, 0x10abf), (0x10ae7, 0x10aea), (0x10af7, 0x10aff),
    (0x10b36, 0x10b38), (0x10b56, 0x10b57), (0x10b73, 0x10b77), (0x10b92, 0x10b98), (0x10b9d, 0x10ba8), (0x10bb0, 0x10bff),
    (0x10c49, 0x10c7f), (0x10cb3, 0x10cbf), (0x10cf3, 0x10cf9), (0x10d28, 0x10d2f), (0x10d3a, 0x10d3f), (0x10d66, 0x10d68),
    (0x10d86, 0x10d8d), (0x10d90, 0x10e5f), (0x10e7f, 0x10e7f), (0x10eaa, 0x10eaa), (0x10eae, 0x10eaf), (0x10eb2, 0x10ec1),
    (0x10ec5, 0x10efb), (0x10f28, 0x10f2f), (0x10f5a, 0x10f6f), (0x10f8a, 0x10faf), (0x10fcc, 0x10fdf), (0x10ff7, 0x10fff),
    (0x1104e, 0x11051), (0x11076, 0x1107e), (0x110bd, 0x110bd), (0x110c3, 0x110cf), (0x110e9, 0x110ef), (0x110fa, 0x110ff),
    (0x11135, 0x11135), (0x11148, 0x1114f), (0x11177, 0x1117f), (0x111e0, 0x111e0), (0x111f5, 0x111ff), (0x11212, 0x11212),
    (0x11242, 0x1127f), (0x11287, 0x11287), (0x11289, 0x11289), (0x1128e, 0x1128e), (0x1129e, 0x1129e), (0x112aa, 0x112af),
    (0x112eb, 0x112ef), (0x112fa, 0x112ff), (0x11304, 0x11304), (0x1130d, 0x1130e), (0x11311, 0x11312), (0x11329, 0x11329),
    (0x11331, 0x11331), (0x11334, 0x11334), (0x1133a, 0x1133a), (0x11345, 0x11346), (0x11349, 0x1134a), (0x1134e, 0x1134f),
    (0x11351, 0x11356), (0x11358, 0x1135c), (0x11364, 0x11365), (0x1136d, 0x1136f), (0x11375, 0x1137f), (0x1138a, 0x1138a),
    (0x1138c, 0x1138d), (0x1138f, 0x1138f), (0x113b6, 0x113b6), (0x113c1, 0x113c1), (0x113c3, 0x113c4), (0x113c6, 0x113c6),
    (0x113cb, 0x113cb), (0x113d6, 0x113d6), (0x113d9, 0x113e0), (0x113e3, 0x113ff), (0x1145c, 0x1145c), (0x11462, 0x1147f),
    (0x114c8, 0x114cf), (0x114da, 0x1157f), (0x115b6, 0x115b7), (0x115de, 0x115ff), (0x11645, 0x1164f), (0x1165a, 0x1165f),
    (0x1166d, 0x1167f), (0x116ba, 0x116bf), (0x116ca, 0x116cf), (0x116e4, 0x116ff), (0x1171b, 0x1171c), (0x1172c, 0x1172f),
    (0x11747, 0x117ff), (0x1183c, 0x1189f), (0x118f3, 0x118fe), (0x11907, 0x11908), (0x1190a, 0x1190b), (0x11914, 0x11914),
    (0x11917, 0x11917), (0x11936, 0x11936), (0x11939, 0x1193a), (0x11947, 0x1194f), (0x1195a, 0x1199f), (0x119a8, 0x119a9),
    (0x119d8, 0x119d9), (0x119e5, 0x119ff), (0x11a48, 0x11a4f), (0x11aa3, 0x11aaf), (0x11af9, 0x11aff), (0x11b0a, 0x11bbf),
    (0x11be2, 0x11bef), (0x11bfa, 0x11bff), (0x11c09, 0x11c09), (0x11c37, 0x11c37), (0x11c46, 0x11c4f), (0x11c6d, 0x11c6f),
    (0x11c90, 0x11c91), (0x11ca8, 0x11ca8), (0x11cb7, 0x11cff), (0x11d07, 0x11d07), (0x11d0a, 0x11d0a), (0x11d37, 0x11d39),
    (0x11d3b, 0x11d3b), (0x11d3e, 0x11d3e), (0x11d48, 0x11d4f), (0x11d5a, 0x11d5f), (0x11d66, 0x11d66), (0x11d69, 0x11d69),
    (0x11d8f, 0x11d8f), (0x11d92, 0x11d92), (0x11d99, 0x11d9f), (0x11daa, 0x11edf), (0x11ef9, 0x11eff), (0x11f11, 0x11f11),
    (0x11f3b, 0x11f3d), (0x11f5b, 0x11faf), (0x11fb1, 0x11fbf), (0x11ff2, 0x11ffe), (0x1239a, 0x123ff), (0x1246f, 0x1246f),
    (0x12475, 0x1247f), (0x12544, 0x12f8f), (0x12ff3, 0x12fff), (0x13430, 0x1343f), (0x13456, 0x1345f), (0x143fb, 0x143ff),
    (0x14647, 0x160ff), (0x1613a, 0x167ff), (0x16a39, 0x16a3f), (0x16a5f, 0x16a5f), (0x16a6a, 0x16a6d), (0x16abf, 0x16abf),
    (0x16aca, 0x16acf), (0x16aee, 0x16aef), (0x16af6, 0x16aff), (0x16b46, 0x16b4f), (0x16b5a, 0x16b5a), (0x16b62, 0x16b62),
    (0x16b78, 0x16b7c), (0x16b90, 0x16d3f), (0x16d7a, 0x16e3f), (0x16e9b, 0x16eff), (0x16f4b, 0x16f4e), (0x16f88, 0x16f8e),
    (0x16fa0, 0x16fdf), (0x16fe5, 0x16fef), (0x16ff2, 0x16fff), (0x187f8, 0x187ff), (0x18cd6, 0x18cfe), (0x18d09, 0x1afef),
    (0x1aff4, 0x1aff4), (0x1affc, 0x1affc), (0x1afff, 0x1afff), (0x1b123, 0x1b131), (0x1b133, 0x1b14f), (0x1b153, 0x1b154),
    (0x1b156, 0x1b163), (0x1b168, 0x1b16f), (0x1b2fc, 0x1bbff), (0x1bc6b, 0x1bc6f), (0x1bc7d, 0x1bc7f), (0x1bc89, 0x1bc8f),
    (0x1bc9a, 0x1bc9b), (0x1bca0, 0x1cbff), (0x1ccfa, 0x1ccff), (0x1ceb4, 0x1ceff), (0x1cf2e, 0x1cf2f), (0x1cf47, 0x1cf4f),
    (0x1cfc4, 0x1cfff), (0x1d0f6, 0x1d0ff), (0x1d127, 0x1d128), (0x1d173, 0x1d17a), (0x1d1eb, 0x1d1ff), (0x1d246, 0x1d2bf),
    (0x1d2d4, 0x1d2df), (0x1d2f4, 0x1d2ff), (0x1d357, 0x1d35f), (0x1d379, 0x1d3ff), (0x1d455, 0x1d455), (0x1d49d, 0x1d49d),
    (0x1d4a0, 0x1d4a1), (0x1d4a3, 0x1d4a4), (0x1d4a7, 0x1d4a8), (0x1d4ad, 0x1d4ad), (0x1d4ba, 0x1d4ba), (0x1d4bc, 0x1d4bc),
    (0x1d4c4, 0x1d4c4), (0x1d506, 0x1d506), (0x1d50b, 0x1d50c), (0x1d515, 0x1d515), (0x1d51d, 0x1d51d), (0x1d53a, 0x1d53a),
    (0x1d53f, 0x1d53f), (0x1d545, 0x1d545), (0x1d547, 0x1d549), (0x1d551, 0x1d551), (0x1d6a6, 0x1d6a7), (0x1d7cc, 0x1d7cd),
    (0x1da8c, 0x1da9a), (0x1daa0, 0x1daa0), (0x1dab0, 0x1deff), (0x1df1f, 0x1df24), (0x1df2b, 0x1dfff), (0x1e007, 0x1e007),
    (0x1e019, 0x1e01a), (0x1e022, 0x1e022), (0x1e025, 0x1e025), (0x1e02b, 0x1e02f), (0x1e06e, 0x1e08e), (0x1e090, 0x1e0ff),
    (0x1e12d, 0x1e12f), (0x1e13e, 0x1e13f), (0x1e14a, 0x1e14d), (0x1e150, 0x1e28f), (0x1e2af, 0x1e2bf), (0x1e2fa, 0x1e2fe),
    (0x1e300, 0x1e4cf), (0x1e4fa, 0x1e5cf), (0x1e5fb, 0x1e5fe), (0x1e600, 0x1e7df), (0x1e7e7, 0x1e7e7), (0x1e7ec, 0x1e7ec),
    (0x1e7ef, 0x1e7ef), (0x1e7ff, 0x1e7ff), (0x1e8c5, 0x1e8c6), (0x1e8d7, 0x1e8ff), (0x1e94c, 0x1e94f), (0x1e95a, 0x1e95d),
    (0x1e960, 0x1ec70), (0x1ecb5, 0x1ed00), (0x1ed3e, 0x1edff), (0x1ee04, 0x1ee04), (0x1ee20, 0x1ee20), (0x1ee23, 0x1ee23),
    (0x1ee25, 0x1ee26), (0x1ee28, 0x1ee28), (0x1ee33, 0x1ee33), (0x1ee38, 0x1ee38), (0x1ee3a, 0x1ee3a), (0x1ee3c, 0x1ee41),
    (0x1ee43, 0x1ee46), (0x1ee48, 0x1ee48), (0x1ee4a, 0x1ee4a), (0x1ee4c, 0x1ee4c), (0x1ee50, 0x1ee50), (0x1ee53, 0x1ee53),
    (0x1ee55, 0x1ee56), (0x1ee58, 0x1ee58), (0x1ee5a, 0x1ee5a), (0x1ee5c, 0x1ee5c), (0x1ee5e, 0x1ee5e), (0x1ee60, 0x1ee60),
    (0x1ee63, 0x1ee63), (0x1ee65, 0x1ee66), (0x1ee6b, 0x1ee6b), (0x1ee73, 0x1ee73), (0x1ee78, 0x1ee78), (0x1ee7d, 0x1ee7d),
    (0x1ee7f, 0x1ee7f), (0x1ee8a, 0x1ee8a), (0x1ee9c, 0x1eea0), (0x1eea4, 0x1eea4), (0x1eeaa, 0x1eeaa), (0x1eebc, 0x1eeef),
    (0x1eef2, 0x1efff), (0x1f02c, 0x1f02f), (0x1f094, 0x1f09f), (0x1f0af, 0x1f0b0), (0x1f0c0, 0x1f0c0), (0x1f0d0, 0x1f0d0),
    (0x1f0f6, 0x1f0ff), (0x1f1ae, 0x1f1e5), (0x1f203, 0x1f20f), (0x1f23c, 0x1f23f), (0x1f249, 0x1f24f), (0x1f252, 0x1f25f),
    (0x1f266, 0x1f2ff), (0x1f6d8, 0x1f6db), (0x1f6ed, 0x1f6ef), (0x1f6fd, 0x1f6ff), (0x1f777, 0x1f77a), (0x1f7da, 0x1f7df),
    (0x1f7ec, 0x1f7ef), (0x1f7f1, 0x1f7ff), (0x1f80c, 0x1f80f), (0x1f848, 0x1f84f), (0x1f85a, 0x1f85f), (0x1f888, 0x1f88f),
    (0x1f8ae, 0x1f8af), (0x1f8bc, 0x1f8bf), (0x1f8c2, 0x1f8ff), (0x1fa54, 0x1fa5f), (0x1fa6e, 0x1fa6f), (0x1fa7d, 0x1fa7f),
    (0x1fa8a, 0x1fa8e), (0x1fac7, 0x1facd), (0x1fadd, 0x1fade), (0x1faea, 0x1faef), (0x1faf9, 0x1faff), (0x1fb93, 0x1fb93),
    (0x1fbfa, 0x1ffff), (0x2a6e0, 0x2a6ff), (0x2b73a, 0x2b73f), (0x2b81e, 0x2b81f), (0x2cea2, 0x2ceaf), (0x2ebe1, 0x2ebef),
    (0x2ee5e, 0x2f7ff), (0x2fa1e, 0x2ffff), (0x3134b, 0x3134f), (0x323b0, 0xe00ff), (0xe01f0, 0x10ffff),
];
