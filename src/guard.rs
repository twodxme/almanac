//! Port of `guard.py`.
//!
//! Settings a tool quietly put back to their defaults.
//!
//! Omarchy's `refresh` commands copy the shipped default of a config file over
//! the user's own, keeping theirs as `<file>.bak.<epoch>`. That is a reasonable
//! repair tool and a terrible surprise: `omarchy-refresh-shell` replaces
//! shell.json, every custom bar widget disappears, and nothing says why. The
//! backup is sitting right there; nobody tells you it exists.
//!
//! A reset is recognised from the files alone, which makes it independent of
//! which command did it and of whether Almanac was running at the time:
//!
//! * `<file>.bak.<epoch>` exists, from the last week,
//! * `<file>` is now identical to the default the distribution ships, and
//! * the backup is not -- so something customised was replaced.
//!
//! When the user chooses the defaults on purpose, the finding still offers the
//! backup for a week, then lets it go. Restoring keeps the replaced defaults as
//! `<file>.defaults.<epoch>`, so the restore is reversible too.
//!
//! On a system without a shipped-defaults tree (anything that is not Omarchy)
//! there is nothing to compare against, and this finds nothing.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize, Serializer};

// Python's `$` also matches before a final newline; `\n?\z` is the same.
pub static BAK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?P<base>.+)\.bak\.(?P<ts>\d{9,11})\n?\z").unwrap());
/// `BAK` on a file name's bytes, which need not be UTF-8: Python matches the
/// surrogateescape'd str, where an undecodable byte is a character `.`
/// matches and `\d` does not -- as any byte but a newline here.
static BAK_BYTES: LazyLock<regex::bytes::Regex> = LazyLock::new(|| {
    regex::bytes::Regex::new(r"^(?P<base>(?-u:[^\n])+)\.bak\.(?P<ts>\d{9,11})\n?\z").unwrap()
});
pub const WINDOW_DAYS: u64 = 7;
/// Deep enough for ~/.config/<app>/<sub>/<file>; a full walk of ~/.config can
/// be large (browser profiles) and nothing reset by a refresh lives that deep.
pub const MAX_DEPTH: u32 = 4;

/// `sys.getrecursionlimit()`: Python frames a thread may have at once.
const RECURSION_LIMIT: usize = 1000;

/// Where the Python would be running `resets`: how many Python frames sit
/// above it, and whether on a worker thread. Python's two limits hang on
/// this, so the same document can be read on one path and raise on another:
///
/// * `_ids`' recursive `walk` shares the recursion limit with every frame
///   above it, and
/// * `json`'s C scanner (CPython 3.14) is bounded by the C stack left. A
///   worker thread starts on a fresh stack of `RLIMIT_STACK` bytes, so its
///   depth is fixed (measured: 160 bytes a level, 43920 before the first;
///   52154 levels under 8 MB, 12832 under 2 MB, 104583 under 16 MB). The
///   main thread's stack starts below the environment, at an address ASLR
///   moves by a few KB, so there Python itself varies from run to run (here
///   52101..52139 levels, 52156..52200 with an empty environment), and GTK's
///   own frames sit above the window's call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PyStack {
    /// Python frames on the stack when `resets` (or the function called) starts.
    pub frames: usize,
    /// On a `threading.Thread` rather than the main thread.
    pub thread: bool,
}

impl PyStack {
    /// app.py `_config_check`: `work` on its own thread (`_bootstrap`,
    /// `_bootstrap_inner`, `run`, `work`).
    pub const WORKER: PyStack = PyStack { frames: 4, thread: true };
    /// The Health check: `maintenance.run_all` on `_maint_reload`'s (or
    /// `_feat_reload`'s) worker thread, then `config_resets` and `finding`.
    pub const HEALTH: PyStack = PyStack { frames: 7, thread: true };
    /// `almanac --guard`: app.py's module frame, then `watcher.main`.
    pub const WATCHER: PyStack = PyStack { frames: 2, thread: false };
    /// app.py `_watch_configs`, from the window's construction: the module,
    /// Gio's `Application.run` override, `do_command_line`, `do_activate`,
    /// `AlmanacWindow.__init__`, `_watch_configs`.
    pub const WINDOW: PyStack = PyStack { frames: 6, thread: false };

    /// How many arrays/objects deep `json.loads` gets before RecursionError.
    fn json_depth(self) -> usize {
        // glibc: a thread's default stack is RLIMIT_STACK (page-aligned), or
        // 2 MB when that is unlimited.
        let size = match rustix::process::getrlimit(rustix::process::Resource::Stack).current {
            Some(n) => (n.max(16384) as usize).div_ceil(4096) * 4096,
            None => 2 * 1024 * 1024,
        };
        let depth = size.saturating_sub(43920) / 160;
        if self.thread {
            depth
        } else {
            // The middle of what the main thread measured here; see above.
            depth.saturating_sub(35)
        }
    }

    /// How deep `_ids`' `walk` can recurse when `_ids` is called with
    /// `above` frames over it.
    fn ids_depth(above: usize) -> usize {
        RECURSION_LIMIT.saturating_sub(above + 1)
    }
}

pub fn config_home() -> PathBuf {
    crate::xdg::config_home()
}

/// Where the distribution keeps the shipped copies of user configs.
pub fn defaults_root() -> Option<PathBuf> {
    // `os.environ.get(...) or default`: bytes as they are, empty means unset.
    let base = std::env::var_os("OMARCHY_PATH")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| OsString::from("/usr/share/omarchy"));
    let root = py_path(&Path::new(&base).join("config"));
    if root.is_dir() {
        Some(root)
    } else {
        None
    }
}

/// A path as JSON: lossy where it is not UTF-8 (a Python str would carry the
/// bytes as surrogates, which JSON from serde cannot), instead of failing.
fn lossy_path<S: Serializer>(p: &Path, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&p.to_string_lossy())
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Reset {
    /// the config file, now holding defaults
    #[serde(serialize_with = "lossy_path")]
    pub path: PathBuf,
    /// the user's version
    #[serde(serialize_with = "lossy_path")]
    pub backup: PathBuf,
    pub when: f64,
    /// what the defaults dropped
    pub lost: Vec<String>,
}

impl Reset {
    pub fn new(path: PathBuf, backup: PathBuf, when: f64, lost: Vec<String>) -> Self {
        Reset { path, backup, when, lost }
    }

    pub fn name(&self) -> String {
        let home = config_home();
        let shown = match py_relative_to(self.path.as_os_str().as_bytes(), home.as_os_str().as_bytes()) {
            Some(rel) => rel,
            None => py_str(self.path.as_os_str().as_bytes()),
        };
        String::from_utf8_lossy(&shown).into_owned()
    }
}

// ---- pathlib -------------------------------------------------------------------

/// pathlib's parse of a POSIX path: the root (`""`, `"/"`, or exactly two
/// slashes, which POSIX keeps distinct) and the parts, with empty and `.`
/// parts dropped.
fn py_parts(raw: &[u8]) -> (&'static [u8], Vec<&[u8]>) {
    let (root, rest): (&'static [u8], &[u8]) = if !raw.starts_with(b"/") {
        (b"", raw)
    } else if !raw.starts_with(b"//") || raw.starts_with(b"///") {
        (b"/", raw)
    } else {
        (b"//", &raw[2..])
    };
    let parts = rest.split(|&c| c == b'/').filter(|p| !p.is_empty() && *p != b".").collect();
    (root, parts)
}

fn py_join(root: &[u8], parts: &[&[u8]]) -> Vec<u8> {
    let mut out = root.to_vec();
    out.extend(parts.join(&b'/'));
    if out.is_empty() {
        out.push(b'.');
    }
    out
}

/// `str(Path(p))`, as bytes.
fn py_str(raw: &[u8]) -> Vec<u8> {
    let (root, parts) = py_parts(raw);
    py_join(root, &parts)
}

