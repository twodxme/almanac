//! Port of `sources.py`.
//!
//! Install from a link: every way a project ships, compared.
//!
//! A GitHub project usually reaches an Arch machine several ways at once: an
//! official package, an AppImage on its releases page, an AUR package that
//! builds from source, an AUR `-bin` that repackages someone's binary. They are
//! not equally trustworthy, and the difference is invisible from the project
//! page. Asked to install "this link", this lays the options out side by side
//! and recommends one, with the reason:
//!
//! 1. **The official repositories** -- built and signed by the distribution.
//! 2. **The project's own AppImage**, when its checksum can be verified: GitHub
//!    publishes a SHA-256 for every release asset, and many projects also ship a
//!    checksum file.
//! 3. **An AUR package that builds from source**, when it points at this same
//!    repository and people actually use it (votes).
//! 4. An AppImage with nothing to verify it against, or an AUR binary
//!    repackage, last -- and an AUR package whose upstream URL is a different
//!    repository is flagged, since that is how a lookalike gets installed.
//!
//! Nothing is installed by resolving. The download of an AppImage is checked
//! against its checksum before it is made executable, and a mismatch deletes it.
//!
//! NOTE: this module defines a struct named `Option` (the Python class name),
//! which shadows the prelude's `Option` inside this file; the standard one is
//! always written `std::option::Option` here.

use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// HTTP header sent with every request (`UA = {"User-Agent": "almanac"}`).
pub const UA: (&str, &str) = ("User-Agent", "almanac");

/// `_GH`: a GitHub repository link, with or without scheme, `www.`, `.git`,
/// or a trailing path/fragment/query. Python's `\w` on a `str` is "letter,
/// number or underscore" (`str.isalnum()`); Rust's `\w` also takes combining
/// marks, so the class is spelled out.
pub const GH_PATTERN: &str = r"^(?:https?://)?(?:www\.)?github\.com/([\p{L}\p{N}_.-]+)/([\p{L}\p{N}_.-]+?)(?:\.git)?(?:[/#?].*)?$";
/// `_X86` (case-insensitive): an asset name that says it is an x86-64 build.
pub const X86_PATTERN: &str = r"(?i)(x86[_-]64|amd64|x64)";
/// `_OTHER_ARCH` (case-insensitive): an asset name for some other architecture.
/// Python's `re.I` also folds `i` with dotless `ı` and dotted `İ` (and `s`
/// with long `ſ`, which Rust's `(?i)` already does); Rust's simple case
/// folding does not, so those two are spelled out.
pub const OTHER_ARCH_PATTERN: &str = r"(?i)(aarch64|arm64|armv7|armhf|[iıİ][3-6]86|r[iıİ]scv)";

pub const REPO: &str = "repo";
pub const APPIMAGE: &str = "appimage";
pub const AUR: &str = "aur";

/// One way to install the project.
///
/// The dataclass fields are typed `str`, but Python stores whatever the
/// GitHub or AUR answer held (`Version: null` stays `None`). The string
/// fields hold what the UI shows (Python's truthiness and `str()` of the
/// value); `py` keeps the value itself, for a field that held something other
/// than a `str`, and is what serialising the struct writes.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Option {
    pub kind: String,
    /// package name, or asset file name
    pub name: String,
    pub version: String,
    /// "Official package", "AppImage"…
    pub title: String,
    pub reasons: Vec<String>,
    pub warnings: Vec<String>,
    /// lower is better
    pub rank: i64,
    /// AppImage download
    pub url: String,
    /// expected digest, when known
    pub sha256: String,
    pub size_mb: f64,
    /// Fields (`version`, `url`) whose Python value is not a `str`.
    #[serde(skip)]
    pub py: BTreeMap<&'static str, Py>,
}

impl Default for Option {
    fn default() -> Self {
        Option {
            kind: String::new(),
            name: String::new(),
            version: String::new(),
            title: String::new(),
            reasons: Vec::new(),
            warnings: Vec::new(),
            rank: 99,
            url: String::new(),
            sha256: String::new(),
            size_mb: 0.0,
            py: BTreeMap::new(),
        }
    }
}

/// Everything known about a link: the repository and its install options,
/// best first.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Resolution {
    pub owner: String,
    pub repo: String,
    pub description: String,
    pub stars: i64,
    pub license: String,
    pub archived: bool,
    pub options: Vec<Option>,
    pub error: String,
    /// Fields (`owner`, `description`, `stars`, `license`) whose Python value
    /// is not what the Rust field can hold: GitHub sent a non-string, or a
    /// star count past i64.
    #[serde(skip)]
    pub py: BTreeMap<&'static str, Py>,
}

impl Resolution {
    /// `@property url`
    pub fn url(&self) -> String {
        format!("https://github.com/{}/{}", self.owner, self.repo)
    }

    /// `@property best`
    pub fn best(&self) -> std::option::Option<&Option> {
        self.options.first()
    }
}

/// Serialise `self.<field>`, or the Python value kept for it in `self.py`.
macro_rules! ser_fields {
    ($st:ident, $self:ident, [$($f:ident),*]) => {
        $(
            match $self.py.get(stringify!($f)) {
                Some(v) => $st.serialize_field(stringify!($f), v)?,
                None => $st.serialize_field(stringify!($f), &$self.$f)?,
            }
        )*
    };
}

impl Serialize for Option {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("Option", 10)?;
        ser_fields!(st, self, [kind, name, version, title, reasons, warnings, rank, url, sha256, size_mb]);
        st.end()
    }
}

impl Serialize for Resolution {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("Resolution", 8)?;
        ser_fields!(st, self, [owner, repo, description, stars, license, archived, options, error]);
        st.end()
    }
}

// ---- Python values ------------------------------------------------------------

/// A Python `int`: arbitrary precision, kept as canonical decimal digits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BigInt {
    neg: bool,
    /// No leading zeros; zero is "0" and never negative.
    digits: String,
}

/// `sys.get_int_max_str_digits()`: Python refuses longer decimal strings.
const INT_MAX_STR_DIGITS: usize = 4300;

impl BigInt {
    fn from_i64(i: i64) -> BigInt {
        BigInt { neg: i < 0, digits: i.unsigned_abs().to_string() }
    }

    /// From ASCII decimal digits (leading zeros allowed).
    fn from_digits(neg: bool, digits: &str) -> Result<BigInt, String> {
        let d = digits.trim_start_matches('0');
        if d.len() > INT_MAX_STR_DIGITS || digits.len() > INT_MAX_STR_DIGITS {
            return Err(format!(
                "ValueError: Exceeds the limit ({INT_MAX_STR_DIGITS} digits) for integer string conversion: value has {} digits; use sys.set_int_max_str_digits() to increase the limit",
                digits.len()
            ));
        }
        let d = if d.is_empty() { "0" } else { d };
        Ok(BigInt { neg: neg && d != "0", digits: d.to_string() })
    }

    /// `int(f)` of a finite float: truncated, exactly.
    fn from_f64(f: f64) -> BigInt {
        let t = f.trunc();
        if t.abs() < 9.0e15 {
            return BigInt::from_i64(t as i64);
        }
        // t = mantissa * 2^exp with exp > 0: double a base-1e9 number.
        let bits = t.to_bits();
        let exp = ((bits >> 52) & 0x7ff) as i64 - 1075;
        let mant = (bits & ((1u64 << 52) - 1)) | (1u64 << 52);
        let mut limbs: Vec<u64> = vec![mant % 1_000_000_000, (mant / 1_000_000_000) % 1_000_000_000, mant / 1_000_000_000_000_000_000];
        for _ in 0..exp.max(0) {
            let mut carry = 0;
            for l in limbs.iter_mut() {
                let v = *l * 2 + carry;
                *l = v % 1_000_000_000;
                carry = v / 1_000_000_000;
            }
            if carry > 0 {
                limbs.push(carry);
            }
        }
        while limbs.len() > 1 && limbs.last() == Some(&0) {
            limbs.pop();
        }
        let mut s = limbs.last().map(|l| l.to_string()).unwrap_or_default();
        for l in limbs.iter().rev().skip(1) {
            s.push_str(&format!("{l:09}"));
        }
        BigInt { neg: t < 0.0, digits: s }
    }

    /// The value as an i64, if it fits.
    fn to_i64(&self) -> std::option::Option<i64> {
        let mag: i128 = if self.digits.len() > 20 { return None } else { self.digits.parse().ok()? };
        i64::try_from(if self.neg { -mag } else { mag }).ok()
    }

    /// The value as an i64, clamped.
    fn to_i64_sat(&self) -> i64 {
        self.to_i64().unwrap_or(if self.neg { i64::MIN } else { i64::MAX })
    }

    /// `float(self)`, infinite when it is too large.
    fn to_f64(&self) -> f64 {
        let m: f64 = self.digits.parse().unwrap_or(f64::INFINITY);
        if self.neg {
            -m
        } else {
            m
        }
    }

    /// `self < n`.
    fn lt(&self, n: i64) -> bool {
        match self.to_i64() {
            Some(v) => v < n,
            None => self.neg,
        }
    }
}

impl std::fmt::Display for BigInt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}{}", if self.neg { "-" } else { "" }, self.digits)
    }
}

/// A value as `json.loads` gives it to Python: `int` of any size, and the
/// `float`s JSON cannot spell (`NaN`, `Infinity`, `1e400`). A `dict` keeps
/// its keys in first-insertion order, with the last value for a repeated key.
#[derive(Debug, Clone, PartialEq)]
pub enum Py {
    None,
    Bool(bool),
    Int(BigInt),
    Float(f64),
    Str(String),
    List(Vec<Py>),
    Dict(Vec<(String, Py)>),
}

impl Serialize for Py {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{SerializeMap, SerializeSeq};
        match self {
            Py::None => s.serialize_unit(),
            Py::Bool(b) => s.serialize_bool(*b),
            // JSON has no room for an int past 64 bits: the nearest float.
            Py::Int(i) => match i.to_i64() {
                Some(v) => s.serialize_i64(v),
                None => s.serialize_f64(i.to_f64()),
            },
            Py::Float(f) => s.serialize_f64(*f),
            Py::Str(t) => s.serialize_str(t),
            Py::List(l) => {
                let mut sq = s.serialize_seq(Some(l.len()))?;
                for x in l {
                    sq.serialize_element(x)?;
                }
                sq.end()
            }
            Py::Dict(d) => {
                let mut m = s.serialize_map(Some(d.len()))?;
                for (k, v) in d {
                    m.serialize_entry(k, v)?;
                }
                m.end()
            }
        }
    }
}

static EMPTY_STR: Py = Py::Str(String::new());
static EMPTY_DICT: Py = Py::Dict(Vec::new());
static EMPTY_LIST: Py = Py::List(Vec::new());

