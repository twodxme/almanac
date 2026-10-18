//! Port of `inventory.py`.
//!
//! Enumerate installed software across every source on this machine.
//!
//! Deliberately read-only. Nothing here mutates package state -- the GUI delegates
//! every install/update to the tool that actually owns it (pacman, yay, omarchy
//! update). The one exception is AppImages, which no package manager tracks, so
//! this module reads a registry that the app itself owns.
//!
//! Naming: Python's "private" helpers that other modules call
//! (`_load_registry`, `_pending_repo`, `_git_cache`, ...) drop the leading
//! underscore here (`load_registry`, `pending_repo`, `git_cache`), the same
//! rule the UI sections use for methods.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::util;

// Runtime data, deliberately not where the code lives.
//
// Almanac installs as a git checkout that updates itself, so anything it
// writes has to sit outside that checkout -- otherwise `git pull` is fighting
// the app's own state, and a fresh clone would have 180MB of someone else's
// AppImages in it. Everything else derives from this one path.

/// `STATE`: `$XDG_STATE_HOME/almanac` (default `~/.local/state/almanac`).
/// A function rather than a constant because it reads the environment; the
/// Python read it once at import, which is the same answer for a process that
/// does not change its own environment.
pub fn state() -> PathBuf {
    // os.environ.get(...) uses the variable whenever it is set, even empty.
    match std::env::var_os("XDG_STATE_HOME") {
        Some(v) => PathBuf::from(v).join("almanac"),
        None => crate::util::home().join(".local/state").join("almanac"),
    }
}

/// `REGISTRY`: the AppImage registry the app itself owns.
pub fn registry() -> PathBuf {
    state().join("appimages.json")
}

// Sources, in the order they should appear.
pub const REPO: &str = "repo";
pub const AUR: &str = "aur";
pub const APPIMAGE: &str = "appimage";
pub const LOCAL: &str = "local";
pub const FLATPAK: &str = "flatpak";
pub const ORPHAN: &str = "orphan";
pub const SYSTEM: &str = "system";
/// AI coding agents: installed by their own scripts or a version manager, so no
/// package manager reports them. See agents.py.
pub const AGENT: &str = "agent";
/// Version-managed developer tools: mise, asdf and friends own software that
/// no package manager reports on.
pub const TOOL: &str = "tool";

/// Updates worth pulling out of the crowd. Two kinds:
///   security   — network-facing or credential-handling; a stale one is exposure
///   core       — can stop the machine booting, or needs a reboot to take effect
///
/// A slice of pairs rather than a map so the Python dict's iteration order
/// (security first) is kept.
pub const CRITICAL: &[(&str, &[&str])] = &[
    (
        "security",
        &[
            "chromium", "google-chrome", "firefox", "brave", "vivaldi", "opera",
            "openssl", "openssh", "gnutls", "curl", "wget", "nss", "ca-certificates",
            "sudo", "polkit", "1password", "bitwarden", "keepassxc", "gnupg",
            "wpa_supplicant", "networkmanager", "dnsmasq", "nginx", "openvpn",
            "wireguard-tools", "tailscale", "nordvpn-bin",
        ],
    ),
    (
        "core",
        &[
            "linux", "linux-lts", "linux-zen", "linux-firmware", "glibc", "systemd",
            "mkinitcpio", "limine", "grub", "mesa", "pacman", "btrfs-progs",
        ],
    ),
];

// ------------------------------------------------------- Python str helpers --

/// `str.isspace()` for one character: Unicode White_Space plus the four
/// ASCII separators (\x1c-\x1f) Python also counts.
fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// `str.split()` with no separator.
fn py_split(s: &str) -> Vec<&str> {
    s.split(py_space).filter(|p| !p.is_empty()).collect()
}

/// `str.strip()` with no argument.
fn py_strip(s: &str) -> &str {
    s.trim_matches(py_space)
}

/// `str.splitlines()`: every line boundary Python recognises.
fn py_splitlines(s: &str) -> Vec<&str> {
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

/// `s[:n]` in code points.
fn py_prefix(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// `urllib.parse.quote(s)` (safe="/").
fn py_quote(s: &str) -> String {
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

/// Python truthiness of a JSON value.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Python's type name for a JSON value, for error messages.
fn py_type(v: &Value) -> &'static str {
    match exotic(v) {
        Some(Exotic::Float(_)) => return "float",
        Some(Exotic::Int(_)) => return "int",
        Some(Exotic::WStr(_)) => return "str",
        None => {}
    }
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

/// The text of a value that is a Python str (a lone surrogate as U+FFFD);
/// None for anything else, a number Python's json made included.
fn real_str(v: &Value) -> Option<String> {
    match exotic(v) {
        Some(Exotic::WStr(t)) => Some(wstr_lossy(t)),
        Some(_) => None,
        None => v.as_str().map(String::from),
    }
}

/// `for x in value`: a list's items, a dict's keys, a string's characters;
/// anything else is the TypeError Python raises.
fn py_iter(v: &Value) -> Result<Vec<Value>, String> {
    if let Some(Exotic::WStr(t)) = exotic(v) {
        return Ok(wstr_units(t).into_iter().map(|u| units_json(&[u])).collect());
    }
    match v {
        Value::Array(a) => Ok(a.clone()),
        Value::Object(o) => Ok(o.keys().map(|k| json!(k)).collect()),
        Value::String(s) if exotic(v).is_none() => Ok(s.chars().map(|c| json!(c.to_string())).collect()),
        other => Err(format!("'{}' object is not iterable", py_type(other))),
    }
}

/// `repr(float)`: like the JSON form, but `inf`/`nan` for the non-finite.
fn py_float_str(f: f64) -> String {
    if f.is_nan() {
        "nan".into()
    } else if f.is_infinite() {
        if f > 0.0 { "inf".into() } else { "-inf".into() }
    } else {
        util::py_float_repr(f)
    }
}

/// `str(value)` of a JSON value, as an f-string would format it: a list or
/// dict is its repr.
fn py_str(v: &Value) -> String {
    match exotic(v) {
        Some(Exotic::Float(f)) => return py_float_str(f),
        Some(Exotic::Int(d)) => return d.to_string(),
        Some(Exotic::WStr(t)) => return wstr_lossy(t),
        None => {}
    }
    match v {
        Value::String(s) => s.clone(),
        other => py_repr(other),
    }
}

/// `str.isprintable()` for one character, as `repr` uses it: control
/// characters, separators other than the space, format characters, private
/// use and surrogates are escaped.
fn py_printable(c: u32) -> bool {
    !(c < 0x20
        || (0x7f..=0xa0).contains(&c)
        || c == 0xad
        || c == 0x1680
        || (0x2000..=0x200f).contains(&c)
        || (0x2028..=0x202f).contains(&c)
        || (0x205f..=0x2064).contains(&c)
        || (0x2066..=0x206f).contains(&c)
        || c == 0x3000
        || (0xd800..=0xf8ff).contains(&c)
        || c == 0xfeff
        || (0xfff9..=0xfffb).contains(&c)
        || (0xf0000..=0x10ffff).contains(&c)
        || c == 0x110bd
        || c == 0xe0001
        || (0xe0020..=0xe007f).contains(&c)
        || c == 0x061c
        || c == 0x180e
        || (0x0600..=0x0605).contains(&c)
        || c == 0x06dd
        || c == 0x070f
        || c == 0x08e2)
}

/// `repr(str)` of a string's code points.
fn py_repr_units(units: &[u32]) -> String {
    let has_single = units.contains(&('\'' as u32));
    let has_double = units.contains(&('"' as u32));
    let q = if has_single && !has_double { '"' } else { '\'' };
    let mut out = String::new();
    out.push(q);
    for &u in units {
        match u {
            0x5c => out.push_str("\\\\"),
            0x0a => out.push_str("\\n"),
            0x0d => out.push_str("\\r"),
            0x09 => out.push_str("\\t"),
            _ if u == q as u32 => {
                out.push('\\');
                out.push(q);
            }
            _ if py_printable(u) => out.push(char::from_u32(u).unwrap_or('\u{fffd}')),
            _ if u < 0x100 => out.push_str(&format!("\\x{u:02x}")),
            _ if u < 0x10000 => out.push_str(&format!("\\u{u:04x}")),
            _ => out.push_str(&format!("\\U{u:08x}")),
        }
    }
    out.push(q);
    out
}

/// `repr(value)` of a JSON value: what an f-string shows for a list or a
/// dict, and for the items inside one.
fn py_repr(v: &Value) -> String {
    match exotic(v) {
        Some(Exotic::Float(f)) => return py_float_str(f),
        Some(Exotic::Int(d)) => return d.to_string(),
        Some(Exotic::WStr(t)) => return py_repr_units(&wstr_units(t)),
        None => {}
    }
    match v {
        Value::Null => "None".into(),
        Value::Bool(b) => if *b { "True".into() } else { "False".into() },
        Value::Number(n) => match n.as_f64().filter(|_| n.is_f64()) {
            Some(f) => util::py_float_repr(f),
            None => n.to_string(),
        },
        Value::String(s) => py_repr_units(&s.chars().map(|c| c as u32).collect::<Vec<_>>()),
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, x)| format!("{}: {}", py_repr(&json!(k)), py_repr(x)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

// ------------------------------------------- Python's json, inside a Value --
//
// Python's `json.loads` takes more than serde_json does: NaN, Infinity and
// -Infinity, integers of any size, and `\ud800` escapes that leave a lone
// surrogate in a str. The files this module reads (the AppImage registry,
// the git cache, plugin manifests, the marketplace cache) are rewritten by
// the Python app too, so a file holding any of those must read the same here
// -- not as missing -- and come back out of a rewrite unchanged.
//
// `serde_json::Value` holds none of them, and the public maps stay `Value`
// for the modules that share them. So such a value is kept as a `String`
// carrying a marker (two Unicode noncharacters no JSON text the app reads
// contains) and the exact JSON token; every helper here reads it as the
// float, int or str it stands for, and `py_dumps` writes the token back.

const PYX: &str = "\u{10FFFF}\u{FDD0}py:";

enum Exotic<'a> {
    /// NaN or an infinity.
    Float(f64),
    /// An int beyond 64 bits: its decimal digits.
    Int(&'a str),
    /// A str with a lone surrogate: its `json.dumps` text, quotes included.
    WStr(&'a str),
}

fn exotic(v: &Value) -> Option<Exotic<'_>> {
    match v {
        Value::String(s) => exotic_str(s),
        _ => None,
    }
}

fn exotic_str(s: &str) -> Option<Exotic<'_>> {
    let rest = s.strip_prefix(PYX)?;
    if let Some(f) = rest.strip_prefix("f:") {
        return Some(Exotic::Float(match f {
            "inf" => f64::INFINITY,
            "-inf" => f64::NEG_INFINITY,
            _ => f64::NAN,
        }));
    }
    if let Some(d) = rest.strip_prefix("i:") {
        return Some(Exotic::Int(d));
    }
    rest.strip_prefix("s:").map(Exotic::WStr)
}

/// A Python float as a Value: NaN and the infinities kept.
fn float_json(f: f64) -> Value {
    serde_json::Number::from_f64(f)
        .map(Value::Number)
        .unwrap_or_else(|| Value::String(format!("{PYX}f:{}", py_float_str(f))))
}

/// A Python int (canonical decimal digits) as a Value: exact at any size.
fn int_json(digits: &str) -> Value {
    if let Ok(i) = digits.parse::<i64>() {
        return json!(i);
    }
    if let Ok(u) = digits.parse::<u64>() {
        return json!(u);
    }
    Value::String(format!("{PYX}i:{digits}"))
}

/// `json.dumps` of a str's code points (ensure_ascii).
fn units_dumps(units: &[u32]) -> String {
    let mut out = String::from("\"");
    for &u in units {
        match char::from_u32(u) {
            Some(c) if !(0xd800..=0xdfff).contains(&u) => {
                let t = util::py_json_string(&c.to_string());
                out.push_str(&t[1..t.len() - 1]);
            }
            _ => out.push_str(&format!("\\u{u:04x}")),
        }
    }
    out.push('"');
    out
}

/// A str from its code points: a `String` when it has no lone surrogate.
fn units_json(units: &[u32]) -> Value {
    match units.iter().map(|&u| char::from_u32(u)).collect::<Option<String>>() {
        Some(s) => Value::String(s),
        None => Value::String(format!("{PYX}s:{}", units_dumps(units))),
    }
}

/// The code points of a `WStr`'s JSON text.
fn wstr_units(text: &str) -> Vec<u32> {
    let mut p = PyJson { b: text.as_bytes(), s: text, i: 0 };
    p.string_units().unwrap_or_default()
}

/// A `WStr` as Rust text, each lone surrogate U+FFFD.
fn wstr_lossy(text: &str) -> String {
    wstr_units(text).into_iter().map(|u| char::from_u32(u).unwrap_or('\u{fffd}')).collect()
}

/// `str(Path)` of a path: a name that is not UTF-8 keeps its bytes as
/// surrogates (surrogateescape), as Python's file-system decoding does.
fn path_json(p: &Path) -> Value {
    use std::os::unix::ffi::OsStrExt;
    let bytes = p.as_os_str().as_bytes();
    if let Ok(s) = std::str::from_utf8(bytes) {
        return json!(s);
    }
    let mut units = Vec::new();
    let mut rest = bytes;
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(s) => {
                units.extend(s.chars().map(|c| c as u32));
                break;
            }
            Err(e) => {
                let (good, bad) = rest.split_at(e.valid_up_to());
                units.extend(std::str::from_utf8(good).unwrap_or_default().chars().map(|c| c as u32));
                let n = e.error_len().unwrap_or(bad.len());
                units.extend(bad[..n].iter().map(|&b| 0xdc00 + b as u32));
                rest = &bad[n..];
            }
        }
    }
    units_json(&units)
}

/// `json.loads(text)` into a Value (see above); Err is the ValueError.
fn py_loads(text: &str) -> Result<Value, String> {
    let mut p = PyJson { b: text.as_bytes(), s: text, i: 0 };
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.i != p.b.len() {
        return Err(format!("Extra data: char {}", p.i));
    }
    Ok(v)
}

/// `json.loads(path.read_text())`, None where the Python's
/// `except (OSError, ValueError)` falls back.
fn py_read_json(path: &Path) -> Option<Value> {
    py_loads(&std::fs::read_to_string(path).ok()?).ok()
}

struct PyJson<'a> {
    b: &'a [u8],
    s: &'a str,
    i: usize,
}

impl PyJson<'_> {
    fn err<T>(&self, what: &str) -> Result<T, String> {
        Err(format!("{what}: char {}", self.i))
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn lit(&mut self, word: &str, v: Value) -> Result<Value, String> {
        if self.s[self.i..].starts_with(word) {
            self.i += word.len();
            Ok(v)
        } else {
            self.err("Expecting value")
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, String> {
        // Python gives up on deep nesting too (RecursionError).
        if depth > 900 {
            return self.err("maximum recursion depth exceeded");
        }
        match self.peek() {
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => Ok(units_json(&self.string_units()?)),
            Some(b'n') => self.lit("null", Value::Null),
            Some(b't') => self.lit("true", json!(true)),
            Some(b'f') => self.lit("false", json!(false)),
            Some(b'N') => self.lit("NaN", float_json(f64::NAN)),
            Some(b'I') => self.lit("Infinity", float_json(f64::INFINITY)),
            Some(b'-') if self.s[self.i..].starts_with("-Infinity") => {
                self.lit("-Infinity", float_json(f64::NEG_INFINITY))
            }
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => self.err("Expecting value"),
        }
    }

    fn digits(&mut self) {
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.i += 1;
        }
    }

    fn number(&mut self) -> Result<Value, String> {
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
            return Ok(float_json(text.parse().unwrap_or(f64::NAN)));
        }
        let digits = self.s[start..int_end].trim_start_matches('-');
        if digits.len() > 4300 {
            return Err(format!(
                "Exceeds the limit (4300 digits) for integer string conversion: value has {} digits",
                digits.len()
            ));
        }
        Ok(int_json(if text == "-0" { "0" } else { text }))
    }

    fn hex4(&self, at: usize) -> Option<u32> {
        let h = self.s.get(at..at + 4)?;
        if !h.bytes().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        u32::from_str_radix(h, 16).ok()
    }

    /// A JSON string's code points; a lone surrogate escape stays one.
    fn string_units(&mut self) -> Result<Vec<u32>, String> {
        self.i += 1;
        let mut out: Vec<u32> = Vec::new();
        loop {
            let chunk = self.i;
            while matches!(self.peek(), Some(c) if c != b'"' && c != b'\\' && c >= 0x20) {
                self.i += 1;
            }
            out.extend(self.s[chunk..self.i].chars().map(|c| c as u32));
            match self.peek() {
                None => return self.err("Unterminated string starting at"),
                Some(b'"') => {
                    self.i += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    let esc = self.b.get(self.i + 1).copied();
                    self.i += 2;
                    let c = match esc {
                        Some(b'"') => '"' as u32,
                        Some(b'\\') => '\\' as u32,
                        Some(b'/') => '/' as u32,
                        Some(b'b') => 8,
                        Some(b'f') => 12,
                        Some(b'n') => 10,
                        Some(b'r') => 13,
                        Some(b't') => 9,
                        Some(b'u') => {
                            let Some(mut c) = self.hex4(self.i) else {
                                return self.err("Invalid \\uXXXX escape");
                            };
                            self.i += 4;
                            if (0xd800..=0xdbff).contains(&c) && self.s[self.i..].starts_with("\\u") {
                                if let Some(lo) =
                                    self.hex4(self.i + 2).filter(|lo| (0xdc00..=0xdfff).contains(lo))
                                {
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
                    out.push(c);
                }
                Some(_) => return self.err("Invalid control character at"),
            }
        }
    }

    fn object(&mut self, depth: usize) -> Result<Value, String> {
        self.i += 1;
        let mut items = Map::new();
        self.ws();
        if self.peek() == Some(b'}') {
            self.i += 1;
            return Ok(Value::Object(items));
        }
        loop {
            if self.peek() != Some(b'"') {
                return self.err("Expecting property name enclosed in double quotes");
            }
            let key = match units_json(&self.string_units()?) {
                Value::String(k) => k,
                _ => String::new(),
            };
            self.ws();
            if self.peek() != Some(b':') {
                return self.err("Expecting ':' delimiter");
            }
            self.i += 1;
            self.ws();
            let v = self.value(depth + 1)?;
            // A repeated key keeps its first place and takes the last value,
            // as a dict does.
            items.insert(key, v);
            self.ws();
            match self.peek() {
                Some(b',') => {
                    self.i += 1;
                    self.ws();
                }
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Value::Object(items));
                }
                _ => return self.err("Expecting ',' delimiter"),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Value, String> {
        self.i += 1;
        let mut items = Vec::new();
        self.ws();
        if self.peek() == Some(b']') {
            self.i += 1;
            return Ok(Value::Array(items));
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
                    return Ok(Value::Array(items));
                }
                _ => return self.err("Expecting ',' delimiter"),
            }
        }
    }
}

/// `json.dumps(value, indent=indent)` with Python's default separators, the
/// values above written back as the tokens they were read from.
fn py_dumps(value: &Value, indent: Option<usize>) -> String {
    let mut out = String::new();
    dump_into(&mut out, value, indent, 0);
    out
}

fn dump_str(out: &mut String, s: &str) {
    match exotic_str(s) {
        Some(Exotic::Float(f)) => out.push_str(&util::py_float_repr(f)),
        Some(Exotic::Int(d)) => out.push_str(d),
        Some(Exotic::WStr(t)) => out.push_str(t),
        None => out.push_str(&util::py_json_string(s)),
    }
}

fn dump_into(out: &mut String, v: &Value, indent: Option<usize>, level: usize) {
    let nl = |out: &mut String, lvl: usize| {
        if let Some(n) = indent {
            out.push('\n');
            out.push_str(&" ".repeat(n * lvl));
        }
    };
    let item_sep = if indent.is_some() { "," } else { ", " };
    match v {
        Value::Array(items) if !items.is_empty() => {
            out.push('[');
            for (i, x) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(item_sep);
                }
                nl(out, level + 1);
                dump_into(out, x, indent, level + 1);
            }
            nl(out, level);
            out.push(']');
        }
        Value::Object(map) if !map.is_empty() => {
            out.push('{');
            for (i, (k, x)) in map.iter().enumerate() {
                if i > 0 {
                    out.push_str(item_sep);
                }
                nl(out, level + 1);
                // A key is always a str (an exotic one only ever a WStr).
                dump_str(out, k);
                out.push_str(": ");
                dump_into(out, x, indent, level + 1);
            }
            nl(out, level);
            out.push('}');
        }
        Value::String(s) => dump_str(out, s),
        other => out.push_str(&util::dumps(other, None, false)),
    }
}

/// For the parity runner: the JSON between the two sides cannot carry NaN,
/// the infinities or a lone surrogate either, so both sides send those as
/// text -- a float's repr ('inf'), a str's `json.dumps` -- and an int past
/// 64 bits as the nearest float (the runner compares numbers numerically).
fn wire(v: Value) -> Value {
    match exotic(&v) {
        Some(Exotic::Float(f)) => return json!(py_float_str(f)),
        Some(Exotic::Int(d)) => {
            return d.parse::<f64>().ok().and_then(serde_json::Number::from_f64).map_or(Value::Null, Value::Number)
        }
        Some(Exotic::WStr(t)) => return json!(t),
        None => {}
    }
    match v {
        Value::Array(a) => Value::Array(a.into_iter().map(wire).collect()),
        Value::Object(o) => Value::Object(
            o.into_iter()
                .map(|(k, x)| {
                    let k = match exotic_str(&k) {
                        Some(Exotic::WStr(t)) => t.to_string(),
                        _ => k,
                    };
                    (k, wire(x))
                })
                .collect(),
        ),
        other => other,
    }
}

/// The zero of every run of Unicode decimal digits (category Nd): Python's
/// `\d`, `int()` and `float()` take any of them, not only ASCII.
const ND_ZEROS: &[u32] = &[
    0x30, 0x660, 0x6F0, 0x7C0, 0x966, 0x9E6, 0xA66, 0xAE6, 0xB66, 0xBE6, 0xC66, 0xCE6, 0xD66,
    0xDE6, 0xE50, 0xED0, 0xF20, 0x1040, 0x1090, 0x17E0, 0x1810, 0x1946, 0x19D0, 0x1A80, 0x1A90,
    0x1B50, 0x1BB0, 0x1C40, 0x1C50, 0xA620, 0xA8D0, 0xA900, 0xA9D0, 0xA9F0, 0xAA50, 0xABF0,
    0xFF10, 0x104A0, 0x10D30, 0x10D40, 0x11066, 0x110F0, 0x11136, 0x111D0, 0x112F0, 0x11450,
    0x114D0, 0x11650, 0x116C0, 0x116D0, 0x116DA, 0x11730, 0x118E0, 0x11950, 0x11BF0, 0x11C50,
    0x11D50, 0x11DA0, 0x11F50, 0x16130, 0x16A60, 0x16AC0, 0x16B50, 0x16D70, 0x1CCF0, 0x1D7CE,
    0x1D7D8, 0x1D7E2, 0x1D7EC, 0x1D7F6, 0x1E140, 0x1E2F0, 0x1E4F0, 0x1E5F1, 0x1E950, 0x1FBF0,
];

/// Every Unicode decimal digit as its ASCII digit; nothing else changes.
fn ascii_digits(s: &str) -> String {
    s.chars()
        .map(|c| {
            let u = c as u32;
            match ND_ZEROS.iter().find(|z| u >= **z && u < **z + 10) {
                Some(z) => char::from(b'0' + (u - z) as u8),
                None => c,
            }
        })
        .collect()
}

/// `float(text)` for a string: Python accepts single underscores between
/// digits, which Rust's parser does not.
fn py_float_text(s: &str) -> Option<f64> {
    let t = ascii_digits(py_strip(s));
    if t.contains('_') {
        let chars: Vec<char> = t.chars().collect();
        for (i, c) in chars.iter().enumerate() {
            if *c == '_' {
                let ok = i > 0
                    && i + 1 < chars.len()
                    && chars[i - 1].is_ascii_digit()
                    && chars[i + 1].is_ascii_digit();
                if !ok {
                    return None;
                }
            }
        }
    }
    let t: String = t.chars().filter(|c| *c != '_').collect();
    t.parse::<f64>().ok()
}

/// `int(text)` for a string: surrounding whitespace, a sign, any Unicode
/// decimal digits and single underscores between them, as Python takes
/// them. The canonical decimal digits, or None where Python raises.
fn py_int_text(s: &str) -> Option<String> {
    let t = ascii_digits(py_strip(s));
    let (neg, body) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, &t[..]),
    };
    let b = body.as_bytes();
    if b.is_empty() || b[0] == b'_' || b[b.len() - 1] == b'_' || body.contains("__") {
        return None;
    }
    if !b.iter().all(|c| c.is_ascii_digit() || *c == b'_') {
        return None;
    }
    let digits: String = body.chars().filter(|c| *c != '_').collect();
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return Some("0".into());
    }
    if digits.len() > 4300 {
        return None; // the int-string conversion limit, a ValueError
    }
    Some(if neg { format!("-{digits}") } else { digits.to_string() })
}

