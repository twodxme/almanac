//! Port of `appimage.py`.
//!
//! AppImage update checking.
//!
//! Most AppImages embed their own update source in a `.upd_info` ELF section --
//! the same string AppImageUpdate uses. Reading it means the update source is
//! discovered from the file rather than configured by hand, which is the whole
//! difference between "tracked" and "managed".
//!
//! Only the GitHub forms are implemented. The zsync-over-http forms exist but
//! need a zsync client to be useful, and every AppImage seen here so far uses
//! gh-releases-*.
//!
//! Signature notes (callers adapt): the Python raises to its caller in a few
//! places these functions used to swallow, so `latest_github`, `check`,
//! `install_desktop_entry`, `remove_desktop_entry` and `vkey` return
//! `Result<_, String>` (the error is the Python exception, `"Type: message"`).
//! `LatestRelease::spec` is the registry's `update_spec` as JSON, stored as
//! given (a hand-written spec keeps every key it had).

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use num_bigint::BigInt;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ghcache, util, xdg};

/// HTTP header sent with every request (`UA = {"User-Agent": "almanac"}`).
pub const UA: (&str, &str) = ("User-Agent", "almanac");

/// `_VERSION`: the first dotted number in a tag or asset name.
pub const VERSION_PATTERN: &str = r"(\d+(?:\.\d+)+)";

/// Where an AppImage's updates come from: the parsed `.upd_info` string
/// (`gh-releases-zsync|owner|repo|release|pattern`), or the `update_spec`
/// a registry record carries.
///
/// In Python this is a plain dict; `release` and `pattern` may be missing
/// from a hand-written registry record (the Python falls back to "latest"
/// and "*.AppImage" with `.get`), so they are optional here and omitted
/// from JSON when absent, keeping the registry file identical.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateSpec {
    pub kind: String,
    pub owner: String,
    pub repo: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
}

/// Newest release plus the asset matching the AppImage's own pattern
/// (the non-empty dict `latest_github` / `check` return). The Python's
/// empty `{}` is `None` at the call site.
///
/// `spec` is only set by `check` (`latest["spec"] = spec`), and is omitted
/// from JSON otherwise. This value is what `inventory` stores in a registry
/// record's `"latest"`, so its JSON shape must match the Python dict. It is
/// the spec exactly as `check` found it (the record's own `update_spec`
/// dict, or the parsed `.upd_info`), so it is untyped JSON like the record.
///
/// The Python passes `tag_name`, `browser_download_url`, `html_url` (and so
/// `version`, which falls back to the tag) through from the release document
/// as they are, so a release can carry `None`, a number or a list there.
/// The string fields are the common case; a value that is not a string is
/// kept in `raw` under the same key (its string field is then "") and is what
/// serialises, so the stored record is the Python's. `value(key)` gives the
/// real value either way.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LatestRelease {
    pub version: String,
    pub tag: String,
    pub url: String,
    pub asset: String,
    pub published: String,
    pub page: String,
    pub spec: Option<Value>,
    pub raw: std::collections::BTreeMap<String, Value>,
    /// A dict read back from the registry keeps its own keys, in its own
    /// order (`None` for one built here, which has the six plus `spec`).
    pub keys: Option<Vec<String>>,
    /// Keys of a stored dict other than the six and `spec`.
    pub extra: serde_json::Map<String, Value>,
}

/// The keys of the Python dict, in its order (`spec` comes last).
const RELEASE_KEYS: [&str; 6] = ["version", "tag", "url", "asset", "published", "page"];

impl LatestRelease {
    fn field_mut(&mut self, key: &str) -> Option<&mut String> {
        Some(match key {
            "version" => &mut self.version,
            "tag" => &mut self.tag,
            "url" => &mut self.url,
            "asset" => &mut self.asset,
            "published" => &mut self.published,
            "page" => &mut self.page,
            _ => return None,
        })
    }

    fn field(&self, key: &str) -> Option<&String> {
        Some(match key {
            "version" => &self.version,
            "tag" => &self.tag,
            "url" => &self.url,
            "asset" => &self.asset,
            "published" => &self.published,
            "page" => &self.page,
            _ => return None,
        })
    }

    /// The dict's value for `key` as the Python has it (`latest["url"]`).
    pub fn value(&self, key: &str) -> Value {
        if let Some(v) = self.raw.get(key) {
            return v.clone();
        }
        match key {
            "spec" => self.spec.clone().unwrap_or(Value::Null),
            _ => self.field(key).map(|s| Value::String(s.clone())).unwrap_or(Value::Null),
        }
    }

    /// Set one of the six release keys to a value of any type.
    pub fn set(&mut self, key: &str, v: Value) {
        if let Some(keys) = self.keys.as_mut() {
            if RELEASE_KEYS.contains(&key) && !keys.iter().any(|k| k == key) {
                keys.push(key.to_string());
            }
        }
        let Some(field) = self.field_mut(key) else { return };
        match v {
            Value::String(s) => {
                *field = s;
                self.raw.remove(key);
            }
            other => {
                field.clear();
                self.raw.insert(key.to_string(), other);
            }
        }
    }
}

impl Serialize for LatestRelease {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = ser.serialize_map(None)?;
        match &self.keys {
            None => {
                for k in RELEASE_KEYS {
                    m.serialize_entry(k, &self.value(k))?;
                }
            }
            Some(keys) => {
                for k in keys {
                    if RELEASE_KEYS.contains(&k.as_str()) {
                        m.serialize_entry(k, &self.value(k))?;
                    } else if k == "spec" {
                        m.serialize_entry(k, self.spec.as_ref().unwrap_or(&Value::Null))?;
                    } else {
                        m.serialize_entry(k, self.extra.get(k).unwrap_or(&Value::Null))?;
                    }
                }
            }
        }
        let spec_listed = self.keys.as_ref().is_some_and(|ks| ks.iter().any(|k| k == "spec"));
        if let (Some(spec), false) = (&self.spec, spec_listed) {
            m.serialize_entry("spec", spec)?;
        }
        m.end()
    }
}

impl<'de> Deserialize<'de> for LatestRelease {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let m = serde_json::Map::<String, Value>::deserialize(de)?;
        // The dict as stored: every key, in its order, `"spec": null` too.
        let mut out = LatestRelease { keys: Some(m.keys().cloned().collect()), ..Default::default() };
        for (k, v) in m {
            if RELEASE_KEYS.contains(&k.as_str()) {
                out.set(&k, v);
            } else if k == "spec" {
                out.spec = Some(v);
            } else {
                out.extra.insert(k, v);
            }
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Small Python-isms this module needs.

/// `str.isspace()` for one character: Rust's `is_whitespace` plus the four
/// ASCII separators (`\x1c`..`\x1f`) Python also counts.
fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// `str.strip()`.
fn py_strip(s: &str) -> &str {
    s.trim_matches(py_space)
}

/// `str.split()` (no argument).
fn py_split(s: &str) -> impl Iterator<Item = &str> {
    s.split(py_space).filter(|t| !t.is_empty())
}

/// `str.splitlines()`.
fn py_splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        let brk = matches!(
            c,
            '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}'
        );
        if brk {
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

/// `Path(value)` on raw OS text: pathlib drops `.` components, repeated
/// separators and a trailing slash; POSIX keeps exactly two leading
/// slashes; "" is ".".
fn py_path_os(value: &std::ffi::OsStr) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    let p: PathBuf = Path::new(value).components().filter(|c| *c != Component::CurDir).collect();
    if p.as_os_str().is_empty() {
        return PathBuf::from(".");
    }
    let b = value.as_bytes();
    if b.starts_with(b"//") && !b.starts_with(b"///") {
        let mut s = std::ffi::OsString::from("/");
        s.push(p.as_os_str());
        PathBuf::from(s)
    } else {
        p
    }
}

/// `Path(value)` of a str.
fn py_path(value: &str) -> PathBuf {
    py_path_os(std::ffi::OsStr::new(value))
}

/// A `Path` as pathlib holds it (what `str(path)` gives, as OS text).
fn py_norm(p: &Path) -> PathBuf {
    py_path_os(p.as_os_str())
}

/// `Path.parent` of a pathlib-normalised path.
fn py_parent(p: &Path) -> PathBuf {
    let p = py_norm(p);
    match p.parent() {
        Some(q) if !q.as_os_str().is_empty() => q.to_path_buf(),
        Some(_) => PathBuf::from("."),
        None => p,
    }
}

/// `Path.with_suffix(suffix)`: replaces the last suffix (".AppImage") or
/// appends when there is none. pathlib refuses a path with an empty name.
fn py_with_suffix(p: &Path, suffix: &str) -> Result<PathBuf, String> {
    use std::os::unix::ffi::OsStrExt;
    let p = py_norm(p);
    // pathlib's name is the last component as written, ".." included.
    let name: &[u8] = match p.components().next_back() {
        Some(Component::Normal(n)) if p.as_os_str() != "." => n.as_bytes(),
        Some(Component::ParentDir) => b"..",
        _ => return Err(format!("ValueError: {:?} has an empty name", p.to_string_lossy())),
    };
    // pathlib's stem: the name up to its last '.', provided that leaves at
    // least one non-dot character ("app." -> "app", ".hidden" stays).
    let stem = match name.iter().rposition(|&c| c == b'.') {
        Some(i) if name[..i].iter().any(|&c| c != b'.') => &name[..i],
        _ => name,
    };
    let mut file = std::ffi::OsStr::from_bytes(stem).to_os_string();
    file.push(suffix);
    let parent = p.parent().unwrap_or(Path::new(""));
    Ok(parent.join(file))
}

/// Python's truthiness for a JSON value.
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

/// `str(value)` for a JSON value, as an f-string would format it.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => py_repr(other),
    }
}

/// A JSON number as Python prints it. serde_json keeps an integer beyond
/// i64/u64 only as an f64 (no arbitrary precision in this crate), so an
/// integral f64 out of that range is taken to be such an integer and printed
/// in full, as Python's int would be (`10**20` -> "100000000000000000000").
/// A JSON float that large prints as an integer too: the two arrive the same.
fn py_number(n: &serde_json::Number) -> String {
    if let Some(i) = n.as_i64() {
        return i.to_string();
    }
    if let Some(u) = n.as_u64() {
        return u.to_string();
    }
    let f = n.as_f64().unwrap_or(0.0);
    if f.is_finite() && f.fract() == 0.0 && (f >= 18446744073709551616.0 || f < -9223372036854775808.0) {
        return format!("{f:.0}");
    }
    util::py_float_repr(f)
}

