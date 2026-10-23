//! Port of `security.py`.
//!
//! Known vulnerabilities in what is installed, from the Arch Security Tracker.
//!
//! Until this, a "security update" meant an update to a package whose *name*
//! suggested exposure -- a browser, OpenSSL, sudo. That flags a routine curl
//! release as urgent and misses a vulnerable library nobody thinks of as
//! network-facing. The Arch Security Tracker publishes every advisory group
//! (AVG) with the packages, the affected and fixed versions, the severity and
//! the CVEs, as one public JSON document. Matching it against the installed
//! versions answers the real question: *is what I am running vulnerable, and
//! does an update fix it?*
//!
//! * Fetched from security.archlinux.org, cached for six hours. Nothing about
//!   this machine is sent; the whole list is downloaded and matched locally.
//! * Versions are compared the way pacman's own `vercmp` compares them (a
//!   port of libalpm's `alpm_pkg_vercmp`, checked against the binary in the
//!   tests), the only comparison that agrees with pacman about epochs and
//!   pkgrels.
//! * When the tracker cannot be reached, callers get None -- "unknown", which
//!   the inventory treats differently from "no vulnerabilities".


use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const URL: &str = "https://security.archlinux.org/issues/all.json";
/// The host of `URL`, which urllib checks against `no_proxy`.
const URL_HOST: &str = "security.archlinux.org";
pub const TTL: i64 = 6 * 3600;
/// `{"Critical": 0, "High": 1, "Medium": 2, "Low": 3, "Unknown": 4}`
pub const SEVERITY_RANK: &[(&str, i64)] = &[
    ("Critical", 0),
    ("High", 1),
    ("Medium", 2),
    ("Low", 3),
    ("Unknown", 4),
];

/// One advisory against one installed package.
///
/// `group`, `severity`, `kind` and `cves` hold whatever the tracker's JSON
/// had there (a null stays null, a number stays a number), as the Python's
/// untyped dataclass fields do; the tracker itself always sends strings.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Vuln {
    pub package: String,
    pub installed: String,
    /// AVG-1234
    pub group: PyValue,
    /// Critical | High | Medium | Low | Unknown
    pub severity: PyValue,
    /// "arbitrary code execution", …
    pub kind: PyValue,
    /// '' when no fixed version exists yet
    pub fixed: String,
    pub cves: Vec<PyValue>,
}

impl Vuln {
    pub fn url(&self) -> String {
        format!("https://security.archlinux.org/{}", _py_str(&self.group))
    }

    /// `SEVERITY_RANK.get(self.severity, 4)`; 4 for a severity that cannot
    /// be a dict key (see `_rank` for the checked form).
    pub fn rank(&self) -> i64 {
        self._rank().unwrap_or(4)
    }

    /// `rank`, raising like the Python for a list or object severity.
    fn _rank(&self) -> Result<i64, String> {
        match &self.severity {
            J::Str(s) => Ok(SEVERITY_RANK.iter().find(|(k, _)| *k == s.as_str()).map(|(_, r)| *r).unwrap_or(4)),
            v @ (J::Arr(_) | J::Obj(_)) => Err(_unhashable(v)),
            _ => Ok(4),
        }
    }
}

/// `by_package()`: a dict in insertion order (first appearance in `vulns`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ByPackage(pub Vec<(String, Vec<Vuln>)>);

impl ByPackage {
    pub fn get(&self, package: &str) -> Option<&Vec<Vuln>> {
        self.0.iter().find(|(p, _)| p == package).map(|(_, vs)| vs)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &Vec<Vuln>)> {
        self.0.iter().map(|(p, vs)| (p, vs))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Serialize for ByPackage {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(Some(self.0.len()))?;
        for (k, v) in &self.0 {
            m.serialize_entry(k, v)?;
        }
        m.end()
    }
}

fn _cache_path() -> PathBuf {
    crate::xdg::cache_home().join("almanac").join("security.json")
}

/// Whether the tracker check is on (`settings.get("net_security_tracker")`).
pub fn enabled() -> bool {
    crate::settings::get_bool("net_security_tracker")
}

// ---- Python's view of JSON values ------------------------------------------

/// A JSON value as Python's `json.loads` gives it. It also takes `NaN`,
/// `Infinity` and `-Infinity`, turns `1e400` into inf, and keeps integers of
/// any size, none of which serde_json can hold; the tracker data stays in
/// this form throughout, so a value tests, compares and prints the way the
/// Python's does.
///
/// A lone surrogate (`"\ud800"`), which a Python str keeps and a Rust
/// string cannot, makes the str a `WStr` of code points. (In an object key
/// it still becomes U+FFFD.)
#[derive(Debug, Clone, Default, PartialEq)]
pub enum PyValue {
    #[default]
    Null,
    Bool(bool),
    /// The integer's digits, as `str(int)` writes them.
    Int(String),
    Float(f64),
    Str(String),
    /// A str holding a lone surrogate: its code points. Only ever made for a
    /// string that has one (see `_from_cps`), so it never equals a `Str`.
    WStr(Vec<u32>),
    Arr(Vec<PyValue>),
    /// In order, a repeated key included; `items` is the dict Python builds.
    Obj(Vec<(String, PyValue)>),
}

use self::PyValue as J;

impl PyValue {
    /// `d.get(key)` / `d[key]`: of a repeated key, the last one wins.
    pub fn get(&self, key: &str) -> Option<&PyValue> {
        match self {
            J::Obj(items) => items.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The dict's items: each key where it first appeared, with its last value.
    fn items(&self) -> Vec<(&str, &PyValue)> {
        let mut out: Vec<(&str, &PyValue)> = Vec::new();
        if let J::Obj(items) = self {
            for (k, v) in items {
                match out.iter_mut().find(|(o, _)| *o == k.as_str()) {
                    Some(slot) => slot.1 = v,
                    None => out.push((k, v)),
                }
            }
        }
        out
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            J::Str(s) => Some(s),
            _ => None,
        }
    }

    /// `type(v).__name__`.
    fn type_name(&self) -> &'static str {
        match self {
            J::Null => "NoneType",
            J::Bool(_) => "bool",
            J::Int(_) => "int",
            J::Float(_) => "float",
            J::Str(_) | J::WStr(_) => "str",
            J::Arr(_) => "list",
            J::Obj(_) => "dict",
        }
    }

    /// `bool(v)` (NaN is true).
    fn truthy(&self) -> bool {
        match self {
            J::Null => false,
            J::Bool(b) => *b,
            J::Int(t) => t != "0",
            J::Float(f) => *f != 0.0,
            J::Str(s) => !s.is_empty(),
            J::WStr(s) => !s.is_empty(),
            J::Arr(a) => !a.is_empty(),
            J::Obj(o) => !o.is_empty(),
        }
    }

    fn is_number(&self) -> bool {
        matches!(self, J::Bool(_) | J::Int(_) | J::Float(_))
    }

