//! Port of `icons.py`.
//!
//! Vendor icons, fetched from the vendors themselves.
//!
//! Every icon here comes from the domain it represents -- stripe.com's favicon is
//! fetched from stripe.com -- rather than from a favicon proxy. A proxy would be
//! one request containing the full list of every service this machine talks to,
//! sent to a company with no other involvement in it. These are hosts the user is
//! already connected to.
//!
//! Nothing is fetched twice: a hit is cached as a file, and a miss is remembered
//! too, so a domain with no usable icon is not retried on every redraw.
//!
//! Two pieces of Python's standard library are ported here with it, because
//! what this module does depends on their exact behaviour: `json` (the misses
//! file: big ints, NaN, lone surrogates, nesting, which errors are caught) and
//! `urllib.request.urlopen` (headers sent, per-read timeouts, redirects,
//! proxies, short bodies, `file:` and `data:` URLs).

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::Value;

// ---------------------------------------------------------------- paths

/// `Path(raw)` as pathlib spells it: repeated slashes collapsed, `.` parts
/// dropped, no trailing slash; exactly two leading slashes are kept, as
/// POSIX allows.
fn py_path(raw: &[u8]) -> PathBuf {
    let (root, rel): (&[u8], &[u8]) = if raw.first() != Some(&b'/') {
        (b"", raw)
    } else if raw.get(1) != Some(&b'/') || raw.get(2) == Some(&b'/') {
        (b"/", &raw[1..])
    } else {
        (b"//", &raw[2..])
    };
    let mut out = root.to_vec();
    let mut first = true;
    for part in rel.split(|&b| b == b'/').filter(|p| !p.is_empty() && *p != b".") {
        if !first {
            out.push(b'/');
        }
        out.extend_from_slice(part);
        first = false;
    }
    if out.is_empty() {
        out.push(b'.');
    }
    PathBuf::from(OsString::from_vec(out))
}

/// `Path(base) / name`.
fn py_join(base: &Path, name: &str) -> PathBuf {
    let mut raw = base.as_os_str().as_bytes().to_vec();
    if name.starts_with('/') {
        raw.clear();
    } else if !raw.is_empty() && !raw.ends_with(b"/") {
        raw.push(b'/');
    }
    raw.extend_from_slice(name.as_bytes());
    py_path(&raw)
}

/// `Path.home()`: `$HOME` without trailing slashes ("/" when that leaves
/// nothing), else the password database.
fn py_home() -> PathBuf {
    let mut h = match std::env::var_os("HOME") {
        Some(h) => h.into_vec(),
        None => passwd_home().unwrap_or_else(|| b"/".to_vec()),
    };
    while h.ends_with(b"/") {
        h.pop();
    }
    if h.is_empty() {
        h.push(b'/');
    }
    py_path(&h)
}

fn passwd_home() -> Option<Vec<u8>> {
    let uid = rustix::process::getuid().as_raw().to_string();
    let text = std::fs::read("/etc/passwd").ok()?;
    text.split(|&b| b == b'\n').find_map(|line| {
        let f: Vec<&[u8]> = line.split(|&b| b == b':').collect();
        (f.len() >= 7 && f[2] == uid.as_bytes()).then(|| f[5].to_vec())
    })
}

/// `CACHE`: `$XDG_CACHE_HOME/almanac`, or `~/.cache/almanac`. Read at call
/// time (Rust has no import-time evaluation), spelled as pathlib spells it.
pub fn cache() -> PathBuf {
    let base = match std::env::var_os("XDG_CACHE_HOME") {
        Some(v) => py_path(v.as_bytes()),
        None => py_join(&py_home(), ".cache"),
    };
    py_join(&base, "almanac")
}

/// `ICONS`: downloaded icons, one file per key.
pub fn icons() -> PathBuf {
    py_join(&cache(), "icons")
}

/// `MISSES`: keys with no usable icon, and when that was found out.
pub fn misses() -> PathBuf {
    py_join(&cache(), "icon-misses.json")
}

pub const MISS_TTL: f64 = 7.0 * 86400.0;
/// `UA`: the User-Agent header sent with every fetch.
pub const UA: &str = "Mozilla/5.0 (X11; Linux x86_64) almanac";

// Tried in order. apple-touch-icon is last because it is the largest and the
// least often present.
pub const PATHS: &[&str] = &[
    "/favicon.ico",
    "/favicon.png",
    "/favicon.svg",
    "/apple-touch-icon.png",
    "/apple-touch-icon-precomposed.png",
];

/// `_EXT`: file signatures and the extension each one is saved under.
/// Checked in this order.
pub const EXT: &[(&[u8], &str)] = &[
    (b"\x89PNG", ".png"),
    (b"\x00\x00\x01\x00", ".ico"),
    (b"GIF8", ".gif"),
    (b"\xff\xd8\xff", ".jpg"),
    (b"<svg", ".svg"),
    (b"<?xm", ".svg"),
    (b"RIFF", ".webp"),
];

fn _kind(data: &[u8]) -> &'static str {
    let head = &data[..data.len().min(4)];
    for (sig, ext) in EXT {
        if head.starts_with(sig) {
            return ext;
        }
    }
    // `bytes.lstrip()` strips ASCII whitespace including \x0b, which
    // `u8::is_ascii_whitespace` leaves alone.
    let first = &data[..data.len().min(200)];
    let start = first
        .iter()
        .position(|b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'\x0b' | b'\x0c'))
        .unwrap_or(first.len());
    if first[start..].starts_with(b"<svg") {
        return ".svg";
    }
    ""
}

/// mcp.stripe.com -> stripe.com. Good enough for finding a brand icon.
pub fn registrable(host: &str) -> String {
    let parts: Vec<&str> = host.split('.').filter(|p| !p.is_empty()).collect();
    if parts.len() <= 2 {
        return parts.join(".");
    }
    // Two-part public suffixes that would otherwise be cut to nothing.
    let n = parts.len();
    if ["co", "com", "org", "net", "ac", "gov"].contains(&parts[n - 2]) {
        return parts[n - 3..].join(".");
    }
    parts[n - 2..].join(".")
}

pub fn host_of(url: &str) -> String {
    let re = regex::Regex::new(r"^[a-z]+://([^/:]+)").unwrap();
    re.captures(url)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .unwrap_or_default()
}

// ---------------------------------------------------------------- json

// The misses file is read and written the way Python's json does it, which
// serde does not: Python keeps ints of any size (and refuses more than 4300
// digits), reads NaN / Infinity, keeps lone surrogate escapes, rejects keys
// that are not strings, and nests about 52,000 deep before a RecursionError
// -- which, unlike a ValueError, `_load_misses` does not catch. Values live
// in an arena (containers hold indexes), so no depth needs recursion here.

#[derive(Debug, Clone)]
enum Node {
    Null,
    Bool(bool),
    /// The decimal digits, as `str(int)` writes them.
    Int(String),
    Float(f64),
    /// UTF-16 units: a Python str may hold lone surrogates.
    Str(Vec<u16>),
    List(Vec<usize>),
    Dict(Vec<(Vec<u16>, usize)>),
}

#[derive(Debug, Clone)]
struct Doc {
    nodes: Vec<Node>,
    root: usize,
}

impl Doc {
    fn empty_dict() -> Doc {
        Doc { nodes: vec![Node::Dict(Vec::new())], root: 0 }
    }
}

/// Why `json.loads` / `json.dumps` failed: a ValueError, or a RecursionError.
#[derive(Debug, PartialEq)]
enum JsonErr {
    Value,
    Recursion,
}

// Python 3.14 bounds recursion by the C stack (8 MB here), so these limits
// were measured: `json.loads` opens about 52,105 nested containers (a dict
// counts as one), and `json.dumps` about 52,105 lists, a dict costing 1.8
// lists. Neither is exact: from one run to the next they land anywhere from
// about 52,085 to 52,130 (stack placement and the caller's own depth), so
// these sit in the middle of what was measured.
const LOADS_DEPTH: usize = 52_105;
const DUMPS_BUDGET: f64 = 52_105.0;
const DUMPS_DICT_COST: f64 = 1.8;

/// `sys.get_int_max_str_digits()`: 4300 unless PYTHONINTMAXSTRDIGITS says.
fn int_max_str_digits() -> usize {
    match std::env::var("PYTHONINTMAXSTRDIGITS").ok().and_then(|v| v.trim().parse::<usize>().ok()) {
        Some(0) => usize::MAX,
        Some(n) if n >= 640 => n,
        _ => 4300,
    }
}

fn json_ws(b: &[u8], mut i: usize) -> usize {
    while matches!(b.get(i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
        i += 1;
    }
    i
}

fn hex4(b: &[u8], i: usize) -> Result<u16, JsonErr> {
    // Python wants one more character after the four digits.
    if i + 4 >= b.len() {
        return Err(JsonErr::Value);
    }
    let s = std::str::from_utf8(&b[i..i + 4]).map_err(|_| JsonErr::Value)?;
    if !s.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(JsonErr::Value);
    }
    u16::from_str_radix(s, 16).map_err(|_| JsonErr::Value)
}

/// `scanstring` (strict): from just after the opening quote to just after
/// the closing one.
fn scanstring(b: &[u8], mut i: usize) -> Result<(Vec<u16>, usize), JsonErr> {
    let mut out = Vec::new();
    loop {
        let c = *b.get(i).ok_or(JsonErr::Value)?;
        match c {
            b'"' => return Ok((out, i + 1)),
            b'\\' => {
                let e = *b.get(i + 1).ok_or(JsonErr::Value)?;
                i += 2;
                let unit = match e {
                    b'"' => 0x22,
                    b'\\' => 0x5c,
                    b'/' => 0x2f,
                    b'b' => 0x08,
                    b'f' => 0x0c,
                    b'n' => 0x0a,
                    b'r' => 0x0d,
                    b't' => 0x09,
                    b'u' => {
                        let u = hex4(b, i)?;
                        i += 4;
                        // A high surrogate escape joins a low one right after it.
                        if (0xd800..0xdc00).contains(&u) && i + 6 < b.len() && b[i] == b'\\' && b[i + 1] == b'u' {
                            let u2 = hex4(b, i + 2)?;
                            if (0xdc00..0xe000).contains(&u2) {
                                out.push(u);
                                out.push(u2);
                                i += 6;
                                continue;
                            }
                        }
                        u
                    }
                    _ => return Err(JsonErr::Value),
                };
                out.push(unit);
            }
            0..=0x1f => return Err(JsonErr::Value),
            _ => {
                let len = match c {
                    0xf0.. => 4,
                    0xe0.. => 3,
                    0xc0.. => 2,
                    _ => 1,
                };
                let ch = b
                    .get(i..i + len)
                    .and_then(|s| std::str::from_utf8(s).ok())
                    .and_then(|s| s.chars().next())
                    .ok_or(JsonErr::Value)?;
                let mut buf = [0u16; 2];
                out.extend_from_slice(ch.encode_utf16(&mut buf));
                i += len;
            }
        }
    }
}

/// A key and its colon: returns where the value starts.
fn scan_key(b: &[u8], i: usize) -> Result<(Vec<u16>, usize), JsonErr> {
    if b.get(i) != Some(&b'"') {
        return Err(JsonErr::Value);
    }
    let (key, i) = scanstring(b, i + 1)?;
    let i = json_ws(b, i);
    if b.get(i) != Some(&b':') {
        return Err(JsonErr::Value);
    }
    Ok((key, json_ws(b, i + 1)))
}

/// A constant or a number at `i`.
fn scan_scalar(b: &[u8], i: usize, digits_limit: usize) -> Result<(Node, usize), JsonErr> {
    let rest = &b[i..];
    let consts: [(&str, Node); 6] = [
        ("null", Node::Null),
        ("true", Node::Bool(true)),
        ("false", Node::Bool(false)),
        ("NaN", Node::Float(f64::NAN)),
        ("Infinity", Node::Float(f64::INFINITY)),
        ("-Infinity", Node::Float(f64::NEG_INFINITY)),
    ];
    for (lit, node) in consts {
        if rest.starts_with(lit.as_bytes()) {
            return Ok((node, i + lit.len()));
        }
    }
    let digit = |j: usize| b.get(j).is_some_and(u8::is_ascii_digit);
    let mut j = i;
    let neg = b.get(j) == Some(&b'-');
    if neg {
        j += 1;
    }
    match b.get(j) {
        Some(b'1'..=b'9') => {
            j += 1;
            while digit(j) {
                j += 1;
            }
        }
        Some(b'0') => j += 1,
        _ => return Err(JsonErr::Value),
    }
    let int_end = j;
    let mut is_float = false;
    if b.get(j) == Some(&b'.') && digit(j + 1) {
        is_float = true;
        j += 2;
        while digit(j) {
            j += 1;
        }
    }
    if matches!(b.get(j), Some(b'e' | b'E')) {
        let mut k = j + 1;
        if matches!(b.get(k), Some(b'+' | b'-')) {
            k += 1;
        }
        let start = k;
        while digit(k) {
            k += 1;
        }
        if k > start {
            is_float = true;
            j = k;
        }
    }
    let lit = std::str::from_utf8(&b[i..j]).map_err(|_| JsonErr::Value)?;
    if is_float {
        return Ok((Node::Float(lit.parse::<f64>().map_err(|_| JsonErr::Value)?), j));
    }
    // int(): more digits than the limit is a ValueError.
    if int_end - i - usize::from(neg) > digits_limit {
        return Err(JsonErr::Value);
    }
    Ok((Node::Int(if lit == "-0" { "0".to_string() } else { lit.to_string() }), j))
}