/// `Path(p)` as pathlib normalises it: repeated separators collapse, `.`
/// components and a trailing slash go, and POSIX keeps exactly two leading
/// slashes as they are.
fn py_path(p: &Path) -> PathBuf {
    PathBuf::from(OsString::from_vec(py_str(p.as_os_str().as_bytes())))
}

/// `PurePath.name`.
fn py_name(p: &Path) -> Vec<u8> {
    py_parts(p.as_os_str().as_bytes()).1.last().map(|n| n.to_vec()).unwrap_or_default()
}

/// `str(Path(path).relative_to(other))`, or `None` where it raises
/// ValueError: `other` must be `path` or one of its parents, root included
/// (so `//x` is not under `/x`).
fn py_relative_to(path: &[u8], other: &[u8]) -> Option<Vec<u8>> {
    let (r1, p1) = py_parts(path);
    let (r2, p2) = py_parts(other);
    if r1 != r2 || p2.len() > p1.len() || p1[..p2.len()] != p2[..] {
        return None;
    }
    Some(py_join(b"", &p1[p2.len()..]))
}

/// `PurePath.with_name(name)`; Python raises ValueError for `""` and `"."`,
/// and for a path that has no name to replace.
fn py_with_name(p: &Path, name: &[u8]) -> Result<PathBuf, String> {
    let raw = p.as_os_str().as_bytes();
    let (root, mut parts) = py_parts(raw);
    if parts.is_empty() {
        return Err(format!("PosixPath({}) has an empty name", py_repr(&surrogateescape(&py_join(root, &parts)))));
    }
    if name.is_empty() || name == b"." || name.contains(&b'/') {
        return Err(format!("Invalid name {}", py_repr(&surrogateescape(name))));
    }
    parts.pop();
    parts.push(name);
    Ok(PathBuf::from(OsString::from_vec(py_join(root, &parts))))
}

// ---- Python strings --------------------------------------------------------------
//
// A Python str is a sequence of code points, lone surrogates included -- json
// keeps `"\ud800"` as one, a file name keeps an undecodable byte as one
// (surrogateescape). A Rust String cannot hold them, so where they decide an
// answer (equality, sets of ids, sort order) a str is its code points.

type PyStr = Vec<u32>;

fn pystr(s: &str) -> PyStr {
    s.chars().map(|c| c as u32).collect()
}

/// For display: a lone surrogate becomes U+FFFD.
fn pystr_lossy(s: &[u32]) -> String {
    s.iter().map(|&c| char::from_u32(c).unwrap_or('\u{fffd}')).collect()
}

/// UTF-8, where `on_bad` decides what an invalid sequence becomes: the code
/// points to push, and how many bytes they stand for -- or `None` to fail.
fn decode_utf8(b: &[u8], on_bad: impl Fn(&[u8]) -> Option<(Vec<u32>, usize)>) -> Option<PyStr> {
    let mut out = Vec::with_capacity(b.len());
    let mut rest = b;
    loop {
        match std::str::from_utf8(rest) {
            Ok(s) => {
                out.extend(s.chars().map(|c| c as u32));
                return Some(out);
            }
            Err(e) => {
                let (good, bad) = rest.split_at(e.valid_up_to());
                // `good` is valid by construction.
                out.extend(String::from_utf8_lossy(good).chars().map(|c| c as u32));
                let (cps, n) = on_bad(bad)?;
                out.extend(cps);
                rest = &bad[n.max(1).min(bad.len())..];
                if rest.is_empty() {
                    return Some(out);
                }
            }
        }
    }
}

/// `os.fsdecode`: an undecodable byte `b` becomes U+DC00+b.
fn surrogateescape(b: &[u8]) -> PyStr {
    decode_utf8(b, |bad| {
        // `error_len` is the maximal invalid prefix; each of its bytes is
        // escaped on its own, as Python's handler does.
        let n = match std::str::from_utf8(bad) {
            Err(e) => e.error_len().unwrap_or(bad.len()),
            Ok(_) => 0,
        };
        Some((bad[..n].iter().map(|&x| 0xdc00 + x as u32).collect(), n))
    })
    .unwrap_or_default()
}

/// `bytes.decode('utf-8', 'surrogatepass')`: an encoded surrogate
/// (`ED A0..BF 80..BF`) is that code point; anything else invalid fails.
fn utf8_surrogatepass(b: &[u8]) -> Option<PyStr> {
    decode_utf8(b, |bad| match bad {
        [0xed, b1 @ 0xa0..=0xbf, b2 @ 0x80..=0xbf, ..] => {
            Some((vec![0xd000 | ((*b1 as u32 & 0x3f) << 6) | (*b2 as u32 & 0x3f)], 3))
        }
        _ => None,
    })
}

/// `repr(str)`, for messages: single quotes unless the text has one and no
/// double quote, and escapes for what is not printable.
fn py_repr(s: &[u32]) -> String {
    let q = if s.contains(&('\'' as u32)) && !s.contains(&('"' as u32)) { '"' } else { '\'' };
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
            Some(ch) if c >= 0x20 && !(0x7f..0xa0).contains(&c) => out.push(ch),
            _ if c < 0x100 => out.push_str(&format!("\\x{c:02x}")),
            _ if c < 0x10000 => out.push_str(&format!("\\u{c:04x}")),
            _ => out.push_str(&format!("\\U{c:08x}")),
        }
    }
    out.push(q);
    out
}

fn path_repr(p: &Path) -> String {
    py_repr(&surrogateescape(p.as_os_str().as_bytes()))
}

/// A Python OSError's text: `[Errno 2] No such file or directory: 'path'`.
fn os_error(e: &std::io::Error, path: &Path) -> String {
    os_error_on(e, &path_repr(path))
}

/// The same, naming `file` (already a repr, or `'src' -> 'dst'`).
fn os_error_on(e: &std::io::Error, file: &str) -> String {
    match e.raw_os_error() {
        Some(n) => {
            let text = e.to_string();
            let msg = text.strip_suffix(&format!(" (os error {n})")).unwrap_or(&text).to_string();
            format!("[Errno {n}] {msg}: {file}")
        }
        None => format!("{e}: {file}"),
    }
}

// ---- content for comparison ---------------------------------------------------
//
// Python compares what `json.loads` gives back with `==`. serde_json differs
// from that in ways that change the answer (key order, which `_ids` walks;
// `1 == 1.0 == true`; NaN/Infinity; duplicate keys; UTF-16 input; lone
// surrogates; nesting depth), so the comparison runs on a small value type
// with Python's semantics instead. Values live in one flat arena and are
// walked with explicit stacks: a document can be 50000 levels deep, which
// neither a recursive walk nor a recursive drop survives on a thread's stack.

/// A value as Python's `json.loads` returns it; containers hold arena indices.
#[derive(Debug, Clone)]
enum Node {
    None,
    Bool(bool),
    Int(i128),
    /// An int too large for i128, as its decimal digits.
    BigInt(String),
    Float(f64),
    Str(PyStr),
    List(Vec<usize>),
    /// Insertion order, as a Python dict keeps it.
    Dict(Vec<(PyStr, usize)>),
}

#[derive(Debug, Clone)]
struct Doc {
    nodes: Vec<Node>,
    root: usize,
}

impl Doc {
    fn top(&self) -> &Node {
        &self.nodes[self.root]
    }
}

/// Content for comparison: what `normal` returns.
#[derive(Debug, Clone)]
enum Normal {
    /// Parsed JSON, so formatting is ignored. Never a top-level `None`: a
    /// JSON `null` is Python's `None`, the same answer as an unreadable file.
    Json(Doc),
    /// The raw bytes, stripped of surrounding whitespace.
    Raw(Vec<u8>),
}

/// Why `json.loads` failed: a ValueError (the caller falls back to bytes) or
/// a RecursionError, which nothing here catches.
enum JsonFail {
    Value,
    Recursion(String),
}

