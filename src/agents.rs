//! Port of `agents.py`.
//!
//! AI coding agents, which no package manager knows about.
//!
//! These install themselves: a curl-to-shell script, a version manager, or their
//! own `update` subcommand. pacman has never heard of them, so without this they
//! are the one class of software on the machine that silently rots -- and they are
//! also the ones shipping a release most weeks.
//!
//! Two things are deliberately not hardcoded:
//!
//! * How an agent is managed. It is read from where the binary actually resolves
//!   to, so moving `claude` from mise to the native installer changes the update
//!   command here without an edit.
//! * Which agents exist. The table below is the known set, but detection is by
//!   what is on PATH; an agent that is not installed simply never appears.
//!
//! The caches this module keeps are rewritten by the Python app too, and the
//! Python round-trips whatever is in them (key order, integers of any size,
//! NaN and Infinity, values of any type). So they are read and written here
//! through [`Py`], a JSON value with Python's `json` semantics, instead of
//! `serde_json::Value`, which cannot hold those. A Python exception that the
//! Python lets escape is an `Err(message)` here.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::inventory::App;
use crate::util;

/// `CACHE`: `Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")) / "almanac"`.
///
/// A function rather than a constant: Python computes it at import time from
/// the environment, which Rust cannot do in a `const`. It is read per call so
/// tests and parity can point it at a temp dir, the way the Python tests patch
/// `agents.CACHE`; a running app never changes its own XDG_CACHE_HOME.
/// Set-but-empty is kept, as `os.environ.get` keeps it: the relative `almanac`.
/// Spelled the way pathlib spells it (see [`pure_path`]).
pub fn cache() -> PathBuf {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let base: Vec<u8> = match std::env::var_os("XDG_CACHE_HOME") {
        Some(v) => v.into_vec(),
        None => {
            // Path(home) / ".cache"
            let mut h = py_home();
            if !h.ends_with(b"/") {
                h.push(b'/');
            }
            h.extend_from_slice(b".cache");
            h
        }
    };
    let mut p = pure_path(&base);
    p = if p == b"." { b"almanac".to_vec() } else if p.ends_with(b"/") { [p, b"almanac".to_vec()].concat() } else { [p, b"/almanac".to_vec()].concat() };
    PathBuf::from(std::ffi::OsStr::from_bytes(&p))
}

/// `os.path.expanduser("~")`, which `Path.home()` is: $HOME when it is set
/// (even to ""), else the password database's entry for this uid; trailing
/// slashes dropped, and "/" for what that leaves empty. (Were the uid not in
/// the database, the Python could not even import; util::home's "/" stands
/// in.)
fn py_home() -> Vec<u8> {
    use std::os::unix::ffi::OsStringExt;
    let home: Vec<u8> = match std::env::var_os("HOME") {
        Some(h) => h.into_vec(),
        None => match nix::unistd::User::from_uid(nix::unistd::getuid()) {
            Ok(Some(u)) => u.dir.into_os_string().into_vec(),
            _ => util::home().into_os_string().into_vec(),
        },
    };
    let mut end = home.len();
    while end > 0 && home[end - 1] == b'/' {
        end -= 1;
    }
    if end == 0 { b"/".to_vec() } else { home[..end].to_vec() }
}

/// How a `PurePosixPath` spells a path: a root of "/" (or exactly "//", which
/// POSIX lets mean something else), then the parts with empty ones and "."
/// dropped; "." when nothing is left. ".." is kept, as pathlib keeps it.
fn pure_path(p: &[u8]) -> Vec<u8> {
    let root: &[u8] = if p.starts_with(b"//") && !p.starts_with(b"///") {
        b"//"
    } else if p.starts_with(b"/") {
        b"/"
    } else {
        b""
    };
    let parts: Vec<&[u8]> = p.split(|&c| c == b'/').filter(|s| !s.is_empty() && *s != b".").collect();
    let mut out = root.to_vec();
    out.extend_from_slice(&parts.join(&b'/'));
    if out.is_empty() { b".".to_vec() } else { out }
}

/// `LATEST_CACHE`: `CACHE / "agent-latest.json"`.
pub fn latest_cache() -> PathBuf {
    cache().join("agent-latest.json")
}

pub const TTL: u64 = 6 * 3600;
pub const UA: (&str, &str) = ("User-Agent", "almanac");

/// inventory source key
pub const AGENT: &str = "agent";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Spec {
    /// the binary name on PATH
    pub id: String,
    pub name: String,
    /// owner/name, for the latest release
    pub repo: String,
    pub home: String,
    /// The vendor's own domain, used only to fetch their icon from them.
    pub domain: String,
    /// Its own updater, when it has one. Used only if the binary is not under
    /// a version manager that owns it.
    pub self_update: Vec<String>,
    pub note: String,
}

fn spec(id: &str, name: &str, repo: &str, home: &str, domain: &str, self_update: &[&str], note: &str) -> Spec {
    Spec {
        id: id.into(),
        name: name.into(),
        repo: repo.into(),
        home: home.into(),
        domain: domain.into(),
        self_update: self_update.iter().map(|s| s.to_string()).collect(),
        note: note.into(),
    }
}

pub static KNOWN: LazyLock<Vec<Spec>> = LazyLock::new(|| {
    vec![
        spec("claude", "Claude Code", "anthropics/claude-code",
             "https://claude.com/claude-code", "claude.com", &["claude", "update"], ""),
        spec("codex", "Codex CLI", "openai/codex",
             "https://github.com/openai/codex", "openai.com", &[], ""),
        spec("gemini", "Gemini CLI", "google-gemini/gemini-cli",
             "https://github.com/google-gemini/gemini-cli", "gemini.google.com", &[], ""),
        spec("opencode", "opencode", "anomalyco/opencode",
             "https://opencode.ai", "opencode.ai", &["opencode", "upgrade"], ""),
        spec("copilot", "GitHub Copilot CLI", "github/copilot-cli",
             "https://github.com/github/copilot-cli", "github.com",
             &["copilot", "update"], ""),
        spec("cursor-agent", "Cursor Agent", "", "https://cursor.com/cli",
             "cursor.com", &["cursor-agent", "update"],
             "Versions are dated builds, not semantic versions."),
        spec("crush", "Crush", "charmbracelet/crush",
             "https://github.com/charmbracelet/crush", "charm.sh", &[], ""),
        spec("goose", "Goose", "block/goose",
             "https://block.github.io/goose/", "block.xyz", &[], ""),
        spec("aider", "Aider", "Aider-AI/aider", "https://aider.chat",
             "aider.chat",
             &["python", "-m", "pip", "install", "--upgrade", "aider-chat"], ""),
        spec("qwen", "Qwen Code", "QwenLM/qwen-code",
             "https://github.com/QwenLM/qwen-code", "qwen.ai", &[], ""),
        spec("amp", "Amp", "", "https://ampcode.com", "ampcode.com",
             &["amp", "update"], ""),
        spec("droid", "Factory Droid", "", "https://factory.ai", "factory.ai",
             &["droid", "update"], ""),
    ]
});

pub static BY_ID: LazyLock<BTreeMap<String, Spec>> =
    LazyLock::new(|| KNOWN.iter().map(|s| (s.id.clone(), s.clone())).collect());

pub static VERSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\d+(?:\.\d+)+(?:[-+][0-9A-Za-z.]+)?").unwrap());

/// `VERSION_CACHE`: `CACHE / "agent-versions.json"`.
pub fn version_cache() -> PathBuf {
    cache().join("agent-versions.json")
}

// ---------------------------------------------------------------------------
// Python's json, its values, and the few builtins this module leans on.
// ---------------------------------------------------------------------------

/// A value as Python's `json.loads` returns it: dicts keep their order,
/// integers any size, floats NaN and Infinity. Dict keys from JSON are always
/// a str; `dict(...)` of a list (see [`py_dict`]) can make other hashable ones.
///
/// Nesting can go as deep as Python's json goes (tens of thousands of
/// levels), and dropping, cloning or writing such a value recurses that deep:
/// the entry points run on a thread with room for it (see [`deep`]).
#[derive(Debug, Clone)]
pub enum Py {
    None,
    Bool(bool),
    /// Canonical decimal digits, with a leading `-` when negative.
    Int(String),
    Float(f64),
    Str(String),
    /// A str holding a lone surrogate (a `\ud800` escape Python's json keeps,
    /// which a Rust `String` cannot): its code points. Only ever made for a
    /// string that has one, so it never equals a `Str`.
    WStr(Vec<u32>),
    List(Vec<Py>),
    Dict(Vec<(Py, Py)>),
}

impl Default for Py {
    fn default() -> Self {
        Py::Str(String::new())
    }
}

impl Py {
    fn s(v: &str) -> Py {
        Py::Str(v.to_string())
    }

    /// Python truthiness.
    pub fn truthy(&self) -> bool {
        match self {
            Py::None => false,
            Py::Bool(b) => *b,
            Py::Int(s) => s != "0",
            Py::Float(f) => *f != 0.0,
            Py::Str(s) => !s.is_empty(),
            Py::WStr(s) => !s.is_empty(),
            Py::List(l) => !l.is_empty(),
            Py::Dict(d) => !d.is_empty(),
        }
    }

