//! Almanac's own preferences, in one file.
//!
//! Everything a person can switch about Almanac itself lives here, with its
//! default -- including each thing it reaches over the network, so that "what
//! leaves your machine" in the README is something a user can turn off, not
//! just read about. Stored as JSON in the XDG state directory; a missing or
//! broken file means the defaults, never a crash.
//!
//! Scheduled updates keep their own file (autoupdate.py), because a shell
//! script reads it too.
//!
//! Port of `settings.py`.

use std::path::PathBuf;

use serde_json::Value;

/// A default value in `DEFAULTS`: every setting is a bool or a string.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DefaultValue {
    Bool(bool),
    Str(&'static str),
}

impl DefaultValue {
    /// As the JSON value the settings file would hold.
    pub fn to_value(self) -> Value {
        match self {
            DefaultValue::Bool(b) => Value::Bool(b),
            DefaultValue::Str(s) => Value::String(s.to_owned()),
        }
    }
}

/// Every setting and its default, in the Python dict's order.
pub const DEFAULTS: &[(&str, DefaultValue)] = &[
    // General
    ("start_page", DefaultValue::Str("last")),           // last | featured | updates | plugins
    // Notifications
    ("notify_plugin_updates", DefaultValue::Bool(true)), // from the background watcher
    ("notify_resets", DefaultValue::Bool(true)),         // a config file put back to its defaults
    ("notify_no_change", DefaultValue::Bool(true)),      // an action that ran but changed nothing
    // Updates
    ("hold_majors", DefaultValue::Bool(true)),           // major agent releases wait for a person
    ("plugin_scope", DefaultValue::Str("all")),          // see plugins.SCOPES
    // Network: each thing Almanac fetches, by what it is for
    ("net_vendor_icons", DefaultValue::Bool(true)),      // logos from each vendor's own domain
    ("net_security_tracker", DefaultValue::Bool(true)),  // security.archlinux.org advisories
    ("net_market_omarchy", DefaultValue::Bool(true)),    // plugins.omarchy.org catalogue
    ("net_market_artcraft", DefaultValue::Bool(true)),   // getartcraft.com + GitHub releases
    ("net_market_themes", DefaultValue::Bool(true)),     // the omarchy-theme topic on GitHub
    ("net_gh_token", DefaultValue::Bool(true)),          // use `gh auth token` for GitHub requests
    ("net_daily_picks", DefaultValue::Bool(true)),       // GitHub stats for Today's picks
];

pub const START_PAGES: &[(&str, &str)] = &[
    ("last", "Where you left off"),
    ("featured", "Today"),
    ("updates", "Updates"),
    ("plugins", "Plugins"),
];

