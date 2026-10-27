//! Scheduled updates: the settings, and the systemd timer behind them.
//!
//! The package ships the timer and its service in /usr/lib/systemd/user, off
//! until switched on here; this is the part that lets the app switch them and
//! change the schedule without the user editing unit files. A full copy of the
//! timer in ~/.config/systemd/user (how it was installed before it shipped) is
//! still edited in place. Everything that is a
//! *policy* decision lives in one JSON file that both this module and the shell
//! script read, so the checkbox in the window and the behaviour at 11am on Sunday
//! cannot drift apart.
//!
//! What is deliberately not configurable: whether risky batches are held when the
//! machine would need a human (kernel, bootloader, fresh Arch news). That is a
//! safety property, not a preference -- see omarchy-auto-update.
//!
//! Port of `autoupdate.py`.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use num_bigint::BigInt;
use serde::{Serialize, Serializer};
use serde_json::{Map, Value};

// --- Paths, as pathlib spells them ---------------------------------------

/// `posixpath.join(a, b)` for a relative `b`.
fn py_join(a: &[u8], b: &[u8]) -> Vec<u8> {
    let mut out = a.to_vec();
    if b.first() == Some(&b'/') {
        return b.to_vec();
    }
    if !out.is_empty() && out.last() != Some(&b'/') {
        out.push(b'/');
    }
    out.extend_from_slice(b);
    out
}

/// `str(PurePosixPath(p))`: exactly two leading slashes stay, any other run
/// of slashes is one, `.` components go, a trailing slash goes, "" is ".".
fn py_norm(p: &[u8]) -> Vec<u8> {
    let (root, rest): (&[u8], &[u8]) = if p.first() != Some(&b'/') {
        (b"", p)
    } else if p.get(1) != Some(&b'/') || p.get(2) == Some(&b'/') {
        (b"/", &p[1..])
    } else {
        (b"//", &p[2..])
    };
    let parts: Vec<&[u8]> = rest.split(|&c| c == b'/').filter(|x| !x.is_empty() && *x != b".").collect();
    let mut out = root.to_vec();
    out.extend_from_slice(&parts.join(&b'/'));
    if out.is_empty() {
        out.push(b'.');
    }
    out
}

/// `Path.home()`: `os.path.expanduser("~")`, which is `$HOME` when it is set
/// (even to ""), else the passwd entry (`pwd.getpwuid`, through NSS, so
/// LDAP / systemd-homed / userdb users count); trailing slashes go, "" is
/// "/". None where `Path.home()` raises RuntimeError: no HOME and no passwd
/// entry, so expanduser leaves "~" alone.
fn py_home() -> Option<Vec<u8>> {
    let home = match std::env::var_os("HOME") {
        Some(h) => h.into_vec(),
        None => passwd_home()?,
    };
    let mut h: &[u8] = &home;
    while h.last() == Some(&b'/') {
        h = &h[..h.len() - 1];
    }
    Some(if h.is_empty() { b"/".to_vec() } else { h.to_vec() })
}

/// `pwd.getpwuid(os.getuid()).pw_dir`; None for KeyError (no entry).
fn passwd_home() -> Option<Vec<u8>> {
    let user = nix::unistd::User::from_uid(nix::unistd::getuid()).ok()??;
    Some(user.dir.into_os_string().into_vec())
}

const NO_HOME: &str = "Could not determine home directory.";

/// The module-level `Path.home()` runs at import, even when
/// $XDG_CONFIG_HOME is set (the `os.environ.get` default is evaluated
/// first), so where it raises, `import autoupdate` fails and none of the
/// functions below can run: each answers that RuntimeError instead.
static IMPORT: LazyLock<Result<(), String>> =
    LazyLock::new(|| py_home().map(|_| ()).ok_or_else(|| NO_HOME.to_owned()));

fn imported() -> Result<(), String> {
    IMPORT.clone()
}

fn to_path(b: Vec<u8>) -> PathBuf {
    PathBuf::from(OsString::from_vec(b))
}

/// `$XDG_CONFIG_HOME` as the Python reads it: set (even to "") wins, with
/// no absolute-path check, unlike `xdg.config_home()`; else `Path.home() / ".config"`.
/// (Without a home the paths are never used: see [`IMPORT`].)
fn config_root() -> Vec<u8> {
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(v) => v.into_vec(),
        None => py_norm(&py_join(&py_norm(&py_home().unwrap_or_else(|| b"~".to_vec())), b".config")),
    }
}

pub static CONFIG_DIR: LazyLock<PathBuf> = LazyLock::new(|| to_path(py_norm(&py_join(&config_root(), b"almanac"))));
pub static CONFIG: LazyLock<PathBuf> =
    LazyLock::new(|| to_path(py_norm(&py_join(CONFIG_DIR.as_os_str().as_bytes(), b"auto-update.json"))));
pub const UNIT: &str = "omarchy-auto-update.timer";
pub static TIMER_FILE: LazyLock<PathBuf> = LazyLock::new(|| {
    to_path(py_norm(&py_join(&py_join(&config_root(), b"systemd/user"), UNIT.as_bytes())))
});
/// The timer as the package installs it. Never edited: a schedule chosen
/// here goes in [`SCHEDULE_DROPIN`], which overrides only `OnCalendar=`.
pub const SHIPPED_TIMER: &str = "/usr/lib/systemd/user/omarchy-auto-update.timer";
/// The user's override of the shipped timer's schedule.
pub static SCHEDULE_DROPIN: LazyLock<PathBuf> =
    LazyLock::new(|| TIMER_FILE.with_file_name(format!("{UNIT}.d")).join("schedule.conf"));
/// The script the timer runs, as the package installs it.
pub const SHIPPED_SCRIPT: &str = "/usr/lib/almanac/omarchy-auto-update";

/// The script to run for "run it now": the packaged one, else the copy in
/// ~/.local/bin that installs from before it shipped used.
pub fn script_path() -> PathBuf {
    let shipped = Path::new(SHIPPED_SCRIPT);
    if shipped.exists() {
        shipped.to_path_buf()
    } else {
        crate::util::home().join(".local/bin/omarchy-auto-update")
    }
}

/// Where the timer's schedule can live: a full user copy of the unit, the
/// packaged unit, and a drop-in overriding the packaged one.
struct TimerFiles<'a> {
    user: &'a Path,
    shipped: &'a Path,
    dropin: &'a Path,
}

impl TimerFiles<'static> {
    fn real() -> Self {
        TimerFiles { user: &TIMER_FILE, shipped: Path::new(SHIPPED_TIMER), dropin: &SCHEDULE_DROPIN }
    }
}

impl TimerFiles<'_> {
    /// Whether a schedule goes in the drop-in: once there is one (it
    /// overrides any unit file, the user's copy included), or when the
    /// packaged unit is the only one.
    fn uses_dropin(&self) -> bool {
        self.dropin.exists() || (std::fs::symlink_metadata(self.user).is_err() && self.shipped.exists())
    }
}

/// label -> OnCalendar: `(key, label, OnCalendar)`.
pub const SCHEDULES: &[(&str, &str, &str)] = &[
    ("daily", "Every day at 11:00", "*-*-* 11:00:00"),
    ("weekly", "Every Sunday at 11:00", "Sun 11:00"),
    ("fortnightly", "Every other Sunday", "Sun *-*-1..7,15..21 11:00:00"),
    ("monthly", "First Sunday of the month", "Sun *-*-1..7 11:00:00"),
];

// --- Python values, as json.loads builds them -----------------------------

/// A Python `str`: code points, so a lone surrogate (`"\ud800"` in the
/// file, which `json.loads` accepts) survives a load/save round trip.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct PyStr(pub Vec<u32>);

impl PyStr {
    pub fn new(s: &str) -> Self {
        PyStr(s.chars().map(u32::from).collect())
    }

    /// The text, or None when it holds a surrogate.
    pub fn as_string(&self) -> Option<String> {
        self.0.iter().map(|&c| char::from_u32(c)).collect()
    }

    /// The text with any surrogate replaced by U+FFFD.
    pub fn to_string_lossy(&self) -> String {
        self.0.iter().map(|&c| char::from_u32(c).unwrap_or('\u{fffd}')).collect()
    }
}

/// What `json.loads` gives: ints of any size (and `-0` is the int 0),
/// floats including NaN and the infinities, dicts in file order with the
/// last of a duplicate key winning.
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

impl Drop for PyValue {
    // Python takes a JSON file nested 50,000 deep; dropping that recursively
    // would overflow the stack, so children are taken out and dropped flat.
    fn drop(&mut self) {
        fn take(v: &mut PyValue, stack: &mut Vec<PyValue>) {
            match v {
                PyValue::List(a) => stack.append(a),
                PyValue::Dict(d) => stack.extend(d.drain(..).map(|(_, x)| x)),
                _ => {}
            }
        }
        let mut stack = Vec::new();
        take(self, &mut stack);
        while let Some(mut v) = stack.pop() {
            take(&mut v, &mut stack);
        }
    }
}

/// A dict key: what a JSON array of pairs can put there.
#[derive(Debug, Clone, PartialEq)]
pub enum PyKey {
    None,
    Bool(bool),
    Int(BigInt),
    Float(f64),
    Str(PyStr),
}

/// Python's hash/equality classes for keys: `True == 1 == 1.0`, `1e16 ==
/// 10**16`, `0 == -0.0 == False`. Every NaN from `json.loads` is the same
/// object (`json.decoder.NaN`), so NaN keys are one key too.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum KeyClass {
    None,
    Int(BigInt),
    Float(u64),
    PosInf,
    NegInf,
    NaN,
    Str(Vec<u32>),
}

/// The exact integer an integral finite float holds.
fn float_to_int(f: f64) -> BigInt {
    BigInt::parse_bytes(format!("{f:.0}").as_bytes(), 10).unwrap_or_default()
}

fn key_class(k: &PyKey) -> KeyClass {
    match k {
        PyKey::None => KeyClass::None,
        PyKey::Bool(b) => KeyClass::Int(BigInt::from(*b as i64)),
        PyKey::Int(n) => KeyClass::Int(n.clone()),
        PyKey::Float(f) if f.is_nan() => KeyClass::NaN,
        PyKey::Float(f) if f.is_infinite() => {
            if *f > 0.0 {
                KeyClass::PosInf
            } else {
                KeyClass::NegInf
            }
        }
        PyKey::Float(f) if f.fract() == 0.0 => KeyClass::Int(float_to_int(*f)),
        PyKey::Float(f) => KeyClass::Float(f.to_bits()),
        PyKey::Str(s) => KeyClass::Str(s.0.clone()),
    }
}

/// Python truthiness.
fn truthy(v: &PyValue) -> bool {
    match v {
        PyValue::None => false,
        PyValue::Bool(b) => *b,
        PyValue::Int(n) => *n != BigInt::ZERO,
        PyValue::Float(f) => *f != 0.0,
        PyValue::Str(s) => !s.0.is_empty(),
        PyValue::List(a) => !a.is_empty(),
        PyValue::Dict(d) => !d.is_empty(),
    }
}

/// `repr(float)` / `str(float)`.
fn float_str(f: f64) -> String {
    if f.is_nan() {
        "nan".into()
    } else if f.is_infinite() {
        if f > 0.0 { "inf".into() } else { "-inf".into() }
    } else {
        crate::util::py_float_repr(f)
    }
}

/// json's `float.__repr__` with `allow_nan=True`.
fn json_float(f: f64) -> String {
    if f.is_nan() {
        "NaN".into()
    } else if f.is_infinite() {
        if f > 0.0 { "Infinity".into() } else { "-Infinity".into() }
    } else {
        crate::util::py_float_repr(f)
    }
}