    fn type_name(&self) -> &'static str {
        match self {
            Py::None => "NoneType",
            Py::Bool(_) => "bool",
            Py::Int(_) => "int",
            Py::Float(_) => "float",
            Py::Str(_) | Py::WStr(_) => "str",
            Py::List(_) => "list",
            Py::Dict(_) => "dict",
        }
    }

    /// `x.get(key)`: an AttributeError on anything but a dict.
    fn get(&self, key: &str) -> Result<Option<&Py>, String> {
        match self {
            Py::Dict(items) => Ok(dict_get(items, &Py::s(key))),
            other => Err(format!("AttributeError: '{}' object has no attribute 'get'", other.type_name())),
        }
    }

    /// `x or default`.
    fn or(&self, default: Py) -> Py {
        if self.truthy() { self.clone() } else { default }
    }

    /// The value as the parity hook reports it. JSON numbers beyond i64/u64
    /// become floats, NaN/Infinity null and a lone surrogate U+FFFD:
    /// `serde_json::Value` holds none of them.
    pub fn to_value(&self) -> serde_json::Value {
        use serde_json::Value;
        match self {
            Py::None => Value::Null,
            Py::Bool(b) => Value::Bool(*b),
            Py::Int(s) => {
                if let Ok(i) = s.parse::<i64>() {
                    Value::from(i)
                } else if let Ok(u) = s.parse::<u64>() {
                    Value::from(u)
                } else {
                    s.parse::<f64>().ok().and_then(serde_json::Number::from_f64).map(Value::Number).unwrap_or(Value::Null)
                }
            }
            Py::Float(f) => serde_json::Number::from_f64(*f).map(Value::Number).unwrap_or(Value::Null),
            Py::Str(s) => Value::String(s.clone()),
            Py::WStr(s) => Value::String(lossy(s)),
            Py::List(l) => Value::Array(l.iter().map(Py::to_value).collect()),
            Py::Dict(d) => Value::Object(d.iter().map(|(k, v)| (k.py_str(), v.to_value())).collect()),
        }
    }

    pub fn from_value(v: &serde_json::Value) -> Py {
        use serde_json::Value;
        match v {
            Value::Null => Py::None,
            Value::Bool(b) => Py::Bool(*b),
            Value::Number(n) => {
                if n.is_i64() || n.is_u64() {
                    Py::Int(n.to_string())
                } else {
                    Py::Float(n.as_f64().unwrap_or(f64::NAN))
                }
            }
            Value::String(s) => Py::Str(s.clone()),
            Value::Array(a) => Py::List(a.iter().map(Py::from_value).collect()),
            Value::Object(m) => Py::Dict(m.iter().map(|(k, v)| (Py::Str(k.clone()), Py::from_value(v))).collect()),
        }
    }

    /// Python's `str(x)`; a lone surrogate as U+FFFD.
    pub fn py_str(&self) -> String {
        match self {
            Py::Str(s) => s.clone(),
            Py::WStr(s) => lossy(s),
            other => other.py_repr(),
        }
    }

    /// Python's `repr(x)`.
    fn py_repr(&self) -> String {
        match self {
            Py::None => "None".into(),
            Py::Bool(true) => "True".into(),
            Py::Bool(false) => "False".into(),
            Py::Int(s) => s.clone(),
            Py::Float(f) if f.is_nan() => "nan".into(),
            Py::Float(f) if f.is_infinite() => if *f > 0.0 { "inf".into() } else { "-inf".into() },
            Py::Float(f) => util::py_float_repr(*f),
            Py::Str(s) => repr_str(&s.chars().map(|c| c as u32).collect::<Vec<_>>()),
            Py::WStr(s) => repr_str(s),
            Py::List(l) => format!("[{}]", l.iter().map(Py::py_repr).collect::<Vec<_>>().join(", ")),
            Py::Dict(d) => format!(
                "{{{}}}",
                d.iter().map(|(k, v)| format!("{}: {}", k.py_repr(), v.py_repr())).collect::<Vec<_>>().join(", ")
            ),
        }
    }
}

/// A str's code points as a Rust `String`, a lone surrogate as U+FFFD.
fn lossy(cps: &[u32]) -> String {
    cps.iter().map(|&c| char::from_u32(c).unwrap_or('\u{fffd}')).collect()
}

/// A str from code points: a `Str` unless one is a lone surrogate.
fn py_str_of(cps: Vec<u32>) -> Py {
    match cps.iter().map(|&c| char::from_u32(c)).collect::<Option<String>>() {
        Some(s) => Py::Str(s),
        None => Py::WStr(cps),
    }
}

/// Python's `repr()` of a str, given as code points (escapes for control
/// characters and lone surrogates).
fn repr_str(cps: &[u32]) -> String {
    let has = |c: char| cps.contains(&(c as u32));
    let q = if has('\'') && !has('"') { '"' } else { '\'' };
    let mut out = String::from(q);
    for &cp in cps {
        match char::from_u32(cp) {
            None => out.push_str(&format!("\\u{cp:04x}")),
            Some('\\') => out.push_str("\\\\"),
            Some('\n') => out.push_str("\\n"),
            Some('\r') => out.push_str("\\r"),
            Some('\t') => out.push_str("\\t"),
            Some(c) if c == q => {
                out.push('\\');
                out.push(c);
            }
            Some(c) if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            Some(c) => out.push(c),
        }
    }
    out.push(q);
    out
}

/// What a non-str value becomes where the Rust types only hold a string
/// (`inventory::App.version`): `str(x)`, and `""` for None.
fn as_text(v: &Py) -> String {
    match v {
        Py::None => String::new(),
        other => other.py_str(),
    }
}

/// `a == b` for the hashable values a dict key can be.
fn py_eq(a: &Py, b: &Py) -> bool {
    fn int_of(v: &Py) -> Option<String> {
        match v {
            Py::Bool(b) => Some(if *b { "1".into() } else { "0".into() }),
            Py::Int(s) => Some(s.clone()),
            Py::Float(f) if f.is_finite() && f.fract() == 0.0 => {
                let s = format!("{f:.0}");
                Some(if s == "-0" { "0".into() } else { s })
            }
            _ => None,
        }
    }
    match (a, b) {
        (Py::Str(x), Py::Str(y)) => x == y,
        (Py::WStr(x), Py::WStr(y)) => x == y,
        (Py::None, Py::None) => true,
        (Py::Float(x), Py::Float(y)) => x == y,
        (Py::Str(_) | Py::WStr(_) | Py::None | Py::List(_) | Py::Dict(_), _)
        | (_, Py::Str(_) | Py::WStr(_) | Py::None | Py::List(_) | Py::Dict(_)) => false,
        _ => match (int_of(a), int_of(b)) {
            (Some(x), Some(y)) => x == y,
            _ => false,
        },
    }
}

fn dict_get<'a>(items: &'a [(Py, Py)], key: &Py) -> Option<&'a Py> {
    items.iter().find(|(k, _)| py_eq(k, key)).map(|(_, v)| v)
}

/// `d[key] = value`: an existing equal key keeps its place.
fn dict_set(items: &mut Vec<(Py, Py)>, key: Py, value: Py) {
    match items.iter_mut().find(|(k, _)| py_eq(k, &key)) {
        Some(slot) => slot.1 = value,
        None => items.push((key, value)),
    }
}

/// The items of `iter(x)` for the iterables JSON has, or None.
fn py_iter(v: &Py) -> Option<Vec<Py>> {
    match v {
        Py::List(l) => Some(l.clone()),
        Py::Str(s) => Some(s.chars().map(|c| Py::Str(c.to_string())).collect()),
        Py::WStr(s) => Some(s.iter().map(|&c| py_str_of(vec![c])).collect()),
        Py::Dict(d) => Some(d.iter().map(|(k, _)| k.clone()).collect()),
        _ => None,
    }
}

/// `dict(x)`.
fn py_dict(v: &Py) -> Result<Vec<(Py, Py)>, String> {
    if let Py::Dict(d) = v {
        return Ok(d.clone());
    }
    let Some(items) = py_iter(v) else {
        return Err(format!("TypeError: '{}' object is not iterable", v.type_name()));
    };
    let mut out = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let Some(pair) = py_iter(item) else {
            return Err(format!("TypeError: cannot convert dictionary update sequence element #{i} to a sequence"));
        };
        if pair.len() != 2 {
            return Err(format!(
                "ValueError: dictionary update sequence element #{i} has length {}; 2 is required",
                pair.len()
            ));
        }
        let mut pair = pair.into_iter();
        let (k, v) = (pair.next().unwrap_or(Py::None), pair.next().unwrap_or(Py::None));
        if matches!(k, Py::List(_) | Py::Dict(_)) {
            return Err(format!("TypeError: unhashable type: '{}'", k.type_name()));
        }
        dict_set(&mut out, k, v);
    }
    Ok(out)
}

/// `needle in haystack` for a str needle.
fn py_contains(haystack: &Py, needle: &str) -> Result<bool, String> {
    match haystack {
        Py::Dict(d) => Ok(dict_get(d, &Py::s(needle)).is_some()),
        Py::List(l) => Ok(l.iter().any(|x| matches!(x, Py::Str(s) if s == needle))),
        Py::Str(s) => Ok(s.contains(needle)),
        Py::WStr(s) => {
            let n: Vec<u32> = needle.chars().map(|c| c as u32).collect();
            Ok(n.is_empty() || s.windows(n.len()).any(|w| w == n.as_slice()))
        }
        other => Err(format!("TypeError: argument of type '{}' is not iterable", other.type_name())),
    }
}

/// `a, b = x`.
fn unpack2(v: &Py) -> Result<(Py, Py), String> {
    let Some(items) = py_iter(v) else {
        return Err(format!("TypeError: cannot unpack non-iterable {} object", v.type_name()));
    };
    match items.len() {
        2 => {
            let mut it = items.into_iter();
            Ok((it.next().unwrap_or(Py::None), it.next().unwrap_or(Py::None)))
        }
        n if n > 2 => Err(format!("ValueError: too many values to unpack (expected 2, got {n})")),
        n => Err(format!("ValueError: not enough values to unpack (expected 2, got {n})")),
    }
}

/// `float - x` needs x to be a number.
fn py_float_of(v: &Py) -> Result<f64, String> {
    match v {
        Py::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Py::Int(s) => match s.parse::<f64>() {
            Ok(f) if f.is_finite() => Ok(f),
            _ => Err("OverflowError: int too large to convert to float".into()),
        },
        Py::Float(f) => Ok(*f),
        other => Err(format!("TypeError: unsupported operand type(s) for -: 'float' and '{}'", other.type_name())),
    }
}

/// Why `json.loads` failed: a ValueError (JSONDecodeError, or too many
/// digits for `int()`), which every caller here catches, or a RecursionError,
/// which none does.
#[derive(Debug, Clone, PartialEq)]
pub enum JsonError {
    Value(String),
    Recursion(String),
}