/// `DEFAULTS.get(key)`: the default, or None for a key that is not a setting.
pub fn default_for(key: &str) -> Option<DefaultValue> {
    DEFAULTS.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

pub fn path() -> PathBuf {
    crate::xdg::state_home().join("almanac").join("settings.json")
}

// The settings file is read and written back whole, so every value in it --
// including keys Almanac does not know -- must survive `set` exactly as
// Python's json keeps it: integers of any size, NaN/Infinity, floats that
// overflow to inf, lone surrogate escapes, deep nesting. serde_json's `Value`
// can hold none of those, so the file goes through a small reader and writer
// that follow CPython's json module (`json.loads` / `json.dumps(indent=2,
// sort_keys=True)`) instead.

/// A Python `str`: code points, lone surrogates included (`"\ud800"` reads
/// as one, which a Rust `String` cannot hold).
type PyStr = Vec<u32>;

/// One value of a parsed file. Containers refer to their children by index
/// into `Doc::nodes`, so neither reading, writing nor dropping a deeply
/// nested file recurses.
#[derive(Debug, Clone)]
enum Node {
    Null,
    Bool(bool),
    /// A Python int, as `str(int)` writes it: any size, `-0` is `0`.
    Int(String),
    Float(f64),
    Str(PyStr),
    Arr(Vec<usize>),
    /// In first-insertion order, each key once (a dict).
    Obj(Vec<(PyStr, usize)>),
}

/// A parsed settings file; `nodes[0]` is the top-level value.
#[derive(Debug, Clone)]
struct Doc {
    nodes: Vec<Node>,
}

/// Nesting at which CPython 3.14's C json scanner raises RecursionError.
/// Its limit is a C-stack byte budget, not a count, so it depends on the
/// thread: with the default 8 MiB stack (`ulimit -s 8192`), a worker thread
/// decodes 52154 containers and not one more, the same every run, arrays and
/// dicts alike (measured). The main thread starts with part of its stack
/// already used (environment, ASLR offset), so its limit moves from run to
/// run -- 52098 to 52131 measured -- and no count matches every run; this is
/// the middle of that range. The Python does not catch RecursionError, so it
/// reaches the caller.
const MAX_DEPTH_MAIN: usize = 52118;
const MAX_DEPTH_THREAD: usize = 52154;

/// The nesting limit for the calling thread: the app's main (GTK) thread
/// stands in for Python's main thread, every other thread for a worker.
fn on_main_thread() -> bool {
    std::thread::current().name() == Some("main")
}

fn max_depth() -> usize {
    if on_main_thread() { MAX_DEPTH_MAIN } else { MAX_DEPTH_THREAD }
}

/// CPython's RecursionError text for a container that would nest too deep:
/// "used" is the stack in use when its budget ran out (8144 kB on the main
/// thread, 8156 kB on a worker, with an 8 MiB stack; measured).
fn recursion_message(is_obj: bool) -> String {
    let used = if on_main_thread() { 8144 } else { 8156 };
    let what = if is_obj { "object" } else { "array" };
    format!("RecursionError: Stack overflow (used {used} kB) while decoding a JSON {what} from a unicode string")
}

/// `int(str)` refuses more digits than this (`sys.get_int_max_str_digits()`),
/// with a ValueError that `_load` catches.
const INT_MAX_STR_DIGITS: usize = 4300;

enum JsonError {
    /// A ValueError (JSONDecodeError): `_load` falls back to `{}`.
    Value,
    /// A RecursionError: raised through `_load` to its caller. True when the
    /// container that went too deep is a dict.
    Recursion(bool),
}

impl Doc {
    fn empty() -> Doc {
        Doc { nodes: vec![Node::Obj(Vec::new())] }
    }

    /// `data.get(key)` on the top-level dict.
    fn get(&self, key: &str) -> Option<&Node> {
        let Node::Obj(entries) = &self.nodes[0] else { return None };
        let key = py_str(key);
        entries.iter().find(|(k, _)| *k == key).map(|(_, id)| &self.nodes[*id])
    }

    /// `data[key] = value` on the top-level dict.
    fn insert(&mut self, key: &str, value: &Setting) {
        let id = self.graft(&value.doc, value.root);
        let key = py_str(key);
        if let Node::Obj(entries) = &mut self.nodes[0] {
            match entries.iter_mut().find(|(k, _)| *k == key) {
                Some(entry) => entry.1 = id,
                None => entries.push((key, id)),
            }
        }
    }

    /// Copy node `root` of `other`, with everything under it, into this
    /// document; returns its new index.
    fn graft(&mut self, other: &Doc, root: usize) -> usize {
        let base = self.nodes.len();
        self.nodes.extend(other.nodes.iter().map(|n| match n {
            Node::Arr(items) => Node::Arr(items.iter().map(|c| c + base).collect()),
            Node::Obj(entries) => Node::Obj(entries.iter().map(|(k, c)| (k.clone(), c + base)).collect()),
            other => other.clone(),
        }));
        root + base
    }

    /// Add a serde value (from a caller: shallow) and return its index.
    fn push_value(&mut self, value: &Value) -> usize {
        let node = match value {
            Value::Null => Node::Null,
            Value::Bool(b) => Node::Bool(*b),
            Value::Number(n) => match n.as_f64().filter(|_| n.is_f64()) {
                Some(f) => Node::Float(f),
                None => Node::Int(n.to_string()),
            },
            Value::String(s) => Node::Str(py_str(s)),
            Value::Array(items) => Node::Arr(items.iter().map(|x| self.push_value(x)).collect()),
            Value::Object(map) => {
                Node::Obj(map.iter().map(|(k, x)| (py_str(k), self.push_value(x))).collect())
            }
        };
        self.nodes.push(node);
        self.nodes.len() - 1
    }

    /// The node as a serde value, for callers that need one: what serde
    /// cannot hold degrades (a lone surrogate to U+FFFD, NaN/inf to null,
    /// an int beyond u64/i64 to the nearest float).
    /// Built without recursion: a file can nest as deep as Python's json
    /// reads.
    fn to_value(&self, id: usize) -> Value {
        crate::util::tree::fold_ids(
            id,
            |n, out| match &self.nodes[n] {
                Node::Arr(items) => {
                    out.extend(items);
                    Some(false)
                }
                Node::Obj(entries) => {
                    out.extend(entries.iter().map(|(_, c)| *c));
                    Some(true)
                }
                _ => None,
            },
            |n| match &self.nodes[n] {
                Node::Null => Value::Null,
                Node::Bool(b) => Value::Bool(*b),
                Node::Int(s) => s.parse::<i64>().map(Value::from)
                    .or_else(|_| s.parse::<u64>().map(Value::from))
                    .unwrap_or_else(|_| float_value(s.parse::<f64>().unwrap_or(f64::NAN))),
                Node::Float(f) => float_value(*f),
                Node::Str(s) => Value::String(lossy(s)),
                Node::Arr(_) | Node::Obj(_) => Value::Null,
            },
            |_, kids| Value::Array(kids),
            |n, kids| match &self.nodes[n] {
                Node::Obj(entries) => Value::Object(entries.iter().map(|(k, _)| lossy(k)).zip(kids).collect()),
                _ => Value::Null,
            },
        )
    }

    /// `json.dumps(data, indent=indent, sort_keys=sort_keys)`: Python's
    /// default separators, `ensure_ascii`, NaN/Infinity allowed.
    fn dumps(&self, indent: Option<usize>, sort_keys: bool) -> String {
        self.dumps_from(0, indent, sort_keys)
    }

    /// `dumps` of node `root` and what is under it.
    fn dumps_from(&self, root: usize, indent: Option<usize>, sort_keys: bool) -> String {
        let item_sep = if indent.is_some() { "," } else { ", " };
        let nl = |out: &mut String, level: usize| {
            if let Some(n) = indent {
                out.push('\n');
                out.push_str(&" ".repeat(n * level));
            }
        };
        // Open containers: their children (with keys for a dict), the next
        // one to write, and the closing bracket.
        let mut stack: Vec<(Vec<(Option<&PyStr>, usize)>, usize, char)> = Vec::new();
        let mut out = String::new();
        let mut next = Some(root);
        loop {
            if let Some(id) = next.take() {
                match &self.nodes[id] {
                    Node::Null => out.push_str("null"),
                    Node::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
                    Node::Int(s) => out.push_str(s),
                    Node::Float(f) => out.push_str(&crate::util::py_float_repr(*f)),
                    Node::Str(s) => push_json_str(&mut out, s),
                    Node::Arr(items) if items.is_empty() => out.push_str("[]"),
                    Node::Obj(entries) if entries.is_empty() => out.push_str("{}"),
                    Node::Arr(items) => {
                        out.push('[');
                        stack.push((items.iter().map(|c| (None, *c)).collect(), 0, ']'));
                    }
                    Node::Obj(entries) => {
                        out.push('{');
                        let mut children: Vec<(Option<&PyStr>, usize)> =
                            entries.iter().map(|(k, c)| (Some(k), *c)).collect();
                        if sort_keys {
                            // sorted() on str: by code point.
                            children.sort_by(|a, b| a.0.cmp(&b.0));
                        }
                        stack.push((children, 0, '}'));
                    }
                }
            }
            let level = stack.len();
            let Some((children, i, close)) = stack.last_mut() else { break };
            if *i < children.len() {
                let (key, child) = children[*i];
                if *i > 0 {
                    out.push_str(item_sep);
                }
                *i += 1;
                nl(&mut out, level);
                if let Some(k) = key {
                    push_json_str(&mut out, k);
                    out.push_str(": ");
                }
                next = Some(child);
            } else {
                let close = *close;
                stack.pop();
                nl(&mut out, level - 1);
                out.push(close);
            }
        }
        out
    }
}

fn py_str(s: &str) -> PyStr {
    s.chars().map(u32::from).collect()
}

fn lossy(s: &[u32]) -> String {
    s.iter().map(|&c| char::from_u32(c).unwrap_or('\u{fffd}')).collect()
}

fn float_value(f: f64) -> Value {
    serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number)
}