/// A JSON string as `json.dumps` writes it (`ensure_ascii=True`), code
/// point by code point, so a lone surrogate comes out as `\udXXX`.
fn json_str(s: &PyStr, out: &mut String) {
    out.push('"');
    for &c in &s.0 {
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

// --- json.loads ------------------------------------------------------------

/// How deep `json.loads` nests before it gives up with RecursionError (3.14
/// measures its C stack: 52130 containers on an 8 MB main-thread stack).
const DECODE_MAX_DEPTH: usize = 52130;
/// The same for `json.dumps(indent=2)`, counting the top-level dict and
/// empty containers (52120, measured the same way).
const ENCODE_MAX_DEPTH: usize = 52120;

/// How `json.loads` failed: a ValueError (JSONDecodeError, or an int over
/// the digit limit), which `load` catches, or a RecursionError, which it
/// does not.
enum LoadsError {
    Value(#[allow(dead_code)] String),
    Recursion(String),
}

/// `json.loads(s)` with the C scanner's rules (strict strings, NaN and
/// Infinity accepted), nesting as deep as Python's.
fn json_loads(text: &str) -> Result<PyValue, LoadsError> {
    use crate::util::pyjson;
    pyjson::loads(text, pyjson::Limit::containers(DECODE_MAX_DEPTH)).map_err(|e| match e {
        pyjson::Error::Value(m) => LoadsError::Value(m),
        pyjson::Error::Recursion(is_obj) => LoadsError::Recursion(format!(
            "maximum recursion depth exceeded while decoding a JSON {} from a unicode string",
            if is_obj { "object" } else { "array" }
        )),
    })
}

impl crate::util::pyjson::Build for PyValue {
    type Key = PyStr;
    fn null() -> Self {
        PyValue::None
    }
    fn bool(b: bool) -> Self {
        PyValue::Bool(b)
    }
    fn num(n: crate::util::pyjson::Num<'_>) -> Self {
        use crate::util::pyjson::Num;
        match n {
            Num::Int(i) => PyValue::Int(BigInt::from(i)),
            Num::UInt(u) => PyValue::Int(BigInt::from(u)),
            Num::Big(digits) => PyValue::Int(BigInt::parse_bytes(digits.as_bytes(), 10).unwrap_or_default()),
            Num::Float(f) => PyValue::Float(f),
        }
    }
    fn str(s: String) -> Self {
        PyValue::Str(PyStr::new(&s))
    }
    fn wstr(units: Vec<u32>) -> Self {
        PyValue::Str(PyStr(units))
    }
    fn key(s: String) -> PyStr {
        PyStr::new(&s)
    }
    fn wkey(units: Vec<u32>) -> PyStr {
        PyStr(units)
    }
    fn list(items: Vec<Self>) -> Self {
        PyValue::List(items)
    }
    fn dict(pairs: Vec<(PyStr, Self)>) -> Self {
        // dict semantics: a repeated key keeps its first place and takes
        // the last value.
        PyValue::Dict(crate::util::pyjson::dict_pairs(pairs))
    }
}

// --- The settings ------------------------------------------------------------

/// The auto-update.json policy file: the Python's `cfg` dict. The Python does
/// not type-check the file (a `"packages": "yes"` stays a string, unknown
/// keys are kept and written back in file order, an `[[1, "a"]]` file gives
/// the int key 1), so neither does this; the UI reads switches with
/// [`Config::get_bool`], i.e. `bool(cfg.get(key))`.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    items: Vec<(PyKey, Arc<PyValue>)>,
    index: HashMap<KeyClass, usize>,
    /// The str keys as serde values, for [`Config::get`]: numbers that do
    /// not fit become the nearest f64, NaN/infinities and too-deep nesting
    /// keep only their truthiness, surrogates become U+FFFD.
    view: Map<String, Value>,
}

impl Default for Config {
    fn default() -> Self {
        let mut c = Config::empty();
        c.set("schedule", "weekly");
        // What the unattended run is allowed to touch. Packages default on
        // because that is what the timer was created for; plugins default off
        // because a shell plugin update can change the desktop under you.
        c.set("packages", true);
        c.set("agents", true);
        c.set("appimages", true);
        c.set("plugins", false);
        c.set("notify", true);
        c
    }
}

/// Deeper than this, [`Config::get`]'s view keeps only truthiness.
const VIEW_MAX_DEPTH: usize = 100;
/// The parity runner's `normalise` is plain Python recursion: past
/// `sys.getrecursionlimit()` it raises.
const NORMALISE_MAX_DEPTH: usize = 990;

/// A value as serde JSON, `str(k)` for keys (the parity runner's
/// `normalise`). Err past `limit` levels.
fn to_value(v: &PyValue, depth: usize, limit: usize) -> Result<Value, ()> {
    Ok(match v {
        PyValue::None => Value::Null,
        PyValue::Bool(b) => Value::Bool(*b),
        PyValue::Int(n) => int_value(n),
        PyValue::Float(f) => match serde_json::Number::from_f64(*f) {
            Some(x) => Value::Number(x),
            None => Value::String(json_float(*f)),
        },
        PyValue::Str(s) => Value::String(s.to_string_lossy()),
        PyValue::List(a) => {
            if depth >= limit {
                return Err(());
            }
            Value::Array(a.iter().map(|x| to_value(x, depth + 1, limit)).collect::<Result<_, _>>()?)
        }
        PyValue::Dict(d) => {
            if depth >= limit {
                return Err(());
            }
            let mut m = Map::new();
            for (k, x) in d {
                m.insert(k.to_string_lossy(), to_value(x, depth + 1, limit)?);
            }
            Value::Object(m)
        }
    })
}

fn int_value(n: &BigInt) -> Value {
    if let Ok(i) = i64::try_from(n) {
        return Value::from(i);
    }
    if let Ok(u) = u64::try_from(n) {
        return Value::from(u);
    }
    let f: f64 = n.to_string().parse().unwrap_or(0.0);
    Value::from(if f.is_finite() { f } else { f64::MAX.copysign(f) })
}

/// A serde value (from a caller of [`Config::set`] or a parity arg) as Python.
fn from_value(v: &Value) -> PyValue {
    match v {
        Value::Null => PyValue::None,
        Value::Bool(b) => PyValue::Bool(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                PyValue::Int(BigInt::from(i))
            } else if let Some(u) = n.as_u64() {
                PyValue::Int(BigInt::from(u))
            } else {
                PyValue::Float(n.as_f64().unwrap_or(0.0))
            }
        }
        Value::String(s) => PyValue::Str(PyStr::new(s)),
        Value::Array(a) => PyValue::List(a.iter().map(from_value).collect()),
        Value::Object(m) => PyValue::Dict(m.iter().map(|(k, x)| (PyStr::new(k), from_value(x))).collect()),
    }
}

fn key_str(k: &PyKey) -> String {
    match k {
        PyKey::None => "None".into(),
        PyKey::Bool(true) => "True".into(),
        PyKey::Bool(false) => "False".into(),
        PyKey::Int(n) => n.to_string(),
        PyKey::Float(f) => float_str(*f),
        PyKey::Str(s) => s.to_string_lossy(),
    }
}

impl Config {
    fn empty() -> Self {
        Config { items: Vec::new(), index: HashMap::new(), view: Map::new() }
    }

    /// `cfg[key] = value`, with dict semantics: an equal key keeps its
    /// place and its first spelling.
    fn insert(&mut self, key: PyKey, value: PyValue) {
        if let PyKey::Str(s) = &key {
            if let Some(name) = s.as_string() {
                let v = to_value(&value, 0, VIEW_MAX_DEPTH).unwrap_or(Value::Bool(truthy(&value)));
                self.view.insert(name, v);
            }
        }
        match self.index.get(&key_class(&key)) {
            Some(&at) => self.items[at].1 = Arc::new(value),
            None => {
                self.index.insert(key_class(&key), self.items.len());
                self.items.push((key, Arc::new(value)));
            }
        }
    }

    fn lookup(&self, key: &str) -> Option<&PyValue> {
        let at = *self.index.get(&KeyClass::Str(PyStr::new(key).0))?;
        Some(&self.items[at].1)
    }

    /// `cfg.get(key)`.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.view.get(key)
    }

    /// `bool(cfg.get(key))`, with Python truthiness.
    pub fn get_bool(&self, key: &str) -> bool {
        self.lookup(key).map(truthy).unwrap_or(false)
    }

    /// `cfg["schedule"]` when it is a string.
    pub fn schedule(&self) -> Option<&str> {
        self.get("schedule").and_then(Value::as_str)
    }

    /// `cfg[key] = value`.
    pub fn set(&mut self, key: &str, value: impl Into<Value>) {
        self.insert(PyKey::Str(PyStr::new(key)), from_value(&value.into()));
    }

    /// The parity runner's `normalise(cfg)`: `str(k)` keys. Recursive, to
    /// Python's recursion limit: on a thread with room for it.
    fn normalised(&self) -> Result<Value, String> {
        crate::util::with_stack(crate::util::DEEP_STACK, || self.normalised_here())
    }

    fn normalised_here(&self) -> Result<Value, String> {
        let mut m = Map::new();
        for (k, v) in &self.items {
            let v = to_value(v, 1, NORMALISE_MAX_DEPTH)
                .map_err(|()| "RecursionError: maximum recursion depth exceeded".to_owned())?;
            m.insert(key_str(k), v);
        }
        Ok(Value::Object(m))
    }
}

impl Serialize for Config {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.normalised().map_err(serde::ser::Error::custom)?.serialize(serializer)
    }
}

/// The Python's `DEFAULTS` dict.
pub static DEFAULTS: LazyLock<Config> = LazyLock::new(Config::default);

/// `next` / `last` as systemd reports them, and whether the timer is on.
#[derive(Debug, Clone, Default, PartialEq, Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Status {
    pub enabled: bool,
    pub next: String,
    pub last: String,
}

/// `json.dumps(cfg, indent=2)`. Iterative, like the depth it must reach;
/// Err (RecursionError) past Python's own limit.
fn dumps(cfg: &Config) -> Result<String, String> {
    const RECURSION: &str = "RecursionError: maximum recursion depth exceeded while encoding a JSON object";
    enum Frame<'a> {
        List(std::slice::Iter<'a, PyValue>),
        Dict(std::slice::Iter<'a, (PyStr, PyValue)>),
    }
    fn indent(out: &mut String, level: usize) {
        out.push('\n');
        out.extend(std::iter::repeat_n(' ', 2 * level));
    }
    // Writes a scalar, or opens a container and returns its frame.
    fn open<'a>(v: &'a PyValue, out: &mut String) -> Option<Frame<'a>> {
        match v {
            PyValue::None => out.push_str("null"),
            PyValue::Bool(true) => out.push_str("true"),
            PyValue::Bool(false) => out.push_str("false"),
            PyValue::Int(n) => out.push_str(&n.to_string()),
            PyValue::Float(f) => out.push_str(&json_float(*f)),
            PyValue::Str(s) => json_str(s, out),
            PyValue::List(a) if a.is_empty() => out.push_str("[]"),
            PyValue::Dict(d) if d.is_empty() => out.push_str("{}"),
            PyValue::List(a) => {
                out.push('[');
                return Some(Frame::List(a.iter()));
            }
            PyValue::Dict(d) => {
                out.push('{');
                return Some(Frame::Dict(d.iter()));
            }
        }
        None
    }
    let mut out = String::new();
    if cfg.items.is_empty() {
        return Ok("{}".into());
    }
    // Depth counts the top-level dict, and empty containers too. Measured
    // before anything is written: indenting a value nested that deep, only
    // to throw it away, would write gigabytes of spaces.
    let deepest = cfg.items.iter().map(|(_, v)| crate::util::tree::depth(&**v, PyValue::tree_shape)).max();
    if deepest.unwrap_or(0) + 1 > ENCODE_MAX_DEPTH {
        return Err(RECURSION.into());
    }
    out.push('{');
    let mut stack: Vec<(Frame, bool)> = Vec::new();
    for (i, (k, v)) in cfg.items.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        indent(&mut out, 1);
        // json's key coercion: str, float, True/False/None, int.
        match k {
            PyKey::Str(s) => json_str(s, &mut out),
            PyKey::Float(f) => json_str(&PyStr::new(&json_float(*f)), &mut out),
            PyKey::Bool(b) => out.push_str(if *b { "\"true\"" } else { "\"false\"" }),
            PyKey::None => out.push_str("\"null\""),
            PyKey::Int(n) => out.push_str(&format!("\"{n}\"")),
        }
        out.push_str(": ");
        if let Some(f) = open(v, &mut out) {
            stack.push((f, true));
        }
        loop {
            let level = stack.len() + 1;
            let Some((frame, first)) = stack.last_mut() else { break };
            let was_first = std::mem::replace(first, false);
            let next = match frame {
                Frame::List(it) => it.next().map(|x| (None, x)),
                Frame::Dict(it) => it.next().map(|(k, x)| (Some(k), x)),
            };
            match next {
                None => {
                    let close = if matches!(frame, Frame::List(_)) { ']' } else { '}' };
                    stack.pop();
                    indent(&mut out, level - 1);
                    out.push(close);
                }
                Some((key, x)) => {
                    if !was_first {
                        out.push(',');
                    }
                    indent(&mut out, level);
                    if let Some(k) = key {
                        json_str(k, &mut out);
                        out.push_str(": ");
                    }
                    if let Some(f) = open(x, &mut out) {
                        stack.push((f, true));
                    }
                }
            }
        }
    }
    indent(&mut out, 0);
    out.push('}');
    Ok(out)
}

