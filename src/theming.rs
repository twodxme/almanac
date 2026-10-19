//! Port of `theming.py`.
//!
//! Wearing whatever theme Omarchy is wearing.
//!
//! Omarchy publishes the active theme as a symlink at
//! `~/.local/state/omarchy/current/theme`, and every theme ships a `colors.toml`
//! with a named palette — accent, three background depths, four foreground
//! weights, and the eight ANSI hues. Almanac reads that and restates it as
//! libadwaita's named colours, so the window re-skins with the rest of the
//! desktop instead of being the one app still wearing stock Adwaita.
//!
//! Two design decisions worth stating:
//!
//! * **Nothing here is Almanac-specific.** The palette is mapped onto
//!   `@accent_bg_color`, `@window_bg_color`, `@card_bg_color` and the rest, which
//!   every libadwaita widget already honours. Almanac's own CSS is written against
//!   those same names, so it follows for free and a new widget inherits the theme
//!   without anyone remembering to theme it.
//! * **Absence is a valid state.** On a machine with no Omarchy — which is most
//!   machines — there is no colours file, this does nothing, and GTK's own theme
//!   shows through. Theming is an enhancement, never a requirement.
//!
//! The theme is watched, not sampled once: `omarchy theme set` swaps the symlink
//! and the window follows within a frame or two.
//!
//! Port notes: the colour helpers (`mix`, `legible`, `dim_opacity`,
//! `pick_css`, `css_for`, `flatten`) can raise in the Python on input that is
//! not really a colour (`mix("#zzzzzz", …)` is a ValueError from `int()`).
//! Every caller in the app hands them validated hex, so the public functions
//! keep their infallible signatures; the exact Python behaviour, raise
//! included, lives in the `try_*` versions they wrap, which the parity hook
//! checks. On such input the wrappers fall back to "" (or 100 for
//! `dim_opacity`) instead of panicking.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use gtk::gio;

/// `STATE`: `$XDG_STATE_HOME`, or `~/.local/state`. The Python builds it once,
/// at import, as a `Path`; so it is computed once here too (an environment
/// change later is not seen) and normalised the way `Path()` normalises.
pub fn state() -> PathBuf {
    static STATE: OnceLock<PathBuf> = OnceLock::new();
    STATE
        .get_or_init(|| match std::env::var_os("XDG_STATE_HOME") {
            Some(v) => py_path(v.as_bytes()),
            None => py_path(py_home().as_bytes()).join(".local/state"),
        })
        .clone()
}

/// `CURRENT`: the symlink Omarchy points at the active theme
/// (`STATE / "omarchy/current/theme"`, also built once at import).
pub fn current() -> PathBuf {
    static CURRENT: OnceLock<PathBuf> = OnceLock::new();
    CURRENT
        .get_or_init(|| {
            let state = state();
            // Path(".") / "x" is "x", not "./x".
            if state.as_os_str() == "." {
                PathBuf::from("omarchy/current/theme")
            } else {
                state.join("omarchy/current/theme")
            }
        })
        .clone()
}

/// `str(Path(raw))` on POSIX: repeated slashes and `.` components dropped,
/// no trailing slash, exactly two leading slashes kept, "" becomes ".".
fn py_path(raw: &[u8]) -> PathBuf {
    let root: &[u8] = if raw.starts_with(b"//") && !raw.starts_with(b"///") {
        b"//"
    } else if raw.starts_with(b"/") {
        b"/"
    } else {
        b""
    };
    let parts: Vec<&[u8]> = raw.split(|&b| b == b'/').filter(|p| !p.is_empty() && *p != b".").collect();
    let mut out = root.to_vec();
    out.extend(parts.join(&b'/'));
    if out.is_empty() {
        out.push(b'.');
    }
    PathBuf::from(OsString::from_vec(out))
}

/// `os.path.expanduser("~")`, as `Path.home()` uses it: `$HOME` with trailing
/// slashes removed, "/" when that leaves nothing. (With HOME unset the
/// Python asks the password database; `util::home()` is used for that.)
fn py_home() -> OsString {
    match std::env::var_os("HOME") {
        Some(h) => {
            let b = h.as_bytes();
            let end = b.iter().rposition(|&c| c != b'/').map_or(0, |i| i + 1);
            if end == 0 {
                OsString::from("/")
            } else {
                OsString::from_vec(b[..end].to_vec())
            }
        }
        None => crate::util::home().into_os_string(),
    }
}

pub const COLORS: &str = "colors.toml";

/// `_HEX` as a pattern string: `#rgb` or `#rrggbb`.
pub const HEX_PATTERN: &str = r"^#(?:[0-9a-fA-F]{3}|[0-9a-fA-F]{6})$";

/// `_HEX.match(value)`. The value is always stripped first, so `$` matching
/// before a trailing newline never comes into it.
fn hex_match(value: &str) -> bool {
    let Some(rest) = value.strip_prefix('#') else {
        return false;
    };
    (rest.len() == 3 || rest.len() == 6) && rest.bytes().all(|b| b.is_ascii_hexdigit())
}

// --- Python string semantics the colour code leans on -----------------------

/// `str.isspace()` for one character: Rust's White_Space plus the four
/// information separators Python also counts.
fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `str.strip()`.
fn py_strip(s: &str) -> &str {
    s.trim_matches(py_isspace)
}

// Python 3.14's case tables are Unicode 16; Rust's std (1.98) has Unicode
// 17's. The letters 17 added are unassigned to Python -- no case, mapped to
// themselves -- and two older small letters gained a capital in 17 that
// Python does not upper-case to. (A sweep of every code point found no
// other difference in lower(), islower() or title().)

/// Assigned only in Unicode 17: Latin ꟎ ꟏ ꟒ ꟔ and Beria Erfe.
fn unicode17_new(c: char) -> bool {
    matches!(c as u32, 0xA7CE | 0xA7CF | 0xA7D2 | 0xA7D4 | 0x16EA0..=0x16EB8 | 0x16EBB..=0x16ED3)
}

/// Small letters whose capital (U+A7D2, U+A7D4) is new in Unicode 17.
fn unicode17_gained_upper(c: char) -> bool {
    matches!(c as u32, 0xA7D3 | 0xA7D5)
}

/// `str.lower()`. An unassigned character is neither cased nor
/// case-ignorable, so it ends a final-sigma context on either side: the
/// text between them is lower-cased on its own.
fn py_lower(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut start = 0;
    for (i, c) in s.char_indices() {
        if unicode17_new(c) {
            out.push_str(&s[start..i].to_lowercase());
            out.push(c);
            start = i + c.len_utf8();
        }
    }
    out.push_str(&s[start..].to_lowercase());
    out
}

/// The zero of every Unicode Nd block (each is ten consecutive digits).
/// Python's `int()` accepts any of them as a decimal digit.
const ND_ZEROS: &[u32] = &[
    0x30, 0x660, 0x6F0, 0x7C0, 0x966, 0x9E6, 0xA66, 0xAE6, 0xB66, 0xBE6, 0xC66, 0xCE6, 0xD66,
    0xDE6, 0xE50, 0xED0, 0xF20, 0x1040, 0x1090, 0x17E0, 0x1810, 0x1946, 0x19D0, 0x1A80, 0x1A90,
    0x1B50, 0x1BB0, 0x1C40, 0x1C50, 0xA620, 0xA8D0, 0xA900, 0xA9D0, 0xA9F0, 0xAA50, 0xABF0,
    0xFF10, 0x104A0, 0x10D30, 0x10D40, 0x11066, 0x110F0, 0x11136, 0x111D0, 0x112F0, 0x11450,
    0x114D0, 0x11650, 0x116C0, 0x116D0, 0x116DA, 0x11730, 0x118E0, 0x11950, 0x11BF0, 0x11C50,
    0x11D50, 0x11DA0, 0x11F50, 0x16130, 0x16A60, 0x16AC0, 0x16B50, 0x16D70, 0x1CCF0, 0x1D7CE,
    0x1D7D8, 0x1D7E2, 0x1D7EC, 0x1D7F6, 0x1E140, 0x1E2F0, 0x1E4F0, 0x1E5F1, 0x1E950, 0x1FBF0,
];

fn nd_value(c: char) -> Option<u32> {
    let cp = c as u32;
    ND_ZEROS.iter().find(|&&z| cp >= z && cp < z + 10).map(|&z| cp - z)
}

/// `int(s, 16)`: surrounding whitespace, a sign, an optional `0x` prefix and
/// single underscores between digits are allowed; Unicode decimal digits
/// count as digits. `Err` is Python's ValueError.
fn py_int16(s: &str) -> Result<i64, String> {
    let err = || Err(format!("invalid literal for int() with base 16: {s:?}"));
    // _PyUnicode_TransformDecimalAndSpaceToASCII: ASCII is kept as it is,
    // non-ASCII whitespace becomes a space and Unicode digits ASCII digits.
    let mut t = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii() {
            t.push(c);
        } else if py_isspace(c) {
            t.push(' ');
        } else if let Some(d) = nd_value(c) {
            t.push(char::from(b'0' + d as u8));
        } else {
            return err();
        }
    }
    // Then C isspace() is skipped at both ends (so "\x1c" is not).
    let t = t.trim_matches(|c: char| matches!(c, ' ' | '\t' | '\n' | '\u{0b}' | '\u{0c}' | '\r'));
    let (neg, mut rest) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let mut prefixed = false;
    if rest.len() >= 2 && (rest.starts_with("0x") || rest.starts_with("0X")) {
        rest = &rest[2..];
        prefixed = true;
    }
    let bytes = rest.as_bytes();
    if bytes.is_empty() {
        return err();
    }
    let mut value: i64 = 0;
    let mut prev_digit = prefixed; // "0x_f" is allowed: an underscore may follow the prefix
    let mut any = false;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'_' {
            if !prev_digit || i + 1 == bytes.len() {
                return err();
            }
            prev_digit = false;
            continue;
        }
        let Some(d) = char::from(b).to_digit(16) else {
            return err();
        };
        value = match value.checked_mul(16).and_then(|v| v.checked_add(i64::from(d))) {
            Some(v) => v,
            None => return err(),
        };
        prev_digit = true;
        any = true;
    }
    if !any {
        return err();
    }
    Ok(if neg { -value } else { value })
}

/// `f"{n:02x}"` for any int, negative ones included (`-5` → `"-5"`).
fn py_hex02(n: i128) -> String {
    if n < 0 {
        format!("-{:x}", n.unsigned_abs())
    } else {
        format!("{n:02x}")
    }
}

/// `f"{round(x):02x}"`: Python's `round()` gives an unbounded int, so a huge
/// `x` still formats (as a long hex string), while NaN and infinity raise.
fn py_round_hex02(x: f64) -> Result<String, String> {
    if x.is_nan() {
        return Err("ValueError: cannot convert float NaN to integer".into());
    }
    if x.is_infinite() {
        return Err("OverflowError: cannot convert float infinity to integer".into());
    }
    let r = crate::util::py_round(x);
    if r.abs() < 9.2e18 {
        return Ok(py_hex02(r as i128));
    }
    // Past 2**63 the float is an exact integer, mantissa * 2**exp with
    // exp >= 11: its hex digits are the mantissa's (shifted by exp % 4)
    // followed by exp / 4 zeros.
    let bits = r.abs().to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i64 - 1075;
    let mant = (bits & ((1u64 << 52) - 1)) | (1u64 << 52);
    let digits = format!("{:x}", mant << (exp % 4)) + &"0".repeat((exp / 4) as usize);
    Ok(if r < 0.0 { format!("-{digits}") } else { digits })
}

