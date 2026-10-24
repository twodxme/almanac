use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;

use super::{codec_map, py_codec, Codec, NAME_BMP, NMSTRT_BMP};

#[derive(Debug)]
pub enum Error {
    /// ET.ParseError: `release_notes` moves on to the next file.
    Parse,
    /// Any other exception: it reaches the caller.
    Raise(String),
}

type R<T> = Result<T, Error>;

fn bad<T>() -> R<T> {
    Err(Error::Parse)
}

#[derive(Debug, Default)]
pub struct Element {
    pub tag: String,
    pub attrib: Vec<(String, String)>,
    pub text: String,
    pub tail: String,
    pub children: Vec<Element>,
}

impl Drop for Element {
    // Iteratively, so a deeply nested document cannot overflow the stack.
    fn drop(&mut self) {
        let mut todo = std::mem::take(&mut self.children);
        while let Some(mut el) = todo.pop() {
            todo.append(&mut el.children);
        }
    }
}

impl Element {
    pub fn find(&self, tag: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.tag == tag)
    }
    /// `find("a/b")`: the first `b` under any `a` child.
    pub fn find2(&self, a: &str, b: &str) -> Option<&Element> {
        self.children.iter().filter(|c| c.tag == a).find_map(|c| c.find(b))
    }
    pub fn get(&self, key: &str) -> Option<&str> {
        self.attrib.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
}

const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";
const XMLNS_NS: &str = "http://www.w3.org/2000/xmlns/";
/// expat's guard against entity expansion bombs: past 8 MiB of expanded
/// text, at most 100 times the size of the document.
const AMPLIFICATION_START: usize = 8 * 1024 * 1024;
const AMPLIFICATION_MAX: usize = 100;
/// `_serialize_xml` recurses once per level; with CPython's limit of 1000
/// frames and the callers below `release_notes`, a child may nest 992 deep,
/// one less where the element still calls `_escape_cdata`/`_escape_attrib`.
const SERIALIZE_DEPTH: usize = 992;

fn valid_char(c: char) -> bool {
    matches!(c as u32, 0x9 | 0xA | 0xD | 0x20..=0xD7FF | 0xE000..=0xFFFD | 0x10000..=0x10FFFF)
}

