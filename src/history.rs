//! Port of `history.py`.
//!
//! What changed on this machine, and when: one timeline.
//!
//! "It worked last week" is the start of most troubleshooting, and the answer to
//! "what changed since?" is scattered: pacman.log for packages, the journal for
//! snapshots, `.bak` files for settings a tool reset, and nowhere at all for
//! what Almanac itself did. This puts them on one timeline, newest first,
//! grouped by day -- and marks the days that have a snapshot to go back to.
//!
//! Almanac's own actions (an update that did not change anything, settings
//! restored, file types handed back) are appended to a small JSON-lines file in
//! the XDG state directory. Everything else is read from where it already is;
//! nothing is collected that the system was not already keeping.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::LazyLock;

use chrono::{Local, NaiveDate, TimeZone};
use num_bigint::{BigInt, Sign};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::util;

/// recorded events kept; older ones drop off
pub const KEEP: usize = 2000;
pub const DAYS: i64 = 30;

pub const PACKAGES: &str = "packages";
pub const SNAPSHOT: &str = "snapshot";
pub const RESET: &str = "reset";
pub const ALMANAC: &str = "almanac";
/// What a scheduled run of almanac-autorun updated.
pub const AUTOUPDATE: &str = "autoupdate";

/// One entry of the timeline.
///
/// The typed fields are what the UI reads. Python's dataclass holds whatever
/// the history log had (a `null` title, a string `items`, a `true` ts) and
/// any str a Python str can be (lone surrogates, surrogate-escaped file
/// names); when an event carries such values, `py` keeps them exactly, and
/// they are what the event serialises as.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Event {
    pub ts: f64,
    pub kind: String,
    pub title: String,
    pub detail: String,
    pub items: Vec<String>,
    /// "warn" for things that went wrong
    pub severity: String,
    #[serde(skip)]
    pub py: Option<Box<PyFields>>,
}

/// The Python values of an event's fields, where they are not plain text.
#[derive(Debug, Clone, PartialEq)]
pub struct PyFields {
    pub ts: PyVal,
    pub title: PyVal,
    pub detail: PyVal,
    pub items: PyVal,
    pub severity: PyVal,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Day {
    /// Serialised as ISO `YYYY-MM-DD`, like Python's `date.isoformat()`.
    #[serde(with = "iso_date")]
    pub day: NaiveDate,
    pub events: Vec<Event>,
    /// a snapshot was taken this day
    pub snapshot: bool,
}

impl Event {
    /// `Event(ts, kind, title, detail="", items=[], severity="")`.
    pub fn new(ts: f64, kind: &str, title: &str, detail: &str, items: Vec<String>, severity: &str) -> Self {
        Event {
            ts,
            kind: kind.to_string(),
            title: title.to_string(),
            detail: detail.to_string(),
            items,
            severity: severity.to_string(),
            py: None,
        }
    }

    /// An event whose title and detail are Python strs (they may hold
    /// surrogates, which `String` cannot).
    fn from_text(ts: f64, kind: &str, title: PyStr, detail: PyStr) -> Self {
        let mut e = Event::new(ts, kind, &lossy(&title), &lossy(&detail), Vec::new(), "");
        if has_surrogate(&title) || has_surrogate(&detail) {
            e.py = Some(Box::new(PyFields {
                ts: PyVal::Float(ts),
                title: PyVal::Str(title),
                detail: PyVal::Str(detail),
                items: PyVal::List(Vec::new()),
                severity: PyVal::Str(Vec::new()),
            }));
        }
        e
    }

    /// An event built from what `json.loads` gave back, values as they are.
    fn from_py(kind: &str, f: PyFields) -> Self {
        let text = |v: &PyVal| match v {
            PyVal::Str(s) => lossy(s),
            _ => String::new(),
        };
        let items = match &f.items {
            PyVal::List(xs) => xs.iter().filter_map(|x| match x {
                PyVal::Str(s) => Some(lossy(s)),
                _ => None,
            }).collect(),
            // Iterating a str gives its characters.
            PyVal::Str(s) => s.iter().map(|&c| lossy(&[c])).collect(),
            _ => Vec::new(),
        };
        Event {
            ts: py_number(&f.ts),
            kind: kind.to_string(),
            title: text(&f.title),
            detail: text(&f.detail),
            items,
            severity: text(&f.severity),
            py: Some(Box::new(f)),
        }
    }

    /// The event as the parity runner sees Python's (`wire`: see `to_value`).
    pub fn to_value(&self, wire: bool) -> Value {
        let mut m = serde_json::Map::new();
        match &self.py {
            Some(f) => {
                m.insert("ts".into(), f.ts.to_value(wire));
                m.insert("kind".into(), Value::String(self.kind.clone()));
                m.insert("title".into(), f.title.to_value(wire));
                m.insert("detail".into(), f.detail.to_value(wire));
                m.insert("items".into(), f.items.to_value(wire));
                m.insert("severity".into(), f.severity.to_value(wire));
            }
            None => {
                m.insert("ts".into(), float_value(self.ts, wire));
                m.insert("kind".into(), Value::String(self.kind.clone()));
                m.insert("title".into(), Value::String(self.title.clone()));
                m.insert("detail".into(), Value::String(self.detail.clone()));
                m.insert("items".into(), self.items.iter().map(|s| Value::String(s.clone())).collect());
                m.insert("severity".into(), Value::String(self.severity.clone()));
            }
        }
        Value::Object(m)
    }
}

impl Serialize for Event {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.to_value(false).serialize(s)
    }
}

impl Day {
    /// "Today", "Yesterday", or e.g. "Monday 12 October".
    pub fn label(&self) -> String {
        use chrono::Datelike;
        // `date.today()`: today's date as glibc's localtime has it.
        let today = local_date(util::now()).unwrap_or_else(|_| Local::now().date_naive());
        if self.day == today {
            return "Today".to_string();
        }
        if (today - self.day).num_days() == 1 {
            return "Yesterday".to_string();
        }
        // Python's `strftime("%A %d %B")` follows LC_TIME, which the app sets
        // from the environment at start (i18n's setlocale; gtk::init here).
        // GLib's formatter reads the same C locale; chrono only knows English.
        let (y, m, d) = (self.day.year(), self.day.month() as i32, self.day.day() as i32);
        gtk::glib::DateTime::from_utc(y, m, d, 12, 0, 0.0)
            .and_then(|t| t.format("%A %d %B"))
            .map(|s| s.to_string())
            .unwrap_or_else(|_| self.day.format("%A %d %B").to_string())
    }
}

