//! Port of `picks.py`.
//!
//! Today's picks: the one or two listings standing out from the rest today.
//!
//! "Most downloaded today" is a number a store front shows and a catalogue
//! almost never publishes. These catalogues publish a cumulative star count and
//! nothing else, so every figure here is measured from somewhere real, and
//! labelled with what it actually is:
//!
//! * **Stars and forks today**: a repository's public event feed records each
//!   star (a `WatchEvent`) and fork with its time, so the last 24 hours can be
//!   counted exactly from the first day, with no history needed. (GitHub no
//!   longer serves stargazer lists to anyone but the owner.) The feed holds 300
//!   events at most, which on a busy project can be under a day; the figure is
//!   then labelled with the hours it really covers, never stretched to a day.
//! * **Downloads**: GitHub counts downloads per release asset, cumulatively.
//!   Almanac keeps one snapshot a day, so the daily figure is the difference
//!   from the last day's count ("since yesterday"). Until there is a yesterday,
//!   the figure is shown as what it is: downloads of the current release.
//! * **Comments this week**: the repository's issue and pull request comments
//!   from the last seven days. A project people are talking about is a project
//!   people are using.
//!
//! Asking GitHub about all 5,600 listings is not possible, and not polite. A
//! shortlist is made locally first (recently worked on, installable, reviewed,
//! best known per marketplace), and only that shortlist is asked about. The
//! answers go through ghcache, so a second look in a day costs nothing.
//!
//! The two with the best blend of those signals are picked. Yesterday's picks
//! are marked down, so the shelf turns over rather than showing the same
//! heavyweight every day, and no marketplace gets both places while another has
//! a contender.


use num_bigint::{BigInt, BigUint, Sign};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::PathBuf;

use crate::store::Plugin;