impl Py {
    /// The same value, from serde (what `ghcache` and parity hand over).
    pub fn from_value(v: &Value) -> Py {
        match v {
            Value::Null => Py::None,
            Value::Bool(b) => Py::Bool(*b),
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Py::Int(BigInt::from_i64(i))
                } else if let Some(u) = n.as_u64() {
                    Py::Int(BigInt { neg: false, digits: u.to_string() })
                } else {
                    Py::Float(n.as_f64().unwrap_or(f64::NAN))
                }
            }
            Value::String(s) => Py::Str(s.clone()),
            Value::Array(a) => Py::List(a.iter().map(Py::from_value).collect()),
            Value::Object(o) => Py::Dict(o.iter().map(|(k, v)| (k.clone(), Py::from_value(v))).collect()),
        }
    }

    /// Python truthiness.
    fn truthy(&self) -> bool {
        match self {
            Py::None => false,
            Py::Bool(b) => *b,
            Py::Int(i) => i.digits != "0",
            Py::Float(f) => *f != 0.0,
            Py::Str(s) => !s.is_empty(),
            Py::List(l) => !l.is_empty(),
            Py::Dict(d) => !d.is_empty(),
        }
    }

    /// `type(x).__name__`.
    fn type_name(&self) -> &'static str {
        match self {
            Py::None => "NoneType",
            Py::Bool(_) => "bool",
            Py::Int(_) => "int",
            Py::Float(_) => "float",
            Py::Str(_) => "str",
            Py::List(_) => "list",
            Py::Dict(_) => "dict",
        }
    }

    /// The AttributeError Python raises calling `method` on this value.
    fn no_attr(&self, method: &str) -> String {
        format!("AttributeError: '{}' object has no attribute '{method}'", self.type_name())
    }

    /// `d.get(key)`: the value must be a dict to have `.get`.
    fn get(&self, key: &str) -> Result<std::option::Option<&Py>, String> {
        match self {
            Py::Dict(d) => Ok(d.iter().find(|(k, _)| k == key).map(|(_, v)| v)),
            other => Err(other.no_attr("get")),
        }
    }

    /// The value used as a `str` (a `str` method called on it).
    fn as_str(&self, method: &str) -> Result<&str, String> {
        match self {
            Py::Str(s) => Ok(s),
            other => Err(other.no_attr(method)),
        }
    }

    /// `for x in self`: a dict gives its keys, a str its characters.
    fn iter(&self) -> Result<Vec<Py>, String> {
        match self {
            Py::List(l) => Ok(l.clone()),
            Py::Dict(d) => Ok(d.iter().map(|(k, _)| Py::Str(k.clone())).collect()),
            Py::Str(s) => Ok(s.chars().map(|c| Py::Str(c.to_string())).collect()),
            other => Err(format!("TypeError: '{}' object is not iterable", other.type_name())),
        }
    }

    /// `str(x)`, as an f-string writes it.
    fn py_str(&self) -> String {
        match self {
            Py::Str(s) => s.clone(),
            other => other.repr(),
        }
    }

    /// `repr(x)`.
    fn repr(&self) -> String {
        match self {
            Py::None => "None".into(),
            Py::Bool(true) => "True".into(),
            Py::Bool(false) => "False".into(),
            Py::Int(i) => i.to_string(),
            Py::Float(f) if f.is_nan() => "nan".into(),
            Py::Float(f) if f.is_infinite() => if *f > 0.0 { "inf".into() } else { "-inf".into() },
            Py::Float(f) => crate::util::py_float_repr(*f),
            Py::Str(s) => str_repr(s),
            Py::List(l) => format!("[{}]", l.iter().map(Py::repr).collect::<Vec<_>>().join(", ")),
            Py::Dict(d) => format!(
                "{{{}}}",
                d.iter().map(|(k, v)| format!("{}: {}", str_repr(k), v.repr())).collect::<Vec<_>>().join(", ")
            ),
        }
    }

    /// What a `str`-typed field shows for this value: the text of a truthy
    /// value, "" for a falsy one (the UI tests `if o.version`).
    fn shown(&self) -> String {
        if self.truthy() {
            self.py_str()
        } else {
            String::new()
        }
    }
}

