//! Port of `ghcache.py`.
//!
//! One GitHub cache, shared by everything that asks GitHub anything.
//!
//! Three modules used to call api.github.com independently, each with its own
//! expiry and no idea what the others had spent. Fourteen ArtCraft release
//! lookups plus five agent releases is nineteen of sixty unauthenticated calls,
//! so two refreshes in an hour left the store with blank versions.
//!
//! This fixes that four ways, in order of how much they matter:
//!
//! 1. **A token, when there is one.** `gh auth token` raises the ceiling from 60
//!    requests an hour to 5,000. It is read on demand, never written to disk here
//!    and never logged.
//! 2. **Conditional requests.** Every response's ETag is kept and sent back as
//!    If-None-Match. GitHub answers 304 for unchanged data, and *304s do not
//!    count against the rate limit* -- so re-checking something that has not
//!    moved is free.
//! 3. **A real cache.** Inside the TTL nothing touches the network at all.
//! 4. **A budget guard.** The remaining quota is read from the response headers.
//!    When it runs low the cache is served stale instead of spending the last
//!    few calls, so a refresh degrades to "slightly old" rather than "empty".
//!
//! Port notes. The Python keeps whatever `json.loads` hands it, so the cache
//! and rate files are read into [`PyValue`] (Python's JSON values: exact
//! ints, inf/nan floats, strings that may hold lone surrogates) and written
//! back with Python's `json.dumps`, so the files on disk stay identical. The
//! public infallible functions other modules call are deprecated: each has a
//! `try_` twin that raises (returns `Err`) exactly where the Python raises.
//!
//! The network side is urllib and http.client themselves, ported: the same
//! request bytes, the same response parsing (an `email` header block, the
//! 100-header and 64 KB-line limits), the same redirect URL handling and the
//! same per-scheme proxies from the environment, over std's TCP and rustls.
//!
//! `CACHE`, `DIR` and `STATE` are read from `XDG_CACHE_HOME` on each call
//! rather than once at import: the app never changes that variable, and the
//! other modules' parity hooks redirect ghcache through it (their Python
//! cases patch `ghcache.CACHE`).

use num_bigint::BigInt;
use serde_json::{Map, Value};
use std::cmp::Ordering;
use std::path::PathBuf;
use std::sync::Mutex;

pub const UA: &str = "almanac";
pub const API: &str = "https://api.github.com";
pub const DEFAULT_TTL: u64 = 6 * 3600;
/// The default `Accept` header of `get_json` (a keyword default in Python).
pub const ACCEPT: &str = "application/vnd.github+json";
/// Leave a few calls for whatever the user does next in a terminal.
pub const RESERVE: i64 = 8;

/// How deep `json.loads` nests before Python's RecursionError. Python's
/// guard is the C stack, not the recursion limit: on this machine (8 MB
/// stack, Python 3.14) arrays and objects alike stop at about 52 100 levels.
const LOADS_DEPTH: usize = 52_100;
/// The same for `json.dumps`, whose C frames are larger for a dict than for
/// a list: a list level costs `DUMPS_LIST` of `DUMPS_BUDGET`, a dict level
/// `DUMPS_DICT` (lists alone stop at about 52 090 levels, dicts at 28 939).
const DUMPS_BUDGET: u64 = 52_090 * 28_939;
const DUMPS_LIST: u64 = 28_939;
const DUMPS_DICT: u64 = 52_090;
/// The stack the JSON work runs on (see [`deep`]): enough for the
/// recursive parser, printer and conversions at Python's depth.
const DEEP_STACK: usize = 512 << 20;
/// Python's `sys.get_int_max_str_digits()` default.
const INT_MAX_STR_DIGITS: usize = 4300;
/// urllib's `timeout=25`: every socket operation, not the whole request.
const SOCKET_TIMEOUT: u64 = 25;

static _LOCK: Mutex<()> = Mutex::new(());

/// `_token` and `_token_checked`: the token read once per process.
struct TokenMemo {
    token: Option<String>,
    checked: bool,
}

static _TOKEN: Mutex<TokenMemo> = Mutex::new(TokenMemo { token: None, checked: false });

/// `CACHE`: `$XDG_CACHE_HOME/almanac` (or `~/.cache/almanac`).
///
/// Python computes this once at import; here it is read on each call, so a
/// test or parity case that sets `XDG_CACHE_HOME` is honoured.
pub fn cache() -> PathBuf {
    let base = match std::env::var_os("XDG_CACHE_HOME") {
        Some(v) => PathBuf::from(v),
        None => crate::util::home().join(".cache"),
    };
    base.join("almanac")
}

/// `DIR`: one file per cached URL.
pub fn dir() -> PathBuf {
    cache().join("github")
}

/// `STATE`: what the last response said about the remaining quota.
pub fn state_file() -> PathBuf {
    cache().join("github-rate.json")
}

// --- Python's JSON values -------------------------------------------------

/// A Python string: code points, so a lone surrogate from `"\ud800"` or a
/// latin-1 header byte survives a round trip.
pub type PyStr = Vec<u32>;

fn ps(s: &str) -> PyStr {
    s.chars().map(|c| c as u32).collect()
}

fn ps_string(s: &[u32]) -> String {
    s.iter().map(|&c| char::from_u32(c).unwrap_or('\u{fffd}')).collect()
}

/// What `json.loads` returns: None, bool, int (any size), float (inf and nan
/// included), str, list, dict (insertion ordered).
#[derive(Debug)]
pub enum PyValue {
    None,
    Bool(bool),
    Int(BigInt),
    Float(f64),
    Str(PyStr),
    List(Vec<PyValue>),
    Dict(Vec<(PyStr, PyValue)>),
}

// As deep as Python's json goes: cloned and compared without recursion.
crate::util::tree_clone_eq!(PyValue, key: PyStr, units: [None], leaves: [Bool, Int, Float, Str], list: List, dict: Dict);

impl PyValue {
    fn int(n: i64) -> PyValue {
        PyValue::Int(BigInt::from(n))
    }

    fn str(s: &str) -> PyValue {
        PyValue::Str(ps(s))
    }

    /// `bool(x)`.
    fn truthy(&self) -> bool {
        match self {
            PyValue::None => false,
            PyValue::Bool(b) => *b,
            PyValue::Int(n) => *n != BigInt::from(0),
            PyValue::Float(f) => *f != 0.0,
            PyValue::Str(s) => !s.is_empty(),
            PyValue::List(v) => !v.is_empty(),
            PyValue::Dict(d) => !d.is_empty(),
        }
    }

    /// `type(x).__name__`.
    fn type_name(&self) -> &'static str {
        match self {
            PyValue::None => "NoneType",
            PyValue::Bool(_) => "bool",
            PyValue::Int(_) => "int",
            PyValue::Float(_) => "float",
            PyValue::Str(_) => "str",
            PyValue::List(_) => "list",
            PyValue::Dict(_) => "dict",
        }
    }

    /// The dict behind `x.get(...)`, or Python's AttributeError.
    fn dict(&self) -> Result<&Vec<(PyStr, PyValue)>, String> {
        match self {
            PyValue::Dict(d) => Ok(d),
            other => Err(format!("AttributeError: '{}' object has no attribute 'get'", other.type_name())),
        }
    }

    /// `d.get(key)` on a dict; None for anything else.
    fn get(&self, key: &str) -> Option<&PyValue> {
        let PyValue::Dict(d) = self else { return None };
        let k = ps(key);
        d.iter().find(|(kk, _)| *kk == k).map(|(_, v)| v)
    }

    /// `d[key] = value`: an existing key keeps its place.
    fn set(&mut self, key: &str, value: PyValue) {
        let PyValue::Dict(d) = self else { return };
        let k = ps(key);
        match d.iter_mut().find(|(kk, _)| *kk == k) {
            Some(slot) => slot.1 = value,
            None => d.push((k, value)),
        }
    }

    /// The closest `serde_json::Value`, the crate's type for parsed JSON.
    /// Lossy only where it cannot follow Python: an int beyond u64 becomes a
    /// float, a non-finite float (or an int too large for one) becomes null,
    /// a lone surrogate U+FFFD. [`try_get_json_py`] hands out the exact value.
    pub fn to_value(&self) -> Value {
        match self {
            PyValue::None => Value::Null,
            PyValue::Bool(b) => Value::Bool(*b),
            PyValue::Int(n) => {
                let s = n.to_string();
                if let Ok(i) = s.parse::<i64>() {
                    Value::from(i)
                } else if let Ok(u) = s.parse::<u64>() {
                    Value::from(u)
                } else {
                    s.parse::<f64>().ok().and_then(serde_json::Number::from_f64).map(Value::Number).unwrap_or(Value::Null)
                }
            }
            PyValue::Float(f) => serde_json::Number::from_f64(*f).map(Value::Number).unwrap_or(Value::Null),
            PyValue::Str(s) => Value::String(ps_string(s)),
            PyValue::List(v) => Value::Array(v.iter().map(PyValue::to_value).collect()),
            PyValue::Dict(d) => {
                let mut m = Map::new();
                for (k, v) in d {
                    m.insert(ps_string(k), v.to_value());
                }
                Value::Object(m)
            }
        }
    }

    #[cfg(test)]
    /// The other way, for parity arguments.
    fn from_value(v: &Value) -> PyValue {
        match v {
            Value::Null => PyValue::None,
            Value::Bool(b) => PyValue::Bool(*b),
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    PyValue::int(i)
                } else if let Some(u) = n.as_u64() {
                    PyValue::Int(BigInt::from(u))
                } else {
                    PyValue::Float(n.as_f64().unwrap_or(0.0))
                }
            }
            Value::String(s) => PyValue::str(s),
            Value::Array(a) => PyValue::List(a.iter().map(PyValue::from_value).collect()),
            Value::Object(m) => PyValue::Dict(m.iter().map(|(k, v)| (ps(k), PyValue::from_value(v))).collect()),
        }
    }

    /// `str(x)`, as an f-string writes it.
    fn py_str(&self) -> String {
        match self {
            PyValue::Str(s) => ps_string(s),
            other => other.py_repr(),
        }
    }

    /// `repr(x)`.
    fn py_repr(&self) -> String {
        match self {
            PyValue::None => "None".into(),
            PyValue::Bool(b) => if *b { "True" } else { "False" }.into(),
            PyValue::Int(n) => n.to_string(),
            PyValue::Float(f) => float_repr(*f),
            PyValue::Str(s) => str_repr(s),
            PyValue::List(v) => format!("[{}]", v.iter().map(PyValue::py_repr).collect::<Vec<_>>().join(", ")),
            PyValue::Dict(d) => format!(
                "{{{}}}",
                d.iter().map(|(k, v)| format!("{}: {}", str_repr(k), v.py_repr())).collect::<Vec<_>>().join(", ")
            ),
        }
    }
}