// --- Python's str helpers ------------------------------------------------------

/// Python's `str.splitlines()`: every line boundary Python knows, not only `\n`.
fn py_splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        let is_break = matches!(
            c,
            '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}'
        );
        if is_break {
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

/// Python's `str.isspace()` per character (Unicode White_Space plus the
/// \x1c-\x1f separators Python also counts).
fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// Python's `str.strip()`.
fn py_strip(s: &str) -> &str {
    s.trim_matches(py_isspace)
}

/// Python's `repr(str)`, for messages that embed one.
fn py_repr(s: &str) -> String {
    py_repr_cps(&s.chars().map(|c| c as u32).collect::<Vec<u32>>())
}

/// `repr()` of a str given as code points, lone surrogates included (a
/// surrogate is unprintable: `\udcff`).
fn py_repr_cps(cps: &[u32]) -> String {
    let has = |c: char| cps.contains(&(c as u32));
    let quote = if has('\'') && !has('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(quote);
    for &n in cps {
        match char::from_u32(n) {
            Some('\\') => out.push_str("\\\\"),
            Some('\n') => out.push_str("\\n"),
            Some('\r') => out.push_str("\\r"),
            Some('\t') => out.push_str("\\t"),
            Some(c) if c == quote => {
                out.push('\\');
                out.push(c);
            }
            Some(c) if py_printable(c) => out.push(c),
            _ if n < 0x100 => out.push_str(&format!("\\x{n:02x}")),
            _ if n < 0x10000 => out.push_str(&format!("\\u{n:04x}")),
            _ => out.push_str(&format!("\\U{n:08x}")),
        }
    }
    out.push(quote);
    out
}

/// `os.fsdecode(bytes)`: UTF-8 with surrogateescape, as code points (a byte
/// that is not part of valid UTF-8 becomes U+DC80 + byte).
fn fsdecode(b: &[u8]) -> Vec<u32> {
    let mut out = Vec::new();
    for chunk in b.utf8_chunks() {
        out.extend(chunk.valid().chars().map(|c| c as u32));
        out.extend(chunk.invalid().iter().map(|&x| 0xdc00 + x as u32));
    }
    out
}

/// `repr(str(path))`: the path's bytes as Python holds them.
fn py_repr_path(p: &Path) -> String {
    py_repr_cps(&fsdecode(p.as_os_str().as_bytes()))
}

#[cfg(test)]
/// For parity: `os.fsencode(s).decode("utf-8", "backslashreplace")`, a str
/// from the OS that JSON can carry (bytes that are not UTF-8 become `\xff`).
fn backslashreplace(b: &[u8]) -> String {
    let mut out = String::new();
    for chunk in b.utf8_chunks() {
        out.push_str(chunk.valid());
        for x in chunk.invalid() {
            out.push_str(&format!("\\x{x:02x}"));
        }
    }
    out
}

/// `str.isprintable()` for one character: false in the Unicode categories
/// Cc, Cf, Cs, Co, Cn, Zl, Zp and Zs other than the space.
fn py_printable(c: char) -> bool {
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

/// `str(OSError)` as Python formats it: `[Errno 2] No such file or directory: '/x'`.
fn py_oserror(e: &std::io::Error, path: &Path) -> String {
    py_oserror2(e, path, None)
}

/// The same with a second filename (`os.replace`): `...: 'src' -> 'dst'`.
fn py_oserror2(e: &std::io::Error, path: &Path, path2: Option<&Path>) -> String {
    let text = e.to_string();
    match e.raw_os_error() {
        Some(n) => {
            let strerror = text.strip_suffix(&format!(" (os error {n})")).unwrap_or(&text).to_owned();
            let mut msg = format!("[Errno {n}] {strerror}: {}", py_repr_path(path));
            if let Some(p2) = path2 {
                msg.push_str(&format!(" -> {}", py_repr_path(p2)));
            }
            msg
        }
        None => text,
    }
}

/// `bytes.decode("utf-8")`: the UnicodeDecodeError message on failure.
fn py_decode(raw: Vec<u8>) -> Result<String, String> {
    String::from_utf8(raw).map_err(|e| {
        let bytes = e.as_bytes();
        let err = e.utf8_error();
        let start = err.valid_up_to();
        let b = bytes.get(start).copied().unwrap_or(0);
        let (end, reason) = match err.error_len() {
            None => (bytes.len(), "unexpected end of data"),
            Some(1) if !(0xc2..=0xf4).contains(&b) => (start + 1, "invalid start byte"),
            Some(k) => (start + k, "invalid continuation byte"),
        };
        if end - start == 1 {
            format!("'utf-8' codec can't decode byte 0x{b:02x} in position {start}: {reason}")
        } else {
            format!("'utf-8' codec can't decode bytes in position {start}-{}: {reason}", end - 1)
        }
    })
}

/// The codec Python decodes text with when no encoding is given
/// (`Path.read_text()`, `subprocess.run(text=True)`): UTF-8 in UTF-8 mode,
/// else the locale's, `locale.getencoding()`.
#[derive(Debug, Clone, PartialEq)]
enum Codec {
    Utf8,
    Ascii,
    Latin1,
    /// Any other codeset, by its `nl_langinfo(CODESET)` name.
    Other(&'static encoding_rs::Encoding, String),
}

/// The interpreter's choice, made once at startup from this environment:
///
/// * `PYTHONUTF8=1` is UTF-8 mode: always UTF-8.
/// * Unset, UTF-8 mode switches itself on when the LC_CTYPE locale is "C" or
///   "POSIX" -- including a locale that is named but not installed, since
///   `setlocale` then fails and leaves "C" (PEP 540).
/// * `PYTHONUTF8=0` keeps the locale's own codeset, except that a "C" locale
///   is coerced to C.UTF-8 unless LC_ALL is set or `PYTHONCOERCECLOCALE=0`
///   (PEP 538); an uncoerced "C" is ASCII.
///
/// Python asks `nl_langinfo(CODESET)`; that needs `unsafe` here, so the same
/// answer comes from `locale charmap` under the LC_CTYPE locale (the rule
/// `patches` and `tasks` use).
static LOCALE_CODEC: LazyLock<Codec> = LazyLock::new(|| {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let utf8_mode = var("PYTHONUTF8");
    if utf8_mode.as_deref() == Some("1") {
        return Codec::Utf8;
    }
    let name = var("LC_ALL").or_else(|| var("LC_CTYPE")).or_else(|| var("LANG")).unwrap_or_else(|| "C".to_owned());
    let (ok, charmap) = if name == "C" || name == "POSIX" {
        (false, String::new())
    } else {
        let out = crate::util::run_with(
            &["locale", "charmap"],
            &crate::util::RunOpts {
                timeout: Some(Duration::from_secs(5)),
                env: vec![("LC_ALL".into(), name.clone().into())],
                ..Default::default()
            },
        );
        (out.ok() && out.stderr.trim().is_empty(), out.stdout.trim().to_owned())
    };
    let is_c = !ok || charmap.is_empty() || charmap == "ANSI_X3.4-1968";
    codec_for(utf8_mode.as_deref(), (!is_c).then_some(charmap.as_str()), var("LC_ALL").is_some(), var("PYTHONCOERCECLOCALE").as_deref())
});

/// The decision above, given the facts: `charmap` is None for a "C" locale.
fn codec_for(utf8_mode: Option<&str>, charmap: Option<&str>, lc_all: bool, coerce: Option<&str>) -> Codec {
    if utf8_mode == Some("1") {
        return Codec::Utf8;
    }
    let Some(charmap) = charmap else {
        if utf8_mode != Some("0") || (!lc_all && coerce != Some("0")) {
            return Codec::Utf8;
        }
        return Codec::Ascii;
    };
    let norm = charmap.to_ascii_lowercase().replace(['-', '_'], "");
    match norm.as_str() {
        "utf8" => Codec::Utf8,
        "ansix3.41968" | "ascii" | "usascii" => Codec::Ascii,
        "iso88591" | "latin1" => Codec::Latin1,
        _ => match encoding_rs::Encoding::for_label(charmap.as_bytes()) {
            Some(e) if e == encoding_rs::UTF_8 => Codec::Utf8,
            Some(e) => Codec::Other(e, charmap.to_owned()),
            None => Codec::Utf8,
        },
    }
}

/// `bytes.decode(<locale encoding>)`, strict: the UnicodeDecodeError message
/// on failure.
fn locale_decode(raw: Vec<u8>) -> Result<String, String> {
    decode_as(&LOCALE_CODEC, raw)
}

fn decode_as(codec: &Codec, raw: Vec<u8>) -> Result<String, String> {
    match codec {
        Codec::Utf8 => py_decode(raw),
        Codec::Latin1 => Ok(raw.iter().map(|&c| c as char).collect()),
        Codec::Ascii => match raw.iter().position(|&c| c >= 0x80) {
            Some(pos) => Err(format!(
                "'ascii' codec can't decode byte 0x{:02x} in position {pos}: ordinal not in range(128)",
                raw[pos]
            )),
            None => Ok(raw.into_iter().map(char::from).collect()),
        },
        Codec::Other(enc, name) => {
            let mut dec = enc.new_decoder_without_bom_handling();
            let cap = dec.max_utf8_buffer_length_without_replacement(raw.len()).unwrap_or(raw.len() * 4 + 16);
            let mut out = String::with_capacity(cap);
            let (res, read) = dec.decode_to_string_without_replacement(&raw, &mut out, true);
            match res {
                encoding_rs::DecoderResult::Malformed(bad, extra) => {
                    let pos = read.saturating_sub(bad as usize + extra as usize);
                    let b = raw.get(pos).copied().unwrap_or(0);
                    Err(if enc.is_single_byte() {
                        format!("'charmap' codec can't decode byte 0x{b:02x} in position {pos}: character maps to <undefined>")
                    } else {
                        let codec = name.to_ascii_lowercase().replace('-', "_");
                        format!("'{codec}' codec can't decode byte 0x{b:02x} in position {pos}: illegal multibyte sequence")
                    })
                }
                _ => Ok(out),
            }
        }
    }
}

/// `Path.read_text()`: Err(Ok(OSError message)) or Err(Err(decode error)),
/// the decode error being a ValueError the Python callers do not catch.
fn read_text(path: &Path) -> Result<String, Result<String, String>> {
    let raw = std::fs::read(path).map_err(|e| Ok(py_oserror(&e, path)))?;
    locale_decode(raw).map_err(Err)
}

fn py_type_name(v: &PyValue) -> &'static str {
    match v {
        PyValue::None => "NoneType",
        PyValue::Bool(_) => "bool",
        PyValue::Int(_) => "int",
        PyValue::Float(_) => "float",
        PyValue::Str(_) => "str",
        PyValue::List(_) => "list",
        PyValue::Dict(_) => "dict",
    }
}

fn one_char_strs(s: &PyStr) -> Vec<PyValue> {
    s.0.iter().map(|&c| PyValue::Str(PyStr(vec![c]))).collect()
}

/// `cfg.update(data)` for whatever `json.loads` gave. Ok(()) when it worked,
/// Err(Ok(..)) for a ValueError (the Python catches it, keeping whatever was
/// already merged), Err(Err(..)) for a TypeError (which escapes `load`).
fn dict_update(cfg: &mut Config, mut data: PyValue) -> Result<(), Result<String, String>> {
    let items: Vec<PyValue> = match &mut data {
        PyValue::Dict(d) => {
            for (k, v) in d.drain(..) {
                cfg.insert(PyKey::Str(k), v);
            }
            return Ok(());
        }
        PyValue::List(a) => std::mem::take(a),
        // A str is a sequence of one-character strings.
        PyValue::Str(s) => one_char_strs(s),
        other => return Err(Err(format!("'{}' object is not iterable", py_type_name(other)))),
    };
    for (i, mut item) in items.into_iter().enumerate() {
        // PySequence_Fast: any iterable; a dict iterates its keys.
        let seq: Vec<PyValue> = match &mut item {
            PyValue::List(a) => std::mem::take(a),
            PyValue::Str(s) => one_char_strs(s),
            PyValue::Dict(d) => d.drain(..).map(|(k, _)| PyValue::Str(k)).collect(),
            _ => {
                return Err(Err(format!("cannot convert dictionary update sequence element #{i} to a sequence")))
            }
        };
        if seq.len() != 2 {
            return Err(Ok(format!(
                "dictionary update sequence element #{i} has length {}; 2 is required",
                seq.len()
            )));
        }
        let mut it = seq.into_iter();
        let (mut k, v) = (it.next().unwrap_or(PyValue::None), it.next().unwrap_or(PyValue::None));
        let key = match &mut k {
            PyValue::None => PyKey::None,
            PyValue::Bool(b) => PyKey::Bool(*b),
            PyValue::Int(n) => PyKey::Int(std::mem::take(n)),
            PyValue::Float(f) => PyKey::Float(*f),
            PyValue::Str(s) => PyKey::Str(std::mem::take(s)),
            other => return Err(Err(format!("unhashable type: '{}'", py_type_name(other)))),
        };
        cfg.insert(key, v);
    }
    Ok(())
}

/// `Err` where the Python raises (a file holding a JSON number, `null`, or a
/// list whose items are not pairs: `dict.update` raises TypeError there,
/// which the Python does not catch; and a file nested past json's
/// RecursionError).
pub fn load() -> Result<Config, String> {
    imported()?;
    load_from(&CONFIG)
}

fn load_from(config: &Path) -> Result<Config, String> {
    let mut cfg = DEFAULTS.clone();
    let text = match read_text(config) {
        Ok(t) => t,
        Err(_) => return Ok(cfg), // OSError, ValueError (UnicodeDecodeError)
    };
    let data = match json_loads(&text) {
        Ok(v) => v,
        Err(LoadsError::Value(_)) => return Ok(cfg),
        Err(LoadsError::Recursion(e)) => return Err(e),
    };
    match dict_update(&mut cfg, data) {
        Ok(()) | Err(Ok(_)) => Ok(cfg),
        Err(Err(e)) => Err(e),
    }
}

/// `Err` where the Python raises: OSError, or RecursionError from json.
pub fn save(cfg: &Config) -> Result<(), String> {
    imported()?;
    save_to(&CONFIG_DIR, &CONFIG, cfg)
}

/// `Path.parent`, lexically.
fn py_parent(p: &Path) -> PathBuf {
    match p.parent() {
        Some(q) if q.as_os_str().is_empty() => PathBuf::from("."),
        Some(q) => q.to_path_buf(),
        None => p.to_path_buf(),
    }
}

/// `Path.mkdir(parents=True, exist_ok=True)`.
fn py_mkdir(p: &Path, parents: bool) -> Result<(), String> {
    match std::fs::create_dir(p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let parent = py_parent(p);
            if !parents || parent == p {
                return Err(py_oserror(&e, p));
            }
            py_mkdir(&parent, true)?;
            py_mkdir(p, false)
        }
        Err(_) if p.is_dir() => Ok(()),
        Err(e) => Err(py_oserror(&e, p)),
    }
}

fn save_to(dir: &Path, config: &Path, cfg: &Config) -> Result<(), String> {
    py_mkdir(dir, true)?;
    let text = dumps(cfg)? + "\n";
    // Flushed before it replaces the old: an empty config after a crash
    // would read as package updates switched back on.
    crate::util::atomic_write(config, text.as_bytes()).map_err(|e| py_oserror(&e, config))
}

// --- systemctl --------------------------------------------------------------------

enum Ran {
    Exited { code: i32, stdout: Vec<u8>, stderr: Vec<u8> },
    TimedOut,
}

/// `subprocess.run(argv, capture_output=True, timeout=t)` in bytes, through
/// `util::run_bytes`: the timeout covers both pipes reaching EOF (a forked
/// child still holding stdout keeps it waiting, as `communicate` does) and
/// the exit; on expiry the child's process group is killed and the child
/// reaped. stdin is inherited.
///
/// The program is found the way `_posixsubprocess` does rather than
/// `execvp`: every `$PATH` entry (`os.defpath` when unset; an empty entry is
/// the current directory) is tried in turn with a plain execve, and an entry
/// that fails for any reason other than ENOENT / ENOTDIR (ENOEXEC from a
/// file with no `#!`, EACCES) does not end the search. If none starts, the
/// first such error is the one reported, else the last.
fn run_capture(argv: &[&str], timeout: Duration) -> std::io::Result<Ran> {
    run_capture_in(argv, timeout, std::env::var_os("PATH").map(OsString::into_vec))
}

fn run_capture_in(argv: &[&str], timeout: Duration, path: Option<Vec<u8>>) -> std::io::Result<Ran> {
    let prog = argv[0];
    let candidates: Vec<PathBuf> = if prog.contains('/') {
        vec![PathBuf::from(prog)]
    } else {
        let path = path.unwrap_or_else(|| b"/bin:/usr/bin".to_vec());
        path.split(|&c| c == b':')
            .map(|dir| {
                // os.path.join("", prog) is the bare name, run relative to the
                // current directory; "./" keeps Command from searching PATH.
                let dir: &[u8] = if dir.is_empty() { b"." } else { dir };
                to_path(py_join(dir, prog.as_bytes()))
            })
            .collect()
    };
    let opts = crate::util::RunOpts {
        timeout: Some(timeout),
        stdin: crate::util::Stdin::Inherit,
        arg0: Some(std::ffi::OsStr::new(prog)),
        ..Default::default()
    };
    let mut first: Option<std::io::Error> = None;
    let mut last: Option<std::io::Error> = None;
    for exe in candidates {
        let mut full: Vec<&std::ffi::OsStr> = vec![exe.as_os_str()];
        full.extend(argv[1..].iter().map(std::ffi::OsStr::new));
        let out = crate::util::run_bytes(&full, &opts);
        if !out.failed_to_start {
            if out.timed_out {
                return Ok(Ran::TimedOut);
            }
            let code = out.code.or(out.signal.map(|s| -s)).unwrap_or(-1);
            return Ok(Ran::Exited { code, stdout: out.stdout, stderr: out.stderr });
        }
        let e = match out.start_errno {
            Some(n) => std::io::Error::from_raw_os_error(n),
            None => std::io::Error::other(out.start_error),
        };
        let soft = matches!(e.raw_os_error(), Some(libc::ENOENT) | Some(libc::ENOTDIR));
        if !soft && first.is_none() {
            first = Some(e);
        } else {
            last = Some(e);
        }
    }
    Err(first.or(last).unwrap_or_else(|| std::io::Error::from_raw_os_error(libc::ENOENT)))
}

/// `text=True`: decode, then universal newlines (`\r\n` and `\r` become `\n`).
fn text_mode(raw: Vec<u8>) -> Result<String, String> {
    Ok(locale_decode(raw)?.replace("\r\n", "\n").replace('\r', "\n"))
}

/// `systemctl --user <args>` -> (returncode, stdout+stderr stripped).
/// Err where the Python raises: output the locale's codec cannot decode
/// (UnicodeDecodeError), or an argument holding a NUL -- both ValueErrors,
/// not among the `except`ed OSError / SubprocessError.
fn systemctl(args: &[&str], timeout: u64) -> Result<(i32, String), String> {
    let mut argv = vec!["systemctl", "--user"];
    argv.extend_from_slice(args);
    // subprocess raises ValueError for an argument with a NUL in it, before
    // anything is started; not one of the exceptions caught here.
    if argv.iter().any(|a| a.contains('\0')) {
        return Err("embedded null byte".to_owned());
    }
    match run_capture(&argv, Duration::from_secs(timeout)) {
        Err(e) => Ok((1, py_oserror(&e, Path::new("systemctl")))),
        Ok(Ran::TimedOut) => {
            let list: Vec<String> = argv.iter().map(|a| py_repr(a)).collect();
            Ok((1, format!("Command '[{}]' timed out after {timeout} seconds", list.join(", "))))
        }
        Ok(Ran::Exited { code, stdout, stderr }) => {
            let out = text_mode(stdout)? + &text_mode(stderr)?;
            Ok((code, py_strip(&out).to_owned()))
        }
    }
}

/// The systemctl the functions below talk to: the real one, or a stand-in
/// for tests and parity (`(args, timeout) -> (code, output)`).
type Sys<'a> = &'a mut dyn FnMut(&[&str], u64) -> Result<(i32, String), String>;

fn real_sys() -> impl FnMut(&[&str], u64) -> Result<(i32, String), String> {
    systemctl
}

/// For the infallible wrappers: what the Python would show as a traceback.
fn report<T>(r: Result<T, String>, fallback: T) -> T {
    r.unwrap_or_else(|e| {
        eprintln!("almanac: {e}");
        fallback
    })
}

/// `is_enabled()`; Err where the Python raises.
pub fn try_is_enabled() -> Result<bool, String> {
    imported()?;
    is_enabled_with(&mut real_sys())
}

/// [`try_is_enabled`], logging the error and answering false where the
/// Python would raise.
#[deprecated(note = "the Python raises out of is_enabled() (UnicodeDecodeError from systemctl output); use try_is_enabled and stop on Err")]
pub fn is_enabled() -> bool {
    report(try_is_enabled(), false)
}

fn is_enabled_with(sys: Sys) -> Result<bool, String> {
    let (code, out) = sys(&["is-enabled", UNIT], 30)?;
    Ok(code == 0 && out.starts_with("enabled"))
}

/// `set_enabled(on)`; Err where the Python raises.
pub fn try_set_enabled(on: bool) -> Result<(bool, String), String> {
    imported()?;
    set_enabled_with(on, &mut real_sys())
}

/// [`try_set_enabled`], reporting the error as the failure message.
#[deprecated(note = "the Python raises out of set_enabled(): the switchboard switch reports str(exc), the Preferences close handler stops with a traceback; use try_set_enabled and return / stop on its Err")]
pub fn set_enabled(on: bool) -> (bool, String) {
    try_set_enabled(on).unwrap_or_else(|e| (false, e))
}

fn set_enabled_with(on: bool, sys: Sys) -> Result<(bool, String), String> {
    let args: &[&str] = if on { &["enable", "--now", UNIT] } else { &["disable", "--now", UNIT] };
    let (code, out) = sys(args, 30)?;
    Ok((code == 0, out))
}

/// Next and last run, read from systemd rather than remembered here.
/// Err where the Python raises.
pub fn try_status() -> Result<Status, String> {
    imported()?;
    status_with(&mut real_sys())
}

/// [`try_status`], logging the error and answering "not scheduled" where
/// the Python would raise.
#[deprecated(note = "the Python raises out of status(): the switch reports str(exc), building the switchboard rows or the Preferences dialog stops; use try_status and propagate its Err")]
pub fn status() -> Status {
    report(try_status(), Status { enabled: false, next: "not scheduled".into(), last: "never".into() })
}

fn status_with(sys: Sys) -> Result<Status, String> {
    let (_code, out) = sys(
        &["show", UNIT, "--property=NextElapseUSecRealtime", "--property=LastTriggerUSec", "--property=Unit"],
        30,
    )?;
    let mut info: BTreeMap<&str, &str> = BTreeMap::new();
    for line in py_splitlines(&out) {
        if let Some((k, v)) = line.split_once('=') {
            info.insert(k, v);
        }
    }
    let or = |v: Option<&&str>, fallback: &str| match v {
        Some(s) if !s.is_empty() => (*s).to_owned(),
        _ => fallback.to_owned(),
    };
    Ok(Status {
        enabled: is_enabled_with(sys)?,
        next: or(info.get("NextElapseUSecRealtime"), "not scheduled"),
        last: or(info.get("LastTriggerUSec"), "never"),
    })
}

/// Rewrite OnCalendar in the timer unit and reload.
///
/// The whole unit is not regenerated: it carries Persistent= and a randomised
/// delay that were chosen for a laptop, and rewriting the file would quietly
/// drop them.
///
/// `Err` where the Python raises: a timer unit that is not UTF-8, or
/// systemctl output that is not.
pub fn set_schedule(key: &str) -> Result<(bool, String), String> {
    imported()?;
    set_schedule_in(key, &TimerFiles::real(), &mut real_sys())
}

/// [`set_schedule_with`] on the user's copy of the unit, or, for the
/// packaged unit, a drop-in: /usr/lib is the package's, and a package update
/// would put an edit there back.
fn set_schedule_in(key: &str, files: &TimerFiles, sys: Sys) -> Result<(bool, String), String> {
    if !files.uses_dropin() {
        return set_schedule_with(key, files.user, sys);
    }
    let calendar = match SCHEDULES.iter().find(|(k, _l, _c)| *k == key) {
        Some((_k, _l, cal)) => *cal,
        None => return Ok((false, format!("unknown schedule {}", py_repr(key)))),
    };
    if let Some(dir) = files.dropin.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            return Ok((false, py_oserror(&e, dir)));
        }
    }
    // OnCalendar= adds to a timer's schedule; the empty one first clears
    // what the packaged unit set.
    let text = format!(
        "# Written by Almanac (Preferences, Updates).\n[Timer]\nOnCalendar=\nOnCalendar={calendar}\n"
    );
    if let Err(e) = write_unit(files.dropin, &text) {
        return Ok((false, e));
    }
    sys(&["daemon-reload"], 30)?;
    if is_enabled_with(sys)? {
        sys(&["restart", UNIT], 30)?;
    }
    Ok((true, calendar.to_owned()))
}