impl JsonError {
    fn message(self) -> String {
        match self {
            JsonError::Value(m) | JsonError::Recursion(m) => m,
        }
    }
}

/// Nesting at which CPython 3.14's C json scanner raises RecursionError (the
/// same figures settings.rs measured). Its limit is a C-stack byte budget,
/// not a count: with the default 8 MiB stack a worker thread decodes 52154
/// containers and not one more, arrays and dicts alike; the main thread
/// starts with part of its stack used and its limit moves from run to run
/// (52098 to 52131), so this is the middle of that range.
const MAX_DEPTH_MAIN: usize = 52118;
const MAX_DEPTH_THREAD: usize = 52154;

thread_local! {
    /// Which Python thread the code on this thread stands in for: Some(true)
    /// for the main thread, Some(false) for a worker, None when not set (then
    /// the thread's own name decides). Set by [`deep`].
    static PY_MAIN: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

fn py_main_thread() -> bool {
    PY_MAIN.with(|c| c.get()).unwrap_or_else(|| std::thread::current().name() == Some("main"))
}

/// Stack for the threads that hold the caches' values: enough to parse,
/// drop, clone and write one nested as deep as Python's json allows.
const DEEP_STACK: usize = 512 << 20;

/// `f()` on a thread with a [`DEEP_STACK`] stack (reserved, not used, until
/// a deep value needs it), standing in for the calling Python thread; at once
/// when already on one.
fn deep<R: Send>(f: impl FnOnce() -> R + Send) -> R {
    if PY_MAIN.with(|c| c.get()).is_some() {
        return f();
    }
    deep_as(py_main_thread(), f)
}

/// [`deep`] for a thread that stands in for the given kind of Python thread.
fn deep_as<R: Send>(main: bool, f: impl FnOnce() -> R + Send) -> R {
    let mut job = Some(f);
    let out = std::thread::scope(|s| {
        let job = &mut job;
        let spawned = std::thread::Builder::new().stack_size(DEEP_STACK).spawn_scoped(s, move || {
            PY_MAIN.with(|c| c.set(Some(main)));
            job.take().map(|f| f())
        });
        match spawned {
            Ok(h) => Some(h.join().unwrap_or_else(|p| std::panic::resume_unwind(p))),
            Err(_) => None,
        }
    });
    if let Some(Some(r)) = out {
        return r;
    }
    // The thread could not be started, so the job never ran: here, then.
    match job.take() {
        Some(f) => f(),
        None => unreachable!("a job that ran returned its result"),
    }
}

/// `json.loads(text)`: NaN, Infinity and -Infinity accepted, integers of any
/// size up to Python's 4300-digit `int()` limit, duplicate keys keep their
/// first place and last value, a lone `\uD8xx` escape kept (as Python keeps
/// it), and nesting as deep as Python's goes. Recursive: call it on a
/// [`deep`] thread.
fn json_loads(text: &str) -> Result<Py, JsonError> {
    let limit = if py_main_thread() { MAX_DEPTH_MAIN } else { MAX_DEPTH_THREAD };
    let mut p = JsonParser { s: text, b: text.as_bytes(), i: 0, limit };
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.i != p.b.len() {
        return Err(JsonError::Value(format!("JSONDecodeError: Extra data: char {}", p.i)));
    }
    Ok(v)
}

struct JsonParser<'a> {
    s: &'a str,
    b: &'a [u8],
    i: usize,
    limit: usize,
}

impl JsonParser<'_> {
    fn err<T>(&self, what: &str) -> Result<T, JsonError> {
        Err(JsonError::Value(format!("JSONDecodeError: {what}: char {}", self.i)))
    }

    /// Opening the container at `depth` (0 for the top level).
    fn enter(&self, depth: usize, what: &str) -> Result<(), JsonError> {
        if depth + 1 > self.limit {
            let used = if py_main_thread() { 8144 } else { 8156 };
            return Err(JsonError::Recursion(format!(
                "RecursionError: Stack overflow (used {used} kB) while decoding a JSON {what} from a unicode string"
            )));
        }
        Ok(())
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn lit(&mut self, word: &str, v: Py) -> Result<Py, JsonError> {
        if self.s[self.i..].starts_with(word) {
            self.i += word.len();
            Ok(v)
        } else {
            self.err("Expecting value")
        }
    }

    fn value(&mut self, depth: usize) -> Result<Py, JsonError> {
        match self.peek() {
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => self.string(),
            Some(b'n') => self.lit("null", Py::None),
            Some(b't') => self.lit("true", Py::Bool(true)),
            Some(b'f') => self.lit("false", Py::Bool(false)),
            Some(b'N') => self.lit("NaN", Py::Float(f64::NAN)),
            Some(b'I') => self.lit("Infinity", Py::Float(f64::INFINITY)),
            Some(b'-') if self.s[self.i..].starts_with("-Infinity") => self.lit("-Infinity", Py::Float(f64::NEG_INFINITY)),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => self.err("Expecting value"),
        }
    }

    fn digits(&mut self) {
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.i += 1;
        }
    }

    fn number(&mut self) -> Result<Py, JsonError> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        match self.peek() {
            Some(b'0') => self.i += 1,
            Some(b'1'..=b'9') => self.digits(),
            _ => {
                self.i = start;
                return self.err("Expecting value");
            }
        }
        let int_end = self.i;
        let mut is_float = false;
        if self.peek() == Some(b'.') && matches!(self.b.get(self.i + 1), Some(b'0'..=b'9')) {
            self.i += 1;
            self.digits();
            is_float = true;
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            let mut j = self.i + 1;
            if matches!(self.b.get(j), Some(b'+' | b'-')) {
                j += 1;
            }
            if matches!(self.b.get(j), Some(b'0'..=b'9')) {
                self.i = j;
                self.digits();
                is_float = true;
            }
        }
        let text = &self.s[start..self.i];
        if is_float {
            return Ok(Py::Float(text.parse().unwrap_or(f64::NAN)));
        }
        let digits = &self.s[start..int_end].trim_start_matches('-');
        if digits.len() > 4300 {
            return Err(JsonError::Value(format!(
                "ValueError: Exceeds the limit (4300 digits) for integer string conversion: value has {} digits",
                digits.len()
            )));
        }
        Ok(Py::Int(if text == "-0" { "0".into() } else { text.to_string() }))
    }

    fn hex4(&self, at: usize) -> Option<u32> {
        let h = self.s.get(at..at + 4)?;
        if !h.bytes().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        u32::from_str_radix(h, 16).ok()
    }

    /// A string, `Str` unless it keeps a lone surrogate (`WStr`).
    fn string(&mut self) -> Result<Py, JsonError> {
        self.i += 1;
        let mut out = String::new();
        // Code points, once a lone surrogate turns up.
        let mut wide: Option<Vec<u32>> = None;
        let push = |out: &mut String, wide: &mut Option<Vec<u32>>, cp: u32| match (wide.as_mut(), char::from_u32(cp)) {
            (Some(w), _) => w.push(cp),
            (None, Some(c)) => out.push(c),
            (None, None) => {
                let mut w: Vec<u32> = out.chars().map(|c| c as u32).collect();
                w.push(cp);
                *wide = Some(w);
            }
        };
        loop {
            let chunk = self.i;
            while matches!(self.peek(), Some(c) if c != b'"' && c != b'\\' && c >= 0x20) {
                self.i += 1;
            }
            for c in self.s[chunk..self.i].chars() {
                push(&mut out, &mut wide, c as u32);
            }
            match self.peek() {
                None => return self.err("Unterminated string starting at"),
                Some(b'"') => {
                    self.i += 1;
                    return Ok(match wide {
                        Some(w) => Py::WStr(w),
                        None => Py::Str(out),
                    });
                }
                Some(b'\\') => {
                    let esc = self.b.get(self.i + 1).copied();
                    self.i += 2;
                    let c = match esc {
                        Some(b'"') => '"' as u32,
                        Some(b'\\') => '\\' as u32,
                        Some(b'/') => '/' as u32,
                        Some(b'b') => 0x8,
                        Some(b'f') => 0xc,
                        Some(b'n') => '\n' as u32,
                        Some(b'r') => '\r' as u32,
                        Some(b't') => '\t' as u32,
                        Some(b'u') => {
                            let Some(mut c) = self.hex4(self.i) else {
                                return self.err("Invalid \\uXXXX escape");
                            };
                            self.i += 4;
                            if (0xd800..=0xdbff).contains(&c) && self.s[self.i..].starts_with("\\u") {
                                if let Some(lo) = self.hex4(self.i + 2).filter(|lo| (0xdc00..=0xdfff).contains(lo)) {
                                    c = 0x10000 + ((c - 0xd800) << 10) + (lo - 0xdc00);
                                    self.i += 6;
                                }
                            }
                            c
                        }
                        _ => {
                            self.i -= 1;
                            return self.err("Invalid \\escape");
                        }
                    };
                    push(&mut out, &mut wide, c);
                }
                Some(_) => return self.err("Invalid control character at"),
            }
        }
    }

    fn object(&mut self, depth: usize) -> Result<Py, JsonError> {
        self.enter(depth, "object")?;
        self.i += 1;
        let mut items = Vec::new();
        self.ws();
        if self.peek() == Some(b'}') {
            self.i += 1;
            return Ok(Py::Dict(items));
        }
        loop {
            if self.peek() != Some(b'"') {
                return self.err("Expecting property name enclosed in double quotes");
            }
            let key = self.string()?;
            self.ws();
            if self.peek() != Some(b':') {
                return self.err("Expecting ':' delimiter");
            }
            self.i += 1;
            self.ws();
            let v = self.value(depth + 1)?;
            dict_set(&mut items, key, v);
            self.ws();
            match self.peek() {
                Some(b',') => {
                    self.i += 1;
                    self.ws();
                }
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Py::Dict(items));
                }
                _ => return self.err("Expecting ',' delimiter"),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Py, JsonError> {
        self.enter(depth, "array")?;
        self.i += 1;
        let mut items = Vec::new();
        self.ws();
        if self.peek() == Some(b']') {
            self.i += 1;
            return Ok(Py::List(items));
        }
        loop {
            items.push(self.value(depth + 1)?);
            self.ws();
            match self.peek() {
                Some(b',') => {
                    self.i += 1;
                    self.ws();
                }
                Some(b']') => {
                    self.i += 1;
                    return Ok(Py::List(items));
                }
                _ => return self.err("Expecting ',' delimiter"),
            }
        }
    }
}