/// Dropped without recursion, so a value nested as deep as Python's json
/// allows can be dropped on any thread's stack.
impl Drop for PyValue {
    fn drop(&mut self) {
        fn children(v: &mut PyValue, stack: &mut Vec<PyValue>) {
            match v {
                PyValue::List(a) => stack.append(a),
                PyValue::Dict(d) => stack.extend(d.drain(..).map(|(_, x)| x)),
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

impl PyValue {
}

/// Runs `f` on a thread with a [`DEEP_STACK`] stack: the JSON code is
/// recursive, and Python's json goes about 52 000 levels deep before it
/// gives up. (Only reserved address space; the pages are touched as used.)
// `&run`, not `run`: the fallback below calls it again.
#[allow(clippy::needless_borrows_for_generic_args)]
fn deep<T: Send, F: FnOnce() -> T + Send>(f: F) -> T {
    let slot = Mutex::new(Some(f));
    let run = || slot.lock().unwrap_or_else(|e| e.into_inner()).take().map(|f| f());
    let out = std::thread::scope(|s| match std::thread::Builder::new().stack_size(DEEP_STACK).spawn_scoped(s, &run) {
        Ok(h) => h.join().unwrap_or_else(|p| std::panic::resume_unwind(p)),
        // No thread to be had: run here, as deep as this stack allows.
        Err(_) => None,
    });
    out.or_else(run).expect("deep(): the closure is still there when the thread could not start")
}

/// `repr(float)`: like `json.dumps` except for the non-finite ones.
fn float_repr(f: f64) -> String {
    if f.is_nan() {
        "nan".into()
    } else if f.is_infinite() {
        if f > 0.0 { "inf" } else { "-inf" }.into()
    } else {
        crate::util::py_float_repr(f)
    }
}

/// `str.isprintable()` for one code point.
fn py_isprintable(c: u32) -> bool {
    match NON_PRINTABLE.binary_search_by(|&(lo, hi)| {
        if hi < c {
            Ordering::Less
        } else if lo > c {
            Ordering::Greater
        } else {
            Ordering::Equal
        }
    }) {
        Ok(_) => false,
        Err(_) => c <= 0x10ffff,
    }
}

/// `repr(str)`.
fn str_repr(s: &[u32]) -> String {
    let quote = if s.contains(&('\'' as u32)) && !s.contains(&('"' as u32)) { '"' } else { '\'' };
    let mut out = String::new();
    out.push(quote);
    for &c in s {
        let printable = py_isprintable(c);
        match char::from_u32(c) {
            _ if c == quote as u32 || c == '\\' as u32 => {
                out.push('\\');
                out.push(quote_char(c));
            }
            _ if c == '\t' as u32 => out.push_str("\\t"),
            _ if c == '\n' as u32 => out.push_str("\\n"),
            _ if c == '\r' as u32 => out.push_str("\\r"),
            Some(ch) if printable => out.push(ch),
            _ if c <= 0xff => out.push_str(&format!("\\x{c:02x}")),
            _ if c <= 0xffff => out.push_str(&format!("\\u{c:04x}")),
            _ => out.push_str(&format!("\\U{c:08x}")),
        }
    }
    out.push(quote);
    out
}

fn quote_char(c: u32) -> char {
    char::from_u32(c).unwrap_or('?')
}

/// The code points `str.isprintable()` rejects (Unicode 16.0.0), as inclusive
/// ranges: Cc, Cf, Cs, Co, Cn, Zl, Zp and Zs other than the space.
const NON_PRINTABLE: [(u32, u32); 737] = [
    (0x0, 0x1f), (0x7f, 0xa0), (0xad, 0xad), (0x378, 0x379), (0x380, 0x383), (0x38b, 0x38b), (0x38d, 0x38d),
    (0x3a2, 0x3a2), (0x530, 0x530), (0x557, 0x558), (0x58b, 0x58c), (0x590, 0x590), (0x5c8, 0x5cf), (0x5eb, 0x5ee),
    (0x5f5, 0x605), (0x61c, 0x61c), (0x6dd, 0x6dd), (0x70e, 0x70f), (0x74b, 0x74c), (0x7b2, 0x7bf), (0x7fb, 0x7fc),
    (0x82e, 0x82f), (0x83f, 0x83f), (0x85c, 0x85d), (0x85f, 0x85f), (0x86b, 0x86f), (0x88f, 0x896), (0x8e2, 0x8e2),
    (0x984, 0x984), (0x98d, 0x98e), (0x991, 0x992), (0x9a9, 0x9a9), (0x9b1, 0x9b1), (0x9b3, 0x9b5), (0x9ba, 0x9bb),
    (0x9c5, 0x9c6), (0x9c9, 0x9ca), (0x9cf, 0x9d6), (0x9d8, 0x9db), (0x9de, 0x9de), (0x9e4, 0x9e5), (0x9ff, 0xa00),
    (0xa04, 0xa04), (0xa0b, 0xa0e), (0xa11, 0xa12), (0xa29, 0xa29), (0xa31, 0xa31), (0xa34, 0xa34), (0xa37, 0xa37),
    (0xa3a, 0xa3b), (0xa3d, 0xa3d), (0xa43, 0xa46), (0xa49, 0xa4a), (0xa4e, 0xa50), (0xa52, 0xa58), (0xa5d, 0xa5d),
    (0xa5f, 0xa65), (0xa77, 0xa80), (0xa84, 0xa84), (0xa8e, 0xa8e), (0xa92, 0xa92), (0xaa9, 0xaa9), (0xab1, 0xab1),
    (0xab4, 0xab4), (0xaba, 0xabb), (0xac6, 0xac6), (0xaca, 0xaca), (0xace, 0xacf), (0xad1, 0xadf), (0xae4, 0xae5),
    (0xaf2, 0xaf8), (0xb00, 0xb00), (0xb04, 0xb04), (0xb0d, 0xb0e), (0xb11, 0xb12), (0xb29, 0xb29), (0xb31, 0xb31),
    (0xb34, 0xb34), (0xb3a, 0xb3b), (0xb45, 0xb46), (0xb49, 0xb4a), (0xb4e, 0xb54), (0xb58, 0xb5b), (0xb5e, 0xb5e),
    (0xb64, 0xb65), (0xb78, 0xb81), (0xb84, 0xb84), (0xb8b, 0xb8d), (0xb91, 0xb91), (0xb96, 0xb98), (0xb9b, 0xb9b),
    (0xb9d, 0xb9d), (0xba0, 0xba2), (0xba5, 0xba7), (0xbab, 0xbad), (0xbba, 0xbbd), (0xbc3, 0xbc5), (0xbc9, 0xbc9),
    (0xbce, 0xbcf), (0xbd1, 0xbd6), (0xbd8, 0xbe5), (0xbfb, 0xbff), (0xc0d, 0xc0d), (0xc11, 0xc11), (0xc29, 0xc29),
    (0xc3a, 0xc3b), (0xc45, 0xc45), (0xc49, 0xc49), (0xc4e, 0xc54), (0xc57, 0xc57), (0xc5b, 0xc5c), (0xc5e, 0xc5f),
    (0xc64, 0xc65), (0xc70, 0xc76), (0xc8d, 0xc8d), (0xc91, 0xc91), (0xca9, 0xca9), (0xcb4, 0xcb4), (0xcba, 0xcbb),
    (0xcc5, 0xcc5), (0xcc9, 0xcc9), (0xcce, 0xcd4), (0xcd7, 0xcdc), (0xcdf, 0xcdf), (0xce4, 0xce5), (0xcf0, 0xcf0),
    (0xcf4, 0xcff), (0xd0d, 0xd0d), (0xd11, 0xd11), (0xd45, 0xd45), (0xd49, 0xd49), (0xd50, 0xd53), (0xd64, 0xd65),
    (0xd80, 0xd80), (0xd84, 0xd84), (0xd97, 0xd99), (0xdb2, 0xdb2), (0xdbc, 0xdbc), (0xdbe, 0xdbf), (0xdc7, 0xdc9),
    (0xdcb, 0xdce), (0xdd5, 0xdd5), (0xdd7, 0xdd7), (0xde0, 0xde5), (0xdf0, 0xdf1), (0xdf5, 0xe00), (0xe3b, 0xe3e),
    (0xe5c, 0xe80), (0xe83, 0xe83), (0xe85, 0xe85), (0xe8b, 0xe8b), (0xea4, 0xea4), (0xea6, 0xea6), (0xebe, 0xebf),
    (0xec5, 0xec5), (0xec7, 0xec7), (0xecf, 0xecf), (0xeda, 0xedb), (0xee0, 0xeff), (0xf48, 0xf48), (0xf6d, 0xf70),
    (0xf98, 0xf98), (0xfbd, 0xfbd), (0xfcd, 0xfcd), (0xfdb, 0xfff), (0x10c6, 0x10c6), (0x10c8, 0x10cc),
    (0x10ce, 0x10cf), (0x1249, 0x1249), (0x124e, 0x124f), (0x1257, 0x1257), (0x1259, 0x1259), (0x125e, 0x125f),
    (0x1289, 0x1289), (0x128e, 0x128f), (0x12b1, 0x12b1), (0x12b6, 0x12b7), (0x12bf, 0x12bf), (0x12c1, 0x12c1),
    (0x12c6, 0x12c7), (0x12d7, 0x12d7), (0x1311, 0x1311), (0x1316, 0x1317), (0x135b, 0x135c), (0x137d, 0x137f),
    (0x139a, 0x139f), (0x13f6, 0x13f7), (0x13fe, 0x13ff), (0x1680, 0x1680), (0x169d, 0x169f), (0x16f9, 0x16ff),
    (0x1716, 0x171e), (0x1737, 0x173f), (0x1754, 0x175f), (0x176d, 0x176d), (0x1771, 0x1771), (0x1774, 0x177f),
    (0x17de, 0x17df), (0x17ea, 0x17ef), (0x17fa, 0x17ff), (0x180e, 0x180e), (0x181a, 0x181f), (0x1879, 0x187f),
    (0x18ab, 0x18af), (0x18f6, 0x18ff), (0x191f, 0x191f), (0x192c, 0x192f), (0x193c, 0x193f), (0x1941, 0x1943),
    (0x196e, 0x196f), (0x1975, 0x197f), (0x19ac, 0x19af), (0x19ca, 0x19cf), (0x19db, 0x19dd), (0x1a1c, 0x1a1d),
    (0x1a5f, 0x1a5f), (0x1a7d, 0x1a7e), (0x1a8a, 0x1a8f), (0x1a9a, 0x1a9f), (0x1aae, 0x1aaf), (0x1acf, 0x1aff),
    (0x1b4d, 0x1b4d), (0x1bf4, 0x1bfb), (0x1c38, 0x1c3a), (0x1c4a, 0x1c4c), (0x1c8b, 0x1c8f), (0x1cbb, 0x1cbc),
    (0x1cc8, 0x1ccf), (0x1cfb, 0x1cff), (0x1f16, 0x1f17), (0x1f1e, 0x1f1f), (0x1f46, 0x1f47), (0x1f4e, 0x1f4f),
    (0x1f58, 0x1f58), (0x1f5a, 0x1f5a), (0x1f5c, 0x1f5c), (0x1f5e, 0x1f5e), (0x1f7e, 0x1f7f), (0x1fb5, 0x1fb5),
    (0x1fc5, 0x1fc5), (0x1fd4, 0x1fd5), (0x1fdc, 0x1fdc), (0x1ff0, 0x1ff1), (0x1ff5, 0x1ff5), (0x1fff, 0x200f),
    (0x2028, 0x202f), (0x205f, 0x206f), (0x2072, 0x2073), (0x208f, 0x208f), (0x209d, 0x209f), (0x20c1, 0x20cf),
    (0x20f1, 0x20ff), (0x218c, 0x218f), (0x242a, 0x243f), (0x244b, 0x245f), (0x2b74, 0x2b75), (0x2b96, 0x2b96),
    (0x2cf4, 0x2cf8), (0x2d26, 0x2d26), (0x2d28, 0x2d2c), (0x2d2e, 0x2d2f), (0x2d68, 0x2d6e), (0x2d71, 0x2d7e),
    (0x2d97, 0x2d9f), (0x2da7, 0x2da7), (0x2daf, 0x2daf), (0x2db7, 0x2db7), (0x2dbf, 0x2dbf), (0x2dc7, 0x2dc7),
    (0x2dcf, 0x2dcf), (0x2dd7, 0x2dd7), (0x2ddf, 0x2ddf), (0x2e5e, 0x2e7f), (0x2e9a, 0x2e9a), (0x2ef4, 0x2eff),
    (0x2fd6, 0x2fef), (0x3000, 0x3000), (0x3040, 0x3040), (0x3097, 0x3098), (0x3100, 0x3104), (0x3130, 0x3130),
    (0x318f, 0x318f), (0x31e6, 0x31ee), (0x321f, 0x321f), (0xa48d, 0xa48f), (0xa4c7, 0xa4cf), (0xa62c, 0xa63f),
    (0xa6f8, 0xa6ff), (0xa7ce, 0xa7cf), (0xa7d2, 0xa7d2), (0xa7d4, 0xa7d4), (0xa7dd, 0xa7f1), (0xa82d, 0xa82f),
    (0xa83a, 0xa83f), (0xa878, 0xa87f), (0xa8c6, 0xa8cd), (0xa8da, 0xa8df), (0xa954, 0xa95e), (0xa97d, 0xa97f),
    (0xa9ce, 0xa9ce), (0xa9da, 0xa9dd), (0xa9ff, 0xa9ff), (0xaa37, 0xaa3f), (0xaa4e, 0xaa4f), (0xaa5a, 0xaa5b),
    (0xaac3, 0xaada), (0xaaf7, 0xab00), (0xab07, 0xab08), (0xab0f, 0xab10), (0xab17, 0xab1f), (0xab27, 0xab27),
    (0xab2f, 0xab2f), (0xab6c, 0xab6f), (0xabee, 0xabef), (0xabfa, 0xabff), (0xd7a4, 0xd7af), (0xd7c7, 0xd7ca),
    (0xd7fc, 0xf8ff), (0xfa6e, 0xfa6f), (0xfada, 0xfaff), (0xfb07, 0xfb12), (0xfb18, 0xfb1c), (0xfb37, 0xfb37),
    (0xfb3d, 0xfb3d), (0xfb3f, 0xfb3f), (0xfb42, 0xfb42), (0xfb45, 0xfb45), (0xfbc3, 0xfbd2), (0xfd90, 0xfd91),
    (0xfdc8, 0xfdce), (0xfdd0, 0xfdef), (0xfe1a, 0xfe1f), (0xfe53, 0xfe53), (0xfe67, 0xfe67), (0xfe6c, 0xfe6f),
    (0xfe75, 0xfe75), (0xfefd, 0xff00), (0xffbf, 0xffc1), (0xffc8, 0xffc9), (0xffd0, 0xffd1), (0xffd8, 0xffd9),
    (0xffdd, 0xffdf), (0xffe7, 0xffe7), (0xffef, 0xfffb), (0xfffe, 0xffff), (0x1000c, 0x1000c), (0x10027, 0x10027),
    (0x1003b, 0x1003b), (0x1003e, 0x1003e), (0x1004e, 0x1004f), (0x1005e, 0x1007f), (0x100fb, 0x100ff),
    (0x10103, 0x10106), (0x10134, 0x10136), (0x1018f, 0x1018f), (0x1019d, 0x1019f), (0x101a1, 0x101cf),
    (0x101fe, 0x1027f), (0x1029d, 0x1029f), (0x102d1, 0x102df), (0x102fc, 0x102ff), (0x10324, 0x1032c),
    (0x1034b, 0x1034f), (0x1037b, 0x1037f), (0x1039e, 0x1039e), (0x103c4, 0x103c7), (0x103d6, 0x103ff),
    (0x1049e, 0x1049f), (0x104aa, 0x104af), (0x104d4, 0x104d7), (0x104fc, 0x104ff), (0x10528, 0x1052f),
    (0x10564, 0x1056e), (0x1057b, 0x1057b), (0x1058b, 0x1058b), (0x10593, 0x10593), (0x10596, 0x10596),
    (0x105a2, 0x105a2), (0x105b2, 0x105b2), (0x105ba, 0x105ba), (0x105bd, 0x105bf), (0x105f4, 0x105ff),
    (0x10737, 0x1073f), (0x10756, 0x1075f), (0x10768, 0x1077f), (0x10786, 0x10786), (0x107b1, 0x107b1),
    (0x107bb, 0x107ff), (0x10806, 0x10807), (0x10809, 0x10809), (0x10836, 0x10836), (0x10839, 0x1083b),
    (0x1083d, 0x1083e), (0x10856, 0x10856), (0x1089f, 0x108a6), (0x108b0, 0x108df), (0x108f3, 0x108f3),
    (0x108f6, 0x108fa), (0x1091c, 0x1091e), (0x1093a, 0x1093e), (0x10940, 0x1097f), (0x109b8, 0x109bb),
    (0x109d0, 0x109d1), (0x10a04, 0x10a04), (0x10a07, 0x10a0b), (0x10a14, 0x10a14), (0x10a18, 0x10a18),
    (0x10a36, 0x10a37), (0x10a3b, 0x10a3e), (0x10a49, 0x10a4f), (0x10a59, 0x10a5f), (0x10aa0, 0x10abf),
    (0x10ae7, 0x10aea), (0x10af7, 0x10aff), (0x10b36, 0x10b38), (0x10b56, 0x10b57), (0x10b73, 0x10b77),
    (0x10b92, 0x10b98), (0x10b9d, 0x10ba8), (0x10bb0, 0x10bff), (0x10c49, 0x10c7f), (0x10cb3, 0x10cbf),
    (0x10cf3, 0x10cf9), (0x10d28, 0x10d2f), (0x10d3a, 0x10d3f), (0x10d66, 0x10d68), (0x10d86, 0x10d8d),
    (0x10d90, 0x10e5f), (0x10e7f, 0x10e7f), (0x10eaa, 0x10eaa), (0x10eae, 0x10eaf), (0x10eb2, 0x10ec1),
    (0x10ec5, 0x10efb), (0x10f28, 0x10f2f), (0x10f5a, 0x10f6f), (0x10f8a, 0x10faf), (0x10fcc, 0x10fdf),
    (0x10ff7, 0x10fff), (0x1104e, 0x11051), (0x11076, 0x1107e), (0x110bd, 0x110bd), (0x110c3, 0x110cf),
    (0x110e9, 0x110ef), (0x110fa, 0x110ff), (0x11135, 0x11135), (0x11148, 0x1114f), (0x11177, 0x1117f),
    (0x111e0, 0x111e0), (0x111f5, 0x111ff), (0x11212, 0x11212), (0x11242, 0x1127f), (0x11287, 0x11287),
    (0x11289, 0x11289), (0x1128e, 0x1128e), (0x1129e, 0x1129e), (0x112aa, 0x112af), (0x112eb, 0x112ef),
    (0x112fa, 0x112ff), (0x11304, 0x11304), (0x1130d, 0x1130e), (0x11311, 0x11312), (0x11329, 0x11329),
    (0x11331, 0x11331), (0x11334, 0x11334), (0x1133a, 0x1133a), (0x11345, 0x11346), (0x11349, 0x1134a),
    (0x1134e, 0x1134f), (0x11351, 0x11356), (0x11358, 0x1135c), (0x11364, 0x11365), (0x1136d, 0x1136f),
    (0x11375, 0x1137f), (0x1138a, 0x1138a), (0x1138c, 0x1138d), (0x1138f, 0x1138f), (0x113b6, 0x113b6),
    (0x113c1, 0x113c1), (0x113c3, 0x113c4), (0x113c6, 0x113c6), (0x113cb, 0x113cb), (0x113d6, 0x113d6),
    (0x113d9, 0x113e0), (0x113e3, 0x113ff), (0x1145c, 0x1145c), (0x11462, 0x1147f), (0x114c8, 0x114cf),
    (0x114da, 0x1157f), (0x115b6, 0x115b7), (0x115de, 0x115ff), (0x11645, 0x1164f), (0x1165a, 0x1165f),
    (0x1166d, 0x1167f), (0x116ba, 0x116bf), (0x116ca, 0x116cf), (0x116e4, 0x116ff), (0x1171b, 0x1171c),
    (0x1172c, 0x1172f), (0x11747, 0x117ff), (0x1183c, 0x1189f), (0x118f3, 0x118fe), (0x11907, 0x11908),
    (0x1190a, 0x1190b), (0x11914, 0x11914), (0x11917, 0x11917), (0x11936, 0x11936), (0x11939, 0x1193a),
    (0x11947, 0x1194f), (0x1195a, 0x1199f), (0x119a8, 0x119a9), (0x119d8, 0x119d9), (0x119e5, 0x119ff),
    (0x11a48, 0x11a4f), (0x11aa3, 0x11aaf), (0x11af9, 0x11aff), (0x11b0a, 0x11bbf), (0x11be2, 0x11bef),
    (0x11bfa, 0x11bff), (0x11c09, 0x11c09), (0x11c37, 0x11c37), (0x11c46, 0x11c4f), (0x11c6d, 0x11c6f),
    (0x11c90, 0x11c91), (0x11ca8, 0x11ca8), (0x11cb7, 0x11cff), (0x11d07, 0x11d07), (0x11d0a, 0x11d0a),
    (0x11d37, 0x11d39), (0x11d3b, 0x11d3b), (0x11d3e, 0x11d3e), (0x11d48, 0x11d4f), (0x11d5a, 0x11d5f),
    (0x11d66, 0x11d66), (0x11d69, 0x11d69), (0x11d8f, 0x11d8f), (0x11d92, 0x11d92), (0x11d99, 0x11d9f),
    (0x11daa, 0x11edf), (0x11ef9, 0x11eff), (0x11f11, 0x11f11), (0x11f3b, 0x11f3d), (0x11f5b, 0x11faf),
    (0x11fb1, 0x11fbf), (0x11ff2, 0x11ffe), (0x1239a, 0x123ff), (0x1246f, 0x1246f), (0x12475, 0x1247f),
    (0x12544, 0x12f8f), (0x12ff3, 0x12fff), (0x13430, 0x1343f), (0x13456, 0x1345f), (0x143fb, 0x143ff),
    (0x14647, 0x160ff), (0x1613a, 0x167ff), (0x16a39, 0x16a3f), (0x16a5f, 0x16a5f), (0x16a6a, 0x16a6d),
    (0x16abf, 0x16abf), (0x16aca, 0x16acf), (0x16aee, 0x16aef), (0x16af6, 0x16aff), (0x16b46, 0x16b4f),
    (0x16b5a, 0x16b5a), (0x16b62, 0x16b62), (0x16b78, 0x16b7c), (0x16b90, 0x16d3f), (0x16d7a, 0x16e3f),
    (0x16e9b, 0x16eff), (0x16f4b, 0x16f4e), (0x16f88, 0x16f8e), (0x16fa0, 0x16fdf), (0x16fe5, 0x16fef),
    (0x16ff2, 0x16fff), (0x187f8, 0x187ff), (0x18cd6, 0x18cfe), (0x18d09, 0x1afef), (0x1aff4, 0x1aff4),
    (0x1affc, 0x1affc), (0x1afff, 0x1afff), (0x1b123, 0x1b131), (0x1b133, 0x1b14f), (0x1b153, 0x1b154),
    (0x1b156, 0x1b163), (0x1b168, 0x1b16f), (0x1b2fc, 0x1bbff), (0x1bc6b, 0x1bc6f), (0x1bc7d, 0x1bc7f),
    (0x1bc89, 0x1bc8f), (0x1bc9a, 0x1bc9b), (0x1bca0, 0x1cbff), (0x1ccfa, 0x1ccff), (0x1ceb4, 0x1ceff),
    (0x1cf2e, 0x1cf2f), (0x1cf47, 0x1cf4f), (0x1cfc4, 0x1cfff), (0x1d0f6, 0x1d0ff), (0x1d127, 0x1d128),
    (0x1d173, 0x1d17a), (0x1d1eb, 0x1d1ff), (0x1d246, 0x1d2bf), (0x1d2d4, 0x1d2df), (0x1d2f4, 0x1d2ff),
    (0x1d357, 0x1d35f), (0x1d379, 0x1d3ff), (0x1d455, 0x1d455), (0x1d49d, 0x1d49d), (0x1d4a0, 0x1d4a1),
    (0x1d4a3, 0x1d4a4), (0x1d4a7, 0x1d4a8), (0x1d4ad, 0x1d4ad), (0x1d4ba, 0x1d4ba), (0x1d4bc, 0x1d4bc),
    (0x1d4c4, 0x1d4c4), (0x1d506, 0x1d506), (0x1d50b, 0x1d50c), (0x1d515, 0x1d515), (0x1d51d, 0x1d51d),
    (0x1d53a, 0x1d53a), (0x1d53f, 0x1d53f), (0x1d545, 0x1d545), (0x1d547, 0x1d549), (0x1d551, 0x1d551),
    (0x1d6a6, 0x1d6a7), (0x1d7cc, 0x1d7cd), (0x1da8c, 0x1da9a), (0x1daa0, 0x1daa0), (0x1dab0, 0x1deff),
    (0x1df1f, 0x1df24), (0x1df2b, 0x1dfff), (0x1e007, 0x1e007), (0x1e019, 0x1e01a), (0x1e022, 0x1e022),
    (0x1e025, 0x1e025), (0x1e02b, 0x1e02f), (0x1e06e, 0x1e08e), (0x1e090, 0x1e0ff), (0x1e12d, 0x1e12f),
    (0x1e13e, 0x1e13f), (0x1e14a, 0x1e14d), (0x1e150, 0x1e28f), (0x1e2af, 0x1e2bf), (0x1e2fa, 0x1e2fe),
    (0x1e300, 0x1e4cf), (0x1e4fa, 0x1e5cf), (0x1e5fb, 0x1e5fe), (0x1e600, 0x1e7df), (0x1e7e7, 0x1e7e7),
    (0x1e7ec, 0x1e7ec), (0x1e7ef, 0x1e7ef), (0x1e7ff, 0x1e7ff), (0x1e8c5, 0x1e8c6), (0x1e8d7, 0x1e8ff),
    (0x1e94c, 0x1e94f), (0x1e95a, 0x1e95d), (0x1e960, 0x1ec70), (0x1ecb5, 0x1ed00), (0x1ed3e, 0x1edff),
    (0x1ee04, 0x1ee04), (0x1ee20, 0x1ee20), (0x1ee23, 0x1ee23), (0x1ee25, 0x1ee26), (0x1ee28, 0x1ee28),
    (0x1ee33, 0x1ee33), (0x1ee38, 0x1ee38), (0x1ee3a, 0x1ee3a), (0x1ee3c, 0x1ee41), (0x1ee43, 0x1ee46),
    (0x1ee48, 0x1ee48), (0x1ee4a, 0x1ee4a), (0x1ee4c, 0x1ee4c), (0x1ee50, 0x1ee50), (0x1ee53, 0x1ee53),
    (0x1ee55, 0x1ee56), (0x1ee58, 0x1ee58), (0x1ee5a, 0x1ee5a), (0x1ee5c, 0x1ee5c), (0x1ee5e, 0x1ee5e),
    (0x1ee60, 0x1ee60), (0x1ee63, 0x1ee63), (0x1ee65, 0x1ee66), (0x1ee6b, 0x1ee6b), (0x1ee73, 0x1ee73),
    (0x1ee78, 0x1ee78), (0x1ee7d, 0x1ee7d), (0x1ee7f, 0x1ee7f), (0x1ee8a, 0x1ee8a), (0x1ee9c, 0x1eea0),
    (0x1eea4, 0x1eea4), (0x1eeaa, 0x1eeaa), (0x1eebc, 0x1eeef), (0x1eef2, 0x1efff), (0x1f02c, 0x1f02f),
    (0x1f094, 0x1f09f), (0x1f0af, 0x1f0b0), (0x1f0c0, 0x1f0c0), (0x1f0d0, 0x1f0d0), (0x1f0f6, 0x1f0ff),
    (0x1f1ae, 0x1f1e5), (0x1f203, 0x1f20f), (0x1f23c, 0x1f23f), (0x1f249, 0x1f24f), (0x1f252, 0x1f25f),
    (0x1f266, 0x1f2ff), (0x1f6d8, 0x1f6db), (0x1f6ed, 0x1f6ef), (0x1f6fd, 0x1f6ff), (0x1f777, 0x1f77a),
    (0x1f7da, 0x1f7df), (0x1f7ec, 0x1f7ef), (0x1f7f1, 0x1f7ff), (0x1f80c, 0x1f80f), (0x1f848, 0x1f84f),
    (0x1f85a, 0x1f85f), (0x1f888, 0x1f88f), (0x1f8ae, 0x1f8af), (0x1f8bc, 0x1f8bf), (0x1f8c2, 0x1f8ff),
    (0x1fa54, 0x1fa5f), (0x1fa6e, 0x1fa6f), (0x1fa7d, 0x1fa7f), (0x1fa8a, 0x1fa8e), (0x1fac7, 0x1facd),
    (0x1fadd, 0x1fade), (0x1faea, 0x1faef), (0x1faf9, 0x1faff), (0x1fb93, 0x1fb93), (0x1fbfa, 0x1ffff),
    (0x2a6e0, 0x2a6ff), (0x2b73a, 0x2b73f), (0x2b81e, 0x2b81f), (0x2cea2, 0x2ceaf), (0x2ebe1, 0x2ebef),
    (0x2ee5e, 0x2f7ff), (0x2fa1e, 0x2ffff), (0x3134b, 0x3134f), (0x323b0, 0xe00ff), (0xe01f0, 0x10ffff),
];

/// Non-ASCII code points whose NFKC form holds one of `/?#@:` (Unicode 16.0.0):
/// urlsplit's `_checknetloc` refuses a netloc containing one.
const NFKC_URL_DELIMS: [u32; 19] = [
    0x2047, 0x2048, 0x2049, 0x2100, 0x2101, 0x2105, 0x2106, 0x2a74, 0xfe13, 0xfe16, 0xfe55, 0xfe56, 0xfe5f, 0xfe6b,
    0xff03, 0xff0f, 0xff1a, 0xff1f, 0xff20,
];

/// What `json.loads` raised: ValueError (JSONDecodeError, the int digit
/// limit) or RecursionError, which most `except` clauses here do not catch.
#[derive(Debug, Clone, Copy, PartialEq)]
enum JsonError {
    Value,
    Recursion,
}

const RECURSION: &str = "RecursionError: maximum recursion depth exceeded";

/// `json.loads(text)`, with Python's extensions: NaN, Infinity, -Infinity,
/// ints of any size (up to 4300 digits), floats that overflow to inf, and
/// unpaired surrogate escapes.
fn loads(text: &str) -> Result<PyValue, JsonError> {
    let cs: Vec<char> = text.chars().collect();
    let mut p = Parser { cs: &cs, pos: 0 };
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.pos != cs.len() {
        return Err(JsonError::Value);
    }
    Ok(v)
}

struct Parser<'a> {
    cs: &'a [char],
    pos: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.cs.get(self.pos).copied()
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) {
            self.pos += 1;
        }
    }

    fn lit(&mut self, word: &str) -> bool {
        let n = word.chars().count();
        if self.cs.len() >= self.pos + n && self.cs[self.pos..self.pos + n].iter().copied().eq(word.chars()) {
            self.pos += n;
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: usize) -> Result<PyValue, JsonError> {
        match self.peek().ok_or(JsonError::Value)? {
            '"' => {
                self.pos += 1;
                Ok(PyValue::Str(self.string()?))
            }
            '[' => {
                if depth + 1 > LOADS_DEPTH {
                    return Err(JsonError::Recursion);
                }
                self.pos += 1;
                self.ws();
                let mut items = Vec::new();
                if self.peek() == Some(']') {
                    self.pos += 1;
                    return Ok(PyValue::List(items));
                }
                loop {
                    items.push(self.value(depth + 1)?);
                    self.ws();
                    match self.peek() {
                        Some(',') => {
                            self.pos += 1;
                            self.ws();
                        }
                        Some(']') => {
                            self.pos += 1;
                            return Ok(PyValue::List(items));
                        }
                        _ => return Err(JsonError::Value),
                    }
                }
            }
            '{' => {
                if depth + 1 > LOADS_DEPTH {
                    return Err(JsonError::Recursion);
                }
                self.pos += 1;
                self.ws();
                let mut items: Vec<(PyStr, PyValue)> = Vec::new();
                let mut index: std::collections::HashMap<PyStr, usize> = std::collections::HashMap::new();
                if self.peek() == Some('}') {
                    self.pos += 1;
                    return Ok(PyValue::Dict(items));
                }
                loop {
                    if self.peek() != Some('"') {
                        return Err(JsonError::Value);
                    }
                    self.pos += 1;
                    let key = self.string()?;
                    self.ws();
                    if self.peek() != Some(':') {
                        return Err(JsonError::Value);
                    }
                    self.pos += 1;
                    self.ws();
                    let v = self.value(depth + 1)?;
                    // dict(pairs): a repeated key keeps its first place, last value.
                    match index.get(&key) {
                        Some(&i) => items[i].1 = v,
                        None => {
                            index.insert(key.clone(), items.len());
                            items.push((key, v));
                        }
                    }
                    self.ws();
                    match self.peek() {
                        Some(',') => {
                            self.pos += 1;
                            self.ws();
                        }
                        Some('}') => {
                            self.pos += 1;
                            return Ok(PyValue::Dict(items));
                        }
                        _ => return Err(JsonError::Value),
                    }
                }
            }
            'n' if self.lit("null") => Ok(PyValue::None),
            't' if self.lit("true") => Ok(PyValue::Bool(true)),
            'f' if self.lit("false") => Ok(PyValue::Bool(false)),
            'N' if self.lit("NaN") => Ok(PyValue::Float(f64::NAN)),
            'I' if self.lit("Infinity") => Ok(PyValue::Float(f64::INFINITY)),
            '-' | '0'..='9' => self.number(),
            _ => Err(JsonError::Value),
        }
    }

    /// `(-?(?:0|[1-9]\d*))(\.\d+)?([eE][-+]?\d+)?`, or `-Infinity`.
    fn number(&mut self) -> Result<PyValue, JsonError> {
        let start = self.pos;
        let digit = |c: Option<char>| matches!(c, Some('0'..='9'));
        if self.peek() == Some('-') {
            self.pos += 1;
            if self.lit("Infinity") {
                return Ok(PyValue::Float(f64::NEG_INFINITY));
            }
        }
        match self.peek() {
            Some('0') => self.pos += 1,
            Some('1'..='9') => {
                while digit(self.peek()) {
                    self.pos += 1;
                }
            }
            _ => return Err(JsonError::Value),
        }
        let mut is_float = false;
        if self.peek() == Some('.') && digit(self.cs.get(self.pos + 1).copied()) {
            self.pos += 1;
            while digit(self.peek()) {
                self.pos += 1;
            }
            is_float = true;
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            let mut q = self.pos + 1;
            if matches!(self.cs.get(q), Some('+' | '-')) {
                q += 1;
            }
            if digit(self.cs.get(q).copied()) {
                while digit(self.cs.get(q).copied()) {
                    q += 1;
                }
                self.pos = q;
                is_float = true;
            }
        }
        let text: String = self.cs[start..self.pos].iter().collect();
        if is_float {
            // float(): correctly rounded, overflowing to inf.
            text.parse::<f64>().map(PyValue::Float).map_err(|_| JsonError::Value)
        } else {
            if text.trim_start_matches('-').len() > INT_MAX_STR_DIGITS {
                return Err(JsonError::Value);
            }
            BigInt::parse_bytes(text.as_bytes(), 10).map(PyValue::Int).ok_or(JsonError::Value)
        }
    }

    fn hex4(&mut self) -> Result<u32, JsonError> {
        let mut n = 0u32;
        for _ in 0..4 {
            let d = self.peek().and_then(|c| c.to_digit(16)).ok_or(JsonError::Value)?;
            n = n * 16 + d;
            self.pos += 1;
        }
        Ok(n)
    }

    /// The rest of a string after its opening quote (strict: no raw controls).
    fn string(&mut self) -> Result<PyStr, JsonError> {
        let mut out = PyStr::new();
        loop {
            let c = self.peek().ok_or(JsonError::Value)?;
            self.pos += 1;
            match c {
                '"' => return Ok(out),
                '\\' => {
                    let e = self.peek().ok_or(JsonError::Value)?;
                    self.pos += 1;
                    let v = match e {
                        '"' => '"' as u32,
                        '\\' => '\\' as u32,
                        '/' => '/' as u32,
                        'b' => 8,
                        'f' => 12,
                        'n' => 10,
                        'r' => 13,
                        't' => 9,
                        'u' => {
                            let mut v = self.hex4()?;
                            // A surrogate pair joins; anything else stays as it is.
                            if (0xd800..=0xdbff).contains(&v)
                                && self.peek() == Some('\\')
                                && self.cs.get(self.pos + 1) == Some(&'u')
                            {
                                let save = self.pos;
                                self.pos += 2;
                                let lo = self.hex4()?;
                                if (0xdc00..=0xdfff).contains(&lo) {
                                    v = 0x10000 + ((v - 0xd800) << 10) + (lo - 0xdc00);
                                } else {
                                    self.pos = save;
                                }
                            }
                            v
                        }
                        _ => return Err(JsonError::Value),
                    };
                    out.push(v);
                }
                c if (c as u32) < 0x20 => return Err(JsonError::Value),
                c => out.push(c as u32),
            }
        }
    }
}

/// `json.dumps(v)`: default separators, `ensure_ascii`, floats as `repr`
/// with `Infinity`/`NaN`. Err is Python's RecursionError.
fn dumps(v: &PyValue) -> Result<String, JsonError> {
    let mut out = String::new();
    dump_into(&mut out, v, 0)?;
    Ok(out)
}

fn dump_into(out: &mut String, v: &PyValue, depth: u64) -> Result<(), JsonError> {
    match v {
        PyValue::None => out.push_str("null"),
        PyValue::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        PyValue::Int(n) => out.push_str(&n.to_string()),
        PyValue::Float(f) => out.push_str(&crate::util::py_float_repr(*f)),
        PyValue::Str(s) => dump_str(out, s),
        PyValue::List(items) => {
            let depth = depth + DUMPS_LIST;
            if depth > DUMPS_BUDGET {
                return Err(JsonError::Recursion);
            }
            out.push('[');
            for (i, x) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                dump_into(out, x, depth)?;
            }
            out.push(']');
        }
        PyValue::Dict(items) => {
            let depth = depth + DUMPS_DICT;
            if depth > DUMPS_BUDGET {
                return Err(JsonError::Recursion);
            }
            out.push('{');
            for (i, (k, x)) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                dump_str(out, k);
                out.push_str(": ");
                dump_into(out, x, depth)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

fn dump_str(out: &mut String, s: &[u32]) {
    out.push('"');
    for &c in s {
        match c {
            0x22 => out.push_str("\\\""),
            0x5c => out.push_str("\\\\"),
            0x0a => out.push_str("\\n"),
            0x0d => out.push_str("\\r"),
            0x09 => out.push_str("\\t"),
            0x08 => out.push_str("\\b"),
            0x0c => out.push_str("\\f"),
            0x20..=0x7e => out.push(char::from_u32(c).unwrap_or('?')),
            0x10000.. => {
                let v = c - 0x10000;
                out.push_str(&format!("\\u{:04x}\\u{:04x}", 0xd800 + (v >> 10), 0xdc00 + (v & 0x3ff)));
            }
            _ => out.push_str(&format!("\\u{c:04x}")),
        }
    }
    out.push('"');
}

// --- Python's numbers -------------------------------------------------------

/// An integral f64 as an exact int.
fn float_to_int(f: f64) -> BigInt {
    BigInt::parse_bytes(format!("{f:.0}").as_bytes(), 10).unwrap_or_default()
}

/// `float(int)`: correctly rounded, OverflowError past the largest float.
fn int_to_float(n: &BigInt) -> Result<f64, String> {
    match n.to_string().parse::<f64>() {
        Ok(f) if f.is_finite() => Ok(f),
        _ => Err("OverflowError: int too large to convert to float".into()),
    }
}

/// Python compares a float with an int exactly, never by rounding the int.
fn cmp_float_int(f: f64, n: &BigInt) -> Option<Ordering> {
    if f.is_nan() {
        return None;
    }
    if f.is_infinite() {
        return Some(if f > 0.0 { Ordering::Greater } else { Ordering::Less });
    }
    let fl = f.floor();
    match float_to_int(fl).cmp(n) {
        Ordering::Equal if f > fl => Some(Ordering::Greater),
        o => Some(o),
    }
}

/// `x - v` / `v - x` with `x` a float: the number `v` as a float.
fn arith_float(v: &PyValue, op: &str, float_left: bool) -> Result<f64, String> {
    match v {
        PyValue::Int(n) => int_to_float(n),
        PyValue::Float(f) => Ok(*f),
        PyValue::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        other => Err(if float_left {
            format!("TypeError: unsupported operand type(s) for {op}: 'float' and '{}'", other.type_name())
        } else {
            format!("TypeError: unsupported operand type(s) for {op}: '{}' and 'float'", other.type_name())
        }),
    }
}

/// `f > v`.
fn float_gt(f: f64, v: &PyValue) -> Result<bool, String> {
    match v {
        PyValue::Int(n) => Ok(cmp_float_int(f, n) == Some(Ordering::Greater)),
        PyValue::Float(g) => Ok(f > *g),
        PyValue::Bool(b) => Ok(f > if *b { 1.0 } else { 0.0 }),
        other => Err(format!("TypeError: '>' not supported between instances of 'float' and '{}'", other.type_name())),
    }
}

/// `v > n`.
fn gt_int(v: &PyValue, n: i64) -> Result<bool, String> {
    let n = BigInt::from(n);
    match v {
        PyValue::Int(m) => Ok(*m > n),
        PyValue::Float(f) => Ok(cmp_float_int(*f, &n) == Some(Ordering::Greater)),
        PyValue::Bool(b) => Ok(BigInt::from(i64::from(*b)) > n),
        other => Err(format!("TypeError: '>' not supported between instances of '{}' and 'int'", other.type_name())),
    }
}


/// `str.isspace()` for one code point.
fn py_isspace(c: u32) -> bool {
    matches!(c, 0x09..=0x0d | 0x1c..=0x20 | 0x85 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000)
}

/// `str.strip()`.
fn py_strip(s: &[u32]) -> &[u32] {
    let start = s.iter().position(|&c| !py_isspace(c)).unwrap_or(s.len());
    let end = s.iter().rposition(|&c| !py_isspace(c)).map_or(start, |i| i + 1);
    &s[start..end]
}

/// The first code point of every run of ten Unicode decimal digits (Nd,
/// Unicode 16), which `int()` reads as 0-9.
const DECIMAL_ZEROS: [u32; 76] = [
    0x30, 0x660, 0x6f0, 0x7c0, 0x966, 0x9e6, 0xa66, 0xae6, 0xb66, 0xbe6, 0xc66, 0xce6, 0xd66, 0xde6, 0xe50,
    0xed0, 0xf20, 0x1040, 0x1090, 0x17e0, 0x1810, 0x1946, 0x19d0, 0x1a80, 0x1a90, 0x1b50, 0x1bb0, 0x1c40,
    0x1c50, 0xa620, 0xa8d0, 0xa900, 0xa9d0, 0xa9f0, 0xaa50, 0xabf0, 0xff10, 0x104a0, 0x10d30, 0x10d40,
    0x11066, 0x110f0, 0x11136, 0x111d0, 0x112f0, 0x11450, 0x114d0, 0x11650, 0x116c0, 0x116d0, 0x116da,
    0x11730, 0x118e0, 0x11950, 0x11bf0, 0x11c50, 0x11d50, 0x11da0, 0x11f50, 0x16130, 0x16a60, 0x16ac0,
    0x16b50, 0x16d70, 0x1ccf0, 0x1d7ce, 0x1d7d8, 0x1d7e2, 0x1d7ec, 0x1d7f6, 0x1e140, 0x1e2f0, 0x1e4f0,
    0x1e5f1, 0x1e950, 0x1fbf0,
];

/// `int(s)` for a str: surrounding whitespace, a sign, `_` between digits,
/// any Unicode decimal digits, any size up to 4300 digits. None is ValueError.
fn py_int(s: &[u32]) -> Option<BigInt> {
    // Non-ASCII spaces become ' ', decimal digits their ASCII digit; ASCII
    // stays as it is (so \x1c-\x1f are *not* whitespace here).
    let t: Vec<u8> = s
        .iter()
        .map(|&c| {
            if c < 0x80 {
                c as u8
            } else if py_isspace(c) {
                b' '
            } else if let Some(z) = DECIMAL_ZEROS.iter().find(|&&z| c >= z && c < z + 10) {
                b'0' + (c - z) as u8
            } else {
                b'?'
            }
        })
        .collect();
    // Py_ISSPACE: \t \n \v \f \r and space (trim_ascii leaves \v).
    let ws = |b: &u8| matches!(b, b'\t' | b'\n' | 0x0b | 0x0c | b'\r' | b' ');
    let start = t.iter().position(|b| !ws(b)).unwrap_or(t.len());
    let end = t.iter().rposition(|b| !ws(b)).map_or(start, |i| i + 1);
    let t = &t[start..end];
    let (neg, body) = match t.first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    if body.is_empty() || !body[0].is_ascii_digit() || !body[body.len() - 1].is_ascii_digit() {
        return None;
    }
    let mut digits = Vec::with_capacity(body.len());
    let mut prev_us = false;
    for &b in body {
        match b {
            b'0'..=b'9' => {
                digits.push(b);
                prev_us = false;
            }
            b'_' if !prev_us => prev_us = true,
            _ => return None,
        }
    }
    if digits.len() > INT_MAX_STR_DIGITS {
        return None;
    }
    let n = BigInt::parse_bytes(&digits, 10)?;
    Some(if neg { -n } else { n })
}

// --- the token --------------------------------------------------------------

/// The user's existing gh credential, if they have one.
///
/// Read through `gh auth token` rather than from a config file, so it keeps
/// working wherever gh stores it -- here that is the system keyring. Held in
/// memory only.
///
/// Output from gh that is not UTF-8 raises in Python; this form reads it as
/// no token. Use [`try_token`] and stop on Err, as the Python caller would.
#[deprecated(note = "the Python raises out of token() when gh's output is not UTF-8; use try_token and stop on Err")]
pub fn token() -> String {
    try_token().unwrap_or_default()
}

/// `token()`, raising where the Python does: `text=True` decodes gh's output
/// strictly, and a UnicodeDecodeError is not among the exceptions caught.
pub fn try_token() -> Result<String, String> {
    if !crate::settings::get_bool("net_gh_token") {
        return Ok(String::new()); // Preferences → Privacy says not to
    }
    let mut memo = _TOKEN.lock().unwrap_or_else(|e| e.into_inner());
    if memo.checked {
        return Ok(memo.token.clone().unwrap_or_default());
    }
    memo.checked = true;
    match _gh_auth_token()? {
        // OSError / SubprocessError
        None => memo.token = Some(String::new()),
        Some((ok, stdout)) => {
            let tok = py_strip(&stdout);
            memo.token = Some(if ok && tok.len() > 20 { ps_string(tok) } else { String::new() });
        }
    }
    Ok(memo.token.clone().unwrap_or_default())
}

/// `subprocess.run(["gh", "auth", "token"], capture_output=True, text=True,
/// timeout=15)`: `(returncode == 0, stdout)`, None for OSError or a timeout,
/// Err for UnicodeDecodeError. Not `util::run`, which decodes lossily: the
/// Python's strict decoding (and its newline translation) decides here.
fn _gh_auth_token() -> Result<Option<(bool, PyStr)>, String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut child = match Command::new("gh")
        .args(["auth", "token"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return Ok(None),
    };
    let drain = |p: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut s = Vec::new();
            if let Some(mut p) = p {
                let _ = p.read_to_end(&mut s);
            }
            s
        })
    };
    let out_t = drain(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err_t = drain(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) if start.elapsed() >= std::time::Duration::from_secs(15) => {
                let _ = child.kill();
                let _ = child.wait();
                return Ok(None); // TimeoutExpired
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
            Err(_) => return Ok(None),
        }
    };
    let out = out_t.join().unwrap_or_default();
    let err = err_t.join().unwrap_or_default();
    let decode = |b: &[u8]| -> Result<PyStr, String> {
        let s = std::str::from_utf8(b).map_err(|e| format!("UnicodeDecodeError: 'utf-8' codec can't decode: {e}"))?;
        Ok(ps(&s.replace("\r\n", "\n").replace('\r', "\n")))
    };
    let stdout = decode(&out)?;
    decode(&err)?;
    Ok(Some((status.code() == Some(0), stdout)))
}

/// Forget the memoised token so the next `token()` asks again (the tests set
/// `ghcache._token_checked = False` for this).
pub fn reset_token() {
    let mut memo = _TOKEN.lock().unwrap_or_else(|e| e.into_inner());
    memo.checked = false;
}

// --- files ------------------------------------------------------------------

fn _path(url: &str) -> PathBuf {
    use sha1::{Digest, Sha1};
    dir().join(format!("{}.json", hex::encode(Sha1::digest(url.as_bytes()))))
}

/// Whatever JSON the cache file holds (None when missing or unreadable).
/// Err is a RecursionError, which `except (OSError, ValueError)` lets through.
fn _read(url: &str) -> Result<Option<PyValue>, String> {
    let Ok(text) = std::fs::read_to_string(_path(url)) else {
        return Ok(None);
    };
    match loads(&text) {
        Ok(v) => Ok(Some(v)),
        Err(JsonError::Value) => Ok(None),
        Err(JsonError::Recursion) => Err(RECURSION.into()),
    }
}

/// `DIR.mkdir(...)`, `json.dumps(entry)` into `<sha>.tmp`, then replace.
/// OSError is ignored; Err is json.dumps's RecursionError.
fn _write(url: &str, entry: &PyValue) -> Result<(), String> {
    let _ = std::fs::create_dir_all(dir());
    let text = dumps(entry).map_err(|_| RECURSION.to_string())?;
    let p = _path(url);
    let tmp = p.with_extension("tmp");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, &p);
    }
    Ok(())
}