/// `repr(value)` for a JSON value: `{'a': 1}`, `[1, 2]`, `'text'`.
fn py_repr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => py_number(n),
        Value::String(s) => py_str_repr(s),
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter().map(|(k, v)| format!("{}: {}", py_str_repr(k), py_repr(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// `str.isprintable()` for one character beyond ASCII: false for the
/// categories Python escapes in a repr -- Other (Cc, Cf, Cs, Co and the
/// unassigned Cn) and Separator (Zs, Zl, Zp; the space itself is ASCII and
/// handled by the caller).
fn py_printable(c: char) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^[\p{C}\p{Z}]$").expect("static pattern"));
    !re.is_match(c.encode_utf8(&mut [0; 4]))
}

/// `repr(str)`: single quotes unless the text has one and no double quote.
fn py_str_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(quote);
    for c in s.chars() {
        let u = c as u32;
        match c {
            _ if c == quote || c == '\\' => {
                out.push('\\');
                out.push(c);
            }
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ if u < 0x20 || u == 0x7f => out.push_str(&format!("\\x{u:02x}")),
            _ if u < 0x7f => out.push(c),
            _ if py_printable(c) => out.push(c),
            _ if u <= 0xff => out.push_str(&format!("\\x{u:02x}")),
            _ if u <= 0xffff => out.push_str(&format!("\\u{u:04x}")),
            _ => out.push_str(&format!("\\U{u:08x}")),
        }
    }
    out.push(quote);
    out
}

/// Python's type name for a JSON value, for error messages.
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

fn version_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(VERSION_PATTERN).expect("static pattern"))
}

fn digit_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\d$").expect("static pattern"))
}

/// The value of one Unicode decimal digit (`int("٣") == 3` in Python).
/// Unicode encodes every decimal digit set as ten consecutive code points
/// starting at zero, so the value is the position within the run of digits.
fn digit_value(c: char) -> Option<u32> {
    if c.is_ascii_digit() {
        return c.to_digit(10);
    }
    let is_digit = |c: char| digit_re().is_match(c.encode_utf8(&mut [0; 4]));
    if !is_digit(c) {
        return None;
    }
    let mut start = c as u32;
    while let Some(prev) = start.checked_sub(1).and_then(char::from_u32) {
        if !is_digit(prev) {
            break;
        }
        start -= 1;
    }
    Some((c as u32 - start) % 10)
}

/// `sys.get_int_max_str_digits()`: Python's int() refuses longer numbers.
const INT_MAX_STR_DIGITS: usize = 4300;

/// `int(text)`: surrounding whitespace, an optional sign, digits with single
/// underscores between them. Anything else is Python's ValueError, and so is
/// a number of more than 4300 digits (Python's integer string limit).
/// Unbounded, like Python's int.
fn py_int(text: &str) -> Result<BigInt, String> {
    let err = || format!("ValueError: invalid literal for int() with base 10: {}", py_str_repr(text));
    let s = py_strip(text);
    let (neg, body) = match s.chars().next() {
        Some('-') => (true, &s[1..]),
        Some('+') => (false, &s[1..]),
        _ => (false, s),
    };
    if body.is_empty() || body.starts_with('_') || body.ends_with('_') || body.contains("__") {
        return Err(err());
    }
    let mut digits = String::with_capacity(body.len() + 1);
    if neg {
        digits.push('-');
    }
    for c in body.chars() {
        if c == '_' {
            continue;
        }
        let d = digit_value(c).ok_or_else(err)?;
        digits.push(char::from(b'0' + d as u8));
    }
    let count = digits.len() - usize::from(neg);
    if count > INT_MAX_STR_DIGITS {
        return Err(format!(
            "ValueError: Exceeds the limit ({INT_MAX_STR_DIGITS} digits) for integer string conversion: value has {count} digits; use sys.set_int_max_str_digits() to increase the limit"
        ));
    }
    digits.parse::<BigInt>().map_err(|_| err())
}

/// `fnmatch.translate` (POSIX: case-sensitive) as a Rust regex, then match.
/// Brackets follow Python's rules: `[!x]` negates, an unclosed `[` is a
/// literal, reversed ranges are dropped.
fn fnmatch(name: &str, pat: &str) -> bool {
    match Regex::new(&fnmatch_translate(pat)) {
        Ok(re) => re.is_match(name),
        Err(_) => false,
    }
}