    /// A str's code points, lone surrogates included.
    fn str_cps(&self) -> Option<Vec<u32>> {
        match self {
            J::Str(s) => Some(_cps(s)),
            J::WStr(c) => Some(c.clone()),
            _ => None,
        }
    }
}

/// A Rust string's code points.
fn _cps(s: &str) -> Vec<u32> {
    s.chars().map(u32::from).collect()
}

/// A str from code points: a `Str` unless one is a lone surrogate.
fn _from_cps(cps: Vec<u32>) -> J {
    match cps.iter().map(|&c| char::from_u32(c)).collect::<Option<String>>() {
        Some(s) => J::Str(s),
        None => J::WStr(cps),
    }
}

/// Code points as a Rust string, a lone surrogate as U+FFFD.
fn _lossy(cps: &[u32]) -> String {
    cps.iter().map(|&c| char::from_u32(c).unwrap_or('\u{fffd}')).collect()
}

impl From<&Value> for PyValue {
    fn from(v: &Value) -> PyValue {
        match v {
            Value::Null => J::Null,
            Value::Bool(b) => J::Bool(*b),
            Value::Number(n) if n.is_f64() => J::Float(n.as_f64().unwrap_or(f64::NAN)),
            Value::Number(n) => J::Int(n.to_string()),
            Value::String(s) => J::Str(s.clone()),
            Value::Array(a) => J::Arr(a.iter().map(PyValue::from).collect()),
            Value::Object(o) => J::Obj(o.iter().map(|(k, v)| (k.clone(), PyValue::from(v))).collect()),
        }
    }
}

/// As JSON: what serde_json can hold of it. inf and NaN become null, an
/// integer past 64 bits the nearest float, a lone surrogate U+FFFD.
impl Serialize for PyValue {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{SerializeMap, SerializeSeq};
        match self {
            J::Null => s.serialize_unit(),
            J::Bool(b) => s.serialize_bool(*b),
            J::Int(t) => {
                if let Ok(i) = t.parse::<i64>() {
                    s.serialize_i64(i)
                } else if let Ok(u) = t.parse::<u64>() {
                    s.serialize_u64(u)
                } else {
                    s.serialize_f64(t.parse::<f64>().unwrap_or(f64::NAN))
                }
            }
            J::Float(f) => s.serialize_f64(*f),
            J::Str(t) => s.serialize_str(t),
            J::WStr(t) => s.serialize_str(&_lossy(t)),
            J::Arr(a) => {
                let mut seq = s.serialize_seq(Some(a.len()))?;
                for x in a {
                    seq.serialize_element(x)?;
                }
                seq.end()
            }
            J::Obj(_) => {
                let items = self.items();
                let mut m = s.serialize_map(Some(items.len()))?;
                for (k, v) in items {
                    m.serialize_entry(k, v)?;
                }
                m.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for PyValue {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<PyValue, D::Error> {
        Value::deserialize(d).map(|v| PyValue::from(&v))
    }
}

fn _unhashable(v: &J) -> String {
    format!("TypeError: unhashable type: '{}'", v.type_name())
}

/// `list(v)`: a list's items, a string's characters, a dict's keys.
fn _py_iter(v: &J) -> Result<Vec<J>, String> {
    match v {
        J::Arr(a) => Ok(a.clone()),
        J::Str(s) => Ok(s.chars().map(|c| J::Str(c.to_string())).collect()),
        J::WStr(s) => Ok(s.iter().map(|&c| _from_cps(vec![c])).collect()),
        J::Obj(_) => Ok(v.items().into_iter().map(|(k, _)| J::Str(k.to_string())).collect()),
        other => Err(format!("TypeError: '{}' object is not iterable", other.type_name())),
    }
}

// Code points Python 3.14's `str.isprintable` (Unicode 16) rejects, as
// inclusive ranges, and the zero of every run of Unicode decimal digits
// (category Nd) that `int()` reads. Generated from CPython itself.
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

/// `str.isprintable()` for one character, by Python's own table.
fn _py_isprintable(c: char) -> bool {
    let cp = c as u32;
    PY_NOT_PRINTABLE
        .binary_search_by(|&(a, b)| {
            if b < cp {
                Ordering::Less
            } else if a > cp {
                Ordering::Greater
            } else {
                Ordering::Equal
            }
        })
        .is_err()
}

/// `str(f)` / `repr(f)` of a float.
fn _py_float_str(f: f64) -> String {
    if f.is_nan() {
        "nan".into()
    } else if f.is_infinite() {
        if f > 0.0 { "inf".into() } else { "-inf".into() }
    } else {
        crate::util::py_float_repr(f)
    }
}

/// `repr(s)` of a string, Python style.
fn _py_repr_str(s: &str) -> String {
    _py_repr_cps(&_cps(s))
}

/// `repr(s)` of a str given as code points (a lone surrogate, which is not
/// printable, as its `\udXXX` escape).
fn _py_repr_cps(cps: &[u32]) -> String {
    let has = |x: char| cps.contains(&u32::from(x));
    let q = if has('\'') && !has('"') { '"' } else { '\'' };
    let mut out = String::from(q);
    for &cp in cps {
        let Some(c) = char::from_u32(cp) else {
            out.push_str(&format!("\\u{cp:04x}"));
            continue;
        };
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == q => {
                out.push('\\');
                out.push(c);
            }
            c if _py_isprintable(c) => out.push(c),
            c if (c as u32) <= 0xff => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if (c as u32) <= 0xffff => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push_str(&format!("\\U{:08x}", c as u32)),
        }
    }
    out.push(q);
    out
}

/// `repr(v)` of a JSON value, Python style.
fn _py_repr(v: &J) -> String {
    match v {
        J::Str(s) => _py_repr_str(s),
        J::WStr(s) => _py_repr_cps(s),
        J::Arr(a) => format!("[{}]", a.iter().map(_py_repr).collect::<Vec<_>>().join(", ")),
        J::Obj(_) => format!(
            "{{{}}}",
            v.items()
                .into_iter()
                .map(|(k, x)| format!("{}: {}", _py_repr_str(k), _py_repr(x)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => _py_str(other),
    }
}

/// `str(v)` / `f"{v}"` of a JSON value (a lone surrogate as U+FFFD; see
/// `_py_str_cps` for the exact text).
fn _py_str(v: &J) -> String {
    match v {
        J::Null => "None".into(),
        J::Bool(true) => "True".into(),
        J::Bool(false) => "False".into(),
        J::Int(t) => t.clone(),
        J::Float(f) => _py_float_str(*f),
        J::Str(s) => s.clone(),
        J::WStr(s) => _lossy(s),
        other => _py_repr(other),
    }
}

/// `str(v)` as code points, lone surrogates kept.
fn _py_str_cps(v: &J) -> Vec<u32> {
    match v {
        J::WStr(s) => s.clone(),
        other => _cps(&_py_str(other)),
    }
}

/// `json.dumps(s)` of a str given as code points: every code point outside
/// printable ASCII as `\uXXXX` (a surrogate as itself, others past the BMP
/// as a pair), as ensure_ascii writes them.
fn _py_json_string_cps(cps: &[u32]) -> String {
    let mut out = String::from('"');
    for &cp in cps {
        match char::from_u32(cp) {
            Some(c) => {
                let one = crate::util::py_json_string(c.encode_utf8(&mut [0; 4]));
                out.push_str(&one[1..one.len() - 1]);
            }
            None => out.push_str(&format!("\\u{cp:04x}")),
        }
    }
    out.push('"');
    out
}

/// `json.dumps(v)`: Python's separators, `ensure_ascii`, and `NaN` /
/// `Infinity` / `-Infinity` for the floats JSON has no word for.
fn _py_dumps(v: &J, out: &mut String) {
    match v {
        J::Null => out.push_str("null"),
        J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        J::Int(t) => out.push_str(t),
        J::Float(f) => out.push_str(&crate::util::py_float_repr(*f)),
        J::Str(s) => out.push_str(&crate::util::py_json_string(s)),
        J::WStr(s) => out.push_str(&_py_json_string_cps(s)),
        J::Arr(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                _py_dumps(x, out);
            }
            out.push(']');
        }
        J::Obj(_) => {
            out.push('{');
            for (i, (k, x)) in v.items().into_iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&crate::util::py_json_string(k));
                out.push_str(": ");
                _py_dumps(x, out);
            }
            out.push('}');
        }
    }
}

/// `json.loads(text)`; None where it raises ValueError (JSONDecodeError).
fn _py_loads(text: &str) -> Option<J> {
    // Nesting far past Python's recursion limit is refused too.
    crate::util::pyjson::loads(text, crate::util::pyjson::Limit::values(1001)).ok()
}

impl crate::util::pyjson::Build for PyValue {
    type Key = String;
    fn null() -> Self {
        J::Null
    }
    fn bool(b: bool) -> Self {
        J::Bool(b)
    }
    fn num(n: crate::util::pyjson::Num<'_>) -> Self {
        use crate::util::pyjson::Num;
        match n {
            Num::Int(i) => J::Int(i.to_string()),
            Num::UInt(u) => J::Int(u.to_string()),
            Num::Big(digits) => J::Int(digits.to_string()),
            Num::Float(f) => J::Float(f),
        }
    }
    fn str(s: String) -> Self {
        J::Str(s)
    }
    fn wstr(units: Vec<u32>) -> Self {
        _from_cps(units)
    }
    fn key(s: String) -> String {
        s
    }
    /// A key keeps a lone surrogate only as U+FFFD.
    fn wkey(units: Vec<u32>) -> String {
        _lossy(&units)
    }
    fn list(items: Vec<Self>) -> Self {
        J::Arr(items)
    }
    /// Every pair, a repeated key included (see [`PyValue::Obj`]).
    fn dict(pairs: Vec<(String, Self)>) -> Self {
        J::Obj(pairs)
    }
}

// ---- Python's int(), ==, hash(), set() and sorted() over those values -------

/// `str.isspace()` for one character: Unicode White_Space plus the
/// separators U+001C..U+001F, which Python also counts.
fn _py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// `int(s)` (base 10), as (negative, ASCII digits without leading zeros);
/// None where it raises ValueError. Around the number, int() skips ASCII
/// whitespace and any non-ASCII space; it takes every Unicode decimal digit
/// and single underscores between digits.
fn _py_int(s: &str) -> Option<(bool, String)> {
    let s = s.trim_matches(|c: char| matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r') || (!c.is_ascii() && _py_isspace(c)));
    let (neg, body) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let mut digits = String::new();
    let mut after_digit = false;
    for c in body.chars() {
        if c == '_' {
            if !after_digit {
                return None;
            }
            after_digit = false;
            continue;
        }
        digits.push(char::from_digit(crate::util::decimal_value(c)?, 10)?);
        after_digit = true;
    }
    if !after_digit {
        return None;
    }
    let digits = digits.trim_start_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    Some((neg && digits != "0", digits.to_string()))
}

/// The sign of `int(out.strip())`, all the Python's comparisons looked at;
/// None where int() raises.
#[cfg(test)]
fn _py_int_sign(out: &str) -> Option<i8> {
    let (neg, digits) = _py_int(out.trim_matches(_py_isspace))?;
    Some(if digits == "0" { 0 } else if neg { -1 } else { 1 })
}

/// An integer as (negative, magnitude digits); a bool is 0 or 1.
fn _int_parts(v: &J) -> Option<(bool, &str)> {
    match v {
        J::Bool(b) => Some((false, if *b { "1" } else { "0" })),
        J::Int(t) => Some(match t.strip_prefix('-') {
            Some(m) => (true, m),
            None => (false, t.as_str()),
        }),
        _ => None,
    }
}

fn _cmp_ints(a: (bool, &str), b: (bool, &str)) -> Ordering {
    match (a.0, b.0) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (neg, _) => {
            let m = a.1.len().cmp(&b.1.len()).then_with(|| a.1.cmp(b.1));
            if neg { m.reverse() } else { m }
        }
    }
}

/// An int against a float, exactly, as Python compares them.
fn _cmp_int_float(i: (bool, &str), f: f64) -> Option<Ordering> {
    if f.is_nan() {
        return None;
    }
    if f.is_infinite() {
        return Some(if f > 0.0 { Ordering::Less } else { Ordering::Greater });
    }
    let fl = f.floor();
    let text = format!("{fl:.0}");
    let fl_parts = match text.strip_prefix('-') {
        Some(m) if m != "0" => (true, m),
        Some(m) => (false, m),
        None => (false, text.as_str()),
    };
    Some(match _cmp_ints(i, fl_parts) {
        Ordering::Equal if f > fl => Ordering::Less,
        o => o,
    })
}

/// Numeric comparison of two numbers (bool, int, float); None when unordered (NaN).
fn _num_cmp(a: &J, b: &J) -> Option<Ordering> {
    match (a, b) {
        (J::Float(x), J::Float(y)) => x.partial_cmp(y),
        (J::Float(x), _) => _cmp_int_float(_int_parts(b)?, *x).map(Ordering::reverse),
        (_, J::Float(y)) => _cmp_int_float(_int_parts(a)?, *y),
        _ => Some(_cmp_ints(_int_parts(a)?, _int_parts(b)?)),
    }
}

/// `a < b`; Err is Python's TypeError for types that have no order.
fn _py_lt(a: &J, b: &J) -> Result<bool, String> {
    match (a, b) {
        // Code point order, which is UTF-8 byte order.
        (J::Str(x), J::Str(y)) => Ok(x < y),
        (J::Str(_) | J::WStr(_), J::Str(_) | J::WStr(_)) => Ok(a.str_cps() < b.str_cps()),
        _ if a.is_number() && b.is_number() => Ok(_num_cmp(a, b) == Some(Ordering::Less)),
        _ => Err(format!(
            "TypeError: '<' not supported between instances of '{}' and '{}'",
            a.type_name(),
            b.type_name()
        )),
    }
}

/// Whether a set holding `a` takes `b` as already there: `a is b or a == b`.
/// Every NaN json.loads gives is the one same float object.
fn _py_set_eq(a: &J, b: &J) -> bool {
    match (a, b) {
        (J::Float(x), J::Float(y)) if x.is_nan() && y.is_nan() => true,
        (J::Null, J::Null) => true,
        (J::Str(x), J::Str(y)) => x == y,
        (J::WStr(x), J::WStr(y)) => x == y,
        _ if a.is_number() && b.is_number() => _num_cmp(a, b) == Some(Ordering::Equal),
        _ => false,
    }
}

/// `sys.hash_info.modulus`
const HASH_MODULUS: u64 = (1 << 61) - 1;

/// `math.frexp(v)` for a finite, nonzero v.
fn _frexp(v: f64) -> (f64, i32) {
    let bits = v.to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i32;
    if exp == 0 {
        // subnormal
        let (m, e) = _frexp(v * 2f64.powi(54));
        return (m, e - 54);
    }
    (f64::from_bits((bits & !(0x7ff << 52)) | (1022 << 52)), exp - 1022)
}

/// `hash(v)` (CPython, 64-bit). A string's hash is salted per process in
/// Python, so any fixed one is as good; NaN hashes by identity.
fn _py_hash(v: &J) -> i64 {
    let fix = |h: i64| if h == -1 { -2 } else { h };
    match v {
        J::Null => 0xFCA8_6420,
        J::Bool(b) => i64::from(*b),
        J::Int(_) => {
            let (neg, mag) = _int_parts(v).unwrap_or((false, "0"));
            let mut h: u128 = 0;
            for d in mag.bytes() {
                h = (h * 10 + u128::from(d - b'0')) % u128::from(HASH_MODULUS);
            }
            let h = h as i64;
            fix(if neg { -h } else { h })
        }
        J::Float(f) if f.is_infinite() => {
            if *f > 0.0 { 314_159 } else { -314_159 }
        }
        J::Float(f) if f.is_nan() || *f == 0.0 => 0,
        J::Float(f) => {
            let (mut m, mut e) = _frexp(*f);
            let mut sign = 1i64;
            if m < 0.0 {
                sign = -1;
                m = -m;
            }
            // process 28 bits at a time
            let mut x: u64 = 0;
            while m != 0.0 {
                x = ((x << 28) & HASH_MODULUS) | (x >> (61 - 28));
                m *= 268_435_456.0;
                e -= 28;
                let y = m as u64;
                m -= y as f64;
                x += y;
                if x >= HASH_MODULUS {
                    x -= HASH_MODULUS;
                }
            }
            // adjust for the exponent; first reduce it modulo 61
            let e = if e >= 0 { e % 61 } else { 61 - 1 - ((-1 - e) % 61) } as u32;
            x = ((x << e) & HASH_MODULUS) | (x >> (61 - e));
            fix(x as i64 * sign)
        }
        J::Str(s) => {
            // FNV-1a
            let mut h: u64 = 0xcbf2_9ce4_8422_2325;
            for b in s.bytes() {
                h = (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
            }
            fix(h as i64)
        }
        J::WStr(s) => {
            // FNV-1a over the code points (never equal to a `Str`).
            let mut h: u64 = 0xcbf2_9ce4_8422_2325;
            for c in s {
                for b in c.to_le_bytes() {
                    h = (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
                }
            }
            fix(h as i64)
        }
        J::Arr(_) | J::Obj(_) => 0,
    }
}

/// `{c for c in items}` iterated: CPython's set table, slot by slot, so a
/// mix of types reaches sorted() in the order Python's does. (Hashable
/// items only; the caller has checked.)
fn _py_set<'a>(items: &[&'a J]) -> Vec<&'a J> {
    const LINEAR_PROBES: usize = 9;
    type Table<'a> = Vec<Option<(i64, &'a J)>>;
    fn insert_clean<'a>(table: &mut Table<'a>, mask: usize, h: i64, key: &'a J) {
        let mut perturb = h as u64;
        let mut i = (h as u64 as usize) & mask;
        loop {
            if table[i].is_none() {
                table[i] = Some((h, key));
                return;
            }
            if i + LINEAR_PROBES <= mask {
                for j in i + 1..=i + LINEAR_PROBES {
                    if table[j].is_none() {
                        table[j] = Some((h, key));
                        return;
                    }
                }
            }
            perturb >>= 5;
            i = i.wrapping_mul(5).wrapping_add(1).wrapping_add(perturb as usize) & mask;
        }
    }
    let mut mask = 7usize;
    let mut table: Table<'a> = vec![None; 8];
    let (mut fill, mut used) = (0usize, 0usize);
    for &key in items {
        let h = _py_hash(key);
        let mut perturb = h as u64;
        let mut i = (h as u64 as usize) & mask;
        let slot = 'probe: loop {
            let probes = if i + LINEAR_PROBES <= mask { LINEAR_PROBES } else { 0 };
            for j in i..=i + probes {
                match table[j] {
                    None => break 'probe Some(j),
                    Some((eh, ek)) if eh == h && _py_set_eq(ek, key) => break 'probe None,
                    Some(_) => {}
                }
            }
            perturb >>= 5;
            i = i.wrapping_mul(5).wrapping_add(1).wrapping_add(perturb as usize) & mask;
        };
        let Some(j) = slot else { continue };
        table[j] = Some((h, key));
        fill += 1;
        used += 1;
        if fill * 5 >= mask * 3 {
            let mut size = 8;
            while size <= used * 4 {
                size <<= 1;
            }
            let old = std::mem::replace(&mut table, vec![None; size]);
            mask = size - 1;
            for (h, k) in old.into_iter().flatten() {
                insert_clean(&mut table, mask, h, k);
            }
            fill = used;
        }
    }
    table.into_iter().flatten().map(|(_, k)| k).collect()
}

