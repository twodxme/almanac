//! Python string semantics this module leans on, a Python int in JSON, and
//! the codecs pyexpat's unknown-encoding handler can reach.

use super::*;

// ---- Python string semantics this module leans on ----

/// Python's `str.isspace()` per character: Unicode White_Space plus the
/// information separators \x1c-\x1f, which `str.split()` also splits on.
pub(super) fn py_ws(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// `str.split()` with no separator.
pub(super) fn py_split(s: &str) -> Vec<&str> {
    s.split(py_ws).filter(|p| !p.is_empty()).collect()
}

/// `str.strip()`.
pub(super) fn py_strip(s: &str) -> &str {
    s.trim_matches(py_ws)
}

/// `str.splitlines()`.
pub(super) fn py_splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        let brk = matches!(c, '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}');
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

/// `int(x)` for a token `str.split()` produced: an ASCII sign, then decimal
/// digits of any script with single underscores between them, at most 4300 of
/// them (`sys.get_int_max_str_digits()`), and no upper bound on the value.
pub(super) fn py_int(s: &str) -> Option<BigInt> {
    let (neg, digits) = match s.strip_prefix('-') {
        Some(d) => (true, d),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let mut ascii = String::new();
    let mut after_underscore = true;
    for c in digits.chars() {
        if c == '_' {
            if after_underscore {
                return None;
            }
            after_underscore = true;
            continue;
        }
        ascii.push(char::from_digit(crate::util::decimal_value(c)?, 10)?);
        after_underscore = false;
    }
    if ascii.is_empty() || after_underscore || ascii.len() > 4300 {
        return None;
    }
    let n = BigInt::parse_bytes(ascii.as_bytes(), 10)?;
    Some(if neg { -n } else { n })
}

/// `repr(str)`, as `str(TimeoutExpired)` shows each argv entry.
pub(super) fn py_str_repr(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::from(q);
    for c in s.chars() {
        let cp = c as u32;
        match c {
            '\\' => out.push_str("\\\\"),
            c if c == q => {
                out.push('\\');
                out.push(c);
            }
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ if cp < 0x20 || cp == 0x7f => out.push_str(&format!("\\x{cp:02x}")),
            c if cp < 0x7f || !py_nonprintable(cp) => out.push(c),
            _ if cp <= 0xff => out.push_str(&format!("\\x{cp:02x}")),
            _ if cp <= 0xffff => out.push_str(&format!("\\u{cp:04x}")),
            _ => out.push_str(&format!("\\U{cp:08x}")),
        }
    }
    out.push(q);
    out
}

/// `not str.isprintable()` for a code point from U+0080 up.
pub(super) fn py_nonprintable(cp: u32) -> bool {
    let i = NONPRINTABLE.partition_point(|&(_, hi)| hi < cp);
    NONPRINTABLE.get(i).is_some_and(|&(lo, _)| lo <= cp)
}

/// A Python int in JSON: a number, exact while it fits 64 bits.
pub(super) mod py_int_json {
    use num_bigint::BigInt;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn to_value(v: &BigInt) -> serde_json::Value {
        if let Ok(n) = i64::try_from(v) {
            serde_json::Value::from(n)
        } else if let Ok(n) = u64::try_from(v) {
            serde_json::Value::from(n)
        } else {
            // serde_json cannot hold a wider integer; the nearest float.
            serde_json::Value::from(v.to_string().parse::<f64>().unwrap_or(f64::INFINITY))
        }
    }

    pub fn serialize<S: Serializer>(v: &BigInt, s: S) -> Result<S::Ok, S::Error> {
        to_value(v).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<BigInt, D::Error> {
        let n = serde_json::Number::deserialize(d)?;
        Ok(if let Some(i) = n.as_i64() {
            BigInt::from(i)
        } else if let Some(u) = n.as_u64() {
            BigInt::from(u)
        } else {
            BigInt::parse_bytes(format!("{:.0}", n.as_f64().unwrap_or(0.0)).as_bytes(), 10).unwrap_or_default()
        })
    }
}

/// What pyexpat's unknown-encoding handler makes of an encoding name: a byte
/// map expat accepts, one it refuses (ParseError), or a Python exception.
#[derive(Debug, Clone, Copy)]
pub(super) enum Codec {
    Map(usize),
    Unusable,
    Raise(&'static str),
}

/// `codecs.lookup(name)` as `encodings.search_function` resolves it, reduced
/// to what `bytes(range(256)).decode(name, "replace")` gives pyexpat.
pub(super) fn py_codec(name: &str) -> Codec {
    // _Py_normalize_encoding: lower-case, every other run of punctuation but
    // '.' becomes one '_', none at the ends.
    let mut norm = String::new();
    let mut punct = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '.' {
            if punct && !norm.is_empty() {
                norm.push('_');
            }
            norm.push(c.to_ascii_lowercase());
            punct = false;
        } else {
            punct = true;
        }
    }
    let table = |t: &'static [(&'static str, &'static str)], k: &str| -> Option<&'static str> {
        t.binary_search_by(|(a, _)| (*a).cmp(k)).ok().map(|i| t[i].1)
    };
    let alias = table(CODEC_ALIASES, &norm).or_else(|| table(CODEC_ALIASES, &norm.replace('.', "_")));
    for module in alias.into_iter().chain(std::iter::once(norm.as_str())) {
        if module.is_empty() || module.contains('.') {
            continue;
        }
        if let Ok(i) = CODEC_MODULES.binary_search_by(|(m, _)| (*m).cmp(module)) {
            return CODEC_MODULES[i].1;
        }
    }
    Codec::Raise("LookupError")
}

/// The 256 characters of a codec map; U+FFFD is a byte expat calls malformed.
pub(super) fn codec_map(i: usize) -> Option<Vec<char>> {
    let mut map: Vec<char> = (0u8..0x80).map(char::from).collect();
    for &(m, b) in CODEC_LOW_UNDEFINED {
        if m == i {
            map[usize::from(b)] = '\u{fffd}';
        }
    }
    map.extend(CODEC_MAPS.get(i)?.chars());
    (map.len() == 256).then_some(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_string_helpers() {
        assert_eq!(py_splitlines("a\r\nb\rc\n"), vec!["a", "b", "c"]);
        assert_eq!(py_split(" a\x1c b\u{85}c\u{a0}d"), vec!["a", "b", "c", "d"]);
        assert_eq!(py_int("+3"), Some(BigInt::from(3)));
        assert_eq!(py_int("1_0"), Some(BigInt::from(10)));
        assert_eq!(py_int("٣"), Some(BigInt::from(3)));
        assert_eq!(py_int("x"), None);
        assert_eq!(py_str_repr("it's"), "\"it's\"");
        assert_eq!(py_str_repr("a\u{a0}\u{2028}\u{85}\u{200b}é"), "'a\\xa0\\u2028\\x85\\u200bé'");
        assert_eq!(py_path(OsStr::new("/tmp//a/./b/")), OsString::from("/tmp/a/b"));
        assert_eq!(py_path(OsStr::new("//x")), OsString::from("//x"));
    }
}
