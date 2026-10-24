//! The non-pacman half of the scheduled update (`almanac-autorun`).
//!
//! The non-pacman half of the scheduled update.
//!
//! omarchy-auto-update handles repo and AUR packages, with holds for anything
//! that needs a human. This handles the three kinds of software it cannot see:
//! agent CLIs, tracked AppImages and Omarchy shell plugins.
//!
//! It is a separate program rather than more shell because each of these already
//! has working logic in this app -- agents.detect() knows what is behind,
//! appimage.check() knows what a release contains -- and reimplementing that in
//! bash would give two answers that drift.
//!
//! Run with no arguments it does only what auto-update.json enables.
//!
//! Testing seams: the Python tests `mock.patch` `autorun._run`, `autorun.log`
//! and `agents.detect` / `store.catalog` / `plugins.scope`. Rust has no
//! monkeypatching, so `log` and `_run` consult a thread-local stand-in (set
//! only by the tests and the parity hook), and each `update_*` is split into
//! the part that gathers its inputs from the machine and a `*_from` / `*_in`
//! / `*_with` part that decides and acts on them.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::{agents, appimage, autoupdate, inventory, plugins, store};

/// `__doc__`: argparse's description for `--help`.
const DOC: &str = "The non-pacman half of the scheduled update.

omarchy-auto-update handles repo and AUR packages, with holds for anything
that needs a human. This handles the three kinds of software it cannot see:
agent CLIs, tracked AppImages and Omarchy shell plugins.

It is a separate program rather than more shell because each of these already
has working logic in this app -- agents.detect() knows what is behind,
appimage.check() knows what a release contains -- and reimplementing that in
bash would give two answers that drift.

Run with no arguments it does only what auto-update.json enables.
";

/// `_run`'s stand-in sees the argv as Python passes it: `list(cmd)` of
/// whatever JSON an agent stored, strings or not.
type RunFake = Box<dyn FnMut(&[Value], u64) -> bool>;
type SectionFake = Box<dyn FnMut(&str) -> usize>;

thread_local! {
    /// What this run updated ("wordcraft 0.7.0 → 0.8.0"), for History.
    static UPDATED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    /// Where `log` writes while a test or parity case captures it.
    static LOG_SINK: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
    /// Stand-in for `_run` (the Python tests' `mock.patch.object(autorun, "_run")`).
    static RUN_FAKE: RefCell<Option<RunFake>> = const { RefCell::new(None) };
    /// Stand-in for the three `update_*` sections, for `main`.
    static SECTION_FAKE: RefCell<Option<SectionFake>> = const { RefCell::new(None) };
    /// What the child reads on stdin, when a parity case stands in for the
    /// stdin the process would otherwise pass on (the Python case swaps fd 0).
    static STDIN_FAKE: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Remember one thing this run updated, for the History entry `main` writes.
fn updated(what: String) {
    UPDATED.with(|u| u.borrow_mut().push(what));
}

/// What this run has updated so far, emptying the list.
pub fn take_updated() -> Vec<String> {
    UPDATED.with(|u| std::mem::take(&mut *u.borrow_mut()))
}

/// The History title for a scheduled run that updated `items`
/// ("wordcraft 0.7.0 → 0.8.0", ...); None when it updated nothing.
pub fn run_title(items: &[String]) -> Option<String> {
    let names: Vec<&str> = items.iter().map(|i| i.split(' ').next().unwrap_or(i)).collect();
    let what = match names.as_slice() {
        [] => return None,
        [one] => (*one).to_string(),
        [a, b] => format!("{a} and {b}"),
        [a, b, c] => format!("{a}, {b} and {c}"),
        _ => format!("{} items", names.len()),
    };
    Some(format!("Scheduled update: updated {what}"))
}

/// The History entry for a scheduled run: one line, the details as items.
pub fn record_run(items: &[String]) -> Result<(), String> {
    match run_title(items) {
        Some(title) => crate::history::record_checked(crate::history::AUTOUPDATE, &title, "", "", items),
        None => Ok(()),
    }
}

/// `print(msg, flush=True)`.
pub fn log(msg: &str) {
    // a Rust str holds no surrogate, so no error handler is ever asked
    let _ = log_py(&pystr(msg));
}

/// The error handler Python's stdout encodes with: `PYTHONIOENCODING`'s
/// when it names one; `surrogateescape` in UTF-8 mode and in the C/POSIX
/// locale (which Python coerces to C.UTF-8); `strict` otherwise.
fn stdout_errors() -> String {
    let get = |k: &str| std::env::var_os(k).map(|v| v.to_string_lossy().into_owned()).filter(|v| !v.is_empty());
    if let Some(errors) = get("PYTHONIOENCODING").and_then(|v| v.split_once(':').map(|(_, e)| e.to_string())) {
        if !errors.is_empty() {
            return errors;
        }
    }
    if get("PYTHONUTF8").as_deref() == Some("1") {
        return "surrogateescape".into();
    }
    let ctype = get("LC_ALL").or_else(|| get("LC_CTYPE")).or_else(|| get("LANG"));
    match ctype.as_deref() {
        None | Some("C") | Some("POSIX") => "surrogateescape".into(),
        Some(_) => "strict".into(),
    }
}

/// `print(msg, flush=True)` of a str that may hold surrogates (a plugin
/// directory whose name is not UTF-8, a registry string with a `\udcff`
/// escape). `Err` is the UnicodeEncodeError stdout raises for one it cannot
/// encode -- the default `strict` handler raises for any -- which nothing
/// in the Python catches; nothing is written then.
fn log_py(msg: &[u32]) -> Result<(), String> {
    let captured = LOG_SINK.with(|s| {
        if let Some(lines) = s.borrow_mut().as_mut() {
            lines.push(render(msg));
            true
        } else {
            false
        }
    });
    if captured {
        return Ok(());
    }
    let mut bytes = py_encode(msg, &stdout_errors())?;
    bytes.push(b'\n');
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(&bytes);
    let _ = out.flush();
    Ok(())
}

/// Concatenate pieces of text into one PyStr.
fn cat(parts: &[&[u32]]) -> PyStr {
    parts.concat()
}

#[cfg(test)]
/// Run `f` with `log` captured; returns its result and the lines logged.
fn capture_log<T>(f: impl FnOnce() -> T) -> (T, Vec<String>) {
    let prev = LOG_SINK.with(|s| s.borrow_mut().replace(Vec::new()));
    let out = f();
    let lines = LOG_SINK
        .with(|s| std::mem::replace(&mut *s.borrow_mut(), prev))
        .unwrap_or_default();
    (out, lines)
}

#[cfg(test)]
/// Run `f` with `_run` replaced by `fake`.
fn with_run_fake<T>(fake: RunFake, f: impl FnOnce() -> T) -> T {
    let prev = RUN_FAKE.with(|r| r.borrow_mut().replace(fake));
    let out = f();
    RUN_FAKE.with(|r| *r.borrow_mut() = prev);
    out
}

// ------------------------------------------------------------ Python bits --

/// Python's `str.splitlines()`: every line boundary it knows, not only `\n`.
fn py_splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        let is_break = matches!(
            c,
            '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}'
        );
        if !is_break {
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

// ------------------------------------------------------------- Python str --

/// A Python `str`: code points, so it can hold the lone surrogates a Rust
/// `String` cannot -- what `os.fsdecode` makes of a byte that is not UTF-8
/// (`\xff` is U+DCFF), or a `"\udcff"` escape in a JSON file.
type PyStr = Vec<u32>;

fn pystr(s: &str) -> PyStr {
    s.chars().map(u32::from).collect()
}

fn is_surrogate(c: u32) -> bool {
    (0xd800..0xe000).contains(&c)
}

/// The text, with each lone surrogate written as its `\udcff` escape: what
/// `backslashreplace` (Python's stderr) prints, and how the captured log and
/// the parity results carry one.
fn render(s: &[u32]) -> String {
    let mut out = String::new();
    for &c in s {
        match char::from_u32(c) {
            Some(ch) => out.push(ch),
            None => out.push_str(&format!("\\u{c:04x}")),
        }
    }
    out
}

/// `os.fsdecode(bytes)`: UTF-8, with each byte of an invalid sequence
/// escaped as U+DC00+byte (`surrogateescape`).
fn fsdecode(b: &[u8]) -> PyStr {
    let mut out = PyStr::new();
    let mut rest = b;
    loop {
        match std::str::from_utf8(rest) {
            Ok(s) => {
                out.extend(s.chars().map(u32::from));
                return out;
            }
            Err(e) => {
                let (good, bad) = rest.split_at(e.valid_up_to());
                out.extend(String::from_utf8_lossy(good).chars().map(u32::from));
                let n = e.error_len().unwrap_or(bad.len()).clamp(1, bad.len());
                out.extend(bad[..n].iter().map(|&x| 0xdc00 + x as u32));
                rest = &bad[n..];
            }
        }
    }
}

/// The UnicodeEncodeError the UTF-8 codec raises for the run of surrogates
/// at `start..end` of `s`.
fn encode_error(s: &[u32], start: usize, end: usize) -> String {
    if end - start == 1 {
        format!(
            "UnicodeEncodeError: 'utf-8' codec can't encode character '\\u{:04x}' in position {start}: surrogates not allowed",
            s[start]
        )
    } else {
        format!(
            "UnicodeEncodeError: 'utf-8' codec can't encode characters in position {start}-{}: surrogates not allowed",
            end - 1
        )
    }
}

/// `s.encode("utf-8", errors)`. The codec hands each run of consecutive
/// surrogates to the handler at once; a handler that cannot place one of
/// them (strict, or `surrogateescape` on one that is not an escaped byte)
/// raises for the whole run.
fn py_encode(s: &[u32], errors: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        if let Some(ch) = char::from_u32(s[i]) {
            out.extend_from_slice(ch.encode_utf8(&mut [0u8; 4]).as_bytes());
            i += 1;
            continue;
        }
        let start = i;
        while i < s.len() && is_surrogate(s[i]) {
            i += 1;
        }
        let run = &s[start..i];
        match errors {
            "surrogateescape" if run.iter().all(|c| (0xdc80..=0xdcff).contains(c)) => {
                out.extend(run.iter().map(|c| (c - 0xdc00) as u8));
            }
            "replace" => out.extend(run.iter().map(|_| b'?')),
            "ignore" => {}
            "backslashreplace" | "namereplace" => {
                out.extend(run.iter().flat_map(|c| format!("\\u{c:04x}").into_bytes()));
            }
            "xmlcharrefreplace" => out.extend(run.iter().flat_map(|c| format!("&#{c};").into_bytes())),
            "surrogatepass" => {
                for c in run {
                    out.extend([0xe0 | (c >> 12) as u8, 0x80 | ((c >> 6) & 0x3f) as u8, 0x80 | (c & 0x3f) as u8]);
                }
            }
            _ => return Err(encode_error(s, start, i)),
        }
    }
    Ok(out)
}

/// `os.fsencode(str)`: `surrogateescape` turns an escaped byte back into
/// itself; any other surrogate raises.
fn fsencode(s: &[u32]) -> Result<Vec<u8>, String> {
    py_encode(s, "surrogateescape")
}

/// `str.isprintable()` for one character: everything but the categories
/// Other (Cc, Cf, Cs, Co, Cn) and Separator (Zl, Zp, Zs), space excepted.
fn py_isprintable(c: u32) -> bool {
    static NON: std::sync::LazyLock<Option<regex::Regex>> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^[\p{Cc}\p{Cf}\p{Co}\p{Cn}\p{Zl}\p{Zp}\p{Zs}]$").ok()
    });
    if c == 0x20 {
        return true;
    }
    let Some(ch) = char::from_u32(c) else {
        return false; // a surrogate (Cs)
    };
    match NON.as_ref() {
        Some(re) => !re.is_match(ch.encode_utf8(&mut [0u8; 4])),
        None => true,
    }
}

/// `repr(str)`: single quotes unless the text has one and no double quote;
/// every character that is not printable is escaped.
fn py_repr_py(s: &[u32]) -> String {
    let q = if s.contains(&u32::from('\'')) && !s.contains(&u32::from('"')) { '"' } else { '\'' };
    let mut out = String::new();
    out.push(q);
    for &c in s {
        match char::from_u32(c) {
            Some('\\') => out.push_str("\\\\"),
            Some('\n') => out.push_str("\\n"),
            Some('\r') => out.push_str("\\r"),
            Some('\t') => out.push_str("\\t"),
            Some(ch) if ch == q => {
                out.push('\\');
                out.push(ch);
            }
            Some(ch) if c < 0x7f && c >= 0x20 => out.push(ch),
            Some(ch) if c > 0x7f && py_isprintable(c) => out.push(ch),
            _ if c < 0x100 => out.push_str(&format!("\\x{c:02x}")),
            _ if c < 0x10000 => out.push_str(&format!("\\u{c:04x}")),
            _ => out.push_str(&format!("\\U{c:08x}")),
        }
    }
    out.push(q);
    out
}

/// `repr(str)`.
fn py_repr(s: &str) -> String {
    py_repr_py(&pystr(s))
}

/// `repr()` of a JSON value, as Python prints the dict/list it decoded.
fn py_value_repr(v: &Value) -> String {
    match v {
        Value::String(s) => py_repr(s),
        Value::Array(a) => format!("[{}]", a.iter().map(py_value_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(m) => format!(
            "{{{}}}",
            m.iter()
                .map(|(k, x)| format!("{}: {}", py_repr(k), py_value_repr(x)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => py_str(other),
    }
}

/// `str()` of a JSON value (an f-string's `{x}`).
fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
            (Some(i), _, _) => i.to_string(),
            (None, Some(u), _) => u.to_string(),
            (_, _, Some(f)) => crate::util::py_float_repr(f),
            _ => n.to_string(),
        },
        Value::String(s) => s.clone(),
        other => py_value_repr(other),
    }
}

/// Python truthiness of a JSON value.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
    }
}

/// `str(exc)` for an error the ported modules report as `"Type: message"`
/// (their convention for what the Python raises): the message alone, which
/// is what the f-string prints.
fn py_exc_str(e: &str) -> String {
    if let Some((head, rest)) = e.split_once(": ") {
        let ident = !head.is_empty()
            && head.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && head.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
        if ident && (head.ends_with("Error") || head.ends_with("Exception") || head.ends_with("Expired")) {
            return rest.to_string();
        }
    }
    e.to_string()
}