/// CPython's `count_run`: the length of the run at the start of `a`, a
/// descending one reversed in place.
fn _count_run(a: &mut [&J]) -> Result<usize, String> {
    fn reverse_last_neq(a: &mut [&J], n: usize, neq: &mut usize) {
        if *neq > 0 {
            *neq += 1;
            a[n - *neq..n].reverse();
            *neq = 0;
        }
    }
    let len = a.len();
    let mut n = 1;
    while n < len {
        if _py_lt(a[n], a[n - 1])? {
            break;
        }
        n += 1;
    }
    if n == len {
        return Ok(n);
    }
    if n > 1 {
        if _py_lt(a[0], a[n - 1])? {
            return Ok(n);
        }
        a[..n].reverse();
    }
    n += 1;
    let mut neq = 0;
    while n < len {
        if _py_lt(a[n], a[n - 1])? {
            reverse_last_neq(a, n, &mut neq);
        } else if _py_lt(a[n - 1], a[n])? {
            break;
        } else {
            neq += 1;
        }
        n += 1;
    }
    reverse_last_neq(a, n, &mut neq);
    a[..n].reverse();
    while n < len {
        if _py_lt(a[n], a[n - 1])? {
            break;
        }
        n += 1;
    }
    Ok(n)
}

/// CPython's `binarysort`: `a[..ok]` is sorted; insert the rest.
fn _binarysort(a: &mut [&J], ok: usize) -> Result<(), String> {
    for ok in ok.max(1)..a.len() {
        let pivot = a[ok];
        let (mut l, mut r) = (0, ok);
        while l < r {
            let m = (l + r) >> 1;
            if _py_lt(pivot, a[m])? {
                r = m;
            } else {
                l = m + 1;
            }
        }
        a[l..=ok].rotate_right(1);
    }
    Ok(())
}

/// `sorted(items)`, making the comparisons CPython's sort makes, so the
/// first pair that cannot be compared is the one the TypeError names.
/// Exact up to 64 items (one run); past that the runs are sorted the same
/// way, and a mix that cannot be merged reports the first two runs' heads.
fn _py_sorted<'a>(items: &[&'a J]) -> Result<Vec<&'a J>, String> {
    let mut a = items.to_vec();
    if a.len() < 2 {
        return Ok(a);
    }
    // merge_compute_minrun
    let (mut n, mut r) = (a.len(), 0);
    while n >= 64 {
        r |= n & 1;
        n >>= 1;
    }
    let minrun = n + r;
    let mut runs = Vec::new();
    let mut lo = 0;
    while lo < a.len() {
        let rest = &mut a[lo..];
        let mut n = _count_run(rest)?;
        if n < minrun {
            let force = rest.len().min(minrun);
            _binarysort(&mut rest[..force], n)?;
            n = force;
        }
        runs.push(lo);
        lo += n;
    }
    if runs.len() > 1 {
        // Merging compares the runs' items across types; the first merge
        // starts at the second run's head against the first run's.
        let mut clash = None;
        'pairs: for (i, x) in a.iter().enumerate() {
            for y in &a[i + 1..] {
                if let Err(e) = _py_lt(x, y) {
                    clash = Some(e);
                    break 'pairs;
                }
            }
        }
        if let Some(e) = clash {
            return Err(_py_lt(a[runs[1]], a[0]).err().unwrap_or(e));
        }
        a.sort_by(|x, y| match (_py_lt(x, y), _py_lt(y, x)) {
            (Ok(true), _) => Ordering::Less,
            (_, Ok(true)) => Ordering::Greater,
            _ => Ordering::Equal,
        });
    }
    Ok(a)
}