/// `s[i:j]` in code points.
fn char_slice(s: &[char], i: usize, j: usize) -> String {
    let j = j.min(s.len());
    if i >= j {
        return String::new();
    }
    s[i..j].iter().collect()
}

// --- Reading the theme -------------------------------------------------------

fn theme_dir_at(current: &Path) -> Option<PathBuf> {
    if current.exists() {
        return current.canonicalize().ok();
    }
    None
}

/// The wallpaper Omarchy is currently showing, if there is one.
///
/// Used as the hero on the Featured page: it is already on disk, it is
/// already the thing the user is looking at behind this window, and it
/// changes with the theme for free.
pub fn background() -> Option<PathBuf> {
    background_at(&current())
}

fn background_at(current: &Path) -> Option<PathBuf> {
    let link = current.parent().unwrap_or(Path::new("")).join("background");
    if link.exists() {
        let target = link.canonicalize().ok()?;
        return if target.is_file() { Some(target) } else { None };
    }
    None
}

fn colors_path_at(current: &Path) -> Option<PathBuf> {
    let p = theme_dir_at(current).map(|d| d.join(COLORS));
    p.filter(|p| p.is_file())
}

/// The theme's named colours, or {} when there is no theme to read.
///
/// Values are validated as hex: a malformed colours file should leave the
/// app looking like stock Adwaita, not inject broken CSS into every widget.
///
/// Port note: the Python lets a RecursionError out of here (a colors.toml
/// nested past what tomllib's recursive parser can follow; it is not an
/// OSError or ValueError); this infallible version gives {} for it, as for
/// any unreadable file. [`try_load_palette`] raises as the Python does.
/// The keys come back sorted, where the Python's dict keeps the file's
/// order; every caller only looks names up, so the order is never seen.
pub fn load_palette() -> BTreeMap<String, String> {
    try_load_palette().unwrap_or_default()
}

/// [`load_palette`] exactly as the Python behaves, RecursionError included.
pub fn try_load_palette() -> Result<BTreeMap<String, String>, String> {
    try_load_palette_at(&current(), CALLER_FRAMES)
}

/// Python frames on the stack under a `load_palette()` call. RecursionError
/// depends on how deep the caller already is; this is the parity runner's
/// depth (module, `main`, `run_module`, the case), and the app calls it from
/// about as deep.
const CALLER_FRAMES: usize = 4;

/// `load_palette()` called with `frames` Python frames under it.
fn try_load_palette_at(current: &Path, frames: usize) -> Result<BTreeMap<String, String>, String> {
    let Some(path) = colors_path_at(current) else {
        return Ok(BTreeMap::new());
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return Ok(BTreeMap::new());
    };
    // tomllib decodes UTF-8 strictly; a decode error is a ValueError too.
    let Ok(text) = String::from_utf8(bytes) else {
        return Ok(BTreeMap::new());
    };
    // load_palette's own frame and tomllib.load's are under loads().
    let raw = match tomllib::loads(&text, frames + 2) {
        Ok(raw) => raw,
        Err(tomllib::Error::Value) => return Ok(BTreeMap::new()),
        Err(tomllib::Error::Recursion) => return Err("RecursionError: maximum recursion depth exceeded".into()),
        Err(tomllib::Error::Index) => return Err("IndexError: list index out of range".into()),
    };

    let mut out = BTreeMap::new();
    for (key, value) in raw.items {
        if let tomllib::Value::Str(value) = value {
            if key == "mode" {
                out.insert("mode".to_string(), py_lower(py_strip(&value)));
            } else if hex_match(py_strip(&value)) {
                out.insert(key, py_strip(&value).to_string());
            }
        }
    }
    Ok(out)
}

/// A port of CPython 3.14's `tomllib` (`Lib/tomllib/_parser.py` and
/// `_re.py`), reduced to what `load_palette` needs: whether a document
/// parses, and its top-level strings in order. Ported rather than borrowed
/// from a TOML crate because the edges are tomllib's own: Python's unbounded
/// ints capped at 4300 decimal digits, `datetime`'s range, and a
/// RecursionError wherever the recursive-descent parser runs past Python's
/// 1000 frames (arrays nest two frames a level, inline tables three; a key
/// may have at most 1000 parts).
///
/// Every function keeps its Python name and takes `d`, the depth of its own
/// Python frame; `check(d)` is Python's recursion check on entering it. A
/// call to one of Python's own helpers (`Flags.is_`, `NestedDict()`,
/// `match_to_number`…) is a frame too and is checked where it is made.
/// Positions are byte offsets where the Python's are code points; they
/// only ever move by whole characters, so the two walk the same text.
mod tomllib {
    use std::collections::{HashMap, HashSet};
    use std::sync::{Mutex, OnceLock};

    /// `sys.getrecursionlimit()`: at most this many Python frames.
    const RECURSION_LIMIT: usize = 1000;
    /// `MAX_KEY_PARTS = sys.getrecursionlimit()`.
    const MAX_KEY_PARTS: usize = RECURSION_LIMIT;
    /// `sys.get_int_max_str_digits()`: `int()` refuses a longer decimal.
    const MAX_STR_DIGITS: usize = 4300;

    /// What `tomllib.loads` raises.
    #[derive(Debug, PartialEq)]
    pub enum Error {
        /// TOMLDecodeError, or the ValueError `int()` raises past 4300
        /// digits: both a ValueError, which `load_palette` catches.
        Value,
        /// RecursionError, which it does not.
        Recursion,
        /// IndexError from `cont[-1]` on an empty list. Not reachable (an
        /// empty list is always an inline array, frozen before it could be
        /// opened as a table); kept so the port has no panic.
        Index,
    }
    type R<T> = Result<T, Error>;

    /// A parsed value. Only strings are looked at; containers are kept
    /// because dotted keys and table headers reach into them.
    pub enum Value {
        Str(String),
        Other,
        Array(Vec<Value>),
        Table(Table),
    }

    /// A Python dict: insertion order, keyed lookups.
    #[derive(Default)]
    pub struct Table {
        pub items: Vec<(String, Value)>,
        index: HashMap<String, usize>,
    }

    impl Table {
        fn contains(&self, k: &str) -> bool {
            self.index.contains_key(k)
        }
        fn get_mut(&mut self, k: &str) -> Option<&mut Value> {
            let i = *self.index.get(k)?;
            Some(&mut self.items[i].1)
        }
        /// `d[k] = v`.
        fn insert(&mut self, k: &str, v: Value) {
            match self.index.get(k) {
                Some(&i) => self.items[i].1 = v,
                None => {
                    self.index.insert(k.to_string(), self.items.len());
                    self.items.push((k.to_string(), v));
                }
            }
        }
    }

    fn check(d: usize) -> R<()> {
        if d > RECURSION_LIMIT {
            Err(Error::Recursion)
        } else {
            Ok(())
        }
    }

    /// `raise TOMLDecodeError(...)` from a frame at depth `d`: its
    /// `__init__` is a Python frame, so at the very limit even building the
    /// error is a RecursionError.
    fn decode_err<T>(d: usize) -> R<T> {
        check(d + 1)?;
        Err(Error::Value)
    }

    // --- character sets ---

    fn ascii_ctrl(c: char) -> bool {
        (c as u32) < 32 || c == '\u{7f}'
    }
    fn illegal_basic_str(c: char) -> bool {
        ascii_ctrl(c) && c != '\t'
    }
    fn illegal_multiline_str(c: char) -> bool {
        ascii_ctrl(c) && c != '\t' && c != '\n'
    }
    fn toml_ws(c: char) -> bool {
        c == ' ' || c == '\t'
    }
    fn toml_ws_and_newline(c: char) -> bool {
        toml_ws(c) || c == '\n'
    }
    fn bare_key(c: char) -> bool {
        c.is_ascii_alphanumeric() || c == '-' || c == '_'
    }
    fn key_initial(c: char) -> bool {
        bare_key(c) || c == '"' || c == '\''
    }

    fn char_at(src: &str, pos: usize) -> Option<char> {
        src.get(pos..).and_then(|t| t.chars().next())
    }
    /// `src[pos:]`.
    fn tail(src: &str, pos: usize) -> &str {
        src.get(pos..).unwrap_or("")
    }

    // --- Flags and NestedDict ---

    type Key = Vec<String>;
    const FROZEN: u8 = 1;
    const EXPLICIT_NEST: u8 = 2;

    #[derive(Default)]
    struct FlagNode {
        flags: u8,
        recursive_flags: u8,
        nested: HashMap<String, FlagNode>,
    }

    /// Flags that map to parsed keys/namespaces.
    #[derive(Default)]
    struct Flags {
        flags: HashMap<String, FlagNode>,
        pending: HashSet<(Key, u8)>,
    }

    impl Flags {
        fn add_pending(&mut self, key: &[String], flag: u8) {
            self.pending.insert((key.to_vec(), flag));
        }
        fn finalize_pending(&mut self) {
            for (key, flag) in std::mem::take(&mut self.pending) {
                self.set(&key, flag, false);
            }
        }
        fn unset_all(&mut self, key: &[String]) {
            let Some((stem, parent)) = key.split_last() else { return };
            let mut cont = &mut self.flags;
            for k in parent {
                match cont.get_mut(k) {
                    Some(n) => cont = &mut n.nested,
                    None => return,
                }
            }
            cont.remove(stem);
        }
        fn set(&mut self, key: &[String], flag: u8, recursive: bool) {
            let Some((stem, parent)) = key.split_last() else { return };
            let mut cont = &mut self.flags;
            for k in parent {
                cont = &mut cont.entry(k.clone()).or_default().nested;
            }
            let node = cont.entry(stem.clone()).or_default();
            if recursive {
                node.recursive_flags |= flag;
            } else {
                node.flags |= flag;
            }
        }
        fn is_(&self, key: &[String], flag: u8) -> bool {
            let Some((stem, parent)) = key.split_last() else {
                return false; // document root has no flags
            };
            let mut cont = &self.flags;
            for k in parent {
                let Some(inner) = cont.get(k) else { return false };
                if inner.recursive_flags & flag != 0 {
                    return true;
                }
                cont = &inner.nested;
            }
            cont.get(stem).is_some_and(|n| (n.flags | n.recursive_flags) & flag != 0)
        }
    }

    /// Why `get_or_create_nest` gave up: KeyError, or IndexError.
    enum NestErr {
        Key,
        Index,
    }