/// `float(value)` of a JSON value; None where Python would raise.
fn py_float_value(v: &Value) -> Option<f64> {
    match exotic(v) {
        Some(Exotic::Float(f)) => return Some(f),
        // float(int) of a huge int: the nearest float (OverflowError past
        // the float range, None here like the others).
        Some(Exotic::Int(d)) => return d.parse::<f64>().ok().filter(|f| f.is_finite()),
        // A str with a lone surrogate is never a number.
        Some(Exotic::WStr(_)) => return None,
        None => {}
    }
    match v {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::String(s) => py_float_text(s),
        _ => None,
    }
}

/// `float(d.get(key) or 0)`; Err where Python raises (ValueError/TypeError).
fn float_or_zero(d: &Map<String, Value>, key: &str) -> Result<f64, String> {
    match d.get(key) {
        Some(v) if truthy(v) => py_float_value(v)
            .ok_or_else(|| format!("could not convert to float: {}", py_str(v))),
        _ => Ok(0.0),
    }
}

/// `d.get(key, default)` as text (a non-string value as Python's `str()`).
fn get_str(d: &Map<String, Value>, key: &str, default: &str) -> String {
    match d.get(key) {
        None => default.to_string(),
        Some(v) => py_str(v),
    }
}

/// `(d.get(key) or "")` as text.
fn get_str_or(d: &Map<String, Value>, key: &str) -> String {
    match d.get(key) {
        Some(v) if truthy(v) => py_str(v),
        _ => String::new(),
    }
}

/// `str(Path(s))`: pathlib collapses repeated separators, drops `.`
/// components, keeps exactly two leading slashes, and `Path("")` is `.`.
fn py_path(s: &str) -> String {
    if s.is_empty() {
        return ".".into();
    }
    let abs = s.starts_with('/');
    let double = s.starts_with("//") && !s.starts_with("///");
    let parts: Vec<&str> = s.split('/').filter(|p| !p.is_empty() && *p != ".").collect();
    let body = parts.join("/");
    if double {
        format!("//{body}")
    } else if abs {
        format!("/{body}")
    } else if body.is_empty() {
        ".".into()
    } else {
        body
    }
}

/// A GET that returns parsed JSON, the way `urllib.request.urlopen` +
/// `json.loads(r.read().decode())` does; any failure is an error.
///
/// urllib's `timeout` bounds each socket operation (the connect, each read),
/// not the whole request: a slow download that keeps arriving is not cut
/// off. ureq's timeouts are per phase, so the connect and the response head
/// get the timeout as their own budget, and the body is read on a thread
/// with the timeout applied to every chunk.
fn http_json(url: &str, timeout: u64) -> Result<Value, Box<dyn std::error::Error>> {
    use std::io::Read;
    let t = std::time::Duration::from_secs(timeout);
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(t))
        .timeout_send_request(Some(t))
        .timeout_recv_response(Some(t))
        .build()
        .into();
    let r = agent.get(url).call()?;
    let mut reader = r.into_body().into_with_config().limit(u64::MAX).reader();
    let (tx, rx) = std::sync::mpsc::channel::<std::io::Result<Vec<u8>>>();
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    let _ = tx.send(Ok(Vec::new()));
                    return;
                }
                Ok(n) => {
                    if tx.send(Ok(buf[..n].to_vec())).is_err() {
                        return;
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                    return;
                }
            }
        }
    });
    let mut body = Vec::new();
    loop {
        match rx.recv_timeout(t) {
            Ok(Ok(chunk)) if chunk.is_empty() => break,
            Ok(Ok(chunk)) => body.extend_from_slice(&chunk),
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => return Err("timed out".into()),
        }
    }
    Ok(py_loads(std::str::from_utf8(&body)?)?)
}

// ------------------------------------------------------------------ module --

/// 'security', 'core', or '' -- empty for the ordinary majority.
pub fn criticality(name: &str) -> String {
    let n = name.strip_suffix("-bin").unwrap_or(name);
    let base = n.strip_suffix("-git").unwrap_or(n).to_lowercase();
    let lower = name.to_lowercase();
    for (kind, names) in CRITICAL {
        if names.contains(&base.as_str()) || names.contains(&lower.as_str()) {
            return kind.to_string();
        }
    }
    String::new()
}

pub const STALE_DAYS: i64 = 180;

// Things that legitimately stop changing. A font or an icon theme two years
// old is finished, not abandoned, and flagging it would train the user to
// ignore the warning that matters.
pub const FINISHED_PREFIXES: &[&str] = &["ttf-", "otf-", "noto-", "adobe-source-"];
pub const FINISHED_SUFFIXES: &[&str] =
    &["-font", "-fonts", "-icon-theme", "-themes", "-cursor-theme"];

/// What the AUR says about one of its packages (one value of `aur_health`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AurHealth {
    pub out_of_date: bool,
    pub last_modified: f64,
    pub maintainer: String,
}

/// `{icon, categories}` read from a package's .desktop file (one value of
/// `desktop_map`). `Default` is the Python's `{}` for a package with none.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DesktopEntry {
    pub icon: String,
    pub categories: String,
}

/// `_staleness`: 'orphaned' | 'flagged' | 'stale' | '' — strongest signal wins.
///
/// Only the first two are authoritative: a human either abandoned the package
/// or flagged it out of date. 'stale' is a build-date proxy and is deliberately
/// limited to real applications, because libraries and fonts sit untouched for
/// years as a matter of course.
pub fn staleness(
    name: &str,
    source: &str,
    aur: &BTreeMap<String, AurHealth>,
    desk: &DesktopEntry,
    meta: &BTreeMap<String, String>,
) -> String {
    let rec = aur.get(name);
    if source == AUR && rec.is_some_and(|r| r.maintainer.is_empty()) {
        return "orphaned".into();
    }
    if rec.is_some_and(|r| r.out_of_date) {
        return "flagged".into();
    }

    let lower = name.to_lowercase();
    if FINISHED_PREFIXES.iter().any(|p| lower.starts_with(p))
        || FINISHED_SUFFIXES.iter().any(|s| lower.ends_with(s))
    {
        return String::new();
    }
    if desk.icon.is_empty() && desk.categories.is_empty() {
        return String::new(); // library or CLI tool, not an app
    }
    let built = date_ts(meta.get("built").map(String::as_str).unwrap_or(""));
    if built != 0.0 && (util::now() - built) / 86400.0 > STALE_DAYS as f64 {
        return "stale".into();
    }
    String::new()
}