// ---- the tracker -------------------------------------------------------------

/// `urllib.request.getproxies_environment()`: every `<scheme>_proxy`
/// variable in any case, a lowercase one winning; empty unsets.
fn _getproxies() -> HashMap<String, String> {
    let env: Vec<(String, String)> = std::env::vars_os()
        .map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned()))
        .collect();
    let mut proxies = HashMap::new();
    let mut environment = Vec::new();
    for (name, value) in &env {
        let chars: Vec<char> = name.chars().collect();
        let n = chars.len();
        if n > 5 && chars[n - 6] == '_' && chars[n - 5..].iter().collect::<String>().to_lowercase() == "proxy" {
            let proxy_name = chars[..n - 6].iter().collect::<String>().to_lowercase();
            if !value.is_empty() {
                proxies.insert(proxy_name.clone(), value.clone());
            }
            environment.push((name, value, proxy_name));
        }
    }
    // CVE-2016-1000110: running as a CGI script, forget HTTP_PROXY.
    if env.iter().any(|(k, _)| k == "REQUEST_METHOD") {
        proxies.remove("http");
    }
    for (name, value, proxy_name) in environment {
        if name.ends_with("_proxy") {
            if value.is_empty() {
                proxies.remove(&proxy_name);
            } else {
                proxies.insert(proxy_name, value.clone());
            }
        }
    }
    proxies
}

/// `urllib.request.proxy_bypass_environment(host)`: `no_proxy` is a list of
/// domain suffixes, or `*` for every host.
fn _proxy_bypass(host: &str, proxies: &HashMap<String, String>) -> bool {
    let Some(no_proxy) = proxies.get("no") else { return false };
    if no_proxy == "*" {
        return true;
    }
    let host = host.to_lowercase();
    for name in no_proxy.split(',') {
        let name = name.trim_matches(_py_isspace);
        if !name.is_empty() {
            let name = name.trim_start_matches('.').to_lowercase();
            if host == name || host.ends_with(&format!(".{name}")) {
                return true;
            }
        }
    }
    false
}

/// `urllib.parse.unquote(s)`: %XX escapes, decoded as UTF-8 with replacement.
fn _unquote(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() && b[i + 1].is_ascii_hexdigit() && b[i + 2].is_ascii_hexdigit() {
            out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap_or(0));
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `urllib.request._parse_proxy(proxy)`: (user, password, host:port); Err
/// for the ValueError of a URL with no authority.
fn _parse_proxy(proxy: &str) -> Result<(Option<&str>, Option<&str>, &str), ()> {
    // _splittype: re.match('([^/:]+):(.*)', proxy)
    let r_scheme = match proxy.find(['/', ':']) {
        Some(i) if i > 0 && proxy[i..].starts_with(':') => &proxy[i + 1..],
        _ => proxy,
    };
    let authority = if !r_scheme.starts_with('/') {
        proxy
    } else {
        if !r_scheme.starts_with("//") {
            return Err(());
        }
        let end = match r_scheme.find('@') {
            Some(sep) => r_scheme[sep..].find('/').map(|e| e + sep),
            None => r_scheme[2..].find('/').map(|e| e + 2),
        };
        &r_scheme[2..end.unwrap_or(r_scheme.len())]
    };
    let (userinfo, hostport) = match authority.rfind('@') {
        Some(i) => (Some(&authority[..i]), &authority[i + 1..]),
        None => (None, authority),
    };
    Ok(match userinfo {
        Some(u) => match u.split_once(':') {
            Some((user, password)) => (Some(user), Some(password), hostport),
            None => (Some(u), None, hostport),
        },
        None => (None, None, hostport),
    })
}

/// `http.client.HTTPSConnection._get_hostport(hostport)`: the port after
/// the last colon (outside an IPv6 literal), 443 when there is none; Err
/// where Python fails (a port int() refuses, or out of range for a socket).
fn _get_hostport(hostport: &str) -> Result<(String, u16), ()> {
    let i = hostport.rfind(':');
    let (host, port) = match i {
        Some(i) if hostport.rfind(']').is_none_or(|j| i > j) => {
            let p = &hostport[i + 1..];
            let port = if p.is_empty() {
                443
            } else {
                match _py_int(p).ok_or(())? {
                    (false, d) => d.parse::<u16>().map_err(|_| ())?,
                    (true, _) => return Err(()),
                }
            };
            (&hostport[..i], port)
        }
        _ => (hostport, 443),
    };
    let host = match host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        Some(h) => h,
        None => host,
    };
    Ok((host.to_string(), port))
}

/// The proxy urllib's `urlopen` takes for `URL`: only `https_proxy` (any
/// case) counts for an https URL, `no_proxy` matches by domain suffix, and
/// the tracker is always reached through a CONNECT tunnel to the proxy's
/// host and port, whatever scheme the proxy URL names. Ok(None) is a direct
/// connection; Err is where urllib fails before connecting.
///
/// The credentials go to ureq inside the proxy URI, so ones holding
/// characters a URI cannot carry (after urllib unquotes them) fail here.
fn _tracker_proxy() -> Result<Option<ureq::Proxy>, ()> {
    let Some((creds, host, port)) = _tracker_proxy_parts()? else { return Ok(None) };
    let creds = if creds.is_empty() { creds } else { format!("{creds}@") };
    let host = if host.contains(':') { format!("[{host}]") } else { host };
    ureq::Proxy::new(&format!("http://{creds}{host}:{port}")).map(Some).map_err(|_| ())
}

/// `_tracker_proxy` before ureq: ("user:password" or "", host, port).
fn _tracker_proxy_parts() -> Result<Option<(String, String, u16)>, ()> {
    let proxies = _getproxies();
    let Some(proxy) = proxies.get("https") else { return Ok(None) };
    let (user, password, hostport) = _parse_proxy(proxy)?;
    if _proxy_bypass(URL_HOST, &proxies) {
        return Ok(None);
    }
    // `if user and password:` Proxy-authorization, Basic, unquoted.
    let creds = match (user, password) {
        (Some(u), Some(p)) if !u.is_empty() && !p.is_empty() => format!("{}:{}", _unquote(u), _unquote(p)),
        _ => String::new(),
    };
    let (host, port) = _get_hostport(&_unquote(hostport))?;
    if host.is_empty() {
        return Err(());
    }
    Ok(Some((creds, host, port)))
}

/// GET the tracker document. `urlopen(req, timeout=30)` limits each blocking
/// socket operation, not the whole download, so the body is read with a 30 s
/// limit per read: a slow download that keeps streaming still succeeds.
fn _fetch() -> Option<Vec<u8>> {
    use std::io::Read;
    let proxy = _tracker_proxy().ok()?;
    let t = Some(std::time::Duration::from_secs(30));
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .proxy(proxy)
        .timeout_connect(t)
        .timeout_send_request(t)
        .timeout_recv_response(t)
        .build()
        .into();
    let r = agent.get(URL).header("User-Agent", "almanac").call().ok()?;
    let mut reader = r.into_body().into_with_config().limit(u64::MAX).reader();
    let (tx, rx) = std::sync::mpsc::channel::<Option<Vec<u8>>>();
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    let _ = tx.send(Some(Vec::new()));
                    break;
                }
                Ok(n) => {
                    if tx.send(Some(buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => {
                    let _ = tx.send(None);
                    break;
                }
            }
        }
    });
    let mut body = Vec::new();
    loop {
        match rx.recv_timeout(std::time::Duration::from_secs(30)) {
            Ok(Some(chunk)) if chunk.is_empty() => return Some(body),
            Ok(Some(chunk)) => body.extend_from_slice(&chunk),
            // A read failed, or one read took longer than 30 s.
            _ => return None,
        }
    }
}

/// The tracker's advisory groups, or None when it cannot be reached or
/// has been turned off in Preferences.
///
/// Groups are kept as the tracker's raw JSON, so the cache file is
/// byte-compatible with the Python's. Err where the Python raises: a cache
/// whose "at" is not a number (TypeError) or an integer too large for a
/// float (OverflowError).
pub fn advisories(ttl: i64) -> Result<Option<PyValue>, String> {
    if !enabled() {
        return Ok(None);
    }
    let path = _cache_path();
    let mut cached: Option<J> = None;
    // `except (OSError, ValueError, AttributeError)`: a missing, unreadable,
    // broken or non-object cache file is no cache.
    if let Some(blob @ J::Obj(_)) = std::fs::read_to_string(&path).ok().as_deref().and_then(_py_loads) {
        // `blob.get("groups")`, whatever it holds; null is None.
        cached = blob.get("groups").filter(|v| !matches!(v, J::Null)).cloned();
        // `time.time() - blob.get("at", 0)`
        let at = match blob.get("at") {
            None => 0.0,
            Some(J::Bool(b)) => f64::from(u8::from(*b)),
            Some(J::Int(t)) => match t.parse::<f64>() {
                Ok(f) if f.is_finite() => f,
                _ => return Err("OverflowError: int too large to convert to float".into()),
            },
            Some(J::Float(f)) => *f,
            Some(other) => {
                return Err(format!(
                    "TypeError: unsupported operand type(s) for -: 'float' and '{}'",
                    other.type_name()
                ))
            }
        };
        if crate::util::now() - at < ttl as f64 {
            return Ok(cached);
        }
    }
    let groups = match _fetch().and_then(|b| String::from_utf8(b).ok()).as_deref().and_then(_py_loads) {
        Some(g) => g,
        // stale beats nothing; None if neither
        None => return Ok(cached),
    };
    // json.dumps({"at": time.time(), "groups": groups}), written atomically.
    let mut text = format!("{{\"at\": {}, \"groups\": ", crate::util::py_float_repr(crate::util::now()));
    _py_dumps(&groups, &mut text);
    text.push('}');
    let write = || -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &text)?;
        std::fs::rename(&tmp, &path)
    };
    let _ = write();
    Ok(Some(groups).filter(|v| !matches!(v, J::Null)))
}