/// `repr(str)`: single quotes unless the text has one and no double quote;
/// backslash escapes for the quote, `\\`, `\t\n\r` and characters that are
/// not printable.
fn str_repr(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(q);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c == q => {
                out.push('\\');
                out.push(c);
            }
            c if !py_isprintable(c) => {
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

/// `str.isprintable()` for one character: not printable are the Unicode
/// categories "Other" (Cc, Cf, Cs, Co, Cn: control, format, surrogate,
/// private use, unassigned) and "Separator" (Zl, Zp, Zs), except the ASCII
/// space. The regex crate's tables are Unicode 16.0, as Python 3.14's are.
fn py_isprintable(c: char) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    if c == ' ' {
        return true;
    }
    let re = RE.get_or_init(|| Regex::new(r"^[\p{C}\p{Z}]$").expect("isprintable"));
    let mut buf = [0u8; 4];
    !re.is_match(c.encode_utf8(&mut buf))
}

// ---- Python built-ins this module leans on ----------------------------------

/// `str.isspace()` for one character: Unicode White_Space plus the ASCII
/// file/group/record/unit separators Python also counts.
fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// `str.strip()`.
fn py_strip(s: &str) -> &str {
    s.trim_matches(py_isspace)
}

/// `str.split()` (no separator: runs of whitespace, no empty parts).
fn py_split(s: &str) -> Vec<&str> {
    s.split(py_isspace).filter(|p| !p.is_empty()).collect()
}

/// `str.splitlines()`: every line boundary Python knows, `\r\n` as one.
fn py_splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        if matches!(c, '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}') {
            out.push(&s[start..i]);
            let mut end = i + c.len_utf8();
            if c == '\r' {
                if let Some(&(j, '\n')) = it.peek() {
                    it.next();
                    end = j + 1;
                }
            }
            start = end;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// `x or default`.
fn or<'a>(v: std::option::Option<&'a Py>, default: &'a Py) -> &'a Py {
    match v {
        Some(x) if x.truthy() => x,
        _ => default,
    }
}

/// The first code point of every run of ten Unicode decimal digits (category
/// Nd, Unicode 16.0 as in Python 3.14): `int()` reads any of them.
const ND_ZEROS: [u32; 76] = [
    0x30, 0x660, 0x6f0, 0x7c0, 0x966, 0x9e6, 0xa66, 0xae6, 0xb66, 0xbe6, 0xc66, 0xce6, 0xd66, 0xde6, 0xe50,
    0xed0, 0xf20, 0x1040, 0x1090, 0x17e0, 0x1810, 0x1946, 0x19d0, 0x1a80, 0x1a90, 0x1b50, 0x1bb0, 0x1c40,
    0x1c50, 0xa620, 0xa8d0, 0xa900, 0xa9d0, 0xa9f0, 0xaa50, 0xabf0, 0xff10, 0x104a0, 0x10d30, 0x10d40,
    0x11066, 0x110f0, 0x11136, 0x111d0, 0x112f0, 0x11450, 0x114d0, 0x11650, 0x116c0, 0x116d0, 0x116da,
    0x11730, 0x118e0, 0x11950, 0x11bf0, 0x11c50, 0x11d50, 0x11da0, 0x11f50, 0x16130, 0x16a60, 0x16ac0,
    0x16b50, 0x16d70, 0x1ccf0, 0x1d7ce, 0x1d7d8, 0x1d7e2, 0x1d7ec, 0x1d7f6, 0x1e140, 0x1e2f0, 0x1e4f0,
    0x1e5f1, 0x1e950, 0x1fbf0,
];

/// `unicodedata.decimal(c)`.
fn py_decimal(c: char) -> std::option::Option<u32> {
    let n = c as u32;
    ND_ZEROS.iter().find(|&&z| (z..z + 10).contains(&n)).map(|z| n - z)
}

/// `int(x)`; raises (here: `Err`) as Python does.
fn py_int(v: &Py) -> Result<BigInt, String> {
    match v {
        Py::Bool(b) => Ok(BigInt::from_i64(*b as i64)),
        Py::Int(i) => Ok(i.clone()),
        Py::Float(f) if f.is_nan() => Err("ValueError: cannot convert float NaN to integer".into()),
        Py::Float(f) if f.is_infinite() => Err("OverflowError: cannot convert float infinity to integer".into()),
        Py::Float(f) => Ok(BigInt::from_f64(*f)),
        Py::Str(s) => {
            // Surrounding whitespace, one sign, then decimal digits of any
            // script, with single underscores between them.
            let bad = || format!("ValueError: invalid literal for int() with base 10: {}", str_repr(s));
            let t = py_strip(s);
            let (neg, body) = match t.strip_prefix('-') {
                Some(b) => (true, b),
                None => (false, t.strip_prefix('+').unwrap_or(t)),
            };
            if body.is_empty() || body.starts_with('_') || body.ends_with('_') || body.contains("__") {
                return Err(bad());
            }
            let mut digits = String::new();
            for c in body.chars() {
                if c == '_' {
                    continue;
                }
                match py_decimal(c) {
                    Some(d) => digits.push(char::from(b'0' + d as u8)),
                    None => return Err(bad()),
                }
            }
            BigInt::from_digits(neg, &digits)
        }
        other => Err(format!(
            "TypeError: int() argument must be a string, a bytes-like object or a real number, not '{}'",
            other.type_name()
        )),
    }
}

/// `urllib.parse.quote(s)` (safe="/"): UTF-8, upper-case hex.
fn quote(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"_.-~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `urllib.parse.unquote_to_bytes(s)`: `%XX` decoded, anything else as is.
fn unquote_to_bytes(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() && b[i + 1].is_ascii_hexdigit() && b[i + 2].is_ascii_hexdigit() {
            let hex = |c: u8| (c as char).to_digit(16).unwrap_or(0) as u8;
            out.push(hex(b[i + 1]) * 16 + hex(b[i + 2]));
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

/// `base64.decodebytes` (`binascii.a2b_base64`, not strict, as in Python
/// 3.14): characters outside the alphabet are skipped, and so is padding,
/// which only excuses a short final group.
fn b64decode(data: &[u8]) -> Result<Vec<u8>, String> {
    let val = |c: u8| -> std::option::Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a') as u32 + 26),
            b'0'..=b'9' => Some((c - b'0') as u32 + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    };
    let mut out = Vec::new();
    let (mut quad_pos, mut pads, mut left, mut n) = (0u32, 0u32, 0u32, 0usize);
    for &c in data {
        if c == b'=' {
            if quad_pos >= 2 {
                pads += 1;
            }
            continue;
        }
        let Some(v) = val(c) else { continue };
        n += 1;
        pads = 0;
        match quad_pos {
            0 => {
                quad_pos = 1;
                left = v;
            }
            1 => {
                quad_pos = 2;
                out.push(((left << 2) | (v >> 4)) as u8);
                left = v & 0x0f;
            }
            2 => {
                quad_pos = 3;
                out.push(((left << 4) | (v >> 2)) as u8);
                left = v & 0x03;
            }
            _ => {
                quad_pos = 0;
                out.push(((left << 6) | v) as u8);
                left = 0;
            }
        }
    }
    if quad_pos != 0 && quad_pos + pads < 4 {
        return Err(if quad_pos == 1 {
            format!("binascii.Error: Invalid base64-encoded string: number of data characters ({n}) cannot be 1 more than a multiple of 4")
        } else {
            "binascii.Error: Incorrect padding".into()
        });
    }
    Ok(out)
}

// ---- json.loads ----------------------------------------------------------------

/// `json.loads(bytes)`: the encoding found the way `json.detect_encoding`
/// does (a UTF-8 BOM is dropped; UTF-16 and UTF-32 are read too), then
/// Python's JSON, which also takes `NaN`, `Infinity` and `-Infinity`, and
/// integers of any size.
///
/// The parser recurses once per level, up to the 2000 it allows: on a
/// thread with room for that, whatever the caller's stack.
fn loads(raw: &[u8]) -> Result<Py, String> {
    crate::util::with_stack(crate::util::DEEP_STACK, || loads_here(raw))
}

fn loads_here(raw: &[u8]) -> Result<Py, String> {
    let text = decode_json_bytes(raw)?;
    let mut p = JsonParser { s: &text, b: text.as_bytes(), i: 0, depth: 0 };
    p.ws();
    let v = p.value()?;
    p.ws();
    if p.i != p.b.len() {
        return Err(format!("JSONDecodeError: Extra data: char {}", p.i));
    }
    Ok(v)
}

fn decode_json_bytes(b: &[u8]) -> Result<String, String> {
    let bad = |e: &str| format!("UnicodeDecodeError: {e}");
    let utf16 = |b: &[u8], be: bool| -> Result<String, String> {
        if b.len() % 2 != 0 {
            return Err(bad("utf-16: truncated data"));
        }
        let units = b.chunks(2).map(|c| if be { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_le_bytes([c[0], c[1]]) });
        char::decode_utf16(units).collect::<Result<String, _>>().map_err(|e| bad(&e.to_string()))
    };
    let utf32 = |b: &[u8], be: bool| -> Result<String, String> {
        if b.len() % 4 != 0 {
            return Err(bad("utf-32: truncated data"));
        }
        b.chunks(4)
            .map(|c| {
                let n = if be { u32::from_be_bytes([c[0], c[1], c[2], c[3]]) } else { u32::from_le_bytes([c[0], c[1], c[2], c[3]]) };
                char::from_u32(n).ok_or_else(|| bad("utf-32: code point not in range"))
            })
            .collect()
    };
    let utf8 = |b: &[u8]| std::str::from_utf8(b).map(str::to_string).map_err(|e| bad(&e.to_string()));
    if b.starts_with(&[0, 0, 0xfe, 0xff]) {
        return utf32(&b[4..], true);
    }
    if b.starts_with(&[0xff, 0xfe, 0, 0]) {
        return utf32(&b[4..], false);
    }
    if b.starts_with(&[0xfe, 0xff]) {
        return utf16(&b[2..], true);
    }
    if b.starts_with(&[0xff, 0xfe]) {
        return utf16(&b[2..], false);
    }
    if b.starts_with(&[0xef, 0xbb, 0xbf]) {
        return utf8(&b[3..]);
    }
    if b.len() >= 4 {
        if b[0] == 0 {
            return if b[1] != 0 { utf16(b, true) } else { utf32(b, true) };
        }
        if b[1] == 0 {
            return if b[2] != 0 || b[3] != 0 { utf16(b, false) } else { utf32(b, false) };
        }
    } else if b.len() == 2 {
        if b[0] == 0 {
            return utf16(b, true);
        }
        if b[1] == 0 {
            return utf16(b, false);
        }
    }
    utf8(b)
}

struct JsonParser<'a> {
    s: &'a str,
    b: &'a [u8],
    i: usize,
    depth: usize,
}

impl JsonParser<'_> {
    fn err(&self, what: &str) -> String {
        format!("JSONDecodeError: {what}: char {}", self.i)
    }

    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn eat(&mut self, word: &str) -> bool {
        if self.b[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self) -> Result<Py, String> {
        // Python's scanner recurses in C; it gives up (RecursionError) on very
        // deep nesting, and so does this, before the stack does.
        if self.depth > 2000 {
            return Err("RecursionError: maximum recursion depth exceeded while decoding a JSON document".into());
        }
        match self.b.get(self.i) {
            Some(b'"') => {
                self.i += 1;
                self.string().map(Py::Str)
            }
            Some(b'{') => {
                self.i += 1;
                self.depth += 1;
                let r = self.object();
                self.depth -= 1;
                r
            }
            Some(b'[') => {
                self.i += 1;
                self.depth += 1;
                let r = self.array();
                self.depth -= 1;
                r
            }
            Some(b'n') if self.eat("null") => Ok(Py::None),
            Some(b't') if self.eat("true") => Ok(Py::Bool(true)),
            Some(b'f') if self.eat("false") => Ok(Py::Bool(false)),
            _ => {
                if let Some(n) = self.number()? {
                    return Ok(n);
                }
                if self.eat("NaN") {
                    Ok(Py::Float(f64::NAN))
                } else if self.eat("Infinity") {
                    Ok(Py::Float(f64::INFINITY))
                } else if self.eat("-Infinity") {
                    Ok(Py::Float(f64::NEG_INFINITY))
                } else {
                    Err(self.err("Expecting value"))
                }
            }
        }
    }

    /// `-?(?:0|[1-9]\d*)(\.\d+)?([eE][-+]?\d+)?`, ASCII digits.
    fn number(&mut self) -> Result<std::option::Option<Py>, String> {
        let b = self.b;
        let start = self.i;
        let mut j = start;
        let digit = |j: usize| b.get(j).is_some_and(u8::is_ascii_digit);
        if b.get(j) == Some(&b'-') {
            j += 1;
        }
        if b.get(j) == Some(&b'0') {
            j += 1;
        } else if b.get(j).is_some_and(|c| (b'1'..=b'9').contains(c)) {
            while digit(j) {
                j += 1;
            }
        } else {
            return Ok(None);
        }
        let int_end = j;
        if b.get(j) == Some(&b'.') && digit(j + 1) {
            j += 1;
            while digit(j) {
                j += 1;
            }
        }
        if matches!(b.get(j), Some(b'e' | b'E')) {
            let mut k = j + 1;
            if matches!(b.get(k), Some(b'+' | b'-')) {
                k += 1;
            }
            if digit(k) {
                while digit(k) {
                    k += 1;
                }
                j = k;
            }
        }
        self.i = j;
        let text = &self.s[start..j];
        if j == int_end {
            let (neg, digits) = match text.strip_prefix('-') {
                Some(d) => (true, d),
                None => (false, text),
            };
            BigInt::from_digits(neg, digits).map(|i| Some(Py::Int(i)))
        } else {
            Ok(Some(Py::Float(text.parse().unwrap_or(f64::NAN))))
        }
    }

    fn hex4(&self, at: usize) -> std::option::Option<u32> {
        let h = self.b.get(at..at + 4)?;
        if !h.iter().all(u8::is_ascii_hexdigit) {
            return None;
        }
        u32::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok()
    }

    /// The rest of a string after its opening quote.
    fn string(&mut self) -> Result<String, String> {
        let mut out = String::new();
        loop {
            let Some(c) = self.s[self.i..].chars().next() else {
                return Err(self.err("Unterminated string starting at"));
            };
            match c {
                '"' => {
                    self.i += 1;
                    return Ok(out);
                }
                '\\' => {
                    let Some(&e) = self.b.get(self.i + 1) else {
                        return Err(self.err("Unterminated string starting at"));
                    };
                    self.i += 2;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\x08'),
                        b'f' => out.push('\x0c'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let Some(u) = self.hex4(self.i) else {
                                return Err(self.err("Invalid \\uXXXX escape"));
                            };
                            self.i += 4;
                            let mut cp = u;
                            if (0xd800..0xdc00).contains(&u) && self.b[self.i..].starts_with(b"\\u") {
                                if let Some(lo) = self.hex4(self.i + 2) {
                                    if (0xdc00..0xe000).contains(&lo) {
                                        cp = 0x10000 + ((u - 0xd800) << 10) + (lo - 0xdc00);
                                        self.i += 6;
                                    }
                                }
                            }
                            // A lone surrogate is a str to Python; a Rust
                            // String cannot hold one.
                            out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                        }
                        _ => return Err(self.err("Invalid \\escape")),
                    }
                }
                c if (c as u32) < 0x20 => return Err(self.err("Invalid control character at")),
                c => {
                    out.push(c);
                    self.i += c.len_utf8();
                }
            }
        }
    }

    fn object(&mut self) -> Result<Py, String> {
        let mut d: Vec<(String, Py)> = Vec::new();
        self.ws();
        if self.b.get(self.i) == Some(&b'}') {
            self.i += 1;
            return Ok(Py::Dict(d));
        }
        loop {
            if self.b.get(self.i) != Some(&b'"') {
                return Err(self.err("Expecting property name enclosed in double quotes"));
            }
            self.i += 1;
            let k = self.string()?;
            self.ws();
            if self.b.get(self.i) != Some(&b':') {
                return Err(self.err("Expecting ':' delimiter"));
            }
            self.i += 1;
            self.ws();
            let v = self.value()?;
            match d.iter_mut().find(|(dk, _)| *dk == k) {
                Some(e) => e.1 = v,
                None => d.push((k, v)),
            }
            self.ws();
            match self.b.get(self.i) {
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Py::Dict(d));
                }
                Some(b',') => {
                    self.i += 1;
                    self.ws();
                }
                _ => return Err(self.err("Expecting ',' delimiter")),
            }
        }
    }

    fn array(&mut self) -> Result<Py, String> {
        let mut l = Vec::new();
        self.ws();
        if self.b.get(self.i) == Some(&b']') {
            self.i += 1;
            return Ok(Py::List(l));
        }
        loop {
            l.push(self.value()?);
            self.ws();
            match self.b.get(self.i) {
                Some(b']') => {
                    self.i += 1;
                    return Ok(Py::List(l));
                }
                Some(b',') => {
                    self.i += 1;
                    self.ws();
                }
                _ => return Err(self.err("Expecting ',' delimiter")),
            }
        }
    }
}

fn gh() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(GH_PATTERN).expect("GH_PATTERN"))
}

fn x86() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(X86_PATTERN).expect("X86_PATTERN"))
}

fn other_arch() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(OTHER_ARCH_PATTERN).expect("OTHER_ARCH_PATTERN"))
}

fn hex64() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[0-9a-fA-F]{64}$").expect("hex64"))
}

/// `ghcache.DEFAULT_TTL`.
const GH_TTL: u64 = 6 * 3600;

type Fetch<'a> = &'a dyn Fn(&str) -> Result<Vec<u8>, String>;

// ---- the module --------------------------------------------------------------

/// `(owner, repo)` of a GitHub repository link, or `None`.
pub fn parse(link: &str) -> std::option::Option<(String, String)> {
    let m = gh().captures(py_strip(link))?;
    Some((m[1].to_string(), m[2].to_string()))
}

/// `_get`: GET `url` with the Almanac user agent, returning the body.
/// Raises (here: `Err`) on any network error.
///
/// `urllib.request.urlopen` also reads `data:` and `file:` URLs, so this
/// does too; `ftp:` is the one scheme urllib has that this does not.
pub fn get(url: &str, timeout: u64) -> Result<Vec<u8>, String> {
    // Request(url): unwrap('<URL:...>'), then the fragment is split off.
    let mut u = py_strip(url);
    if u.starts_with('<') && u.ends_with('>') && u.len() >= 2 {
        u = py_strip(&u[1..u.len() - 1]);
    }
    if let Some(rest) = u.strip_prefix("URL:") {
        u = py_strip(rest);
    }
    let (base, fragment) = match u.rfind('#') {
        Some(i) => (&u[..i], &u[i + 1..]),
        None => (u, ""),
    };
    let full_url = if fragment.is_empty() { base.to_string() } else { format!("{base}#{fragment}") };
    // _splittype: the scheme is everything before the first ':', no '/'.
    let Some((scheme, rest)) = base.split_once(':').filter(|(s, _)| !s.is_empty() && !s.contains('/')) else {
        return Err(format!("ValueError: unknown url type: {}", str_repr(&full_url)));
    };
    let scheme = scheme.to_lowercase();
    match scheme.as_str() {
        "http" | "https" => {
            // _splithost: '//host' up to the first '/', '?' or '#'; the rest
            // is the selector sent in the request line.
            let after = rest.strip_prefix("//").unwrap_or("");
            let end = after.find(['/', '?', '#']).unwrap_or(after.len());
            let (authority, selector) = (&after[..end], &after[end..]);
            if !rest.starts_with("//") || authority.is_empty() {
                return Err("URLError: <urlopen error no host given>".into());
            }
            http_get(&scheme, authority, selector, timeout)
        }
        "data" => data_url(&full_url),
        "file" => file_url(&full_url),
        _ => Err(format!("URLError: <urlopen error unknown url type: {scheme}>")),
    }
}