    /// `NestedDict.get_or_create_nest`.
    fn get_or_create_nest<'a>(mut cont: &'a mut Table, key: &[String], access_lists: bool) -> Result<&'a mut Table, NestErr> {
        for k in key {
            if !cont.contains(k) {
                cont.insert(k, Value::Table(Table::default()));
            }
            let Some(v) = cont.get_mut(k) else { return Err(NestErr::Key) };
            let v = match (access_lists, v) {
                (true, Value::Array(a)) => a.last_mut().ok_or(NestErr::Index)?,
                (_, v) => v,
            };
            match v {
                Value::Table(t) => cont = t,
                _ => return Err(NestErr::Key),
            }
        }
        Ok(cont)
    }

    /// `NestedDict.append_nest_to_list`.
    fn append_nest_to_list(data: &mut Table, key: &[String]) -> Result<(), NestErr> {
        let Some((last, parent)) = key.split_last() else { return Err(NestErr::Index) };
        let cont = get_or_create_nest(data, parent, true)?;
        match cont.get_mut(last) {
            Some(Value::Array(list)) => list.push(Value::Table(Table::default())),
            Some(_) => return Err(NestErr::Key),
            None => cont.insert(last, Value::Array(vec![Value::Table(Table::default())])),
        }
        Ok(())
    }

    /// A NestErr as the Python's `except KeyError: raise TOMLDecodeError`
    /// sees it (an IndexError goes past that handler).
    fn nest_err<T>(e: NestErr, d: usize) -> R<T> {
        match e {
            NestErr::Key => decode_err(d),
            NestErr::Index => Err(Error::Index),
        }
    }

    struct Output {
        data: Table,
        flags: Flags,
    }

    // --- the parser ---

    /// `tomllib.loads(s)` called from a frame at depth `caller` (the
    /// `tomllib.load` frame), so `loads` itself is at `caller + 1`.
    ///
    /// Run on a thread of its own with a large stack: the recursion goes as
    /// deep as Python's 1000 frames allow, more than a 2 MB thread (or an
    /// unoptimised build) has room for.
    pub fn loads(s: &str, caller: usize) -> R<Table> {
        std::thread::scope(|scope| {
            match std::thread::Builder::new().stack_size(64 << 20).spawn_scoped(scope, || loads_here(s, caller)) {
                Ok(h) => h.join().unwrap_or(Err(Error::Recursion)),
                Err(_) => loads_here(s, caller),
            }
        })
    }

    fn loads_here(s: &str, caller: usize) -> R<Table> {
        let d = caller + 1;
        check(d)?;
        // The spec allows converting "\r\n" to "\n", even in string
        // literals. Let's do so to simplify parsing.
        let src = s.replace("\r\n", "\n");
        let src = src.as_str();
        // Output() and, inside it, NestedDict() and Flags(); then
        // make_safe_parse_float().
        check(d + 2)?;
        let mut out = Output { data: Table::default(), flags: Flags::default() };
        let mut header: Key = Vec::new();
        let mut pos = 0;

        // Parse one statement at a time
        // (typically means one line in TOML source)
        loop {
            // 1. Skip line leading whitespace
            pos = skip_chars(src, pos, toml_ws, d + 1)?;

            // 2. Parse rules. Expect one of the following:
            //    - end of file
            //    - end of line
            //    - comment
            //    - key/value pair
            //    - append dict to list (and move to its namespace)
            //    - create dict (and move to its namespace)
            // Skip trailing whitespace when applicable.
            let Some(char) = char_at(src, pos) else { break };
            if char == '\n' {
                pos += 1;
                continue;
            }
            if key_initial(char) {
                pos = key_value_rule(src, pos, &mut out, &header, d + 1)?;
                pos = skip_chars(src, pos, toml_ws, d + 1)?;
            } else if char == '[' {
                let second_char = char_at(src, pos + 1);
                // finalize_pending(), and Flags.set() under it.
                check(d + 1)?;
                if !out.flags.pending.is_empty() {
                    check(d + 2)?;
                }
                out.flags.finalize_pending();
                (pos, header) = if second_char == Some('[') {
                    create_list_rule(src, pos, &mut out, d + 1)?
                } else {
                    create_dict_rule(src, pos, &mut out, d + 1)?
                };
                pos = skip_chars(src, pos, toml_ws, d + 1)?;
            } else if char != '#' {
                return decode_err(d); // Invalid statement
            }

            // 3. Skip comment
            pos = skip_comment(src, pos, d + 1)?;

            // 4. Expect end of line or end of file
            let Some(char) = char_at(src, pos) else { break };
            if char != '\n' {
                return decode_err(d); // Expected newline or end of document after a statement
            }
            pos += 1;
        }
        Ok(out.data)
    }

    fn skip_chars(src: &str, mut pos: usize, chars: fn(char) -> bool, d: usize) -> R<usize> {
        check(d)?;
        while let Some(c) = char_at(src, pos) {
            if !chars(c) {
                break;
            }
            pos += c.len_utf8();
        }
        Ok(pos)
    }

    fn skip_until(src: &str, pos: usize, expect: &str, error_on: fn(char) -> bool, error_on_eof: bool, d: usize) -> R<usize> {
        check(d)?;
        let new_pos = match tail(src, pos).find(expect) {
            Some(i) => pos + i,
            None => {
                if error_on_eof {
                    return decode_err(d); // Expected {expect!r}
                }
                src.len()
            }
        };
        if src.get(pos..new_pos).unwrap_or("").chars().any(error_on) {
            return decode_err(d); // Found invalid character
        }
        Ok(new_pos)
    }

    fn skip_comment(src: &str, pos: usize, d: usize) -> R<usize> {
        check(d)?;
        if char_at(src, pos) == Some('#') {
            return skip_until(src, pos + 1, "\n", illegal_basic_str, false, d + 1);
        }
        Ok(pos)
    }

    fn skip_comments_and_array_ws(src: &str, mut pos: usize, d: usize) -> R<usize> {
        check(d)?;
        loop {
            let pos_before_skip = pos;
            pos = skip_chars(src, pos, toml_ws_and_newline, d + 1)?;
            pos = skip_comment(src, pos, d + 1)?;
            if pos == pos_before_skip {
                return Ok(pos);
            }
        }
    }

    fn create_dict_rule(src: &str, mut pos: usize, out: &mut Output, d: usize) -> R<(usize, Key)> {
        check(d)?;
        pos += 1; // Skip "["
        pos = skip_chars(src, pos, toml_ws, d + 1)?;
        let (mut pos, key) = parse_key(src, pos, d + 1)?;

        // Flags.is_, Flags.set and get_or_create_nest are frames at d + 1,
        // as deep as parse_key, which got there.
        if out.flags.is_(&key, EXPLICIT_NEST) || out.flags.is_(&key, FROZEN) {
            return decode_err(d); // Cannot declare {key} twice
        }
        out.flags.set(&key, EXPLICIT_NEST, false);
        if let Err(e) = get_or_create_nest(&mut out.data, &key, true) {
            return nest_err(e, d); // Cannot overwrite a value
        }

        if !tail(src, pos).starts_with(']') {
            return decode_err(d); // Expected ']' at the end of a table declaration
        }
        pos += 1;
        Ok((pos, key))
    }

    fn create_list_rule(src: &str, mut pos: usize, out: &mut Output, d: usize) -> R<(usize, Key)> {
        check(d)?;
        pos += 2; // Skip "[["
        pos = skip_chars(src, pos, toml_ws, d + 1)?;
        let (mut pos, key) = parse_key(src, pos, d + 1)?;

        if out.flags.is_(&key, FROZEN) {
            return decode_err(d); // Cannot mutate immutable namespace {key}
        }
        // Free the namespace now that it points to another empty list item...
        out.flags.unset_all(&key);
        // ...but this key precisely is still prohibited from table declaration
        out.flags.set(&key, EXPLICIT_NEST, false);
        // append_nest_to_list, and get_or_create_nest under it.
        check(d + 2)?;
        if let Err(e) = append_nest_to_list(&mut out.data, &key) {
            return nest_err(e, d); // Cannot overwrite a value
        }

        if !tail(src, pos).starts_with("]]") {
            return decode_err(d); // Expected ']]' at the end of an array declaration
        }
        pos += 2;
        Ok((pos, key))
    }

    fn key_value_rule(src: &str, pos: usize, out: &mut Output, header: &[String], d: usize) -> R<usize> {
        check(d)?;
        let (pos, key, value) = parse_key_value_pair(src, pos, d + 1)?;
        let Some((key_stem, key_parent)) = key.split_last() else { return Err(Error::Index) };
        let abs_key_parent: Key = header.iter().chain(key_parent).cloned().collect();

        // (The generator, Flags.is_ and the rest are frames at d + 1, which
        // parse_key_value_pair has already reached.)
        for i in 1..key.len() {
            let cont_key: Key = header.iter().chain(&key[..i]).cloned().collect();
            // Check that dotted key syntax does not redefine an existing table
            if out.flags.is_(&cont_key, EXPLICIT_NEST) {
                return decode_err(d); // Cannot redefine namespace {cont_key}
            }
            // Containers in the relative path can't be opened with the table syntax or
            // dotted key/value syntax in following table sections.
            out.flags.add_pending(&cont_key, EXPLICIT_NEST);
        }

        if out.flags.is_(&abs_key_parent, FROZEN) {
            return decode_err(d); // Cannot mutate immutable namespace {abs_key_parent}
        }

        let nest = match get_or_create_nest(&mut out.data, &abs_key_parent, true) {
            Ok(n) => n,
            Err(e) => return nest_err(e, d), // Cannot overwrite a value
        };
        if nest.contains(key_stem) {
            return decode_err(d); // Cannot overwrite a value
        }
        let container = matches!(value, Value::Table(_) | Value::Array(_));
        nest.insert(key_stem, value);
        // Mark inline table and array namespaces recursively immutable
        if container {
            let full: Key = header.iter().chain(&key).cloned().collect();
            out.flags.set(&full, FROZEN, true);
        }
        Ok(pos)
    }

    fn parse_key_value_pair(src: &str, pos: usize, d: usize) -> R<(usize, Key, Value)> {
        check(d)?;
        let (mut pos, key) = parse_key(src, pos, d + 1)?;
        if char_at(src, pos) != Some('=') {
            return decode_err(d); // Expected '=' after a key in a key/value pair
        }
        pos += 1;
        pos = skip_chars(src, pos, toml_ws, d + 1)?;
        let (pos, value) = parse_value(src, pos, d + 1)?;
        Ok((pos, key, value))
    }

    fn parse_key(src: &str, pos: usize, d: usize) -> R<(usize, Key)> {
        check(d)?;
        let (mut pos, key_part) = parse_key_part(src, pos, d + 1)?;
        let mut key: Key = vec![key_part];
        pos = skip_chars(src, pos, toml_ws, d + 1)?;
        loop {
            if char_at(src, pos) != Some('.') {
                return Ok((pos, key));
            }
            pos += 1;
            pos = skip_chars(src, pos, toml_ws, d + 1)?;
            let (p, key_part) = parse_key_part(src, pos, d + 1)?;
            pos = p;
            key.push(key_part);
            if key.len() > MAX_KEY_PARTS {
                // TOML key has more than the allowed {MAX_KEY_PARTS} parts
                return Err(Error::Recursion);
            }
            pos = skip_chars(src, pos, toml_ws, d + 1)?;
        }
    }

    fn parse_key_part(src: &str, pos: usize, d: usize) -> R<(usize, String)> {
        check(d)?;
        match char_at(src, pos) {
            Some(c) if bare_key(c) => {
                let start_pos = pos;
                let pos = skip_chars(src, pos, bare_key, d + 1)?;
                Ok((pos, src[start_pos..pos].to_string()))
            }
            Some('\'') => parse_literal_str(src, pos, d + 1),
            Some('"') => parse_one_line_basic_str(src, pos, d + 1),
            _ => decode_err(d), // Invalid initial character for a key part
        }
    }

    fn parse_one_line_basic_str(src: &str, pos: usize, d: usize) -> R<(usize, String)> {
        check(d)?;
        parse_basic_str(src, pos + 1, false, d + 1)
    }

    fn parse_array(src: &str, mut pos: usize, d: usize) -> R<(usize, Vec<Value>)> {
        check(d)?;
        pos += 1;
        let mut array = Vec::new();

        pos = skip_comments_and_array_ws(src, pos, d + 1)?;
        if tail(src, pos).starts_with(']') {
            return Ok((pos + 1, array));
        }
        loop {
            let (p, val) = parse_value(src, pos, d + 1)?;
            array.push(val);
            pos = skip_comments_and_array_ws(src, p, d + 1)?;

            match char_at(src, pos) {
                Some(']') => return Ok((pos + 1, array)),
                Some(',') => {}
                _ => return decode_err(d), // Unclosed array
            }
            pos += 1;

            pos = skip_comments_and_array_ws(src, pos, d + 1)?;
            if tail(src, pos).starts_with(']') {
                return Ok((pos + 1, array));
            }
        }
    }

    fn parse_inline_table(src: &str, mut pos: usize, d: usize) -> R<(usize, Table)> {
        check(d)?;
        pos += 1;
        // NestedDict() and Flags(): their __init__ frames.
        check(d + 1)?;
        let mut nested_dict = Table::default();
        let mut flags = Flags::default();

        pos = skip_chars(src, pos, toml_ws, d + 1)?;
        if tail(src, pos).starts_with('}') {
            return Ok((pos + 1, nested_dict));
        }
        loop {
            let (p, key, value) = parse_key_value_pair(src, pos, d + 1)?;
            pos = p;
            let Some((key_stem, key_parent)) = key.split_last() else { return Err(Error::Index) };
            if flags.is_(&key, FROZEN) {
                return decode_err(d); // Cannot mutate immutable namespace {key}
            }
            let nest = match get_or_create_nest(&mut nested_dict, key_parent, false) {
                Ok(n) => n,
                Err(e) => return nest_err(e, d), // Cannot overwrite a value
            };
            if nest.contains(key_stem) {
                return decode_err(d); // Duplicate inline table key {key_stem!r}
            }
            let container = matches!(value, Value::Table(_) | Value::Array(_));
            nest.insert(key_stem, value);
            pos = skip_chars(src, pos, toml_ws, d + 1)?;
            match char_at(src, pos) {
                Some('}') => return Ok((pos + 1, nested_dict)),
                Some(',') => {}
                _ => return decode_err(d), // Unclosed inline table
            }
            if container {
                flags.set(&key, FROZEN, true);
            }
            pos += 1;
            pos = skip_chars(src, pos, toml_ws, d + 1)?;
        }
    }

    fn parse_basic_str_escape(src: &str, pos: usize, multiline: bool, d: usize) -> R<(usize, String)> {
        check(d)?;
        // escape_id = src[pos : pos + 2]; pos += 2
        let second = char_at(src, pos + 1);
        let mut pos = pos + 1 + second.map_or(1, char::len_utf8);
        if multiline && matches!(second, Some(' ' | '\t' | '\n')) {
            // Skip whitespace until next non-whitespace character or end of
            // the doc. Error if non-whitespace is found before newline.
            if second != Some('\n') {
                pos = skip_chars(src, pos, toml_ws, d + 1)?;
                match char_at(src, pos) {
                    None => return Ok((pos, String::new())),
                    Some('\n') => {}
                    Some(_) => return decode_err(d), // Unescaped '\' in a string
                }
                pos += 1;
            }
            pos = skip_chars(src, pos, toml_ws_and_newline, d + 1)?;
            return Ok((pos, String::new()));
        }
        let out = match second {
            Some('u') => return parse_hex_char(src, pos, 4, d + 1),
            Some('U') => return parse_hex_char(src, pos, 8, d + 1),
            Some('b') => "\u{8}",
            Some('t') => "\t",
            Some('n') => "\n",
            Some('f') => "\u{c}",
            Some('r') => "\r",
            Some('"') => "\"",
            Some('\\') => "\\",
            _ => return decode_err(d), // Unescaped '\' in a string
        };
        Ok((pos, out.to_string()))
    }

    fn parse_basic_str_escape_multiline(src: &str, pos: usize, d: usize) -> R<(usize, String)> {
        check(d)?;
        parse_basic_str_escape(src, pos, true, d + 1)
    }

    fn parse_hex_char(src: &str, pos: usize, hex_len: usize, d: usize) -> R<(usize, String)> {
        check(d)?;
        let hex_str: String = tail(src, pos).chars().take(hex_len).collect();
        if hex_str.chars().count() != hex_len || !hex_str.bytes().all(|b| b.is_ascii_hexdigit()) {
            return decode_err(d); // Invalid hex value
        }
        let pos = pos + hex_len;
        let hex_int = u32::from_str_radix(&hex_str, 16).map_err(|_| Error::Value)?;
        // is_unicode_scalar_value()
        check(d + 1)?;
        match char::from_u32(hex_int) {
            Some(c) => Ok((pos, c.to_string())),
            None => decode_err(d), // Escaped character is not a Unicode scalar value
        }
    }

    fn parse_literal_str(src: &str, mut pos: usize, d: usize) -> R<(usize, String)> {
        check(d)?;
        pos += 1; // Skip starting apostrophe
        let start_pos = pos;
        let pos = skip_until(src, pos, "'", illegal_basic_str, true, d + 1)?;
        Ok((pos + 1, src[start_pos..pos].to_string())) // Skip ending apostrophe
    }

    fn parse_multiline_str(src: &str, mut pos: usize, literal: bool, d: usize) -> R<(usize, String)> {
        check(d)?;
        pos += 3;
        if tail(src, pos).starts_with('\n') {
            pos += 1;
        }

        let delim;
        let mut result;
        if literal {
            delim = '\'';
            let end_pos = skip_until(src, pos, "'''", illegal_multiline_str, true, d + 1)?;
            result = src[pos..end_pos].to_string();
            pos = end_pos + 3;
        } else {
            delim = '"';
            (pos, result) = parse_basic_str(src, pos, true, d + 1)?;
        }

        // Add at maximum two extra apostrophes/quotes if the end sequence
        // is 4 or 5 chars long instead of just 3.
        if !tail(src, pos).starts_with(delim) {
            return Ok((pos, result));
        }
        pos += 1;
        result.push(delim);
        if !tail(src, pos).starts_with(delim) {
            return Ok((pos, result));
        }
        pos += 1;
        result.push(delim);
        Ok((pos, result))
    }

    fn parse_basic_str(src: &str, mut pos: usize, multiline: bool, d: usize) -> R<(usize, String)> {
        check(d)?;
        let error_on = if multiline { illegal_multiline_str } else { illegal_basic_str };
        let mut result = String::new();
        let mut start_pos = pos;
        loop {
            let Some(char) = char_at(src, pos) else {
                return decode_err(d); // Unterminated string
            };
            if char == '"' {
                if !multiline {
                    result.push_str(&src[start_pos..pos]);
                    return Ok((pos + 1, result));
                }
                if tail(src, pos).starts_with("\"\"\"") {
                    result.push_str(&src[start_pos..pos]);
                    return Ok((pos + 3, result));
                }
                pos += 1;
                continue;
            }
            if char == '\\' {
                result.push_str(&src[start_pos..pos]);
                let (p, parsed_escape) = if multiline {
                    parse_basic_str_escape_multiline(src, pos, d + 1)?
                } else {
                    parse_basic_str_escape(src, pos, false, d + 1)?
                };
                pos = p;
                result.push_str(&parsed_escape);
                start_pos = pos;
                continue;
            }
            if error_on(char) {
                return decode_err(d); // Illegal character {char!r}
            }
            pos += char.len_utf8();
        }
    }

    fn parse_value(src: &str, pos: usize, d: usize) -> R<(usize, Value)> {
        check(d)?;
        let char = char_at(src, pos);
        let rest = tail(src, pos);

        // IMPORTANT: order conditions based on speed of checking and likelihood

        // Basic strings
        if char == Some('"') {
            let (pos, s) = if rest.starts_with("\"\"\"") {
                parse_multiline_str(src, pos, false, d + 1)?
            } else {
                parse_one_line_basic_str(src, pos, d + 1)?
            };
            return Ok((pos, Value::Str(s)));
        }

        // Literal strings
        if char == Some('\'') {
            let (pos, s) = if rest.starts_with("'''") {
                parse_multiline_str(src, pos, true, d + 1)?
            } else {
                parse_literal_str(src, pos, d + 1)?
            };
            return Ok((pos, Value::Str(s)));
        }

        // Booleans
        if rest.starts_with("true") {
            return Ok((pos + 4, Value::Other));
        }
        if rest.starts_with("false") {
            return Ok((pos + 5, Value::Other));
        }

        // Arrays
        if char == Some('[') {
            let (pos, a) = parse_array(src, pos, d + 1)?;
            return Ok((pos, Value::Array(a)));
        }

        // Inline tables
        if char == Some('{') {
            let (pos, t) = parse_inline_table(src, pos, d + 1)?;
            return Ok((pos, Value::Table(t)));
        }

        // Dates and times
        if let Some(m) = re_datetime().captures(rest) {
            // match_to_datetime()
            check(d + 1)?;
            let num = |i: usize| m.get(i).map_or(0, |g| g.as_str().parse::<u32>().unwrap_or(0));
            if let Some(sign) = m.get(9) {
                // cached_tz() is an lru_cache: a Python frame only the first
                // time this process sees the offset.
                let tz = (num(10), num(11), sign.as_str() == "+");
                let seen = TZ_CACHE.get_or_init(Default::default);
                let miss = !seen.lock().map(|s| s.contains(&tz)).unwrap_or(true);
                if miss {
                    check(d + 2)?;
                    if let Ok(mut s) = seen.lock() {
                        s.insert(tz);
                    }
                }
            }
            if !valid_date(num(1), num(2), num(3)) {
                return decode_err(d); // Invalid date or datetime
            }
            return Ok((pos + m.get(0).map_or(0, |g| g.end()), Value::Other));
        }
        if let Some(m) = re_localtime().find(rest) {
            // match_to_localtime()
            check(d + 1)?;
            return Ok((pos + m.end(), Value::Other));
        }

        // Integers and "normal" floats.
        // The regex will greedily match any type starting with a decimal
        // char, so needs to be located after handling of dates and times.
        if let Some(m) = re_number().captures(rest) {
            // match_to_number()
            check(d + 1)?;
            let text = m.get(0).map_or("", |g| g.as_str());
            let float = m.name("floatpart").is_some_and(|g| !g.as_str().is_empty());
            if !float && !text.starts_with("0x") && !text.starts_with("0o") && !text.starts_with("0b") {
                // int(s, 0) refuses a decimal of more than 4300 digits
                // (underscores and sign not counted): a ValueError.
                if text.bytes().filter(u8::is_ascii_digit).count() > MAX_STR_DIGITS {
                    return Err(Error::Value);
                }
            }
            return Ok((pos + text.len(), Value::Other));
        }

        // Special floats
        if rest.starts_with("inf") || rest.starts_with("nan") {
            return Ok((pos + 3, Value::Other));
        }
        if ["-inf", "+inf", "-nan", "+nan"].iter().any(|s| rest.starts_with(s)) {
            return Ok((pos + 4, Value::Other));
        }

        decode_err(d) // Invalid value
    }

    /// The offsets `cached_tz` has been called with in this process.
    static TZ_CACHE: OnceLock<Mutex<HashSet<(u32, u32, bool)>>> = OnceLock::new();

    /// Whether Python's `date(year, month, day)` accepts it (month and day
    /// are already in 1..=12 and 1..=31 by the pattern).
    fn valid_date(year: u32, month: u32, day: u32) -> bool {
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let days = match month {
            2 if leap => 29,
            2 => 28,
            4 | 6 | 9 | 11 => 30,
            _ => 31,
        };
        (1..=9999).contains(&year) && (1..=12).contains(&month) && (1..=days).contains(&day)
    }

    // _TIME_RE_STR: e.g. 00:32:00.999999 or 00:32:00
    const TIME_RE: &str = r"([01][0-9]|2[0-3]):([0-5][0-9]):([0-5][0-9])(?:\.([0-9]{1,6})[0-9]*)?";

    fn re_number() -> &'static regex::Regex {
        static RE: OnceLock<regex::Regex> = OnceLock::new();
        RE.get_or_init(|| {
            regex::Regex::new(concat!(
                r"^(?:0(?:x[0-9A-Fa-f](?:_?[0-9A-Fa-f])*|b[01](?:_?[01])*|o[0-7](?:_?[0-7])*)",
                r"|[+-]?(?:0|[1-9](?:_?[0-9])*)",
                r"(?P<floatpart>(?:\.[0-9](?:_?[0-9])*)?(?:[eE][+-]?[0-9](?:_?[0-9])*)?))"
            ))
            .expect("static regex")
        })
    }

    fn re_localtime() -> &'static regex::Regex {
        static RE: OnceLock<regex::Regex> = OnceLock::new();
        RE.get_or_init(|| regex::Regex::new(&format!("^{TIME_RE}")).expect("static regex"))
    }

    fn re_datetime() -> &'static regex::Regex {
        static RE: OnceLock<regex::Regex> = OnceLock::new();
        RE.get_or_init(|| {
            regex::Regex::new(&format!(
                r"^([0-9]{{4}})-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01])(?:[Tt ]{TIME_RE}(?:([Zz])|([+-])([01][0-9]|2[0-3]):([0-5][0-9]))?)?"
            ))
            .expect("static regex")
        })
    }
}