fn is_s(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

fn in_table(t: &[(u16, u16)], cp: u32) -> bool {
    let i = t.partition_point(|&(_, hi)| u32::from(hi) < cp);
    t.get(i).is_some_and(|&(lo, _)| u32::from(lo) <= cp)
}

/// expat's name-start characters. `l1`: the document is ISO-8859-1 or
/// UTF-16, where U+0080-U+00FF use expat's Latin-1 table instead.
/// ':' is not one: in namespace mode it only separates a prefix.
fn nmstrt(c: char, l1: bool) -> bool {
    let cp = c as u32;
    if cp < 0x80 {
        return c.is_ascii_alphabetic() || c == '_';
    }
    if l1 && cp < 0x100 {
        return matches!(cp, 0xAA | 0xB5 | 0xBA | 0xC0..=0xD6 | 0xD8..=0xF6 | 0xF8..=0xFF);
    }
    cp <= 0xFFFF && in_table(NMSTRT_BMP, cp)
}

fn namech(c: char, l1: bool) -> bool {
    let cp = c as u32;
    if cp < 0x80 {
        return c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.');
    }
    if l1 && cp < 0x100 {
        return nmstrt(c, true) || cp == 0xB7;
    }
    cp <= 0xFFFF && in_table(NAME_BMP, cp)
}

/// A character expat types BT_NONASCII (or a UTF-8 lead byte), which is an
/// error wherever a name may not continue, rather than BT_OTHER.
fn nonascii_type(c: char, l1: bool) -> bool {
    let cp = c as u32;
    cp >= 0x80 && !(l1 && cp < 0x100)
}

fn at(s: &[char], i: usize) -> R<char> {
    s.get(i).copied().ok_or(Error::Parse)
}

fn starts(s: &[char], i: usize, lit: &str) -> bool {
    let mut j = i;
    for c in lit.chars() {
        if s.get(j) != Some(&c) {
            return false;
        }
        j += 1;
    }
    true
}

fn eq_ci(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

// ---- bytes to characters ----

struct Decl {
    encoding: Option<String>,
    standalone: bool,
}

struct Text {
    chars: Vec<char>,
    /// Where the prolog continues after the XML declaration.
    start: usize,
    standalone: bool,
    l1: bool,
}

const BUILTIN: [&str; 6] = ["ISO-8859-1", "US-ASCII", "UTF-8", "UTF-16", "UTF-16BE", "UTF-16LE"];

fn raise<T>(e: &str) -> R<T> {
    Err(Error::Raise(e.to_string()))
}

/// expat's auto-detection (BOM, or a zero byte for UTF-16), then the
/// encoding the declaration names; what pyexpat's handler says of a name
/// expat does not know itself.
fn decode(b: &[u8]) -> R<Text> {
    if b.len() < 2 {
        return bad();
    }
    let utf16 = match (b[0], b[1]) {
        (0xFE, 0xFF) => Some((false, 2)),
        (0xFF, 0xFE) => Some((true, 2)),
        (0, _) => Some((false, 0)),
        (_, 0) => Some((true, 0)),
        _ => None,
    };
    if let Some((le, skip)) = utf16 {
        let body = &b[skip..];
        let units = body.chunks_exact(2).map(|p| {
            if le { u16::from_le_bytes([p[0], p[1]]) } else { u16::from_be_bytes([p[0], p[1]]) }
        });
        let mut chars = Vec::new();
        let mut broken = body.len() % 2 == 1;
        for r in char::decode_utf16(units) {
            match r {
                Ok(c) => chars.push(c),
                Err(_) => {
                    broken = true;
                    break;
                }
            }
        }
        let decl = xml_decl(&chars)?;
        if let Some(name) = decl.as_ref().and_then(|d| d.encoding.as_deref()) {
            let ok = if eq_ci(name, "UTF-16") {
                true
            } else if eq_ci(name, "UTF-16LE") {
                le
            } else if eq_ci(name, "UTF-16BE") {
                !le
            } else if BUILTIN.iter().any(|n| eq_ci(name, n)) {
                false
            } else {
                match py_codec(name) {
                    Codec::Raise(e) => return raise(e),
                    // A one-byte map over UTF-16 text: NULs everywhere.
                    _ => false,
                }
            };
            if !ok || broken {
                return bad();
            }
        }
        if broken {
            return bad();
        }
        return finish(chars, decl, true);
    }
    let skip = if b.starts_with(&[0xEF, 0xBB, 0xBF]) { 3 } else { 0 };
    let body = &b[skip..];
    let decl_chars: Vec<char> =
        if body.starts_with(b"<?xml") && matches!(body.get(5), Some(b' ' | b'\t' | b'\n' | b'\r' | b'?')) {
            let Some(end) = body[5..].windows(2).position(|w| w == b"?>").map(|i| i + 7) else {
                return bad();
            };
            if body[..end].iter().any(|&c| c >= 0x80) {
                return bad();
            }
            body[..end].iter().map(|&c| char::from(c)).collect()
        } else {
            Vec::new()
        };
    let decl = xml_decl(&decl_chars)?;
    let name = decl.as_ref().and_then(|d| d.encoding.clone());
    let mut l1 = false;
    let chars: Vec<char> = match name.as_deref() {
        None => utf8(body)?,
        Some(n) if eq_ci(n, "UTF-8") => utf8(body)?,
        Some(n) if eq_ci(n, "ISO-8859-1") => {
            l1 = true;
            body.iter().map(|&c| char::from(c)).collect()
        }
        Some(n) if eq_ci(n, "US-ASCII") => {
            if body.iter().any(|&c| c >= 0x80) {
                return bad();
            }
            body.iter().map(|&c| char::from(c)).collect()
        }
        // UTF-16 named in a one-byte document.
        Some(n) if BUILTIN.iter().any(|b| eq_ci(n, b)) => return bad(),
        Some(n) => match py_codec(n) {
            Codec::Raise(e) => return raise(e),
            Codec::Unusable => return bad(),
            Codec::Map(i) => {
                let map = codec_map(i).ok_or(Error::Parse)?;
                let mut out = Vec::with_capacity(body.len());
                for &c in body {
                    match map[usize::from(c)] {
                        '\u{fffd}' => return bad(),
                        ch => out.push(ch),
                    }
                }
                out
            }
        },
    };
    finish(chars, decl, l1)
}

fn utf8(b: &[u8]) -> R<Vec<char>> {
    Ok(std::str::from_utf8(b).map_err(|_| Error::Parse)?.chars().collect())
}

/// Every character must be one XML allows; then XML end-of-line handling.
fn finish(chars: Vec<char>, decl: Option<Decl>, l1: bool) -> R<Text> {
    if !chars.iter().all(|&c| valid_char(c)) {
        return bad();
    }
    let mut out = Vec::with_capacity(chars.len());
    let mut it = chars.into_iter().peekable();
    while let Some(c) = it.next() {
        if c == '\r' {
            if it.peek() == Some(&'\n') {
                it.next();
            }
            out.push('\n');
        } else {
            out.push(c);
        }
    }
    let start = match &decl {
        Some(_) => (5..out.len()).find(|&i| out[i] == '?' && out.get(i + 1) == Some(&'>')).map_or(0, |i| i + 2),
        None => 0,
    };
    let standalone = decl.as_ref().is_some_and(|d| d.standalone);
    Ok(Text { chars: out, start, standalone, l1 })
}

/// The XML declaration at the very start, if there is one (expat's
/// doParseXmlDecl: version, then optional encoding, then standalone).
fn xml_decl(s: &[char]) -> R<Option<Decl>> {
    if !(starts(s, 0, "<?xml") && s.get(5).is_some_and(|&c| is_s(c) || c == '?')) {
        return Ok(None);
    }
    let Some(end) = (5..s.len()).find(|&i| s[i] == '?' && s.get(i + 1) == Some(&'>')) else {
        return bad();
    };
    if !s[..end].iter().all(|&c| valid_char(c)) {
        return bad();
    }
    let body = &s[5..end];
    let mut p = 0;
    let Some((name, _)) = pseudo_attr(body, &mut p)? else { return bad() };
    if name != "version" {
        return bad();
    }
    let mut decl = Decl { encoding: None, standalone: false };
    let Some((mut name, mut val)) = pseudo_attr(body, &mut p)? else { return Ok(Some(decl)) };
    if name == "encoding" {
        if !val.starts_with(|c: char| c.is_ascii_alphabetic()) {
            return bad();
        }
        decl.encoding = Some(val);
        match pseudo_attr(body, &mut p)? {
            None => return Ok(Some(decl)),
            Some((n, v)) => (name, val) = (n, v),
        }
    }
    if name != "standalone" {
        return bad();
    }
    decl.standalone = match val.as_str() {
        "yes" => true,
        "no" => false,
        _ => return bad(),
    };
    while p < body.len() && is_s(body[p]) {
        p += 1;
    }
    if p != body.len() {
        return bad();
    }
    Ok(Some(decl))
}

/// expat's parsePseudoAttribute: ` name = "value"`, the value restricted
/// to [A-Za-z0-9._-]; None when only whitespace is left.
fn pseudo_attr(s: &[char], p: &mut usize) -> R<Option<(String, String)>> {
    if *p == s.len() {
        return Ok(None);
    }
    if !is_s(s[*p]) {
        return bad();
    }
    while *p < s.len() && is_s(s[*p]) {
        *p += 1;
    }
    if *p == s.len() {
        return Ok(None);
    }
    let name_start = *p;
    let name_end;
    loop {
        let c = at(s, *p)?;
        if !c.is_ascii() {
            return bad();
        }
        if c == '=' {
            name_end = *p;
            break;
        }
        if is_s(c) {
            name_end = *p;
            while *p < s.len() && is_s(s[*p]) {
                *p += 1;
            }
            if s.get(*p) != Some(&'=') {
                return bad();
            }
            break;
        }
        *p += 1;
    }
    if name_end == name_start {
        return bad();
    }
    *p += 1;
    while *p < s.len() && is_s(s[*p]) {
        *p += 1;
    }
    let open = at(s, *p)?;
    if open != '"' && open != '\'' {
        return bad();
    }
    *p += 1;
    let val_start = *p;
    loop {
        let c = at(s, *p)?;
        if c == open {
            break;
        }
        if !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')) {
            return bad();
        }
        *p += 1;
    }
    let val: String = s[val_start..*p].iter().collect();
    *p += 1;
    Ok(Some((s[name_start..name_end].iter().collect(), val)))
}

// ---- tokens shared by prolog and content ----

#[derive(Debug, Clone, Copy, PartialEq)]
enum NameKind {
    Name,
    Prefixed,
    Nmtoken,
}

#[derive(Debug)]
enum Tok {
    S,
    Pi,
    XmlDecl,
    Comment,
    DeclOpen(String),
    InstanceStart,
    Name(NameKind, String),
    /// A name with `?`, `*` or `+` straight after it.
    NameRep,
    /// The span between the quotes.
    Literal(Range<usize>),
    Pound(String),
    Percent,
    PeRef,
    OpenBracket,
    CloseBracket,
    CondOpen,
    CondClose,
    OpenParen,
    CloseParen(Option<char>),
    Or,
    Comma,
    DeclClose,
}

/// expat's prologTok: the next token at `s[*i..]`, None at the end.
fn prolog_tok(s: &[char], i: &mut usize, l1: bool) -> R<Option<Tok>> {
    let Some(&c) = s.get(*i) else { return Ok(None) };
    let one = |i: &mut usize, t: Tok| {
        *i += 1;
        Ok(Some(t))
    };
    match c {
        '"' | '\'' => {
            let start = *i + 1;
            let mut k = start;
            while at(s, k)? != c {
                k += 1;
            }
            match s.get(k + 1) {
                Some(&d) if is_s(d) || matches!(d, '>' | '%' | '[') => {}
                _ => return bad(),
            }
            *i = k + 1;
            Ok(Some(Tok::Literal(start..k)))
        }
        '<' => match at(s, *i + 1)? {
            '!' => {
                *i += 2;
                scan_decl(s, i).map(Some)
            }
            '?' => {
                *i += 2;
                scan_pi(s, i, l1).map(Some)
            }
            d if nmstrt(d, l1) || nonascii_type(d, l1) => Ok(Some(Tok::InstanceStart)),
            _ => bad(),
        },
        c if is_s(c) => {
            while s.get(*i).is_some_and(|&d| is_s(d)) {
                *i += 1;
            }
            Ok(Some(Tok::S))
        }
        '%' => {
            *i += 1;
            let d = at(s, *i)?;
            if is_s(d) || d == '%' {
                return Ok(Some(Tok::Percent));
            }
            if !nmstrt(d, l1) {
                return bad();
            }
            *i += 1;
            loop {
                let d = at(s, *i)?;
                *i += 1;
                if d == ';' {
                    return Ok(Some(Tok::PeRef));
                }
                if !namech(d, l1) {
                    return bad();
                }
            }
        }
        ',' => one(i, Tok::Comma),
        '[' => one(i, Tok::OpenBracket),
        ']' => {
            if s.get(*i + 1) == Some(&']') {
                if at(s, *i + 2)? == '>' {
                    *i += 3;
                    return Ok(Some(Tok::CondClose));
                }
            }
            one(i, Tok::CloseBracket)
        }
        '(' => one(i, Tok::OpenParen),
        ')' => match at(s, *i + 1)? {
            d @ ('*' | '?' | '+') => {
                *i += 2;
                Ok(Some(Tok::CloseParen(Some(d))))
            }
            d if is_s(d) || matches!(d, '>' | ',' | '|' | ')') => one(i, Tok::CloseParen(None)),
            _ => bad(),
        },
        '|' => one(i, Tok::Or),
        '>' => one(i, Tok::DeclClose),
        '#' => {
            *i += 1;
            let start = *i;
            if !nmstrt(at(s, *i)?, l1) {
                return bad();
            }
            *i += 1;
            loop {
                let d = at(s, *i)?;
                if namech(d, l1) {
                    *i += 1;
                } else if is_s(d) || matches!(d, ')' | '>' | '%' | '|') {
                    return Ok(Some(Tok::Pound(s[start..*i].iter().collect())));
                } else {
                    return bad();
                }
            }
        }
        c => {
            let start = *i;
            let mut kind = if nmstrt(c, l1) {
                NameKind::Name
            } else if namech(c, l1) || c == ':' {
                NameKind::Nmtoken
            } else {
                return bad();
            };
            *i += 1;
            loop {
                let d = at(s, *i)?;
                if namech(d, l1) {
                    *i += 1;
                } else if is_s(d) || matches!(d, '>' | ')' | ',' | '|' | '[' | '%') {
                    return Ok(Some(Tok::Name(kind, s[start..*i].iter().collect())));
                } else if d == ':' {
                    *i += 1;
                    match kind {
                        NameKind::Name => {
                            let e = at(s, *i)?;
                            if namech(e, l1) {
                                kind = NameKind::Prefixed;
                                *i += 1;
                            } else if nonascii_type(e, l1) {
                                return bad();
                            } else {
                                kind = NameKind::Nmtoken;
                            }
                        }
                        NameKind::Prefixed => kind = NameKind::Nmtoken,
                        NameKind::Nmtoken => {}
                    }
                } else if matches!(d, '+' | '*' | '?') {
                    if kind == NameKind::Nmtoken {
                        return bad();
                    }
                    *i += 1;
                    return Ok(Some(Tok::NameRep));
                } else {
                    return bad();
                }
            }
        }
    }
}

/// After `<!`: a comment, `<![`, or a declaration keyword.
fn scan_decl(s: &[char], i: &mut usize) -> R<Tok> {
    match at(s, *i)? {
        '-' => {
            *i += 1;
            return scan_comment(s, i);
        }
        '[' => {
            *i += 1;
            return Ok(Tok::CondOpen);
        }
        c if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return bad(),
    }
    let start = *i;
    loop {
        let c = at(s, *i)?;
        if c == '%' {
            // don't allow <!ENTITY% foo "whatever">
            let d = at(s, *i + 1)?;
            if is_s(d) || d == '%' {
                return bad();
            }
            break;
        }
        if is_s(c) {
            break;
        }
        if !(c.is_ascii_alphabetic() || c == '_') {
            return bad();
        }
        *i += 1;
    }
    Ok(Tok::DeclOpen(s[start..*i].iter().collect()))
}

/// After `<!-`.
fn scan_comment(s: &[char], i: &mut usize) -> R<Tok> {
    if at(s, *i)? != '-' {
        return bad();
    }
    *i += 1;
    loop {
        let c = at(s, *i)?;
        *i += 1;
        if c == '-' && at(s, *i)? == '-' {
            *i += 1;
            if at(s, *i)? != '>' {
                return bad();
            }
            *i += 1;
            return Ok(Tok::Comment);
        }
    }
}

/// After `<?`: a processing instruction, or the XML declaration's `xml`.
fn scan_pi(s: &[char], i: &mut usize, l1: bool) -> R<Tok> {
    let start = *i;
    if !nmstrt(at(s, *i)?, l1) {
        return bad();
    }
    *i += 1;
    while s.get(*i).is_some_and(|&c| namech(c, l1)) {
        *i += 1;
    }
    let target: String = s[start..*i].iter().collect();
    let tok = if target == "xml" {
        Tok::XmlDecl
    } else if eq_ci(&target, "xml") {
        return bad();
    } else {
        Tok::Pi
    };
    let c = at(s, *i)?;
    *i += 1;
    if is_s(c) {
        loop {
            let c = at(s, *i)?;
            *i += 1;
            if c == '?' && at(s, *i)? == '>' {
                *i += 1;
                return Ok(tok);
            }
        }
    }
    if c == '?' && at(s, *i)? == '>' {
        *i += 1;
        return Ok(tok);
    }
    bad()
}

enum Ref {
    Char(char),
    Name(String),
}

/// After `&`: `name;`, `#digits;` or `#xhex;` (expat's scanRef and
/// XmlCharRefNumber); returns the reference and where it ends.
fn scan_ref(s: &[char], i: usize, l1: bool) -> R<(Ref, usize)> {
    let mut k = i;
    if at(s, k)? == '#' {
        k += 1;
        let hex = s.get(k) == Some(&'x');
        if hex {
            k += 1;
        }
        let digit = |c: char| if hex { c.is_ascii_hexdigit() } else { c.is_ascii_digit() };
        if !digit(at(s, k)?) {
            return bad();
        }
        let start = k;
        while at(s, k)? != ';' {
            if !digit(s[k]) {
                return bad();
            }
            k += 1;
        }
        let mut n: u32 = 0;
        for &c in &s[start..k] {
            n = n * if hex { 16 } else { 10 } + c.to_digit(16).unwrap_or(0);
            if n >= 0x110000 {
                return bad();
            }
        }
        let c = char::from_u32(n).filter(|&c| valid_char(c)).ok_or(Error::Parse)?;
        return Ok((Ref::Char(c), k + 1));
    }
    if !nmstrt(at(s, k)?, l1) {
        return bad();
    }
    k += 1;
    loop {
        let c = at(s, k)?;
        if c == ';' {
            return Ok((Ref::Name(s[i..k].iter().collect()), k + 1));
        }
        if !namech(c, l1) {
            return bad();
        }
        k += 1;
    }
}

fn predefined(name: &str) -> Option<char> {
    Some(match name {
        "lt" => '<',
        "gt" => '>',
        "amp" => '&',
        "quot" => '"',
        "apos" => '\'',
        _ => return None,
    })
}

/// A start-tag or attribute name: a QName, at most one ':' with a name
/// start character after it.
fn qname(s: &[char], p: &mut usize, l1: bool) -> R<String> {
    let start = *p;
    if !nmstrt(at(s, *p)?, l1) {
        return bad();
    }
    *p += 1;
    let mut colon = false;
    loop {
        match s.get(*p) {
            Some(&c) if namech(c, l1) => *p += 1,
            Some(':') => {
                if colon || !nmstrt(at(s, *p + 1)?, l1) {
                    return bad();
                }
                colon = true;
                *p += 2;
            }
            _ => return Ok(s[start..*p].iter().collect()),
        }
    }
}

fn skip_s(s: &[char], p: &mut usize) -> bool {
    let start = *p;
    while s.get(*p).is_some_and(|&c| is_s(c)) {
        *p += 1;
    }
    *p > start
}

// ---- the parser ----

enum Ent {
    Text(Rc<[char]>),
    External,
    Unparsed,
}

#[derive(Clone)]
struct DefAtt {
    name: String,
    value: Option<String>,
    cdata: bool,
}

/// One stream of characters being read: the document, or the replacement
/// text of an entity referenced from content.
struct Src {
    text: Rc<[char]>,
    pos: usize,
    l1: bool,
    entity: Option<String>,
    /// Open elements when it began: it must close exactly those it opens.
    depth: usize,
}

struct Open {
    el: Element,
    raw: String,
    bindings: usize,
}

type Bindings = Vec<(Option<String>, String)>;

struct Parser {
    s: Rc<[char]>,
    l1: bool,
    standalone: bool,
    has_pe_refs: bool,
    keep_processing: bool,
    entities: HashMap<String, Ent>,
    attlists: HashMap<String, Vec<DefAtt>>,
    open: HashSet<String>,
    direct: usize,
    expanded: usize,
}

/// `ET.parse(path).getroot()`; Err(Parse) is ET.ParseError.
pub fn parse(bytes: &[u8]) -> Result<Element, Error> {
    let text = decode(bytes)?;
    let mut p = Parser {
        s: Rc::from(text.chars),
        l1: text.l1,
        standalone: text.standalone,
        has_pe_refs: false,
        keep_processing: true,
        entities: HashMap::new(),
        attlists: HashMap::new(),
        open: HashSet::new(),
        direct: bytes.len().max(1),
        expanded: 0,
    };
    let mut i = text.start;
    p.prolog(&mut i)?;
    let (root, end) = p.content(i)?;
    p.epilog(end)?;
    Ok(root)
}

#[derive(Clone, Copy, PartialEq)]
enum St {
    Prolog,
    AfterDoctype,
    Doctype0,
    Doctype1,
    Doctype2,
    Doctype3,
    Doctype4,
    Doctype5,
    Subset,
    Entity0,
    Entity1,
    Entity2,
    Entity3,
    Entity4,
    Entity5,
    Entity6,
    Entity7,
    Entity8,
    Entity9,
    Entity10,
    Notation0,
    Notation1,
    Notation2,
    Notation3,
    Notation4,
    Attlist0,
    Attlist1,
    Attlist2,
    Attlist3,
    Attlist4,
    Attlist5,
    Attlist6,
    Attlist7,
    Attlist8,
    Attlist9,
    Element0,
    Element1,
    Element2,
    Element3,
    Element4,
    Element5,
    Element6,
    Element7,
    DeclClose,
}

fn pubid_ok(s: &[char]) -> bool {
    s.iter().all(|&c| c.is_ascii_alphanumeric() || " \r\n-'()+,./:=?;!*#@$_%".contains(c))
}

impl Parser {
    /// The prolog and internal DTD subset, as expat's xmlrole.c state
    /// machine reads them; returns at the root element's `<`.
    fn prolog(&mut self, i: &mut usize) -> R<()> {
        use NameKind::{Name, Nmtoken, Prefixed};
        let s = self.s.clone();
        let mut st = St::Prolog;
        let mut decl_entity: Option<String> = None;
        let (mut att_el, mut att_name) = (String::new(), String::new());
        let (mut att_cdata, mut att_id) = (false, false);
        // Content-model groups: the connector each open level has used.
        let mut groups: Vec<Option<char>> = Vec::new();
        loop {
            // The document ending in the prolog: no element found.
            let tok = prolog_tok(&s, i, self.l1)?.ok_or(Error::Parse)?;
            if matches!(tok, Tok::S) && !matches!(st, St::Prolog | St::AfterDoctype | St::Subset) {
                continue;
            }
            st = match (st, tok) {
                (St::Prolog | St::AfterDoctype, Tok::S | Tok::Pi | Tok::Comment) => st,
                (St::Prolog | St::AfterDoctype, Tok::InstanceStart) => return Ok(()),
                (St::Prolog, Tok::DeclOpen(n)) if n == "DOCTYPE" => St::Doctype0,
                (St::Doctype0, Tok::Name(Name | Prefixed, _)) => St::Doctype1,
                (St::Doctype1 | St::Doctype4, Tok::OpenBracket) => St::Subset,
                (St::Doctype1 | St::Doctype4 | St::Doctype5, Tok::DeclClose) => St::AfterDoctype,
                (St::Doctype1, Tok::Name(Name, n)) if n == "SYSTEM" => St::Doctype3,
                (St::Doctype1, Tok::Name(Name, n)) if n == "PUBLIC" => St::Doctype2,
                (St::Doctype2, Tok::Literal(r)) => {
                    if !pubid_ok(&s[r]) {
                        return bad();
                    }
                    self.has_pe_refs = true;
                    St::Doctype3
                }
                (St::Doctype3, Tok::Literal(_)) => {
                    self.has_pe_refs = true;
                    St::Doctype4
                }
                (St::Subset, Tok::S | Tok::Pi | Tok::Comment) => St::Subset,
                (St::Subset, Tok::DeclOpen(n)) => match n.as_str() {
                    "ENTITY" => St::Entity0,
                    "ATTLIST" => St::Attlist0,
                    "ELEMENT" => St::Element0,
                    "NOTATION" => St::Notation0,
                    _ => return bad(),
                },
                (St::Subset, Tok::PeRef) => {
                    // Parameter entities are never read (pyexpat's default),
                    // so what follows may depend on one we did not see.
                    self.has_pe_refs = true;
                    self.keep_processing = self.standalone;
                    St::Subset
                }
                (St::Subset, Tok::CloseBracket) => St::Doctype5,

                (St::Entity0, Tok::Percent) => St::Entity1,
                (St::Entity0, Tok::Name(Name, n)) => {
                    decl_entity = None;
                    if self.keep_processing && !self.entities.contains_key(&n) {
                        self.entities.insert(n.clone(), Ent::External);
                        decl_entity = Some(n);
                    }
                    St::Entity2
                }
                (St::Entity1, Tok::Name(Name, _)) => {
                    decl_entity = None;
                    St::Entity7
                }
                (St::Entity2, Tok::Name(Name, n)) if n == "SYSTEM" => St::Entity4,
                (St::Entity2, Tok::Name(Name, n)) if n == "PUBLIC" => St::Entity3,
                (St::Entity2 | St::Entity7, Tok::Literal(r)) => {
                    if self.keep_processing {
                        let value = self.entity_value(&s[r])?;
                        if let Some(n) = decl_entity.take() {
                            self.entities.insert(n, Ent::Text(Rc::from(value)));
                        }
                    }
                    St::DeclClose
                }
                (St::Entity3 | St::Entity8, Tok::Literal(r)) => {
                    if !pubid_ok(&s[r]) {
                        return bad();
                    }
                    if st == St::Entity3 { St::Entity4 } else { St::Entity9 }
                }
                (St::Entity4, Tok::Literal(_)) => St::Entity5,
                (St::Entity5 | St::Entity10, Tok::DeclClose) => St::Subset,
                (St::Entity5, Tok::Name(Name, n)) if n == "NDATA" => St::Entity6,
                (St::Entity6, Tok::Name(Name, _)) => {
                    if let Some(n) = decl_entity.take() {
                        self.entities.insert(n, Ent::Unparsed);
                    }
                    St::DeclClose
                }
                (St::Entity7, Tok::Name(Name, n)) if n == "SYSTEM" => St::Entity9,
                (St::Entity7, Tok::Name(Name, n)) if n == "PUBLIC" => St::Entity8,
                (St::Entity9, Tok::Literal(_)) => St::Entity10,

                (St::Notation0, Tok::Name(Name, _)) => St::Notation1,
                (St::Notation1, Tok::Name(Name, n)) if n == "SYSTEM" => St::Notation3,
                (St::Notation1, Tok::Name(Name, n)) if n == "PUBLIC" => St::Notation2,
                (St::Notation2, Tok::Literal(r)) => {
                    if !pubid_ok(&s[r]) {
                        return bad();
                    }
                    St::Notation4
                }
                (St::Notation3 | St::Notation4, Tok::Literal(_)) => St::DeclClose,
                (St::Notation4, Tok::DeclClose) => St::Subset,

                (St::Attlist0, Tok::Name(Name | Prefixed, n)) => {
                    att_el = n;
                    St::Attlist1
                }
                (St::Attlist1, Tok::DeclClose) => St::Subset,
                (St::Attlist1, Tok::Name(Name | Prefixed, n)) => {
                    att_name = n;
                    (att_cdata, att_id) = (false, false);
                    St::Attlist2
                }
                (St::Attlist2, Tok::Name(Name, n)) => match n.as_str() {
                    "CDATA" => {
                        att_cdata = true;
                        St::Attlist8
                    }
                    "ID" => {
                        att_id = true;
                        St::Attlist8
                    }
                    "IDREF" | "IDREFS" | "ENTITY" | "ENTITIES" | "NMTOKEN" | "NMTOKENS" => St::Attlist8,
                    "NOTATION" => St::Attlist5,
                    _ => return bad(),
                },
                (St::Attlist2, Tok::OpenParen) => St::Attlist3,
                (St::Attlist3, Tok::Name(_, _)) => St::Attlist4,
                (St::Attlist4, Tok::CloseParen(None)) | (St::Attlist7, Tok::CloseParen(None)) => St::Attlist8,
                (St::Attlist4, Tok::Or) => St::Attlist3,
                (St::Attlist5, Tok::OpenParen) => St::Attlist6,
                (St::Attlist6, Tok::Name(Name, _)) => St::Attlist7,
                (St::Attlist7, Tok::Or) => St::Attlist6,
                (St::Attlist8, Tok::Pound(n)) => match n.as_str() {
                    "IMPLIED" | "REQUIRED" => {
                        if self.keep_processing {
                            self.define(&att_el, &att_name, att_cdata, att_id, None);
                        }
                        St::Attlist1
                    }
                    "FIXED" => St::Attlist9,
                    _ => return bad(),
                },
                (St::Attlist8 | St::Attlist9, Tok::Literal(r)) => {
                    if self.keep_processing {
                        let value = self.attr_value(&s[r], self.l1, att_cdata)?;
                        self.define(&att_el, &att_name, att_cdata, att_id, Some(value));
                    }
                    St::Attlist1
                }

                (St::Element0, Tok::Name(Name | Prefixed, _)) => St::Element1,
                (St::Element1, Tok::Name(Name, n)) if n == "EMPTY" || n == "ANY" => St::DeclClose,
                (St::Element1, Tok::OpenParen) => {
                    groups = vec![None];
                    St::Element2
                }
                (St::Element2, Tok::Pound(n)) if n == "PCDATA" => St::Element3,
                (St::Element2 | St::Element6, Tok::OpenParen) => {
                    groups.push(None);
                    St::Element6
                }
                (St::Element2 | St::Element6, Tok::Name(Name | Prefixed, _) | Tok::NameRep) => St::Element7,
                (St::Element3, Tok::CloseParen(None | Some('*'))) => St::DeclClose,
                (St::Element3 | St::Element5, Tok::Or) => St::Element4,
                (St::Element4, Tok::Name(Name | Prefixed, _)) => St::Element5,
                (St::Element5, Tok::CloseParen(Some('*'))) => St::DeclClose,
                (St::Element7, Tok::CloseParen(_)) => {
                    groups.pop();
                    if groups.is_empty() { St::DeclClose } else { St::Element7 }
                }
                (St::Element7, t @ (Tok::Comma | Tok::Or)) => {
                    // One group may not mix ',' and '|'.
                    let conn = if matches!(t, Tok::Comma) { ',' } else { '|' };
                    let Some(level) = groups.last_mut() else { return bad() };
                    if level.is_some_and(|c| c != conn) {
                        return bad();
                    }
                    *level = Some(conn);
                    St::Element6
                }
                (St::DeclClose, Tok::DeclClose) => St::Subset,
                (_, Tok::Name(Nmtoken, _)) | (_, _) => return bad(),
            };
        }
    }

    /// expat's defineAttribute: the first declaration of an attribute
    /// wins, except that #IMPLIED/#REQUIRED ones are always recorded.
    fn define(&mut self, el: &str, name: &str, cdata: bool, id: bool, value: Option<String>) {
        let list = self.attlists.entry(el.to_string()).or_default();
        if (value.is_some() || id) && list.iter().any(|d| d.name == name) {
            return;
        }
        list.push(DefAtt { name: name.to_string(), value, cdata });
    }

    /// An entity value as declared: character references replaced, entity
    /// references kept for later; a parameter entity reference is an
    /// error in the internal subset.
    fn entity_value(&self, lit: &[char]) -> R<Vec<char>> {
        let mut out = Vec::with_capacity(lit.len());
        let mut i = 0;
        while i < lit.len() {
            match lit[i] {
                '%' => return bad(),
                '&' => {
                    let (r, j) = scan_ref(lit, i + 1, self.l1)?;
                    match r {
                        Ref::Char(c) => out.push(c),
                        Ref::Name(_) => out.extend_from_slice(&lit[i..j]),
                    }
                    i = j;
                }
                c => {
                    out.push(c);
                    i += 1;
                }
            }
        }
        Ok(out)
    }

    /// Count expanded entity text against expat's amplification limit.
    fn account(&mut self, n: usize) -> R<()> {
        self.expanded += n;
        if self.expanded >= AMPLIFICATION_START && (self.direct + self.expanded) / self.direct > AMPLIFICATION_MAX {
            return bad();
        }
        Ok(())
    }

    /// The value of an attribute (expat's appendAttributeValue): references
    /// replaced, whitespace characters made spaces; for a type other than
    /// CDATA, leading, trailing and repeated spaces dropped too.
    fn attr_value(&mut self, raw: &[char], l1: bool, cdata: bool) -> R<String> {
        let mut out = String::new();
        let skip_space = |out: &String| !cdata && (out.is_empty() || out.ends_with(' '));
        let mut frames: Vec<(Rc<[char]>, usize, bool, Option<String>)> = vec![(Rc::from(raw), 0, l1, None)];
        while let Some(top) = frames.len().checked_sub(1) {
            let (text, pos, fl1) = (frames[top].0.clone(), frames[top].1, frames[top].2);
            if pos >= text.len() {
                if let Some(name) = frames[top].3.take() {
                    self.open.remove(&name);
                }
                frames.pop();
                continue;
            }
            match text[pos] {
                '&' => {
                    let (r, next) = scan_ref(&text, pos + 1, fl1)?;
                    frames[top].1 = next;
                    match r {
                        Ref::Char(c) => {
                            if !(c == ' ' && skip_space(&out)) {
                                out.push(c);
                            }
                        }
                        Ref::Name(name) => {
                            if let Some(c) = predefined(&name) {
                                out.push(c);
                                continue;
                            }
                            let check = self.standalone || !self.has_pe_refs;
                            let repl = match self.entities.get(&name) {
                                // Not declared, where it might have been in a
                                // DTD we did not read: dropped.
                                None if !check => continue,
                                Some(Ent::Text(t)) => t.clone(),
                                _ => return bad(),
                            };
                            if self.open.contains(&name) {
                                return bad();
                            }
                            self.account(repl.len())?;
                            self.open.insert(name.clone());
                            frames.push((repl, 0, false, Some(name)));
                        }
                    }
                }
                '<' => return bad(),
                c => {
                    frames[top].1 += 1;
                    if is_s(c) {
                        if !skip_space(&out) {
                            out.push(' ');
                        }
                    } else {
                        out.push(c);
                    }
                }
            }
        }
        if !cdata && out.ends_with(' ') {
            out.pop();
        }
        Ok(out)
    }

    /// The root element and everything in it, with entity references in
    /// content read in place; returns the root and where the document goes on.
    fn content(&mut self, start: usize) -> R<(Element, usize)> {
        let mut srcs = vec![Src { text: self.s.clone(), pos: start, l1: self.l1, entity: None, depth: 0 }];
        let mut stack: Vec<Open> = Vec::new();
        let mut binds: Bindings = Vec::new();
        loop {
            let top = srcs.len() - 1;
            let (t, mut p, l1, depth) = (srcs[top].text.clone(), srcs[top].pos, srcs[top].l1, srcs[top].depth);
            if p >= t.len() {
                if let Some(name) = srcs[top].entity.take() {
                    if stack.len() != depth {
                        return bad(); // asynchronous entity
                    }
                    self.open.remove(&name);
                    srcs.pop();
                    continue;
                }
                return bad(); // the document ended inside the root element
            }
            match t[p] {
                '<' => match at(&t, p + 1)? {
                    '/' => {
                        p += 2;
                        let start = p;
                        if !nmstrt(at(&t, p)?, l1) {
                            return bad();
                        }
                        p += 1;
                        while t.get(p).is_some_and(|&c| namech(c, l1) || c == ':') {
                            p += 1;
                        }
                        let raw: String = t[start..p].iter().collect();
                        skip_s(&t, &mut p);
                        if at(&t, p)? != '>' {
                            return bad();
                        }
                        p += 1;
                        if stack.len() <= depth {
                            return bad(); // closes what the entity did not open
                        }
                        let Some(open) = stack.pop() else { return bad() };
                        if open.raw != raw {
                            return bad(); // mismatched tag
                        }
                        binds.truncate(open.bindings);
                        match stack.last_mut() {
                            Some(parent) => parent.el.children.push(open.el),
                            None => return Ok((open.el, p)),
                        }
                    }
                    '!' => {
                        if starts(&t, p, "<!--") {
                            p += 3;
                            scan_comment(&t, &mut p)?;
                        } else if starts(&t, p, "<![CDATA[") {
                            p += 9;
                            let a = p;
                            while !starts(&t, p, "]]>") {
                                at(&t, p)?;
                                p += 1;
                            }
                            if let Some(open) = stack.last_mut() {
                                push_text(&mut open.el, &t[a..p]);
                            }
                            p += 3;
                        } else {
                            return bad();
                        }
                    }
                    '?' => {
                        p += 2;
                        if !matches!(scan_pi(&t, &mut p, l1)?, Tok::Pi) {
                            return bad(); // XML declaration not at start
                        }
                    }
                    _ => {
                        p += 1;
                        if let Some(root) = self.start_tag(&t, &mut p, l1, &mut stack, &mut binds)? {
                            return Ok((root, p));
                        }
                    }
                },
                '&' => {
                    let (r, next) = scan_ref(&t, p + 1, l1)?;
                    p = next;
                    let c = match r {
                        Ref::Char(c) => c,
                        Ref::Name(name) => match predefined(&name) {
                            Some(c) => c,
                            None => {
                                // Undeclared, external or unparsed: an error
                                // from expat or from ET's default handler.
                                let Some(Ent::Text(repl)) = self.entities.get(&name) else { return bad() };
                                let repl = repl.clone();
                                if self.open.contains(&name) {
                                    return bad(); // recursive entity reference
                                }
                                self.account(repl.len())?;
                                srcs[top].pos = p;
                                self.open.insert(name.clone());
                                srcs.push(Src { text: repl, pos: 0, l1: false, entity: Some(name), depth: stack.len() });
                                continue;
                            }
                        },
                    };
                    if let Some(open) = stack.last_mut() {
                        push_text(&mut open.el, &[c]);
                    }
                }
                _ => {
                    let a = p;
                    while p < t.len() && t[p] != '<' && t[p] != '&' {
                        if t[p] == ']' && t.get(p + 1) == Some(&']') && t.get(p + 2) == Some(&'>') {
                            return bad();
                        }
                        p += 1;
                    }
                    if let Some(open) = stack.last_mut() {
                        push_text(&mut open.el, &t[a..p]);
                    }
                }
            }
            srcs[top].pos = p;
        }
    }

    /// After `<`: a start tag (expat's scanAtts, then storeAtts with the
    /// declared defaults and types, then namespace processing). Returns the
    /// root when it was an empty-element tag with nothing open.
    fn start_tag(
        &mut self,
        t: &[char],
        p: &mut usize,
        l1: bool,
        stack: &mut Vec<Open>,
        binds: &mut Bindings,
    ) -> R<Option<Element>> {
        let raw = qname(t, p, l1)?;
        let mut specified: Vec<(String, Range<usize>)> = Vec::new();
        let empty = loop {
            let ws = skip_s(t, p);
            match at(t, *p)? {
                '>' => {
                    *p += 1;
                    break false;
                }
                '/' => {
                    if at(t, *p + 1)? != '>' {
                        return bad();
                    }
                    *p += 2;
                    break true;
                }
                _ if ws => {}
                _ => return bad(),
            }
            let name = qname(t, p, l1)?;
            skip_s(t, p);
            if at(t, *p)? != '=' {
                return bad();
            }
            *p += 1;
            skip_s(t, p);
            let q = at(t, *p)?;
            if q != '"' && q != '\'' {
                return bad();
            }
            *p += 1;
            let a = *p;
            loop {
                match at(t, *p)? {
                    c if c == q => break,
                    '<' => return bad(),
                    '&' => *p = scan_ref(t, *p + 1, l1)?.1,
                    _ => *p += 1,
                }
            }
            specified.push((name, a..*p));
            *p += 1;
        };
        for (k, (name, _)) in specified.iter().enumerate() {
            if specified[..k].iter().any(|(n, _)| n == name) {
                return bad(); // duplicate attribute
            }
        }
        let decls = self.attlists.get(&raw).cloned().unwrap_or_default();
        let mark = binds.len();
        let mut attrs: Vec<(String, String)> = Vec::new();
        for (name, range) in &specified {
            let cdata = decls.iter().find(|d| &d.name == name).is_none_or(|d| d.cdata);
            let value = self.attr_value(&t[range.clone()], l1, cdata)?;
            attribute(name, value, binds, &mut attrs)?;
        }
        for d in &decls {
            if let Some(v) = &d.value {
                if !specified.iter().any(|(n, _)| *n == d.name) {
                    attribute(&d.name, v.clone(), binds, &mut attrs)?;
                }
            }
        }
        let tag = match raw.split_once(':') {
            Some((prefix, local)) => format!("{{{}}}{local}", lookup(binds, prefix)?),
            None => match binds.iter().rev().find(|(p, _)| p.is_none()) {
                Some((_, uri)) if !uri.is_empty() => format!("{{{uri}}}{raw}"),
                _ => raw.clone(),
            },
        };
        let mut el = Element::default();
        el.tag = tag;
        for (name, value) in attrs {
            // Attributes without a prefix are in no namespace.
            let key = match name.split_once(':') {
                Some((prefix, local)) => format!("{{{}}}{local}", lookup(binds, prefix)?),
                None => name,
            };
            if el.attrib.iter().any(|(k, _)| *k == key) {
                return bad();
            }
            el.attrib.push((key, value));
        }
        if !empty {
            stack.push(Open { el, raw, bindings: mark });
            return Ok(None);
        }
        binds.truncate(mark);
        match stack.last_mut() {
            Some(parent) => {
                parent.el.children.push(el);
                Ok(None)
            }
            None => Ok(Some(el)),
        }
    }

    /// After the root element: only whitespace, comments and PIs.
    fn epilog(&self, mut i: usize) -> R<()> {
        loop {
            match prolog_tok(&self.s, &mut i, self.l1)? {
                None => return Ok(()),
                Some(Tok::S | Tok::Pi | Tok::Comment) => {}
                Some(_) => return bad(),
            }
        }
    }
}

/// A specified or defaulted attribute: a namespace declaration, or kept.
fn attribute(name: &str, value: String, binds: &mut Bindings, attrs: &mut Vec<(String, String)>) -> R<()> {
    if name == "xmlns" {
        add_binding(binds, None, value)
    } else if let Some(prefix) = name.strip_prefix("xmlns:") {
        add_binding(binds, Some(prefix), value)
    } else {
        attrs.push((name.to_string(), value));
        Ok(())
    }
}

/// expat's addBinding: no undeclaring a prefix, `xml` only for its own
/// namespace, `xmlns` never, neither reserved namespace for anything else,
/// and no '}' (ET's separator) in a namespace name.
fn add_binding(binds: &mut Bindings, prefix: Option<&str>, uri: String) -> R<()> {
    if uri.is_empty() && prefix.is_some() {
        return bad();
    }
    if prefix == Some("xmlns") || uri.contains('}') {
        return bad();
    }
    let must_be_xml = prefix == Some("xml");
    if must_be_xml != (uri == XML_NS) || uri == XMLNS_NS {
        return bad();
    }
    binds.push((prefix.map(String::from), uri));
    Ok(())
}

fn lookup(binds: &Bindings, prefix: &str) -> R<String> {
    match binds.iter().rev().find(|(p, _)| p.as_deref() == Some(prefix)) {
        Some((_, uri)) => Ok(uri.clone()),
        None if prefix == "xml" => Ok(XML_NS.to_string()),
        None => bad(), // unbound prefix
    }
}

fn push_text(el: &mut Element, t: &[char]) {
    let dest = match el.children.last_mut() {
        Some(last) => &mut last.tail,
        None => &mut el.text,
    };
    dest.extend(t.iter());
}

// ---- ET.tostring ----

fn escape_cdata(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn escape_attrib(s: &str) -> String {
    escape_cdata(s)
        .replace('"', "&quot;")
        .replace('\r', "&#13;")
        .replace('\n', "&#10;")
        .replace('\t', "&#09;")
}

fn known_prefix(uri: &str) -> Option<&'static str> {
    Some(match uri {
        XML_NS => "xml",
        "http://www.w3.org/1999/xhtml" => "html",
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#" => "rdf",
        "http://schemas.xmlsoap.org/wsdl/" => "wsdl",
        "http://www.w3.org/2001/XMLSchema" => "xs",
        "http://www.w3.org/2001/XMLSchema-instance" => "xsi",
        "http://purl.org/dc/elements/1.1/" => "dc",
        _ => return None,
    })
}

struct Ns {
    /// uri -> prefix, in the order `_namespaces` meets them.
    map: Vec<(String, String)>,
}

impl Ns {
    fn add(&mut self, qname: &str) {
        let Some(rest) = qname.strip_prefix('{') else { return };
        let Some((uri, _)) = rest.rsplit_once('}') else { return };
        if self.map.iter().any(|(u, _)| u == uri) {
            return;
        }
        let prefix = known_prefix(uri).map(String::from).unwrap_or_else(|| format!("ns{}", self.map.len()));
        if prefix != "xml" {
            self.map.push((uri.to_string(), prefix));
        }
    }
    fn qname(&self, q: &str) -> String {
        let Some(rest) = q.strip_prefix('{') else { return q.to_string() };
        let Some((uri, local)) = rest.rsplit_once('}') else { return q.to_string() };
        let prefix = self
            .map
            .iter()
            .find(|(u, _)| u == uri)
            .map(|(_, p)| p.as_str())
            .or_else(|| known_prefix(uri))
            .unwrap_or("");
        if prefix.is_empty() { local.to_string() } else { format!("{prefix}:{local}") }
    }
    /// In document order, as `elem.iter()` walks it.
    fn collect(&mut self, root: &Element) {
        let mut todo = vec![root];
        while let Some(el) = todo.pop() {
            self.add(&el.tag);
            for (k, _) in &el.attrib {
                self.add(k);
            }
            todo.extend(el.children.iter().rev());
        }
    }
}

fn serialize(out: &mut String, el: &Element, ns: &Ns, level: usize) -> Result<(), String> {
    let calls = !el.text.is_empty() || !el.tail.is_empty() || !el.attrib.is_empty();
    if level > SERIALIZE_DEPTH || (calls && level == SERIALIZE_DEPTH) {
        return Err("RecursionError: maximum recursion depth exceeded".to_string());
    }
    let tag = ns.qname(&el.tag);
    out.push('<');
    out.push_str(&tag);
    if level == 1 {
        let mut decls: Vec<&(String, String)> = ns.map.iter().collect();
        decls.sort_by(|a, b| a.1.cmp(&b.1));
        for (uri, prefix) in decls {
            out.push_str(&format!(" xmlns:{prefix}=\"{}\"", escape_attrib(uri)));
        }
    }
    for (k, v) in &el.attrib {
        out.push_str(&format!(" {}=\"{}\"", ns.qname(k), escape_attrib(v)));
    }
    if !el.text.is_empty() || !el.children.is_empty() {
        out.push('>');
        out.push_str(&escape_cdata(&el.text));
        for c in &el.children {
            serialize(out, c, ns, level + 1)?;
        }
        out.push_str(&format!("</{tag}>"));
    } else {
        out.push_str(" />");
    }
    out.push_str(&escape_cdata(&el.tail));
    Ok(())
}

/// `ET.tostring(el, encoding="unicode")` -- tail included, as there. Err is
/// the RecursionError CPython raises for a deeply nested element.
pub fn tostring(el: &Element) -> Result<String, String> {
    let mut ns = Ns { map: Vec::new() };
    ns.collect(el);
    let mut out = String::new();
    serialize(&mut out, el, &ns, 1)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use crate::selfupdate::et;

    #[test]
    fn tostring_matches_elementtree() {
        let doc = et::parse(
            br#"<a xmlns:x="u" xml:lang="de"><x:b x:k="1" z="2">t<c/></x:b>tail<d xmlns="v"><e/></d></a>"#,
        )
        .unwrap();
        let got: Vec<String> = doc.children.iter().map(|c| et::tostring(c).unwrap()).collect();
        assert_eq!(got, vec![
            r#"<ns0:b xmlns:ns0="u" ns0:k="1" z="2">t<c /></ns0:b>tail"#.to_string(),
            r#"<ns0:d xmlns:ns0="v"><ns0:e /></ns0:d>"#.to_string(),
        ]);
        assert!(et::parse(b"<a><b></a>").is_err());
        assert!(et::parse(b"<a>&nbsp;</a>").is_err());
        assert!(et::parse(b"<a/><b/>").is_err());
    }

    #[test]
    fn expat_dtd_and_encodings() {
        let text = |b: &[u8]| et::parse(b).ok().map(|d| (d.text.clone(), d.attrib.clone()));
        assert_eq!(text(b"<!DOCTYPE a [<!ENTITY w \"World\">]><a>Hi &w;</a>").unwrap().0, "Hi World");
        let (_, attrs) = text(b"<!DOCTYPE a [<!ATTLIST a v CDATA \"4.2\" n NMTOKENS #IMPLIED>]><a n=\" x  y \"/>").unwrap();
        assert_eq!(attrs, vec![("n".to_string(), "x y".to_string()), ("v".to_string(), "4.2".to_string())]);
        assert_eq!(text(b"<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?><a>caf\xe9</a>").unwrap().0, "café");
        assert!(matches!(et::parse(b"<?xml version=\"1.0\" encoding=\"bogus\"?><a/>"), Err(et::Error::Raise(_))));
        assert!(et::parse(b"<?xml?><a/>").is_err());
        assert!(et::parse(b"<a xmlns:xml=\"urn:x\"/>").is_err());
        assert_eq!(text(b"<a>&#x00000000000000041;</a>").unwrap().0, "A");
    }
}