/// `json.dumps(v)` with Python's defaults (ensure_ascii, `", "` and `": "`).
/// Recursive: call it on a [`deep`] thread.
fn json_dumps(v: &Py) -> String {
    let mut out = String::new();
    dump_into(&mut out, v);
    out
}

fn dump_into(out: &mut String, v: &Py) {
    match v {
        Py::None => out.push_str("null"),
        Py::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Py::Int(s) => out.push_str(s),
        Py::Float(f) => out.push_str(&util::py_float_repr(*f)),
        Py::Str(s) => out.push_str(&util::py_json_string(s)),
        Py::WStr(s) => out.push_str(&json_string_cps(s)),
        Py::List(l) => {
            out.push('[');
            for (i, x) in l.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                dump_into(out, x);
            }
            out.push(']');
        }
        Py::Dict(d) => {
            out.push('{');
            for (i, (k, x)) in d.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                // Non-str keys are written the way json.dumps coerces them.
                let key = match k {
                    Py::Str(s) => s.clone(),
                    Py::WStr(s) => {
                        out.push_str(&json_string_cps(s));
                        out.push_str(": ");
                        dump_into(out, x);
                        continue;
                    }
                    Py::Float(f) => util::py_float_repr(*f),
                    Py::Bool(b) => (if *b { "true" } else { "false" }).into(),
                    Py::None => "null".into(),
                    other => other.py_str(),
                };
                out.push_str(&util::py_json_string(&key));
                out.push_str(": ");
                dump_into(out, x);
            }
            out.push('}');
        }
    }
}

/// `util::py_json_string` for a str that holds a lone surrogate: every code
/// point outside printable ASCII as `\uXXXX` (a surrogate as itself, others
/// past the BMP as a pair), as ensure_ascii writes them.
fn json_string_cps(cps: &[u32]) -> String {
    let mut out = String::from('"');
    for &cp in cps {
        match char::from_u32(cp) {
            Some(c) => {
                let one = util::py_json_string(c.encode_utf8(&mut [0; 4]));
                out.push_str(&one[1..one.len() - 1]);
            }
            None => out.push_str(&format!("\\u{cp:04x}")),
        }
    }
    out.push('"');
    out
}

/// `Path.read_text()`: strict UTF-8 (a decode error is a ValueError, which
/// every caller here catches with the OSErrors), universal newlines.
fn read_text(path: &std::path::Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    decode_text(&bytes).ok()
}

/// What `text=True` does to a child's output: strict UTF-8, then `\r\n` and
/// `\r` become `\n`.
fn decode_text(bytes: &[u8]) -> Result<String, String> {
    match std::str::from_utf8(bytes) {
        Ok(s) => Ok(s.replace("\r\n", "\n").replace('\r', "\n")),
        Err(e) => {
            let at = e.valid_up_to();
            Err(format!(
                "UnicodeDecodeError: 'utf-8' codec can't decode byte 0x{:02x} in position {at}: invalid start byte",
                bytes.get(at).copied().unwrap_or(0)
            ))
        }
    }
}

/// `json.loads(path.read_text())`: None for what the callers' `except
/// (OSError, ValueError)` swallows, Err for the RecursionError it does not.
fn read_json_file(path: &std::path::Path) -> Result<Option<Py>, String> {
    let Some(text) = read_text(path) else { return Ok(None) };
    match json_loads(&text) {
        Ok(v) => Ok(Some(v)),
        Err(JsonError::Value(_)) => Ok(None),
        Err(e @ JsonError::Recursion(_)) => Err(e.message()),
    }
}

/// `json.loads(out.stdout or "{}")` on a child's decoded output: None for a
/// ValueError (caught), Err for a RecursionError (not).
fn loads_output(text: &str) -> Result<Option<Py>, String> {
    match json_loads(if text.is_empty() { "{}" } else { text }) {
        Ok(v) => Ok(Some(v)),
        Err(JsonError::Value(_)) => Ok(None),
        Err(e @ JsonError::Recursion(_)) => Err(e.message()),
    }
}

/// `CACHE.mkdir(parents=True, exist_ok=True); path.write_text(json.dumps(v))`,
/// an OSError ignored as the Python ignores it.
fn write_json_file(path: &std::path::Path, v: &Py) {
    if std::fs::create_dir_all(cache()).is_ok() {
        let _ = std::fs::write(path, json_dumps(v));
    }
}

