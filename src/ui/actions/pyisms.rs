//! Small helpers: Python's str, repr and OSError text, probes, numbers.

use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use crate::inventory::App;
use crate::verify::ProbeValue;

/// `app.extra.get(key, "")` as text: a missing key or null is "".
pub(super) fn extra_str(app: &App, key: &str) -> String {
    match app.extra.get(key) {
        Some(Value::String(s)) => s.clone(),
        None | Some(Value::Null) => String::new(),
        Some(other) => other.to_string(),
    }
}

/// The TypeError subprocess raises for an argv item that is not a path.
pub(super) fn not_a_path(v: &Value) -> String {
    format!("TypeError: expected str, bytes or os.PathLike object, not {}", py_type(v))
}

/// Python truthiness of a JSON value.
pub(in crate::ui) fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `type(v).__name__` of a JSON value as Python loads it.
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

/// Python's `repr()` of a str.
pub(super) fn py_str_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python's `repr()` of a JSON value as Python loads it.
fn py_repr(v: &Value) -> String {
    match v {
        Value::String(s) => py_str_repr(s),
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, x)| format!("{}: {}", py_str_repr(k), py_repr(x)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => py_str(other),
    }
}

/// `f"{v}"` of a JSON value as Python loads it.
pub(super) fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".to_string(),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        Value::Number(n) => match n.as_f64() {
            Some(f) if n.is_f64() => crate::util::py_float_repr(f),
            _ => n.to_string(),
        },
        Value::String(s) => s.clone(),
        other => py_repr(other),
    }
}

/// `str(Path(s))`: "" is ".", and "." components and trailing slashes go.
pub(super) fn py_path(s: &str) -> PathBuf {
    let p: PathBuf = Path::new(s).components().filter(|c| *c != Component::CurDir).collect();
    if p.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        p
    }
}

/// `repr()` of a `pathlib.Path`.
pub(super) fn path_repr(p: &Path) -> String {
    format!("PosixPath({})", py_str_repr(&p.to_string_lossy()))
}

/// The strerror part of an io::Error, which prints "<strerror> (os error N)".
pub(super) fn strerror(e: &std::io::Error) -> String {
    let text = e.to_string();
    match e.raw_os_error() {
        Some(n) => text.strip_suffix(&format!(" (os error {n})")).unwrap_or(&text).to_string(),
        None => text,
    }
}

/// `str(OSError)`: `[Errno 13] Permission denied: '/path'`.
pub(super) fn py_oserror(e: &std::io::Error, path: &Path) -> String {
    match e.raw_os_error() {
        Some(n) => format!("[Errno {n}] {}: {}", strerror(e), py_str_repr(&path.to_string_lossy())),
        None => e.to_string(),
    }
}

/// `str(OSError)` with a second filename: `[Errno 28] ...: 'a' -> 'b'`.
pub(super) fn py_oserror2(e: &std::io::Error, a: &Path, b: &Path) -> String {
    match e.raw_os_error() {
        Some(n) => format!("[Errno {n}] {}: {} -> {}", strerror(e),
                           py_str_repr(&a.to_string_lossy()), py_str_repr(&b.to_string_lossy())),
        None => e.to_string(),
    }
}

/// The traceback's last line for an OSError: its subclass, then `str()`.
pub(super) fn py_oserror_line(e: &std::io::Error, path: &Path) -> String {
    let class = match e.raw_os_error() {
        Some(1) | Some(13) => "PermissionError",
        Some(2) => "FileNotFoundError",
        Some(17) => "FileExistsError",
        Some(20) => "NotADirectoryError",
        Some(21) => "IsADirectoryError",
        _ => "OSError",
    };
    format!("{class}: {}", py_oserror(e, path))
}

/// `str.isspace()` of one character: Unicode whitespace, plus the
/// information separators \x1c-\x1f, which Python counts and Rust does not.
fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// `s.strip()`.
pub(super) fn py_strip(s: &str) -> &str {
    s.trim_matches(py_isspace)
}

/// `s.splitlines()[0]` of a non-empty string.
pub(super) fn py_first_line(s: &str) -> &str {
    const BREAKS: [char; 10] = ['\n', '\r', '\x0b', '\x0c', '\x1c', '\x1d', '\x1e', '\u{85}',
                                '\u{2028}', '\u{2029}'];
    s.split(BREAKS).next().unwrap_or(s)
}

/// `str(UnicodeDecodeError)` of `bytes.decode("utf-8")`.
pub(super) fn utf8_error(bytes: &[u8], e: &std::str::Utf8Error) -> String {
    let pos = e.valid_up_to();
    let (count, reason) = match e.error_len() {
        Some(n) => {
            let start_ok = matches!(bytes[pos], 0xC2..=0xF4);
            (n, if start_ok { "invalid continuation byte" } else { "invalid start byte" })
        }
        None => (bytes.len() - pos, "unexpected end of data"),
    };
    if count == 1 {
        format!("'utf-8' codec can't decode byte 0x{:02x} in position {pos}: {reason}", bytes[pos])
    } else {
        format!("'utf-8' codec can't decode bytes in position {pos}-{}: {reason}", pos + count - 1)
    }
}

/// Python truthiness of a probe's answer.
pub(super) fn probe_truthy(v: &Option<ProbeValue>) -> bool {
    match v {
        None => false,
        Some(ProbeValue::Str(s)) => !s.is_empty(),
        Some(ProbeValue::Identity(..)) => true,
        Some(ProbeValue::Set(s)) => !s.is_empty(),
        Some(ProbeValue::Bool(b)) => *b,
        Some(ProbeValue::Map(m)) => !m.is_empty(),
    }
}

/// `f"{a}"` of a probe's answer (the installed version, for a package).
pub(super) fn probe_text(v: &Option<ProbeValue>) -> String {
    match v {
        None => "None".to_string(),
        Some(ProbeValue::Str(s)) => s.clone(),
        Some(ProbeValue::Bool(b)) => if *b { "True" } else { "False" }.to_string(),
        Some(other) => format!("{other:?}"),
    }
}

/// `f"{n:,}"`: thousands separated by commas.
pub(super) fn thousands(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

pub(super) fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stars_have_thousands_separators() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(1234567), "1,234,567");
    }

    #[test]
    fn python_text_rules() {
        assert_eq!(py_strip(" \x1c\x1fx\x1f"), "x");
        assert_eq!(py_first_line("a\x1cb"), "a");
        assert_eq!(py_first_line("a\u{85}b"), "a");
        assert_eq!(py_first_line("a"), "a");
        let b: &[u8] = &[b'a', b'b', 0xe2, 0x82];
        #[allow(invalid_from_utf8)] // bad UTF-8 on purpose: the error is what is tested
        let e = std::str::from_utf8(b).unwrap_err();
        assert_eq!(utf8_error(b, &e), "'utf-8' codec can't decode bytes in position 2-3: unexpected end of data");
        let b: &[u8] = &[0xff];
        #[allow(invalid_from_utf8)] // bad UTF-8 on purpose: the error is what is tested
        let e = std::str::from_utf8(b).unwrap_err();
        assert_eq!(utf8_error(b, &e), "'utf-8' codec can't decode byte 0xff in position 0: invalid start byte");
        assert_eq!(py_str(&serde_json::json!(true)), "True");
        assert_eq!(py_str(&serde_json::json!([1, "a"])), "[1, 'a']");
        assert_eq!(py_path("./a//b/"), PathBuf::from("a/b"));
    }
}