/// An int and a float, compared exactly as Python does.
fn int_eq_float(i: &str, f: f64) -> bool {
    if !f.is_finite() || f.fract() != 0.0 {
        return false;
    }
    if f == 0.0 {
        return i == "0";
    }
    // An integral f64 prints as its exact decimal value.
    format!("{f:.0}") == i
}

fn as_int(v: &Node) -> Option<String> {
    match v {
        Node::Bool(b) => Some(if *b { "1".into() } else { "0".into() }),
        Node::Int(i) => Some(i.to_string()),
        Node::BigInt(s) => Some(s.clone()),
        _ => None,
    }
}

/// Python's `a == b` on two parsed documents. Below the top, Python first
/// checks identity -- and `json` hands out one shared NaN object, so `[NaN]
/// == [NaN]` although `NaN != NaN`.
fn py_eq(da: &Doc, db: &Doc) -> bool {
    let mut todo = vec![(da.root, db.root, true)];
    while let Some((ia, ib, top)) = todo.pop() {
        let (a, b) = (&da.nodes[ia], &db.nodes[ib]);
        let same = match (a, b) {
            (Node::None, Node::None) => true,
            (Node::Float(x), Node::Float(y)) => x == y || (!top && x.is_nan() && y.is_nan()),
            (Node::Float(f), other) | (other, Node::Float(f)) => match as_int(other) {
                Some(i) => int_eq_float(&i, *f),
                None => false,
            },
            (Node::Str(x), Node::Str(y)) => x == y,
            (Node::List(x), Node::List(y)) => {
                if x.len() != y.len() {
                    false
                } else {
                    todo.extend(x.iter().zip(y).map(|(p, q)| (*p, *q, false)));
                    true
                }
            }
            (Node::Dict(x), Node::Dict(y)) => {
                if x.len() != y.len() {
                    false
                } else {
                    let index: HashMap<&[u32], usize> = y.iter().map(|(k, v)| (k.as_slice(), *v)).collect();
                    let mut ok = true;
                    for (k, v) in x {
                        match index.get(k.as_slice()) {
                            Some(w) => todo.push((*v, *w, false)),
                            None => {
                                ok = false;
                                break;
                            }
                        }
                    }
                    ok
                }
            }
            _ => match (as_int(a), as_int(b)) {
                (Some(x), Some(y)) => x == y,
                _ => false,
            },
        };
        if !same {
            return false;
        }
    }
    true
}

/// `a == b` for two results of `normal` (`None` for Python's None).
fn normal_eq(a: &Option<Normal>, b: &Option<Normal>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(Normal::Json(x)), Some(Normal::Json(y))) => py_eq(x, y),
        (Some(Normal::Raw(x)), Some(Normal::Raw(y))) => x == y,
        _ => false,
    }
}

/// `bytes.strip()`: ASCII whitespace, vertical tab included.
fn py_bytes_strip(raw: &[u8]) -> Vec<u8> {
    let ws = |c: &u8| matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c);
    let start = raw.iter().position(|c| !ws(c)).unwrap_or(raw.len());
    let end = raw.iter().rposition(|c| !ws(c)).map(|e| e + 1).unwrap_or(start);
    raw[start..end.max(start)].to_vec()
}

/// `json.detect_encoding` and `bytes.decode(enc, 'surrogatepass')`; `None`
/// where that raises (UnicodeDecodeError, a ValueError).
fn py_json_decode(b: &[u8]) -> Option<PyStr> {
    let utf16 = |data: &[u8], be: bool| -> Option<PyStr> {
        if data.len() % 2 != 0 {
            return None;
        }
        let units: Vec<u32> = data
            .chunks(2)
            .map(|c| if be { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_le_bytes([c[0], c[1]]) } as u32)
            .collect();
        // A pair is one code point; a lone surrogate passes as itself.
        let mut out = Vec::with_capacity(units.len());
        let mut i = 0;
        while i < units.len() {
            let u = units[i];
            if (0xd800..0xdc00).contains(&u) && i + 1 < units.len() && (0xdc00..0xe000).contains(&units[i + 1]) {
                out.push(0x10000 + ((u - 0xd800) << 10) + (units[i + 1] - 0xdc00));
                i += 2;
            } else {
                out.push(u);
                i += 1;
            }
        }
        Some(out)
    };
    let utf32 = |data: &[u8], be: bool| -> Option<PyStr> {
        if data.len() % 4 != 0 {
            return None;
        }
        let mut out = Vec::with_capacity(data.len() / 4);
        for c in data.chunks(4) {
            let n = if be {
                u32::from_be_bytes([c[0], c[1], c[2], c[3]])
            } else {
                u32::from_le_bytes([c[0], c[1], c[2], c[3]])
            };
            if n > 0x10ffff {
                return None;
            }
            out.push(n);
        }
        Some(out)
    };
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
    if let Some(rest) = b.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        return utf8_surrogatepass(rest);
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
    utf8_surrogatepass(b)
}

/// A container being filled while the parser reads its members.
enum Open {
    List(Vec<usize>),
    /// Members so far, where each key sits, and the key of the value being read.
    Dict(Vec<(PyStr, usize)>, HashMap<PyStr, usize>, PyStr),
}

/// `json.loads` on a decoded document, with Python's grammar: NaN and
/// Infinity allowed, duplicate keys keep the last value at the first place,
/// and RecursionError past the depth Python's scanner reaches.
struct PyJson<'a> {
    s: &'a [u32],
    i: usize,
    /// Arrays/objects deep it may go (see [`PyStack::json_depth`]).
    max: usize,
}

const fn cp(c: char) -> u32 {
    c as u32
}

