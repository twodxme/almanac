//! Port of `shortcuts.py`.
//!
//! Keyboard shortcuts that open a plugin's panel, in Hyprland's bindings.lua.
//!
//! The approach is omaplug's (github.com/fross100/omaplug, MIT): check the live
//! bindings for a conflict before writing, write only a block Almanac owns and
//! can recognise again, and undo the change if Hyprland reports a config error
//! after reloading.
//!
//! Only Omarchy's Lua configuration is supported -- ~/.config/hypr/bindings.lua
//! with `o.bind(...)` -- since that is the one file a user is told to put their
//! own bindings in. Anywhere else, `available()` is False and the feature is
//! not offered.
//!
//! A block looks like this, and is the only text this module ever edits:
//!
//! ```text
//!     -- almanac-shortcut-start: lxp.vitals
//!     o.bind("SUPER + ALT + V", "Almanac: lxp.vitals", "omarchy-shell shell toggle lxp.vitals")
//!     -- almanac-shortcut-end: lxp.vitals
//! ```

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::{util, xdg};

/// Hyprland modifier masks, in the order a combination is written.
pub const MODIFIERS: &[(&str, u32)] = &[("SUPER", 64), ("CTRL", 4), ("ALT", 8), ("SHIFT", 1)];

/// Python `_KEY.fullmatch`: anchored here.
static KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"^(?:[A-Z0-9]|F(?:[1-9]|1[0-2])|SLASH|SPACE|RETURN|TAB|HOME|END|",
        r"INSERT|DELETE|PAGEUP|PAGEDOWN|UP|DOWN|LEFT|RIGHT|COMMA|PERIOD|",
        r"SEMICOLON|APOSTROPHE|MINUS|EQUAL|GRAVE|BACKSLASH|",
        r"BRACKETLEFT|BRACKETRIGHT)$"
    ))
    .unwrap()
});
/// Python `_ID.match`. Python's `$` also matches before one trailing
/// newline; `id_match` reproduces that.
static ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._-]*$").unwrap());

fn id_match(pid: &str) -> bool {
    ID.is_match(pid) || pid.strip_suffix('\n').is_some_and(|p| ID.is_match(p))
}

/// Python's `str.isspace()`: Unicode White_Space plus the four ASCII
/// information separators (U+001C..U+001F) that Python also counts.
fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Python's `str.strip()` with no arguments.
fn py_strip(s: &str) -> &str {
    s.trim_matches(py_isspace)
}

/// Python's `str.splitlines()` (no ends kept).
fn py_splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        let sep = matches!(
            c,
            '\n' | '\r' | '\u{0b}' | '\u{0c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}'
                | '\u{2028}' | '\u{2029}'
        );
        if !sep {
            continue;
        }
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
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// Text mode's universal newlines: `\r\n` and `\r` read as `\n`.
fn universal_newlines(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}

/// Python's `str.isprintable()` for one character: false for the categories
/// `repr()` escapes (Cc, Cf, Co, Cn, Zl, Zp, and Zs other than space).
fn py_isprintable(c: char) -> bool {
    if c.is_ascii() {
        return (' '..='~').contains(&c);
    }
    static NONPRINT: LazyLock<Option<Regex>> =
        LazyLock::new(|| Regex::new(r"^[\p{Cc}\p{Cf}\p{Co}\p{Cn}\p{Zl}\p{Zp}\p{Zs}]$").ok());
    match &*NONPRINT {
        Some(re) => !re.is_match(c.encode_utf8(&mut [0u8; 4])),
        None => true,
    }
}

/// Python's `repr()` of a str.
fn py_str_repr(s: &str) -> String {
    let units: Vec<u32> = s.chars().map(|c| c as u32).collect();
    py_units_repr(&units)
}