/// `http.client.HTTPConnection._get_hostport`: `host[:port]`, the port
/// `default` when it is not given; brackets come off an IPv6 address.
fn get_hostport(host: &str, default: u16) -> Result<(String, u16), String> {
    let mut h = host;
    let mut port = default;
    let i = host.rfind(':');
    let j = host.rfind(']');
    if let Some(i) = i.filter(|&i| j.is_none_or(|j| i > j)) {
        let p = &host[i + 1..];
        if !p.is_empty() {
            let n = py_int(&Py::Str(p.to_string())).map_err(|_| format!("InvalidURL: nonnumeric port: '{p}'"))?;
            port = n
                .to_i64()
                .and_then(|n| u16::try_from(n).ok())
                .ok_or_else(|| "OverflowError: getsockaddrarg: port must be 0-65535.".to_string())?;
        }
        h = &host[..i];
    }
    if h.len() >= 2 && h.starts_with('[') && h.ends_with(']') {
        h = &h[1..h.len() - 1];
    }
    Ok((h.to_string(), port))
}

/// `urllib.parse.unquote(s)`: `%XX` decoded as UTF-8, bad bytes replaced.
fn unquote(s: &str) -> String {
    String::from_utf8_lossy(&unquote_to_bytes(s)).into_owned()
}

/// `urllib.request.getproxies_environment()`: `<scheme>_proxy` variables,
/// any case, the all-lowercase spelling winning; an empty lowercase one
/// removes the scheme. Under CGI (`REQUEST_METHOD` set) an upper-case
/// `HTTP_PROXY` is ignored.
fn env_proxies() -> HashMap<String, String> {
    let env: Vec<(String, String)> =
        std::env::vars_os().filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?))).collect();
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
    if env.iter().any(|(k, _)| k == "REQUEST_METHOD") {
        proxies.remove("http");
    }
    for (name, value, proxy_name) in environment {
        if name.ends_with("_proxy") {
            if !value.is_empty() {
                proxies.insert(proxy_name, value.clone());
            } else {
                proxies.remove(&proxy_name);
            }
        }
    }
    proxies
}

/// `urllib.request.proxy_bypass_environment(host)`: `no_proxy` is `*` or
/// a comma-separated list of host names and DNS suffixes.
fn proxy_bypass(host: &str, proxies: &HashMap<String, String>) -> bool {
    let Some(no_proxy) = proxies.get("no") else { return false };
    if no_proxy == "*" {
        return true;
    }
    let host = host.to_lowercase();
    // _splitport: a trailing ':digits' comes off.
    let hostonly = match host.rfind(':') {
        Some(i) if host[i + 1..].bytes().all(|b| b.is_ascii_digit()) => &host[..i],
        _ => host.as_str(),
    };
    for name in no_proxy.split(',') {
        let name = py_strip(name);
        if name.is_empty() {
            continue;
        }
        let name = name.trim_start_matches('.').to_lowercase();
        if hostonly == name || host == name {
            return true;
        }
        let name = format!(".{name}");
        if hostonly.ends_with(&name) || host.ends_with(&name) {
            return true;
        }
    }
    false
}

/// `urllib.request._parse_proxy(proxy)`: (scheme, user, password, host:port).
#[allow(clippy::type_complexity)]
fn parse_proxy(
    proxy: &str,
) -> Result<(std::option::Option<String>, std::option::Option<String>, std::option::Option<String>, String), String> {
    // _splittype: '([^/:]+):(.*)'
    let (scheme, r_scheme) = match proxy.split_once(':') {
        Some((s, r)) if !s.is_empty() && !s.contains('/') => (Some(s.to_lowercase()), r),
        _ => (None, proxy),
    };
    let (scheme, authority) = if !r_scheme.starts_with('/') {
        (None, proxy)
    } else {
        if !r_scheme.starts_with("//") {
            return Err(format!("ValueError: proxy URL with no authority: {}", str_repr(proxy)));
        }
        let end = match r_scheme.find('@') {
            Some(at) => r_scheme[at..].find('/').map(|e| e + at),
            None => r_scheme[2..].find('/').map(|e| e + 2),
        };
        (scheme, &r_scheme[2..end.unwrap_or(r_scheme.len())])
    };
    // _splituser / _splitpasswd
    let (user, password, hostport) = match authority.rsplit_once('@') {
        Some((userinfo, hostport)) => match userinfo.split_once(':') {
            Some((u, p)) => (Some(u.to_string()), Some(p.to_string()), hostport),
            None => (Some(userinfo.to_string()), None, hostport),
        },
        None => (None, None, authority),
    };
    Ok((scheme, user, password, hostport.to_string()))
}

/// The proxy urllib's default `ProxyHandler` sends this request through:
/// the `<scheme>_proxy` of the URL's own scheme (`all_proxy` is not read),
/// unless `no_proxy` names the host. An https URL is tunnelled with CONNECT
/// through the proxy's host and port whatever scheme the proxy names; an
/// http URL goes to an http (or https) proxy, and fails for any other.
///
/// ureq tunnels with CONNECT through an http proxy for an http URL too,
/// where urllib sends the request to the proxy in absolute form; the proxy
/// and the server reached are the same. ureq sends proxy credentials as
/// written in the variable, where urllib percent-decodes them first.
fn proxy_for(scheme: &str, host: &str) -> Result<std::option::Option<ureq::Proxy>, String> {
    let proxies = env_proxies();
    let Some(proxy) = proxies.get(scheme) else { return Ok(None) };
    let (proxy_type, user, password, hostport) = parse_proxy(proxy)?;
    let proxy_type = proxy_type.unwrap_or_else(|| scheme.to_string());
    if !host.is_empty() && proxy_bypass(host, &proxies) {
        return Ok(None);
    }
    let hostport = unquote(&hostport);
    let (protocol, default_port) = if scheme == "https" || proxy_type == "http" {
        (ureq::ProxyProtocol::Http, if scheme == "https" { 443 } else { 80 })
    } else if proxy_type == "https" {
        (ureq::ProxyProtocol::Https, 443)
    } else {
        return Err(format!("URLError: <urlopen error unknown url type: {proxy_type}>"));
    };
    let (h, port) = get_hostport(&hostport, default_port)?;
    let h = if h.contains(':') { format!("[{h}]") } else { h };
    let mut b = ureq::Proxy::builder(protocol).host(&h).port(port);
    if let (Some(u), Some(p)) = (user.filter(|u| !u.is_empty()), password.filter(|p| !p.is_empty())) {
        b = b.username(&u).password(&p);
    }
    b.build().map(Some).map_err(|e| format!("URLError: <urlopen error {e}>"))
}

/// The CA certificates Python's `ssl.create_default_context()` trusts:
/// OpenSSL's default verify paths, `SSL_CERT_FILE` (else `/etc/ssl/cert.pem`)
/// and the hashed `<subject hash>.<n>` files of each `SSL_CERT_DIR` entry
/// (else `/etc/ssl/certs`). Read once per pair of settings.
fn system_roots() -> ureq::tls::RootCerts {
    type Key = (std::option::Option<std::ffi::OsString>, std::option::Option<std::ffi::OsString>);
    static CACHE: std::sync::Mutex<std::option::Option<(Key, ureq::tls::RootCerts)>> = std::sync::Mutex::new(None);
    let key: Key = (std::env::var_os("SSL_CERT_FILE"), std::env::var_os("SSL_CERT_DIR"));
    if let Ok(cache) = CACHE.lock() {
        if let Some((k, roots)) = cache.as_ref() {
            if *k == key {
                return roots.clone();
            }
        }
    }
    let mut certs: Vec<ureq::tls::Certificate<'static>> = Vec::new();
    let mut add_pem = |path: &Path| {
        if let Ok(pem) = std::fs::read(path) {
            for item in ureq::tls::parse_pem(&pem).flatten() {
                if let ureq::tls::PemItem::Certificate(c) = item {
                    certs.push(c);
                }
            }
        }
    };
    let file = key.0.clone().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/etc/ssl/cert.pem"));
    add_pem(&file);
    static HASHED: OnceLock<Regex> = OnceLock::new();
    let hashed = HASHED.get_or_init(|| Regex::new(r"^[0-9a-f]{8}\.[0-9]+$").expect("hashed"));
    let dirs = key.1.clone().unwrap_or_else(|| "/etc/ssl/certs".into());
    for dir in std::env::split_paths(&dirs) {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let mut names: Vec<PathBuf> = rd
            .flatten()
            .filter(|e| e.file_name().to_str().is_some_and(|n| hashed.is_match(n)))
            .map(|e| e.path())
            .collect();
        names.sort();
        for p in names {
            add_pem(&p);
        }
    }
    let roots = ureq::tls::RootCerts::new_with_certs(&certs);
    if let Ok(mut cache) = CACHE.lock() {
        *cache = Some((key, roots.clone()));
    }
    roots
}