/// A string the way `json.dumps` writes it (`ensure_ascii=True`): lone
/// surrogates come back out as the `\udXXX` escapes they were read from.
fn push_json_str(out: &mut String, s: &[u32]) {
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
            0x20..=0x7e => out.push(char::from(c as u8)),
            0x10000.. => {
                let v = c - 0x10000;
                out.push_str(&format!("\\u{:04x}\\u{:04x}", 0xd800 | (v >> 10), 0xdc00 | (v & 0x3ff)));
            }
            _ => out.push_str(&format!("\\u{c:04x}")),
        }
    }
    out.push('"');
}

/// `json.loads(text)`, following CPython's C scanner.
fn parse(text: &str) -> Result<Doc, JsonError> {
    let s: Vec<char> = text.chars().collect();
    // json.loads refuses a str that starts with a BOM.
    if s.first() == Some(&'\u{feff}') {
        return Err(JsonError::Value);
    }
    let ws = |mut i: usize| {
        while matches!(s.get(i), Some(' ' | '\t' | '\n' | '\r')) {
            i += 1;
        }
        i
    };
    let mut nodes: Vec<Node> = vec![Node::Null]; // [0]: the top level, set at the end
    // Open containers: index, and for a dict the key awaiting its value and
    // where each key already is (a repeated key keeps its place, last value).
    struct Open {
        id: usize,
        key: Option<PyStr>,
        seen: std::collections::HashMap<PyStr, usize>,
    }
    let mut stack: Vec<Open> = Vec::new();
    let limit = max_depth();
    let mut i = ws(0);
    // A dict key and its colon, from `"` on; returns the key.
    let key_at = |i: &mut usize| -> Result<PyStr, JsonError> {
        if s.get(*i) != Some(&'"') {
            return Err(JsonError::Value);
        }
        let key = parse_string(&s, i)?;
        *i = ws(*i);
        if s.get(*i) != Some(&':') {
            return Err(JsonError::Value);
        }
        *i = ws(*i + 1);
        Ok(key)
    };
    'value: loop {
        let mut node = match s.get(i) {
            Some(c @ ('{' | '[')) => {
                let is_obj = *c == '{';
                if stack.len() + 1 > limit {
                    return Err(JsonError::Recursion(is_obj));
                }
                nodes.push(if is_obj { Node::Obj(Vec::new()) } else { Node::Arr(Vec::new()) });
                let id = nodes.len() - 1;
                i = ws(i + 1);
                if s.get(i) == Some(if is_obj { &'}' } else { &']' }) {
                    i += 1;
                    id
                } else {
                    let key = if is_obj { Some(key_at(&mut i)?) } else { None };
                    stack.push(Open { id, key, seen: Default::default() });
                    continue 'value;
                }
            }
            Some('"') => {
                let v = parse_string(&s, &mut i)?;
                nodes.push(Node::Str(v));
                nodes.len() - 1
            }
            Some(_) => {
                let v = parse_scalar(&s, &mut i)?;
                nodes.push(v);
                nodes.len() - 1
            }
            None => return Err(JsonError::Value),
        };
        // A value is complete: hand it to its container, and close every
        // container that ends here.
        loop {
            let Some(top) = stack.last_mut() else {
                // The top level: nothing but whitespace may follow.
                if ws(i) != s.len() {
                    return Err(JsonError::Value);
                }
                // Move it into slot 0; no container refers to it.
                nodes.swap(0, node);
                return Ok(Doc { nodes });
            };
            let is_obj = top.key.is_some();
            match top.key.take() {
                Some(key) => {
                    let Node::Obj(entries) = &mut nodes[top.id] else { unreachable!() };
                    match top.seen.get(&key) {
                        Some(&at) => entries[at].1 = node,
                        None => {
                            top.seen.insert(key.clone(), entries.len());
                            entries.push((key, node));
                        }
                    }
                }
                None => {
                    let Node::Arr(items) = &mut nodes[top.id] else { unreachable!() };
                    items.push(node);
                }
            }
            i = ws(i);
            match s.get(i) {
                Some(',') => {
                    i = ws(i + 1);
                    if is_obj {
                        top.key = Some(key_at(&mut i)?);
                    }
                    continue 'value;
                }
                Some('}') if is_obj => {}
                Some(']') if !is_obj => {}
                _ => return Err(JsonError::Value),
            }
            i += 1;
            node = top.id;
            stack.pop();
        }
    }
}