/// Ask the AUR about its own packages: flagged out-of-date, and when the
/// packaging was last touched.
///
/// This is the only *authoritative* staleness signal available -- a human
/// maintainer flags out-of-date, and LastModified is real packaging activity.
/// Build age (below) is only a proxy. One RPC call covers every package.
/// Offline returns an empty map: offline is not a reason to show nothing.
/// Err where the Python raises after the fetch (a reply of an unexpected
/// shape: not an object, `results` not a list of objects, a LastModified
/// that is not a number); `packages` then fails, as in the Python.
pub fn try_aur_health(names: &[String]) -> Result<BTreeMap<String, AurHealth>, String> {
    if names.is_empty() {
        return Ok(BTreeMap::new());
    }
    let query = names
        .iter()
        .map(|n| format!("arg[]={}", py_quote(n)))
        .collect::<Vec<_>>()
        .join("&");
    let url = format!("https://aur.archlinux.org/rpc/v5/info?{query}");
    let data = match http_json(&url, 15) {
        Ok(d) => d,
        Err(_) => return Ok(BTreeMap::new()), // offline is not a reason to show nothing
    };
    let mut out = BTreeMap::new();
    let Value::Object(data) = data else {
        return Err(format!("'{}' object has no attribute 'get'", py_type(&data)));
    };
    for rec in py_iter(data.get("results").unwrap_or(&json!([])))? {
        let Value::Object(rec) = &rec else {
            return Err(format!("'{}' object has no attribute 'get'", py_type(&rec)));
        };
        let name = match rec.get("Name") {
            Some(v @ (Value::Array(_) | Value::Object(_))) => {
                return Err(format!("unhashable type: '{}'", py_type(v)))
            }
            Some(v) => py_str(v),
            None => String::new(),
        };
        out.insert(
            name,
            AurHealth {
                out_of_date: rec.get("OutOfDate").is_some_and(truthy),
                last_modified: float_or_zero(rec, "LastModified")?,
                maintainer: get_str_or(rec, "Maintainer"),
            },
        );
    }
    Ok(out)
}

/// `try_aur_health` for callers that need a plain map: where the Python
/// raises, this answers `{}`.
pub fn aur_health(names: &[String]) -> BTreeMap<String, AurHealth> {
    try_aur_health(names).unwrap_or_default()
}

/// '1737.27 KiB' -> 1.70. Sorting needs a number: as text, '9 KiB' sorts
/// above '10 MiB'.
pub fn size_mb(value: &str) -> f64 {
    let parts = py_split(value);
    if parts.len() < 2 {
        return 0.0;
    }
    // The Python builds the whole unit table first, so a bad number is a
    // ValueError whatever the unit.
    let Some(num) = py_float_text(parts[0]) else { return 0.0 };
    match parts[1] {
        "B" => num / 1048576.0,
        "KiB" => num / 1024.0,
        "MiB" => num,
        "GiB" => num * 1024.0,
        _ => 0.0,
    }
}

/// `datetime.strptime(text, fmt)` for the two formats `date_ts` tries, as a
/// naive local datetime.
fn strptime(text: &str, with_time: bool) -> Option<chrono::NaiveDateTime> {
    use std::sync::LazyLock;
    // _strptime's regexes for %d %b %Y [%I:%M:%S %p], case-insensitive, with
    // format whitespace as \s+.
    static FULL: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(concat!(
            r"(?i)^(3[01]|[12]\d|0[1-9]|[1-9]| [1-9])\s+",
            r"(jan|feb|mar|apr|may|jun|jul|aug|sep|oct|nov|dec)\s+(\d\d\d\d)\s+",
            r"(1[0-2]|0[1-9]|[1-9]| [1-9]):([0-5]\d|\d):(6[0-1]|[0-5]\d|\d)\s+(am|pm)$"
        ))
        .unwrap()
    });
    static DATE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(concat!(
            r"(?i)^(3[01]|[12]\d|0[1-9]|[1-9]| [1-9])\s+",
            r"(jan|feb|mar|apr|may|jun|jul|aug|sep|oct|nov|dec)\s+(\d\d\d\d)$"
        ))
        .unwrap()
    });
    const MONTHS: [&str; 12] =
        ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    // `\d` is any Unicode decimal digit, in Python's re as in this one, and
    // int() reads them all; the literal ranges ([12], 0[1-9], ...) are ASCII.
    let caps = if with_time { FULL.captures(text)? } else { DATE.captures(text)? };
    let num = |i: usize| -> u32 { ascii_digits(caps[i].trim()).parse().unwrap_or(0) };
    let day = num(1);
    let month = MONTHS.iter().position(|m| *m == caps[2].to_lowercase())? as u32 + 1;
    let year = num(3) as i32;
    if year < 1 {
        return None; // datetime: year 0 is out of range
    }
    let (mut hour, minute, second) = if with_time { (num(4), num(5), num(6)) } else { (0, 0, 0) };
    if with_time {
        let pm = caps[7].eq_ignore_ascii_case("pm");
        if pm && hour != 12 {
            hour += 12;
        } else if !pm && hour == 12 {
            hour = 0;
        }
    }
    chrono::NaiveDate::from_ymd_opt(year, month, day)?.and_hms_opt(hour, minute, second)
}

/// `naive.timestamp()`: the naive datetime read as local time, by CPython's
/// own algorithm (`datetime._mktime`, fold=0): solve `t = local(u)` for u,
/// taking the earlier offset in a fold and the later instant in a gap.
/// None where CPython raises: `local()` builds a datetime from
/// `localtime(u)`, and a year outside 1..=9999 there is a ValueError
/// (1 Jan 0001 probes the day before it, which is year 0).
fn local_timestamp(naive: chrono::NaiveDateTime) -> Option<f64> {
    use chrono::{Datelike, Offset, TimeZone};
    const MAX_FOLD_SECONDS: i64 = 24 * 3600;
    let t = naive.and_utc().timestamp();
    let local = |u: i64| -> Option<i64> {
        let utc = chrono::DateTime::from_timestamp(u, 0)?.naive_utc();
        let off = chrono::Local.offset_from_utc_datetime(&utc).fix().local_minus_utc() as i64;
        let lt = utc + chrono::Duration::seconds(off);
        if !(1..=9999).contains(&lt.year()) {
            return None;
        }
        Some(u + off)
    };
    let a = local(t)? - t;
    let u1 = t - a;
    let t1 = local(u1)?;
    let b = if t1 == t {
        // We found one solution, but it may not be the one we need: look
        // for an earlier one (fold=0).
        let u2 = u1 - MAX_FOLD_SECONDS;
        let b = local(u2)? - u2;
        if a == b {
            return Some(u1 as f64);
        }
        b
    } else {
        t1 - u1
    };
    let u2 = t - b;
    let t2 = local(u2)?;
    if t2 == t {
        return Some(u2 as f64);
    }
    if t1 == t {
        return Some(u1 as f64);
    }
    // Both offsets found, but neither t - a nor t - b is a solution: t is
    // in the gap.
    Some(u1.max(u2) as f64)
}

/// pacman's 'Fri 16 Oct 2026 04:08:05 PM PDT' -> epoch seconds, so the
/// column sorts chronologically instead of by the weekday name.
pub fn date_ts(value: &str) -> f64 {
    if value.is_empty() {
        return 0.0;
    }
    let words = py_split(value);
    for (with_time, count) in [(true, 6usize), (false, 4usize)] {
        let end = count.min(words.len());
        let text = if words.len() > 1 { words[1..end.max(1)].join(" ") } else { String::new() };
        // (A ValueError from .timestamp() is caught like strptime's.)
        if let Some(ts) = strptime(&text, with_time).and_then(local_timestamp) {
            return ts;
        }
    }
    0.0
}

/// One row of the inventory, from any source.
///
/// The Python dataclass does not check its field types, so a malformed
/// registry or manifest puts a None or an int in `name`/`version`/... and
/// that is what the row then holds. The Rust fields stay `String` (their
/// `str()`, for display), and the value itself, when it is not a plain str,
/// is kept in `extra` under `RAW_FIELDS`; serialising the row writes it
/// back in place of the text and leaves that key out of `extra`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct App {
    pub name: String,
    pub version: String,
    pub source: String,
    pub description: String,
    /// new version if one is available
    pub update: String,
    /// what to launch, when launchable
    pub exec_path: String,
    /// Mixed-type per-source details (`dict` in the Python).
    pub extra: Map<String, Value>,
}

/// The `extra` key holding the Python values of fields that are not a
/// plain str (two noncharacters no JSON the app reads contains).
const RAW_FIELDS: &str = "\u{10FFFF}\u{FDD0}py:fields";

const APP_FIELDS: [&str; 6] = ["name", "version", "source", "description", "update", "exec_path"];

impl App {
    fn field_mut(&mut self, field: &str) -> Option<&mut String> {
        Some(match field {
            "name" => &mut self.name,
            "version" => &mut self.version,
            "source" => &mut self.source,
            "description" => &mut self.description,
            "update" => &mut self.update,
            "exec_path" => &mut self.exec_path,
            _ => return None,
        })
    }

    /// Set a field to a Python value: its `str()` as the text, and the value
    /// itself kept when it is not a plain str.
    fn set_value(&mut self, field: &str, v: &Value) {
        let plain = matches!(v, Value::String(s) if exotic_str(s).is_none());
        if let Some(f) = self.field_mut(field) {
            // A falsy update (None, 0) shows as no update, as `has_update`
            // reads it.
            *f = if field == "update" && !truthy(v) { String::new() } else { py_str(v) };
        }
        if plain {
            if let Some(Value::Object(r)) = self.extra.get_mut(RAW_FIELDS) {
                r.remove(field);
            }
            return;
        }
        if let Value::Object(r) =
            self.extra.entry(RAW_FIELDS.to_string()).or_insert_with(|| Value::Object(Map::new()))
        {
            r.insert(field.to_string(), v.clone());
        }
    }

    /// `set_value` for several fields; returns the row.
    fn with_values(mut self, fields: &[(&str, &Value)]) -> App {
        for (k, v) in fields {
            self.set_value(k, v);
        }
        self
    }

    /// `extra` as the Python dict holds it (without `RAW_FIELDS`).
    pub fn extra_dict(&self) -> Map<String, Value> {
        let mut m = self.extra.clone();
        m.remove(RAW_FIELDS);
        m
    }

    /// `App(name=..., version=..., source=...)` with every other field at
    /// its default; set the rest with struct update syntax.
    pub fn new(name: &str, version: &str, source: &str) -> App {
        App { name: name.to_string(), version: version.to_string(), source: source.to_string(), ..Default::default() }
    }

    /// `@property has_update`.
    pub fn has_update(&self) -> bool {
        // `bool(update)`, of the Python value when it is not a str.
        if let Some(v) = self.extra.get(RAW_FIELDS).and_then(|r| r.get("update")) {
            return truthy(v);
        }
        !self.update.is_empty()
    }
}

impl Serialize for App {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let raw = self.extra.get(RAW_FIELDS).and_then(Value::as_object);
        let text = [&self.name, &self.version, &self.source, &self.description, &self.update, &self.exec_path];
        let mut st = s.serialize_struct("App", 7)?;
        for (k, t) in APP_FIELDS.iter().zip(text) {
            match raw.and_then(|r| r.get(*k)) {
                Some(v) => st.serialize_field(k, v)?,
                None => st.serialize_field(k, t)?,
            }
        }
        if raw.is_some() {
            st.serialize_field("extra", &self.extra_dict())?;
        } else {
            st.serialize_field("extra", &self.extra)?;
        }
        st.end()
    }
}

impl<'de> Deserialize<'de> for App {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<App, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Wire {
            name: Option<Value>,
            version: Option<Value>,
            source: Option<Value>,
            description: Option<Value>,
            update: Option<Value>,
            exec_path: Option<Value>,
            extra: Map<String, Value>,
        }
        let w = Wire::deserialize(d)?;
        let mut app = App { extra: w.extra, ..Default::default() };
        app.extra.remove(RAW_FIELDS);
        for (k, v) in APP_FIELDS.iter().zip([w.name, w.version, w.source, w.description, w.update, w.exec_path]) {
            if let Some(v) = v {
                app.set_value(k, &v);
            }
        }
        Ok(app)
    }
}

/// What `subprocess.run(cmd, capture_output=True, text=True, timeout=t)`
/// returns or raises.
#[derive(Debug)]
enum RunError {
    /// OSError: the program could not be started.
    Os,
    /// subprocess.TimeoutExpired.
    Timeout,
    /// UnicodeDecodeError (a ValueError): the output is not UTF-8.
    Decode(String),
    /// ValueError("embedded null byte"): an argument holds a NUL, which no
    /// argv can carry.
    Nul,
}

struct RunResult {
    code: Option<i32>,
    stdout: String,
}

/// Text mode's decoding: strict UTF-8, then universal newlines ("\r\n" and
/// a lone "\r" both become "\n").
fn py_decode(bytes: &[u8]) -> Result<String, RunError> {
    match std::str::from_utf8(bytes) {
        Ok(s) => Ok(s.replace("\r\n", "\n").replace('\r', "\n")),
        Err(e) => {
            let pos = e.valid_up_to();
            let why = if e.error_len().is_none() {
                "unexpected end of data"
            } else if (0x80..0xC2).contains(&bytes[pos]) || bytes[pos] >= 0xF5 {
                "invalid start byte"
            } else {
                "invalid continuation byte"
            };
            Err(RunError::Decode(format!(
                "'utf-8' codec can't decode byte 0x{:02x} in position {pos}: {why}",
                bytes[pos]
            )))
        }
    }
}

/// `subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)`.
///
/// Built here rather than on `util::run`, which decodes lossily and keeps
/// "\r": text mode raises on bytes that are not UTF-8 and translates
/// newlines. The timeout behaves as Python's does: on expiry the child is
/// killed and reaped and the call gives up at once, without waiting for a
/// grandchild that still holds the pipes open. stdin is inherited, as
/// subprocess.run leaves it.
fn subprocess_run<S: AsRef<std::ffi::OsStr>>(cmd: &[S], timeout: u64) -> Result<RunResult, RunError> {
    use std::io::Read;
    use std::os::unix::ffi::OsStrExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    // subprocess checks every argument before it forks.
    if cmd.iter().any(|a| a.as_ref().as_bytes().contains(&0)) {
        return Err(RunError::Nul);
    }
    let Some((prog, args)) = cmd.split_first() else { return Err(RunError::Os) };
    let mut child = Command::new(prog)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| RunError::Os)?;
    let deadline = Instant::now() + Duration::from_secs(timeout);
    let (tx, rx) = std::sync::mpsc::channel::<(bool, Vec<u8>)>();
    let pipes: [(bool, Option<Box<dyn Read + Send>>); 2] = [
        (true, child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>)),
        (false, child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>)),
    ];
    for (is_out, pipe) in pipes {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            let _ = tx.send((is_out, buf));
        });
    }
    drop(tx);
    let give_up = |child: &mut std::process::Child| {
        let _ = child.kill();
        let _ = child.wait();
        RunError::Timeout
    };
    let (mut out, mut err) = (None, None);
    while out.is_none() || err.is_none() {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok((true, b)) => out = Some(b),
            Ok((false, b)) => err = Some(b),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => return Err(give_up(&mut child)),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) => {
                if Instant::now() >= deadline {
                    return Err(give_up(&mut child));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => break None,
        }
    };
    let stdout = py_decode(&out.unwrap_or_default())?;
    py_decode(&err.unwrap_or_default())?;
    Ok(RunResult { code: status.and_then(|s| s.code()), stdout })
}

/// `_run`: run a command, returning stdout. Never raises: a missing or failing
/// helper should degrade one section of the list, not take the window down.
/// (Python default timeout: 25.)
///
/// A timeout is `subprocess.TimeoutExpired`, caught like any other failure,
/// so it answers "" rather than whatever was printed before the kill. Output
/// that is not UTF-8 is the UnicodeDecodeError `_run` does not catch: Err,
/// which takes the calling collector down with it, as in the Python.
///
/// A NUL byte in an argument is the ValueError subprocess raises before it
/// starts anything, which `_run` does not catch either: Err.
pub fn run(cmd: &[&str], timeout: u64) -> Result<String, String> {
    run_os(cmd, timeout)
}

/// `run` for arguments that are paths, which need not be UTF-8.
fn run_os<S: AsRef<std::ffi::OsStr>>(cmd: &[S], timeout: u64) -> Result<String, String> {
    match subprocess_run(cmd, timeout) {
        Ok(r) => Ok(r.stdout),
        Err(RunError::Os | RunError::Timeout) => Ok(String::new()),
        Err(RunError::Decode(e)) => Err(e),
        Err(RunError::Nul) => Err("embedded null byte".into()),
    }
}

/// `_FIELDS`: pacman -Qi field -> the key we keep it under.
pub const FIELDS: &[(&str, &str)] = &[
    ("Description", "description"),
    ("URL", "url"),
    ("Groups", "groups"),
    ("Installed Size", "size"),
    ("Build Date", "built"),
    ("Install Date", "installed"),
    ("Licenses", "license"),
];