/// `str(Path(value))` as bytes: pathlib's normalisation (no `.` parts, no
/// repeated or trailing slashes; a leading `//` -- exactly two -- is kept).
fn py_path_bytes(b: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    let root: &[u8] = if b.starts_with(b"//") && !b.starts_with(b"///") {
        b"//"
    } else if b.starts_with(b"/") {
        b"/"
    } else {
        b""
    };
    let parts: Vec<&[u8]> = b.split(|c| *c == b'/').filter(|p| !p.is_empty() && *p != b".").collect();
    let mut out = root.to_vec();
    out.extend(parts.join(&b'/'));
    if out.is_empty() {
        out.push(b'.');
    }
    PathBuf::from(std::ffi::OsStr::from_bytes(&out))
}

fn py_type(v: &Value) -> &'static str {
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

/// `Path(rec["path"])` for a record that is a dict, with the message
/// Python's exception would carry. The str is encoded as the filesystem
/// encoding does (`surrogateescape`); a surrogate that is not an escaped
/// byte is kept as U+FFFD.
fn record_path(reg: &PyJson, rec: usize) -> Result<PathBuf, String> {
    let Some(p) = reg.get(rec, "path") else {
        return Err("KeyError: 'path'".into());
    };
    match reg.node(p) {
        Node::Str(s) => {
            let b = fsencode(s).unwrap_or_else(|_| render_lossy(s).into_bytes());
            Ok(py_path_bytes(&b))
        }
        _ => Err(format!(
            "TypeError: argument should be a str or an os.PathLike object where __fspath__ returns a str, not '{}'",
            reg.type_name(p)
        )),
    }
}

/// The text with each lone surrogate as U+FFFD (what a Rust String can hold).
fn render_lossy(s: &[u32]) -> String {
    s.iter().map(|&c| char::from_u32(c).unwrap_or('\u{fffd}')).collect()
}

// ------------------------------------------------------------ Python json --
//
// The registry is read with `json.loads` and written back with `json.dumps`
// whole, so every value in it must come through as Python's json holds it:
// integers of any size (`-0` is `0`), NaN and Infinity, floats that
// overflow to inf, lone surrogate escapes. serde_json's `Value` can hold
// none of those -- it rejects NaN and 1e400 outright, which made a registry
// Python updates read as `{}` -- so the file goes through a small reader and
// writer that follow CPython's (`json.loads`, `json.dumps(indent=2)`).

/// One value of a parsed document; containers refer to their children by
/// index into `PyJson::nodes`, so neither reading nor dropping a deeply
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

/// A JSON document as Python's json holds it; `nodes[root]` is the top value.
#[derive(Debug, Clone)]
pub struct PyJson {
    nodes: Vec<Node>,
    root: usize,
}

/// Nesting at which CPython 3.14's C json scanner raises RecursionError on
/// the main thread with the default 8 MiB stack (measured for settings.py;
/// the limit is a stack budget, so it moves a little from run to run).
/// `_load_registry` catches only OSError and ValueError, so it ends the run.
const JSON_MAX_DEPTH: usize = 52118;

enum JsonFail {
    /// A ValueError (JSONDecodeError, or an int past 4300 digits).
    Value,
    /// A RecursionError; true when the container that went too deep is a dict.
    Recursion(bool),
}

impl PyJson {
    /// A document holding what a serde value holds.
    pub fn from_value(v: &Value) -> PyJson {
        let mut doc = PyJson { nodes: Vec::new(), root: 0 };
        doc.root = doc.push_value(v);
        doc
    }

    fn node(&self, id: usize) -> &Node {
        &self.nodes[id]
    }

    /// Add a serde value and what is under it; returns its index.
    fn push_value(&mut self, value: &Value) -> usize {
        let node = match value {
            Value::Null => Node::Null,
            Value::Bool(b) => Node::Bool(*b),
            Value::Number(n) => match n.as_f64().filter(|_| n.is_f64()) {
                Some(f) => Node::Float(f),
                None => Node::Int(n.to_string()),
            },
            Value::String(s) => Node::Str(pystr(s)),
            Value::Array(items) => Node::Arr(items.iter().map(|x| self.push_value(x)).collect()),
            Value::Object(map) => Node::Obj(map.iter().map(|(k, x)| (pystr(k), self.push_value(x))).collect()),
        };
        self.nodes.push(node);
        self.nodes.len() - 1
    }

    /// Copy node `root` of `other`, with everything under it; returns its
    /// new index.
    fn graft(&mut self, other: &PyJson, root: usize) -> usize {
        let base = self.nodes.len();
        self.nodes.extend(other.nodes.iter().map(|n| match n {
            Node::Arr(items) => Node::Arr(items.iter().map(|c| c + base).collect()),
            Node::Obj(entries) => Node::Obj(entries.iter().map(|(k, c)| (k.clone(), c + base)).collect()),
            other => other.clone(),
        }));
        root + base
    }

    /// `d.get(key)` on a dict node.
    fn get(&self, id: usize, key: &str) -> Option<usize> {
        let Node::Obj(entries) = &self.nodes[id] else { return None };
        let key = pystr(key);
        entries.iter().find(|(k, _)| *k == key).map(|(_, c)| *c)
    }

    /// `d[key] = value` on a dict node.
    fn set(&mut self, id: usize, key: &str, value: Node) {
        self.nodes.push(value);
        let child = self.nodes.len() - 1;
        let key = pystr(key);
        if let Node::Obj(entries) = &mut self.nodes[id] {
            match entries.iter_mut().find(|(k, _)| *k == key) {
                Some(e) => e.1 = child,
                None => entries.push((key, child)),
            }
        }
    }

    /// `d[key] = value` with a serde value.
    fn set_value(&mut self, id: usize, key: &str, value: &Value) {
        let v = PyJson::from_value(value);
        let child = self.graft(&v, v.root);
        let key = pystr(key);
        if let Node::Obj(entries) = &mut self.nodes[id] {
            match entries.iter_mut().find(|(k, _)| *k == key) {
                Some(e) => e.1 = child,
                None => entries.push((key, child)),
            }
        }
    }

    /// `type(x).__name__`.
    fn type_name(&self, id: usize) -> &'static str {
        match &self.nodes[id] {
            Node::Null => "NoneType",
            Node::Bool(_) => "bool",
            Node::Int(_) => "int",
            Node::Float(_) => "float",
            Node::Str(_) => "str",
            Node::Arr(_) => "list",
            Node::Obj(_) => "dict",
        }
    }

    /// A node's children, for the walks in `util::tree`: none for a
    /// scalar, `Some(is_dict)` for a container.
    fn kids(&self, id: usize, out: &mut Vec<usize>) -> Option<bool> {
        match &self.nodes[id] {
            Node::Arr(items) => {
                out.extend(items);
                Some(false)
            }
            Node::Obj(entries) => {
                out.extend(entries.iter().map(|(_, c)| *c));
                Some(true)
            }
            _ => None,
        }
    }

    /// The keys of a dict node (none for anything else).
    fn keys(&self, id: usize) -> impl Iterator<Item = &PyStr> {
        let entries: &[(PyStr, usize)] = match &self.nodes[id] {
            Node::Obj(entries) => entries,
            _ => &[],
        };
        entries.iter().map(|(k, _)| k)
    }

    /// A scalar as a serde value.
    fn scalar_value(&self, id: usize, text: fn(&[u32]) -> String) -> Value {
        match &self.nodes[id] {
            Node::Null => Value::Null,
            Node::Bool(b) => Value::Bool(*b),
            Node::Int(s) => s
                .parse::<i64>()
                .map(Value::from)
                .or_else(|_| s.parse::<u64>().map(Value::from))
                .unwrap_or_else(|_| float_value(s.parse::<f64>().unwrap_or(f64::NAN))),
            Node::Float(f) => float_value(*f),
            Node::Str(s) => Value::String(text(s)),
            Node::Arr(_) | Node::Obj(_) => Value::Null,
        }
    }

    /// As a serde value, for the calls that take one: what serde cannot
    /// hold degrades (a lone surrogate to U+FFFD, NaN/inf to null, an int
    /// past u64/i64 to the nearest float). Built without recursion: a
    /// registry nests as deep as Python's json goes. A deep result is the
    /// caller's to drop with `util::tree::drop_value`.
    fn to_value(&self, id: usize) -> Value {
        self.value_with(id, render_lossy)
    }


    fn value_with(&self, id: usize, text: fn(&[u32]) -> String) -> Value {
        crate::util::tree::fold_ids(
            id,
            |n, out| self.kids(n, out),
            |n| self.scalar_value(n, text),
            |_, kids| Value::Array(kids),
            |n, kids| Value::Object(self.keys(n).map(|k| text(k)).zip(kids).collect()),
        )
    }

    /// `str(x)` (an f-string's `{x}`); Err is repr's RecursionError.
    fn py_str(&self, id: usize) -> Result<PyStr, String> {
        match &self.nodes[id] {
            Node::Str(s) => Ok(s.clone()),
            _ => self.py_repr(id),
        }
    }

    /// `repr(x)`, without recursion, and with the RecursionError CPython
    /// raises past the C stack it has for it (see `shortcuts::REPR_BUDGET`).
    fn py_repr(&self, id: usize) -> Result<PyStr, String> {
        use crate::shortcuts::{REPR_BUDGET, REPR_DICT_COST, REPR_LIST_COST, REPR_OVERFLOW};
        enum W<'a> {
            Val(usize),
            Lit(&'static str),
            Key(&'a PyStr),
            Exit(u64),
        }
        let mut out = PyStr::new();
        let mut used = 0u64;
        let mut stack = vec![W::Val(id)];
        while let Some(w) = stack.pop() {
            match w {
                W::Lit(s) => out.extend(pystr(s)),
                W::Key(k) => out.extend(pystr(&py_repr_py(k))),
                W::Exit(cost) => used -= cost,
                W::Val(n) => match &self.nodes[n] {
                    Node::Null => out.extend(pystr("None")),
                    Node::Bool(true) => out.extend(pystr("True")),
                    Node::Bool(false) => out.extend(pystr("False")),
                    Node::Int(s) => out.extend(pystr(s)),
                    Node::Float(f) => out.extend(pystr(&py_float_str(*f))),
                    // repr() escapes what is not printable, surrogates included
                    Node::Str(s) => out.extend(pystr(&py_repr_py(s))),
                    Node::Arr(items) if items.is_empty() => out.extend(pystr("[]")),
                    Node::Obj(entries) if entries.is_empty() => out.extend(pystr("{}")),
                    Node::Arr(items) => {
                        used += REPR_LIST_COST;
                        if used > REPR_BUDGET {
                            return Err(format!("RecursionError: {REPR_OVERFLOW}"));
                        }
                        out.extend(pystr("["));
                        stack.push(W::Exit(REPR_LIST_COST));
                        stack.push(W::Lit("]"));
                        for (i, c) in items.iter().enumerate().rev() {
                            stack.push(W::Val(*c));
                            if i > 0 {
                                stack.push(W::Lit(", "));
                            }
                        }
                    }
                    Node::Obj(entries) => {
                        used += REPR_DICT_COST;
                        if used > REPR_BUDGET {
                            return Err(format!("RecursionError: {REPR_OVERFLOW}"));
                        }
                        out.extend(pystr("{"));
                        stack.push(W::Exit(REPR_DICT_COST));
                        stack.push(W::Lit("}"));
                        for (i, (k, c)) in entries.iter().enumerate().rev() {
                            stack.push(W::Val(*c));
                            stack.push(W::Lit(": "));
                            stack.push(W::Key(k));
                            if i > 0 {
                                stack.push(W::Lit(", "));
                            }
                        }
                    }
                },
            }
        }
        Ok(out)
    }

    /// `json.dumps(x, indent=2)`: `ensure_ascii`, NaN/Infinity allowed.
    /// Without recursion (CPython's C encoder takes any depth json.loads
    /// gave it).
    fn dumps_indent2(&self, id: usize) -> String {
        enum W<'a> {
            Val(usize, usize),
            Lit(&'static str),
            Nl(usize),
            Key(&'a PyStr),
        }
        let mut out = String::new();
        let mut stack = vec![W::Val(id, 0)];
        while let Some(w) = stack.pop() {
            match w {
                W::Lit(s) => out.push_str(s),
                W::Nl(level) => {
                    out.push('\n');
                    out.push_str(&" ".repeat(2 * level));
                }
                W::Key(k) => {
                    push_json_str(&mut out, k);
                    out.push_str(": ");
                }
                W::Val(n, level) => match &self.nodes[n] {
                    Node::Null => out.push_str("null"),
                    Node::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
                    Node::Int(s) => out.push_str(s),
                    Node::Float(f) => out.push_str(&crate::util::py_float_repr(*f)),
                    Node::Str(s) => push_json_str(&mut out, s),
                    Node::Arr(items) if items.is_empty() => out.push_str("[]"),
                    Node::Obj(entries) if entries.is_empty() => out.push_str("{}"),
                    Node::Arr(items) => {
                        out.push('[');
                        stack.push(W::Lit("]"));
                        stack.push(W::Nl(level));
                        for (i, c) in items.iter().enumerate().rev() {
                            stack.push(W::Val(*c, level + 1));
                            stack.push(W::Nl(level + 1));
                            if i > 0 {
                                stack.push(W::Lit(","));
                            }
                        }
                    }
                    Node::Obj(entries) => {
                        out.push('{');
                        stack.push(W::Lit("}"));
                        stack.push(W::Nl(level));
                        for (i, (k, c)) in entries.iter().enumerate().rev() {
                            stack.push(W::Val(*c, level + 1));
                            stack.push(W::Key(k));
                            stack.push(W::Nl(level + 1));
                            if i > 0 {
                                stack.push(W::Lit(","));
                            }
                        }
                    }
                },
            }
        }
        out
    }
}

fn float_value(f: f64) -> Value {
    serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number)
}