/// Whatever JSON the rate file holds, `{}` when missing or unreadable.
fn _state() -> Result<PyValue, String> {
    let Ok(text) = std::fs::read_to_string(state_file()) else {
        return Ok(PyValue::Dict(Vec::new()));
    };
    match loads(&text) {
        Ok(v) => Ok(v),
        Err(JsonError::Value) => Ok(PyValue::Dict(Vec::new())),
        Err(JsonError::Recursion) => Err(RECURSION.into()),
    }
}

/// Record what the response said about the remaining quota.
fn _note_limits(headers: &Hdrs) {
    _note_limits_with(|name| headers.get(name));
}

/// `_note_limits` over any header lookup (`headers.get(name)`).
fn _note_limits_with(get: impl Fn(&str) -> Option<PyStr>) {
    let remaining = get("X-RateLimit-Remaining");
    let reset = get("X-RateLimit-Reset");
    let Some(remaining) = remaining else {
        return;
    };
    // The directory comes first, so it is made even when a value is bad.
    if std::fs::create_dir_all(cache()).is_err() {
        return;
    }
    // int(...) raising ValueError falls through to `pass`; `x or 0` makes a
    // missing or empty header 0.
    let or_zero = |v: Option<PyStr>| match v.filter(|s| !s.is_empty()) {
        Some(s) => py_int(&s),
        None => Some(BigInt::from(0)),
    };
    let Some(remaining) = py_int(&remaining) else {
        return;
    };
    let Some(limit) = or_zero(get("X-RateLimit-Limit")) else {
        return;
    };
    let Some(reset) = or_zero(reset) else {
        return;
    };
    let state = PyValue::Dict(vec![
        (ps("remaining"), PyValue::Int(remaining)),
        (ps("limit"), PyValue::Int(limit)),
        (ps("reset"), PyValue::Int(reset)),
        (ps("at"), PyValue::Float(crate::util::now())),
    ]);
    if let Ok(text) = dumps(&state) {
        let _ = std::fs::write(state_file(), text);
    }
}