fn field_key(key: &str) -> Option<&'static str> {
    FIELDS.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

fn argv<'a>(head: &[&'a str], names: &'a [String]) -> Vec<&'a str> {
    let mut v: Vec<&str> = head.to_vec();
    v.extend(names.iter().map(String::as_str));
    v
}

/// `_package_info`: full metadata for every package in ONE pacman call.
///
/// pacman -Qi emits "Field : value" blocks separated by blank lines. Values
/// can wrap onto continuation lines (long dependency lists), which is why a
/// naive line-per-field parse drops data -- continuation lines are appended
/// to the field already in progress.
///
/// Err: pacman's output is not UTF-8 (`_run`'s UnicodeDecodeError).
pub fn package_info(names: &[String]) -> Result<BTreeMap<String, BTreeMap<String, String>>, String> {
    if names.is_empty() {
        return Ok(BTreeMap::new());
    }
    let out = run(&argv(&["pacman", "-Qi"], names), 60)?;
    let mut info: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let (mut current, mut field): (Option<String>, Option<&str>) = (None, None);
    for line in py_splitlines(&out) {
        if py_strip(line).is_empty() {
            current = None;
            field = None;
            continue;
        }
        let cur_set = current.as_deref().is_some_and(|c| !c.is_empty());
        if line.starts_with(' ') && cur_set && field.is_some() {
            if let (Some(c), Some(f)) = (&current, field) {
                if let Some(entry) = info.get_mut(c).and_then(|m| m.get_mut(f)) {
                    entry.push(' ');
                    entry.push_str(py_strip(line));
                }
            }
            continue;
        }
        let Some((key, value)) = line.split_once(':') else { continue };
        let (key, value) = (py_strip(key), py_strip(value));
        if key == "Name" {
            current = Some(value.to_string());
            info.insert(value.to_string(), BTreeMap::new());
            field = None;
        } else if cur_set {
            if let Some(f) = field_key(key) {
                field = Some(f);
                if let Some(c) = &current {
                    info.entry(c.clone()).or_default().insert(f.to_string(), value.to_string());
                }
            }
        }
    }
    Ok(info)
}

/// `_desktop_map`: package -> {icon, categories} by reading the .desktop files it owns.
///
/// One `pacman -Ql` for the whole set, then read only the desktop files it
/// points at. Looking each package up individually would be far slower.
pub fn desktop_map(names: &[String]) -> Result<BTreeMap<String, DesktopEntry>, String> {
    if names.is_empty() {
        return Ok(BTreeMap::new());
    }
    // Insertion-ordered, like the Python dict.
    let mut owns: Vec<(String, Vec<String>)> = Vec::new();
    for line in py_splitlines(&run(&argv(&["pacman", "-Ql"], names), 60)?) {
        let (pkg, path) = line.split_once(' ').unwrap_or((line, ""));
        if path.ends_with(".desktop") && path.contains("/applications/") {
            match owns.iter_mut().find(|(p, _)| p == pkg) {
                Some((_, v)) => v.push(path.to_string()),
                None => owns.push((pkg.to_string(), vec![path.to_string()])),
            }
        }
    }

    let mut result = BTreeMap::new();
    for (pkg, paths) in owns {
        for path in paths {
            let text = match std::fs::read(&path) {
                Ok(b) => String::from_utf8_lossy(&b).into_owned(),
                Err(_) => continue,
            };
            let (mut icon, mut cats) = (String::new(), String::new());
            for line in py_splitlines(&text) {
                if line.starts_with("Icon=") && icon.is_empty() {
                    icon = py_strip(line.split_once('=').map(|x| x.1).unwrap_or("")).to_string();
                } else if line.starts_with("Categories=") && cats.is_empty() {
                    cats = py_strip(line.split_once('=').map(|x| x.1).unwrap_or("")).to_string();
                } else if line.starts_with("NoDisplay=true") {
                    icon.clear(); // helper entry, not a real app
                    cats.clear();
                    break;
                }
            }
            if !icon.is_empty() || !cats.is_empty() {
                result.insert(pkg.clone(), DesktopEntry { icon, categories: cats });
                break; // first real entry wins
            }
        }
    }
    Ok(result)
}

fn pending_from(out: &str) -> BTreeMap<String, String> {
    let mut pending = BTreeMap::new();
    for line in py_splitlines(out) {
        let parts = py_split(line);
        if parts.len() >= 4 {
            pending.insert(parts[0].to_string(), parts[3].to_string());
        }
    }
    pending
}

/// `_pending_repo`: name -> new version, from checkupdates (which never touches the db).
/// Err: the output is not UTF-8 (`_run`'s UnicodeDecodeError).
pub fn try_pending_repo() -> Result<BTreeMap<String, String>, String> {
    Ok(pending_from(&run(&["checkupdates"], 60)?))
}

/// `try_pending_repo` for callers that need a plain map: where the Python
/// raises, this answers `{}`.
pub fn pending_repo() -> BTreeMap<String, String> {
    try_pending_repo().unwrap_or_default()
}

/// The version pacman's own sync database offers for `name`, or ''.
///
/// checkupdates reads a private, freshly synced copy of the database, so it
/// can see a release the system database has not heard of yet. Installing
/// one package without -y uses the system copy, and against that copy the
/// package is already current -- pacman skips it and reports success.
///
/// Err where the Python raises: output that is not UTF-8, or a "Version"
/// line without a colon (IndexError).
pub fn try_synced_version(name: &str) -> Result<String, String> {
    for line in py_splitlines(&run(&["env", "LC_ALL=C", "pacman", "-Si", name], 25)?) {
        if line.starts_with("Version") {
            let Some((_, v)) = line.split_once(':') else {
                return Err("list index out of range".into());
            };
            return Ok(py_strip(v).to_string());
        }
    }
    Ok(String::new())
}

/// `try_synced_version` for callers that need a plain answer: where the
/// Python raises, this answers ''.
pub fn synced_version(name: &str) -> String {
    try_synced_version(name).unwrap_or_default()
}

/// `_pending_aur`: name -> new version, from `yay -Qua`.
/// Err: the output is not UTF-8 (`_run`'s UnicodeDecodeError).
pub fn try_pending_aur() -> Result<BTreeMap<String, String>, String> {
    Ok(pending_from(&run(&["yay", "-Qua"], 90)?))
}

/// `try_pending_aur` for callers that need a plain map: where the Python
/// raises, this answers `{}`.
pub fn pending_aur() -> BTreeMap<String, String> {
    try_pending_aur().unwrap_or_default()
}

/// Omarchy's own packages: the distro updating itself, which deserves to be
/// visible rather than buried as somebody's dependency.
pub const OMARCHY_PREFIX: &str = "omarchy";

/// `dict(line.split()[:2] for line in out.splitlines() if line.split())`.
/// Err: a one-word line, the ValueError `dict()` raises on a 1-element pair.
fn version_map(out: &str) -> Result<HashMap<String, String>, String> {
    let mut versions = HashMap::new();
    let lines = py_splitlines(out).into_iter().map(py_split).filter(|p| !p.is_empty());
    for (i, parts) in lines.enumerate() {
        if parts.len() < 2 {
            return Err(format!(
                "dictionary update sequence element #{i} has length 1; 2 is required"
            ));
        }
        versions.insert(parts[0].to_string(), parts[1].to_string());
    }
    Ok(versions)
}

/// Explicitly-installed packages, plus anything with an update waiting.
///
/// Listing only explicit installs keeps the dependency closure out of the
/// way -- nobody installed libjpeg on purpose. But a dependency with an
/// update pending is still something the user has to act on, and omarchy
/// itself arrives that way, so pending updates are always included.
///
/// Err where the Python raises (output that is not UTF-8, a one-word
/// `pacman -Q` line, an AUR reply of the wrong shape); `collect` then drops
/// the section.
pub fn packages() -> Result<Vec<App>, String> {
    let words = |s: String| -> Vec<String> { py_split(&s).into_iter().map(String::from).collect() };
    let mut repo_names = words(run(&["pacman", "-Qentq"], 25)?);
    let mut aur_names = words(run(&["pacman", "-Qemq"], 25)?);
    let versions = version_map(&run(&["pacman", "-Q"], 25)?)?;
    // Pull in any non-explicit package that has an update waiting, so the
    // list can never disagree with "Update all".
    let (up_repo_pre, up_aur_pre) = (try_pending_repo()?, try_pending_aur()?);
    let known: HashSet<String> = repo_names.iter().chain(aur_names.iter()).cloned().collect();
    let foreign: HashSet<String> = words(run(&["pacman", "-Qmq"], 25)?).into_iter().collect();
    // (The Python walks a set here, in hash order; sorted is one fixed order.)
    let pending_names: BTreeSet<String> =
        up_repo_pre.keys().chain(up_aur_pre.keys()).filter(|n| !known.contains(*n)).cloned().collect();
    for name in pending_names {
        if foreign.contains(&name) {
            aur_names.push(name);
        } else {
            repo_names.push(name);
        }
    }

    let all_names: Vec<String> = repo_names.iter().chain(aur_names.iter()).cloned().collect();
    let info = package_info(&all_names)?;
    let desktops = desktop_map(&all_names)?;
    let aur = try_aur_health(&aur_names)?;
    let (up_repo, up_aur) = (up_repo_pre, up_aur_pre);
    let explicit: HashSet<String> = words(run(&["pacman", "-Qentq"], 25)?)
        .into_iter()
        .chain(words(run(&["pacman", "-Qemq"], 25)?))
        .collect();

    // "Security update" means the update fixes a known vulnerability in the
    // installed version, per the Arch Security Tracker. Only when the tracker
    // cannot be reached does the old guess-by-name stand in.
    let vulns = crate::security::vulnerabilities(Some(&versions), None);
    let vuln_map = vulns.as_ref().map(|v| crate::security::by_package(v));

    let critical_for = |name: &str, pend: &str| -> String {
        if pend.is_empty() {
            return String::new();
        }
        let Some(vm) = &vuln_map else { return criticality(name) };
        let none = Vec::new();
        if !crate::security::fixed_by(vm.get(name).unwrap_or(&none), pend).is_empty() {
            return "security".into();
        }
        if criticality(name) == "core" { "core".into() } else { String::new() }
    };

    let empty_meta = BTreeMap::new();
    let empty_desk = DesktopEntry::default();
    let mut apps = Vec::new();
    for (names, source, pending) in [(&repo_names, REPO, &up_repo), (&aur_names, AUR, &up_aur)] {
        for n in names {
            let meta = info.get(n).unwrap_or(&empty_meta);
            let desk = desktops.get(n).unwrap_or(&empty_desk);
            let m = |k: &str| meta.get(k).cloned().unwrap_or_default();
            let pend = pending.get(n).cloned().unwrap_or_default();
            let version = versions.get(n).cloned();
            // Omarchy's own packages are the distro updating itself; grouping
            // them under System stops them reading as ordinary apps.
            let row_source = if n.starts_with(OMARCHY_PREFIX) { SYSTEM } else { source };
            let vulns_json: Vec<Value> = vuln_map
                .as_ref()
                .and_then(|vm| vm.get(n))
                .map(|vs| {
                    vs.iter()
                        .map(|v| json!({"group": v.group, "severity": v.severity,
                                        "kind": v.kind, "fixed": v.fixed,
                                        "cves": v.cves}))
                        .collect()
                })
                .unwrap_or_default();
            let behind = if !pend.is_empty() {
                let (n, kind) = versions_behind_big(version.as_deref().unwrap_or(""), &pend);
                (big_json(&n), kind)
            } else {
                (json!(0), String::new())
            };
            let rec = aur.get(n);
            let mut extra = Map::new();
            extra.insert("icon".into(), json!(desk.icon));
            extra.insert("categories".into(), json!(desk.categories));
            extra.insert("size".into(), json!(m("size")));
            extra.insert("installed".into(), json!(m("installed")));
            extra.insert("built".into(), json!(m("built")));
            extra.insert("url".into(), json!(m("url")));
            extra.insert("license".into(), json!(m("license")));
            // A package with a .desktop entry is something the user
            // actually launches, rather than a toolchain or library
            // pulled in explicitly. Worth distinguishing in the list.
            extra.insert("is_app".into(), json!(!desk.icon.is_empty() || !desk.categories.is_empty()));
            extra.insert("dependency".into(), json!(!explicit.contains(n)));
            extra.insert("real_source".into(), json!(source));
            // Numeric twins of size/installed, purely so the columns
            // can sort on magnitude rather than on their text form.
            extra.insert("critical".into(), json!(critical_for(n, &pend)));
            extra.insert("vulns".into(), Value::Array(vulns_json));
            extra.insert("behind".into(), json!([behind.0, behind.1]));
            extra.insert("maintainer".into(), json!(rec.map(|r| r.maintainer.clone()).unwrap_or_default()));
            extra.insert("orphaned".into(), json!(source == AUR && rec.is_some_and(|r| r.maintainer.is_empty())));
            extra.insert("flagged".into(), json!(rec.is_some_and(|r| r.out_of_date)));
            extra.insert("stale".into(), json!(staleness(n, source, &aur, desk, meta)));
            extra.insert("size_mb".into(), json!(size_mb(&m("size"))));
            extra.insert("installed_ts".into(), json!(date_ts(&m("installed"))));
            extra.insert("built_ts".into(), json!(date_ts(&m("built"))));
            apps.push(App {
                name: n.clone(),
                version: version.unwrap_or_else(|| "?".into()),
                source: row_source.to_string(),
                description: m("description"),
                update: pend,
                exec_path: n.clone(),
                extra,
            });
        }
    }
    Ok(apps)
}

// ---------------------------------------------------------------- appimages --

/// `_load_registry`: the AppImage registry as the Python returns it --
/// whatever JSON the file holds (a list stays a list), `{}` when it is
/// missing or unreadable.
pub fn load_registry_value() -> Value {
    py_read_json(&registry()).unwrap_or_else(|| json!({}))
}