/// `str(float)`: as repr, with `inf` / `nan` spelled as Python does.
fn py_float_str(f: f64) -> String {
    if f.is_nan() {
        "nan".into()
    } else if f.is_infinite() {
        if f > 0.0 { "inf".into() } else { "-inf".into() }
    } else {
        crate::util::py_float_repr(f)
    }
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
                out.push_str(&format!("\\u{:04x}\\u{:04x}", 0xd800 + (v >> 10), 0xdc00 + (v & 0x3ff)));
            }
            _ => out.push_str(&format!("\\u{c:04x}")),
        }
    }
    out.push('"');
}

/// `json.loads(text)` as CPython's C scanner reads it: whitespace is
/// ` \t\n\r`, NaN / Infinity / -Infinity are numbers, control characters
/// in strings are errors, `\uD8xx\uDCxx` pairs combine and lone surrogate
/// escapes stay, a repeated key keeps its first place and its last value.
fn parse_json(text: &str) -> Result<PyJson, JsonFail> {
    use crate::util::pyjson;
    let mut sink = Nodes(Vec::new());
    match pyjson::loads_into(text, pyjson::Limit::containers(JSON_MAX_DEPTH), &mut sink) {
        Ok(root) => Ok(PyJson { nodes: sink.0, root }),
        Err(pyjson::Error::Value(_)) => Err(JsonFail::Value),
        Err(pyjson::Error::Recursion(is_obj)) => Err(JsonFail::Recursion(is_obj)),
    }
}

/// A parse as the nodes of a [`PyJson`]: each value is its index.
struct Nodes(Vec<Node>);

impl Nodes {
    fn add(&mut self, node: Node) -> usize {
        self.0.push(node);
        self.0.len() - 1
    }
}

impl crate::util::pyjson::Sink for Nodes {
    type Value = usize;
    type Key = PyStr;
    fn reset(&mut self) {
        self.0.clear();
    }
    fn null(&mut self) -> usize {
        self.add(Node::Null)
    }
    fn bool(&mut self, b: bool) -> usize {
        self.add(Node::Bool(b))
    }
    fn num(&mut self, n: crate::util::pyjson::Num<'_>) -> usize {
        use crate::util::pyjson::Num;
        self.add(match n {
            Num::Int(i) => Node::Int(i.to_string()),
            Num::UInt(u) => Node::Int(u.to_string()),
            Num::Big(digits) => Node::Int(digits.to_owned()),
            Num::Float(f) => Node::Float(f),
        })
    }
    fn str(&mut self, s: String) -> usize {
        self.add(Node::Str(s.chars().map(u32::from).collect()))
    }
    fn wstr(&mut self, units: Vec<u32>) -> usize {
        self.add(Node::Str(units))
    }
    fn key(&mut self, s: String) -> PyStr {
        s.chars().map(u32::from).collect()
    }
    fn wkey(&mut self, units: Vec<u32>) -> PyStr {
        units
    }
    fn list(&mut self, items: Vec<usize>) -> usize {
        self.add(Node::Arr(items))
    }
    fn dict(&mut self, pairs: Vec<(PyStr, usize)>) -> usize {
        // A repeated key keeps its place and takes the last value.
        self.add(Node::Obj(crate::util::pyjson::dict_pairs(pairs)))
    }
}

// --------------------------------------------------------------- running --

/// `str(OSError)` for an error the OS reported: `[Errno N] strerror`, then
/// the filename(s) the call was given, as Python's `OSError` prints them.
fn py_os_error(e: &std::io::Error, files: &[&Path]) -> String {
    use std::os::unix::ffi::OsStrExt;
    let files: Vec<PyStr> = files.iter().map(|f| fsdecode(f.as_os_str().as_bytes())).collect();
    py_os_error_py(e, &files)
}

/// `py_os_error`, with the filenames as the Python str they were given as.
fn py_os_error_py(e: &std::io::Error, files: &[PyStr]) -> String {
    let Some(n) = e.raw_os_error() else {
        return e.to_string();
    };
    let text = e.to_string();
    let text = text.split(" (os error").next().unwrap_or(&text).to_string();
    let names: Vec<String> = files.iter().map(|f| py_repr_py(f)).collect();
    match names.as_slice() {
        [] => format!("[Errno {n}] {text}"),
        [a] => format!("[Errno {n}] {text}: {a}"),
        [a, b, ..] => format!("[Errno {n}] {text}: {a} -> {b}"),
    }
}

/// `bytes.decode("utf-8")` (strict), with the UnicodeDecodeError it raises.
fn py_decode(b: Vec<u8>) -> Result<String, String> {
    match String::from_utf8(b) {
        Ok(s) => Ok(s),
        Err(e) => {
            let bytes = e.as_bytes();
            let err = e.utf8_error();
            let start = err.valid_up_to();
            let (end, reason) = match err.error_len() {
                None => (bytes.len(), "unexpected end of data"),
                Some(n) => {
                    let first = bytes[start];
                    let reason = if (0x80..=0xc1).contains(&first) || first >= 0xf5 {
                        "invalid start byte"
                    } else {
                        "invalid continuation byte"
                    };
                    (start + n, reason)
                }
            };
            Err(if end - start > 1 {
                format!("UnicodeDecodeError: 'utf-8' codec can't decode bytes in position {start}-{}: {reason}", end - 1)
            } else {
                format!(
                    "UnicodeDecodeError: 'utf-8' codec can't decode byte 0x{:02x} in position {start}: {reason}",
                    bytes[start]
                )
            })
        }
    }
}

/// What `subprocess.run(argv, capture_output=True, text=True, timeout=...)`
/// gives `_run`. `Ok(Ok(..))`: the return code (negative for a signal),
/// stdout and stderr. `Ok(Err(..))`: the text of the OSError /
/// TimeoutExpired it raised, which `_run` catches. `Err(..)`: an exception
/// `_run` does not catch ("Type: message"), which ends the run: output that
/// is not UTF-8 (`text=True` decodes strictly), a NUL in an argument, an
/// empty argv.
///
/// Not `util::run`: its `Output` keeps neither the errno of a failed start
/// nor the signal that ended a process, and `_run` logs both. The timeout
/// bounds reading the pipes as well as the wait, as `communicate()` does: a
/// grandchild that keeps them open past it is a timeout, not a hang.
type Captured = Result<(i64, String, String), String>;

/// What `os.get_exec_path()` and `_execute_child` try for a program name:
/// the name itself when it has a directory part, else the name joined
/// (`os.path.join`) to each `$PATH` entry in turn (`/bin:/usr/bin` when
/// PATH is unset; an empty entry is the name alone, relative to the cwd).
fn exec_candidates(prog: &[u8]) -> Vec<Vec<u8>> {
    if prog.contains(&b'/') {
        return vec![prog.to_vec()];
    }
    use std::os::unix::ffi::OsStrExt;
    let path = std::env::var_os("PATH");
    let path: &[u8] = path.as_deref().map(|p| p.as_bytes()).unwrap_or(b"/bin:/usr/bin");
    path.split(|c| *c == b':')
        .map(|dir| {
            let mut c = dir.to_vec();
            if !dir.is_empty() && !dir.ends_with(b"/") {
                c.push(b'/');
            }
            c.extend_from_slice(prog);
            c
        })
        .collect()
}

fn run_capture(argv: &[PyStr], timeout: u64) -> Result<Captured, String> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::process::CommandExt;
    let Some(prog) = argv.first() else {
        return Err("IndexError: list index out of range".into());
    };
    // `os.fsencode` of each argument (a surrogate that is not an escaped
    // byte raises), and the NUL check the C side makes.
    let mut bytes: Vec<Vec<u8>> = Vec::new();
    for a in argv {
        let b = fsencode(a)?;
        if b.contains(&0) {
            return Err("ValueError: embedded null byte".into());
        }
        bytes.push(b);
    }
    // The child shares this process's stdin, as Python's does (no stdin=).
    let fed = match STDIN_FAKE.with(|s| s.borrow().clone()) {
        None => None,
        Some(text) => {
            use std::io::{Seek, Write};
            let fed = tempfile::tempfile().and_then(|mut f| {
                f.write_all(text.as_bytes())?;
                f.rewind()?;
                Ok(f)
            });
            match fed {
                Ok(f) => Some(f),
                Err(e) => return Ok(Err(py_os_error(&e, &[]))),
            }
        }
    };
    // close_fds=True (subprocess's default): the child gets fds 0-2 and
    // nothing else this process inherited. Setting close-on-exec here does
    // that without touching them in this process.
    close_fds::set_fds_cloexec_threadsafe(3, &[]);
    // Python execs each candidate in turn and reports the first error that
    // is not ENOENT/ENOTDIR (else the last one), naming the program as
    // given: an empty name searched along PATH reaches `dir/`, a directory
    // (EACCES), where execvp would stop at once with ENOENT.
    let mut saved: Option<i32> = None;
    let mut last = libc::ENOENT;
    let mut child = None;
    for cand in exec_candidates(&bytes[0]) {
        let attempt = if cand.is_empty() {
            Err(std::io::Error::from_raw_os_error(libc::ENOENT))
        } else {
            // a name without a slash (an empty PATH entry) is exec'd from
            // the cwd, not searched for again
            let cand = if cand.contains(&b'/') { cand } else { [b"./".as_slice(), &cand].concat() };
            let stdin = match &fed {
                None => Ok(Stdio::inherit()),
                Some(f) => f.try_clone().map(Stdio::from),
            };
            stdin.and_then(|stdin| {
                crate::util::spawn_group(Command::new(std::ffi::OsStr::from_bytes(&cand))
                    .arg0(std::ffi::OsStr::from_bytes(&bytes[0]))
                    .args(bytes[1..].iter().map(|b| std::ffi::OsStr::from_bytes(b)))
                    .stdin(stdin)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped()))
            })
        };
        match attempt {
            Ok(c) => {
                child = Some(c);
                break;
            }
            Err(e) => match e.raw_os_error() {
                Some(n) => {
                    if saved.is_none() && n != libc::ENOENT && n != libc::ENOTDIR {
                        saved = Some(n);
                    }
                    last = n;
                }
                None => return Ok(Err(e.to_string())),
            },
        }
    }
    let Some(mut child) = child else {
        let e = std::io::Error::from_raw_os_error(saved.unwrap_or(last));
        return Ok(Err(py_os_error_py(&e, std::slice::from_ref(prog))));
    };
    // Drain both pipes on their own threads, or a chatty child fills one and
    // blocks forever while we wait on it. Each reports on the channel when
    // its pipe reaches EOF; one still blocked at the deadline is left behind.
    let (tx, rx) = std::sync::mpsc::channel::<(usize, Vec<u8>)>();
    for (i, pipe) in [
        child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>),
        child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .enumerate()
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut s = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut s);
            }
            let _ = tx.send((i, s));
        });
    }
    drop(tx);
    let start = Instant::now();
    let limit = Duration::from_secs(timeout);
    let mut bufs: [Option<Vec<u8>>; 2] = [None, None];
    let mut status = None;
    loop {
        if status.is_none() {
            match child.try_wait() {
                Ok(Some(st)) => status = Some(st),
                Ok(None) => {}
                Err(e) => {
                    crate::util::kill_group(&mut child);
                    return Ok(Err(py_os_error(&e, &[])));
                }
            }
        }
        if status.is_some() && bufs.iter().all(Option::is_some) {
            break;
        }
        let elapsed = start.elapsed();
        if elapsed >= limit {
            crate::util::kill_group(&mut child);
            let cmd: Vec<String> = argv.iter().map(|a| py_repr_py(a)).collect();
            return Ok(Err(format!(
                "Command '[{}]' timed out after {timeout} seconds",
                cmd.join(", ")
            )));
        }
        let step = (limit - elapsed).min(Duration::from_millis(10));
        match rx.recv_timeout(step) {
            Ok((i, b)) => bufs[i] = Some(b),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                // both readers have reported
                for b in bufs.iter_mut() {
                    b.get_or_insert_with(Vec::new);
                }
            }
        }
    }
    let [out, err] = bufs;
    let stdout = py_decode(out.unwrap_or_default())?;
    let stderr = py_decode(err.unwrap_or_default())?;
    use std::os::unix::process::ExitStatusExt;
    let st = status.unwrap_or_else(|| std::process::ExitStatus::from_raw(0));
    let code = match (st.code(), st.signal()) {
        (Some(c), _) => c as i64,
        (None, Some(sig)) => -(sig as i64),
        (None, None) => -1,
    };
    Ok(Ok((code, stdout, stderr)))
}

/// `_run(argv, timeout=1800)`. `Err` is an exception `_run` lets out
/// ("Type: message"), which ends the whole run in the Python.
pub fn _run(argv: &[String], timeout: u64) -> Result<bool, String> {
    let argv: Vec<PyStr> = argv.iter().map(|a| pystr(a)).collect();
    run_py(&argv, timeout)
}

/// `_run` on the argv as Python would hold it: `' '.join(argv)` raises
/// TypeError on an item that is not a string.
fn run_values(argv: &[Value], timeout: u64) -> Result<bool, String> {
    let faked = RUN_FAKE.with(|r| r.borrow_mut().as_mut().map(|f| f(argv, timeout)));
    if let Some(ok) = faked {
        return Ok(ok);
    }
    let mut strs: Vec<PyStr> = Vec::new();
    for (i, a) in argv.iter().enumerate() {
        match a {
            Value::String(s) => strs.push(pystr(s)),
            other => {
                return Err(format!(
                    "TypeError: sequence item {i}: expected str instance, {} found",
                    py_type(other)
                ))
            }
        }
    }
    run_real(&strs, timeout)
}

/// `_run` on str arguments that may hold surrogate escapes (a plugin id
/// from a directory name that is not UTF-8). The stand-in sees them as
/// `render` writes them.
fn run_py(argv: &[PyStr], timeout: u64) -> Result<bool, String> {
    let faked = RUN_FAKE.with(|r| {
        r.borrow_mut().as_mut().map(|f| {
            let values: Vec<Value> = argv.iter().map(|a| Value::String(render(a))).collect();
            f(&values, timeout)
        })
    });
    if let Some(ok) = faked {
        return Ok(ok);
    }
    run_real(argv, timeout)
}