impl PyJson<'_> {
    fn peek(&self) -> Option<u32> {
        self.s.get(self.i).copied()
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(c) if c == cp(' ') || c == cp('\t') || c == cp('\n') || c == cp('\r')) {
            self.i += 1;
        }
    }

    fn starts(&self, lit: &str) -> bool {
        let n = lit.len();
        self.i + n <= self.s.len() && self.s[self.i..self.i + n].iter().copied().eq(lit.chars().map(|c| c as u32))
    }

    fn eat(&mut self, c: char) -> Result<(), JsonFail> {
        if self.peek() == Some(cp(c)) {
            self.i += 1;
            Ok(())
        } else {
            Err(JsonFail::Value)
        }
    }

    /// The key of an object member and its colon, up to the value.
    fn key(&mut self) -> Result<PyStr, JsonFail> {
        self.eat('"')?;
        let k = self.string()?;
        self.ws();
        self.eat(':')?;
        self.ws();
        Ok(k)
    }

    fn document(&mut self) -> Result<Doc, JsonFail> {
        let mut nodes: Vec<Node> = Vec::new();
        let mut stack: Vec<Open> = Vec::new();
        loop {
            // A value starts here.
            let c = self.peek().ok_or(JsonFail::Value)?;
            let node = if c == cp('[') || c == cp('{') {
                if stack.len() + 1 > self.max {
                    let what = if c == cp('[') { "array" } else { "object" };
                    return Err(JsonFail::Recursion(format!(
                        "maximum recursion depth exceeded while decoding a JSON {what} from a unicode string"
                    )));
                }
                self.i += 1;
                self.ws();
                if c == cp('[') {
                    if self.peek() == Some(cp(']')) {
                        self.i += 1;
                        Node::List(Vec::new())
                    } else {
                        stack.push(Open::List(Vec::new()));
                        continue;
                    }
                } else if self.peek() == Some(cp('}')) {
                    self.i += 1;
                    Node::Dict(Vec::new())
                } else {
                    let k = self.key()?;
                    stack.push(Open::Dict(Vec::new(), HashMap::new(), k));
                    continue;
                }
            } else if c == cp('"') {
                self.i += 1;
                Node::Str(self.string()?)
            } else {
                self.scalar()?
            };
            nodes.push(node);
            let mut v = nodes.len() - 1;
            // Hand it to the container it is in, closing what ends here.
            loop {
                match stack.last_mut() {
                    None => {
                        return Ok(Doc { nodes, root: v });
                    }
                    Some(Open::List(items)) => {
                        items.push(v);
                        self.ws();
                        if self.peek() == Some(cp(',')) {
                            self.i += 1;
                            self.ws();
                            break;
                        }
                        self.eat(']')?;
                    }
                    Some(Open::Dict(items, at, key)) => {
                        let k = std::mem::take(key);
                        match at.get(&k) {
                            Some(&slot) => items[slot].1 = v,
                            None => {
                                at.insert(k.clone(), items.len());
                                items.push((k, v));
                            }
                        }
                        self.ws();
                        if self.peek() == Some(cp(',')) {
                            self.i += 1;
                            self.ws();
                            *key = self.key()?;
                            break;
                        }
                        self.eat('}')?;
                    }
                }
                let node = match stack.pop() {
                    Some(Open::List(items)) => Node::List(items),
                    Some(Open::Dict(items, _, _)) => Node::Dict(items),
                    None => return Err(JsonFail::Value),
                };
                nodes.push(node);
                v = nodes.len() - 1;
            }
        }
    }

    fn scalar(&mut self) -> Result<Node, JsonFail> {
        for (lit, node) in [
            ("null", Node::None),
            ("true", Node::Bool(true)),
            ("false", Node::Bool(false)),
            ("NaN", Node::Float(f64::NAN)),
            ("Infinity", Node::Float(f64::INFINITY)),
            ("-Infinity", Node::Float(f64::NEG_INFINITY)),
        ] {
            if self.starts(lit) {
                self.i += lit.len();
                return Ok(node);
            }
        }
        self.number().ok_or(JsonFail::Value)
    }

    /// NUMBER_RE: `(-?(?:0|[1-9][0-9]*))(\.[0-9]+)?([eE][-+]?[0-9]+)?`
    fn number(&mut self) -> Option<Node> {
        let start = self.i;
        let digit = |c: Option<&u32>| c.is_some_and(|&c| (cp('0')..=cp('9')).contains(&c));
        if self.peek() == Some(cp('-')) {
            self.i += 1;
        }
        match self.peek() {
            Some(c) if c == cp('0') => self.i += 1,
            Some(c) if (cp('1')..=cp('9')).contains(&c) => {
                while digit(self.s.get(self.i)) {
                    self.i += 1;
                }
            }
            _ => {
                self.i = start;
                return None;
            }
        }
        let mut is_float = false;
        if self.peek() == Some(cp('.')) && digit(self.s.get(self.i + 1)) {
            self.i += 1;
            while digit(self.s.get(self.i)) {
                self.i += 1;
            }
            is_float = true;
        }
        if matches!(self.peek(), Some(c) if c == cp('e') || c == cp('E')) {
            let save = self.i;
            self.i += 1;
            if matches!(self.peek(), Some(c) if c == cp('+') || c == cp('-')) {
                self.i += 1;
            }
            if digit(self.s.get(self.i)) {
                while digit(self.s.get(self.i)) {
                    self.i += 1;
                }
                is_float = true;
            } else {
                self.i = save;
            }
        }
        // All ASCII by construction.
        let text: String = self.s[start..self.i].iter().filter_map(|&c| char::from_u32(c)).collect();
        if is_float {
            return text.parse::<f64>().ok().map(Node::Float);
        }
        // int() refuses more than 4300 digits (ValueError, so: not JSON).
        if text.trim_start_matches('-').len() > 4300 {
            return None;
        }
        match text.parse::<i128>() {
            Ok(i) => Some(Node::Int(i)),
            Err(_) => Some(Node::BigInt(text)),
        }
    }

    /// After the opening quote; strict, as `json.loads` is by default.
    fn string(&mut self) -> Result<PyStr, JsonFail> {
        let mut out = Vec::new();
        loop {
            let c = self.peek().ok_or(JsonFail::Value)?;
            self.i += 1;
            if c == cp('"') {
                return Ok(out);
            }
            if c < 0x20 {
                return Err(JsonFail::Value);
            }
            if c != cp('\\') {
                out.push(c);
                continue;
            }
            let e = self.peek().and_then(char::from_u32).ok_or(JsonFail::Value)?;
            self.i += 1;
            let plain = match e {
                '"' => '"',
                '\\' => '\\',
                '/' => '/',
                'b' => '\u{8}',
                'f' => '\u{c}',
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                'u' => {
                    let u = self.hex4()?;
                    if (0xd800..0xdc00).contains(&u) && self.starts("\\u") {
                        let save = self.i;
                        self.i += 2;
                        let lo = self.hex4()?;
                        if (0xdc00..0xe000).contains(&lo) {
                            out.push(0x10000 + ((u - 0xd800) << 10) + (lo - 0xdc00));
                            continue;
                        }
                        self.i = save;
                    }
                    // A lone surrogate stays one, as in a Python str.
                    out.push(u);
                    continue;
                }
                _ => return Err(JsonFail::Value),
            };
            out.push(plain as u32);
        }
    }

    fn hex4(&mut self) -> Result<u32, JsonFail> {
        if self.i + 4 > self.s.len() {
            return Err(JsonFail::Value);
        }
        let mut v = 0;
        for &c in &self.s[self.i..self.i + 4] {
            v = v * 16 + char::from_u32(c).and_then(|c| c.to_digit(16)).ok_or(JsonFail::Value)?;
        }
        self.i += 4;
        Ok(v)
    }
}

/// `json.loads(raw)`, nesting at most `max` deep.
fn py_json_loads(raw: &[u8], max: usize) -> Result<Doc, JsonFail> {
    let text = py_json_decode(raw).ok_or(JsonFail::Value)?;
    let mut p = PyJson { s: &text, i: 0, max };
    p.ws();
    let v = p.document()?;
    p.ws();
    if p.i != text.len() {
        return Err(JsonFail::Value);
    }
    Ok(v)
}

/// Python 3.14's `PurePath.suffix`.
fn py_suffix(path: &Path) -> Vec<u8> {
    let name = py_name(path);
    match name.iter().rposition(|&c| c == b'.') {
        Some(i) if i > 0 => name[i..].to_vec(),
        _ => Vec::new(),
    }
}

/// Content for comparison: parsed when JSON, so formatting is ignored.
///
/// `like` is the file whose type decides -- a backup is named
/// `shell.json.bak.<epoch>`, so its own suffix says nothing.
///
/// Only a ValueError falls back to the bytes; a RecursionError (a document
/// deeper than `json` can parse) is the caller's, as in Python.
fn normal(path: &Path, like: Option<&Path>, at: PyStack) -> Result<Option<Normal>, String> {
    let Ok(raw) = std::fs::read(path) else {
        return Ok(None);
    };
    if py_suffix(like.unwrap_or(path)) == b".json" {
        match py_json_loads(&raw, at.json_depth()) {
            Ok(doc) if matches!(doc.top(), Node::None) => return Ok(None),
            Ok(doc) => return Ok(Some(Normal::Json(doc))),
            Err(JsonFail::Recursion(e)) => return Err(e),
            Err(JsonFail::Value) => {}
        }
    }
    Ok(Some(Normal::Raw(py_bytes_strip(&raw))))
}