pub const DAY: i64 = 86400;
/// with a GitHub sign-in; 4 without (60 calls an hour)
pub const SHORTLIST: usize = 10;
/// of download snapshots and past picks
pub const KEEP_DAYS: i64 = 8;
/// How much each signal counts, after scaling each to the best on the shortlist.
pub const WEIGHTS: [(&str, f64); 6] = [
    ("stars_today", 0.30),
    ("forks_today", 0.10),
    ("downloads", 0.25),
    ("comments", 0.15),
    ("stars", 0.10),
    ("fresh", 0.10),
];
/// picked in the last three days
pub const REPEAT_PENALTY: f64 = 0.5;
/// GitHub serves 300 events at most
pub const EVENT_PAGES: u32 = 3;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Signals {
    /// -1: not known
    pub stars_today: i64,
    pub forks_today: i64,
    /// what "today" covered; less on a busy repo
    pub window_hours: i64,
    pub downloads: i64,
    /// a measured daily figure, not a total
    pub downloads_today: bool,
    /// "v1.4" when it is a release total
    pub downloads_label: String,
    pub comments: i64,
    /// 100 or more
    pub comments_capped: bool,
    /// the raw cumulative count, for the snapshot
    pub release_total: i64,
    pub release_tag: String,
    // What Python holds in downloads, downloads_label, release_total and
    // release_tag when it is not an i64 and a str (set by `measure`).
    #[serde(skip)]
    pub exact: Option<Box<Exact>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Exact {
    pub downloads: Num,
    pub downloads_label: PyJson,
    pub release_total: BigInt,
    pub release_tag: PyJson,
}

impl Default for Signals {
    fn default() -> Self {
        Signals {
            stars_today: -1,
            forks_today: -1,
            window_hours: 24,
            downloads: -1,
            downloads_today: false,
            downloads_label: String::new(),
            comments: -1,
            comments_capped: false,
            release_total: -1,
            release_tag: String::new(),
            exact: None,
        }
    }
}

impl Signals {
    fn dl(&self) -> Num {
        match &self.exact {
            Some(e) => e.downloads.clone(),
            None => Num::Int(BigInt::from(self.downloads)),
        }
    }

    fn label(&self) -> PyJson {
        match &self.exact {
            Some(e) => e.downloads_label.clone(),
            None => PyJson::Str(ps(&self.downloads_label)),
        }
    }

    fn total(&self) -> BigInt {
        match &self.exact {
            Some(e) => e.release_total.clone(),
            None => BigInt::from(self.release_total),
        }
    }

    fn tag(&self) -> PyJson {
        match &self.exact {
            Some(e) => e.release_tag.clone(),
            None => PyJson::Str(ps(&self.release_tag)),
        }
    }

    fn set_exact(&mut self, e: Exact) {
        self.downloads = e.downloads.to_i64();
        self.downloads_label = py_str(&e.downloads_label);
        self.release_total = big_i64(&e.release_total);
        self.release_tag = py_str(&e.release_tag);
        self.exact = Some(Box::new(e));
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Pick {
    pub plugin: Plugin,
    pub signals: Signals,
    pub score: f64,
    /// [(number, label)]
    pub reasons: Vec<(String, String)>,
}

// ---- Python's numbers and json values ----------------------------------------

/// A Python int (any size) or float.
#[derive(Debug, Clone, PartialEq)]
pub enum Num {
    Int(BigInt),
    Float(f64),
}

impl Num {
    fn to_i64(&self) -> i64 {
        match self {
            Num::Int(n) => big_i64(n),
            Num::Float(f) => *f as i64,
        }
    }

    /// `x > 0`
    fn gt_zero(&self) -> bool {
        match self {
            Num::Int(n) => n.sign() == Sign::Plus,
            Num::Float(f) => *f > 0.0,
        }
    }

    /// `math.log1p(max(x, 0))`
    fn log1p_max0(&self) -> Result<f64, String> {
        match self {
            Num::Int(n) if n.sign() != Sign::Plus => Ok(0.0f64.ln_1p()),
            Num::Int(n) => Ok(big_f64(n)?.ln_1p()),
            Num::Float(f) => Ok(if 0.0 > *f { 0.0f64 } else { *f }.ln_1p()),
        }
    }

    fn to_json(&self) -> Value {
        match self {
            Num::Int(n) => big_json(n),
            Num::Float(f) => _float_json(*f),
        }
    }
}

/// A float for the parity runner: JSON cannot carry NaN or the infinities,
/// so both sides send those as their repr.
fn _float_json(f: f64) -> Value {
    serde_json::Number::from_f64(f).map_or_else(|| Value::String(float_repr(f)), Value::Number)
}

/// An i64 that stands for `n` where `n` does not fit (only the exact copy is read).
fn big_i64(n: &BigInt) -> i64 {
    i64::try_from(n).unwrap_or(if n.sign() == Sign::Minus { i64::MIN } else { i64::MAX })
}

fn big_json(n: &BigInt) -> Value {
    if let Ok(i) = i64::try_from(n) {
        return Value::from(i);
    }
    if let Ok(u) = u64::try_from(n) {
        return Value::from(u);
    }
    ratio_f64(n, &BigInt::from(1))
        .and_then(serde_json::Number::from_f64)
        .map_or(Value::Null, Value::Number)
}

/// `2.0 ** e`, exact wherever the result is a normal float.
fn pow2(e: i64) -> f64 {
    if e > 1023 {
        f64::INFINITY
    } else if e >= -1022 {
        f64::from_bits(((e + 1023) as u64) << 52)
    } else {
        pow2(e + 600) * pow2(-600)
    }
}

/// `a / b` for Python ints: correctly rounded (half to even), `None` when the
/// result is too large for a float (Python's OverflowError). `b` must be > 0.
fn ratio_f64(a: &BigInt, b: &BigInt) -> Option<f64> {
    if a.sign() == Sign::NoSign {
        return Some(0.0);
    }
    let neg = a.sign() == Sign::Minus;
    let (am, bm) = (a.magnitude().clone(), b.magnitude().clone());
    if am.bits() <= 53 && bm.bits() <= 53 {
        let (x, y) = (u64::try_from(&am).ok()? as f64, u64::try_from(&bm).ok()? as f64);
        let q = x / y;
        return Some(if neg { -q } else { q });
    }
    // Shift so the quotient has 55 or 56 bits, then round it to 53 by hand.
    let s = 55 - (am.bits() as i64 - bm.bits() as i64);
    let (num, den) = if s >= 0 { (am << s as u64, bm) } else { (am, bm << (-s) as u64) };
    let q: BigUint = &num / &den;
    let sticky = (&num % &den) != BigUint::from(0u32);
    let extra = q.bits() - 53;
    let mut m = u64::try_from(&(&q >> extra)).ok()?;
    let rem = u64::try_from(&(&q & ((BigUint::from(1u32) << extra) - 1u32))).ok()?;
    let half = 1u64 << (extra - 1);
    if rem > half || (rem == half && (sticky || m & 1 == 1)) {
        m += 1;
    }
    let f = m as f64 * pow2(extra as i64 - s);
    if f.is_infinite() {
        return None;
    }
    Some(if neg { -f } else { f })
}

/// `float(n)` for a Python int.
fn big_f64(n: &BigInt) -> Result<f64, String> {
    ratio_f64(n, &BigInt::from(1)).ok_or_else(|| "OverflowError: int too large to convert to float".to_string())
}

/// `int(f)`: toward zero, exactly.
fn f64_big(f: f64) -> Result<BigInt, String> {
    if f.is_nan() {
        return Err("ValueError: cannot convert float NaN to integer".into());
    }
    if f.is_infinite() {
        return Err("OverflowError: cannot convert float infinity to integer".into());
    }
    let bits = f.to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i64;
    if exp == 0 {
        return Ok(BigInt::from(0));
    }
    let mant = (bits & ((1u64 << 52) - 1)) | (1u64 << 52);
    let e = exp - 1075;
    let mag = if e >= 0 {
        BigInt::from(mant) << e as u64
    } else if -e >= 64 {
        BigInt::from(0)
    } else {
        BigInt::from(mant >> (-e) as u64)
    };
    Ok(if f < 0.0 { -mag } else { mag })
}

/// Python's exact comparison of an int with a float; `None` for NaN.
fn cmp_int_float(n: &BigInt, f: f64) -> Option<Ordering> {
    if f.is_nan() {
        return None;
    }
    if f.is_infinite() {
        return Some(if f > 0.0 { Ordering::Less } else { Ordering::Greater });
    }
    let t = f.trunc();
    let ti = f64_big(t).ok()?;
    Some(match n.cmp(&ti) {
        Ordering::Equal if f > t => Ordering::Less,
        Ordering::Equal if f < t => Ordering::Greater,
        o => o,
    })
}

/// A Python str: code points, so a lone surrogate read from `"\ud800"`
/// survives (a Rust `String` cannot hold one).
pub type PStr = Vec<u32>;

fn ps(s: &str) -> PStr {
    s.chars().map(|c| c as u32).collect()
}

fn is_surrogate(c: u32) -> bool {
    (0xd800..0xe000).contains(&c)
}

/// A `String` for display: a lone surrogate becomes U+FFFD.
fn ps_lossy(s: &[u32]) -> String {
    s.iter().map(|&c| char::from_u32(c).unwrap_or('\u{fffd}')).collect()
}

/// The `String` when `s` is one (no lone surrogate).
fn ps_string(s: &[u32]) -> Option<String> {
    s.iter().map(|&c| char::from_u32(c)).collect()
}

/// A value as Python's `json` module hands it over: ints of any size, NaN and
/// the infinities, strs with lone surrogates, dicts in insertion order.
#[derive(Debug)]
pub enum PyJson {
    None,
    Bool(bool),
    Int(BigInt),
    Float(f64),
    Str(PStr),
    List(Vec<PyJson>),
    Dict(PyDict),
}

// As deep as Python's json goes (MAX_DEPTH): cloned and compared without
// recursion.
crate::util::tree_clone_eq!(PyJson, key: PStr, units: [None], leaves: [Bool, Int, Float, Str], list: List, dict: Dict(PyDict));


#[derive(Debug, Clone, PartialEq, Default)]
pub struct PyDict(pub Vec<(PStr, PyJson)>);

impl PyDict {
    pub fn get(&self, k: &[u32]) -> Option<&PyJson> {
        self.0.iter().find(|(x, _)| x == k).map(|(_, v)| v)
    }

    pub fn get_str(&self, k: &str) -> Option<&PyJson> {
        self.get(&ps(k))
    }

    pub fn get_mut(&mut self, k: &[u32]) -> Option<&mut PyJson> {
        self.0.iter_mut().find(|(x, _)| x == k).map(|(_, v)| v)
    }

    pub fn contains_key(&self, k: &[u32]) -> bool {
        self.get(k).is_some()
    }

    /// `d[k] = v`: a key already there keeps its place.
    pub fn insert(&mut self, k: PStr, v: PyJson) {
        match self.get_mut(&k) {
            Some(slot) => *slot = v,
            None => self.0.push((k, v)),
        }
    }
}

impl PyJson {
    pub fn from_serde(v: &Value) -> PyJson {
        match v {
            Value::Null => PyJson::None,
            Value::Bool(b) => PyJson::Bool(*b),
            Value::Number(n) => match (n.as_i64(), n.as_u64()) {
                (Some(i), _) => PyJson::Int(BigInt::from(i)),
                (_, Some(u)) => PyJson::Int(BigInt::from(u)),
                _ => PyJson::Float(n.as_f64().unwrap_or(f64::NAN)),
            },
            Value::String(s) => PyJson::Str(ps(s)),
            Value::Array(a) => PyJson::List(a.iter().map(PyJson::from_serde).collect()),
            Value::Object(o) => PyJson::Dict(PyDict(o.iter().map(|(k, v)| (ps(k), PyJson::from_serde(v))).collect())),
        }
    }

    /// For the parity runner: JSON between the two sides cannot carry NaN,
    /// the infinities or a lone surrogate, so both sides send those as text
    /// (a float's repr, a str's `json.dumps`).
    pub fn to_serde(&self) -> Value {
        match self {
            PyJson::None => Value::Null,
            PyJson::Bool(b) => Value::Bool(*b),
            PyJson::Int(n) => big_json(n),
            PyJson::Float(f) => Num::Float(*f).to_json(),
            PyJson::Str(s) => _str_json(s),
            PyJson::List(a) => Value::Array(a.iter().map(PyJson::to_serde).collect()),
            PyJson::Dict(d) => Value::Object(d.0.iter().map(|(k, v)| (ps_lossy(k), v.to_serde())).collect()),
        }
    }

    /// The same value as ghcache's own: what `ghcache::get_json` makes of it
    /// is `to_value()` of this.
    fn to_ghcache(&self) -> crate::ghcache::PyValue {
        use crate::ghcache::PyValue as G;
        match self {
            PyJson::None => G::None,
            PyJson::Bool(b) => G::Bool(*b),
            PyJson::Int(n) => G::Int(n.clone()),
            PyJson::Float(f) => G::Float(*f),
            PyJson::Str(s) => G::Str(s.clone()),
            PyJson::List(a) => G::List(a.iter().map(PyJson::to_ghcache).collect()),
            PyJson::Dict(d) => G::Dict(d.0.iter().map(|(k, v)| (k.clone(), v.to_ghcache())).collect()),
        }
    }

    fn type_name(&self) -> &'static str {
        match self {
            PyJson::None => "NoneType",
            PyJson::Bool(_) => "bool",
            PyJson::Int(_) => "int",
            PyJson::Float(_) => "float",
            PyJson::Str(_) => "str",
            PyJson::List(_) => "list",
            PyJson::Dict(_) => "dict",
        }
    }

    fn truthy(&self) -> bool {
        match self {
            PyJson::None => false,
            PyJson::Bool(b) => *b,
            PyJson::Int(n) => n.sign() != Sign::NoSign,
            PyJson::Float(f) => *f != 0.0,
            PyJson::Str(s) => !s.is_empty(),
            PyJson::List(a) => !a.is_empty(),
            PyJson::Dict(d) => !d.0.is_empty(),
        }
    }

    /// A bool, int or float as a number.
    fn num(&self) -> Option<Num> {
        match self {
            PyJson::Bool(b) => Some(Num::Int(BigInt::from(u8::from(*b)))),
            PyJson::Int(n) => Some(Num::Int(n.clone())),
            PyJson::Float(f) => Some(Num::Float(*f)),
            _ => None,
        }
    }

    fn unhashable(&self) -> bool {
        matches!(self, PyJson::List(_) | PyJson::Dict(_))
    }
}

/// `a == b`
fn py_eq(a: &PyJson, b: &PyJson) -> bool {
    if let (Some(x), Some(y)) = (a.num(), b.num()) {
        return match (x, y) {
            (Num::Int(x), Num::Int(y)) => x == y,
            (Num::Int(x), Num::Float(y)) | (Num::Float(y), Num::Int(x)) => cmp_int_float(&x, y) == Some(Ordering::Equal),
            (Num::Float(x), Num::Float(y)) => x == y,
        };
    }
    match (a, b) {
        (PyJson::None, PyJson::None) => true,
        (PyJson::Str(x), PyJson::Str(y)) => x == y,
        (PyJson::List(x), PyJson::List(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| py_eq(p, q)),
        (PyJson::Dict(x), PyJson::Dict(y)) => {
            x.0.len() == y.0.len() && x.0.iter().all(|(k, v)| y.get(k).is_some_and(|w| py_eq(v, w)))
        }
        _ => false,
    }
}

/// `for x in v`
fn py_iter(v: &PyJson) -> Result<Vec<PyJson>, String> {
    match v {
        PyJson::List(a) => Ok(a.clone()),
        PyJson::Str(s) => Ok(s.iter().map(|&c| PyJson::Str(vec![c])).collect()),
        PyJson::Dict(d) => Ok(d.0.iter().map(|(k, _)| PyJson::Str(k.clone())).collect()),
        other => Err(format!("TypeError: '{}' object is not iterable", other.type_name())),
    }
}

/// `v[i]`
fn py_index(v: &PyJson, i: usize) -> Result<PyJson, String> {
    match v {
        PyJson::List(a) => a.get(i).cloned().ok_or_else(|| "IndexError: list index out of range".to_string()),
        PyJson::Str(s) => s.get(i).map(|&c| PyJson::Str(vec![c])).ok_or_else(|| "IndexError: string index out of range".to_string()),
        PyJson::Dict(_) => Err(format!("KeyError: {i}")),
        other => Err(format!("TypeError: '{}' object is not subscriptable", other.type_name())),
    }
}

/// `s in v`
fn py_contains(v: &PyJson, s: &[u32]) -> Result<bool, String> {
    match v {
        PyJson::Dict(d) => Ok(d.contains_key(s)),
        PyJson::List(a) => Ok(a.iter().any(|x| matches!(x, PyJson::Str(y) if y == s))),
        PyJson::Str(t) => Ok(s.is_empty() || t.windows(s.len()).any(|w| w == s)),
        other => Err(format!("TypeError: argument of type '{}' is not iterable", other.type_name())),
    }
}

/// `v[k] = x`
fn py_setitem(v: &mut PyJson, k: &[u32], x: PyJson) -> Result<(), String> {
    match v {
        PyJson::Dict(d) => {
            d.insert(k.to_vec(), x);
            Ok(())
        }
        PyJson::List(_) => Err("TypeError: list indices must be integers or slices, not str".into()),
        other => Err(format!("TypeError: '{}' object does not support item assignment", other.type_name())),
    }
}

/// `repr(float)` / `str(float)`
fn float_repr(f: f64) -> String {
    if f.is_nan() {
        "nan".into()
    } else if f.is_infinite() {
        if f > 0.0 { "inf" } else { "-inf" }.into()
    } else {
        crate::util::py_float_repr(f)
    }
}

/// `str(v)`, for display (a lone surrogate shows as U+FFFD).
fn py_str(v: &PyJson) -> String {
    match v {
        PyJson::Str(s) => ps_lossy(s),
        other => py_repr(other),
    }
}

/// `repr(v)`
fn py_repr(v: &PyJson) -> String {
    match v {
        PyJson::None => "None".into(),
        PyJson::Bool(b) => if *b { "True" } else { "False" }.into(),
        PyJson::Int(n) => n.to_string(),
        PyJson::Float(f) => float_repr(*f),
        PyJson::Str(s) => py_str_repr(s),
        PyJson::List(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        PyJson::Dict(d) => format!(
            "{{{}}}",
            d.0.iter().map(|(k, v)| format!("{}: {}", py_str_repr(k), py_repr(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// Python's `str.isprintable()` for one character: false for the categories
/// `repr()` escapes (Cc, Cf, Cs, Co, Cn, Zl, Zp, and Zs other than space).
fn py_isprintable(c: u32) -> bool {
    let Some(c) = char::from_u32(c) else {
        return false; // Cs
    };
    if c.is_ascii() {
        return (' '..='~').contains(&c);
    }
    static NONPRINT: std::sync::OnceLock<Option<regex::Regex>> = std::sync::OnceLock::new();
    let re = NONPRINT.get_or_init(|| regex::Regex::new(r"^[\p{Cc}\p{Cf}\p{Co}\p{Cn}\p{Zl}\p{Zp}\p{Zs}]$").ok());
    match re {
        Some(re) => !re.is_match(c.encode_utf8(&mut [0u8; 4])),
        None => true,
    }
}

/// Python's `repr()` of a str.
fn py_str_repr(s: &[u32]) -> String {
    let has = |c: char| s.contains(&(c as u32));
    let quote = if has('\'') && !has('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(quote);
    for &n in s {
        match char::from_u32(n) {
            Some('\\') => out.push_str("\\\\"),
            Some('\n') => out.push_str("\\n"),
            Some('\r') => out.push_str("\\r"),
            Some('\t') => out.push_str("\\t"),
            Some(c) if c == quote => {
                out.push('\\');
                out.push(c);
            }
            Some(c) if py_isprintable(n) => out.push(c),
            _ => {
                if n <= 0xff {
                    out.push_str(&format!("\\x{n:02x}"));
                } else if n <= 0xffff {
                    out.push_str(&format!("\\u{n:04x}"));
                } else {
                    out.push_str(&format!("\\U{n:08x}"));
                }
            }
        }
    }
    out.push(quote);
    out
}

/// Python's `str.isspace()` for one character: Unicode White_Space plus the
/// four separators \x1c-\x1f.
fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// The value of a Unicode decimal digit (category Nd), as `int()` reads it.
fn decimal_value(c: char) -> Option<u32> {
    if c.is_ascii_digit() {
        return Some(c as u32 - '0' as u32);
    }
    static ND: std::sync::OnceLock<Option<regex::Regex>> = std::sync::OnceLock::new();
    let re = ND.get_or_init(|| regex::Regex::new(r"^\p{Nd}$").ok()).as_ref()?;
    let is_nd = |c: char| re.is_match(c.encode_utf8(&mut [0u8; 4]));
    if !is_nd(c) {
        return None;
    }
    // Nd digits come in runs of whole 0-9 sets.
    let mut start = c as u32;
    while let Some(prev) = start.checked_sub(1).and_then(char::from_u32) {
        if !is_nd(prev) {
            break;
        }
        start -= 1;
    }
    Some((c as u32 - start) % 10)
}

/// `int(s)` for a str.
fn py_int_str(s: &[u32]) -> Result<BigInt, String> {
    let bad = || format!("ValueError: invalid literal for int() with base 10: {}", py_str_repr(s));
    // _PyUnicode_TransformDecimalAndSpaceToASCII
    let mut t = String::new();
    for &n in s {
        let Some(c) = char::from_u32(n) else {
            return Err(bad());
        };
        if n < 127 {
            t.push(c);
        } else if py_isspace(c) {
            t.push(' ');
        } else if let Some(d) = decimal_value(c) {
            t.push(char::from(b'0' + d as u8));
        } else {
            return Err(bad());
        }
    }
    let t = t.trim_matches(|c: char| matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r'));
    let (neg, body) = match t.as_bytes().first() {
        Some(b'+') => (false, &t[1..]),
        Some(b'-') => (true, &t[1..]),
        _ => (false, t),
    };
    let mut digits = String::new();
    let mut prev_digit = false;
    for (i, c) in body.chars().enumerate() {
        if c.is_ascii_digit() {
            digits.push(c);
            prev_digit = true;
        } else if c == '_' && prev_digit && i + 1 < body.chars().count() {
            prev_digit = false;
        } else {
            return Err(bad());
        }
    }
    if digits.is_empty() || !prev_digit {
        return Err(bad());
    }
    if digits.len() > 4300 {
        return Err("ValueError: Exceeds the limit (4300 digits) for integer string conversion".into());
    }
    let n: BigInt = digits.parse().map_err(|_| bad())?;
    Ok(if neg { -n } else { n })
}

/// `int(v or 0)` for a JSON value.
fn py_int(v: Option<&PyJson>) -> Result<BigInt, String> {
    let Some(v) = v.filter(|v| v.truthy()) else {
        return Ok(BigInt::from(0));
    };
    match v {
        PyJson::Bool(b) => Ok(BigInt::from(u8::from(*b))),
        PyJson::Int(n) => Ok(n.clone()),
        PyJson::Float(f) => f64_big(*f),
        PyJson::Str(s) => py_int_str(s),
        other => Err(format!(
            "TypeError: int() argument must be a string, a bytes-like object or a real number, not '{}'",
            other.type_name()
        )),
    }
}

// ---- json.loads / json.dumps -------------------------------------------------

/// Nesting deeper than this is refused with a RecursionError. Python 3.14's
/// limit is the C stack itself (the scanner checks how much is left), so it
/// moves with the stack size: 52,113 levels on an 8 MB stack, measured. This
/// is that figure, less a little for the frames `today` sits under; the
/// parse runs on a thread with room for it (see `_deep`).
const MAX_DEPTH: usize = 52_000;

enum LoadErr {
    /// ValueError: not JSON
    Value,
    /// RecursionError
    Recursion,
}

/// `json.loads(text)`
fn json_loads(text: &str) -> Result<PyJson, LoadErr> {
    use crate::util::pyjson;
    pyjson::loads(text, pyjson::Limit::containers(MAX_DEPTH)).map_err(|e| match e {
        pyjson::Error::Value(_) => LoadErr::Value,
        pyjson::Error::Recursion(_) => LoadErr::Recursion,
    })
}

impl crate::util::pyjson::Build for PyJson {
    type Key = PStr;
    fn null() -> Self {
        PyJson::None
    }
    fn bool(b: bool) -> Self {
        PyJson::Bool(b)
    }
    fn num(n: crate::util::pyjson::Num<'_>) -> Self {
        use crate::util::pyjson::Num;
        match n {
            Num::Int(i) => PyJson::Int(BigInt::from(i)),
            Num::UInt(u) => PyJson::Int(BigInt::from(u)),
            Num::Big(digits) => PyJson::Int(BigInt::parse_bytes(digits.as_bytes(), 10).unwrap_or_default()),
            Num::Float(f) => PyJson::Float(f),
        }
    }
    fn str(s: String) -> Self {
        PyJson::Str(ps(&s))
    }
    fn wstr(units: Vec<u32>) -> Self {
        PyJson::Str(units)
    }
    fn key(s: String) -> PStr {
        ps(&s)
    }
    fn wkey(units: Vec<u32>) -> PStr {
        units
    }
    fn list(items: Vec<Self>) -> Self {
        PyJson::List(items)
    }
    fn dict(pairs: Vec<(PStr, Self)>) -> Self {
        // `d[k] = v`: a repeated key keeps its place and takes the last value.
        PyJson::Dict(PyDict(crate::util::pyjson::dict_pairs(pairs)))
    }
}

/// A str the way `json.dumps` writes it (`ensure_ascii=True`): a lone
/// surrogate as its own `\udXXX`.
fn _json_string(s: &[u32]) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for &n in s {
        match char::from_u32(n) {
            Some('"') => out.push_str("\\\""),
            Some('\\') => out.push_str("\\\\"),
            Some('\n') => out.push_str("\\n"),
            Some('\r') => out.push_str("\\r"),
            Some('\t') => out.push_str("\\t"),
            Some('\u{08}') => out.push_str("\\b"),
            Some('\u{0c}') => out.push_str("\\f"),
            Some(c) if (0x20..0x7f).contains(&n) => out.push(c),
            Some(c) => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
            None => out.push_str(&format!("\\u{n:04x}")),
        }
    }
    out.push('"');
    out
}

/// A str for the parity runner: as itself, or as `json.dumps` writes it when
/// it holds a lone surrogate (which JSON between the sides cannot carry).
fn _str_json(s: &[u32]) -> Value {
    match ps_string(s) {
        Some(s) => Value::String(s),
        None => Value::String(_json_string(s)),
    }
}

/// `json.dumps(v)`
fn json_dumps(v: &PyJson, out: &mut String) {
    match v {
        PyJson::None => out.push_str("null"),
        PyJson::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        PyJson::Int(n) => out.push_str(&n.to_string()),
        PyJson::Float(f) => out.push_str(&crate::util::py_float_repr(*f)),
        PyJson::Str(s) => out.push_str(&_json_string(s)),
        PyJson::List(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                json_dumps(x, out);
            }
            out.push(']');
        }
        PyJson::Dict(d) => {
            out.push('{');
            for (i, (k, x)) in d.0.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&_json_string(k));
                out.push_str(": ");
                json_dumps(x, out);
            }
            out.push('}');
        }
    }
}

// ---- datetime ----------------------------------------------------------------

/// What `datetime.fromisoformat` read: the fields, and the UTC offset in
/// microseconds when there was one.
struct IsoDt {
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
    us: u32,
    tz_us: Option<i64>,
}

fn _days_in_month(year: i32, month: u32) -> u32 {
    const DAYS: [u32; 13] = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    if month == 2 && leap {
        29
    } else {
        DAYS.get(month as usize).copied().unwrap_or(0)
    }
}

fn _byte(b: &[u8], i: usize) -> u8 {
    b.get(i).copied().unwrap_or(0)
}

fn _digits(b: &[u8], p: usize, n: usize) -> Option<(usize, u32)> {
    let mut v = 0u32;
    for i in 0..n {
        let c = _byte(b, p + i);
        if !c.is_ascii_digit() {
            return None;
        }
        v = v * 10 + u32::from(c - b'0');
    }
    Some((p + n, v))
}

/// `_find_isoformat_datetime_separator`; `None` where CPython returns -1.
fn _iso_separator(b: &[u8]) -> Option<usize> {
    let len = b.len();
    if len == 7 {
        return Some(7);
    }
    if _byte(b, 4) == b'-' {
        if _byte(b, 5) == b'W' {
            if len < 8 {
                return None;
            }
            if len > 8 && _byte(b, 8) == b'-' {
                if len == 9 {
                    return None;
                }
                if len > 10 && _byte(b, 10).is_ascii_digit() {
                    return Some(8);
                }
                return Some(10);
            }
            return Some(8);
        }
        return Some(10);
    }
    if _byte(b, 4) == b'W' {
        let mut idx = 7;
        while idx < len && _byte(b, idx).is_ascii_digit() {
            idx += 1;
        }
        if idx < 9 {
            return Some(idx);
        }
        return Some(if idx % 2 == 0 { 7 } else { 8 });
    }
    Some(8)
}

/// `parse_isoformat_date`
fn _iso_date(b: &[u8], len: usize) -> Option<(i32, u32, u32)> {
    let (mut p, year) = _digits(b, 0, 4)?;
    let uses_sep = _byte(b, p) == b'-';
    if uses_sep {
        p += 1;
    }
    if _byte(b, p) == b'W' {
        p += 1;
        let (np, week) = _digits(b, p, 2)?;
        p = np;
        let wd = if p < len {
            if uses_sep {
                if _byte(b, p) != b'-' {
                    return None;
                }
                p += 1;
            }
            _digits(b, p, 1)?.1
        } else {
            1
        };
        if !(1..=9999).contains(&year) {
            return None;
        }
        let weekday = match wd {
            1 => chrono::Weekday::Mon,
            2 => chrono::Weekday::Tue,
            3 => chrono::Weekday::Wed,
            4 => chrono::Weekday::Thu,
            5 => chrono::Weekday::Fri,
            6 => chrono::Weekday::Sat,
            7 => chrono::Weekday::Sun,
            _ => return None,
        };
        use chrono::Datelike;
        let d = chrono::NaiveDate::from_isoywd_opt(year as i32, week, weekday)?;
        return Some((d.year(), d.month(), d.day()));
    }
    let (np, month) = _digits(b, p, 2)?;
    p = np;
    if uses_sep {
        if _byte(b, p) != b'-' {
            return None;
        }
        p += 1;
    }
    let (_, day) = _digits(b, p, 2)?;
    Some((year as i32, month, day))
}

/// `parse_hh_mm_ss_ff`: the fields, and whether anything follows.
fn _iso_hms(b: &[u8], mut p: usize, end: usize) -> Option<(bool, [u32; 4])> {
    let mut vals = [0u32; 4];
    let mut has_sep = true;
    let mut i = 0;
    while i < 3 {
        let (np, v) = _digits(b, p, 2)?;
        vals[i] = v;
        p = np;
        let c = _byte(b, p);
        p += 1;
        if i == 0 {
            has_sep = c == b':';
        }
        if p >= end {
            if c == b'.' || c == b',' {
                return None;
            }
            return Some((c != 0, vals));
        } else if has_sep && c == b':' {
            if i == 2 {
                return None;
            }
            i += 1;
            continue;
        } else if c == b'.' || c == b',' {
            if i < 2 {
                return None;
            }
            break;
        } else if !has_sep {
            p -= 1;
        } else {
            return None;
        }
        i += 1;
    }
    let to_parse = (end - p).min(6);
    let (np, mut us) = _digits(b, p, to_parse)?;
    p = np;
    if to_parse < 6 {
        us *= [100000, 10000, 1000, 100, 10].get(to_parse.wrapping_sub(1)).copied()?;
    }
    while _byte(b, p).is_ascii_digit() {
        p += 1;
    }
    vals[3] = us;
    Some((_byte(b, p) != 0, vals))
}

/// `parse_isoformat_time`
fn _iso_time(b: &[u8], start: usize, end: usize) -> Option<([u32; 4], Option<i64>)> {
    let mut tz = start;
    loop {
        if matches!(_byte(b, tz), b'Z' | b'+' | b'-') {
            break;
        }
        tz += 1;
        if tz >= end {
            break;
        }
    }
    let (more, t) = _iso_hms(b, start, tz)?;
    if tz == end {
        return if more { None } else { Some((t, None)) };
    }
    if _byte(b, tz) == b'Z' {
        return if _byte(b, tz + 1) != 0 { None } else { Some((t, Some(0))) };
    }
    let sign: i64 = if _byte(b, tz) == b'-' { -1 } else { 1 };
    let (more, o) = _iso_hms(b, tz + 1, end)?;
    if more {
        return None;
    }
    let off = (i64::from(o[0]) * 3600 + i64::from(o[1]) * 60 + i64::from(o[2])) * 1_000_000 + i64::from(o[3]);
    Some((t, Some(sign * off)))
}

/// `datetime.fromisoformat(s)` (CPython 3.14), `None` for its ValueError.
fn _fromisoformat(s: &str) -> Option<IsoDt> {
    if s.chars().count() < 7 {
        return None;
    }
    let b = s.as_bytes();
    let len = b.len();
    let sep = _iso_separator(b)?;
    let (mut year, mut month, mut day) = _iso_date(b, sep)?;
    let mut t = [0u32; 4];
    let mut tz = None;
    if len > sep {
        // The separator is any one character.
        let w = s[sep..].chars().next().map_or(1, char::len_utf8);
        (t, tz) = _iso_time(b, sep + w, len)?;
    }
    let [mut hour, minute, second, us] = t;
    if hour == 24 && (1..=12).contains(&month) {
        let dim = _days_in_month(year, month);
        if day <= dim {
            if minute == 0 && second == 0 && us == 0 {
                hour = 0;
                day += 1;
                if day > dim {
                    day = 1;
                    month += 1;
                    if month > 12 {
                        month = 1;
                        year += 1;
                    }
                }
            } else {
                return None;
            }
        }
    }
    if !(1..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || day < 1
        || day > _days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    if let Some(off) = tz {
        if off.abs() >= DAY * 1_000_000 {
            return None;
        }
    }
    Some(IsoDt { year, month, day, hour, minute, second, us, tz_us: tz })
}

/// `local(u)` of CPython's `local_to_seconds`: the local wall clock at the
/// UTC instant `u`, as seconds; `None` for its ValueError (a year outside
/// 1..9999).
fn _local(u: i64) -> Option<i64> {
    use chrono::{Datelike, TimeZone};
    let d = chrono::Local.timestamp_opt(u, 0).single()?;
    if !(1..=9999).contains(&d.naive_local().year()) {
        return None;
    }
    Some(u + i64::from(d.offset().local_minus_utc()))
}

impl IsoDt {
    /// `.timestamp()`: an aware time exactly, a naive one as local time
    /// (`fold=0`); `None` for its ValueError.
    fn timestamp(&self) -> Option<f64> {
        let days = chrono::NaiveDate::from_ymd_opt(self.year, self.month, self.day)
            .map_or(0, |d| i64::from(chrono::Datelike::num_days_from_ce(&d)) - 719_163);
        let t = days * DAY + i64::from(self.hour) * 3600 + i64::from(self.minute) * 60 + i64::from(self.second);
        match self.tz_us {
            Some(off) => {
                let total = BigInt::from(t) * 1_000_000 + i64::from(self.us) - off;
                ratio_f64(&total, &BigInt::from(1_000_000))
            }
            None => {
                // local_to_seconds(..., fold=0)
                let a = _local(t)? - t;
                let u1 = t - a;
                let t1 = _local(u1)?;
                let b;
                if t1 == t {
                    let u2 = u1 - DAY;
                    let bb = _local(u2)? - u2;
                    if a == bb {
                        return Some(u1 as f64 + f64::from(self.us) / 1e6);
                    }
                    b = bb;
                } else {
                    b = t1 - u1;
                }
                let u2 = t - b;
                let secs = if _local(u2)? == t {
                    u2
                } else if t1 == t {
                    u1
                } else {
                    u1.max(u2)
                };
                Some(secs as f64 + f64::from(self.us) / 1e6)
            }
        }
    }
}

/// `date.fromtimestamp(now).isoformat()`: the local day.
fn _local_day(now: f64) -> Result<String, String> {
    use chrono::{Datelike, TimeZone};
    if now.is_nan() {
        return Err("ValueError: Invalid value NaN (not a number)".into());
    }
    let f = now.floor();
    if !(-9.223372036854775808e18..9.223372036854775808e18).contains(&f) {
        return Err("OverflowError: timestamp out of range for platform time_t".into());
    }
    let d = chrono::Local
        .timestamp_opt(f as i64, 0)
        .single()
        .ok_or_else(|| "OverflowError: timestamp out of range for platform time_t".to_string())?;
    if !(1..=9999).contains(&d.year()) {
        return Err(format!("ValueError: year {} is out of range", d.year()));
    }
    Ok(d.format("%Y-%m-%d").to_string())
}

/// `datetime.fromtimestamp(t, timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")`:
/// rounded to the microsecond, half to even, as Python does.
fn _utc_stamp(t: f64) -> Result<String, String> {
    use chrono::Datelike;
    if t.is_nan() {
        return Err("ValueError: Invalid value NaN (not a number)".into());
    }
    if t.is_infinite() {
        return Err("OverflowError: timestamp out of range for platform time_t".into());
    }
    let mut int = t.trunc();
    let us = ((t - int) * 1e6).round_ties_even();
    if us >= 1e6 {
        int += 1.0;
    } else if us < 0.0 {
        int -= 1.0;
    }
    if !(-9.223372036854775808e18..9.223372036854775808e18).contains(&int) {
        return Err("OverflowError: timestamp out of range for platform time_t".into());
    }
    let d = chrono::DateTime::from_timestamp(int as i64, 0)
        .ok_or_else(|| "OverflowError: timestamp out of range for platform time_t".to_string())?;
    if !(1..=9999).contains(&d.year()) {
        return Err(format!("ValueError: year {} is out of range", d.year()));
    }
    Ok(d.format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

// ---- the seams ---------------------------------------------------------------
//
// The Python tests replace `ghcache.get_json` and `picks.eligible` on the
// module. Rust cannot, so the functions that reach either take them as
// arguments in a private `_x` twin, and the public function passes the real
// ones. Same code either way.

/// `ghcache.get_json(url, ttl=ttl)`: Err where it raises.
type GetJson<'a> = &'a (dyn Fn(&str, u64) -> Result<Option<PyJson>, String> + Sync);

fn _real_get(url: &str, ttl: u64) -> Result<Option<PyJson>, String> {
    let Some(v) = crate::ghcache::try_get_json(url, ttl, true, crate::ghcache::ACCEPT)? else {
        return Ok(None);
    };
    // ghcache answers with a serde Value, which cannot hold NaN, an int past
    // u64 or a lone surrogate the way Python's json does. What it answered
    // is the body of the cache entry for the URL (fresh, revalidated or
    // stale alike); read that back exactly, and keep it when it is the same
    // answer.
    if let Some(body) = _cached_body(url) {
        if body.to_ghcache().to_value() == v {
            return Ok(Some(body));
        }
    }
    Ok(Some(PyJson::from_serde(&v)))
}

/// `entry["body"]` of ghcache's file for `url`, read as Python reads it.
fn _cached_body(url: &str) -> Option<PyJson> {
    use sha1::{Digest, Sha1};
    let path = crate::ghcache::dir().join(format!("{}.json", hex::encode(Sha1::digest(url.as_bytes()))));
    let text = std::fs::read_to_string(path).ok()?;
    match json_loads(&text).ok()? {
        PyJson::Dict(d) => d.get_str("body").cloned(),
        _ => None,
    }
}

// `ghcache.token()` raises out of here when gh's output is not UTF-8.
fn _real_token() -> Result<bool, String> {
    Ok(!crate::ghcache::try_token()?.is_empty())
}

/// What `today` reaches outside this module.
struct Deps<'a> {
    get: GetJson<'a>,
    token: &'a (dyn Fn() -> Result<bool, String> + Sync),
    eligible: &'a (dyn Fn(&Plugin) -> bool + Sync),
    /// `settings.get("net_daily_picks")`
    enabled: &'a (dyn Fn() -> bool + Sync),
}

/// Run `f` on a thread with a stack deep enough for what Python's json
/// reads (see `MAX_DEPTH`): parsing, copying, writing and dropping a
/// nested picks.json all recurse. Inline when no such thread can be made.
fn _deep<T: Send>(f: impl FnOnce() -> T + Send) -> T {
    let cell = std::sync::Mutex::new(Some(f));
    let take = || cell.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
    let ran = std::thread::scope(|sc| {
        let h = std::thread::Builder::new().name("picks".into()).stack_size(DEEP_STACK).spawn_scoped(sc, || take().map(|f| f())).ok()?;
        h.join().unwrap_or_else(|e| std::panic::resume_unwind(e))
    });
    match ran {
        Some(v) => v,
        // the thread could not be made: the work is still here
        None => match take() {
            Some(f) => f(),
            None => unreachable!("a thread that ran the work returned it"),
        },
    }
}

/// Room for MAX_DEPTH levels (reserved, not used, until a file is that deep).
const DEEP_STACK: usize = 512 << 20;

fn _state_path() -> PathBuf {
    crate::xdg::state_home().join("almanac").join("picks.json")
}

/// Err only for what Python's `except (OSError, ValueError)` lets through.
fn _load() -> Result<PyDict, String> {
    let Ok(bytes) = std::fs::read(_state_path()) else {
        return Ok(PyDict::default());
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return Ok(PyDict::default());
    };
    match json_loads(&text) {
        Ok(PyJson::Dict(d)) => Ok(d),
        Ok(_) | Err(LoadErr::Value) => Ok(PyDict::default()),
        Err(LoadErr::Recursion) => Err("RecursionError: maximum recursion depth exceeded while decoding a JSON document".into()),
    }
}

fn _save(data: PyDict) -> PyDict {
    let p = _state_path();
    let mut text = String::new();
    let data = PyJson::Dict(data);
    json_dumps(&data, &mut text);
    let write = || -> std::io::Result<()> {
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = p.with_extension("tmp");
        std::fs::write(&tmp, text.as_bytes())?;
        std::fs::rename(&tmp, &p)
    };
    let _ = write();
    match data {
        PyJson::Dict(d) => d,
        _ => PyDict::default(),
    }
}

fn _ts(value: &str) -> f64 {
    _ts_ps(&ps(value))
}

fn _ts_ps(value: &[u32]) -> f64 {
    if value.is_empty() {
        return 0.0;
    }
    let mut v: PStr = Vec::with_capacity(value.len());
    for &c in value {
        if c == 'Z' as u32 {
            v.extend(ps("+00:00"));
        } else {
            v.push(c);
        }
    }
    // fromisoformat takes a surrogate as the date/time separator (it can
    // only be at 7, 8 or 10), and no other: a str that is not UTF-8 is not
    // an ISO string.
    if v.len() >= 7 {
        if let Some(&i) = [7, 8, 10].iter().find(|&&i| v.get(i).copied().is_some_and(is_surrogate)) {
            v[i] = 'T' as u32;
        }
    }
    let Some(v) = ps_string(&v) else {
        return 0.0;
    };
    match _fromisoformat(&v) {
        Some(dt) => dt.timestamp().unwrap_or(0.0),
        None => 0.0,
    }
}

/// owner/name for a github.com repository, '' for anything else.
fn _github(repo_url: &str) -> String {
    let mut url = repo_url.trim_matches(py_isspace).trim_end_matches('/');
    if let Some(u) = url.strip_suffix(".git") {
        url = u;
    }
    let prefix = "https://github.com/";
    let Some(rest) = url.strip_prefix(prefix) else {
        return String::new();
    };
    let parts: Vec<&str> = rest.split('/').collect();
    if parts.len() >= 2 && parts[..2].iter().all(|x| !x.is_empty()) {
        parts[..2].join("/")
    } else {
        String::new()
    }
}

/// Something that can be featured: installable from here, not installed,
/// not a theme (themes have their own page), and from a source that has been
/// looked at. A standout that turns out to be unreviewed is the worst thing
/// to put at the top of the landing page.
pub fn eligible(p: &Plugin) -> bool {
    p.can_install()
        && !p.installed
        && p.install_kind != "theme"
        && ["official", "first-party", "reviewed", "verified"].contains(&p.trust().key.as_str())
        && !_github(&p.repo).is_empty()
}

/// `now or time.time()`
fn _now(now: Option<f64>) -> f64 {
    match now {
        Some(n) if n != 0.0 => n,
        _ => crate::util::now(),
    }
}

/// The few worth asking GitHub about. Worked on in the last fortnight,
/// then the best known of each marketplace in turn, so the vendor with the
/// 15,000-star flagship cannot fill the list by itself.
pub fn shortlist(plugins: &[Plugin], size: usize, now: Option<f64>) -> Vec<Plugin> {
    _shortlist(plugins, size, _now(now), &eligible)
}

fn _shortlist(plugins: &[Plugin], size: usize, now: f64, eligible: &dyn Fn(&Plugin) -> bool) -> Vec<Plugin> {
    let mut pool: Vec<&Plugin> = plugins
        .iter()
        .filter(|p| eligible(p) && now - _ts(&p.updated) < (14 * DAY) as f64)
        .collect();
    pool.sort_by_key(|p| std::cmp::Reverse(p.stars));
    let mut by_market: BTreeMap<String, VecDeque<Plugin>> = BTreeMap::new();
    for p in pool {
        by_market.entry(p.market.clone()).or_default().push_back(p.clone());
    }
    let mut out: Vec<Plugin> = Vec::new();
    while out.len() < size && by_market.values().any(|v| !v.is_empty()) {
        for list in by_market.values_mut() {
            if !list.is_empty() && out.len() < size {
                if let Some(p) = list.pop_front() {
                    out.push(p);
                }
            }
        }
    }
    out
}

/// Stars and forks in the last 24 hours, from the repository's event feed,
/// and how many hours of it the feed actually reached back.
pub fn activity_today(full: &str, now: f64, pages: u32) -> Result<(i64, i64, i64), String> {
    _activity_today(full, now, pages, &_real_get)
}

/// `int(x)` of a float.
fn _float_int(x: f64) -> Result<i64, String> {
    if x.is_nan() {
        return Err("ValueError: cannot convert float NaN to integer".into());
    }
    if x.is_infinite() {
        return Err("OverflowError: cannot convert float infinity to integer".into());
    }
    Ok(x as i64)
}

fn _activity_today(full: &str, now: f64, pages: u32, get: GetJson) -> Result<(i64, i64, i64), String> {
    let (mut stars, mut forks) = (0i64, 0i64);
    let mut oldest = now;
    let mut exhausted = false; // the feed ended: nothing older exists
    let day = DAY as f64;
    for page in 1..=pages {
        let rows = get(
            &format!("{}/repos/{full}/events?per_page=100&page={page}", crate::ghcache::API),
            3 * 3600,
        )?;
        let Some(PyJson::List(rows)) = rows else {
            if page == 1 {
                return Ok((-1, -1, 24));
            }
            break;
        };
        for r in &rows {
            let PyJson::Dict(r) = r else {
                return Err(format!("AttributeError: '{}' object has no attribute 'get'", r.type_name()));
            };
            let at = match r.get_str("created_at") {
                None => 0.0,
                Some(v) if !v.truthy() => 0.0,
                Some(PyJson::Str(s)) => _ts_ps(s),
                Some(v) => {
                    return Err(format!("AttributeError: '{}' object has no attribute 'replace'", v.type_name()));
                }
            };
            if at < oldest {
                oldest = at;
            }
            if now - at < day {
                let kind = r.get_str("type");
                stars += i64::from(matches!(kind, Some(PyJson::Str(k)) if *k == ps("WatchEvent")));
                forks += i64::from(matches!(kind, Some(PyJson::Str(k)) if *k == ps("ForkEvent")));
            }
        }
        if rows.len() < 100 {
            exhausted = true;
            break;
        }
        if now - oldest >= day {
            break;
        }
    }
    if exhausted || now - oldest >= day {
        return Ok((stars, forks, 24));
    }
    Ok((stars, forks, 1.max(_float_int((now - oldest) / 3600.0)?)))
}

pub fn comments_this_week(full: &str, now: f64) -> Result<(i64, bool), String> {
    _comments_this_week(full, now, &_real_get)
}

fn _comments_this_week(full: &str, now: f64, get: GetJson) -> Result<(i64, bool), String> {
    let since = _utc_stamp(now - (7 * DAY) as f64)?;
    let rows = get(
        &format!("{}/repos/{full}/issues/comments?since={since}&per_page=100", crate::ghcache::API),
        6 * 3600,
    )?;
    let Some(PyJson::List(rows)) = rows else {
        return Ok((-1, false));
    };
    Ok((rows.len() as i64, rows.len() >= 100))
}

/// Downloads of the current release's assets, and its tag.
pub fn release_downloads(full: &str) -> Result<(BigInt, PyJson), String> {
    _release_downloads(full, &_real_get)
}

fn _release_downloads(full: &str, get: GetJson) -> Result<(BigInt, PyJson), String> {
    // ghcache.latest_release(full, ttl=6 * 3600)
    let rel = get(&format!("{}/repos/{full}/releases/latest", crate::ghcache::API), 6 * 3600)?;
    let nothing = (BigInt::from(-1), PyJson::Str(PStr::new()));
    let Some(PyJson::Dict(rel)) = rel else {
        return Ok(nothing);
    };
    let Some(assets) = rel.get_str("assets").filter(|a| a.truthy()) else {
        return Ok(nothing);
    };
    let mut total = BigInt::from(0);
    for a in py_iter(assets)? {
        let PyJson::Dict(a) = a else {
            return Err(format!("AttributeError: '{}' object has no attribute 'get'", a.type_name()));
        };
        total += py_int(a.get_str("download_count"))?;
    }
    let tag = rel.get_str("tag_name").filter(|t| t.truthy()).cloned();
    Ok((total, tag.unwrap_or(PyJson::Str(PStr::new()))))
}

pub fn measure(p: &Plugin, now: f64, history: &PyJson) -> Result<Signals, String> {
    _measure(p, now, history, _real_token()?, &_real_get)
}

fn _measure(p: &Plugin, now: f64, history: &PyJson, token: bool, get: GetJson) -> Result<Signals, String> {
    let full = _github(&p.repo);
    let mut s = Signals::default();
    let pages = if token { EVENT_PAGES } else { 1 };
    (s.stars_today, s.forks_today, s.window_hours) = _activity_today(&full, now, pages, get)?;
    (s.comments, s.comments_capped) = _comments_this_week(&full, now, get)?;
    let (total, tag) = _release_downloads(&full, get)?;
    let mut e = Exact {
        downloads: Num::Int(BigInt::from(-1)),
        downloads_label: PyJson::Str(PStr::new()),
        release_total: total.clone(),
        release_tag: tag.clone(),
    };
    if total.sign() != Sign::Minus {
        // Yesterday's count of the same release, when there is one.
        let today = ps(&_local_day(now)?);
        let mut past: Vec<PStr> = Vec::new();
        for d in py_iter(history)? {
            match d {
                PyJson::Str(d) => {
                    if d < today {
                        past.push(d);
                    }
                }
                other => {
                    return Err(format!(
                        "TypeError: '<' not supported between instances of '{}' and 'str'",
                        other.type_name()
                    ));
                }
            }
        }
        past.sort();
        let before = match past.last() {
            None => None,
            Some(last) => {
                let PyJson::Dict(h) = history else {
                    return Err(format!("AttributeError: '{}' object has no attribute 'get'", history.type_name()));
                };
                match h.get(last) {
                    Some(PyJson::Dict(snap)) => snap.get(&ps(&p.id)).cloned(),
                    Some(other) => {
                        return Err(format!("AttributeError: '{}' object has no attribute 'get'", other.type_name()));
                    }
                    None => None,
                }
            }
        };
        let mut daily = None;
        if let Some(before) = before.filter(PyJson::truthy) {
            if py_eq(&py_index(&before, 1)?, &tag) {
                let b0 = py_index(&before, 0)?;
                let (ge, diff) = match b0.num() {
                    Some(Num::Int(b)) => (total >= b, Num::Int(&total - &b)),
                    Some(Num::Float(f)) => {
                        let ge = matches!(cmp_int_float(&total, f), Some(Ordering::Greater | Ordering::Equal));
                        (ge, Num::Float(if ge { big_f64(&total)? - f } else { 0.0 }))
                    }
                    None => {
                        return Err(format!(
                            "TypeError: '>=' not supported between instances of 'int' and '{}'",
                            b0.type_name()
                        ));
                    }
                };
                if ge {
                    daily = Some(diff);
                }
            }
        }
        match daily {
            Some(diff) => {
                e.downloads = diff;
                s.downloads_today = true;
            }
            None => {
                e.downloads = Num::Int(total);
                e.downloads_label = tag;
            }
        }
    }
    s.set_exact(e);
    Ok(s)
}

fn _scaled(values: &[f64]) -> Vec<f64> {
    // max(values), which keeps the first of equals (and a leading NaN)
    let top = match values.split_first() {
        None => 0.0,
        Some((first, rest)) => rest.iter().fold(*first, |m, v| if *v > m { *v } else { m }),
    };
    values.iter().map(|v| if top > 0.0 { v / top } else { 0.0 }).collect()
}

/// Blend the signals. Each is scaled to the best in the same marketplace,
/// so a plugin is judged against plugins and a vendor's app against the
/// vendor's apps: 60 stars is a crowd for an Omarchy plugin and a quiet day
/// for a 15,000-star app. A signal nobody has counts for nothing.
pub fn rank(candidates: &[Plugin], signals: &[Signals], now: f64, recent_picks: &HashSet<String>) -> Result<Vec<Pick>, String> {
    let log = |n: i64| (n.max(0) as f64).ln_1p();
    // Log scales: one viral repository should not make every other number zero.
    let cols: Vec<(&str, Vec<f64>)> = vec![
        ("stars_today", signals.iter().map(|s| log(s.stars_today)).collect()),
        ("forks_today", signals.iter().map(|s| log(s.forks_today)).collect()),
        ("downloads", signals.iter().map(|s| s.dl().log1p_max0()).collect::<Result<_, _>>()?),
        ("comments", signals.iter().map(|s| log(s.comments)).collect()),
        (
            "stars",
            candidates
                .iter()
                .map(|p| {
                    if p.stars <= -1 {
                        Err(format!("ValueError: expected argument value > -1, got {}", p.stars))
                    } else {
                        Ok((p.stars as f64).ln_1p())
                    }
                })
                .collect::<Result<_, _>>()?,
        ),
        (
            "fresh",
            candidates
                .iter()
                .map(|p| 0.0f64.max(1.0 - (now - _ts(&p.updated)) / (14 * DAY) as f64))
                .collect(),
        ),
    ];
    let mut scaled: BTreeMap<&str, Vec<f64>> = cols.iter().map(|(k, _)| (*k, vec![0.0; candidates.len()])).collect();
    let markets: BTreeSet<&str> = candidates.iter().map(|p| p.market.as_str()).collect();
    for market in markets {
        let idx: Vec<usize> = candidates.iter().enumerate().filter(|(_, p)| p.market == market).map(|(i, _)| i).collect();
        for (k, v) in &cols {
            let vals: Vec<f64> = idx
                .iter()
                .map(|&i| v.get(i).copied().ok_or_else(|| "IndexError: list index out of range".to_string()))
                .collect::<Result<_, _>>()?;
            if let Some(col) = scaled.get_mut(k) {
                for (&i, x) in idx.iter().zip(_scaled(&vals)) {
                    col[i] = x;
                }
            }
        }
    }
    let mut out: Vec<Pick> = Vec::new();
    for (i, (p, s)) in candidates.iter().zip(signals).enumerate() {
        let mut score = _py_sum(WEIGHTS.iter().map(|(k, w)| w * scaled.get(k).map_or(0.0, |c| c[i])));
        if recent_picks.contains(&p.id) {
            score *= REPEAT_PENALTY;
        }
        out.push(Pick { plugin: p.clone(), signals: s.clone(), score, reasons: reasons(p, s) });
    }
    // sort(key=lambda pk: -pk.score), with list.sort's own comparisons, so
    // a NaN score lands where it lands in Python.
    let keys: Vec<f64> = out.iter().map(|pk| -pk.score).collect();
    let order = _py_sort(keys.len(), |a, b| keys[a] < keys[b]);
    let mut slots: Vec<Option<Pick>> = out.into_iter().map(Some).collect();
    Ok(order.into_iter().filter_map(|i| slots[i].take()).collect())
}

/// `sum(xs)` of floats: Python (3.12+) adds them with Neumaier's
/// compensation, starting from the int 0.
fn _py_sum(xs: impl IntoIterator<Item = f64>) -> f64 {
    let mut xs = xs.into_iter();
    let Some(first) = xs.next() else {
        return 0.0; // the int 0, which only an empty blend gives
    };
    let (mut hi, mut lo) = (0.0 + first, 0.0f64);
    for x in xs {
        let t = hi + x;
        if hi.abs() >= x.abs() {
            lo += (hi - t) + x;
        } else {
            lo += (x - t) + hi;
        }
        hi = t;
    }
    // Avoid losing the sign on a negative result, and don't let adding the
    // compensation convert an infinite or overflowed sum to a NaN.
    if lo != 0.0 && lo.is_finite() {
        hi + lo
    } else {
        hi
    }
}

// ---- list.sort -----------------------------------------------------------------
//
// CPython's timsort (Objects/listobject.c, 3.14), over the indices 0..n with
// `lt` as the only comparison. Any stable sort agrees with it while `lt` is a
// strict weak order; a NaN key is not, and then only the same comparisons in
// the same order put the items where Python puts them.

const MIN_GALLOP: usize = 7;

struct Timsort<'a> {
    lt: &'a dyn Fn(usize, usize) -> bool,
    a: Vec<usize>,
    min_gallop: usize,
    /// pending runs: (start, len, power)
    runs: Vec<(usize, usize, u32)>,
}

/// The order `sorted(range(n), key=...)` puts 0..n in, comparing with `lt`.
fn _py_sort(n: usize, lt: impl Fn(usize, usize) -> bool) -> Vec<usize> {
    let mut ts = Timsort { lt: &lt, a: (0..n).collect(), min_gallop: MIN_GALLOP, runs: Vec::new() };
    if n < 2 {
        return ts.a;
    }
    let minrun = {
        let (mut m, mut r) = (n, 0);
        while m >= 64 {
            r |= m & 1;
            m >>= 1;
        }
        m + r
    };
    let mut lo = 0;
    while lo < n {
        let remaining = n - lo;
        let mut run = ts.count_run(lo, remaining);
        if run < minrun {
            let force = remaining.min(minrun);
            ts.binarysort(lo, force, run);
            run = force;
        }
        ts.found_new_run(lo, run, n);
        ts.runs.push((lo, run, 0));
        lo += run;
    }
    // merge_force_collapse
    while ts.runs.len() > 1 {
        let mut i = ts.runs.len() - 2;
        if i > 0 && ts.runs[i - 1].1 < ts.runs[i + 1].1 {
            i -= 1;
        }
        ts.merge_at(i);
    }
    ts.a
}

impl Timsort<'_> {
    fn lt(&self, x: usize, y: usize) -> bool {
        (self.lt)(x, y)
    }

    /// `a[lo..lo+n]`, of which the first `ok` are sorted.
    fn binarysort(&mut self, lo: usize, n: usize, ok: usize) {
        for ok in ok.max(1)..n {
            let pivot = self.a[lo + ok];
            let (mut l, mut r) = (0, ok);
            while l < r {
                let m = (l + r) >> 1;
                if self.lt(pivot, self.a[lo + m]) {
                    r = m;
                } else {
                    l = m + 1;
                }
            }
            self.a[lo + l..=lo + ok].rotate_right(1);
        }
    }

    /// The run at `lo`, a descending one turned around in place.
    fn count_run(&mut self, lo: usize, remaining: usize) -> usize {
        let at = |s: &Self, i: usize| s.a[lo + i];
        let smaller = |s: &Self, n: usize| s.lt(at(s, n), at(s, n - 1));
        let larger = |s: &Self, n: usize| s.lt(at(s, n - 1), at(s, n));
        let mut n = 1;
        while n < remaining {
            if smaller(self, n) {
                break;
            }
            n += 1;
        }
        if n == remaining {
            return n;
        }
        if n > 1 {
            if self.lt(at(self, 0), at(self, n - 1)) {
                return n;
            }
            self.a[lo..lo + n].reverse();
        }
        n += 1;
        let mut neq = 0;
        let reverse_last_neq = |s: &mut Self, n: usize, neq: &mut usize| {
            if *neq > 0 {
                *neq += 1;
                s.a[lo + n - *neq..lo + n].reverse();
                *neq = 0;
            }
        };
        while n < remaining {
            if smaller(self, n) {
                reverse_last_neq(self, n, &mut neq);
            } else if larger(self, n) {
                break;
            } else {
                neq += 1;
            }
            n += 1;
        }
        reverse_last_neq(self, n, &mut neq);
        self.a[lo..lo + n].reverse();
        while n < remaining {
            if smaller(self, n) {
                break;
            }
            n += 1;
        }
        n
    }

    /// `gallop_left(key, a[base..base+n], hint)` over `v`.
    fn gallop_left(&self, key: usize, v: &[usize], hint: usize) -> usize {
        let n = v.len();
        let (mut lastofs, mut ofs): (isize, isize);
        let h = hint as isize;
        lastofs = 0;
        ofs = 1;
        if self.lt(v[hint], key) {
            let maxofs = (n - hint) as isize;
            while ofs < maxofs {
                if self.lt(v[(h + ofs) as usize], key) {
                    lastofs = ofs;
                    ofs = (ofs << 1) + 1;
                } else {
                    break;
                }
            }
            ofs = ofs.min(maxofs);
            lastofs += h;
            ofs += h;
        } else {
            let maxofs = h + 1;
            while ofs < maxofs {
                if self.lt(v[(h - ofs) as usize], key) {
                    break;
                }
                lastofs = ofs;
                ofs = (ofs << 1) + 1;
            }
            ofs = ofs.min(maxofs);
            let k = lastofs;
            lastofs = h - ofs;
            ofs = h - k;
        }
        lastofs += 1;
        while lastofs < ofs {
            let m = lastofs + ((ofs - lastofs) >> 1);
            if self.lt(v[m as usize], key) {
                lastofs = m + 1;
            } else {
                ofs = m;
            }
        }
        ofs as usize
    }

    /// `gallop_right(key, v, hint)`.
    fn gallop_right(&self, key: usize, v: &[usize], hint: usize) -> usize {
        let n = v.len();
        let (mut lastofs, mut ofs): (isize, isize);
        let h = hint as isize;
        lastofs = 0;
        ofs = 1;
        if self.lt(key, v[hint]) {
            let maxofs = h + 1;
            while ofs < maxofs {
                if self.lt(key, v[(h - ofs) as usize]) {
                    lastofs = ofs;
                    ofs = (ofs << 1) + 1;
                } else {
                    break;
                }
            }
            ofs = ofs.min(maxofs);
            let k = lastofs;
            lastofs = h - ofs;
            ofs = h - k;
        } else {
            let maxofs = (n - hint) as isize;
            while ofs < maxofs {
                if self.lt(key, v[(h + ofs) as usize]) {
                    break;
                }
                lastofs = ofs;
                ofs = (ofs << 1) + 1;
            }
            ofs = ofs.min(maxofs);
            lastofs += h;
            ofs += h;
        }
        lastofs += 1;
        while lastofs < ofs {
            let m = lastofs + ((ofs - lastofs) >> 1);
            if self.lt(key, v[m as usize]) {
                ofs = m;
            } else {
                lastofs = m + 1;
            }
        }
        ofs as usize
    }

    fn found_new_run(&mut self, s2: usize, n2: usize, len: usize) {
        let Some(&(s1, n1, _)) = self.runs.last() else {
            return;
        };
        debug_assert_eq!(s1 + n1, s2);
        // powerloop
        let mut power = 0u32;
        let (mut a, mut b) = (2 * s1 + n1, 2 * s1 + n1 + n1 + n2);
        loop {
            power += 1;
            if a >= len {
                a -= len;
                b -= len;
            } else if b >= len {
                break;
            }
            a <<= 1;
            b <<= 1;
        }
        while self.runs.len() > 1 && self.runs[self.runs.len() - 2].2 > power {
            self.merge_at(self.runs.len() - 2);
        }
        if let Some(last) = self.runs.last_mut() {
            last.2 = power;
        }
    }

    fn merge_at(&mut self, i: usize) {
        let (sa, mut na, _) = self.runs[i];
        let (sb, mut nb, _) = self.runs[i + 1];
        self.runs[i].1 = na + nb;
        self.runs.remove(i + 1);
        let k = self.gallop_right(self.a[sb], &self.a[sa..sa + na], 0);
        let sa = sa + k;
        na -= k;
        if na == 0 {
            return;
        }
        nb = self.gallop_left(self.a[sa + na - 1], &self.a[sb..sb + nb], nb - 1);
        if nb == 0 {
            return;
        }
        if na <= nb {
            self.merge_lo(sa, na, sb, nb);
        } else {
            self.merge_hi(sa, na, sb, nb);
        }
    }

    fn merge_lo(&mut self, sa: usize, mut na: usize, sb: usize, mut nb: usize) {
        let tmp: Vec<usize> = self.a[sa..sa + na].to_vec();
        let (mut dest, mut pa, mut pb) = (sa, 0usize, sb);
        self.a[dest] = self.a[pb];
        dest += 1;
        pb += 1;
        nb -= 1;
        'done: {
            if nb == 0 {
                break 'done;
            }
            if na == 1 {
                // CopyB
                self.a.copy_within(pb..pb + nb, dest);
                self.a[dest + nb] = tmp[pa];
                return;
            }
            let mut min_gallop = self.min_gallop;
            loop {
                let (mut acount, mut bcount) = (0usize, 0usize);
                loop {
                    if self.lt(self.a[pb], tmp[pa]) {
                        self.a[dest] = self.a[pb];
                        dest += 1;
                        pb += 1;
                        bcount += 1;
                        acount = 0;
                        nb -= 1;
                        if nb == 0 {
                            break 'done;
                        }
                        if bcount >= min_gallop {
                            break;
                        }
                    } else {
                        self.a[dest] = tmp[pa];
                        dest += 1;
                        pa += 1;
                        acount += 1;
                        bcount = 0;
                        na -= 1;
                        if na == 1 {
                            self.a.copy_within(pb..pb + nb, dest);
                            self.a[dest + nb] = tmp[pa];
                            return;
                        }
                        if acount >= min_gallop {
                            break;
                        }
                    }
                }
                min_gallop += 1;
                loop {
                    min_gallop -= usize::from(min_gallop > 1);
                    self.min_gallop = min_gallop;
                    let k = self.gallop_right(self.a[pb], &tmp[pa..pa + na], 0);
                    acount = k;
                    if k > 0 {
                        self.a[dest..dest + k].copy_from_slice(&tmp[pa..pa + k]);
                        dest += k;
                        pa += k;
                        na -= k;
                        if na == 1 {
                            self.a.copy_within(pb..pb + nb, dest);
                            self.a[dest + nb] = tmp[pa];
                            return;
                        }
                        if na == 0 {
                            break 'done;
                        }
                    }
                    self.a[dest] = self.a[pb];
                    dest += 1;
                    pb += 1;
                    nb -= 1;
                    if nb == 0 {
                        break 'done;
                    }
                    let k = self.gallop_left(tmp[pa], &self.a[pb..pb + nb], 0);
                    bcount = k;
                    if k > 0 {
                        self.a.copy_within(pb..pb + k, dest);
                        dest += k;
                        pb += k;
                        nb -= k;
                        if nb == 0 {
                            break 'done;
                        }
                    }
                    self.a[dest] = tmp[pa];
                    dest += 1;
                    pa += 1;
                    na -= 1;
                    if na == 1 {
                        self.a.copy_within(pb..pb + nb, dest);
                        self.a[dest + nb] = tmp[pa];
                        return;
                    }
                    if !(acount >= MIN_GALLOP || bcount >= MIN_GALLOP) {
                        break;
                    }
                }
                min_gallop += 1;
                self.min_gallop = min_gallop;
            }
        }
        // Succeed
        if na > 0 {
            self.a[dest..dest + na].copy_from_slice(&tmp[pa..pa + na]);
        }
    }

    fn merge_hi(&mut self, sa: usize, mut na: usize, sb: usize, mut nb: usize) {
        let tmp: Vec<usize> = self.a[sb..sb + nb].to_vec();
        // positions as isize: they step down past the start of a run
        let mut dest = (sb + nb - 1) as isize;
        let mut pa = (sa + na - 1) as isize;
        let mut pb = nb as isize - 1;
        let u = |x: isize| x as usize;
        self.a[u(dest)] = self.a[u(pa)];
        dest -= 1;
        pa -= 1;
        na -= 1;
        'done: {
            if na == 0 {
                break 'done;
            }
            if nb == 1 {
                // CopyA
                self.copy_a(dest, pa, na, tmp[u(pb)]);
                return;
            }
            let mut min_gallop = self.min_gallop;
            loop {
                let (mut acount, mut bcount) = (0usize, 0usize);
                loop {
                    if self.lt(tmp[u(pb)], self.a[u(pa)]) {
                        self.a[u(dest)] = self.a[u(pa)];
                        dest -= 1;
                        pa -= 1;
                        acount += 1;
                        bcount = 0;
                        na -= 1;
                        if na == 0 {
                            break 'done;
                        }
                        if acount >= min_gallop {
                            break;
                        }
                    } else {
                        self.a[u(dest)] = tmp[u(pb)];
                        dest -= 1;
                        pb -= 1;
                        bcount += 1;
                        acount = 0;
                        nb -= 1;
                        if nb == 1 {
                            self.copy_a(dest, pa, na, tmp[u(pb)]);
                            return;
                        }
                        if bcount >= min_gallop {
                            break;
                        }
                    }
                }
                min_gallop += 1;
                loop {
                    min_gallop -= usize::from(min_gallop > 1);
                    self.min_gallop = min_gallop;
                    let k = na - self.gallop_right(tmp[u(pb)], &self.a[sa..sa + na], na - 1);
                    acount = k;
                    if k > 0 {
                        dest -= k as isize;
                        pa -= k as isize;
                        self.a.copy_within(u(pa + 1)..u(pa + 1) + k, u(dest + 1));
                        na -= k;
                        if na == 0 {
                            break 'done;
                        }
                    }
                    self.a[u(dest)] = tmp[u(pb)];
                    dest -= 1;
                    pb -= 1;
                    nb -= 1;
                    if nb == 1 {
                        self.copy_a(dest, pa, na, tmp[u(pb)]);
                        return;
                    }
                    let k = nb - self.gallop_left(self.a[u(pa)], &tmp[..nb], nb - 1);
                    bcount = k;
                    if k > 0 {
                        dest -= k as isize;
                        pb -= k as isize;
                        self.a[u(dest + 1)..u(dest + 1) + k].copy_from_slice(&tmp[u(pb + 1)..u(pb + 1) + k]);
                        nb -= k;
                        if nb == 1 {
                            self.copy_a(dest, pa, na, tmp[u(pb)]);
                            return;
                        }
                        if nb == 0 {
                            break 'done;
                        }
                    }
                    self.a[u(dest)] = self.a[u(pa)];
                    dest -= 1;
                    pa -= 1;
                    na -= 1;
                    if na == 0 {
                        break 'done;
                    }
                    if !(acount >= MIN_GALLOP || bcount >= MIN_GALLOP) {
                        break;
                    }
                }
                min_gallop += 1;
                self.min_gallop = min_gallop;
            }
        }
        // Succeed
        if nb > 0 {
            let start = u(dest - (nb as isize - 1));
            self.a[start..start + nb].copy_from_slice(&tmp[..nb]);
        }
    }

    /// merge_hi's CopyA: the last `na` of A slide up by one and the one B
    /// left goes below them.
    fn copy_a(&mut self, dest: isize, pa: isize, na: usize, b: usize) {
        let na_i = na as isize;
        self.a.copy_within((pa + 1 - na_i) as usize..(pa + 1) as usize, (dest + 1 - na_i) as usize);
        self.a[(dest - na_i) as usize] = b;
    }
}

fn _compact(n: i64) -> String {
    _compact_num(&Num::Int(BigInt::from(n)))
}

fn _compact_num(n: &Num) -> String {
    let over = |by: i64| -> Option<f64> {
        match n {
            Num::Int(i) if *i >= BigInt::from(by) => Some(ratio_f64(i, &BigInt::from(by)).unwrap_or(f64::INFINITY)),
            Num::Float(f) if *f >= by as f64 => Some(f / by as f64),
            _ => None,
        }
    };
    if let Some(x) = over(1_000_000) {
        return format!("{x:.1}M").replace(".0M", "M");
    }
    if let Some(x) = over(1000) {
        return format!("{x:.1}k").replace(".0k", "k");
    }
    match n {
        Num::Int(i) => i.to_string(),
        Num::Float(f) => float_repr(*f),
    }
}

/// What made it stand out, as (number, what it counts), each one a fact.
pub fn reasons(p: &Plugin, s: &Signals) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    if s.stars_today > 0 {
        let noun = if s.stars_today == 1 { "star" } else { "stars" };
        out.push((
            format!("+{}", _compact(s.stars_today)),
            if s.window_hours >= 24 { format!("{noun} today") } else { format!("{noun} in {}h", s.window_hours) },
        ));
    }
    let downloads = s.dl();
    if downloads.gt_zero() {
        let label = s.label();
        out.push((
            _compact_num(&downloads),
            if s.downloads_today {
                "downloads since yesterday".to_string()
            } else if label.truthy() {
                format!("downloads of {}", py_str(&label))
            } else {
                "downloads".to_string()
            },
        ));
    }
    if s.forks_today > 0 && out.len() < 2 {
        let noun = if s.forks_today == 1 { "fork" } else { "forks" };
        out.push((
            format!("+{}", _compact(s.forks_today)),
            if s.window_hours >= 24 { format!("{noun} today") } else { format!("{noun} in {}h", s.window_hours) },
        ));
    }
    if s.comments > 0 {
        out.push((
            _compact(s.comments) + if s.comments_capped { "+" } else { "" },
            if s.comments == 1 { "comment this week".to_string() } else { "comments this week".to_string() },
        ));
    }
    if p.stars != 0 {
        out.push((_compact(p.stars), "stars".to_string()));
    }
    out.truncate(3);
    out
}

/// The top pick, and a second from another marketplace when one is close
/// enough to deserve the place; the same marketplace only as a last resort.
pub fn choose(ranked: &[Pick], limit: usize) -> Vec<Pick> {
    let Some(first) = ranked.first() else {
        return Vec::new();
    };
    let mut picked = vec![first.clone()];
    let rest: Vec<&Pick> = ranked[1..].iter().filter(|pk| pk.score >= 0.5 * first.score).collect();
    let other: Vec<&Pick> = rest.iter().copied().filter(|pk| pk.plugin.market != first.plugin.market).collect();
    let same: Vec<&Pick> = rest.iter().copied().filter(|pk| !other.contains(pk)).collect();
    for pk in other.iter().chain(same.iter()) {
        if picked.len() >= limit {
            break;
        }
        picked.push((*pk).clone());
    }
    picked
}

/// Today's picks. Blocking (it may ask GitHub); call off the UI thread.
// Where the Python raises, this is the empty list its caller falls back to.
pub fn today(plugins: &[Plugin], now: Option<f64>) -> Vec<Pick> {
    let now = _now(now);
    if crate::demo::active() {
        return crate::demo::picks(plugins);
    }
    _today(
        plugins,
        now,
        &Deps {
            get: &_real_get,
            token: &_real_token,
            eligible: &eligible,
            enabled: &|| crate::settings::get_bool("net_daily_picks"),
        },
    )
    .unwrap_or_default()
}

fn _today(plugins: &[Plugin], now: f64, deps: &Deps) -> Result<Vec<Pick>, String> {
    _deep(|| _today_in(plugins, now, deps))
}

fn _today_in(plugins: &[Plugin], now: f64, deps: &Deps) -> Result<Vec<Pick>, String> {
    if !(deps.enabled)() {
        return Ok(Vec::new());
    }
    let size = if (deps.token)()? { SHORTLIST } else { 4 };
    let candidates = _shortlist(plugins, size, now, deps.eligible);
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let mut data = _load()?;
    let mut history = data.get_str("downloads").cloned().unwrap_or(PyJson::Dict(PyDict::default()));
    let signals: Vec<Signals> = candidates
        .iter()
        .map(|p| _measure(p, now, &history, (deps.token)()?, deps.get))
        .collect::<Result<_, _>>()?;

    let key = ps(&_local_day(now)?);
    let PyJson::Dict(hist) = &mut history else {
        return Err(format!("AttributeError: '{}' object has no attribute 'setdefault'", history.type_name()));
    };
    if !hist.contains_key(&key) {
        hist.insert(key.clone(), PyJson::Dict(PyDict::default()));
    }
    if let Some(snap) = hist.get_mut(&key) {
        for (p, s) in candidates.iter().zip(&signals) {
            // Every day, the first count seen: tomorrow's figure is measured
            // from it, whether or not today's was.
            let pid = ps(&p.id);
            if s.total().sign() != Sign::Minus && !py_contains(snap, &pid)? {
                py_setitem(snap, &pid, PyJson::List(vec![PyJson::Int(s.total()), s.tag()]))?;
            }
        }
    }
    let mut past = data.get_str("picked").cloned().unwrap_or(PyJson::Dict(PyDict::default()));
    let PyJson::Dict(past) = &mut past else {
        return Err(format!("AttributeError: '{}' object has no attribute 'items'", past.type_name()));
    };
    let day_ts = |d: &[u32]| _ts_ps(&[d, &ps("T00:00:00+00:00")[..]].concat());
    let mut recent: HashSet<String> = HashSet::new();
    for (day, ids) in &past.0 {
        if *day != key && now - day_ts(day) < (3 * DAY) as f64 {
            for pid in py_iter(ids)? {
                match pid {
                    // (one with a lone surrogate is no plugin's id)
                    PyJson::Str(pid) => {
                        recent.extend(ps_string(&pid));
                    }
                    pid if pid.unhashable() => {
                        return Err(format!("TypeError: unhashable type: '{}'", pid.type_name()));
                    }
                    _ => {}
                }
            }
        }
    }

    let mut picked = choose(&rank(&candidates, &signals, now, &recent)?, 2);
    // Today's picks stay today's picks: a reload in the afternoon must not
    // reshuffle them because a number moved.
    if let Some(kept) = past.get(&key).filter(|k| k.truthy()) {
        let mut by_id: HashMap<String, Pick> = HashMap::new();
        for pk in rank(&candidates, &signals, now, &HashSet::new())? {
            by_id.insert(pk.plugin.id.clone(), pk);
        }
        let mut again: Vec<Pick> = Vec::new();
        for i in py_iter(kept)? {
            if i.unhashable() {
                return Err(format!("TypeError: unhashable type: '{}'", i.type_name()));
            }
            if let Some(pk) = match &i {
                PyJson::Str(i) => ps_string(i).and_then(|i| by_id.get(&i)),
                _ => None,
            } {
                again.push(pk.clone());
            }
        }
        if !again.is_empty() {
            picked = again;
        }
    }
    past.insert(key, PyJson::List(picked.iter().map(|pk| PyJson::Str(ps(&pk.plugin.id))).collect()));

    let keep = (KEEP_DAYS * DAY) as f64;
    let kept_days = |m: &PyDict| PyDict(m.0.iter().filter(|(d, _)| now - day_ts(d) < keep).cloned().collect());
    data.insert(ps("downloads"), PyJson::Dict(kept_days(hist)));
    data.insert(ps("picked"), PyJson::Dict(kept_days(past)));
    _save(data);
    Ok(picked)
}

fn _arg<T: serde::de::DeserializeOwned + Default>(args: &Value, key: &str) -> T {
    args.get(key).and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default()
}

/// The test's `eligible`: "every candidate counts as reviewed and
/// installable here".
fn _stub_eligible(p: &Plugin) -> bool {
    !p.installed && !_github(&p.repo).is_empty()
}

/// A Signals as Python's dataclass holds it.
fn _signals_json(s: &Signals) -> Value {
    let mut v = serde_json::to_value(s).unwrap_or(Value::Null);
    if let (Some(e), Value::Object(m)) = (&s.exact, &mut v) {
        m.insert("downloads".into(), e.downloads.to_json());
        m.insert("downloads_label".into(), e.downloads_label.to_serde());
        m.insert("release_total".into(), big_json(&e.release_total));
        m.insert("release_tag".into(), e.release_tag.to_serde());
    }
    v
}

fn _picks_json(picks: &[Pick]) -> Value {
    Value::Array(
        picks
            .iter()
            .map(|pk| {
                serde_json::json!({
                    "plugin": serde_json::to_value(&pk.plugin).unwrap_or(Value::Null),
                    "signals": _signals_json(&pk.signals),
                    "score": _float_json(pk.score),
                    "reasons": pk.reasons,
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    //! `TodaysPicks`: the picks are ranked from measured figures, labelled
    //! with what each one actually counts, fair across marketplaces, and
    //! stable within a day.
    use super::*;
    use std::sync::Mutex;

    const NOW: f64 = 1_800_000_000.0;

    fn plugin_at(pid: &str, market: &str, stars: i64, days_ago: f64) -> Plugin {
        let updated = chrono::DateTime::from_timestamp((NOW - days_ago * 86400.0) as i64, 0)
            .unwrap()
            .format("%Y-%m-%dT%H:%M:%S+00:00")
            .to_string();
        let mut name = pid.to_string();
        if let Some(c) = name.get_mut(0..1) {
            c.make_ascii_uppercase();
        }
        Plugin {
            id: pid.into(),
            name,
            stars,
            market: market.into(),
            repo: format!("https://github.com/o/{pid}"),
            installable: true,
            verified: true,
            install_cmd: "x".into(),
            updated,
            installed: false,
            ..Default::default()
        }
    }

    fn plugin(pid: &str) -> Plugin {
        plugin_at(pid, "omarchy", 50, 1.0)
    }

    /// Every candidate counts as reviewed and installable here.
    fn eligible(p: &Plugin) -> bool {
        _stub_eligible(p)
    }

    fn sig(f: impl FnOnce(&mut Signals)) -> Signals {
        let mut s = Signals::default();
        f(&mut s);
        s
    }

    fn has(v: &[(String, String)], a: &str, b: &str) -> bool {
        v.iter().any(|(x, y)| x == a && y == b)
    }

    #[test]
    fn test_a_short_event_feed_is_labelled_with_the_hours_it_covers() {
        let iso = |h: f64| {
            let t = NOW - h * 3600.0;
            chrono::DateTime::from_timestamp(t.floor() as i64, ((t - t.floor()) * 1e9) as u32)
                .unwrap()
                .format("%Y-%m-%dT%H:%M:%S%.6f+00:00")
                .to_string()
        };
        let busy: Vec<Value> = (0..100)
            .map(|i| serde_json::json!({"type": "WatchEvent", "created_at": iso(i as f64 / 50.0)}))
            .collect();
        let quiet = vec![
            serde_json::json!({"type": "WatchEvent", "created_at": iso(2.0)}),
            serde_json::json!({"type": "ForkEvent", "created_at": iso(30.0)}),
        ];
        let get = |url: &str, _ttl: u64| {
            Ok(Some(PyJson::from_serde(&Value::Array(if url.contains("busy") { busy.clone() } else { quiet.clone() }))))
        };
        assert_eq!(_activity_today("o/busy", NOW, 1, &get), Ok((100, 0, 1)));
        // A feed that ran out is complete: the day is fully counted.
        assert_eq!(_activity_today("o/quiet", NOW, EVENT_PAGES, &get), Ok((1, 0, 24)));
        let s = sig(|s| {
            s.stars_today = 100;
            s.window_hours = 2;
        });
        assert!(has(&reasons(&plugin("a"), &s), "+100", "stars in 2h"));
        let s = sig(|s| s.stars_today = 1);
        assert!(has(&reasons(&plugin("a"), &s), "+1", "star today"));
    }

    #[test]
    fn test_a_release_total_is_never_called_today() {
        let p = plugin("a");
        let total = sig(|s| {
            s.downloads = 5000;
            s.downloads_label = "v2.0".into();
        });
        assert!(has(&reasons(&p, &total), "5k", "downloads of v2.0"));
        let daily = sig(|s| {
            s.downloads = 40;
            s.downloads_today = true;
        });
        assert!(has(&reasons(&p, &daily), "40", "downloads since yesterday"));
    }

    #[test]
    fn test_marketplaces_are_judged_against_themselves() {
        let mut cands: Vec<Plugin> = (0..3).map(|i| plugin_at(&format!("app{i}"), "artcraft", 15000 + i, 1.0)).collect();
        cands.extend((0..3).map(|i| plugin_at(&format!("plug{i}"), "omarchy", 40 + i, 1.0)));
        let mut sigs = vec![
            sig(|s| {
                s.stars_today = 150;
                s.comments = 100;
            });
            3
        ];
        sigs.extend(vec![
            sig(|s| {
                s.stars_today = 3;
                s.comments = 4;
            });
            3
        ]);
        let ranked = rank(&cands, &sigs, NOW, &HashSet::new()).unwrap();
        let chosen = choose(&ranked, 2);
        let markets: BTreeSet<String> = chosen.iter().map(|pk| pk.plugin.market.clone()).collect();
        assert_eq!(markets, ["artcraft".to_string(), "omarchy".to_string()].into_iter().collect());
    }

    #[test]
    fn test_the_shortlist_takes_turns_between_marketplaces() {
        let mut pool: Vec<Plugin> = (0..6).map(|i| plugin_at(&format!("app{i}"), "artcraft", 9000, 1.0)).collect();
        pool.extend((0..6).map(|i| plugin_at(&format!("plug{i}"), "omarchy", 9, 1.0)));
        let short = _shortlist(&pool, 4, NOW, &eligible);
        let markets: Vec<&str> = short.iter().map(|p| p.market.as_str()).collect();
        assert_eq!(markets, ["artcraft", "omarchy", "artcraft", "omarchy"]);
        let stale = vec![plugin_at("old", "omarchy", 50, 30.0)];
        assert_eq!(_shortlist(&stale, 4, NOW, &eligible), Vec::<Plugin>::new());
    }

    #[test]
    fn test_yesterdays_pick_makes_room() {
        let cands = vec![plugin("a"), plugin("b")];
        let sigs = vec![sig(|s| s.stars_today = 10), sig(|s| s.stars_today = 8)];
        assert_eq!(rank(&cands, &sigs, NOW, &HashSet::new()).unwrap()[0].plugin.id, "a");
        let recent: HashSet<String> = ["a".to_string()].into_iter().collect();
        assert_eq!(rank(&cands, &sigs, NOW, &recent).unwrap()[0].plugin.id, "b");
    }

    #[test]
    fn test_picks_hold_for_the_day_and_switch_off() {
        let _env = crate::util::TEST_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let old = std::env::var_os("XDG_STATE_HOME");
        std::env::set_var("XDG_STATE_HOME", tmp.path());
        let numbers: Mutex<(usize, usize)> = Mutex::new((1, 9));
        let get = |url: &str, _ttl: u64| {
            Ok(Some(PyJson::from_serde(&if url.contains("/events") {
                let (a, b) = *numbers.lock().unwrap();
                let n = if url.contains("/o/a/") { a } else { b };
                Value::Array(vec![
                    serde_json::json!({"type": "WatchEvent", "created_at": "2027-01-15T07:00:00+00:00"});
                    n
                ])
            } else {
                Value::Array(vec![])
            })))
        };
        let deps = Deps {
            get: &get,
            token: &|| Ok(false),
            eligible: &eligible,
            enabled: &|| crate::settings::get_bool("net_daily_picks"),
        };
        let cands = vec![plugin("a"), plugin("b")];
        let first: Vec<String> = _today(&cands, NOW, &deps).unwrap().into_iter().map(|pk| pk.plugin.id).collect();
        *numbers.lock().unwrap() = (50, 1); // the afternoon's figures
        let again: Vec<String> = _today(&cands, NOW + 3600.0, &deps).unwrap().into_iter().map(|pk| pk.plugin.id).collect();
        assert_eq!(first, again);
        crate::settings::set("net_daily_picks", Value::Bool(false)).unwrap();
        assert_eq!(_today(&cands, NOW, &deps), Ok(Vec::<Pick>::new()));
        match old {
            Some(v) => std::env::set_var("XDG_STATE_HOME", v),
            None => std::env::remove_var("XDG_STATE_HOME"),
        }
    }

    #[test]
    fn compact_and_github() {
        assert_eq!(_compact(999), "999");
        assert_eq!(_compact(1000), "1k");
        assert_eq!(_compact(15300), "15.3k");
        assert_eq!(_compact(2_000_000), "2M");
        assert_eq!(_github("https://github.com/o/r.git/"), "o/r");
        assert_eq!(_github("https://gitlab.com/o/r"), "");
        assert_eq!(_github("https://github.com/o"), "");
        assert_eq!(_github("\x1chttps://github.com/o/r\x1f"), "o/r");
    }

    #[test]
    fn state_file_round_trips_like_pythons_json() {
        let text = r#"{"x": NaN, "y": -Infinity, "z": 1e400, "big": 123456789012345678901234567890, "f": 1E5, "n": -0, "a": 1, "a": [1.0, true, null, "BS-u00e9BS-ud83dBS-ude00"]}"#.replace("BS-", "\\");
        let Ok(v) = json_loads(&text) else { panic!("did not parse") };
        let mut out = String::new();
        json_dumps(&v, &mut out);
        assert_eq!(
            out,
            r#"{"x": NaN, "y": -Infinity, "z": Infinity, "big": 123456789012345678901234567890, "f": 100000.0, "n": 0, "a": [1.0, true, null, "BS-u00e9BS-ud83dBS-ude00"]}"#.replace("BS-", "\\")
        );
        assert!(json_loads("[1,]").is_err());
        assert!(json_loads("01").is_err());
        assert!(json_loads("\u{feff}{}").is_err());
    }

    #[test]
    fn ts_reads_what_fromisoformat_reads() {
        assert_eq!(_ts("2027-01-15T07:00:00+00:00"), 1_799_996_400.0);
        assert_eq!(_ts(" 2027-01-15T07:00:00+00:00"), 0.0);
        assert_eq!(_ts("2027-01-15T23:59:60+00:00"), 0.0);
        assert_eq!(_ts("20270115T070000Z"), 1_799_996_400.0);
        assert_eq!(_ts("2027-01-15T07:00:00+0000"), 1_799_996_400.0);
        assert_eq!(_ts("2027-01-15T24:00:00+00:00"), 1_800_057_600.0);
        assert_eq!(_ts("2027-01-15t07:00:00z"), 0.0);
        assert_eq!(_ts("2027-01-15T07:00:00.+00:00"), 0.0);
    }

    #[test]
    fn sorts_like_list_sort_even_with_nan() {
        let keys = |n: usize, mode: u32| -> Vec<f64> {
            (0..n)
                .map(|i| {
                    let x = (i * 7919 + 13) % 1009;
                    if x % 9 == 0 {
                        f64::NAN
                    } else if mode == 0 {
                        (x % 50) as f64
                    } else if mode == 1 {
                        (if i < n / 2 { i } else { n - i }) as f64
                    } else if (i / 40) % 2 == 0 {
                        (i / 3) as f64
                    } else {
                        1000.0 - i as f64
                    }
                })
                .collect()
        };
        // sum(i * order[i]) and the first three, from CPython 3.14's
        // sorted(range(n), key=keys.__getitem__)
        let want: [(usize, u32, usize, [usize; 3]); 21] = [
            (5, 0, 16, [4, 0, 3]),
            (5, 1, 26, [0, 1, 4]),
            (5, 2, 30, [0, 1, 2]),
            (30, 0, 7703, [10, 6, 9]),
            (30, 1, 7609, [0, 1, 2]),
            (30, 2, 8555, [0, 1, 2]),
            (64, 0, 82763, [10, 6, 9]),
            (64, 1, 80849, [0, 1, 2]),
            (64, 2, 83044, [0, 1, 2]),
            (65, 0, 86590, [10, 6, 9]),
            (65, 1, 84480, [0, 1, 2]),
            (65, 2, 86840, [0, 1, 2]),
            (150, 0, 1012084, [10, 6, 9]),
            (150, 1, 1039075, [0, 1, 2]),
            (150, 2, 1093120, [0, 1, 2]),
            (300, 0, 8813288, [10, 6, 9]),
            (300, 1, 8415016, [0, 1, 2]),
            (300, 2, 6817888, [0, 1, 2]),
            (1000, 0, 332143367, [39, 42, 10]),
            (1000, 1, 305991998, [0, 1, 999]),
            (1000, 2, 331798624, [0, 1, 2]),
        ];
        for (n, mode, sum, head) in want {
            let k = keys(n, mode);
            let o = _py_sort(n, |a, b| k[a] < k[b]);
            assert_eq!((o.iter().enumerate().map(|(i, v)| i * v).sum::<usize>(), [o[0], o[1], o[2]]), (sum, head), "n={n} mode={mode}");
        }
        // without NaN it is a stable sort
        let k: Vec<f64> = (0..500).map(|i| ((i * 37) % 23) as f64).collect();
        let mut stable: Vec<usize> = (0..500).collect();
        stable.sort_by(|a, b| k[*a].total_cmp(&k[*b]));
        assert_eq!(_py_sort(500, |a, b| k[a] < k[b]), stable);
    }

    #[test]
    fn sums_like_python_sum() {
        assert_eq!(_py_sum([0.30, 0.15, 0.10, 0.10]), 0.65);
        assert_eq!(_py_sum([-0.0]), 0.0);
        assert!(_py_sum([f64::INFINITY, 1.0]).is_infinite());
        assert_eq!(_py_sum([]), 0.0);
    }

    #[test]
    fn state_file_keeps_what_python_keeps() {
        // lone surrogates are kept and written back as they were
        for text in [r#"{"x": "\ud800A"}"#, r#"{"x": "\udc00"}"#, r#"{"x": "\ud83d\ude00\ud83d"}"#] {
            let Ok(v) = json_loads(text) else { panic!("did not parse") };
            let mut out = String::new();
            json_dumps(&v, &mut out);
            assert_eq!(out, text);
        }
        // the int limit counts digits, not the sign
        assert!(json_loads(&format!("-{}", "1".repeat(4300))).is_ok());
        assert!(json_loads(&format!("-{}", "1".repeat(4301))).is_err());
        // nesting as deep as Python's json reads, on the thread made for it
        let deep = |n: usize| _deep(move || json_loads(&format!("{}{}", "[".repeat(n), "]".repeat(n))).is_ok());
        assert!(deep(MAX_DEPTH));
        assert!(!deep(MAX_DEPTH + 1));
        assert_eq!(_ts_ps(&[&ps("2027-01-15")[..], &[0xd800], &ps("07:00:00Z")[..]].concat()), 1_799_996_400.0);
        assert_eq!(_ts_ps(&[&ps("2027-01-15T07:00:00Z")[..], &[0xd800]].concat()), 0.0);
    }

    #[test]
    fn int_reads_what_python_int_reads() {
        let int = |s: &str| py_int_str(&ps(s));
        assert_eq!(int("1_000"), Ok(BigInt::from(1000)));
        assert_eq!(int("١٢"), Ok(BigInt::from(12)));
        assert_eq!(int("𝟏𝟐"), Ok(BigInt::from(12)));
        assert_eq!(int(" +7\u{a0}"), Ok(BigInt::from(7)));
        assert!(int("12.5").is_err());
        assert!(int("1__0").is_err());
        assert!(int("\x1c5").is_err());
        assert!(py_int_str(&[0xd800]).is_err());
        assert_eq!(f64_big(1e20), Ok("100000000000000000000".parse().unwrap()));
    }
}