fn run_real(argv: &[PyStr], timeout: u64) -> Result<bool, String> {
    log_py(&cat(&[&pystr("$ "), &argv.join(&u32::from(' '))]))?;
    let (code, stdout, stderr) = match run_capture(argv, timeout)? {
        Ok(out) => out,
        Err(exc) => {
            log(&format!("  failed to start: {exc}"));
            return Ok(false);
        }
    };
    let lines = py_splitlines(&stdout);
    for line in &lines[lines.len().saturating_sub(20)..] {
        log(&format!("  {line}"));
    }
    if code != 0 {
        let lines = py_splitlines(&stderr);
        for line in &lines[lines.len().saturating_sub(10)..] {
            log(&format!("  {line}"));
        }
        log(&format!("  exited {code}"));
    }
    Ok(code == 0)
}

// ---------------------------------------------------------------- agents --

/// Agent CLIs with a self-updater; returns how many were updated. `Err` is
/// an exception the Python lets out of `update_agents` ("Type: message").
pub fn update_agents() -> Result<usize, String> {
    // `agents.detect()` raising (a latest-versions cache holding a list,
    // say) is not caught: it ends the run.
    update_agents_from(agents::try_detect(true)?)
}

/// `list(cmd)` for the JSON an `update_cmd` holds: the items as they are
/// (a string gives its characters, a dict its keys); a number or a bool is
/// the TypeError `list()` raises.
fn py_list(v: &Value) -> Result<Vec<Value>, String> {
    match v {
        Value::Array(a) => Ok(a.clone()),
        Value::String(s) => Ok(s.chars().map(|c| Value::String(c.to_string())).collect()),
        Value::Object(m) => Ok(m.keys().map(|k| Value::String(k.clone())).collect()),
        other => Err(format!("TypeError: '{}' object is not iterable", py_type(other))),
    }
}

/// `update_agents` on the apps `agents.detect()` returned.
pub fn update_agents_from(apps: Vec<inventory::App>) -> Result<usize, String> {
    let mut done = 0;
    for a in apps {
        if a.update.is_empty() {
            continue;
        }
        let cmd = a.extra.get("update_cmd").filter(|c| truthy(c));
        let Some(cmd) = cmd else {
            log(&format!(
                "{}: {} → {} available, no self-updater",
                a.name, a.version, a.update
            ));
            continue;
        };
        if agents::try_hold_major(&a.version, &a.update)? {
            // A new major version waits for a person: it is the kind of
            // update that changes flags and config formats.
            log(&format!(
                "{}: {} → {} is a major release, held",
                a.name, a.version, a.update
            ));
            continue;
        }
        log(&format!("{}: {} → {}", a.name, a.version, a.update));
        if run_values(&py_list(cmd)?, 1800)? {
            done += 1;
            updated(format!("{} {} → {}", a.name, a.version, a.update));
        }
    }
    if done == 0 {
        log("agents: nothing to update");
    }
    Ok(done)
}

// ------------------------------------------------------------- appimages --

/// Tracked AppImages; returns how many were updated. `Err` is an exception
/// the Python lets out of `update_appimages` ("Type: message").
pub fn update_appimages() -> Result<usize, String> {
    update_appimages_with(
        load_registry()?,
        &mut |_key, rec| check_record(rec),
        &mut |url, dest, sha256| {
            // `Request(url)` reads a non-string url as `str(url)`
            appimage::download_verified(&py_str(url), dest, None, sha256)
                .map(|d| d.checksum_verified)
                .map_err(|e| py_net_error(&e))
        },
        &mut |dest, key| appimage::install_desktop_entry(dest, key).map(|_| ()).map_err(|e| py_net_error(&e)),
        &mut save_updated,
    )
}

/// Record one AppImage `update_appimages` has just updated (to `version`,
/// at `dest`). The registry was read before the downloads, which take
/// minutes; writing that copy back dropped whatever the window had changed
/// meanwhile. Under the registry's lock it is read again and only this
/// entry's version, latest and checked_at change.
///
/// An entry removed meanwhile is not brought back, and the build just
/// downloaded for it is deleted again: that is an error for the log.
fn save_updated(key: &PyStr, version: &Value, dest: &Path) -> Result<(), String> {
    let _lock = inventory::lock_registry().map_err(|e| format!("OSError: {e}"))?;
    let mut reg = load_registry()?;
    let rec = match reg.node(reg.root) {
        Node::Obj(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, c)| *c),
        _ => None,
    };
    let Some(rec) = rec.filter(|&r| matches!(reg.node(r), Node::Obj(_))) else {
        let _ = std::fs::remove_file(dest);
        return Err("RuntimeError: it was removed while it downloaded, so the download was discarded".into());
    };
    mark_updated(&mut reg, rec, version);
    save_registry(&reg)
}

/// `rec['version'] = version; rec['latest'] = {}; rec['checked_at'] = 0`.
fn mark_updated(reg: &mut PyJson, rec: usize, version: &Value) {
    reg.set_value(rec, "version", version);
    reg.set(rec, "latest", Node::Obj(Vec::new()));
    reg.set(rec, "checked_at", Node::Int("0".into()));
}

/// `inventory._load_registry()`: whatever JSON the registry file holds (a
/// list or null as much as a dict), exactly as Python's json reads it, `{}`
/// when it cannot be read or parsed (OSError, ValueError). Not
/// `inventory::load_registry`, which reads anything but an object as `{}`:
/// the Python's `reg.items()` then fails on it. `Err` for the RecursionError
/// a too-deeply nested file raises, which `_load_registry` does not catch.
fn load_registry() -> Result<PyJson, String> {
    let empty = || PyJson::from_value(&json!({}));
    // read_text(): UTF-8, strictly
    let Ok(text) = std::fs::read_to_string(inventory::registry()) else {
        return Ok(empty());
    };
    match parse_json(&text) {
        Ok(doc) => Ok(doc),
        Err(JsonFail::Value) => Ok(empty()),
        Err(JsonFail::Recursion(is_obj)) => Err(format!(
            "RecursionError: Stack overflow (used 8144 kB) while decoding a JSON {} from a unicode string",
            if is_obj { "object" } else { "array" }
        )),
    }
}

/// `inventory.save_registry(reg)`, failing with the text Python's OSError
/// carries (`[Errno 13] Permission denied: '/…/appimages.tmp'`) rather than
/// Rust's, since `update_appimages` logs it. Written as `json.dumps(data,
/// indent=2)` writes it: big ints, NaN and lone surrogates come back out
/// exactly as they were read.
fn save_registry(data: &PyJson) -> Result<(), String> {
    let reg = inventory::registry();
    if let Some(parent) = reg.parent() {
        py_mkdir_parents(parent)?;
    }
    // A name of its own, so no other writer writes into the same file.
    let tmp = inventory::registry_tmp(&reg);
    let text = data.dumps_indent2(data.root) + "\n";
    let written = (|| {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp).map_err(|e| format!("OSError: {}", py_os_error(&e, &[&tmp])))?;
        f.write_all(text.as_bytes()).map_err(|e| format!("OSError: {}", py_os_error(&e, &[])))?;
        // atomic: a crash mid-write cannot truncate it
        std::fs::rename(&tmp, &reg).map_err(|e| format!("OSError: {}", py_os_error(&e, &[&tmp, &reg])))
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// `Path.mkdir(parents=True, exist_ok=True)`, with Python's error text.
fn py_mkdir_parents(p: &Path) -> Result<(), String> {
    match std::fs::create_dir(p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            match p.parent().filter(|q| *q != p && !q.as_os_str().is_empty()) {
                Some(q) => {
                    py_mkdir_parents(q)?;
                    match std::fs::create_dir(p) {
                        Ok(()) => Ok(()),
                        Err(_) if p.is_dir() => Ok(()),
                        Err(e) => Err(format!("OSError: {}", py_os_error(&e, &[p]))),
                    }
                }
                None => Err(format!("OSError: {}", py_os_error(&e, &[p]))),
            }
        }
        Err(_) if p.is_dir() => Ok(()),
        Err(e) => Err(format!("OSError: {}", py_os_error(&e, &[p]))),
    }
}

/// The reason phrase urllib's HTTPError carries (`http.HTTPStatus`).
fn http_reason(code: u16) -> Option<&'static str> {
    Some(match code {
        400 => "Bad Request",
        401 => "Unauthorized",
        402 => "Payment Required",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        406 => "Not Acceptable",
        407 => "Proxy Authentication Required",
        408 => "Request Timeout",
        409 => "Conflict",
        410 => "Gone",
        411 => "Length Required",
        412 => "Precondition Failed",
        413 => "Content Too Large",
        414 => "URI Too Long",
        415 => "Unsupported Media Type",
        416 => "Range Not Satisfiable",
        417 => "Expectation Failed",
        418 => "I'm a Teapot",
        421 => "Misdirected Request",
        422 => "Unprocessable Content",
        423 => "Locked",
        424 => "Failed Dependency",
        425 => "Too Early",
        426 => "Upgrade Required",
        428 => "Precondition Required",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        451 => "Unavailable For Legal Reasons",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        505 => "HTTP Version Not Supported",
        506 => "Variant Also Negotiates",
        507 => "Insufficient Storage",
        508 => "Loop Detected",
        510 => "Not Extended",
        511 => "Network Authentication Required",
        _ => return None,
    })
}

/// Bring an error from `appimage::download` / `install_desktop_entry`
/// closer to the `str(exc)` Python logs: `HTTP Error 404: Not Found`,
/// `<urlopen error …>`, `[Errno 28] No space left on device`. Text already
/// in the Python's form passes through.
fn py_net_error(e: &str) -> String {
    if let Some(rest) = e.strip_prefix("HTTPError: HTTP Error ") {
        if let Ok(code) = rest.parse::<u16>() {
            if let Some(reason) = http_reason(code) {
                return format!("HTTPError: HTTP Error {code}: {reason}");
            }
        }
        return e.to_string();
    }
    if let Some(rest) = e.strip_prefix("URLError: ") {
        if !rest.starts_with("<urlopen error") {
            return format!("URLError: <urlopen error {rest}>");
        }
        return e.to_string();
    }
    // "Message (os error N)", Rust's io::Error text, is "[Errno N] Message"
    if let Some((head, tail)) = e.rsplit_once(" (os error ") {
        if let Some(n) = tail.strip_suffix(')').filter(|n| n.parse::<i32>().is_ok()) {
            let (ty, msg) = match head.split_once(": ") {
                Some((ty, msg)) if ty.ends_with("Error") && !ty.contains(' ') => (format!("{ty}: "), msg),
                _ => (String::new(), head),
            };
            return format!("{ty}[Errno {n}] {msg}");
        }
    }
    e.to_string()
}

/// `appimage.check(record)`: `record.get(...)` on something that is not a
/// dict is the AttributeError Python raises; `{}` when there is nothing newer.
fn check_record(rec: &Value) -> Result<Value, String> {
    if !rec.is_object() {
        return Err(format!("AttributeError: '{}' object has no attribute 'get'", py_type(rec)));
    }
    let latest = appimage::check(rec).map_err(|e| {
        // Python 3.14's pathlib words this TypeError more fully than the
        // appimage port does; the log line is this module's.
        match e.strip_prefix("TypeError: argument should be a str or an os.PathLike object, not '") {
            Some(rest) => format!(
                "TypeError: argument should be a str or an os.PathLike object where __fspath__ returns a str, not '{rest}"
            ),
            None => e,
        }
    })?;
    Ok(match latest {
        Some(l) => serde_json::to_value(l).unwrap_or_else(|_| json!({})),
        None => json!({}),
    })
}

/// `update_appimages` with the registry and the four calls it makes passed
/// in (`check(key, record)` giving the dict `appimage.check` returns,
/// `download(url, dest, sha256)`, true when the download was checked against
/// a published sha256, `install_desktop_entry(dest, key)`, and
/// `save(key, version, dest)`, which records one entry's update, see
/// `save_updated`). Errors are the `"Type: message"` strings the ported
/// modules return; `Err` is an exception the Python lets out.
pub fn update_appimages_with(
    mut reg: PyJson,
    check: &mut dyn FnMut(&str, &Value) -> Result<Value, String>,
    download: &mut dyn FnMut(&Value, &Path, &str) -> Result<bool, String>,
    install: &mut dyn FnMut(&Path, &str) -> Result<(), String>,
    save: &mut dyn FnMut(&PyStr, &Value, &Path) -> Result<(), String>,
) -> Result<usize, String> {
    let entries = match reg.node(reg.root) {
        Node::Obj(e) => e.clone(),
        _ => {
            return Err(format!(
                "AttributeError: '{}' object has no attribute 'items'",
                reg.type_name(reg.root)
            ))
        }
    };
    let mut done = 0;
    for (key, rec) in entries {
        // the stand-ins and install_desktop_entry take the key as a Rust str
        let key_s = render(&key);
        let rec_value = reg.to_value(rec);
        let checked = check(&key_s, &rec_value);
        crate::util::tree::drop_value(rec_value);
        let latest = match checked {
            Ok(l) => l,
            Err(exc) => {
                log_py(&cat(&[&key, &pystr(&format!(": check failed: {}", py_exc_str(&exc)))]))?;
                continue;
            }
        };
        // `latest.get("url")`, `rec.get('version')` and `latest['version']`
        // sit outside the try: a failure there ends the run.
        let Value::Object(latest) = latest else {
            return Err(format!("AttributeError: '{}' object has no attribute 'get'", py_type(&latest)));
        };
        let url = latest.get("url").cloned().unwrap_or(Value::Null);
        if !truthy(&url) {
            continue;
        }
        if !matches!(reg.node(rec), Node::Obj(_)) {
            return Err(format!("AttributeError: '{}' object has no attribute 'get'", reg.type_name(rec)));
        }
        let have = match reg.get(rec, "version") {
            // repr of a value nested past its stack: RecursionError
            Some(v) => reg.py_str(v)?,
            None => pystr("None"),
        };
        let Some(version) = latest.get("version").cloned() else {
            return Err("KeyError: 'version'".into());
        };
        log_py(&cat(&[&key, &pystr(": "), &have, &pystr(&format!(" → {}", py_str(&version)))]))?;
        // Checked against the asset's published sha256 when it has one.
        let sha256 = latest.get("sha256").and_then(Value::as_str).unwrap_or_default();
        let attempt = (|| -> Result<(), String> {
            let dest = record_path(&reg, rec)?;
            // Without a digest the build is only as trusted as TLS and the
            // ELF check: the log says so rather than passing it over.
            if !download(&url, &dest, sha256)? {
                log("  unverified (no checksum published)");
            }
            mark_updated(&mut reg, rec, &version);
            save(&key, &version, &dest)?;
            install(&dest, &key_s)?;
            Ok(())
        })();
        match attempt {
            Ok(()) => {
                done += 1;
                log(&format!("  installed {}", py_str(&version)));
                updated(format!("{} {} → {}", key_s, render(&have), py_str(&version)));
            }
            Err(exc) => {
                // download() swaps atomically, so a failure here leaves the
                // working build in place rather than a half-written file.
                log(&format!("  failed: {}", py_exc_str(&exc)));
            }
        }
    }
    if done == 0 {
        log("appimages: nothing to update");
    }
    Ok(done)
}