/// `load_registry_value` for callers that work on the registry as a map.
/// Keys are registry keys; values are the mixed-type records. A registry
/// that is valid JSON but not an object, which the Python hands on as is
/// (and every `.items()` then fails on), is `{}` here.
pub fn load_registry() -> Map<String, Value> {
    match load_registry_value() {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

/// Write the AppImage registry (indent=2, trailing newline), atomically: a
/// crash mid-write cannot truncate it. Errors propagate, as the Python's
/// OSError does.
pub fn save_registry(data: &Map<String, Value>) -> Result<(), String> {
    let reg = registry();
    if let Some(parent) = reg.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = reg.with_extension("tmp");
    let text = py_dumps(&Value::Object(data.clone()), Some(2)) + "\n";
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &reg).map_err(|e| e.to_string()) // atomic: a crash mid-write cannot truncate it
}

pub const APPIMAGE_CHECK_SECONDS: f64 = 3600.0;

/// Registered AppImages. (Python default: `check_updates=True`.)
///
/// An error is what the Python raises out of `appimage.check` on a malformed
/// record (a spec without an owner, say); `collect` then drops the whole
/// AppImage section, as the Python's catch-all does.
///
/// Also Err where the Python raises on a malformed registry (not an object,
/// a record that is not one, a path that is not a string, a `latest` that
/// is truthy but not an object), and when the registry cannot be written
/// back after an update check.
pub fn appimages(check_updates: bool) -> Result<Vec<App>, String> {
    let mut registry = match load_registry_value() {
        Value::Object(m) => m,
        other => return Err(format!("'{}' object has no attribute 'items'", py_type(&other))),
    };
    let mut apps = Vec::new();
    let mut dirty = false;
    let mut keys: Vec<String> = registry.keys().cloned().collect();
    keys.sort();
    for key in keys {
        let mut rec = match registry.get(&key) {
            Some(Value::Object(m)) => m.clone(),
            Some(other) => return Err(format!("'{}' object has no attribute 'get'", py_type(other))),
            None => Map::new(),
        };
        let path_s = match rec.get("path") {
            None => py_path(""),
            Some(p) if real_str(p).is_some() => py_path(&real_str(p).unwrap_or_default()),
            Some(other) => {
                return Err(format!(
                    "argument should be a str or an os.PathLike object where __fspath__ returns a str, not '{}'",
                    py_type(other)
                ))
            }
        };
        let path = Path::new(&path_s);

        // Ask GitHub at most hourly: a rescan happens on every package change,
        // and the release feed does not move nearly that fast.
        // `rec.get("latest") or {}`: a truthy non-dict is kept, and fails
        // at its first `.get` below unless a check replaces it first.
        let mut cached: Value = match rec.get("latest") {
            Some(v) if truthy(v) => v.clone(),
            _ => json!({}),
        };
        let age = util::now() - float_or_zero(&rec, "checked_at")?;
        if check_updates && path.exists() && age > APPIMAGE_CHECK_SECONDS {
            let found = crate::appimage::check(&Value::Object(rec.clone()))?;
            let found_v = match &found {
                Some(f) => serde_json::to_value(f).unwrap_or_else(|_| json!({})),
                None => json!({}),
            };
            rec.insert("latest".into(), found_v.clone());
            rec.insert("checked_at".into(), json!(util::now()));
            if let Some(spec) = found_v.get("spec").filter(|s| truthy(s)) {
                rec.insert("update_spec".into(), spec.clone());
            }
            registry.insert(key.clone(), Value::Object(rec.clone()));
            cached = found_v;
            dirty = true;
        }

        let has_cached = truthy(&cached);
        let cached = match cached {
            Value::Object(m) => m,
            _ if !has_cached => Map::new(),
            other => return Err(format!("'{}' object has no attribute 'get'", py_type(&other))),
        };
        let cget = |k: &str| -> Value {
            if has_cached { cached.get(k).cloned().unwrap_or(json!("")) } else { json!("") }
        };
        let exists = path.exists();
        let meta = if exists { std::fs::metadata(path).ok() } else { None };
        let url = match rec.get("url") {
            Some(v) if truthy(v) => v.clone(),
            _ => cget("page"),
        };
        let mut extra = Map::new();
        extra.insert("present".into(), json!(exists));
        extra.insert("url".into(), url);
        extra.insert("key".into(), json!(key));
        extra.insert("download".into(), cget("url"));
        extra.insert("published".into(), cget("published"));
        extra.insert(
            "installed_ts".into(),
            match &meta {
                Some(m) => {
                    use std::os::unix::fs::MetadataExt;
                    json!(m.mtime() as f64 + m.mtime_nsec() as f64 / 1e9)
                }
                None => json!(0),
            },
        );
        extra.insert(
            "size_mb".into(),
            match &meta {
                Some(m) => json!(m.len() as f64 / 1048576.0),
                None => json!(0),
            },
        );
        extra.insert("is_app".into(), json!(true));
        let field = |k: &str, d: Value| rec.get(k).cloned().unwrap_or(d);
        apps.push(
            App { source: APPIMAGE.into(), exec_path: path_s.clone(), extra, ..Default::default() }.with_values(&[
                ("name", &field("name", json!(key))),
                ("version", &field("version", json!("?"))),
                ("description", &field("description", json!("AppImage"))),
                // A falsy version (None, "") is no update: `has_update` is
                // `bool(update)`.
                ("update", &cget("version")),
            ]),
        );
    }
    if dirty {
        // An OSError here propagates to the caller, as in the Python.
        save_registry(&registry)?;
    }
    Ok(apps)
}

/// `_KEEP_ORPHANS`: orphans that are usually wanted back. Build tooling gets
/// pulled in by the next AUR package and removing it just means downloading
/// it again; a debug package was almost certainly a deliberate choice.
pub const KEEP_ORPHANS: &[&str] = &[
    "rust", "cargo", "go", "python-build", "python-installer",
    "python-wheel", "python-setuptools", "meson", "ninja", "cmake",
];

/// How far behind, and at which level.
///
/// Returns (count, kind) where kind is 'major'/'minor'/'patch'/'rel'. Only the
/// first differing component is counted: 1.2.9 -> 3.0.0 is "2 major behind",
/// not "2 major and 2 minor", because once the major moves the lower numbers
/// restart and differencing them is meaningless.
///
/// Serialises as a JSON array `[count, kind]`, like the Python tuple.
///
/// The count is a machine integer for callers that do arithmetic on it; a
/// difference beyond i64 saturates. `versions_behind_big` has the exact
/// (unbounded, like Python's int) count.
pub fn versions_behind(current: &str, latest: &str) -> (i64, String) {
    use num_bigint::ToBigInt;
    let (n, kind) = versions_behind_big(current, latest);
    let max = i64::MAX.to_bigint().unwrap_or_default();
    let min = i64::MIN.to_bigint().unwrap_or_default();
    let clamped = if n > max { i64::MAX } else if n < min { i64::MIN } else { i64::try_from(&n).unwrap_or(0) };
    (clamped, kind)
}

/// `versions_behind` with Python's unbounded int for the count.
pub fn versions_behind_big(current: &str, latest: &str) -> (num_bigint::BigInt, String) {
    use num_bigint::BigInt;
    fn nums(v: &str) -> Vec<BigInt> {
        let v = ascii_digits(v);
        let last = v.rsplit(':').next().unwrap_or("");
        let core = last.split(['-', '+', '~']).next().unwrap_or("");
        let mut out = Vec::new();
        for part in core.split('.') {
            let digits: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
            if digits.is_empty() {
                break;
            }
            out.push(digits.parse::<BigInt>().unwrap_or_default());
        }
        out
    }

    let zero = BigInt::from(0);
    let (a, b) = (nums(current), nums(latest));
    if a.is_empty() || b.is_empty() {
        return (zero, String::new());
    }
    // Calendar versions are not release counters. Both the packed form
    // (20181225 -> 20261002, which differenced to "79777 major behind") and
    // the dotted form (2026.10.16 -> 2026.10.17, which read as "1 patch")
    // describe when a build was cut, not how many releases were missed.
    fn calendar(v: &[BigInt]) -> bool {
        let n = |x: i64| BigInt::from(x);
        if v[0] > n(100000) {
            // 20261002
            return true;
        }
        (n(1980) <= v[0] && v[0] <= n(2100)) // 2026.10.17
            && v.len() >= 2 && (n(1) <= v[1] && v[1] <= n(12))
            && (v.len() < 3 || (n(1) <= v[2] && v[2] <= n(31)))
    }

    if calendar(&a) || calendar(&b) {
        return (zero, String::new());
    }
    let names = ["major", "minor", "patch"];
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).cloned().unwrap_or_default();
        let y = b.get(i).cloned().unwrap_or_default();
        if y != x {
            let kind = if i < 3 { names[i] } else { "point" };
            return (y - x, kind.to_string());
        }
    }
    // Same version numbers: the difference is only the pkgrel (e.g. -1 -> -2),
    // which is a rebuild, not a new upstream release.
    (zero, "rel".into())
}

/// A Python int as JSON: exact up to 64 bits; beyond that the nearest
/// float, the most a `serde_json::Value` (without arbitrary precision) can
/// hold.
fn big_json(n: &num_bigint::BigInt) -> Value {
    if let Ok(i) = i64::try_from(n) {
        return json!(i);
    }
    if let Ok(u) = u64::try_from(n) {
        return json!(u);
    }
    n.to_string().parse::<f64>().map(|f| json!(f)).unwrap_or(Value::Null)
}

/// 'keep' when removing it is likely a false economy, else 'remove'.
pub fn orphan_advice(name: &str) -> String {
    if KEEP_ORPHANS.contains(&name) || name.ends_with("-debug") || name.ends_with("-docs") {
        return "keep".into();
    }
    "remove".into()
}

/// Packages installed as dependencies that nothing requires any more.
///
/// Listed separately rather than mixed into the main inventory: they were
/// never chosen, so they are a cleanup decision, not software the user owns.
///
/// Err where the Python raises: output that is not UTF-8, or a one-word
/// `pacman -Q` line.
pub fn try_orphans() -> Result<Vec<App>, String> {
    let names: Vec<String> =
        py_split(&run(&["pacman", "-Qdtq"], 30)?).into_iter().map(String::from).collect();
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let info = package_info(&names)?;
    let versions = version_map(&run(&argv(&["pacman", "-Q"], &names), 25)?)?;
    let empty = BTreeMap::new();
    let mut apps = Vec::new();
    for n in &names {
        let meta = info.get(n).unwrap_or(&empty);
        let m = |k: &str| meta.get(k).cloned().unwrap_or_default();
        let advice = orphan_advice(n);
        let mut extra = Map::new();
        extra.insert("size".into(), json!(m("size")));
        extra.insert("size_mb".into(), json!(size_mb(&m("size"))));
        extra.insert("installed_ts".into(), json!(date_ts(&m("installed"))));
        extra.insert("url".into(), json!(m("url")));
        extra.insert("advice".into(), json!(advice));
        extra.insert("is_app".into(), json!(false));
        apps.push(App {
            name: n.clone(),
            version: versions.get(n).cloned().unwrap_or_else(|| "?".into()),
            source: ORPHAN.into(),
            description: m("description"),
            exec_path: String::new(),
            extra,
            ..Default::default()
        });
    }
    Ok(apps)
}

/// `try_orphans` for callers that need a plain list: where the Python
/// raises, this answers `[]`.
pub fn orphans() -> Vec<App> {
    try_orphans().unwrap_or_default()
}

/// Same software present twice -- an AppImage plus its packaged build, or
/// a -bin and a source package side by side.
///
/// A Vec of pairs, not a map: the Python dict keeps first-seen order and the
/// UI shows the first five.
pub fn duplicates(apps: &[App]) -> Vec<(String, Vec<String>)> {
    let mut seen: Vec<(String, Vec<&App>)> = Vec::new();
    for a in apps {
        if a.source == ORPHAN {
            continue;
        }
        let mut base = a.name.to_lowercase();
        for suffix in ["-bin", "-git", "-appimage", "-desktop"] {
            if let Some(s) = base.strip_suffix(suffix) {
                base = s.to_string();
            }
        }
        match seen.iter_mut().find(|(k, _)| *k == base) {
            Some((_, v)) => v.push(a),
            None => seen.push((base, vec![a])),
        }
    }
    seen.into_iter()
        .filter(|(_, v)| v.len() > 1)
        .map(|(k, v)| (k, v.iter().map(|a| format!("{} ({})", a.name, a.source)).collect()))
        .collect()
}

/// Installed Flatpak apps. Silent no-op when flatpak is not installed, so
/// the section simply does not appear rather than erroring.
/// (Err: flatpak's output is not UTF-8, `_run`'s UnicodeDecodeError.)
pub fn flatpaks() -> Result<Vec<App>, String> {
    let out = run(&["flatpak", "list", "--app", "--columns=application,name,version"], 30)?;
    if py_strip(&out).is_empty() {
        return Ok(Vec::new());
    }
    let mut pending = HashSet::new();
    for line in py_splitlines(&run(&["flatpak", "remote-ls", "--updates", "--columns=application"], 60)?) {
        if !py_strip(line).is_empty() {
            pending.insert(py_strip(line).to_string());
        }
    }

    let mut apps = Vec::new();
    for line in py_splitlines(&out) {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.is_empty() || py_strip(parts[0]).is_empty() {
            continue;
        }
        let appid = py_strip(parts[0]).to_string();
        let name = if parts.len() > 1 { py_strip(parts[1]).to_string() } else { appid.clone() };
        let version = if parts.len() > 2 { py_strip(parts[2]).to_string() } else { "?".into() };
        apps.push(App {
            name: if name.is_empty() { appid.clone() } else { name },
            version: if version.is_empty() { "?".into() } else { version },
            source: FLATPAK.into(),
            description: appid.clone(),
            update: if pending.contains(&appid) { "available".into() } else { String::new() },
            exec_path: appid.clone(),
            extra: json!({"icon": appid, "is_app": true, "appid": appid})
                .as_object()
                .cloned()
                .unwrap_or_default(),
        });
    }
    Ok(apps)
}

pub const GIT_CHECK_SECONDS: f64 = 3600.0;

/// `_GIT_CACHE`: `$XDG_CACHE_HOME/almanac/git.json`.
pub fn git_cache_path() -> PathBuf {
    match std::env::var_os("XDG_CACHE_HOME") {
        Some(v) => PathBuf::from(v).join("almanac/git.json"),
        None => crate::util::home().join(".cache").join("almanac/git.json"),
    }
}

/// `_git_cache` as the Python returns it: whatever JSON the file holds (a
/// list stays a list), `{}` when it is missing or unreadable.
pub fn git_cache_value() -> Value {
    py_read_json(&git_cache_path()).unwrap_or_else(|| json!({}))
}