/// A string from its opening quote; `i` ends past the closing one.
fn parse_string(s: &[char], i: &mut usize) -> Result<PyStr, JsonError> {
    let hex4 = |at: usize| -> Result<u32, JsonError> {
        let mut v = 0;
        for k in 0..4 {
            let d = s.get(at + k).and_then(|c| c.to_digit(16).filter(|_| c.is_ascii()));
            v = v * 16 + d.ok_or(JsonError::Value)?;
        }
        Ok(v)
    };
    let mut out = Vec::new();
    let mut j = *i + 1;
    loop {
        let c = *s.get(j).ok_or(JsonError::Value)?;
        j += 1;
        match c {
            '"' => break,
            '\\' => {
                let e = *s.get(j).ok_or(JsonError::Value)?;
                j += 1;
                out.push(match e {
                    '"' => 0x22,
                    '\\' => 0x5c,
                    '/' => 0x2f,
                    'b' => 0x08,
                    'f' => 0x0c,
                    'n' => 0x0a,
                    'r' => 0x0d,
                    't' => 0x09,
                    'u' => {
                        let mut v = hex4(j)?;
                        j += 4;
                        // A high surrogate followed by an escaped low one is
                        // one character; anything else stays as it is.
                        if (0xd800..0xdc00).contains(&v) && s.get(j) == Some(&'\\') && s.get(j + 1) == Some(&'u') {
                            let low = hex4(j + 2)?;
                            if (0xdc00..0xe000).contains(&low) {
                                v = 0x10000 + ((v - 0xd800) << 10) + (low - 0xdc00);
                                j += 6;
                            }
                        }
                        v
                    }
                    _ => return Err(JsonError::Value),
                });
            }
            // strict=True: no raw control characters.
            c if (c as u32) < 0x20 => return Err(JsonError::Value),
            c => out.push(c as u32),
        }
    }
    *i = j;
    Ok(out)
}