// --- the budget ---------------------------------------------------------------

/// What is left, as far as the last response told us (the dict `budget()`
/// returns in Python). The values are whatever the rate file held, as Python
/// passes them through: usually ints, but a float or a string stays one.
#[derive(Debug, Clone, PartialEq)]
pub struct Budget {
    pub remaining: PyValue,
    pub limit: PyValue,
    pub reset: PyValue,
    pub known: bool,
}

impl Budget {
    /// `{"remaining", "limit", "reset", "known"}` (see [`PyValue::to_value`]).
    pub fn to_value(&self) -> Value {
        serde_json::json!({
            "remaining": self.remaining.to_value(),
            "limit": self.limit.to_value(),
            "reset": self.reset.to_value(),
            "known": self.known,
        })
    }

}

/// What is left, as far as the last response told us. Err where Python
/// raises: a rate file that is not an object, a reset that is not a number.
pub fn budget() -> Result<Budget, String> {
    deep(_budget)
}

fn _budget() -> Result<Budget, String> {
    let st = _state()?;
    let d = st.dict()?;
    let get = |k: &str| d.iter().find(|(kk, _)| *kk == ps(k)).map(|(_, v)| v.clone());
    let reset = get("reset").unwrap_or(PyValue::int(0));
    if reset.truthy() && float_gt(crate::util::now(), &reset)? {
        // The window rolled over; the recorded number is meaningless now.
        let remaining = match get("limit") {
            Some(l) if l.truthy() => l,
            _ => PyValue::int(if !try_token()?.is_empty() { 5000 } else { 60 }),
        };
        return Ok(Budget { remaining, limit: get("limit").unwrap_or(PyValue::int(0)), reset: PyValue::int(0), known: false });
    }
    Ok(Budget {
        remaining: get("remaining").unwrap_or(PyValue::int(-1)),
        limit: get("limit").unwrap_or(PyValue::int(0)),
        reset,
        known: get("remaining").is_some(),
    })
}

pub fn can_spend() -> Result<bool, String> {
    deep(_can_spend)
}

fn _can_spend() -> Result<bool, String> {
    let b = _budget()?;
    Ok(!b.known || gt_int(&b.remaining, RESERVE)?)
}

// --- get_json -----------------------------------------------------------------

/// A `ttl` as Python takes it: an int or a float, negative included.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Ttl {
    Int(i128),
    Float(f64),
}

impl From<u64> for Ttl {
    fn from(v: u64) -> Ttl {
        Ttl::Int(v.into())
    }
}
impl From<i64> for Ttl {
    fn from(v: i64) -> Ttl {
        Ttl::Int(v.into())
    }
}
impl From<u32> for Ttl {
    fn from(v: u32) -> Ttl {
        Ttl::Int(v.into())
    }
}
impl From<i32> for Ttl {
    fn from(v: i32) -> Ttl {
        Ttl::Int(v.into())
    }
}
impl From<f64> for Ttl {
    fn from(v: f64) -> Ttl {
        Ttl::Float(v)
    }
}

impl Ttl {
    /// `age < ttl`.
    fn above(self, age: f64) -> bool {
        match self {
            Ttl::Int(n) => cmp_float_int(age, &BigInt::from(n)) == Some(Ordering::Less),
            Ttl::Float(f) => age < f,
        }
    }
}

/// `entry.get("body") if (entry and allow_stale) else None`.
fn _stale(entry: &PyValue, allow_stale: bool) -> PyValue {
    if entry.truthy() && allow_stale {
        entry.get("body").cloned().unwrap_or(PyValue::None)
    } else {
        PyValue::None
    }
}

fn _some(v: PyValue) -> Option<Value> {
    match v {
        PyValue::None => None,
        v => Some(v.to_value()),
    }
}

/// A cached GitHub API GET. Returns parsed JSON, or None.
///
/// Never raises for network or quota problems: callers want "what do we know
/// about this", and a listing that is six hours old beats no listing.
///
/// Python's keyword defaults are `ttl=DEFAULT_TTL`, `allow_stale=True`,
/// `accept=ACCEPT`. A JSON `null` body comes back as `None`. The body is a
/// `serde_json::Value` (see [`PyValue::to_value`] for what that cannot
/// hold; [`try_get_json_py`] has the exact value). Where the Python raises
/// (a cache or rate file of the wrong shape, a URL with no scheme) this
/// answers None.
#[deprecated(note = "the Python raises out of get_json() for a broken cache or rate file or a URL with no scheme; use try_get_json and stop on Err")]
pub fn get_json(url: &str, ttl: impl Into<Ttl>, allow_stale: bool, accept: &str) -> Option<Value> {
    try_get_json(url, ttl, allow_stale, accept).ok().flatten()
}