fn set_schedule_with(key: &str, timer_file: &Path, sys: Sys) -> Result<(bool, String), String> {
    let calendar = match SCHEDULES.iter().find(|(k, _l, _c)| *k == key) {
        Some((_k, _l, cal)) => *cal,
        None => return Ok((false, format!("unknown schedule {}", py_repr(key)))),
    };
    let text = match read_text(timer_file) {
        Ok(t) => t,
        Err(Ok(e)) => return Ok((false, e)),
        Err(Err(e)) => return Err(e),
    };

    let mut out: Vec<String> = Vec::new();
    let mut replaced = false;
    for line in py_splitlines(&text) {
        if line.starts_with("OnCalendar=") && !replaced {
            out.push(format!("OnCalendar={calendar}"));
            replaced = true;
        } else {
            out.push(line.to_owned());
        }
    }
    if !replaced {
        return Ok((false, "no OnCalendar= line in the timer unit".to_owned()));
    }

    if let Err(e) = write_unit(timer_file, &(out.join("\n") + "\n")) {
        return Ok((false, e));
    }

    sys(&["daemon-reload"], 30)?;
    if is_enabled_with(sys)? {
        sys(&["restart", UNIT], 30)?;
    }
    Ok((true, calendar.to_owned()))
}

/// Replace the unit file at `path` with `text` atomically, as `save_to`
/// does the config. Writing in place left a truncated unit, which systemd
/// then refuses, if the write was cut short (a full disk, a crash). Mode
/// 0644, what systemd units are installed with; a unit symlinked from a
/// dotfiles checkout stays a link.
fn write_unit(path: &Path, text: &str) -> Result<(), String> {
    crate::util::atomic_write_mode(path, text.as_bytes(), Some(0o644)).map_err(|e| py_oserror(&e, path))
}