// --- Measuring colours -------------------------------------------------------

/// WCAG relative luminance of a hex colour.
///
/// (`_luminance` in the Python; the tests call it, so it is public here.)
pub fn luminance(value: &str) -> f64 {
    let mut h: Vec<char> = value.trim_start_matches('#').chars().collect();
    if h.len() == 3 {
        h = h.iter().flat_map(|&c| [c, c]).collect();
    }
    if h.len() != 6 {
        return 0.0;
    }
    let mut chan = [0.0f64; 3];
    for (k, i) in [0usize, 2, 4].into_iter().enumerate() {
        match py_int16(&char_slice(&h, i, i + 2)) {
            Ok(v) => chan[k] = v as f64 / 255.0,
            Err(_) => return 0.0,
        }
    }
    let conv: Vec<f64> = chan
        .iter()
        .map(|&c| if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) })
        .collect();
    0.2126 * conv[0] + 0.7152 * conv[1] + 0.0722 * conv[2]
}

pub fn contrast(a: &str, b: &str) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = (la.max(lb), la.min(lb));
    (hi + 0.05) / (lo + 0.05)
}

// Contrast targets. WCAG AA asks 4.5:1 of body text; secondary text here is
// often small (captions, pills), so it is held to more than the minimum.
/// Secondary and accent-coloured text on any surface.
pub const TEXT_TARGET: f64 = 5.5;
/// Primary text: WCAG AAA, the most legible tier.
pub const BODY_TARGET: f64 = 7.0;