/// null / true / false / NaN / Infinity / -Infinity, or a number.
fn parse_scalar(s: &[char], i: &mut usize) -> Result<Node, JsonError> {
    let starts = |word: &str| word.chars().enumerate().all(|(k, c)| s.get(*i + k) == Some(&c));
    for (word, node) in [
        ("null", Node::Null),
        ("true", Node::Bool(true)),
        ("false", Node::Bool(false)),
        ("NaN", Node::Float(f64::NAN)),
        ("Infinity", Node::Float(f64::INFINITY)),
        ("-Infinity", Node::Float(f64::NEG_INFINITY)),
    ] {
        if starts(word) {
            *i += word.chars().count();
            return Ok(node);
        }
    }
    // -?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][-+]?[0-9]+)?
    let digit = |k: usize| s.get(k).is_some_and(char::is_ascii_digit);
    let start = *i;
    let mut j = start;
    if s.get(j) == Some(&'-') {
        j += 1;
    }
    match s.get(j) {
        Some('0') => j += 1,
        Some('1'..='9') => {
            while digit(j) {
                j += 1;
            }
        }
        _ => return Err(JsonError::Value),
    }
    let mut is_float = false;
    if s.get(j) == Some(&'.') && digit(j + 1) {
        j += 1;
        while digit(j) {
            j += 1;
        }
        is_float = true;
    }
    if matches!(s.get(j), Some('e' | 'E')) {
        let mut k = j + 1;
        if matches!(s.get(k), Some('+' | '-')) {
            k += 1;
        }
        if digit(k) {
            while digit(k) {
                k += 1;
            }
            j = k;
            is_float = true;
        }
    }
    let lit: String = s[start..j].iter().collect();
    *i = j;
    if is_float {
        // float(): correctly rounded, and inf past the largest double --
        // as Rust's parser is.
        return lit.parse::<f64>().map(Node::Float).map_err(|_| JsonError::Value);
    }
    let digits = lit.trim_start_matches('-');
    if digits.len() > INT_MAX_STR_DIGITS {
        return Err(JsonError::Value);
    }
    // int("-0") is 0.
    Ok(Node::Int(if digits == "0" { "0".to_owned() } else { lit }))
}