/// `_git_cache` for callers that work on it as a map. Keys are repo paths
/// (values are `GitBehind` records) plus `"_plugins"`, the cache
/// `plugins::classify` keeps. A cache that is valid JSON but not an object
/// (which the Python hands on as is) is `{}` here.
pub fn git_cache() -> Map<String, Value> {
    match git_cache_value() {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

/// One `_git_behind` result, as stored in the git cache. Optional fields are
/// absent (not null) in the JSON, exactly like the Python dict.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GitBehind {
    pub tracked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behind: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_subject: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dirty: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<bool>,
    pub fetched_at: f64,
}

/// `_git_behind` as the Python dict: the record written into `cache`.
/// Err is what the Python raises outside its try: a cached entry that is
/// truthy but not a dict (AttributeError), or a non-numeric cached
/// `fetched_at` (ValueError).
fn git_behind_raw(repo: &Path, cache: &mut Map<String, Value>) -> Result<Map<String, Value>, String> {
    // str(repo): a name that is not UTF-8 keeps its bytes (surrogateescape),
    // so the key is the one the Python writes.
    let key = match path_json(repo) {
        Value::String(k) => k,
        _ => String::new(),
    };
    let entry: Map<String, Value> = match cache.get(&key) {
        Some(Value::Object(m)) => m.clone(),
        Some(v) if truthy(v) => {
            return Err(format!("'{}' object has no attribute 'get'", py_type(v)))
        }
        _ => Map::new(),
    };
    let mut fetched_at = float_or_zero(&entry, "fetched_at")?;

    // OSError (no git), TimeoutExpired and ValueError (output that is not
    // UTF-8 is a UnicodeDecodeError) are the exceptions the Python catches.
    // Text mode also turns a "\r" in a subject into "\n".
    let git = |args: &[&str], timeout: u64| -> Result<RunResult, ()> {
        let mut v: Vec<&std::ffi::OsStr> = vec!["git".as_ref(), "-C".as_ref(), repo.as_os_str()];
        v.extend(args.iter().map(|a| std::ffi::OsStr::new(a)));
        subprocess_run(&v, timeout).map_err(|_| ())
    };

    let attempt = (|| -> Result<Map<String, Value>, ()> {
        if git(&["rev-parse", "--abbrev-ref", "@{upstream}"], 25)?.code != Some(0) {
            let mut r = Map::new();
            r.insert("tracked".into(), json!(false));
            return Ok(r);
        }
        if util::now() - fetched_at >= GIT_CHECK_SECONDS {
            git(&["fetch", "--quiet"], 45)?;
            fetched_at = util::now();
        }
        let behind_out = git(&["rev-list", "--count", "HEAD..@{upstream}"], 25)?;
        let behind = py_strip(&behind_out.stdout).to_string();
        let subject_out = git(&["log", "-1", "--format=%s", "@{upstream}"], 25)?;
        let subject = py_strip(&subject_out.stdout).to_string();
        let dirty = !py_strip(&git(&["status", "--porcelain"], 25)?.stdout).is_empty();
        // int(behind or 0): a non-number is the ValueError the Python catches.
        let behind_n = if behind.is_empty() { "0".to_string() } else { py_int_text(&behind).ok_or(())? };
        let mut r = Map::new();
        r.insert("tracked".into(), json!(true));
        r.insert("behind".into(), int_json(&behind_n));
        r.insert("latest_subject".into(), json!(py_prefix(&subject, 90)));
        r.insert("dirty".into(), json!(dirty));
        Ok(r)
    })();
    let mut result = match attempt {
        Ok(r) => r,
        Err(()) => {
            let mut r = if entry.is_empty() {
                let mut m = Map::new();
                m.insert("tracked".into(), json!(false));
                m
            } else {
                entry.clone()
            };
            r.insert("error".into(), json!(true));
            r
        }
    };

    result.insert("fetched_at".into(), float_json(fetched_at));
    cache.insert(key, Value::Object(result.clone()));
    Ok(result)
}

/// `_git_behind`: how far a checkout is behind its upstream.
///
/// Only the `git fetch` is cached -- it is a network round trip per repo, so
/// it runs at most once an hour. Counting commits against the fetched refs is
/// local and cheap, so it is done every time: a cached count kept a plugin
/// "901 commits behind" for an hour after it had been pulled. A repo with no
/// upstream, or an unreachable remote, returns nothing rather than a
/// misleading zero -- "up to date" and "could not check" are different answers.
///
/// Writes the result back into `cache` under the repo path. Err where the
/// Python raises: a cached `fetched_at` that is not a number.
pub fn git_behind(repo: &Path, cache: &mut Map<String, Value>) -> Result<GitBehind, String> {
    let raw = git_behind_raw(repo, cache)?;
    // Field by field: a fetched_at of inf is a float, not the str a Value
    // carries it as.
    let fetched_at = raw.get("fetched_at").and_then(py_float_value).unwrap_or(0.0);
    let mut r: GitBehind = serde_json::from_value(Value::Object(
        raw.into_iter().filter(|(k, _)| k != "fetched_at").collect(),
    ))
    .unwrap_or_default();
    r.fetched_at = fetched_at;
    Ok(r)
}

/// `_github_page`: a git remote as a page a browser can open, or ''.
pub fn github_page(url: &str) -> String {
    let u = py_strip(url);
    let mut url = u.strip_suffix(".git").unwrap_or(u).to_string();
    if let Some(rest) = url.strip_prefix("git@github.com:") {
        url = format!("https://github.com/{rest}");
    }
    if url.starts_with("https://") { url } else { String::new() }
}

/// The Almanac checkout (Python: `Path(__file__).resolve().parent`, the
/// checkout the running code came from), symlinks resolved. A binary cargo
/// built inside a checkout (`<checkout>/rust/target/.../almanac-rs`) stands
/// for the checkout it was built in, whose code is beside it (the rule
/// `i18n::localedir` uses). A binary that was copied elsewhere
/// (`~/.local/bin`, `~/.cargo/bin`) still came from the checkout it was
/// built in, so that is the one left to its self-update; only when that
/// checkout is gone does the program's own directory stand in.
fn own_checkout() -> PathBuf {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let built_from = crate_dir.parent().filter(|c| c.is_dir()).map(resolve);
    let exe = match std::env::current_exe() {
        Ok(e) => resolve(&e),
        // No /proc: the checkout this binary was built from.
        Err(_) => return built_from.unwrap_or_else(|| resolve(&crate_dir)),
    };
    let dir = exe.parent().map(PathBuf::from).unwrap_or_default();
    let built_in = dir
        .ancestors()
        .find(|a| {
            a.file_name().is_some_and(|n| n == "target")
                && a.parent().is_some_and(|c| c.join("Cargo.toml").is_file())
        })
        .and_then(|t| t.parent()?.parent())
        .map(PathBuf::from);
    match built_in {
        Some(c) => resolve(&c),
        None => built_from.unwrap_or_else(|| resolve(&dir)),
    }
}

/// `Path.resolve()` (non-strict): never fails.
fn resolve(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// `sorted(d.glob(pattern))` for a one-level pattern: entries of `d` (dot
/// files included, as pathlib's glob includes them), sorted by name.
fn sorted_children(d: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = match std::fs::read_dir(d) {
        Ok(rd) => rd.filter_map(|e| e.ok()).map(|e| e.path()).collect(),
        Err(_) => Vec::new(),
    };
    v.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    v
}

/// A file name as Rust text. (`App.name` is a `String`, which cannot hold
/// the surrogates Python keeps for a name that is not UTF-8: such a name
/// shows with U+FFFD. The paths handed to git and kept in `extra` are exact.)
fn name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Software built from source here. Listed so the inventory is honest about
/// what exists; these update through their own repos, not a package manager.
/// (Python default: `check_updates=True`.)
///
/// Err where the Python raises (`collect` then drops the section, as the
/// Python's catch-all does): a git cache that is not an object, or with an
/// entry that is not one or whose `fetched_at` is not a number; git output
/// that is not UTF-8; a plugin manifest that is valid JSON but not an object.
pub fn local_builds(check_updates: bool) -> Result<Vec<App>, String> {
    let mut apps = Vec::new();
    let mut cache = if check_updates {
        match git_cache_value() {
            Value::Object(m) => m,
            // Every path below reaches `cache.get` or `cache.setdefault`.
            other => {
                return Err(format!("'{}' object has no attribute 'setdefault'", py_type(&other)))
            }
        }
    } else {
        Map::new()
    };
    let mut dirty_cache = false;

    let git_extra = |repo: &Path, cache: &mut Map<String, Value>, dirty_cache: &mut bool| -> Result<Map<String, Value>, String> {
        if !check_updates || !repo.join(".git").exists() {
            return Ok(Map::new());
        }
        let info = git_behind_raw(repo, cache)?;
        *dirty_cache = true;
        let get = |k: &str, d: Value| info.get(k).cloned().unwrap_or(d);
        let mut g = Map::new();
        g.insert("git_behind".into(), get("behind", json!(0)));
        g.insert("git_subject".into(), get("latest_subject", json!("")));
        g.insert("git_dirty".into(), get("dirty", json!(false)));
        g.insert("git_tracked".into(), get("tracked", json!(false)));
        Ok(g)
    };

    // Anything cloned straight into the data directory and built there --
    // a driver, a tool installed with `make install PREFIX=~/.local`. Found
    // by being a git checkout, not by name; Almanac's own checkout is left
    // to its self-update.
    let own = own_checkout();
    for child in sorted_children(&crate::xdg::data_home()) {
        // glob("*/.git"): the entry exists, even as a dangling link.
        if std::fs::symlink_metadata(child.join(".git")).is_err() {
            continue;
        }
        let repo = child;
        if resolve(&repo) == own {
            continue;
        }
        // The path as the bytes it is: git must find the directory even when
        // its name is not UTF-8.
        let git_argv = |args: &[&'static str]| -> Vec<std::ffi::OsString> {
            let mut v: Vec<std::ffi::OsString> = vec!["git".into(), "-C".into(), repo.clone().into_os_string()];
            v.extend(args.iter().map(std::ffi::OsString::from));
            v
        };
        let rev = py_strip(&run_os(&git_argv(&["describe", "--always", "--dirty"]), 25)?).to_string();
        let origin = py_strip(&run_os(&git_argv(&["remote", "get-url", "origin"]), 25)?).to_string();
        let g = git_extra(&repo, &mut cache, &mut dirty_cache)?;
        let launcher = crate::xdg::bin_home().join(repo.file_name().unwrap_or_default());
        let mut extra = g.clone();
        extra.insert("repo".into(), path_json(&repo));
        extra.insert("is_app".into(), json!(false));
        let update = match g.get("git_behind") {
            Some(b) if truthy(b) => format!("{} commits", py_str(b)),
            _ => String::new(),
        };
        // `repo.name` and `str(launcher)` keep a name that is not UTF-8 as
        // surrogates.
        let exec = if launcher.exists() { path_json(&launcher) } else { json!("") };
        apps.push(App {
            version: if rev.is_empty() { "git".into() } else { rev },
            source: LOCAL.into(),
            description: format!(
                "Built from source{}",
                if origin.is_empty() { String::new() } else { format!(" · {origin}") }
            ),
            update,
            extra,
            ..Default::default()
        }
        .with_values(&[("name", &path_json(Path::new(repo.file_name().unwrap_or_default()))), ("exec_path", &exec)]));
    }
    // Omarchy shell plugins: classified before anything offers to touch them
    // (plugins.py), with whether Omarchy currently has each one switched on.
    let pdir = crate::plugins::plugins_dir();
    let pdir_ok = pdir.is_dir();
    let switched_on = if pdir_ok { crate::plugins::enabled() } else { BTreeMap::new() };
    // pcache = cache.setdefault("_plugins", {}): an existing value is used
    // as it is, whatever it is.
    if !cache.contains_key("_plugins") {
        cache.insert("_plugins".into(), json!({}));
    }
    let children = if pdir_ok { sorted_children(&pdir) } else { Vec::new() };
    for p in children {
        if !p.is_dir() {
            continue;
        }
        let pname = path_json(Path::new(p.file_name().unwrap_or_default()));
        let (mut desc, mut ver, mut pid) = (json!("Omarchy shell plugin"), json!("—"), pname.clone());
        // OSError and ValueError (bad JSON, or text that is not UTF-8) are
        // caught; a document that is not an object fails at `m.get`.
        if let Ok(text) = std::fs::read_to_string(p.join("manifest.json")) {
            match py_loads(&text) {
                Ok(Value::Object(m)) => {
                    desc = m.get("description").cloned().unwrap_or(desc);
                    ver = m.get("version").cloned().unwrap_or(ver);
                    if let Some(id) = m.get("id").filter(|v| truthy(v)) {
                        pid = id.clone();
                    }
                }
                Ok(other) => {
                    return Err(format!("'{}' object has no attribute 'get'", py_type(&other)))
                }
                Err(_) => {}
            }
        }
        let st = match cache.get_mut("_plugins") {
            Some(Value::Object(pc)) => crate::plugins::classify(&p, Some(pc), check_updates),
            // None is classify's "no cache".
            Some(Value::Null) | None => crate::plugins::classify(&p, None, check_updates),
            // Anything else is handed to classify as is, which only fails
            // once `_remote` uses it: `(cache or {}).get(...)` on a truthy
            // non-dict, or `cache[key] = ...` after a fetch on a falsy one.
            Some(other) => {
                let other = other.clone();
                let st = crate::plugins::classify(&p, None, check_updates);
                let before_remote = ["symlink", "not-git", "no-origin", "no-commits", "detached"]
                    .contains(&st.detail.as_str())
                    || (st.kind == crate::plugins::ERROR && st.detail != "fetch failed");
                if !before_remote {
                    if truthy(&other) {
                        return Err(format!("'{}' object has no attribute 'get'", py_type(&other)));
                    }
                    if check_updates && st.detail != "fetch failed" {
                        return Err(format!(
                            "'{}' object does not support item assignment",
                            py_type(&other)
                        ));
                    }
                }
                st
            }
        };
        // Only a run that checked has anything worth saving; without the
        // check the cache started empty, and saving it would erase the last
        // fetch results.
        dirty_cache = dirty_cache || check_updates;
        // switched_on.get(pid): only a string id can match; an unhashable
        // one is a TypeError.
        let on = match &pid {
            Value::String(id) if exotic(&pid).is_none() => switched_on.get(id).copied(),
            Value::Array(_) | Value::Object(_) => {
                return Err(format!("unhashable type: '{}'", py_type(&pid)))
            }
            _ => None,
        };
        // An edit with an update waiting is still offered, through the
        // patches.py weighing; ahead, diverged, detached and symlinked
        // plugins are not, because no update is right for them.
        let behind = if st.updatable() || st.detail == "modified" { st.behind } else { 0 };
        let mut extra = Map::new();
        extra.insert("repo".into(), path_json(&p));
        extra.insert("is_app".into(), json!(false));
        extra.insert("plugin_id".into(), pid.clone());
        extra.insert("plugin_state".into(), json!(st.kind));
        extra.insert("plugin_detail".into(), json!(st.describe()));
        extra.insert("enabled".into(), json!(on));
        extra.insert("url".into(), json!(github_page(&st.url)));
        extra.insert("git_behind".into(), json!(behind));
        extra.insert("git_dirty".into(), json!(st.detail == "modified"));
        extra.insert("git_tracked".into(), json!(st.kind != crate::plugins::LOCAL));
        extra.insert("git_subject".into(), json!(""));
        let desc = if on != Some(false) { desc } else { json!(format!("Turned off · {}", py_str(&desc))) };
        apps.push(
            App {
                source: LOCAL.into(),
                update: if behind != 0 { format!("{behind} commits") } else { String::new() },
                extra,
                ..Default::default()
            }
            .with_values(&[("name", &pname), ("version", &ver), ("description", &desc)]),
        );
    }

    if dirty_cache {
        let path = git_cache_path();
        let write = || -> std::io::Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, py_dumps(&Value::Object(cache.clone()), Some(2)))
        };
        let _ = write(); // a lost cache only costs a refetch
    }
    Ok(apps)
}

/// Everything, in one list.
///
/// The six collectors are independent and each is dominated by waiting --
/// checkupdates and yay on the network, git on disk, agents on process
/// start-up. Run together the total is the slowest one rather than the sum.
pub fn collect() -> Vec<App> {
    if crate::demo::active() {
        return crate::demo::collect();
    }

    // Almanac lists itself. An app whose job is telling you what is stale
    // should not be the one thing that silently is.
    // An Err is a collector that raised, like a panic.
    let sources: Vec<fn() -> Result<Vec<App>, String>> = vec![
        packages,
        || appimages(true),
        flatpaks,
        || local_builds(true),
        || crate::agents::try_detect(true),
        || crate::agents::try_mise_tools(true),
        // `lambda: [selfupdate.as_app()]`: where as_app raises, the row is
        // dropped with the rest of that collector.
        || crate::selfupdate::try_as_app(true).map(|a| vec![a]),
        try_orphans,
    ];
    let mut out = Vec::new();
    std::thread::scope(|s| {
        let handles: Vec<_> = sources.iter().map(|f| s.spawn(f)).collect();
        for h in handles {
            match h.join() {
                Ok(Ok(apps)) => out.extend(apps),
                // One collector failing must not empty the whole window; the
                // others still describe most of the system.
                Ok(Err(_)) | Err(_) => continue,
            }
        }
    });
    out
}

/// Everything worth showing in a detail view, fetched on demand.
///
/// Deliberately not part of collect(): dependency lists mean a pacman call
/// per package, which would turn a 2-second scan into a minute for data that
/// is only read one row at a time.
///
/// Err: pacman's output is not UTF-8 (`_run`'s UnicodeDecodeError).
pub fn try_details(app: &App) -> Result<Map<String, Value>, String> {
    let mut out = app.extra_dict();
    if [REPO, AUR, SYSTEM, ORPHAN].contains(&app.source.as_str()) {
        let text = run(&["pacman", "-Qi", &app.name], 20)?;
        let mut field: Option<String> = None;
        let mut acc: HashMap<String, String> = HashMap::new();
        for line in py_splitlines(&text) {
            // `if line.startswith(" ") and field`: a field named "" is
            // falsy, so the line is parsed as a field of its own.
            if line.starts_with(' ') {
                if let Some(f) = field.as_ref().filter(|f| !f.is_empty()) {
                    if let Some(v) = acc.get_mut(f) {
                        v.push(' ');
                        v.push_str(py_strip(line));
                    }
                    continue;
                }
            }
            let Some((k, v)) = line.split_once(':') else { continue };
            let k = py_strip(k).to_string();
            acc.insert(k.clone(), py_strip(v).to_string());
            field = Some(k);
        }
        for key in ["Depends On", "Required By", "Optional Deps", "Provides",
                    "Conflicts With", "Packager", "Architecture", "Validated By"] {
            if let Some(v) = acc.get(key) {
                if !v.is_empty() && v != "None" {
                    out.insert(key.to_string(), json!(v));
                }
            }
        }
        let files = py_splitlines(&run(&["pacman", "-Qlq", &app.name], 20)?).len();
        out.insert("files".into(), json!(files));
    }
    Ok(out)
}

/// `try_details` for callers that need a plain map: where the Python
/// raises, this answers the row's own `extra`.
pub fn details(app: &App) -> Map<String, Value> {
    try_details(app).unwrap_or_else(|_| app.extra_dict())
}

// ---------------------------------------------------------------- discover --
pub const AVAILABLE: &str = "available";