/// `subprocess.run(argv, capture_output=True, timeout=secs)` before the text
/// decode: stdin is inherited, as Python leaves it, and the raw bytes come
/// back so the decode can fail the way Python's does. None for an OSError or
/// a timeout. (`util::run` decodes lossily and closes stdin.)
///
/// The timeout covers reading both pipes to their end, as `communicate()`
/// does: a child that exits while something it started still holds its
/// output open times out like any other. On a timeout the child is killed
/// and reaped, and the readers are left to finish whenever the pipes close.
fn run_raw(argv: &[&str], timeout_secs: u64) -> Option<(Vec<u8>, Vec<u8>)> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let (prog, args) = argv.split_first()?;
    let mut child = Command::new(prog)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let reader = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut s = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut s);
            }
            s
        })
    };
    let out_t = reader(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err_t = reader(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);
    let mut exited = false;
    loop {
        if !exited {
            match child.try_wait() {
                Ok(Some(_)) => exited = true,
                Ok(None) => {}
                Err(_) => return None,
            }
        }
        if exited && out_t.is_finished() && err_t.is_finished() {
            break;
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Some((out_t.join().unwrap_or_default(), err_t.join().unwrap_or_default()))
}

/// `shutil.which(cmd)`: PATH, or `os.confstr("CS_PATH")` when PATH is unset
/// (glibc's is /bin:/usr/bin), and `os.access(X_OK)` for "executable" -- the
/// owner's bits for the owner, which `util::which` does not check. A name
/// with a directory part is looked up there alone, re-spelled as
/// `os.path.join(*os.path.split(cmd))` (so `bin//x` comes back as `bin/x`).
pub fn which(cmd: &str) -> Option<String> {
    fn access_check(p: &str) -> bool {
        match std::fs::metadata(p) {
            Ok(m) => !m.is_dir() && rustix::fs::access(p, rustix::fs::Access::EXEC_OK).is_ok(),
            Err(_) => false,
        }
    }
    if let Some(i) = cmd.rfind('/') {
        // posixpath.split: the head loses its trailing slashes unless it is
        // all slashes; join puts one back between head and tail.
        let (mut head, tail) = (&cmd[..=i], &cmd[i + 1..]);
        if !head.bytes().all(|c| c == b'/') {
            head = head.trim_end_matches('/');
        }
        let name = if head.ends_with('/') { format!("{head}{tail}") } else { format!("{head}/{tail}") };
        return access_check(&name).then_some(name);
    }
    let path = match std::env::var_os("PATH") {
        Some(p) => p.to_string_lossy().into_owned(),
        None => "/bin:/usr/bin".to_string(),
    };
    if path.is_empty() {
        return None;
    }
    let mut seen = HashSet::new();
    for dir in path.split(':') {
        if !seen.insert(dir) {
            continue;
        }
        let name = if dir.is_empty() {
            cmd.to_string()
        } else if dir.ends_with('/') {
            format!("{dir}{cmd}")
        } else {
            format!("{dir}/{cmd}")
        };
        if access_check(&name) {
            return Some(name);
        }
    }
    None
}

/// `os.path.realpath(path)` (non-strict), step for step as Python 3.14 does
/// it: `..` is applied to the path resolved so far, missing parts are kept.
/// Err only when the working directory cannot be read (an OSError).
pub fn realpath(filename: &str) -> Result<String, String> {
    let mut rest: Vec<Option<String>> = filename.split('/').rev().map(|s| Some(s.to_string())).collect();
    let mut part_count = rest.len();
    let mut path = if filename.starts_with('/') {
        "/".to_string()
    } else {
        std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .map_err(|e| format!("OSError: {e}"))?
    };
    let mut seen: HashMap<String, Option<String>> = HashMap::new();
    while part_count > 0 {
        let Some(name) = rest.pop() else { break };
        let Some(name) = name else {
            // resolved symlink target
            if let Some(Some(link)) = rest.pop() {
                seen.insert(link, Some(path.clone()));
            }
            continue;
        };
        part_count -= 1;
        if name.is_empty() || name == "." {
            continue;
        }
        if name == ".." {
            path = match path.rfind('/') {
                Some(i) if i > 0 => path[..i].to_string(),
                _ => "/".to_string(),
            };
            continue;
        }
        let newpath = if path == "/" { format!("/{name}") } else { format!("{path}/{name}") };
        match std::fs::symlink_metadata(&newpath) {
            Ok(m) if m.file_type().is_symlink() => {}
            _ => {
                path = newpath;
                continue;
            }
        }
        if let Some(hit) = seen.get(&newpath) {
            // Already seen: a cached target, or a symlink loop.
            path = hit.clone().unwrap_or(newpath);
            continue;
        }
        let target = match std::fs::read_link(&newpath) {
            Ok(t) => t.to_string_lossy().into_owned(),
            Err(_) => {
                path = newpath;
                continue;
            }
        };
        if target.starts_with('/') {
            path = "/".to_string();
        }
        seen.insert(newpath.clone(), None);
        rest.push(Some(newpath));
        rest.push(None);
        let parts: Vec<Option<String>> = target.split('/').rev().map(|s| Some(s.to_string())).collect();
        part_count += parts.len();
        rest.extend(parts);
    }
    Ok(path)
}

// ---------------------------------------------------------------------------

/// Size and mtime of what the name resolves to.
///
/// A CLI's version cannot change without its binary changing, so this is a
/// sound cache key -- and asking a 215MB binary for its version costs a
/// second of process start-up every time the inventory reloads.
///
/// Err for a NUL in the path: realpath's `os.lstat` raises ValueError there,
/// which `except OSError` does not catch.
pub fn binary_id(path: &str) -> Result<String, String> {
    use std::os::unix::fs::MetadataExt;
    if path.contains('\0') {
        return Err("ValueError: lstat: embedded null character in path".into());
    }
    let Ok(real) = realpath(path) else { return Ok(String::new()) };
    Ok(match std::fs::metadata(real) {
        // int(st.st_mtime) truncates toward zero, as `as i64` on the float does.
        Ok(st) => {
            let mtime = st.mtime() as f64 + st.mtime_nsec() as f64 / 1e9;
            format!("{}:{}", st.size(), mtime as i64)
        }
        Err(_) => String::new(),
    })
}

/// The cached version, or a fresh `version_of`. A cache hit comes back as
/// whatever the file holds (the Python returns `hit[1]` unchanged); Err for
/// a store that is not a dict (AttributeError on `.get`), one nested too deep
/// for Python's json (RecursionError), and for what [`binary_id`] and
/// [`try_version_of`] raise.
pub fn cached_version(binary: &str, path: &str) -> Result<Py, String> {
    deep(|| cached_version_here(binary, path))
}

fn cached_version_here(binary: &str, path: &str) -> Result<Py, String> {
    let key = binary_id(path)?;
    if key.is_empty() {
        return try_version_of(binary).map(Py::Str);
    }
    let mut store = read_json_file(&version_cache())?.unwrap_or(Py::Dict(Vec::new()));
    if let Some(Py::List(hit)) = store.get(binary)? {
        if hit.len() == 2 && matches!(&hit[0], Py::Str(k) if *k == key) {
            return Ok(hit[1].clone());
        }
    }
    let version = try_version_of(binary)?;
    if let Py::Dict(items) = &mut store {
        dict_set(items, Py::s(binary), Py::List(vec![Py::Str(key), Py::s(&version)]));
    }
    write_json_file(&version_cache(), &store);
    Ok(Py::Str(version))
}

/// `_version_of`: the version a binary reports for itself (`--version`,
/// `version`, `-v`), or "" when it reports none. Err when its output is not
/// UTF-8 (the Python's UnicodeDecodeError is not caught there) and for a NUL
/// in the name (subprocess's ValueError).
pub fn try_version_of(binary: &str) -> Result<String, String> {
    if binary.contains('\0') {
        return Err("ValueError: embedded null byte".into());
    }
    for args in [["--version"], ["version"], ["-v"]] {
        // (OSError, subprocess.SubprocessError): could not start, or timed out.
        let Some((stdout, stderr)) = run_raw(&[binary, args[0]], 20) else {
            return Ok(String::new());
        };
        let text = decode_text(&stdout)? + &decode_text(&stderr)?;
        if let Some(m) = VERSION.find(&text) {
            return Ok(m.as_str().to_string());
        }
    }
    Ok(String::new())
}

/// [`try_version_of`] with what the Python raises swallowed: "" for output
/// that is not UTF-8 or a NUL in the name. No Python caller swallows those
/// (`verify`'s probe lets them out), so callers should use `try_version_of`.
#[deprecated(note = "the Python's _version_of raises (UnicodeDecodeError, ValueError) to its caller; this answers \"\" instead. Use try_version_of and stop on Err as the Python caller would")]
pub fn version_of(binary: &str) -> String {
    try_version_of(binary).unwrap_or_default()
}

/// `MISE_CACHE`: `CACHE / "mise-outdated.json"`.
pub fn mise_cache() -> PathBuf {
    cache().join("mise-outdated.json")
}

/// Tools mise manages that are not coding agents. They update the same way and
/// were previously invisible: nothing on the machine reported that `gh` or
/// `uv` had a newer release waiting.
///
/// tool -> (display name, vendor domain)
pub const TOOL_DOMAINS: &[(&str, (&str, &str))] = &[
    ("gh", ("GitHub CLI", "github.com")),
    ("node", ("Node.js", "nodejs.org")),
    ("uv", ("uv", "astral.sh")),
    ("python", ("Python", "python.org")),
    ("go", ("Go", "go.dev")),
    ("rust", ("Rust", "rust-lang.org")),
    ("deno", ("Deno", "deno.com")),
    ("bun", ("Bun", "bun.sh")),
    ("terraform", ("Terraform", "terraform.io")),
    ("kubectl", ("kubectl", "kubernetes.io")),
];

/// {tool: active version} for everything mise manages.
///
/// Asked of mise rather than inferred from where a binary resolves to: a
/// shimmed tool resolves to /usr/bin/mise, which looks nothing like a mise
/// install path. Five of seven agents were misread as self-managed that
/// way, and then had their updates checked against the wrong upstream.
///
/// Never cached. `mise ls` reads local state in milliseconds, and a cached
/// answer kept reporting updates that had already been installed.
///
/// In mise's order, each version as mise wrote it (any JSON value). Err when
/// mise prints JSON that is not an object (AttributeError on `.items()`) or
/// is nested too deep for Python's json (RecursionError).
pub fn try_mise_installed() -> Result<Vec<(String, Py)>, String> {
    deep(|| MISE_LS.run(mise_installed_here))
}

/// One `mise ls --json` at a time: the scan's collectors asked for it
/// together, three runs within 40 ms. Not a cache; a caller that arrives
/// while one runs gets that run's answer.
static MISE_LS: SingleFlight<Result<Vec<(String, Py)>, String>> = SingleFlight::new();

/// One `mise outdated --json` at a time, as `MISE_LS`: concurrent callers
/// all missed the cache before the first had written it.
static MISE_OUTDATED: SingleFlight<Result<Py, String>> = SingleFlight::new();

/// Concurrent callers of `run` share one call of `f`: the first runs it,
/// the others wait for its answer and get a copy. Nothing is kept after.
pub struct SingleFlight<T> {
    inflight: std::sync::Mutex<Option<std::sync::Arc<Flight<T>>>>,
}

/// One call in flight: its answer once it has one (None inside when it
/// panicked, and each waiter then calls `f` itself).
struct Flight<T> {
    answer: std::sync::Mutex<Option<Option<T>>>,
    landed: std::sync::Condvar,
}

impl<T: Clone> SingleFlight<T> {
    pub const fn new() -> Self {
        SingleFlight { inflight: std::sync::Mutex::new(None) }
    }

    pub fn run(&self, f: impl FnOnce() -> T) -> T {
        use std::sync::{Arc, Condvar, Mutex};
        let (flight, leads) = {
            let mut g = self.inflight.lock().unwrap_or_else(|e| e.into_inner());
            match &*g {
                Some(flight) => (flight.clone(), false),
                None => {
                    let flight = Arc::new(Flight { answer: Mutex::new(None), landed: Condvar::new() });
                    *g = Some(flight.clone());
                    (flight, true)
                }
            }
        };
        if !leads {
            let mut answer = flight.answer.lock().unwrap_or_else(|e| e.into_inner());
            while answer.is_none() {
                answer = flight.landed.wait(answer).unwrap_or_else(|e| e.into_inner());
            }
            if let Some(Some(v)) = answer.as_ref() {
                return v.clone();
            }
            drop(answer);
            return f();
        }
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        // Landed: the next caller starts a run of its own.
        *self.inflight.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *flight.answer.lock().unwrap_or_else(|e| e.into_inner()) = Some(out.as_ref().ok().cloned());
        flight.landed.notify_all();
        match out {
            Ok(v) => v,
            Err(p) => std::panic::resume_unwind(p),
        }
    }
}

fn mise_installed_here() -> Result<Vec<(String, Py)>, String> {
    if which("mise").is_none() {
        return Ok(Vec::new());
    }
    let Some((stdout, stderr)) = run_raw(&["mise", "ls", "--json"], 90) else {
        return Ok(Vec::new());
    };
    // text=True decodes stderr too; its UnicodeDecodeError is a ValueError.
    let (Ok(text), Ok(_)) = (decode_text(&stdout), decode_text(&stderr)) else { return Ok(Vec::new()) };
    let Some(raw) = loads_output(&text)? else {
        return Ok(Vec::new());
    };
    let Py::Dict(raw) = raw else {
        return Err(format!("AttributeError: '{}' object has no attribute 'items'", raw.type_name()));
    };
    let mut data = Vec::new();
    for (tool, entries) in raw {
        let entries = match entries {
            Py::List(l) => l,
            other => vec![other],
        };
        for e in &entries {
            if let Py::Dict(fields) = e {
                if dict_get(fields, &Py::s("active")).is_some_and(Py::truthy) {
                    let version = dict_get(fields, &Py::s("version")).cloned().unwrap_or_default();
                    data.push((tool.py_str(), version));
                    break;
                }
            }
        }
    }
    Ok(data)
}

/// [`try_mise_installed`] as strings: a falsy version is "", a non-str one
/// `str()` of it, and what the Python raises is {}. The Python's caller
/// (`verify`'s mise probe, `.get(tool) or None`) gets the raw value and the
/// exception, so it should use `try_mise_installed`.
#[deprecated(note = "the Python's _mise_installed raises to its caller and keeps non-str versions as they are; this answers {} and strings instead. Use try_mise_installed")]
pub fn mise_installed() -> BTreeMap<String, String> {
    deep(|| {
        mise_installed_here()
            .unwrap_or_default()
            .into_iter()
            .map(|(k, v)| (k, if v.truthy() { as_text(&v) } else { String::new() }))
            .collect()
    })
}

/// What mise already knows. One call covers every tool it manages.
///
/// Cached because `mise outdated` queries every tool's upstream and takes
/// seconds; the answer does not change between two refreshes a minute apart.
/// Only its `latest` is trusted from the cache: its `current` goes stale the
/// moment anything is upgraded, so callers take that from mise_installed.
///
/// {tool: {"current", "latest", "release_url", ...}} as mise reports it --
/// whatever JSON it printed, dict or not. Pass [`TTL`] for Python's default.
/// Err for a cache file whose blob or `at` the Python cannot read (an
/// AttributeError or TypeError it does not catch), and for JSON nested too
/// deep for Python's (RecursionError).
pub fn mise_outdated(ttl: u64) -> Result<Py, String> {
    deep(|| mise_outdated_here(ttl))
}

fn mise_outdated_here(ttl: u64) -> Result<Py, String> {
    if which("mise").is_none() {
        return Ok(Py::Dict(Vec::new()));
    }
    if let Some(blob) = read_json_file(&mise_cache())? {
        let at = blob.get("at")?.cloned().unwrap_or(Py::Int("0".into()));
        if util::now() - py_float_of(&at)? < ttl as f64 {
            return Ok(blob.get("data")?.cloned().unwrap_or(Py::None).or(Py::Dict(Vec::new())));
        }
    }
    MISE_OUTDATED.run(|| {
        let Some((stdout, stderr)) = run_raw(&["mise", "outdated", "--json"], 120) else {
            return Ok(Py::Dict(Vec::new()));
        };
        // text=True decodes stderr too; its UnicodeDecodeError is a ValueError.
        let (Ok(text), Ok(_)) = (decode_text(&stdout), decode_text(&stderr)) else {
            return Ok(Py::Dict(Vec::new()));
        };
        let Some(data) = loads_output(&text)? else {
            return Ok(Py::Dict(Vec::new()));
        };
        let blob = Py::Dict(vec![(Py::s("at"), Py::Float(util::now())), (Py::s("data"), data.clone())]);
        write_json_file(&mise_cache(), &blob);
        Ok(data)
    })
}

/// {repo: [latest, release_url]} from the cache, or {} once it is older than
/// TTL: `blob.get("versions", {})` as stored, whatever its type. Err where
/// the Python raises (a blob that is not a dict, an `at` that is not a number,
/// nesting too deep for Python's json).
pub fn load_latest() -> Result<Py, String> {
    deep(load_latest_here)
}

fn load_latest_here() -> Result<Py, String> {
    if let Some(blob) = read_json_file(&latest_cache())? {
        let at = blob.get("at")?.cloned().unwrap_or(Py::Int("0".into()));
        if util::now() - py_float_of(&at)? < TTL as f64 {
            return Ok(blob.get("versions")?.cloned().unwrap_or(Py::Dict(Vec::new())));
        }
    }
    Ok(Py::Dict(Vec::new()))
}

pub fn save_latest(versions: &Py) {
    deep(|| save_latest_here(versions))
}

fn save_latest_here(versions: &Py) {
    let blob = Py::Dict(vec![(Py::s("at"), Py::Float(util::now())), (Py::s("versions"), versions.clone())]);
    write_json_file(&latest_cache(), &blob);
}

/// (version, release page) for the newest release, or ('','').
///
/// Goes through the shared cache, so this competes with nothing else for the
/// rate limit and costs nothing when the release has not moved.
///
/// The release page is `d.get("html_url", "")`: None, or whatever else the
/// release holds there, comes back as is. Err for a tag that is not a string.
pub fn github_latest(repo: &str) -> Result<(String, Py), String> {
    // What ghcache.get_json raises (a broken cache or rate file) raises
    // out of here too.
    let d = crate::ghcache::try_latest_release(repo, crate::ghcache::DEFAULT_TTL)?.map(|v| Py::from_value(&v));
    let Some(d @ Py::Dict(_)) = d else {
        return Ok((String::new(), Py::s("")));
    };
    let tag = d.get("tag_name")?.cloned().unwrap_or(Py::None).or(Py::s(""));
    let Py::Str(tag) = tag else {
        return Err(format!("TypeError: expected string or bytes-like object, got '{}'", tag.type_name()));
    };
    let version = match VERSION.find(&tag) {
        Some(m) => m.as_str().to_string(),
        None => tag.trim_start_matches('v').to_string(),
    };
    Ok((version, d.get("html_url")?.cloned().unwrap_or(Py::s(""))))
}

/// `pool.map(f, items)` on `workers` threads, results in input order.
/// The workers are Python worker threads (see [`deep`]); an item no worker
/// got to (one could not be started) is done here.
fn pool_map<T: Sync, R: Send>(items: &[T], workers: usize, f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<R>>> = Mutex::new((0..items.len()).map(|_| None).collect());
    std::thread::scope(|s| {
        for _ in 0..workers.min(items.len()) {
            let _ = worker(s, || loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                if i >= items.len() {
                    break;
                }
                let r = f(&items[i]);
                if let Ok(mut g) = results.lock() {
                    g[i] = Some(r);
                }
            });
        }
    });
    let results = results.into_inner().unwrap_or_else(|e| e.into_inner());
    results.into_iter().zip(items).map(|(r, item)| r.unwrap_or_else(|| f(item))).collect()
}

/// A scoped thread standing in for a Python worker thread, with a
/// [`DEEP_STACK`] stack.
fn worker<'scope, 'env, R: Send + 'scope>(
    s: &'scope std::thread::Scope<'scope, 'env>,
    f: impl FnOnce() -> R + Send + 'scope,
) -> std::io::Result<std::thread::ScopedJoinHandle<'scope, R>> {
    std::thread::Builder::new().stack_size(DEEP_STACK).spawn_scoped(s, move || {
        PY_MAIN.with(|c| c.set(Some(false)));
        f()
    })
}