// --------------------------------------------------------------- plugins --

/// Omarchy shell plugins. Off by default: these change the desktop.
///
/// One at a time, and only those plugins.py classifies as a clean update
/// inside the chosen scope: `omarchy plugin update` with no id would also
/// fast-forward a symlinked development plugin's workspace.
pub fn update_plugins() -> Result<usize, String> {
    let trust = catalog_trust().unwrap_or_default();
    let scope = plugins::scope();
    update_plugins_in(&plugins::plugins_dir(), &scope, &trust)
}

/// `{p.id: p.trust.key for p in store.catalog()}`, failing where the
/// Python raises -- `catalog()` itself (a market whose load raises, a row
/// `Plugin(**r)` rejects), or `p.trust` on a field of the wrong type -- so
/// the caller's `except Exception: trust = {}` holds every plugin back, as
/// the Python does. (`store::catalog` instead skips the market or the row
/// and keeps the rest, and `Plugin::trust` cannot fail.)
fn catalog_trust() -> Result<BTreeMap<String, plugins::Listing>, String> {
    trust_map(store::try_catalog(false)?)
}

/// The dict comprehension over the plugins: the key (`p.id`) must be
/// hashable; an id that is not a str is kept by the Python but can never
/// equal a plugin directory's name, so it is left out here. Each id keeps
/// its listing's repository and reviewed commit beside the key: the key only
/// counts for a checkout of that repository (`plugins::listed_trust`).
fn trust_map(plugins: Vec<store::Plugin>) -> Result<BTreeMap<String, plugins::Listing>, String> {
    let mut out = BTreeMap::new();
    for p in plugins {
        // the fields as the Python object holds them, wrong types included
        let v = serde_json::to_value(&p).map_err(|e| format!("TypeError: {e}"))?;
        let id = v.get("id").cloned().unwrap_or(Value::Null);
        if matches!(id, Value::Array(_) | Value::Object(_)) {
            return Err(format!("TypeError: unhashable type: '{}'", py_type(&id)));
        }
        let key = trust_key(&v)?;
        let text = |f: &str| v.get(f).and_then(Value::as_str).unwrap_or_default().to_string();
        if let Value::String(id) = id {
            out.insert(id, plugins::Listing { key, repo: text("repo"), validated_commit: text("validated_commit") });
        }
    }
    Ok(out)
}

/// `p.trust.key`, following store.py's `trust` property branch by branch
/// on the raw field values, and raising where it raises: `MARKETS.get` of
/// an unhashable market, `self.repo.split` on a repo that is not a str,
/// `x[:n]` on a value that cannot be sliced (`self.upstream_checked_at[:10]`
/// on null, say).
fn trust_key(v: &Value) -> Result<String, String> {
    let null = Value::Null;
    let f = |k: &str| v.get(k).unwrap_or(&null);
    let slice = |k: &str| -> Result<(), String> {
        match f(k) {
            Value::String(_) | Value::Array(_) => Ok(()),
            Value::Object(_) => Err("KeyError: slice(None, 10, None)".into()),
            other => Err(format!("TypeError: '{}' object is not subscriptable", py_type(other))),
        }
    };
    let market = f("market");
    let hashable = || -> Result<(), String> {
        match market {
            Value::Array(_) | Value::Object(_) => Err(format!("TypeError: unhashable type: '{}'", py_type(market))),
            _ => Ok(()),
        }
    };
    let is = |k: &str, want: &[&str]| matches!(f(k), Value::String(s) if want.contains(&s.as_str()));
    let drifted = truthy(f("validated_commit")) && truthy(f("upstream_commit")) && f("validated_commit") != f("upstream_commit");
    if truthy(f("built_in")) {
        hashable()?; // self.market_name
        return Ok("official".into());
    }
    hashable()?;
    let m = market.as_str().and_then(store::market);
    if let Some(m) = m.filter(|m| !m.first_party_owner.is_empty()) {
        // self.owner: [x for x in self.repo.split("/") if x][2]
        let Value::String(repo) = f("repo") else {
            return Err(format!("AttributeError: '{}' object has no attribute 'split'", py_type(f("repo"))));
        };
        let parts: Vec<&str> = repo.split('/').filter(|x| !x.is_empty()).collect();
        let owner = if parts.len() > 2 { parts[2] } else { "" };
        let host = if parts.len() > 1 { parts[1] } else { "" };
        if owner == m.first_party_owner && host.eq_ignore_ascii_case("github.com") {
            return Ok("first-party".into());
        }
    }
    if is("upstream_status", &["failed", "unreachable"]) {
        slice("upstream_checked_at")?;
        return Ok("unreachable".into());
    }
    if is("review_method", &["maintainer-reviewed"]) {
        if truthy(f("reviewed_at")) {
            slice("reviewed_at")?;
        }
        return Ok(if drifted { "reviewed-stale" } else { "reviewed" }.into());
    }
    if is("coverage", &["snapshot-verified"]) && !drifted {
        slice("validated_commit")?;
        return Ok("verified".into());
    }
    if is("coverage", &["snapshot-verified", "update-unverified"]) {
        slice("validated_commit")?;
        slice("upstream_commit")?;
        return Ok("stale".into());
    }
    Ok("unverified".into())
}

/// `sorted(pdir.glob("*"))`: every entry, dotfiles included, by name as
/// Python's str compares it (code points, a byte that is not UTF-8 as its
/// surrogate escape).
fn glob_star(pdir: &Path) -> Vec<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let mut out: Vec<PathBuf> = match std::fs::read_dir(pdir) {
        Ok(rd) => rd.filter_map(|e| e.ok()).map(|e| e.path()).collect(),
        Err(_) => Vec::new(),
    };
    out.sort_by_cached_key(|p| fsdecode(p.as_os_str().as_bytes()));
    out
}

/// `plugins.classify(d, {})`. The port's classify hands git the path as
/// UTF-8 text, which a path that is not UTF-8 cannot be (Python passes its
/// bytes): such a directory is classified from inside it, as `.`, with the
/// working directory put back afterwards. A symlink is LOCAL either way.
fn classify(d: &Path) -> plugins::State {
    let mut cache = plugins::FetchCache::new();
    if d.to_str().is_some() {
        return plugins::classify(d, Some(&mut cache), true);
    }
    if d.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false) {
        return plugins::State::new("", plugins::LOCAL, "symlink", 0, "");
    }
    let Ok(prev) = std::env::current_dir() else {
        return plugins::classify(d, Some(&mut cache), true);
    };
    if std::env::set_current_dir(d).is_err() {
        return plugins::classify(d, Some(&mut cache), true);
    }
    let st = plugins::classify(Path::new("."), Some(&mut cache), true);
    let _ = std::env::set_current_dir(&prev);
    st
}

/// `update_plugins` with the plugins directory, the scope and the
/// marketplace's trust keys (`{id: trust.key}`) passed in.
pub fn update_plugins_in(pdir: &Path, scope: &str, trust: &BTreeMap<String, plugins::Listing>) -> Result<usize, String> {
    use std::os::unix::ffi::OsStrExt;
    let mut done = 0;
    let dirs = if pdir.is_dir() { glob_star(pdir) } else { Vec::new() };
    for d in dirs {
        if !d.join("manifest.json").is_file() {
            continue;
        }
        let st = classify(&d);
        if !st.updatable() {
            continue;
        }
        // `st.id` is `d.name`. A name that is not UTF-8 keeps its bytes as
        // surrogate escapes, as Python's does; the port's State holds it
        // lossily, so it is taken from the path here.
        let id: PyStr = d.file_name().map(|n| fsdecode(n.as_bytes())).unwrap_or_default();
        let key = String::from_utf8(d.file_name().map(|n| n.as_bytes().to_vec()).unwrap_or_default()).ok();
        let listing = key.as_ref().and_then(|k| trust.get(k));
        let trust_key = plugins::listed_trust(&st.url, listing);
        if !plugins::in_scope(trust_key, scope) {
            log_py(&cat(&[&pystr("plugins: "), &id, &pystr(&format!(" held back by the update scope ({scope})"))]))?;
            continue;
        }
        if let Some(why) = plugins::pin_hold(&st, listing.filter(|_| trust_key.is_some()), scope) {
            log_py(&cat(&[&pystr("plugins: "), &id, &pystr(&format!(" held back: {why}"))]))?;
            continue;
        }
        log_py(&cat(&[&pystr("plugins: "), &id, &pystr(&format!(" ({} commits)", st.behind))]))?;
        // plugins.update_argv(st.id), with the id as Python holds it
        const ID: &str = "\u{0}almanac-plugin-id\u{0}";
        let argv: Vec<PyStr> = plugins::update_argv(ID)
            .iter()
            .map(|a| if a == ID { id.clone() } else { pystr(a) })
            .collect();
        if run_py(&argv, 300)? {
            done += 1;
            updated(format!("{} ({} commits)", render(&id), st.behind));
            // `omarchy plugin update` takes no commit: upstream may have
            // moved again between the check and the pull.
            let pinned = scope == "verified" && listing.is_some_and(|l| !l.validated_commit.trim().is_empty());
            if pinned && !plugins::checkout_head(&d).eq_ignore_ascii_case(&st.remote) {
                log_py(&cat(&[&pystr("plugins: "), &id, &pystr(" is past the reviewed commit: upstream changed during the update")]))?;
            }
        }
    }
    if done == 0 {
        log("plugins: nothing to update");
    }
    Ok(done)
}

// -------------------------------------------------------------- argparse --

const OPTIONS: &[&str] = &["-h", "--help", "--agents", "--appimages", "--plugins"];

/// What `ap.parse_args(argv)` came to.
#[derive(Debug, Clone, PartialEq)]
enum Parsed {
    Flags {
        agents: bool,
        appimages: bool,
        plugins: bool,
    },
    /// `--help`: this text on stdout, exit 0.
    Help,
    /// An error: the message after "error: ", exit 2. Arguments in it are
    /// written as `render` writes them (stderr's `backslashreplace`).
    Error(String),
}

/// The option an exact option string names.
fn dest_of(opt: &str) -> &'static str {
    match opt {
        "-h" | "--help" => "help",
        "--agents" => "agents",
        "--appimages" => "appimages",
        _ => "plugins",
    }
}

/// argparse's name for an option in "argument X: ..." messages.
fn arg_name(dest: &str) -> &'static str {
    match dest {
        "help" => "-h/--help",
        "agents" => "--agents",
        "appimages" => "--appimages",
        _ => "--plugins",
    }
}

fn ignored(dest: &str, explicit: &[u32]) -> Parsed {
    Parsed::Error(format!(
        "argument {}: ignored explicit argument {}",
        arg_name(dest),
        py_repr_py(explicit)
    ))
}

/// The option string `arg` is exactly, if any.
fn option_named(arg: &[u32]) -> Option<&'static str> {
    OPTIONS.iter().copied().find(|o| pystr(o) == arg)
}

/// `arg.split('=', 1)`.
fn split_eq(arg: &[u32]) -> Option<(&[u32], &[u32])> {
    let i = arg.iter().position(|c| *c == u32::from('='))?;
    Some((&arg[..i], &arg[i + 1..]))
}

/// `ArgumentParser.parse_args` (Python 3.14) for `-h/--help`, `--agents`,
/// `--appimages`, `--plugins`: unique prefixes of a long option are
/// accepted, `--x=v` is an error for these flags, and anything else is
/// "unrecognized". Options act in order, so `--help` wins over what follows.
/// The arguments are Python strs: one that is not UTF-8 holds surrogate
/// escapes, which never match an option.
fn parse_args(argv: &[PyStr]) -> Parsed {
    let (mut agents, mut appimages, mut plugins) = (false, false, false);
    let mut extras: Vec<&[u32]> = Vec::new();
    let mut rest = false;
    let dash = u32::from('-');
    for arg in argv {
        let arg = arg.as_slice();
        if rest {
            extras.push(arg);
            continue;
        }
        if arg == pystr("--").as_slice() {
            // Everything after `--` is positional, and there are none.
            rest = true;
            extras.push(arg);
            continue;
        }
        if arg.first() != Some(&dash) || arg.len() == 1 {
            extras.push(arg);
            continue;
        }
        // (action, explicit argument) for this option string
        let (dest, sep, explicit): (&str, Option<&str>, Option<PyStr>) = if let Some(o) = option_named(arg) {
            (dest_of(o), None, None)
        } else if let Some((o, val)) = split_eq(arg).and_then(|(o, v)| option_named(o).map(|o| (o, v))) {
            (dest_of(o), Some("="), Some(val.to_vec()))
        } else if arg.starts_with(&[dash, dash]) {
            let (prefix, val) = match split_eq(arg) {
                Some((p, v)) => (p, Some(v.to_vec())),
                None => (arg, None),
            };
            let matches: Vec<&str> = OPTIONS.iter().copied().filter(|o| pystr(o).starts_with(prefix)).collect();
            match matches.len() {
                0 => {
                    extras.push(arg);
                    continue;
                }
                1 => (dest_of(matches[0]), val.as_ref().map(|_| "="), val),
                _ => {
                    return Parsed::Error(format!("ambiguous option: {} could match {}", render(arg), matches.join(", ")))
                }
            }
        } else if arg.starts_with(&pystr("-h")) {
            // A single-dash cluster: `-h` then more short flags.
            (dest_of("-h"), Some(""), Some(arg[2..].to_vec()))
        } else {
            extras.push(arg);
            continue;
        };
        match (dest, explicit) {
            (d, None) => match d {
                "help" => return Parsed::Help,
                "agents" => agents = true,
                "appimages" => appimages = true,
                _ => plugins = true,
            },
            ("help", Some(mut ex)) if sep == Some("") => {
                // `-hX...`: each following letter is another short flag;
                // only -h exists, so this is help or an error.
                loop {
                    if ex.is_empty() {
                        return Parsed::Help;
                    }
                    if ex[0] == dash {
                        return ignored("help", &ex);
                    }
                    if ex[0] != u32::from('h') {
                        // an unknown short flag: an extra, but help runs first
                        return Parsed::Help;
                    }
                    let tail = ex[1..].to_vec();
                    if tail.is_empty() {
                        return Parsed::Help;
                    }
                    if tail[0] == u32::from('=') {
                        return ignored("help", &tail[1..]);
                    }
                    ex = tail;
                }
            }
            (d, Some(ex)) => return ignored(d, &ex),
        }
    }
    if !extras.is_empty() {
        let shown: Vec<String> = extras.iter().map(|a| render(a)).collect();
        return Parsed::Error(format!("unrecognized arguments: {}", shown.join(" ")));
    }
    Parsed::Flags {
        agents,
        appimages,
        plugins,
    }
}