/// Python's class text (after `fnmatch.translate` has escaped it) as the
/// same class in Rust's syntax: an escape stays an escape, a leading '^' is
/// negation, and the brackets Rust would read as a nested class are escaped.
fn class_to_rust(stuff: &str) -> String {
    let mut out = String::new();
    let mut it = stuff.chars().enumerate();
    while let Some((n, c)) = it.next() {
        match c {
            '\\' => {
                out.push('\\');
                if let Some((_, e)) = it.next() {
                    out.push(e);
                }
            }
            '^' if n == 0 => out.push('^'),
            '[' | ']' | '^' | '&' | '~' | '|' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

fn fnmatch_translate(pat: &str) -> String {
    let p: Vec<char> = pat.chars().collect();
    let n = p.len();
    let mut res = String::from("(?s)^");
    let mut i = 0;
    while i < n {
        let c = p[i];
        i += 1;
        if c == '*' {
            res.push_str(".*");
            while i < n && p[i] == '*' {
                i += 1;
            }
        } else if c == '?' {
            res.push('.');
        } else if c == '[' {
            let mut j = i;
            if j < n && p[j] == '!' {
                j += 1;
            }
            if j < n && p[j] == ']' {
                j += 1;
            }
            while j < n && p[j] != ']' {
                j += 1;
            }
            if j >= n {
                res.push_str("\\[");
                continue;
            }
            let raw: String = p[i..j].iter().collect();
            let mut stuff: String = if !raw.contains('-') {
                raw.replace('\\', "\\\\")
            } else {
                let mut chunks: Vec<Vec<char>> = Vec::new();
                let mut k = if p[i] == '!' { i + 2 } else { i + 1 };
                let mut ii = i;
                loop {
                    let found = (k..j).find(|&x| p[x] == '-');
                    let Some(kk) = found else { break };
                    chunks.push(p[ii..kk].to_vec());
                    ii = kk + 1;
                    k = kk + 3;
                }
                let chunk: Vec<char> = p[ii..j].to_vec();
                if !chunk.is_empty() {
                    chunks.push(chunk);
                } else if let Some(last) = chunks.last_mut() {
                    last.push('-');
                }
                // Remove empty ranges -- invalid in RE.
                let mut k = chunks.len().saturating_sub(1);
                while k > 0 {
                    let (a, b) = (chunks[k - 1].last().copied(), chunks[k].first().copied());
                    if let (Some(a), Some(b)) = (a, b) {
                        if a > b {
                            let tail: Vec<char> = chunks[k][1..].to_vec();
                            let prev = &mut chunks[k - 1];
                            prev.pop();
                            prev.extend(tail);
                            chunks.remove(k);
                        }
                    }
                    k -= 1;
                }
                // Escape backslashes and hyphens for set difference (--).
                // Hyphens that create ranges shouldn't be escaped.
                chunks
                    .iter()
                    .map(|s| s.iter().collect::<String>().replace('\\', "\\\\").replace('-', "\\-"))
                    .collect::<Vec<_>>()
                    .join("-")
            };
            i = j + 1;
            if stuff.is_empty() {
                // Empty range: never match.
                res.push_str("[^\\s\\S]");
            } else if stuff == "!" {
                // Negated empty range: match any character.
                res.push('.');
            } else {
                // Escape set operations (&&, ~~ and ||).
                let mut esc = String::new();
                for ch in stuff.chars() {
                    if matches!(ch, '&' | '~' | '|') {
                        esc.push('\\');
                    }
                    esc.push(ch);
                }
                stuff = esc;
                // Negation is decided on this text, after the empty ranges
                // are gone: "[a-*!b]" leaves "!b", which is "[^b]".
                if let Some(rest) = stuff.strip_prefix('!') {
                    stuff = format!("^{rest}");
                } else if stuff.starts_with(['^', '[']) {
                    stuff = format!("\\{stuff}");
                }
                res.push_str(&format!("[{}]", class_to_rust(&stuff)));
            }
        } else {
            res.push_str(&regex::escape(&c.to_string()));
        }
    }
    res.push_str("\\z");
    res
}

/// `subprocess.run(argv, capture_output=True, timeout=secs, cwd=cwd).stdout`
/// as bytes (`util::run` decodes, and this module must strip NULs before
/// decoding). `None` is the OSError / TimeoutExpired the Python catches.
/// No `stdin=` is given in the Python, so the child inherits ours (util's
/// runners give it /dev/null). The argv is raw OS text, as Python passes
/// a str path's own bytes.
///
/// `communicate(timeout=...)` bounds the whole exchange: the child's exit
/// *and* the end of its output. A helper the child leaves running that
/// keeps the pipes open runs the clock out too; the child is then killed
/// and reaped, and the readers are left to finish on their own.
fn run_bytes(argv: &[&std::ffi::OsStr], timeout_secs: u64, cwd: Option<&Path>) -> Option<Vec<u8>> {
    use std::process::{Command, Stdio};
    let (prog, args) = argv.split_first()?;
    let mut cmd = Command::new(prog);
    cmd.args(args).stdin(Stdio::inherit()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let mut child = cmd.spawn().ok()?;
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
    });
    let start = Instant::now();
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
        if start.elapsed() >= Duration::from_secs(timeout_secs) {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let out = out_t.join().unwrap_or_default();
    let _ = err_t.join();
    Some(out)
}

// ---------------------------------------------------------------------------

/// The raw .upd_info string, or '' when the AppImage carries none.
pub fn read_update_info(path: &Path) -> String {
    let p = py_norm(path);
    let arg = |s: &'static str| std::ffi::OsStr::new(s);
    let Some(out) = run_bytes(
        &[arg("objcopy"), arg("-O"), arg("binary"), arg("--only-section=.upd_info"), p.as_os_str(), arg("/dev/stdout")],
        20,
        None,
    ) else {
        return String::new();
    };
    let bytes: Vec<u8> = out.into_iter().filter(|&b| b != 0).collect();
    py_strip(&String::from_utf8_lossy(&bytes)).to_string()
}

/// gh-releases-zsync|owner|repo|release|pattern -> a dict we can act on.
///
/// `None` is the Python's `{}`.
pub fn parse_update_info(info: &str) -> Option<UpdateSpec> {
    let parts: Vec<&str> = info.split('|').collect();
    if parts.len() >= 5 && parts[0].starts_with("gh-releases") {
        return Some(UpdateSpec {
            kind: "github".into(),
            owner: parts[1].into(),
            repo: parts[2].into(),
            release: Some(parts[3].into()),
            pattern: Some(parts[4].into()),
        });
    }
    None
}

/// `_version_of`: the first dotted version number in `text`, or "".
pub fn version_of(text: &str) -> String {
    version_re()
        .captures(text)
        .and_then(|m| m.get(1))
        .map(|m| m.as_str().to_string())
        .unwrap_or_default()
}

/// `_vkey`: comparable tuple. '0.10.2' must sort above '0.9.9', which it does
/// not as text -- the reason a string compare is wrong here.
///
/// Fallible like the Python's `int()`: `check` also calls it on a release's
/// version, which falls back to the raw tag name ("nightly") when the tag
/// carries no number, and Python raises ValueError there.
pub fn vkey(v: &str) -> Result<Vec<BigInt>, String> {
    if v.is_empty() {
        return Ok(Vec::new());
    }
    v.split('.').map(py_int).collect()
}

fn release_url(owner: &str, repo: &str, release: &Value) -> String {
    let latest = matches!(release.as_str(), Some("latest") | Some(""));
    let tag = if latest { "releases/latest".to_string() } else { format!("releases/tags/{}", py_str(release)) };
    format!("https://api.github.com/repos/{owner}/{repo}/{tag}")
}

/// The body of `latest_github` after the fetch: `data` is what the cache
/// gave back, `pattern` is `None` when the spec's pattern is not a string.
fn latest_from(data: Option<Value>, pattern: Option<&Value>) -> Result<Option<LatestRelease>, String> {
    let Some(Value::Object(data)) = data else {
        return Ok(None); // offline or rate-limited: no update shown
    };
    let pattern = match pattern {
        Some(Value::String(s)) => s.as_str(),
        Some(other) => {
            return Err(format!("AttributeError: '{}' object has no attribute 'endswith'", py_type(other)))
        }
        None => "*.AppImage",
    };

    // The pattern names a .zsync companion; the AppImage is the same name
    // without that suffix.
    let want = pattern.strip_suffix(".zsync").unwrap_or(pattern);
    let mut asset_url = Value::String(String::new());
    let mut asset_name = String::new();
    let empty = Value::Array(Vec::new());
    let assets = data.get("assets").unwrap_or(&empty);
    let items: Vec<&Value> = match assets {
        Value::Array(a) => a.iter().collect(),
        Value::Object(o) if o.is_empty() => Vec::new(),
        Value::String(s) if s.is_empty() => Vec::new(),
        Value::Object(_) | Value::String(_) => {
            return Err("AttributeError: 'str' object has no attribute 'get'".into())
        }
        other => return Err(format!("TypeError: '{}' object is not iterable", py_type(other))),
    };
    for a in items {
        let Some(a) = a.as_object() else {
            return Err(format!("AttributeError: '{}' object has no attribute 'get'", py_type(a)));
        };
        let name = match a.get("name") {
            None => "",
            Some(Value::String(s)) => s.as_str(),
            Some(other) => {
                return Err(format!(
                    "TypeError: expected str, bytes or os.PathLike object, not {}",
                    py_type(other)
                ))
            }
        };
        if fnmatch(name, want) {
            // Passed through as it is: None or a number survives into "url".
            asset_url = a.get("browser_download_url").cloned().unwrap_or_else(|| Value::String(String::new()));
            asset_name = name.to_string();
            break;
        }
    }

    let tag_name = data.get("tag_name").cloned().unwrap_or_else(|| Value::String(String::new()));
    // `_version_of(tag_name) or _version_of(asset_name) or tag_name`
    let mut version = Value::String(version_of_value(&tag_name)?);
    if version.as_str() == Some("") {
        let v = version_of(&asset_name);
        version = if v.is_empty() { tag_name.clone() } else { Value::String(v) };
    }
    // `(data.get("published_at") or "")[:10]`
    let published = match data.get("published_at").filter(|v| truthy(v)) {
        None => Value::String(String::new()),
        Some(Value::String(s)) => Value::String(s.chars().take(10).collect()),
        Some(Value::Array(a)) => Value::Array(a.iter().take(10).cloned().collect()),
        Some(Value::Object(_)) => return Err("KeyError: slice(None, 10, None)".into()),
        Some(other) => return Err(format!("TypeError: '{}' object is not subscriptable", py_type(other))),
    };
    let page = data.get("html_url").cloned().unwrap_or_else(|| Value::String(String::new()));
    let mut out = LatestRelease::default();
    out.set("version", version);
    out.set("tag", tag_name);
    out.set("url", asset_url);
    out.set("asset", Value::String(asset_name));
    out.set("published", published);
    out.set("page", page);
    Ok(Some(out))
}

/// `_version_of(value)` for whatever a release document holds: a falsy
/// value is searched as "" (`text or ""`), any other non-string is the
/// TypeError `re.search` raises.
fn version_of_value(v: &Value) -> Result<String, String> {
    match v {
        _ if !truthy(v) => Ok(String::new()),
        Value::String(s) => Ok(version_of(s)),
        other => Err(format!("TypeError: expected string or bytes-like object, got '{}'", py_type(other))),
    }
}

/// `latest_github` with the fetch and the loosely-typed arguments the
/// registry can hand `check` made explicit.
fn latest_impl(
    owner: &str,
    repo: &str,
    pattern: Option<&Value>,
    release: &Value,
    fetch: &dyn Fn(&str) -> Option<Value>,
) -> Result<Option<LatestRelease>, String> {
    let url = release_url(owner, repo, release);
    // Shared cache: authenticated when gh is logged in, conditional otherwise,
    // and never competing with the store for the same rate limit.
    let data = fetch(&url);
    latest_from(data, pattern)
}

// `try: data = ghcache.get_json(url) / except Exception: data = None`:
// what get_json raises is "no update shown" here, so the lenient form is
// the right one.
#[allow(deprecated)]
fn fetch_github(url: &str) -> Option<Value> {
    ghcache::get_json(url, ghcache::DEFAULT_TTL, true, ghcache::ACCEPT)
}

/// Newest release plus the asset matching the AppImage's own pattern.
///
/// `None` when GitHub gave nothing (offline or rate-limited: no update
/// shown). `release` is "latest" (or "") or a tag name. An error is a
/// malformed release document the Python would raise on.
pub fn latest_github(owner: &str, repo: &str, pattern: &str, release: &str) -> Result<Option<LatestRelease>, String> {
    latest_impl(
        owner,
        repo,
        Some(&Value::String(pattern.into())),
        &Value::String(release.into()),
        &fetch_github,
    )
}

type LatestFn<'a> = &'a dyn Fn(&str, &str, Option<&Value>, &Value) -> Result<Option<LatestRelease>, String>;

fn check_impl(record: &Value, latest_fn: LatestFn) -> Result<Option<LatestRelease>, String> {
    // `record.get(...)`: anything but a dict has no such method.
    let Some(record) = record.as_object() else {
        return Err(format!("AttributeError: '{}' object has no attribute 'get'", py_type(record)));
    };
    let get = |k: &str| record.get(k);
    let path = match get("path") {
        None => py_path(""),
        Some(Value::String(s)) => py_path(s),
        Some(other) => {
            return Err(format!(
                "TypeError: argument should be a str or an os.PathLike object, not '{}'",
                py_type(other)
            ))
        }
    };
    if !path.exists() {
        return Ok(None);
    }

    let spec: Value = match get("update_spec") {
        Some(v) if truthy(v) => v.clone(),
        _ => parse_update_info(&read_update_info(&path))
            .and_then(|s| serde_json::to_value(s).ok())
            .unwrap_or_else(|| Value::Object(Default::default())),
    };
    let Some(obj) = spec.as_object() else {
        return Err(format!("AttributeError: '{}' object has no attribute 'get'", py_type(&spec)));
    };
    if obj.get("kind").and_then(Value::as_str) != Some("github") {
        return Ok(None);
    }

    let owner = obj.get("owner").ok_or("KeyError: 'owner'")?;
    let repo = obj.get("repo").ok_or("KeyError: 'repo'")?;
    let default_release = Value::String("latest".into());
    let latest = latest_fn(
        &py_str(owner),
        &py_str(repo),
        obj.get("pattern"),
        obj.get("release").unwrap_or(&default_release),
    )?;
    let Some(mut latest) = latest else { return Ok(None) };
    if !truthy(&latest.value("url")) {
        return Ok(None);
    }

    let have = match get("version") {
        Some(v) if !truthy(v) => String::new(),
        None => String::new(),
        Some(Value::String(s)) => version_of(s),
        Some(_) => return Err("TypeError: expected string or bytes-like object".into()),
    };
    // `new` is a string, or the tag's own falsy value (None, 0, []): a truthy
    // tag that is not a string has already raised in `_version_of`.
    let new_v = latest.value("version");
    let new = new_v.as_str().unwrap_or("").to_string();
    // Only report a genuinely higher version. Equal or unparseable means
    // "nothing to do" -- claiming an update that reinstalls the same build
    // would be worse than staying quiet.
    if !have.is_empty() && !new.is_empty() && vkey(&new)? <= vkey(&have)? {
        return Ok(None);
    }
    if have.is_empty() && !truthy(&new_v) {
        return Ok(None);
    }
    latest.spec = Some(spec);
    Ok(Some(latest))
}

/// Given a registry record, return {} or the newer release's details.
///
/// `record` is one value of the AppImage registry (`inventory` owns that
/// file and keeps records as untyped JSON objects, as the Python does).
/// `None` is the Python's `{}`; an error is what the Python raises on a
/// malformed record (a spec without an owner, say).
pub fn check(record: &Value) -> Result<Option<LatestRelease>, String> {
    check_impl(record, &|o, r, p, rel| latest_impl(o, r, p, rel, &fetch_github))
}

/// urllib's `timeout=120`: it bounds every socket operation, so a body that
/// keeps arriving may take as long as it takes, but one that stalls for this
/// long fails.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);

/// `HTTPResponse.read(262144)`: the size of each block written (and of each
/// progress step).
const BLOCK: usize = 262144;

/// What `urllib.request.urlopen` hands back: the body and its
/// `Content-Length` header (None when absent).
struct Opened {
    body: Box<dyn Read + Send>,
    content_length: Option<String>,
}

/// `Request(url)`'s clean-up of the URL, `unwrap()`: surrounding
/// whitespace, `<...>` and a `URL:` prefix go. The fragment stays part of
/// `full_url` (the data: handler reads it as data).
fn request_url(url: &str) -> String {
    let mut u = py_strip(url);
    if u.starts_with('<') && u.ends_with('>') && u.len() >= 2 {
        u = py_strip(&u[1..u.len() - 1]);
    }
    if let Some(rest) = u.strip_prefix("URL:") {
        u = py_strip(rest);
    }
    u.to_string()
}

/// `_splittag`: the URL without its fragment (split at the last '#').
fn without_fragment(url: &str) -> &str {
    match url.rfind('#') {
        Some(i) => &url[..i],
        None => url,
    }
}

/// `_splittype`: the scheme (lowercased) before the first ':', if the text
/// up to it has no '/'.
fn url_scheme(url: &str) -> Option<String> {
    let i = url.find(':')?;
    let scheme = &url[..i];
    if scheme.is_empty() || scheme.contains('/') {
        return None;
    }
    Some(scheme.to_lowercase())
}

/// `urllib.parse.unquote_to_bytes`: %XX becomes the byte, anything else
/// (including a '%' not followed by two hex digits) stays as it is.
fn unquote_to_bytes(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() && b[i + 1].is_ascii_hexdigit() && b[i + 2].is_ascii_hexdigit() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("00");
            out.push(u8::from_str_radix(hex, 16).unwrap_or(0));
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

/// `base64.decodebytes` (`binascii.a2b_base64`, not strict): characters
/// outside the alphabet are skipped, a complete pad sequence ends the data.
fn a2b_base64(data: &[u8]) -> Result<Vec<u8>, String> {
    let val = |c: u8| -> Option<u8> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    };
    let mut out = Vec::new();
    let (mut quad_pos, mut leftchar, mut pads) = (0u8, 0u8, 0u8);
    for &c in data {
        if c == b'=' {
            if quad_pos >= 2 {
                pads += 1;
                if quad_pos + pads >= 4 {
                    return Ok(out);
                }
            }
            continue;
        }
        let Some(v) = val(c) else { continue };
        pads = 0;
        match quad_pos {
            0 => {
                quad_pos = 1;
                leftchar = v;
            }
            1 => {
                quad_pos = 2;
                out.push((leftchar << 2) | (v >> 4));
                leftchar = v & 0x0f;
            }
            2 => {
                quad_pos = 3;
                out.push((leftchar << 4) | (v >> 2));
                leftchar = v & 0x03;
            }
            _ => {
                quad_pos = 0;
                out.push((leftchar << 6) | v);
                leftchar = 0;
            }
        }
    }
    match quad_pos {
        0 => Ok(out),
        1 => {
            let n = data.iter().filter(|&&c| val(c).is_some()).count();
            Err(format!(
                "binascii.Error: Invalid base64-encoded string: number of data characters ({n}) cannot be 1 more than a multiple of 4"
            ))
        }
        _ => Err("binascii.Error: Incorrect padding".into()),
    }
}

/// `DataHandler.data_open`: RFC 2397 `data:[<mediatype>][;base64],<data>`.
fn open_data(url: &str) -> Result<Opened, String> {
    let rest = url.split_once(':').map(|x| x.1).unwrap_or("");
    let Some((mediatype, data)) = rest.split_once(',') else {
        return Err("ValueError: not enough values to unpack (expected 2, got 1)".into());
    };
    // Disallow control characters within mediatype.
    if mediatype.chars().any(|c| (c as u32) < 0x20 || c == '\x7f') {
        return Err("ValueError: Control characters not allowed in data: mediatype".into());
    }
    // even base64 encoded data URLs might be quoted so unquote in any case:
    let mut bytes = unquote_to_bytes(data);
    if mediatype.ends_with(";base64") {
        bytes = a2b_base64(&bytes)?;
    }
    let len = bytes.len();
    Ok(Opened { body: Box::new(std::io::Cursor::new(bytes)), content_length: Some(len.to_string()) })
}

/// `socket.gethostbyname(name)`: its first IPv4 address.
fn host_ipv4(name: &str) -> Option<std::net::Ipv4Addr> {
    use std::net::ToSocketAddrs;
    (name, 0).to_socket_addrs().ok()?.find_map(|a| match a.ip() {
        std::net::IpAddr::V4(ip) => Some(ip),
        std::net::IpAddr::V6(_) => None,
    })
}

/// `_is_local_authority(authority, resolve=True)`.
fn is_local_authority(authority: &str) -> bool {
    if authority.is_empty() || authority == "localhost" {
        return true;
    }
    let hostname = rustix::system::uname().nodename().to_string_lossy().into_owned();
    if authority == hostname {
        return true;
    }
    let Some(addr) = host_ipv4(authority) else { return false };
    // FileHandler.get_names(): every IPv4 address of localhost and of us.
    use std::net::ToSocketAddrs;
    let names: Vec<std::net::Ipv4Addr> = ["localhost", hostname.as_str()]
        .iter()
        .filter_map(|h| (*h, 0).to_socket_addrs().ok())
        .flatten()
        .filter_map(|a| match a.ip() {
            std::net::IpAddr::V4(ip) => Some(ip),
            std::net::IpAddr::V6(_) => None,
        })
        .collect();
    names.contains(&addr)
}

/// `FileHandler.open_local_file`: `url2pathname` (localhost only, query and
/// fragment dropped, %XX decoded to bytes), then the file itself, with its
/// size as the Content-Length.
fn open_file(url: &str) -> Result<Opened, String> {
    use std::os::unix::ffi::OsStrExt;
    // urlsplit: C0 controls and spaces are stripped from the front, tabs and
    // newlines removed anywhere.
    let u: String = url
        .trim_start_matches(|c: char| (c as u32) <= 0x20)
        .chars()
        .filter(|c| !matches!(c, '\t' | '\r' | '\n'))
        .collect();
    let rest = u.split_once(':').map(|x| x.1).unwrap_or("");
    let (authority, path) = match rest.strip_prefix("//") {
        Some(r) => {
            let end = r.find(['/', '?', '#']).unwrap_or(r.len());
            (&r[..end], &r[end..])
        }
        None => ("", rest),
    };
    let path = &path[..path.find(['?', '#']).unwrap_or(path.len())];
    if !is_local_authority(authority) {
        return Err("URLError: <urlopen error file:// scheme is supported only on localhost>".into());
    }
    let local = PathBuf::from(std::ffi::OsStr::from_bytes(&unquote_to_bytes(path)));
    let err = |e: std::io::Error| format!("URLError: <urlopen error {e}>");
    let size = std::fs::metadata(&local).map_err(err)?.len();
    // open() of a directory is IsADirectoryError in Python; on Linux the
    // open itself succeeds, so refuse it here, before anything is written.
    if std::fs::metadata(&local).map_err(err)?.is_dir() {
        return Err(err(std::io::Error::from_raw_os_error(libc::EISDIR)));
    }
    let f = std::fs::File::open(&local).map_err(err)?;
    Ok(Opened { body: Box::new(f), content_length: Some(size.to_string()) })
}

/// `urllib.request.urlopen`: http and https over the network, file and
/// data URLs locally; any other scheme is the URLError urllib raises
/// (ftp, which urllib also speaks, has no client here).
fn urlopen(url: &str) -> Result<Opened, String> {
    let clean = request_url(url);
    // `Request.full_url`: the fragment comes back only when it is not empty.
    let base = without_fragment(&clean);
    let full_url = match clean.rfind('#').map(|i| &clean[i + 1..]) {
        Some(f) if !f.is_empty() => format!("{base}#{f}"),
        _ => base.to_string(),
    };
    let Some(scheme) = url_scheme(base) else {
        return Err(format!("ValueError: unknown url type: {}", py_str_repr(&clean)));
    };
    urllib::check_request(&full_url)?;
    urllib::build_opener();
    match scheme.as_str() {
        "http" | "https" => {
            let (body, content_length) = urllib::open_http(url, UA, DOWNLOAD_TIMEOUT)?;
            Ok(Opened { body, content_length })
        }
        "file" => open_file(&clean),
        "data" => open_data(&full_url),
        other => Err(format!("URLError: <urlopen error unknown url type: {other}>")),
    }
}

/// Read `body` in `BLOCK`-sized pieces (the last may be shorter), each read
/// bounded by `DOWNLOAD_TIMEOUT` the way urllib's socket timeout bounds it.
/// The reads run on a helper thread so a stalled connection cannot hang the
/// caller; on a timeout that thread is left to die with the connection.
fn read_blocks(mut body: Box<dyn Read + Send>, mut each: impl FnMut(&[u8]) -> Result<(), String>) -> Result<(), String> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<Result<Vec<u8>, String>>(16);
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let msg = match body.read(&mut buf) {
                Ok(n) => Ok(buf[..n].to_vec()),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => Err(format!("OSError: {e}")),
            };
            let done = !matches!(&msg, Ok(c) if !c.is_empty());
            if tx.send(msg).is_err() || done {
                return;
            }
        }
    });
    let mut block: Vec<u8> = Vec::with_capacity(BLOCK);
    loop {
        let chunk = match rx.recv_timeout(DOWNLOAD_TIMEOUT) {
            Ok(Ok(c)) => c,
            Ok(Err(e)) => return Err(e),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => return Err("TimeoutError: The read operation timed out".into()),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Vec::new(),
        };
        if chunk.is_empty() {
            if !block.is_empty() {
                each(&block)?;
            }
            return Ok(());
        }
        let mut rest = chunk.as_slice();
        while !rest.is_empty() {
            let take = (BLOCK - block.len()).min(rest.len());
            block.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
            if block.len() == BLOCK {
                each(&block)?;
                block.clear();
            }
        }
    }
}