/// `Err` where the Python raises: a timer unit that is not UTF-8.
pub fn current_schedule() -> Result<String, String> {
    imported()?;
    current_schedule_of(&TimerFiles::real())
}

/// The schedule in effect: the drop-in's, over the user's copy of the unit,
/// over the packaged unit.
fn current_schedule_of(files: &TimerFiles) -> Result<String, String> {
    match read_text(files.dropin) {
        Ok(text) => {
            // systemd's reading: an empty OnCalendar= clears those before it,
            // and what is left is the schedule.
            let mut cals: Vec<&str> = Vec::new();
            for line in py_splitlines(&text) {
                if let Some(v) = line.strip_prefix("OnCalendar=") {
                    match py_strip(v) {
                        "" => cals.clear(),
                        cal => cals.push(cal),
                    }
                }
            }
            if let Some(cal) = cals.last() {
                let key = SCHEDULES.iter().find(|(_k, _l, c)| c == cal).map_or("weekly", |(k, _l, _c)| *k);
                return Ok(key.to_owned());
            }
        }
        Err(Ok(_)) => {}
        Err(Err(e)) => return Err(e),
    }
    if std::fs::symlink_metadata(files.user).is_ok() || !files.shipped.exists() {
        current_schedule_in(files.user)
    } else {
        current_schedule_in(files.shipped)
    }
}

fn current_schedule_in(timer_file: &Path) -> Result<String, String> {
    let text = match read_text(timer_file) {
        Ok(t) => t,
        Err(Ok(_)) => return Ok("weekly".to_owned()),
        Err(Err(e)) => return Err(e),
    };
    for line in py_splitlines(&text) {
        if line.starts_with("OnCalendar=") {
            let cal = py_strip(line.split_once('=').map(|(_, v)| v).unwrap_or(""));
            for (key, _label, c) in SCHEDULES {
                if *c == cal {
                    return Ok((*key).to_owned());
                }
            }
        }
    }
    Ok("weekly".to_owned())
}


