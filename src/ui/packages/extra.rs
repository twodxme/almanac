//! Reading the mixed-type `extra` dict the way Python's builtins do, and
//! the values the details sheet prints.

use std::cmp::Ordering;

use gtk::glib;
use serde_json::Value;

/// Python truthiness of a JSON value.
pub(super) fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// `str(value)` for a JSON value. A list or dict prints as its repr, as
/// Python's `str()` does.
pub(super) fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(_) | Value::Object(_) => py_repr(v),
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(n) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
            (Some(i), _, _) => i.to_string(),
            (_, Some(u), _) => u.to_string(),
            (_, _, Some(f)) => crate::util::py_float_repr(f),
            _ => n.to_string(),
        },
    }
}

/// `type(value).__name__` for a JSON value, as Python decoded it.
pub(super) fn py_type_name(v: &Value) -> &'static str {
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

/// `repr(value)` for a JSON value, as Python prints the decoded object.
/// Without recursion: the details sheet reprs `extra` values on the
/// window's thread, and one can be as deep as a cache file holds.
pub(super) fn py_repr(v: &Value) -> String {
    crate::util::tree::repr_value(v, py_repr_str, py_str)
}

/// `repr(str)`: single quotes unless the text has one and no double quote;
/// control and non-space separator characters escaped.
pub(super) fn py_repr_str(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::from(q);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == q => {
                out.push('\\');
                out.push(c);
            }
            c if c != ' ' && (c.is_control() || c.is_whitespace()) => {
                let n = c as u32;
                if n < 0x100 {
                    out.push_str(&format!("\\x{n:02x}"));
                } else if n < 0x10000 {
                    out.push_str(&format!("\\u{n:04x}"));
                } else {
                    out.push_str(&format!("\\U{n:08x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push(q);
    out
}

/// `e.get(key, "")` for a string field.
pub(super) fn text_of(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

/// Python's `str.isspace()`: Unicode White_Space plus the four information
/// separators (U+001C-U+001F), which Python counts and Rust does not.
pub(super) fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `text.strip()`.
pub fn py_strip(text: &str) -> &str {
    text.trim_matches(py_space)
}

/// An integer as Python's `int()` holds it: unbounded, so kept as a sign
/// and its decimal digits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PyInt {
    pub(super) neg: bool,
    /// No leading zeros; "0" for zero (which is never negative).
    pub(super) mag: String,
}

impl PyInt {
    pub(super) fn negated(&self) -> PyInt {
        PyInt { neg: !self.neg && self.mag != "0", mag: self.mag.clone() }
    }
}

impl Ord for PyInt {
    fn cmp(&self, other: &Self) -> Ordering {
        let by_mag = (self.mag.len(), &self.mag).cmp(&(other.mag.len(), &other.mag));
        match (self.neg, other.neg) {
            (false, false) => by_mag,
            (true, true) => by_mag.reverse(),
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
        }
    }
}

impl PartialOrd for PyInt {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The value of a Unicode decimal digit (category Nd), as Python's int()
/// reads it. Every Nd run in Unicode is a whole 0-9 set starting at its
/// zero, so the value is the distance from the run's start, mod 10.
fn decimal_value(c: char) -> Option<u32> {
    use glib::Unichar;
    if c.is_ascii_digit() {
        return c.to_digit(10);
    }
    let is_nd = |c: char| c.unicode_type() == glib::UnicodeType::DecimalNumber;
    if !is_nd(c) {
        return None;
    }
    let mut back = 0u32;
    let mut n = c as u32;
    while let Some(prev) = n.checked_sub(1).and_then(char::from_u32) {
        if !is_nd(prev) {
            break;
        }
        back += 1;
        n -= 1;
    }
    Some(back % 10)
}

/// `int(text)`, or None where it raises ValueError: surrounding whitespace,
/// an optional sign, decimal digits from any script, and single underscores
/// between digits.
pub(super) fn py_int(text: &str) -> Option<PyInt> {
    let t = py_strip(text);
    let (neg, body) = match t.chars().next() {
        Some('-') => (true, &t[1..]),
        Some('+') => (false, &t[1..]),
        _ => (false, t),
    };
    let chars: Vec<char> = body.chars().collect();
    if chars.is_empty() {
        return None;
    }
    let mut digits = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' {
            let before = i > 0 && chars[i - 1] != '_';
            let after = chars.get(i + 1).is_some_and(|&n| n != '_' && decimal_value(n).is_some());
            if !before || !after {
                return None;
            }
            continue;
        }
        digits.push(char::from_digit(decimal_value(c)?, 10)?);
    }
    let mag = digits.trim_start_matches('0');
    let mag = if mag.is_empty() { "0".to_string() } else { mag.to_string() };
    let zero = mag == "0";
    Some(PyInt { neg: neg && !zero, mag })
}

/// `float(x)` where Python's datetime.fromtimestamp takes it: an int, a
/// bool or a float. Anything else is the TypeError it raises.
pub(super) fn timestamp_of(v: Option<&Value>) -> Result<f64, String> {
    match v {
        Some(Value::Number(n)) => n.as_f64().ok_or_else(|| "timestamp out of range".to_string()),
        Some(Value::Bool(b)) => Ok(f64::from(u8::from(*b))),
        Some(other) => Err(format!("TypeError: not a timestamp: {}", py_repr(other))),
        None => Err("KeyError: 'built_ts'".to_string()),
    }
}

/// Insert a comma every three digits, from the right.
fn group_digits(digits: &str) -> String {
    let mut out = String::new();
    let n = digits.chars().count();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (n - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `f"{n:,}"`: an integer with thousands separators.
pub fn thousands(n: i64) -> String {
    let out = group_digits(&n.unsigned_abs().to_string());
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

/// `f"{x:,}"` for a JSON value: ints and bools group their digits, a float
/// groups the integer part of its repr ('1,234.0'); a str, list or dict is
/// the ValueError/TypeError Python raises.
pub(super) fn py_format_thousands(v: &Value) -> Result<String, String> {
    match v {
        Value::Bool(b) => Ok(u8::from(*b).to_string()),
        Value::Number(n) if n.as_i64().is_some() || n.as_u64().is_some() => {
            let s = n.to_string();
            let (sign, digits) = s.strip_prefix('-').map_or(("", s.as_str()), |d| ("-", d));
            Ok(format!("{sign}{}", group_digits(digits)))
        }
        Value::Number(n) => {
            let r = crate::util::py_float_repr(n.as_f64().unwrap_or(0.0));
            if r.contains(['e', 'N', 'I']) {
                return Ok(r);
            }
            let (sign, body) = r.strip_prefix('-').map_or(("", r.as_str()), |d| ("-", d));
            let (int, frac) = body.split_once('.').unwrap_or((body, ""));
            Ok(format!("{sign}{}.{frac}", group_digits(int)))
        }
        Value::String(_) => Err("ValueError: Cannot specify ',' with 's'.".to_string()),
        other => Err(format!("TypeError: unsupported format string passed to {}", py_repr(other))),
    }
}

/// `datetime.fromtimestamp(ts).strftime(fmt)`, in the user's locale (the
/// month names follow LC_TIME, as Python's strftime does once i18n has
/// called setlocale). Err where fromtimestamp raises: NaN, infinity, or a
/// year outside 1-9999.
pub(super) fn local_time(ts: f64, fmt: &str) -> Result<String, String> {
    if !ts.is_finite() {
        return Err(format!("ValueError: Invalid value {} for timestamp", crate::util::py_float_repr(ts)));
    }
    // fromtimestamp rounds to the microsecond, half to even.
    let micros = (ts * 1e6).round_ties_even();
    let secs = (micros / 1e6).floor();
    if !(-62_135_596_800.0..=253_402_300_799.0).contains(&secs) {
        return Err("OverflowError: timestamp out of range for platform time_t".to_string());
    }
    glib::DateTime::from_unix_local(secs as i64)
        .and_then(|t| t.format(fmt))
        .map(|s| s.to_string())
        .map_err(|e| format!("ValueError: {e}"))
}

/// What `_detail_row` accepts: any value. `None` and `""` mean "no row";
/// everything else is shown as `str(value)`, so 0 and False still show.
pub trait DetailValue {
    /// None for `value in (None, "")`, otherwise `str(value)`.
    fn detail_text(&self) -> Option<String>;
}

impl DetailValue for str {
    fn detail_text(&self) -> Option<String> {
        (!self.is_empty()).then(|| self.to_string())
    }
}

impl DetailValue for String {
    fn detail_text(&self) -> Option<String> {
        self.as_str().detail_text()
    }
}

impl DetailValue for glib::GString {
    fn detail_text(&self) -> Option<String> {
        self.as_str().detail_text()
    }
}

impl DetailValue for Value {
    fn detail_text(&self) -> Option<String> {
        match self {
            Value::Null => None,
            Value::String(s) => s.detail_text(),
            other => Some(py_str(other)),
        }
    }
}

impl DetailValue for i64 {
    fn detail_text(&self) -> Option<String> {
        Some(self.to_string())
    }
}

impl DetailValue for bool {
    fn detail_text(&self) -> Option<String> {
        Some(if *self { "True" } else { "False" }.to_string())
    }
}

impl<T: DetailValue> DetailValue for Option<T> {
    fn detail_text(&self) -> Option<String> {
        self.as_ref().and_then(DetailValue::detail_text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::packages::sort_cmp;
    use crate::ui::shell::AppItem;

    #[test]
    fn values_print_as_python_str_and_format() {
        assert_eq!(py_str(&serde_json::json!(["GPL", "MIT"])), "['GPL', 'MIT']");
        assert_eq!(py_str(&serde_json::json!({"a": 1, "b": [true, null]})), "{'a': 1, 'b': [True, None]}");
        assert_eq!(py_str(&serde_json::json!(["it's"])), "[\"it's\"]");
        assert_eq!(py_str(&serde_json::json!(1.5)), "1.5");
        assert_eq!(py_format_thousands(&serde_json::json!(1234)).as_deref(), Ok("1,234"));
        assert_eq!(py_format_thousands(&serde_json::json!(-1234567)).as_deref(), Ok("-1,234,567"));
        assert_eq!(py_format_thousands(&serde_json::json!(1234.0)).as_deref(), Ok("1,234.0"));
        assert_eq!(py_format_thousands(&serde_json::json!(1e16)).as_deref(), Ok("1e+16"));
        assert_eq!(py_format_thousands(&serde_json::json!(true)).as_deref(), Ok("1"));
        assert!(py_format_thousands(&serde_json::json!("12")).is_err());
        assert!(timestamp_of(Some(&serde_json::json!("1700000000"))).is_err());
        assert_eq!(timestamp_of(Some(&serde_json::json!(5))), Ok(5.0));
        assert!(local_time(f64::NAN, "%Y").is_err());
        assert!(local_time(1e300, "%Y").is_err());
        assert_eq!(Value::Null.detail_text(), None);
        assert_eq!(serde_json::json!("").detail_text(), None);
        assert_eq!(serde_json::json!(0).detail_text().as_deref(), Some("0"));
        assert_eq!(serde_json::json!(false).detail_text().as_deref(), Some("False"));
        assert_eq!("".detail_text(), None);
    }

    #[test]
    fn int_and_strip_follow_python() {
        let int = |s: &str| py_int(s).map(|n| (n.neg, n.mag));
        assert_eq!(int("20240102"), Some((false, "20240102".into())));
        assert_eq!(int("2024_01_02"), Some((false, "20240102".into())));
        assert_eq!(int(" +007 "), Some((false, "7".into())));
        assert_eq!(int("-0"), Some((false, "0".into())));
        assert_eq!(int("\u{0662}\u{0660}"), Some((false, "20".into()))); // Arabic-Indic
        assert_eq!(int("1__2"), None);
        assert_eq!(int("_1"), None);
        assert_eq!(int("1_"), None);
        assert_eq!(int(""), None);
        assert_eq!(int("1.0"), None);
        let big = "9".repeat(60);
        assert!(py_int(&big) > py_int("1"));
        assert!(py_int(&big).map(|n| n.negated()) < py_int("-1"));
        assert_eq!(py_strip("\u{1c} ab \u{1f}\n"), "ab");
        let mut a = AppItem { added: "2024_01_02".into(), ..Default::default() };
        let b = AppItem { added: "2024-03-01".into(), ..Default::default() };
        assert_eq!(sort_cmp("trending", &a, &b), Ordering::Greater);
        a.added = "\u{0662}\u{0660}\u{0662}\u{0665}\u{0660}\u{0661}\u{0660}\u{0661}".into(); // 20250101
        assert_eq!(sort_cmp("trending", &a, &b), Ordering::Less);
    }
}