/// `_hex6`: a hex colour normalised to lowercase `#rrggbb`, or "" when it
/// is not one. (Like the Python it checks only the length, not the digits.)
pub fn hex6(value: &str) -> String {
    let mut h: Vec<char> = value.trim_start_matches('#').chars().collect();
    if h.len() == 3 {
        h = h.iter().flat_map(|&c| [c, c]).collect();
    }
    if h.len() == 6 {
        let s: String = h.into_iter().collect();
        format!("#{}", py_lower(&s))
    } else {
        String::new()
    }
}

/// `a` moved a fraction `t` of the way to `b`.
pub fn mix(a: &str, b: &str, t: f64) -> String {
    try_mix(a, b, t).unwrap_or_default()
}

/// [`mix`] exactly as the Python behaves, ValueError included.
pub fn try_mix(a: &str, b: &str, t: f64) -> Result<String, String> {
    let (a, b) = (hex6(a), hex6(b));
    if a.is_empty() || b.is_empty() {
        return Ok(if a.is_empty() { b } else { a });
    }
    let (ac, bc): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut ca = Vec::with_capacity(3);
    for i in [1usize, 3, 5] {
        ca.push(py_int16(&char_slice(&ac, i, i + 2))?);
    }
    let mut cb = Vec::with_capacity(3);
    for i in [1usize, 3, 5] {
        cb.push(py_int16(&char_slice(&bc, i, i + 2))?);
    }
    let mut out = String::from("#");
    for (&x, &y) in ca.iter().zip(cb.iter()) {
        let (x, y) = (x as f64, y as f64);
        out.push_str(&py_round_hex02(x + (y - x) * t)?);
    }
    Ok(out)
}

/// `color`, moved toward `toward` only as far as it takes to reach
/// `target` contrast on every surface -- the theme's hue, kept wherever it
/// already reads.
pub fn legible(color: &str, toward: &str, surfaces: &[&str], target: f64) -> String {
    try_legible(color, toward, surfaces, target).unwrap_or_default()
}

/// [`legible`] exactly as the Python behaves, ValueError included.
pub fn try_legible(color: &str, toward: &str, surfaces: &[&str], target: f64) -> Result<String, String> {
    if color.is_empty() {
        return Ok(String::new());
    }
    let reaches = |c: &str| surfaces.iter().filter(|s| !s.is_empty()).all(|s| contrast(c, s) >= target);
    for step in 0..=50 {
        let c = try_mix(color, toward, f64::from(step) / 50.0)?;
        if reaches(&c) {
            return Ok(c);
        }
    }
    // The theme's own text is not enough (a light theme's foreground can
    // itself be ~5:1): go on toward black or white, whichever the surface
    // wants.
    let extreme = readable_on(surfaces.iter().copied().find(|s| !s.is_empty()).unwrap_or("#000000"));
    for step in 1..=50 {
        let c = try_mix(toward, &extreme, f64::from(step) / 50.0)?;
        if reaches(&c) {
            return Ok(c);
        }
    }
    Ok(extreme)
}

/// The faintest libadwaita can draw dimmed text (an opacity of the
/// foreground) while it still reaches `target` on every surface, in percent.
/// Never fainter than libadwaita's own 55%.
pub fn dim_opacity(fg: &str, surfaces: &[&str], target: f64) -> u32 {
    try_dim_opacity(fg, surfaces, target).unwrap_or(100)
}

/// [`dim_opacity`] exactly as the Python behaves, ValueError included.
pub fn try_dim_opacity(fg: &str, surfaces: &[&str], target: f64) -> Result<u32, String> {
    for pct in 55..=100u32 {
        let mut all = true;
        for s in surfaces.iter().filter(|s| !s.is_empty()) {
            if contrast(&try_mix(s, fg, f64::from(pct) / 100.0)?, s) < target {
                all = false;
                break;
            }
        }
        if all {
            return Ok(pct);
        }
    }
    Ok(100)
}

// Status pills are solid: the status colour as the fill, text measured against
// it. Tinted pills kept the colour in the text, and on a low-contrast palette
// that text could not be both coloured and readable (Everforest's red read at
// 3.6:1); solid fills carry the colour and leave the text free to be legible.
/// (CSS class, fill role) pairs.
pub const PILL_FILLS: &[(&str, &str)] = &[
    ("pill-crit", "error"),
    ("pill-warn", "warning"),
    ("pill-ok", "success"),
    ("pill-accent", "accent"),
    ("count-badge.is-warn", "warning"),
];

/// Solid pill rules: each fill with the text that reads best on it.
///
/// `prefer` are the theme's own dark and light text colours; one is used
/// when it reaches 7:1, so the pill still looks like the theme, and black or
/// white otherwise. (Python's default `prefer=()` is `&[]`.)
pub fn pill_css(fills: &BTreeMap<String, String>, prefer: &[&str]) -> String {
    let mut lines: Vec<String> = Vec::new();
    for (cls, role) in PILL_FILLS {
        let fill = hex6(fills.get(*role).map(String::as_str).unwrap_or(""));
        if fill.is_empty() {
            continue;
        }
        let own: Vec<&str> = prefer.iter().copied().filter(|c| !c.is_empty() && contrast(c, &fill) >= 7.0).collect();
        let text = if own.is_empty() {
            readable_on(&fill)
        } else {
            // max() keeps the first of equals.
            let mut best = own[0];
            for &c in &own[1..] {
                if contrast(c, &fill) > contrast(best, &fill) {
                    best = c;
                }
            }
            best.to_string()
        };
        lines.push(format!(".{cls} {{ background: {fill}; color: {text}; }}"));
    }
    lines.join("\n") + if lines.is_empty() { "" } else { "\n" }
}

// Today's picks sit on the card colour washed toward the accent. That wash
// lowers the contrast of everything on it, so the card's text is measured
// against the strongest point of it rather than borrowed from a plain card.
pub const PICK_WASH: f64 = 0.12;

/// Text colours for the picks' accent-washed card.
pub fn pick_css(card: &str, fg: &str, accent: &str) -> String {
    try_pick_css(card, fg, accent).unwrap_or_default()
}

/// [`pick_css`] exactly as the Python behaves, ValueError included.
pub fn try_pick_css(card: &str, fg: &str, accent: &str) -> Result<String, String> {
    let (card, fg, accent) = (hex6(card), hex6(fg), hex6(accent));
    if card.is_empty() || fg.is_empty() || accent.is_empty() {
        return Ok(String::new());
    }
    let wash = try_mix(&card, &accent, PICK_WASH)?;
    let text = try_legible(&fg, &readable_on(&wash), &[&wash], BODY_TARGET)?;
    let eyebrow = try_legible(&accent, &text, &[&wash], TEXT_TARGET)?;
    Ok(format!(".pick-card {{ color: {text}; }}\n.pick-eyebrow {{ color: {eyebrow}; }}\n"))
}

/// A colour with alpha (0-1 channels), as the opaque hex it shows as
/// over `under` (an opaque rgb, 0-1). Contrast is only defined for what is
/// actually on screen.
pub fn flatten(rgba: (f64, f64, f64, f64), under: Option<(f64, f64, f64)>) -> String {
    try_flatten(rgba, under).unwrap_or_default()
}

/// [`flatten`] exactly as the Python behaves (NaN or infinity raise).
pub fn try_flatten(rgba: (f64, f64, f64, f64), under: Option<(f64, f64, f64)>) -> Result<String, String> {
    let (mut r, mut g, mut b, a) = rgba;
    if let Some((ur, ug, ub)) = under {
        if a < 1.0 {
            r = r * a + ur * (1.0 - a);
            g = g * a + ug * (1.0 - a);
            b = b * a + ub * (1.0 - a);
        }
    }
    let mut out = String::from("#");
    for c in [r, g, b] {
        out.push_str(&py_round_hex02(c * 255.0)?);
    }
    Ok(out)
}

/// Black or white, whichever actually reads better on this colour.
///
/// Measured, not guessed from a luminance cutoff: a mid-blue accent sits
/// close enough to any threshold you pick that the wrong side of it costs
/// four times the contrast. Comparing both candidates is exact and cannot
/// be off by one theme.
///
/// The theme's own darkest background was used here before, which is only
/// right when the accent happens to be light.
pub fn readable_on(background: &str) -> String {
    if background.is_empty() {
        return String::new();
    }
    // max() keeps the first of equals: black wins a tie.
    if contrast("#ffffff", background) > contrast("#000000", background) {
        "#ffffff".into()
    } else {
        "#000000".into()
    }
}

/// `_pick`: the first of `names` the palette defines (non-empty), or "".
fn pick(p: &BTreeMap<String, String>, names: &[&str]) -> String {
    for n in names {
        if let Some(v) = p.get(*n) {
            if !v.is_empty() {
                return v.clone();
            }
        }
    }
    String::new()
}