/// `_load()`: the file as a dict; anything unreadable or not a dict is `{}`.
/// `Err` only for the RecursionError Python does not catch.
fn load() -> Result<Doc, String> {
    // `except (OSError, ValueError)`: a missing file, unreadable bytes (a
    // UnicodeDecodeError is a ValueError) or broken JSON all mean `{}`.
    let Ok(text) = std::fs::read_to_string(path()) else {
        return Ok(Doc::empty());
    };
    match parse(&text) {
        Ok(doc) if matches!(doc.nodes[0], Node::Obj(_)) => Ok(doc),
        Ok(_) | Err(JsonError::Value) => Ok(Doc::empty()),
        Err(JsonError::Recursion(is_obj)) => Err(recursion_message(is_obj)),
    }
}

/// A value exactly as Python's json holds it -- what `get` returns and `set`
/// stores in the Python, where a serde `Value` cannot: a str with a lone
/// surrogate, an int of any size, NaN and infinities.
#[derive(Debug, Clone)]
pub struct Setting {
    doc: Doc,
    root: usize,
}

impl Setting {
    /// `json.loads(text)`: a value given as JSON text, exactly.
    pub fn from_json(text: &str) -> Result<Setting, String> {
        match parse(text) {
            Ok(doc) => Ok(Setting { doc, root: 0 }),
            Err(JsonError::Value) => Err("JSONDecodeError: not a JSON document".into()),
            Err(JsonError::Recursion(is_obj)) => Err(recursion_message(is_obj)),
        }
    }

    /// `json.dumps(value)`: exact, lone surrogates as their `\udXXX` escapes.
    pub fn to_json(&self) -> String {
        self.doc.dumps_from(self.root, None, false)
    }

    /// As a serde value: exact, except what serde cannot hold (a lone
    /// surrogate becomes U+FFFD, NaN/inf null, an int past u64/i64 a float).
    pub fn to_value(&self) -> Value {
        self.doc.to_value(self.root)
    }

    /// The str, when the value is one; lone surrogates as U+FFFD.
    pub fn as_str_lossy(&self) -> Option<String> {
        match &self.doc.nodes[self.root] {
            Node::Str(s) => Some(lossy(s)),
            _ => None,
        }
    }

    fn leaf(node: Node) -> Setting {
        Setting { doc: Doc { nodes: vec![node] }, root: 0 }
    }
}

impl From<Value> for Setting {
    fn from(value: Value) -> Setting {
        let mut doc = Doc { nodes: Vec::new() };
        let root = doc.push_value(&value);
        Setting { doc, root }
    }
}

/// The setting, or its default, exactly as Python returns it (a str setting
/// may hold a lone surrogate). Unknown keys are a programming error
/// (`Err("KeyError: <key>")`, as the Python raises `KeyError`).
pub fn get_exact(key: &str) -> Result<Setting, String> {
    let Some(default) = default_for(key) else {
        return Err(format!("KeyError: {key}"));
    };
    let doc = load()?;
    // A hand-edited file with the wrong type falls back, not through.
    Ok(match (default, doc.get(key)) {
        (DefaultValue::Bool(_), Some(Node::Bool(b))) => Setting::leaf(Node::Bool(*b)),
        (DefaultValue::Str(_), Some(Node::Str(s))) => Setting::leaf(Node::Str(s.clone())),
        (DefaultValue::Bool(b), _) => Setting::leaf(Node::Bool(b)),
        (DefaultValue::Str(s), _) => Setting::leaf(Node::Str(py_str(s))),
    })
}

/// `get_exact` as a serde value: the same, except that a str setting holding
/// a lone surrogate (only a hand-edited file can) comes back with U+FFFD in
/// its place, which a Rust `String` must. Callers that write the value back
/// use `get_exact`.
pub fn get(key: &str) -> Result<Value, String> {
    get_exact(key).map(|v| v.to_value())
}