/// Code points `str.isprintable()` rejects (Python 3.14, Unicode 16.0.0),
/// as inclusive ranges, generated from `unicodedata`.
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
    (0xfbd, 0xfbd), (0xfcd, 0xfcd), (0xfdb, 0xfff), (0x10c6, 0x10c6), (0x10c8, 0x10cc),
    (0x10ce, 0x10cf), (0x1249, 0x1249), (0x124e, 0x124f), (0x1257, 0x1257), (0x1259, 0x1259),
    (0x125e, 0x125f), (0x1289, 0x1289), (0x128e, 0x128f), (0x12b1, 0x12b1), (0x12b6, 0x12b7),
    (0x12bf, 0x12bf), (0x12c1, 0x12c1), (0x12c6, 0x12c7), (0x12d7, 0x12d7), (0x1311, 0x1311),
    (0x1316, 0x1317), (0x135b, 0x135c), (0x137d, 0x137f), (0x139a, 0x139f), (0x13f6, 0x13f7),
    (0x13fe, 0x13ff), (0x1680, 0x1680), (0x169d, 0x169f), (0x16f9, 0x16ff), (0x1716, 0x171e),
    (0x1737, 0x173f), (0x1754, 0x175f), (0x176d, 0x176d), (0x1771, 0x1771), (0x1774, 0x177f),
    (0x17de, 0x17df), (0x17ea, 0x17ef), (0x17fa, 0x17ff), (0x180e, 0x180e), (0x181a, 0x181f),
    (0x1879, 0x187f), (0x18ab, 0x18af), (0x18f6, 0x18ff), (0x191f, 0x191f), (0x192c, 0x192f),
    (0x193c, 0x193f), (0x1941, 0x1943), (0x196e, 0x196f), (0x1975, 0x197f), (0x19ac, 0x19af),
    (0x19ca, 0x19cf), (0x19db, 0x19dd), (0x1a1c, 0x1a1d), (0x1a5f, 0x1a5f), (0x1a7d, 0x1a7e),
    (0x1a8a, 0x1a8f), (0x1a9a, 0x1a9f), (0x1aae, 0x1aaf), (0x1acf, 0x1aff), (0x1b4d, 0x1b4d),
    (0x1bf4, 0x1bfb), (0x1c38, 0x1c3a), (0x1c4a, 0x1c4c), (0x1c8b, 0x1c8f), (0x1cbb, 0x1cbc),
    (0x1cc8, 0x1ccf), (0x1cfb, 0x1cff), (0x1f16, 0x1f17), (0x1f1e, 0x1f1f), (0x1f46, 0x1f47),
    (0x1f4e, 0x1f4f), (0x1f58, 0x1f58), (0x1f5a, 0x1f5a), (0x1f5c, 0x1f5c), (0x1f5e, 0x1f5e),
    (0x1f7e, 0x1f7f), (0x1fb5, 0x1fb5), (0x1fc5, 0x1fc5), (0x1fd4, 0x1fd5), (0x1fdc, 0x1fdc),
    (0x1ff0, 0x1ff1), (0x1ff5, 0x1ff5), (0x1fff, 0x200f), (0x2028, 0x202f), (0x205f, 0x206f),
    (0x2072, 0x2073), (0x208f, 0x208f), (0x209d, 0x209f), (0x20c1, 0x20cf), (0x20f1, 0x20ff),
    (0x218c, 0x218f), (0x242a, 0x243f), (0x244b, 0x245f), (0x2b74, 0x2b75), (0x2b96, 0x2b96),
    (0x2cf4, 0x2cf8), (0x2d26, 0x2d26), (0x2d28, 0x2d2c), (0x2d2e, 0x2d2f), (0x2d68, 0x2d6e),
    (0x2d71, 0x2d7e), (0x2d97, 0x2d9f), (0x2da7, 0x2da7), (0x2daf, 0x2daf), (0x2db7, 0x2db7),
    (0x2dbf, 0x2dbf), (0x2dc7, 0x2dc7), (0x2dcf, 0x2dcf), (0x2dd7, 0x2dd7), (0x2ddf, 0x2ddf),
    (0x2e5e, 0x2e7f), (0x2e9a, 0x2e9a), (0x2ef4, 0x2eff), (0x2fd6, 0x2fef), (0x3000, 0x3000),
    (0x3040, 0x3040), (0x3097, 0x3098), (0x3100, 0x3104), (0x3130, 0x3130), (0x318f, 0x318f),
    (0x31e6, 0x31ee), (0x321f, 0x321f), (0xa48d, 0xa48f), (0xa4c7, 0xa4cf), (0xa62c, 0xa63f),
    (0xa6f8, 0xa6ff), (0xa7ce, 0xa7cf), (0xa7d2, 0xa7d2), (0xa7d4, 0xa7d4), (0xa7dd, 0xa7f1),
    (0xa82d, 0xa82f), (0xa83a, 0xa83f), (0xa878, 0xa87f), (0xa8c6, 0xa8cd), (0xa8da, 0xa8df),
    (0xa954, 0xa95e), (0xa97d, 0xa97f), (0xa9ce, 0xa9ce), (0xa9da, 0xa9dd), (0xa9ff, 0xa9ff),
    (0xaa37, 0xaa3f), (0xaa4e, 0xaa4f), (0xaa5a, 0xaa5b), (0xaac3, 0xaada), (0xaaf7, 0xab00),
    (0xab07, 0xab08), (0xab0f, 0xab10), (0xab17, 0xab1f), (0xab27, 0xab27), (0xab2f, 0xab2f),
    (0xab6c, 0xab6f), (0xabee, 0xabef), (0xabfa, 0xabff), (0xd7a4, 0xd7af), (0xd7c7, 0xd7ca),
    (0xd7fc, 0xf8ff), (0xfa6e, 0xfa6f), (0xfada, 0xfaff), (0xfb07, 0xfb12), (0xfb18, 0xfb1c),
    (0xfb37, 0xfb37), (0xfb3d, 0xfb3d), (0xfb3f, 0xfb3f), (0xfb42, 0xfb42), (0xfb45, 0xfb45),
    (0xfbc3, 0xfbd2), (0xfd90, 0xfd91), (0xfdc8, 0xfdce), (0xfdd0, 0xfdef), (0xfe1a, 0xfe1f),
    (0xfe53, 0xfe53), (0xfe67, 0xfe67), (0xfe6c, 0xfe6f), (0xfe75, 0xfe75), (0xfefd, 0xff00),
    (0xffbf, 0xffc1), (0xffc8, 0xffc9), (0xffd0, 0xffd1), (0xffd8, 0xffd9), (0xffdd, 0xffdf),
    (0xffe7, 0xffe7), (0xffef, 0xfffb), (0xfffe, 0xffff), (0x1000c, 0x1000c), (0x10027, 0x10027),
    (0x1003b, 0x1003b), (0x1003e, 0x1003e), (0x1004e, 0x1004f), (0x1005e, 0x1007f),
    (0x100fb, 0x100ff), (0x10103, 0x10106), (0x10134, 0x10136), (0x1018f, 0x1018f),
    (0x1019d, 0x1019f), (0x101a1, 0x101cf), (0x101fe, 0x1027f), (0x1029d, 0x1029f),
    (0x102d1, 0x102df), (0x102fc, 0x102ff), (0x10324, 0x1032c), (0x1034b, 0x1034f),
    (0x1037b, 0x1037f), (0x1039e, 0x1039e), (0x103c4, 0x103c7), (0x103d6, 0x103ff),
    (0x1049e, 0x1049f), (0x104aa, 0x104af), (0x104d4, 0x104d7), (0x104fc, 0x104ff),
    (0x10528, 0x1052f), (0x10564, 0x1056e), (0x1057b, 0x1057b), (0x1058b, 0x1058b),
    (0x10593, 0x10593), (0x10596, 0x10596), (0x105a2, 0x105a2), (0x105b2, 0x105b2),
    (0x105ba, 0x105ba), (0x105bd, 0x105bf), (0x105f4, 0x105ff), (0x10737, 0x1073f),
    (0x10756, 0x1075f), (0x10768, 0x1077f), (0x10786, 0x10786), (0x107b1, 0x107b1),
    (0x107bb, 0x107ff), (0x10806, 0x10807), (0x10809, 0x10809), (0x10836, 0x10836),
    (0x10839, 0x1083b), (0x1083d, 0x1083e), (0x10856, 0x10856), (0x1089f, 0x108a6),
    (0x108b0, 0x108df), (0x108f3, 0x108f3), (0x108f6, 0x108fa), (0x1091c, 0x1091e),
    (0x1093a, 0x1093e), (0x10940, 0x1097f), (0x109b8, 0x109bb), (0x109d0, 0x109d1),
    (0x10a04, 0x10a04), (0x10a07, 0x10a0b), (0x10a14, 0x10a14), (0x10a18, 0x10a18),
    (0x10a36, 0x10a37), (0x10a3b, 0x10a3e), (0x10a49, 0x10a4f), (0x10a59, 0x10a5f),
    (0x10aa0, 0x10abf), (0x10ae7, 0x10aea), (0x10af7, 0x10aff), (0x10b36, 0x10b38),
    (0x10b56, 0x10b57), (0x10b73, 0x10b77), (0x10b92, 0x10b98), (0x10b9d, 0x10ba8),
    (0x10bb0, 0x10bff), (0x10c49, 0x10c7f), (0x10cb3, 0x10cbf), (0x10cf3, 0x10cf9),
    (0x10d28, 0x10d2f), (0x10d3a, 0x10d3f), (0x10d66, 0x10d68), (0x10d86, 0x10d8d),
    (0x10d90, 0x10e5f), (0x10e7f, 0x10e7f), (0x10eaa, 0x10eaa), (0x10eae, 0x10eaf),
    (0x10eb2, 0x10ec1), (0x10ec5, 0x10efb), (0x10f28, 0x10f2f), (0x10f5a, 0x10f6f),
    (0x10f8a, 0x10faf), (0x10fcc, 0x10fdf), (0x10ff7, 0x10fff), (0x1104e, 0x11051),
    (0x11076, 0x1107e), (0x110bd, 0x110bd), (0x110c3, 0x110cf), (0x110e9, 0x110ef),
    (0x110fa, 0x110ff), (0x11135, 0x11135), (0x11148, 0x1114f), (0x11177, 0x1117f),
    (0x111e0, 0x111e0), (0x111f5, 0x111ff), (0x11212, 0x11212), (0x11242, 0x1127f),
    (0x11287, 0x11287), (0x11289, 0x11289), (0x1128e, 0x1128e), (0x1129e, 0x1129e),
    (0x112aa, 0x112af), (0x112eb, 0x112ef), (0x112fa, 0x112ff), (0x11304, 0x11304),
    (0x1130d, 0x1130e), (0x11311, 0x11312), (0x11329, 0x11329), (0x11331, 0x11331),
    (0x11334, 0x11334), (0x1133a, 0x1133a), (0x11345, 0x11346), (0x11349, 0x1134a),
    (0x1134e, 0x1134f), (0x11351, 0x11356), (0x11358, 0x1135c), (0x11364, 0x11365),
    (0x1136d, 0x1136f), (0x11375, 0x1137f), (0x1138a, 0x1138a), (0x1138c, 0x1138d),
    (0x1138f, 0x1138f), (0x113b6, 0x113b6), (0x113c1, 0x113c1), (0x113c3, 0x113c4),
    (0x113c6, 0x113c6), (0x113cb, 0x113cb), (0x113d6, 0x113d6), (0x113d9, 0x113e0),
    (0x113e3, 0x113ff), (0x1145c, 0x1145c), (0x11462, 0x1147f), (0x114c8, 0x114cf),
    (0x114da, 0x1157f), (0x115b6, 0x115b7), (0x115de, 0x115ff), (0x11645, 0x1164f),
    (0x1165a, 0x1165f), (0x1166d, 0x1167f), (0x116ba, 0x116bf), (0x116ca, 0x116cf),
    (0x116e4, 0x116ff), (0x1171b, 0x1171c), (0x1172c, 0x1172f), (0x11747, 0x117ff),
    (0x1183c, 0x1189f), (0x118f3, 0x118fe), (0x11907, 0x11908), (0x1190a, 0x1190b),
    (0x11914, 0x11914), (0x11917, 0x11917), (0x11936, 0x11936), (0x11939, 0x1193a),
    (0x11947, 0x1194f), (0x1195a, 0x1199f), (0x119a8, 0x119a9), (0x119d8, 0x119d9),
    (0x119e5, 0x119ff), (0x11a48, 0x11a4f), (0x11aa3, 0x11aaf), (0x11af9, 0x11aff),
    (0x11b0a, 0x11bbf), (0x11be2, 0x11bef), (0x11bfa, 0x11bff), (0x11c09, 0x11c09),
    (0x11c37, 0x11c37), (0x11c46, 0x11c4f), (0x11c6d, 0x11c6f), (0x11c90, 0x11c91),
    (0x11ca8, 0x11ca8), (0x11cb7, 0x11cff), (0x11d07, 0x11d07), (0x11d0a, 0x11d0a),
    (0x11d37, 0x11d39), (0x11d3b, 0x11d3b), (0x11d3e, 0x11d3e), (0x11d48, 0x11d4f),
    (0x11d5a, 0x11d5f), (0x11d66, 0x11d66), (0x11d69, 0x11d69), (0x11d8f, 0x11d8f),
    (0x11d92, 0x11d92), (0x11d99, 0x11d9f), (0x11daa, 0x11edf), (0x11ef9, 0x11eff),
    (0x11f11, 0x11f11), (0x11f3b, 0x11f3d), (0x11f5b, 0x11faf), (0x11fb1, 0x11fbf),
    (0x11ff2, 0x11ffe), (0x1239a, 0x123ff), (0x1246f, 0x1246f), (0x12475, 0x1247f),
    (0x12544, 0x12f8f), (0x12ff3, 0x12fff), (0x13430, 0x1343f), (0x13456, 0x1345f),
    (0x143fb, 0x143ff), (0x14647, 0x160ff), (0x1613a, 0x167ff), (0x16a39, 0x16a3f),
    (0x16a5f, 0x16a5f), (0x16a6a, 0x16a6d), (0x16abf, 0x16abf), (0x16aca, 0x16acf),
    (0x16aee, 0x16aef), (0x16af6, 0x16aff), (0x16b46, 0x16b4f), (0x16b5a, 0x16b5a),
    (0x16b62, 0x16b62), (0x16b78, 0x16b7c), (0x16b90, 0x16d3f), (0x16d7a, 0x16e3f),
    (0x16e9b, 0x16eff), (0x16f4b, 0x16f4e), (0x16f88, 0x16f8e), (0x16fa0, 0x16fdf),
    (0x16fe5, 0x16fef), (0x16ff2, 0x16fff), (0x187f8, 0x187ff), (0x18cd6, 0x18cfe),
    (0x18d09, 0x1afef), (0x1aff4, 0x1aff4), (0x1affc, 0x1affc), (0x1afff, 0x1afff),
    (0x1b123, 0x1b131), (0x1b133, 0x1b14f), (0x1b153, 0x1b154), (0x1b156, 0x1b163),
    (0x1b168, 0x1b16f), (0x1b2fc, 0x1bbff), (0x1bc6b, 0x1bc6f), (0x1bc7d, 0x1bc7f),
    (0x1bc89, 0x1bc8f), (0x1bc9a, 0x1bc9b), (0x1bca0, 0x1cbff), (0x1ccfa, 0x1ccff),
    (0x1ceb4, 0x1ceff), (0x1cf2e, 0x1cf2f), (0x1cf47, 0x1cf4f), (0x1cfc4, 0x1cfff),
    (0x1d0f6, 0x1d0ff), (0x1d127, 0x1d128), (0x1d173, 0x1d17a), (0x1d1eb, 0x1d1ff),
    (0x1d246, 0x1d2bf), (0x1d2d4, 0x1d2df), (0x1d2f4, 0x1d2ff), (0x1d357, 0x1d35f),
    (0x1d379, 0x1d3ff), (0x1d455, 0x1d455), (0x1d49d, 0x1d49d), (0x1d4a0, 0x1d4a1),
    (0x1d4a3, 0x1d4a4), (0x1d4a7, 0x1d4a8), (0x1d4ad, 0x1d4ad), (0x1d4ba, 0x1d4ba),
    (0x1d4bc, 0x1d4bc), (0x1d4c4, 0x1d4c4), (0x1d506, 0x1d506), (0x1d50b, 0x1d50c),
    (0x1d515, 0x1d515), (0x1d51d, 0x1d51d), (0x1d53a, 0x1d53a), (0x1d53f, 0x1d53f),
    (0x1d545, 0x1d545), (0x1d547, 0x1d549), (0x1d551, 0x1d551), (0x1d6a6, 0x1d6a7),
    (0x1d7cc, 0x1d7cd), (0x1da8c, 0x1da9a), (0x1daa0, 0x1daa0), (0x1dab0, 0x1deff),
    (0x1df1f, 0x1df24), (0x1df2b, 0x1dfff), (0x1e007, 0x1e007), (0x1e019, 0x1e01a),
    (0x1e022, 0x1e022), (0x1e025, 0x1e025), (0x1e02b, 0x1e02f), (0x1e06e, 0x1e08e),
    (0x1e090, 0x1e0ff), (0x1e12d, 0x1e12f), (0x1e13e, 0x1e13f), (0x1e14a, 0x1e14d),
    (0x1e150, 0x1e28f), (0x1e2af, 0x1e2bf), (0x1e2fa, 0x1e2fe), (0x1e300, 0x1e4cf),
    (0x1e4fa, 0x1e5cf), (0x1e5fb, 0x1e5fe), (0x1e600, 0x1e7df), (0x1e7e7, 0x1e7e7),
    (0x1e7ec, 0x1e7ec), (0x1e7ef, 0x1e7ef), (0x1e7ff, 0x1e7ff), (0x1e8c5, 0x1e8c6),
    (0x1e8d7, 0x1e8ff), (0x1e94c, 0x1e94f), (0x1e95a, 0x1e95d), (0x1e960, 0x1ec70),
    (0x1ecb5, 0x1ed00), (0x1ed3e, 0x1edff), (0x1ee04, 0x1ee04), (0x1ee20, 0x1ee20),
    (0x1ee23, 0x1ee23), (0x1ee25, 0x1ee26), (0x1ee28, 0x1ee28), (0x1ee33, 0x1ee33),
    (0x1ee38, 0x1ee38), (0x1ee3a, 0x1ee3a), (0x1ee3c, 0x1ee41), (0x1ee43, 0x1ee46),
    (0x1ee48, 0x1ee48), (0x1ee4a, 0x1ee4a), (0x1ee4c, 0x1ee4c), (0x1ee50, 0x1ee50),
    (0x1ee53, 0x1ee53), (0x1ee55, 0x1ee56), (0x1ee58, 0x1ee58), (0x1ee5a, 0x1ee5a),
    (0x1ee5c, 0x1ee5c), (0x1ee5e, 0x1ee5e), (0x1ee60, 0x1ee60), (0x1ee63, 0x1ee63),
    (0x1ee65, 0x1ee66), (0x1ee6b, 0x1ee6b), (0x1ee73, 0x1ee73), (0x1ee78, 0x1ee78),
    (0x1ee7d, 0x1ee7d), (0x1ee7f, 0x1ee7f), (0x1ee8a, 0x1ee8a), (0x1ee9c, 0x1eea0),
    (0x1eea4, 0x1eea4), (0x1eeaa, 0x1eeaa), (0x1eebc, 0x1eeef), (0x1eef2, 0x1efff),
    (0x1f02c, 0x1f02f), (0x1f094, 0x1f09f), (0x1f0af, 0x1f0b0), (0x1f0c0, 0x1f0c0),
    (0x1f0d0, 0x1f0d0), (0x1f0f6, 0x1f0ff), (0x1f1ae, 0x1f1e5), (0x1f203, 0x1f20f),
    (0x1f23c, 0x1f23f), (0x1f249, 0x1f24f), (0x1f252, 0x1f25f), (0x1f266, 0x1f2ff),
    (0x1f6d8, 0x1f6db), (0x1f6ed, 0x1f6ef), (0x1f6fd, 0x1f6ff), (0x1f777, 0x1f77a),
    (0x1f7da, 0x1f7df), (0x1f7ec, 0x1f7ef), (0x1f7f1, 0x1f7ff), (0x1f80c, 0x1f80f),
    (0x1f848, 0x1f84f), (0x1f85a, 0x1f85f), (0x1f888, 0x1f88f), (0x1f8ae, 0x1f8af),
    (0x1f8bc, 0x1f8bf), (0x1f8c2, 0x1f8ff), (0x1fa54, 0x1fa5f), (0x1fa6e, 0x1fa6f),
    (0x1fa7d, 0x1fa7f), (0x1fa8a, 0x1fa8e), (0x1fac7, 0x1facd), (0x1fadd, 0x1fade),
    (0x1faea, 0x1faef), (0x1faf9, 0x1faff), (0x1fb93, 0x1fb93), (0x1fbfa, 0x1ffff),
    (0x2a6e0, 0x2a6ff), (0x2b73a, 0x2b73f), (0x2b81e, 0x2b81f), (0x2cea2, 0x2ceaf),
    (0x2ebe1, 0x2ebef), (0x2ee5e, 0x2f7ff), (0x2fa1e, 0x2ffff), (0x3134b, 0x3134f),
    (0x323b0, 0xe00ff), (0xe01f0, 0x10ffff),
];

