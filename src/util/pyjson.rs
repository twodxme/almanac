//! `json.loads`, once for every module that needs Python's reading of a
//! JSON text rather than serde_json's.
//!
//! Python takes more than serde_json does: the constants `NaN`, `Infinity`
//! and `-Infinity`, a float that overflows to inf (`1e400`), ints of any
//! size (`-0` is the int 0), a `\uXXXX` escape that leaves a lone surrogate
//! in a str, and nesting far deeper than serde's 128 levels. Real files hold
//! none of that, so every document is first read by serde_json, straight
//! into the caller's value type ([`Build`], or a [`Sink`] that keeps state,
//! such as an arena); only a document serde refuses, or holds a number serde
//! would read differently, is read again by the scanner below, which follows
//! CPython's C scanner. One parse for the common case, and Python's answer
//! for the rest.
//!
//! The scanner is iterative, so a document nested tens of thousands deep
//! (Python's own limit is its C stack) needs no big stack; the serde pass
//! recurses at most 128 levels before it gives up.

use serde::de::{DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use std::marker::PhantomData;

/// A JSON number as `json.loads` reads it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Num<'a> {
    /// An int that fits in 64 bits signed (`-0` is 0).
    Int(i64),
    /// An int between `i64::MAX` and `u64::MAX`.
    UInt(u64),
    /// A longer int: its decimal digits as written, sign included.
    Big(&'a str),
    /// A float, NaN and the infinities included.
    Float(f64),
}

/// A caller's value type, built from the parse. Containers come with their
/// children already built; a dict's pairs come in file order, a repeated key
/// included -- deduplicating (Python's dict keeps a key's first place and its
/// last value) is the builder's to do ([`dict_pairs`]).
pub trait Build: Sized {
    type Key;
    /// Ints as Python reads them: `-0` is the int 0, and an int past 64
    /// bits stays exact ([`Num::Big`]). Off, numbers are what serde_json
    /// makes of them, whichever pass reads the document: `-0` is the float
    /// -0.0 and a longer int the nearest float.
    const EXACT_INTS: bool = true;
    fn null() -> Self;
    fn bool(b: bool) -> Self;
    fn num(n: Num<'_>) -> Self;
    fn str(s: String) -> Self;
    /// A str holding a lone surrogate, as code points.
    fn wstr(units: Vec<u32>) -> Self;
    fn key(s: String) -> Self::Key;
    /// A key holding a lone surrogate, as code points.
    fn wkey(units: Vec<u32>) -> Self::Key;
    fn list(items: Vec<Self>) -> Self;
    fn dict(pairs: Vec<(Self::Key, Self)>) -> Self;
}

/// [`Build`] with state: what [`loads_into`] fills, e.g. an arena whose
/// values are indices. Children come before their container, as there.
pub trait Sink {
    type Value;
    type Key;
    /// As [`Build::EXACT_INTS`].
    const EXACT_INTS: bool = true;
    /// Forget what a failed serde pass built, before the scanner starts.
    fn reset(&mut self) {}
    fn null(&mut self) -> Self::Value;
    fn bool(&mut self, b: bool) -> Self::Value;
    fn num(&mut self, n: Num<'_>) -> Self::Value;
    fn str(&mut self, s: String) -> Self::Value;
    fn wstr(&mut self, units: Vec<u32>) -> Self::Value;
    fn key(&mut self, s: String) -> Self::Key;
    fn wkey(&mut self, units: Vec<u32>) -> Self::Key;
    fn list(&mut self, items: Vec<Self::Value>) -> Self::Value;
    fn dict(&mut self, pairs: Vec<(Self::Key, Self::Value)>) -> Self::Value;
}

/// A [`Build`] type as a [`Sink`].
struct Tree<T>(PhantomData<T>);

impl<T: Build> Sink for Tree<T> {
    type Value = T;
    type Key = T::Key;
    const EXACT_INTS: bool = T::EXACT_INTS;
    fn null(&mut self) -> T {
        T::null()
    }
    fn bool(&mut self, b: bool) -> T {
        T::bool(b)
    }
    fn num(&mut self, n: Num<'_>) -> T {
        T::num(n)
    }
    fn str(&mut self, s: String) -> T {
        T::str(s)
    }
    fn wstr(&mut self, units: Vec<u32>) -> T {
        T::wstr(units)
    }
    fn key(&mut self, s: String) -> T::Key {
        T::key(s)
    }
    fn wkey(&mut self, units: Vec<u32>) -> T::Key {
        T::wkey(units)
    }
    fn list(&mut self, items: Vec<T>) -> T {
        T::list(items)
    }
    fn dict(&mut self, pairs: Vec<(T::Key, T)>) -> T {
        T::dict(pairs)
    }
}

/// How deep a document may nest. A value at level `n` (the top-level value
/// is level 1, an item of it level 2, ...) deeper than `depth` is a
/// RecursionError: a container always, a scalar too when `scalars`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limit {
    pub depth: usize,
    pub scalars: bool,
}

impl Limit {
    /// Containers nest at most `depth` deep (CPython's C scanner checks
    /// on entering each array or object).
    pub const fn containers(depth: usize) -> Limit {
        Limit {
            depth,
            scalars: false,
        }
    }

    /// No value, scalar or container, sits deeper than level `depth`.
    pub const fn values(depth: usize) -> Limit {
        Limit {
            depth,
            scalars: true,
        }
    }
}

/// Why `json.loads` refused a document.
#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// ValueError: JSONDecodeError's text (`Expecting value: line 1 column
    /// 1 (char 0)`), or the int digit limit's.
    Value(String),
    /// RecursionError past the [`Limit`]: true when entering an object,
    /// false for an array (or a scalar).
    Recursion(bool),
}

/// `json.loads(text)` into `T`, nesting no deeper than `limit`.
pub fn loads<T: Build>(text: &str, limit: Limit) -> Result<T, Error> {
    loads_into(text, limit, &mut Tree(PhantomData))
}

/// `json.loads(text)` into `sink`, nesting no deeper than `limit`.
pub fn loads_into<S: Sink>(text: &str, limit: Limit, sink: &mut S) -> Result<S::Value, Error> {
    // serde nests 127 containers at most, so it takes nothing a limit at
    // least that deep refuses.
    if limit.depth >= SERDE_DEPTH + usize::from(limit.scalars) {
        if let Some(v) = fast(text, sink) {
            return Ok(v);
        }
        sink.reset();
    }
    scan(text, limit, sink)
}

/// `dict(pairs)`, for a [`Build::dict`] that keeps pairs: a repeated key
/// keeps its first place and takes the last value.
pub fn dict_pairs<K: Eq + std::hash::Hash + Clone, V>(pairs: Vec<(K, V)>) -> Vec<(K, V)> {
    // Most dicts repeat no key: that is checked without copying one.
    let unique = {
        let mut seen = std::collections::HashSet::with_capacity(pairs.len());
        pairs.iter().all(|(k, _)| seen.insert(k))
    };
    if unique {
        return pairs;
    }
    let mut out: Vec<(K, V)> = Vec::with_capacity(pairs.len());
    let mut index: std::collections::HashMap<K, usize> = std::collections::HashMap::new();
    for (k, v) in pairs {
        match index.get(&k) {
            Some(&at) => out[at].1 = v,
            None => {
                index.insert(k.clone(), out.len());
                out.push((k, v));
            }
        }
    }
    out
}

// ---- the serde pass --------------------------------------------------------

/// How many containers deep serde_json reads before it gives up.
pub const SERDE_DEPTH: usize = 127;

/// serde_json's parse, or None where it fails or would differ from Python's.
fn fast<S: Sink>(text: &str, sink: &mut S) -> Option<S::Value> {
    let mut de = serde_json::Deserializer::from_str(text);
    let v = Fast(sink).deserialize(&mut de).ok()?;
    de.end().ok()?;
    Some(v)
}

/// One value of the serde pass, built into the sink.
struct Fast<'s, S>(&'s mut S);