/// `repr()` of a str given as code points, which may include lone
/// surrogates (escaped `\udXXX`, as Python does: they are not printable).
fn py_units_repr(units: &[u32]) -> String {
    let has = |c: char| units.contains(&(c as u32));
    let quote = if has('\'') && !has('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(quote);
    for &u in units {
        let Some(c) = char::from_u32(u) else {
            out.push_str(&format!("\\u{u:04x}"));
            continue;
        };
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if py_isprintable(c) => out.push(c),
            c => {
                let n = c as u32;
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

/// An OSError's `str()`: `[Errno 2] No such file or directory: '/path'`,
/// with Python's `repr()` of each filename (and `-> 'dst'` for a second one,
/// as `os.replace` reports it).
fn os_error(e: &std::io::Error, path: Option<&Path>, path2: Option<&Path>) -> String {
    match e.raw_os_error() {
        Some(n) => {
            let msg = e.to_string();
            let mut out = format!("[Errno {n}] {}", msg.strip_suffix(&format!(" (os error {n})")).unwrap_or(&msg));
            if let Some(p) = path {
                out.push_str(&format!(": {}", py_str_repr(&p.to_string_lossy())));
                if let Some(p2) = path2 {
                    out.push_str(&format!(" -> {}", py_str_repr(&p2.to_string_lossy())));
                }
            }
            out
        }
        None => e.to_string(),
    }
}

/// A `UnicodeDecodeError`'s `str()` for strict UTF-8, as Python words it.
fn utf8_error(bytes: &[u8], e: &std::str::Utf8Error) -> String {
    let start = e.valid_up_to();
    let first = bytes.get(start).copied().unwrap_or(0);
    let (end, reason) = match e.error_len() {
        None => (bytes.len(), "unexpected end of data"),
        Some(1) if !(0xc2..=0xf4).contains(&first) => (start + 1, "invalid start byte"),
        Some(n) => (start + n, "invalid continuation byte"),
    };
    if end - start <= 1 {
        format!("'utf-8' codec can't decode byte 0x{first:02x} in position {start}: {reason}")
    } else {
        format!("'utf-8' codec can't decode bytes in position {start}-{}: {reason}", end - 1)
    }
}

/// `bytes.decode("utf-8")` (strict); Err with Python's message.
fn decode_utf8(bytes: Vec<u8>) -> Result<String, String> {
    String::from_utf8(bytes).map_err(|e| utf8_error(e.as_bytes(), &e.utf8_error()))
}

// ---- values as Python's `json.loads` returns them --------------------------
//
// hyprctl's output and plugin manifests go through `json.loads`, which takes
// NaN / Infinity / -Infinity and integers of any size, and whose error text
// app.py shows. serde_json differs on all three, so this module parses with
// Python's grammar into a value with Python's semantics.
//
// Python's decoder recurses in C and nests as deep as the C stack allows
// (Python 3.14: about 52k levels, then RecursionError); `repr()` of the
// result likewise. Recursing here would overflow Rust's stack long before
// that and abort the process, so parsing, `repr()` and dropping a value all
// run with an explicit stack, and the depth where Python gives up is
// reproduced with a fixed limit measured on Python 3.14 (8 MiB stack).

/// A value as Python's `json.loads` returns it.
#[derive(Debug, PartialEq)]
enum PyVal {
    None,
    Bool(bool),
    Int(i128),
    /// An int too large for i128, as its decimal digits.
    BigInt(String),
    Float(f64),
    Str(String),
    /// A str holding lone surrogates (from `\udXXX` escapes), which a Python
    /// str can hold and a Rust String cannot: its code points. Never equal to
    /// a `Str`, as such a Python str is never equal to a surrogate-free one.
    SurStr(Vec<u32>),
    List(Vec<PyVal>),
    /// Insertion order, as a Python dict keeps it. Keys are `Str`/`SurStr`.
    Dict(Vec<(PyVal, PyVal)>),
}

/// Dropping a deeply nested value would recurse once per level.
impl Drop for PyVal {
    fn drop(&mut self) {
        fn take(v: &mut PyVal, into: &mut Vec<PyVal>) {
            match v {
                PyVal::List(a) => into.append(a),
                PyVal::Dict(d) => into.extend(d.drain(..).map(|(_, x)| x)),
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

/// A str value from code points: `Str` unless it holds a lone surrogate.
fn str_from_units(units: Vec<u32>) -> PyVal {
    match units.iter().map(|u| char::from_u32(*u)).collect::<Option<String>>() {
        Some(s) => PyVal::Str(s),
        None => PyVal::SurStr(units),
    }
}

/// `repr()` nesting Python 3.14 manages before RecursionError ("Stack
/// overflow ... while getting the repr of an object"): 37228 lists or 30651
/// dicts; a mix uses the stack in proportion. Each level costs the other
/// kind's count, out of a budget of their product.
pub(crate) const REPR_LIST_COST: u64 = 30651;
pub(crate) const REPR_DICT_COST: u64 = 37228;
pub(crate) const REPR_BUDGET: u64 = 37228 * 30651;
pub(crate) const REPR_OVERFLOW: &str = "Stack overflow (used 8148 kB) while getting the repr of an object";

impl PyVal {
    /// From a serde value (the parity hook's arguments).
    fn from_json(v: &Value) -> PyVal {
        match v {
            Value::Null => PyVal::None,
            Value::Bool(b) => PyVal::Bool(*b),
            Value::Number(n) => match (n.as_i64(), n.as_u64()) {
                (Some(i), _) => PyVal::Int(i as i128),
                (_, Some(u)) => PyVal::Int(u as i128),
                _ => PyVal::Float(n.as_f64().unwrap_or(f64::NAN)),
            },
            Value::String(s) => PyVal::Str(s.clone()),
            Value::Array(a) => PyVal::List(a.iter().map(PyVal::from_json).collect()),
            Value::Object(o) => PyVal::Dict(
                o.iter().map(|(k, x)| (PyVal::Str(k.clone()), PyVal::from_json(x))).collect(),
            ),
        }
    }

    /// `dict.get(key)` (None when absent).
    fn get(&self, key: &str) -> Option<&PyVal> {
        match self {
            PyVal::Dict(d) => d.iter().find(|(k, _)| matches!(k, PyVal::Str(s) if s == key)).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Python's `type(v).__name__`.
    fn type_name(&self) -> &'static str {
        match self {
            PyVal::None => "NoneType",
            PyVal::Bool(_) => "bool",
            PyVal::Int(_) | PyVal::BigInt(_) => "int",
            PyVal::Float(_) => "float",
            PyVal::Str(_) | PyVal::SurStr(_) => "str",
            PyVal::List(_) => "list",
            PyVal::Dict(_) => "dict",
        }
    }

    /// Python truthiness.
    fn truthy(&self) -> bool {
        match self {
            PyVal::None => false,
            PyVal::Bool(b) => *b,
            PyVal::Int(i) => *i != 0,
            PyVal::BigInt(_) => true,
            PyVal::Float(f) => *f != 0.0, // NaN is true
            PyVal::Str(s) => !s.is_empty(),
            PyVal::SurStr(u) => !u.is_empty(),
            PyVal::List(a) => !a.is_empty(),
            PyVal::Dict(d) => !d.is_empty(),
        }
    }

    /// Python `str()`. A lone surrogate, which no Rust String can hold,
    /// comes out as U+FFFD; inside a container's repr it is escaped as
    /// Python escapes it. Err is Python's RecursionError.
    fn py_str(&self) -> Result<String, String> {
        match self {
            PyVal::Str(s) => Ok(s.clone()),
            PyVal::SurStr(u) => Ok(u.iter().map(|c| char::from_u32(*c).unwrap_or('\u{fffd}')).collect()),
            other => other.py_repr(),
        }
    }

    /// `repr()` of anything but a list or dict.
    fn leaf_repr(&self) -> String {
        match self {
            PyVal::None => "None".into(),
            PyVal::Bool(b) => if *b { "True" } else { "False" }.into(),
            PyVal::Int(i) => i.to_string(),
            PyVal::BigInt(s) => s.clone(),
            PyVal::Float(f) if f.is_nan() => "nan".into(),
            PyVal::Float(f) if f.is_infinite() => if *f > 0.0 { "inf" } else { "-inf" }.into(),
            PyVal::Float(f) => util::py_float_repr(*f),
            PyVal::Str(s) => py_str_repr(s),
            PyVal::SurStr(u) => py_units_repr(u),
            PyVal::List(_) | PyVal::Dict(_) => String::new(),
        }
    }

    /// Python `repr()`; Err is its RecursionError on very deep nesting.
    fn py_repr(&self) -> Result<String, String> {
        enum W<'a> {
            Val(&'a PyVal),
            Lit(&'static str),
            Exit(u64),
        }
        let mut out = String::new();
        let mut used = 0u64;
        let mut stack = vec![W::Val(self)];
        while let Some(w) = stack.pop() {
            match w {
                W::Lit(s) => out.push_str(s),
                W::Exit(cost) => used -= cost,
                W::Val(PyVal::List(a)) if !a.is_empty() => {
                    used += REPR_LIST_COST;
                    if used > REPR_BUDGET {
                        return Err(REPR_OVERFLOW.into());
                    }
                    out.push('[');
                    stack.push(W::Exit(REPR_LIST_COST));
                    stack.push(W::Lit("]"));
                    for (i, x) in a.iter().enumerate().rev() {
                        stack.push(W::Val(x));
                        if i > 0 {
                            stack.push(W::Lit(", "));
                        }
                    }
                }
                W::Val(PyVal::Dict(d)) if !d.is_empty() => {
                    used += REPR_DICT_COST;
                    if used > REPR_BUDGET {
                        return Err(REPR_OVERFLOW.into());
                    }
                    out.push('{');
                    stack.push(W::Exit(REPR_DICT_COST));
                    stack.push(W::Lit("}"));
                    for (i, (k, x)) in d.iter().enumerate().rev() {
                        stack.push(W::Val(x));
                        stack.push(W::Lit(": "));
                        stack.push(W::Val(k));
                        if i > 0 {
                            stack.push(W::Lit(", "));
                        }
                    }
                }
                W::Val(PyVal::List(_)) => out.push_str("[]"),
                W::Val(PyVal::Dict(_)) => out.push_str("{}"),
                W::Val(leaf) => out.push_str(&leaf.leaf_repr()),
            }
        }
        Ok(out)
    }
}

/// CPython's limit on int <-> str conversion (`sys.get_int_max_str_digits()`).
const INT_MAX_STR_DIGITS: usize = 4300;

fn int_limit_error(digits: usize) -> String {
    format!(
        "Exceeds the limit ({INT_MAX_STR_DIGITS} digits) for integer string conversion: \
         value has {digits} digits; use sys.set_int_max_str_digits() to increase the limit"
    )
}

/// An int from decimal digits (optionally signed): exact when it fits i128.
fn int_from_digits(text: &str) -> PyVal {
    match text.parse::<i128>() {
        Ok(i) => PyVal::Int(i),
        Err(_) => PyVal::BigInt(text.to_string()),
    }
}

/// Containers `json.loads` opens, one inside the other, before Python 3.14
/// raises RecursionError (measured: 52105 arrays or objects, any mix).
const JSON_MAX_DEPTH: usize = 52105;

/// A failed `json.loads`: a ValueError (JSONDecodeError and the int digit
/// limit) or, with `recursion`, a RecursionError -- which has_panel does not
/// catch.
struct LoadErr {
    msg: String,
    recursion: bool,
}

impl From<LoadErr> for String {
    fn from(e: LoadErr) -> String {
        e.msg
    }
}

impl From<String> for LoadErr {
    fn from(msg: String) -> LoadErr {
        LoadErr { msg, recursion: false }
    }
}

/// `json.loads(s)` with Python's grammar and error text
/// (`Expecting value: line 1 column 1 (char 0)`).
fn py_json_loads(text: &str) -> Result<PyVal, LoadErr> {
    let s: Vec<char> = text.chars().collect();
    let p = PyJson { s: &s };
    if s.first() == Some(&'\u{feff}') {
        return Err(p.err("Unexpected UTF-8 BOM (decode using utf-8-sig)", 0).into());
    }
    let idx = p.ws(0);
    let (v, end) = p.scan_once(idx)?;
    let end = p.ws(end);
    if end != s.len() {
        return Err(p.err("Extra data", end).into());
    }
    Ok(v)
}

/// json.decoder / json.scanner, over the document's characters (Python's
/// error positions count characters).
struct PyJson<'a> {
    s: &'a [char],
}

/// A failed scan of a scalar: StopIteration(idx) ("Expecting value" there),
/// or a finished error message.
enum ScanErr {
    Stop(usize),
    Msg(String),
}

/// An array or object being filled, with (for an object) the key whose
/// value is being scanned.
enum Frame {
    Arr(Vec<PyVal>),
    Obj(Vec<(PyVal, PyVal)>, PyVal),
}

impl PyJson<'_> {
    /// `JSONDecodeError(msg, doc, pos)`'s text.
    fn err(&self, msg: &str, pos: usize) -> String {
        let before = &self.s[..pos.min(self.s.len())];
        let lineno = before.iter().filter(|c| **c == '\n').count() + 1;
        let colno = match before.iter().rposition(|c| *c == '\n') {
            Some(nl) => pos - nl,
            None => pos + 1,
        };
        format!("{msg}: line {lineno} column {colno} (char {pos})")
    }

    fn fail(&self, msg: &str, pos: usize) -> LoadErr {
        self.err(msg, pos).into()
    }

    fn at(&self, i: usize) -> Option<char> {
        self.s.get(i).copied()
    }

    fn is_ws(c: Option<char>) -> bool {
        matches!(c, Some(' ' | '\t' | '\n' | '\r'))
    }

    /// `WHITESPACE.match(s, i).end()` (a start past the end is the end).
    fn ws(&self, i: usize) -> usize {
        let mut i = i.min(self.s.len());
        while Self::is_ws(self.at(i)) {
            i += 1;
        }
        i
    }

    fn starts(&self, i: usize, lit: &str) -> bool {
        let n = lit.chars().count();
        i + n <= self.s.len() && self.s[i..i + n].iter().copied().eq(lit.chars())
    }

    /// `Py_EnterRecursiveCall` before opening the `depth`-th container.
    fn enter(&self, depth: usize, what: &str) -> Result<(), LoadErr> {
        if depth > JSON_MAX_DEPTH {
            return Err(LoadErr {
                msg: format!("Stack overflow (used 8144 kB) while decoding a JSON {what} from a unicode string"),
                recursion: true,
            });
        }
        Ok(())
    }

    /// `scan_once(s, idx)`: one value, with arrays and objects (JSONArray /
    /// JSONObject) kept on an explicit stack instead of recursing.
    fn scan_once(&self, idx: usize) -> Result<(PyVal, usize), LoadErr> {
        let mut stack: Vec<Frame> = Vec::new();
        let mut pos = idx;
        'value: loop {
            // A value starts at `pos`: a scalar, an empty container, or a
            // container to fill.
            let (mut v, mut end) = match self.at(pos) {
                Some('[') => {
                    self.enter(stack.len() + 1, "array")?;
                    let mut end = pos + 1;
                    let mut next = self.at(end);
                    // `'' in ' \t\n\r'` is True in Python: the end of input counts.
                    if next.is_none() || Self::is_ws(next) {
                        end = self.ws(end + 1);
                        next = self.at(end);
                    }
                    if next != Some(']') {
                        stack.push(Frame::Arr(Vec::new()));
                        pos = end;
                        continue 'value;
                    }
                    (PyVal::List(Vec::new()), end + 1)
                }
                Some('{') => {
                    self.enter(stack.len() + 1, "object")?;
                    let mut end = pos + 1;
                    let mut next = self.at(end);
                    let mut empty = false;
                    if next != Some('"') {
                        if next.is_none() || Self::is_ws(next) {
                            end = self.ws(end);
                            next = self.at(end);
                        }
                        if next == Some('}') {
                            empty = true;
                        } else if next != Some('"') {
                            return Err(self.fail("Expecting property name enclosed in double quotes", end));
                        }
                    }
                    if !empty {
                        let (key, e) = self.key(end + 1)?;
                        stack.push(Frame::Obj(Vec::new(), key));
                        pos = e;
                        continue 'value;
                    }
                    (PyVal::Dict(Vec::new()), end + 1)
                }
                _ => self.scalar(pos).map_err(|e| match e {
                    ScanErr::Stop(i) => self.fail("Expecting value", i),
                    ScanErr::Msg(m) => m.into(),
                })?,
            };
            // Hand the finished value to the containers it closes.
            loop {
                match stack.pop() {
                    None => return Ok((v, end)),
                    Some(Frame::Arr(mut values)) => {
                        values.push(v);
                        let mut next = self.at(end);
                        if next.is_none() || Self::is_ws(next) {
                            end = self.ws(end + 1);
                            next = self.at(end);
                        }
                        end += 1;
                        if next == Some(']') {
                            v = PyVal::List(values);
                            continue;
                        } else if next != Some(',') {
                            return Err(self.fail("Expecting ',' delimiter", end - 1));
                        }
                        let comma = end - 1;
                        if Self::is_ws(self.at(end)) {
                            end += 1;
                            if Self::is_ws(self.at(end)) {
                                end = self.ws(end + 1);
                            }
                        }
                        if self.at(end) == Some(']') {
                            return Err(self.fail("Illegal trailing comma before end of array", comma));
                        }
                        stack.push(Frame::Arr(values));
                        pos = end;
                        continue 'value;
                    }
                    Some(Frame::Obj(mut pairs, key)) => {
                        // dict(pairs): a repeated key keeps its first place, last value.
                        match pairs.iter_mut().find(|(k, _)| *k == key) {
                            Some(slot) => slot.1 = v,
                            None => pairs.push((key, v)),
                        }
                        let mut next = self.at(end);
                        if Self::is_ws(next) {
                            end = self.ws(end + 1);
                            next = self.at(end);
                        }
                        end += 1;
                        if next == Some('}') {
                            v = PyVal::Dict(pairs);
                            continue;
                        } else if next != Some(',') {
                            return Err(self.fail("Expecting ',' delimiter", end - 1));
                        }
                        let comma = end - 1;
                        end = self.ws(end);
                        let next = self.at(end);
                        end += 1;
                        if next != Some('"') {
                            if next == Some('}') {
                                return Err(self.fail("Illegal trailing comma before end of object", comma));
                            }
                            return Err(self.fail("Expecting property name enclosed in double quotes", end - 1));
                        }
                        let (key, e) = self.key(end)?;
                        stack.push(Frame::Obj(pairs, key));
                        pos = e;
                        continue 'value;
                    }
                }
            }
        }
    }

    /// An object's key and its `:` (`end` is just after the opening quote);
    /// returns the key and where its value starts.
    fn key(&self, end: usize) -> Result<(PyVal, usize), LoadErr> {
        let (key, mut end) = self.scanstring(end).map_err(|e| match e {
            ScanErr::Stop(i) => self.fail("Expecting value", i),
            ScanErr::Msg(m) => m.into(),
        })?;
        if self.at(end) != Some(':') {
            end = self.ws(end);
            if self.at(end) != Some(':') {
                return Err(self.fail("Expecting ':' delimiter", end));
            }
        }
        end += 1;
        if Self::is_ws(self.at(end)) {
            end += 1;
            if Self::is_ws(self.at(end)) {
                end = self.ws(end + 1);
            }
        }
        Ok((key, end))
    }

    /// `scan_once` for anything but an array or object.
    fn scalar(&self, idx: usize) -> Result<(PyVal, usize), ScanErr> {
        let Some(c) = self.at(idx) else {
            return Err(ScanErr::Stop(idx));
        };
        match c {
            '"' => return self.scanstring(idx + 1),
            'n' if self.starts(idx, "null") => return Ok((PyVal::None, idx + 4)),
            't' if self.starts(idx, "true") => return Ok((PyVal::Bool(true), idx + 4)),
            'f' if self.starts(idx, "false") => return Ok((PyVal::Bool(false), idx + 5)),
            _ => {}
        }
        if let Some(r) = self.number(idx)? {
            return Ok(r);
        }
        if c == 'N' && self.starts(idx, "NaN") {
            Ok((PyVal::Float(f64::NAN), idx + 3))
        } else if c == 'I' && self.starts(idx, "Infinity") {
            Ok((PyVal::Float(f64::INFINITY), idx + 8))
        } else if c == '-' && self.starts(idx, "-Infinity") {
            Ok((PyVal::Float(f64::NEG_INFINITY), idx + 9))
        } else {
            Err(ScanErr::Stop(idx))
        }
    }

    /// NUMBER_RE: `(-?(?:0|[1-9][0-9]*))(\.[0-9]+)?([eE][-+]?[0-9]+)?`.
    fn number(&self, idx: usize) -> Result<Option<(PyVal, usize)>, ScanErr> {
        let digit = |i: usize| self.at(i).is_some_and(|c| c.is_ascii_digit());
        let mut i = idx;
        if self.at(i) == Some('-') {
            i += 1;
        }
        match self.at(i) {
            Some('0') => i += 1,
            Some('1'..='9') => {
                while digit(i) {
                    i += 1;
                }
            }
            _ => return Ok(None),
        }
        let int_end = i;
        let mut is_float = false;
        if self.at(i) == Some('.') && digit(i + 1) {
            i += 1;
            while digit(i) {
                i += 1;
            }
            is_float = true;
        }
        if matches!(self.at(i), Some('e' | 'E')) {
            let mut j = i + 1;
            if matches!(self.at(j), Some('+' | '-')) {
                j += 1;
            }
            if digit(j) {
                while digit(j) {
                    j += 1;
                }
                i = j;
                is_float = true;
            }
        }
        let text: String = self.s[idx..i].iter().collect();
        if is_float {
            return Ok(Some((PyVal::Float(text.parse().unwrap_or(f64::NAN)), i)));
        }
        let digits = int_end - idx - usize::from(self.at(idx) == Some('-'));
        if digits > INT_MAX_STR_DIGITS {
            return Err(ScanErr::Msg(int_limit_error(digits)));
        }
        Ok(Some((int_from_digits(&text), i)))
    }

    /// `_decode_uXXXX(s, pos)`: `pos` is the `u`.
    fn hex4(&self, pos: usize) -> Result<u32, ScanErr> {
        let ok = pos + 5 <= self.s.len() && self.s[pos + 1..pos + 5].iter().all(|c| c.is_ascii_hexdigit());
        if !ok {
            return Err(ScanErr::Msg(self.err("Invalid \\uXXXX escape", pos)));
        }
        let h: String = self.s[pos + 1..pos + 5].iter().collect();
        Ok(u32::from_str_radix(&h, 16).unwrap_or(0))
    }

    /// `scanstring(s, end)`: `end` is just after the opening quote. A lone
    /// surrogate escape stays in the str, as in Python.
    fn scanstring(&self, mut end: usize) -> Result<(PyVal, usize), ScanErr> {
        let begin = end - 1;
        let mut out: Vec<u32> = Vec::new();
        loop {
            // STRINGCHUNK: up to the next quote, backslash or control char.
            let Some(off) = self.s[end.min(self.s.len())..]
                .iter()
                .position(|c| *c == '"' || *c == '\\' || (*c as u32) < 0x20)
            else {
                return Err(ScanErr::Msg(self.err("Unterminated string starting at", begin)));
            };
            out.extend(self.s[end..end + off].iter().map(|c| *c as u32));
            let term = self.s[end + off];
            end += off + 1;
            if term == '"' {
                break;
            }
            if term != '\\' {
                return Err(ScanErr::Msg(self.err("Invalid control character at", end - 1)));
            }
            let Some(esc) = self.at(end) else {
                return Err(ScanErr::Msg(self.err("Unterminated string starting at", begin)));
            };
            if esc != 'u' {
                let ch = match esc {
                    '"' => '"',
                    '\\' => '\\',
                    '/' => '/',
                    'b' => '\u{8}',
                    'f' => '\u{c}',
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    _ => return Err(ScanErr::Msg(self.err("Invalid \\escape", end - 1))),
                };
                out.push(ch as u32);
                end += 1;
                continue;
            }
            let mut uni = self.hex4(end)?;
            end += 5;
            if (0xd800..=0xdbff).contains(&uni) && self.at(end) == Some('\\') && self.at(end + 1) == Some('u') {
                let uni2 = self.hex4(end + 1)?;
                if (0xdc00..=0xdfff).contains(&uni2) {
                    uni = 0x10000 + (((uni - 0xd800) << 10) | (uni2 - 0xdc00));
                    end += 6;
                }
            }
            out.push(uni);
        }
        Ok((str_from_units(out), end))
    }
}

/// The decimal value of a Unicode digit (category Nd), as `int()` reads it.
/// Nd digits come in contiguous runs of ten, zero first (a Unicode
/// stability guarantee), so the value is the place in the run.
fn nd_value(c: char) -> Option<u32> {
    if c.is_ascii_digit() {
        return Some(c as u32 - '0' as u32);
    }
    static ND: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"^\p{Nd}$").ok());
    let re = ND.as_ref()?;
    let is_nd = |c: char| re.is_match(c.encode_utf8(&mut [0u8; 4]));
    if !is_nd(c) {
        return None;
    }
    let mut k = 0;
    let mut cp = c as u32;
    while let Some(prev) = cp.checked_sub(1).and_then(char::from_u32) {
        if !is_nd(prev) {
            break;
        }
        k += 1;
        cp -= 1;
    }
    Some(k % 10)
}

/// Python `int()` of a JSON value; Err (ValueError / TypeError) otherwise.
/// Ok(None) is an int too large to be any modifier mask.
fn py_int(v: &PyVal) -> Result<Option<i128>, String> {
    match v {
        PyVal::Bool(b) => Ok(Some(*b as i128)),
        PyVal::Int(i) => Ok(Some(*i)),
        PyVal::BigInt(_) => Ok(None),
        PyVal::Float(f) if f.is_nan() => Err("cannot convert float NaN to integer".into()),
        PyVal::Float(f) if f.is_infinite() => Err("cannot convert float infinity to integer".into()),
        PyVal::Float(f) => Ok((f.abs() < 1e37).then(|| f.trunc() as i128)),
        // A lone surrogate is never a digit.
        PyVal::SurStr(u) => Err(format!("invalid literal for int() with base 10: {}", py_units_repr(u))),
        PyVal::Str(s) => {
            let bad = || format!("invalid literal for int() with base 10: {}", py_str_repr(s));
            // int() strips ASCII whitespace (space \t \n \v \f \r) and the
            // non-ASCII Unicode spaces -- not str.strip()'s \x1c..\x1f.
            let t = s.trim_matches(char::is_whitespace);
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
                let d = nd_value(c).ok_or_else(bad)?;
                digits.push(char::from_digit(d, 10).unwrap_or('0'));
            }
            if digits.len() > INT_MAX_STR_DIGITS {
                return Err(int_limit_error(digits.len()));
            }
            Ok(digits.parse::<i128>().ok().map(|n| if neg { -n } else { n }))
        }
        other => Err(format!(
            "int() argument must be a string, a bytes-like object or a real number, not '{}'",
            other.type_name()
        )),
    }
}

pub fn bindings_path() -> PathBuf {
    xdg::config_home().join("hypr").join("bindings.lua")
}

/// `shutil.which("hyprctl")`: with PATH unset it searches `os.confstr
/// ("CS_PATH")` (/bin:/usr/bin), with PATH empty nothing, and an empty
/// entry is the current directory. A candidate counts when it exists,
/// `os.access(X_OK)` allows it (the caller's own permission bits, so a
/// file executable only by group or others does not count for its owner)
/// and it is not a directory -- a FIFO or device counts.
fn which(name: &str) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let path = std::env::var_os("PATH").unwrap_or_else(|| "/bin:/usr/bin".into());
    if path.is_empty() {
        return false;
    }
    path.as_bytes().split(|b| *b == b':').any(|d| {
        let f = Path::new(std::ffi::OsStr::from_bytes(d)).join(name);
        f.exists() && rustix::fs::access(&f, rustix::fs::Access::EXEC_OK).is_ok() && !f.is_dir()
    })
}

/// Omarchy's Lua bindings file, and Hyprland to ask about live binds.
pub fn available() -> bool {
    let path = bindings_path();
    // is_file() follows a symlink; is_symlink() then rules one out.
    if !path.is_file() || path.is_symlink() {
        return false;
    }
    let Ok(bytes) = std::fs::read(&path) else {
        return false; // except OSError: return False
    };
    universal_newlines(&String::from_utf8_lossy(&bytes)).contains("o.bind(") && which("hyprctl")
}

/// 'super+alt+v' -> ('SUPER + ALT + V', modmask, 'V'). Err (Python ValueError)
/// with the message to show.
pub fn normalise(value: &str) -> Result<(String, u32, String), String> {
    let is_mod = |p: &str| MODIFIERS.iter().any(|(m, _)| *m == p);
    let parts: Vec<String> =
        value.split('+').map(|p| py_strip(p).to_uppercase().replace("CONTROL", "CTRL")).collect();
    let keys: Vec<&String> = parts.iter().filter(|p| !is_mod(p)).collect();
    let mods: Vec<&String> = parts.iter().filter(|p| is_mod(p)).collect();
    let unique: std::collections::HashSet<&String> = parts.iter().collect();
    if keys.len() != 1 || mods.is_empty() || unique.len() != parts.len() || !KEY.is_match(keys[0]) {
        return Err("Use one or more modifiers (Super, Ctrl, Alt, Shift) \
                    and one key, such as Super + Alt + V."
            .into());
    }
    let ordered: Vec<(&str, u32)> =
        MODIFIERS.iter().copied().filter(|(m, _)| mods.iter().any(|x| x.as_str() == *m)).collect();
    let mut names: Vec<&str> = ordered.iter().map(|(m, _)| *m).collect();
    names.push(keys[0]);
    Ok((names.join(" + "), ordered.iter().map(|(_, v)| v).sum(), keys[0].clone()))
}

fn _block(pid: &str, combo: &str) -> String {
    format!(
        "\n-- almanac-shortcut-start: {pid}\n\
         o.bind({}, {}, {})\n\
         -- almanac-shortcut-end: {pid}\n",
        util::py_json_string(combo),
        util::py_json_string(&format!("Almanac: {pid}")),
        util::py_json_string(&format!("omarchy-shell shell toggle {pid}")),
    )
}

/// `_pattern(pid).search(raw, from)`: the span of the first block at or after
/// `from`. The Python pattern is `\n-- almanac-shortcut-start: <pid>\n.*?
/// -- almanac-shortcut-end: <pid>\n` with re.S, both pids escaped: two
/// literals with the shortest run of anything between, which is what this
/// finds. (It is not compiled as a regex: a long pid would exceed the regex
/// crate's size limit, where Python's re takes any length.) If the first
/// start marker has no end marker after it, no later one can either.
fn _pattern(raw: &str, pid: &str, from: usize) -> Option<(usize, usize)> {
    let start = format!("\n-- almanac-shortcut-start: {pid}\n");
    let end = format!("-- almanac-shortcut-end: {pid}\n");
    let s = raw.get(from..)?.find(&start)? + from;
    let body = s + start.len();
    let e = raw[body..].find(&end)? + body;
    Some((s, e + end.len()))
}

/// `_pattern(pid).sub("", raw)`: every block removed, left to right.
fn strip_blocks(raw: &str, pid: &str) -> String {
    let mut out = String::new();
    let mut pos = 0;
    while let Some((s, e)) = _pattern(raw, pid, pos) {
        out.push_str(&raw[pos..s]);
        pos = e;
    }
    out.push_str(&raw[pos..]);
    out
}

static BIND: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"o\.bind\("([^"\n]+)""#).unwrap());

/// The combination Almanac bound for this plugin, or ''. `raw` is the file's
/// text when the caller already read it (None: read it now). Err when the
/// block was edited by hand, or the file cannot be read.
pub fn current(pid: &str, raw: Option<&str>) -> Result<String, String> {
    let owned;
    let raw = match raw {
        Some(r) => r,
        None => {
            owned = _read()?.0;
            &owned
        }
    };
    let Some((s, e)) = _pattern(raw, pid, 0) else {
        return Ok(String::new());
    };
    let m = &raw[s..e];
    let found = BIND.captures(m).map(|c| c[1].to_string());
    match found {
        Some(combo) if m == _block(pid, &combo) => Ok(combo),
        _ => Err("The shortcut block in bindings.lua was edited by hand; \
                  change it there."
            .into()),
    }
}

/// Why `combo` cannot be used, from Hyprland's live bindings (`hyprctl -j
/// binds`, one JSON object each), or ''. Err when `combo` does not normalise.
pub fn conflict(binds: &[Value], combo: &str, pid: &str) -> Result<String, String> {
    let binds: Vec<PyVal> = binds.iter().map(PyVal::from_json).collect();
    conflict_py(&binds, combo, pid)
}

/// `conflict` over the values `json.loads` gave.
fn conflict_py(binds: &[PyVal], combo: &str, pid: &str) -> Result<String, String> {
    let (_c, mask, key) = normalise(combo)?;
    for b in binds {
        if !matches!(b, PyVal::Dict(_)) {
            return Err(format!("'{}' object has no attribute 'get'", b.type_name()));
        }
        let truthy = |k: &str| b.get(k).is_some_and(PyVal::truthy);
        let modmask = match b.get("modmask") {
            Some(v) => py_int(v)?,
            None => Some(0),
        };
        if modmask.is_none_or(|m| m & !2 != mask as i128) || truthy("mouse") {
            continue; // 2 is Caps Lock, never chosen
        }
        let bound = match b.get("key") {
            Some(k) => k.py_str()?,
            None => String::new(),
        }
        .to_uppercase();
        let unsure = truthy("keycode") || bound.starts_with("CODE:") || truthy("catch_all");
        if bound != key && !unsure {
            continue;
        }
        if bound == key && b.get("description") == Some(&PyVal::Str(format!("Almanac: {pid}"))) {
            continue; // our own, being replaced
        }
        let what = match ["description", "arg", "dispatcher"].iter().filter_map(|k| b.get(k)).find(|v| v.truthy()) {
            Some(v) => v.py_str()?,
            None => "another action".into(),
        };
        if unsure || truthy("submap") {
            return Ok(format!("Cannot be sure it is free ({what}). Choose another."));
        }
        return Ok(format!("Already used by {what}."));
    }
    Ok(String::new())
}

/// The real user id (`os.getuid()`), from /proc without `unsafe`.
fn getuid() -> Option<u32> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("Uid:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

/// (app.py calls this.) bindings.lua's text and permission bits,
/// opened without following a symlink. Err (OSError / ValueError) when it is
/// missing, not a regular file the user owns, or over 1 MiB.
pub fn _read() -> Result<(String, u32), String> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let path = bindings_path();
    let mut fh = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
        .map_err(|e| os_error(&e, Some(&path), None))?;
    let info = fh.metadata().map_err(|e| os_error(&e, None, None))?;
    // os.fdopen refuses a directory before the checks below run, naming the
    // descriptor: IsADirectoryError "[Errno 21] Is a directory: 5".
    if info.is_dir() {
        let e = std::io::Error::from_raw_os_error(libc::EISDIR);
        let msg = os_error(&e, None, None);
        return Err(format!("{msg}: {}", fh.as_raw_fd()));
    }
    if !info.file_type().is_file() || Some(info.uid()) != getuid() || info.size() > 1024 * 1024 {
        return Err("bindings.lua is not a regular file you own.".into());
    }
    let mut bytes = Vec::new();
    fh.read_to_end(&mut bytes).map_err(|e| os_error(&e, None, None))?;
    let text = decode_utf8(bytes)?;
    Ok((universal_newlines(&text), info.mode() & 0o7777))
}

/// `tempfile.mkstemp(prefix=..., dir=dir)`: a new file, mode 0600, opened
/// O_EXCL | O_NOFOLLOW, under a name with eight random characters.
fn mkstemp(dir: &Path, prefix: &str) -> Result<(std::fs::File, PathBuf), String> {
    use std::hash::{BuildHasher, Hasher};
    use std::os::unix::fs::OpenOptionsExt;
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789_";
    for n in 0..10000u64 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(n);
        let mut r = h.finish();
        let name: String = (0..8)
            .map(|_| {
                let c = CHARS[(r % CHARS.len() as u64) as usize] as char;
                r /= CHARS.len() as u64;
                c
            })
            .collect();
        let file = dir.join(format!("{prefix}{name}"));
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&file)
        {
            Ok(f) => return Ok((f, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(os_error(&e, Some(&file), None)),
        }
    }
    Err("[Errno 17] No usable temporary file name found".into())
}

/// Atomically replace bindings.lua with `raw`, keeping `mode`.
fn _write(raw: &str, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let path = bindings_path();
    let parent = path.parent().unwrap_or(Path::new("/")).to_path_buf();
    let (mut fh, tmp) = mkstemp(&parent, ".almanac-shortcut-")?;
    let res = (|| {
        let err = |e: std::io::Error| os_error(&e, None, None);
        fh.set_permissions(std::fs::Permissions::from_mode(mode)).map_err(err)?;
        fh.write_all(raw.as_bytes()).map_err(err)?;
        fh.flush().map_err(err)?;
        fh.sync_all().map_err(err)?;
        drop(fh);
        std::fs::rename(&tmp, &path).map_err(|e| os_error(&e, Some(&tmp), Some(&path)))
    })();
    // finally: if os.path.exists(tmp): os.unlink(tmp)
    if tmp.exists() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

/// `subprocess.run(argv, capture_output=True, text=True, timeout=...)`:
/// (returncode, stdout, stderr), decoded strictly as UTF-8 with universal
/// newlines. Err with the message of what Python would raise: the OSError
/// of a failed exec, TimeoutExpired, or UnicodeDecodeError.
fn run_text(argv: &[&str], timeout: u64) -> Result<(Option<i32>, String, String), String> {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let mut child = Command::new(argv[0])
        .args(&argv[1..])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| os_error(&e, Some(Path::new(argv[0])), None))?;
    let mut out_pipe = child.stdout.take();
    let mut err_pipe = child.stderr.take();
    let out_t = std::thread::spawn(move || {
        let mut s = Vec::new();
        if let Some(p) = out_pipe.as_mut() {
            let _ = p.read_to_end(&mut s);
        }
        s
    });
    let err_t = std::thread::spawn(move || {
        let mut s = Vec::new();
        if let Some(p) = err_pipe.as_mut() {
            let _ = p.read_to_end(&mut s);
        }
        s
    });
    // communicate(timeout=...): read both pipes to EOF and reap the child,
    // all within the one deadline.
    let start = Instant::now();
    let mut status = None;
    let mut reaped = false;
    loop {
        if !reaped {
            match child.try_wait() {
                Ok(Some(st)) => (status, reaped) = (Some(st), true),
                Ok(None) => {}
                Err(_) => reaped = true,
            }
        }
        if reaped && out_t.is_finished() && err_t.is_finished() {
            break;
        }
        if start.elapsed() >= Duration::from_secs(timeout) {
            // run(): process.kill(); process.wait(); raise -- TimeoutExpired,
            // without decoding what was read and without waiting for the
            // pipes, which a grandchild may still hold open. The reader
            // threads end on their own when it lets go.
            let _ = child.kill();
            let _ = child.wait();
            let list: Vec<String> = argv.iter().map(|a| py_str_repr(a)).collect();
            return Err(format!("Command '[{}]' timed out after {timeout} seconds", list.join(", ")));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // Text mode decodes stdout, then stderr.
    let stdout = universal_newlines(&decode_utf8(out_t.join().unwrap_or_default())?);
    let stderr = universal_newlines(&decode_utf8(err_t.join().unwrap_or_default())?);
    // A signal is a negative returncode in Python: not 0 either way.
    Ok((status.and_then(|s| s.code()), stdout, stderr))
}

/// `hyprctl *args`, stripped stdout; Err with its message when it fails.
pub fn _hyprctl(args: &[&str]) -> Result<String, String> {
    let mut argv = vec!["hyprctl"];
    argv.extend_from_slice(args);
    let (code, stdout, stderr) = run_text(&argv, 10)?;
    if code != Some(0) {
        let msg = if !stderr.is_empty() { &stderr } else { &stdout };
        let msg = py_strip(msg);
        return Err(if msg.is_empty() { "hyprctl failed".into() } else { msg.to_string() });
    }
    Ok(py_strip(&stdout).to_string())
}

/// Bind (or with None, unbind) a plugin's shortcut. Returns the result.
///
/// Reloads Hyprland and checks for config errors; on any, the previous file
/// is restored and Hyprland reloaded again, and Err (ValueError) says why.
pub fn set_shortcut(pid: &str, combo: Option<&str>) -> Result<String, String> {
    set_shortcut_with(pid, combo, &mut |args| _hyprctl(args))
}

/// `json.loads(text)` iterated the way `conflict`'s `for b in binds` would:
/// a list gives its items, a dict its keys, a str its characters; anything
/// else fails as the Python does (TypeError).
fn binds_of(text: &str) -> Result<Vec<PyVal>, String> {
    let mut v = py_json_loads(text)?;
    match &mut v {
        PyVal::List(a) => Ok(std::mem::take(a)),
        PyVal::Dict(d) => Ok(d.drain(..).map(|(k, _)| k).collect()),
        PyVal::Str(s) => Ok(s.chars().map(|c| PyVal::Str(c.to_string())).collect()),
        PyVal::SurStr(u) => Ok(u.iter().map(|c| str_from_units(vec![*c])).collect()),
        other => Err(format!("'{}' object is not iterable", other.type_name())),
    }
}

/// `set_shortcut` with `hyprctl` supplied by the caller -- the seam the
/// Python tests reach with `mock.patch.object(shortcuts, "_hyprctl")`.
pub fn set_shortcut_with(
    pid: &str,
    combo: Option<&str>,
    hyprctl: &mut dyn FnMut(&[&str]) -> Result<String, String>,
) -> Result<String, String> {
    if !id_match(pid) || pid.contains("..") {
        return Err("Not a plugin id.".into());
    }
    let (raw, mode) = _read()?;
    let old = current(pid, Some(&raw))?;
    let stripped = strip_blocks(&raw, pid);
    let new;
    let combo = match combo {
        None => {
            if old.is_empty() {
                return Ok("No shortcut was set.".into());
            }
            new = stripped;
            None
        }
        Some(c) => {
            let combo = normalise(c)?.0;
            let why = conflict_py(&binds_of(&hyprctl(&["-j", "binds"])?)?, &combo, pid)?;
            if !why.is_empty() {
                return Err(why);
            }
            new = format!("{}\n{}", stripped.trim_end_matches('\n'), _block(pid, &combo));
            Some(combo)
        }
    };
    if !hyprctl(&["configerrors"])?.is_empty() {
        return Err("Hyprland already reports config errors; fix those first.".into());
    }
    _write(&new, mode)?;
    hyprctl(&["reload"])?;
    let errors = hyprctl(&["configerrors"])?;
    if !errors.is_empty() {
        _write(&raw, mode)?;
        hyprctl(&["reload"])?;
        return Err(format!(
            "Hyprland rejected it, so bindings.lua was put back: {}",
            py_splitlines(&errors).first().copied().unwrap_or_default()
        ));
    }
    Ok(match combo {
        Some(c) => format!("{c} opens {pid}"),
        None => format!("Shortcut for {pid} removed"),
    })
}

/// Only a plugin with something to open can be given a shortcut to it.
///
/// The UI asks this as a yes/no; a manifest that is JSON but not an object,
/// or whose `entryPoints` is truthy but not one, makes the Python raise
/// AttributeError to its caller -- `try_has_panel` reports that, and here it
/// reads as no panel.
pub fn has_panel(plugin_dir: &Path) -> bool {
    try_has_panel(plugin_dir).unwrap_or(false)
}

/// `has_panel` exactly as the Python behaves: Err (AttributeError) for a
/// manifest that is not an object, or whose truthy `entryPoints` is not one.
pub fn try_has_panel(plugin_dir: &Path) -> Result<bool, String> {
    let Ok(bytes) = std::fs::read(plugin_dir.join("manifest.json")) else {
        return Ok(false); // OSError
    };
    // read_text(): strict UTF-8, universal newlines; json.loads: Python's
    // grammar (NaN, Infinity, ints of any size). Any ValueError is no panel.
    let Ok(text) = decode_utf8(bytes) else {
        return Ok(false);
    };
    // A RecursionError (nesting too deep) is neither, and propagates.
    let m = match py_json_loads(&universal_newlines(&text)) {
        Ok(m) => m,
        Err(e) if e.recursion => return Err(e.msg),
        Err(_) => return Ok(false),
    };
    let no_get = |v: &PyVal| format!("'{}' object has no attribute 'get'", v.type_name());
    if !matches!(m, PyVal::Dict(_)) {
        return Err(no_get(&m));
    }
    let empty = PyVal::Dict(Vec::new());
    let points = match m.get("entryPoints") {
        Some(p) if p.truthy() => p,
        _ => &empty,
    };
    if !matches!(points, PyVal::Dict(_)) {
        return Err(no_get(points));
    }
    Ok(["panel", "barWidget", "overlay"].iter().any(|k| points.get(k).is_some_and(PyVal::truthy)))
}

/// Parity hook: see ARCHITECTURE.md.
///
/// `args.env` (optional) is `{VAR: value | null}` applied for the duration of
/// the call. `set_shortcut` takes `fake`, a scripted `hyprctl` identical to
/// the Python case's (`binds` text, `before`/`after` configerrors, `fail`:
/// the first argument that makes it fail); with `report` the result is
/// `{"result", "error", "file", "calls"}` instead of the bare return.
pub fn parity(func: &str, args: &Value) -> Option<Value> {
    let saved: Vec<(String, Option<std::ffi::OsString>)> = args
        .get("env")
        .and_then(|e| e.as_object())
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
    let res = |r: Result<Value, String>| match r {
        Ok(v) => v,
        Err(e) => serde_json::json!({ "error": e }),
    };
    let s = |k: &str| util::arg_str(args, k);
    let out = match func {
        "bindings_path" => Some(Value::String(bindings_path().to_string_lossy().into_owned())),
        "available" => Some(Value::Bool(available())),
        "normalise" => Some(res(normalise(s("value")).map(|(a, b, c)| serde_json::json!([a, b, c])))),
        "_block" => Some(Value::String(_block(s("pid"), s("combo")))),
        "current" => {
            let raw = args.get("raw").and_then(|v| v.as_str());
            Some(res(current(s("pid"), raw).map(Value::String)))
        }
        "conflict" => {
            let binds = args.get("binds").and_then(|b| b.as_array()).cloned().unwrap_or_default();
            Some(res(conflict(&binds, s("combo"), s("pid")).map(Value::String)))
        }
        "_read" => Some(res(_read().map(|(t, m)| serde_json::json!([t, m])))),
        "_hyprctl" => {
            let a = util::arg_strs(args, "args");
            let a: Vec<&str> = a.iter().map(String::as_str).collect();
            Some(res(_hyprctl(&a).map(Value::String)))
        }
        "has_panel" => Some(res(try_has_panel(Path::new(s("dir"))).map(Value::Bool))),
        "set_shortcut" => {
            let fake = args.get("fake").cloned().unwrap_or_default();
            let fs = |k: &str| fake.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let (binds, before, after, fail) = (fs("binds"), fs("before"), fs("after"), fs("fail"));
            let mut reloaded = false;
            let mut calls: Vec<Vec<String>> = Vec::new();
            let mut run = |a: &[&str]| -> Result<String, String> {
                calls.push(a.iter().map(|x| x.to_string()).collect());
                if !fail.is_empty() && a.first() == Some(&fail.as_str()) {
                    return Err("fake failure".into());
                }
                if a.len() >= 2 && a[0] == "-j" && a[1] == "binds" {
                    return Ok(binds.clone());
                }
                if a == ["reload"] {
                    reloaded = true;
                    return Ok("ok".into());
                }
                if a == ["configerrors"] {
                    return Ok(if reloaded { after.clone() } else { before.clone() });
                }
                Ok(String::new())
            };
            let combo = args.get("combo").and_then(|v| v.as_str());
            let r = set_shortcut_with(s("pid"), combo, &mut run);
            if util::arg_bool(args, "report") {
                let file = std::fs::read_to_string(bindings_path()).ok().map(|t| universal_newlines(&t));
                let (result, error) = match r {
                    Ok(v) => (Some(v), None),
                    Err(e) => (None, Some(e)),
                };
                Some(serde_json::json!({"result": result, "error": error, "file": file, "calls": calls}))
            } else {
                Some(res(r.map(Value::String)))
            }
        }
        _ => None,
    };
    // `mask`: [[regex, text], ...] replaced in every string of the result, so
    // a case can compare a message whose fd number or random temp name
    // differs between processes (the case applies the same to Python's).
    let masks: Vec<(Regex, String)> = args
        .get("mask")
        .and_then(|m| m.as_array())
        .map(|m| {
            m.iter()
                .filter_map(|p| Some((Regex::new(p.get(0)?.as_str()?).ok()?, p.get(1)?.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default();
    fn mask(v: &mut Value, masks: &[(Regex, String)]) {
        match v {
            Value::String(s) => {
                for (re, to) in masks {
                    *s = re.replace_all(s, regex::NoExpand(to)).into_owned();
                }
            }
            Value::Array(a) => a.iter_mut().for_each(|x| mask(x, masks)),
            Value::Object(o) => o.values_mut().for_each(|x| mask(x, masks)),
            _ => {}
        }
    }
    let mut out = out;
    if let Some(v) = out.as_mut() {
        mask(v, &masks);
    }
    for (k, old) in saved {
        match old {
            Some(v) => std::env::set_var(&k, v),
            None => std::env::remove_var(&k),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGINAL: &str = "-- mine\no.bind(\"SUPER + SPACE\", \"Omarchy menu\", \"x\")\n";

    /// PluginShortcuts.setUp: a temp XDG_CONFIG_HOME holding hypr/bindings.lua.
    struct Fixture {
        _tmp: tempfile::TempDir,
        path: PathBuf,
        saved: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl Fixture {
        fn new() -> Fixture {
            static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
            let lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let tmp = tempfile::tempdir().unwrap();
            let hypr = tmp.path().join("hypr");
            std::fs::create_dir(&hypr).unwrap();
            let path = hypr.join("bindings.lua");
            std::fs::write(&path, ORIGINAL).unwrap();
            let saved = std::env::var_os("XDG_CONFIG_HOME");
            std::env::set_var("XDG_CONFIG_HOME", tmp.path());
            Fixture { _tmp: tmp, path, saved, _lock: lock }
        }

        fn read(&self) -> String {
            std::fs::read_to_string(&self.path).unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            match self.saved.take() {
                Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
        }
    }

    fn binds() -> String {
        serde_json::json!([{"modmask": 64, "key": "SPACE", "description": "Omarchy menu",
                            "keycode": 0, "mouse": false, "catch_all": false, "submap": ""}])
        .to_string()
    }

    /// PluginShortcuts.hyprctl: the scripted `_hyprctl`.
    fn hyprctl(errors_after_reload: &'static str) -> impl FnMut(&[&str]) -> Result<String, String> {
        let mut reloaded = false;
        move |args: &[&str]| {
            if args.len() >= 2 && args[..2] == ["-j", "binds"] {
                return Ok(binds());
            }
            if args == ["reload"] {
                reloaded = true;
                return Ok("ok".into());
            }
            if args == ["configerrors"] {
                return Ok(if reloaded { errors_after_reload.into() } else { String::new() });
            }
            Ok(String::new())
        }
    }

    #[test]
    fn test_combinations_are_normalised() {
        assert_eq!(normalise("alt+super+v").unwrap().0, "SUPER + ALT + V");
        assert!(normalise("v").is_err()); // no modifier
    }

    #[test]
    fn test_a_shortcut_is_written_in_its_own_block() {
        let f = Fixture::new();
        set_shortcut_with("lxp.vitals", Some("super+alt+v"), &mut hyprctl("")).unwrap();
        let raw = f.read();
        assert!(raw.starts_with(ORIGINAL.trim_end_matches('\n')));
        assert_eq!(current("lxp.vitals", None).unwrap(), "SUPER + ALT + V");
        assert!(raw.contains("omarchy-shell shell toggle lxp.vitals"));
        set_shortcut_with("lxp.vitals", None, &mut hyprctl("")).unwrap();
        assert_eq!(f.read().trim(), ORIGINAL.trim());
    }

    #[test]
    fn test_a_taken_combination_is_refused_and_nothing_written() {
        let f = Fixture::new();
        let e = set_shortcut_with("lxp.vitals", Some("super+space"), &mut hyprctl("")).unwrap_err();
        assert!(e.contains("Omarchy menu"));
        assert_eq!(f.read(), ORIGINAL);
    }

    #[test]
    fn test_a_rejected_change_is_rolled_back() {
        let f = Fixture::new();
        assert!(set_shortcut_with("lxp.vitals", Some("super+alt+v"), &mut hyprctl("line 3: bad bind")).is_err());
        assert_eq!(f.read(), ORIGINAL);
    }

    #[test]
    fn test_a_hand_edited_block_is_not_overwritten() {
        let f = Fixture::new();
        set_shortcut_with("lxp.vitals", Some("super+alt+v"), &mut hyprctl("")).unwrap();
        std::fs::write(&f.path, f.read().replace("toggle", "summon")).unwrap();
        assert!(current("lxp.vitals", None).is_err());
    }
}