/// Every "id" in a JSON document, in order: widgets, plugins, entries.
///
/// Python's `walk` recurses once per level, scalars included, and raises
/// RecursionError past the interpreter's limit: `max` levels, which the
/// caller works out from the frames above (see [`PyStack::ids_depth`]).
fn ids(doc: &Doc, max: usize) -> Result<Vec<PyStr>, String> {
    let id = pystr("id");
    let mut out = Vec::new();
    let mut todo = vec![(doc.root, 1usize)];
    while let Some((n, depth)) = todo.pop() {
        if depth > max {
            return Err("maximum recursion depth exceeded".to_string());
        }
        match &doc.nodes[n] {
            Node::Dict(items) => {
                if let Some((_, v)) = items.iter().find(|(k, _)| *k == id) {
                    if let Node::Str(s) = &doc.nodes[*v] {
                        out.push(s.clone());
                    }
                }
                todo.extend(items.iter().rev().map(|(_, v)| (*v, depth + 1)));
            }
            Node::List(items) => todo.extend(items.iter().rev().map(|v| (*v, depth + 1))),
            _ => {}
        }
    }
    Ok(out)
}

/// What the reset removed, in terms a person recognises.
///
/// `at` is where `_lost` itself is called from.
fn lost(current: &Path, backup: &Path, at: PyStack) -> Result<Vec<PyStr>, String> {
    let (a, b) = (normal(backup, Some(current), at)?, normal(current, None, at)?);
    let max = PyStack::ids_depth(at.frames + 1);
    let container = |n: &Option<Normal>| match n {
        Some(Normal::Json(d)) if matches!(d.top(), Node::Dict(_) | Node::List(_)) => true,
        _ => false,
    };
    if let (true, true, Some(Normal::Json(a)), Some(Normal::Json(b))) = (container(&a), container(&b), &a, &b) {
        let kept: HashSet<PyStr> = ids(b, max)?.into_iter().collect();
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for i in ids(a, max)? {
            if !kept.contains(&i) && !seen.contains(&i) {
                seen.insert(i.clone());
                out.push(i);
            }
        }
        return Ok(out);
    }
    Ok(Vec::new())
}

/// Every regular file under `root`, `depth` directory levels deep at most,
/// not following symlinks; unreadable entries are skipped. (Python yields
/// lazily; the order is `os.scandir`'s.) Also used by `history::reset_events`. (`_walk` in the Python.)
pub fn walk(root: &Path, depth: u32) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk_into(root, depth, &mut out);
    out
}

fn walk_into(root: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    // `list(os.scandir(root))`: an error part way through loses the lot.
    let entries: Vec<std::fs::DirEntry> = match std::fs::read_dir(root) {
        Ok(it) => match it.collect::<Result<Vec<_>, _>>() {
            Ok(v) => v,
            Err(_) => return,
        },
        Err(_) => return,
    };
    for e in entries {
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            if depth > 1 {
                walk_into(&e.path(), depth - 1, out);
            }
        } else if ft.is_file() {
            out.push(e.path());
        }
    }
}

static DIGIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d$").unwrap());

/// `float()` of the digits `\d` matched -- which, as in Python, may be any
/// Unicode decimal digits. Unicode keeps each script's 0-9 contiguous, so a
/// digit's value is its distance from the start of its run, mod 10.
fn py_digits_value(ts: &str) -> Option<f64> {
    let mut text = String::new();
    for c in ts.chars() {
        if c.is_ascii_digit() {
            text.push(c);
            continue;
        }
        let mut n = 0u32;
        let mut cp = c as u32;
        while cp > 0 {
            match char::from_u32(cp - 1) {
                Some(p) if DIGIT.is_match(p.encode_utf8(&mut [0; 4])) => {
                    n += 1;
                    cp -= 1;
                }
                _ => break,
            }
        }
        text.push(char::from_digit(n % 10, 10)?);
    }
    text.parse().ok()
}

/// How pathlib orders paths: `str(path).split("/")`, each part compared by
/// code point -- an undecodable byte (U+DCxx) sorts below `豈` although its
/// byte 0xff sorts above that character's UTF-8.
fn py_sort_key(p: &Path) -> Vec<PyStr> {
    py_str(p.as_os_str().as_bytes()).split(|&c| c == b'/').map(surrogateescape).collect()
}

/// `now` defaults to the current time (`None`, or 0 as in Python's `now or`).
///
/// Python raises out of `resets` in two places nothing catches: a backup
/// named `..bak.<epoch>` (`with_name(".")`, a ValueError) and a document too
/// deep to parse or walk (RecursionError).
///
/// Called from where app.py's `_config_check` calls it; see
/// [`try_resets_at`] for the others.
pub fn try_resets(now: Option<f64>) -> Result<Vec<Reset>, String> {
    try_resets_at(now, PyStack::WORKER)
}

/// [`try_resets`] as called from `at`, which decides how deep a document
/// may be before Python raises.
pub fn try_resets_at(now: Option<f64>, at: PyStack) -> Result<Vec<Reset>, String> {
    Ok(resets_exact(now, at)?.into_iter().map(|(r, _)| r).collect())
}

/// Each reset with its lost ids as Python's strs (`Reset.lost` is their
/// display form, where a lone surrogate cannot survive).
fn resets_exact(now: Option<f64>, at: PyStack) -> Result<Vec<(Reset, Vec<PyStr>)>, String> {
    let Some(defaults) = defaults_root() else {
        return Ok(Vec::new());
    };
    let home = config_home();
    let now = match now {
        Some(n) if n != 0.0 => n,
        _ => crate::util::now(),
    };
    let cutoff = now - (WINDOW_DAYS * 86400) as f64;

    // Keyed the way Python's dict of Paths compares and `sorted` orders them.
    let mut newest: BTreeMap<Vec<PyStr>, (f64, PathBuf, PathBuf)> = BTreeMap::new();
    for f in walk(&home, MAX_DEPTH) {
        let Some(fname) = f.file_name() else { continue };
        let raw = fname.as_bytes();
        let Some(m) = BAK_BYTES.captures(raw) else {
            continue;
        };
        let Some(ts) = std::str::from_utf8(&m["ts"]).ok().and_then(py_digits_value) else {
            continue;
        };
        if ts < cutoff {
            continue;
        }
        let base = py_with_name(&f, &m["base"])?;
        let key = py_sort_key(&base);
        let replace = match newest.get(&key) {
            None => true,
            Some((t, _, _)) => ts > *t,
        };
        if replace {
            newest.insert(key, (ts, f, base));
        }
    }

    let mut out = Vec::new();
    for (_, (ts, bak, base)) in newest {
        let Ok(rel) = base.strip_prefix(&home) else { continue };
        let shipped = defaults.join(rel);
        if !(base.is_file() && shipped.is_file()) {
            continue;
        }
        let (current, theirs, stock) =
            (normal(&base, None, at)?, normal(&bak, Some(&base), at)?, normal(&shipped, Some(&base), at)?);
        if current.is_none() || theirs.is_none() {
            continue;
        }
        if normal_eq(&current, &stock) && !normal_eq(&theirs, &stock) {
            let gone = lost(&base, &bak, PyStack { frames: at.frames + 1, ..at })?;
            let shown = gone.iter().map(|i| pystr_lossy(i)).collect();
            out.push((Reset::new(base, bak, ts, shown), gone));
        }
    }
    Ok(out)
}