/// Restate an Omarchy palette as libadwaita's named colours.
///
/// Only names the theme actually defines are emitted. A theme that ships no
/// `green` keeps libadwaita's success colour rather than getting a guess.
pub fn css_for(palette: &BTreeMap<String, String>) -> String {
    try_css_for(palette).unwrap_or_default()
}

/// [`css_for`] exactly as the Python behaves, ValueError included.
pub fn try_css_for(palette: &BTreeMap<String, String>) -> Result<String, String> {
    if palette.is_empty() {
        return Ok(String::new());
    }

    let bg = pick(palette, &["background"]);
    let bg_dark = pick(palette, &["dark_background", "background"]);
    let bg_dkr = pick(palette, &["darker_background", "dark_background", "background"]);
    let bg_lit = pick(palette, &["lighter_background", "background"]);
    let mut fg = pick(palette, &["foreground"]);
    // Primary text held to 7:1 on the lightest surface it is drawn on (the
    // cards); a theme that already reads that well is left exactly as it is.
    if !fg.is_empty() {
        let on = readable_on(if bg_lit.is_empty() { &bg } else { &bg_lit });
        fg = try_legible(&fg, &on, &[&bg_lit, &bg], BODY_TARGET)?;
    }
    let _fg_dim = pick(palette, &["dark_foreground", "muted", "foreground"]);
    let accent = pick(palette, &["accent", "blue"]);
    let select = pick(palette, &["selection"]);

    let green = pick(palette, &["green"]);
    let warn = pick(palette, &["yellow", "orange"]);
    let red = pick(palette, &["red"]);

    // (libadwaita name, value). Order matters only for readability.
    let mut pairs: Vec<(&str, String)> = vec![
        ("accent_bg_color", accent.clone()),
        ("accent_color", accent.clone()),
        ("accent_fg_color", if accent.is_empty() { String::new() } else { readable_on(&accent) }),
        ("window_bg_color", bg.clone()),
        ("window_fg_color", fg.clone()),
        ("view_bg_color", bg_dark.clone()),
        ("view_fg_color", fg.clone()),
        ("headerbar_bg_color", bg_dark.clone()),
        ("headerbar_fg_color", fg.clone()),
        ("sidebar_bg_color", bg_dkr.clone()),
        ("sidebar_fg_color", fg.clone()),
        ("secondary_sidebar_bg_color", bg_dkr.clone()),
        ("card_bg_color", bg_lit.clone()),
        ("card_fg_color", fg.clone()),
        ("dialog_bg_color", bg_lit.clone()),
        ("dialog_fg_color", fg.clone()),
        ("popover_bg_color", bg_lit.clone()),
        ("popover_fg_color", fg.clone()),
        ("success_color", green.clone()),
        ("success_bg_color", green.clone()),
        ("warning_color", warn.clone()),
        ("warning_bg_color", warn.clone()),
        ("error_color", red.clone()),
        ("error_bg_color", red.clone()),
        ("destructive_color", red.clone()),
        ("destructive_bg_color", red.clone()),
        // Foregrounds for the solid semantic fills, measured against each
        // fill rather than assumed to be white.
        ("error_fg_color", readable_on(&red)),
        ("warning_fg_color", readable_on(&warn)),
        ("success_fg_color", readable_on(&green)),
    ];

    // Text colours, measured. A theme's accent and semantic colours are made
    // for fills; as text on its own backgrounds they can fall short (in
    // Everforest red text read at 3.6:1 in a pill). libadwaita keeps the text
    // variants (accent_color, success_color…) apart from the fills
    // (accent_bg_color…), so only the text is lifted, toward the foreground,
    // and only as far as it needs to go.
    let surfaces: [&str; 2] = [&bg_lit, &bg];
    let css_pairs_raw = pairs.clone();
    if !fg.is_empty() {
        let text = ["accent_color", "success_color", "warning_color", "error_color", "destructive_color"];
        for (name, value) in pairs.iter_mut() {
            if text.contains(name) && !value.is_empty() {
                *value = try_legible(value, &fg, &surfaces, TEXT_TARGET)?;
            }
        }
    }

    let mut lines: Vec<String> = pairs
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(name, value)| format!("@define-color {name} {value};"))
        .collect();

    // Dimmed text in libadwaita is the foreground at --dim-opacity (55%).
    // On a low-contrast palette that is unreadable -- 3.1:1 on Everforest's
    // cards -- so the opacity is computed from the palette instead. (The
    // theme's "dark_foreground" used to be painted here as dim text; it is a
    // terminal colour, and read at 1.5:1.)
    if !fg.is_empty() && !bg.is_empty() {
        lines.push(format!(":root {{ --dim-opacity: {}%; }}", try_dim_opacity(&fg, &surfaces, TEXT_TARGET)?));
        let card = if bg_lit.is_empty() { &bg } else { &bg_lit };
        lines.push(try_pick_css(card, &fg, &accent)?.trim_end_matches('\n').to_string());
        // dict(css_pairs_raw): the last value of a repeated name wins.
        let mut raw: BTreeMap<&str, String> = BTreeMap::new();
        for (name, value) in &css_pairs_raw {
            raw.insert(name, value.clone());
        }
        let get = |k: &str| raw.get(k).cloned().unwrap_or_default();
        let mut fills = BTreeMap::new();
        fills.insert("error".to_string(), get("error_bg_color"));
        fills.insert("warning".to_string(), get("warning_bg_color"));
        fills.insert("success".to_string(), get("success_bg_color"));
        fills.insert("accent".to_string(), accent.clone());
        lines.push(pill_css(&fills, &[&bg_dkr, &fg]).trim_end_matches('\n').to_string());
    }
    if !select.is_empty() {
        lines.push(format!("@define-color accent_hover_color {select};"));
    }
    Ok(lines.join("\n") + "\n")
}

/// True/False from the theme's own `mode`, or None when it says nothing.
pub fn is_dark(palette: &BTreeMap<String, String>) -> Option<bool> {
    match palette.get("mode").map(String::as_str) {
        Some("dark") => Some(true),
        Some("light") => Some(false),
        _ => None,
    }
}

/// The theme's display name.
///
/// `current/theme` is not always a symlink into the themes directory, so the
/// directory name can be the useless literal "theme". Omarchy writes the real
/// name next to it, which costs a file read instead of a subprocess.
///
/// Port note: where the Python raises out of here (a theme.name or an
/// `omarchy theme current` output that is not UTF-8: UnicodeDecodeError,
/// which it does not catch), this infallible version treats the source as
/// unreadable and goes on to the next one; [`try_theme_name`] raises as the
/// Python does. A directory name that is not UTF-8 (a str holding
/// surrogate escapes in the Python) is shown with U+FFFD here.
pub fn theme_name() -> String {
    shown(&try_theme_name_at(&current(), true).unwrap_or_default())
}

/// [`theme_name`] exactly as the Python behaves, UnicodeDecodeError included.
pub fn try_theme_name() -> Result<String, String> {
    try_theme_name_at(&current(), false).map(|n| shown(&n))
}

/// The theme name as the Python computes it: `Err` where it raised, unless
/// `lenient`, which treats an undecodable source like an unreadable one.
/// Undecodable bytes of a directory name come back escaped (see
/// [`escaped_name`]).
fn try_theme_name_at(current: &Path, lenient: bool) -> Result<String, String> {
    let parent = current.parent().unwrap_or(Path::new(""));
    // read_text(): text mode, so strict UTF-8 (the locale's encoding) and
    // universal newlines -- "\r\n" and a lone "\r" both read as "\n".
    match std::fs::read(parent.join("theme.name")) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => {
                let text = universal_newlines(&text);
                let name = py_strip(&text);
                if !name.is_empty() {
                    // The file holds the slug ("apple-dark"); the display name is
                    // what the theme picker shows ("Apple Dark").
                    return Ok(if py_islower(name) {
                        py_title(&name.replace('-', " ").replace('_', " "))
                    } else {
                        name.to_string()
                    });
                }
            }
            Err(_) if !lenient => {
                return Err("UnicodeDecodeError: theme.name is not UTF-8".into());
            }
            Err(_) => {}
        },
        Err(_) => {} // OSError
    }
    if py_which("omarchy").is_some() {
        match omarchy_theme_current() {
            Ok(Some((code, stdout))) => {
                let stdout = py_strip(&stdout);
                let first = py_splitlines_first(stdout);
                if code == Some(0) && !stdout.is_empty() && !first.is_empty() {
                    return Ok(first.to_string());
                }
            }
            Ok(None) => {} // OSError, TimeoutExpired
            Err(e) if !lenient => return Err(e),
            Err(_) => {}
        }
    }
    Ok(theme_dir_at(current).and_then(|d| d.file_name().map(escaped_name)).unwrap_or_default())
}

/// `shutil.which(cmd)` for a bare command name. Not `util::which`, which
/// differs in three ways that matter here: with PATH unset Python searches
/// `os.confstr("CS_PATH")`, with PATH empty it finds nothing (not the
/// current directory), and a file counts only if `os.access(X_OK)` says the
/// caller may run it, not if any execute bit is set.
fn py_which(cmd: &str) -> Option<PathBuf> {
    let path = match std::env::var_os("PATH") {
        Some(p) => p,
        // glibc's _CS_PATH.
        None => OsString::from("/bin:/usr/bin"),
    };
    // PATH='' doesn't match, whereas PATH=':' looks in the current
    // directory
    if path.is_empty() {
        return None;
    }
    path.as_bytes()
        .split(|&b| b == b':')
        .map(|dir| Path::new(OsStr::from_bytes(dir)).join(cmd))
        .find(|name| py_access_check(name))
}

/// `shutil._access_check(fn, os.F_OK | os.X_OK)`.
fn py_access_check(name: &Path) -> bool {
    use rustix::fs::{access, Access};
    name.exists() && access(name, Access::EXISTS | Access::EXEC_OK).is_ok() && !name.is_dir()
}

/// `subprocess.run(["omarchy", "theme", "current"], capture_output=True,
/// text=True, timeout=10)`: `Ok(None)` where it raises OSError or
/// TimeoutExpired (both caught by the caller), `Err` for the
/// UnicodeDecodeError strict decoding raises on output that is not UTF-8
/// (not caught), else the return code and stdout. Not `util::run`, which
/// decodes lossily and gives the child /dev/null as stdin: this keeps the
/// raw bytes and, like `subprocess.run` without `stdin=`, the parent's stdin.
///
/// The 10 seconds cover the output as well as the process, as
/// `communicate(timeout=10)` does: a child that exits but leaves something
/// holding its stdout (a background job) times out too.
fn omarchy_theme_current() -> Result<Option<(Option<i32>, String)>, String> {
    let mut child = match Command::new("omarchy")
        .args(["theme", "current"])
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return Ok(None),
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    // Both pipes drained on their own threads, so a chatty child cannot fill
    // one and block while it is waited on. A reader still blocked when time
    // runs out is left behind; it ends when the pipe closes.
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            let _ = tx.send(buf);
        });
        rx
    };
    let out_rx = drain(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err_rx = drain(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    // TimeoutExpired: subprocess.run kills the child and waits for it.
    let timed_out = |child: &mut std::process::Child| {
        let _ = child.kill();
        let _ = child.wait();
        Ok(None)
    };
    let left = || deadline.saturating_duration_since(Instant::now());
    let Ok(stdout) = out_rx.recv_timeout(left()) else {
        return timed_out(&mut child);
    };
    let Ok(stderr) = err_rx.recv_timeout(left()) else {
        return timed_out(&mut child);
    };
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) if Instant::now() >= deadline => return timed_out(&mut child),
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => return Ok(None),
        }
    };
    // communicate() decodes stdout, then stderr; either can raise.
    let decode = |b: Vec<u8>| {
        String::from_utf8(b)
            .map(|t| universal_newlines(&t))
            .map_err(|_| "UnicodeDecodeError: 'utf-8' codec can't decode `omarchy theme current` output".to_string())
    };
    let stdout = decode(stdout)?;
    decode(stderr)?;
    Ok(Some((status.code(), stdout)))
}