/// Fetch to a temporary file beside dest, then swap it in.
///
/// Downloading over the live file would leave a half-written AppImage if the
/// connection dropped; the swap is the last step and is atomic.
///
/// `progress` receives the fraction done when the source gives a
/// Content-Length. Errors (network, I/O) are returned, as the Python raises.
pub fn download(url: &str, dest: &Path, progress: Option<&dyn Fn(f64)>) -> Result<PathBuf, String> {
    let dest = py_norm(dest);
    let tmp = py_with_suffix(&dest, ".download")?;
    let r = urlopen(url)?;
    // `int(r.headers.get("Content-Length") or 0)`
    let total: f64 = match r.content_length.as_deref() {
        None | Some("") => 0.0,
        Some(v) => py_int(v)?.to_string().parse::<f64>().unwrap_or(0.0),
    };
    let mut done: u64 = 0;
    {
        let mut f = std::fs::File::create(&tmp).map_err(|e| format!("OSError: {e}"))?;
        read_blocks(r.body, |chunk| {
            f.write_all(chunk).map_err(|e| format!("OSError: {e}"))?;
            done += chunk.len() as u64;
            if let Some(cb) = progress {
                if total != 0.0 {
                    cb(done as f64 / total);
                }
            }
            Ok(())
        })?;
    }
    // Keep the previous build: a bad update must be revertible.
    if dest.exists() {
        let prev = py_with_suffix(&dest, ".previous")?;
        std::fs::rename(&dest, &prev).map_err(|e| format!("OSError: {e}"))?;
    }
    std::fs::rename(&tmp, &dest).map_err(|e| format!("OSError: {e}"))?;
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&dest).map_err(|e| format!("OSError: {e}"))?.permissions().mode();
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(mode | 0o111))
            .map_err(|e| format!("OSError: {e}"))?;
    }
    Ok(dest)
}