/// `get_json`, raising where the Python does.
pub fn try_get_json(url: &str, ttl: impl Into<Ttl>, allow_stale: bool, accept: &str) -> Result<Option<Value>, String> {
    let ttl = ttl.into();
    deep(|| _get_json(url, ttl, allow_stale, accept).map(_some))
}

/// `get_json`, raising where the Python does, with the body exactly as
/// Python's json reads it (ints of any size, inf/nan, lone surrogates).
pub fn try_get_json_py(url: &str, ttl: impl Into<Ttl>, allow_stale: bool, accept: &str) -> Result<PyValue, String> {
    let ttl = ttl.into();
    deep(|| _get_json(url, ttl, allow_stale, accept))
}

fn _get_json(url: &str, ttl: Ttl, allow_stale: bool, accept: &str) -> Result<PyValue, String> {
    let _guard = _LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut entry = _read(url)?.unwrap_or(PyValue::None);
    let now = crate::util::now();

    if entry.truthy() {
        let at = entry.dict()?.iter().find(|(k, _)| *k == ps("at")).map(|(_, v)| v.clone()).unwrap_or(PyValue::int(0));
        if ttl.above(now - arith_float(&at, "-", true)?) {
            return Ok(entry.get("body").cloned().unwrap_or(PyValue::None));
        }
    }

    if !_can_spend()? {
        // Out of budget. A 304 would be free, but a miss would not, and
        // there is no way to know which it is before spending it.
        return Ok(_stale(&entry, allow_stale));
    }

    let mut headers: Vec<(String, PyValue)> = vec![
        ("User-Agent".into(), PyValue::str(UA)),
        ("Accept".into(), PyValue::str(accept)),
        ("X-GitHub-Api-Version".into(), PyValue::str("2022-11-28")),
    ];
    let tok = try_token()?;
    if !tok.is_empty() {
        headers.push(("Authorization".into(), PyValue::str(&format!("Bearer {tok}"))));
    }
    if entry.truthy() {
        // The values go out as they are: http.client str()s an int or bool.
        if let Some(etag) = entry.get("etag").filter(|v| v.truthy()) {
            headers.push(("If-None-Match".into(), etag.clone()));
        } else if let Some(modified) = entry.get("modified").filter(|v| v.truthy()) {
            headers.push(("If-Modified-Since".into(), modified.clone()));
        }
    }

    // urllib.request.Request(url, headers=headers): outside the try, so a
    // bad URL raises.
    let req = _request(url, headers)?;
    match _urlopen(req) {
        Fetched::Ok { raw, headers } => {
            _note_limits(&headers);
            // json.loads(raw.decode() or "null")
            let Ok(text) = std::str::from_utf8(&raw) else {
                return Ok(_stale(&entry, allow_stale));
            };
            let Ok(body) = loads(if text.is_empty() { "null" } else { text }) else {
                return Ok(_stale(&entry, allow_stale));
            };
            let fresh = PyValue::Dict(vec![
                (ps("at"), PyValue::Float(now)),
                (ps("body"), body),
                (ps("etag"), PyValue::Str(headers.get("ETag").unwrap_or_default())),
                (ps("modified"), PyValue::Str(headers.get("Last-Modified").unwrap_or_default())),
            ]);
            if _write(url, &fresh).is_err() {
                return Ok(_stale(&entry, allow_stale));
            }
            Ok(fresh.get("body").cloned().unwrap_or(PyValue::None))
        }
        // except urllib.error.HTTPError
        Fetched::HttpError { code, headers } => {
            _note_limits(&headers);
            if code == 304 && entry.truthy() {
                // Unchanged, and this cost nothing. Reset the clock so the
                // next call inside the TTL does not even ask.
                entry.set("at", PyValue::Float(now));
                _write(url, &entry)?;
                return Ok(entry.get("body").cloned().unwrap_or(PyValue::None));
            }
            if entry.truthy() && allow_stale && [403, 429, 500, 502, 503].contains(&code) {
                return Ok(entry.get("body").cloned().unwrap_or(PyValue::None));
            }
            Ok(_stale(&entry, allow_stale))
        }
        Fetched::Failed => Ok(_stale(&entry, allow_stale)),
    }
}

/// What `urlopen` ended in.
enum Fetched {
    /// A 2xx response (or a data:/file: URL), body read in full.
    Ok { raw: Vec<u8>, headers: Hdrs },
    /// `urllib.error.HTTPError`, with the headers it carries.
    HttpError { code: u16, headers: Hdrs },
    /// Any other exception.
    Failed,
}

// --- urllib.parse ---------------------------------------------------------------
//
// The URL handling is urllib's own, ported as it is, because the bytes it puts
// on the wire (and what it refuses) decide what GitHub -- or a redirect --
// answers.

/// `unwrap()`: `<URL:...>` wrappers and surrounding whitespace removed.
fn _unwrap(url: &str) -> String {
    let strip = |s: &str| ps_string(py_strip(&ps(s)));
    let mut u = strip(url);
    if u.starts_with('<') && u.ends_with('>') && u.len() >= 2 {
        u = strip(&u[1..u.len() - 1]);
    }
    if let Some(rest) = u.strip_prefix("URL:") {
        u = strip(rest);
    }
    u
}

/// `_splittype`: `'([^/:]+):(.*)'`, the scheme lowercased.
fn _splittype(url: &str) -> Option<(String, &str)> {
    match url.find([':', '/']) {
        Some(i) if i > 0 && url.as_bytes()[i] == b':' => Some((url[..i].to_lowercase(), &url[i + 1..])),
        _ => None,
    }
}

/// `_splithost`: `'//([^/#?]*)(.*)'`; a path that does not start with `/`
/// gets one.
fn _splithost(url: &str) -> (Option<String>, String) {
    let Some(rest) = url.strip_prefix("//") else {
        return (None, url.to_owned());
    };
    let i = rest.find(['/', '#', '?']).unwrap_or(rest.len());
    let path = &rest[i..];
    let path = if !path.is_empty() && !path.starts_with('/') { format!("/{path}") } else { path.to_owned() };
    (Some(rest[..i].to_owned()), path)
}

/// `_splitport`: `'(.*):([0-9]*)'` (fullmatch), the port only when it has digits.
fn _splitport(host: &str) -> (String, Option<String>) {
    if let Some(i) = host.rfind(':') {
        let port = &host[i + 1..];
        if port.bytes().all(|b| b.is_ascii_digit()) {
            return (host[..i].to_owned(), (!port.is_empty()).then(|| port.to_owned()));
        }
    }
    (host.to_owned(), None)
}