/// chrono is built without its serde feature; dates go through ISO strings.
mod iso_date {
    use chrono::NaiveDate;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(d: &NaiveDate, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&d.format("%Y-%m-%d").to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<NaiveDate, D::Error> {
        let text = String::deserialize(d)?;
        NaiveDate::parse_from_str(&text, "%Y-%m-%d").map_err(serde::de::Error::custom)
    }
}

fn _log_path() -> PathBuf {
    crate::xdg::state_home().join("almanac").join("history.jsonl")
}

/// Python's `str.splitlines()`: every Unicode line boundary, and no empty
/// last line after a trailing terminator.
fn splitlines(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let is_break = matches!(
            c,
            '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}'
        );
        if !is_break {
            continue;
        }
        out.push(&text[start..i]);
        let mut end = i + c.len_utf8();
        if c == '\r' {
            if let Some(&(j, '\n')) = chars.peek() {
                chars.next();
                end = j + 1;
            }
        }
        start = end;
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// `bytes.decode("utf-8")` as `Path.read_text()` does it: strict, so a log
/// that is not UTF-8 raises UnicodeDecodeError (not an OSError). The message
/// is CPython's: the bytes of the maximal invalid subpart, and why.
fn decode_strict(raw: Vec<u8>) -> Result<String, String> {
    String::from_utf8(raw).map_err(|e| {
        let b = e.as_bytes();
        let start = e.utf8_error().valid_up_to();
        let (end, reason) = match e.utf8_error().error_len() {
            Some(n) => {
                let lead = b[start];
                let invalid_start = (0x80..=0xc1).contains(&lead) || lead >= 0xf5;
                (start + n, if n == 1 && invalid_start { "invalid start byte" } else { "invalid continuation byte" })
            }
            None => (b.len(), "unexpected end of data"),
        };
        if end == start + 1 {
            format!("UnicodeDecodeError: 'utf-8' codec can't decode byte 0x{:02x} in position {start}: {reason}", b[start])
        } else {
            format!("UnicodeDecodeError: 'utf-8' codec can't decode bytes in position {start}-{}: {reason}", end - 1)
        }
    })
}

// ---- Python values -------------------------------------------------------------
//
// The log and the journal are read with Python's `json.loads`, which takes
// what serde_json does not (NaN, Infinity, 1e400 as inf, lone surrogate
// escapes, integers of any size), and the values are used as they come:
// compared with `>=`, passed to `int()`, kept in the event. So they are
// parsed into a small value type with Python's semantics.

/// A Python `str`: code points, lone surrogates included.
pub type PyStr = Vec<u32>;

/// A value as Python's `json.loads` returns it. As deep as Python's json
/// goes (52105 levels), so cloning, comparing and dropping it never recurse.
#[derive(Debug)]
pub enum PyVal {
    None,
    Bool(bool),
    Int(BigInt),
    Float(f64),
    Str(PyStr),
    List(Vec<PyVal>),
    /// Insertion order, as a Python dict keeps it.
    Dict(Vec<(PyStr, PyVal)>),
}

impl Drop for PyVal {
    // A deeply nested value would recurse through drop; take it apart flat.
    fn drop(&mut self) {
        fn children(v: &mut PyVal, out: &mut Vec<PyVal>) {
            match v {
                PyVal::List(items) => out.append(items),
                PyVal::Dict(entries) => out.extend(std::mem::take(entries).into_iter().map(|(_, x)| x)),
                _ => {}
            }
        }
        let mut stack = Vec::new();
        children(self, &mut stack);
        while let Some(mut v) = stack.pop() {
            children(&mut v, &mut stack);
        }
    }
}

crate::util::tree_clone_eq!(PyVal, key: PyStr, units: [None], leaves: [Bool, Int, Float, Str], list: List, dict: Dict);

impl PyVal {
    fn type_name(&self) -> &'static str {
        match self {
            PyVal::None => "NoneType",
            PyVal::Bool(_) => "bool",
            PyVal::Int(_) => "int",
            PyVal::Float(_) => "float",
            PyVal::Str(_) => "str",
            PyVal::List(_) => "list",
            PyVal::Dict(_) => "dict",
        }
    }

    /// `bool(v)`.
    fn truthy(&self) -> bool {
        match self {
            PyVal::None => false,
            PyVal::Bool(b) => *b,
            PyVal::Int(i) => i.sign() != Sign::NoSign,
            PyVal::Float(f) => *f != 0.0,
            PyVal::Str(s) => !s.is_empty(),
            PyVal::List(x) => !x.is_empty(),
            PyVal::Dict(x) => !x.is_empty(),
        }
    }

    /// `d.get(key)` on a dict.
    fn get(&self, key: &str) -> Option<&PyVal> {
        let PyVal::Dict(entries) = self else { return None };
        let key = py_str(key);
        entries.iter().find(|(k, _)| *k == key).map(|(_, v)| v)
    }

    /// The value as JSON. JSON cannot carry everything a Python value can;
    /// with `wire` such values are spelled out (`{"__float__": "inf"}`,
    /// `{"__str__": [code points]}`, as the parity cases encode Python's),
    /// without it they degrade (inf/NaN to null, a surrogate to U+FFFD).
    /// Built without recursion; the caller drops (or writes) the result on
    /// a stack that can take its depth.
    pub fn to_value(&self, wire: bool) -> Value {
        crate::util::tree::fold(
            self,
            PyVal::tree_shape,
            |n| match n {
                PyVal::None => Value::Null,
                PyVal::Bool(b) => Value::Bool(*b),
                PyVal::Int(i) => i64::try_from(i).map(Value::from)
                    .or_else(|_| u64::try_from(i).map(Value::from))
                    .unwrap_or_else(|_| float_value(int_f64(i), wire)),
                PyVal::Float(f) => float_value(*f, wire),
                PyVal::Str(s) => str_value(s, wire),
                PyVal::List(_) | PyVal::Dict(_) => unreachable!("containers are built from their children"),
            },
            |_, kids| Value::Array(kids),
            |n, kids| match n {
                PyVal::Dict(entries) => Value::Object(entries.iter().map(|(k, _)| lossy(k)).zip(kids).collect()),
                _ => unreachable!("only a dict is built as one"),
            },
        )
    }
}

fn py_str(s: &str) -> PyStr {
    s.chars().map(u32::from).collect()
}

fn lossy(s: &[u32]) -> String {
    s.iter().map(|&c| char::from_u32(c).unwrap_or('\u{fffd}')).collect()
}

/// Code points Python 3.14's `str.isprintable` (Unicode 16) rejects, as
/// inclusive ranges: categories Cc Cf Cs Co Cn Zl Zp and Zs except space.
/// Generated from CPython itself.
const PY_NOT_PRINTABLE: &[(u32, u32)] = &[
    (0x0, 0x1F), (0x7F, 0xA0), (0xAD, 0xAD), (0x378, 0x379), (0x380, 0x383), (0x38B, 0x38B),
    (0x38D, 0x38D), (0x3A2, 0x3A2), (0x530, 0x530), (0x557, 0x558), (0x58B, 0x58C), (0x590, 0x590),
    (0x5C8, 0x5CF), (0x5EB, 0x5EE), (0x5F5, 0x605), (0x61C, 0x61C), (0x6DD, 0x6DD), (0x70E, 0x70F),
    (0x74B, 0x74C), (0x7B2, 0x7BF), (0x7FB, 0x7FC), (0x82E, 0x82F), (0x83F, 0x83F), (0x85C, 0x85D),
    (0x85F, 0x85F), (0x86B, 0x86F), (0x88F, 0x896), (0x8E2, 0x8E2), (0x984, 0x984), (0x98D, 0x98E),
    (0x991, 0x992), (0x9A9, 0x9A9), (0x9B1, 0x9B1), (0x9B3, 0x9B5), (0x9BA, 0x9BB), (0x9C5, 0x9C6),
    (0x9C9, 0x9CA), (0x9CF, 0x9D6), (0x9D8, 0x9DB), (0x9DE, 0x9DE), (0x9E4, 0x9E5), (0x9FF, 0xA00),
    (0xA04, 0xA04), (0xA0B, 0xA0E), (0xA11, 0xA12), (0xA29, 0xA29), (0xA31, 0xA31), (0xA34, 0xA34),
    (0xA37, 0xA37), (0xA3A, 0xA3B), (0xA3D, 0xA3D), (0xA43, 0xA46), (0xA49, 0xA4A), (0xA4E, 0xA50),
    (0xA52, 0xA58), (0xA5D, 0xA5D), (0xA5F, 0xA65), (0xA77, 0xA80), (0xA84, 0xA84), (0xA8E, 0xA8E),
    (0xA92, 0xA92), (0xAA9, 0xAA9), (0xAB1, 0xAB1), (0xAB4, 0xAB4), (0xABA, 0xABB), (0xAC6, 0xAC6),
    (0xACA, 0xACA), (0xACE, 0xACF), (0xAD1, 0xADF), (0xAE4, 0xAE5), (0xAF2, 0xAF8), (0xB00, 0xB00),
    (0xB04, 0xB04), (0xB0D, 0xB0E), (0xB11, 0xB12), (0xB29, 0xB29), (0xB31, 0xB31), (0xB34, 0xB34),
    (0xB3A, 0xB3B), (0xB45, 0xB46), (0xB49, 0xB4A), (0xB4E, 0xB54), (0xB58, 0xB5B), (0xB5E, 0xB5E),
    (0xB64, 0xB65), (0xB78, 0xB81), (0xB84, 0xB84), (0xB8B, 0xB8D), (0xB91, 0xB91), (0xB96, 0xB98),
    (0xB9B, 0xB9B), (0xB9D, 0xB9D), (0xBA0, 0xBA2), (0xBA5, 0xBA7), (0xBAB, 0xBAD), (0xBBA, 0xBBD),
    (0xBC3, 0xBC5), (0xBC9, 0xBC9), (0xBCE, 0xBCF), (0xBD1, 0xBD6), (0xBD8, 0xBE5), (0xBFB, 0xBFF),
    (0xC0D, 0xC0D), (0xC11, 0xC11), (0xC29, 0xC29), (0xC3A, 0xC3B), (0xC45, 0xC45), (0xC49, 0xC49),
    (0xC4E, 0xC54), (0xC57, 0xC57), (0xC5B, 0xC5C), (0xC5E, 0xC5F), (0xC64, 0xC65), (0xC70, 0xC76),
    (0xC8D, 0xC8D), (0xC91, 0xC91), (0xCA9, 0xCA9), (0xCB4, 0xCB4), (0xCBA, 0xCBB), (0xCC5, 0xCC5),
    (0xCC9, 0xCC9), (0xCCE, 0xCD4), (0xCD7, 0xCDC), (0xCDF, 0xCDF), (0xCE4, 0xCE5), (0xCF0, 0xCF0),
    (0xCF4, 0xCFF), (0xD0D, 0xD0D), (0xD11, 0xD11), (0xD45, 0xD45), (0xD49, 0xD49), (0xD50, 0xD53),
    (0xD64, 0xD65), (0xD80, 0xD80), (0xD84, 0xD84), (0xD97, 0xD99), (0xDB2, 0xDB2), (0xDBC, 0xDBC),
    (0xDBE, 0xDBF), (0xDC7, 0xDC9), (0xDCB, 0xDCE), (0xDD5, 0xDD5), (0xDD7, 0xDD7), (0xDE0, 0xDE5),
    (0xDF0, 0xDF1), (0xDF5, 0xE00), (0xE3B, 0xE3E), (0xE5C, 0xE80), (0xE83, 0xE83), (0xE85, 0xE85),
    (0xE8B, 0xE8B), (0xEA4, 0xEA4), (0xEA6, 0xEA6), (0xEBE, 0xEBF), (0xEC5, 0xEC5), (0xEC7, 0xEC7),
    (0xECF, 0xECF), (0xEDA, 0xEDB), (0xEE0, 0xEFF), (0xF48, 0xF48), (0xF6D, 0xF70), (0xF98, 0xF98),
    (0xFBD, 0xFBD), (0xFCD, 0xFCD), (0xFDB, 0xFFF), (0x10C6, 0x10C6), (0x10C8, 0x10CC),
    (0x10CE, 0x10CF), (0x1249, 0x1249), (0x124E, 0x124F), (0x1257, 0x1257), (0x1259, 0x1259),
    (0x125E, 0x125F), (0x1289, 0x1289), (0x128E, 0x128F), (0x12B1, 0x12B1), (0x12B6, 0x12B7),
    (0x12BF, 0x12BF), (0x12C1, 0x12C1), (0x12C6, 0x12C7), (0x12D7, 0x12D7), (0x1311, 0x1311),
    (0x1316, 0x1317), (0x135B, 0x135C), (0x137D, 0x137F), (0x139A, 0x139F), (0x13F6, 0x13F7),
    (0x13FE, 0x13FF), (0x1680, 0x1680), (0x169D, 0x169F), (0x16F9, 0x16FF), (0x1716, 0x171E),
    (0x1737, 0x173F), (0x1754, 0x175F), (0x176D, 0x176D), (0x1771, 0x1771), (0x1774, 0x177F),
    (0x17DE, 0x17DF), (0x17EA, 0x17EF), (0x17FA, 0x17FF), (0x180E, 0x180E), (0x181A, 0x181F),
    (0x1879, 0x187F), (0x18AB, 0x18AF), (0x18F6, 0x18FF), (0x191F, 0x191F), (0x192C, 0x192F),
    (0x193C, 0x193F), (0x1941, 0x1943), (0x196E, 0x196F), (0x1975, 0x197F), (0x19AC, 0x19AF),
    (0x19CA, 0x19CF), (0x19DB, 0x19DD), (0x1A1C, 0x1A1D), (0x1A5F, 0x1A5F), (0x1A7D, 0x1A7E),
    (0x1A8A, 0x1A8F), (0x1A9A, 0x1A9F), (0x1AAE, 0x1AAF), (0x1ACF, 0x1AFF), (0x1B4D, 0x1B4D),
    (0x1BF4, 0x1BFB), (0x1C38, 0x1C3A), (0x1C4A, 0x1C4C), (0x1C8B, 0x1C8F), (0x1CBB, 0x1CBC),
    (0x1CC8, 0x1CCF), (0x1CFB, 0x1CFF), (0x1F16, 0x1F17), (0x1F1E, 0x1F1F), (0x1F46, 0x1F47),
    (0x1F4E, 0x1F4F), (0x1F58, 0x1F58), (0x1F5A, 0x1F5A), (0x1F5C, 0x1F5C), (0x1F5E, 0x1F5E),
    (0x1F7E, 0x1F7F), (0x1FB5, 0x1FB5), (0x1FC5, 0x1FC5), (0x1FD4, 0x1FD5), (0x1FDC, 0x1FDC),
    (0x1FF0, 0x1FF1), (0x1FF5, 0x1FF5), (0x1FFF, 0x200F), (0x2028, 0x202F), (0x205F, 0x206F),
    (0x2072, 0x2073), (0x208F, 0x208F), (0x209D, 0x209F), (0x20C1, 0x20CF), (0x20F1, 0x20FF),
    (0x218C, 0x218F), (0x242A, 0x243F), (0x244B, 0x245F), (0x2B74, 0x2B75), (0x2B96, 0x2B96),
    (0x2CF4, 0x2CF8), (0x2D26, 0x2D26), (0x2D28, 0x2D2C), (0x2D2E, 0x2D2F), (0x2D68, 0x2D6E),
    (0x2D71, 0x2D7E), (0x2D97, 0x2D9F), (0x2DA7, 0x2DA7), (0x2DAF, 0x2DAF), (0x2DB7, 0x2DB7),
    (0x2DBF, 0x2DBF), (0x2DC7, 0x2DC7), (0x2DCF, 0x2DCF), (0x2DD7, 0x2DD7), (0x2DDF, 0x2DDF),
    (0x2E5E, 0x2E7F), (0x2E9A, 0x2E9A), (0x2EF4, 0x2EFF), (0x2FD6, 0x2FEF), (0x3000, 0x3000),
    (0x3040, 0x3040), (0x3097, 0x3098), (0x3100, 0x3104), (0x3130, 0x3130), (0x318F, 0x318F),
    (0x31E6, 0x31EE), (0x321F, 0x321F), (0xA48D, 0xA48F), (0xA4C7, 0xA4CF), (0xA62C, 0xA63F),
    (0xA6F8, 0xA6FF), (0xA7CE, 0xA7CF), (0xA7D2, 0xA7D2), (0xA7D4, 0xA7D4), (0xA7DD, 0xA7F1),
    (0xA82D, 0xA82F), (0xA83A, 0xA83F), (0xA878, 0xA87F), (0xA8C6, 0xA8CD), (0xA8DA, 0xA8DF),
    (0xA954, 0xA95E), (0xA97D, 0xA97F), (0xA9CE, 0xA9CE), (0xA9DA, 0xA9DD), (0xA9FF, 0xA9FF),
    (0xAA37, 0xAA3F), (0xAA4E, 0xAA4F), (0xAA5A, 0xAA5B), (0xAAC3, 0xAADA), (0xAAF7, 0xAB00),
    (0xAB07, 0xAB08), (0xAB0F, 0xAB10), (0xAB17, 0xAB1F), (0xAB27, 0xAB27), (0xAB2F, 0xAB2F),
    (0xAB6C, 0xAB6F), (0xABEE, 0xABEF), (0xABFA, 0xABFF), (0xD7A4, 0xD7AF), (0xD7C7, 0xD7CA),
    (0xD7FC, 0xF8FF), (0xFA6E, 0xFA6F), (0xFADA, 0xFAFF), (0xFB07, 0xFB12), (0xFB18, 0xFB1C),
    (0xFB37, 0xFB37), (0xFB3D, 0xFB3D), (0xFB3F, 0xFB3F), (0xFB42, 0xFB42), (0xFB45, 0xFB45),
    (0xFBC3, 0xFBD2), (0xFD90, 0xFD91), (0xFDC8, 0xFDCE), (0xFDD0, 0xFDEF), (0xFE1A, 0xFE1F),
    (0xFE53, 0xFE53), (0xFE67, 0xFE67), (0xFE6C, 0xFE6F), (0xFE75, 0xFE75), (0xFEFD, 0xFF00),
    (0xFFBF, 0xFFC1), (0xFFC8, 0xFFC9), (0xFFD0, 0xFFD1), (0xFFD8, 0xFFD9), (0xFFDD, 0xFFDF),
    (0xFFE7, 0xFFE7), (0xFFEF, 0xFFFB), (0xFFFE, 0xFFFF), (0x1000C, 0x1000C), (0x10027, 0x10027),
    (0x1003B, 0x1003B), (0x1003E, 0x1003E), (0x1004E, 0x1004F), (0x1005E, 0x1007F),
    (0x100FB, 0x100FF), (0x10103, 0x10106), (0x10134, 0x10136), (0x1018F, 0x1018F),
    (0x1019D, 0x1019F), (0x101A1, 0x101CF), (0x101FE, 0x1027F), (0x1029D, 0x1029F),
    (0x102D1, 0x102DF), (0x102FC, 0x102FF), (0x10324, 0x1032C), (0x1034B, 0x1034F),
    (0x1037B, 0x1037F), (0x1039E, 0x1039E), (0x103C4, 0x103C7), (0x103D6, 0x103FF),
    (0x1049E, 0x1049F), (0x104AA, 0x104AF), (0x104D4, 0x104D7), (0x104FC, 0x104FF),
    (0x10528, 0x1052F), (0x10564, 0x1056E), (0x1057B, 0x1057B), (0x1058B, 0x1058B),
    (0x10593, 0x10593), (0x10596, 0x10596), (0x105A2, 0x105A2), (0x105B2, 0x105B2),
    (0x105BA, 0x105BA), (0x105BD, 0x105BF), (0x105F4, 0x105FF), (0x10737, 0x1073F),
    (0x10756, 0x1075F), (0x10768, 0x1077F), (0x10786, 0x10786), (0x107B1, 0x107B1),
    (0x107BB, 0x107FF), (0x10806, 0x10807), (0x10809, 0x10809), (0x10836, 0x10836),
    (0x10839, 0x1083B), (0x1083D, 0x1083E), (0x10856, 0x10856), (0x1089F, 0x108A6),
    (0x108B0, 0x108DF), (0x108F3, 0x108F3), (0x108F6, 0x108FA), (0x1091C, 0x1091E),
    (0x1093A, 0x1093E), (0x10940, 0x1097F), (0x109B8, 0x109BB), (0x109D0, 0x109D1),
    (0x10A04, 0x10A04), (0x10A07, 0x10A0B), (0x10A14, 0x10A14), (0x10A18, 0x10A18),
    (0x10A36, 0x10A37), (0x10A3B, 0x10A3E), (0x10A49, 0x10A4F), (0x10A59, 0x10A5F),
    (0x10AA0, 0x10ABF), (0x10AE7, 0x10AEA), (0x10AF7, 0x10AFF), (0x10B36, 0x10B38),
    (0x10B56, 0x10B57), (0x10B73, 0x10B77), (0x10B92, 0x10B98), (0x10B9D, 0x10BA8),
    (0x10BB0, 0x10BFF), (0x10C49, 0x10C7F), (0x10CB3, 0x10CBF), (0x10CF3, 0x10CF9),
    (0x10D28, 0x10D2F), (0x10D3A, 0x10D3F), (0x10D66, 0x10D68), (0x10D86, 0x10D8D),
    (0x10D90, 0x10E5F), (0x10E7F, 0x10E7F), (0x10EAA, 0x10EAA), (0x10EAE, 0x10EAF),
    (0x10EB2, 0x10EC1), (0x10EC5, 0x10EFB), (0x10F28, 0x10F2F), (0x10F5A, 0x10F6F),
    (0x10F8A, 0x10FAF), (0x10FCC, 0x10FDF), (0x10FF7, 0x10FFF), (0x1104E, 0x11051),
    (0x11076, 0x1107E), (0x110BD, 0x110BD), (0x110C3, 0x110CF), (0x110E9, 0x110EF),
    (0x110FA, 0x110FF), (0x11135, 0x11135), (0x11148, 0x1114F), (0x11177, 0x1117F),
    (0x111E0, 0x111E0), (0x111F5, 0x111FF), (0x11212, 0x11212), (0x11242, 0x1127F),
    (0x11287, 0x11287), (0x11289, 0x11289), (0x1128E, 0x1128E), (0x1129E, 0x1129E),
    (0x112AA, 0x112AF), (0x112EB, 0x112EF), (0x112FA, 0x112FF), (0x11304, 0x11304),
    (0x1130D, 0x1130E), (0x11311, 0x11312), (0x11329, 0x11329), (0x11331, 0x11331),
    (0x11334, 0x11334), (0x1133A, 0x1133A), (0x11345, 0x11346), (0x11349, 0x1134A),
    (0x1134E, 0x1134F), (0x11351, 0x11356), (0x11358, 0x1135C), (0x11364, 0x11365),
    (0x1136D, 0x1136F), (0x11375, 0x1137F), (0x1138A, 0x1138A), (0x1138C, 0x1138D),
    (0x1138F, 0x1138F), (0x113B6, 0x113B6), (0x113C1, 0x113C1), (0x113C3, 0x113C4),
    (0x113C6, 0x113C6), (0x113CB, 0x113CB), (0x113D6, 0x113D6), (0x113D9, 0x113E0),
    (0x113E3, 0x113FF), (0x1145C, 0x1145C), (0x11462, 0x1147F), (0x114C8, 0x114CF),
    (0x114DA, 0x1157F), (0x115B6, 0x115B7), (0x115DE, 0x115FF), (0x11645, 0x1164F),
    (0x1165A, 0x1165F), (0x1166D, 0x1167F), (0x116BA, 0x116BF), (0x116CA, 0x116CF),
    (0x116E4, 0x116FF), (0x1171B, 0x1171C), (0x1172C, 0x1172F), (0x11747, 0x117FF),
    (0x1183C, 0x1189F), (0x118F3, 0x118FE), (0x11907, 0x11908), (0x1190A, 0x1190B),
    (0x11914, 0x11914), (0x11917, 0x11917), (0x11936, 0x11936), (0x11939, 0x1193A),
    (0x11947, 0x1194F), (0x1195A, 0x1199F), (0x119A8, 0x119A9), (0x119D8, 0x119D9),
    (0x119E5, 0x119FF), (0x11A48, 0x11A4F), (0x11AA3, 0x11AAF), (0x11AF9, 0x11AFF),
    (0x11B0A, 0x11BBF), (0x11BE2, 0x11BEF), (0x11BFA, 0x11BFF), (0x11C09, 0x11C09),
    (0x11C37, 0x11C37), (0x11C46, 0x11C4F), (0x11C6D, 0x11C6F), (0x11C90, 0x11C91),
    (0x11CA8, 0x11CA8), (0x11CB7, 0x11CFF), (0x11D07, 0x11D07), (0x11D0A, 0x11D0A),
    (0x11D37, 0x11D39), (0x11D3B, 0x11D3B), (0x11D3E, 0x11D3E), (0x11D48, 0x11D4F),
    (0x11D5A, 0x11D5F), (0x11D66, 0x11D66), (0x11D69, 0x11D69), (0x11D8F, 0x11D8F),
    (0x11D92, 0x11D92), (0x11D99, 0x11D9F), (0x11DAA, 0x11EDF), (0x11EF9, 0x11EFF),
    (0x11F11, 0x11F11), (0x11F3B, 0x11F3D), (0x11F5B, 0x11FAF), (0x11FB1, 0x11FBF),
    (0x11FF2, 0x11FFE), (0x1239A, 0x123FF), (0x1246F, 0x1246F), (0x12475, 0x1247F),
    (0x12544, 0x12F8F), (0x12FF3, 0x12FFF), (0x13430, 0x1343F), (0x13456, 0x1345F),
    (0x143FB, 0x143FF), (0x14647, 0x160FF), (0x1613A, 0x167FF), (0x16A39, 0x16A3F),
    (0x16A5F, 0x16A5F), (0x16A6A, 0x16A6D), (0x16ABF, 0x16ABF), (0x16ACA, 0x16ACF),
    (0x16AEE, 0x16AEF), (0x16AF6, 0x16AFF), (0x16B46, 0x16B4F), (0x16B5A, 0x16B5A),
    (0x16B62, 0x16B62), (0x16B78, 0x16B7C), (0x16B90, 0x16D3F), (0x16D7A, 0x16E3F),
    (0x16E9B, 0x16EFF), (0x16F4B, 0x16F4E), (0x16F88, 0x16F8E), (0x16FA0, 0x16FDF),
    (0x16FE5, 0x16FEF), (0x16FF2, 0x16FFF), (0x187F8, 0x187FF), (0x18CD6, 0x18CFE),
    (0x18D09, 0x1AFEF), (0x1AFF4, 0x1AFF4), (0x1AFFC, 0x1AFFC), (0x1AFFF, 0x1AFFF),
    (0x1B123, 0x1B131), (0x1B133, 0x1B14F), (0x1B153, 0x1B154), (0x1B156, 0x1B163),
    (0x1B168, 0x1B16F), (0x1B2FC, 0x1BBFF), (0x1BC6B, 0x1BC6F), (0x1BC7D, 0x1BC7F),
    (0x1BC89, 0x1BC8F), (0x1BC9A, 0x1BC9B), (0x1BCA0, 0x1CBFF), (0x1CCFA, 0x1CCFF),
    (0x1CEB4, 0x1CEFF), (0x1CF2E, 0x1CF2F), (0x1CF47, 0x1CF4F), (0x1CFC4, 0x1CFFF),
    (0x1D0F6, 0x1D0FF), (0x1D127, 0x1D128), (0x1D173, 0x1D17A), (0x1D1EB, 0x1D1FF),
    (0x1D246, 0x1D2BF), (0x1D2D4, 0x1D2DF), (0x1D2F4, 0x1D2FF), (0x1D357, 0x1D35F),
    (0x1D379, 0x1D3FF), (0x1D455, 0x1D455), (0x1D49D, 0x1D49D), (0x1D4A0, 0x1D4A1),
    (0x1D4A3, 0x1D4A4), (0x1D4A7, 0x1D4A8), (0x1D4AD, 0x1D4AD), (0x1D4BA, 0x1D4BA),
    (0x1D4BC, 0x1D4BC), (0x1D4C4, 0x1D4C4), (0x1D506, 0x1D506), (0x1D50B, 0x1D50C),
    (0x1D515, 0x1D515), (0x1D51D, 0x1D51D), (0x1D53A, 0x1D53A), (0x1D53F, 0x1D53F),
    (0x1D545, 0x1D545), (0x1D547, 0x1D549), (0x1D551, 0x1D551), (0x1D6A6, 0x1D6A7),
    (0x1D7CC, 0x1D7CD), (0x1DA8C, 0x1DA9A), (0x1DAA0, 0x1DAA0), (0x1DAB0, 0x1DEFF),
    (0x1DF1F, 0x1DF24), (0x1DF2B, 0x1DFFF), (0x1E007, 0x1E007), (0x1E019, 0x1E01A),
    (0x1E022, 0x1E022), (0x1E025, 0x1E025), (0x1E02B, 0x1E02F), (0x1E06E, 0x1E08E),
    (0x1E090, 0x1E0FF), (0x1E12D, 0x1E12F), (0x1E13E, 0x1E13F), (0x1E14A, 0x1E14D),
    (0x1E150, 0x1E28F), (0x1E2AF, 0x1E2BF), (0x1E2FA, 0x1E2FE), (0x1E300, 0x1E4CF),
    (0x1E4FA, 0x1E5CF), (0x1E5FB, 0x1E5FE), (0x1E600, 0x1E7DF), (0x1E7E7, 0x1E7E7),
    (0x1E7EC, 0x1E7EC), (0x1E7EF, 0x1E7EF), (0x1E7FF, 0x1E7FF), (0x1E8C5, 0x1E8C6),
    (0x1E8D7, 0x1E8FF), (0x1E94C, 0x1E94F), (0x1E95A, 0x1E95D), (0x1E960, 0x1EC70),
    (0x1ECB5, 0x1ED00), (0x1ED3E, 0x1EDFF), (0x1EE04, 0x1EE04), (0x1EE20, 0x1EE20),
    (0x1EE23, 0x1EE23), (0x1EE25, 0x1EE26), (0x1EE28, 0x1EE28), (0x1EE33, 0x1EE33),
    (0x1EE38, 0x1EE38), (0x1EE3A, 0x1EE3A), (0x1EE3C, 0x1EE41), (0x1EE43, 0x1EE46),
    (0x1EE48, 0x1EE48), (0x1EE4A, 0x1EE4A), (0x1EE4C, 0x1EE4C), (0x1EE50, 0x1EE50),
    (0x1EE53, 0x1EE53), (0x1EE55, 0x1EE56), (0x1EE58, 0x1EE58), (0x1EE5A, 0x1EE5A),
    (0x1EE5C, 0x1EE5C), (0x1EE5E, 0x1EE5E), (0x1EE60, 0x1EE60), (0x1EE63, 0x1EE63),
    (0x1EE65, 0x1EE66), (0x1EE6B, 0x1EE6B), (0x1EE73, 0x1EE73), (0x1EE78, 0x1EE78),
    (0x1EE7D, 0x1EE7D), (0x1EE7F, 0x1EE7F), (0x1EE8A, 0x1EE8A), (0x1EE9C, 0x1EEA0),
    (0x1EEA4, 0x1EEA4), (0x1EEAA, 0x1EEAA), (0x1EEBC, 0x1EEEF), (0x1EEF2, 0x1EFFF),
    (0x1F02C, 0x1F02F), (0x1F094, 0x1F09F), (0x1F0AF, 0x1F0B0), (0x1F0C0, 0x1F0C0),
    (0x1F0D0, 0x1F0D0), (0x1F0F6, 0x1F0FF), (0x1F1AE, 0x1F1E5), (0x1F203, 0x1F20F),
    (0x1F23C, 0x1F23F), (0x1F249, 0x1F24F), (0x1F252, 0x1F25F), (0x1F266, 0x1F2FF),
    (0x1F6D8, 0x1F6DB), (0x1F6ED, 0x1F6EF), (0x1F6FD, 0x1F6FF), (0x1F777, 0x1F77A),
    (0x1F7DA, 0x1F7DF), (0x1F7EC, 0x1F7EF), (0x1F7F1, 0x1F7FF), (0x1F80C, 0x1F80F),
    (0x1F848, 0x1F84F), (0x1F85A, 0x1F85F), (0x1F888, 0x1F88F), (0x1F8AE, 0x1F8AF),
    (0x1F8BC, 0x1F8BF), (0x1F8C2, 0x1F8FF), (0x1FA54, 0x1FA5F), (0x1FA6E, 0x1FA6F),
    (0x1FA7D, 0x1FA7F), (0x1FA8A, 0x1FA8E), (0x1FAC7, 0x1FACD), (0x1FADD, 0x1FADE),
    (0x1FAEA, 0x1FAEF), (0x1FAF9, 0x1FAFF), (0x1FB93, 0x1FB93), (0x1FBFA, 0x1FFFF),
    (0x2A6E0, 0x2A6FF), (0x2B73A, 0x2B73F), (0x2B81E, 0x2B81F), (0x2CEA2, 0x2CEAF),
    (0x2EBE1, 0x2EBEF), (0x2EE5E, 0x2F7FF), (0x2FA1E, 0x2FFFF), (0x3134B, 0x3134F),
    (0x323B0, 0xE00FF), (0xE01F0, 0x10FFFF),
];

/// `str.isprintable` for one code point, by Python's own tables.
fn py_printable(cp: u32) -> bool {
    PY_NOT_PRINTABLE
        .binary_search_by(|&(a, b)| {
            if b < cp {
                std::cmp::Ordering::Less
            } else if a > cp {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_err()
}

/// Python's `repr(str)`, over code points (surrogates show as `\udcXX`),
/// as `int()` quotes a value it cannot read. (Same as autostart's.)
fn py_repr(cps: &[u32]) -> String {
    let has = |c: char| cps.contains(&(c as u32));
    let q = if has('\'') && !has('"') { '"' } else { '\'' };
    let printable = |cp: u32| char::from_u32(cp).is_some() && py_printable(cp);
    let mut out = String::new();
    out.push(q);
    for &cp in cps {
        match char::from_u32(cp) {
            Some('\\') => out.push_str("\\\\"),
            Some(c) if c == q => {
                out.push('\\');
                out.push(c);
            }
            Some('\t') => out.push_str("\\t"),
            Some('\n') => out.push_str("\\n"),
            Some('\r') => out.push_str("\\r"),
            Some(c) if printable(cp) => out.push(c),
            _ if cp < 0x100 => out.push_str(&format!("\\x{cp:02x}")),
            _ if cp < 0x10000 => out.push_str(&format!("\\u{cp:04x}")),
            _ => out.push_str(&format!("\\U{cp:08x}")),
        }
    }
    out.push(q);
    out
}

fn has_surrogate(s: &[u32]) -> bool {
    s.iter().any(|c| (0xd800..=0xdfff).contains(c))
}

fn float_value(f: f64, wire: bool) -> Value {
    match serde_json::Number::from_f64(f) {
        Some(n) => Value::Number(n),
        None if wire => {
            let name = if f.is_nan() { "nan" } else if f > 0.0 { "inf" } else { "-inf" };
            serde_json::json!({ "__float__": name })
        }
        None => Value::Null,
    }
}

fn str_value(s: &[u32], wire: bool) -> Value {
    if wire && has_surrogate(s) {
        serde_json::json!({ "__str__": s })
    } else {
        Value::String(lossy(s))
    }
}

/// `float(i)`, correctly rounded; inf where Python raises OverflowError.
fn int_f64(i: &BigInt) -> f64 {
    i.to_string().parse().unwrap_or(f64::NAN)
}

/// A JSON number (bool included) as a float, for the typed `Event::ts`.
fn py_number(v: &PyVal) -> f64 {
    match v {
        PyVal::Bool(b) => f64::from(u8::from(*b)),
        PyVal::Int(i) => int_f64(i),
        PyVal::Float(f) => *f,
        _ => f64::NAN,
    }
}

/// The exact integer value of a finite float's floor/trunc result.
fn float_int(f: f64) -> BigInt {
    format!("{f:.0}").parse().unwrap_or_default()
}

/// `i >= f`, compared exactly as Python compares an int with a float.
fn int_ge(i: &BigInt, f: f64) -> bool {
    if f.is_nan() || f == f64::INFINITY {
        return false;
    }
    if f == f64::NEG_INFINITY {
        return true;
    }
    let fl = f.floor();
    let fi = float_int(fl);
    if *i != fi {
        return *i > fi;
    }
    fl == f
}

/// `v >= since`; TypeError where Python cannot compare.
fn py_ge(v: &PyVal, since: f64) -> Result<bool, String> {
    match v {
        PyVal::Bool(b) => Ok(int_ge(&BigInt::from(u8::from(*b)), since)),
        PyVal::Int(i) => Ok(int_ge(i, since)),
        PyVal::Float(f) => Ok(*f >= since),
        other => Err(format!(
            "TypeError: '>=' not supported between instances of '{}' and 'float'",
            other.type_name()
        )),
    }
}

/// Python's `str.isspace()` for one code point (\x1c-\x1f included).
fn py_isspace(c: u32) -> bool {
    char::from_u32(c).is_some_and(|ch| ch.is_whitespace() || ('\x1c'..='\x1f').contains(&ch))
}

static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[\p{L}\p{N}]$").unwrap());

/// A word character for Python's `re` (`\w`): `str.isalnum()` or `_`.
/// (Rust's `\w` also takes marks and connector punctuation.)
fn py_isword(c: u32) -> bool {
    if c < 0x80 {
        let b = c as u8;
        return b.is_ascii_alphanumeric() || b == b'_';
    }
    char::from_u32(c).is_some_and(|ch| WORD.is_match(ch.encode_utf8(&mut [0; 4])))
}

/// `str.strip()` on a Python str.
fn py_strip_cp(s: &[u32]) -> &[u32] {
    let start = s.iter().position(|&c| !py_isspace(c)).unwrap_or(s.len());
    let end = s.iter().rposition(|&c| !py_isspace(c)).map_or(start, |e| e + 1);
    &s[start..end.max(start)]
}

/// `sys.get_int_max_str_digits()`: `int(str)` refuses longer numbers.
const INT_MAX_STR_DIGITS: usize = 4300;

/// `int(s)` for a str: surrounding whitespace, a sign, decimal digits of any
/// script, single underscores between digits.
fn py_int_str(s: &[u32]) -> Result<BigInt, String> {
    let bad = || format!("ValueError: invalid literal for int() with base 10: {}", py_repr(s));
    // ASCII whitespace as C's isspace (no \x1c-\x1f); beyond ASCII, str.isspace.
    let space = |c: u32| matches!(c, 0x20 | 0x09..=0x0d) || (c >= 0x80 && py_isspace(c));
    let start = s.iter().position(|&c| !space(c)).unwrap_or(s.len());
    let end = s.iter().rposition(|&c| !space(c)).map_or(start, |e| e + 1);
    let mut body = &s[start..end.max(start)];
    let mut text = String::new();
    if let Some(&c) = body.first() {
        if c == u32::from('-') || c == u32::from('+') {
            if c == u32::from('-') {
                text.push('-');
            }
            body = &body[1..];
        }
    }
    let mut digits = 0;
    let mut prev_digit = false;
    for (i, &c) in body.iter().enumerate() {
        if c == u32::from('_') {
            if !prev_digit || i + 1 == body.len() {
                return Err(bad());
            }
            prev_digit = false;
            continue;
        }
        let Some(d) = char::from_u32(c).and_then(crate::util::decimal_value) else { return Err(bad()) };
        text.push(char::from_digit(d, 10).unwrap_or('0'));
        digits += 1;
        prev_digit = true;
    }
    if digits == 0 {
        return Err(bad());
    }
    if digits > INT_MAX_STR_DIGITS {
        return Err(format!(
            "ValueError: Exceeds the limit ({INT_MAX_STR_DIGITS} digits) for integer string conversion: value has {digits} digits"
        ));
    }
    text.parse().map_err(|_| bad())
}

/// Python's `int(v)` for a JSON value.
fn py_int(v: &PyVal) -> Result<BigInt, String> {
    match v {
        PyVal::Bool(b) => Ok(BigInt::from(u8::from(*b))),
        PyVal::Int(i) => Ok(i.clone()),
        PyVal::Float(f) if f.is_nan() => Err("ValueError: cannot convert float NaN to integer".into()),
        PyVal::Float(f) if f.is_infinite() => {
            Err("OverflowError: cannot convert float infinity to integer".into())
        }
        PyVal::Float(f) => Ok(float_int(f.trunc())),
        PyVal::Str(s) => py_int_str(s),
        other => Err(format!(
            "TypeError: int() argument must be a string, a bytes-like object or a real number, not '{}'",
            other.type_name()
        )),
    }
}

/// `float(i)` where an int too large raises OverflowError.
fn int_to_float(i: &BigInt) -> Result<f64, String> {
    let f = int_f64(i);
    if f.is_finite() {
        Ok(f)
    } else {
        Err("OverflowError: int too large to convert to float".into())
    }
}

/// Why `json.loads` refused a document.
enum LoadErr {
    /// A ValueError (JSONDecodeError): the callers skip the line.
    Value,
    /// A RecursionError: not caught, so it reaches the caller. Which
    /// container ("array" or "object") was being entered.
    Recursion(&'static str),
}

/// CPython 3.14's RecursionError from the C json scanner, which checks the C
/// stack on entering each array or object. The kB figure is what a plain
/// process on the default 8 MiB stack reports; it varies a little run to run.
fn recursion(what: &str) -> String {
    format!("RecursionError: Stack overflow (used 8148 kB) while decoding a JSON {what} from a unicode string")
}

/// Nesting at which CPython 3.14's C json scanner raises RecursionError on
/// the default 8 MiB stack (as measured for settings.rs). CPython's limit is
/// the stack itself, so it is not exact: measured runs gave out between
/// about 52090 and 52110 levels.
const MAX_DEPTH: usize = 52105;

/// `json.loads(text)` for a str, following CPython's C scanner (without
/// recursion, so a deeply nested line cannot overflow the stack).
fn py_loads(text: &str) -> Result<PyVal, LoadErr> {
    use crate::util::pyjson;
    pyjson::loads(text, pyjson::Limit::containers(MAX_DEPTH)).map_err(|e| match e {
        pyjson::Error::Value(_) => LoadErr::Value,
        pyjson::Error::Recursion(is_obj) => LoadErr::Recursion(if is_obj { "object" } else { "array" }),
    })
}

impl crate::util::pyjson::Build for PyVal {
    type Key = PyStr;
    fn null() -> Self {
        PyVal::None
    }
    fn bool(b: bool) -> Self {
        PyVal::Bool(b)
    }
    fn num(n: crate::util::pyjson::Num<'_>) -> Self {
        use crate::util::pyjson::Num;
        match n {
            Num::Int(i) => PyVal::Int(BigInt::from(i)),
            Num::UInt(u) => PyVal::Int(BigInt::from(u)),
            Num::Big(digits) => PyVal::Int(BigInt::parse_bytes(digits.as_bytes(), 10).unwrap_or_default()),
            Num::Float(f) => PyVal::Float(f),
        }
    }
    fn str(s: String) -> Self {
        PyVal::Str(s.chars().map(u32::from).collect())
    }
    fn wstr(units: Vec<u32>) -> Self {
        PyVal::Str(units)
    }
    fn key(s: String) -> PyStr {
        s.chars().map(u32::from).collect()
    }
    fn wkey(units: Vec<u32>) -> PyStr {
        units
    }
    fn list(items: Vec<Self>) -> Self {
        PyVal::List(items)
    }
    fn dict(pairs: Vec<(PyStr, Self)>) -> Self {
        // A repeated key keeps its first place and takes the new value.
        PyVal::Dict(crate::util::pyjson::dict_pairs(pairs))
    }
}

/// `json.loads(line)` where the caller catches ValueError (`Ok(None)`).
fn loads_line(line: &str) -> Result<Option<PyVal>, String> {
    match py_loads(line) {
        Ok(v) => Ok(Some(v)),
        Err(LoadErr::Value) => Ok(None),
        Err(LoadErr::Recursion(what)) => Err(recursion(what)),
    }
}

// ---- what Almanac did --------------------------------------------------------

/// Append one of Almanac's own actions. Never raises.
///
/// Python's `items=None` is an empty slice here. (The Python can raise all
/// the same: see `record_checked`. What happens then is up to each caller --
/// a GTK handler's traceback, the rest of the handler skipped -- and the
/// callers here take no error, so it is printed and the caller carries on.
/// Callers that should stop as Python's do call `record_checked`.)
#[deprecated(note = "the Python raises out of record() when trimming a log that is not UTF-8; use record_checked and stop on Err as the Python caller would")]
pub fn record(kind: &str, title: &str, detail: &str, severity: &str, items: &[String]) {
    if let Err(e) = record_checked(kind, title, detail, severity, items) {
        eprintln!("{e}");
    }
}

/// `record` with what escapes Python's `except OSError`: trimming a log
/// that is not UTF-8 raises UnicodeDecodeError (after the line is written).
pub fn record_checked(kind: &str, title: &str, detail: &str, severity: &str, items: &[String]) -> Result<(), String> {
    let path = _log_path();
    let r = (|| -> std::io::Result<Result<(), String>> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let line = util::dumps(
            &serde_json::json!({"ts": util::now(), "kind": kind, "title": title,
                                "detail": detail, "severity": severity,
                                "items": items}),
            None,
            false,
        );
        let mut fh = std::fs::OpenOptions::new().append(true).create(true).open(&path)?;
        fh.write_all(format!("{line}\n").as_bytes())?;
        drop(fh);
        // Trim occasionally rather than on every write.
        if std::fs::metadata(&path)?.len() > 512 * 1024 {
            let text = match decode_strict(std::fs::read(&path)?) {
                Ok(t) => t,
                Err(e) => return Ok(Err(e)),
            };
            let lines = splitlines(&text);
            let lines = &lines[lines.len().saturating_sub(KEEP)..];
            std::fs::write(&path, lines.join("\n") + "\n")?;
        }
        Ok(Ok(()))
    })();
    r.unwrap_or(Ok(()))
}

/// Almanac's own recorded actions since `since` (epoch seconds), values as
/// the log has them. `Err` where Python raises: a log that is not UTF-8, a
/// line that is not an object, a "ts" that is missing (when 0 >= since) or
/// cannot be compared.
pub fn recorded(since: f64) -> Result<Vec<Event>, String> {
    let Ok(raw) = std::fs::read(_log_path()) else {
        return Ok(Vec::new());
    };
    let text = decode_strict(raw)?;
    let mut out = Vec::new();
    for line in splitlines(&text) {
        let Some(d) = loads_line(line)? else { continue };
        if !matches!(d, PyVal::Dict(_)) {
            return Err(format!("AttributeError: '{}' object has no attribute 'get'", d.type_name()));
        }
        let zero = PyVal::Int(BigInt::from(0));
        if !py_ge(d.get("ts").unwrap_or(&zero), since)? {
            continue;
        }
        let Some(ts) = d.get("ts") else { return Err("KeyError: 'ts'".into()) };
        let text = |k: &str| d.get(k).cloned().unwrap_or(PyVal::Str(Vec::new()));
        let items = match d.get("items") {
            Some(v) if v.truthy() => v.clone(),
            _ => PyVal::List(Vec::new()),
        };
        out.push(Event::from_py(ALMANAC, PyFields {
            ts: ts.clone(),
            title: text("title"),
            detail: text("detail"),
            items,
            severity: text("severity"),
        }));
    }
    Ok(out)
}

// ---- what the system recorded ------------------------------------------------

static _LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[(?P<ts>[^\]]+)\] \[(?P<who>[^\]]+)\] (?P<msg>.*)$").unwrap());
// Python's \S also excludes \x1c-\x1f, which Unicode does not call whitespace.
static _CHANGE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(installed|upgraded|removed|downgraded|reinstalled) ([^\s\x1c-\x1f]+)").unwrap()
});