/// `json.loads(s)`.
fn py_loads(s: &str) -> Result<Doc, JsonErr> {
    enum Frame {
        List(usize),
        Dict(usize, Vec<u16>, HashMap<Vec<u16>, usize>),
    }
    let b = s.as_bytes();
    let limit = int_max_str_digits();
    let mut nodes: Vec<Node> = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();
    let mut i = json_ws(b, 0);
    'value: loop {
        let mut v = match b.get(i) {
            Some(&open @ (b'[' | b'{')) => {
                if stack.len() >= LOADS_DEPTH {
                    return Err(JsonErr::Recursion);
                }
                i = json_ws(b, i + 1);
                if open == b'[' {
                    nodes.push(Node::List(Vec::new()));
                    let id = nodes.len() - 1;
                    if b.get(i) == Some(&b']') {
                        i += 1;
                        id
                    } else {
                        stack.push(Frame::List(id));
                        continue 'value;
                    }
                } else {
                    nodes.push(Node::Dict(Vec::new()));
                    let id = nodes.len() - 1;
                    if b.get(i) == Some(&b'}') {
                        i += 1;
                        id
                    } else {
                        let (key, ni) = scan_key(b, i)?;
                        i = ni;
                        stack.push(Frame::Dict(id, key, HashMap::new()));
                        continue 'value;
                    }
                }
            }
            Some(b'"') => {
                let (st, ni) = scanstring(b, i + 1)?;
                i = ni;
                nodes.push(Node::Str(st));
                nodes.len() - 1
            }
            Some(_) => {
                let (node, ni) = scan_scalar(b, i, limit)?;
                i = ni;
                nodes.push(node);
                nodes.len() - 1
            }
            None => return Err(JsonErr::Value),
        };
        // Hand the value to its container; a closing bracket hands that one on.
        loop {
            match stack.last_mut() {
                None => {
                    if json_ws(b, i) != b.len() {
                        return Err(JsonErr::Value);
                    }
                    return Ok(Doc { nodes, root: v });
                }
                Some(Frame::List(id)) => {
                    let id = *id;
                    if let Node::List(items) = &mut nodes[id] {
                        items.push(v);
                    }
                    i = json_ws(b, i);
                    match b.get(i) {
                        Some(b',') => {
                            i = json_ws(b, i + 1);
                            continue 'value;
                        }
                        Some(b']') => {
                            i += 1;
                            stack.pop();
                            v = id;
                        }
                        _ => return Err(JsonErr::Value),
                    }
                }
                Some(Frame::Dict(id, key, seen)) => {
                    let id = *id;
                    let key = std::mem::take(key);
                    // A repeated key keeps its first place and takes the last value.
                    if let Node::Dict(items) = &mut nodes[id] {
                        match seen.get(&key) {
                            Some(&pos) => items[pos].1 = v,
                            None => {
                                seen.insert(key.clone(), items.len());
                                items.push((key, v));
                            }
                        }
                    }
                    i = json_ws(b, i);
                    match b.get(i) {
                        Some(b',') => {
                            let (k, ni) = scan_key(b, json_ws(b, i + 1))?;
                            i = ni;
                            if let Some(Frame::Dict(_, slot, _)) = stack.last_mut() {
                                *slot = k;
                            }
                            continue 'value;
                        }
                        Some(b'}') => {
                            i += 1;
                            stack.pop();
                            v = id;
                        }
                        _ => return Err(JsonErr::Value),
                    }
                }
            }
        }
    }
}

/// A str as `json.dumps` writes it (`ensure_ascii=True`).
fn dump_str(out: &mut String, s: &[u16]) {
    out.push('"');
    for &u in s {
        match u {
            0x22 => out.push_str("\\\""),
            0x5c => out.push_str("\\\\"),
            0x0a => out.push_str("\\n"),
            0x0d => out.push_str("\\r"),
            0x09 => out.push_str("\\t"),
            0x08 => out.push_str("\\b"),
            0x0c => out.push_str("\\f"),
            0x20..=0x7e => out.push(u as u8 as char),
            _ => out.push_str(&format!("\\u{u:04x}")),
        }
    }
    out.push('"');
}

/// `json.dumps(doc)`.
fn py_dumps(doc: &Doc) -> Result<String, JsonErr> {
    // How deep the encoder's recursion goes.
    let mut worst = 0.0f64;
    let mut todo = vec![(doc.root, 0.0f64)];
    while let Some((id, cost)) = todo.pop() {
        match &doc.nodes[id] {
            Node::List(items) => {
                let c = cost + 1.0;
                worst = worst.max(c);
                todo.extend(items.iter().map(|&x| (x, c)));
            }
            Node::Dict(items) => {
                let c = cost + DUMPS_DICT_COST;
                worst = worst.max(c);
                todo.extend(items.iter().map(|(_, x)| (*x, c)));
            }
            _ => {}
        }
    }
    if worst > DUMPS_BUDGET {
        return Err(JsonErr::Recursion);
    }
    enum W<'a> {
        V(usize),
        S(&'static str),
        K(&'a [u16]),
    }
    let mut out = String::new();
    let mut work = vec![W::V(doc.root)];
    while let Some(w) = work.pop() {
        match w {
            W::S(s) => out.push_str(s),
            W::K(k) => {
                dump_str(&mut out, k);
                out.push_str(": ");
            }
            W::V(id) => match &doc.nodes[id] {
                Node::Null => out.push_str("null"),
                Node::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
                Node::Int(s) => out.push_str(s),
                Node::Float(f) => out.push_str(&crate::util::py_float_repr(*f)),
                Node::Str(s) => dump_str(&mut out, s),
                Node::List(items) if items.is_empty() => out.push_str("[]"),
                Node::Dict(items) if items.is_empty() => out.push_str("{}"),
                Node::List(items) => {
                    out.push('[');
                    work.push(W::S("]"));
                    for (n, &x) in items.iter().enumerate().rev() {
                        work.push(W::V(x));
                        if n > 0 {
                            work.push(W::S(", "));
                        }
                    }
                }
                Node::Dict(items) => {
                    out.push('{');
                    work.push(W::S("}"));
                    for (n, (k, x)) in items.iter().enumerate().rev() {
                        work.push(W::V(*x));
                        work.push(W::K(k));
                        if n > 0 {
                            work.push(W::S(", "));
                        }
                    }
                }
            },
        }
    }
    Ok(out)
}

/// Python's `type(x).__name__`, for the error messages.
fn py_type(n: &Node) -> &'static str {
    match n {
        Node::Null => "NoneType",
        Node::Bool(_) => "bool",
        Node::Int(_) => "int",
        Node::Float(_) => "float",
        Node::Str(_) => "str",
        Node::List(_) => "list",
        Node::Dict(_) => "dict",
    }
}

/// A miss stamp as `time.time() - stamp` sees it, or the exception it raises.
fn as_number(n: &Node) -> Result<f64, String> {
    match n {
        // `float - int` converts the int exactly, then rounds; past the
        // float range that is an OverflowError.
        Node::Int(s) => match s.parse::<f64>() {
            Ok(f) if f.is_finite() => Ok(f),
            _ => Err("OverflowError: int too large to convert to float".into()),
        },
        Node::Float(f) => Ok(*f),
        Node::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        other => Err(format!("TypeError: unsupported operand type(s) for -: 'float' and '{}'", py_type(other))),
    }
}

/// The parsed file as plain JSON (for the parity output). NaN and the
/// infinities, which JSON cannot hold, become null; lone surrogates U+FFFD.
fn to_value(doc: &Doc, id: usize) -> Value {
    match &doc.nodes[id] {
        Node::Null => Value::Null,
        Node::Bool(b) => Value::Bool(*b),
        Node::Int(s) => s
            .parse::<i64>()
            .map(Value::from)
            .or_else(|_| s.parse::<u64>().map(Value::from))
            .unwrap_or_else(|_| serde_json::Number::from_f64(s.parse::<f64>().unwrap_or(0.0)).map_or(Value::Null, Value::Number)),
        Node::Float(f) => serde_json::Number::from_f64(*f).map_or(Value::Null, Value::Number),
        Node::Str(s) => Value::String(String::from_utf16_lossy(s)),
        Node::List(items) => Value::Array(items.iter().map(|&x| to_value(doc, x)).collect()),
        Node::Dict(items) => {
            Value::Object(items.iter().map(|(k, x)| (String::from_utf16_lossy(k), to_value(doc, *x))).collect())
        }
    }
}

const RECURSION_LOADS: &str = "RecursionError: maximum recursion depth exceeded while decoding a JSON object from a unicode string";
const RECURSION_DUMPS: &str = "RecursionError: maximum recursion depth exceeded while encoding a JSON object";

/// `json.loads(MISSES.read_text())`, or `{}` on `(OSError, ValueError)`.
/// Like the Python, the result need not be a dict. A RecursionError is not
/// caught: it is the `Err`.
fn _load_misses() -> Result<Doc, String> {
    let Ok(text) = std::fs::read_to_string(misses()) else {
        return Ok(Doc::empty_dict());
    };
    match py_loads(&text) {
        Ok(doc) => Ok(doc),
        Err(JsonErr::Value) => Ok(Doc::empty_dict()),
        Err(JsonErr::Recursion) => Err(RECURSION_LOADS.into()),
    }
}

fn _save_miss(key: &str) -> Result<(), String> {
    let mut doc = _load_misses()?;
    let root = doc.root;
    match &doc.nodes[root] {
        Node::Dict(_) => {}
        // misses[key] = ... on a list: the index is checked before anything.
        Node::List(_) => return Err("TypeError: list indices must be integers or slices, not str".into()),
        n => return Err(format!("TypeError: '{}' object does not support item assignment", py_type(n))),
    }
    doc.nodes.push(Node::Float(crate::util::now()));
    let stamp = doc.nodes.len() - 1;
    let key16: Vec<u16> = key.encode_utf16().collect();
    if let Node::Dict(items) = &mut doc.nodes[root] {
        match items.iter_mut().find(|(k, _)| *k == key16) {
            Some(entry) => entry.1 = stamp,
            None => items.push((key16, stamp)),
        }
    }
    // except OSError: pass -- but json.dumps runs inside the try, and its
    // RecursionError is not an OSError.
    if std::fs::create_dir_all(cache()).is_err() {
        return Ok(());
    }
    let text = py_dumps(&doc).map_err(|_| RECURSION_DUMPS.to_string())?;
    let _ = std::fs::write(misses(), text);
    Ok(())
}

// ---------------------------------------------------------------- cache

/// An already-downloaded icon, without touching the network.
pub fn cached(key: &str) -> Option<PathBuf> {
    // A broken symlink among the icons makes the Python raise out of the
    // caller's row builder. This signature cannot carry that; callers that
    // need the exception use `cached_checked`.
    cached_checked(key).unwrap_or(None)
}

/// `cached`, with the Python's one exception (a file that cannot be
/// stat'ed, e.g. a broken symlink) as `Err`.
pub fn cached_checked(key: &str) -> Result<Option<PathBuf>, String> {
    if key.is_empty() {
        return Ok(None);
    }
    // ICONS.glob(f"{_safe(key)}.*"): `_safe` leaves no glob characters, so
    // this is a prefix match on the raw name (which need not be UTF-8), in
    // directory order as Python's scandir gives.
    let prefix = format!("{}.", _safe(key));
    let dir = icons();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(None);
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.as_bytes().starts_with(prefix.as_bytes()) {
            continue;
        }
        let f = dir.join(&name);
        let meta = std::fs::metadata(&f).map_err(|e| format!("OSError: {e}"))?;
        if meta.len() > 0 {
            return Ok(Some(f));
        }
    }
    Ok(None)
}

fn _safe(key: &str) -> String {
    key.chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '_' })
        .collect()
}

// ---------------------------------------------------------------- urllib

// `urllib.request.urlopen`, as much of it as `_fetch` reaches: the request
// it sends (GET, HTTP/1.1, `Accept-Encoding: identity`, `Connection: close`,
// no decompression), a timeout on every socket operation rather than on the
// whole transfer, `read(n)` returning a short body without complaint, the
// redirect rules, proxies from `<scheme>_proxy` only, and `file:` / `data:`
// URLs. Errors carry Python's exception name; only that there is one matters.

/// Python's `str.isspace()` for one character.
fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

fn py_strip(s: &str) -> &str {
    s.trim_matches(py_isspace)
}