/// `os.path.join(dir, name)` on raw path text: no separator is added after
/// one that is already there.
fn join_os(dir: &std::ffi::OsStr, name: &std::ffi::OsStr) -> std::ffi::OsString {
    use std::os::unix::ffi::OsStrExt;
    let mut out = dir.to_os_string();
    if !dir.as_bytes().is_empty() && !dir.as_bytes().ends_with(b"/") {
        out.push("/");
    }
    out.push(name);
    out
}

/// `os.scandir(path)`: (name, path) of each entry, in directory order.
fn scandir(path: &std::ffi::OsStr) -> std::io::Result<Vec<(String, std::ffi::OsString, std::fs::DirEntry)>> {
    let rd = std::fs::read_dir(path)?;
    Ok(rd
        .filter_map(Result::ok)
        .map(|e| {
            let name = e.file_name();
            (name.to_string_lossy().into_owned(), join_os(path, &name), e)
        })
        .collect())
}

fn has_magic(s: &str) -> bool {
    s.contains(['*', '?', '['])
}

/// One step of pathlib's globber (`glob._GlobberBase.selector`), applied to
/// `parts[i..]` from `path` (raw text ending in '/' where more follows).
fn glob_select(parts: &[String], i: usize, path: std::ffi::OsString, exists: bool, out: &mut Vec<std::ffi::OsString>) {
    let n = parts.len();
    if i == n {
        // select_exists
        if exists || std::fs::symlink_metadata(&path).is_ok() {
            out.push(path);
        }
        return;
    }
    let part = parts[i].as_str();
    if part == "**" {
        // recursive_selector, not following symlinks (recurse_symlinks=False)
        let mut j = i + 1;
        while j < n && parts[j] == "**" {
            j += 1;
        }
        let dir_only = j < n;
        glob_select(parts, j, path.clone(), exists, out);
        let mut stack = vec![path];
        while let Some(p) = stack.pop() {
            let Ok(entries) = scandir(&p) else { continue };
            for (_name, entry_path, entry) in entries {
                let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                if is_dir || !dir_only {
                    let mut ep = entry_path;
                    if dir_only {
                        ep.push("/");
                        glob_select(parts, j, ep.clone(), true, out);
                    } else {
                        out.push(ep.clone());
                    }
                    if is_dir {
                        stack.push(ep);
                    }
                }
            }
        }
    } else if part.is_empty() || part == ".." {
        // special_selector
        let mut p = path;
        p.push(part);
        if i + 1 < n {
            p.push("/");
        }
        glob_select(parts, i + 1, p, exists, out);
    } else if !has_magic(part) {
        // literal_selector: joins the literal parts that follow, too.
        let mut lit = part.to_string();
        let mut j = i + 1;
        while j < n && !has_magic(&parts[j]) {
            lit.push('/');
            lit.push_str(&parts[j]);
            j += 1;
        }
        if j < n {
            lit.push('/');
        }
        let mut p = path;
        p.push(lit);
        glob_select(parts, j, p, false, out);
    } else {
        // wildcard_selector
        let dir_only = i + 1 < n;
        let Ok(entries) = scandir(&path) else { return };
        for (name, entry_path, _entry) in entries {
            if part == "*" || fnmatch(&name, part) {
                if dir_only {
                    // DirEntry.is_dir() follows a symlink.
                    if !std::fs::metadata(&entry_path).map(|m| m.is_dir()).unwrap_or(false) {
                        continue;
                    }
                    let mut ep = entry_path;
                    ep.push("/");
                    glob_select(parts, i + 1, ep, true, out);
                } else {
                    out.push(entry_path);
                }
            }
        }
    }
}

/// `Path(root).glob(pattern)` (Python 3.14, POSIX): the pattern is split on
/// '/', so a '/' that came in with a name is a directory step like any
/// other; '**' recurses without following symlinks, wildcards match hidden
/// names too.
fn py_glob(root: &Path, pattern: &str) -> Vec<PathBuf> {
    let mut parts: Vec<String> =
        pattern.split('/').filter(|x| !x.is_empty() && *x != ".").map(String::from).collect();
    if parts.is_empty() || pattern.starts_with('/') {
        return Vec::new(); // the Python raises; no caller passes such a pattern
    }
    if pattern.ends_with('/') {
        parts.push(String::new());
    }
    let start = join_os(root.as_os_str(), std::ffi::OsStr::new(""));
    let mut out = Vec::new();
    glob_select(&parts, 0, start, false, &mut out);
    out.into_iter().map(PathBuf::from).collect()
}

/// `shutil.copy2`: contents, then `copystat` -- timestamps (`os.utime` by
/// path, which needs no write access to the copy), extended attributes, and
/// the permission bits.
fn copy2(src: &Path, dst: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let dst = if dst.is_dir() { dst.join(src.file_name().unwrap_or_default()) } else { dst.to_path_buf() };
    // copyfile: the destination keeps its own mode until copystat.
    {
        let mut from = std::fs::File::open(src)?;
        // Python's open() of a directory is IsADirectoryError, raised before
        // the copy is created; on Linux the open itself succeeds.
        if from.metadata()?.is_dir() {
            return Err(std::io::Error::from_raw_os_error(libc::EISDIR));
        }
        let mut to = std::fs::File::create(&dst)?;
        std::io::copy(&mut from, &mut to)?;
    }
    let st = rustix::fs::stat(src)?;
    let times = rustix::fs::Timestamps {
        last_access: rustix::fs::Timespec { tv_sec: st.st_atime as _, tv_nsec: st.st_atime_nsec as _ },
        last_modification: rustix::fs::Timespec { tv_sec: st.st_mtime as _, tv_nsec: st.st_mtime_nsec as _ },
    };
    rustix::fs::utimensat(rustix::fs::CWD, &dst, &times, rustix::fs::AtFlags::empty())?;
    copy_xattr(src, &dst)?;
    std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(st.st_mode as u32 & 0o7777))?;
    Ok(())
}

/// `shutil._copyxattr`: every extended attribute, skipping the ones the
/// target filesystem or our privileges refuse. The buffers are the largest
/// `os.listxattr` / `os.getxattr` try (XATTR_LIST_MAX, XATTR_SIZE_MAX, both
/// 64 KiB, also the kernel's own limits): past that Python raises ERANGE too.
/// Names are passed through as bytes, as `os.fsdecode` / `fsencode` do.
fn copy_xattr(src: &Path, dst: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    use rustix::io::Errno;
    let mut list = vec![0u8; 65536];
    let n = match rustix::fs::listxattr(src, &mut list[..]) {
        Ok(n) => n,
        Err(e) if [Errno::NOTSUP, Errno::NODATA, Errno::INVAL].contains(&e) => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let mut value = vec![0u8; 65536];
    for name in list[..n].split(|&b| b == 0).filter(|s| !s.is_empty()) {
        let name = std::ffi::OsStr::from_bytes(name);
        let r = rustix::fs::getxattr(src, name, &mut value[..])
            .and_then(|len| rustix::fs::setxattr(dst, name, &value[..len], rustix::fs::XattrFlags::empty()));
        match r {
            Ok(()) => {}
            Err(e) if [Errno::PERM, Errno::NOTSUP, Errno::NODATA, Errno::INVAL, Errno::ACCESS].contains(&e) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// `tempfile.TemporaryDirectory(dir=...)`: removed on the way out even when
/// the tree inside has read-only directories (its cleanup makes them
/// writable and retries, where a plain remove_dir_all gives up).
struct TempTree(Option<PathBuf>);

impl TempTree {
    /// `mkdtemp(prefix="tmp", dir=dir)`: "tmp" and eight characters of
    /// `_RandomNameSequence` (a-z, 0-9, '_'), made 0o700, retried on a
    /// name that exists; the path made absolute.
    fn new_in(dir: &Path) -> std::io::Result<TempTree> {
        use std::hash::BuildHasher;
        use std::os::unix::fs::DirBuilderExt;
        const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789_";
        let state = std::collections::hash_map::RandomState::new();
        for attempt in 0..10000u64 {
            let mut name = String::from("tmp");
            for k in 0..8u64 {
                let r = state.hash_one((attempt, k, util::now().to_bits()));
                name.push(char::from(CHARS[(r % CHARS.len() as u64) as usize]));
            }
            let file = std::path::absolute(dir.join(&name))?;
            match std::fs::DirBuilder::new().mode(0o700).create(&file) {
                Ok(()) => return Ok(TempTree(Some(file))),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "No usable temporary directory name found"))
    }

    fn path(&self) -> &Path {
        self.0.as_deref().unwrap_or(Path::new(""))
    }

    /// The `with` block's exit; a failure here is what the Python raises.
    fn cleanup(mut self) -> std::io::Result<()> {
        match self.0.take() {
            Some(p) => rmtree_writable(&p),
            None => Ok(()),
        }
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = rmtree_writable(&p);
        }
    }
}

/// Give every directory under `p` (not through symlinks) owner rwx, then
/// remove the tree. Already gone is fine.
fn rmtree_writable(p: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fn open_up(d: &Path) {
        if let Ok(m) = std::fs::symlink_metadata(d) {
            if m.is_dir() {
                let mode = m.permissions().mode();
                if mode & 0o700 != 0o700 {
                    let _ = std::fs::set_permissions(d, std::fs::Permissions::from_mode(mode | 0o700));
                }
                if let Ok(rd) = std::fs::read_dir(d) {
                    for e in rd.filter_map(Result::ok) {
                        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                            open_up(&e.path());
                        }
                    }
                }
            }
        }
    }
    open_up(p);
    match std::fs::remove_dir_all(p) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        r => r,
    }
}

/// Give a tracked AppImage a launcher entry and icon.
///
/// An AppImage ships its own .desktop and icon; extracting them is what makes
/// it a real app rather than a file we happen to know about. Exec is rewritten
/// to the stored path, since the shipped one assumes the AppImage runtime's
/// own mount point.
///
/// `Ok(false)` when the AppImage would not extract or carries no .desktop;
/// an error is an I/O failure the Python lets propagate.
pub fn install_desktop_entry(appimage_path: &Path, name: &str) -> Result<bool, String> {
    let io = |e: std::io::Error| format!("OSError: {e}");
    let apps_dir = xdg::data_home().join("applications");
    let icons_dir = xdg::data_home().join("icons/hicolor");
    std::fs::create_dir_all(&apps_dir).map_err(io)?;
    // `str(appimage_path)`: its own bytes in argv; in the .desktop text it
    // must be UTF-8 (`write_text` refuses the surrogates a non-UTF-8 name
    // decodes to).
    let app_os = py_norm(appimage_path);
    let app_str = app_os.to_string_lossy().into_owned();
    let app_utf8 = app_os.to_str().is_some();

    let tmp = TempTree::new_in(&py_parent(appimage_path)).map_err(io)?;
    let inside = || -> Result<bool, String> {
        let argv = [app_os.as_os_str(), std::ffi::OsStr::new("--appimage-extract")];
        if run_bytes(&argv, 180, Some(tmp.path())).is_none() {
            return Ok(false);
        }
        let root = tmp.path().join("squashfs-root");
        let desktops = py_glob(&root, "*.desktop");
        let Some(desktop) = desktops.first() else {
            return Ok(false);
        };

        let icon_name = format!("almanac-{name}");
        for size in ["512x512", "256x256", "128x128", "64x64", "48x48"] {
            if let Some(src) = py_glob(&root, &format!("usr/share/icons/hicolor/{size}/apps/*.png")).first() {
                let d = icons_dir.join(size).join("apps");
                std::fs::create_dir_all(&d).map_err(io)?;
                copy2(src, &d.join(format!("{icon_name}.png"))).map_err(io)?;
            }
        }

        let raw = std::fs::read(desktop).map_err(io)?;
        let text = String::from_utf8_lossy(&raw).replace("\r\n", "\n").replace('\r', "\n");
        let mut lines: Vec<String> = Vec::new();
        let mut unencodable = false;
        for line in py_splitlines(&text) {
            if line.starts_with("Exec=") || line.starts_with("TryExec=") {
                unencodable |= !app_utf8;
            }
            if line.starts_with("Exec=") {
                // Preserve any field codes (%F, %U) the entry declared.
                let tail = line.split_once('=').map(|x| x.1).unwrap_or("");
                let codes: Vec<&str> = py_split(tail).filter(|t| t.starts_with('%')).collect();
                lines.push(py_strip(&format!("Exec={app_str} {}", codes.join(" "))).to_string());
            } else if line.starts_with("Icon=") {
                lines.push(format!("Icon={icon_name}"));
            } else if line.starts_with("TryExec=") {
                lines.push(format!("TryExec={app_str}"));
            } else {
                lines.push(line.to_string());
            }
        }
        let entry = apps_dir.join(format!("almanac-{name}.desktop"));
        if unencodable {
            // open(mode="w") has created the file before the write fails.
            std::fs::File::create(&entry).map_err(io)?;
            return Err("UnicodeEncodeError: 'utf-8' codec can't encode characters: surrogates not allowed".into());
        }
        std::fs::write(entry, lines.join("\n") + "\n").map_err(io)?;
        Ok(true)
    };
    let r = inside();
    // Leaving the `with`: the cleanup's own failure is what propagates.
    tmp.cleanup().map_err(io)?;
    if !r? {
        return Ok(false);
    }

    let arg = std::ffi::OsStr::new;
    for cmd in [
        vec![arg("update-desktop-database"), apps_dir.as_os_str()],
        vec![arg("gtk-update-icon-cache"), arg("-f"), arg("-t"), icons_dir.as_os_str()],
    ] {
        let _ = run_bytes(&cmd, 30, None);
    }
    Ok(true)
}

/// Remove the launcher entry and icons `install_desktop_entry` added.
///
/// A file that is already gone is fine (`missing_ok`); any other failure is
/// returned, as the Python raises.
pub fn remove_desktop_entry(name: &str) -> Result<(), String> {
    let unlink = |p: &Path| match std::fs::remove_file(p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("OSError: {e}")),
    };
    let apps = xdg::data_home().join("applications").join(format!("almanac-{name}.desktop"));
    unlink(&apps)?;
    for png in py_glob(&xdg::data_home().join("icons/hicolor"), &format!("*/apps/almanac-{name}.png")) {
        unlink(&png)?;
    }
    Ok(())
}