impl<'de, S: Sink> DeserializeSeed<'de> for Fast<'_, S> {
    type Value = S::Value;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<S::Value, D::Error> {
        d.deserialize_any(self)
    }
}

/// 2**63: serde_json reads an int at least this far from zero that does not
/// fit its integer types as a float.
const TWO_63: f64 = 9_223_372_036_854_775_808.0;

impl<'de, S: Sink> Visitor<'de> for Fast<'_, S> {
    type Value = S::Value;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_unit<E>(self) -> Result<S::Value, E> {
        Ok(self.0.null())
    }

    fn visit_bool<E>(self, b: bool) -> Result<S::Value, E> {
        Ok(self.0.bool(b))
    }

    fn visit_i64<E>(self, n: i64) -> Result<S::Value, E> {
        Ok(self.0.num(Num::Int(n)))
    }

    fn visit_u64<E>(self, n: u64) -> Result<S::Value, E> {
        Ok(self.0.num(match i64::try_from(n) {
            Ok(i) => Num::Int(i),
            Err(_) => Num::UInt(n),
        }))
    }

    fn visit_f64<E: serde::de::Error>(self, f: f64) -> Result<S::Value, E> {
        // serde_json reads `-0`, and an int too long for 64 bits, as a
        // float; so is `-0.0` or `1e19`, and only the text tells them
        // apart. Python's int is no float: the scanner decides.
        if S::EXACT_INTS
            && f.fract() == 0.0
            && (f.abs() >= TWO_63 || (f == 0.0 && f.is_sign_negative()))
        {
            return Err(E::custom("an integral float: maybe an int"));
        }
        Ok(self.0.num(Num::Float(f)))
    }

    fn visit_str<E>(self, s: &str) -> Result<S::Value, E> {
        Ok(self.0.str(s.to_owned()))
    }

    fn visit_string<E>(self, s: String) -> Result<S::Value, E> {
        Ok(self.0.str(s))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<S::Value, A::Error> {
        let mut items = Vec::with_capacity(seq.size_hint().unwrap_or(0));
        while let Some(x) = seq.next_element_seed(Fast(&mut *self.0))? {
            items.push(x);
        }
        Ok(self.0.list(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<S::Value, A::Error> {
        let mut pairs = Vec::with_capacity(map.size_hint().unwrap_or(0));
        while let Some(k) = map.next_key::<String>()? {
            let k = self.0.key(k);
            let v = map.next_value_seed(Fast(&mut *self.0))?;
            pairs.push((k, v));
        }
        Ok(self.0.dict(pairs))
    }
}

// ---- the scanner -------------------------------------------------------------

/// `sys.get_int_max_str_digits()`: a longer int literal is a ValueError.
const INT_MAX_STR_DIGITS: usize = 4300;

/// A str being read: plain text until a lone surrogate turns up.
enum Text {
    Plain(String),
    Wide(Vec<u32>),
}

impl Text {
    fn push(&mut self, cp: u32) {
        match (self, char::from_u32(cp)) {
            (Text::Plain(s), Some(c)) => s.push(c),
            (Text::Wide(w), _) => w.push(cp),
            (t @ Text::Plain(_), None) => {
                let Text::Plain(s) = std::mem::replace(t, Text::Wide(Vec::new())) else {
                    return;
                };
                let mut w: Vec<u32> = s.chars().map(u32::from).collect();
                w.push(cp);
                *t = Text::Wide(w);
            }
        }
    }

    fn push_str(&mut self, s: &str) {
        match self {
            Text::Plain(p) => p.push_str(s),
            Text::Wide(w) => w.extend(s.chars().map(u32::from)),
        }
    }
}

struct Scan<'a> {
    text: &'a str,
    b: &'a [u8],
}

impl Scan<'_> {
    /// JSONDecodeError at byte `at`: its message counts lines, columns and
    /// characters, not bytes.
    fn err<T>(&self, msg: &str, at: usize) -> Result<T, Error> {
        let before = self.text.get(..at).unwrap_or(self.text);
        let pos = before.chars().count();
        let line = before.matches('\n').count() + 1;
        let col = match before.rfind('\n') {
            Some(nl) => before[nl..].chars().count() - 1,
            None => pos,
        } + 1;
        Err(Error::Value(format!(
            "{msg}: line {line} column {col} (char {pos})"
        )))
    }

    fn ws(&self, mut i: usize) -> usize {
        while matches!(self.b.get(i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            i += 1;
        }
        i
    }

    fn at(&self, i: usize, word: &str) -> bool {
        self.b.get(i..i + word.len()) == Some(word.as_bytes())
    }

    fn hex4(&self, at: usize) -> Option<u32> {
        let h = self.b.get(at..at + 4)?;
        h.iter()
            .try_fold(0, |v, &c| Some(v << 4 | char::from(c).to_digit(16)?))
    }

    /// The str whose opening quote is at `start`, and the index after its
    /// closing quote. Control characters are errors (`strict=True`).
    fn string(&self, start: usize) -> Result<(Text, usize), Error> {
        let b = self.b;
        let mut out = Text::Plain(String::new());
        let mut i = start + 1;
        loop {
            let run = i;
            while matches!(b.get(i), Some(&c) if c != b'"' && c != b'\\' && c >= 0x20) {
                i += 1;
            }
            out.push_str(&self.text[run..i]);
            match b.get(i) {
                None => return self.err("Unterminated string starting at", start),
                Some(b'"') => return Ok((out, i + 1)),
                Some(b'\\') => {
                    let Some(&e) = b.get(i + 1) else {
                        return self.err("Unterminated string starting at", start);
                    };
                    let simple = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => '\0',
                        _ => return self.err("Invalid \\escape", i),
                    };
                    if e != b'u' {
                        out.push(u32::from(simple));
                        i += 2;
                        continue;
                    }
                    // `\uXXXX`, with a character after it (the scanner asks
                    // for one before it reads the digits).
                    let end = i + 6;
                    if end >= b.len() {
                        return self.err("Invalid \\uXXXX escape", i + 1);
                    }
                    let Some(mut cp) = self.hex4(i + 2) else {
                        return self.err("Invalid \\uXXXX escape", i + 1);
                    };
                    let mut next = end;
                    // A high surrogate joins a following `\uXXXX` low one;
                    // otherwise both stay lone, as Python keeps them.
                    if (0xd800..=0xdbff).contains(&cp)
                        && end + 6 < b.len()
                        && b[end] == b'\\'
                        && b[end + 1] == b'u'
                    {
                        let Some(lo) = self.hex4(end + 2) else {
                            return self.err("Invalid \\uXXXX escape", end + 1);
                        };
                        if (0xdc00..=0xdfff).contains(&lo) {
                            cp = 0x10000 + ((cp - 0xd800) << 10) + (lo - 0xdc00);
                            next = end + 6;
                        }
                    }
                    out.push(cp);
                    i = next;
                }
                Some(_) => return self.err("Invalid control character at", i),
            }
        }
    }

    /// A number or a constant at `*i`.
    fn scalar<S: Sink>(&self, i: &mut usize, sink: &mut S) -> Result<S::Value, Error> {
        let at = *i;
        for (word, v) in [
            ("null", 0),
            ("true", 1),
            ("false", 2),
            ("NaN", 3),
            ("Infinity", 4),
            ("-Infinity", 5),
        ] {
            if self.at(at, word) {
                *i = at + word.len();
                return Ok(match v {
                    0 => sink.null(),
                    1 => sink.bool(true),
                    2 => sink.bool(false),
                    3 => sink.num(Num::Float(f64::NAN)),
                    4 => sink.num(Num::Float(f64::INFINITY)),
                    _ => sink.num(Num::Float(f64::NEG_INFINITY)),
                });
            }
        }
        // -?(0|[1-9]\d*)(\.\d+)?([eE][-+]?\d+)?
        let b = self.b;
        let digit = |k: usize| b.get(k).is_some_and(u8::is_ascii_digit);
        let mut k = at;
        if b.get(k) == Some(&b'-') {
            k += 1;
        }
        match b.get(k) {
            Some(b'0') => k += 1,
            Some(b'1'..=b'9') => {
                while digit(k) {
                    k += 1;
                }
            }
            _ => return self.err("Expecting value", at),
        }
        let mut is_float = false;
        if b.get(k) == Some(&b'.') && digit(k + 1) {
            is_float = true;
            k += 1;
            while digit(k) {
                k += 1;
            }
        }
        if matches!(b.get(k), Some(b'e' | b'E')) {
            let mut e = k + 1;
            if matches!(b.get(e), Some(b'-' | b'+')) {
                e += 1;
            }
            if digit(e) {
                while digit(e) {
                    e += 1;
                }
                is_float = true;
                k = e;
            }
        }
        *i = k;
        let tok = &self.text[at..k];
        if is_float {
            // float(): correctly rounded, overflowing to inf.
            return Ok(sink.num(Num::Float(tok.parse().unwrap_or(f64::NAN))));
        }
        let digits = tok.trim_start_matches('-').len();
        if digits > INT_MAX_STR_DIGITS {
            return Err(Error::Value(format!(
                "Exceeds the limit ({INT_MAX_STR_DIGITS} digits) for integer string conversion: value has {digits} digits; use sys.set_int_max_str_digits() to increase the limit"
            )));
        }
        let n = if let Ok(n) = tok.parse::<i64>() {
            Num::Int(n)
        } else if let Ok(n) = tok.parse::<u64>() {
            Num::UInt(n)
        } else if S::EXACT_INTS {
            Num::Big(tok)
        } else {
            // As serde_json reads it.
            Num::Float(tok.parse().unwrap_or(f64::NAN))
        };
        if !S::EXACT_INTS && tok == "-0" {
            return Ok(sink.num(Num::Float(-0.0)));
        }
        Ok(sink.num(n))
    }

    /// `"key"` and its `:` from `*i`; leaves `*i` at the value.
    fn key<S: Sink>(&self, i: &mut usize, sink: &mut S) -> Result<S::Key, Error> {
        if self.b.get(*i) != Some(&b'"') {
            return self.err("Expecting property name enclosed in double quotes", *i);
        }
        let (k, end) = self.string(*i)?;
        let at = self.ws(end);
        if self.b.get(at) != Some(&b':') {
            return self.err("Expecting ':' delimiter", at);
        }
        *i = self.ws(at + 1);
        Ok(match k {
            Text::Plain(s) => sink.key(s),
            Text::Wide(w) => sink.wkey(w),
        })
    }
}

/// A container being filled.
enum Frame<S: Sink> {
    List(Vec<S::Value>),
    /// The pairs so far, and the key whose value is being read.
    Dict(Vec<(S::Key, S::Value)>, Option<S::Key>),
}

/// CPython's C scanner, without recursion.
fn scan<S: Sink>(text: &str, limit: Limit, sink: &mut S) -> Result<S::Value, Error> {
    let p = Scan {
        text,
        b: text.as_bytes(),
    };
    if text.starts_with('\u{feff}') {
        return p.err("Unexpected UTF-8 BOM (decode using utf-8-sig)", 0);
    }
    let n = p.b.len();
    let mut stack: Vec<Frame<S>> = Vec::new();
    let mut i = p.ws(0);
    loop {
        // One value at i, or a container opened and its first item next.
        let c = p.b.get(i).copied();
        let is_open = matches!(c, Some(b'{' | b'['));
        if stack.len() + 1 > limit.depth && (is_open || limit.scalars) {
            return Err(Error::Recursion(c == Some(b'{')));
        }
        let mut v = match c {
            None => return p.err("Expecting value", i),
            Some(b'"') => {
                let (s, end) = p.string(i)?;
                i = end;
                match s {
                    Text::Plain(s) => sink.str(s),
                    Text::Wide(w) => sink.wstr(w),
                }
            }
            Some(b'[') => {
                i = p.ws(i + 1);
                if p.b.get(i) == Some(&b']') {
                    i += 1;
                    sink.list(Vec::new())
                } else {
                    stack.push(Frame::List(Vec::new()));
                    continue;
                }
            }
            Some(b'{') => {
                i = p.ws(i + 1);
                if p.b.get(i) == Some(&b'}') {
                    i += 1;
                    sink.dict(Vec::new())
                } else {
                    let k = p.key(&mut i, sink)?;
                    stack.push(Frame::Dict(Vec::new(), Some(k)));
                    continue;
                }
            }
            Some(_) => p.scalar(&mut i, sink)?,
        };
        // Hand the value to its container, closing every container that
        // ends here.
        loop {
            match stack.last_mut() {
                None => {
                    let end = p.ws(i);
                    if end != n {
                        return p.err("Extra data", end);
                    }
                    return Ok(v);
                }
                Some(Frame::List(items)) => {
                    items.push(v);
                    i = p.ws(i);
                    match p.b.get(i) {
                        Some(b']') => {
                            i += 1;
                            let Some(Frame::List(items)) = stack.pop() else {
                                unreachable!("the top frame is a list")
                            };
                            v = sink.list(items);
                        }
                        Some(b',') => {
                            let comma = i;
                            i = p.ws(i + 1);
                            if p.b.get(i) == Some(&b']') {
                                return p.err("Illegal trailing comma before end of array", comma);
                            }
                            break;
                        }
                        _ => return p.err("Expecting ',' delimiter", i),
                    }
                }
                Some(Frame::Dict(pairs, key)) => {
                    if let Some(k) = key.take() {
                        pairs.push((k, v));
                    }
                    i = p.ws(i);
                    match p.b.get(i) {
                        Some(b'}') => {
                            i += 1;
                            let Some(Frame::Dict(pairs, _)) = stack.pop() else {
                                unreachable!("the top frame is a dict")
                            };
                            v = sink.dict(pairs);
                        }
                        Some(b',') => {
                            let comma = i;
                            i = p.ws(i + 1);
                            if p.b.get(i) == Some(&b'}') {
                                return p.err("Illegal trailing comma before end of object", comma);
                            }
                            *key = Some(p.key(&mut i, sink)?);
                            break;
                        }
                        _ => return p.err("Expecting ',' delimiter", i),
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every distinction the builders can see, for comparing the passes.
    #[derive(Debug, Clone, PartialEq)]
    enum V {
        Null,
        Bool(bool),
        Int(i64),
        UInt(u64),
        Big(String),
        /// The float's bits, so NaN equals NaN and -0.0 is not 0.0.
        Float(u64),
        Str(String),
        WStr(Vec<u32>),
        List(Vec<V>),
        Dict(Vec<(Result<String, Vec<u32>>, V)>),
    }

    impl Build for V {
        type Key = Result<String, Vec<u32>>;
        fn null() -> Self {
            V::Null
        }
        fn bool(b: bool) -> Self {
            V::Bool(b)
        }
        fn num(n: Num<'_>) -> Self {
            match n {
                Num::Int(i) => V::Int(i),
                Num::UInt(u) => V::UInt(u),
                Num::Big(d) => V::Big(d.into()),
                Num::Float(f) => V::Float(f.to_bits()),
            }
        }
        fn str(s: String) -> Self {
            V::Str(s)
        }
        fn wstr(units: Vec<u32>) -> Self {
            V::WStr(units)
        }
        fn key(s: String) -> Self::Key {
            Ok(s)
        }
        fn wkey(units: Vec<u32>) -> Self::Key {
            Err(units)
        }
        fn list(items: Vec<Self>) -> Self {
            V::List(items)
        }
        fn dict(pairs: Vec<(Self::Key, Self)>) -> Self {
            V::Dict(pairs)
        }
    }

    /// The same builder, reading numbers as serde_json does.
    #[derive(Debug, PartialEq)]
    struct Serde(V);

    impl Build for Serde {
        type Key = Result<String, Vec<u32>>;
        const EXACT_INTS: bool = false;
        fn null() -> Self {
            Serde(V::Null)
        }
        fn bool(b: bool) -> Self {
            Serde(V::bool(b))
        }
        fn num(n: Num<'_>) -> Self {
            Serde(V::num(n))
        }
        fn str(s: String) -> Self {
            Serde(V::Str(s))
        }
        fn wstr(units: Vec<u32>) -> Self {
            Serde(V::WStr(units))
        }
        fn key(s: String) -> Self::Key {
            Ok(s)
        }
        fn wkey(units: Vec<u32>) -> Self::Key {
            Err(units)
        }
        fn list(items: Vec<Self>) -> Self {
            Serde(V::List(items.into_iter().map(|x| x.0).collect()))
        }
        fn dict(pairs: Vec<(Self::Key, Self)>) -> Self {
            Serde(V::Dict(pairs.into_iter().map(|(k, x)| (k, x.0)).collect()))
        }
    }

    const BIG: Limit = Limit::containers(100_000);

    fn fast_v(text: &str) -> Option<V> {
        fast(text, &mut Tree(PhantomData))
    }

    fn scan_v(text: &str, limit: Limit) -> Result<V, Error> {
        scan(text, limit, &mut Tree(PhantomData))
    }

    fn both(text: &str) -> V {
        let slow = scan_v(text, BIG).unwrap();
        if let Some(f) = fast_v(text) {
            assert_eq!(f, slow, "{text}");
        }
        slow
    }

    #[test]
    fn the_passes_agree_on_ordinary_json() {
        for t in [
            r#"{"a": [1, -2, 2.5, 1e3, true, false, null, "x\n\u00e9\ud83d\ude00", {}], "a": [], "b": {"c": -0.5}}"#,
            "[18446744073709551615, -9223372036854775808, 9223372036854775807, 0.1, 1.5e-400]",
            " \t\n[\"\\/\\\\\\\"\"] \r\n",
            "\"top\"",
            "123",
        ] {
            assert!(fast_v(t).is_some(), "{t}");
            both(t);
        }
    }

    #[test]
    fn reads_what_python_reads_and_serde_does_not() {
        let f = |x: f64| V::Float(x.to_bits());
        assert_eq!(
            both("[NaN, Infinity, -Infinity, 1e400, -1e400]"),
            V::List(vec![
                f(f64::NAN),
                f(f64::INFINITY),
                f(f64::NEG_INFINITY),
                f(f64::INFINITY),
                f(f64::NEG_INFINITY)
            ])
        );
        assert_eq!(both("-0"), V::Int(0));
        assert_eq!(
            both("[-0.0, 1e19, 18446744073709551616]"),
            V::List(vec![
                f(-0.0),
                f(1e19),
                V::Big("18446744073709551616".into())
            ])
        );
        assert_eq!(
            both("-9223372036854775809"),
            V::Big("-9223372036854775809".into())
        );
        assert_eq!(both(r#""a\ud800b""#), V::WStr(vec![0x61, 0xd800, 0x62]));
        assert_eq!(both(r#""\udc00\ud800""#), V::WStr(vec![0xdc00, 0xd800]));
        assert_eq!(
            both(r#""\ud83d\ude00\ud83d""#),
            V::WStr(vec![0x1f600, 0xd83d])
        );
        assert_eq!(
            both(r#"{"\ud800": 1, "k": 2, "k": 3}"#),
            V::Dict(vec![
                (Err(vec![0xd800]), V::Int(1)),
                (Ok("k".into()), V::Int(2)),
                (Ok("k".into()), V::Int(3))
            ])
        );
        assert_eq!(
            both(&format!("-{}", "1".repeat(4300))),
            V::Big(format!("-{}", "1".repeat(4300)))
        );
        assert!(
            matches!(scan_v(&"1".repeat(4301), BIG), Err(Error::Value(m)) if m.contains("4301 digits"))
        );
    }

    #[test]
    fn numbers_as_serde_reads_them_when_asked() {
        let f = |x: f64| V::Float(x.to_bits());
        let s = |t: &str| loads::<Serde>(t, BIG).unwrap().0;
        assert_eq!(s("-0"), f(-0.0));
        assert_eq!(s("[-0, NaN]"), V::List(vec![f(-0.0), f(f64::NAN)]));
        assert_eq!(s("100000000000000000000"), f(1e20));
        assert_eq!(
            s("[100000000000000000000, NaN]"),
            V::List(vec![f(1e20), f(f64::NAN)])
        );
        assert_eq!(s("18446744073709551615"), V::UInt(u64::MAX));
    }

    #[test]
    fn refuses_what_python_refuses() {
        let err = |t: &str| match scan_v(t, BIG) {
            Err(Error::Value(m)) => m,
            other => panic!("{t}: {other:?}"),
        };
        assert_eq!(err(""), "Expecting value: line 1 column 1 (char 0)");
        assert_eq!(
            err("[1,]"),
            "Illegal trailing comma before end of array: line 1 column 3 (char 2)"
        );
        assert_eq!(
            err("{\"a\": 1,}"),
            "Illegal trailing comma before end of object: line 1 column 8 (char 7)"
        );
        assert_eq!(
            err("{\"a\" 1}"),
            "Expecting ':' delimiter: line 1 column 6 (char 5)"
        );
        assert_eq!(
            err("{1: 1}"),
            "Expecting property name enclosed in double quotes: line 1 column 2 (char 1)"
        );
        assert_eq!(
            err("[1 2]"),
            "Expecting ',' delimiter: line 1 column 4 (char 3)"
        );
        assert_eq!(
            err("\"é\n\"x"),
            "Invalid control character at: line 1 column 3 (char 2)"
        );
        assert_eq!(
            err("[\"é\",\n 1] x"),
            "Extra data: line 2 column 5 (char 10)"
        );
        assert_eq!(
            err("\"ab"),
            "Unterminated string starting at: line 1 column 1 (char 0)"
        );
        assert_eq!(err(r#""\x""#), "Invalid \\escape: line 1 column 2 (char 1)");
        assert_eq!(
            err(r#""\u12""#),
            "Invalid \\uXXXX escape: line 1 column 3 (char 2)"
        );
        assert_eq!(
            err(r#""\ud800\uzzzz""#),
            "Invalid \\uXXXX escape: line 1 column 9 (char 8)"
        );
        assert_eq!(
            err("\u{feff}1"),
            "Unexpected UTF-8 BOM (decode using utf-8-sig): line 1 column 1 (char 0)"
        );
        for t in [
            "01", "-", "1.", ".5", "+1", "-NaN", "nul", "[", "{", "{\"a\"", "NaNx", "1e", "tru",
        ] {
            err(t);
            assert!(loads::<V>(t, BIG).is_err(), "{t}");
        }
    }

    #[test]
    fn nests_to_the_limit_without_recursion() {
        let deep = |n: usize, inner: &str| format!("{}{inner}{}", "[".repeat(n), "]".repeat(n));
        // The test type drops recursively: a deep result is leaked.
        let ok = |r: Result<V, Error>| r.map(std::mem::forget);
        // On a small stack: the scanner may not recurse once per level, and
        // the serde pass only as deep as serde goes before it gives up.
        std::thread::scope(|s| {
            std::thread::Builder::new()
                .stack_size(256 << 10)
                .spawn_scoped(s, || {
                    let lim = Limit::containers(60_000);
                    assert!(fast_v(&deep(SERDE_DEPTH, "1")).is_some());
                    assert!(fast_v(&deep(SERDE_DEPTH + 1, "1")).is_none());
                    assert_eq!(ok(loads(&deep(60_000, ""), lim)), Ok(()));
                    assert_eq!(
                        ok(loads(&deep(60_001, ""), lim)),
                        Err(Error::Recursion(false))
                    );
                    let mixed = format!("[{}]", deep(5, "{\"a\": {}}"));
                    assert_eq!(
                        ok(loads(&mixed, Limit::containers(7))),
                        Err(Error::Recursion(true))
                    );
                    assert_eq!(ok(loads(&mixed, Limit::containers(8))), Ok(()));
                    // Scalars count too: level 3 holds the 1.
                    assert_eq!(ok(loads(&deep(2, "1"), Limit::values(3))), Ok(()));
                    assert_eq!(
                        ok(loads(&deep(2, "1"), Limit::values(2))),
                        Err(Error::Recursion(false))
                    );
                    assert_eq!(ok(loads(&deep(2, ""), Limit::values(2))), Ok(()));
                    assert_eq!(ok(loads(&deep(2, "1"), Limit::containers(2))), Ok(()));
                })
                .unwrap()
                .join()
                .unwrap();
        });
    }
}