/// `int(s, base)`: surrounding whitespace, a sign, `_` between digits, a
/// `0x` prefix for base 16. `ascii_ws` for bytes, where only ASCII
/// whitespace is stripped. Saturates instead of growing without bound.
fn py_int(s: &str, base: u32, ascii_ws: bool) -> Option<i128> {
    let t = if ascii_ws {
        s.trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r'))
    } else {
        py_strip(s)
    };
    let (neg, mut t) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let mut prefixed = false;
    if base == 16 && (t.starts_with("0x") || t.starts_with("0X")) {
        t = &t[2..];
        prefixed = true;
    }
    let (mut val, mut digits, mut last_us) = (0i128, 0usize, false);
    for (n, c) in t.chars().enumerate() {
        if c == '_' {
            if last_us || (n == 0 && !prefixed) {
                return None;
            }
            last_us = true;
            continue;
        }
        let d = c.to_digit(base)?;
        val = val.saturating_mul(i128::from(base)).saturating_add(i128::from(d)).min(1 << 100);
        digits += 1;
        last_us = false;
    }
    if digits == 0 || last_us || (base == 10 && digits > int_max_str_digits()) {
        return None;
    }
    Some(if neg { -val } else { val })
}

/// `str.split(None, maxsplit)`.
fn py_split(s: &str, maxsplit: usize) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = s.trim_start_matches(py_isspace);
    while !rest.is_empty() {
        if out.len() == maxsplit {
            out.push(rest);
            break;
        }
        let end = rest.find(py_isspace).unwrap_or(rest.len());
        out.push(&rest[..end]);
        rest = rest[end..].trim_start_matches(py_isspace);
    }
    out
}

/// `str.capitalize()`.
fn py_capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c.flat_map(char::to_lowercase)).collect(),
        None => String::new(),
    }
}

/// A Python dict of str, in insertion order.
fn dict_set<V>(d: &mut Vec<(String, V)>, k: &str, v: V) {
    match d.iter_mut().find(|(x, _)| x == k) {
        Some(e) => e.1 = v,
        None => d.push((k.to_string(), v)),
    }
}

fn dict_get<'a, V>(d: &'a [(String, V)], k: &str) -> Option<&'a V> {
    d.iter().find(|(x, _)| x == k).map(|(_, v)| v)
}

fn dict_pop<V>(d: &mut Vec<(String, V)>, k: &str) -> Option<V> {
    let i = d.iter().position(|(x, _)| x == k)?;
    Some(d.remove(i).1)
}