/// `unquote_to_bytes`: `%XX` decoded, anything else as UTF-8.
fn _unquote_to_bytes(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() && b[i + 1].is_ascii_hexdigit() && b[i + 2].is_ascii_hexdigit() {
            let h = |c: u8| (c as char).to_digit(16).unwrap_or(0) as u8;
            out.push(h(b[i + 1]) * 16 + h(b[i + 2]));
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

/// `unquote(s)`: `%XX` runs decoded as UTF-8, invalid sequences replaced.
fn _unquote(s: &str) -> String {
    if !s.contains('%') {
        return s.to_owned();
    }
    // Only the ASCII runs are unquoted; the rest is kept as it is.
    let mut out = String::new();
    let mut run = String::new();
    for c in s.chars() {
        if c.is_ascii() {
            run.push(c);
        } else {
            if !run.is_empty() {
                out.push_str(&String::from_utf8_lossy(&_unquote_to_bytes(&run)));
                run.clear();
            }
            out.push(c);
        }
    }
    if !run.is_empty() {
        out.push_str(&String::from_utf8_lossy(&_unquote_to_bytes(&run)));
    }
    out
}

/// `quote(s, encoding="iso-8859-1", safe=string.punctuation)`: None where
/// latin-1 cannot encode a character (UnicodeEncodeError).
fn _quote_punct(s: &str) -> Option<String> {
    let mut out = String::new();
    for c in s.chars() {
        let b = u8::try_from(u32::from(c)).ok()?;
        if b.is_ascii_alphanumeric() || b.is_ascii_punctuation() {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    Some(out)
}

const USES_RELATIVE: [&str; 20] = [
    "", "ftp", "http", "gopher", "nntp", "imap", "wais", "file", "https", "shttp", "mms", "prospero", "rtsp", "rtsps",
    "rtspu", "sftp", "svn", "svn+ssh", "ws", "wss",
];
const USES_NETLOC: [&str; 27] = [
    "", "ftp", "http", "gopher", "nntp", "telnet", "imap", "wais", "file", "mms", "https", "shttp", "snews",
    "prospero", "rtsp", "rtsps", "rtspu", "rsync", "svn", "svn+ssh", "sftp", "nfs", "git", "git+ssh", "ws", "wss",
    "itms-services",
];
const USES_PARAMS: [&str; 15] =
    ["", "ftp", "hdl", "prospero", "http", "imap", "https", "shttp", "rtsp", "rtsps", "rtspu", "sip", "sips", "mms", "sftp"];

/// `_urlsplit(url, None)`: absent parts are None.
#[derive(Debug, Clone, PartialEq)]
struct Split {
    scheme: Option<String>,
    netloc: Option<String>,
    path: String,
    query: Option<String>,
    fragment: Option<String>,
}

/// `_urlsplit(url, scheme=None, allow_fragments=True)`; Err is its ValueError.
fn _urlsplit(url: &str) -> Result<Split, String> {
    let url: String = url
        .trim_start_matches(|c: char| c <= ' ')
        .chars()
        .filter(|c| !matches!(c, '\t' | '\r' | '\n'))
        .collect();
    let mut rest = url.as_str();
    let mut scheme = None;
    if let Some(i) = rest.find(':') {
        let first = rest.chars().next().unwrap_or(' ');
        if i > 0
            && first.is_ascii_alphabetic()
            && rest[..i].chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
        {
            scheme = Some(rest[..i].to_ascii_lowercase());
            rest = &rest[i + 1..];
        }
    }
    let mut netloc = None;
    if let Some(r) = rest.strip_prefix("//") {
        let i = r.find(['/', '?', '#']).unwrap_or(r.len());
        let n = &r[..i];
        rest = &r[i..];
        if n.contains('[') != n.contains(']') {
            return Err("ValueError: Invalid IPv6 URL".into());
        }
        if n.contains('[') && n.contains(']') {
            _check_bracketed_netloc(n)?;
        }
        netloc = Some(n.to_owned());
    }
    let (rest, fragment) = match rest.split_once('#') {
        Some((a, b)) => (a, Some(b.to_owned())),
        None => (rest, None),
    };
    let (path, query) = match rest.split_once('?') {
        Some((a, b)) => (a.to_owned(), Some(b.to_owned())),
        None => (rest.to_owned(), None),
    };
    if let Some(n) = &netloc {
        _checknetloc(n)?;
    }
    Ok(Split { scheme, netloc, path, query, fragment })
}

/// `_checknetloc`: a netloc whose NFKC form grows URL delimiters is refused.
fn _checknetloc(netloc: &str) -> Result<(), String> {
    if netloc.is_ascii() {
        return Ok(());
    }
    if netloc.chars().any(|c| NFKC_URL_DELIMS.binary_search(&u32::from(c)).is_ok()) {
        return Err(format!("ValueError: netloc '{netloc}' contains invalid characters under NFKC normalization"));
    }
    Ok(())
}

/// `_check_bracketed_netloc` and `_check_bracketed_host`.
fn _check_bracketed_netloc(netloc: &str) -> Result<(), String> {
    let bad = || Err("ValueError: Invalid IPv6 URL".to_string());
    let hostport = netloc.rsplit_once('@').map_or(netloc, |(_, h)| h);
    let hostname = match hostport.split_once('[') {
        Some((before, bracketed)) => {
            if !before.is_empty() {
                return bad();
            }
            let (h, port) = bracketed.split_once(']').unwrap_or((bracketed, ""));
            if !port.is_empty() && !port.starts_with(':') {
                return bad();
            }
            h
        }
        None => hostport.split_once(':').map_or(hostport, |(h, _)| h),
    };
    if hostname.starts_with(['v', 'V']) {
        // \A[vV][a-fA-F0-9]+\..+\z
        let rest = &hostname[1..];
        let hex = rest.find(|c: char| !c.is_ascii_hexdigit()).unwrap_or(rest.len());
        let ok = hex > 0 && rest[hex..].starts_with('.') && !rest[hex + 1..].is_empty() && !rest[hex + 1..].contains('\n');
        return if ok { Ok(()) } else { Err("ValueError: IPvFuture address is invalid".into()) };
    }
    // ipaddress.ip_address(): an IPv4 address is refused in brackets, an
    // IPv6 one may carry a %scope.
    if hostname.parse::<std::net::Ipv4Addr>().is_ok() {
        return Err("ValueError: An IPv4 address cannot be in brackets".into());
    }
    let (addr, scope) = match hostname.split_once('%') {
        Some((a, s)) => (a, Some(s)),
        None => (hostname, None),
    };
    if scope.is_some_and(|s| s.is_empty() || s.contains('%')) || addr.parse::<std::net::Ipv6Addr>().is_err() {
        return Err(format!("ValueError: {} does not appear to be an IPv4 or IPv6 address", str_repr(&ps(hostname))));
    }
    Ok(())
}

/// `urlparse(url)`: the six parts, absent ones empty.
struct Parsed {
    scheme: String,
    netloc: String,
    path: String,
    params: String,
    query: String,
    fragment: String,
}

fn _urlparse(url: &str) -> Result<Parsed, String> {
    let s = _urlsplit(url)?;
    let scheme = s.scheme.unwrap_or_default();
    let (path, params) = if USES_PARAMS.contains(&scheme.as_str()) && s.path.contains(';') {
        // _splitparams(url, allow_none=True)
        let i = match s.path.rfind('/') {
            Some(slash) => s.path[slash..].find(';').map(|j| slash + j),
            None => s.path.find(';'),
        };
        match i {
            Some(i) => (s.path[..i].to_owned(), s.path[i + 1..].to_owned()),
            None => (s.path.clone(), String::new()),
        }
    } else {
        (s.path.clone(), String::new())
    };
    Ok(Parsed {
        scheme,
        netloc: s.netloc.unwrap_or_default(),
        path,
        params,
        query: s.query.unwrap_or_default(),
        fragment: s.fragment.unwrap_or_default(),
    })
}

/// `_urlunsplit`.
fn _urlunsplit(scheme: Option<&str>, netloc: Option<&str>, url: &str, query: Option<&str>, fragment: Option<&str>) -> String {
    let mut url = url.to_owned();
    if let Some(n) = netloc {
        if !url.is_empty() && !url.starts_with('/') {
            url = format!("/{url}");
        }
        url = format!("//{n}{url}");
    } else if url.starts_with("//") {
        url = format!("//{url}");
    }
    if let Some(s) = scheme.filter(|s| !s.is_empty()) {
        url = format!("{s}:{url}");
    }
    if let Some(q) = query {
        url = format!("{url}?{q}");
    }
    if let Some(f) = fragment {
        url = format!("{url}#{f}");
    }
    url
}

/// `urlunparse(parts)`.
fn _urlunparse(p: &Parsed) -> String {
    let netloc = if p.netloc.is_empty() {
        (!p.scheme.is_empty() && USES_NETLOC.contains(&p.scheme.as_str()) && (p.path.is_empty() || p.path.starts_with('/')))
            .then_some("")
    } else {
        Some(p.netloc.as_str())
    };
    let url = if p.params.is_empty() { p.path.clone() } else { format!("{};{}", p.path, p.params) };
    let opt = |s: &str| (!s.is_empty()).then(|| s.to_owned());
    _urlunsplit(Some(&p.scheme), netloc, &url, opt(&p.query).as_deref(), opt(&p.fragment).as_deref())
}

/// `urljoin(base, url)`.
fn _urljoin(base: &str, url: &str) -> Result<String, String> {
    if base.is_empty() {
        return Ok(url.to_owned());
    }
    if url.is_empty() {
        return Ok(base.to_owned());
    }
    let b = _urlsplit(base)?;
    let u = _urlsplit(url)?;
    let scheme = u.scheme.clone().or(b.scheme.clone());
    if scheme != b.scheme || scheme.as_deref().is_some_and(|s| !USES_RELATIVE.contains(&s)) {
        return Ok(url.to_owned());
    }
    let mut netloc = u.netloc.clone();
    if scheme.as_deref().is_none_or(|s| s.is_empty() || USES_NETLOC.contains(&s)) {
        if netloc.as_deref().is_some_and(|n| !n.is_empty()) {
            return Ok(_urlunsplit(scheme.as_deref(), netloc.as_deref(), &u.path, u.query.as_deref(), u.fragment.as_deref()));
        }
        netloc = b.netloc.clone();
    }
    if u.path.is_empty() {
        let mut query = u.query.clone();
        let mut fragment = u.fragment.clone();
        if query.is_none() {
            query = b.query.clone();
            if fragment.is_none() {
                fragment = b.fragment.clone();
            }
        }
        return Ok(_urlunsplit(scheme.as_deref(), netloc.as_deref(), &b.path, query.as_deref(), fragment.as_deref()));
    }
    let mut base_parts: Vec<&str> = b.path.split('/').collect();
    if base_parts.last().is_some_and(|s| !s.is_empty()) {
        base_parts.pop();
    }
    let segments: Vec<&str> = if u.path.starts_with('/') {
        u.path.split('/').collect()
    } else {
        let mut s: Vec<&str> = base_parts;
        s.extend(u.path.split('/'));
        // segments[1:-1] = filter(None, segments[1:-1])
        if s.len() > 2 {
            let last = s.len() - 1;
            let mid: Vec<&str> = s[1..last].iter().copied().filter(|x| !x.is_empty()).collect();
            let mut t = vec![s[0]];
            t.extend(mid);
            t.push(s[last]);
            s = t;
        }
        s
    };
    let mut resolved: Vec<&str> = Vec::new();
    for seg in &segments {
        match *seg {
            ".." => {
                resolved.pop();
            }
            "." => {}
            s => resolved.push(s),
        }
    }
    if matches!(segments.last(), Some(&".") | Some(&"..")) {
        resolved.push("");
    }
    let joined = resolved.join("/");
    let path = if joined.is_empty() { "/".to_owned() } else { joined };
    Ok(_urlunsplit(scheme.as_deref(), netloc.as_deref(), &path, u.query.as_deref(), u.fragment.as_deref()))
}

// --- urllib.request ---------------------------------------------------------------

/// `urllib.request.Request`, the parts used here.
#[derive(Debug, Clone)]
struct Req {
    /// `_full_url`: unwrapped, the fragment split off.
    url: String,
    /// `fragment`, None without a `#`.
    fragment: Option<String>,
    /// `type`; a proxy may change it.
    scheme: String,
    /// `host`, percent-decoded; a proxy puts its own here.
    host: Option<String>,
    /// `selector`: what the request line asks for.
    selector: String,
    /// `_tunnel_host`: the real host of an https request through a proxy.
    tunnel_host: Option<String>,
    /// `headers`: `key.capitalize()` -> value, in insertion order.
    headers: Vec<(String, PyValue)>,
    /// `unredirected_hdrs` (Host).
    unredirected: Vec<(String, PyValue)>,
}

/// `str.capitalize()`.
fn py_capitalize(s: &str) -> String {
    let mut cs = s.chars();
    match cs.next() {
        Some(c) => c.to_uppercase().chain(cs.flat_map(char::to_lowercase)).collect(),
        None => String::new(),
    }
}

/// `d[key] = value` on an ordered list of pairs.
fn upsert(d: &mut Vec<(String, PyValue)>, key: &str, value: PyValue) {
    match d.iter_mut().find(|(k, _)| k == key) {
        Some(slot) => slot.1 = value,
        None => d.push((key.to_owned(), value)),
    }
}

impl Req {
    /// `full_url`: the fragment back on when there is one.
    fn full_url(&self) -> String {
        match &self.fragment {
            Some(f) if !f.is_empty() => format!("{}#{f}", self.url),
            _ => self.url.clone(),
        }
    }

    fn add_header(&mut self, key: &str, value: PyValue) {
        upsert(&mut self.headers, &py_capitalize(key), value);
    }

    fn has_header(&self, name: &str) -> bool {
        self.headers.iter().chain(&self.unredirected).any(|(k, _)| k == name)
    }
}

/// `Request(url, headers=headers)`: Err is the ValueError it raises (no
/// scheme, or a netloc urlparse refuses).
fn _request(url: &str, headers: Vec<(String, PyValue)>) -> Result<Req, String> {
    let u = _unwrap(url);
    let (base, fragment) = match u.rfind('#') {
        Some(i) => (u[..i].to_owned(), Some(u[i + 1..].to_owned())),
        None => (u.clone(), None),
    };
    let full = match &fragment {
        Some(f) if !f.is_empty() => format!("{base}#{f}"),
        _ => base.clone(),
    };
    let Some((scheme, rest)) = _splittype(&base) else {
        return Err(format!("ValueError: unknown url type: {}", str_repr(&ps(&full))));
    };
    let (host, selector) = _splithost(rest);
    let host = host.map(|h| if h.is_empty() { h } else { _unquote(&h) });
    let mut req = Req {
        url: base.clone(),
        fragment,
        scheme,
        host,
        selector,
        tunnel_host: None,
        headers: Vec::new(),
        unredirected: Vec::new(),
    };
    for (k, v) in headers {
        req.add_header(&k, v);
    }
    // origin_req_host = request_host(self): urlparse(full_url).
    _urlsplit(&full)?;
    Ok(req)
}

/// `urllib.request.urlopen(req, timeout=25)`, through the default handlers.
fn _urlopen(req: Req) -> Fetched {
    // build_opener() runs once, on the first urlopen: its ProxyHandler keeps
    // the proxies the environment had then.
    let _ = _proxies();
    match req.scheme.as_str() {
        "http" | "https" => _http_open(req),
        "data" => _data_open(&req.full_url()),
        "file" => _file_open(&req.full_url()),
        // ftp (not ported) and unknown schemes: URLError.
        _ => Fetched::Failed,
    }
}

/// `getproxies_environment()`: `<scheme>_proxy` variables, lowercase ones
/// winning, `HTTP_PROXY` ignored under CGI.
fn _getproxies_environment() -> std::collections::HashMap<String, String> {
    let mut proxies = std::collections::HashMap::new();
    let mut env = Vec::new();
    for (name, value) in std::env::vars_os() {
        let name = name.to_string_lossy().into_owned();
        let value = value.to_string_lossy().into_owned();
        let cs: Vec<char> = name.chars().collect();
        if cs.len() > 5 && cs[cs.len() - 6] == '_' && cs[cs.len() - 5..].iter().collect::<String>().to_lowercase() == "proxy" {
            let proxy_name = cs[..cs.len() - 6].iter().collect::<String>().to_lowercase();
            if !value.is_empty() {
                proxies.insert(proxy_name.clone(), value.clone());
            }
            env.push((name, value, proxy_name));
        }
    }
    if std::env::var_os("REQUEST_METHOD").is_some() {
        proxies.remove("http");
    }
    for (name, value, proxy_name) in env {
        if name.ends_with("_proxy") {
            if value.is_empty() {
                proxies.remove(&proxy_name);
            } else {
                proxies.insert(proxy_name, value);
            }
        }
    }
    proxies
}

/// The ProxyHandler's proxies, read on the first urlopen.
fn _proxies() -> &'static std::collections::HashMap<String, String> {
    static PROXIES: std::sync::OnceLock<std::collections::HashMap<String, String>> = std::sync::OnceLock::new();
    PROXIES.get_or_init(_getproxies_environment)
}

/// `proxy_bypass_environment(host)`: `no_proxy`, read afresh each time.
fn _proxy_bypass(host: &str) -> bool {
    let proxies = _getproxies_environment();
    let Some(no_proxy) = proxies.get("no") else {
        return false;
    };
    if no_proxy == "*" {
        return true;
    }
    let host = host.to_lowercase();
    let (hostonly, _) = _splitport(&host);
    for name in no_proxy.split(',') {
        let name = ps_string(py_strip(&ps(name)));
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

/// `_parse_proxy(proxy)`: (scheme, user, password, host:port); Err for a
/// URL without an authority.
#[allow(clippy::type_complexity)]
fn _parse_proxy(proxy: &str) -> Result<(Option<String>, Option<String>, Option<String>, String), ()> {
    let (mut scheme, r_scheme) = match _splittype(proxy) {
        Some((s, r)) => (Some(s), r),
        None => (None, proxy),
    };
    let authority = if !r_scheme.starts_with('/') {
        scheme = None;
        proxy
    } else {
        if !r_scheme.starts_with("//") {
            return Err(());
        }
        let end = match r_scheme.find('@') {
            Some(at) => r_scheme[at..].find('/').map(|j| at + j),
            None => r_scheme[2..].find('/').map(|j| 2 + j),
        };
        &r_scheme[2..end.unwrap_or(r_scheme.len())]
    };
    let (userinfo, hostport) = match authority.rsplit_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, authority),
    };
    let (user, password) = match userinfo {
        Some(u) => match u.split_once(':') {
            Some((a, b)) => (Some(a.to_owned()), Some(b.to_owned())),
            None => (Some(u.to_owned()), None),
        },
        None => (None, None),
    };
    Ok((scheme, user, password, hostport.to_owned()))
}

/// `base64.b64encode`.
fn _b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// `ProxyHandler.proxy_open`, when the environment names a proxy for the
/// request's scheme. Err is an exception.
fn _proxy_open(req: &mut Req) -> Result<(), ()> {
    let Some(proxy) = _proxies().get(&req.scheme) else {
        return Ok(());
    };
    let orig_type = req.scheme.clone();
    let (proxy_type, user, password, hostport) = _parse_proxy(proxy)?;
    let proxy_type = proxy_type.unwrap_or_else(|| orig_type.clone());
    if req.host.as_deref().is_some_and(|h| !h.is_empty() && _proxy_bypass(h)) {
        return Ok(());
    }
    if let (Some(u), Some(p)) = (&user, &password) {
        if !u.is_empty() && !p.is_empty() {
            let creds = _b64(format!("{}:{}", _unquote(u), _unquote(p)).as_bytes());
            req.add_header("Proxy-authorization", PyValue::str(&format!("Basic {creds}")));
        }
    }
    let hostport = _unquote(&hostport);
    // req.set_proxy(hostport, proxy_type)
    if req.scheme == "https" && req.tunnel_host.is_none() {
        req.tunnel_host = req.host.clone();
    } else {
        req.scheme = proxy_type;
        req.selector = req.full_url();
    }
    req.host = Some(hostport);
    // A proxy of another scheme reopens the request as that scheme (its own
    // proxy, if the environment names one for it, is not consulted here).
    Ok(())
}

/// Response headers the way http.client keeps them (an `email` message
/// parsed from the latin-1 header block): `get` is case-insensitive and
/// answers the first of repeated headers.
struct Hdrs(Vec<(String, PyStr)>);

impl Hdrs {
    fn get(&self, name: &str) -> Option<PyStr> {
        let name = name.to_ascii_lowercase();
        self.0.iter().find(|(k, _)| *k == name).map(|(_, v)| v.clone())
    }
}

/// `email.parser.Parser().parsestr(block)` under the compat32 policy, the
/// headers only: lines split on `\r\n`, `\r` or `\n`; the headers end at the
/// first line that is neither `Name:`, a continuation nor a `From ` line;
/// a value keeps its trailing whitespace and the folds of continuation lines.
fn _parse_header_block(lines: &[Vec<u8>]) -> Hdrs {
    let text: String = lines.iter().flatten().map(|&b| char::from(b)).collect();
    let mut split: Vec<&str> = Vec::new();
    let bytes = text.as_bytes();
    let (mut start, mut i) = (0, 0);
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                split.push(&text[start..=i]);
                start = i + 1;
            }
            b'\r' if bytes.get(i + 1) == Some(&b'\n') => {
                split.push(&text[start..i + 2]);
                i += 1;
                start = i + 1;
            }
            b'\r' => {
                split.push(&text[start..=i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    if start < text.len() {
        split.push(&text[start..]);
    }
    // headerRE: ^(From |[\041-\071\073-\176]*:|[\t ])
    let header_re = |line: &str| {
        line.starts_with("From ")
            || line.starts_with([' ', '\t'])
            || line.find(':').is_some_and(|i| line[..i].bytes().all(|b| (0x21..=0x7e).contains(&b) && b != b':'))
    };
    let headers: Vec<&str> = split.into_iter().take_while(|l| header_re(l)).collect();
    // FeedParser._parse_headers
    let mut out: Vec<(String, PyStr)> = Vec::new();
    let mut last: Vec<&str> = Vec::new();
    let flush = |last: &mut Vec<&str>, out: &mut Vec<(String, PyStr)>| {
        if last.is_empty() {
            return;
        }
        // Compat32.header_source_parse
        let (name, first) = last[0].split_once(':').unwrap_or((last[0], ""));
        let mut value = String::from(first);
        for l in &last[1..] {
            value.push_str(l);
        }
        let value = value.trim_start_matches([' ', '\t', '\r', '\n']).trim_end_matches(['\r', '\n']);
        out.push((name.to_ascii_lowercase(), ps(value)));
        last.clear();
    };
    for (lineno, line) in headers.iter().enumerate() {
        if line.starts_with([' ', '\t']) {
            // A continuation; one with nothing to continue is dropped.
            if !last.is_empty() {
                last.push(line);
            }
            continue;
        }
        flush(&mut last, &mut out);
        if line.starts_with("From ") {
            // The unix-from envelope (or a stray one): never a header.
            if lineno == headers.len() - 1 && lineno != 0 {
                break;
            }
            continue;
        }
        if line.find(':') == Some(0) {
            continue; // missing header name
        }
        last.push(line);
    }
    flush(&mut last, &mut out);
    Hdrs(out)
}

// --- http.client --------------------------------------------------------------

/// `_MAXLINE`: the longest status, header or chunk-size line.
const MAXLINE: usize = 65536;
/// `_MAXHEADERS`: more header lines than this (the blank one included) is an
/// HTTPException.
const MAXHEADERS: usize = 100;
/// `_MAXINTERIMRESPONSES`.
const MAXINTERIM: usize = 100;

/// A connected socket, TLS or not.
enum Stream {
    Plain(std::net::TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, std::net::TcpStream>>),
}

impl std::io::Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            // ssl's suppress_ragged_eofs: a close without close_notify is EOF.
            Stream::Tls(s) => match s.read(buf) {
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(0),
                r => r,
            },
        }
    }
}

impl std::io::Write for Stream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

/// `sock.makefile("rb")`: a buffered reader with `readline(limit)`.
struct Conn {
    s: Stream,
    buf: Vec<u8>,
    pos: usize,
}

impl Conn {
    fn new(s: Stream) -> Conn {
        Conn { s, buf: Vec::new(), pos: 0 }
    }

    /// Read more; Ok(false) at EOF.
    fn fill(&mut self) -> std::io::Result<bool> {
        use std::io::Read;
        if self.pos > 0 {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
        let mut chunk = [0u8; 8192];
        loop {
            match self.s.read(&mut chunk) {
                Ok(0) => return Ok(false),
                Ok(n) => {
                    self.buf.extend_from_slice(&chunk[..n]);
                    return Ok(true);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
    }

    fn take(&mut self, n: usize) -> Vec<u8> {
        let out = self.buf[self.pos..self.pos + n].to_vec();
        self.pos += n;
        out
    }

    /// `readline(limit)`: up to and including `\n`, at most `limit` bytes.
    fn readline(&mut self, limit: usize) -> std::io::Result<Vec<u8>> {
        let mut scanned = 0;
        loop {
            let avail = &self.buf[self.pos..];
            if let Some(i) = avail[scanned..].iter().position(|&b| b == b'\n') {
                return Ok(self.take((scanned + i + 1).min(limit)));
            }
            scanned = avail.len();
            if scanned >= limit {
                return Ok(self.take(limit));
            }
            if !self.fill()? {
                let n = self.buf.len() - self.pos;
                return Ok(self.take(n));
            }
        }
    }

    /// `read(n)`: up to `n` bytes, fewer only at EOF.
    fn read_upto(&mut self, n: u64) -> std::io::Result<Vec<u8>> {
        while ((self.buf.len() - self.pos) as u64) < n {
            if !self.fill()? {
                break;
            }
        }
        let have = (self.buf.len() - self.pos) as u64;
        Ok(self.take(have.min(n) as usize))
    }

    /// `read()`: everything up to EOF.
    fn read_all(&mut self) -> std::io::Result<Vec<u8>> {
        while self.fill()? {}
        let n = self.buf.len() - self.pos;
        Ok(self.take(n))
    }
}

/// `_read_status`: (version, status); Err for BadStatusLine, LineTooLong or
/// RemoteDisconnected.
fn _read_status(conn: &mut Conn) -> Result<(String, u16), ()> {
    let line = conn.readline(MAXLINE + 1).map_err(|_| ())?;
    if line.len() > MAXLINE || line.is_empty() {
        return Err(());
    }
    let line: PyStr = line.iter().map(|&b| u32::from(b)).collect();
    // line.split(None, 2), falling back to split(None, 1)
    let mut words: Vec<&[u32]> = Vec::new();
    let mut i = 0;
    while i < line.len() && words.len() < 2 {
        while i < line.len() && py_isspace(line[i]) {
            i += 1;
        }
        let start = i;
        while i < line.len() && !py_isspace(line[i]) {
            i += 1;
        }
        if i > start {
            words.push(&line[start..i]);
        }
    }
    if words.len() < 2 {
        return Err(());
    }
    let version = ps_string(words[0]);
    if !version.starts_with("HTTP/") {
        return Err(());
    }
    let status = py_int(words[1]).ok_or(())?;
    if status < BigInt::from(100) || status > BigInt::from(999) {
        return Err(());
    }
    let status = u16::try_from(status).map_err(|_| ())?;
    Ok((version, status))
}

/// `_read_headers`: the raw header lines, the blank one included.
fn _read_headers(conn: &mut Conn) -> Result<Vec<Vec<u8>>, ()> {
    let mut headers = Vec::new();
    loop {
        let line = conn.readline(MAXLINE + 1).map_err(|_| ())?;
        if line.len() > MAXLINE {
            return Err(()); // LineTooLong
        }
        let end = matches!(line.as_slice(), b"\r\n" | b"\n" | b"");
        headers.push(line);
        if headers.len() > MAXHEADERS {
            return Err(()); // got more than 100 headers
        }
        if end {
            return Ok(headers);
        }
    }
}

/// An HTTPResponse after `begin()`.
struct Resp {
    code: u16,
    headers: Hdrs,
    conn: Conn,
    chunked: bool,
    /// None: read to EOF.
    length: Option<BigInt>,
}

/// `HTTPResponse.begin()`.
fn _begin(mut conn: Conn) -> Result<Resp, ()> {
    let mut status = None;
    for _ in 0..MAXINTERIM {
        let (version, code) = _read_status(&mut conn)?;
        if code != 100 {
            status = Some((version, code));
            break;
        }
        _read_headers(&mut conn)?;
    }
    let (version, code) = status.ok_or(())?;
    if !(version == "HTTP/1.0" || version == "HTTP/0.9" || version.starts_with("HTTP/1.")) {
        return Err(()); // UnknownProtocol
    }
    let headers = _parse_header_block(&_read_headers(&mut conn)?);
    let chunked = headers.get("transfer-encoding").is_some_and(|t| ps_string(&t).to_lowercase() == "chunked");
    let mut length = None;
    if !chunked {
        if let Some(l) = headers.get("content-length").filter(|l| !l.is_empty()) {
            length = py_int(&l).filter(|n| *n >= BigInt::from(0));
        }
    }
    if code == 204 || code == 304 || (100..200).contains(&code) {
        length = Some(BigInt::from(0));
    }
    Ok(Resp { code, headers, conn, chunked, length })
}

/// `int(line, 16)` for a chunk size: whitespace, a sign, `0x`, underscores.
fn _chunk_size(line: &[u8]) -> Option<BigInt> {
    let ws = |b: &u8| matches!(b, b'\t' | b'\n' | 0x0b | 0x0c | b'\r' | b' ');
    let start = line.iter().position(|b| !ws(b)).unwrap_or(line.len());
    let end = line.iter().rposition(|b| !ws(b)).map_or(start, |i| i + 1);
    let t = &line[start..end];
    let (neg, mut body) = match t.first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let mut after_prefix = false;
    if body.len() >= 2 && body[0] == b'0' && matches!(body[1], b'x' | b'X') {
        body = &body[2..];
        after_prefix = true;
    }
    let mut digits = Vec::new();
    let mut prev_us = false;
    for (i, &b) in body.iter().enumerate() {
        if b == b'_' {
            if prev_us || (i == 0 && !after_prefix) {
                return None;
            }
            prev_us = true;
        } else if b.is_ascii_hexdigit() {
            digits.push(b);
            prev_us = false;
        } else {
            return None;
        }
    }
    if digits.is_empty() || prev_us {
        return None;
    }
    let n = BigInt::parse_bytes(&digits, 16)?;
    Some(if neg { -n } else { n })
}

/// `_safe_read(n)`: exactly n bytes, or IncompleteRead.
fn _safe_read(conn: &mut Conn, n: &BigInt) -> Result<Vec<u8>, ()> {
    let Ok(n) = u64::try_from(n) else {
        // Python asks for that many, gets what there is, and raises.
        let _ = conn.read_all();
        return Err(());
    };
    let data = conn.read_upto(n).map_err(|_| ())?;
    if (data.len() as u64) < n {
        return Err(());
    }
    Ok(data)
}

/// `HTTPResponse.read()`: the whole body.
fn _read_body(r: &mut Resp) -> Result<Vec<u8>, ()> {
    if !r.chunked {
        return match &r.length {
            None => r.conn.read_all().map_err(|_| ()),
            Some(n) => _safe_read(&mut r.conn, &n.clone()),
        };
    }
    let mut value = Vec::new();
    let mut first = true;
    loop {
        if !first {
            _safe_read(&mut r.conn, &BigInt::from(2))?; // the CRLF after a chunk
        }
        first = false;
        let line = r.conn.readline(MAXLINE + 1).map_err(|_| ())?;
        if line.len() > MAXLINE {
            return Err(());
        }
        let line = match line.iter().position(|&b| b == b';') {
            Some(i) => &line[..i],
            None => &line[..],
        };
        let size = _chunk_size(line).ok_or(())?;
        if size == BigInt::from(0) {
            // _read_and_discard_trailer
            let mut trailers = 0;
            loop {
                let line = r.conn.readline(MAXLINE + 1).map_err(|_| ())?;
                if line.len() > MAXLINE {
                    return Err(());
                }
                if matches!(line.as_slice(), b"\r\n" | b"\n" | b"") {
                    return Ok(value);
                }
                trailers += 1;
                if trailers > MAXHEADERS {
                    return Err(());
                }
            }
        }
        if size < BigInt::from(0) {
            // _safe_read of a negative size reads to EOF; the CRLF after
            // it is then missing.
            let _ = r.conn.read_all();
            return Err(());
        }
        value.extend(_safe_read(&mut r.conn, &size)?);
    }
}

/// `HTTPConnection._get_hostport`: Err for a port that is not a number (or
/// one `create_connection` would refuse).
fn _get_hostport(host: &str, default_port: u16) -> Result<(String, u16), ()> {
    let mut host = host.to_owned();
    let mut port = default_port;
    let i = host.rfind(':');
    let j = host.rfind(']');
    if let Some(i) = i.filter(|&i| j.is_none_or(|j| i > j)) {
        let p = &host[i + 1..];
        if !p.is_empty() {
            let n = py_int(&ps(p)).ok_or(())?;
            port = u16::try_from(n).map_err(|_| ())?;
        }
        host.truncate(i);
    }
    if host.len() >= 2 && host.starts_with('[') && host.ends_with(']') {
        host = host[1..host.len() - 1].to_owned();
    }
    Ok((host, port))
}

/// `[\x00-\x20\x7f]` anywhere: http.client's InvalidURL.
fn _has_ctl(s: &str) -> bool {
    s.chars().any(|c| c <= ' ' || c == '\x7f')
}

/// A host for getaddrinfo, which Python encodes with the idna codec.
fn _idna(host: &str) -> Option<String> {
    if host.is_ascii() {
        return Some(host.to_owned());
    }
    match url::Host::parse(host) {
        Ok(url::Host::Domain(d)) => Some(d),
        Ok(other) => Some(other.to_string()),
        Err(_) => None,
    }
}

/// `socket.create_connection((host, port), 25)`: each address in turn, the
/// socket timeout on every later operation too.
fn _connect(host: &str, port: u16) -> Result<std::net::TcpStream, ()> {
    use std::net::ToSocketAddrs;
    let host = _idna(host).ok_or(())?;
    let timeout = std::time::Duration::from_secs(SOCKET_TIMEOUT);
    let addrs = (host.as_str(), port).to_socket_addrs().map_err(|_| ())?;
    for a in addrs {
        if let Ok(s) = std::net::TcpStream::connect_timeout(&a, timeout) {
            let _ = s.set_read_timeout(Some(timeout));
            let _ = s.set_write_timeout(Some(timeout));
            let _ = s.set_nodelay(true);
            return Ok(s);
        }
    }
    Err(())
}

/// `ssl.create_default_context()` with ALPN `http/1.1`: the CA file and
/// directory OpenSSL would use (`SSL_CERT_FILE`, `SSL_CERT_DIR`, else
/// `/etc/ssl/cert.pem` and `/etc/ssl/certs`).
fn _tls_config() -> Option<std::sync::Arc<rustls::ClientConfig>> {
    use rustls::pki_types::{pem::PemObject, CertificateDer};
    static CFG: std::sync::OnceLock<Option<std::sync::Arc<rustls::ClientConfig>>> = std::sync::OnceLock::new();
    CFG.get_or_init(|| {
        let mut roots = rustls::RootCertStore::empty();
        let mut add = |bytes: &[u8]| {
            let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(bytes).filter_map(Result::ok).collect();
            roots.add_parsable_certificates(certs);
        };
        let cafile = std::env::var_os("SSL_CERT_FILE").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/etc/ssl/cert.pem"));
        if let Ok(b) = std::fs::read(cafile) {
            add(&b);
        }
        let capath = std::env::var_os("SSL_CERT_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/etc/ssl/certs"));
        if let Ok(rd) = std::fs::read_dir(capath) {
            for e in rd.flatten() {
                if e.path().is_file() {
                    if let Ok(b) = std::fs::read(e.path()) {
                        add(&b);
                    }
                }
            }
        }
        let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
        let mut cfg = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .ok()?
            .with_root_certificates(roots)
            .with_no_client_auth();
        cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        Some(std::sync::Arc::new(cfg))
    })
    .clone()
}

/// `_is_legal_header_name`: `[^:\s][^:\r\n]*`.
fn _legal_header_name(b: &[u8]) -> bool {
    match b.split_first() {
        Some((&f, rest)) => f != b':' && !b" \t\n\r\x0b\x0c".contains(&f) && !rest.iter().any(|c| b":\r\n".contains(c)),
        None => false,
    }
}

/// What http.client's `putheader` sends for a value: a str as latin-1, an
/// int or bool as `str()`, anything else (or an illegal value) raises.
fn _header_bytes(v: &PyValue) -> Option<Vec<u8>> {
    let b: Vec<u8> = match v {
        PyValue::Str(s) => s.iter().map(|&c| u8::try_from(c).ok()).collect::<Option<Vec<u8>>>()?,
        PyValue::Int(n) => n.to_string().into_bytes(),
        PyValue::Bool(b) => if *b { "True" } else { "False" }.as_bytes().to_vec(),
        _ => return None,
    };
    // _is_illegal_header_value: \n(?![ \t])|\r(?![ \t\n])
    for (i, &c) in b.iter().enumerate() {
        let next = b.get(i + 1).copied();
        if c == b'\n' && !matches!(next, Some(b' ' | b'\t')) {
            return None;
        }
        if c == b'\r' && !matches!(next, Some(b' ' | b'\t' | b'\n')) {
            return None;
        }
    }
    Some(b)
}

/// `AbstractHTTPHandler.do_open` + `HTTPConnection.request` +
/// `getresponse()`: one request, one response with its headers read.
fn _do_open(req: &Req) -> Result<Resp, ()> {
    use std::io::Write;
    let https = req.scheme == "https";
    let default_port = if https { 443 } else { 80 };
    let host = req.host.as_deref().filter(|h| !h.is_empty()).ok_or(())?;
    let (h, port) = _get_hostport(host, default_port)?;
    if _has_ctl(&h) {
        return Err(());
    }
    // headers = dict(unredirected_hdrs); headers.update(headers);
    // headers["Connection"] = "close"; then every name .title()d.
    let mut merged = req.unredirected.clone();
    for (k, v) in &req.headers {
        upsert(&mut merged, k, v.clone());
    }
    upsert(&mut merged, "Connection", PyValue::str("close"));
    let mut headers: Vec<(String, PyValue)> = Vec::new();
    for (k, v) in merged {
        upsert(&mut headers, &crate::util::py_title(&k), v);
    }
    let mut tunnel = None;
    if let Some(th) = &req.tunnel_host {
        let mut th_headers: Vec<(String, PyValue)> = Vec::new();
        if let Some(i) = headers.iter().position(|(k, _)| k == "Proxy-Authorization") {
            th_headers.push(headers.remove(i));
        }
        let (thost, tport) = _get_hostport(th, default_port)?;
        if !th_headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
            th_headers.push(("Host".into(), PyValue::str(&format!("{}:{tport}", _idna(&thost).ok_or(())?))));
        }
        tunnel = Some((thost, tport, th_headers));
    }
    // putrequest: the path as it is, but no controls or spaces, ASCII only.
    let sel = if req.selector.is_empty() { "/" } else { req.selector.as_str() };
    if _has_ctl(sel) || !sel.is_ascii() {
        return Err(());
    }
    let mut lines: Vec<Vec<u8>> = vec![format!("GET {sel} HTTP/1.1").into_bytes()];
    // Host is always given, so only Accept-Encoding is added.
    if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("accept-encoding")) {
        lines.push(b"Accept-Encoding: identity".to_vec());
    }
    for (k, v) in &headers {
        let name = k.as_bytes();
        if !k.is_ascii() || !_legal_header_name(name) {
            return Err(());
        }
        let mut line = name.to_vec();
        line.extend_from_slice(b": ");
        line.extend(_header_bytes(v).ok_or(())?);
        lines.push(line);
    }
    let mut msg = lines.join(&b"\r\n"[..]);
    msg.extend_from_slice(b"\r\n\r\n");

    // connect(): the TCP connection, the CONNECT tunnel, then TLS.
    let tcp = _connect(&h, port)?;
    let mut stream = Stream::Plain(tcp);
    let mut server_hostname = h.clone();
    if let Some((thost, tport, th_headers)) = tunnel {
        if _has_ctl(&thost) {
            return Err(());
        }
        let t = _idna(&thost).ok_or(())?;
        let t = if t.contains(':') && !t.starts_with('[') { format!("[{t}]") } else { t };
        let mut req_bytes = format!("CONNECT {t}:{tport} HTTP/1.1\r\n").into_bytes();
        for (k, v) in &th_headers {
            let b = _header_bytes(v).ok_or(())?;
            if !_legal_header_name(k.as_bytes()) {
                return Err(());
            }
            req_bytes.extend_from_slice(k.as_bytes());
            req_bytes.extend_from_slice(b": ");
            req_bytes.extend(b);
            req_bytes.extend_from_slice(b"\r\n");
        }
        req_bytes.extend_from_slice(b"\r\n");
        stream.write_all(&req_bytes).map_err(|_| ())?;
        let mut c = Conn::new(stream);
        let (_, code) = _read_status(&mut c)?;
        _read_headers(&mut c)?;
        if code != 200 {
            return Err(()); // Tunnel connection failed
        }
        stream = c.s;
        server_hostname = thost;
    }
    if https {
        let Stream::Plain(tcp) = stream else { return Err(()) };
        let name = rustls::pki_types::ServerName::try_from(server_hostname).map_err(|_| ())?;
        let tls = rustls::ClientConnection::new(_tls_config().ok_or(())?, name).map_err(|_| ())?;
        let mut s = rustls::StreamOwned::new(tls, tcp);
        // wrap_socket handshakes at once.
        while s.conn.is_handshaking() {
            s.conn.complete_io(&mut s.sock).map_err(|_| ())?;
        }
        stream = Stream::Tls(Box::new(s));
    }
    stream.write_all(&msg).map_err(|_| ())?;
    stream.flush().map_err(|_| ())?;
    _begin(Conn::new(stream))
}

/// The http/https handlers: http.client with a 25 s timeout on every socket
/// operation, the environment's proxy for the scheme, urllib's redirect
/// handling (headers carried along, at most 10 hops and 4 visits to one URL,
/// then HTTPError with the last headers) and HTTPError for anything that is
/// not 2xx.
fn _http_open(mut req: Req) -> Fetched {
    let mut visited: Option<std::collections::HashMap<String, u32>> = None;
    loop {
        // AbstractHTTPHandler.do_request_
        let Some(host) = req.host.clone().filter(|h| !h.is_empty()) else {
            return Fetched::Failed; // no host given
        };
        if !req.has_header("Host") {
            upsert(&mut req.unredirected, "Host", PyValue::str(&host));
        }
        if _proxy_open(&mut req).is_err() {
            return Fetched::Failed;
        }
        if !matches!(req.scheme.as_str(), "http" | "https") {
            return Fetched::Failed;
        }
        let Ok(mut r) = _do_open(&req) else {
            return Fetched::Failed;
        };
        let code = r.code;
        if (200..300).contains(&code) {
            return match _read_body(&mut r) {
                Ok(raw) => Fetched::Ok { raw, headers: r.headers },
                Err(()) => Fetched::Failed,
            };
        }
        if ![301, 302, 303, 307, 308].contains(&code) {
            return Fetched::HttpError { code, headers: r.headers };
        }
        // HTTPRedirectHandler.http_error_302
        let Some(loc) = r.headers.get("location").or_else(|| r.headers.get("uri")) else {
            return Fetched::HttpError { code, headers: r.headers };
        };
        let Ok(mut parts) = _urlparse(&ps_string(&loc)) else {
            return Fetched::Failed;
        };
        if !["http", "https", "ftp", ""].contains(&parts.scheme.as_str()) {
            return Fetched::HttpError { code, headers: r.headers };
        }
        if parts.path.is_empty() && !parts.netloc.is_empty() {
            parts.path = "/".into();
        }
        let Some(quoted) = _quote_punct(&_urlunparse(&parts)) else {
            return Fetched::Failed;
        };
        let Ok(newurl) = _urljoin(&req.full_url(), &quoted) else {
            return Fetched::Failed;
        };
        // redirect_request: the headers go along, minus the content ones.
        let carried: Vec<(String, PyValue)> = req
            .headers
            .iter()
            .filter(|(k, _)| !["content-length", "content-type"].contains(&k.to_lowercase().as_str()))
            .cloned()
            .collect();
        let Ok(new) = _request(&newurl.replace(' ', "%20"), carried) else {
            return Fetched::Failed;
        };
        let v = visited.get_or_insert_with(std::collections::HashMap::new);
        if !v.is_empty() && (v.get(&newurl).copied().unwrap_or(0) >= 4 || v.len() >= 10) {
            return Fetched::HttpError { code, headers: r.headers };
        }
        *v.entry(newurl).or_insert(0) += 1;
        // fp.read(); fp.close()
        if _read_body(&mut r).is_err() {
            return Fetched::Failed;
        }
        if !matches!(new.scheme.as_str(), "http" | "https") {
            return Fetched::Failed; // ftp is not ported
        }
        req = new;
    }
}

/// `binascii.a2b_base64` (non-strict): other characters skipped, decoding
/// stops at complete padding, a dangling quad is an error.
fn _a2b_base64(data: &[u8]) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let mut out = Vec::new();
    let (mut quad, mut pads, mut left) = (0u32, 0u32, 0u32);
    for &c in data {
        if c == b'=' {
            if quad >= 2 {
                pads += 1;
                if quad + pads >= 4 {
                    return Some(out);
                }
            }
            continue;
        }
        let Some(v) = val(c) else { continue };
        pads = 0;
        match quad {
            0 => {
                quad = 1;
                left = v;
            }
            1 => {
                quad = 2;
                out.push(((left << 2) | (v >> 4)) as u8);
                left = v & 0x0f;
            }
            2 => {
                quad = 3;
                out.push(((left << 4) | (v >> 2)) as u8);
                left = v & 0x03;
            }
            _ => {
                quad = 0;
                out.push(((left << 6) | v) as u8);
                left = 0;
            }
        }
    }
    (quad == 0).then_some(out)
}

/// urllib's DataHandler (RFC 2397). Its headers are only Content-type and
/// Content-length, so nothing here carries a rate limit or an ETag.
fn _data_open(full_url: &str) -> Fetched {
    let Some((_, rest)) = full_url.split_once(':') else {
        return Fetched::Failed;
    };
    let Some((mediatype, data)) = rest.split_once(',') else {
        return Fetched::Failed;
    };
    if mediatype.chars().any(|c| c < '\x20' || c == '\x7f') {
        return Fetched::Failed;
    }
    let mut raw = _unquote_to_bytes(data);
    let mut mediatype = mediatype.to_owned();
    if let Some(m) = mediatype.strip_suffix(";base64") {
        let Some(d) = _a2b_base64(&raw) else {
            return Fetched::Failed;
        };
        raw = d;
        mediatype = m.to_owned();
    }
    if mediatype.is_empty() {
        mediatype = "text/plain;charset=US-ASCII".into();
    }
    let len = raw.len();
    Fetched::Ok {
        raw,
        headers: Hdrs(vec![("content-type".into(), ps(&mediatype)), ("content-length".into(), ps(&len.to_string()))]),
    }
}

/// `socket.gethostbyname_ex(name)[2]`: the IPv4 addresses; None for gaierror.
fn _ipv4s(name: &str) -> Option<Vec<std::net::Ipv4Addr>> {
    use std::net::ToSocketAddrs;
    let name = _idna(name)?;
    let v: Vec<std::net::Ipv4Addr> = (name.as_str(), 0)
        .to_socket_addrs()
        .ok()?
        .filter_map(|a| match a {
            std::net::SocketAddr::V4(a) => Some(*a.ip()),
            std::net::SocketAddr::V6(_) => None,
        })
        .collect();
    (!v.is_empty()).then_some(v)
}

/// `FileHandler.get_names()`: this machine's addresses, looked up once.
fn _file_names() -> Option<&'static Vec<std::net::Ipv4Addr>> {
    static NAMES: std::sync::OnceLock<Option<Vec<std::net::Ipv4Addr>>> = std::sync::OnceLock::new();
    NAMES
        .get_or_init(|| {
            let hostname = rustix::system::uname().nodename().to_string_lossy().into_owned();
            match (_ipv4s("localhost"), _ipv4s(&hostname)) {
                (Some(mut a), Some(b)) => {
                    a.extend(b);
                    Some(a)
                }
                _ => _ipv4s("localhost").map(|v| v[..1].to_vec()),
            }
        })
        .as_ref()
}

/// `_is_local_authority(authority, resolve=True)`: the name, then its IPv4
/// address against this machine's. Err where `get_names` raises.
fn _is_local_authority(authority: &str) -> Result<bool, ()> {
    if authority.is_empty() || authority == "localhost" {
        return Ok(true);
    }
    let hostname = rustix::system::uname().nodename().to_string_lossy().into_owned();
    if authority == hostname {
        return Ok(true);
    }
    let Some(address) = _ipv4s(authority).and_then(|v| v.first().copied()) else {
        return Ok(false);
    };
    Ok(_file_names().ok_or(())?.contains(&address))
}

/// urllib's FileHandler: a local file, with a Last-modified header (which
/// becomes the entry's `modified`).
fn _file_open(full_url: &str) -> Fetched {
    use std::os::unix::ffi::OsStringExt;
    // url2pathname(url, require_scheme=True, resolve_host=True)
    let Ok(sp) = _urlsplit(full_url) else {
        return Fetched::Failed;
    };
    if sp.scheme.as_deref() != Some("file") {
        return Fetched::Failed;
    }
    if _is_local_authority(sp.netloc.as_deref().unwrap_or("")) != Ok(true) {
        return Fetched::Failed;
    }
    let local = PathBuf::from(std::ffi::OsString::from_vec(_unquote_to_bytes(&sp.path)));
    let Ok(meta) = std::fs::metadata(&local) else {
        return Fetched::Failed;
    };
    let mtime = meta
        .modified()
        .ok()
        .map(|t| match t.duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => d.as_secs() as i64,
            Err(e) => -(e.duration().as_secs_f64().ceil() as i64),
        })
        .unwrap_or(0);
    // email.utils.formatdate(st_mtime, usegmt=True)
    let modified = chrono::DateTime::from_timestamp(mtime, 0)
        .map(|d| d.format("%a, %d %b %Y %H:%M:%S GMT").to_string())
        .unwrap_or_default();
    let Ok(raw) = std::fs::read(&local) else {
        return Fetched::Failed;
    };
    Fetched::Ok {
        raw,
        headers: Hdrs(vec![
            ("content-type".into(), ps("text/plain")),
            ("content-length".into(), ps(&meta.len().to_string())),
            ("last-modified".into(), ps(&modified)),
        ]),
    }
}