/// `http_open`/`https_open`. urllib's timeout bounds the connect and every
/// read from the socket, so a body that stalls fails after `timeout` seconds
/// while a slow but steady one does not. ureq bounds the connect and the wait
/// for the headers; the body is read on its own thread, each read waited for
/// at most `timeout`.
///
/// urllib sends the selector as it is; the `http` crate under ureq refuses
/// `<`, `>` and `` ` `` (and `#`, which only survives here when the URL had
/// two), so those go percent-encoded, which servers read as the same path.
/// A host with userinfo (`u:p@host`) is to urllib a host name, which does
/// not resolve; ureq would strip it and send Basic auth.
fn http_get(scheme: &str, authority: &str, selector: &str, timeout: u64) -> Result<Vec<u8>, String> {
    let host = unquote(authority);
    // _validate_host / _validate_path, then the request line in ASCII.
    let bad = |s: &str| s.chars().find(|&c| (c as u32) <= 0x20 || c == '\x7f');
    if let Some(c) = bad(&host) {
        return Err(format!(
            "InvalidURL: URL can't contain control characters. {} (found at least {})",
            str_repr(&host),
            str_repr(&c.to_string())
        ));
    }
    let selector = if selector.is_empty() { "/" } else { selector };
    if let Some(c) = bad(selector) {
        return Err(format!(
            "InvalidURL: URL can't contain control characters. {} (found at least {})",
            str_repr(selector),
            str_repr(&c.to_string())
        ));
    }
    if let Some((pos, c)) = selector.chars().enumerate().find(|(_, c)| !c.is_ascii()) {
        return Err(format!(
            "UnicodeEncodeError: 'ascii' codec can't encode character {} in position {}: ordinal not in range(128)",
            str_repr(&c.to_string()),
            pos + 4
        ));
    }
    let proxy = proxy_for(scheme, &host)?;
    if proxy.is_none() && host.contains('@') {
        use std::net::ToSocketAddrs;
        let (h, port) = get_hostport(&host, if scheme == "https" { 443 } else { 80 })?;
        if let Err(e) = (h.as_str(), port).to_socket_addrs() {
            let msg = e.to_string();
            let msg = msg.strip_prefix("failed to lookup address information: ").unwrap_or(&msg).to_string();
            let errno = if msg.contains("Temporary failure") { -3 } else { -2 };
            return Err(format!("URLError: <urlopen error [Errno {errno}] {msg}>"));
        }
    }
    let mut sel = String::with_capacity(selector.len());
    for c in selector.chars() {
        match c {
            '<' => sel.push_str("%3C"),
            '>' => sel.push_str("%3E"),
            '`' => sel.push_str("%60"),
            '#' => sel.push_str("%23"),
            c => sel.push(c),
        }
    }
    let url = format!("{scheme}://{authority}{sel}");
    let t = std::time::Duration::from_secs(timeout);
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(t))
        .timeout_send_request(Some(t))
        .timeout_recv_response(Some(t))
        .proxy(proxy)
        .tls_config(ureq::tls::TlsConfig::builder().root_certs(system_roots()).build())
        .build()
        .into();
    let r = agent.get(&url).header(UA.0, UA.1).call().map_err(|e| match e {
        ureq::Error::StatusCode(code) => {
            let reason = ureq::http::StatusCode::from_u16(code).ok().and_then(|s| s.canonical_reason()).unwrap_or("");
            format!("HTTPError: HTTP Error {code}: {reason}")
        }
        ureq::Error::Timeout(_) => "URLError: <urlopen error timed out>".into(),
        other => format!("URLError: <urlopen error {other}>"),
    })?;
    let mut reader = r.into_body().into_reader();
    let (tx, rx) = std::sync::mpsc::channel::<Result<std::option::Option<Vec<u8>>, String>>();
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let msg = match reader.read(&mut buf) {
                Ok(0) => Ok(None),
                Ok(n) => Ok(Some(buf[..n].to_vec())),
                Err(e) => Err(e.to_string()),
            };
            let last = !matches!(msg, Ok(Some(_)));
            if tx.send(msg).is_err() || last {
                break;
            }
        }
    });
    let mut out = Vec::new();
    loop {
        match rx.recv_timeout(t) {
            Ok(Ok(Some(chunk))) => out.extend_from_slice(&chunk),
            Ok(Ok(None)) => return Ok(out),
            Ok(Err(e)) => return Err(format!("OSError: {e}")),
            Err(_) => return Err("TimeoutError: timed out".into()),
        }
    }
}

/// `DataHandler.data_open`: RFC 2397, `data:[mediatype][;base64],data`.
fn data_url(full_url: &str) -> Result<Vec<u8>, String> {
    let (_, data) = full_url.split_once(':').unwrap_or((full_url, ""));
    let Some((mediatype, data)) = data.split_once(',') else {
        return Err("ValueError: not enough values to unpack (expected 2, got 1)".into());
    };
    if mediatype.chars().any(|c| (c as u32) < 0x20 || c == '\x7f') {
        return Err("ValueError: Control characters not allowed in data: mediatype".into());
    }
    // even base64 encoded data URLs might be quoted so unquote in any case:
    let bytes = unquote_to_bytes(data);
    if mediatype.ends_with(";base64") {
        return b64decode(&bytes);
    }
    Ok(bytes)
}

/// `FileHandler.open_local_file`: `url2pathname(full_url)` read from disk,
/// for a URL with no host, `localhost`, or a name of this machine.
fn file_url(full_url: &str) -> Result<Vec<u8>, String> {
    // urlsplit: tabs and newlines removed, query and fragment discarded.
    let u: String = full_url.chars().filter(|c| !matches!(c, '\t' | '\r' | '\n')).collect();
    let rest = u.split_once(':').map(|(_, r)| r).unwrap_or("");
    let (authority, path) = match rest.strip_prefix("//") {
        Some(r) => {
            let end = r.find(['/', '?', '#']).unwrap_or(r.len());
            (&r[..end], &r[end..])
        }
        None => ("", rest),
    };
    let path = &path[..path.find(['?', '#']).unwrap_or(path.len())];
    if !is_local_authority(authority) {
        return Err("URLError: <urlopen error file:// scheme is supported only on localhost>".into());
    }
    use std::os::unix::ffi::OsStringExt;
    let local = PathBuf::from(std::ffi::OsString::from_vec(unquote_to_bytes(path)));
    std::fs::read(&local).map_err(|e| format!("URLError: <urlopen error {e}: {}>", str_repr(&local.to_string_lossy())))
}

/// `urllib.request._is_local_authority(authority, resolve=True)`.
fn is_local_authority(authority: &str) -> bool {
    use std::net::ToSocketAddrs;
    if authority.is_empty() || authority == "localhost" {
        return true;
    }
    let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname").map(|h| h.trim().to_string()).unwrap_or_default();
    if !hostname.is_empty() && authority == hostname {
        return true;
    }
    // gethostbyname: the first IPv4 address.
    let v4 = |h: &str| -> Vec<std::net::IpAddr> {
        (h, 0).to_socket_addrs().map(|a| a.map(|s| s.ip()).filter(|ip| ip.is_ipv4()).collect()).unwrap_or_default()
    };
    let Some(addr) = v4(authority).into_iter().next() else { return false };
    let mut names = v4("localhost");
    if !hostname.is_empty() {
        names.extend(v4(&hostname));
    }
    names.contains(&addr)
}

/// `_official`: the official repository package of this name, if pacman
/// knows one.
pub fn official(name: &str) -> std::option::Option<Option> {
    let out = crate::util::run(&["env", "LC_ALL=C", "pacman", "-Si", name], 20);
    if out.failed_to_start || out.timed_out {
        return None;
    }
    // dict(l.split(":", 1) for ...): first appearance fixes the order, the
    // last appearance the value; then the keys and values are stripped.
    let mut raw: Vec<(&str, &str)> = Vec::new();
    for l in py_splitlines(&out.stdout) {
        if !l.contains(':') || l.starts_with(' ') {
            continue;
        }
        let (k, v) = l.split_once(':').unwrap_or((l, ""));
        match raw.iter_mut().find(|(rk, _)| *rk == k) {
            Some(e) => e.1 = v,
            None => raw.push((k, v)),
        }
    }
    let mut fields: HashMap<&str, &str> = HashMap::new();
    for (k, v) in raw {
        fields.insert(py_strip(k), py_strip(v));
    }
    let version = fields.get("Version").copied().unwrap_or("");
    if version.is_empty() {
        return None;
    }
    Some(Option {
        kind: REPO.into(),
        name: name.into(),
        version: version.into(),
        title: format!("Official package ({})", fields.get("Repository").copied().unwrap_or("repo")),
        reasons: vec!["Built and signed by your distribution".into(), "Updated with the rest of the system".into()],
        rank: 0,
        ..Default::default()
    })
}

/// `_norm`: a URL lowercased, without scheme, `www.`, trailing `/` or `.git`,
/// so two spellings of one repository compare equal.
pub fn norm(url: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^https?://(www\.)?").expect("norm"));
    let lower = url.to_lowercase();
    let u = py_strip(&lower).trim_end_matches('/');
    let u = u.strip_suffix(".git").unwrap_or(u);
    re.replace(u, "").into_owned()
}

/// `_norm` of any value: `(url or "").lower()` needs a `str`.
fn norm_py(url: &Py) -> Result<String, String> {
    Ok(norm(or(Some(url), &EMPTY_STR).as_str("lower")?))
}

/// `_aur`: AUR packages by these names. `homes` are the URLs that count as this
/// project: its repository, and the homepage the repository itself names.
/// Raises (here: `Err`) on an answer that is not the AUR's shape.
pub fn aur(names: &[&str], homes: &[&str]) -> Result<Vec<Option>, String> {
    let homes: Vec<Py> = homes.iter().map(|h| Py::Str(h.to_string())).collect();
    aur_with(names, &homes, &|u: &str| get(u, 20))
}

fn aur_with(names: &[&str], homes: &[Py], fetch: Fetch) -> Result<Vec<Option>, String> {
    let query: Vec<String> = names.iter().map(|n| format!("arg[]={}", quote(n))).collect();
    let data = match fetch(&format!("https://aur.archlinux.org/rpc/v5/info?{}", query.join("&"))).and_then(|b| loads(&b)) {
        Ok(d) => d,
        Err(_) => return Ok(Vec::new()),
    };
    aur_records(&data, homes)
}

/// The decision part of `_aur`, on the parsed RPC answer. `Err` wherever the
/// Python would raise.
fn aur_records(data: &Py, homes: &[Py]) -> Result<Vec<Option>, String> {
    let mut out = Vec::new();
    let mut want: std::collections::HashSet<String> = std::collections::HashSet::new();
    for h in homes.iter().filter(|h| h.truthy()) {
        want.insert(norm_py(h)?);
    }
    for rec in data.get("results")?.unwrap_or(&EMPTY_LIST).iter()? {
        let name = rec.get("Name")?.unwrap_or(&EMPTY_STR);
        let votes = py_int(or(rec.get("NumVotes")?, &Py::Int(BigInt::from_i64(0))))?;
        let url = norm_py(or(rec.get("URL")?, &EMPTY_STR))?;
        let name = name.as_str("endswith")?;
        let binary = name.ends_with("-bin") || name.ends_with("-appimage");
        let mut reasons: Vec<String> = Vec::new();
        let mut warnings: Vec<String> = Vec::new();
        let same = want.contains(&url);
        if same {
            reasons.push("Points at this project".into());
        } else {
            let stated = Py::Str("not stated".into());
            warnings.push(format!("Its upstream is {}, which is not this project", or(rec.get("URL")?, &stated).py_str()));
        }
        reasons.push(format!("{votes} vote(s) on the AUR"));
        let maintained = rec.get("Maintainer")?.is_some_and(Py::truthy);
        let out_of_date = rec.get("OutOfDate")?.is_some_and(Py::truthy);
        if !maintained {
            warnings.push("Orphaned: nobody maintains it".into());
        }
        if out_of_date {
            warnings.push("Flagged out of date".into());
        }
        if binary {
            warnings.push("Repackages a prebuilt binary; nothing is built from the source you can read".into());
        } else if name.ends_with("-git") {
            warnings.push("Builds the newest commit, not a release".into());
        } else {
            reasons.push("Builds from the published source".into());
        }
        let mut rank = 10;
        if !same {
            rank += 40;
        }
        if binary {
            rank += 10;
        }
        if name.ends_with("-git") {
            rank += 5;
        }
        if votes.lt(5) {
            rank += 15;
        }
        if !maintained || out_of_date {
            rank += 20;
        }
        // rec.get("Version", ""): whatever the RPC sent.
        let version = rec.get("Version")?.cloned().unwrap_or_else(|| EMPTY_STR.clone());
        let mut opt = Option {
            kind: AUR.into(),
            name: name.into(),
            version: version.shown(),
            title: "AUR package".into(),
            reasons,
            warnings,
            rank,
            ..Default::default()
        };
        if !matches!(version, Py::Str(_)) {
            opt.py.insert("version", version);
        }
        out.push(opt);
    }
    Ok(out)
}