/// Search what could be installed, across the official repos and the AUR.
/// (Python default: `limit=60`.)
///
/// Results are marked AVAILABLE so the UI can offer Install rather than
/// Update. Anything already installed is dropped: it belongs in the main
/// inventory, where its real version and update state live.
///
/// Err where the Python raises: pacman output that is not UTF-8, or an AUR
/// reply whose `results` is not a list of objects with numeric popularity
/// (the sort and the reads after it are outside the Python's try).
pub fn try_search_available(query: &str, limit: usize) -> Result<Vec<App>, String> {
    let query = py_strip(query);
    if query.chars().count() < 2 {
        return Ok(Vec::new());
    }

    let installed: HashSet<String> =
        py_split(&run(&["pacman", "-Qq"], 25)?).into_iter().map(String::from).collect();
    let mut found: Vec<App> = Vec::new();

    // --- official repos ---
    let out = run(&["pacman", "-Ss", query], 40)?;
    let (mut name, mut version, mut repo) = (String::new(), String::new(), String::new());
    for line in py_splitlines(&out) {
        if !line.starts_with(' ') {
            let head = py_split(line);
            if head.is_empty() {
                continue;
            }
            let repo_name = head[0];
            let (r, n) = repo_name.split_once('/').unwrap_or((repo_name, ""));
            repo = r.to_string();
            name = n.to_string();
            version = head.get(1).map(|s| s.to_string()).unwrap_or_default();
            continue;
        }
        if !name.is_empty() && !installed.contains(&name) {
            found.push(App {
                name: name.clone(),
                version: version.clone(),
                source: AVAILABLE.into(),
                description: py_strip(line).to_string(),
                extra: json!({"installable": true, "repo_name": repo, "origin": REPO,
                              "is_app": true})
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
                ..Default::default()
            });
        }
        name.clear();
    }

    // --- AUR ---
    let url = format!("https://aur.archlinux.org/rpc/v5/search/{}?by=name-desc", py_quote(query));
    // json.loads(...).get("results", []) inside the try: a reply that is not
    // an object is caught like a failed fetch.
    let results: Value = match http_json(&url, 20) {
        Ok(Value::Object(d)) => d.get("results").cloned().unwrap_or(json!([])),
        _ => json!([]),
    };
    let Value::Array(mut results) = results else {
        return Err(format!("'{}' object has no attribute 'sort'", py_type(&results)));
    };

    // Popularity first: on the AUR it is the only signal that separates a
    // maintained package from one person's abandoned experiment.
    // key=lambda r: -(r.get("Popularity") or 0)
    let mut keyed = Vec::with_capacity(results.len());
    for r in results.drain(..) {
        let Value::Object(rec) = &r else {
            return Err(format!("'{}' object has no attribute 'get'", py_type(&r)));
        };
        let pop = match rec.get("Popularity") {
            Some(v) if truthy(v) => match v {
                Value::Number(n) => n.as_f64().unwrap_or(0.0),
                Value::Bool(_) => 1.0,
                // NaN, an infinity, an int past 64 bits.
                Value::String(_) if matches!(exotic(v), Some(Exotic::Float(_) | Exotic::Int(_))) => {
                    py_float_value(v).unwrap_or(f64::INFINITY)
                }
                other => {
                    return Err(format!("bad operand type for unary -: '{}'", py_type(other)))
                }
            },
            _ => 0.0,
        };
        keyed.push((-pop, r));
    }
    keyed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    for (_, rec) in &keyed {
        let Some(rec) = rec.as_object() else { continue };
        let nm = rec.get("Name").cloned().unwrap_or(json!(""));
        // `if not nm or nm in installed`: an empty list or dict is skipped
        // before the membership test could find it unhashable.
        if !truthy(&nm) {
            continue;
        }
        if let Value::Array(_) | Value::Object(_) = nm {
            return Err(format!("unhashable type: '{}'", py_type(&nm)));
        }
        if exotic(&nm).is_none() && nm.as_str().is_some_and(|n| installed.contains(n)) {
            continue;
        }
        let description = match rec.get("Description") {
            Some(d) if real_str(d).is_some() => py_strip(&real_str(d).unwrap_or_default()).to_string(),
            Some(v) if truthy(v) => {
                return Err(format!("'{}' object has no attribute 'strip'", py_type(v)))
            }
            _ => String::new(),
        };
        // round(rec.get("Popularity") or 0, 2): an int (or True) stays an int.
        let popularity = match rec.get("Popularity") {
            Some(v) if truthy(v) => match v {
                Value::Bool(_) => json!(1),
                Value::Number(n) if n.is_f64() => json!(util::py_round_to(n.as_f64().unwrap_or(0.0), 2)),
                _ => v.clone(),
            },
            _ => json!(0),
        };
        let or_empty = |k: &str| -> Value {
            match rec.get(k) {
                Some(v) if truthy(v) => v.clone(),
                _ => json!(""),
            }
        };
        let mut extra = Map::new();
        extra.insert("installable".into(), json!(true));
        extra.insert("origin".into(), json!(AUR));
        extra.insert("is_app".into(), json!(true));
        extra.insert("votes".into(), rec.get("NumVotes").cloned().unwrap_or(json!(0)));
        extra.insert("popularity".into(), popularity);
        extra.insert("maintainer".into(), or_empty("Maintainer"));
        extra.insert("orphaned".into(), json!(!rec.get("Maintainer").is_some_and(truthy)));
        extra.insert("flagged".into(), json!(rec.get("OutOfDate").is_some_and(truthy)));
        extra.insert("url".into(), or_empty("URL"));
        found.push(App {
            name: py_str(&nm),
            version: get_str(rec, "Version", ""),
            source: AVAILABLE.into(),
            description,
            extra,
            ..Default::default()
        });
    }
    found.truncate(limit);
    Ok(found)
}

/// `try_search_available` for callers that need a plain list: where the
/// Python raises, this answers `[]`.
pub fn search_available(query: &str, limit: usize) -> Vec<App> {
    try_search_available(query, limit).unwrap_or_default()
}

// ------------------------------------------------- omarchy plugin registry --
pub const PLUGIN: &str = "plugin";
pub const REGISTRY_URL: &str =
    "https://raw.githubusercontent.com/omacom/omarchy-plugin-marketplace/main/registry.json";

/// `_REGISTRY_CACHE`: `$XDG_CACHE_HOME/almanac/registry.json`.
pub fn registry_cache_path() -> PathBuf {
    match std::env::var_os("XDG_CACHE_HOME") {
        Some(v) => PathBuf::from(v).join("almanac/registry.json"),
        None => crate::util::home().join(".cache").join("almanac/registry.json"),
    }
}

pub const REGISTRY_TTL: f64 = 86400.0;

/// The official Omarchy plugin marketplace index. (Python default: `force=False`.)
///
/// ~5,500 entries and a megabyte of JSON, so it is cached for a day. A failed
/// fetch falls back to whatever is on disk rather than emptying the catalogue.
/// Returns the raw JSON (an empty object when nothing is available).
pub fn plugin_registry(force: bool) -> Value {
    let path = registry_cache_path();
    let read = || -> Option<(f64, Value)> {
        let mtime = std::fs::metadata(&path).ok()?.modified().ok()?;
        let mtime = mtime.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
        let text = std::fs::read_to_string(&path).ok()?;
        let v: Value = py_loads(&text).ok()?;
        Some((util::now() - mtime, v))
    };
    // JSON null is Python's None here, the same as no cache.
    let (age, cached) = match read() {
        Some((age, v)) if !v.is_null() => (age, Some(v)),
        Some((age, _)) => (age, None),
        None => (1e9, None),
    };

    if let Some(c) = &cached {
        if !force && age < REGISTRY_TTL {
            return c.clone();
        }
    }

    let fallback = || match &cached {
        Some(c) if truthy(c) => c.clone(),
        _ => json!({}),
    };
    let data = match http_json(REGISTRY_URL, 30) {
        Ok(d) => d,
        Err(_) => return fallback(),
    };
    let write = || -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, py_dumps(&data, None))
    };
    if write().is_err() {
        return fallback();
    }
    data
}

/// `_plugin_name`: (name, description, author) for a registry entry.
///
/// Only a couple of entries ship catalog metadata, so for the rest the repo
/// URL is all there is -- the last path segment is the plugin's identity.
///
/// The values are the entry's own, as the Python returns them (a null
/// description stays None). Err where the Python raises: an entry that is
/// not an object, or a repo that is truthy but not a string.
pub fn plugin_name(entry: &Value) -> Result<(Value, Value, Value), String> {
    let Value::Object(e) = entry else {
        return Err(format!("'{}' object has no attribute 'get'", py_type(entry)));
    };
    let mut cat = e.get("catalog").cloned().unwrap_or(Value::Null);
    if let Value::Array(a) = &cat {
        if let Some(first) = a.first() {
            cat = first.clone();
        }
    }
    if let Value::Object(c) = &cat {
        if c.get("name").is_some_and(truthy) {
            let get = |k: &str| c.get(k).cloned().unwrap_or(json!(""));
            return Ok((get("name"), get("description"), get("author")));
        }
    }

    let repo_full = match e.get("repo") {
        Some(r) if real_str(r).is_some() => real_str(r).unwrap_or_default(),
        Some(v) if truthy(v) => {
            return Err(format!("'{}' object has no attribute 'rstrip'", py_type(v)))
        }
        _ => String::new(),
    };
    let repo = repo_full.trim_end_matches('/');
    let parts: Vec<&str> = repo.split('/').collect();
    let name = parts.last().copied().unwrap_or(repo).to_string();
    let author = if parts.len() >= 2 { parts[parts.len() - 2].to_string() } else { String::new() };
    Ok((json!(name), json!(""), json!(author)))
}

/// Python `==` between two hashable JSON scalars (`1 == 1.0 == True`).
fn py_eq(a: &Value, b: &Value) -> bool {
    let num = |v: &Value| match v {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::String(_) if matches!(exotic(v), Some(Exotic::Float(_) | Exotic::Int(_))) => py_float_value(v),
        _ => None,
    };
    match (num(a), num(b)) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

/// Search the official Omarchy plugin marketplace. (Python default: `limit=40`.)
///
/// Retired plugins are excluded: they were delisted, so offering them would
/// be recommending something the registry has withdrawn.
///
/// (The Python's `_reverse` str subclass -- "sorts as its own reverse, so a
/// date field reads newest-first" -- becomes a reversed comparison inside
/// this function's sort.)
///
/// Err where the Python raises on a registry of the wrong shape: not an
/// object, `sources` that are not a list of objects, and the like.
pub fn try_search_plugins(query: &str, limit: usize) -> Result<Vec<App>, String> {
    let reg = plugin_registry(false);
    let Value::Object(reg) = &reg else {
        return Err(format!("'{}' object has no attribute 'get'", py_type(&reg)));
    };
    // sources = reg.get("sources") or []
    let sources = match reg.get("sources") {
        Some(v) if truthy(v) => py_iter(v)?,
        _ => return Ok(Vec::new()),
    };

    // set(reg.get("retiredPluginIds") or [])
    let retired: Vec<Value> = match reg.get("retiredPluginIds") {
        Some(v) if truthy(v) => py_iter(v)?,
        _ => Vec::new(),
    };
    if let Some(bad) = retired.iter().find(|v| v.is_array() || v.is_object()) {
        return Err(format!("unhashable type: '{}'", py_type(bad)));
    }
    let installed: HashSet<String> = sorted_children(&crate::xdg::config_home().join("omarchy/plugins"))
        .into_iter()
        .filter(|d| d.is_dir())
        .map(|d| name_of(&d))
        .collect();
    let q = py_strip(query).to_lowercase();
    let mut out: Vec<App> = Vec::new();
    for entry in &sources {
        let Value::Object(e) = entry else {
            return Err(format!("'{}' object has no attribute 'get'", py_type(entry)));
        };
        let repo = match e.get("repo") {
            Some(v) if truthy(v) => v.clone(),
            _ => json!(""),
        };
        let (name, desc, author) = plugin_name(entry)?;
        let hay = format!("{} {} {} {}", py_str(&name), py_str(&desc), py_str(&author), py_str(&repo))
            .to_lowercase();
        if !q.is_empty() && !hay.contains(&q) {
            continue;
        }
        if name.is_array() || name.is_object() {
            return Err(format!("unhashable type: '{}'", py_type(&name)));
        }
        if retired.iter().any(|r| py_eq(r, &name)) {
            continue;
        }
        // The sort below calls `a.name.lower()` on every row.
        let Some(name) = real_str(&name) else {
            return Err(format!("'{}' object has no attribute 'lower'", py_type(&name)));
        };
        // `desc or f"..." if author else "Omarchy plugin"` parses as
        // `(desc or f"...") if author else "Omarchy plugin"`.
        let description = if truthy(&author) {
            if truthy(&desc) { py_str(&desc) } else { format!("Omarchy plugin — {}", py_str(&author)) }
        } else {
            "Omarchy plugin".to_string()
        };
        // (entry.get("addedAt") or "")[:10]
        let added = match e.get("addedAt") {
            Some(a) if real_str(a).is_some() => match exotic(a) {
                Some(Exotic::WStr(t)) => units_json(&wstr_units(t).into_iter().take(10).collect::<Vec<_>>()),
                _ => json!(py_prefix(&real_str(a).unwrap_or_default(), 10)),
            },
            Some(Value::Array(a)) => Value::Array(a.iter().take(10).cloned().collect()),
            Some(v) if truthy(v) => {
                return Err(format!("'{}' object is not subscriptable", py_type(v)))
            }
            _ => json!(""),
        };
        let mut extra = Map::new();
        extra.insert("installable".into(), json!(true));
        extra.insert("origin".into(), json!(PLUGIN));
        extra.insert("is_app".into(), json!(true));
        extra.insert("url".into(), repo.clone());
        extra.insert("repo".into(), repo);
        extra.insert("author".into(), author);
        // A validated listing has been checked by the marketplace;
        // worth showing, since anyone can submit a repo.
        extra.insert("verified".into(), json!(e.get("listingValidatedAt").is_some_and(truthy)));
        extra.insert("added".into(), added);
        extra.insert("suite".into(), json!(e.get("type").and_then(Value::as_str) == Some("suite")));
        extra.insert("already".into(), json!(installed.contains(&name)));
        out.push(
            App { name, source: PLUGIN.into(), description, extra, ..Default::default() }
                // (A non-string type, null included, is kept as it is.)
                .with_values(&[("version", &e.get("type").cloned().unwrap_or(json!("plugin")))]),
        );
        // Browsing (no query) needs the whole set available to sort before
        // trimming, or "most recent" would only be the most recent of an
        // arbitrary first slice.
        if !q.is_empty() && out.len() >= limit * 3 {
            break;
        }
    }

    // Verified listings first, then the ones with real metadata.
    let key = |a: &App| -> (bool, bool, String, String) {
        let verified = a.extra.get("verified").is_some_and(truthy);
        let added_v = a.extra.get("added").cloned().unwrap_or(json!(""));
        (!verified, !truthy(&added_v), py_str(&added_v), a.name.to_lowercase())
    };
    out.sort_by(|a, b| {
        let (ka, kb) = (key(a), key(b));
        ka.0.cmp(&kb.0)
            .then(ka.1.cmp(&kb.1))
            .then(kb.2.cmp(&ka.2)) // _reverse: newest first
            .then(ka.3.cmp(&kb.3))
    });
    out.truncate(limit);
    Ok(out)
}

/// `try_search_plugins` for callers that need a plain list: where the
/// Python raises, this answers `[]`.
pub fn search_plugins(query: &str, limit: usize) -> Vec<App> {
    try_search_plugins(query, limit).unwrap_or_default()
}

// ------------------------------------------------------------------ parity --

/// Parity hook: see ARCHITECTURE.md.
///
/// Common args: `env` ({VAR: value | null}, applied for the call; null
/// removes it) and `files` ({absolute path: text | null}, written -- or
/// removed -- before the call). A call that reaches a module still `todo!()`
/// is reported as an error object rather than taking the whole run down.
pub fn parity(func: &str, args: &Value) -> Option<Value> {
    let saved: Vec<(String, Option<std::ffi::OsString>)> = args
        .get("env")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .map(|(k, v)| {
                    let old = std::env::var_os(k);
                    match v.as_str() {
                        Some(s) => std::env::set_var(k, s),
                        None => std::env::remove_var(k),
                    }
                    (k.clone(), old)
                })
                .collect()
        })
        .unwrap_or_default();
    if let Some(files) = args.get("files").and_then(Value::as_object) {
        for (p, content) in files {
            let p = Path::new(p);
            match content.as_str() {
                Some(text) => {
                    if let Some(parent) = p.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let _ = std::fs::write(p, text);
                }
                None => {
                    let _ = std::fs::remove_file(p);
                }
            }
        }
    }
    let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| parity_call(func, args)))
        .unwrap_or_else(|_| Some(json!({ "error": "blocked: panicked (a module still todo!()?)" })));
    for (k, old) in saved {
        match old {
            Some(v) => std::env::set_var(&k, v),
            None => std::env::remove_var(&k),
        }
    }
    out
}