/// `{(a, b): sign}`, as `vercmp a b` prints it.
///
/// This used to start one `vercmp` process per pair (over a thousand on
/// every inventory scan); `vercmp` is computed in-process now. None where
/// the process could not have answered: a NUL byte cannot go into its argv,
/// and `vercmp -h x` / `vercmp --help x` print the usage instead of a number.
fn _vercmp_all(pairs: &[(String, String)]) -> HashMap<(String, String), Option<i8>> {
    let mut out = HashMap::new();
    for (a, b) in pairs {
        if out.contains_key(&(a.clone(), b.clone())) {
            continue;
        }
        let sign = if a.contains('\0') || b.contains('\0') || a == "-h" || a == "--help" {
            None
        } else {
            Some(vercmp(a, b) as i8)
        };
        out.insert((a.clone(), b.clone()), sign);
    }
    out
}

/// pacman's version comparison (libalpm `alpm_pkg_vercmp`): `[epoch:]version[-release]`,
/// the epoch first (missing means 0), then the version, then the release
/// only when both have one. Each part is compared with `rpmvercmp`.
pub fn vercmp(a: &str, b: &str) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }
    let (e1, v1, r1) = parse_evr(a.as_bytes());
    let (e2, v2, r2) = parse_evr(b.as_bytes());
    rpmvercmp(e1, e2).then_with(|| rpmvercmp(v1, v2)).then_with(|| match (r1, r2) {
        (Some(r1), Some(r2)) => rpmvercmp(r1, r2),
        _ => Ordering::Equal,
    })
}

/// libalpm's `parseEVR`: the epoch is the leading digits when a ':' follows
/// them ("0" when there are none, or no ':'); the release is what follows the
/// last '-' after those digits, None when there is no '-'.
fn parse_evr(evr: &[u8]) -> (&[u8], &[u8], Option<&[u8]>) {
    let digits = evr.iter().take_while(|c| c.is_ascii_digit()).count();
    let dash = evr[digits..].iter().rposition(|&c| c == b'-').map(|i| digits + i);
    let (epoch, version_start) = if evr.get(digits) == Some(&b':') {
        (if digits == 0 { &b"0"[..] } else { &evr[..digits] }, digits + 1)
    } else {
        (&b"0"[..], 0)
    };
    match dash {
        Some(d) => (epoch, &evr[version_start..d], Some(&evr[d + 1..])),
        None => (epoch, &evr[version_start..], None),
    }
}

/// libalpm's `rpmvercmp`, with its C-locale `isalnum`/`isdigit`/`isalpha`.
///
/// Both strings are walked as alternating runs of separators (anything not
/// ASCII alphanumeric) and segments (all digits or all letters). Separator
/// runs of different lengths decide; a number beats letters; numbers compare
/// by value (leading zeros dropped, then length, then digits); letters
/// compare bytewise. When one string runs out, a leftover letter segment is
/// older than nothing and anything else is newer.
fn rpmvercmp(a: &[u8], b: &[u8]) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }
    let (mut one, mut two) = (0usize, 0usize);
    let (mut ptr1, mut ptr2) = (0usize, 0usize);
    let at = |s: &[u8], i: usize| s.get(i).copied().unwrap_or(0);
    while at(a, one) != 0 && at(b, two) != 0 {
        while at(a, one) != 0 && !at(a, one).is_ascii_alphanumeric() {
            one += 1;
        }
        while at(b, two) != 0 && !at(b, two).is_ascii_alphanumeric() {
            two += 1;
        }
        if at(a, one) == 0 || at(b, two) == 0 {
            break;
        }
        // Separator runs of different lengths.
        if one - ptr1 != two - ptr2 {
            return (one - ptr1).cmp(&(two - ptr2));
        }
        ptr1 = one;
        ptr2 = two;
        let isnum = at(a, ptr1).is_ascii_digit();
        let class: fn(&u8) -> bool = if isnum { u8::is_ascii_digit } else { u8::is_ascii_alphabetic };
        while at(a, ptr1) != 0 && class(&at(a, ptr1)) {
            ptr1 += 1;
        }
        while at(b, ptr2) != 0 && class(&at(b, ptr2)) {
            ptr2 += 1;
        }
        // A number against letters (an empty segment of b's kind).
        if two == ptr2 {
            return if isnum { Ordering::Greater } else { Ordering::Less };
        }
        let (mut s1, mut s2) = (&a[one..ptr1], &b[two..ptr2]);
        if isnum {
            while s1.first() == Some(&b'0') {
                s1 = &s1[1..];
            }
            while s2.first() == Some(&b'0') {
                s2 = &s2[1..];
            }
            if s1.len() != s2.len() {
                return s1.len().cmp(&s2.len());
            }
        }
        match s1.cmp(s2) {
            Ordering::Equal => {}
            o => return o,
        }
        one = ptr1;
        two = ptr2;
    }
    let (c1, c2) = (at(a, one), at(b, two));
    if c1 == 0 && c2 == 0 {
        return Ordering::Equal;
    }
    // A leftover letter segment never beats an empty string.
    if (c1 == 0 && !c2.is_ascii_alphabetic()) || c1.is_ascii_alphabetic() {
        Ordering::Less
    } else {
        Ordering::Greater
    }
}

/// `str.splitlines()`: every line boundary Python recognises.
fn _splitlines(s: &str) -> Vec<&str> {
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

/// `l.split(None, 1)`.
fn _split1(l: &str) -> Vec<&str> {
    let l = l.trim_start_matches(_py_isspace);
    if l.is_empty() {
        return Vec::new();
    }
    let Some(cut) = l.find(_py_isspace) else {
        return vec![l];
    };
    let (name, rest) = l.split_at(cut);
    let rest = rest.trim_start_matches(_py_isspace);
    if rest.is_empty() {
        vec![name]
    } else {
        vec![name, rest]
    }
}

/// `subprocess.run(argv, capture_output=True, timeout=t).stdout`, as bytes
/// (util::run decodes lossily, and here the bytes decide): None where it
/// raises, the program missing (OSError) or too slow (TimeoutExpired).
fn _run_bytes(argv: &[&str], timeout_secs: u64) -> Option<Vec<u8>> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let (prog, args) = argv.split_first()?;
    let mut child = Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut pipe = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = pipe.read_to_end(&mut b);
        b
    });
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() < std::time::Duration::from_secs(timeout_secs) => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    reader.join().ok()
}

/// `str(UnicodeDecodeError)` of a strict UTF-8 decode of `b`, CPython's words.
fn _utf8_error(b: &[u8], e: &std::str::Utf8Error) -> String {
    let start = e.valid_up_to();
    let first = b[start];
    let (end, reason) = if !(0xc2..=0xf4).contains(&first) {
        (start + 1, "invalid start byte")
    } else {
        match e.error_len() {
            None => (b.len(), "unexpected end of data"),
            Some(n) => (start + n, "invalid continuation byte"),
        }
    };
    if end - start == 1 {
        format!("'utf-8' codec can't decode byte 0x{first:02x} in position {start}: {reason}")
    } else {
        format!("'utf-8' codec can't decode bytes in position {start}-{}: {reason}", end - 1)
    }
}

/// `pacman -Q` as {name: version}; empty when pacman cannot run. Err where
/// the Python raises: a line with a space that is not two fields (dict()'s
/// ValueError), or output that is not UTF-8.
pub fn installed_versions() -> Result<HashMap<String, String>, String> {
    let Some(raw) = _run_bytes(&["pacman", "-Q"], 30) else {
        return Ok(HashMap::new());
    };
    let out = match std::str::from_utf8(&raw) {
        Ok(s) => s,
        Err(e) => return Err(format!("UnicodeDecodeError: {}", _utf8_error(&raw, &e))),
    };
    let mut map = HashMap::new();
    for (n, l) in _splitlines(out).into_iter().filter(|l| l.contains(' ')).enumerate() {
        let parts = _split1(l);
        if parts.len() != 2 {
            return Err(format!(
                "ValueError: dictionary update sequence element #{n} has length {}; 2 is required",
                parts.len()
            ));
        }
        map.insert(parts[0].to_string(), parts[1].to_string());
    }
    Ok(map)
}

/// Every advisory affecting an installed version. None: tracker unknown.
///
/// The infallible form: where the Python raises (tracker data of the wrong
/// shape) this gives None. `try_vulnerabilities` keeps the error, and is the
/// one to call where the Python lets the exception through (inventory).
pub fn vulnerabilities(installed: Option<&HashMap<String, String>>, groups: Option<&PyValue>) -> Option<Vec<Vuln>> {
    try_vulnerabilities(installed, groups).ok().flatten()
}