#[cfg(test)]
mod tests {
    // test_app.py has no tests for autoupdate; these pin the behaviour the
    // parity cases also check, so `cargo test` covers it without Python.
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("almanac-autoupdate-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    const TIMER: &str = "[Unit]\nDescription=x\n\n[Timer]\nOnCalendar=Sun 11:00\nPersistent=true\nRandomizedDelaySec=30m\n";

    fn round_trip(text: &str) -> String {
        let d = tmp(&format!("rt{}", text.len()));
        let f = d.join("almanac/auto-update.json");
        std::fs::create_dir_all(d.join("almanac")).unwrap();
        std::fs::write(&f, text).unwrap();
        let c = load_from(&f).unwrap();
        save_to(&d.join("almanac"), &f, &c).unwrap();
        std::fs::read_to_string(&f).unwrap()
    }

    #[test]
    fn load_defaults_and_merge() {
        let d = tmp("load");
        let f = d.join("auto-update.json");
        assert_eq!(load_from(&f).unwrap(), *DEFAULTS);
        std::fs::write(&f, r#"{"plugins": true, "x": 1}"#).unwrap();
        let c = load_from(&f).unwrap();
        assert!(c.get_bool("plugins"));
        assert_eq!(c.get("x"), Some(&Value::from(1)));
        std::fs::write(&f, "not json").unwrap();
        assert_eq!(load_from(&f).unwrap(), *DEFAULTS);
        std::fs::write(&f, "null").unwrap();
        assert!(load_from(&f).is_err());
    }

    #[test]
    fn save_round_trips() {
        let d = tmp("save");
        let f = d.join("almanac/auto-update.json");
        save_to(&d.join("almanac"), &f, &DEFAULTS).unwrap();
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(text.starts_with("{\n  \"schedule\": \"weekly\",\n"));
        assert!(text.ends_with("}\n"));
        assert_eq!(load_from(&f).unwrap(), *DEFAULTS);
    }

    #[test]
    fn json_like_python() {
        // What json.loads accepts and json.dumps writes back.
        let t = round_trip(r#"{"x": NaN, "y": -Infinity, "z": 1e400, "big": 123456789012345678901234567890, "n": -0, "f": -0.0, "s": "\ud800", "p": "\ud83d\ude00"}"#);
        assert!(t.contains("\"x\": NaN,\n  \"y\": -Infinity,\n  \"z\": Infinity,\n  \"big\": 123456789012345678901234567890,\n  \"n\": 0,\n  \"f\": -0.0,\n  \"s\": \"\\ud800\",\n  \"p\": \"\\ud83d\\ude00\""), "{t}");
        // Keys keep their Python type and equality: 1e16 == 10**16, True == 1.
        let t = round_trip(r#"[[1e16, "a"], [10000000000000000, "b"], [1, "c"], ["1", "d"], [true, "e"], [null, 0]]"#);
        assert!(t.contains("\"1e+16\": \"b\",\n  \"1\": \"e\",\n  \"1\": \"d\",\n  \"null\": 0\n}"), "{t}");
        for bad in ["[1,]", "01", "1.", "\u{feff}{}", "{\"a\": \"\x01\"}", "nan", "-NaN", "\"\\ud800\\udc0\""] {
            assert!(matches!(json_loads(bad), Err(LoadsError::Value(_))), "{bad}");
        }
        assert!(matches!(json_loads(&"1".repeat(4301)), Err(LoadsError::Value(_))));
        assert!(json_loads(&"1".repeat(4300)).is_ok());
    }

    #[test]
    fn deep_nesting_does_not_overflow() {
        let deep = |n: usize| format!("{{\"a\": {}{}}}", "[".repeat(n), "]".repeat(n));
        let ok = json_loads(&deep(DECODE_MAX_DEPTH - 1));
        assert!(ok.is_ok());
        assert!(matches!(json_loads(&deep(DECODE_MAX_DEPTH)), Err(LoadsError::Recursion(_))));
        let mut c = Config::empty();
        let Ok(mut top) = ok else { unreachable!() };
        let PyValue::Dict(d) = &mut top else { unreachable!() };
        let (k, v) = d.pop().unwrap();
        c.insert(PyKey::Str(k), v);
        assert!(c.get_bool("a"));
        assert!(dumps(&c).is_err());
    }

    #[test]
    fn paths_like_pathlib() {
        assert_eq!(py_norm(b"/home/tim//.config/./"), b"/home/tim/.config");
        assert_eq!(py_norm(b"//x//y"), b"//x/y");
        assert_eq!(py_norm(b"///x"), b"/x");
        assert_eq!(py_norm(b""), b".");
        assert_eq!(py_norm(&py_join(b"", b"almanac")), b"almanac");
    }

    #[test]
    fn repr_like_python() {
        assert_eq!(py_repr("\u{378}"), "'\\u0378'");
        assert_eq!(py_repr("\u{e0001}"), "'\\U000e0001'");
        assert_eq!(py_repr("é日"), "'é日'");
        assert_eq!(py_repr("it's"), "\"it's\"");
    }

    #[test]
    fn decode_errors_like_python() {
        assert_eq!(py_decode(b"ab\xff".to_vec()).unwrap_err(), "'utf-8' codec can't decode byte 0xff in position 2: invalid start byte");
        assert_eq!(py_decode(b"\xe2\x28".to_vec()).unwrap_err(), "'utf-8' codec can't decode byte 0xe2 in position 0: invalid continuation byte");
        assert_eq!(py_decode(b"\xe2\x82".to_vec()).unwrap_err(), "'utf-8' codec can't decode bytes in position 0-1: unexpected end of data");
    }

    #[test]
    fn systemctl_text_mode() {
        assert_eq!(text_mode(b"a\r\nb\rc".to_vec()).unwrap(), "a\nb\nc");
        let r = run_capture(&["sh", "-c", "kill -TERM $$"], Duration::from_secs(5)).unwrap();
        assert!(matches!(r, Ran::Exited { code: -15, .. }));
        let r = run_capture(&["sh", "-c", "(sleep 3 &); echo x"], Duration::from_secs(1)).unwrap();
        assert!(matches!(r, Ran::TimedOut));
    }

    #[test]
    fn set_schedule_rewrites_only_oncalendar() {
        let d = tmp("sched");
        let t = d.join(UNIT);
        std::fs::write(&t, TIMER).unwrap();
        let mut calls = Vec::new();
        let mut sys = |a: &[&str], _t: u64| {
            calls.push(a.join(" "));
            Ok(if a[0] == "is-enabled" { (0, "enabled".to_owned()) } else { (0, String::new()) })
        };
        let r = set_schedule_with("daily", &t, &mut sys).unwrap();
        assert_eq!(r, (true, "*-*-* 11:00:00".to_owned()));
        let text = std::fs::read_to_string(&t).unwrap();
        assert!(text.contains("OnCalendar=*-*-* 11:00:00\nPersistent=true\nRandomizedDelaySec=30m\n"));
        assert_eq!(calls, vec!["daemon-reload".to_owned(), format!("is-enabled {UNIT}"), format!("restart {UNIT}")]);
        assert_eq!(current_schedule_in(&t).unwrap(), "daily");
        let mut none = |_a: &[&str], _t: u64| Ok((0, String::new()));
        assert_eq!(set_schedule_with("hourly", &t, &mut none).unwrap(), (false, "unknown schedule 'hourly'".to_owned()));
    }

    /// The unit is replaced by a rename, not rewritten in place: no
    /// temporary file is left beside it, and it ends up 0644 whatever the
    /// file it replaced was.
    #[test]
    fn set_schedule_replaces_the_unit_atomically() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmp("sched-atomic");
        let t = d.join(UNIT);
        std::fs::write(&t, TIMER).unwrap();
        std::fs::set_permissions(&t, std::fs::Permissions::from_mode(0o600)).unwrap();
        let ino = |p: &Path| std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(p).unwrap());
        let before = ino(&t);
        let mut sys = |_a: &[&str], _t: u64| Ok((1, String::new()));
        assert_eq!(set_schedule_with("daily", &t, &mut sys).unwrap(), (true, "*-*-* 11:00:00".to_owned()));
        assert_ne!(ino(&t), before, "written in place");
        assert_eq!(std::fs::metadata(&t).unwrap().permissions().mode() & 0o777, 0o644);
        let names: Vec<_> = std::fs::read_dir(&d).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, vec![std::ffi::OsString::from(UNIT)]);
        assert_eq!(current_schedule_in(&t).unwrap(), "daily");
    }

    fn files(d: &Path) -> (PathBuf, PathBuf, PathBuf) {
        std::fs::create_dir_all(d.join("user")).unwrap();
        std::fs::create_dir_all(d.join("usr")).unwrap();
        (d.join("user").join(UNIT), d.join("usr").join(UNIT), d.join("user").join(format!("{UNIT}.d/schedule.conf")))
    }

    /// With only the packaged unit, the schedule goes in a drop-in in the
    /// user's directory; the packaged file is left as it was.
    #[test]
    fn a_packaged_timer_is_rescheduled_with_a_drop_in() {
        let d = tmp("sched-dropin");
        let (user, shipped, dropin) = files(&d);
        std::fs::write(&shipped, TIMER).unwrap();
        let f = TimerFiles { user: &user, shipped: &shipped, dropin: &dropin };
        assert_eq!(current_schedule_of(&f).unwrap(), "weekly");
        let mut calls = Vec::new();
        let mut sys = |a: &[&str], _t: u64| {
            calls.push(a.join(" "));
            Ok(if a[0] == "is-enabled" { (1, "disabled".to_owned()) } else { (0, String::new()) })
        };
        assert_eq!(set_schedule_in("monthly", &f, &mut sys).unwrap(), (true, "Sun *-*-1..7 11:00:00".to_owned()));
        assert_eq!(calls, vec!["daemon-reload".to_owned(), format!("is-enabled {UNIT}")]);
        assert_eq!(std::fs::read_to_string(&shipped).unwrap(), TIMER);
        assert!(!user.exists(), "no user copy of the unit is made");
        let text = std::fs::read_to_string(&dropin).unwrap();
        assert!(text.ends_with("[Timer]\nOnCalendar=\nOnCalendar=Sun *-*-1..7 11:00:00\n"), "{text}");
        assert_eq!(current_schedule_of(&f).unwrap(), "monthly");
        // A second change rewrites the drop-in, not adds to it.
        let mut none = |_a: &[&str], _t: u64| Ok((0, String::new()));
        set_schedule_in("daily", &f, &mut none).unwrap();
        assert_eq!(std::fs::read_to_string(&dropin).unwrap().matches("OnCalendar=").count(), 2);
        assert_eq!(current_schedule_of(&f).unwrap(), "daily");
        assert_eq!(set_schedule_in("hourly", &f, &mut none).unwrap(), (false, "unknown schedule 'hourly'".to_owned()));
    }

    /// A full copy in the user's directory (installs from before the timer
    /// shipped) is still edited in place, and is what the schedule is read
    /// from, over the packaged unit.
    #[test]
    fn a_user_copy_of_the_timer_is_still_edited_in_place() {
        let d = tmp("sched-usercopy");
        let (user, shipped, dropin) = files(&d);
        std::fs::write(&shipped, TIMER).unwrap();
        std::fs::write(&user, TIMER.replace("Sun 11:00", "*-*-* 11:00:00")).unwrap();
        let f = TimerFiles { user: &user, shipped: &shipped, dropin: &dropin };
        assert_eq!(current_schedule_of(&f).unwrap(), "daily");
        let mut none = |_a: &[&str], _t: u64| Ok((1, String::new()));
        assert!(set_schedule_in("fortnightly", &f, &mut none).unwrap().0);
        assert!(std::fs::read_to_string(&user).unwrap().contains("OnCalendar=Sun *-*-1..7,15..21 11:00:00\n"));
        assert!(!dropin.exists());
        assert_eq!(current_schedule_of(&f).unwrap(), "fortnightly");
    }

    /// A drop-in overrides whichever unit file there is, so once one exists
    /// it is the one changed and read.
    #[test]
    fn an_existing_drop_in_wins_over_a_user_copy() {
        let d = tmp("sched-both");
        let (user, shipped, dropin) = files(&d);
        std::fs::write(&user, TIMER).unwrap();
        std::fs::create_dir_all(dropin.parent().unwrap()).unwrap();
        std::fs::write(&dropin, "[Timer]\nOnCalendar=\nOnCalendar=*-*-* 11:00:00\n").unwrap();
        let f = TimerFiles { user: &user, shipped: &shipped, dropin: &dropin };
        assert_eq!(current_schedule_of(&f).unwrap(), "daily");
        let mut none = |_a: &[&str], _t: u64| Ok((1, String::new()));
        set_schedule_in("monthly", &f, &mut none).unwrap();
        assert_eq!(std::fs::read_to_string(&user).unwrap(), TIMER);
        assert_eq!(current_schedule_of(&f).unwrap(), "monthly");
    }

    /// Neither unit on disk: the schedule reads as the default, and setting
    /// one says why it could not, as before the timer shipped.
    #[test]
    fn no_timer_at_all_reads_weekly_and_cannot_be_rescheduled() {
        let d = tmp("sched-none");
        let (user, shipped, dropin) = files(&d);
        let f = TimerFiles { user: &user, shipped: &shipped, dropin: &dropin };
        assert_eq!(current_schedule_of(&f).unwrap(), "weekly");
        let mut none = |_a: &[&str], _t: u64| Ok((0, String::new()));
        let (ok, msg) = set_schedule_in("daily", &f, &mut none).unwrap();
        assert!(!ok);
        assert!(msg.starts_with("[Errno 2] No such file or directory"), "{msg}");
        assert!(!dropin.exists());
    }

    #[test]
    fn status_parses_show() {
        let mut sys = |a: &[&str], _t: u64| {
            Ok(match a[0] {
                "show" => (0, "NextElapseUSecRealtime=Sun 2026-10-18 11:00:00 PDT\nLastTriggerUSec=\nUnit=x".to_owned()),
                _ => (1, "disabled".to_owned()),
            })
        };
        let s = status_with(&mut sys).unwrap();
        assert_eq!(s, Status { enabled: false, next: "Sun 2026-10-18 11:00:00 PDT".into(), last: "never".into() });
    }

    #[test]
    fn splitlines_like_python() {
        assert_eq!(py_splitlines("a\r\nb\rc\n\nd\u{2028}e\n"), vec!["a", "b", "c", "", "d", "e"]);
        assert!(py_splitlines("").is_empty());
    }

    #[test]
    fn codec_like_python_startup() {
        assert_eq!(codec_for(Some("1"), None, true, None), Codec::Utf8);
        assert_eq!(codec_for(None, None, true, None), Codec::Utf8); // C locale: UTF-8 mode
        assert_eq!(codec_for(Some("0"), None, true, None), Codec::Ascii); // LC_ALL=C: no coercion
        assert_eq!(codec_for(Some("0"), None, false, None), Codec::Utf8); // coerced to C.UTF-8
        assert_eq!(codec_for(Some("0"), None, false, Some("0")), Codec::Ascii);
        assert_eq!(codec_for(None, Some("UTF-8"), false, None), Codec::Utf8);
        assert_eq!(codec_for(None, Some("ISO-8859-1"), false, None), Codec::Latin1);
        assert!(matches!(codec_for(None, Some("ISO-8859-15"), false, None), Codec::Other(..)));
        assert_eq!(
            decode_as(&Codec::Ascii, b"ab\xc3\xa9".to_vec()).unwrap_err(),
            "'ascii' codec can't decode byte 0xc3 in position 2: ordinal not in range(128)"
        );
        assert_eq!(decode_as(&Codec::Latin1, b"\xe9".to_vec()).unwrap(), "\u{e9}");
        assert_eq!(decode_as(&Codec::Ascii, b"ok".to_vec()).unwrap(), "ok");
    }

    #[test]
    fn os_bytes_like_python() {
        let p = to_path(b"/tmp/al\xffm".to_vec());
        assert_eq!(py_repr_path(&p), "'/tmp/al\\udcffm'");
        assert_eq!(backslashreplace(b"/tmp/al\xffm/\xe2\x82"), "/tmp/al\\xffm/\\xe2\\x82");
        assert_eq!(backslashreplace("/é".as_bytes()), "/é");
    }

    #[test]
    fn path_search_like_posixsubprocess() {
        let d = tmp("pathsearch");
        let bad = d.join("bad");
        let good = d.join("good");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::create_dir_all(&good).unwrap();
        use std::os::unix::fs::PermissionsExt;
        for (dir, body) in [(&bad, &b"echo bad\n"[..]), (&good, &b"#!/bin/sh\necho good\n"[..])] {
            let f = dir.join("prog");
            std::fs::write(&f, body).unwrap();
            std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let both = format!("{}:{}", bad.display(), good.display()).into_bytes();
        let run = |path: Vec<u8>| run_capture_in(&["prog"], Duration::from_secs(5), Some(path));
        let Ok(Ran::Exited { stdout, .. }) = run(both) else { panic!("prog did not run") };
        assert_eq!(stdout, b"good\n"); // past the ENOEXEC entry
        let Err(e) = run(bad.as_os_str().as_bytes().to_vec()) else { panic!("ran") };
        assert_eq!(e.raw_os_error(), Some(libc::ENOEXEC));
        let Err(e) = run(b"/nonexistent".to_vec()) else { panic!("ran") };
        assert_eq!(e.raw_os_error(), Some(libc::ENOENT));
        assert_eq!(systemctl(&["is-enabled", "a\0b"], 5).unwrap_err(), "embedded null byte");
    }
}