/// The last 4 MiB of pacman.log, decoded with replacement.
fn read_log_tail() -> std::io::Result<String> {
    let mut fh = std::fs::File::open(crate::snapshots::PACMAN_LOG)?;
    let end = fh.seek(SeekFrom::End(0))?;
    fh.seek(SeekFrom::Start(end.saturating_sub(4 * 1024 * 1024)))?;
    let mut buf = Vec::new();
    fh.read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// One event per pacman transaction that changed something.
///
/// `log_text` None reads the tail of `snapshots::PACMAN_LOG`.
pub fn package_events(since: f64, log_text: Option<&str>) -> Vec<Event> {
    let owned;
    let log_text = match log_text {
        Some(t) => t,
        None => match read_log_tail() {
            Ok(t) => {
                owned = t;
                owned.as_str()
            }
            Err(_) => return Vec::new(),
        },
    };
    let mut out: Vec<Event> = Vec::new();
    let mut current: Option<(f64, Vec<String>)> = None;
    for line in splitlines(log_text) {
        let Some(m) = _LINE.captures(line) else { continue };
        let msg = &m["msg"];
        if msg == "transaction started" {
            current = Some((crate::snapshots::_ts(&m["ts"]), Vec::new()));
        } else if current.is_none() {
            continue;
        } else if &m["who"] == "ALPM" && _CHANGE.is_match(msg) {
            if let Some((_, changes)) = current.as_mut() {
                changes.push(msg.to_string());
            }
        } else if msg == "transaction completed" {
            let Some((started, changes)) = current.take() else { continue };
            // `omarchy update` reinstalls archlinux-keyring every time: same
            // version, nothing changed. A transaction of nothing but those is
            // noise between the entries that matter.
            if changes.iter().all(|c| c.starts_with("reinstalled ")) {
                continue;
            }
            if !changes.is_empty() && started >= since {
                out.push(Event::new(started, PACKAGES, &_summarise(&changes), "", changes, ""));
            }
        }
    }
    out
}

/// 'Updated 5 packages · installed wordcraft' -- verbs in a fixed order.
pub fn _summarise(changes: &[String]) -> String {
    let mut verbs: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for c in changes {
        if let Some(m) = _CHANGE.captures(c) {
            verbs.entry(m[1].to_string()).or_default().push(m[2].to_string());
        }
    }
    let mut parts: Vec<String> = Vec::new();
    for (verb, label) in [("upgraded", "Updated"), ("installed", "Installed"),
                          ("removed", "Removed"), ("downgraded", "Downgraded"),
                          ("reinstalled", "Reinstalled")] {
        let Some(names) = verbs.get(verb).filter(|n| !n.is_empty()) else { continue };
        let what = if names.len() == 1 { names[0].clone() } else { format!("{} packages", names.len()) };
        let label = if parts.is_empty() { label.to_string() } else { label.to_lowercase() };
        parts.push(format!("{label} {what}"));
    }
    if parts.is_empty() {
        "Package change".to_string()
    } else {
        parts.join(" · ")
    }
}

/// "a", "a and b", "a, b and c", or "{n} {noun}" past three.
fn few(names: &[String], noun: &str) -> String {
    match names {
        [one] => one.clone(),
        [a, b] => format!("{a} and {b}"),
        [a, b, c] => format!("{a}, {b} and {c}"),
        _ => format!("{} {noun}", names.len()),
    }
}

/// One line for the top of History: everything that changed in the 24 hours
/// before `now`, so "did something install last night?" has an answer
/// without reading the timeline. "Nothing changed." when nothing did.
pub fn last_day(days: &[Day], now: f64) -> String {
    let since = now - 86_400.0;
    let mut verbs: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let (mut scheduled, mut snapshots, mut resets, mut other) = (Vec::<String>::new(), 0, 0, 0);
    let push = |list: &mut Vec<String>, name: &str| {
        if !list.iter().any(|n| n == name) {
            list.push(name.to_string());
        }
    };
    for e in days.iter().flat_map(|d| &d.events).filter(|e| e.ts >= since && e.ts <= now) {
        match e.kind.as_str() {
            PACKAGES => {
                for c in &e.items {
                    let Some(m) = _CHANGE.captures(c) else { continue };
                    let verb = match &m[1] {
                        "upgraded" => "upgraded",
                        "installed" => "installed",
                        "removed" => "removed",
                        "downgraded" => "downgraded",
                        _ => continue, // reinstalled: nothing changed
                    };
                    push(verbs.entry(verb).or_default(), &m[2]);
                }
            }
            AUTOUPDATE => {
                for item in &e.items {
                    push(&mut scheduled, item.split(' ').next().unwrap_or(item));
                }
            }
            SNAPSHOT => snapshots += 1,
            RESET => resets += 1,
            _ => other += 1,
        }
    }
    let plural = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let mut parts: Vec<String> = Vec::new();
    for (verb, label) in [("upgraded", "updated"), ("installed", "installed"),
                          ("removed", "removed"), ("downgraded", "downgraded")] {
        if let Some(names) = verbs.get(verb) {
            parts.push(format!("{label} {}", few(names, "packages")));
        }
    }
    if !scheduled.is_empty() {
        parts.push(format!("the scheduled update updated {}", few(&scheduled, "items")));
    }
    if snapshots > 0 {
        parts.push(format!("{} taken", plural(snapshots, "snapshot", "snapshots")));
    }
    if resets > 0 {
        parts.push(format!("{} reset to defaults", plural(resets, "settings file", "settings files")));
    }
    if other > 0 {
        parts.push(format!("{} made in Almanac", plural(other, "other change", "other changes")));
    }
    let Some(first) = parts.first_mut() else { return "Nothing changed.".to_string() };
    let mut chars = first.chars();
    if let Some(c) = chars.next() {
        *first = c.to_uppercase().chain(chars).collect();
    }
    parts.join(" · ")
}

/// `re.search(r"COMMAND=\S*snapper\b.*\bcreate\b", msg)`, with Python's
/// `\S`, `\w` (for `\b`) and `.` (anything but a newline).
fn snapper_create(msg: &[u32]) -> bool {
    let n = msg.len();
    let at = |i: usize, lit: &str| {
        let lit = py_str(lit);
        i + lit.len() <= n && msg[i..i + lit.len()] == lit[..]
    };
    let word = |i: usize| i < n && py_isword(msg[i]);
    let nl = u32::from('\n');
    for i in 0..n {
        if !at(i, "COMMAND=") {
            continue;
        }
        let mut j = i + 8;
        loop {
            if at(j, "snapper") && !word(j + 7) {
                let mut k = j + 7;
                while k < n {
                    if at(k, "create") && !word(k - 1) && !word(k + 6) {
                        return true;
                    }
                    if msg[k] == nl {
                        break;
                    }
                    k += 1;
                }
            }
            if j >= n || py_isspace(msg[j]) {
                break;
            }
            j += 1;
        }
    }
    false
}

/// `re.search(r"-d (.+?)(?:$| -)", msg).group(1)`; Python's `$` also
/// matches before a final newline.
fn desc_group(msg: &[u32]) -> Option<&[u32]> {
    let n = msg.len();
    let at = |i: usize, lit: &str| {
        let lit = py_str(lit);
        i + lit.len() <= n && msg[i..i + lit.len()] == lit[..]
    };
    let nl = u32::from('\n');
    for p in 0..n {
        if !at(p, "-d ") {
            continue;
        }
        let s = p + 3;
        if s >= n || msg[s] == nl {
            continue;
        }
        let mut e = s + 1;
        loop {
            if e == n || (e + 1 == n && msg[e] == nl) || at(e, " -") {
                return Some(&msg[s..e]);
            }
            if msg[e] == nl {
                break;
            }
            e += 1;
        }
    }
    None
}

/// `f"{int(since)}"` for a float.
fn int_text(since: f64) -> Result<String, String> {
    if since.is_nan() {
        return Err("ValueError: cannot convert float NaN to integer".into());
    }
    if since.is_infinite() {
        return Err("OverflowError: cannot convert float infinity to integer".into());
    }
    Ok(float_int(since.trunc()).to_string())
}

/// `snapper create` runs recorded by sudo or pkexec in the journal.
///
/// `journal_text` None runs `journalctl -o json`. `Err` where Python raises:
/// a line that is not an object, a `__REALTIME_TIMESTAMP` that `int()`
/// refuses or that is too large for a float.
pub fn snapshot_events(since: f64, journal_text: Option<&str>) -> Result<Vec<Event>, String> {
    let owned;
    let journal_text = match journal_text {
        Some(t) => t,
        None => {
            let since_arg = format!("@{}", int_text(since)?);
            // Python decodes strictly (text=True); journalctl's JSON output is
            // UTF-8 by construction (a field that is not comes as a byte array).
            let r = util::run(
                &["journalctl", "--no-pager", "-q", "-o", "json",
                  "--since", &since_arg, "_COMM=sudo", "+", "_COMM=pkexec"],
                30,
            );
            if r.failed_to_start || r.timed_out {
                return Ok(Vec::new());
            }
            owned = if r.ok() { r.stdout } else { String::new() };
            owned.as_str()
        }
    };
    let mut out = Vec::new();
    for line in splitlines(journal_text) {
        let Some(rec) = loads_line(line)? else { continue };
        if !matches!(rec, PyVal::Dict(_)) {
            return Err(format!("AttributeError: '{}' object has no attribute 'get'", rec.type_name()));
        }
        // `rec.get("MESSAGE") or ""`: binary messages come as byte arrays.
        let Some(PyVal::Str(msg)) = rec.get("MESSAGE") else { continue };
        if !snapper_create(msg) {
            continue;
        }
        let desc = desc_group(msg);
        let ts = match rec.get("__REALTIME_TIMESTAMP") {
            None => BigInt::from(0),
            Some(v) => py_int(v)?,
        };
        let ts = int_to_float(&ts)? / 1e6;
        let detail = match desc {
            Some(d) => {
                let mut t = py_str("“");
                t.extend_from_slice(py_strip_cp(d));
                t.extend(py_str("”"));
                t
            }
            None => Vec::new(),
        };
        out.push(Event::from_text(ts, SNAPSHOT, py_str("Snapshot taken"), detail));
    }
    Ok(out)
}

/// `os.fsdecode`: UTF-8, each undecodable byte as a surrogate escape.
fn fs_decode(raw: &[u8]) -> PyStr {
    let mut out = Vec::new();
    for chunk in raw.utf8_chunks() {
        out.extend(chunk.valid().chars().map(u32::from));
        out.extend(chunk.invalid().iter().map(|&b| 0xdc00 | u32::from(b)));
    }
    out
}

/// Config files a tool replaced, from the backups it left behind.
///
/// `Err` where Python raises: a backup of "." (`with_name('.')`).
pub fn reset_events(since: f64) -> Result<Vec<Event>, String> {
    use crate::guard;
    let mut out = Vec::new();
    let home = guard::config_home();
    for f in guard::walk(&home, guard::MAX_DEPTH) {
        let Some(fname) = f.file_name() else { continue };
        let name = fname.to_string_lossy();
        let Some(m) = guard::BAK.captures(&name) else { continue };
        let Some(ts) = crate::util::py_digits_value(&m["ts"]) else {
            return Err(format!("ValueError: could not convert string to float: {}", py_repr(&py_str(&m["ts"]))));
        };
        if ts < since {
            continue;
        }
        // The base name is the file's own bytes up to the matched suffix,
        // which is plain text even when the name is not UTF-8.
        let suffix = name.len() - m.name("base").map(|b| b.end()).unwrap_or(0);
        let raw = fname.as_bytes();
        let base = &raw[..raw.len() - suffix];
        if base == b"." {
            return Err("ValueError: Invalid name '.'".into());
        }
        let based = f.with_file_name(std::ffi::OsStr::from_bytes(base));
        let rel = based.strip_prefix(&home).unwrap_or(&based);
        let mut title = fs_decode(rel.as_os_str().as_bytes());
        title.extend(py_str(" replaced"));
        let mut detail = py_str("Your previous version was kept as ");
        detail.extend(fs_decode(raw));
        out.push(Event::from_text(ts, RESET, title, detail));
    }
    Ok(out)
}

// ---- the timeline ------------------------------------------------------------

/// Whether glibc's localtime, which Python's `datetime.fromtimestamp` and
/// `date.today` use, reads this process's TZ as UTC where chrono's `Local`
/// would not. glibc takes a TZ it cannot read (a typo such as
/// `Nowhere/Invalid`) as UTC; chrono drops it and falls back to
/// /etc/localtime. GLib reads TZ as glibc does (a zoneinfo name, `:file`, or
/// a POSIX rule), and makes one it cannot read UTC, named "UTC". (A TZ of
/// UTC itself reads as UTC either way.) No TZ, or one that reads, chrono
/// takes as glibc does.
fn tz_unreadable() -> bool {
    match std::env::var_os("TZ") {
        None => false,
        Some(v) => v.to_str().is_none_or(|tz| gtk::glib::TimeZone::new(Some(tz)).identifier() == "UTC"),
    }
}

/// The local UTC offset (seconds east) in force at `t`, as glibc's
/// localtime finds it; 0 where it reads TZ as UTC. chrono reads the zone
/// for about ±262,000 years only; past the zone's last transition the rule
/// repeats every 400 Gregorian years (the calendar does), and before its
/// first one the zone keeps its first offset. (As snapshots does it.)
fn local_offset(t: i64, utc: bool) -> i64 {
    use chrono::Offset;
    const CYCLE: i64 = 12_622_780_800;
    const HI: i64 = 150_000_000_000;
    const LO: i64 = -50_000_000_000;
    if utc {
        return 0;
    }
    let probe = if t >= HI {
        HI + (t - HI).rem_euclid(CYCLE)
    } else if t < LO {
        LO
    } else {
        t
    };
    match chrono::DateTime::from_timestamp(probe, 0) {
        Some(dt) => i64::from(Local.offset_from_utc_datetime(&dt.naive_utc()).fix().local_minus_utc()),
        None => 0,
    }
}

/// Days since 1970-01-01 to (year, month, day), for any year.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// `datetime.fromtimestamp(ts).date()`: the local calendar day, after
/// Python's rounding to the microsecond (half to even); `Err` where Python
/// raises (NaN, out of range), with Python's message.
fn local_date(ts: f64) -> Result<NaiveDate, String> {
    local_date_in(ts, tz_unreadable())
}

/// `local_date`, with `utc` from `tz_unreadable` (asked once per timeline).
fn local_date_in(ts: f64, utc: bool) -> Result<NaiveDate, String> {
    const TIME_T: &str = "OverflowError: timestamp out of range for platform time_t";
    if ts.is_nan() {
        return Err("ValueError: Invalid value NaN (not a number)".into());
    }
    if !ts.is_finite() {
        return Err(TIME_T.into());
    }
    let (mut secs, frac) = (ts.trunc(), ts.fract());
    let us = util::py_round(frac * 1e6);
    if us >= 1e6 {
        secs += 1.0;
    } else if us < 0.0 {
        secs -= 1.0;
    }
    // time_t is 64 bits: [-2**63, 2**63).
    if !(-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&secs) {
        return Err(TIME_T.into());
    }
    let t = secs as i64;
    // localtime_r fails (EOVERFLOW) where the year does not fit a C int.
    let fits = |year: i64| i32::try_from(year - 1900).is_ok();
    let utc_year = civil_from_days(t.div_euclid(86_400)).0;
    let days = (i128::from(t) + i128::from(local_offset(t, utc))).div_euclid(86_400);
    let overflow = || "OSError: [Errno 75] Value too large for defined data type".to_string();
    let days = i64::try_from(days).map_err(|_| overflow())?;
    let (year, month, day) = civil_from_days(days);
    if !fits(utc_year) || !fits(year) {
        return Err(overflow());
    }
    if !(1..=9999).contains(&year) {
        return Err(format!("ValueError: year must be in 1..9999, not {year}"));
    }
    NaiveDate::from_ymd_opt(year as i32, month, day).ok_or_else(|| format!("ValueError: year must be in 1..9999, not {year}"))
}

/// Every event of the last `days` days, newest first, grouped by day.
///
/// Python's `timeline()` raises where something cannot be read (a log that
/// is not UTF-8, a line that is not an object, a time out of range), and the
/// app calls it on a worker thread with no handler: the thread dies with a
/// traceback, nothing is shown, and the History page stays on "Reading the
/// last 30 days…" -- it never claims there is nothing to show. The UI calls
/// this through `shell::spawn_bg` (gio::spawn_blocking), which catches a
/// panic in the work and then never runs the callback; so the same error
/// panics here, with Python's message, to the same effect. Callers that can
/// take the error use `timeline_result`.
pub fn timeline(days: i64) -> Vec<Day> {
    match timeline_result(days) {
        Ok(days) => days,
        Err(e) => panic!("{e}"),
    }
}

/// `timeline`, with Python's exceptions as `Err`.
pub fn timeline_result(days: i64) -> Result<Vec<Day>, String> {
    if crate::demo::active() {
        return Ok(crate::demo::history());
    }
    let since = util::now() - (i128::from(days) * 86400) as f64;
    let mut events = package_events(since, None);
    events.extend(snapshot_events(since, None)?);
    events.extend(reset_events(since)?);
    events.extend(recorded(since)?);
    group(events)
}

/// The second half of `timeline`: sort newest first, group by local day.
/// (Separate so the tests can feed it events, as Python's mock.patch does.)
fn group(mut events: Vec<Event>) -> Result<Vec<Day>, String> {
    // Stable, like Python's sort(reverse=True): equal times keep their order.
    events.sort_by(|a, b| b.ts.partial_cmp(&a.ts).unwrap_or(std::cmp::Ordering::Equal));
    let mut by_day: BTreeMap<NaiveDate, Vec<Event>> = BTreeMap::new();
    let utc = tz_unreadable();
    for e in events {
        by_day.entry(local_date_in(e.ts, utc)?).or_default().push(e);
    }
    Ok(by_day
        .into_iter()
        .rev()
        .map(|(d, evs)| {
            let snapshot = evs.iter().any(|e| e.kind == SNAPSHOT);
            Day { day: d, events: evs, snapshot }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day_of(events: Vec<Event>) -> Vec<Day> {
        vec![Day { day: NaiveDate::from_ymd_opt(2026, 10, 18).unwrap(), events, snapshot: false }]
    }

    fn changes(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn last_day_answers_what_changed_overnight() {
        let now = 1_792_400_000.0;
        // 2026-10-17: rust by hand at 18:18; 2026-10-18: the timer's AppImages.
        let days = day_of(vec![
            Event::new(now - 3600.0, AUTOUPDATE, "Scheduled update: updated vectorcraft and wordcraft", "",
                       changes(&["vectorcraft 0.12.0 → 0.13.1", "wordcraft 0.7.0 → 0.8.0"]), ""),
            Event::new(now - 20.0 * 3600.0, PACKAGES, "Installed 2 packages", "",
                       changes(&["installed lld (22.1.8-1)", "installed rust (1:1.98.1-1)"]), ""),
            Event::new(now - 30.0 * 3600.0, PACKAGES, "Updated hermes-desktop", "",
                       changes(&["upgraded hermes-desktop (1 -> 2)"]), ""),
        ]);
        assert_eq!(
            last_day(&days, now),
            "Installed lld and rust · the scheduled update updated vectorcraft and wordcraft"
        );
    }

    #[test]
    fn last_day_counts_past_three_and_skips_reinstalls() {
        let now = 1_792_400_000.0;
        let days = day_of(vec![
            Event::new(now - 60.0, PACKAGES, "", "", changes(&[
                "upgraded a (1 -> 2)", "upgraded b (1 -> 2)", "upgraded c (1 -> 2)", "upgraded d (1 -> 2)",
                "upgraded a (2 -> 3)", "reinstalled archlinux-keyring (1)", "removed e (1)",
            ]), ""),
            Event::new(now - 120.0, SNAPSHOT, "Snapshot", "", vec![], ""),
            Event::new(now - 180.0, RESET, "x reset", "", vec![], ""),
            Event::new(now - 240.0, RESET, "y reset", "", vec![], ""),
            Event::new(now - 300.0, ALMANAC, "Shell restarted", "", vec![], ""),
        ]);
        assert_eq!(
            last_day(&days, now),
            "Updated 4 packages · removed e · 1 snapshot taken · 2 settings files reset to defaults \
             · 1 other change made in Almanac"
        );
        assert_eq!(last_day(&day_of(vec![]), now), "Nothing changed.");
        // Older than a day, or in the future (a clock that jumped): not counted.
        let stale = day_of(vec![
            Event::new(now - 90_000.0, SNAPSHOT, "", "", vec![], ""),
            Event::new(now + 600.0, SNAPSHOT, "", "", vec![], ""),
        ]);
        assert_eq!(last_day(&stale, now), "Nothing changed.");
    }

    const LOG: &str = concat!(
        "[2026-10-16T23:02:32-0700] [ALPM] transaction started\n",
        "[2026-10-16T23:02:33-0700] [ALPM] upgraded a (1-1 -> 2-1)\n",
        "[2026-10-16T23:02:33-0700] [ALPM] upgraded b (1-1 -> 2-1)\n",
        "[2026-10-16T23:02:33-0700] [ALPM] installed c (1-1)\n",
        "[2026-10-16T23:02:34-0700] [ALPM] transaction completed\n",
        "[2026-10-16T23:05:00-0700] [ALPM] transaction started\n",
        "[2026-10-16T23:05:01-0700] [ALPM] transaction completed\n",
    );

    // Tests that touch the environment run one at a time.

    #[test]
    fn test_a_transaction_is_one_event_with_a_plain_summary() {
        let evs = package_events(0.0, Some(LOG));
        assert_eq!(evs.len(), 1);
        let e = &evs[0];
        assert_eq!(e.title, "Updated 2 packages · installed c");
        assert_eq!(e.items.len(), 3);
    }

    #[test]
    fn test_snapshots_are_read_from_the_journal() {
        let rec = serde_json::json!({
            "MESSAGE": "tim : TTY=pts/4 ; COMMAND=/usr/bin/snapper -c root create -c number -d 4.0.4-1",
            "__REALTIME_TIMESTAMP": "1792216942000000"});
        let evs = snapshot_events(0.0, Some(&format!("{rec}\n"))).unwrap();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].kind, SNAPSHOT);
        assert!(evs[0].detail.contains("4.0.4-1"));
    }

    #[test]
    fn test_almanacs_own_actions_round_trip() {
        let _g = crate::util::TEST_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let d = std::env::temp_dir().join(format!("almanac-history-test-{}", std::process::id()));
        let old = std::env::var_os("XDG_STATE_HOME");
        std::env::set_var("XDG_STATE_HOME", &d);
        record(ALMANAC, "yaru did not change", "", "warn", &[]);
        let evs = recorded(0.0).unwrap();
        match old {
            Some(v) => std::env::set_var("XDG_STATE_HOME", v),
            None => std::env::remove_var("XDG_STATE_HOME"),
        }
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(evs.len(), 1);
        assert_eq!((evs[0].title.as_str(), evs[0].severity.as_str()), ("yaru did not change", "warn"));
    }

    #[test]
    fn test_days_with_a_snapshot_are_marked() {
        // Python mocks the four readers; `group` is what timeline does with them.
        let snap = Event::new(1792216942.0, SNAPSHOT, "Snapshot taken", "", vec![], "");
        let pkg = Event::new(1792216952.0, PACKAGES, "Updated a", "", vec![], "");
        let days = group(vec![pkg.clone(), snap]).unwrap();
        assert_eq!(days.len(), 1);
        assert!(days[0].snapshot);
        assert_eq!(days[0].events[0], pkg); // newest first
    }

    #[test]
    fn test_history_is_synthetic() {
        // Demo mode must never read the real log: timeline returns demo::history().
        let _g = crate::util::TEST_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let old = std::env::var_os("ALMANAC_DEMO");
        std::env::set_var("ALMANAC_DEMO", "1");
        let r = std::panic::catch_unwind(|| timeline(DAYS));
        match old {
            Some(v) => std::env::set_var("ALMANAC_DEMO", v),
            None => std::env::remove_var("ALMANAC_DEMO"),
        }
        assert!(!r.expect("timeline panicked (demo::history still a stub?)").is_empty());
    }

    #[test]
    fn test_keyring_reinstalls_are_not_history() {
        let log = concat!(
            "[2026-10-16T23:02:32-0700] [ALPM] transaction started\n",
            "[2026-10-16T23:02:33-0700] [ALPM] reinstalled archlinux-keyring (20261001-1)\n",
            "[2026-10-16T23:02:34-0700] [ALPM] transaction completed\n",
        );
        assert_eq!(package_events(0.0, Some(log)), Vec::<Event>::new());
    }

    #[test]
    fn loads_follows_python() {
        let v = |t: &str| py_loads(t).ok();
        assert_eq!(v("Infinity"), Some(PyVal::Float(f64::INFINITY)));
        assert_eq!(v("1e400"), Some(PyVal::Float(f64::INFINITY)));
        assert_eq!(v("-0"), Some(PyVal::Int(BigInt::from(0))));
        assert_eq!(v(r#""\ud800x""#), Some(PyVal::Str(vec![0xd800, 0x78])));
        assert_eq!(v(r#""\ud83d\ude00""#), Some(PyVal::Str(vec![0x1f600])));
        assert_eq!(v(r#"{"a": 1, "a": 2, "b": [true, null]}"#).and_then(|d| d.get("a").cloned()),
                   Some(PyVal::Int(BigInt::from(2))));
        for bad in ["", "[1,]", "{\"a\":1,}", "01", "1.", "\u{feff}1", "\"\t\"", "nul", "[1] x"] {
            assert!(v(bad).is_none(), "{bad:?}");
        }
        let deep = "[".repeat(MAX_DEPTH + 1);
        assert!(matches!(py_loads(&deep), Err(LoadErr::Recursion("array"))));
    }

    #[test]
    fn int_follows_python() {
        let i = |t: &str| py_int_str(&py_str(t)).ok().map(|b| b.to_string());
        assert_eq!(i(" \u{ff11}_2 "), Some("12".into()));
        assert_eq!(i("\u{85}5"), Some("5".into()));
        assert_eq!(i("-007"), Some("-7".into()));
        for bad in ["\x1c5", "- 5", "5_", "_1", "1__2", "1.5", "12\u{b2}", ""] {
            assert_eq!(i(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn snapper_word_boundaries_follow_python() {
        let m = |t: &str| snapper_create(&py_str(t));
        assert!(m("COMMAND=/usr/bin/snapper create"));
        assert!(m("COMMAND=/usr/bin/snapper\u{301} create"));
        assert!(m("COMMAND=/usr/bin/snapper\u{203f} create"));
        assert!(!m("COMMAND=/usr/bin/snapper\u{b2} create"));
        assert!(!m("COMMAND=/usr/bin/snapperx create"));
        assert!(!m("COMMAND=/usr/bin/ls ; snapper create"));
        assert!(!m("COMMAND=snapper list\ncreate"));
        assert_eq!(desc_group(&py_str("x -d a b -c")).map(lossy), Some("a b".into()));
        assert_eq!(desc_group(&py_str("x -d line\n")).map(lossy), Some("line".into()));
    }

    #[test]
    fn fromtimestamp_rounds_to_the_microsecond() {
        // 0.4 microseconds before a midnight lands on the next day, as in Python.
        let midnight = Local.with_ymd_and_hms(2026, 10, 17, 0, 0, 0).earliest().unwrap().timestamp() as f64;
        assert_eq!(local_date(midnight - 4e-7).unwrap(), NaiveDate::from_ymd_opt(2026, 10, 17).unwrap());
        assert_eq!(local_date(midnight - 6e-7).unwrap(), NaiveDate::from_ymd_opt(2026, 10, 16).unwrap());
        assert!(local_date(1e39).is_err());
        assert!(local_date(f64::NAN).is_err());
    }

    #[test]
    fn errors_read_as_pythons() {
        let d = |b: &[u8]| decode_strict(b.to_vec()).unwrap_err();
        assert_eq!(d(b"ab\xc3x"), "UnicodeDecodeError: 'utf-8' codec can't decode byte 0xc3 in position 2: invalid continuation byte");
        assert_eq!(d(b"\xe2\x82"), "UnicodeDecodeError: 'utf-8' codec can't decode bytes in position 0-1: unexpected end of data");
        assert_eq!(d(b"\xff"), "UnicodeDecodeError: 'utf-8' codec can't decode byte 0xff in position 0: invalid start byte");
        assert_eq!(py_int_str(&py_str("1.5")).unwrap_err(), "ValueError: invalid literal for int() with base 10: '1.5'");
        assert_eq!(py_int_str(&[0x61, 0x27, 0xdcff]).unwrap_err(), "ValueError: invalid literal for int() with base 10: \"a'\\udcff\"");
        assert_eq!(local_date(f64::INFINITY).unwrap_err(), "OverflowError: timestamp out of range for platform time_t");
        assert_eq!(local_date_in(1e12, true).unwrap_err(), "ValueError: year must be in 1..9999, not 33658");
        assert_eq!(local_date_in(1e17, true).unwrap_err(), "OSError: [Errno 75] Value too large for defined data type");
        assert_eq!(local_date_in(1792465200.0, true).unwrap(), NaiveDate::from_ymd_opt(2026, 10, 20).unwrap());
        // CPython checks its stack entering every container, empty ones too.
        let n = MAX_DEPTH + 1;
        let empty = "[".repeat(n) + &"]".repeat(n);
        assert!(matches!(py_loads(&empty), Err(LoadErr::Recursion("array"))));
        let ok = "[".repeat(n - 1) + &"]".repeat(n - 1);
        assert!(py_loads(&ok).is_ok());
    }

    #[test]
    fn splitlines_matches_python() {
        assert_eq!(splitlines("a\nb\r\nc\rd\x1ce\u{2028}f\n"), vec!["a", "b", "c", "d", "e", "f"]);
        assert_eq!(splitlines(""), Vec::<&str>::new());
        assert_eq!(splitlines("\n\n"), vec!["", ""]);
    }

    #[test]
    fn a_line_as_deep_as_python_reads_is_cloned_compared_and_dropped_on_a_plain_thread() {
        // An item nested as deep as json.loads goes, on a default 2 MiB
        // thread: a derived Clone or PartialEq recursed once per level.
        let n = MAX_DEPTH - 2;
        let text = format!("{{\"ts\": 1, \"items\": {}{}}}", "[".repeat(n), "]".repeat(n));
        std::thread::Builder::new()
            .stack_size(2 << 20)
            .spawn(move || {
                let Ok(v) = py_loads(&text) else { panic!("Python reads it") };
                let w = v.clone();
                assert!(v == w);
                crate::util::tree::drop_value(v.to_value(true));
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