/// `_sums`: {file name: sha256} from any checksum file in the release.
///
/// `assets` are the GitHub release's `assets` objects, as returned. Raises
/// (here: `Err`) on assets that are not GitHub's shape.
pub fn sums(assets: &[Value]) -> Result<BTreeMap<String, String>, String> {
    let assets: Vec<Py> = assets.iter().map(Py::from_value).collect();
    sums_with(&assets, &|u: &str| get(u, 20))
}

/// `x < 65536` for a JSON value.
fn lt_65536(v: &Py) -> Result<bool, String> {
    match v {
        Py::Bool(b) => Ok({ let _ = b; true }),
        Py::Int(i) => Ok(i.lt(65536)),
        Py::Float(f) => Ok(*f < 65536.0),
        other => Err(format!("TypeError: '<' not supported between instances of '{}' and 'int'", other.type_name())),
    }
}

fn sums_with(assets: &[Py], fetch: Fetch) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    for a in assets {
        let nm = a.get("name")?.unwrap_or(&EMPTY_STR).as_str("lower")?.to_lowercase();
        if !(nm.contains("sha256") || nm.contains("checksum")) {
            continue;
        }
        if !lt_65536(a.get("size")?.unwrap_or(&Py::Int(BigInt::from_i64(0))))? {
            continue;
        }
        // a["browser_download_url"] and _get are inside the try: a missing or
        // unusable URL is skipped. (str() of anything but a str is never a
        // URL urllib can open.)
        let body = match a.get("browser_download_url")? {
            Some(Py::Str(u)) => fetch(u),
            _ => continue,
        };
        let Ok(body) = body else { continue };
        let text = String::from_utf8_lossy(&body);
        for line in py_splitlines(&text) {
            let parts = py_split(line);
            if parts.len() >= 2 && hex64().is_match(parts[0]) {
                out.insert(parts[parts.len() - 1].trim_start_matches('*').to_string(), parts[0].to_lowercase());
            } else if parts.len() == 1 && hex64().is_match(parts[0]) && nm.ends_with(".sha256") {
                // a["name"][: -len(".sha256")], on the original spelling
                let name = a.get("name")?.unwrap_or(&EMPTY_STR).as_str("lower")?;
                let cut: String = {
                    let chars: Vec<char> = name.chars().collect();
                    chars[..chars.len().saturating_sub(7)].iter().collect()
                };
                out.insert(cut, parts[0].to_lowercase());
            }
        }
    }
    Ok(out)
}

/// `_appimage`: the project's own Linux x86-64 AppImage from a GitHub release
/// object, with its checksum when one is published. Raises (here: `Err`) on
/// a release that is not GitHub's shape.
pub fn appimage(release: &Value) -> Result<std::option::Option<Option>, String> {
    appimage_with(&Py::from_value(release), &|u: &str| get(u, 20))
}

fn appimage_with(release: &Py, fetch: Fetch) -> Result<std::option::Option<Option>, String> {
    let assets = or(release.get("assets")?, &EMPTY_LIST).iter()?;
    let mut images: Vec<&Py> = Vec::new();
    for a in &assets {
        let name = a.get("name")?.unwrap_or(&EMPTY_STR).as_str("endswith")?;
        if name.ends_with(".AppImage") && !other_arch().is_match(name) {
            images.push(a);
        }
    }
    if images.is_empty() {
        return Ok(None);
    }
    // An explicitly x86_64 build beats one that does not say.
    let name_of = |a: &Py| -> String {
        match a.get("name") {
            Ok(Some(Py::Str(s))) => s.clone(),
            _ => String::new(),
        }
    };
    images.sort_by_key(|a| if x86().is_match(&name_of(a)) { 0 } else { 1 });
    let a = images[0];
    let a_name = name_of(a);
    let mut sha = String::new();
    let digest = or(a.get("digest")?, &EMPTY_STR).as_str("startswith")?;
    if let Some(d) = digest.strip_prefix("sha256:") {
        sha = d.to_string();
    }
    if sha.is_empty() {
        sha = sums_with(&assets, fetch)?.get(&a_name).cloned().unwrap_or_default();
    }
    let tag = or(release.get("tag_name")?, &EMPTY_STR).as_str("lstrip")?.trim_start_matches('v').to_string();
    let mut reasons: Vec<String> = vec![
        "Published by the project itself, on its releases page".into(),
        "Runs without installing anything system-wide".into(),
    ];
    let mut warnings: Vec<String> = Vec::new();
    if !sha.is_empty() {
        reasons.push("Checksum verified before it is made runnable".into());
    } else {
        warnings.push("No checksum published to verify the download".into());
    }
    let size = match or(a.get("size")?, &Py::Int(BigInt::from_i64(0))) {
        Py::Int(i) => {
            let f = i.to_f64();
            if f.is_infinite() {
                return Err("OverflowError: integer division result too large for a float".into());
            }
            f
        }
        Py::Float(f) => *f,
        Py::Bool(b) => *b as i64 as f64,
        other => {
            return Err(format!("TypeError: unsupported operand type(s) for /: '{}' and 'int'", other.type_name()))
        }
    };
    // a.get("browser_download_url", ""): whatever the release held.
    let url = a.get("browser_download_url")?.cloned().unwrap_or_else(|| EMPTY_STR.clone());
    let mut opt = Option {
        kind: APPIMAGE.into(),
        name: a_name,
        version: tag,
        title: "AppImage".into(),
        reasons,
        warnings,
        rank: if !sha.is_empty() { 5 } else { 30 },
        url: url.shown(),
        sha256: sha,
        size_mb: crate::util::py_round_to(size / 1048576.0, 1),
        ..Default::default()
    };
    if !matches!(url, Py::Str(_)) {
        opt.py.insert("url", url);
    }
    Ok(Some(opt))
}

/// Look a link up and lay out every way to install it, best first.
///
/// The Python raises out of here on a GitHub or AUR answer of the wrong
/// shape: the lookup thread dies (its traceback on stderr) and the UI is
/// left at "Looking it up…". The UI runs this through `shell::spawn_bg`,
/// whose `done` is not called when the work panics, so the exception is a
/// panic here, with the same effect. [`try_resolve`] returns it as `Err`.
pub fn resolve(link: &str) -> Resolution {
    match try_resolve(link) {
        Ok(res) => res,
        Err(e) => panic!("{e}"),
    }
}

/// `resolve`, raising (here: `Err`) where the Python raises.
pub fn try_resolve(link: &str) -> Result<Resolution, String> {
    // What ghcache.get_json raises (a cache or rate file of the wrong
    // shape) raises out of resolve too.
    let repo_fn = |r: &str| crate::ghcache::try_repo(r, GH_TTL);
    let release_fn = |r: &str| crate::ghcache::try_latest_release(r, GH_TTL);
    resolve_with(link, &repo_fn, &release_fn, &|u: &str| get(u, 20))
}

fn resolve_with(
    link: &str,
    repo_fn: &dyn Fn(&str) -> Result<std::option::Option<Value>, String>,
    release_fn: &dyn Fn(&str) -> Result<std::option::Option<Value>, String>,
    fetch: Fetch,
) -> Result<Resolution, String> {
    let Some((owner, repo)) = parse(link) else {
        return Ok(Resolution { error: "That is not a GitHub repository link.".into(), ..Default::default() });
    };
    let meta = repo_fn(&format!("{owner}/{repo}"))?.map(|v| Py::from_value(&v)).unwrap_or(Py::None);
    let meta = or(Some(&meta), &EMPTY_DICT).clone();
    let has_name = match &meta {
        Py::Dict(_) => meta.get("name")?.is_some_and(Py::truthy),
        _ => false,
    };
    if !has_name {
        return Ok(Resolution {
            owner,
            repo,
            error: "GitHub did not return that repository. Check the link, or try again in a minute.".into(),
            ..Default::default()
        });
    }
    // owner = meta.get("owner", {}).get("login") or owner
    let login = meta.get("owner")?.unwrap_or(&EMPTY_DICT).get("login")?;
    let owner_py = or(login, &Py::Str(owner)).clone();
    let repo_py = meta.get("name")?.cloned().unwrap_or(Py::None);
    let description = or(meta.get("description")?, &EMPTY_STR).clone();
    let stars = py_int(or(meta.get("stargazers_count")?, &Py::Int(BigInt::from_i64(0))))?;
    let license = or(or(meta.get("license")?, &EMPTY_DICT).get("spdx_id")?, &EMPTY_STR).clone();
    let archived = meta.get("archived")?.is_some_and(Py::truthy);
    // name = repo.lower()
    let repo = repo_py.as_str("lower")?.to_string();
    let mut res = Resolution {
        owner: owner_py.shown(),
        repo: repo.clone(),
        description: description.shown(),
        stars: stars.to_i64_sat(),
        license: license.shown(),
        archived,
        ..Default::default()
    };
    for (field, v) in [("owner", &owner_py), ("description", &description), ("license", &license)] {
        if !matches!(v, Py::Str(_)) {
            res.py.insert(field, v.clone());
        }
    }
    if stars.to_i64().is_none() {
        res.py.insert("stars", Py::Int(stars));
    }

    let name = repo.to_lowercase();
    let mut opts = Vec::new();
    if let Some(o) = official(&name) {
        opts.push(o);
    }
    let release = release_fn(&format!("{}/{repo}", res.owner))?;
    if let Some(release @ Value::Object(_)) = &release {
        if let Some(o) = appimage_with(&Py::from_value(release), fetch)? {
            opts.push(o);
        }
    }
    // [res.url, meta.get("homepage") or ""]: _aur only reads them once the
    // AUR has answered.
    let homes = [Py::Str(res.url()), or(meta.get("homepage")?, &EMPTY_STR).clone()];
    let (bin, git, app) = (format!("{name}-bin"), format!("{name}-git"), format!("{name}-appimage"));
    opts.extend(aur_with(&[name.as_str(), &bin, &git, &app], &homes, fetch)?);
    if res.archived {
        for o in &mut opts {
            o.warnings.push("The repository is archived: no further updates".into());
        }
    }
    opts.sort_by_key(|o| o.rank);
    res.options = opts;
    if res.options.is_empty() {
        res.error = "No installable build found: no official package, no Linux AppImage in the latest release, and nothing on the AUR under that name.".into();
    }
    Ok(res)
}

/// `str(exc)` of an error this crate spells "Type: message": the message
/// alone, which is what the task log prints (`failed: {exc}`). An io error's
/// "text (os error N)" becomes OSError's "[Errno N] text".
fn exc_str(e: &str) -> String {
    let msg = match e.split_once(": ") {
        Some((t, m)) if t.ends_with("Error") && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.') => m,
        _ => e,
    };
    static OS: OnceLock<Regex> = OnceLock::new();
    let os = OS.get_or_init(|| Regex::new(r"^(.*) \(os error (\d+)\)$").expect("os error"));
    match os.captures(msg) {
        Some(c) => format!("[Errno {}] {}", &c[2], &c[1]),
        None => msg.to_string(),
    }
}