/// `shutil.copy2`: the content, then the times, extended attributes and
/// permission bits -- all set by path, as `copystat` does, so a read-only
/// source (whose mode the copy takes) does not stop the times being set.
/// Errors read as Python's OSError does.
fn copy2(src: &Path, dst: &Path) -> Result<(), String> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
    // restore passes Paths; a directory `dst` becomes os.path.join's str.
    let dst_buf;
    let (dst, dst_str) = if dst.is_dir() {
        dst_buf = dst.join(src.file_name().unwrap_or_default());
        (dst_buf.as_path(), true)
    } else {
        (dst, false)
    };
    let shown = |p: &Path, is_str: bool| {
        let text = surrogateescape(&py_str(p.as_os_str().as_bytes()));
        if is_str { (py_repr(&text), text) } else { (format!("PosixPath({})", py_repr(&text)), text) }
    };
    // copyfile: refuses to copy a file onto itself, names each by its repr.
    if let (Ok(a), Ok(b)) = (std::fs::metadata(src), std::fs::metadata(dst)) {
        if a.dev() == b.dev() && a.ino() == b.ino() {
            return Err(format!("{} and {} are the same file", shown(src, false).0, shown(dst, dst_str).0));
        }
    }
    // ...and to read from or write to a named pipe (SpecialFileError), which
    // would block; checked before anything is opened.
    for (fname, is_str) in [(src, false), (dst, dst_str)] {
        if let Ok(st) = std::fs::metadata(fname) {
            if st.file_type().is_fifo() {
                return Err(format!("`{}` is a named pipe", pystr_lossy(&shown(fname, is_str).1)));
            }
        }
    }
    let mut fsrc = std::fs::File::open(src).map_err(|e| os_error(&e, src))?;
    let mut fdst = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o666)
        .open(dst)
        .map_err(|e| os_error(&e, dst))?;
    // A failed sendfile names both files.
    std::io::copy(&mut fsrc, &mut fdst)
        .map_err(|e| os_error_on(&e, &format!("{} -> {}", path_repr(src), path_repr(dst))))?;
    drop(fdst);
    // copystat
    let st = std::fs::metadata(src).map_err(|e| os_error(&e, src))?;
    let ts = |s: i64, ns: i64| rustix::fs::Timespec { tv_sec: s, tv_nsec: ns as _ };
    let times = rustix::fs::Timestamps {
        last_access: ts(st.atime(), st.atime_nsec()),
        last_modification: ts(st.mtime(), st.mtime_nsec()),
    };
    rustix::fs::utimensat(rustix::fs::CWD, dst, &times, rustix::fs::AtFlags::empty())
        .map_err(|e| os_error(&std::io::Error::from(e), dst))?;
    copy_xattr(src, dst)?;
    std::fs::set_permissions(dst, std::fs::Permissions::from_mode(st.mode() & 0o7777))
        .map_err(|e| os_error(&e, dst))?;
    Ok(())
}

/// `shutil._copyxattr`: what the filesystem does not support is skipped.
fn copy_xattr(src: &Path, dst: &Path) -> Result<(), String> {
    use rustix::io::Errno;
    let list = |p: &Path| -> rustix::io::Result<Vec<u8>> {
        let n = rustix::fs::listxattr(p, &mut [0u8; 0][..])?;
        let mut buf = vec![0u8; n];
        let n = rustix::fs::listxattr(p, &mut buf[..])?;
        buf.truncate(n);
        Ok(buf)
    };
    let names = match list(src) {
        Ok(n) => n,
        Err(e) if [Errno::NOTSUP, Errno::NODATA, Errno::INVAL].contains(&e) => return Ok(()),
        Err(e) => return Err(os_error(&std::io::Error::from(e), src)),
    };
    for name in names.split(|&c| c == 0).filter(|n| !n.is_empty()) {
        let name = OsStr::from_bytes(name);
        let copy = || -> rustix::io::Result<()> {
            let n = rustix::fs::getxattr(src, name, &mut [0u8; 0][..])?;
            let mut value = vec![0u8; n];
            let n = rustix::fs::getxattr(src, name, &mut value[..])?;
            value.truncate(n);
            rustix::fs::setxattr(dst, name, &value, rustix::fs::XattrFlags::empty())
        };
        match copy() {
            Ok(()) => {}
            Err(e) if [Errno::PERM, Errno::NOTSUP, Errno::NODATA, Errno::INVAL, Errno::ACCESS].contains(&e) => {}
            Err(e) => return Err(os_error(&std::io::Error::from(e), dst)),
        }
    }
    Ok(())
}

fn file_name(p: &Path) -> String {
    String::from_utf8_lossy(&py_name(p)).into_owned()
}

/// Put the user's version back, keeping the defaults it replaces.
///
/// Errors (a failed copy) go to the caller, as the Python raises.
pub fn restore(reset: &Reset, log: crate::tasks::LogFn<'_>) -> Result<(), String> {
    // The name's own bytes, which need not be UTF-8.
    let mut keep_name = py_name(&reset.path);
    keep_name.extend_from_slice(format!(".defaults.{}", crate::util::now() as i64).as_bytes());
    let keep = py_with_name(&reset.path, &keep_name)?;
    copy2(&reset.path, &keep)?;
    copy2(&reset.backup, &reset.path)?;
    log(&format!("restored {} from {}", reset.name(), file_name(&reset.backup)));
    crate::history::record_checked(
        crate::history::ALMANAC,
        &format!("{} restored", reset.name()),
        &format!("From {}; the defaults were kept as {}", file_name(&reset.backup), file_name(&keep)),
        "",
        &[],
    )?;
    log(&format!("the defaults it replaced are in {}", file_name(&keep)));
    Ok(())
}

/// `shutil.which(name)`: `os.access(X_OK)`, so the execute bit that counts
/// is ours, and with PATH unset the C library's default search path
/// (`os.confstr("CS_PATH")`, `/bin:/usr/bin` in glibc).
fn py_which(name: &str) -> Option<PathBuf> {
    path_candidates(name).into_iter().find(|file| {
        file.exists()
            && rustix::fs::access(file.as_path(), rustix::fs::Access::EXISTS | rustix::fs::Access::EXEC_OK).is_ok()
            && !file.is_dir()
    })
}

/// `name` in each directory of PATH, as `shutil.which` and exec look.
fn path_candidates(name: &str) -> Vec<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_else(|| OsString::from("/bin:/usr/bin"));
    // PATH='' doesn't match, whereas PATH=':' looks in the current directory.
    if path.is_empty() {
        return Vec::new();
    }
    path.as_bytes()
        .split(|&c| c == b':')
        .map(|dir| if dir.is_empty() { PathBuf::from(name) } else { Path::new(OsStr::from_bytes(dir)).join(name) })
        .collect()
}

/// Commands that make a running desktop pick a restored file up.
///
/// Hyprland watches its own config. Omarchy's shell does not re-read
/// shell.json, so it has to be restarted -- with the restart command, never
/// the refresh one, which is what reset it in the first place.
pub fn reload_for<I, P>(paths: I) -> Vec<Vec<String>>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    reload_for_with(paths, |cmd| py_which(cmd).is_some())
}

/// `reload_for`, asking `installed` whether a command is on PATH.
fn reload_for_with<I, P>(paths: I, installed: impl Fn(&str) -> bool) -> Vec<Vec<String>>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    let mut cmds = Vec::new();
    let names: HashSet<Vec<u8>> = paths.into_iter().map(|p| py_name(p.as_ref())).collect();
    if names.contains(&b"shell.json"[..]) && installed("omarchy-restart-shell") {
        cmds.push(vec!["omarchy-restart-shell".to_string()]);
    }
    cmds
}

/// `time.strftime("%a %H:%M", time.localtime(when))`. The day name follows
/// LC_TIME, which the app sets from the environment at start (i18n's
/// setlocale; gtk::init here); GLib's formatter reads the same C locale,
/// chrono only knows English.
fn local_when(when: f64) -> String {
    use chrono::TimeZone;
    let secs = when.floor() as i64;
    if let Some(s) = gtk::glib::DateTime::from_unix_local(secs)
        .ok()
        .and_then(|t| t.format("%a %H:%M").ok())
        .filter(|s| !s.is_empty())
    {
        return s.to_string();
    }
    match chrono::Local.timestamp_opt(secs, 0) {
        chrono::LocalResult::Single(t) | chrono::LocalResult::Ambiguous(t, _) => t.format("%a %H:%M").to_string(),
        chrono::LocalResult::None => String::new(),
    }
}

/// `repr(list_of_str)`, as `str(cmd)` shows a command in Python's messages.
fn py_list_repr(items: &[String]) -> String {
    let parts: Vec<String> = items.iter().map(|s| py_repr(&pystr(s))).collect();
    format!("[{}]", parts.join(", "))
}