/// Text mode's newline translation: "\r\n" and a lone "\r" become "\n".
fn universal_newlines(s: &str) -> String {
    if s.contains('\r') {
        s.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        s.to_string()
    }
}

/// `s.splitlines()[0]` (s non-empty): up to the first of Python's line
/// boundaries.
fn py_splitlines_first(s: &str) -> &str {
    let end = s
        .find(|c: char| {
            matches!(c, '\n' | '\r' | '\u{0b}' | '\u{0c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}')
        })
        .unwrap_or(s.len());
    &s[..end]
}

// --- Python's str.title() / str.islower() -----------------------------------
// (`util::py_title` / `util::py_islower` upper-case where Python title-cases
// and ignore titlecase letters, so the full versions live here.)

/// Category Lt: the titlecase letters (ǅ ǈ ǋ ǲ and the Greek ones carrying
/// a prosgegrammeni).
fn py_istitle_char(c: char) -> bool {
    matches!(c as u32, 0x1C5 | 0x1C8 | 0x1CB | 0x1F2 | 0x1F88..=0x1F8F | 0x1F98..=0x1F9F | 0x1FA8..=0x1FAF | 0x1FBC | 0x1FCC | 0x1FFC)
}

/// `_PyUnicode_IsCased`: Lowercase, Uppercase or Lt.
fn py_is_cased(c: char) -> bool {
    !unicode17_new(c) && (c.is_lowercase() || c.is_uppercase() || py_istitle_char(c))
}

/// `str.islower()`: no upper-case or titlecase character, and at least one
/// lower-case one.
fn py_islower(s: &str) -> bool {
    let mut cased = false;
    for c in s.chars().filter(|&c| !unicode17_new(c)) {
        if c.is_uppercase() || py_istitle_char(c) {
            return false;
        }
        if c.is_lowercase() {
            cased = true;
        }
    }
    cased
}

/// Full titlecase mappings (SpecialCasing included) that differ from both
/// the character and its uppercase mapping, from Python's unicodedata.
const TITLE_FULL: &[(char, &str)] = &[
    ('\u{DF}', "\u{53}\u{73}"), ('\u{1C4}', "\u{1C5}"), ('\u{1C6}', "\u{1C5}"), ('\u{1C7}', "\u{1C8}"),
    ('\u{1C9}', "\u{1C8}"), ('\u{1CA}', "\u{1CB}"), ('\u{1CC}', "\u{1CB}"), ('\u{1F1}', "\u{1F2}"),
    ('\u{1F3}', "\u{1F2}"), ('\u{587}', "\u{535}\u{582}"), ('\u{1F80}', "\u{1F88}"), ('\u{1F81}', "\u{1F89}"),
    ('\u{1F82}', "\u{1F8A}"), ('\u{1F83}', "\u{1F8B}"), ('\u{1F84}', "\u{1F8C}"), ('\u{1F85}', "\u{1F8D}"),
    ('\u{1F86}', "\u{1F8E}"), ('\u{1F87}', "\u{1F8F}"), ('\u{1F90}', "\u{1F98}"), ('\u{1F91}', "\u{1F99}"),
    ('\u{1F92}', "\u{1F9A}"), ('\u{1F93}', "\u{1F9B}"), ('\u{1F94}', "\u{1F9C}"), ('\u{1F95}', "\u{1F9D}"),
    ('\u{1F96}', "\u{1F9E}"), ('\u{1F97}', "\u{1F9F}"), ('\u{1FA0}', "\u{1FA8}"), ('\u{1FA1}', "\u{1FA9}"),
    ('\u{1FA2}', "\u{1FAA}"), ('\u{1FA3}', "\u{1FAB}"), ('\u{1FA4}', "\u{1FAC}"), ('\u{1FA5}', "\u{1FAD}"),
    ('\u{1FA6}', "\u{1FAE}"), ('\u{1FA7}', "\u{1FAF}"), ('\u{1FB2}', "\u{1FBA}\u{345}"), ('\u{1FB3}', "\u{1FBC}"),
    ('\u{1FB4}', "\u{386}\u{345}"), ('\u{1FB7}', "\u{391}\u{342}\u{345}"), ('\u{1FC2}', "\u{1FCA}\u{345}"),
    ('\u{1FC3}', "\u{1FCC}"), ('\u{1FC4}', "\u{389}\u{345}"), ('\u{1FC7}', "\u{397}\u{342}\u{345}"),
    ('\u{1FF2}', "\u{1FFA}\u{345}"), ('\u{1FF3}', "\u{1FFC}"), ('\u{1FF4}', "\u{38F}\u{345}"),
    ('\u{1FF7}', "\u{3A9}\u{342}\u{345}"), ('\u{FB00}', "\u{46}\u{66}"), ('\u{FB01}', "\u{46}\u{69}"),
    ('\u{FB02}', "\u{46}\u{6C}"), ('\u{FB03}', "\u{46}\u{66}\u{69}"), ('\u{FB04}', "\u{46}\u{66}\u{6C}"),
    ('\u{FB05}', "\u{53}\u{74}"), ('\u{FB06}', "\u{53}\u{74}"), ('\u{FB13}', "\u{544}\u{576}"),
    ('\u{FB14}', "\u{544}\u{565}"), ('\u{FB15}', "\u{544}\u{56B}"), ('\u{FB16}', "\u{54E}\u{576}"),
    ('\u{FB17}', "\u{544}\u{56D}"),
];

/// `_PyUnicode_ToTitleFull` for one character.
fn push_title(c: char, out: &mut String) {
    // Titlecase letters and Georgian Mkhedruli are their own titlecase
    // (their uppercase is a different letter).
    if py_istitle_char(c)
        || matches!(c as u32, 0x10D0..=0x10FA | 0x10FD..=0x10FF)
        || unicode17_new(c)
        || unicode17_gained_upper(c)
    {
        out.push(c);
    } else if let Some((_, t)) = TITLE_FULL.iter().find(|(k, _)| *k == c) {
        out.push_str(t);
    } else {
        out.extend(c.to_uppercase());
    }
}

/// `str.title()`: the first cased character after an uncased one is
/// title-cased, the rest lower-cased (a capital sigma by the final-sigma
/// rule, as Python's lower() does).
fn py_title(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_cased = false;
    let mut lowered: Option<String> = None;
    for (i, c) in s.char_indices() {
        if !prev_cased {
            push_title(c, &mut out);
        } else if c == '\u{3a3}' {
            // Whether this Σ is final depends on its neighbours; str's
            // to_lowercase applies the same rule. Every other lower-casing is
            // context-free and σ/ς are both two bytes, so the prefix's
            // lower-cased length is this Σ's offset in the whole.
            let all = lowered.get_or_insert_with(|| py_lower(s));
            let at = py_lower(&s[..i]).len();
            out.push(if all[at..].starts_with('\u{3c2}') { '\u{3c2}' } else { '\u{3c3}' });
        } else if unicode17_new(c) {
            out.push(c);
        } else {
            out.extend(c.to_lowercase());
        }
        prev_cased = py_is_cased(c);
    }
    out
}

// --- Names that are not UTF-8 ------------------------------------------------

/// A Python str decoded with surrogateescape holds an undecodable byte b as
/// the lone surrogate U+DC00+b, which a Rust String cannot. Such a byte is
/// kept as U+10FF00+b (private use, unique per byte) while the name is
/// computed, and shown as U+FFFD by the public functions.
const ESCAPE_BASE: u32 = 0x10FF00;

/// `d.name` (`os.fsdecode`, surrogateescape) in the escaped form above.
fn escaped_name(name: &OsStr) -> String {
    let mut out = String::with_capacity(name.len());
    for chunk in name.as_bytes().utf8_chunks() {
        out.push_str(chunk.valid());
        for &b in chunk.invalid() {
            out.push(char::from_u32(ESCAPE_BASE + u32::from(b)).unwrap_or('\u{fffd}'));
        }
    }
    out
}

/// An escaped name as text to show: each escaped byte as U+FFFD.
fn shown(s: &str) -> String {
    s.chars()
        .map(|c| if (ESCAPE_BASE + 0x80..=ESCAPE_BASE + 0xFF).contains(&(c as u32)) { '\u{fffd}' } else { c })
        .collect()
}

/// Call `on_change()` whenever the active theme changes.
///
/// Both the pointer and the file it points at are watched: `omarchy theme
/// set` replaces the symlink, while editing a theme in place rewrites
/// colors.toml without the pointer moving.
///
/// Port note: the Python builds the monitors with `Gio.FileMonitor.directory`
/// / `Gio.FileMonitor.file`, which do not exist in PyGObject. Every target
/// raises AttributeError, its `except Exception: continue` swallows it, and
/// the list comes back empty: nothing is ever watched. That is reproduced
/// here; watching for real would be `gio::File::monitor_directory` /
/// `monitor_file` with `FileMonitorFlags::WATCH_MOVES`, connected to
/// `changed`, once the Python is fixed.
pub fn watch(on_change: impl Fn() + 'static) -> Vec<gio::FileMonitor> {
    watch_at(&current(), on_change)
}

fn watch_at(current: &Path, _on_change: impl Fn() + 'static) -> Vec<gio::FileMonitor> {
    let mut targets: Vec<PathBuf> = vec![current.parent().unwrap_or(Path::new("")).to_path_buf(), current.to_path_buf()];
    if let Some(colors) = colors_path_at(current) {
        targets.push(colors);
    }
    // Each target: AttributeError, `continue`. None is appended.
    let monitors: Vec<gio::FileMonitor> = Vec::new();
    // Returned so the caller can keep them alive; a dropped monitor stops
    // emitting without any error.
    monitors
}

/// Port note: as [`theme_name`] and [`load_palette`], infallible where the
/// Python can raise (a RecursionError reads as an unreadable colors.toml);
/// [`try_describe`] raises as it does.
pub fn describe() -> String {
    shown(&try_describe_at(&current(), true).unwrap_or_default())
}

/// [`describe`] exactly as the Python behaves, UnicodeDecodeError included.
pub fn try_describe() -> Result<String, String> {
    try_describe_at(&current(), false).map(|d| shown(&d))
}

/// `describe()` with the semantics of [`try_theme_name_at`].
fn try_describe_at(current: &Path, lenient: bool) -> Result<String, String> {
    let Some(d) = theme_dir_at(current) else {
        return Ok("No Omarchy theme detected — using the system GTK theme".to_string());
    };
    // One frame deeper than a direct load_palette() call: describe's own.
    let palette = match try_load_palette_at(current, CALLER_FRAMES + 1) {
        Ok(p) => p,
        Err(e) if !lenient => return Err(e),
        Err(_) => BTreeMap::new(),
    };
    let mut name = try_theme_name_at(current, lenient)?;
    if name.is_empty() {
        name = d.file_name().map(escaped_name).unwrap_or_default();
    }
    if palette.is_empty() {
        return Ok(format!("{name} (no readable colors.toml — using the system theme)"));
    }
    Ok(format!("Following the Omarchy theme: {name} ({})", palette.get("mode").map(String::as_str).unwrap_or("?")))
}