/// `inventory.App.version` is a `String`; the Python stores whatever the
/// version source held (`current or "unknown"`).
fn app_version(v: &Py) -> String {
    match v {
        Py::Str(s) => s.clone(),
        other => as_text(other),
    }
}

/// Every known agent that is actually installed, with its update state.
///
/// Returns inventory.App objects so agents sit in the same list, the same
/// Updates count and the same sorting as everything else -- being unmanaged
/// by pacman is a packaging detail, not a reason for a separate screen.
///
/// Python's default is `check_updates=True`. Err where the Python raises.
pub fn try_detect(check_updates: bool) -> Result<Vec<App>, String> {
    deep(|| detect_here(check_updates)).map(strip_raw)
}

/// Each App with the version exactly as the Python stores it (any value:
/// `inventory::App.version` only holds its `str`), for the parity hook.
type RawApps = Vec<(App, Py)>;

fn strip_raw(v: RawApps) -> Vec<App> {
    v.into_iter().map(|(a, _)| a).collect()
}

fn detect_here(check_updates: bool) -> Result<RawApps, String> {
    use crate::inventory;
    use serde_json::{json, Value};

    let present: Vec<(&Spec, String)> =
        KNOWN.iter().filter_map(|spec| which(&spec.id).map(|p| (spec, p))).collect();
    if present.is_empty() {
        return Ok(Vec::new());
    }

    // Every mise shim resolves to the same mise binary, so the --version
    // cache cannot tell them apart; mise answers for its own tools instead.
    let mise_have = try_mise_installed()?;
    let have = |id: &str| mise_have.iter().find(|(k, _)| k == id).map(|(_, v)| v);
    let unmanaged: Vec<(&Spec, String)> =
        present.iter().filter(|(spec, _)| have(&spec.id).is_none()).cloned().collect();
    let (found, mise) = std::thread::scope(|s| {
        let mise_future = check_updates.then(|| worker(s, || mise_outdated(TTL)).ok());
        let found = pool_map(&unmanaged, 8, |(spec, path)| cached_version(&spec.id, path));
        let mise = match mise_future {
            Some(Some(h)) => h.join().unwrap_or_else(|p| std::panic::resume_unwind(p)),
            // The worker could not be started: ask here instead.
            Some(None) => mise_outdated(TTL),
            None => Ok(Py::Dict(Vec::new())),
        };
        (found, mise)
    });
    let mut versions: Vec<(String, Py)> = Vec::new();
    for ((spec, _), v) in unmanaged.iter().zip(found) {
        versions.push((spec.id.clone(), v?));
    }
    let mise = mise?;

    let cached = if check_updates { load_latest()? } else { Py::Dict(Vec::new()) };
    let mut fetched = py_dict(&cached)?;
    let mut added = false;
    let mut out = Vec::new();

    for (spec, path) in &present {
        let real = realpath(path)?;
        let mut current =
            versions.iter().find(|(k, _)| *k == spec.id).map(|(_, v)| v.clone()).unwrap_or(Py::s(""));

        // Who owns the update. Asked of mise, because a shimmed binary
        // resolves to the mise executable itself and tells you nothing.
        let managed_by_mise = have(&spec.id).is_some() || real.contains("/mise/installs/");
        let (mut latest, mut release_url) = (Py::s(""), Py::s(""));
        let (update_cmd, manager);

        if managed_by_mise {
            manager = "mise";
            update_cmd = vec!["mise".to_string(), "upgrade".to_string(), spec.id.clone()];
            let info = mise.get(&spec.id)?.cloned().unwrap_or(Py::None).or(Py::Dict(Vec::new()));
            latest = info.get("latest")?.cloned().unwrap_or(Py::None).or(Py::s(""));
            release_url = info.get("release_url")?.cloned().unwrap_or(Py::None).or(Py::s(""));
            // The active version from `mise ls`, read live -- the cached
            // `mise outdated` entry still names whatever was installed before
            // the last upgrade. `claude --version` can be an older linked build.
            let live = have(&spec.id).cloned().unwrap_or(Py::None);
            current = live.or(info.get("current")?.cloned().unwrap_or(Py::None).or(current));
        } else {
            manager = "self";
            update_cmd = spec.self_update.clone();
            if !spec.repo.is_empty() && check_updates {
                if py_contains(&cached, &spec.repo)? {
                    let hit = match &cached {
                        Py::Dict(d) => dict_get(d, &Py::s(&spec.repo)).cloned().unwrap_or(Py::None),
                        _ => Py::None,
                    };
                    (latest, release_url) = unpack2(&hit)?;
                } else {
                    let (l, u) = github_latest(&spec.repo)?;
                    if !l.is_empty() {
                        dict_set(&mut fetched, Py::s(&spec.repo), Py::List(vec![Py::s(&l), u.clone()]));
                        added = true;
                    }
                    (latest, release_url) = (Py::Str(l), u);
                }
            }
        }

        let mut update = String::new();
        if latest.truthy() && current.truthy() && newer_py(&latest, &current)? {
            // _newer only accepts strings, so both are by now.
            update = as_text(&latest);
        }

        // The rest of the inventory stores this as one (count, kind) tuple;
        // matching that is what lets agents use the same row renderer.
        let behind = if !update.is_empty() {
            inventory::versions_behind(&as_text(&current), &update)
        } else {
            (0, String::new())
        };

        let mut extra = serde_json::Map::new();
        extra.insert("key".into(), json!(spec.id));
        extra.insert("binary".into(), json!(spec.id));
        extra.insert("manager".into(), json!(manager));
        extra.insert("update_cmd".into(), json!(update_cmd));
        extra.insert("repo".into(), json!(spec.repo));
        extra.insert("url".into(), release_url.or(Py::s(&spec.home)).to_value());
        extra.insert("home".into(), json!(spec.home));
        extra.insert("note".into(), json!(spec.note));
        extra.insert("domain".into(), json!(spec.domain));
        extra.insert("behind".into(), json!([behind.0, behind.1]));
        extra.insert("is_app".into(), Value::Bool(false));
        extra.insert("real_path".into(), json!(real));

        let raw = current.or(Py::s("unknown"));
        out.push((App {
            name: spec.name.clone(),
            version: app_version(&raw),
            source: AGENT.into(),
            description: format!("AI coding agent · managed by {manager}"),
            update,
            exec_path: path.clone(),
            extra: extra.into(),
        }, raw));
    }

    // `fetched != cached`: a copy of a dict differs only by what was added;
    // anything else read from the cache is not a dict, so never equal.
    if added || !matches!(cached, Py::Dict(_)) {
        save_latest(&Py::Dict(fetched));
    }
    Ok(out)
}