/// `str.isspace()`.
fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// `int(text)`: surrounding whitespace, a sign, `_` between digits and any
/// Unicode decimal digits are accepted. Out-of-range values saturate.
fn py_int(text: &str) -> Option<i64> {
    // _PyUnicode_TransformDecimalAndSpaceToASCII, then PyLong_FromString
    let mut ascii = String::new();
    for c in text.chars() {
        if py_isspace(c) {
            ascii.push(' ');
        } else if c.is_ascii() {
            ascii.push(c);
        } else if let Some(d) = crate::util::decimal_value(c) {
            ascii.push(char::from_digit(d, 10)?);
        } else {
            return None;
        }
    }
    let s = ascii.trim_matches(|c: char| " \t\n\x0b\x0c\r".contains(c));
    let (neg, digits) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let b = digits.as_bytes();
    if b.is_empty() || !b[0].is_ascii_digit() || !b[b.len() - 1].is_ascii_digit() {
        return None;
    }
    let mut v: i128 = 0;
    let mut count = 0;
    let mut prev_us = false;
    for &ch in b {
        if ch == b'_' {
            if prev_us {
                return None;
            }
            prev_us = true;
            continue;
        }
        if !ch.is_ascii_digit() {
            return None;
        }
        prev_us = false;
        count += 1;
        v = (v * 10 + (ch - b'0') as i128).min(i64::MAX as i128 + 1);
    }
    // sys.int_info.default_max_str_digits
    if count > 4300 {
        return None;
    }
    let v = if neg { -v } else { v };
    Some(v.clamp(i64::MIN as i128, i64::MAX as i128) as i64)
}

/// `shutil.get_terminal_size().columns`: `$COLUMNS` when it is a positive
/// number; otherwise (or when `$LINES` is not) the terminal on stdout is
/// asked, and 80 is the fallback.
fn columns() -> i64 {
    let env = |k: &str| std::env::var_os(k).and_then(|v| v.to_str().and_then(py_int)).unwrap_or(0);
    let mut columns = env("COLUMNS");
    let lines = env("LINES");
    if columns <= 0 || lines <= 0 {
        let size = rustix::termios::tcgetwinsize(std::io::stdout()).map(|w| w.ws_col as i64).unwrap_or(80);
        if columns <= 0 {
            columns = if size == 0 { 80 } else { size };
        }
    }
    columns
}

/// The colours Python 3.14's argparse uses (`_colorize.Argparse`), or none.
#[derive(Clone, Copy)]
struct Theme {
    usage: &'static str,
    prog: &'static str,
    heading: &'static str,
    summary_long_option: &'static str,
    summary_short_option: &'static str,
    long_option: &'static str,
    short_option: &'static str,
    reset: &'static str,
}

const COLOR: Theme = Theme {
    usage: "\x1b[1;34m",
    prog: "\x1b[1;35m",
    heading: "\x1b[1;34m",
    summary_long_option: "\x1b[36m",
    summary_short_option: "\x1b[32m",
    long_option: "\x1b[1;36m",
    short_option: "\x1b[1;32m",
    reset: "\x1b[0m",
};

const NO_COLOR: Theme = Theme {
    usage: "",
    prog: "",
    heading: "",
    summary_long_option: "",
    summary_short_option: "",
    long_option: "",
    short_option: "",
    reset: "",
};

/// `_colorize.can_colorize()` for stdout, which argparse (`color=True`, the
/// 3.14 default) consults for help and usage errors alike.
fn can_colorize() -> bool {
    let get = |k: &str| std::env::var_os(k);
    match get("PYTHON_COLORS").as_deref().and_then(|v| v.to_str()) {
        Some("0") => return false,
        Some("1") => return true,
        _ => {}
    }
    if get("NO_COLOR").is_some_and(|v| !v.is_empty()) {
        return false;
    }
    if get("FORCE_COLOR").is_some_and(|v| !v.is_empty()) {
        return true;
    }
    if get("TERM").as_deref().and_then(|v| v.to_str()) == Some("dumb") {
        return false;
    }
    use std::io::IsTerminal;
    std::io::stdout().is_terminal()
}

/// `_colorize.decolor`.
fn decolor(s: &str) -> String {
    let mut out = s.to_string();
    for t in [COLOR.usage, COLOR.prog, COLOR.summary_long_option, COLOR.summary_short_option, COLOR.long_option, COLOR.short_option, COLOR.reset] {
        out = out.replace(t, "");
    }
    out
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `[^\d\W]`
fn is_letter(c: char) -> bool {
    is_word(c) && !c.is_numeric()
}

/// `[\w!"'&.,?]`
fn is_word_punct(c: char) -> bool {
    is_word(c) || "!\"'&.,?".contains(c)
}

/// `TextWrapper.wordsep_re` splitting a whitespace-normalised text.
fn tw_chunks(text: &str) -> Vec<String> {
    let t: Vec<char> = text.chars().collect();
    let n = t.len();
    let at = |i: usize| -> Option<char> { t.get(i).copied() };
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < n {
        if t[pos].is_whitespace() {
            let mut e = pos;
            while e < n && t[e].is_whitespace() {
                e += 1;
            }
            out.push(t[pos..e].iter().collect());
            pos = e;
            continue;
        }
        // em-dash between words: (?<=wp)-{2,}(?=\w)
        if pos > 0 && is_word_punct(t[pos - 1]) && t[pos] == '-' {
            let mut e = pos;
            while e < n && t[e] == '-' {
                e += 1;
            }
            if e - pos >= 2 && at(e).is_some_and(is_word) {
                out.push(t[pos..e].iter().collect());
                pos = e;
                continue;
            }
        }
        // a word, possibly hyphenated: the shortest run that ends at a
        // breakable hyphen, before whitespace / the end, or before an em-dash
        let mut e = pos + 1;
        loop {
            if e >= n || t[e].is_whitespace() {
                break;
            }
            let j = e - 1;
            if t[j] == '-' && j > pos {
                let behind = (j >= 2 && is_letter(t[j - 1]) && is_letter(t[j - 2]))
                    || (j >= 3 && is_letter(t[j - 1]) && t[j - 2] == '-' && is_letter(t[j - 3]));
                let ahead = at(e).is_some_and(is_letter)
                    && (at(e + 1).is_some_and(is_letter)
                        || (at(e + 1) == Some('-') && at(e + 2).is_some_and(is_letter)));
                if behind && ahead {
                    break;
                }
            }
            if is_word_punct(t[e - 1]) && t[e] == '-' {
                let mut k = e;
                while k < n && t[k] == '-' {
                    k += 1;
                }
                if k - e >= 2 && at(k).is_some_and(is_word) {
                    break;
                }
            }
            e += 1;
        }
        out.push(t[pos..e].iter().collect());
        pos = e;
    }
    out
}

/// `textwrap.wrap(text, width)` with the defaults argparse uses.
fn tw_wrap(text: &str, width: i64) -> Vec<String> {
    let mut chunks: Vec<String> = tw_chunks(text);
    chunks.reverse();
    let len = |s: &str| s.chars().count() as i64;
    let mut lines: Vec<String> = Vec::new();
    while !chunks.is_empty() {
        let mut cur: Vec<String> = Vec::new();
        let mut cur_len = 0i64;
        if chunks.last().is_some_and(|c| c.trim().is_empty()) && !lines.is_empty() {
            chunks.pop();
        }
        while let Some(c) = chunks.last() {
            let l = len(c);
            if cur_len + l <= width {
                cur_len += l;
                cur.push(chunks.pop().unwrap_or_default());
            } else {
                break;
            }
        }
        if chunks.last().is_some_and(|c| len(c) > width) {
            // _handle_long_word
            let space_left = if width < 1 { 1 } else { width - cur_len };
            let chunk: Vec<char> = chunks.last().map(|c| c.chars().collect()).unwrap_or_default();
            if space_left <= 0 {
                // no room left on this line: a word alone on one is put
                // there whole, otherwise it starts the next line
                if cur.is_empty() {
                    cur.push(chunks.pop().unwrap_or_default());
                }
            } else {
                let mut end = space_left as usize;
                if chunk.len() > end {
                    if let Some(h) = chunk[..end].iter().rposition(|c| *c == '-') {
                        if h > 0 && chunk[..h].iter().any(|c| *c != '-') {
                            end = h + 1;
                        }
                    }
                }
                let end = end.min(chunk.len());
                cur.push(chunk[..end].iter().collect());
                if let Some(last) = chunks.last_mut() {
                    *last = chunk[end..].iter().collect();
                }
            }
        }
        if cur.last().is_some_and(|c| c.trim().is_empty()) {
            cur.pop();
        }
        if !cur.is_empty() {
            lines.push(cur.concat());
        }
    }
    lines
}

/// `re.sub(r'\s+', ' ', text).strip()` (ASCII whitespace).
fn collapse_ws(text: &str) -> String {
    text.split(|c: char| c.is_ascii_whitespace())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// argparse's usage block, without the trailing newline(s).
fn format_usage(prog: &str, width: i64, t: &Theme) -> String {
    let prefix = "usage: ";
    let opt = |color: &str, s: &str| format!("[{color}{s}{}]", t.reset);
    let opt_parts_owned = [
        opt(t.summary_short_option, "-h"),
        opt(t.summary_long_option, "--agents"),
        opt(t.summary_long_option, "--appimages"),
        opt(t.summary_long_option, "--plugins"),
    ];
    let opt_parts: Vec<&str> = opt_parts_owned.iter().map(String::as_str).collect();
    let usage = format!("{prog} {}", opt_parts.join(" "));
    let len = |s: &str| decolor(s).chars().count() as i64;
    let text_width = width;
    let done = |usage: &str| {
        let rest = usage.strip_prefix(prog).unwrap_or(usage);
        format!("{}{prefix}{}{}{}{}{rest}", t.usage, t.reset, t.prog, prog, t.reset)
    };
    if len(prefix) + len(&usage) <= text_width {
        return done(&usage);
    }
    let get_lines = |parts: &[&str], indent: &str, prefix: Option<&str>| -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        let mut line: Vec<&str> = Vec::new();
        let indent_length = indent.chars().count() as i64;
        let mut line_len = match prefix {
            Some(p) => len(p) - 1,
            None => indent_length - 1,
        };
        for part in parts {
            if line_len + 1 + len(part) > text_width && !line.is_empty() {
                lines.push(format!("{indent}{}", line.join(" ")));
                line.clear();
                line_len = indent_length - 1;
            }
            line.push(part);
            line_len += len(part) + 1;
        }
        if !line.is_empty() {
            lines.push(format!("{indent}{}", line.join(" ")));
        }
        if prefix.is_some() {
            if let Some(first) = lines.first_mut() {
                *first = first.chars().skip(indent.chars().count()).collect();
            }
        }
        lines
    };
    let lines: Vec<String> = if (len(prefix) + len(prog)) as f64 <= 0.75 * text_width as f64 {
        let indent = " ".repeat((len(prefix) + len(prog) + 1) as usize);
        let mut parts = vec![prog];
        parts.extend(opt_parts.iter().copied());
        get_lines(&parts, &indent, Some(prefix))
    } else {
        // prog is long: it goes on its own line (with no positionals, the
        // Python's re-split of optionals and positionals is the same lines)
        let indent = " ".repeat(len(prefix) as usize);
        let lines = get_lines(&opt_parts, &indent, None);
        let mut all = vec![prog.to_string()];
        all.extend(lines);
        all
    };
    done(&lines.join("\n"))
}

/// `ap.format_help()` for `columns` terminal columns.
fn format_help(prog: &str, columns: i64, t: &Theme) -> String {
    let width = columns - 2;
    let mut out = format_usage(prog, width, t);
    out.push_str("\n\n");
    // the description: one paragraph, whitespace collapsed
    let text_width = width.max(11);
    out.push_str(&tw_wrap(&collapse_ws(DOC), text_width).join("\n"));
    out.push_str(&format!("\n\n{}options:{}\n", t.heading, t.reset));
    let indent = 2i64;
    let max_help_position = 24i64.min((width - 20).max(4));
    let action_max_length = "--appimages".len() as i64 + indent;
    let help_position = (action_max_length + 2).min(max_help_position);
    let help_width = (width - help_position).max(11);
    let action_width = help_position - indent - 2;
    let pad = |n: i64| " ".repeat(n.max(0) as usize);
    // -h, --help: widths from the text without colour codes, then the
    // coloured header swapped in
    let header = "-h, --help";
    let header_color = format!("{}-h{}, {}--help{}", t.short_option, t.reset, t.long_option, t.reset);
    let help_lines = tw_wrap("show this help message and exit", help_width);
    let indent_first = if header.len() as i64 <= action_width {
        let line = format!("{}{:<w$}  ", pad(indent), header, w = action_width.max(0) as usize);
        out.push_str(&line.replace(header, &header_color));
        0
    } else {
        out.push_str(&format!("{}{header_color}\n", pad(indent)));
        help_position
    };
    for (i, line) in help_lines.iter().enumerate() {
        let lead = if i == 0 { indent_first } else { help_position };
        out.push_str(&format!("{}{line}\n", pad(lead)));
    }
    for opt in ["--agents", "--appimages", "--plugins"] {
        out.push_str(&format!("{}{}{opt}{}\n", pad(indent), t.long_option, t.reset));
    }
    out
}

/// The program name its usage and errors give.
const PROG: &str = "almanac-autorun";

// ------------------------------------------------------------------- main --

/// One of the three sections (or its stand-in).
fn section(name: &str) -> Result<usize, String> {
    let faked = SECTION_FAKE.with(|s| s.borrow_mut().as_mut().map(|f| f(name)));
    if let Some(n) = faked {
        return Ok(n);
    }
    match name {
        "agents" => update_agents(),
        "appimages" => update_appimages(),
        _ => update_plugins(),
    }
}

/// `main` once the config is loaded: (exit code, stdout text, stderr text)
/// for what argparse prints itself; the run's own lines go through `log`.
/// `Err` is an exception a section let out, which ends the Python run.
fn run_main(
    cfg: &autoupdate::Config,
    argv: &[PyStr],
    prog: &str,
    columns: i64,
    color: bool,
) -> Result<(i32, String, String), String> {
    let theme = if color { COLOR } else { NO_COLOR };
    let (agents, appimages, plugins) = match parse_args(argv) {
        Parsed::Flags {
            agents,
            appimages,
            plugins,
        } => (agents, appimages, plugins),
        Parsed::Help => return Ok((0, format_help(prog, columns, &theme), String::new())),
        Parsed::Error(msg) => {
            let usage = format_usage(prog, columns - 2, &theme);
            return Ok((2, String::new(), format!("{usage}\n{prog}: error: {msg}\n")));
        }
    };

    // An explicit flag overrides the config; no flags means "do what the
    // settings say", which is how the timer invokes it.
    let explicit = agents || appimages || plugins;
    let setting = |key: &str, default: bool| cfg.get(key).map(truthy).unwrap_or(default);
    let want = [
        ("agents", if explicit { agents } else { setting("agents", true) }),
        (
            "appimages",
            if explicit {
                appimages
            } else {
                setting("appimages", true)
            },
        ),
        ("plugins", if explicit { plugins } else { setting("plugins", false) }),
    ];

    let mut changed = 0;
    if want[0].1 {
        log("== agent CLIs ==");
        changed += section("agents")?;
    }
    if want[1].1 {
        log("== AppImages ==");
        changed += section("appimages")?;
    }
    if want[2].1 {
        log("== Omarchy plugins ==");
        changed += section("plugins")?;
    }
    if !want.iter().any(|(_, w)| *w) {
        log("nothing enabled in auto-update.json");
    }
    log(&format!("== {changed} item(s) updated =="));
    Ok((0, String::new(), String::new()))
}

/// The CLI (`--agents`, `--appimages`, `--plugins`). `argv` None reads the
/// process arguments; the arguments exclude the program name. Returns the
/// exit code: 0 for a run (as in the Python), 0 for `--help`, 2 for a usage
/// error (argparse's SystemExit), 1 when auto-update.json cannot be read or
/// a section raises (where the Python's uncaught exception ends the process
/// with its traceback; the port prints the exception line).
pub fn main(argv: Option<&[String]>) -> i32 {
    use std::os::unix::ffi::OsStrExt;
    // An argument that is not UTF-8 reaches Python's sys.argv as surrogate
    // escapes (and argparse reports it); it must not stop the port.
    let args: Vec<PyStr> = match argv {
        Some(a) => a.iter().map(|s| pystr(s)).collect(),
        None => std::env::args_os().skip(1).map(|a| fsdecode(a.as_bytes())).collect(),
    };
    // Python walks a deeply nested registry (repr, json.dumps) where this
    // port recurses: give the run a stack that cannot run out first.
    std::thread::scope(|s| {
        match std::thread::Builder::new()
            .name("autorun".into())
            .stack_size(MAIN_STACK)
            .spawn_scoped(s, || main_with(&args))
        {
            Ok(h) => h.join().unwrap_or(1),
            Err(_) => main_with(&args),
        }
    })
}

/// Stack for the run: room for the deepest registry Python's json accepts.
const MAIN_STACK: usize = 512 << 20;

fn main_with(args: &[PyStr]) -> i32 {
    let cfg = match autoupdate::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    take_updated();
    let result = run_main(&cfg, args, PROG, columns(), can_colorize());
    // What did update goes on the timeline even when a later section failed.
    if let Err(e) = record_run(&take_updated()) {
        eprintln!("almanac-autorun: history: {e}");
    }
    let (code, out, err) = match result {
        Ok(r) => r,
        Err(exc) => {
            eprintln!("Traceback (most recent call last):\n{exc}");
            return 1;
        }
    };
    if !out.is_empty() {
        use std::io::Write;
        let mut so = std::io::stdout().lock();
        let _ = so.write_all(out.as_bytes());
        let _ = so.flush();
    }
    if !err.is_empty() {
        eprint!("{err}");
    }
    code
}

// --- --core-holds -----------------------------------------------------------------

/// The names among pending-update lines that Almanac counts as core (the
/// kernel, boot, firmware, DKMS modules -- what can stop the machine
/// booting), one per
/// core package, in the order given. Each line is `name old -> new`, what
/// both `checkupdates` and `yay -Qua` print, so the repository and AUR
/// lists are read alike; blank lines are skipped.
pub fn core_holds(pending: &str) -> Vec<String> {
    pending
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| inventory::criticality(name) == "core")
        .map(str::to_owned)
        .collect()
}