/// `%XX` decoded, other bytes as they are.
fn percent_decode(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'%' && s.get(i + 1).is_some_and(u8::is_ascii_hexdigit) && s.get(i + 2).is_some_and(u8::is_ascii_hexdigit) {
            let h = std::str::from_utf8(&s[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
            if let Some(h) = h {
                out.push(h);
                i += 3;
                continue;
            }
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

/// `urllib.parse.unquote` (UTF-8, errors="replace").
fn unquote(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    String::from_utf8_lossy(&percent_decode(s.as_bytes())).into_owned()
}

/// `unwrap('<URL:type://host/path>')`.
fn unwrap_url(url: &str) -> String {
    let mut u = py_strip(url);
    if u.starts_with('<') && u.ends_with('>') && !u.is_empty() {
        u = py_strip(&u[1..u.len() - 1]);
        if u.is_empty() {
            return String::new();
        }
    }
    if let Some(r) = u.strip_prefix("URL:") {
        u = py_strip(r);
    }
    u.to_string()
}

/// `_splittype`: `([^/:]+):(.*)`.
fn _splittype(url: &str) -> (Option<String>, &str) {
    for (i, c) in url.char_indices() {
        if c == '/' {
            break;
        }
        if c == ':' {
            if i == 0 {
                break;
            }
            return (Some(url[..i].to_lowercase()), &url[i + 1..]);
        }
    }
    (None, url)
}

/// `_splithost`: `//([^/#?]*)(.*)`.
fn _splithost(url: &str) -> (Option<String>, String) {
    let Some(r) = url.strip_prefix("//") else {
        return (None, url.to_string());
    };
    let end = r.find(['/', '#', '?']).unwrap_or(r.len());
    let path = &r[end..];
    let path = if !path.is_empty() && !path.starts_with('/') { format!("/{path}") } else { path.to_string() };
    (Some(r[..end].to_string()), path)
}

/// `_splittag`: the last `#`.
fn _splittag(url: &str) -> (String, Option<String>) {
    match url.rfind('#') {
        Some(i) => (url[..i].to_string(), Some(url[i + 1..].to_string())),
        None => (url.to_string(), None),
    }
}

/// `_splitport`: `(.*):([0-9]*)`, the port only when non-empty.
fn _splitport(host: &str) -> (&str, Option<&str>) {
    if let Some(i) = host.rfind(':') {
        let port = &host[i + 1..];
        if port.bytes().all(|b| b.is_ascii_digit()) {
            return (&host[..i], (!port.is_empty()).then_some(port));
        }
    }
    (host, None)
}

#[derive(Debug, Clone, Default)]
struct Split {
    scheme: Option<String>,
    netloc: Option<String>,
    path: String,
    query: Option<String>,
    fragment: Option<String>,
}

const USES_RELATIVE: &[&str] = &[
    "", "ftp", "http", "gopher", "nntp", "imap", "wais", "file", "https", "shttp", "mms", "prospero", "rtsp",
    "rtsps", "rtspu", "sftp", "svn", "svn+ssh", "ws", "wss",
];
const USES_NETLOC: &[&str] = &[
    "", "ftp", "http", "gopher", "nntp", "telnet", "imap", "wais", "file", "mms", "https", "shttp", "snews",
    "prospero", "rtsp", "rtsps", "rtspu", "rsync", "svn", "svn+ssh", "sftp", "nfs", "git", "git+ssh", "ws", "wss",
    "itms-services",
];
const USES_PARAMS: &[&str] = &[
    "", "ftp", "hdl", "prospero", "http", "imap", "https", "shttp", "rtsp", "rtsps", "rtspu", "sip", "sips", "mms",
    "sftp", "tel",
];

/// Characters whose NFKC form holds one of `/?#@:` (`_checknetloc`).
const NFKC_DELIMS: &[char] = &[
    '\u{2047}', '\u{2048}', '\u{2049}', '\u{2100}', '\u{2101}', '\u{2105}', '\u{2106}', '\u{2a74}', '\u{fe13}',
    '\u{fe16}', '\u{fe55}', '\u{fe56}', '\u{fe5f}', '\u{fe6b}', '\u{ff03}', '\u{ff0f}', '\u{ff1a}', '\u{ff1f}',
    '\u{ff20}',
];

fn _check_bracketed_host(hostname: &str) -> Result<(), String> {
    let bad = || Err("ValueError: Invalid IPv6 URL".to_string());
    if hostname.starts_with(['v', 'V']) {
        // \A[vV][a-fA-F0-9]+\..+\z
        let r = &hostname[1..];
        let hex = r.find(|c: char| !c.is_ascii_hexdigit()).unwrap_or(r.len());
        if hex == 0 || !r[hex..].starts_with('.') || r[hex + 1..].is_empty() || r[hex + 1..].contains('\n') {
            return bad();
        }
        return Ok(());
    }
    if hostname.parse::<std::net::Ipv4Addr>().is_ok() {
        return bad();
    }
    let (addr, scope) = match hostname.split_once('%') {
        Some((a, s)) => (a, Some(s)),
        None => (hostname, None),
    };
    if scope.is_some_and(|s| s.is_empty() || s.contains('%')) || addr.parse::<std::net::Ipv6Addr>().is_err() {
        return bad();
    }
    Ok(())
}

/// `urllib.parse._urlsplit`.
fn _urlsplit(url: &str, allow_fragments: bool) -> Result<Split, String> {
    let url: String = url.trim_start_matches(|c: char| c <= ' ').chars().filter(|c| !matches!(c, '\t' | '\r' | '\n')).collect();
    let mut sp = Split::default();
    let mut rest = url.as_str();
    if let Some(i) = rest.find(':') {
        let first = rest.chars().next().unwrap_or(' ');
        if i > 0
            && first.is_ascii_alphabetic()
            && rest[..i].chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        {
            sp.scheme = Some(rest[..i].to_ascii_lowercase());
            rest = &rest[i + 1..];
        }
    }
    if let Some(r) = rest.strip_prefix("//") {
        let end = r.find(['/', '?', '#']).unwrap_or(r.len());
        let netloc = &r[..end];
        rest = &r[end..];
        if netloc.contains('[') != netloc.contains(']') {
            return Err("ValueError: Invalid IPv6 URL".into());
        }
        if netloc.contains('[') {
            let hp = netloc.rsplit_once('@').map_or(netloc, |(_, h)| h);
            let hostname = match hp.split_once('[') {
                Some((before, bracketed)) => {
                    if !before.is_empty() {
                        return Err("ValueError: Invalid IPv6 URL".into());
                    }
                    let (h, port) = bracketed.split_once(']').unwrap_or((bracketed, ""));
                    if !port.is_empty() && !port.starts_with(':') {
                        return Err("ValueError: Invalid IPv6 URL".into());
                    }
                    h
                }
                None => hp.split_once(':').map_or(hp, |(h, _)| h),
            };
            _check_bracketed_host(hostname)?;
        }
        sp.netloc = Some(netloc.to_string());
    }
    let mut path = rest.to_string();
    if allow_fragments {
        if let Some((p, f)) = path.clone().split_once('#') {
            path = p.to_string();
            sp.fragment = Some(f.to_string());
        }
    }
    if let Some((p, q)) = path.clone().split_once('?') {
        path = p.to_string();
        sp.query = Some(q.to_string());
    }
    sp.path = path;
    if let Some(n) = &sp.netloc {
        if !n.is_ascii() && n.chars().filter(|c| !matches!(c, '@' | ':' | '#' | '?')).any(|c| NFKC_DELIMS.contains(&c)) {
            return Err("ValueError: netloc contains invalid characters under NFKC normalization".into());
        }
    }
    Ok(sp)
}

/// `urllib.parse._urlunsplit`.
fn _urlunsplit(scheme: Option<&str>, netloc: Option<&str>, url: &str, query: Option<&str>, fragment: Option<&str>) -> String {
    let mut url = url.to_string();
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

/// `urlparse(url)`: the six parts, `None` turned into "".
fn urlparse(url: &str) -> Result<[String; 6], String> {
    let sp = _urlsplit(url, true)?;
    let scheme = sp.scheme.unwrap_or_default();
    let mut path = sp.path;
    let mut params = String::new();
    if USES_PARAMS.contains(&scheme.as_str()) && path.contains(';') {
        let i = match path.rfind('/') {
            Some(slash) => path[slash..].find(';').map(|j| slash + j),
            None => path.find(';'),
        };
        if let Some(i) = i {
            params = path[i + 1..].to_string();
            path.truncate(i);
        }
    }
    Ok([scheme, sp.netloc.unwrap_or_default(), path, params, sp.query.unwrap_or_default(), sp.fragment.unwrap_or_default()])
}

/// `urlunparse(parts)`.
fn urlunparse(p: &[String; 6]) -> String {
    let [scheme, netloc, url, params, query, fragment] = p;
    let netloc = if netloc.is_empty() {
        (!scheme.is_empty() && USES_NETLOC.contains(&scheme.as_str()) && (url.is_empty() || url.starts_with('/'))).then_some("")
    } else {
        Some(netloc.as_str())
    };
    let url = if params.is_empty() { url.clone() } else { format!("{url};{params}") };
    let opt = |s: &String| (!s.is_empty()).then(|| s.clone());
    _urlunsplit(opt(scheme).as_deref(), netloc, &url, opt(query).as_deref(), opt(fragment).as_deref())
}

/// `urllib.parse.urljoin(base, url)`.
fn urljoin(base: &str, url: &str) -> Result<String, String> {
    if base.is_empty() {
        return Ok(url.to_string());
    }
    if url.is_empty() {
        return Ok(base.to_string());
    }
    let b = _urlsplit(base, true)?;
    let u = _urlsplit(url, true)?;
    let scheme = u.scheme.clone().or(b.scheme.clone());
    let s = scheme.clone().unwrap_or_default();
    if scheme != b.scheme || (!s.is_empty() && !USES_RELATIVE.contains(&s.as_str())) {
        return Ok(url.to_string());
    }
    let mut netloc = u.netloc.clone();
    if s.is_empty() || USES_NETLOC.contains(&s.as_str()) {
        if netloc.as_deref().is_some_and(|n| !n.is_empty()) {
            return Ok(_urlunsplit(scheme.as_deref(), netloc.as_deref(), &u.path, u.query.as_deref(), u.fragment.as_deref()));
        }
        netloc = b.netloc.clone();
    }
    let (mut query, mut fragment) = (u.query.clone(), u.fragment.clone());
    if u.path.is_empty() {
        if query.is_none() {
            query = b.query.clone();
            if fragment.is_none() {
                fragment = b.fragment.clone();
            }
        }
        return Ok(_urlunsplit(scheme.as_deref(), netloc.as_deref(), &b.path, query.as_deref(), fragment.as_deref()));
    }
    let mut base_parts: Vec<&str> = b.path.split('/').collect();
    if base_parts.last() != Some(&"") {
        // the last item is not a directory, so will not be taken into account
        // in resolving the relative path
        base_parts.pop();
    }
    let segments: Vec<&str> = if u.path.starts_with('/') {
        u.path.split('/').collect()
    } else {
        let mut s: Vec<&str> = base_parts;
        s.extend(u.path.split('/'));
        // filter out elements that would cause redundant slashes on re-joining
        // the resolved_path
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
            other => resolved.push(other),
        }
    }
    if matches!(segments.last(), Some(&".") | Some(&"..")) {
        resolved.push("");
    }
    let joined = resolved.join("/");
    let path = if joined.is_empty() { "/".to_string() } else { joined };
    Ok(_urlunsplit(scheme.as_deref(), netloc.as_deref(), &path, query.as_deref(), fragment.as_deref()))
}

/// `quote(url, encoding="iso-8859-1", safe=string.punctuation)`.
fn quote_redirect(url: &str) -> Result<String, String> {
    let mut out = String::new();
    for c in url.chars() {
        let b = u32::from(c);
        if b > 0xff {
            return Err("UnicodeEncodeError: 'latin-1' codec can't encode character".into());
        }
        if (0x21..0x7f).contains(&b) {
            out.push(c);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    Ok(out)
}

/// The `idna` codec (IDNA 2003). Non-ASCII labels go through UTS 46, which
/// maps them the same way for ordinary names.
fn idna_encode(host: &str) -> Result<String, String> {
    let err = |r: &str| Err(format!("UnicodeEncodeError: 'idna' codec can't encode: {r}"));
    if host.is_empty() {
        return Ok(String::new());
    }
    if host.is_ascii() {
        let labels: Vec<&str> = host.split('.').collect();
        if labels[..labels.len() - 1].iter().any(|l| l.is_empty()) {
            return err("label empty");
        }
        if labels.iter().any(|l| l.len() >= 64) {
            return err("label too long");
        }
        return Ok(host.to_string());
    }
    let mut labels: Vec<&str> = host.split(['.', '\u{3002}', '\u{ff0e}', '\u{ff61}']).collect();
    let trailing = labels.last() == Some(&"");
    if trailing {
        labels.pop();
    }
    let mut out = Vec::new();
    for label in labels {
        let ascii = if label.is_ascii() { label.to_string() } else { url::quirks::domain_to_ascii(label) };
        if ascii.is_empty() {
            return err("label empty");
        }
        if ascii.len() >= 64 {
            return err("label too long");
        }
        out.push(ascii);
    }
    let mut s = out.join(".");
    if trailing {
        s.push('.');
    }
    Ok(s)
}

fn b64encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |a, (i, &b)| a | (u32::from(b) << (16 - 8 * i)));
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

/// `binascii.a2b_base64` (not strict): only whether it raises matters.
fn a2b_base64(data: &[u8]) -> Result<Vec<u8>, String> {
    let val = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let (mut quad, mut pads, mut left, mut out) = (0u8, 0u8, 0u8, Vec::new());
    for &c in data {
        if c == b'=' {
            if quad >= 2 {
                pads += 1;
                if quad + pads >= 4 {
                    return Ok(out);
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
                out.push((left << 2) | (v >> 4));
                left = v & 0x0f;
            }
            2 => {
                quad = 3;
                out.push((left << 4) | (v >> 2));
                left = v & 0x03;
            }
            _ => {
                quad = 0;
                out.push((left << 6) | v);
                left = 0;
            }
        }
    }
    match quad {
        0 => Ok(out),
        1 => Err("binascii.Error: Invalid base64-encoded string".into()),
        _ => Err("binascii.Error: Incorrect padding".into()),
    }
}

// -- proxies

/// `getproxies_environment()`: `<scheme>_proxy` variables, the all-lowercase
/// spelling winning; `HTTP_PROXY` ignored under CGI. Note there is no
/// fallback: `all_proxy` only ever serves an `all:` URL.
fn getproxies_environment() -> Vec<(String, String)> {
    let env: Vec<(String, String)> = std::env::vars_os()
        .map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned()))
        .collect();
    let mut proxies: Vec<(String, String)> = Vec::new();
    let mut environment = Vec::new();
    for (name, value) in &env {
        let chars: Vec<char> = name.chars().collect();
        let n = chars.len();
        if n > 5 && chars[n - 6] == '_' && chars[n - 5..].iter().collect::<String>().to_lowercase() == "proxy" {
            let proxy_name = chars[..n - 6].iter().collect::<String>().to_lowercase();
            if !value.is_empty() {
                dict_set(&mut proxies, &proxy_name, value.clone());
            }
            environment.push((name, value, proxy_name));
        }
    }
    if env.iter().any(|(k, _)| k == "REQUEST_METHOD") {
        dict_pop(&mut proxies, "http");
    }
    for (name, value, proxy_name) in environment {
        if name.ends_with("_proxy") {
            if !value.is_empty() {
                dict_set(&mut proxies, &proxy_name, value.clone());
            } else {
                dict_pop(&mut proxies, &proxy_name);
            }
        }
    }
    proxies
}

/// The ProxyHandler's table, fixed when the first URL is opened (urllib
/// builds its opener once).
fn proxies() -> &'static [(String, String)] {
    static P: OnceLock<Vec<(String, String)>> = OnceLock::new();
    P.get_or_init(getproxies_environment)
}

/// `proxy_bypass_environment(host)`: `no_proxy`, read afresh each time.
fn proxy_bypass(host: &str) -> bool {
    let proxies = getproxies_environment();
    let Some(no_proxy) = dict_get(&proxies, "no") else {
        return false;
    };
    if no_proxy == "*" {
        return true;
    }
    let host = host.to_lowercase();
    let (hostonly, _) = _splitport(&host);
    for name in no_proxy.split(',') {
        let name = py_strip(name);
        if !name.is_empty() {
            let name = name.trim_start_matches('.').to_lowercase();
            if hostonly == name || host == name {
                return true;
            }
            let name = format!(".{name}");
            if hostonly.ends_with(&name) || host.ends_with(&name) {
                return true;
            }
        }
    }
    false
}

type ProxyParts = (Option<String>, Option<String>, Option<String>, String);

/// `_parse_proxy(proxy)`: scheme, user, password, host:port.
fn _parse_proxy(proxy: &str) -> Result<ProxyParts, String> {
    let (scheme, r_scheme) = _splittype(proxy);
    let (scheme, authority) = if !r_scheme.starts_with('/') {
        (None, proxy.to_string())
    } else {
        if !r_scheme.starts_with("//") {
            return Err(format!("ValueError: proxy URL with no authority: {proxy:?}"));
        }
        let end = match r_scheme.find('@') {
            Some(at) => r_scheme[at..].find('/').map(|j| at + j),
            None => r_scheme[2..].find('/').map(|j| 2 + j),
        };
        (scheme, r_scheme[2..end.unwrap_or(r_scheme.len()).max(2)].to_string())
    };
    let (userinfo, hostport) = match authority.rfind('@') {
        Some(i) => (Some(authority[..i].to_string()), authority[i + 1..].to_string()),
        None => (None, authority.clone()),
    };
    let (user, password) = match userinfo {
        Some(u) => match u.split_once(':') {
            Some((a, b)) => (Some(a.to_string()), Some(b.to_string())),
            None => (Some(u), None),
        },
        None => (None, None),
    };
    Ok((scheme, user, password, hostport))
}

// -- the request

/// `urllib.request.Request`, the parts used here.
#[derive(Clone)]
struct Req {
    full: String,
    fragment: Option<String>,
    typ: String,
    host: Option<String>,
    selector: String,
    tunnel_host: Option<String>,
    headers: Vec<(String, String)>,
    /// `None` is a Host header Python would fail to send.
    unredirected: Vec<(String, Option<String>)>,
    redirect_dict: Option<Rc<RefCell<HashMap<String, u32>>>>,
    timeout: Duration,
}

impl Req {
    /// `Request(url, headers=...)`; `origin` false where the caller passes
    /// `origin_req_host` (so the URL is not run through `urlparse`).
    fn new(url: &str, headers: &[(String, String)], origin: bool, timeout: Duration) -> Result<Req, String> {
        let (full, fragment) = _splittag(&unwrap_url(url));
        let (typ, rest) = _splittype(&full);
        let Some(typ) = typ else {
            return Err(format!("ValueError: unknown url type: {url:?}"));
        };
        let (host, selector) = _splithost(rest);
        let host = host.map(|h| if h.is_empty() { h } else { unquote(&h) });
        let mut req = Req {
            full: full.clone(),
            fragment,
            typ,
            host,
            selector,
            tunnel_host: None,
            headers: Vec::new(),
            unredirected: Vec::new(),
            redirect_dict: None,
            timeout,
        };
        for (k, v) in headers {
            dict_set(&mut req.headers, &py_capitalize(k), v.clone());
        }
        if origin {
            // request_host(): urlparse may raise.
            urlparse(&req.full_url())?;
        }
        Ok(req)
    }

    fn full_url(&self) -> String {
        match self.fragment.as_deref() {
            Some(f) if !f.is_empty() => format!("{}#{f}", self.full),
            _ => self.full.clone(),
        }
    }

    fn set_proxy(&mut self, host: String, typ: String) {
        if self.typ == "https" && self.tunnel_host.as_deref().is_none_or(str::is_empty) {
            self.tunnel_host = self.host.clone();
        } else {
            self.typ = typ;
            self.selector = self.full_url();
        }
        self.host = Some(host);
    }

    fn has_proxy(&self) -> bool {
        self.selector == self.full_url()
    }

    fn has_header(&self, name: &str) -> bool {
        dict_get(&self.headers, name).is_some() || dict_get(&self.unredirected, name).is_some()
    }
}

// -- the connection

enum Sock {
    Tcp(TcpStream),
    Tls(Box<openssl::ssl::SslStream<TcpStream>>),
}

/// OpenSSL's "unexpected eof while reading" (SSL_R_UNEXPECTED_EOF_WHILE_READING).
const SSL_R_UNEXPECTED_EOF_WHILE_READING: std::ffi::c_int = 294;

impl Read for Sock {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use openssl::ssl::ErrorCode;
        match self {
            Sock::Tcp(s) => s.read(buf),
            // suppress_ragged_eofs: a close without close_notify is an EOF.
            Sock::Tls(s) => loop {
                match s.ssl_read(buf) {
                    Ok(n) => return Ok(n),
                    Err(e) if e.code() == ErrorCode::ZERO_RETURN => return Ok(0),
                    Err(e) if e.code() == ErrorCode::SYSCALL && e.io_error().is_none() => return Ok(0),
                    Err(e) if e.code() == ErrorCode::WANT_READ && e.io_error().is_none() => {}
                    Err(e) => {
                        let eof = e.ssl_error().is_some_and(|st| {
                            st.errors().iter().any(|x| x.reason_code() == SSL_R_UNEXPECTED_EOF_WHILE_READING)
                        });
                        if eof {
                            return Ok(0);
                        }
                        return Err(match e.into_io_error() {
                            Ok(io) => io,
                            Err(e) => std::io::Error::other(format!("ssl.SSLError: {e}")),
                        });
                    }
                }
            },
        }
    }
}

impl Write for Sock {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Sock::Tcp(s) => s.write(buf),
            Sock::Tls(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Sock::Tcp(s) => s.flush(),
            Sock::Tls(s) => s.flush(),
        }
    }
}

/// `sock.makefile("rb")`.
struct Conn {
    sock: Sock,
    buf: Vec<u8>,
    pos: usize,
}

fn io_err(e: std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => "TimeoutError: timed out".into(),
        _ => format!("OSError: {e}"),
    }
}

impl Conn {
    fn new(sock: Sock) -> Conn {
        Conn { sock, buf: Vec::new(), pos: 0 }
    }