/// `bool(settings.get(key))` for a bool setting; an unknown key is false.
pub fn get_bool(key: &str) -> bool {
    match get(key) {
        Ok(Value::Bool(b)) => b,
        // `bool()` of a string setting: true when non-empty.
        Ok(Value::String(s)) => !s.is_empty(),
        _ => false,
    }
}

/// `settings.get(key)` for a string setting; an unknown key is "". A lone
/// surrogate comes back as U+FFFD (a `String` cannot hold one); see
/// `get_exact`.
pub fn get_str(key: &str) -> String {
    match get(key) {
        Ok(Value::String(s)) => s,
        _ => String::new(),
    }
}

/// Store one setting: a serde `Value`, or a `Setting` for what a `Value`
/// cannot hold (an int past u64/i64, a lone surrogate). `Err` for an unknown
/// key (Python's KeyError) or when the file cannot be written (OSError).
// noqa: A001 - settings.set reads well
pub fn set(key: &str, value: impl Into<Setting>) -> Result<(), String> {
    if default_for(key).is_none() {
        return Err(format!("KeyError: {key}"));
    }
    let value: Setting = value.into();
    let mut data = load()?;
    data.insert(key, &value);
    let p = path();
    let write = || -> std::io::Result<()> {
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Written by Doc::dumps, not util::write_json: the file may hold
        // values a serde `Value` cannot (see Doc). Same tmp-then-replace.
        let tmp = p.with_extension("tmp");
        std::fs::write(&tmp, data.dumps(Some(2), true))?;
        std::fs::rename(&tmp, &p)
    };
    write().map_err(|e| format!("OSError: {e}"))
}