/// [`core_holds`] of what `checkupdates` printed, failing closed: the
/// script holds what this returns, so a check that did not finish must not
/// read as "nothing to hold". checkupdates exits 2 when there is nothing to
/// update; any other failure, a timeout, or a checkupdates that could not
/// be started at all is an `Err` for the caller to report.
pub fn core_holds_checked(out: &crate::util::Output) -> Result<Vec<String>, String> {
    if out.failed_to_start {
        return Err("checkupdates could not be started (is pacman-contrib installed?)".into());
    }
    if out.timed_out {
        return Err("checkupdates timed out".into());
    }
    match out.code {
        Some(0) => Ok(core_holds(&out.stdout)),
        Some(2) => Ok(Vec::new()),
        Some(code) => Err(format!("checkupdates failed (exit {code}): {}", out.stderr.trim())),
        None => Err("checkupdates was killed by a signal".into()),
    }
}

#[cfg(test)]
/// A `_run` stand-in that records `[argv, timeout]` and answers `ok`.
fn recording_run(calls: std::rc::Rc<RefCell<Vec<Value>>>, ok: bool) -> RunFake {
    Box::new(move |argv: &[Value], timeout: u64| {
        calls.borrow_mut().push(json!([argv, timeout]));
        ok
    })
}


#[cfg(test)]
mod tests {
    use super::*;

    /// Repository and AUR lines read alike; blank and stray lines are
    /// skipped, and only what the Updates page marks core is held.
    #[test]
    fn core_holds_reads_repo_and_aur_lines() {
        let repo = "linux 6.11.1.arch1-1 -> 6.11.2.arch1-1\nfirefox 131.0-1 -> 131.0.2-1\n\n";
        let aur = "  \nlinux-firmware-git 20240909-1 -> 20241010-1\nyay-bin 12.3.5-1 -> 12.4.2-1\n";
        assert_eq!(core_holds(&format!("{repo}{aur}")), vec!["linux", "linux-firmware-git"]);
        assert!(core_holds("").is_empty());
        assert!(core_holds("\n\n   \n").is_empty());
        assert!(core_holds("firefox 1 -> 2").is_empty());
        // a DKMS module is rebuilt against the next kernel: held like one
        assert_eq!(core_holds("nvidia-dkms 560.35-1 -> 565.57-1\nzfs-dkms 2.2.6-1 -> 2.2.7-1\n"), vec!["nvidia-dkms", "zfs-dkms"]);
    }

    /// checkupdates' exit 2 is "nothing to update"; every other failure is
    /// an error, never an empty list of holds.
    #[test]
    fn core_holds_from_checkupdates_fail_closed() {
        use crate::util::Output;
        let ran = |code: Option<i32>, stdout: &str| Output { code, stdout: stdout.into(), ..Default::default() };
        assert_eq!(core_holds_checked(&ran(Some(0), "linux 1 -> 2\nfirefox 1 -> 2\n")), Ok(vec!["linux".to_owned()]));
        assert_eq!(core_holds_checked(&ran(Some(2), "")), Ok(vec![]));
        let err = core_holds_checked(&Output { code: Some(1), stderr: "==> ERROR: Cannot fetch updates\n".into(), ..Default::default() })
            .unwrap_err();
        assert_eq!(err, "checkupdates failed (exit 1): ==> ERROR: Cannot fetch updates");
        // A failure that still printed lines is not trusted either.
        assert!(core_holds_checked(&ran(Some(1), "linux 1 -> 2\n")).is_err());
        assert!(core_holds_checked(&Output { timed_out: true, ..Default::default() }).unwrap_err().contains("timed out"));
        assert!(core_holds_checked(&Output { failed_to_start: true, ..Default::default() })
            .unwrap_err()
            .contains("could not be started"));
        assert!(core_holds_checked(&ran(None, "")).is_err());
    }

    #[test]
    fn a_scheduled_run_is_titled_by_what_it_updated() {
        let items = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(run_title(&[]), None);
        assert_eq!(
            run_title(&items(&["wordcraft 0.7.0 → 0.8.0"])).as_deref(),
            Some("Scheduled update: updated wordcraft")
        );
        assert_eq!(
            run_title(&items(&["vectorcraft 0.12.0 → 0.13.1", "wordcraft 0.7.0 → 0.8.0"])).as_deref(),
            Some("Scheduled update: updated vectorcraft and wordcraft")
        );
        assert_eq!(
            run_title(&items(&["a 1 → 2", "b 1 → 2", "c 1 → 2", "d (3 commits)"])).as_deref(),
            Some("Scheduled update: updated 4 items")
        );
    }

    /// The scheduled update writes back only the entry it updated, read
    /// afresh under the registry's lock: what the window changed while it
    /// downloaded stays, and an app removed meanwhile is not brought back.
    #[test]
    fn an_update_keeps_what_changed_while_it_downloaded() {
        let _env = crate::util::TEST_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_STATE_HOME", d.path());
        let reg_path = inventory::registry();
        std::fs::create_dir_all(reg_path.parent().unwrap()).unwrap();
        std::fs::write(&reg_path, serde_json::to_string(&json!({
            "a": {"version": "1", "path": "/x/a.AppImage", "latest": {"version": "2"}},
            "b": {"version": "1", "path": "/x/b.AppImage"},
        })).unwrap()).unwrap();
        // Meanwhile the window renamed one entry's description and added
        // another; then the update of `a` lands.
        let mut now = inventory::load_registry();
        now["b"]["description"] = json!("changed in the window");
        now.insert("c".into(), json!({"version": "3", "path": "/x/c.AppImage"}));
        inventory::save_registry(&now).unwrap();
        let dest = d.path().join("a.AppImage");
        save_updated(&pystr("a"), &json!("2"), &dest).unwrap();
        let after = inventory::load_registry();
        assert_eq!(after["a"]["version"], json!("2"));
        assert_eq!(after["a"]["latest"], json!({}));
        assert_eq!(after["b"]["description"], json!("changed in the window"));
        assert_eq!(after["c"]["version"], json!("3"));
        // Removed while it downloaded: not brought back, its download gone.
        std::fs::write(&dest, b"new build").unwrap();
        let mut now = inventory::load_registry();
        now.remove("a");
        inventory::save_registry(&now).unwrap();
        assert!(save_updated(&pystr("a"), &json!("3"), &dest).is_err());
        assert!(!inventory::load_registry().contains_key("a"));
        assert!(!dest.exists());
        let names: Vec<_> = std::fs::read_dir(reg_path.parent().unwrap()).unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert!(!names.iter().any(|n| n.ends_with(".tmp")), "{names:?}");
    }

    /// The release's published sha256 reaches the download.
    #[test]
    fn the_published_checksum_reaches_the_download() {
        take_updated();
        let hex = "a".repeat(64);
        let mut check = |_k: &str, _r: &Value| {
            Ok(json!({"url": "https://example.org/x.AppImage", "version": "0.8.0", "sha256": hex}))
        };
        let reg = PyJson::from_value(&json!({"wordcraft": {"version": "0.7.0", "path": "/nonexistent/wordcraft.AppImage"}}));
        let mut seen = Vec::new();
        let (r, _) = capture_log(|| {
            update_appimages_with(reg, &mut check, &mut |_, _, sha| {
                seen.push(sha.to_string());
                Ok(true)
            }, &mut |_, _| Ok(()), &mut |_, _, _| Ok(()))
        });
        assert_eq!(r, Ok(1));
        assert_eq!(seen, vec![hex.clone()]);
        take_updated();
    }

    /// A build with no published checksum is installed, and the log says
    /// it was not verified; a checked one gets no such line.
    #[test]
    fn an_unverified_download_is_logged() {
        take_updated();
        for (verified, want) in [(false, true), (true, false)] {
            let mut check = |_k: &str, _r: &Value| Ok(json!({"url": "https://example.org/x.AppImage", "version": "0.8.0"}));
            let reg = PyJson::from_value(&json!({"wordcraft": {"version": "0.7.0", "path": "/nonexistent/wordcraft.AppImage"}}));
            let (r, lines) = capture_log(|| {
                update_appimages_with(reg, &mut check, &mut |_, _, _| Ok(verified), &mut |_, _| Ok(()), &mut |_, _, _| Ok(()))
            });
            assert_eq!(r, Ok(1));
            assert_eq!(lines.iter().any(|l| l == "  unverified (no checksum published)"), want, "{lines:?}");
        }
        take_updated();
    }