/// `urllib.request.urlopen` for http and https: the request it sends (GET,
/// HTTP/1.1, `Accept-Encoding: identity`, `Connection: close`, no
/// decompression), `http.client`'s reading of the response (lenient
/// Content-Length, chunked bodies, a short body returned without complaint),
/// a timeout on every socket operation rather than on the whole transfer,
/// HTTPErrorProcessor (anything outside 2xx is an error unless the redirect
/// handler follows it), the redirect rules, proxies from `<scheme>_proxy`
/// as urllib picks them, and OpenSSL's trust store (`SSL_CERT_FILE` /
/// `SSL_CERT_DIR` included). Errors carry Python's exception name; only that
/// there is one matters.
///
/// Same code as `icons`' copy (private there).
mod urllib {
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

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
    if digits == 0 || last_us || (base == 10 && digits > super::INT_MAX_STR_DIGITS) {
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

const TLS_SUFFIX_EOF: std::io::ErrorKind = std::io::ErrorKind::UnexpectedEof;

enum Sock {
    Tcp(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Read for Sock {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Sock::Tcp(s) => s.read(buf),
            // suppress_ragged_eofs: a close without close_notify is an EOF.
            Sock::Tls(s) => match s.read(buf) {
                Err(e) if e.kind() == TLS_SUFFIX_EOF => Ok(0),
                r => r,
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

/// The default HTTPS context: the system's CA store (as OpenSSL finds it,
/// `SSL_CERT_FILE` / `SSL_CERT_DIR` included), TLS 1.2 and up, ALPN
/// http/1.1. Built once, like urllib's.
fn tls_config() -> Result<Arc<rustls::ClientConfig>, String> {
    static CFG: OnceLock<Result<Arc<rustls::ClientConfig>, String>> = OnceLock::new();
    CFG.get_or_init(|| {
        let anchors = system_certs();
        let mut roots = rustls::RootCertStore::empty();
        roots.add_parsable_certificates(anchors.clone());
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let inner = rustls::client::WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone())
            .build()
            .map_err(|e| format!("ssl.SSLError: {e}"))?;
        let mut cfg = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| format!("ssl.SSLError: {e}"))?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(TrustStore { inner, anchors }))
            .with_no_client_auth();
        cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Arc::new(cfg))
    })
    .clone()
}

/// OpenSSL's verification as rustls' WebPKI does it, plus the one case
/// WebPKI refuses and OpenSSL accepts: a self-signed certificate that is
/// itself in the trust store (a locally trusted certificate, often marked
/// CA:TRUE), still checked for its validity period and the host name.
#[derive(Debug)]
struct TrustStore {
    inner: Arc<rustls::client::WebPkiServerVerifier>,
    anchors: Vec<rustls::pki_types::CertificateDer<'static>>,
}

impl rustls::client::danger::ServerCertVerifier for TrustStore {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        intermediates: &[rustls::pki_types::CertificateDer<'_>],
        server_name: &rustls::pki_types::ServerName<'_>,
        ocsp_response: &[u8],
        now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let err = match self.inner.verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now) {
            Ok(v) => return Ok(v),
            Err(e) => e,
        };
        if !self.anchors.iter().any(|a| a.as_ref() == end_entity.as_ref()) {
            return Err(err);
        }
        let Some((issuer, subject, not_before, not_after)) = cert_names_and_validity(end_entity.as_ref()) else {
            return Err(err);
        };
        let t = i64::try_from(now.as_secs()).unwrap_or(i64::MAX);
        if issuer != subject || t < not_before || t > not_after {
            return Err(err);
        }
        let parsed = rustls::server::ParsedCertificate::try_from(end_entity)?;
        rustls::client::verify_server_name(&parsed, server_name)?;
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

/// One DER element: tag, the whole element, its contents, what follows.
fn der_next(b: &[u8]) -> Option<(u8, &[u8], &[u8], &[u8])> {
    let tag = *b.first()?;
    let first = *b.get(1)? as usize;
    let (len, head) = if first < 0x80 {
        (first, 2)
    } else {
        let n = first & 0x7f;
        if n == 0 || n > 4 {
            return None;
        }
        let mut len = 0usize;
        for k in 0..n {
            len = (len << 8) | *b.get(2 + k)? as usize;
        }
        (len, 2 + n)
    };
    let end = head.checked_add(len)?;
    if end > b.len() {
        return None;
    }
    Some((tag, &b[..end], &b[head..end], &b[end..]))
}

/// An X.509 Time (UTCTime or GeneralizedTime) as Unix seconds.
fn der_time(tag: u8, v: &[u8]) -> Option<i64> {
    let s = std::str::from_utf8(v).ok()?.strip_suffix('Z')?;
    let full = match tag {
        0x17 if s.len() == 12 => {
            let yy: i32 = s[..2].parse().ok()?;
            format!("{}{}", if yy < 50 { 2000 + yy } else { 1900 + yy }, &s[2..])
        }
        0x18 if s.len() == 14 => s.to_string(),
        _ => return None,
    };
    let dt = chrono::NaiveDateTime::parse_from_str(&full, "%Y%m%d%H%M%S").ok()?;
    Some(dt.and_utc().timestamp())
}

/// A certificate's issuer and subject (as DER) and its validity period.
fn cert_names_and_validity(der: &[u8]) -> Option<(&[u8], &[u8], i64, i64)> {
    let (_, _, cert, _) = der_next(der)?;
    let (_, _, tbs, _) = der_next(cert)?;
    let mut r = tbs;
    if r.first() == Some(&0xa0) {
        r = der_next(r)?.3; // version
    }
    r = der_next(r)?.3; // serialNumber
    r = der_next(r)?.3; // signature
    let (_, issuer, _, rest) = der_next(r)?;
    let (_, _, validity, rest) = der_next(rest)?;
    let (_, subject, _, _) = der_next(rest)?;
    let (t1, _, nb, more) = der_next(validity)?;
    let (t2, _, na, _) = der_next(more)?;
    Some((issuer, subject, der_time(t1, nb)?, der_time(t2, na)?))
}

fn system_certs() -> Vec<rustls::pki_types::CertificateDer<'static>> {
    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::CertificateDer;
    let mut out = Vec::new();
    let file = std::env::var_os("SSL_CERT_FILE").unwrap_or_else(|| "/etc/ssl/cert.pem".into());
    if let Ok(it) = CertificateDer::pem_file_iter(&file) {
        out.extend(it.flatten());
    }
    // A capath is looked up by subject hash: <8 hex digits>.<n>.
    let dirs = std::env::var_os("SSL_CERT_DIR").unwrap_or_else(|| "/etc/ssl/certs".into());
    for dir in dirs.as_bytes().split(|&b| b == b':') {
        let Ok(rd) = std::fs::read_dir(Path::new(std::ffi::OsStr::from_bytes(dir))) else { continue };
        for e in rd.flatten() {
            let name = e.file_name();
            let n = name.as_bytes();
            let hashed = n.len() > 9
                && n[..8].iter().all(u8::is_ascii_hexdigit)
                && n[8] == b'.'
                && n[9..].iter().all(u8::is_ascii_digit);
            if hashed {
                if let Ok(it) = CertificateDer::pem_file_iter(e.path()) {
                    out.extend(it.flatten());
                }
            }
        }
    }
    out
}

fn wrap_tls(tcp: TcpStream, server_hostname: &str) -> Result<Sock, String> {
    let cfg = tls_config()?;
    let name = rustls::pki_types::ServerName::try_from(idna_encode(server_hostname)?)
        .map_err(|e| format!("ValueError: {e}"))?;
    let mut tls = rustls::ClientConnection::new(cfg, name).map_err(|e| format!("ssl.SSLError: {e}"))?;
    let mut tcp = tcp;
    while tls.is_handshaking() {
        tls.complete_io(&mut tcp).map_err(|e| format!("ssl.SSLError: {e}"))?;
    }
    Ok(Sock::Tls(Box::new(rustls::StreamOwned::new(tls, tcp))))
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
    /// `fp is None`: the body is done, every read gives b"".
    closed: bool,
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
        Ok(HttpResp { conn, status, headers, chunked, chunk_left: None, length, closed: false })
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
                // we read everything; close the "file"
                self.chunk_left = None;
                self.closed = true;
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
        if self.closed {
            return Ok(Vec::new());
        }
        if self.chunked {
            return self.read_chunked(Some(amt as i128));
        }
        // clip the read to the "end of response"
        let amt = match self.length {
            Some(l) if (amt as i128) > l => usize::try_from(l).unwrap_or(0),
            _ => amt,
        };
        let s = self.conn.read(amt)?;
        if s.is_empty() && amt > 0 {
            // Ideally, we would raise IncompleteRead if the content-length
            // wasn't satisfied, but it might break compatibility.
            self.closed = true;
        } else if let Some(l) = self.length {
            let left = l - s.len() as i128;
            self.length = Some(left);
            if left == 0 {
                self.closed = true;
            }
        }
        Ok(s)
    }

    /// `read()`.
    fn read_all(&mut self) -> Result<Vec<u8>, String> {
        if self.closed {
            return Ok(Vec::new());
        }
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
        // Reached only through a proxy whose URL names another scheme. Such a
        // response has no status, and HTTPErrorProcessor then raises on it.
        "file" | "data" => Ok(Resp::Local),
        // urllib would try an FTP download (a redirect may lead there).
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


/// What urllib fixes when it builds its opener, on the first `urlopen` of
/// the process: the proxy table and the HTTPS context.
pub(super) fn build_opener() {
    let _ = proxies();
    let _ = tls_config();
}

/// `Request(url, headers=UA)`'s checks that come before any handler:
/// `request_host` runs `urlparse`, which may raise.
pub(super) fn check_request(full_url: &str) -> Result<(), String> {
    urlparse(full_url).map(|_| ())
}

/// `urlopen(Request(url, headers=UA), timeout=...)` for an http(s) URL:
/// the response's body and `Content-Length` header.
pub(super) fn open_http(url: &str, ua: (&str, &str), timeout: Duration) -> Result<(Box<dyn Read + Send>, Option<String>), String> {
    let headers = [(ua.0.to_string(), ua.1.to_string())];
    let req = Req::new(url, &headers, true, timeout)?;
    match open(req, 0)? {
        Resp::Http(r) => {
            let length = header_get(&r.headers, "content-length");
            Ok((Box::new(Body(r)), length))
        }
        Resp::Local => Err("TypeError: '<=' not supported between instances of 'int' and 'NoneType'".into()),
    }
}

/// `HTTPResponse.read(n)` as a reader.
struct Body(Box<HttpResp>);

impl Read for Body {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let data = self.0.read(buf.len()).map_err(std::io::Error::other)?;
        buf[..data.len()].copy_from_slice(&data);
        Ok(data.len())
    }
}
}

// ---------------------------------------------------------------------------

fn result_json<T: Serialize>(r: Result<Option<T>, String>) -> Value {
    match r {
        Ok(Some(v)) => serde_json::to_value(v).unwrap_or_default(),
        Ok(None) => serde_json::json!({}),
        // Inside a combined answer only the fact of the exception is compared
        // (the Python case maps any exception to the same marker).
        Err(_) => serde_json::json!({ "error": true }),
    }
}

/// Every file under `dir`, relative path -> contents (lossy text), so a
/// file-writing call can be compared without comparing temp dir names.
fn tree(dir: &Path) -> serde_json::Map<String, Value> {
    fn walk(base: &Path, d: &Path, out: &mut serde_json::Map<String, Value>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.filter_map(Result::ok) {
            let p = e.path();
            if p.is_dir() {
                walk(base, &p, out);
            } else {
                let rel = p.strip_prefix(base).unwrap_or(&p).to_string_lossy().into_owned();
                let body = std::fs::read(&p).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
                out.insert(rel, Value::String(body));
            }
        }
    }
    let mut out = serde_json::Map::new();
    walk(dir, dir, &mut out);
    let mut keys: Vec<_> = out.into_iter().collect();
    keys.sort_by(|a, b| a.0.cmp(&b.0));
    keys.into_iter().collect()
}

/// Parity hook: see ARCHITECTURE.md.
///
/// `args.env` (optional, `{VAR: value | null}`) is applied for the call.
/// `latest_github` and `check` take `data`: the JSON the GitHub cache would
/// return (the Python case patches `ghcache.get_json`), and answer
/// `{"result": ..., "urls": [...]}` so the URL asked for is compared too.
pub fn parity(func: &str, args: &Value) -> Option<Value> {
    use serde_json::json;
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
    let s = |k: &str| util::arg_str(args, k).to_string();
    let data = args.get("data").cloned();
    let urls = std::cell::RefCell::new(Vec::<String>::new());
    let fetch = |u: &str| {
        urls.borrow_mut().push(u.to_string());
        data.clone().filter(|d| !d.is_null())
    };
    let out = match func {
        "read_update_info" => Some(json!(read_update_info(Path::new(&s("path"))))),
        "parse_update_info" => Some(match parse_update_info(&s("info")) {
            Some(spec) => serde_json::to_value(spec).unwrap_or_default(),
            None => json!({}),
        }),
        "_version_of" => Some(json!(version_of(&s("text")))),
        "_vkey" => Some(match vkey(&s("v")) {
            // JSON here has no integers past i64 (no arbitrary precision):
            // a bigger component goes out as the nearest f64.
            Ok(k) => json!(k
                .iter()
                .map(|x| match x.to_string().parse::<i64>() {
                    Ok(i) => json!(i),
                    Err(_) => json!(x.to_string().parse::<f64>().unwrap_or(0.0)),
                })
                .collect::<Vec<_>>()),
            Err(e) => json!({ "error": e }),
        }),
        "latest_github" => {
            let r = latest_impl(
                &s("owner"),
                &s("repo"),
                Some(args.get("pattern").unwrap_or(&json!("*.AppImage"))),
                args.get("release").unwrap_or(&json!("latest")),
                &fetch,
            );
            Some(json!({ "result": result_json(r), "urls": *urls.borrow() }))
        }
        "check" => {
            let record = args.get("record").cloned().unwrap_or(json!({}));
            let r = check_impl(&record, &|o, rp, p, rel| latest_impl(o, rp, p, rel, &fetch));
            Some(json!({ "result": result_json(r), "urls": *urls.borrow() }))
        }
        "download" => {
            let dir = PathBuf::from(s("dir"));
            let dest = dir.join(s("name"));
            let seen = std::cell::RefCell::new(Vec::<f64>::new());
            let cb = |f: f64| seen.borrow_mut().push(f);
            let r = download(&s("url"), &dest, Some(&cb));
            let result = match r {
                Ok(p) => json!(p.strip_prefix(&dir).unwrap_or(&p).to_string_lossy()),
                Err(_) => json!({ "error": true }),
            };
            Some(json!({ "result": result, "progress": *seen.borrow(), "files": tree(&dir) }))
        }
        "install_desktop_entry" => {
            let r = install_desktop_entry(Path::new(&s("path")), &s("name"));
            let result = match r {
                Ok(b) => json!(b),
                Err(_) => json!({ "error": true }),
            };
            let files: serde_json::Map<String, Value> =
                tree(&xdg::data_home()).into_iter().filter(|(k, _)| !k.ends_with("icon-theme.cache") && !k.ends_with("mimeinfo.cache")).collect();
            Some(json!({ "result": result, "files": files }))
        }
        "remove_desktop_entry" => {
            let result = match remove_desktop_entry(&s("name")) {
                Ok(()) => Value::Null,
                Err(_) => json!({ "error": true }),
            };
            Some(json!({ "result": result, "files": tree(&xdg::data_home()) }))
        }
        "_fnmatch" => Some(json!(fnmatch(&s("name"), &s("pat")))),
        _ => None,
    };
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
    use serde_json::json;

    fn latest(version: &str, url: &str) -> LatestRelease {
        LatestRelease { version: version.into(), url: url.into(), ..Default::default() }
    }

    // AppImageUpdateInfo

    #[test]
    fn test_parses_github_release_spec() {
        let spec = parse_update_info(
            "gh-releases-zsync|storytold|vectorcraft|latest|vectorcraft-*-linux-x86_64.AppImage.zsync",
        )
        .unwrap_or_default();
        assert_eq!(spec.kind, "github");
        assert_eq!(spec.owner, "storytold");
        assert_eq!(spec.repo, "vectorcraft");
    }

    #[test]
    fn test_unknown_scheme_yields_nothing() {
        // zsync-over-http needs a zsync client we do not have; claiming support
        // would mean offering an update that cannot be applied.
        assert_eq!(parse_update_info("zsync|https://example/x.zsync"), None);
        assert_eq!(parse_update_info(""), None);
    }

    #[test]
    fn test_version_ordering_is_numeric() {
        // The bug a string compare would introduce: 0.9.9 > 0.10.2 as text.
        assert!(vkey("0.9.9").unwrap() < vkey("0.10.2").unwrap());
    }

    #[test]
    fn test_same_or_older_release_is_not_an_update() {
        let fake = |_: &str, _: &str, _: Option<&Value>, _: &Value| Ok(Some(latest("1.0.0", "u")));
        let mut rec = json!({"path": file!(), "version": "1.0.0",
                             "update_spec": {"kind": "github", "owner": "o", "repo": "r"}});
        // file!() is relative to the crate root, where cargo runs tests.
        assert_eq!(check_impl(&rec, &fake), Ok(None));
        rec["version"] = json!("2.0.0");
        assert_eq!(check_impl(&rec, &fake), Ok(None));
    }

    #[test]
    fn test_newer_release_is_reported() {
        let fake = |_: &str, _: &str, _: Option<&Value>, _: &Value| Ok(Some(latest("2.0.0", "u")));
        let rec = json!({"path": file!(), "version": "1.0.0",
                         "update_spec": {"kind": "github", "owner": "o", "repo": "r"}});
        assert_eq!(check_impl(&rec, &fake).unwrap().map(|l| l.version), Some("2.0.0".to_string()));
    }

    // AppImageDownloadSafety: the replacement must never leave the user
    // without a working binary.

    /// A one-shot HTTP server on localhost answering `body`.
    fn serve(body: &'static [u8]) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = s.write_all(head.as_bytes());
                let _ = s.write_all(body);
            }
        });
        format!("http://{addr}/x")
    }

    #[test]
    fn test_previous_build_is_kept_for_rollback() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("app.AppImage");
        std::fs::write(&dest, b"OLD").unwrap();
        download(&serve(b"NEW"), &dest, None).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"NEW");
        assert_eq!(
            std::fs::read(dest.with_extension("previous")).unwrap(),
            b"OLD",
            "the old build must survive so a bad update is revertible"
        );
    }

    #[test]
    fn test_failed_download_leaves_the_original_intact() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("app.AppImage");
        std::fs::write(&dest, b"OLD").unwrap();
        // A port nothing listens on: the connection dies.
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        assert!(download(&format!("http://127.0.0.1:{port}/x"), &dest, None).is_err());
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            b"OLD",
            "a dropped connection must not destroy the installed build"
        );
    }

    #[test]
    fn test_with_suffix_matches_pathlib() {
        let w = |p: &str, s: &str| py_with_suffix(Path::new(p), s).map(|p| p.to_string_lossy().into_owned());
        assert_eq!(w("/a/app.AppImage", ".previous").unwrap(), "/a/app.previous");
        assert_eq!(w("/a/app", ".previous").unwrap(), "/a/app.previous");
        assert_eq!(w("/a/.hidden", ".previous").unwrap(), "/a/.hidden.previous");
        assert_eq!(w("/a/x.tar.gz", ".download").unwrap(), "/a/x.tar.download");
        assert_eq!(w("/q/app.", ".previous").unwrap(), "/q/app.previous");
        assert_eq!(w("/q/app..x", ".previous").unwrap(), "/q/app..previous");
        assert_eq!(w("/q/..", ".previous").unwrap(), "/q/...previous");
        assert!(w("/", ".x").is_err());
    }

    // Review round 2: behaviour the parity cases pinned down.

    /// A one-shot server answering a raw HTTP response.
    fn serve_raw(resp: Vec<u8>) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                let _ = s.write_all(&resp);
            }
        });
        format!("http://{addr}/x")
    }

    #[test]
    fn test_short_body_is_kept_like_http_client() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("app.AppImage");
        let url = serve_raw(b"HTTP/1.0 200 OK\r\nContent-Length: 100\r\n\r\nNEW".to_vec());
        let seen = std::cell::RefCell::new(Vec::new());
        let cb = |f: f64| seen.borrow_mut().push(f);
        download(&url, &dest, Some(&cb)).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"NEW");
        assert_eq!(*seen.borrow(), vec![0.03]);
    }

    #[test]
    fn test_encoded_body_is_written_as_sent() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("app.AppImage");
        // gzip of "x", sent although identity was asked for.
        let body: &[u8] = &[0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3, 0xab, 0, 0, 0x83, 0x16, 0xdc, 0x8c, 1, 0, 0, 0];
        let mut resp = format!("HTTP/1.0 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
        resp.extend_from_slice(body);
        download(&serve_raw(resp), &dest, None).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), body);
    }

    #[test]
    fn test_file_and_data_urls_download() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("s rc");
        std::fs::write(&src, b"FILE").unwrap();
        let dest = tmp.path().join("app.AppImage");
        download(&format!("file://{}", tmp.path().join("s%20rc").display()), &dest, None).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"FILE");
        download("data:,NEW%41#frag", &dest, None).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"NEWA#frag");
        download("data:text/plain;base64,TkVX", &dest, None).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"NEW");
        assert!(download("data:no-comma", &dest, None).is_err());
        assert!(download("gopher://x/y", &dest, None).is_err());
    }

    #[test]
    fn test_copy2_of_a_read_only_file() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("a.png");
        std::fs::write(&src, b"PNG").unwrap();
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o444)).unwrap();
        let dst = tmp.path().join("b.png");
        copy2(&src, &dst).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"PNG");
        let (a, b) = (std::fs::metadata(&src).unwrap(), std::fs::metadata(&dst).unwrap());
        assert_eq!(b.permissions().mode() & 0o777, 0o444);
        assert_eq!(a.modified().unwrap(), b.modified().unwrap());
    }

    #[test]
    fn test_temp_tree_removes_read_only_dirs() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let t = TempTree::new_in(tmp.path()).unwrap();
        let ro = t.path().join("squashfs-root/ro");
        std::fs::create_dir_all(&ro).unwrap();
        std::fs::write(ro.join("file"), b"x").unwrap();
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
        t.cleanup().unwrap();
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
    }

    #[test]
    fn test_glob_splits_the_pattern_on_slashes() {
        let tmp = tempfile::tempdir().unwrap();
        for rel in ["48x48/apps/almanac-x/y.png", "48x48/apps/almanac-x/z.png", ".h/apps/almanac-t.png", "f/almanac-t.png"] {
            let p = tmp.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, b"").unwrap();
        }
        let rel = |v: Vec<PathBuf>| -> Vec<String> {
            v.iter().map(|p| p.strip_prefix(tmp.path()).unwrap().to_string_lossy().into_owned()).collect()
        };
        assert_eq!(rel(py_glob(tmp.path(), "*/apps/almanac-x/y.png")), ["48x48/apps/almanac-x/y.png"]);
        assert_eq!(rel(py_glob(tmp.path(), "*/apps/almanac-t.png")), [".h/apps/almanac-t.png"]);
        let mut all = rel(py_glob(tmp.path(), "**/*.png"));
        all.sort();
        assert_eq!(all.len(), 4);
    }

    #[test]
    fn test_vkey_compares_big_components_exactly() {
        let a = vkey(&format!("1.1{}", "0".repeat(42))).unwrap();
        let b = vkey(&format!("1.2{}", "0".repeat(42))).unwrap();
        assert!(a < b);
        assert!(vkey(&format!("1.{}", "1".repeat(5000))).is_err());
        assert!(vkey(&"1_".repeat(4300).trim_end_matches('_').to_string()).is_ok());
    }

    #[test]
    fn test_release_values_pass_through() {
        let data = json!({"tag_name": null, "html_url": 7,
                          "assets": [{"name": "a.AppImage", "browser_download_url": null}]});
        let r = latest_from(Some(data), None).unwrap().unwrap();
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v, json!({"version": null, "tag": null, "url": null, "asset": "a.AppImage",
                             "published": "", "page": 7}));
        let back: LatestRelease = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(serde_json::to_value(&back).unwrap(), v);
        // A stored dict comes back as it was: odd keys, order, a null spec.
        let stored = json!({"url": "u", "x": [1], "version": "2", "spec": null});
        let back: LatestRelease = serde_json::from_value(stored.clone()).unwrap();
        assert_eq!(back.value("url"), json!("u"));
        assert_eq!(serde_json::to_string(&back).unwrap(), serde_json::to_string(&stored).unwrap());
        assert!(latest_from(Some(json!({"tag_name": 5})), None).is_err());
        assert!(latest_from(Some(json!({"published_at": true})), None).is_err());
    }

    // Review round 3.

    #[test]
    fn test_fnmatch_negation_after_dropped_range() {
        // "[a-*!b]": the dropped range leaves "!b", which Python reads as [^b].
        assert!(fnmatch("c", "[a-*!b]"));
        assert!(!fnmatch("b", "[a-*!b]"));
        assert!(fnmatch("c", "[a-*!]"));
        assert!(fnmatch("a&", "a[&&]"));
        assert!(fnmatch("a^", "a[^]"));
    }

    #[test]
    fn test_check_of_a_record_that_is_not_a_dict() {
        let fake = |_: &str, _: &str, _: Option<&Value>, _: &Value| Ok(Some(latest("2.0.0", "u")));
        for rec in [json!(null), json!([]), json!("text"), json!(5)] {
            assert!(check_impl(&rec, &fake).is_err());
        }
    }

    #[test]
    fn test_non_2xx_and_lenient_content_length() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("app.AppImage");
        std::fs::write(&dest, b"OLD").unwrap();
        let url = serve_raw(b"HTTP/1.0 304 Not Modified\r\nContent-Length: 3\r\n\r\nNEW".to_vec());
        assert!(download(&url, &dest, None).is_err());
        assert_eq!(std::fs::read(&dest).unwrap(), b"OLD");
        for len in ["+10", "1_0", "-5"] {
            let resp = format!("HTTP/1.0 200 OK\r\nContent-Length: {len}\r\n\r\n0123456789");
            download(&serve_raw(resp.into_bytes()), &dest, None).unwrap();
            assert_eq!(std::fs::read(&dest).unwrap(), b"0123456789");
        }
        download("data:,abc#", &dest, None).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"abc");
    }

    #[test]
    fn test_copy2_of_a_directory_creates_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("dir.png");
        std::fs::create_dir(&src).unwrap();
        let dst = tmp.path().join("out.png");
        assert!(copy2(&src, &dst).is_err());
        assert!(!dst.exists());
    }

    #[test]
    fn test_run_bytes_times_out_on_a_held_pipe() {
        let sh = std::ffi::OsStr::new("sh");
        let started = Instant::now();
        let argv = [sh, std::ffi::OsStr::new("-c"), std::ffi::OsStr::new("(sleep 5) & echo x")];
        assert_eq!(run_bytes(&argv, 1, None), None);
        assert!(started.elapsed() < Duration::from_secs(4));
    }

    #[test]
    fn test_printable_knows_unassigned_code_points() {
        assert_eq!(py_str(&json!(["\u{378}", "\u{2065}", "é"])), "['\\u0378', '\\u2065', 'é']");
    }

    #[test]
    fn test_py_str_formats_like_python() {
        assert_eq!(py_str(&json!({"a": 1})), "{'a': 1}");
        assert_eq!(py_str(&json!([1, "x'y", null])), "[1, \"x'y\", None]");
        assert_eq!(py_str(&serde_json::from_str::<Value>("100000000000000000000").unwrap()), "100000000000000000000");
        assert_eq!(py_str(&json!("\u{a0}")), "\u{a0}");
        assert_eq!(py_str(&json!(["\u{a0}\n"])), "['\\xa0\\n']");
    }
}