/// Apps sorted by (real_source or source, name), for collectors whose Python
/// order comes from a set.
fn sorted_apps(mut apps: Vec<App>) -> Vec<App> {
    let k = |a: &App| {
        (a.extra.get("real_source").and_then(Value::as_str).unwrap_or(&a.source).to_string(), a.name.clone())
    };
    apps.sort_by_key(k);
    apps
}

/// A `fetched_at` that is "now" cannot match across two processes: replace
/// it with whether it is within the last ten minutes.
fn mask_time(v: &mut Value) {
    if let Some(m) = v.as_object_mut() {
        for (k, x) in m.iter_mut() {
            if k == "fetched_at" {
                if let Some(f) = x.as_f64() {
                    *x = json!(if f > util::now() - 600.0 { "recent" } else { "old" });
                }
            } else {
                mask_time(x);
            }
        }
    }
}

/// A Result as the parity hook reports it.
fn to<T: Serialize>(r: Result<T, String>) -> Value {
    match r {
        Ok(v) => serde_json::to_value(v).unwrap_or(Value::Null),
        Err(e) => json!({ "error": e }),
    }
}

fn parity_call(func: &str, args: &Value) -> Option<Value> {
    let s = |k: &str| util::arg_str(args, k).to_string();
    let b = |k: &str, d: bool| args.get(k).and_then(Value::as_bool).unwrap_or(d);
    let n = |k: &str, d: usize| args.get(k).and_then(Value::as_u64).map(|x| x as usize).unwrap_or(d);
    let obj = |k: &str| args.get(k).and_then(Value::as_object).cloned().unwrap_or_default();
    let apps = |v: Vec<App>| -> Value {
        let v = if b("sort", false) { sorted_apps(v) } else { v };
        serde_json::to_value(v).unwrap_or(Value::Null)
    };
    let res = |r: Result<Value, String>| r.unwrap_or_else(|e| json!({"error": e}));
    let v = match func {
        "criticality" => json!(criticality(&s("name"))),
        "staleness" => {
            let aur: BTreeMap<String, AurHealth> =
                serde_json::from_value(args.get("aur").cloned().unwrap_or(json!({}))).unwrap_or_default();
            let desk: DesktopEntry =
                serde_json::from_value(args.get("desk").cloned().unwrap_or(json!({}))).unwrap_or_default();
            let meta: BTreeMap<String, String> =
                serde_json::from_value(args.get("meta").cloned().unwrap_or(json!({}))).unwrap_or_default();
            json!(staleness(&s("name"), &s("source"), &aur, &desk, &meta))
        }
        "aur_health" => to(try_aur_health(&util::arg_strs(args, "names"))),
        "size_mb" => json!(size_mb(&s("value"))),
        "date_ts" => json!(date_ts(&s("value"))),
        "package_info" => to(package_info(&util::arg_strs(args, "names"))),
        "desktop_map" => to(desktop_map(&util::arg_strs(args, "names"))),
        "pending_repo" => to(try_pending_repo()),
        "pending_aur" => to(try_pending_aur()),
        "synced_version" => to(try_synced_version(&s("name"))),
        "packages" => res(packages().map(apps)),
        "load_registry" => load_registry_value(),
        // `data_text`: the data as JSON text, for values (an int past 64
        // bits) the args themselves cannot carry exactly.
        "save_registry" => match save_registry(
            &args
                .get("data_text")
                .and_then(Value::as_str)
                .and_then(|t| py_loads(t).ok())
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_else(|| obj("data")),
        ) {
            Ok(()) => {
                let text = std::fs::read_to_string(registry()).unwrap_or_default();
                let tmps: Vec<String> = sorted_children(&state())
                    .into_iter()
                    .filter(|p| p.extension().is_some_and(|e| e == "tmp"))
                    .map(|p| name_of(&p))
                    .collect();
                json!({"text": text, "tmp": tmps})
            }
            Err(e) => json!({"error": e}),
        },
        "appimages" => match appimages(b("check_updates", true)) {
            Ok(v) => apps(v),
            Err(e) => json!({"error": e}),
        },
        "versions_behind" => {
            let (c, k) = versions_behind_big(&s("current"), &s("latest"));
            json!([big_json(&c), k])
        }
        "orphan_advice" => json!(orphan_advice(&s("name"))),
        "orphans" => res(try_orphans().map(apps)),
        "duplicates" => {
            let list: Vec<App> =
                serde_json::from_value(args.get("apps").cloned().unwrap_or(json!([]))).unwrap_or_default();
            let mut m = Map::new();
            for (k, v) in duplicates(&list) {
                m.insert(k, json!(v));
            }
            Value::Object(m)
        }
        "flatpaks" => res(flatpaks().map(apps)),
        "git_cache" => git_cache_value(),
        "git_behind" => {
            let mut cache = obj("cache");
            let mut r = match git_behind_raw(Path::new(&s("repo")), &mut cache) {
                Ok(r) => Value::Object(r),
                Err(e) => return Some(json!({"error": e})),
            };
            let mut c = Value::Object(cache);
            if b("mask_time", false) {
                mask_time(&mut r);
                mask_time(&mut c);
            }
            json!({"result": r, "cache": c})
        }
        "github_page" => json!(github_page(&s("url"))),
        "local_builds" => {
            let mut v = match local_builds(b("check_updates", true)) {
                Ok(v) => apps(v),
                Err(e) => return Some(json!({"error": e})),
            };
            if b("mask_time", false) {
                let mut c = py_read_json(&git_cache_path()).unwrap_or(Value::Null);
                mask_time(&mut c);
                v = json!({"apps": v, "cache": c});
            }
            v
        }
        "collect" => apps(collect()),
        "details" => {
            let app: App = serde_json::from_value(args.get("app").cloned().unwrap_or(json!({}))).unwrap_or_default();
            res(try_details(&app).map(Value::Object))
        }
        "search_available" => {
            let mut v = match try_search_available(&s("query"), n("limit", 60)) {
                Ok(v) => v,
                Err(e) => return Some(json!({"error": e})),
            };
            // The fake-pacman cases compare only the repo half: the AUR half
            // is the network, covered by the real-machine cases.
            if let Some(o) = args.get("only_origin").and_then(Value::as_str) {
                v.retain(|a| a.extra.get("origin").and_then(Value::as_str) == Some(o));
            }
            apps(v)
        }
        "plugin_registry" => plugin_registry(b("force", false)),
        "plugin_name" => {
            res(plugin_name(&args.get("entry").cloned().unwrap_or(json!({}))).map(|(a, b2, c)| json!([a, b2, c])))
        }
        "search_plugins" => res(try_search_plugins(&s("query"), n("limit", 40)).map(apps)),
        _ => return None,
    };
    Some(wire(v))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Tests that move XDG variables run one at a time.
    static ENV: Mutex<()> = Mutex::new(());

    fn bh(c: &str, l: &str) -> (i64, String) {
        versions_behind(c, l)
    }

    // ---- VersionsBehind
    #[test]
    fn test_counts_first_differing_level_only() {
        // 1.2.9 -> 3.0.0 is two majors; the lower components restart, so
        // differencing them as well would be meaningless.
        assert_eq!(bh("1.2.9", "3.0.0"), (2, "major".into()));
        assert_eq!(bh("0.6.0", "0.12.0"), (6, "minor".into()));
        assert_eq!(bh("1.2.3", "1.2.9"), (6, "patch".into()));
    }

    #[test]
    fn test_pkgrel_bump_is_a_rebuild_not_a_release() {
        assert_eq!(bh("0.29.0-1", "0.29.0-2"), (0, "rel".into()));
    }

    #[test]
    fn test_epoch_is_ignored() {
        // "1:1.2.96" -- the epoch is packaging metadata, not an upstream version.
        assert_eq!(bh("1:1.2.96.518-2", "1:1.3.4.258-1"), (1, "minor".into()));
    }

    #[test]
    fn test_date_stamped_versions_are_not_counted_as_releases() {
        // 20181225 -> 20261002 differenced as a release count gives 79,777,
        // which is a fact about calendars, not about software.
        assert_eq!(bh("20181225-1", "20261002.r63-3"), (0, "".into()));
        assert_eq!(bh("2026.10.16", "2026.10.17"), (0, "".into()));
    }

    #[test]
    fn test_unparseable_is_silent_rather_than_wrong() {
        assert_eq!(bh("", "1.0"), (0, "".into()));
        assert_eq!(bh("git-abc123", "git-def456"), (0, "".into()));
    }

    // ---- OrphanAdvice
    #[test]
    fn test_build_tooling_is_kept() {
        // Removing rust just means downloading 300MB again on the next AUR build.
        for name in ["rust", "python-build", "python-installer", "cmake"] {
            assert_eq!(orphan_advice(name), "keep", "{name}");
        }
    }

    #[test]
    fn test_debug_and_docs_are_kept() {
        assert_eq!(orphan_advice("logiops-debug"), "keep");
        assert_eq!(orphan_advice("foo-docs"), "keep");
    }

    #[test]
    fn test_ordinary_orphan_is_removable() {
        assert_eq!(orphan_advice("libfoo"), "remove");
    }

    // ---- Criticality
    #[test]
    fn test_network_facing_is_security() {
        assert_eq!(criticality("google-chrome"), "security");
        assert_eq!(criticality("openssh"), "security");
    }

    #[test]
    fn test_bin_suffix_is_stripped_before_matching() {
        assert_eq!(criticality("nordvpn-bin"), "security");
    }

    #[test]
    fn test_boot_critical_is_core() {
        assert_eq!(criticality("linux"), "core");
        assert_eq!(criticality("mkinitcpio"), "core");
    }

    #[test]
    fn test_ordinary_package_is_neither() {
        assert_eq!(criticality("bat"), "");
    }

    // ---- Duplicates
    fn app(name: &str, source: &str) -> App {
        App::new(name, "1", source)
    }

    #[test]
    fn test_bin_and_source_build_are_one_app() {
        let apps = [app("cursor", REPO), app("cursor-bin", AUR)];
        assert!(duplicates(&apps).iter().any(|(k, _)| k == "cursor"));
    }

    #[test]
    fn test_orphans_are_not_counted_as_duplicates() {
        // An orphaned library is not a second copy of anything.
        let apps = [app("foo", REPO), app("foo", ORPHAN)];
        assert!(duplicates(&apps).is_empty());
    }

    #[test]
    fn test_distinct_software_is_not_flagged() {
        let apps = [app("firefox", REPO), app("chromium", REPO)];
        assert!(duplicates(&apps).is_empty());
    }

    // ---- RegistryWrites
    fn with_state<T>(f: impl FnOnce(&Path) -> T) -> T {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let old = std::env::var_os("XDG_STATE_HOME");
        std::env::set_var("XDG_STATE_HOME", tmp.path());
        let r = f(tmp.path());
        match old {
            Some(v) => std::env::set_var("XDG_STATE_HOME", v),
            None => std::env::remove_var("XDG_STATE_HOME"),
        }
        r
    }

    #[test]
    fn test_save_is_atomic() {
        with_state(|tmp| {
            let target = tmp.join("almanac/appimages.json");
            let data = json!({"a": {"name": "a"}}).as_object().cloned().unwrap();
            save_registry(&data).unwrap();
            let back: Value = serde_json::from_str(&std::fs::read_to_string(&target).unwrap()).unwrap();
            assert_eq!(back["a"]["name"], "a");
            // The temp file used for the swap must not be left behind.
            let tmps: Vec<_> = std::fs::read_dir(tmp.join("almanac"))
                .unwrap()
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().is_some_and(|x| x == "tmp"))
                .collect();
            assert!(tmps.is_empty());
        });
    }

    #[test]
    fn test_corrupt_registry_does_not_raise() {
        with_state(|tmp| {
            let target = tmp.join("almanac/appimages.json");
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(&target, "{ this is not json").unwrap();
            assert!(load_registry().is_empty(), "a damaged registry must not take the window down");
        });
    }

    // ---- UpdatesClearAfterUpdating
    #[test]
    fn test_commit_count_is_recounted_while_the_fetch_is_cached() {
        let d = tempfile::tempdir().unwrap();
        let git = |a: &[&str], cwd: &Path| {
            let mut v = vec!["git", "-c", "user.name=t", "-c", "user.email=t@t"];
            v.extend_from_slice(a);
            let out = util::run_with(&v, &util::RunOpts { cwd: Some(cwd), ..Default::default() });
            assert!(out.ok(), "{:?}: {}", a, out.stderr);
        };
        let (up, clone) = (d.path().join("up"), d.path().join("clone"));
        git(&["init", "-q", "-b", "main", up.to_str().unwrap()], d.path());
        git(&["commit", "-q", "--allow-empty", "-m", "one"], &up);
        git(&["clone", "-q", up.to_str().unwrap(), clone.to_str().unwrap()], d.path());
        git(&["commit", "-q", "--allow-empty", "-m", "two"], &up);

        let mut cache = Map::new();
        assert_eq!(git_behind(&clone, &mut cache).unwrap().behind, Some(1));
        git(&["pull", "-q", "--ff-only"], &clone);
        // Inside the fetch window: no network, but the count is fresh.
        assert_eq!(git_behind(&clone, &mut cache).unwrap().behind, Some(0));
    }

    // ---- Python's json (port-specific: what serde_json alone cannot read)
    #[test]
    fn test_python_json_round_trips_what_serde_cannot_hold() {
        let text = r#"{"a": NaN, "b": -Infinity, "big": 1180591620717411303424, "s": "\ud800x", "e": "\ud83d\ude00", "n": -0}"#;
        let v = py_loads(text).unwrap();
        assert_eq!(
            py_dumps(&v, None),
            r#"{"a": NaN, "b": -Infinity, "big": 1180591620717411303424, "s": "\ud800x", "e": "\ud83d\ude00", "n": 0}"#
        );
        assert!(py_float_value(&v["b"]).is_some_and(|f| f == f64::NEG_INFINITY));
        assert_eq!(py_type(&v["big"]), "int");
        assert_eq!(py_str(&v["a"]), "nan");
        assert!(py_loads("{\"a\": 1} x").is_err());
    }

    #[test]
    fn test_int_text_takes_what_python_int_takes() {
        assert_eq!(py_int_text("1_0").as_deref(), Some("10"));
        assert_eq!(py_int_text("\u{663}").as_deref(), Some("3"));
        assert_eq!(py_int_text(" +4 ").as_deref(), Some("4"));
        assert_eq!(py_int_text("1__0"), None);
        assert_eq!(py_int_text("_1"), None);
        assert_eq!(py_int_text("x"), None);
    }

    #[test]
    fn test_str_of_a_list_is_its_repr() {
        assert_eq!(py_str(&json!(["Al", 1, null, {"k": true}])), "['Al', 1, None, {'k': True}]");
        assert_eq!(py_repr(&json!("it's")), "\"it's\"");
    }

    #[test]
    fn test_nul_in_argv_is_an_error() {
        assert!(run(&["true", "a\0b"], 5).is_err());
    }

    // ---- AnyonesMachine
    #[test]
    fn test_local_builds_are_found_by_being_checkouts_not_by_name() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        let repo = d.path().join("some-driver");
        assert!(util::run(&["git", "init", "-q", repo.to_str().unwrap()], 30).ok());
        let saved: Vec<_> = ["XDG_DATA_HOME", "XDG_CONFIG_HOME"]
            .iter()
            .map(|k| (k.to_string(), std::env::var_os(k)))
            .collect();
        for (k, _) in &saved {
            std::env::set_var(k, d.path());
        }
        let names: Vec<String> = std::panic::catch_unwind(|| {
            local_builds(false).unwrap().into_iter().map(|a| a.name).collect()
        })
        .unwrap_or_default();
        for (k, v) in saved {
            match v {
                Some(v) => std::env::set_var(&k, v),
                None => std::env::remove_var(&k),
            }
        }
        assert!(names.contains(&"some-driver".to_string()), "{names:?}");
    }
}