    /// One more read from the socket; false at EOF.
    fn fill(&mut self) -> Result<bool, String> {
        if self.pos > 0 {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
        let mut tmp = [0u8; 8192];
        loop {
            match self.sock.read(&mut tmp) {
                Ok(n) => {
                    self.buf.extend_from_slice(&tmp[..n]);
                    return Ok(n > 0);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(io_err(e)),
            }
        }
    }

    fn take(&mut self, n: usize) -> Vec<u8> {
        let n = n.min(self.buf.len() - self.pos);
        let out = self.buf[self.pos..self.pos + n].to_vec();
        self.pos += n;
        out
    }

    /// `readline(limit)`.
    fn readline(&mut self, limit: usize) -> Result<Vec<u8>, String> {
        loop {
            let avail = &self.buf[self.pos..];
            if let Some(nl) = avail.iter().position(|&b| b == b'\n') {
                return Ok(self.take((nl + 1).min(limit)));
            }
            if avail.len() >= limit || !self.fill()? {
                return Ok(self.take(limit));
            }
        }
    }

    /// `read(n)`: up to n bytes, fewer only at EOF.
    fn read(&mut self, n: usize) -> Result<Vec<u8>, String> {
        while self.buf.len() - self.pos < n {
            if !self.fill()? {
                break;
            }
        }
        Ok(self.take(n))
    }

    /// `read()`: to EOF.
    fn read_to_end(&mut self) -> Result<Vec<u8>, String> {
        while self.fill()? {}
        Ok(self.take(usize::MAX))
    }

    fn send(&mut self, data: &[u8]) -> Result<(), String> {
        self.sock.write_all(data).and_then(|_| self.sock.flush()).map_err(io_err)
    }
}

/// `socket.create_connection((host, port), timeout)`: every address in
/// turn, each with the full timeout, which then holds for every read and
/// write on the socket.
fn create_connection(host: &str, port: i128, timeout: Duration) -> Result<TcpStream, String> {
    if !(0..=65535).contains(&port) {
        return Err("OverflowError: getsockaddrarg: port must be 0-65535.".into());
    }
    let h = if host.is_ascii() { host.to_string() } else { idna_encode(host)? };
    let addrs: Vec<_> = (h.as_str(), port as u16).to_socket_addrs().map_err(|e| format!("socket.gaierror: {e}"))?.collect();
    let mut last = "OSError: getaddrinfo returns an empty list".to_string();
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, timeout) {
            Ok(s) => {
                let _ = s.set_nodelay(true);
                s.set_read_timeout(Some(timeout)).map_err(io_err)?;
                s.set_write_timeout(Some(timeout)).map_err(io_err)?;
                return Ok(s);
            }
            Err(e) => last = io_err(e),
        }
    }
    Err(last)
}

/// `http.client._create_https_context(11)`, i.e. `ssl.create_default_context()`
/// plus ALPN http/1.1, through the same OpenSSL Python's `ssl` module uses.
/// Every setting is the one CPython 3.14 makes, so what is accepted and
/// rejected is identical: the cipher string (TLS 1.2 CBC suites such as
/// ECDHE-RSA-AES128-SHA256 included), TLS 1.2 and up, VERIFY_X509_STRICT |
/// TRUSTED_FIRST | PARTIAL_CHAIN, hostname checks that fall back to the
/// subject CN when there is no subjectAltName, and the system CA store as
/// OpenSSL finds it (`SSL_CERT_FILE` / `SSL_CERT_DIR` included). Built once,
/// like urllib's opener.
fn tls_context() -> Result<openssl::ssl::SslContext, String> {
    use openssl::ssl::{SslContext, SslMethod, SslOptions, SslVerifyMode, SslVersion};
    use openssl::x509::verify::{X509CheckFlags, X509VerifyFlags};
    static CTX: OnceLock<Result<SslContext, String>> = OnceLock::new();
    CTX.get_or_init(|| {
        let err = |e: openssl::error::ErrorStack| format!("ssl.SSLError: {e}");
        let mut b = SslContext::builder(SslMethod::tls_client()).map_err(err)?;
        // ctx.options as Python leaves them: OP_ALL | OP_NO_SSLv3 |
        // OP_NO_COMPRESSION | OP_CIPHER_SERVER_PREFERENCE |
        // OP_ENABLE_MIDDLEBOX_COMPAT.
        b.clear_options(SslOptions::all());
        b.set_options(SslOptions::from_bits_retain(0x8252_0050));
        b.set_cipher_list("@SECLEVEL=2:ECDH+AESGCM:ECDH+CHACHA20:ECDH+AES:DHE+AES:!aNULL:!eNULL:!aDSS:!SHA1:!AESCCM")
            .map_err(err)?;
        b.set_min_proto_version(Some(SslVersion::TLS1_2)).map_err(err)?;
        b.set_verify(SslVerifyMode::PEER);
        let param = b.verify_param_mut();
        param.set_hostflags(X509CheckFlags::NO_PARTIAL_WILDCARDS);
        param
            .set_flags(X509VerifyFlags::X509_STRICT | X509VerifyFlags::TRUSTED_FIRST | X509VerifyFlags::PARTIAL_CHAIN)
            .map_err(err)?;
        b.set_default_verify_paths().map_err(err)?;
        b.set_alpn_protos(b"\x08http/1.1").map_err(err)?;
        Ok(b.build())
    })
    .clone()
}

/// `context.wrap_socket(sock, server_hostname=host)`: the hostname is
/// IDNA-encoded; an IP address is checked as an IP and sends no SNI, a name
/// goes out as SNI and is matched by OpenSSL exactly as given (so
/// "localhost." does not match a certificate for "localhost").
fn wrap_tls(tcp: TcpStream, server_hostname: &str) -> Result<Sock, String> {
    use openssl::ssl::{HandshakeError, Ssl};
    let ctx = tls_context()?;
    let host = idna_encode(server_hostname)?;
    let err = |e: openssl::error::ErrorStack| format!("ssl.SSLError: {e}");
    let mut ssl = Ssl::new(&ctx).map_err(err)?;
    match host.parse::<std::net::IpAddr>() {
        Ok(ip) => ssl.param_mut().set_ip(ip).map_err(err)?,
        Err(_) => {
            ssl.set_hostname(&host).map_err(err)?;
            ssl.param_mut().set_host(&host).map_err(err)?;
        }
    }
    match ssl.connect(tcp) {
        Ok(s) => Ok(Sock::Tls(Box::new(s))),
        Err(HandshakeError::SetupFailure(e)) => Err(err(e)),
        Err(HandshakeError::Failure(mid)) | Err(HandshakeError::WouldBlock(mid)) => {
            let e = mid.into_error();
            if let Some(io) = e.io_error() {
                if matches!(io.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) {
                    return Err("TimeoutError: _ssl.c:1011: The handshake operation timed out".into());
                }
            }
            Err(format!("ssl.SSLError: {e}"))
        }
    }
}

// -- the response

const MAXLINE: usize = 65536;
const MAXHEADERS: usize = 100;

/// `_read_status()`.
fn read_status(conn: &mut Conn) -> Result<(String, i128, String), String> {
    let raw = conn.readline(MAXLINE + 1)?;
    if raw.len() > MAXLINE {
        return Err("LineTooLong: status line".into());
    }
    if raw.is_empty() {
        return Err("RemoteDisconnected: Remote end closed connection without response".into());
    }
    let line: String = raw.iter().map(|&b| b as char).collect();
    let parts = py_split(&line, 2);
    let (version, status, reason) = match parts.len() {
        3 => (parts[0], parts[1], parts[2]),
        2 => (parts[0], parts[1], ""),
        _ => ("", "", ""),
    };
    if !version.starts_with("HTTP/") {
        return Err(format!("BadStatusLine: {line:?}"));
    }
    match py_int(status, 10, false) {
        Some(s) if (100..=999).contains(&s) => Ok((version.to_string(), s, reason.to_string())),
        _ => Err(format!("BadStatusLine: {line:?}")),
    }
}

/// `_read_headers()`: the raw lines, the blank one included.
fn read_header_lines(conn: &mut Conn) -> Result<Vec<u8>, String> {
    let mut all = Vec::new();
    let mut count = 0;
    loop {
        let line = conn.readline(MAXLINE + 1)?;
        if line.len() > MAXLINE {
            return Err("LineTooLong: header line".into());
        }
        count += 1;
        if count > MAXHEADERS {
            return Err(format!("HTTPException: got more than {MAXHEADERS} headers"));
        }
        let end = matches!(line.as_slice(), b"\r\n" | b"\n" | b"");
        all.extend_from_slice(&line);
        if end {
            return Ok(all);
        }
    }
}

/// The lines with their endings, split as the email package splits them.
fn split_keepends(s: &str) -> Vec<&str> {
    let b = s.as_bytes();
    let (mut out, mut start, mut i) = (Vec::new(), 0, 0);
    while i < b.len() {
        let end = match b[i] {
            b'\r' if b.get(i + 1) == Some(&b'\n') => i + 2,
            b'\r' | b'\n' => i + 1,
            _ => {
                i += 1;
                continue;
            }
        };
        out.push(&s[start..end]);
        start = end;
        i = end;
    }
    if start < b.len() {
        out.push(&s[start..]);
    }
    out
}

/// `email.parser.Parser().parsestr(...)` of a header block, compat32:
/// the header lines end at the first line that is not one.
fn parse_headers(raw: &[u8]) -> Vec<(String, String)> {
    let text: String = raw.iter().map(|&b| b as char).collect();
    let is_header = |line: &str| {
        if line.starts_with("From ") || line.starts_with([' ', '\t']) {
            return true;
        }
        let name_end = line.find(|c: char| !matches!(c, '\x21'..='\x39' | '\x3b'..='\x7e')).unwrap_or(line.len());
        line[name_end..].starts_with(':')
    };
    let lines: Vec<&str> = split_keepends(&text).into_iter().take_while(|l| is_header(l)).collect();
    let source_parse = |lines: &[&str]| {
        let (name, value) = lines[0].split_once(':').unwrap_or((lines[0], ""));
        let mut v = value.to_string();
        v.extend(lines[1..].iter().copied());
        (name.to_string(), v.trim_start_matches([' ', '\t', '\r', '\n']).trim_end_matches(['\r', '\n']).to_string())
    };
    let mut out = Vec::new();
    let mut last: Vec<&str> = Vec::new();
    for (lineno, &line) in lines.iter().enumerate() {
        if line.starts_with([' ', '\t']) {
            if !last.is_empty() {
                last.push(line);
            }
            continue;
        }
        if !last.is_empty() {
            out.push(source_parse(&last));
            last.clear();
        }
        if line.starts_with("From ") {
            if lineno == lines.len() - 1 && lineno != 0 {
                return out;
            }
            continue;
        }
        match line.find(':') {
            Some(0) | None => continue,
            Some(_) => last.push(line),
        }
    }
    if !last.is_empty() {
        out.push(source_parse(&last));
    }
    out
}

/// `msg.get(name)`: the first, by name in any case.
fn header_get(h: &[(String, String)], name: &str) -> Option<String> {
    h.iter().find(|(k, _)| k.to_lowercase() == name).map(|(_, v)| v.clone())
}

/// `http.client.HTTPResponse`.
struct HttpResp {
    conn: Conn,
    status: i128,
    headers: Vec<(String, String)>,
    chunked: bool,
    chunk_left: Option<i128>,
    length: Option<i128>,
}

impl HttpResp {
    /// `begin()`.
    fn begin(mut conn: Conn) -> Result<HttpResp, String> {
        let mut first = None;
        for _ in 0..100 {
            let (version, status, _) = read_status(&mut conn)?;
            if status != 100 {
                first = Some((version, status));
                break;
            }
            read_header_lines(&mut conn)?;
        }
        let Some((version, status)) = first else {
            return Err("HTTPException: got more than 100 interim responses".into());
        };
        if !(version == "HTTP/1.0" || version == "HTTP/0.9" || version.starts_with("HTTP/1.")) {
            return Err(format!("UnknownProtocol: {version}"));
        }
        let headers = parse_headers(&read_header_lines(&mut conn)?);
        let chunked = header_get(&headers, "transfer-encoding").is_some_and(|t| t.to_lowercase() == "chunked");
        let mut length = None;
        if let Some(l) = header_get(&headers, "content-length").filter(|l| !l.is_empty()) {
            if !chunked {
                length = py_int(&l, 10, false).filter(|&n| n >= 0);
            }
        }
        if status == 204 || status == 304 || (100..200).contains(&status) {
            length = Some(0);
        }
        Ok(HttpResp { conn, status, headers, chunked, chunk_left: None, length })
    }

    /// `_safe_read(n)`: exactly n bytes, or IncompleteRead. A negative n
    /// (a negative chunk size) reads to EOF, as `fp.read(-n)` does.
    fn safe_read(&mut self, n: i128) -> Result<Vec<u8>, String> {
        if n < 0 {
            return self.conn.read_to_end();
        }
        let want = usize::try_from(n).unwrap_or(usize::MAX);
        let data = self.conn.read(want)?;
        if data.len() < want {
            return Err("IncompleteRead".into());
        }
        Ok(data)
    }

    /// `_get_chunk_left()`.
    fn get_chunk_left(&mut self) -> Result<Option<i128>, String> {
        let chunk_left = self.chunk_left;
        if chunk_left.is_none() || chunk_left == Some(0) {
            if chunk_left.is_some() {
                self.safe_read(2)?; // toss the CRLF at the end of the chunk
            }
            let line = self.conn.readline(MAXLINE + 1)?;
            if line.len() > MAXLINE {
                return Err("LineTooLong: chunk size".into());
            }
            let line = match line.iter().position(|&b| b == b';') {
                Some(i) => &line[..i], // strip chunk-extensions
                None => &line[..],
            };
            let text: String = line.iter().map(|&b| b as char).collect();
            let Some(n) = py_int(&text, 16, true) else {
                return Err("IncompleteRead".into());
            };
            if n == 0 {
                // _read_and_discard_trailer()
                let mut trailers = 0;
                loop {
                    let l = self.conn.readline(MAXLINE + 1)?;
                    if l.len() > MAXLINE {
                        return Err("LineTooLong: trailer line".into());
                    }
                    if matches!(l.as_slice(), b"" | b"\r\n" | b"\n") {
                        break;
                    }
                    trailers += 1;
                    if trailers > MAXHEADERS {
                        return Err(format!("HTTPException: got more than {MAXHEADERS} trailers"));
                    }
                }
                self.chunk_left = None;
                return Ok(None);
            }
            self.chunk_left = Some(n);
            return Ok(Some(n));
        }
        Ok(chunk_left)
    }

