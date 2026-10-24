//! Python's str and json as the files and outputs this module reads need
//! them: splitting, stripping, repr, and values serde_json cannot hold.

use super::*;

// ------------------------------------------------------- Python str helpers --

/// `str.isspace()` for one character: Unicode White_Space plus the four
/// ASCII separators (\x1c-\x1f) Python also counts.
pub(super) fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// `str.split()` with no separator.
pub(super) fn py_split(s: &str) -> Vec<&str> {
    s.split(py_space).filter(|p| !p.is_empty()).collect()
}

/// `str.strip()` with no argument.
pub(super) fn py_strip(s: &str) -> &str {
    s.trim_matches(py_space)
}

/// `str.splitlines()`: every line boundary Python recognises.
pub(super) fn py_splitlines(s: &str) -> Vec<&str> {
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
pub(super) fn py_prefix(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// `urllib.parse.quote(s)` (safe="/").
pub(super) fn py_quote(s: &str) -> String {
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

/// Python's type name for a JSON value, for error messages.
pub(super) fn py_type(v: &Value) -> &'static str {
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
pub(super) fn real_str(v: &Value) -> Option<String> {
    match exotic(v) {
        Some(Exotic::WStr(t)) => Some(wstr_lossy(t)),
        Some(_) => None,
        None => v.as_str().map(String::from),
    }
}

/// `for x in value`: a list's items, a dict's keys, a string's characters;
/// anything else is the TypeError Python raises.
pub(super) fn py_iter(v: &Value) -> Result<Vec<Value>, String> {
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
pub(super) fn py_float_str(f: f64) -> String {
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
pub(super) fn py_str(v: &Value) -> String {
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
pub(super) fn py_printable(c: u32) -> bool {
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
pub(super) fn py_repr_units(units: &[u32]) -> String {
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
pub(super) fn py_repr(v: &Value) -> String {
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

pub(super) const PYX: &str = "\u{10FFFF}\u{FDD0}py:";

pub(super) enum Exotic<'a> {
    /// NaN or an infinity.
    Float(f64),
    /// An int beyond 64 bits: its decimal digits.
    Int(&'a str),
    /// A str with a lone surrogate: its `json.dumps` text, quotes included.
    WStr(&'a str),
}

pub(super) fn exotic(v: &Value) -> Option<Exotic<'_>> {
    match v {
        Value::String(s) => exotic_str(s),
        _ => None,
    }
}

pub(super) fn exotic_str(s: &str) -> Option<Exotic<'_>> {
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
pub(super) fn float_json(f: f64) -> Value {
    serde_json::Number::from_f64(f)
        .map(Value::Number)
        .unwrap_or_else(|| Value::String(format!("{PYX}f:{}", py_float_str(f))))
}

/// A Python int (canonical decimal digits) as a Value: exact at any size.
pub(super) fn int_json(digits: &str) -> Value {
    if let Ok(i) = digits.parse::<i64>() {
        return json!(i);
    }
    if let Ok(u) = digits.parse::<u64>() {
        return json!(u);
    }
    Value::String(format!("{PYX}i:{digits}"))
}

/// `json.dumps` of a str's code points (ensure_ascii).
pub(super) fn units_dumps(units: &[u32]) -> String {
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
pub(super) fn units_json(units: &[u32]) -> Value {
    match units.iter().map(|&u| char::from_u32(u)).collect::<Option<String>>() {
        Some(s) => Value::String(s),
        None => Value::String(format!("{PYX}s:{}", units_dumps(units))),
    }
}

/// The code points of a `WStr`'s JSON text.
pub(super) fn wstr_units(text: &str) -> Vec<u32> {
    struct Units(Vec<u32>);
    impl Build for Units {
        type Key = ();
        fn null() -> Self {
            Units(Vec::new())
        }
        fn bool(_: bool) -> Self {
            Units::null()
        }
        fn num(_: Num<'_>) -> Self {
            Units::null()
        }
        fn str(s: String) -> Self {
            Units(s.chars().map(u32::from).collect())
        }
        fn wstr(units: Vec<u32>) -> Self {
            Units(units)
        }
        fn key(_: String) {}
        fn wkey(_: Vec<u32>) {}
        fn list(_: Vec<Self>) -> Self {
            Units::null()
        }
        fn dict(_: Vec<((), Self)>) -> Self {
            Units::null()
        }
    }
    pyjson::loads::<Units>(text, pyjson::Limit::values(1)).map(|u| u.0).unwrap_or_default()
}

/// A `WStr` as Rust text, each lone surrogate U+FFFD.
pub(super) fn wstr_lossy(text: &str) -> String {
    wstr_units(text).into_iter().map(|u| char::from_u32(u).unwrap_or('\u{fffd}')).collect()
}

/// `str(Path)` of a path: a name that is not UTF-8 keeps its bytes as
/// surrogates (surrogateescape), as Python's file-system decoding does.
pub(super) fn path_json(p: &Path) -> Value {
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
pub(super) fn py_loads(text: &str) -> Result<Value, String> {
    // Python gives up on deep nesting too (RecursionError).
    match pyjson::loads::<PyLoaded>(text, pyjson::Limit::values(901)) {
        Ok(v) => Ok(v.0),
        Err(pyjson::Error::Value(e)) => Err(e),
        Err(pyjson::Error::Recursion(_)) => Err("maximum recursion depth exceeded".into()),
    }
}

/// `json.loads(path.read_text())`, None where the Python's
/// `except (OSError, ValueError)` falls back.
pub(super) fn py_read_json(path: &Path) -> Option<Value> {
    py_loads(&std::fs::read_to_string(path).ok()?).ok()
}

/// What [`py_loads`] builds: a Value, with what it cannot hold marked.
pub(super) struct PyLoaded(Value);

impl Build for PyLoaded {
    type Key = String;
    fn null() -> Self {
        PyLoaded(Value::Null)
    }
    fn bool(b: bool) -> Self {
        PyLoaded(Value::Bool(b))
    }
    fn num(n: Num<'_>) -> Self {
        PyLoaded(match n {
            Num::Int(i) => json!(i),
            Num::UInt(u) => json!(u),
            Num::Big(digits) => int_json(digits),
            Num::Float(f) => float_json(f),
        })
    }
    fn str(s: String) -> Self {
        PyLoaded(Value::String(s))
    }
    fn wstr(units: Vec<u32>) -> Self {
        PyLoaded(units_json(&units))
    }
    fn key(s: String) -> String {
        s
    }
    fn wkey(units: Vec<u32>) -> String {
        match units_json(&units) {
            Value::String(k) => k,
            _ => String::new(),
        }
    }
    fn list(items: Vec<Self>) -> Self {
        PyLoaded(Value::Array(items.into_iter().map(|x| x.0).collect()))
    }
    fn dict(pairs: Vec<(String, Self)>) -> Self {
        // A repeated key keeps its first place and takes the last value, as
        // a dict does.
        let mut items = Map::with_capacity(pairs.len());
        for (k, v) in pairs {
            items.insert(k, v.0);
        }
        PyLoaded(Value::Object(items))
    }
}

/// `json.dumps(value, indent=indent)` with Python's default separators, the
/// values above written back as the tokens they were read from.
pub(super) fn py_dumps(value: &Value, indent: Option<usize>) -> String {
    let mut out = String::new();
    dump_into(&mut out, value, indent, 0);
    out
}

pub(super) fn dump_str(out: &mut String, s: &str) {
    match exotic_str(s) {
        Some(Exotic::Float(f)) => out.push_str(&util::py_float_repr(f)),
        Some(Exotic::Int(d)) => out.push_str(d),
        Some(Exotic::WStr(t)) => out.push_str(t),
        None => out.push_str(&util::py_json_string(s)),
    }
}

pub(super) fn dump_into(out: &mut String, v: &Value, indent: Option<usize>, level: usize) {
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


/// Every Unicode decimal digit as its ASCII digit; nothing else changes.
pub(super) fn ascii_digits(s: &str) -> String {
    s.chars()
        .map(|c| match crate::util::decimal_value(c) {
            Some(d) => char::from(b'0' + d as u8),
            None => c,
        })
        .collect()
}

/// `float(text)` for a string: Python accepts single underscores between
/// digits, which Rust's parser does not.
pub(super) fn py_float_text(s: &str) -> Option<f64> {
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
pub(super) fn py_int_text(s: &str) -> Option<String> {
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
pub(super) fn py_float_value(v: &Value) -> Option<f64> {
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
pub(super) fn float_or_zero(d: &Map<String, Value>, key: &str) -> Result<f64, String> {
    match d.get(key) {
        Some(v) if truthy(v) => py_float_value(v)
            .ok_or_else(|| format!("could not convert to float: {}", py_str(v))),
        _ => Ok(0.0),
    }
}

/// `d.get(key, default)` as text (a non-string value as Python's `str()`).
pub(super) fn get_str(d: &Map<String, Value>, key: &str, default: &str) -> String {
    match d.get(key) {
        None => default.to_string(),
        Some(v) => py_str(v),
    }
}

/// `(d.get(key) or "")` as text.
pub(super) fn get_str_or(d: &Map<String, Value>, key: &str) -> String {
    match d.get(key) {
        Some(v) if truthy(v) => py_str(v),
        _ => String::new(),
    }
}

/// `str(Path(s))`: pathlib collapses repeated separators, drops `.`
/// components, keeps exactly two leading slashes, and `Path("")` is `.`.
pub(super) fn py_path(s: &str) -> String {
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
pub(super) fn http_json(url: &str, timeout: u64) -> Result<Value, Box<dyn std::error::Error>> {
    use std::io::Read;
    let t = std::time::Duration::from_secs(timeout);
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(t))
        .timeout_send_request(Some(t))
        .timeout_recv_response(Some(t))
        .tls_config(ureq::tls::TlsConfig::builder().root_certs(crate::sources::system_roots()).build())
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