/// `vulnerabilities()`, Err where the Python raises.
///
/// `installed` None runs `installed_versions()`; `groups` None calls
/// `advisories(TTL)`.
pub fn try_vulnerabilities(
    installed: Option<&HashMap<String, String>>,
    groups: Option<&PyValue>,
) -> Result<Option<Vec<Vuln>>, String> {
    let fetched;
    let groups: &J = match groups.filter(|g| !matches!(g, J::Null)) {
        Some(g) => g,
        None => match advisories(TTL)? {
            Some(g) => {
                fetched = g;
                &fetched
            }
            None => return Ok(None),
        },
    };
    let own;
    let installed = match installed {
        Some(i) => i,
        None => {
            own = installed_versions()?;
            &own
        }
    };

    let groups = _py_iter(groups)?;
    let mut candidates: Vec<(&J, String, String)> = Vec::new();
    let mut pairs: Vec<(String, J)> = Vec::new();
    for g in &groups {
        if !matches!(g, J::Obj(_)) {
            return Err(format!("AttributeError: '{}' object has no attribute 'get'", g.type_name()));
        }
        if g.get("status").and_then(J::as_str) == Some("Not affected") {
            continue;
        }
        let pkgs = match g.get("packages").filter(|v| v.truthy()) {
            Some(v) => _py_iter(v)?,
            None => Vec::new(),
        };
        for pkg in pkgs {
            // `installed.get(pkg)`: only a string can match a name.
            let have = match &pkg {
                J::Str(p) => installed.get(p).cloned(),
                J::Arr(_) | J::Obj(_) => return Err(_unhashable(&pkg)),
                _ => None,
            };
            let Some(have) = have.filter(|h| !h.is_empty()) else { continue };
            let J::Str(pkg) = pkg else { continue };
            candidates.push((g, pkg, have.clone()));
            if let Some(f) = g.get("fixed").filter(|v| v.truthy()) {
                pairs.push((have.clone(), f.clone()));
            }
            if let Some(a) = g.get("affected").filter(|v| v.truthy()) {
                pairs.push((have.clone(), a.clone()));
            }
        }
    }
    // `set(pairs)` needs hashable versions, and subprocess.run then needs
    // strings: anything else (a number, NaN included) is the Python's
    // TypeError.
    if let Some((_, v)) = pairs.iter().find(|(_, v)| matches!(v, J::Arr(_) | J::Obj(_))) {
        return Err(_unhashable(v));
    }
    let mut str_pairs = Vec::new();
    for (have, v) in &pairs {
        match v {
            J::Str(s) => str_pairs.push((have.clone(), s.clone())),
            // A lone surrogate cannot go into vercmp's argv (the
            // UnicodeEncodeError is a ValueError `_vercmp_all` catches): its
            // sign is None. (Python's surrogateescape would pass \udc80..
            // \udcff on as raw bytes; util::run takes str, so those too.)
            J::WStr(_) => {}
            other => {
                return Err(format!(
                    "TypeError: expected str, bytes or os.PathLike object, not {}",
                    other.type_name()
                ))
            }
        }
    }
    let signs = _vercmp_all(&str_pairs);
    let sign = |a: &str, b: &str| signs.get(&(a.to_string(), b.to_string())).copied().flatten();
    // `g.get(k) or ""`: only strings reach here (checked above).
    let s = |g: &J, k: &str| -> String { g.get(k).filter(|v| v.truthy()).and_then(J::as_str).unwrap_or("").to_string() };

    let mut out = Vec::new();
    // A version holding a lone surrogate: compared to nothing (sign None).
    let wide = |g: &J, k: &str| matches!(g.get(k), Some(J::WStr(_)));
    for (g, pkg, have) in candidates {
        if wide(g, "fixed") {
            continue;
        }
        let fixed = s(g, "fixed");
        if !fixed.is_empty() {
            // Vulnerable when older than the version that fixes it.
            match sign(&have, &fixed) {
                Some(s) if s < 0 => {}
                _ => continue,
            }
        } else {
            // No fix yet: vulnerable when at or past the first affected one,
            // and only while the tracker still calls it vulnerable.
            if g.get("status").and_then(J::as_str) != Some("Vulnerable") {
                continue;
            }
            let affected = s(g, "affected");
            if !wide(g, "affected") && !affected.is_empty() && sign(&have, &affected).unwrap_or(0) < 0 {
                continue;
            }
        }
        // `g.get(k, default)`: the default only when the key is missing.
        let get = |k: &str, d: &str| g.get(k).cloned().unwrap_or_else(|| J::Str(d.to_string()));
        let cves = match g.get("issues").filter(|v| v.truthy()) {
            Some(v) => _py_iter(v)?,
            None => Vec::new(),
        };
        out.push(Vuln {
            package: pkg,
            installed: have,
            group: get("name", ""),
            severity: get("severity", "Unknown"),
            kind: get("type", ""),
            fixed,
            cves,
        });
    }
    // The sort key is computed for every item, so a bad severity raises.
    for v in &out {
        v._rank()?;
    }
    out.sort_by(|a, b| (a.rank(), &a.package).cmp(&(b.rank(), &b.package)));
    Ok(Some(out))
}

/// The vulnerabilities grouped by package, in order of first appearance.
pub fn by_package(vulns: &[Vuln]) -> ByPackage {
    let mut out = ByPackage::default();
    for v in vulns {
        match out.0.iter_mut().find(|(p, _)| *p == v.package) {
            Some((_, vs)) => vs.push(v.clone()),
            None => out.0.push((v.package.clone(), vec![v.clone()])),
        }
    }
    out
}

/// The vulnerabilities a pending version resolves.
pub fn fixed_by(vulns: &[Vuln], pending: &str) -> Vec<Vuln> {
    let have: Vec<&Vuln> = vulns.iter().filter(|v| !v.fixed.is_empty()).collect();
    if pending.is_empty() || have.is_empty() {
        return Vec::new();
    }
    let pairs: Vec<(String, String)> = have.iter().map(|v| (pending.to_string(), v.fixed.clone())).collect();
    let signs = _vercmp_all(&pairs);
    // 0 means "is the fixed version": falsy, so compared explicitly.
    have.into_iter()
        .filter(|v| matches!(signs.get(&(pending.to_string(), v.fixed.clone())), Some(Some(s)) if *s >= 0))
        .cloned()
        .collect()
}

/// The Health check row.
///
/// Where the Python raises (tracker data of the wrong shape), this returns
/// the row `maintenance.run_all` builds for a check that raised, so the
/// health list reads the same; `try_finding` keeps the error.
pub fn finding() -> crate::maintenance::Finding {
    match try_finding() {
        Ok(f) => f,
        Err(e) => {
            // str(exc): the message without the exception's type.
            let exc = e.split_once(": ").map(|(_, m)| m).unwrap_or(&e);
            let name = "known_vulnerabilities";
            crate::maintenance::Finding {
                id: name.into(),
                title: crate::util::py_title(&name.replace('_', " ")),
                detail: format!("Check failed: {exc}"),
                severity: crate::maintenance::INFO.into(),
                ..Default::default()
            }
        }
    }
}

/// `finding()`, Err where the Python raises.
pub fn try_finding() -> Result<crate::maintenance::Finding, String> {
    use crate::maintenance as m;

    if !enabled() {
        return Ok(m::Finding {
            id: "vulnerabilities".into(),
            title: "Known vulnerabilities".into(),
            detail: "Checking against the Arch Security Tracker is turned off \
                     in Preferences → Privacy."
                .into(),
            severity: m::OK.into(),
            ..Default::default()
        });
    }
    _finding_for(try_vulnerabilities(None, None)?)
}


/// The rest of `finding()`, from the result of `vulnerabilities()` (split out
/// so the tests can hand it one, where the Python mocks it).
fn _finding_for(vulns: Option<Vec<Vuln>>) -> Result<crate::maintenance::Finding, String> {
    _finding_exact(vulns).map(|(f, _)| f)
}

/// `str.lower()` of a str given as code points: each run between lone
/// surrogates lowered as a whole (the surrogates themselves stay).
fn _lower_cps(cps: &[u32]) -> Vec<u32> {
    let mut out = Vec::new();
    let mut run = String::new();
    for &cp in cps {
        match char::from_u32(cp) {
            Some(c) => run.push(c),
            None => {
                out.extend(_cps(&run.to_lowercase()));
                run.clear();
                out.push(cp);
            }
        }
    }
    out.extend(_cps(&run.to_lowercase()));
    out
}