    /// `_read_chunked(amt)`.
    fn read_chunked(&mut self, mut amt: Option<i128>) -> Result<Vec<u8>, String> {
        let mut value = Vec::new();
        while let Some(chunk_left) = self.get_chunk_left()? {
            if let Some(a) = amt {
                if a <= chunk_left {
                    value.extend(self.safe_read(a)?);
                    self.chunk_left = Some(chunk_left - a);
                    break;
                }
            }
            value.extend(self.safe_read(chunk_left)?);
            amt = amt.map(|a| a - chunk_left);
            self.chunk_left = Some(0);
        }
        Ok(value)
    }

    /// `read(amt)`: a body cut short by the server is returned as it is.
    fn read(&mut self, amt: usize) -> Result<Vec<u8>, String> {
        if self.chunked {
            return self.read_chunked(Some(amt as i128));
        }
        let amt = match self.length {
            Some(l) if (amt as i128) > l => usize::try_from(l).unwrap_or(0),
            _ => amt,
        };
        self.conn.read(amt)
    }

    /// `read()`.
    fn read_all(&mut self) -> Result<Vec<u8>, String> {
        if self.chunked {
            return self.read_chunked(None);
        }
        match self.length {
            None => self.conn.read_to_end(),
            Some(l) => self.safe_read(l),
        }
    }
}

enum Resp {
    Http(Box<HttpResp>),
    /// `addinfourl` from a `file:` or `data:` URL: no status.
    Local,
}

// -- the handlers

/// `HTTPConnection._get_hostport(host, None)`.
fn get_hostport(host: &str, default_port: i128) -> Result<(String, i128), String> {
    let i = host.rfind(':');
    let j = host.rfind(']');
    let (h, port) = match i {
        Some(i) if j.is_none_or(|j| i > j) => {
            let p = &host[i + 1..];
            let port = match py_int(p, 10, false) {
                Some(n) => n,
                None if p.is_empty() => default_port,
                None => return Err(format!("InvalidURL: nonnumeric port: '{p}'")),
            };
            (&host[..i], port)
        }
        _ => (host, default_port),
    };
    let h = if h.starts_with('[') && h.ends_with(']') && h.len() >= 2 { &h[1..h.len() - 1] } else { h };
    Ok((h.to_string(), port))
}

fn check_url_chars(s: &str) -> Result<(), String> {
    if s.chars().any(|c| c <= ' ' || c == '\x7f') {
        return Err(format!("InvalidURL: URL can't contain control characters. {s:?}"));
    }
    Ok(())
}

/// `putheader(name, value)`.
fn putheader(out: &mut Vec<u8>, name: &str, value: Option<&str>) -> Result<(), String> {
    let Some(value) = value else {
        return Err("TypeError: header value is None".into());
    };
    let legal_name = name.is_ascii()
        && name.chars().next().is_some_and(|c| c != ':' && !c.is_ascii_whitespace() && c != '\x0b')
        && name.chars().skip(1).all(|c| !matches!(c, ':' | '\r' | '\n'));
    if !legal_name {
        return Err(format!("ValueError: Invalid header name {name:?}"));
    }
    let mut v = Vec::new();
    for c in value.chars() {
        let b = u8::try_from(u32::from(c)).map_err(|_| "UnicodeEncodeError: 'latin-1' codec can't encode".to_string())?;
        v.push(b);
    }
    // \n(?![ \t])|\r(?![ \t\n])
    let illegal = v.iter().enumerate().any(|(i, &b)| match b {
        b'\n' => !matches!(v.get(i + 1), Some(b' ' | b'\t')),
        b'\r' => !matches!(v.get(i + 1), Some(b' ' | b'\t' | b'\n')),
        _ => false,
    });
    if illegal {
        return Err("ValueError: Invalid header value".into());
    }
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(b": ");
    out.extend_from_slice(&v);
    out.extend_from_slice(b"\r\n");
    Ok(())
}

/// `AbstractHTTPHandler.do_request_`.
fn do_request_(req: &mut Req) -> Result<(), String> {
    let Some(host) = req.host.clone().filter(|h| !h.is_empty()) else {
        return Err("URLError: no host given".into());
    };
    let mut sel_host = Some(host);
    if req.has_proxy() {
        let (_, sel) = _splittype(&req.selector);
        sel_host = _splithost(sel).0;
    }
    if !req.has_header("Host") {
        dict_set(&mut req.unredirected, "Host", sel_host);
    }
    if !req.has_header("User-agent") {
        dict_set(&mut req.unredirected, "User-agent", Some("Python-urllib/3.14".to_string()));
    }
    Ok(())
}

/// `AbstractHTTPHandler.do_open`, with HTTPConnection / HTTPSConnection.
fn do_open(req: &mut Req, https: bool) -> Result<Resp, String> {
    let Some(host) = req.host.clone().filter(|h| !h.is_empty()) else {
        return Err("URLError: no host given".into());
    };
    let default_port = if https { 443 } else { 80 };
    let (h_host, h_port) = get_hostport(&host, default_port)?;
    check_url_chars(&h_host)?;

    let mut merged: Vec<(String, Option<String>)> = req.unredirected.clone();
    for (k, v) in &req.headers {
        dict_set(&mut merged, k, Some(v.clone()));
    }
    dict_set(&mut merged, "Connection", Some("close".to_string()));
    let mut headers: Vec<(String, Option<String>)> = Vec::new();
    for (k, v) in merged {
        dict_set(&mut headers, &crate::util::py_title(&k), v);
    }
    let mut tunnel = None;
    if let Some(th) = req.tunnel_host.clone().filter(|t| !t.is_empty()) {
        let mut tunnel_headers: Vec<(String, String)> = Vec::new();
        if let Some(v) = dict_pop(&mut headers, "Proxy-Authorization") {
            tunnel_headers.push(("Proxy-Authorization".into(), v.unwrap_or_default()));
        }
        let (t_host, t_port) = get_hostport(&th, default_port)?;
        if !tunnel_headers.iter().any(|(k, _)| k.to_lowercase() == "host") {
            tunnel_headers.push(("Host".into(), format!("{}:{t_port}", idna_encode(&t_host)?)));
        }
        tunnel = Some((t_host, t_port, tunnel_headers));
    }

    // putrequest + putheader: everything is checked before connecting.
    let url = if req.selector.is_empty() { "/" } else { req.selector.as_str() };
    check_url_chars(url)?;
    if !url.is_ascii() {
        return Err("UnicodeEncodeError: 'ascii' codec can't encode the request line".into());
    }
    let mut out = format!("GET {url} HTTP/1.1\r\n").into_bytes();
    if !headers.iter().any(|(k, _)| k.to_lowercase() == "accept-encoding") {
        putheader(&mut out, "Accept-Encoding", Some("identity"))?;
    }
    for (k, v) in &headers {
        putheader(&mut out, k, v.as_deref())?;
    }
    out.extend_from_slice(b"\r\n");

    let tcp = create_connection(&h_host, h_port, req.timeout)?;
    let mut sni = h_host.clone();
    let tcp = match &tunnel {
        Some((t_host, t_port, tunnel_headers)) => {
            check_url_chars(t_host).map_err(|_| "ValueError: Tunnel host can't contain control characters".to_string())?;
            let mut target = idna_encode(t_host)?;
            if target.contains(':') && !target.starts_with('[') {
                target = format!("[{target}]");
            }
            let mut connect = format!("CONNECT {target}:{t_port} HTTP/1.1\r\n").into_bytes();
            for (k, v) in tunnel_headers {
                putheader(&mut connect, k, Some(v))?;
            }
            connect.extend_from_slice(b"\r\n");
            let mut c = Conn::new(Sock::Tcp(tcp));
            c.send(&connect)?;
            let (_, code, message) = read_status(&mut c)?;
            read_header_lines(&mut c)?;
            if code != 200 {
                return Err(format!("OSError: Tunnel connection failed: {code} {}", py_strip(&message)));
            }
            sni = t_host.clone();
            match c.sock {
                Sock::Tcp(t) => t,
                Sock::Tls(_) => return Err("OSError: tunnel".into()),
            }
        }
        None => tcp,
    };
    let sock = if https { wrap_tls(tcp, &sni)? } else { Sock::Tcp(tcp) };
    let mut conn = Conn::new(sock);
    conn.send(&out)?;
    Ok(Resp::Http(Box::new(HttpResp::begin(conn)?)))
}

/// `FileHandler.names`: the IPv4 addresses of localhost and this host.
fn local_names() -> &'static [String] {
    static N: OnceLock<Vec<String>> = OnceLock::new();
    N.get_or_init(|| {
        let v4 = |h: &str| -> Option<Vec<String>> {
            let a: Vec<String> = (h, 0u16).to_socket_addrs().ok()?.filter(|a| a.is_ipv4()).map(|a| a.ip().to_string()).collect();
            (!a.is_empty()).then_some(a)
        };
        let hostname = rustix::system::uname().nodename().to_string_lossy().into_owned();
        match (v4("localhost"), v4(&hostname)) {
            (Some(mut a), Some(b)) => {
                a.extend(b);
                a
            }
            (a, _) => a.map(|a| a[..1].to_vec()).unwrap_or_default(),
        }
    })
}

/// `_is_local_authority(authority, resolve=True)`.
fn is_local_authority(authority: &str) -> bool {
    if authority.is_empty() || authority == "localhost" {
        return true;
    }
    if rustix::system::uname().nodename().to_bytes() == authority.as_bytes() {
        return true;
    }
    let Ok(h) = (if authority.is_ascii() { Ok(authority.to_string()) } else { idna_encode(authority) }) else {
        return false;
    };
    let first = (h.as_str(), 0u16).to_socket_addrs().ok().and_then(|mut a| a.find(|a| a.is_ipv4()));
    first.is_some_and(|a| local_names().contains(&a.ip().to_string()))
}

/// `FileHandler.open_local_file`: a response with no status, or an error.
fn file_open(req: &Req) -> Result<Resp, String> {
    let sp = _urlsplit(&req.full_url(), true)?;
    if sp.scheme.as_deref() != Some("file") {
        return Err("URLError: URL is missing a 'file:' scheme".into());
    }
    if !is_local_authority(sp.netloc.as_deref().unwrap_or("")) {
        return Err("URLError: file:// scheme is supported only on localhost".into());
    }
    let raw = percent_decode(sp.path.as_bytes());
    if raw.contains(&0) {
        return Err("ValueError: embedded null byte".into());
    }
    let path = PathBuf::from(OsString::from_vec(raw));
    let meta = std::fs::metadata(&path).map_err(|e| format!("URLError: {e}"))?;
    // formatdate(st_mtime): datetime's years 1..9999.
    use std::os::unix::fs::MetadataExt;
    if !(-62_135_596_800..=253_402_300_799).contains(&meta.mtime()) {
        return Err("ValueError: year is out of range".into());
    }
    let f = std::fs::File::open(&path).map_err(|e| format!("URLError: {e}"))?;
    if f.metadata().map(|m| m.is_dir()).unwrap_or(false) {
        return Err("URLError: Is a directory".into());
    }
    Ok(Resp::Local)
}

/// `DataHandler.data_open`.
fn data_open(req: &Req) -> Result<Resp, String> {
    let url = req.full_url();
    let (_, data) = url.split_once(':').unwrap_or(("", ""));
    let Some((mediatype, data)) = data.split_once(',') else {
        return Err("ValueError: not enough values to unpack".into());
    };
    if mediatype.chars().any(|c| c < '\x20' || c == '\x7f') {
        return Err("ValueError: Control characters not allowed in data: mediatype".into());
    }
    let bytes = percent_decode(data.as_bytes());
    if mediatype.ends_with(";base64") {
        a2b_base64(&bytes)?;
    }
    Ok(Resp::Local)
}

/// `ProxyHandler.proxy_open`: `None` lets the next handler take it.
fn proxy_open(req: &mut Req, proxy: &str, depth: usize) -> Result<Option<Resp>, String> {
    let orig_type = req.typ.clone();
    let (ptype, user, password, hostport) = _parse_proxy(proxy)?;
    let ptype = ptype.unwrap_or_else(|| orig_type.clone());
    if req.host.as_deref().is_some_and(|h| !h.is_empty() && proxy_bypass(h)) {
        return Ok(None);
    }
    if let (Some(u), Some(p)) = (user.filter(|u| !u.is_empty()), password.filter(|p| !p.is_empty())) {
        let creds = b64encode(format!("{}:{}", unquote(&u), unquote(&p)).as_bytes());
        dict_set(&mut req.headers, "Proxy-authorization", format!("Basic {creds}"));
    }
    req.set_proxy(unquote(&hostport), ptype.clone());
    if orig_type == ptype || orig_type == "https" {
        return Ok(None);
    }
    // need to start over, because the other handlers don't grok the proxy's
    // URL type
    Ok(Some(open(req.clone(), depth + 1)?))
}