/// The Health check row for this.
///
/// Where Python's `finding()` raises (see [`try_resets`]), the maintenance
/// run turns that into a "Check failed" row; this returns that row.
pub fn finding() -> crate::maintenance::Finding {
    match try_finding() {
        Ok(f) => f,
        // maintenance.run_all's `except Exception` for this check.
        Err(exc) => crate::maintenance::Finding {
            id: "config_resets".into(),
            title: "Config Resets".into(),
            detail: format!("Check failed: {exc}"),
            severity: crate::maintenance::INFO.into(),
            ..Default::default()
        },
    }
}

/// [`finding`], with the error Python would raise.
///
/// Called from the Health check, as `maintenance.run_all` calls it.
pub fn try_finding() -> Result<crate::maintenance::Finding, String> {
    try_finding_at(PyStack { frames: PyStack::HEALTH.frames - 1, ..PyStack::HEALTH })
}

/// [`try_finding`] as called from `at` (frames above `finding`).
pub fn try_finding_at(at: PyStack) -> Result<crate::maintenance::Finding, String> {
    use crate::maintenance as m;

    let found = try_resets_at(None, PyStack { frames: at.frames + 1, ..at })?;
    if found.is_empty() {
        return Ok(m::Finding {
            id: "config-reset".into(),
            title: "Settings reset to defaults".into(),
            detail: "No config file has been put back to its defaults in the last week.".into(),
            severity: m::OK.into(),
            ..Default::default()
        });
    }

    let mut parts = Vec::new();
    let mut items = Vec::new();
    for r in &found {
        let when = local_when(r.when);
        let lost = if r.lost.is_empty() { String::new() } else { format!(", dropping {} entries", r.lost.len()) };
        parts.push(format!("{} at {when}{lost}", r.name()));
        items.push(format!("{} — your version is {}", r.name(), file_name(&r.backup)));
        items.extend(r.lost.iter().take(30).map(|i| format!("    removed: {i}")));
    }

    let count = found.len() as i64;
    let fix = m::Fix::func(move |log| {
        for r in &found {
            restore(r, log)?;
        }
        for cmd in reload_for(found.iter().map(|r| r.path.clone())) {
            log(&format!("$ {}", cmd.join(" ")));
            let argv: Vec<&str> = cmd.iter().map(String::as_str).collect();
            let out = crate::util::run(&argv, 60);
            // subprocess.run raises on a timeout or a program it cannot start.
            if out.timed_out {
                return Err(format!("Command '{}' timed out after 60 seconds", py_list_repr(&cmd)));
            }
            if out.failed_to_start {
                // util::run does not say why; exec fails with EACCES where
                // a file of that name is on PATH but not ours to run, with
                // ENOENT where there is none.
                let prog = py_repr(&pystr(&cmd[0]));
                let there = path_candidates(&cmd[0]).iter().any(|f| f.exists() && !f.is_dir());
                return Err(if there {
                    format!("[Errno 13] Permission denied: {prog}")
                } else {
                    format!("[Errno 2] No such file or directory: {prog}")
                });
            }
        }
        Ok(())
    });

    Ok(m::Finding {
        id: "config-reset".into(),
        title: "Settings reset to defaults".into(),
        detail: format!(
            "Replaced with the shipped defaults: {}. Your own version was kept as a backup.",
            parts.join("; ")
        ),
        severity: m::WARN.into(),
        count,
        items,
        fix_label: "Restore my settings".into(),
        fix: Some(fix),
        fix_note: "Copies your version back from the backup. The defaults it \
                   replaces are kept alongside, so this can be undone. To keep \
                   the defaults instead, leave this; it stops being offered a \
                   week after the reset."
            .into(),
        ..Default::default()
    })
}

#[cfg(test)]
/// The parsed document as serde JSON, for the parity hook. Built without
/// recursion, as deep as the document is (Python's limit, which follows
/// `ulimit -s`, allows 100000 levels on a 16 MiB stack): a container's
/// children sit before it in the arena, so one pass in index order builds
/// each node from its children's finished values.
fn doc_json(d: &Doc, n: usize) -> serde_json::Value {
    use serde_json::Value;
    // Only the nodes reachable from `n` (a duplicate key leaves its first
    // value in the arena, unreferenced).
    let mut wanted = vec![false; d.nodes.len()];
    let mut todo = vec![n];
    while let Some(i) = todo.pop() {
        wanted[i] = true;
        match &d.nodes[i] {
            Node::List(items) => todo.extend(items),
            Node::Dict(items) => todo.extend(items.iter().map(|(_, v)| *v)),
            _ => {}
        }
    }
    let mut done: Vec<Option<Value>> = vec![None; d.nodes.len()];
    for i in 0..=n {
        if !wanted[i] {
            continue;
        }
        let mut take = |c: usize| done[c].take().unwrap_or(Value::Null);
        let v = match &d.nodes[i] {
            Node::None => Value::Null,
            Node::Bool(b) => Value::Bool(*b),
            Node::Int(i) => match i64::try_from(*i) {
                Ok(n) => Value::from(n),
                Err(_) => match u64::try_from(*i) {
                    Ok(n) => Value::from(n),
                    Err(_) => Value::from(*i as f64),
                },
            },
            Node::BigInt(s) => Value::from(s.parse::<f64>().unwrap_or(0.0)),
            Node::Float(f) => Value::from(*f),
            Node::Str(s) => Value::String(pystr_lossy(s)),
            Node::List(items) => Value::Array(items.iter().map(|c| take(*c)).collect()),
            Node::Dict(items) => Value::Object(items.iter().map(|(k, c)| (pystr_lossy(k), take(*c))).collect()),
        };
        done[i] = Some(v);
    }
    done[n].take().unwrap_or(Value::Null)
}


#[cfg(test)]
mod tests {
    use super::*;

    // The fixture lives in the environment (XDG_CONFIG_HOME, OMARCHY_PATH),
    // which is process-wide; one test at a time.

    fn dumps(v: &serde_json::Value, indent: Option<usize>) -> String {
        crate::util::dumps(v, indent, false)
    }

    struct ConfigReset {
        _tmp: tempfile::TempDir,
        _guard: std::sync::MutexGuard<'static, ()>,
        tmp: PathBuf,
        cfg: PathBuf,
        bak: PathBuf,
        default: serde_json::Value,
        mine: serde_json::Value,
        saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }

    impl ConfigReset {
        fn set_up() -> ConfigReset {
            let guard = crate::util::TEST_ENV.lock().unwrap_or_else(|e| e.into_inner());
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().to_path_buf();
            let home = root.join("config");
            let shipped = root.join("omarchy");
            std::fs::create_dir_all(home.join("omarchy")).unwrap();
            std::fs::create_dir_all(shipped.join("config/omarchy")).unwrap();
            let default = serde_json::json!({"bar": [{"id": "omarchy.clock"}]});
            let mine = serde_json::json!({"bar": [{"id": "omarchy.clock"}, {"id": "my.widget"},
                                                  {"id": "my.tray"}]});
            std::fs::write(shipped.join("config/omarchy/shell.json"), dumps(&default, None)).unwrap();
            let cfg = home.join("omarchy/shell.json");
            std::fs::write(&cfg, dumps(&default, Some(2))).unwrap();
            let bak = home.join(format!("omarchy/shell.json.bak.{}", crate::util::now() as i64 - 60));
            std::fs::write(&bak, dumps(&mine, None)).unwrap();
            let mut saved = Vec::new();
            for (k, v) in [("XDG_CONFIG_HOME", home.clone()), ("OMARCHY_PATH", shipped.clone())] {
                saved.push((k, std::env::var_os(k)));
                std::env::set_var(k, v);
            }
            ConfigReset { _tmp: tmp, _guard: guard, tmp: root, cfg, bak, default, mine, saved }
        }
    }