#[deprecated(note = "the Python raises out of latest_release() where get_json() raises; use try_latest_release and stop on Err")]
#[allow(deprecated)]
pub fn latest_release(owner_repo: &str, ttl: impl Into<Ttl>) -> Option<Value> {
    get_json(&format!("{API}/repos/{owner_repo}/releases/latest"), ttl, true, ACCEPT)
}

pub fn try_latest_release(owner_repo: &str, ttl: impl Into<Ttl>) -> Result<Option<Value>, String> {
    try_get_json(&format!("{API}/repos/{owner_repo}/releases/latest"), ttl, true, ACCEPT)
}

#[deprecated(note = "the Python raises out of repo() where get_json() raises; use try_repo and stop on Err")]
#[allow(deprecated)]
pub fn repo(owner_repo: &str, ttl: impl Into<Ttl>) -> Option<Value> {
    get_json(&format!("{API}/repos/{owner_repo}"), ttl, true, ACCEPT)
}

pub fn try_repo(owner_repo: &str, ttl: impl Into<Ttl>) -> Result<Option<Value>, String> {
    try_get_json(&format!("{API}/repos/{owner_repo}"), ttl, true, ACCEPT)
}

#[deprecated(note = "the Python raises out of org_repos() where get_json() raises; use try_org_repos and stop on Err")]
#[allow(deprecated)]
pub fn org_repos(org: &str, ttl: impl Into<Ttl>) -> Option<Value> {
    get_json(&format!("{API}/orgs/{org}/repos?per_page=100"), ttl, true, ACCEPT)
}