fn _open(req: &mut Req, depth: usize) -> Result<Resp, String> {
    let t = req.typ.clone();
    if let Some(proxy) = dict_get(proxies(), &t).cloned() {
        if let Some(r) = proxy_open(req, &proxy, depth)? {
            return Ok(r);
        }
    }
    match t.as_str() {
        "http" => do_open(req, false),
        "https" => do_open(req, true),
        "file" => file_open(req),
        "data" => data_open(req),
        // urllib would try an FTP download; such a response has no status,
        // so `_fetch` would return b"" -- which `fetch` skips like an error.
        "ftp" => Err("URLError: ftp is not supported here".into()),
        other => Err(format!("URLError: unknown url type: {other}")),
    }
}

/// `HTTPRedirectHandler.http_error_302` (and the default handler's
/// HTTPError for every other status).
fn http_error(req: Req, resp: Resp, code: i128, depth: usize) -> Result<Resp, String> {
    let http_error = || Err(format!("HTTPError: HTTP Error {code}"));
    let Resp::Http(mut h) = resp else {
        return http_error();
    };
    if !matches!(code, 301 | 302 | 303 | 307 | 308) {
        return http_error();
    }
    let Some(newurl) = header_get(&h.headers, "location").or_else(|| header_get(&h.headers, "uri")) else {
        return http_error();
    };
    let mut parts = urlparse(&newurl)?;
    if !["http", "https", "ftp", ""].contains(&parts[0].as_str()) {
        return http_error();
    }
    if parts[2].is_empty() && !parts[1].is_empty() {
        parts[2] = "/".into();
    }
    let newurl = quote_redirect(&urlunparse(&parts))?;
    let newurl = urljoin(&req.full_url(), &newurl)?;
    let headers: Vec<(String, String)> = req
        .headers
        .iter()
        .filter(|(k, _)| !matches!(k.to_lowercase().as_str(), "content-length" | "content-type"))
        .cloned()
        .collect();
    let mut new = Req::new(&newurl.replace(' ', "%20"), &headers, false, req.timeout)?;
    let visited = match &req.redirect_dict {
        Some(v) => {
            let d = v.borrow();
            if d.get(&newurl).copied().unwrap_or(0) >= 4 || d.len() >= 10 {
                return http_error();
            }
            v.clone()
        }
        None => Rc::new(RefCell::new(HashMap::new())),
    };
    *visited.borrow_mut().entry(newurl).or_insert(0) += 1;
    new.redirect_dict = Some(visited);
    h.read_all()?;
    open(new, depth + 1)
}

/// `OpenerDirector.open(req)`.
fn open(mut req: Req, depth: usize) -> Result<Resp, String> {
    if depth > 100 {
        return Err("RecursionError: maximum recursion depth exceeded".into());
    }
    let protocol = req.typ.clone();
    let is_http = protocol == "http" || protocol == "https";
    if is_http {
        do_request_(&mut req)?;
    }
    let resp = _open(&mut req, depth)?;
    if is_http {
        // HTTPErrorProcessor
        let code = match &resp {
            Resp::Http(h) => h.status,
            Resp::Local => return Err("TypeError: '<=' not supported between instances of 'int' and 'NoneType'".into()),
        };
        if !(200..300).contains(&code) {
            return http_error(req, resp, code, depth);
        }
    }
    Ok(resp)
}

#[cfg(test)]
static FETCHES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn _fetch(url: &str, timeout: u64) -> Result<Vec<u8>, String> {
    #[cfg(test)]
    FETCHES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let ua = [("User-Agent".to_string(), UA.to_string())];
    let req = Req::new(url, &ua, true, Duration::from_secs(timeout))?;
    match open(req, 0)? {
        Resp::Http(mut r) if r.status == 200 => r.read(512 * 1024),
        _ => Ok(Vec::new()),
    }
}

/// The URLs `fetch` tries, in order.
fn _candidates(url: &str, domain: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    if !url.is_empty() {
        candidates.push(url.to_string());
    }
    if !domain.is_empty() {
        // The exact host first: mcp.example.com may have its own mark. Then
        // the registrable domain, which is where the brand icon usually is.
        let reg = registrable(domain);
        let mut hosts = vec![domain.to_string()];
        if reg != domain {
            hosts.push(reg);
        }
        for host in hosts {
            candidates.extend(PATHS.iter().map(|p| format!("https://{host}{p}")));
        }
    }
    candidates
}

/// Get an icon for `key`, from an explicit url or a domain. Blocking.
///
/// `key` is what the result is cached under, so two services on the same
/// domain share one download. Pass "" for `url` / `domain` where the Python
/// leaves them at their defaults.
pub fn fetch(key: &str, url: &str, domain: &str) -> Option<PathBuf> {
    // Where the Python raises (a misses file that is not an object, a stamp
    // that is not a number), its caller -- a thread-pool task -- shows
    // nothing; None does the same.
    fetch_checked(key, url, domain).unwrap_or(None)
}

/// Python's `bool(x)` for a JSON value.
fn py_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_none_or(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `fetch`, with the Python's exceptions as `Err`.
pub fn fetch_checked(key: &str, url: &str, domain: &str) -> Result<Option<PathBuf>, String> {
    if key.is_empty() {
        return Ok(None);
    }
    let hit = cached_checked(key)?;
    if hit.is_some() {
        return Ok(hit);
    }
    if !py_truthy(&crate::settings::get("net_vendor_icons")?) {
        return Ok(None); // what is cached still shows
    }

    let misses = _load_misses()?;
    let Node::Dict(items) = &misses.nodes[misses.root] else {
        return Err(format!("AttributeError: '{}' object has no attribute 'get'", py_type(&misses.nodes[misses.root])));
    };
    let key16: Vec<u16> = key.encode_utf16().collect();
    let stamp = match items.iter().find(|(k, _)| *k == key16) {
        Some((_, id)) => as_number(&misses.nodes[*id])?,
        None => 0.0,
    };
    if crate::util::now() - stamp < MISS_TTL {
        return Ok(None);
    }

    for cand in _candidates(url, domain) {
        let Ok(data) = _fetch(&cand, 12) else {
            continue;
        };
        if data.len() < 64 {
            continue;
        }
        let ext = _kind(&data);
        if ext.is_empty() {
            continue;
        }
        let save = || -> std::io::Result<PathBuf> {
            let dir = icons();
            std::fs::create_dir_all(&dir)?;
            let dest = dir.join(format!("{}{ext}", _safe(key)));
            let tmp = dir.join(format!("{}{ext}.tmp", _safe(key)));
            std::fs::write(&tmp, &data)?;
            std::fs::rename(&tmp, &dest)?;
            Ok(dest)
        };
        return Ok(save().ok());
    }

    _save_miss(key)?;
    Ok(None)
}

/// The org or user avatar, which is the brand mark for most repos.
pub fn github_avatar(owner: &str) -> String {
    if owner.is_empty() {
        String::new()
    } else {
        format!("https://github.com/{owner}.png?size=128")
    }
}

// ---------------------------------------------------------------- parity

/// Lay out a cache / state directory from the parity args, the same way the
/// Python case does before it runs: `cache_home` is emptied and `setup`
/// entries (`{"path", "bytes" | "text" | "dir" | "symlink"}`) written under
/// it; `state_home` gets `settings` (text) as its settings file.
fn parity_prepare(args: &Value) {
    if let Some(dir) = args.get("cache_home").and_then(Value::as_str) {
        let root = PathBuf::from(dir);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::create_dir_all(&root);
        for e in args.get("setup").and_then(Value::as_array).into_iter().flatten() {
            let p = root.join(e.get("path").and_then(Value::as_str).unwrap_or_default());
            if let Some(parent) = p.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Some(b) = e.get("bytes").and_then(Value::as_array) {
                let raw: Vec<u8> = b.iter().filter_map(Value::as_u64).map(|b| b as u8).collect();
                let _ = std::fs::write(&p, raw);
            } else if let Some(t) = e.get("text").and_then(Value::as_str) {
                let _ = std::fs::write(&p, t);
            } else if e.get("dir").is_some() {
                let _ = std::fs::create_dir_all(&p);
            } else if let Some(t) = e.get("symlink").and_then(Value::as_str) {
                let _ = std::os::unix::fs::symlink(t, &p);
            }
        }
    }
    if let Some(dir) = args.get("state_home").and_then(Value::as_str) {
        let p = PathBuf::from(dir).join("almanac").join("settings.json");
        let _ = std::fs::remove_file(&p);
        if let Some(t) = args.get("settings").and_then(Value::as_str) {
            let _ = std::fs::create_dir_all(PathBuf::from(dir).join("almanac"));
            let _ = std::fs::write(&p, t);
        }
    }
}

/// What a `fetch` left behind: every file under `cache_home/almanac` with
/// its size, and the misses file with `key`'s fresh time stamp as `T`.
fn parity_after(args: &Value, key: &str) -> Value {
    use serde_json::json;
    let root = PathBuf::from(args.get("cache_home").and_then(Value::as_str).unwrap_or_default());
    let mut files = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let rel = p.strip_prefix(&root).unwrap_or(&p).to_string_lossy().into_owned();
            match std::fs::symlink_metadata(&p) {
                Ok(m) if m.is_dir() => stack.push(p),
                // The misses file's size depends on the time stamp's digits.
                Ok(_) if rel.ends_with("icon-misses.json") => files.push(json!([rel, null])),
                Ok(m) => files.push(json!([rel, m.len()])),
                Err(_) => {}
            }
        }
    }
    files.sort_by_key(|f| f[0].as_str().unwrap_or_default().to_string());
    let misses_text = std::fs::read_to_string(root.join("almanac").join("icon-misses.json"))
        .ok()
        .map(|t| stamp_out(&t, key));
    json!({ "files": files, "misses": misses_text })
}

/// Replace the number after `"<key>": ` with `T`.
fn stamp_out(text: &str, key: &str) -> String {
    let needle = format!("{}: ", crate::util::py_json_string(key));
    let Some(i) = text.find(&needle) else {
        return text.to_string();
    };
    let start = i + needle.len();
    let end = text[start..]
        .find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '-' | '+')))
        .map_or(text.len(), |n| start + n);
    format!("{}T{}", &text[..start], &text[end..])
}

/// A path for the parity output (a name that is not UTF-8 comes out with
/// U+FFFD, as the Python case writes it too).
fn lossy(p: &Path) -> Value {
    Value::String(p.to_string_lossy().into_owned())
}

/// Parity hook: see ARCHITECTURE.md.
///
/// `cache_home` / `state_home` set `XDG_CACHE_HOME` / `XDG_STATE_HOME` for
/// the call (restored after), laid out first by `parity_prepare`.
pub fn parity(func: &str, args: &serde_json::Value) -> Option<serde_json::Value> {
    use crate::util::arg_str;
    use serde_json::json;

    let old_cache = std::env::var_os("XDG_CACHE_HOME");
    let old_state = std::env::var_os("XDG_STATE_HOME");
    parity_prepare(args);
    if let Some(d) = args.get("cache_home").and_then(Value::as_str) {
        std::env::set_var("XDG_CACHE_HOME", d);
    }
    if let Some(d) = args.get("state_home").and_then(Value::as_str) {
        std::env::set_var("XDG_STATE_HOME", d);
    }
    let err = |e: String| json!({ "error": e });
    let opt_path = |p: Option<PathBuf>| p.as_deref().map_or(Value::Null, lossy);
    let key = arg_str(args, "key");
    let out = match func {
        "CACHE" => Some(lossy(&cache())),
        "ICONS" => Some(lossy(&icons())),
        "MISSES" => Some(lossy(&misses())),
        "MISS_TTL" => Some(json!(MISS_TTL)),
        "UA" => Some(json!({ "User-Agent": UA })),
        "PATHS" => Some(json!(PATHS)),
        "_kind" => {
            let data: Vec<u8> = args["data"]
                .as_array()
                .map(|a| a.iter().filter_map(Value::as_u64).map(|b| b as u8).collect())
                .unwrap_or_default();
            Some(json!(_kind(&data)))
        }
        "_safe" => Some(json!(_safe(key))),
        "registrable" => Some(json!(registrable(arg_str(args, "host")))),
        "host_of" => Some(json!(host_of(arg_str(args, "url")))),
        "github_avatar" => Some(json!(github_avatar(arg_str(args, "owner")))),
        "_load_misses" => Some(match _load_misses() {
            Ok(doc) => to_value(&doc, doc.root),
            Err(e) => err(e),
        }),
        "cached" => Some(match cached_checked(key) {
            Ok(p) => opt_path(p),
            Err(e) => err(e),
        }),
        "_fetch" => Some(match _fetch(arg_str(args, "url"), 12) {
            Ok(b) => json!(b),
            Err(e) => err(e),
        }),
        "_save_miss" => Some(match _save_miss(key) {
            Ok(()) => parity_after(args, key),
            Err(e) => err(e),
        }),
        "fetch" => Some(match fetch_checked(key, arg_str(args, "url"), arg_str(args, "domain")) {
            Ok(p) => json!({ "result": opt_path(p), "after": parity_after(args, key) }),
            Err(e) => err(e),
        }),
        _ => None,
    };
    restore("XDG_CACHE_HOME", old_cache);
    restore("XDG_STATE_HOME", old_state);
    out
}