/// `_finding_for`, with the items also as the exact strs Python builds (a
/// lone surrogate from the tracker data kept; `Finding.items` can only hold
/// it as U+FFFD).
fn _finding_exact(vulns: Option<Vec<Vuln>>) -> Result<(crate::maintenance::Finding, Vec<J>), String> {
    use crate::maintenance as m;

    let Some(vulns) = vulns else {
        return Ok((m::Finding {
            id: "vulnerabilities".into(),
            title: "Known vulnerabilities".into(),
            detail: "The Arch Security Tracker could not be reached, so \
                     installed versions could not be checked."
                .into(),
            severity: m::INFO.into(),
            ..Default::default()
        }, Vec::new()));
    };
    if vulns.is_empty() {
        return Ok((m::Finding {
            id: "vulnerabilities".into(),
            title: "Known vulnerabilities".into(),
            detail: "No installed package version has a known vulnerability \
                     in the Arch Security Tracker."
                .into(),
            severity: m::OK.into(),
            ..Default::default()
        }, Vec::new()));
    }

    let groups = by_package(&vulns);
    let fixable: Vec<&String> =
        groups.iter().filter(|(_, vs)| vs.iter().any(|v| !v.fixed.is_empty())).map(|(p, _)| p).collect();
    let worst = vulns.iter().map(Vuln::rank).min().unwrap_or(4);
    let min_rank = |vs: &[Vuln]| vs.iter().map(Vuln::rank).min().unwrap_or(4);
    let mut sorted: Vec<(&String, &Vec<Vuln>)> = groups.iter().collect();
    // Stable, like Python's sorted().
    sorted.sort_by_key(|(_, vs)| min_rank(vs));
    let mut items = Vec::new();
    for (pkg, vs) in sorted {
        // min() keeps the first of equal ranks.
        let mut top = &vs[0];
        for v in vs {
            if v.rank() < top.rank() {
                top = v;
            }
        }
        // `sorted({c for v in vs for c in v.cves})`
        let all: Vec<&J> = vs.iter().flat_map(|v| v.cves.iter()).collect();
        if let Some(c) = all.iter().find(|c| matches!(c, J::Arr(_) | J::Obj(_))) {
            return Err(_unhashable(c));
        }
        let cves = _py_sorted(&_py_set(&all))?;
        let fix = if !top.fixed.is_empty() {
            format!("fixed in {}", top.fixed)
        } else {
            "no fix released yet".to_string()
        };
        let Some(severity) = top.severity.str_cps() else {
            return Err(format!(
                "AttributeError: '{}' object has no attribute 'lower'",
                top.severity.type_name()
            ));
        };
        let kind = if top.kind.truthy() { _py_str_cps(&top.kind) } else { _cps("vulnerability") };
        // `', '.join(cves[:3])`
        let mut first3: Vec<u32> = Vec::new();
        for (i, c) in cves.iter().take(3).enumerate() {
            match c.str_cps() {
                Some(s) => {
                    if i > 0 {
                        first3.extend(_cps(", "));
                    }
                    first3.extend(s);
                }
                None => {
                    let other = c;
                    return Err(format!(
                        "TypeError: sequence item {i}: expected str instance, {} found",
                        other.type_name()
                    ))
                }
            }
        }
        // f"{pkg} {top.installed} — {severity.lower()}, {kind}; {fix} · {cves}…"
        let mut item = _cps(&format!("{pkg} {} — ", top.installed));
        item.extend(_lower_cps(&severity));
        item.extend(_cps(", "));
        item.extend(kind);
        item.extend(_cps(&format!("; {fix} · ")));
        item.extend(first3);
        if cves.len() > 3 {
            item.push(u32::from('…'));
        }
        items.push(_from_cps(item));
    }
    let n = groups.len();
    let mut detail = format!(
        "{n} installed package(s) have known vulnerabilities; \
         {} can be fixed by updating.",
        fixable.len()
    );
    let unfixed = n - fixable.len();
    if unfixed > 0 {
        detail += &format!(" {unfixed} have no fixed version released yet.");
    }
    let lossy = items.iter().map(_py_str).collect();
    Ok((m::Finding {
        id: "vulnerabilities".into(),
        title: "Known vulnerabilities".into(),
        detail,
        // Fixable is actionable; unfixed is only worth knowing about.
        severity: (if !fixable.is_empty() && worst <= 1 {
            m::CRIT
        } else if !fixable.is_empty() {
            m::WARN
        } else {
            m::INFO
        })
        .into(),
        count: n as i64,
        items: lossy,
        fix_note: "Update all installs every fixed version. Packages with no \
                   fixed version are tracked at security.archlinux.org."
            .into(),
        ..Default::default()
    }, items))
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::maintenance as m;
    use serde_json::json;

    /// Matched against the Arch Security Tracker with pacman's own vercmp.
    fn groups() -> PyValue {
        PyValue::from(&json!([
            {"name": "AVG-1", "packages": ["openssl"], "status": "Fixed",
             "severity": "High", "type": "arbitrary code execution",
             "affected": "3.0.0-1", "fixed": "3.0.2-1", "issues": ["CVE-1"]},
            {"name": "AVG-2", "packages": ["libxml2"], "status": "Vulnerable",
             "severity": "Medium", "type": "denial of service",
             "affected": "2.14.0-1", "fixed": null, "issues": ["CVE-2"]},
            {"name": "AVG-3", "packages": ["curl"], "status": "Not affected",
             "severity": "Low", "type": "", "affected": "1-1", "fixed": null,
             "issues": []},
        ]))
    }

    fn vulns(pkg: &str, ver: &str) -> Vec<Vuln> {
        let installed: HashMap<String, String> = [(pkg.to_string(), ver.to_string())].into();
        vulnerabilities(Some(&installed), Some(&groups())).unwrap()
    }

    fn s(t: &str) -> PyValue {
        PyValue::Str(t.into())
    }

    #[test]
    fn test_older_than_the_fix_is_vulnerable() {
        let v = vulns("openssl", "3.0.1-1");
        assert_eq!(v.iter().map(|x| x.group.clone()).collect::<Vec<_>>(), vec![s("AVG-1")]);
    }

    #[test]
    fn test_the_fixed_version_is_not() {
        assert_eq!(vulns("openssl", "3.0.2-1"), vec![]);
        // pkgrel and epoch are compared the way pacman compares them.
        assert_eq!(vulns("openssl", "1:2.0-1"), vec![]);
    }

    #[test]
    fn test_no_fix_yet_counts_from_the_first_affected_version() {
        assert_eq!(vulns("libxml2", "2.15.0-1").len(), 1);
        assert_eq!(vulns("libxml2", "2.13.0-1"), vec![]);
    }

    #[test]
    fn test_not_affected_is_ignored() {
        assert_eq!(vulns("curl", "8-1"), vec![]);
    }

    #[test]
    fn test_a_pending_update_that_fixes_it_is_a_security_update() {
        let v = vulns("openssl", "3.0.1-1");
        assert!(!fixed_by(&v, "3.0.2-1").is_empty());
        assert!(fixed_by(&v, "3.0.1-2").is_empty());
    }

    #[test]
    fn test_unfixable_advisories_are_not_a_warning() {
        let v = vulns("libxml2", "2.15.0-1");
        assert_eq!(_finding_for(Some(v)).unwrap().severity, m::INFO);
        let v = vulns("openssl", "3.0.1-1");
        assert_eq!(_finding_for(Some(v)).unwrap().severity, m::CRIT);
    }

    #[test]
    fn test_an_unreachable_tracker_is_not_a_clean_bill() {
        let f = _finding_for(None).unwrap();
        assert_ne!(f.severity, m::OK);
    }

    #[test]
    fn py_split_and_loads() {
        assert_eq!(_split1("a  b c "), vec!["a", "b c "]);
        assert_eq!(_split1("a\x1fb"), vec!["a", "b"]);
        assert_eq!(_split1("a "), vec!["a"]);
        assert!(_split1("  ").is_empty());
        assert!(matches!(_py_loads("[NaN, -Infinity, 1e400]"), Some(J::Arr(_))));
        assert!(_py_loads("{\"a\": 1,}").is_none());
        assert!(_py_loads("01").is_none());
        assert_eq!(_py_loads("-0"), Some(J::Int("0".into())));
        assert!(_py_loads(&"1".repeat(4301)).is_none());
        assert!(_py_loads(&format!("-{}", "1".repeat(4300))).is_some());
        let at = |t: &str| _py_loads(t).and_then(|j| j.get("at").cloned());
        assert!(matches!(at("{\"at\": 1, \"at\": \"x\"}"), Some(J::Str(_))));
        let mut out = String::new();
        _py_dumps(&_py_loads("{\"a\": NaN, \"b\": [Infinity, 1e16, 123456789012345678901234567890], \"a\": \"ü\"}").unwrap(), &mut out);
        assert_eq!(out, "{\"a\": \"\\u00fc\", \"b\": [Infinity, 1e+16, 123456789012345678901234567890]}");
    }

    #[test]
    fn py_int_like_python() {
        // int(out.strip()) of vercmp's output, by sign.
        for (t, want) in [
            ("-1\n", Some(-1)),
            ("0", Some(0)),
            ("-0", Some(0)),
            ("1", Some(1)),
            ("-99999999999999999999\n", Some(-1)),
            ("-1_0", Some(-1)),
            ("1_0", Some(1)),
            ("-\u{661}", Some(-1)),
            ("\u{661}", Some(1)),
            ("\x1c-1\x1c\n", Some(-1)),
            ("+007", Some(1)),
            ("1\u{a0}", Some(1)),
            ("- 1", None),
            ("_1", None),
            ("1_", None),
            ("1__0", None),
            ("", None),
            ("-", None),
            ("x", None),
            ("1.0", None),
            ("\u{fffd}", None),
        ] {
            assert_eq!(_py_int_sign(t), want, "{t:?}");
        }
        // int() itself does not strip U+001C.
        assert_eq!(_py_int("\x1c1"), None);
        assert_eq!(_py_int(" 9 "), Some((false, "9".into())));
    }

    #[test]
    fn vercmp_known_answers() {
        use Ordering::*;
        for (a, b, want) in [
            ("1.0", "1.0", Equal),
            ("1.0", "1.1", Less),
            ("1.0.0", "1.0", Greater),
            ("1.0a", "1.0", Less),
            ("1.0alpha", "1.0", Less),
            ("1.0", "1.0.a", Less),
            ("1.0rc1", "1.0", Less),
            ("1.001", "1.1", Equal),
            ("1.0-1", "1.0-2", Less),
            ("1.0-2", "1.0", Equal),
            ("1:1.0", "2.0", Greater),
            ("0:1.0", "1.0", Equal),
            (":1.0", "1.0", Equal),
            ("1.0~rc1", "1.0", Greater),
            ("1..0", "1.0", Greater),
            ("1-", "1-a", Greater),
            ("1", "a", Greater),
            ("", "", Equal),
            ("", "1", Less),
            ("", "a", Greater),
        ] {
            assert_eq!(vercmp(a, b), want, "{a:?} vs {b:?}");
            assert_eq!(vercmp(b, a), want.reverse(), "{b:?} vs {a:?}");
        }
        let signs = _vercmp_all(&[
            ("1".into(), "2".into()),
            ("-h".into(), "1".into()),
            ("1\0".into(), "1".into()),
        ]);
        assert_eq!(signs[&("1".into(), "2".into())], Some(-1));
        assert_eq!(signs[&("-h".into(), "1".into())], None);
        assert_eq!(signs[&("1\0".into(), "1".into())], None);
    }

    /// The port agrees with pacman's `vercmp` binary, when it is installed,
    /// on a few thousand generated pairs and the versions installed here.
    #[test]
    fn vercmp_agrees_with_pacman() {
        if !std::path::Path::new("/usr/bin/vercmp").exists() {
            return;
        }
        // Building blocks for the awkward cases: epochs, empty epochs and
        // releases, leading zeros, letters against digits, '~', '+', '_',
        // repeated and non-ASCII separators, very long numbers.
        const TOKENS: &[&str] = &[
            "0", "1", "2", "9", "10", "01", "007", "123456789012345678901234567890", "a", "b", "z", "A", "alpha",
            "beta", "rc", "pre", "git", "r", ".", ".", "..", "-", "-", ":", ":", "~", "+", "_", "é", "1:", "2:",
            "-1", "-2", ".r", "g1a2",
        ];
        let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = move |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        let mut pairs: Vec<(String, String)> = Vec::new();
        for _ in 0..3000 {
            let a: String = (0..next(6)).map(|_| TOKENS[next(TOKENS.len())]).collect();
            // Half the time a near miss of a, which is where the rules bite.
            let b: String = if next(2) == 0 {
                format!("{a}{}", TOKENS[next(TOKENS.len())])
            } else {
                (0..next(6)).map(|_| TOKENS[next(TOKENS.len())]).collect()
            };
            pairs.push((a, b));
        }
        let curated = [
            "1.0", "1.0.0", "1.0a", "1.0.a", "1.0-1", "1.0-2", "1:1.0", "1:1.0-1", "2:0.1", "0:1.0", ":1.0", "1.0~rc1",
            "1.0rc1", "1.0+git1", "1.0_1", "1..0", "1.001", "1.1", "1-", "-1", "1-a", "a", "", "r123.abc",
            "1.2.3.r45.g6789", "20241001", "6.11.2.arch1-1", "6.11.2.arch1-2", "1:6.11.2.arch1-1",
        ];
        for a in curated {
            for b in curated {
                pairs.push((a.into(), b.into()));
            }
        }
        // Real versions: each installed package against its neighbour's.
        if let Ok(out) = std::process::Command::new("pacman").arg("-Q").output() {
            let text = String::from_utf8_lossy(&out.stdout);
            let vers: Vec<&str> = text.lines().filter_map(|l| l.split_whitespace().nth(1)).take(400).collect();
            for w in vers.windows(2) {
                pairs.push((w[0].into(), w[1].into()));
                pairs.push((w[0].into(), w[0].into()));
            }
        }
        pairs.retain(|(a, _)| a != "-h" && a != "--help");
        let per = pairs.len().div_ceil(8);
        let mismatches: Vec<String> = std::thread::scope(|s| {
            let handles: Vec<_> = pairs
                .chunks(per)
                .map(|chunk| {
                    s.spawn(move || {
                        let mut bad = Vec::new();
                        for (a, b) in chunk {
                            let out = std::process::Command::new("/usr/bin/vercmp").args([a, b]).output().unwrap();
                            let real: i32 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
                            if vercmp(a, b) != real.cmp(&0) {
                                bad.push(format!("{a:?} vs {b:?}: vercmp says {real}, port {:?}", vercmp(a, b)));
                            }
                        }
                        bad
                    })
                })
                .collect();
            handles.into_iter().flat_map(|h| h.join().unwrap()).collect()
        });
        assert!(pairs.len() > 3500, "{}", pairs.len());
        assert!(mismatches.is_empty(), "{} of {} disagree:\n{}", mismatches.len(), pairs.len(), mismatches.join("\n"));
    }

    #[test]
    fn utf8_error_text_is_cpythons() {
        for (b, want) in [
            (&b"a\xff"[..], "'utf-8' codec can't decode byte 0xff in position 1: invalid start byte"),
            (b"ab\x80c", "'utf-8' codec can't decode byte 0x80 in position 2: invalid start byte"),
            (b"\xe2\x82", "'utf-8' codec can't decode bytes in position 0-1: unexpected end of data"),
            (b"\xe2\x82(", "'utf-8' codec can't decode bytes in position 0-1: invalid continuation byte"),
            (b"\xe2(\xa1", "'utf-8' codec can't decode byte 0xe2 in position 0: invalid continuation byte"),
            (b"\xed\xa0\x80", "'utf-8' codec can't decode byte 0xed in position 0: invalid continuation byte"),
            (b"\xf0\x90\x80", "'utf-8' codec can't decode bytes in position 0-2: unexpected end of data"),
            (b"\xc0\xaf", "'utf-8' codec can't decode byte 0xc0 in position 0: invalid start byte"),
            (b"\xc2", "'utf-8' codec can't decode byte 0xc2 in position 0: unexpected end of data"),
            (b"x\xe2\x82\xacy\xf0\x9f\x98", "'utf-8' codec can't decode bytes in position 5-7: unexpected end of data"),
        ] {
            let e = std::str::from_utf8(b).unwrap_err();
            assert_eq!(_utf8_error(b, &e), want);
        }
    }

    #[test]
    fn proxies_the_urllib_way() {
        assert_eq!(_parse_proxy("http://127.0.0.1:9"), Ok((None, None, "127.0.0.1:9")));
        assert_eq!(_parse_proxy("127.0.0.1:9"), Ok((None, None, "127.0.0.1:9")));
        assert_eq!(_parse_proxy("socks5://u:p:q@h:1/x"), Ok((Some("u"), Some("p:q"), "h:1")));
        assert_eq!(_parse_proxy("http:/x"), Err(()));
        assert_eq!(_get_hostport("h"), Ok(("h".into(), 443)));
        assert_eq!(_get_hostport("h:"), Ok(("h".into(), 443)));
        assert_eq!(_get_hostport("[::1]:8_0"), Ok(("::1".into(), 80)));
        assert_eq!(_get_hostport("[::1]"), Ok(("::1".into(), 443)));
        assert_eq!(_get_hostport("h:x"), Err(()));
        assert_eq!(_get_hostport("h:70000"), Err(()));
        assert_eq!(_unquote("a%40b%zz%e2%82%ac"), "a@b%zz€");
        let p = |no: &str| HashMap::from([("no".to_string(), no.to_string())]);
        assert!(_proxy_bypass(URL_HOST, &p("archlinux.org")));
        assert!(_proxy_bypass(URL_HOST, &p("x, .ArchLinux.org ")));
        assert!(_proxy_bypass(URL_HOST, &p("*")));
        assert!(!_proxy_bypass(URL_HOST, &p("linux.org")));
        assert!(!_proxy_bypass(URL_HOST, &p("*.archlinux.org")));
        assert!(!_proxy_bypass(URL_HOST, &HashMap::new()));
    }

    #[test]
    fn python_values_print_and_sort_like_python() {
        let v = _py_loads("[\"a\\u200bb\", \"\\u0085\", 1e16, NaN, Infinity, {\"k'\": null}, true]").unwrap();
        assert_eq!(_py_repr(&v), "['a\\u200bb', '\\x85', 1e+16, nan, inf, {\"k'\": None}, True]");
        assert_eq!(_py_str(&J::Int("123456789012345678901234567890".into())), "123456789012345678901234567890");
        assert_eq!(_py_hash(&J::Float(2.5)), 1152921504606846978);
        assert_eq!(_py_hash(&J::Int("-1".into())), -2);
        assert_eq!(_py_hash(&J::Float(2.0)), 2);
        let items = [J::Null, J::Int("1".into())];
        let set = _py_set(&items.iter().collect::<Vec<_>>());
        assert_eq!(set, vec![&J::Null, &J::Int("1".into())]);
        assert_eq!(
            _py_sorted(&set),
            Err("TypeError: '<' not supported between instances of 'int' and 'NoneType'".into())
        );
        let items = [J::Float(2.5), J::Int("1".into()), J::Bool(true), J::Float(1.0)];
        let set = _py_set(&items.iter().collect::<Vec<_>>());
        assert_eq!(set.len(), 2);
        assert_eq!(_py_sorted(&set).unwrap(), vec![&J::Int("1".into()), &J::Float(2.5)]);
    }

    #[test]
    fn findings_print_the_values_python_does() {
        let vuln = |kind: J, cves: Vec<J>| Vuln {
            package: "pacman".into(),
            installed: "1-1".into(),
            group: s("AVG-1"),
            severity: s("High"),
            kind,
            fixed: "99999-1".into(),
            cves,
        };
        let item = |kind: J| _finding_for(Some(vec![vuln(kind, vec![])])).unwrap().items[0].clone();
        assert_eq!(item(J::Float(f64::INFINITY)), "pacman 1-1 — high, inf; fixed in 99999-1 · ");
        assert_eq!(item(J::Float(f64::NAN)), "pacman 1-1 — high, nan; fixed in 99999-1 · ");
        assert_eq!(
            item(J::Int("123456789012345678901234567890".into())),
            "pacman 1-1 — high, 123456789012345678901234567890; fixed in 99999-1 · "
        );
        let err = |cves: Vec<J>| _finding_for(Some(vec![vuln(s("t"), cves)])).unwrap_err();
        assert_eq!(err(vec![J::Null, J::Int("1".into())]), "TypeError: '<' not supported between instances of 'int' and 'NoneType'");
        assert_eq!(err(vec![J::Float(2.5), J::Int("1".into())]), "TypeError: sequence item 0: expected str instance, int found");
        // fixed: NaN is handed to vercmp, which Python refuses.
        let g = _py_loads("[{\"name\": \"AVG-N\", \"packages\": [\"a\"], \"status\": \"Vulnerable\", \"fixed\": NaN}]").unwrap();
        let installed = HashMap::from([("a".to_string(), "1-1".to_string())]);
        assert_eq!(
            try_vulnerabilities(Some(&installed), Some(&g)),
            Err("TypeError: expected str, bytes or os.PathLike object, not float".into())
        );
    }

    #[test]
    fn test_switched_off_network_features_make_no_request() {
        // The security part of the Python test: with the setting off,
        // advisories() is None without a request and finding() says so.
        let _env = crate::util::TEST_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("almanac-security-test-{}", std::process::id()));
        let old = std::env::var_os("XDG_STATE_HOME");
        std::env::set_var("XDG_STATE_HOME", &dir);
        crate::settings::set("net_security_tracker", json!(false)).unwrap();
        let a = advisories(TTL);
        let f = finding();
        match old {
            Some(v) => std::env::set_var("XDG_STATE_HOME", v),
            None => std::env::remove_var("XDG_STATE_HOME"),
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(a, Ok(None));
        assert!(f.detail.contains("turned off"));
    }

    // Python's json keeps a lone surrogate escape, and writes it back as one.
    #[test]
    fn lone_surrogates_kept() {
        let v = _py_loads(r#"["a\ud800b", "\ud83d\ude00", "\udc00\ud800"]"#).unwrap();
        let mut out = String::new();
        _py_dumps(&v, &mut out);
        assert_eq!(out, r#"["a\ud800b", "\ud83d\ude00", "\udc00\ud800"]"#);
        let J::Arr(a) = &v else { panic!() };
        assert!(matches!(&a[0], J::WStr(c) if c == &[0x61, 0xd800, 0x62]));
        assert!(matches!(&a[1], J::Str(s) if s == "\u{1f600}"));
        assert_eq!(_py_repr(&a[0]), "'a\\ud800b'");
        assert_eq!(_lower_cps(&[0x41, 0xd800, 0x42]), vec![0x61, 0xd800, 0x62]);
    }
}
