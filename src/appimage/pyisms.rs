//! Small Python-isms this module needs.

use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use num_bigint::BigInt;
use regex::Regex;
use serde_json::Value;

use crate::util;

use super::VERSION_PATTERN;

/// `str.isspace()` for one character: Rust's `is_whitespace` plus the four
/// ASCII separators (`\x1c`..`\x1f`) Python also counts.
fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// `str.strip()`.
pub(super) fn py_strip(s: &str) -> &str {
    s.trim_matches(py_space)
}

/// `str.split()` (no argument).
pub(super) fn py_split(s: &str) -> impl Iterator<Item = &str> {
    s.split(py_space).filter(|t| !t.is_empty())
}

/// `str.splitlines()`.
pub(super) fn py_splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        let brk = matches!(
            c,
            '\n' | '\r'
                | '\x0b'
                | '\x0c'
                | '\x1c'
                | '\x1d'
                | '\x1e'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
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
    let p: PathBuf = Path::new(value)
        .components()
        .filter(|c| *c != Component::CurDir)
        .collect();
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
pub(super) fn py_path(value: &str) -> PathBuf {
    py_path_os(std::ffi::OsStr::new(value))
}

/// A `Path` as pathlib holds it (what `str(path)` gives, as OS text).
pub(super) fn py_norm(p: &Path) -> PathBuf {
    py_path_os(p.as_os_str())
}

/// `Path.parent` of a pathlib-normalised path.
pub(super) fn py_parent(p: &Path) -> PathBuf {
    let p = py_norm(p);
    match p.parent() {
        Some(q) if !q.as_os_str().is_empty() => q.to_path_buf(),
        Some(_) => PathBuf::from("."),
        None => p,
    }
}

/// `Path.with_suffix(suffix)`: replaces the last suffix (".AppImage") or
/// appends when there is none. pathlib refuses a path with an empty name.
pub(super) fn py_with_suffix(p: &Path, suffix: &str) -> Result<PathBuf, String> {
    use std::os::unix::ffi::OsStrExt;
    let p = py_norm(p);
    // pathlib's name is the last component as written, ".." included.
    let name: &[u8] = match p.components().next_back() {
        Some(Component::Normal(n)) if p.as_os_str() != "." => n.as_bytes(),
        Some(Component::ParentDir) => b"..",
        _ => {
            return Err(format!(
                "ValueError: {:?} has an empty name",
                p.to_string_lossy()
            ))
        }
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
pub(super) fn truthy(v: &Value) -> bool {
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
pub(super) fn py_str(v: &Value) -> String {
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
    if f.is_finite()
        && f.fract() == 0.0
        && (f >= 18446744073709551616.0 || f < -9223372036854775808.0)
    {
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
            o.iter()
                .map(|(k, v)| format!("{}: {}", py_str_repr(k), py_repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
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
pub(super) fn py_str_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
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
pub(super) fn py_type(v: &Value) -> &'static str {
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

pub(super) fn version_re() -> &'static Regex {
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
pub(super) const INT_MAX_STR_DIGITS: usize = 4300;

/// `int(text)`: surrounding whitespace, an optional sign, digits with single
/// underscores between them. Anything else is Python's ValueError, and so is
/// a number of more than 4300 digits (Python's integer string limit).
/// Unbounded, like Python's int.
pub(super) fn py_int(text: &str) -> Result<BigInt, String> {
    let err = || {
        format!(
            "ValueError: invalid literal for int() with base 10: {}",
            py_str_repr(text)
        )
    };
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
pub(super) fn fnmatch(name: &str, pat: &str) -> bool {
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
                    .map(|s| {
                        s.iter()
                            .collect::<String>()
                            .replace('\\', "\\\\")
                            .replace('-', "\\-")
                    })
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
/// No `stdin=` is given in the Python, so the child inherits ours. The argv
/// is raw OS text, as Python passes a str path's own bytes.
///
/// `communicate(timeout=...)` bounds the whole exchange: the child's exit
/// *and* the end of its output. A helper the child leaves running that
/// keeps the pipes open runs the clock out too; `util::run_bytes` then
/// kills the child's whole process group.
pub(super) fn run_bytes(
    argv: &[&std::ffi::OsStr],
    timeout_secs: u64,
    cwd: Option<&Path>,
) -> Option<Vec<u8>> {
    run_bytes_within(argv, Duration::from_secs(timeout_secs), cwd)
}

/// `run_bytes` with any timeout (the tests use a fraction of a second).
fn run_bytes_within(
    argv: &[&std::ffi::OsStr],
    timeout: Duration,
    cwd: Option<&Path>,
) -> Option<Vec<u8>> {
    let out = crate::util::run_bytes(
        argv,
        &crate::util::RunOpts {
            timeout: Some(timeout),
            stdin: crate::util::Stdin::Inherit,
            cwd,
            ..Default::default()
        },
    );
    (!out.failed_to_start && !out.timed_out).then_some(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Instant;

    #[test]
    fn test_with_suffix_matches_pathlib() {
        let w = |p: &str, s: &str| {
            py_with_suffix(Path::new(p), s).map(|p| p.to_string_lossy().into_owned())
        };
        assert_eq!(
            w("/a/app.AppImage", ".previous").unwrap(),
            "/a/app.previous"
        );
        assert_eq!(w("/a/app", ".previous").unwrap(), "/a/app.previous");
        assert_eq!(w("/a/.hidden", ".previous").unwrap(), "/a/.hidden.previous");
        assert_eq!(w("/a/x.tar.gz", ".download").unwrap(), "/a/x.tar.download");
        assert_eq!(w("/q/app.", ".previous").unwrap(), "/q/app.previous");
        assert_eq!(w("/q/app..x", ".previous").unwrap(), "/q/app..previous");
        assert_eq!(w("/q/..", ".previous").unwrap(), "/q/...previous");
        assert!(w("/", ".x").is_err());
    }

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
    fn test_run_bytes_times_out_on_a_held_pipe() {
        let sh = std::ffi::OsStr::new("sh");
        let started = Instant::now();
        let argv = [
            sh,
            std::ffi::OsStr::new("-c"),
            std::ffi::OsStr::new("(sleep 5) & echo x"),
        ];
        assert_eq!(
            run_bytes_within(&argv, Duration::from_millis(200), None),
            None
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn test_printable_knows_unassigned_code_points() {
        assert_eq!(
            py_str(&json!(["\u{378}", "\u{2065}", "é"])),
            "['\\u0378', '\\u2065', 'é']"
        );
    }

    #[test]
    fn test_py_str_formats_like_python() {
        assert_eq!(py_str(&json!({"a": 1})), "{'a': 1}");
        assert_eq!(py_str(&json!([1, "x'y", null])), "[1, \"x'y\", None]");
        assert_eq!(
            py_str(&serde_json::from_str::<Value>("100000000000000000000").unwrap()),
            "100000000000000000000"
        );
        assert_eq!(py_str(&json!("\u{a0}")), "\u{a0}");
        assert_eq!(py_str(&json!(["\u{a0}\n"])), "['\\xa0\\n']");
    }
}