fn restore(var: &str, old: Option<std::ffi::OsString>) {
    match old {
        Some(v) => std::env::set_var(var, v),
        None => std::env::remove_var(var),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    use std::sync::Mutex;

    // Tests share the process environment; one at a time.
    static ENV: Mutex<()> = Mutex::new(());

    /// `Settings.setUp`/`tearDown`: XDG_STATE_HOME (and here XDG_CACHE_HOME,
    /// which the Python reads at import) in a temp dir.
    struct Env {
        _guard: std::sync::MutexGuard<'static, ()>,
        _tmp: tempfile::TempDir,
        old: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }

    impl Env {
        fn new() -> Env {
            let guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
            let tmp = tempfile::tempdir().unwrap();
            let mut old = Vec::new();
            for (var, sub) in [("XDG_STATE_HOME", "state"), ("XDG_CACHE_HOME", "cache")] {
                old.push((var, std::env::var_os(var)));
                std::env::set_var(var, tmp.path().join(sub));
            }
            Env { _guard: guard, _tmp: tmp, old }
        }
    }

    impl Drop for Env {
        fn drop(&mut self) {
            for (var, v) in self.old.drain(..) {
                restore(var, v);
            }
        }
    }

    #[test]
    fn test_switched_off_network_features_make_no_request() {
        let _env = Env::new();
        for key in ["net_vendor_icons", "net_security_tracker", "net_gh_token",
                    "net_market_omarchy", "net_market_artcraft", "net_market_themes"] {
            crate::settings::set(key, Value::Bool(false)).unwrap();
        }
        // The cache is empty (cached() is None) and _fetch must not run.
        let before = FETCHES.load(Ordering::SeqCst);
        assert_eq!(fetch("tailscale.com", "", "tailscale.com"), None);
        assert_eq!(FETCHES.load(Ordering::SeqCst), before, "fetched");
    }

    #[test]
    fn kind_reads_signatures() {
        assert_eq!(_kind(b"\x89PNG\r\n"), ".png");
        assert_eq!(_kind(b"\x00\x00\x01\x00"), ".ico");
        assert_eq!(_kind(b"\xff\xd8\xff\xe0"), ".jpg");
        assert_eq!(_kind(b"<?xml version"), ".svg");
        assert_eq!(_kind(b" \x0b\n<svg xmlns"), ".svg");
        assert_eq!(_kind(b"<html>"), "");
        assert_eq!(_kind(b""), "");
    }

    #[test]
    fn registrable_and_hosts() {
        assert_eq!(registrable("mcp.stripe.com"), "stripe.com");
        assert_eq!(registrable("www.bbc.co.uk"), "bbc.co.uk");
        assert_eq!(registrable("a..b"), "a.b");
        assert_eq!(host_of("https://mcp.stripe.com:443/x"), "mcp.stripe.com");
        assert_eq!(host_of("HTTPS://x"), "");
        assert_eq!(github_avatar(""), "");
    }

    #[test]
    fn candidates_exact_host_then_registrable() {
        let c = _candidates("https://x/y.png", "mcp.stripe.com");
        assert_eq!(c.len(), 11);
        assert_eq!(c[0], "https://x/y.png");
        assert_eq!(c[1], "https://mcp.stripe.com/favicon.ico");
        assert_eq!(c[6], "https://stripe.com/favicon.ico");
        assert_eq!(_candidates("", "stripe.com").len(), 5);
        assert!(_candidates("", "").is_empty());
    }

    #[test]
    fn cached_skips_empty_files_and_misses_are_remembered() {
        let _env = Env::new();
        let dir = icons();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a_b.png"), b"").unwrap();
        assert_eq!(cached("a b"), None);
        std::fs::write(dir.join("a_b.ico"), b"x").unwrap();
        assert_eq!(cached("a b"), Some(dir.join("a_b.ico")));
        assert_eq!(cached(""), None);
        // A name that is not UTF-8 still matches.
        std::fs::remove_file(dir.join("a_b.ico")).unwrap();
        let raw = dir.join(OsString::from_vec(b"a_b.\xff".to_vec()));
        std::fs::write(&raw, b"x").unwrap();
        assert_eq!(cached("a b"), Some(raw));
        // A recent miss is not retried.
        crate::settings::set("net_vendor_icons", Value::Bool(true)).unwrap();
        _save_miss("nothing.invalid").unwrap();
        let before = FETCHES.load(Ordering::SeqCst);
        assert_eq!(fetch("nothing.invalid", "", "nothing.invalid"), None);
        assert_eq!(FETCHES.load(Ordering::SeqCst), before);
        // A misses file that is not an object raises in the Python.
        std::fs::write(misses(), "[1]").unwrap();
        assert!(fetch_checked("k", "", "").is_err());
        assert_eq!(fetch("k", "", ""), None);
        // An int stamp past the float range is an OverflowError.
        std::fs::write(misses(), format!("{{\"k\": -1{}}}", "0".repeat(400))).unwrap();
        assert!(fetch_checked("k", "", "").is_err());
    }

    #[test]
    fn stamp_out_hides_the_time() {
        assert_eq!(stamp_out(r#"{"a": 1.5, "b": 17.25e3}"#, "b"), r#"{"a": 1.5, "b": T}"#);
    }

    fn roundtrip(s: &str) -> Result<String, JsonErr> {
        py_loads(s).and_then(|d| py_dumps(&d))
    }

    #[test]
    fn json_reads_and_writes_as_python_does() {
        assert_eq!(roundtrip(r#"{"a": 1, "a": 2, "b": [1.5, -0, -0.0, 1E+2]}"#).unwrap(), r#"{"a": 2, "b": [1.5, 0, -0.0, 100.0]}"#);
        assert_eq!(roundtrip(r#"{"x": NaN, "y": -Infinity, "z": 1e400}"#).unwrap(), r#"{"x": NaN, "y": -Infinity, "z": Infinity}"#);
        assert_eq!(roundtrip(r#""\ud83d\ude00\udc00\ud800x \u00E9""#).unwrap(), r#""\ud83d\ude00\udc00\ud800x \u00e9""#);
        assert_eq!(roundtrip("\"\u{1F600}\u{7f}\"").unwrap(), r#""\ud83d\ude00\u007f""#);
        assert_eq!(roundtrip(&format!("[{}]", "9".repeat(400))).unwrap(), format!("[{}]", "9".repeat(400)));
        for bad in ["{1: 5}", "[1,]", "{\"a\":1,}", "\u{feff}{}", "-NaN", "01", "1.", "\"\x01\"", "{\"a\" 1}", "",
                    "[1] x", "\"\\u12\"", "\"\\x\""] {
            assert_eq!(py_loads(bad).err(), Some(JsonErr::Value), "{bad:?}");
        }
        assert_eq!(py_loads(&format!("[{}]", "1".repeat(4301))).err(), Some(JsonErr::Value));
        let deep = format!("{}{}", "[".repeat(200), "]".repeat(200));
        assert_eq!(roundtrip(&deep).unwrap(), deep);
        let too_deep = format!("{}{}", "[".repeat(LOADS_DEPTH + 1), "]".repeat(LOADS_DEPTH + 1));
        assert_eq!(py_loads(&too_deep).err(), Some(JsonErr::Recursion));
        let dicts = format!("{}1{}", "{\"a\": ".repeat(30_000), "}".repeat(30_000));
        assert_eq!(py_dumps(&py_loads(&dicts).unwrap()).err(), Some(JsonErr::Recursion));
    }

    #[test]
    fn paths_are_spelled_as_pathlib_spells_them() {
        assert_eq!(py_path(b"/a//b/./c/"), PathBuf::from("/a/b/c"));
        assert_eq!(py_path(b"//a"), PathBuf::from("//a"));
        assert_eq!(py_path(b"///a"), PathBuf::from("/a"));
        assert_eq!(py_path(b""), PathBuf::from("."));
        assert_eq!(py_join(&py_path(b""), "almanac"), PathBuf::from("almanac"));
        assert_eq!(py_join(&py_path(b"/"), "almanac"), PathBuf::from("/almanac"));
    }

    #[test]
    fn urls_split_and_join_as_urllib_does() {
        assert_eq!(urljoin("http://h/a/b?q#f", "/png").unwrap(), "http://h/png");
        assert_eq!(urljoin("http://h/a/b", "c/../d").unwrap(), "http://h/a/d");
        assert_eq!(urljoin("http://h/a/b", "..").unwrap(), "http://h/");
        assert_eq!(urljoin("http://h/a", "//x/y").unwrap(), "http://x/y");
        assert_eq!(urljoin("http://h/a", "mailto:x").unwrap(), "mailto:x");
        assert!(_urlsplit("http://[::1/x", true).is_err());
        assert!(_urlsplit("http://[127.0.0.1]/x", true).is_err());
        assert!(_urlsplit("http://[::1]:80/x", true).is_ok());
        assert!(_urlsplit("http://a\u{ff0f}b/", true).is_err());
        assert_eq!(quote_redirect("/a b\u{e9}%2F").unwrap(), "/a%20b%E9%2F");
        assert_eq!(_splittype("HTTP://x"), (Some("http".into()), "//x"));
        assert_eq!(_splithost("//h?q"), (Some("h".into()), "/?q".into()));
        assert_eq!(get_hostport("h:", 80).unwrap(), ("h".into(), 80));
        assert_eq!(get_hostport("[::1]:8", 80).unwrap(), ("::1".into(), 8));
        assert!(get_hostport("u:pw@h", 80).is_err());
        assert_eq!(py_int(" +1_0\r\n", 10, false), Some(10));
        assert_eq!(py_int("0x_1f", 16, true), Some(31));
        assert_eq!(py_int("1__0", 10, false), None);
        assert_eq!(b64encode(b"user:pw"), "dXNlcjpwdw==");
        assert!(a2b_base64(b"abc").is_err());
        assert_eq!(a2b_base64(b"YW J\njyw==").unwrap(), b"abc\xcb");
        assert_eq!(idna_encode("a..b").ok(), None);
        assert_eq!(
            parse_headers(b"Content-Length:  5\r\nX: a\r\n b\r\nbad line\r\nLate: no\r\n\r\n"),
            vec![("Content-Length".to_string(), "5".to_string()), ("X".to_string(), "a\r\n b".to_string())]
        );
    }

    /// A server that answers one connection with `reply`, then closes.
    fn serve(reply: Vec<u8>, pause: Option<Duration>) -> (String, std::thread::JoinHandle<Vec<u8>>) {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut req = Vec::new();
            let mut buf = [0u8; 1024];
            while !req.ends_with(b"\r\n\r\n") {
                let n = s.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                req.extend_from_slice(&buf[..n]);
            }
            for piece in reply.chunks(7) {
                let _ = s.write_all(piece);
                if let Some(p) = pause {
                    std::thread::sleep(p);
                }
            }
            req
        });
        (base, h)
    }

    #[test]
    fn fetch_sends_what_urllib_sends_and_keeps_short_bodies() {
        let (base, h) = serve(b"HTTP/1.1 200 OK\r\nContent-Length: 5000\r\n\r\nshort body".to_vec(), None);
        assert_eq!(_fetch(&format!("{base}/x?q#frag"), 12).unwrap(), b"short body");
        let req = String::from_utf8(h.join().unwrap()).unwrap();
        let port = base.rsplit(':').next().unwrap();
        assert_eq!(
            req,
            format!("GET /x?q HTTP/1.1\r\nAccept-Encoding: identity\r\nHost: 127.0.0.1:{port}\r\nUser-Agent: {UA}\r\nConnection: close\r\n\r\n")
        );
        // Chunked, gzip left alone, and a read timeout per read, not overall.
        let body = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Encoding: gzip\r\n\r\n5;x\r\nhello\r\n0\r\n\r\n";
        let (base, h) = serve(body.to_vec(), Some(Duration::from_millis(150)));
        assert_eq!(_fetch(&base, 1).unwrap(), b"hello");
        h.join().unwrap();
        // A chunked body cut short is an error.
        let (base, h) = serve(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n50\r\nabc".to_vec(), None);
        assert!(_fetch(&base, 12).is_err());
        h.join().unwrap();
        // Userinfo stays in the host, which then does not resolve.
        assert!(_fetch("http://user:pw@127.0.0.1:1/x", 12).is_err());
        // file: and data: open, with no status: b"".
        assert_eq!(_fetch("file:///etc/hostname", 12).unwrap(), b"");
        assert_eq!(_fetch("data:image/png;base64,iVBORw0KGgo=", 12).unwrap(), b"");
        assert!(_fetch("data:;base64,abc", 12).is_err());
        assert!(_fetch("file:///nonexistent/x", 12).is_err());
    }
}