/// [`try_detect`] with what the Python raises swallowed: no agents. Only
/// for `inventory::collect`, whose Python catches every exception around
/// it; the other Python callers (`ai.harnesses`, `autorun.update_agents`)
/// let it out, so they use `try_detect`.
pub fn detect(check_updates: bool) -> Vec<App> {
    try_detect(check_updates).unwrap_or_default()
}

/// Everything mise manages that is not one of the agents above.
///
/// Same update mechanism, same staleness problem, no reason to leave them
/// out just because they are not AI tooling.
///
/// Python's default is `check_updates=True`. Err where the Python raises.
pub fn try_mise_tools(check_updates: bool) -> Result<Vec<App>, String> {
    deep(|| mise_tools_here(check_updates)).map(strip_raw)
}

fn mise_tools_here(check_updates: bool) -> Result<RawApps, String> {
    let have = try_mise_installed()?;
    if have.is_empty() {
        return Ok(Vec::new());
    }
    let outdated = if check_updates { mise_outdated(TTL)? } else { Py::Dict(Vec::new()) };
    mise_tools_from_here(&have, &outdated)
}

/// [`try_mise_tools`], no tools where the Python would raise. Only for
/// `inventory::collect`, whose Python catches every exception around it.
pub fn mise_tools(check_updates: bool) -> Vec<App> {
    try_mise_tools(check_updates).unwrap_or_default()
}

/// The body of [`mise_tools`] once mise has been asked: split out so the tests
/// can hand it what the Python tests mock `_mise_installed` and
/// `_mise_outdated` to return.
pub fn mise_tools_from(have: &[(String, Py)], outdated: &Py) -> Result<Vec<App>, String> {
    deep(|| mise_tools_from_here(have, outdated)).map(strip_raw)
}

fn mise_tools_from_here(have: &[(String, Py)], outdated: &Py) -> Result<RawApps, String> {
    use crate::inventory;
    use serde_json::{json, Value};

    let mut have: Vec<&(String, Py)> = have.iter().collect();
    have.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = Vec::new();
    for (tool, version) in have {
        if BY_ID.contains_key(tool) {
            continue; // already reported as an agent
        }
        let info = outdated.get(tool)?.cloned().unwrap_or(Py::None).or(Py::Dict(Vec::new()));
        let latest = info.get("latest")?.cloned().unwrap_or(Py::None).or(Py::s(""));
        let update = if latest.truthy() && newer_py(&latest, version)? { as_text(&latest) } else { String::new() };
        let (name, domain) = TOOL_DOMAINS
            .iter()
            .find(|(k, _)| k == tool)
            .map(|(_, (n, d))| (n.to_string(), d.to_string()))
            .unwrap_or_else(|| (tool.clone(), String::new()));
        let fallback = if domain.is_empty() { String::new() } else { format!("https://{domain}") };
        let url = info.get("release_url")?.cloned().unwrap_or(Py::None).or(Py::Str(fallback));
        let behind = if !update.is_empty() {
            inventory::versions_behind(&as_text(version), &update)
        } else {
            (0, String::new())
        };
        let mut extra = serde_json::Map::new();
        extra.insert("key".into(), json!(tool));
        extra.insert("manager".into(), json!("mise"));
        extra.insert("domain".into(), json!(domain));
        extra.insert("update_cmd".into(), json!(["mise", "upgrade", tool]));
        extra.insert("url".into(), url.to_value());
        extra.insert("behind".into(), json!([behind.0, behind.1]));
        extra.insert("is_app".into(), Value::Bool(false));
        out.push((App {
            name,
            version: app_version(version),
            source: inventory::TOOL.into(),
            description: "Developer tool · managed by mise".into(),
            update,
            exec_path: which(tool).unwrap_or_default(),
            extra: extra.into(),
        }, version.clone()));
    }
    Ok(out)
}

/// A new major version: the kind of update that changes how a tool works.
///
/// These are held back from "update all" and from scheduled runs, and shown
/// with their release notes, because a CLI agent that renames its flags or
/// moves its config is not something to discover mid-task.
pub fn major_jump(current: &str, latest: &str) -> bool {
    !latest.is_empty() && crate::inventory::versions_behind(current, latest).1 == "major"
}

/// Python truthiness of a JSON value.
fn truthy_value(v: &serde_json::Value) -> bool {
    Py::from_value(v).truthy()
}

/// A major release that update-all and scheduled runs leave for a person
/// -- unless Preferences says to update majors like anything else.
///
/// Err for what `settings.get` raises (a settings file nested too deep for
/// Python's json: RecursionError).
pub fn try_hold_major(current: &str, latest: &str) -> Result<bool, String> {
    Ok(major_jump(current, latest) && truthy_value(&crate::settings::get("hold_majors")?))
}

/// [`try_hold_major`], false where the Python raises. No Python caller
/// catches that, so callers should use `try_hold_major`.
#[deprecated(note = "the Python's hold_major raises what settings.get raises (RecursionError) to its caller; this answers false instead. Use try_hold_major and stop on Err as the Python caller would")]
pub fn hold_major(current: &str, latest: &str) -> bool {
    try_hold_major(current, latest).unwrap_or(false)
}

/// Python's `str.strip()`: what `str.isspace` calls whitespace, which also
/// takes in the ASCII separators U+001C..U+001F that Rust's `trim` keeps.
fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}

/// The latest release's notes from GitHub, trimmed, or ''.
///
/// Python's default is `limit=2000` (characters); a negative one slices
/// from the end, as `body[:limit]` does. Err for a body that is not a string
/// (AttributeError on `.strip`).
pub fn try_release_notes(repo: &str, limit: i64) -> Result<String, String> {
    if repo.is_empty() {
        return Ok(String::new());
    }
    // What ghcache.get_json raises (a broken cache or rate file) raises
    // out of here too.
    let d = crate::ghcache::try_latest_release(repo, crate::ghcache::DEFAULT_TTL)?.map(|v| Py::from_value(&v));
    let mut body = match &d {
        Some(d @ Py::Dict(_)) => match d.get("body")?.cloned().unwrap_or(Py::None).or(Py::s("")) {
            Py::Str(s) => py_strip(&s).to_string(),
            other => return Err(format!("AttributeError: '{}' object has no attribute 'strip'", other.type_name())),
        },
        _ => String::new(),
    };
    let n = body.chars().count() as i64;
    if n > limit {
        let end = if limit >= 0 { limit.min(n) } else { (n + limit).max(0) };
        let cut: String = body.chars().take(end as usize).collect();
        let head = match cut.rfind('\n') {
            Some(i) => cut[..i].to_string(),
            None => cut,
        };
        body = head + "\n…";
    }
    Ok(body)
}

/// [`try_release_notes`], '' where the Python would raise.
pub fn release_notes(repo: &str, limit: i64) -> String {
    try_release_notes(repo, limit).unwrap_or_default()
}

static DIGIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d$").unwrap());
static DIGITS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d+").unwrap());

/// The value Python's `int()` gives a Unicode decimal digit. Decimal digits
/// come in contiguous runs of ten from zero, so a digit's value is how far
/// it sits into its run (runs can abut, hence the modulo).
fn digit_value(c: char) -> u8 {
    if let Some(d) = c.to_digit(10) {
        return d as u8;
    }
    let mut k = 0u32;
    let mut cp = c as u32;
    while cp > 0 {
        cp -= 1;
        match char::from_u32(cp) {
            Some(p) if DIGIT.is_match(p.encode_utf8(&mut [0; 4])) => k += 1,
            _ => break,
        }
    }
    (k % 10) as u8
}

/// An arbitrarily large non-negative integer, as Python's `int` holds one:
/// digits without leading zeros, compared by length and then lexically.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct BigUint {
    len: usize,
    digits: Vec<u8>,
}