#[cfg(test)]
fn restore(old: Option<std::ffi::OsString>) {
    match old {
        Some(v) => std::env::set_var("XDG_STATE_HOME", v),
        None => std::env::remove_var("XDG_STATE_HOME"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests share the process environment; one at a time.

    /// `Settings.setUp`/`tearDown`: XDG_STATE_HOME in a temp dir.
    struct Env {
        _guard: std::sync::MutexGuard<'static, ()>,
        _tmp: tempfile::TempDir,
        old: Option<std::ffi::OsString>,
    }

    impl Env {
        fn new() -> Env {
            let guard = crate::util::TEST_ENV.lock().unwrap_or_else(|e| e.into_inner());
            let tmp = tempfile::tempdir().unwrap();
            let old = std::env::var_os("XDG_STATE_HOME");
            std::env::set_var("XDG_STATE_HOME", tmp.path());
            Env { _guard: guard, _tmp: tmp, old }
        }
    }

    impl Drop for Env {
        fn drop(&mut self) {
            restore(self.old.take());
        }
    }

    #[test]
    fn test_defaults_and_a_bad_file() {
        let _env = Env::new();
        assert_eq!(get("net_vendor_icons").unwrap(), Value::Bool(true));
        std::fs::create_dir_all(path().parent().unwrap()).unwrap();
        std::fs::write(path(), r#"{"net_vendor_icons": "nope""#).unwrap(); // broken
        assert_eq!(get("net_vendor_icons").unwrap(), Value::Bool(true));
        std::fs::write(path(), r#"{"net_vendor_icons": "nope"}"#).unwrap(); // wrong type
        assert_eq!(get("net_vendor_icons").unwrap(), Value::Bool(true));
        assert!(get("no_such_setting").is_err());
    }

    /// The settings half of `test_switched_off_network_features_make_no_request`
    /// (the network halves belong to icons, security, ghcache and store).
    #[test]
    fn test_switched_off_network_features_make_no_request() {
        let _env = Env::new();
        for key in ["net_vendor_icons", "net_security_tracker", "net_gh_token",
                    "net_market_omarchy", "net_market_artcraft", "net_market_themes"] {
            set(key, Value::Bool(false)).unwrap();
        }
        for key in ["net_vendor_icons", "net_security_tracker", "net_gh_token",
                    "net_market_omarchy", "net_market_artcraft", "net_market_themes"] {
            assert!(!get_bool(key));
        }
        assert!(get_bool("net_daily_picks"));
    }

    /// The settings half of `test_major_releases_are_held_only_while_set`
    /// (`agents.hold_major` is ported and tested in agents).
    #[test]
    fn test_major_releases_are_held_only_while_set() {
        let _env = Env::new();
        assert!(get_bool("hold_majors"));
        set("hold_majors", Value::Bool(false)).unwrap();
        assert!(!get_bool("hold_majors"));
    }

    /// `set` writes back what Python's json read, untouched: not part of
    /// test_app.py, the review-round cases in one place.
    #[test]
    fn test_set_keeps_values_serde_cannot_hold() {
        let _env = Env::new();
        std::fs::create_dir_all(path().parent().unwrap()).unwrap();
        let deep = format!("{}{}", "[".repeat(300), "]".repeat(300));
        std::fs::write(path(), format!(concat!(
            r#"{{"hold_majors": false, "big": -123456789012345678901234567890, "z": -0, "#,
            r#""f": [1e400, NaN, -Infinity, 2.2250738585072011e-308, 9007199254740993.0], "#,
            r#""s": "\udc00\ud800\ud83d\ude00", "deep": {}}}"#), deep)).unwrap();
        assert_eq!(get("hold_majors").unwrap(), Value::Bool(false));
        set("start_page", Value::String("plugins".into())).unwrap();
        let text = std::fs::read_to_string(path()).unwrap();
        assert!(text.starts_with(concat!(
            "{\n  \"big\": -123456789012345678901234567890,\n  \"deep\": [\n    [\n")));
        assert!(text.contains("\"f\": [\n    Infinity,\n    NaN,\n    -Infinity,\n    2.225073858507201e-308,\n    9007199254740992.0\n  ],"));
        assert!(text.contains("\"s\": \"\\udc00\\ud800\\ud83d\\ude00\","));
        assert!(text.ends_with("\"start_page\": \"plugins\",\n  \"z\": 0\n}"));
        // Past CPython's nesting limit: RecursionError, raised. (Tests run
        // on worker threads.)
        let n = MAX_DEPTH_THREAD + 1;
        std::fs::write(path(), format!("{}{}", "[".repeat(n), "]".repeat(n))).unwrap();
        assert_eq!(get("hold_majors").unwrap_err(), "RecursionError: Stack overflow (used 8156 kB) while decoding a JSON array from a unicode string");
        let n = MAX_DEPTH_THREAD;
        std::fs::write(path(), format!("{}{}", "[".repeat(n), "]".repeat(n))).unwrap();
        assert_eq!(get("hold_majors").unwrap(), Value::Bool(true));
        // More digits than int() takes: a ValueError, so the defaults.
        std::fs::write(path(), format!(r#"{{"hold_majors": false, "n": {}}}"#, "1".repeat(4301))).unwrap();
        assert_eq!(get("hold_majors").unwrap(), Value::Bool(true));
    }

    #[test]
    fn test_set_writes_sorted_indented_json() {
        let _env = Env::new();
        set("start_page", Value::String("plugins".into())).unwrap();
        set("hold_majors", Value::Bool(false)).unwrap();
        assert_eq!(std::fs::read_to_string(path()).unwrap(),
                   "{\n  \"hold_majors\": false,\n  \"start_page\": \"plugins\"\n}");
        assert_eq!(get_str("start_page"), "plugins");
        assert!(set("no_such_setting", Value::Bool(true)).is_err());
    }

    /// What a serde `Value` cannot carry, through `Setting`: a lone
    /// surrogate read back exactly, an int past u64 written exactly.
    #[test]
    fn test_exact_values() {
        let _env = Env::new();
        std::fs::create_dir_all(path().parent().unwrap()).unwrap();
        std::fs::write(path(), r#"{"plugin_scope": "a\udfffb"}"#).unwrap();
        assert_eq!(get_exact("plugin_scope").unwrap().to_json(), r#""a\udfffb""#);
        assert_eq!(get_str("plugin_scope"), "a\u{fffd}b");
        set("plugin_scope", get_exact("plugin_scope").unwrap()).unwrap();
        assert_eq!(std::fs::read_to_string(path()).unwrap(), "{\n  \"plugin_scope\": \"a\\udfffb\"\n}");
        set("start_page", Setting::from_json("18446744073709551616").unwrap()).unwrap();
        assert!(std::fs::read_to_string(path()).unwrap().contains("\"start_page\": 18446744073709551616\n"));
        assert_eq!(get_exact("hold_majors").unwrap().to_json(), "true");
    }
}