/// `str(OSError)` for a failed call on `path`: "[Errno N] text: 'path'".
fn os_error(e: &std::io::Error, path: &Path) -> String {
    let text = exc_str(&e.to_string());
    format!("{text}: {}", str_repr(&path.to_string_lossy()))
}

/// `str(OSError)` for a failed `src.replace(dst)`: "... 'src' -> 'dst'".
fn os_error2(e: &std::io::Error, src: &Path, dst: &Path) -> String {
    format!("{} -> {}", os_error(e, src), str_repr(&dst.to_string_lossy()))
}

/// `inventory._load_registry()`, as Python reads it: the file's text through
/// `json.loads` (NaN, Infinity, integers of any size, keys in file order),
/// `{}` when it is missing or not JSON. Whatever JSON it holds is returned,
/// a list or a number included. `Err` only for what the Python does not
/// catch (a RecursionError on absurd nesting).
fn load_registry_py() -> Result<Py, String> {
    let Ok(raw) = std::fs::read(crate::inventory::registry()) else { return Ok(Py::Dict(Vec::new())) };
    // read_text(): strict UTF-8 (a UnicodeDecodeError is a ValueError).
    let Ok(text) = String::from_utf8(raw) else { return Ok(Py::Dict(Vec::new())) };
    // json.loads(str) refuses a BOM ("Unexpected UTF-8 BOM").
    if text.starts_with('\u{feff}') {
        return Ok(Py::Dict(Vec::new()));
    }
    match loads(text.as_bytes()) {
        Ok(v) => Ok(v),
        Err(e) if e.starts_with("RecursionError") => Err(e),
        Err(_) => Ok(Py::Dict(Vec::new())),
    }
}

/// `json.dumps(value, indent=indent)` of a Python value: ensure_ascii,
/// `NaN`/`Infinity` allowed, floats by `repr`, ints in full.
fn py_dumps(v: &Py, indent: usize, level: usize, out: &mut String) {
    let nl = |out: &mut String, lvl: usize| {
        out.push('\n');
        out.push_str(&" ".repeat(indent * lvl));
    };
    match v {
        Py::None => out.push_str("null"),
        Py::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Py::Int(i) => out.push_str(&i.to_string()),
        Py::Float(f) => out.push_str(&crate::util::py_float_repr(*f)),
        Py::Str(t) => out.push_str(&crate::util::py_json_string(t)),
        Py::List(l) if l.is_empty() => out.push_str("[]"),
        Py::List(l) => {
            out.push('[');
            for (i, x) in l.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                nl(out, level + 1);
                py_dumps(x, indent, level + 1, out);
            }
            nl(out, level);
            out.push(']');
        }
        Py::Dict(d) if d.is_empty() => out.push_str("{}"),
        Py::Dict(d) => {
            out.push('{');
            for (i, (k, x)) in d.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                nl(out, level + 1);
                out.push_str(&crate::util::py_json_string(k));
                out.push_str(": ");
                py_dumps(x, indent, level + 1, out);
            }
            nl(out, level);
            out.push('}');
        }
    }
}

/// `inventory.save_registry(data)`: indent=2 and a trailing newline,
/// written to a temp file and moved into place, so a crash mid-write cannot
/// truncate it. Errors are `str(OSError)`.
fn save_registry_py(data: &Py) -> Result<(), String> {
    let reg = crate::inventory::registry();
    if let Some(parent) = reg.parent() {
        std::fs::create_dir_all(parent).map_err(|e| os_error(&e, parent))?;
    }
    let tmp = reg.with_extension("tmp");
    let mut text = String::new();
    py_dumps(data, 2, 0, &mut text);
    text.push('\n');
    std::fs::write(&tmp, text).map_err(|e| os_error(&e, &tmp))?;
    std::fs::rename(&tmp, &reg).map_err(|e| os_error2(&e, &tmp, &reg)) // atomic: a crash mid-write cannot truncate it
}

/// Download, verify, register and add a launcher. Raises on failure.
///
/// `log` receives one line of progress at a time (the task runner's log).
/// The error is the message as Python's `str(exc)` reads, which the task
/// log prints as `failed: <message>`, with no type name in front.
///
/// The registry is read and rewritten as Python's json reads and writes
/// it, so entries this install does not touch keep their values and their
/// spelling (NaN, integers past 64 bits); a registry that is JSON but not
/// an object fails the install (the TypeError of `reg[key] = ...`) and is
/// left as it was. The new entry records GitHub's values as they came
/// (a description that is not a string stays one).
pub fn install_appimage(res: &Resolution, opt: &Option, log: &dyn Fn(&str)) -> Result<PathBuf, String> {
    static KEY: OnceLock<Regex> = OnceLock::new();
    let key_re = KEY.get_or_init(|| Regex::new(r"[^a-z0-9._-]").expect("key"));

    let key = key_re.replace_all(&res.repo.to_lowercase(), "-").into_owned();
    let registry = crate::inventory::registry();
    let dest = registry.parent().unwrap_or(Path::new("")).join("apps").join(format!("{key}.AppImage"));
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| os_error(&e, parent))?;
    }
    let partial = dest.with_extension("unverified");
    log(&format!("{}  ({:.0} MB)", opt.name, opt.size_mb));
    crate::appimage::download(&opt.url, &partial, None).map_err(|e| exc_str(&e))?;
    if !opt.sha256.is_empty() {
        let mut h = Sha256::new();
        let mut fh = std::fs::File::open(&partial).map_err(|e| os_error(&e, &partial))?;
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = fh.read(&mut buf).map_err(|e| exc_str(&e.to_string()))?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
        }
        if hex::encode(h.finalize()) != opt.sha256 {
            let _ = std::fs::remove_file(&partial);
            return Err("checksum mismatch: the download is not the file the project published, so it was deleted".into());
        }
        log("checksum verified");
    }
    std::fs::set_permissions(&partial, std::fs::Permissions::from_mode(0o755)).map_err(|e| os_error(&e, &partial))?;
    std::fs::rename(&partial, &dest).map_err(|e| os_error2(&e, &partial, &dest))?;

    // The values as Python holds them: what GitHub sent, kept in `py` for a
    // field that was not a string.
    let raw = |py: &BTreeMap<&'static str, Py>, f: &str, shown: &str| py.get(f).cloned().unwrap_or_else(|| Py::Str(shown.to_string()));
    let version = raw(&opt.py, "version", &opt.version);
    // Track by the asset's naming pattern with the version wildcarded, so the
    // existing update check finds the next release.
    let pattern = match &version {
        v if !v.truthy() => opt.name.clone(),
        Py::Str(v) => opt.name.replace(v.as_str(), "*"),
        other => return Err(format!("replace() argument 1 must be str, not {}", other.type_name())),
    };
    let mut reg = load_registry_py().map_err(|e| exc_str(&e))?;
    let entry = Py::Dict(vec![
        ("name".into(), Py::Str(key.clone())),
        ("path".into(), Py::Str(dest.to_string_lossy().into_owned())),
        ("version".into(), version.clone()),
        ("description".into(), raw(&res.py, "description", &res.description)),
        ("url".into(), Py::Str(res.url())),
        ("latest".into(), Py::Dict(Vec::new())),
        ("checked_at".into(), Py::Int(BigInt::from_i64(0))),
        (
            "update_spec".into(),
            Py::Dict(vec![
                ("kind".into(), Py::Str("github".into())),
                ("owner".into(), raw(&res.py, "owner", &res.owner)),
                ("repo".into(), Py::Str(res.repo.clone())),
                ("release".into(), Py::Str("latest".into())),
                ("pattern".into(), Py::Str(pattern)),
            ]),
        ),
    ]);
    // reg[key] = {...}: a key already there keeps its place.
    match &mut reg {
        Py::Dict(d) => match d.iter_mut().find(|(k, _)| *k == key) {
            Some(e) => e.1 = entry,
            None => d.push((key.clone(), entry)),
        },
        Py::List(_) => return Err("list indices must be integers or slices, not str".into()),
        other => return Err(format!("'{}' object does not support item assignment", other.type_name())),
    }
    save_registry_py(&reg)?;
    if crate::appimage::install_desktop_entry(&dest, &key).map_err(|e| exc_str(&e))? {
        log("launcher entry added");
    }
    crate::history::record_checked(
        crate::history::ALMANAC,
        &format!("Installed {} {}", res.repo, version.py_str()),
        &format!("AppImage from {}{}", res.url(), if !opt.sha256.is_empty() { " · checksum verified" } else { "" }),
        "",
        &[],
    )
    .map_err(|e| exc_str(&e))?;
    Ok(dest)
}

// ---- parity ------------------------------------------------------------------

/// A fake `_get` for parity: `responses` maps a URL to its body, or to null
/// for a URL that fails.
fn fake_fetch(responses: &Value) -> impl Fn(&str) -> Result<Vec<u8>, String> + '_ {
    move |u: &str| match responses.get(u) {
        Some(Value::String(s)) => Ok(s.as_bytes().to_vec()),
        _ => Err(format!("OSError: no response for {u}")),
    }
}

fn opt_json(o: std::option::Option<Option>) -> Value {
    o.map(|o| serde_json::to_value(o).unwrap_or_default()).unwrap_or(Value::Null)
}

fn res_json(r: Result<Value, String>) -> Value {
    r.unwrap_or_else(|e| json!({ "error": e }))
}