    impl Drop for ConfigReset {
        fn drop(&mut self) {
            for (k, v) in self.saved.drain(..) {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    #[test]
    fn test_a_reset_is_found_with_what_it_removed() {
        let t = ConfigReset::set_up();
        let found = try_resets(None).unwrap();
        assert_eq!(found.len(), 1);
        let r = &found[0];
        assert_eq!(r.path, t.cfg);
        assert_eq!(r.lost, vec!["my.widget".to_string(), "my.tray".to_string()]);
    }

    #[test]
    fn test_restoring_brings_it_back_and_keeps_the_defaults() {
        let t = ConfigReset::set_up();
        let state = tempfile::tempdir().unwrap();
        let old_state = std::env::var_os("XDG_STATE_HOME");
        std::env::set_var("XDG_STATE_HOME", state.path());
        let found = try_resets(None).unwrap();
        assert_eq!(found.len(), 1);
        let res = restore(&found[0], &|_m: &str| {});
        match old_state {
            Some(v) => std::env::set_var("XDG_STATE_HOME", v),
            None => std::env::remove_var("XDG_STATE_HOME"),
        }
        res.unwrap();
        let back: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&t.cfg).unwrap()).unwrap();
        assert_eq!(back, t.mine);
        let kept = std::fs::read_dir(t.cfg.parent().unwrap())
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().starts_with("shell.json.defaults."));
        assert!(kept);
        assert_eq!(try_resets(None).unwrap(), vec![]);
    }

    #[test]
    fn test_a_backup_that_was_itself_the_defaults_is_not_a_reset() {
        let t = ConfigReset::set_up();
        // Formatted differently, same content: nothing was lost.
        std::fs::write(&t.bak, dumps(&t.default, Some(4))).unwrap();
        assert_eq!(try_resets(None).unwrap(), vec![]);
    }

    #[test]
    fn test_a_customised_file_is_not_a_reset() {
        let t = ConfigReset::set_up();
        std::fs::write(&t.cfg, dumps(&t.mine, None)).unwrap();
        assert_eq!(try_resets(None).unwrap(), vec![]);
    }

    #[test]
    fn test_an_old_backup_is_let_go() {
        let t = ConfigReset::set_up();
        let old = t.cfg.with_file_name("shell.json.bak.1000000000");
        std::fs::rename(&t.bak, old).unwrap();
        assert_eq!(try_resets(None).unwrap(), vec![]);
    }

    #[test]
    fn test_without_shipped_defaults_nothing_is_claimed() {
        let t = ConfigReset::set_up();
        let old = std::env::var_os("OMARCHY_PATH");
        std::env::set_var("OMARCHY_PATH", t.tmp.join("none"));
        let found = try_resets(None).unwrap();
        match old {
            Some(v) => std::env::set_var("OMARCHY_PATH", v),
            None => std::env::remove_var("OMARCHY_PATH"),
        }
        assert_eq!(found, vec![]);
    }

    #[test]
    fn test_restoring_shell_json_restarts_never_refreshes() {
        // The lookup is passed in rather than PATH swapped for a fake one:
        // every other test spawning git or pacman meanwhile would fail.
        let t = ConfigReset::set_up();
        let cmds = reload_for_with([&t.cfg], |cmd| cmd == "omarchy-restart-shell");
        assert_eq!(cmds, vec![vec!["omarchy-restart-shell".to_string()]]);
        assert_eq!(reload_for_with([&t.cfg], |_| false), Vec::<Vec<String>>::new());
    }

    #[test]
    fn python_equality_of_parsed_json() {
        let p = |s: &str| py_json_loads(s.as_bytes(), usize::MAX).ok().unwrap();
        let eq = |a: &str, b: &str| py_eq(&p(a), &p(b));
        assert!(eq("{\"a\": 1, \"b\": [true]}", "{\"b\":[1.0],\"a\":1}"));
        assert!(!eq("NaN", "NaN"));
        assert!(eq("[NaN]", "[NaN]"));
        assert!(eq("{\"a\":1,\"a\":2}", "{\"a\":2}"));
        assert!(matches!(py_json_loads(b"[1,]", usize::MAX), Err(JsonFail::Value)));
        let got = ids(&p("{\"z\":{\"id\":\"b\"},\"id\":\"a\"}"), usize::MAX).unwrap();
        assert_eq!(got, vec![pystr("a"), pystr("b")]);
    }

    #[test]
    fn lone_surrogates_stay_distinct() {
        let p = |b: &[u8]| py_json_loads(b, usize::MAX).ok().unwrap();
        assert!(!py_eq(&p(br#""\ud800""#), &p(br#""\udbff""#)));
        assert!(!py_eq(&p(b"\"\xed\xa0\x80\""), &p(b"\"\xed\xaf\xbf\"")));
        assert!(py_eq(&p(b"\"\xed\xa0\x80\""), &p(br#""\ud800""#)));
        // an escaped pair is one character, the encoded pair two
        assert!(py_eq(&p(br#""\ud83d\ude00""#), &p("\"\u{1f600}\"".as_bytes())));
        assert!(!py_eq(&p(b"\"\xed\xa0\xbd\xed\xb8\x80\""), &p(br#""\ud83d\ude00""#)));
        assert_eq!(surrogateescape(b"a\xffb"), vec![0x61, 0xdcff, 0x62]);
    }

    #[test]
    fn nesting_as_deep_as_python_parses() {
        let deep = |n: usize| format!("{}{}", "[".repeat(n), "]".repeat(n));
        let loads = |n: usize, at: PyStack| py_json_loads(deep(n).as_bytes(), at.json_depth());
        assert!(loads(3000, PyStack::WATCHER).is_ok());
        // a worker thread's depth is fixed by RLIMIT_STACK (8 MB: 52154)
        let t = PyStack::HEALTH.json_depth();
        assert!(loads(t, PyStack::HEALTH).is_ok());
        assert!(matches!(loads(t + 1, PyStack::HEALTH), Err(JsonFail::Recursion(_))));
        assert!(PyStack::WINDOW.json_depth() < t);
        // `_ids` shares the recursion limit with the frames above it: under
        // run.py's 6, `resets`, `_lost` and `_ids`, 991 levels fit
        let max = PyStack::ids_depth(6 + 2);
        assert_eq!(max, 991);
        let d = py_json_loads(deep(max).as_bytes(), usize::MAX).ok().unwrap();
        assert!(ids(&d, max).is_ok());
        let d = py_json_loads(deep(max + 1).as_bytes(), usize::MAX).ok().unwrap();
        assert!(ids(&d, max).is_err());
    }

    #[test]
    fn pathlib_relative_to_keeps_a_double_slash_root() {
        assert_eq!(py_relative_to(b"/tmp/cfg/a/b", b"/tmp/cfg").as_deref(), Some(&b"a/b"[..]));
        assert_eq!(py_relative_to(b"/tmp/cfg", b"/tmp/cfg/").as_deref(), Some(&b"."[..]));
        assert_eq!(py_relative_to(b"//tmp/cfg/a", b"/tmp/cfg"), None);
        assert_eq!(py_relative_to(b"/tmp/cfg/a", b"//tmp/cfg"), None);
        assert_eq!(py_relative_to(b"///tmp/cfg/a", b"/tmp/cfg").as_deref(), Some(&b"a"[..]));
        assert!(py_with_name(Path::new("/a/..bak.1"), b".").is_err());
    }

    #[test]
    fn a_document_as_deep_as_python_allows_converts_without_recursion() {
        // Python's limit grows with `ulimit -s` (100000 levels at 16 MiB);
        // the parity hook's conversion must not recurse with it.
        let n = 100_000;
        let raw = "[".repeat(n) + &"]".repeat(n);
        std::thread::Builder::new()
            .stack_size(256 << 10)
            .spawn(move || {
                let doc = py_json_loads(raw.as_bytes(), n + 1).ok().unwrap();
                let v = doc_json(&doc, doc.root);
                assert_eq!(crate::util::tree::value_depth(&v), n);
                crate::util::tree::drop_value(v);
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