    #[test]
    fn successful_updates_are_remembered_for_history() {
        take_updated();
        let mut no_check = |_k: &str, _r: &Value| Ok(json!({"url": "https://example.org/x.AppImage", "version": "0.8.0"}));
        let reg = PyJson::from_value(&json!({"wordcraft": {"version": "0.7.0", "path": "/nonexistent/wordcraft.AppImage"}}));
        let (r, _) = capture_log(|| {
            update_appimages_with(reg, &mut no_check, &mut |_, _, _| Ok(true), &mut |_, _| Ok(()), &mut |_, _, _| Ok(()))
        });
        assert_eq!(r, Ok(1));
        assert_eq!(take_updated(), vec!["wordcraft 0.7.0 → 0.8.0".to_string()]);
        assert!(take_updated().is_empty(), "taking empties the list");
    }
    use std::rc::Rc;

    #[test]
    fn test_scheduled_runs_hold_major_releases() {
        let mut a = inventory::App::new("opencode", "1.19.3", "agent");
        a.update = "2.0.30".into();
        a.extra.insert("update_cmd".into(), json!(["true"]));
        let calls = Rc::new(RefCell::new(Vec::new()));
        let (done, _) = with_run_fake(recording_run(calls.clone(), true), || {
            capture_log(|| update_agents_from(vec![a]))
        });
        assert_eq!(done, Ok(0));
        assert!(calls.borrow().is_empty());
    }

    struct PluginStates {
        _tmp: tempfile::TempDir,
        d: PathBuf,
        up: PathBuf,
        plug: PathBuf,
    }

    fn git(args: &[&str], cwd: &Path) {
        let st = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("git");
        assert!(
            st.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&st.stderr)
        );
    }

    impl PluginStates {
        fn set_up() -> PluginStates {
            let tmp = tempfile::tempdir().expect("tmp");
            let d = tmp.path().to_path_buf();
            let up = d.join("up");
            git(&["init", "-q", "-b", "main", up.to_str().unwrap()], &d);
            std::fs::write(up.join("manifest.json"), "{}").unwrap();
            git(&["add", "."], &up);
            git(&["commit", "-qm", "one"], &up);
            let plug = d.join("plugins").join("a.plugin");
            git(&["clone", "-q", up.to_str().unwrap(), plug.to_str().unwrap()], &d);
            PluginStates { _tmp: tmp, d, up, plug }
        }

        fn advance_upstream(&self) {
            std::fs::write(self.up.join("f"), "x").unwrap();
            git(&["add", "."], &self.up);
            git(&["commit", "-qm", "two"], &self.up);
        }
    }

    #[test]
    fn test_scheduled_runs_skip_links_and_out_of_scope_plugins() {
        let s = PluginStates::set_up();
        s.advance_upstream();
        std::os::unix::fs::symlink(&s.plug, s.d.join("plugins").join("dev.plugin")).unwrap();
        let pdir = s.d.join("plugins");
        let trust = BTreeMap::new(); // store.catalog() -> []
        let calls = Rc::new(RefCell::new(Vec::new()));
        let _ = with_run_fake(recording_run(calls.clone(), true), || {
            capture_log(|| update_plugins_in(&pdir, "verified", &trust))
        });
        assert!(calls.borrow().is_empty()); // unlisted, so outside "verified"
        let calls = Rc::new(RefCell::new(Vec::new()));
        let _ = with_run_fake(recording_run(calls.clone(), true), || {
            capture_log(|| update_plugins_in(&pdir, "all", &trust))
        });
        let updated: Vec<String> = calls
            .borrow()
            .iter()
            .map(|c| c[0][3].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(updated, vec!["a.plugin".to_string()]); // never the symlink
    }

    #[test]
    fn help_matches_argparse_at_80_columns() {
        let help = format_help("autorun.py", 80, &NO_COLOR);
        assert!(help.starts_with("usage: autorun.py [-h] [--agents] [--appimages] [--plugins]\n\n"));
        assert!(help.contains("\noptions:\n  -h, --help   show this help message and exit\n  --agents\n"));
        assert!(help.contains("the scheduled update. omarchy-auto-update handles repo\nand AUR"));
    }

    #[test]
    fn textwrap_breaks_like_python() {
        assert_eq!(
            tw_wrap("aa-bb a--b x-y-z ab--cd 1-2 abc-1x", 4),
            vec!["aa-", "bb a", "--b", "x-y-", "z ab", "--cd", "1-2", "abc-", "1x"]
        );
    }

    #[test]
    fn output_that_is_not_utf8_ends_the_run() {
        let argv: Vec<String> = ["sh", "-c", "printf 'ok\\377\\n'"].map(String::from).to_vec();
        let (r, _) = capture_log(|| _run(&argv, 30));
        assert_eq!(
            r,
            Err("UnicodeDecodeError: 'utf-8' codec can't decode byte 0xff in position 2: invalid start byte".into())
        );
        assert_eq!(
            py_decode(vec![b'a', 0xe2, 0x82]),
            Err("UnicodeDecodeError: 'utf-8' codec can't decode bytes in position 1-2: unexpected end of data".into())
        );
    }

    #[test]
    fn the_timeout_covers_pipes_a_grandchild_holds_open() {
        let argv: Vec<String> = ["sh", "-c", "(sleep 3) & exit 0"].map(String::from).to_vec();
        let t = Instant::now();
        let (r, lines) = capture_log(|| _run(&argv, 1));
        assert_eq!(r, Ok(false));
        assert!(t.elapsed() < Duration::from_millis(2500));
        assert_eq!(
            lines[1],
            "  failed to start: Command '['sh', '-c', '(sleep 3) & exit 0']' timed out after 1 seconds"
        );
    }

    #[test]
    fn update_cmd_is_passed_to_run_as_python_lists_it() {
        let mut a = inventory::App::new("x", "1.0", "agent");
        a.update = "1.1".into();
        a.extra.insert("update_cmd".into(), json!(5));
        let (r, _) = capture_log(|| update_agents_from(vec![a.clone()]));
        assert_eq!(r, Err("TypeError: 'int' object is not iterable".into()));
        a.extra.insert("update_cmd".into(), json!(["true", 1]));
        let (r, _) = capture_log(|| update_agents_from(vec![a]));
        assert_eq!(r, Err("TypeError: sequence item 1: expected str instance, int found".into()));
    }

    #[test]
    fn a_registry_that_is_not_a_dict_is_an_error() {
        let mut no = |_: &str, _: &Value| -> Result<Value, String> { Ok(json!({})) };
        let r = update_appimages_with(PyJson::from_value(&json!([1, 2])), &mut no, &mut |_, _, _| Ok(true), &mut |_, _| Ok(()), &mut |_, _, _| Ok(()));
        assert_eq!(r, Err("AttributeError: 'list' object has no attribute 'items'".into()));
        assert_eq!(check_record(&json!("x")), Err("AttributeError: 'str' object has no attribute 'get'".into()));
    }

    #[test]
    fn columns_reads_like_int() {
        assert_eq!(py_int("1_20"), Some(120));
        assert_eq!(py_int("\u{ff11}\u{ff12}\u{ff10}"), Some(120));
        assert_eq!(py_int(" 50\n"), Some(50));
        assert_eq!(py_int("1__0"), None);
        assert_eq!(py_int("_1"), None);
        assert_eq!(py_int("\u{0663}"), Some(3));
        assert_eq!(py_int("abc"), None);
    }

    #[test]
    fn colored_usage_matches_argparse() {
        assert_eq!(
            format_usage("autorun.py", 78, &COLOR),
            "\x1b[1;34musage: \x1b[0m\x1b[1;35mautorun.py\x1b[0m [\x1b[32m-h\x1b[0m] [\x1b[36m--agents\x1b[0m] \
             [\x1b[36m--appimages\x1b[0m] [\x1b[36m--plugins\x1b[0m]"
        );
    }

    #[test]
    fn repr_escapes_what_python_cannot_print() {
        assert_eq!(py_repr("a\u{a0}b"), "'a\\xa0b'");
        assert_eq!(py_repr("\u{200b}\u{ad}\u{2028}"), "'\\u200b\\xad\\u2028'");
        assert_eq!(py_repr("é😀 ☃"), "'é😀 ☃'");
        assert_eq!(py_repr_py(&[0xdcff, u32::from('x')]), "'\\udcffx'");
        assert_eq!(py_repr("\u{e0001}"), "'\\U000e0001'");
    }

    #[test]
    fn bytes_round_trip_through_surrogate_escapes() {
        let s = fsdecode(b"a\xffb\xe2\x82");
        assert_eq!(s, vec![0x61, 0xdcff, 0x62, 0xdce2, 0xdc82]);
        assert_eq!(fsencode(&s).unwrap(), b"a\xffb\xe2\x82".to_vec());
        assert_eq!(
            py_encode(&s, "strict"),
            Err("UnicodeEncodeError: 'utf-8' codec can't encode character '\\udcff' in position 1: surrogates not allowed".into())
        );
        assert_eq!(
            py_encode(&[0x61, 0xdcff, 0xdcfe], "strict"),
            Err("UnicodeEncodeError: 'utf-8' codec can't encode characters in position 1-2: surrogates not allowed".into())
        );
        assert!(fsencode(&[0xd800]).is_err());
        assert_eq!(render(&s), "a\\udcffb\\udce2\\udc82");
    }

    #[test]
    fn an_empty_program_is_searched_along_path_as_python_does() {
        let (r, lines) = capture_log(|| _run(&[String::new()], 30));
        assert_eq!(r, Ok(false));
        assert_eq!(lines[1], "  failed to start: [Errno 13] Permission denied: ''");
    }

    #[test]
    fn the_child_gets_no_inherited_descriptors() {
        // a descriptor this process holds open: not passed on
        let f = tempfile::tempfile().unwrap();
        let argv: Vec<String> = ["sh", "-c", "ls /proc/self/fd | wc -l"].map(String::from).to_vec();
        let (r, lines) = capture_log(|| _run(&argv, 30));
        drop(f);
        assert_eq!(r, Ok(true));
        // 0, 1, 2 and the one ls opens to list the directory
        assert_eq!(lines[1].trim(), "4");
    }

    #[test]
    fn json_reads_and_writes_as_python_does() {
        let doc = match parse_json(r#"{"a": 100000000000000000000, "b": -0, "c": 1e400, "d": NaN, "a": [1.5, "\udcff"]}"#) {
            Ok(d) => d,
            Err(_) => panic!("parse"),
        };
        assert_eq!(
            doc.dumps_indent2(doc.root),
            "{\n  \"a\": [\n    1.5,\n    \"\\udcff\"\n  ],\n  \"b\": 0,\n  \"c\": Infinity,\n  \"d\": NaN\n}"
        );
        assert_eq!(render(&doc.py_str(doc.get(doc.root, "c").unwrap()).unwrap()), "inf");
        for bad in ["", "[1,]", "{\"a\": 01}", "1 x", "\u{feff}1", "\"\t\""] {
            assert!(parse_json(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn trust_raising_empties_the_table() {
        let row = |extra: Value| -> store::Plugin {
            let mut m = json!({"id": "p", "name": "n", "market": "omarchy"});
            if let (Value::Object(m), Value::Object(e)) = (&mut m, extra) {
                m.extend(e);
            }
            store::Plugin::from_row(m.as_object().unwrap()).unwrap()
        };
        let ok = row(json!({"upstream_status": "failed", "upstream_checked_at": "2026-01-01"}));
        assert_eq!(trust_map(vec![ok]).unwrap().get("p").map(|l| l.key.as_str()), Some("unreachable"));
        let bad = row(json!({"upstream_status": "failed", "upstream_checked_at": null}));
        assert!(trust_map(vec![bad]).is_err());
    }

    #[test]
    fn arguments_that_are_not_utf8_are_reported() {
        assert_eq!(
            parse_args(&[fsdecode(b"\xff")]),
            Parsed::Error("unrecognized arguments: \\udcff".into())
        );
        assert_eq!(
            parse_args(&[fsdecode(b"--agents=\xfe")]),
            Parsed::Error("argument --agents: ignored explicit argument '\\udcfe'".into())
        );
    }

    #[test]
    fn os_and_http_errors_read_as_python_prints_them() {
        assert_eq!(py_net_error("HTTPError: HTTP Error 404"), "HTTPError: HTTP Error 404: Not Found");
        assert_eq!(py_net_error("OSError: No space left on device (os error 28)"), "OSError: [Errno 28] No space left on device");
        let e = std::io::Error::from_raw_os_error(13);
        assert_eq!(py_os_error(&e, &[Path::new("/a.tmp")]), "[Errno 13] Permission denied: '/a.tmp'");
    }

    #[test]
    fn a_registry_as_deep_as_python_reads_is_walked_without_recursion() {
        // A registry nested as deep as json.loads goes, on a default 2 MiB
        // thread: converting, writing and repr'ing it recursed per level.
        let n = JSON_MAX_DEPTH - 2;
        let text = format!("{{\"k\": {}{}}}", "[".repeat(n), "]".repeat(n));
        std::thread::Builder::new()
            .stack_size(2 << 20)
            .spawn(move || {
                let Ok(doc) = parse_json(&text) else { panic!("Python reads it") };
                let v = doc.to_value(doc.root);
                assert_eq!(crate::util::tree::value_depth(&v), n + 1);
                crate::util::tree::drop_value(v);
                let k = doc.get(doc.root, "k").unwrap();
                // repr(): RecursionError past the C stack CPython has for it
                assert_eq!(
                    doc.py_str(k).unwrap_err(),
                    "RecursionError: Stack overflow (used 8148 kB) while getting the repr of an object"
                );
                assert!(doc.dumps_indent2(doc.root).starts_with("{\n  \"k\": [\n    ["));
            })
            .unwrap()
            .join()
            .unwrap();
        let shallow = parse_json("{\"k\": [[1, {\"a\": []}], \"x\"]}").ok().unwrap();
        let k = shallow.get(shallow.root, "k").unwrap();
        assert_eq!(render(&shallow.py_str(k).unwrap()), "[[1, {'a': []}], 'x']");
        assert_eq!(shallow.dumps_indent2(shallow.root), "{\n  \"k\": [\n    [\n      1,\n      {\n        \"a\": []\n      }\n    ],\n    \"x\"\n  ]\n}");
    }
}