#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    // TextContrast: every kind of text measured against the surfaces it sits
    // on, for a low-contrast dark palette (Everforest) and a light one.
    fn everforest() -> BTreeMap<String, String> {
        map(&[
            ("mode", "dark"), ("background", "#2d353b"), ("dark_background", "#21272c"),
            ("darker_background", "#181d20"), ("lighter_background", "#343f44"),
            ("foreground", "#d3c6aa"), ("dark_foreground", "#4f585e"),
            ("accent", "#7fbbb3"), ("red", "#e67e80"), ("yellow", "#dbbc7f"),
            ("green", "#a7c080"),
        ])
    }

    fn light() -> BTreeMap<String, String> {
        map(&[
            ("mode", "light"), ("background", "#fdf6e3"), ("dark_background", "#eee8d5"),
            ("darker_background", "#e4ddc8"), ("lighter_background", "#ffffff"),
            ("foreground", "#586e75"), ("accent", "#268bd2"), ("red", "#dc322f"),
            ("yellow", "#b58900"), ("green", "#859900"),
        ])
    }

    fn pill_pairs(css: &str) -> Vec<(String, String)> {
        let re = regex::Regex::new(r"background: (#[0-9a-f]{6}); color: (#[0-9a-f]{6})").unwrap();
        re.captures_iter(css).map(|c| (c[1].to_string(), c[2].to_string())).collect()
    }

    fn check(pal: &BTreeMap<String, String>) -> String {
        let css = css_for(pal);
        let re = regex::Regex::new(r"@define-color (\w+) (#[0-9a-f]{6});").unwrap();
        let colours: BTreeMap<String, String> =
            re.captures_iter(&css).map(|c| (c[1].to_string(), c[2].to_string())).collect();
        let surfaces = [pal["lighter_background"].as_str(), pal["background"].as_str()];
        for name in ["accent_color", "success_color", "warning_color", "error_color"] {
            for surf in surfaces {
                assert!(contrast(&colours[name], surf) >= 5.5, "{name} on {surf}");
            }
        }
        let fg = &colours["window_fg_color"];
        for surf in surfaces {
            assert!(contrast(fg, surf) >= 7.0, "text on {surf}");
        }
        let pct: u32 = regex::Regex::new(r"--dim-opacity: (\d+)%").unwrap().captures(&css).unwrap()[1]
            .parse()
            .unwrap();
        for surf in surfaces {
            let dim = mix(surf, fg, f64::from(pct) / 100.0);
            assert!(contrast(&dim, surf) >= 5.5, "dim on {surf}");
        }
        for (fill, text) in pill_pairs(&css) {
            let best = contrast(&fill, "#000000").max(contrast(&fill, "#ffffff"));
            assert!(contrast(&fill, &text) >= best.min(7.0), "{fill}");
        }
        css
    }

    #[test]
    fn test_a_low_contrast_dark_palette() {
        let css = check(&everforest());
        // The terminal colour once painted as dim text, at 1.5:1.
        assert!(!css.contains("#4f585e"));
    }

    #[test]
    fn test_a_light_palette() {
        check(&light());
    }

    #[test]
    fn test_stock_theme_pills_are_measured() {
        let css = pill_css(
            &map(&[("error", "#c01c28"), ("warning", "#cd9309"), ("success", "#2ec27e"), ("accent", "#3584e4")]),
            &[],
        );
        for (fill, text) in pill_pairs(&css) {
            assert!(contrast(&fill, &text) >= 4.5, "{fill}");
        }
    }

    /// libadwaita's dark card is white at 8% over #222226. Measured as
    /// plain white, the pick card got black text on a dark card.
    #[test]
    fn test_translucent_theme_colours_are_measured_as_shown() {
        let card = flatten((1.0, 1.0, 1.0, 0.08), Some((34.0 / 255.0, 34.0 / 255.0, 38.0 / 255.0)));
        assert!(luminance(&card) < 0.05);
        let css = pick_css(&card, "#ffffff", "#78aeed");
        let text = regex::Regex::new(r"\.pick-card \{ color: (#[0-9a-f]{6})").unwrap().captures(&css).unwrap()[1]
            .to_string();
        assert!(luminance(&text) > 0.5);
    }

    /// Text on the picks' accent wash, measured at its strongest point,
    /// for a dark and a light palette and for stock Adwaita.
    #[test]
    fn test_the_wash_keeps_accent_text_legible() {
        let mut cases: Vec<(String, String, String)> = [everforest(), light()]
            .iter()
            .map(|p| (p["lighter_background"].clone(), p["foreground"].clone(), p["accent"].clone()))
            .collect();
        cases.push(("#ffffff".into(), "#000000cc"[..7].into(), "#3584e4".into())); // Adwaita light
        cases.push(("#383838".into(), "#ffffff".into(), "#78aeed".into())); // Adwaita dark
        let re_t = regex::Regex::new(r"\.pick-card \{ color: (#[0-9a-f]{6})").unwrap();
        let re_e = regex::Regex::new(r"\.pick-eyebrow \{ color: (#[0-9a-f]{6})").unwrap();
        for (card, fg, accent) in cases {
            let css = pick_css(&card, &fg, &accent);
            let text = re_t.captures(&css).unwrap()[1].to_string();
            let eyebrow = re_e.captures(&css).unwrap()[1].to_string();
            let wash = mix(&card, &accent, PICK_WASH);
            assert!(contrast(&text, &wash) >= 7.0, "{card}");
            assert!(contrast(&eyebrow, &wash) >= 5.5, "{card}");
        }
        // and the palette path emits it
        assert!(css_for(&everforest()).contains(".pick-eyebrow"));
    }

    #[test]
    fn python_int_parsing() {
        assert_eq!(py_int16("ff"), Ok(255));
        assert_eq!(py_int16(" f"), Ok(15));
        assert_eq!(py_int16("\u{0b}f"), Ok(15));
        assert!(py_int16("\u{1c}f").is_err());
        assert_eq!(py_int16("\u{a0}f"), Ok(15));
        assert_eq!(py_int16("-f"), Ok(-15));
        assert!(py_int16("zz").is_err());
        assert!(py_int16("_f").is_err());
        assert_eq!(py_int16("\u{661}\u{662}"), Ok(0x12));
        assert_eq!(py_hex02(-5), "-5");
        assert_eq!(py_hex02(5), "05");
    }

    #[test]
    fn python_str_title_and_islower() {
        assert_eq!(py_title("apple dark"), "Apple Dark");
        assert_eq!(py_title("\u{df}lue x"), "Sslue X");
        assert_eq!(py_title("\u{fb01}re dark"), "Fire Dark");
        assert_eq!(py_title("\u{1c6}a b"), "\u{1c5}a B");
        assert_eq!(py_title("a\u{3a3}"), "A\u{3c2}");
        assert!(!py_islower("\u{1c5}a-b"));
        assert!(py_islower("apple-dark"));
        assert!(!py_islower("--"));
    }

    #[test]
    fn python_round_is_unbounded() {
        assert_eq!(py_round_hex02(2.5).unwrap(), "02");
        assert_eq!(py_round_hex02(-5.0).unwrap(), "-5");
        assert_eq!(py_round_hex02(2f64.powi(64)).unwrap(), "10000000000000000");
        assert_eq!(py_round_hex02(-(2f64.powi(70))).unwrap(), "-400000000000000000");
        assert!(py_round_hex02(f64::NAN).is_err());
        assert!(py_round_hex02(f64::INFINITY).is_err());
    }

    #[test]
    fn python_path_normalisation() {
        assert_eq!(py_path(b""), PathBuf::from("."));
        assert_eq!(py_path(b"/a//b/./c/"), PathBuf::from("/a/b/c"));
        assert_eq!(py_path(b"//a"), PathBuf::from("//a"));
        assert_eq!(py_path(b"///a"), PathBuf::from("/a"));
        assert_eq!(py_path(b"./a/../b"), PathBuf::from("a/../b"));
    }

    fn loads(text: &str) -> Result<(), tomllib::Error> {
        // As deep as the parity runner's load_palette() call.
        tomllib::loads(text, CALLER_FRAMES + 2).map(|_| ())
    }

    #[test]
    fn tomllib_number_and_datetime_limits() {
        use tomllib::Error::Value;
        assert_eq!(loads("x = 99999999999999999999\n"), Ok(()));
        assert_eq!(loads("x = -1e1000\n"), Ok(()));
        assert_eq!(loads("t = 23:59:60\n"), Err(Value));
        assert_eq!(loads("d = 0000-01-01\n"), Err(Value));
        assert_eq!(loads("d = 1979-02-29\n"), Err(Value));
        assert_eq!(loads("d = 1980-02-29T00:00:00+23:59\n"), Ok(()));
        assert_eq!(loads("99999999999999999999 = 1\n99999999999999999999 = 2\n"), Err(Value));
        assert_eq!(loads("x = 099999999999999999999\n"), Err(Value));
        // int() stops at 4300 decimal digits; underscores, signs and
        // power-of-two bases do not count.
        assert_eq!(loads(&format!("x = {}\n", "9".repeat(4300))), Ok(()));
        assert_eq!(loads(&format!("x = -{}\n", "9".repeat(4300))), Ok(()));
        assert_eq!(loads(&format!("x = {}\n", "9".repeat(4301))), Err(Value));
        assert_eq!(loads(&format!("x = 1{}\n", "_0".repeat(4299))), Ok(()));
        assert_eq!(loads(&format!("x = 0x{}\n", "f".repeat(5000))), Ok(()));
        assert_eq!(loads("\u{feff}x = 1\n"), Err(Value));
    }

    #[test]
    fn tomllib_recursion() {
        use tomllib::Error::{Recursion, Value};
        let arr = |d: usize| format!("x = {}{}\n", "[".repeat(d), "]".repeat(d));
        let tbl = |d: usize| format!("x = {}1{}\n", "{a=".repeat(d), "}".repeat(d));
        let keys = |n: usize| format!("x.{} = 1\n", (0..n).map(|i| format!("k{i}")).collect::<Vec<_>>().join("."));
        // loads at frame 7: key_value_rule 8, parse_key_value_pair 9, then
        // two frames an array level and skip_comments_and_array_ws +
        // skip_chars under the innermost: 11 + 2d <= 1000.
        assert_eq!(loads(&arr(494)), Ok(()));
        assert_eq!(loads(&arr(495)), Err(Recursion));
        assert_eq!(loads(&tbl(300)), Ok(()));
        assert_eq!(loads(&tbl(400)), Err(Recursion));
        assert_eq!(loads(&keys(999)), Ok(()));
        assert_eq!(loads(&keys(1000)), Err(Recursion));
        // An error before the deep part is a plain decode error.
        assert_eq!(loads(&format!("x = ?\n{}", arr(700))), Err(Value));
    }

    #[test]
    fn unicode_16_case_tables() {
        assert_eq!(py_lower("A\u{a7ce}B"), "a\u{a7ce}b");
        assert_eq!(py_lower("\u{16ea0}"), "\u{16ea0}");
        assert_eq!(hex6("#\u{a7d2}bc"), "#\u{a7d2}\u{a7d2}bbcc");
        assert!(py_islower("x-\u{a7ce}\u{a7ce}"));
        assert_eq!(py_title("x \u{a7ce}\u{a7ce}"), "X \u{a7ce}\u{a7ce}");
        assert_eq!(py_title("x \u{a7d3}\u{a7d3}"), "X \u{a7d3}\u{a7d3}");
        assert_eq!(py_lower("a\u{3a3}\u{a7ce}"), "a\u{3c2}\u{a7ce}");
    }
}