/// True when `latest` is a higher version than `current`.
///
/// A string compare says 0.9.9 > 0.10.2, which is the bug this exists to
/// avoid. Unparseable pairs (Cursor's dated builds vs a tag) compare as
/// equal rather than claiming an update that may not exist.
///
/// Err as Python's `int()` refuses a run of more than 4300 digits.
pub fn newer(latest: &str, current: &str) -> Result<bool, String> {
    newer_py(&Py::s(latest), &Py::s(current))
}

/// [`newer`] on whatever the caches held: `re.findall` needs a string, so a
/// truthy non-string is a TypeError.
fn newer_py(latest: &Py, current: &Py) -> Result<bool, String> {
    fn key(v: &Py) -> Result<Vec<BigUint>, String> {
        let text = match v.or(Py::s("")) {
            Py::Str(s) => s,
            // A surrogate is no digit: U+FFFD in its place finds the same runs.
            Py::WStr(s) => lossy(&s),
            other => return Err(format!("TypeError: expected string or bytes-like object, got '{}'", other.type_name())),
        };
        let mut parts = Vec::new();
        for m in DIGITS.find_iter(&text).take(4) {
            let n = m.as_str().chars().count();
            if n > 4300 {
                return Err(format!(
                    "ValueError: Exceeds the limit (4300 digits) for integer string conversion: value has {n} digits"
                ));
            }
            let ds: Vec<u8> = m.as_str().chars().map(digit_value).collect();
            let start = ds.iter().position(|&d| d != 0).unwrap_or(ds.len());
            let digits = ds[start..].to_vec();
            parts.push(BigUint { len: digits.len(), digits });
        }
        Ok(parts)
    }

    let (a, b) = (key(latest)?, key(current)?);
    if a.is_empty() || b.is_empty() {
        return Ok(false);
    }
    Ok(a > b)
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_flight_shares_one_run_among_concurrent_callers() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static RUNS: AtomicUsize = AtomicUsize::new(0);
        static FLIGHT: SingleFlight<usize> = SingleFlight::new();
        let slow = || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            RUNS.fetch_add(1, Ordering::SeqCst) + 1
        };
        let answers: Vec<usize> = std::thread::scope(|s| {
            let hs: Vec<_> = (0..6).map(|_| s.spawn(|| FLIGHT.run(slow))).collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        // Six callers within the 200 ms the first one took: one run.
        assert_eq!(RUNS.load(Ordering::SeqCst), 1);
        assert!(answers.iter().all(|&a| a == 1));
        // Nothing is kept: the next caller runs again.
        assert_eq!(FLIGHT.run(slow), 2);
        // A run that panics lets each waiter run for itself.
        static PANICKY: SingleFlight<u8> = SingleFlight::new();
        let r = std::thread::scope(|s| {
            let leader = s.spawn(|| {
                std::panic::catch_unwind(|| {
                    PANICKY.run(|| {
                        std::thread::sleep(std::time::Duration::from_millis(200));
                        panic!("boom")
                    })
                })
                .is_err()
            });
            std::thread::sleep(std::time::Duration::from_millis(50));
            let follower = s.spawn(|| PANICKY.run(|| 7));
            (leader.join().unwrap(), follower.join().unwrap())
        });
        assert_eq!(r, (true, 7));
    }

    // UpdatesClearAfterUpdating
    #[test]
    fn test_mise_compares_the_live_version_not_the_cached_one() {
        let stale = Py::from_value(&serde_json::json!({"uv": {"current": "0.12.13", "latest": "0.13.1"}}));
        let have = vec![("uv".to_string(), Py::s("0.13.1"))];
        let tools = mise_tools_from(&have, &stale).unwrap();
        assert_eq!(tools.len(), 1);
        let uv = &tools[0];
        assert_eq!(uv.version, "0.13.1");
        assert!(!uv.has_update());
    }

    // MajorReleases
    #[test]
    fn test_major_jumps_are_recognised() {
        assert!(major_jump("1.19.3", "2.0.30"));
        assert!(!major_jump("2.1.265", "2.1.301"));
        assert!(!major_jump("1.0", ""));
    }

    // Settings (setUp points XDG_STATE_HOME at a temp dir)
    #[test]
    fn test_major_releases_are_held_only_while_set() {
        let _g = crate::util::TEST_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let old = std::env::var_os("XDG_STATE_HOME");
        std::env::set_var("XDG_STATE_HOME", tmp.path());
        let held = try_hold_major("1.0", "2.0").unwrap();
        crate::settings::set("hold_majors", serde_json::Value::Bool(false)).unwrap();
        let after = try_hold_major("1.0", "2.0").unwrap();
        match old {
            Some(v) => std::env::set_var("XDG_STATE_HOME", v),
            None => std::env::remove_var("XDG_STATE_HOME"),
        }
        assert!(held);
        assert!(!after);
    }

    // The docstring's own example: a string compare gets this wrong.
    #[test]
    fn newer_compares_numerically() {
        assert!(newer("0.10.2", "0.9.9").unwrap());
        assert!(!newer("0.9.9", "0.10.2").unwrap());
        assert!(!newer("2025.01.01-abc", "").unwrap());
        assert!(newer("99999999999999999999999.1", "99999999999999999999998.9").unwrap());
        assert!(newer(&("1".repeat(5000) + ".1"), "1.0").is_err());
    }

    // The caches round-trip the way Python's json does.
    #[test]
    fn json_round_trips_like_python() {
        let text = r#"{"zz": [1, 123456789012345678901234, 2.50, 1e5, -0, NaN, -Infinity], "a": {"ü": null}, "zz": true}"#;
        let v = json_loads(text).unwrap();
        assert_eq!(
            json_dumps(&v),
            r#"{"zz": true, "a": {"\u00fc": null}}"#
        );
        let v = json_loads(r#"[123456789012345678901234, 2.50, 1e5, -0, NaN, -Infinity, 1e-7, "\ud83d\ude00"]"#).unwrap();
        assert_eq!(json_dumps(&v), r#"[123456789012345678901234, 2.5, 100000.0, 0, NaN, -Infinity, 1e-07, "\ud83d\ude00"]"#);
        assert!(json_loads("[1,]").is_err());
        assert!(json_loads("01").is_err());
        assert!(json_loads(&"9".repeat(4301)).is_err());
    }

    #[test]
    fn realpath_normalises_before_resolving() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("f");
        std::fs::write(&f, "x").unwrap();
        let f = f.to_string_lossy().into_owned();
        let real = realpath(&f).unwrap();
        assert_eq!(realpath(&format!("{f}/")).unwrap(), real);
        assert_eq!(realpath(&format!("{f}/.")).unwrap(), real);
        let via = format!("{}/nope/../f", tmp.path().display());
        assert_eq!(realpath(&via).unwrap(), real);
    }

    // Python's json keeps a lone surrogate escape and writes it back as one.
    #[test]
    fn lone_surrogates_round_trip() {
        let v = json_loads(r#"{"other": ["1:2", "a\ud800b"], "\udc00": 1, "ok": "😀"}"#).unwrap();
        assert_eq!(json_dumps(&v), r#"{"other": ["1:2", "a\ud800b"], "\udc00": 1, "ok": "\ud83d\ude00"}"#);
        let Py::Dict(d) = &v else { panic!() };
        assert!(matches!(&d[1].0, Py::WStr(c) if c == &vec![0xdc00]));
        assert!(matches!(&d[2].1, Py::Str(_)));
        assert!(!py_eq(&d[1].0, &Py::s("\u{fffd}")));
        assert_eq!(d[1].0.py_repr(), r"'\udc00'");
    }

    // As deep as Python's json goes, and a RecursionError (not a ValueError)
    // one level past it.
    #[test]
    fn nesting_as_deep_as_python() {
        let nest = |n: usize| "[".repeat(n) + &"]".repeat(n);
        let (ok, past) = deep_as(false, || {
            let ok = json_loads(&nest(MAX_DEPTH_THREAD)).map(|v| json_dumps(&v).len());
            (ok, json_loads(&nest(MAX_DEPTH_THREAD + 1)).err())
        });
        assert_eq!(ok, Ok(2 * MAX_DEPTH_THREAD));
        assert_eq!(
            past,
            Some(JsonError::Recursion(
                "RecursionError: Stack overflow (used 8156 kB) while decoding a JSON array from a unicode string".into()
            ))
        );
        assert!(matches!(json_loads("[1,"), Err(JsonError::Value(_))));
    }

    // How pathlib spells a path.
    #[test]
    fn paths_are_spelled_as_pathlib_does() {
        assert_eq!(pure_path(b"/a//b/./c/"), b"/a/b/c");
        assert_eq!(pure_path(b"//a"), b"//a");
        assert_eq!(pure_path(b"///a/"), b"/a");
        assert_eq!(pure_path(b""), b".");
        assert_eq!(pure_path(b"a/../b"), b"a/../b");
    }

    // shutil.which re-spells a name with a directory part.
    #[test]
    fn which_rejoins_a_directory_part() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("tool");
        std::fs::write(&f, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&f, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let dir = tmp.path().display().to_string();
        assert_eq!(which(&format!("{dir}//tool")), Some(format!("{dir}/tool")));
        assert_eq!(which(&format!("{dir}/nope")), None);
    }

    // The timeout covers the pipes, as communicate()'s does: a child that
    // exits while what it started still holds stdout open times out.
    #[test]
    fn timeout_waits_for_the_pipes() {
        let t = std::time::Instant::now();
        assert_eq!(run_raw(&["sh", "-c", "echo 1.2.3; (sleep 3) &"], 1), None);
        assert!(t.elapsed() < std::time::Duration::from_secs(3));
        let (out, _) = run_raw(&["sh", "-c", "echo 1.2.3"], 5).unwrap();
        assert_eq!(out, b"1.2.3\n");
        assert_eq!(try_version_of("ls\0x"), Err("ValueError: embedded null byte".into()));
        assert!(binary_id("/usr/bin/ls\0x").is_err());
    }
}