pub fn try_org_repos(org: &str, ttl: impl Into<Ttl>) -> Result<Option<Value>, String> {
    try_get_json(&format!("{API}/orgs/{org}/repos?per_page=100"), ttl, true, ACCEPT)
}

/// One line for the UI about where the quota stands.
///
/// Where the Python raises (a broken rate file, gh output that is not
/// UTF-8) this says only "GitHub API: unauthenticated"; see [`try_describe`].
#[deprecated(note = "the Python raises out of describe() for a broken rate file; use try_describe and stop on Err")]
pub fn describe() -> String {
    try_describe().unwrap_or_else(|_| "GitHub API: unauthenticated".into())
}

/// `describe()`, raising where the Python does.
pub fn try_describe() -> Result<String, String> {
    deep(_describe)
}

fn _describe() -> Result<String, String> {
    let b = _budget()?;
    let auth = if !try_token()?.is_empty() { "authenticated" } else { "unauthenticated" };
    if !b.known {
        return Ok(format!("GitHub API: {auth}"));
    }
    let left = b.remaining.py_str();
    let mins = if b.reset.truthy() {
        let q = (arith_float(&b.reset, "-", false)? - crate::util::now()) / 60.0;
        // int(q): nan and inf raise.
        if q.is_nan() {
            return Err("ValueError: cannot convert float NaN to integer".into());
        }
        if q.is_infinite() {
            return Err("OverflowError: cannot convert float infinity to integer".into());
        }
        float_to_int(q.trunc()).max(BigInt::from(0))
    } else {
        BigInt::from(0)
    };
    Ok(format!("GitHub API: {left}/{} left ({auth})", b.limit.py_str())
        + &(if mins != BigInt::from(0) { format!(", resets in {mins}m") } else { String::new() }))
}




#[cfg(test)]
fn restore(cache_home: Option<std::ffi::OsString>, state_home: Option<std::ffi::OsString>) {
    for (k, v) in [("XDG_CACHE_HOME", cache_home), ("XDG_STATE_HOME", state_home)] {
        match v {
            Some(v) => std::env::set_var(k, v),
            None => std::env::remove_var(k),
        }
    }
}

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use super::*;

    // Tests share the process environment; one at a time.

    /// XDG_STATE_HOME and XDG_CACHE_HOME in a temp dir, as the Python test
    /// class's setUp does.
    struct Env {
        _guard: std::sync::MutexGuard<'static, ()>,
        _tmp: tempfile::TempDir,
        old: (Option<std::ffi::OsString>, Option<std::ffi::OsString>),
    }

    impl Env {
        fn new() -> Env {
            let guard = crate::util::TEST_ENV.lock().unwrap_or_else(|e| e.into_inner());
            let tmp = tempfile::tempdir().unwrap();
            let old = (std::env::var_os("XDG_CACHE_HOME"), std::env::var_os("XDG_STATE_HOME"));
            std::env::set_var("XDG_CACHE_HOME", tmp.path().join("cache"));
            std::env::set_var("XDG_STATE_HOME", tmp.path().join("state"));
            Env { _guard: guard, _tmp: tmp, old }
        }
    }

    impl Drop for Env {
        fn drop(&mut self) {
            restore(self.old.0.take(), self.old.1.take());
        }
    }

    fn entry(at: f64, body: Value) -> PyValue {
        PyValue::Dict(vec![(ps("at"), PyValue::Float(at)), (ps("body"), PyValue::from_value(&body))])
    }

    #[test]
    fn test_switched_off_network_features_make_no_request() {
        // The ghcache part: with `net_gh_token` off, no `gh auth token`.
        let _env = Env::new();
        for key in ["net_vendor_icons", "net_security_tracker", "net_gh_token",
                    "net_market_omarchy", "net_market_artcraft", "net_market_themes"] {
            crate::settings::set(key, Value::Bool(false)).unwrap();
        }
        reset_token();
        assert_eq!(token(), "");
    }

    #[test]
    fn cache_inside_ttl_does_not_touch_the_network() {
        let _env = Env::new();
        let url = "http://127.0.0.1:1/never";
        _write(url, &entry(crate::util::now(), serde_json::json!({"tag_name": "v1"}))).unwrap();
        assert_eq!(get_json(url, DEFAULT_TTL, true, ACCEPT), Some(serde_json::json!({"tag_name": "v1"})));
    }

    #[test]
    fn out_of_budget_serves_stale_or_nothing() {
        let _env = Env::new();
        crate::settings::set("net_gh_token", Value::Bool(false)).unwrap();
        std::fs::create_dir_all(cache()).unwrap();
        let reset = crate::util::now() as i64 + 3600;
        std::fs::write(state_file(), format!("{{\"remaining\": 3, \"limit\": 60, \"reset\": {reset}}}")).unwrap();
        assert!(!can_spend().unwrap());
        let url = "http://127.0.0.1:1/stale";
        _write(url, &entry(0.0, serde_json::json!([1, 2]))).unwrap();
        assert_eq!(get_json(url, DEFAULT_TTL, true, ACCEPT), Some(serde_json::json!([1, 2])));
        assert_eq!(get_json(url, DEFAULT_TTL, false, ACCEPT), None);
        assert!(describe().starts_with("GitHub API: 3/60 left (unauthenticated), resets in "));
    }

    #[test]
    fn json_round_trips_like_python() {
        // What json.dumps(json.loads(text)) gives in Python.
        for (text, want) in [
            (r#"{"n": 123456789012345678901234567890}"#, r#"{"n": 123456789012345678901234567890}"#),
            ("[1e400, -Infinity, NaN, 1.5, 1E2]", "[Infinity, -Infinity, NaN, 1.5, 100.0]"),
            (r#""\ud800\ud83d\ude00é""#, r#""\ud800\ud83d\ude00\u00e9""#),
            (r#"{"a": 1, "b": 2, "a": 3}"#, r#"{"a": 3, "b": 2}"#),
        ] {
            assert_eq!(dumps(&loads(text).unwrap()).unwrap(), want);
        }
        for bad in ["", "[1,]", "01", "{'a': 1}", "\"\x01\"", "1.", "[1] x", "-"] {
            assert_eq!(loads(bad), Err(JsonError::Value), "{bad:?}");
        }
        // Python's json goes about 52 000 levels deep (C stack bound).
        deep(|| {
            let ok = "[".repeat(LOADS_DEPTH) + &"]".repeat(LOADS_DEPTH);
            assert!(loads(&ok).is_ok());
            // dumps stops a little earlier for lists (52 090), much earlier
            // for dicts (28 939).
            let lists = |n: usize| loads(&("[".repeat(n) + &"]".repeat(n))).unwrap();
            assert_eq!(dumps(&lists(52_090)).map(|s| s.len()), Ok(2 * 52_090));
            assert_eq!(dumps(&lists(52_091)), Err(JsonError::Recursion));
            let too = "[".repeat(LOADS_DEPTH + 1) + &"]".repeat(LOADS_DEPTH + 1);
            assert_eq!(loads(&too), Err(JsonError::Recursion));
            let dicts = "{\"a\": ".repeat(30_000) + "1" + &"}".repeat(30_000);
            assert_eq!(dumps(&loads(&dicts).unwrap()), Err(JsonError::Recursion));
        });
    }

    #[test]
    fn int_reads_headers_like_python() {
        for (s, want) in [("1_000", Some(1000)), ("\u{663}", Some(3)), (" 7 ", Some(7)), ("\u{a0}5", Some(5)),
                          ("+60", Some(60)), ("-3", Some(-3)), ("1__0", None), ("_1", None), ("1.5", None),
                          ("\x1c5", None), ("", None), ("abc", None)] {
            assert_eq!(py_int(&ps(s)), want.map(BigInt::from), "{s:?}");
        }
        assert_eq!(py_int(&ps("99999999999999999999")).map(|n| n.to_string()), Some("99999999999999999999".into()));
    }
}