/// Every file under `d`, relative, sorted.
fn tree(d: &Path) -> Vec<String> {
    fn walk(root: &Path, d: &Path, out: &mut Vec<String>) {
        if let Ok(rd) = std::fs::read_dir(d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(root, &p, out);
                } else if let Ok(rel) = p.strip_prefix(root) {
                    out.push(rel.to_string_lossy().into_owned());
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(d, d, &mut out);
    out.sort();
    out
}

/// NaN and the infinities as the JSON tokens that spell them (the case reads
/// the registry with `parse_constant`), since JSON cannot carry them.
fn constants_as_tokens(v: Py) -> Py {
    match v {
        Py::Float(f) if f.is_nan() => Py::Str("NaN".into()),
        Py::Float(f) if f.is_infinite() => Py::Str(if f > 0.0 { "Infinity" } else { "-Infinity" }.into()),
        Py::List(l) => Py::List(l.into_iter().map(constants_as_tokens).collect()),
        Py::Dict(d) => Py::Dict(d.into_iter().map(|(k, x)| (k, constants_as_tokens(x))).collect()),
        other => other,
    }
}

/// Parity hook: see ARCHITECTURE.md.
pub fn parity(func: &str, args: &Value) -> std::option::Option<Value> {
    let s = |k: &str| crate::util::arg_str(args, k).to_string();
    let strs = |k: &str| crate::util::arg_strs(args, k);
    let null = Value::Null;
    let a = |k: &str| args.get(k).unwrap_or(&null);
    let out = match func {
        "parse" => json!(parse(&s("link")).map(|(o, r)| vec![o, r])),
        "_get" => match get(&s("url"), 20) {
            Ok(b) => json!(String::from_utf8_lossy(&b)),
            Err(e) => json!({ "error": e }),
        },
        "_official" => opt_json(official(&s("name"))),
        "_norm" => json!(norm(&s("url"))),
        "_aur" => {
            let names = strs("names");
            let homes = strs("homes");
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            let homes: Vec<&str> = homes.iter().map(String::as_str).collect();
            if args.get("live").is_some() {
                res_json(aur(&names, &homes).map(|v| json!(v)))
            } else {
                // The raw answer (`data`) as `_get` would have returned it.
                let data = s("data");
                let homes: Vec<Py> = homes.iter().map(|h| Py::Str(h.to_string())).collect();
                res_json(aur_with(&names, &homes, &|_u: &str| Ok(data.as_bytes().to_vec())).map(|v| json!(v)))
            }
        }
        "_sums" => {
            let assets: Vec<Value> = a("assets").as_array().cloned().unwrap_or_default();
            res_json(sums(&assets).map(|m| json!(m)))
        }
        "_appimage" => res_json(appimage(a("release")).map(opt_json)),
        "resolve" => {
            let meta = a("meta").clone();
            let release = a("release").clone();
            let repo_fn = move |_r: &str| Ok(Some(meta.clone()).filter(|m| !m.is_null()));
            let release_fn = move |_r: &str| Ok(Some(release.clone()).filter(|m| !m.is_null()));
            let responses = a("responses");
            let fetch = fake_fetch(responses);
            let r = resolve_with(&s("link"), &repo_fn, &release_fn, &fetch);
            if args.get("raises_as_value").is_some() {
                // The case turned a Python raise into "python raised <Type>".
                match r {
                    Ok(r) => json!(r),
                    Err(e) => json!(format!("python raised {}", e.split(':').next().unwrap_or(""))),
                }
            } else {
                res_json(r.map(|r| json!(r)))
            }
        }
        "Resolution.url" | "Resolution.best" => {
            let r: Resolution = serde_json::from_value(a("res").clone()).unwrap_or_default();
            if func == "Resolution.url" {
                json!(r.url())
            } else {
                json!(r.best())
            }
        }
        "install_appimage" => {
            // State, data and history go under the temp dir `root`.
            let root = PathBuf::from(s("root"));
            std::env::set_var("XDG_STATE_HOME", root.join("state"));
            std::env::set_var("XDG_DATA_HOME", root.join("data"));
            let res: Resolution = serde_json::from_value(a("res").clone()).unwrap_or_default();
            let opt: Option = serde_json::from_value(a("opt").clone()).unwrap_or_default();
            let lines = std::cell::RefCell::new(Vec::<String>::new());
            let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                install_appimage(&res, &opt, &|m: &str| lines.borrow_mut().push(m.to_string()))
            }));
            match run {
                Err(_) => json!({ "error": "blocked: a module this calls is still a stub" }),
                Ok(Err(e)) => json!({ "error": e, "log": lines.into_inner(), "files": tree(&root) }),
                Ok(Ok(p)) => {
                    let rel = p.strip_prefix(&root).map(|r| r.to_string_lossy().into_owned()).unwrap_or_default();
                    // The registry records the absolute path; the temp dir is
                    // not the same on both sides.
                    let registry = std::fs::read_to_string(root.join("state/almanac/appimages.json"))
                        .ok()
                        .map(|r| r.replace(&root.to_string_lossy().into_owned(), "<root>"))
                        .and_then(|r| loads(r.as_bytes()).ok())
                        .map(|r| serde_json::to_value(constants_as_tokens(r)).unwrap_or_default());
                    json!({ "path": rel, "log": lines.into_inner(), "files": tree(&root), "registry": registry })
                }
            }
        }
        _ => return None,
    };
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // InstallFromLink
    #[test]
    fn test_links_are_recognised() {
        for link in [
            "https://github.com/storytold/wordcraft",
            "github.com/storytold/wordcraft/releases",
            "https://github.com/storytold/wordcraft.git",
        ] {
            assert_eq!(parse(link), Some(("storytold".to_string(), "wordcraft".to_string())));
        }
        assert_eq!(parse("https://gitlab.com/a/b"), None);
        assert_eq!(parse("wordcraft"), None);
    }

    #[test]
    fn test_the_x86_appimage_is_chosen_and_its_digest_used() {
        let rel = json!({"tag_name": "v1.2.0", "assets": [
            {"name": "app-1.2.0-aarch64.AppImage", "size": 1,
             "browser_download_url": "u1"},
            {"name": "app-1.2.0-x86_64.AppImage", "size": 1048576,
             "browser_download_url": "u2", "digest": format!("sha256:{}", "a".repeat(64))}]});
        let o = appimage(&rel).expect("a release").expect("an AppImage");
        assert_eq!(o.name, "app-1.2.0-x86_64.AppImage");
        assert_eq!(o.sha256, "a".repeat(64));
        assert_eq!(o.version, "1.2.0");
    }

    #[test]
    fn test_an_unverifiable_appimage_ranks_below_a_verified_one() {
        let rel = json!({"tag_name": "v1", "assets": [
            {"name": "app-1-x86_64.AppImage", "size": 1,
             "browser_download_url": "u"}]});
        let unverified = appimage(&rel).expect("a release").expect("an AppImage");
        assert!(!unverified.warnings.is_empty());
        assert!(unverified.rank > 5);
    }

    #[test]
    fn test_an_aur_package_for_another_project_is_flagged() {
        let data = json!({"results": [
            {"Name": "app", "Version": "1", "NumVotes": 50, "Maintainer": "m",
             "URL": "https://github.com/someone-else/app"},
            {"Name": "app-bin", "Version": "1", "NumVotes": 50, "Maintainer": "m",
             "URL": "https://app.example.org"}]})
        .to_string();
        let fetch = |_u: &str| Ok(data.as_bytes().to_vec());
        let homes = [Py::Str("https://github.com/owner/app".into()), Py::Str("https://app.example.org/".into())];
        let opts: HashMap<String, Option> = aur_with(&["app", "app-bin"], &homes, &fetch)
            .expect("an AUR answer")
            .into_iter()
            .map(|o| (o.name.clone(), o))
            .collect();
        assert!(opts["app"].warnings.iter().any(|w| w.contains("not this project")));
        assert!(opts["app-bin"].reasons.contains(&"Points at this project".to_string()));
    }

    #[test]
    fn test_a_checksum_mismatch_deletes_the_download() {
        // The Python patches appimage.download to write a file; here a
        // one-shot local HTTP server serves it, and XDG_STATE_HOME points
        // inventory's REGISTRY at the temp dir.
        use std::io::Write;
        let _env = crate::util::TEST_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().expect("tempdir");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let server = std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                let body = b"not what was published";
                let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = s.write_all(body);
            }
        });
        std::env::set_var("XDG_STATE_HOME", d.path());
        let res = Resolution { owner: "o".into(), repo: "app".into(), ..Default::default() };
        let opt = Option {
            kind: APPIMAGE.into(),
            name: "app-1-x86_64.AppImage".into(),
            version: "1".into(),
            title: "AppImage".into(),
            url: format!("http://127.0.0.1:{port}/app"),
            sha256: "0".repeat(64),
            ..Default::default()
        };
        assert!(install_appimage(&res, &opt, &|_m: &str| {}).is_err());
        let _ = server.join();
        let apps = d.path().join("almanac").join("apps");
        assert_eq!(std::fs::read_dir(apps).expect("apps dir").count(), 0);
    }

    #[test]
    fn py_helpers() {
        assert_eq!(py_splitlines("a\r\nb\rc\n\nd"), vec!["a", "b", "c", "", "d"]);
        assert_eq!(py_splitlines("a\n"), vec!["a"]);
        assert_eq!(quote("a b/ü"), "a%20b/%C3%BC");
        assert_eq!(norm(" HTTPS://www.GitHub.com/O/R.git/ "), "github.com/o/r");
        assert_eq!(norm("https://github.com/o/r.git"), "github.com/o/r");
        assert_eq!(BigInt::from_f64(1e20).to_string(), "100000000000000000000");
        assert_eq!(BigInt::from_f64(-2f64.powi(70)).to_string(), "-1180591620717411303424");
        assert_eq!(py_int(&Py::Str(" １_２ ".into())).map(|i| i.to_string()), Ok("12".into()));
        assert!(py_int(&Py::Str("1__2".into())).is_err());
        assert_eq!(b64decode(b"aGk=").as_deref(), Ok(&b"hi"[..]));
        assert!(b64decode(b"aGk").is_err());
        assert_eq!(b64decode(b"aGVsbA==QQ==").as_deref(), Ok(&b"hell\x04\x10"[..]));
        assert!(b64decode(b"aGVsbG8=abcd").is_err());
        assert_eq!(decode_json_bytes(b"\xff\xfe[\x001\x00]\x00").as_deref(), Ok("[1]"));
        assert_eq!(decode_json_bytes(b"\x00\x00\x00[").as_deref(), Ok("["));
        assert_eq!(loads(b"\xef\xbb\xbf[NaN, -Infinity, 1e400, 18446744073709551616]").map(|v| v.repr()), Ok("[nan, -inf, inf, 18446744073709551616]".into()));
        assert!(loads(b"[1,]").is_err());
        assert!(loads(b"01").is_err());
        assert!(other_arch().is_match("app-\u{131}686.AppImage"));
        assert!(other_arch().is_match("app-R\u{130}SCV.AppImage"));
        assert!(other_arch().is_match("app-ri\u{17f}cv.AppImage"));
        // str.isprintable(): format (Cf) and unassigned (Cn) are not
        assert!(!py_isprintable('\u{180e}'));
        assert!(!py_isprintable('\u{378}'));
        assert!(!py_isprintable('\u{e0001}'));
        assert!(!py_isprintable('\u{a0}'));
        assert!(py_isprintable(' ') && py_isprintable('a') && py_isprintable('\u{10d50}'));
        assert_eq!(str_repr("\u{180e}\u{378}x"), "'\\u180e\\u0378x'");
        // json.dumps(indent=2) of what json.loads read
        let v = loads(b"{\"b\": NaN, \"a\": [100000000000000000000000, -Infinity, 1.5, {}], \"c\": \"\\u00fc\"}").expect("json");
        let mut out = String::new();
        py_dumps(&v, 2, 0, &mut out);
        assert_eq!(out, "{\n  \"b\": NaN,\n  \"a\": [\n    100000000000000000000000,\n    -Infinity,\n    1.5,\n    {}\n  ],\n  \"c\": \"\\u00fc\"\n}");
        // str(exc)
        assert_eq!(exc_str("RuntimeError: checksum mismatch: x"), "checksum mismatch: x");
        assert_eq!(exc_str("OSError: No such file or directory (os error 2)"), "[Errno 2] No such file or directory");
        assert_eq!(exc_str("HTTPError: HTTP Error 404: Not Found"), "HTTP Error 404: Not Found");
        // urllib's proxy helpers
        assert_eq!(get_hostport("[::1]:8080", 80), Ok(("::1".to_string(), 8080)));
        assert_eq!(get_hostport("h:", 443), Ok(("h".to_string(), 443)));
        assert!(get_hostport("u:p@h", 80).is_err());
        assert_eq!(
            parse_proxy("http://u:p@proxy:3128/"),
            Ok((Some("http".into()), Some("u".into()), Some("p".into()), "proxy:3128".into()))
        );
        assert_eq!(parse_proxy("proxy:3128"), Ok((None, None, None, "proxy:3128".into())));
        assert!(parse_proxy("http:/x").is_err());
        let p: HashMap<String, String> = [("no".to_string(), " .Example.org, localhost".to_string())].into();
        assert!(proxy_bypass("a.example.org:443", &p));
        assert!(proxy_bypass("LOCALHOST:80", &p));
        assert!(!proxy_bypass("example.com", &p));
    }
}
