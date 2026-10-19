//! Translations, through GNU gettext.
//!
//! Strings shown to people are wrapped in `_()`; `xgettext` collects them into
//! po/almanac.pot, translators produce po/<language>.po, and the package build
//! compiles those to the system locale directory. A checkout run from source
//! uses ./locale instead, if `po/update.sh` has compiled it there.
//!
//! With no translation for the user's language, `_()` returns the English text
//! unchanged -- nothing here can stop the app from starting.
//!
//! Port of `i18n.py`. Python's `_` is `tr` here (ARCHITECTURE.md). Python
//! does its setup at import time; the Rust program does it in `init`, which
//! the program calls first thing (the first `tr`/`ngettext` does it too).
//!
//! The Python module leans on the standard library's `gettext` module, which
//! does not use the C library at all: it picks the language from the
//! environment (LANGUAGE, LC_ALL, LC_MESSAGES, LANG), reads the .mo file
//! itself and evaluates its Plural-Forms expression itself. The parts of it
//! this module relies on (`gettext.find`, `_expand_lang`, `locale.normalize`,
//! `GNUTranslations`, `c2py`, the `codecs` that decode a catalog) are ported
//! below so the Rust app picks the same catalog, and the same plural form, as
//! the Python app would. Python's integers are unbounded, so plural forms are
//! evaluated on `BigInt`s. The tables Python's codecs and Unicode database
//! carry are generated from Python by `parity/i18n_tables.py` into
//! data/i18n/.
//!
//! A Python `str` can hold lone surrogates (a byte `surrogateescape` kept
//! from the environment, `\ud800` in a `unicode_escape` catalog, a UTF-7 or
//! Punycode one); a Rust `String` cannot. Text that came from outside is kept
//! here as a "pystr" (see `py_push`) until it leaves the module.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use num_bigint::BigInt;

pub const DOMAIN: &str = "almanac";

/// ./locale in a checkout; None means the system's own locale directory.
///
/// The checkout this binary was compiled from has its compiled catalogues in
/// ./locale (po/update.sh), wherever the binary or the target directory are.
/// A packaged build's source tree is gone at run time, so it gets None and
/// uses the system's locale directory.
pub fn localedir() -> Option<String> {
    let checkout = Path::new(env!("CARGO_MANIFEST_DIR"));
    let checkout = std::fs::canonicalize(checkout).unwrap_or_else(|_| checkout.to_path_buf());
    let here = checkout.join("locale");
    if here.is_dir() {
        Some(here.to_string_lossy().into_owned())
    } else {
        None
    }
}

/// `_translation = gettext.translation(DOMAIN, localedir=localedir(), fallback=True)`.
///
/// Like the Python import, a catalog that cannot be read (corrupt .mo file,
/// unknown charset, bad Plural-Forms) is an error, not English.
static TRANSLATION: LazyLock<Result<Translation, String>> = LazyLock::new(|| {
    LazyLock::force(&LC_TIME);
    translation(DOMAIN, localedir().as_deref(), None)
});

/// The Python module's import-time setup: `locale.setlocale(LC_ALL, "")` and
/// `gettext.translation(...)`, which reads the environment once, here. Call
/// it first thing in the program, before anything else can run; an `Err` is
/// what makes `import i18n` raise in Python (the app does not start). Safe to
/// call more than once.
///
/// `setlocale` itself cannot be called from here: its only Rust binding
/// (gettext-rs's) is an `unsafe fn`, and this crate has no `unsafe`. The
/// windowed app gets the same call from `gtk::init`. What Python's call
/// changes that the app shows is LC_TIME's day and month names in
/// `strftime`; those are read here, as setlocale would make the C library
/// read them, into `lc_time`, for code that formats dates without GTK
/// (`--guard`). Which translation is used does not depend on it: like
/// Python's `gettext`, the lookup below reads the environment.
pub fn init() -> Result<(), String> {
    LazyLock::force(&LC_TIME);
    match &*TRANSLATION {
        Ok(_) => Ok(()),
        Err(e) => Err(e.clone()),
    }
}

fn the_translation() -> &'static Translation {
    match &*TRANSLATION {
        Ok(t) => t,
        // Python: the exception escapes `import i18n`; nothing is translated
        // because nothing runs.
        Err(e) => panic!("i18n: {e}"),
    }
}

/// `_(text)` that reports, rather than raises, Python's exceptions: a catalog
/// that failed to load, or a Plural-Forms expression that fails to evaluate
/// (division by zero).
pub fn try_tr(text: &str) -> Result<String, String> {
    TRANSLATION.as_ref().map_err(|e| e.clone())?.gettext(text)
}

/// `ngettext(singular, plural, n)` that reports, rather than raises, Python's
/// exceptions (see `try_tr`).
#[track_caller]
pub fn try_ngettext(singular: &str, plural: &str, n: impl Into<PluralN>) -> Result<String, String> {
    let caller = std::panic::Location::caller();
    let n = n.into();
    let t = TRANSLATION.as_ref().map_err(|e| e.clone())?;
    let mut warned = false;
    let r = t.ngettext_n(singular, plural, &n, &mut warned);
    if warned {
        deprecation_warning(caller, n.type_name());
    }
    r
}

/// `_(text)`: the translation of `text`, or `text` itself when there is none.
///
/// Where Python's `_()` raises (see `try_tr`), this panics: the exception is
/// not caught in Python either.
pub fn tr(text: &str) -> String {
    try_tr(text).unwrap_or_else(|e| panic!("i18n: {e}"))
}

/// `ngettext(singular, plural, n)`. `n` is any integer, as in Python
/// (negative counts included), or a float, which Python also takes (with a
/// DeprecationWarning). Panics where Python raises, like `tr`.
#[track_caller]
pub fn ngettext(singular: &str, plural: &str, n: impl Into<PluralN>) -> String {
    let caller = std::panic::Location::caller();
    let n = n.into();
    let mut warned = false;
    let r = the_translation().ngettext_n(singular, plural, &n, &mut warned);
    if warned {
        deprecation_warning(caller, n.type_name());
    }
    r.unwrap_or_else(|e| panic!("i18n: {e}"))
}

/// The `n` of `ngettext`: Python takes any object. An int (bool included)
/// is the count; a float is evaluated as a float; anything else (by its
/// type name; only the parity hook makes one) fails where Python's
/// `_as_int` raises TypeError.
#[derive(Debug, Clone, PartialEq)]
pub enum PluralN {
    Int(BigInt),
    Float(f64),
    Other(String),
}

impl PluralN {
    fn type_name(&self) -> &str {
        match self {
            PluralN::Int(_) => "int",
            PluralN::Float(_) => "float",
            PluralN::Other(t) => t,
        }
    }

    /// `n == 1`.
    fn is_one(&self) -> bool {
        match self {
            PluralN::Int(i) => *i == BigInt::from(1),
            PluralN::Float(f) => *f == 1.0,
            PluralN::Other(_) => false,
        }
    }
}

macro_rules! plural_n_from_int {
    ($($t:ty),*) => {$(
        impl From<$t> for PluralN {
            fn from(n: $t) -> PluralN {
                PluralN::Int(BigInt::from(n))
            }
        }
    )*};
}
plural_n_from_int!(i8, i16, i32, i64, i128, isize, u8, u16, u32, u64, u128, usize);

impl From<bool> for PluralN {
    fn from(n: bool) -> PluralN {
        PluralN::Int(BigInt::from(n as u8))
    }
}
impl From<BigInt> for PluralN {
    fn from(n: BigInt) -> PluralN {
        PluralN::Int(n)
    }
}
impl From<&BigInt> for PluralN {
    fn from(n: &BigInt) -> PluralN {
        PluralN::Int(n.clone())
    }
}
impl From<f64> for PluralN {
    fn from(n: f64) -> PluralN {
        PluralN::Float(n)
    }
}
impl From<f32> for PluralN {
    fn from(n: f32) -> PluralN {
        PluralN::Float(n as f64)
    }
}

/// `warnings.warn('Plural value must be an integer, got %s', DeprecationWarning)`
/// from gettext's `_as_int2`, attributed to the caller of `ngettext`. Python
/// shows a DeprecationWarning only when that caller is `__main__` (app.py,
/// whose port is src/ui/), once per location.
fn deprecation_warning(caller: &'static std::panic::Location<'static>, type_name: &str) {
    static SHOWN: LazyLock<Mutex<HashSet<(&'static str, u32)>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
    let file = caller.file();
    if !(file.starts_with("src/ui/") || file.contains("/src/ui/")) {
        return;
    }
    if let Ok(mut shown) = SHOWN.lock() {
        if shown.insert((file, caller.line())) {
            eprintln!("{file}:{}: DeprecationWarning: Plural value must be an integer, got {type_name}", caller.line());
        }
    }
}

// ---------------------------------------------------------------------------
// LC_TIME, as `locale.setlocale(locale.LC_ALL, "")` leaves it.

/// The day and month names the C library's `strftime` uses (`%a`, `%A`,
/// `%b`, `%B`) once the program has called `setlocale(LC_ALL, "")`, as
/// Python's i18n does at import. Read through `locale(1)`, which makes that
/// same call and prints them in the locale's charset; a locale the C library
/// does not support (or no `locale` program) leaves the C locale's English
/// names, as a failed setlocale does: an unsupported locale is still English.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LcTime {
    /// Sunday first.
    pub abday: Vec<String>,
    pub day: Vec<String>,
    /// January first.
    pub abmon: Vec<String>,
    pub mon: Vec<String>,
}

impl LcTime {
    /// The "C" locale's names.
    pub fn c() -> LcTime {
        let v = |s: &str| s.split(';').map(String::from).collect::<Vec<_>>();
        LcTime {
            abday: v("Sun;Mon;Tue;Wed;Thu;Fri;Sat"),
            day: v("Sunday;Monday;Tuesday;Wednesday;Thursday;Friday;Saturday"),
            abmon: v("Jan;Feb;Mar;Apr;May;Jun;Jul;Aug;Sep;Oct;Nov;Dec"),
            mon: v("January;February;March;April;May;June;July;August;September;October;November;December"),
        }
    }
}

static LC_TIME: LazyLock<LcTime> = LazyLock::new(read_lc_time);

/// LC_TIME's names for this process (see `LcTime`).
pub fn lc_time() -> &'static LcTime {
    &LC_TIME
}

fn read_lc_time() -> LcTime {
    let c = LcTime::c();
    let Ok(out) = std::process::Command::new("locale")
        .args(["charmap", "abday", "day", "abmon", "mon"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
    else {
        return c;
    };
    let lines: Vec<&[u8]> = out.stdout.split(|&b| b == b'\n').collect();
    // Python decodes strftime's bytes from the locale's charset
    let charmap = String::from_utf8_lossy(lines.first().copied().unwrap_or_default()).into_owned();
    let field = |i: usize, n: usize| -> Option<Vec<String>> {
        let text = decode(lines.get(i)?, &charmap).map(|s| py_lossy(&s)).ok()?;
        let v: Vec<String> = text.split(';').map(String::from).collect();
        (v.len() == n).then_some(v)
    };
    match (field(1, 7), field(2, 7), field(3, 12), field(4, 12)) {
        (Some(abday), Some(day), Some(abmon), Some(mon)) => LcTime { abday, day, abmon, mon },
        _ => c,
    }
}

// ---------------------------------------------------------------------------
// Python strings. A lone surrogate s is kept in a String as the character
// PY_SUR + (s - 0xD800), U+10F800..U+10FFFF; a real character from PY_LIT
// up is kept as PY_LIT followed by itself. Every character of that end of
// the private use plane is, like a surrogate to Python, not alphanumeric,
// not cased, not case-ignorable, and its own lower case, so string code
// reads a pystr the way Python reads the str.

const PY_LIT: char = '\u{10F7FF}';
const PY_SUR: u32 = 0x10F800;

/// Appends one Python code point (a lone surrogate included) to a pystr.
fn py_push(out: &mut String, cp: u32) {
    if (0xD800..=0xDFFF).contains(&cp) {
        out.extend(char::from_u32(PY_SUR + (cp - 0xD800)));
    } else if let Some(c) = char::from_u32(cp) {
        if c >= PY_LIT {
            out.push(PY_LIT);
        }
        out.push(c);
    }
}

fn py_push_str(out: &mut String, s: &str) {
    for c in s.chars() {
        py_push(out, c as u32);
    }
}

/// The pystr of a Rust string.
fn py_from_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    py_push_str(&mut out, s);
    out
}

/// The code points of a pystr, lone surrogates included.
fn py_codepoints(s: &str) -> Vec<u32> {
    let mut out = Vec::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == PY_LIT {
            out.extend(it.next().map(|n| n as u32));
        } else if c as u32 >= PY_SUR {
            out.push(c as u32 - PY_SUR + 0xD800);
        } else {
            out.push(c as u32);
        }
    }
    out
}

fn py_from_codepoints(cps: &[u32]) -> String {
    let mut out = String::with_capacity(cps.len());
    for &c in cps {
        py_push(&mut out, c);
    }
    out
}

/// A pystr as Rust text: a lone surrogate, which no Rust string (and no GTK
/// label: Python raises there) can hold, becomes U+FFFD.
fn py_lossy(s: &str) -> String {
    py_codepoints(s).into_iter().map(|u| char::from_u32(u).unwrap_or('\u{fffd}')).collect()
}

#[cfg(test)]
/// `ascii(s)`.
fn py_ascii(s: &str) -> String {
    let cps = py_codepoints(s);
    let quote = if cps.contains(&('\'' as u32)) && !cps.contains(&('"' as u32)) { '"' } else { '\'' };
    let mut out = String::new();
    out.push(quote);
    for u in cps {
        match u {
            _ if u == quote as u32 || u == '\\' as u32 => {
                out.push('\\');
                out.extend(char::from_u32(u));
            }
            0x09 => out.push_str("\\t"),
            0x0a => out.push_str("\\n"),
            0x0d => out.push_str("\\r"),
            0x20..=0x7e => out.extend(char::from_u32(u)),
            0..=0xff => out.push_str(&format!("\\x{u:02x}")),
            0x100..=0xffff => out.push_str(&format!("\\u{u:04x}")),
            _ => out.push_str(&format!("\\U{u:08x}")),
        }
    }
    out.push(quote);
    out
}

fn in_ranges(ranges: &[(u32, u32)], u: u32) -> bool {
    ranges
        .binary_search_by(|&(lo, hi)| {
            if hi < u {
                std::cmp::Ordering::Less
            } else if lo > u {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// `str.lower()` of a pystr (Unicode 16.0, as CPython 3.14 has it): each
/// character's full lower case, and 'Σ' as 'ς' where it ends a word (a
/// cased letter before it, none after, case-ignorable ones skipped).
fn py_lower(s: &str) -> String {
    let cps = py_codepoints(s);
    let cased = |u: u32| in_ranges(PY_CASED, u);
    let ignorable = |u: u32| in_ranges(PY_CASE_IGNORABLE, u);
    let mut out = String::with_capacity(s.len());
    for (i, &u) in cps.iter().enumerate() {
        if u == 0x3A3 {
            let before = cps[..i].iter().rev().find(|&&c| !ignorable(c));
            let mut final_sigma = before.is_some_and(|&c| cased(c));
            if final_sigma {
                let after = cps[i + 1..].iter().find(|&&c| !ignorable(c));
                final_sigma = after.is_none_or(|&c| !cased(c));
            }
            py_push(&mut out, if final_sigma { 0x3C2 } else { 0x3C3 });
            continue;
        }
        match PY_LOWER.binary_search_by_key(&u, |&(c, _)| c) {
            Ok(k) => py_push_str(&mut out, PY_LOWER[k].1),
            Err(_) => py_push(&mut out, u),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The standard library's gettext, as far as `gettext.translation(...,
// fallback=True)`, `.gettext` and `.ngettext` go.

/// Where translations are installed: the packages put Almanac's catalogue in
/// `/usr/share/locale/<lang>/LC_MESSAGES/almanac.mo`.
fn default_localedir() -> &'static str {
    "/usr/share/locale"
}


/// A chain of catalogs, first match wins: `GNUTranslations` with its
/// fallbacks; empty is `NullTranslations`.
#[derive(Debug, Clone, Default)]
pub struct Translation {
    chain: Vec<Arc<GnuCatalog>>,
}

/// A catalog key; the strings are pystrs.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Key {
    Msg(String),
    Plural(String, i128),
}

#[derive(Debug, Default)]
struct GnuCatalog {
    /// Keys and values are pystrs.
    catalog: HashMap<Key, String>,
    /// None is the germanic plural, `int(n != 1)`.
    plural: Option<Expr>,
}

impl GnuCatalog {
    /// `self.plural(n)`; `warned` is set where gettext's `_as_int2` warns.
    fn plural(&self, n: &PluralN, warned: &mut bool) -> Result<BigInt, String> {
        match &self.plural {
            // `lambda n: int(n != 1)`: anything that is not 1 is plural
            None => Ok(BigInt::from(!n.is_one() as i32)),
            Some(e) => e.call(n, warned),
        }
    }

    /// `self._catalog.get((msgid, index))`: the catalog's indices are small,
    /// so an index outside i128 is simply not there.
    fn get_plural(&self, msgid: &str, index: &BigInt) -> Option<&String> {
        let i = i128::try_from(index).ok()?;
        self.catalog.get(&Key::Plural(msgid.to_string(), i))
    }

    /// `GNUTranslations._parse`.
    fn parse(buf: &[u8], filename: &str) -> Result<GnuCatalog, String> {
        const LE_MAGIC: u32 = 0x950412de;
        const BE_MAGIC: u32 = 0xde120495;
        let buflen = buf.len();
        let word = |at: usize, le: bool| -> Result<u32, String> {
            let b: [u8; 4] = buf
                .get(at..at + 4)
                .and_then(|s| s.try_into().ok())
                .ok_or_else(|| "unpack requires a buffer of 4 bytes".to_string())?;
            Ok(if le { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) })
        };
        let mut cat = GnuCatalog::default();
        let mut charset: Option<String> = None;
        // Are we big endian or little endian?
        let magic = word(0, true)?;
        let le = if magic == LE_MAGIC {
            true
        } else if magic == BE_MAGIC {
            false
        } else {
            return Err(format!("[Errno 0] Bad magic number: '{filename}'"));
        };
        if buflen < 20 {
            return Err("unpack requires a buffer of 16 bytes".into());
        }
        let version = word(4, le)?;
        let msgcount = word(8, le)?;
        let mut masteridx = word(12, le)? as usize;
        let mut transidx = word(16, le)? as usize;
        let major_version = version >> 16;
        if major_version > 1 {
            return Err(format!("[Errno 0] Bad version number {major_version}: '{filename}'"));
        }
        // Now put all messages from the .mo file buffer into the catalog
        // dictionary.
        for _ in 0..msgcount {
            let pair = |at: usize| -> Result<(usize, usize), String> {
                if buf.len() < at.saturating_add(8) {
                    return Err("unpack requires a buffer of 8 bytes".into());
                }
                Ok((word(at, le)? as usize, word(at + 4, le)? as usize))
            };
            let (mlen, moff) = pair(masteridx)?;
            let mend = moff + mlen;
            let (tlen, toff) = pair(transidx)?;
            let tend = toff + tlen;
            if !(mend < buflen && tend < buflen) {
                return Err(format!("[Errno 0] File is corrupt: '{filename}'"));
            }
            let msg = &buf[moff..mend];
            let tmsg = &buf[toff..tend];
            // See if we're looking at GNU .mo conventions for metadata
            if mlen == 0 {
                // Catalog description
                let mut lastk: Option<String> = None;
                for b_item in tmsg.split(|&b| b == b'\n') {
                    let item = py_strip(
                        std::str::from_utf8(b_item).map_err(|e| format!("'utf-8' codec can't decode: {e}"))?,
                    );
                    if item.is_empty() {
                        continue;
                    }
                    // Skip over comment lines:
                    if item.starts_with("#-#-#-#-#") && item.ends_with("#-#-#-#-#") {
                        continue;
                    }
                    let mut kv: Option<(String, String)> = None;
                    if let Some((k, v)) = item.split_once(':') {
                        let k = py_lossy(&py_lower(&py_from_str(py_strip(k))));
                        let v = py_strip(v).to_string();
                        lastk = Some(k.clone());
                        kv = Some((k, v));
                    } else if lastk.is_some() {
                        // self._info[lastk] += '\n' + item: metadata nobody reads here
                    }
                    let Some((k, v)) = kv else { continue };
                    if k == "content-type" {
                        let cs = v.split("charset=").nth(1).ok_or("list index out of range")?;
                        charset = Some(cs.to_string());
                    } else if k == "plural-forms" {
                        let v: Vec<&str> = v.split(';').collect();
                        let plural = v
                            .get(1)
                            .and_then(|p| p.split("plural=").nth(1))
                            .ok_or("list index out of range")?;
                        cat.plural = Some(c2py(plural)?);
                    }
                }
            }
            // Note: we unconditionally convert both msgids and msgstrs to
            // Unicode using the character encoding specified in the charset
            // parameter of the Content-Type header.  The gettext documentation
            // strongly encourages msgids to be us-ascii, but some applications
            // require alternative encodings (e.g. Zope's ZCML and ZPT).  For
            // traditional gettext applications, the msgid conversion will
            // cause no problems since us-ascii should always be a subset of
            // the charset encoding.  We may want to fall back to 8-bit msgids
            // if the Unicode conversion fails.
            let cs = match charset.as_deref() {
                Some(c) if !c.is_empty() => c,
                _ => "ascii",
            };
            if msg.contains(&0) {
                // Plural forms
                let parts: Vec<&[u8]> = msg.split(|&b| b == 0).collect();
                if parts.len() != 2 {
                    return Err(format!(
                        "too many values to unpack (expected 2, got {})",
                        parts.len()
                    ));
                }
                let msgid1 = decode(parts[0], cs)?;
                for (i, x) in tmsg.split(|&b| b == 0).enumerate() {
                    cat.catalog.insert(Key::Plural(msgid1.clone(), i as i128), decode(x, cs)?);
                }
            } else {
                cat.catalog.insert(Key::Msg(decode(msg, cs)?), decode(tmsg, cs)?);
            }
            // advance to next entry in the seek tables
            masteridx += 8;
            transidx += 8;
        }
        Ok(cat)
    }
}

/// `str.strip()`: Python's whitespace is Unicode White_Space plus the
/// information separators U+001C..U+001F.
fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}

/// `codecs.lookup(charset).name` as far as finding the codec module goes:
/// the C-level `_Py_normalize_encoding` (ASCII letters and digits and '.'
/// kept, lower-cased; any run of other characters becomes one '_'), then
/// `encodings.search_function` (`encodings.aliases`, else the name itself;
/// a module name with a '.' in it is never imported).
fn codec_module(charset: &str) -> Option<String> {
    let mut lower = String::new();
    let mut punct = false;
    for c in charset.chars() {
        if c.is_ascii_alphanumeric() || c == '.' {
            if punct && !lower.is_empty() {
                lower.push('_');
            }
            punct = false;
            lower.push(c.to_ascii_lowercase());
        } else {
            punct = true;
        }
    }
    let norm = normalize_encoding(&lower);
    let aliases = &*ENCODING_ALIASES_MAP;
    let aliased = aliases.get(norm.as_str()).or_else(|| aliases.get(norm.replace('.', "_").as_str()));
    let mut modnames: Vec<String> = Vec::new();
    if let Some(a) = aliased {
        modnames.push(a.to_string());
    }
    modnames.push(norm);
    // The first module that imports is the codec (or, for a module that is
    // not a codec, no codec at all); mbcs and oem only import on Windows.
    modnames.into_iter().find(|m| !m.is_empty() && !m.contains('.') && ENCODING_MODULES.contains(&m.as_str()) && m != "mbcs" && m != "oem")
}

/// The modules of Python's `encodings` package (CPython 3.14).
const ENCODING_MODULES: &[&str] = &[
    "aliases", "ascii", "base64_codec", "big5", "big5hkscs", "bz2_codec", "charmap", "cp037",
    "cp1006", "cp1026", "cp1125", "cp1140", "cp1250", "cp1251", "cp1252", "cp1253", "cp1254",
    "cp1255", "cp1256", "cp1257", "cp1258", "cp273", "cp424", "cp437", "cp500", "cp720", "cp737",
    "cp775", "cp850", "cp852", "cp855", "cp856", "cp857", "cp858", "cp860", "cp861", "cp862",
    "cp863", "cp864", "cp865", "cp866", "cp869", "cp874", "cp875", "cp932", "cp949", "cp950",
    "euc_jis_2004", "euc_jisx0213", "euc_jp", "euc_kr", "gb18030", "gb2312", "gbk", "hex_codec",
    "hp_roman8", "hz", "idna", "iso2022_jp", "iso2022_jp_1", "iso2022_jp_2", "iso2022_jp_2004",
    "iso2022_jp_3", "iso2022_jp_ext", "iso2022_kr", "iso8859_1", "iso8859_10", "iso8859_11",
    "iso8859_13", "iso8859_14", "iso8859_15", "iso8859_16", "iso8859_2", "iso8859_3", "iso8859_4",
    "iso8859_5", "iso8859_6", "iso8859_7", "iso8859_8", "iso8859_9", "johab", "koi8_r", "koi8_t",
    "koi8_u", "kz1048", "latin_1", "mac_arabic", "mac_croatian", "mac_cyrillic", "mac_farsi",
    "mac_greek", "mac_iceland", "mac_latin2", "mac_roman", "mac_romanian", "mac_turkish", "mbcs",
    "oem", "palmos", "ptcp154", "punycode", "quopri_codec", "raw_unicode_escape", "rot_13",
    "shift_jis", "shift_jis_2004", "shift_jisx0213", "tis_620", "undefined", "unicode_escape",
    "utf_16", "utf_16_be", "utf_16_le", "utf_32", "utf_32_be", "utf_32_le", "utf_7", "utf_8",
    "utf_8_sig", "uu_codec", "zlib_codec",
];

/// The stateless multibyte codecs (CJKCodecs), decoded by table.
const MULTIBYTE_CODECS: &[&str] = &[
    "gb2312", "gbk", "gb18030", "big5", "cp950", "big5hkscs", "cp932", "shift_jis", "euc_jp",
    "euc_jis_2004", "euc_jisx0213", "shift_jis_2004", "shift_jisx0213", "euc_kr", "cp949", "johab",
];

static CHARMAPS_MAP: LazyLock<HashMap<&'static str, Vec<char>>> =
    LazyLock::new(|| CHARMAPS.iter().map(|(n, t)| (*n, t.chars().collect())).collect());

/// `str(bytes, charset)`, strict errors, as Python's codecs decode: the
/// text as a pystr.
fn decode(b: &[u8], charset: &str) -> Result<String, String> {
    if charset.contains('\0') {
        return Err("embedded null character".into());
    }
    let unknown = || format!("unknown encoding: {charset}");
    let cant = |codec: &str| format!("'{codec}' codec can't decode bytes");
    let module = codec_module(charset).ok_or_else(unknown)?;
    let utf16 = |b: &[u8], le: bool| -> Result<String, String> {
        if b.len() % 2 != 0 {
            return Err(cant("utf-16"));
        }
        let units = b.chunks(2).map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) });
        char::decode_utf16(units).collect::<Result<String, _>>().map_err(|_| cant("utf-16"))
    };
    let utf32 = |b: &[u8], le: bool| -> Result<String, String> {
        if b.len() % 4 != 0 {
            return Err(cant("utf-32"));
        }
        b.chunks(4)
            .map(|c| {
                let w = [c[0], c[1], c[2], c[3]];
                let u = if le { u32::from_le_bytes(w) } else { u32::from_be_bytes(w) };
                char::from_u32(u).ok_or_else(|| cant("utf-32"))
            })
            .collect()
    };
    let mut out = String::with_capacity(b.len());
    let text: String = match module.as_str() {
        "utf_8" => String::from_utf8(b.to_vec()).map_err(|_| cant("utf-8"))?,
        "utf_8_sig" => {
            let b = b.strip_prefix(b"\xef\xbb\xbf").unwrap_or(b);
            String::from_utf8(b.to_vec()).map_err(|_| cant("utf-8"))?
        }
        "ascii" => {
            if b.is_ascii() {
                String::from_utf8_lossy(b).into_owned()
            } else {
                return Err("'ascii' codec can't decode byte".into());
            }
        }
        // `charmap` with no mapping is Latin-1
        "latin_1" | "charmap" => b.iter().map(|&c| c as char).collect(),
        // Without a byte order mark, the machine's own order: little-endian
        // on every machine this runs on.
        "utf_16" => match b {
            [0xff, 0xfe, rest @ ..] => utf16(rest, true)?,
            [0xfe, 0xff, rest @ ..] => utf16(rest, false)?,
            _ => utf16(b, cfg!(target_endian = "little"))?,
        },
        "utf_16_le" => utf16(b, true)?,
        "utf_16_be" => utf16(b, false)?,
        "utf_32" => match b {
            [0xff, 0xfe, 0, 0, rest @ ..] => utf32(rest, true)?,
            [0, 0, 0xfe, 0xff, rest @ ..] => utf32(rest, false)?,
            _ => utf32(b, cfg!(target_endian = "little"))?,
        },
        "utf_32_le" => utf32(b, true)?,
        "utf_32_be" => utf32(b, false)?,
        // The codecs below may give lone surrogates, or are long: they write
        // the pystr themselves.
        "unicode_escape" => {
            unicode_escape(b, &mut out)?;
            return Ok(out);
        }
        "raw_unicode_escape" => {
            raw_unicode_escape(b, &mut out)?;
            return Ok(out);
        }
        "utf_7" => {
            utf7_decode(b, &mut out)?;
            return Ok(out);
        }
        "punycode" => {
            let cps = punycode_decode(b).ok_or_else(|| cant("punycode"))?;
            return Ok(py_from_codepoints(&cps));
        }
        "idna" => {
            idna_decode(b, &mut out).ok_or_else(|| cant("idna"))?;
            return Ok(out);
        }
        "hz" => {
            hz_decode(b, &mut out).ok_or_else(|| cant("hz"))?;
            return Ok(out);
        }
        m if m.starts_with("iso2022_") => {
            iso2022_decode(m, b, &mut out).ok_or_else(|| cant(m))??;
            return Ok(out);
        }
        m if MULTIBYTE_CODECS.contains(&m) => {
            multibyte_decode(m, b, &mut out)?;
            return Ok(out);
        }
        m => match CHARMAPS_MAP.get(m) {
            // A charmap codec: one character per byte, U+FFFE where the
            // byte is undefined. A 128-entry table is the upper half, below
            // which the codec is ASCII.
            Some(table) => b
                .iter()
                .map(|&c| {
                    let ch = if table.len() == 128 {
                        if c < 0x80 { c as char } else { table[c as usize - 0x80] }
                    } else {
                        table[c as usize]
                    };
                    if ch == '\u{fffe}' { Err(format!("'charmap' codec can't decode byte {c:#04x}")) } else { Ok(ch) }
                })
                .collect::<Result<String, String>>()?,
            // Not a text codec (base64_codec, ...), `undefined`, or the
            // Windows-only mbcs and oem.
            None => return Err(unknown()),
        },
    };
    py_push_str(&mut out, &text);
    Ok(out)
}

/// The `unicode_escape` codec: Python string-literal escapes over Latin-1.
/// `\N{name}` looks the name up in the Unicode database.
fn unicode_escape(b: &[u8], out: &mut String) -> Result<(), String> {
    let bad = |msg: &str| format!("'unicodeescape' codec can't decode bytes: {msg}");
    let mut s = 0;
    while s < b.len() {
        let c = b[s];
        s += 1;
        // Non-escape characters are interpreted as Unicode ordinals
        if c != b'\\' {
            py_push(out, c as u32);
            continue;
        }
        // \ - Escapes
        let Some(&c) = b.get(s) else {
            return Err(bad("\\ at end of string"));
        };
        s += 1;
        let count = match c {
            b'\n' => continue,
            b'\\' | b'\'' | b'"' => {
                py_push(out, c as u32);
                continue;
            }
            b'b' => {
                py_push(out, 0x08);
                continue;
            }
            b'f' => {
                py_push(out, 0x0c);
                continue;
            }
            b't' => {
                py_push(out, 0x09);
                continue;
            }
            b'n' => {
                py_push(out, 0x0a);
                continue;
            }
            b'r' => {
                py_push(out, 0x0d);
                continue;
            }
            b'v' => {
                py_push(out, 0x0b);
                continue;
            }
            b'a' => {
                py_push(out, 0x07);
                continue;
            }
            // \OOO (octal) escapes
            b'0'..=b'7' => {
                let mut ch = (c - b'0') as u32;
                for _ in 0..2 {
                    match b.get(s) {
                        Some(&d @ b'0'..=b'7') => {
                            ch = (ch << 3) + (d - b'0') as u32;
                            s += 1;
                        }
                        _ => break,
                    }
                }
                py_push(out, ch);
                continue;
            }
            // hex escapes: \xXX, \uXXXX, \UXXXXXXXX
            b'x' => 2,
            b'u' => 4,
            b'U' => 8,
            // \N{name}
            b'N' => {
                if b.get(s) != Some(&b'{') {
                    return Err(bad("malformed \\N character escape"));
                }
                s += 1;
                let start = s;
                // look for the closing brace
                while s < b.len() && b[s] != b'}' {
                    s += 1;
                }
                if s >= b.len() || s == start {
                    return Err(bad("malformed \\N character escape"));
                }
                let name = &b[start..s];
                s += 1;
                match unicode_lookup(name) {
                    Some(ch) => py_push(out, ch),
                    None => return Err(bad("unknown Unicode character name")),
                }
                continue;
            }
            // an unknown escape stays as it is
            other => {
                py_push(out, '\\' as u32);
                py_push(out, other as u32);
                continue;
            }
        };
        let mut ch: u32 = 0;
        for _ in 0..count {
            let d = *b.get(s).ok_or_else(|| bad("truncated escape"))?;
            ch = (ch << 4) + (d as char).to_digit(16).ok_or_else(|| bad("truncated escape"))?;
            s += 1;
        }
        if ch > 0x10FFFF {
            return Err(bad("illegal Unicode character"));
        }
        py_push(out, ch);
    }
    Ok(())
}

/// The `raw_unicode_escape` codec: Latin-1, but `\uXXXX` and `\UXXXXXXXX`.
fn raw_unicode_escape(b: &[u8], out: &mut String) -> Result<(), String> {
    let bad = |msg: &str| format!("'rawunicodeescape' codec can't decode bytes: {msg}");
    let mut s = 0;
    while s < b.len() {
        let c = b[s];
        s += 1;
        // Non-escape characters are interpreted as Unicode ordinals
        if c != b'\\' || s >= b.len() {
            py_push(out, c as u32);
            continue;
        }
        let c = b[s];
        s += 1;
        let count = match c {
            b'u' => 4,
            b'U' => 8,
            _ => {
                py_push(out, '\\' as u32);
                py_push(out, c as u32);
                continue;
            }
        };
        // \uHHHH with 4 hex digits, \U00HHHHHH with 8
        let mut ch: u32 = 0;
        for _ in 0..count {
            let d = *b.get(s).ok_or_else(|| bad("truncated escape"))?;
            ch = (ch << 4) + (d as char).to_digit(16).ok_or_else(|| bad("truncated escape"))?;
            s += 1;
        }
        if ch > 0x10FFFF {
            return Err(bad("\\Uxxxxxxxx out of range"));
        }
        py_push(out, ch);
    }
    Ok(())
}

/// `PyUnicode_DecodeUTF7`, strict.
fn utf7_decode(b: &[u8], out: &mut String) -> Result<(), String> {
    let bad = |msg: &str| format!("'utf7' codec can't decode bytes: {msg}");
    let is_base64 = |c: u8| c.is_ascii_alphanumeric() || c == b'+' || c == b'/';
    let from_base64 = |c: u8| -> u64 {
        match c {
            b'A'..=b'Z' => (c - b'A') as u64,
            b'a'..=b'z' => (c - b'a' + 26) as u64,
            b'0'..=b'9' => (c - b'0' + 52) as u64,
            b'+' => 62,
            _ => 63,
        }
    };
    // this byte encountered in a UTF-7 string decodes as itself
    let decode_direct = |c: u8| c <= 127 && c != b'+';
    let mut in_shift = false;
    let mut base64bits: u32 = 0;
    let mut base64buffer: u64 = 0;
    let mut surrogate: u32 = 0;
    let mut s = 0;
    while s < b.len() {
        let ch = b[s];
        if in_shift {
            // in a base-64 section
            if is_base64(ch) {
                // consume a base-64 character
                base64buffer = (base64buffer << 6) | from_base64(ch);
                base64bits += 6;
                s += 1;
                if base64bits >= 16 {
                    // we have enough bits for a UTF-16 value
                    let out_ch = (base64buffer >> (base64bits - 16)) as u32;
                    base64bits -= 16;
                    base64buffer &= (1 << base64bits) - 1; // clear high bits
                    if surrogate != 0 {
                        // expecting a second surrogate
                        if (0xDC00..=0xDFFF).contains(&out_ch) {
                            py_push(out, 0x10000 + ((surrogate - 0xD800) << 10) + (out_ch - 0xDC00));
                            surrogate = 0;
                            continue;
                        }
                        py_push(out, surrogate);
                        surrogate = 0;
                    }
                    if (0xD800..=0xDBFF).contains(&out_ch) {
                        // first surrogate
                        surrogate = out_ch;
                    } else {
                        py_push(out, out_ch);
                    }
                }
            } else {
                // now leaving a base-64 section
                in_shift = false;
                if base64bits > 0 {
                    // left-over bits
                    if base64bits >= 6 {
                        // We've seen at least one base-64 character
                        return Err(bad("partial character in shift sequence"));
                    }
                    // Some bits remain; they should be zero
                    if base64buffer != 0 {
                        return Err(bad("non-zero padding bits in shift sequence"));
                    }
                }
                if surrogate != 0 && decode_direct(ch) {
                    py_push(out, surrogate);
                }
                surrogate = 0;
                if ch == b'-' {
                    // '-' is absorbed; other terminating characters are
                    // preserved
                    s += 1;
                }
            }
        } else if ch == b'+' {
            s += 1; // consume '+'
            if b.get(s) == Some(&b'-') {
                // '+-' encodes '+'
                s += 1;
                py_push(out, '+' as u32);
            } else if b.get(s).is_some_and(|&c| !is_base64(c)) {
                return Err(bad("ill-formed sequence"));
            } else {
                // begin base64-encoded section
                in_shift = true;
                surrogate = 0;
                base64bits = 0;
                base64buffer = 0;
            }
        } else if decode_direct(ch) {
            // character decodes as itself
            s += 1;
            py_push(out, ch as u32);
        } else {
            return Err(bad("unexpected special character"));
        }
    }
    // end of string: in a shift sequence, no more to follow; an
    // inconsistent state is an error
    if in_shift && (surrogate != 0 || base64bits >= 6 || (base64bits > 0 && base64buffer != 0)) {
        return Err(bad("unterminated shift sequence"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// CJKCodecs: the multibyte East Asian codecs. Their mappings, as Python's
// own decoders give them, are tables in data/i18n/codecs.txt (one line per
// table, `name json`): for each byte prefix, what each next byte is, a
// character or a lead into a longer sequence, the rest illegal.

static CODECS_DATA: &str = include_str!("../data/i18n/codecs.txt");

fn codec_line(name: &str) -> Option<&'static str> {
    CODECS_DATA.lines().find_map(|l| l.strip_prefix(name)?.strip_prefix(' '))
}

const T_ILLEGAL: u32 = u32::MAX;
const T_LEAD: u32 = u32::MAX - 1;
const T_MULTI: u32 = u32::MAX - 2;

/// One codec's (or character set's) decoding tables.
struct Trie {
    /// For each prefix, the 256 next bytes: a code point, T_LEAD, T_MULTI
    /// (a sequence of another length than one character), or T_ILLEGAL.
    tables: HashMap<Vec<u8>, Vec<u32>>,
    multi: HashMap<Vec<u8>, String>,
}

impl Trie {
    fn load(name: &str) -> Option<Trie> {
        #[derive(serde::Deserialize)]
        struct Raw {
            t: HashMap<String, Vec<(usize, String)>>,
            m: HashMap<String, String>,
        }
        let raw: Raw = serde_json::from_str(codec_line(name)?).ok()?;
        let mut tables: HashMap<Vec<u8>, Vec<u32>> = HashMap::new();
        for (prefix, runs) in raw.t {
            let mut entries = vec![T_ILLEGAL; 256];
            for (start, chars) in runs {
                for (j, c) in chars.chars().enumerate() {
                    *entries.get_mut(start + j)? = c as u32;
                }
            }
            tables.insert(hex::decode(prefix).ok()?, entries);
        }
        let mut multi = HashMap::new();
        for (seq, value) in raw.m {
            let seq = hex::decode(seq).ok()?;
            let (&last, prefix) = seq.split_last()?;
            tables.get_mut(prefix)?[last as usize] = T_MULTI;
            multi.insert(seq, value);
        }
        // a prefix with a table of its own is a lead
        let leads: Vec<Vec<u8>> = tables.keys().filter(|p| !p.is_empty()).cloned().collect();
        for p in leads {
            let (&last, parent) = p.split_last()?;
            tables.get_mut(parent)?[last as usize] = T_LEAD;
        }
        Some(Trie { tables, multi })
    }

    /// Decodes the one sequence at `b[i..]` into `out`: where the next one
    /// starts, or None where it is illegal or cut short.
    fn step(&self, b: &[u8], mut i: usize, out: &mut String) -> Option<usize> {
        let start = i;
        let mut table = self.tables.get(&b""[..])?;
        loop {
            let &byte = b.get(i)?;
            i += 1;
            match table[byte as usize] {
                T_ILLEGAL => return None,
                T_LEAD => table = self.tables.get(&b[start..i])?,
                T_MULTI => {
                    py_push_str(out, self.multi.get(&b[start..i])?);
                    return Some(i);
                }
                cp => {
                    py_push(out, cp);
                    return Some(i);
                }
            }
        }
    }
}

static TRIES: LazyLock<Mutex<HashMap<String, Arc<Trie>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn trie(name: &str) -> Result<Arc<Trie>, String> {
    if let Some(t) = TRIES.lock().ok().and_then(|m| m.get(name).cloned()) {
        return Ok(t);
    }
    let t = Arc::new(Trie::load(name).ok_or_else(|| format!("i18n: no table for {name}"))?);
    if let Ok(mut m) = TRIES.lock() {
        m.insert(name.to_string(), t.clone());
    }
    Ok(t)
}

/// A stateless CJKCodecs decoder: gb18030's four-byte sequences and
/// euc_kr's KS X 1001:1998 make-up sequences by rule, the rest by table.
fn multibyte_decode(name: &str, b: &[u8], out: &mut String) -> Result<(), String> {
    let t = trie(name)?;
    let cant = || format!("'{name}' codec can't decode bytes");
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        i = if name == "gb18030" && c >= 0x80 && b.get(i + 1).is_some_and(|c2| (0x30..=0x39).contains(c2)) {
            gb18030_four(b, i, out)
        } else if name == "euc_kr" && c == 0xa4 && b.get(i + 1) == Some(&0xd4) {
            euckr_makeup(b, i, out)
        } else {
            t.step(b, i, out)
        }
        .ok_or_else(cant)?;
    }
    Ok(())
}

/// gb18030's four-byte sequences: U+0080..U+FFFF in ranges, then the
/// supplementary planes in order.
fn gb18030_four(b: &[u8], i: usize, out: &mut String) -> Option<usize> {
    static RANGES: LazyLock<Vec<(u32, u32)>> =
        LazyLock::new(|| codec_line("gb18030_ranges").and_then(|l| serde_json::from_str(l).ok()).unwrap_or_default());
    let s = b.get(i..i + 4)?;
    let (c, c2, c3, c4) = (s[0], s[1], s[2], s[3]);
    if !(0x81..=0xfe).contains(&c) || !(0x81..=0xfe).contains(&c3) || !(0x30..=0x39).contains(&c4) {
        return None;
    }
    let (c, c2, c3, c4) = ((c - 0x81) as u32, (c2 - 0x30) as u32, (c3 - 0x81) as u32, (c4 - 0x30) as u32);
    if c < 4 {
        // U+0080 - U+FFFF
        let lseq = (c * 10 + c2) * 1260 + c3 * 10 + c4;
        if lseq < 39420 {
            let k = RANGES.partition_point(|&(base, _)| base <= lseq).checked_sub(1)?;
            let (base, first) = RANGES[k];
            py_push(out, first + lseq - base);
            return Some(i + 4);
        }
    } else if c >= 15 {
        // U+10000 - U+10FFFF
        let lseq = 0x10000 + ((c - 15) * 10 + c2) * 1260 + c3 * 10 + c4;
        if lseq <= 0x10FFFF {
            py_push(out, lseq);
            return Some(i + 4);
        }
    }
    None
}

/// euc_kr's eight-byte make-up sequence: 0xa4d4, then choseong, jungseong
/// and jongseong, each after 0xa4.
fn euckr_makeup(b: &[u8], i: usize, out: &mut String) -> Option<usize> {
    #[derive(serde::Deserialize, Default)]
    struct Jamo {
        cho: Vec<i32>,
        jong: Vec<i32>,
    }
    static JAMO: LazyLock<Jamo> =
        LazyLock::new(|| codec_line("euc_kr_jamo").and_then(|l| serde_json::from_str(l).ok()).unwrap_or_default());
    let s = b.get(i..i + 8)?;
    if s[2] != 0xa4 || s[4] != 0xa4 || s[6] != 0xa4 {
        return None;
    }
    let cho = if (0xa1..=0xbe).contains(&s[3]) { *JAMO.cho.get((s[3] - 0xa1) as usize)? } else { -1 };
    let jung = if (0xbf..=0xd3).contains(&s[5]) { (s[5] - 0xbf) as i32 } else { -1 };
    let jong = if s[7] == 0xd4 {
        0
    } else if (0xa1..=0xbe).contains(&s[7]) {
        *JAMO.jong.get((s[7] - 0xa1) as usize)?
    } else {
        -1
    };
    if cho < 0 || jung < 0 || jong < 0 {
        return None;
    }
    py_push(out, (0xac00 + cho * 588 + jung * 28 + jong) as u32);
    Some(i + 8)
}

/// The HZ codec: ASCII, with `~{` ... `~}` around GB2312.
fn hz_decode(b: &[u8], out: &mut String) -> Option<()> {
    let t = trie("iso2022_hz_gb2312").ok()?;
    let mut gb = false;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'~' {
            match (*b.get(i + 1)?, gb) {
                (b'~', false) => py_push(out, '~' as u32),
                (b'{', false) => gb = true,    // set GB
                (b'\n', false) => {}           // line-continuation
                (b'}', true) => gb = false,    // set ASCII
                _ => return None,
            }
            i += 2;
            continue;
        }
        if c & 0x80 != 0 {
            return None;
        }
        if !gb {
            py_push(out, c as u32);
            i += 1;
        } else {
            i = t.step(b, i, out)?;
        }
    }
    Some(())
}

/// An ISO-2022 codec's configuration: its flags, and the character sets it
/// can designate (final byte, with 0x80 for a double-byte set; the table;
/// bytes per character), first match wins.
struct Iso2022 {
    no_shift: bool,
    use_g2: bool,
    jisx0208_ext: bool,
    designations: &'static [(u8, &'static str, usize)],
}

const DBCS: u8 = 0x80;
const CHARSET_ASCII: u8 = b'B';

fn iso2022_config(module: &str) -> Option<Iso2022> {
    const JISX0208: (u8, &str, usize) = (b'B' | DBCS, "jisx0208", 2);
    const JISX0208_O: (u8, &str, usize) = (b'@' | DBCS, "jisx0208", 2);
    const JISX0212: (u8, &str, usize) = (b'D' | DBCS, "jisx0212", 2);
    const JISX0201_R: (u8, &str, usize) = (b'J', "jisx0201_r", 1);
    const JISX0201_K: (u8, &str, usize) = (b'I', "jisx0201_k", 1);
    const KSX1001: (u8, &str, usize) = (b'C' | DBCS, "ksx1001", 2);
    const GB2312: (u8, &str, usize) = (b'A' | DBCS, "gb2312", 2);
    // ISO 8859 in G0 has no decoder: only SS2 (G2) reads it
    const ISO8859_1: (u8, &str, usize) = (b'A', "", 1);
    const ISO8859_7: (u8, &str, usize) = (b'F', "", 1);
    const JISX0213_2000_1: (u8, &str, usize) = (b'O' | DBCS, "jisx0213_2000_1", 2);
    const JISX0213_2000_2: (u8, &str, usize) = (b'P' | DBCS, "jisx0213_2000_2", 2);
    const JISX0213_2004_1: (u8, &str, usize) = (b'Q' | DBCS, "jisx0213_2004_1", 2);
    const JISX0213_2004_2: (u8, &str, usize) = (b'P' | DBCS, "jisx0213_2004_2", 2);
    let jp = |designations| Iso2022 { no_shift: true, use_g2: false, jisx0208_ext: true, designations };
    Some(match module {
        "iso2022_kr" => Iso2022 { no_shift: false, use_g2: false, jisx0208_ext: false, designations: &[KSX1001] },
        "iso2022_jp" => jp(&[JISX0208, JISX0201_R, JISX0208_O]),
        "iso2022_jp_1" => jp(&[JISX0208, JISX0212, JISX0201_R, JISX0208_O]),
        "iso2022_jp_2" => Iso2022 {
            no_shift: true,
            use_g2: true,
            jisx0208_ext: true,
            designations: &[JISX0208, JISX0212, KSX1001, GB2312, JISX0201_R, JISX0208_O, ISO8859_1, ISO8859_7],
        },
        "iso2022_jp_2004" => jp(&[JISX0213_2004_1, JISX0208, JISX0213_2004_1, JISX0213_2004_2]),
        "iso2022_jp_3" => jp(&[JISX0213_2000_1, JISX0208, JISX0213_2000_1, JISX0213_2000_2]),
        "iso2022_jp_ext" => jp(&[JISX0208, JISX0212, JISX0201_R, JISX0201_K, JISX0208_O]),
        _ => return None,
    })
}

fn is_escend(c: u8) -> bool {
    c.is_ascii_uppercase() || c == b'@'
}

/// The ISO-2022 decoder (CJKCodecs' `iso2022` DECODER): None for a codec
/// that is not one, Some(Err) where the bytes do not decode.
fn iso2022_decode(module: &str, b: &[u8], out: &mut String) -> Option<Result<(), String>> {
    const ESC: u8 = 0x1b;
    const SO: u8 = 0x0e;
    const SI: u8 = 0x0f;
    const LF: u8 = 0x0a;
    let cfg = iso2022_config(module)?;
    let err = || format!("'{module}' codec can't decode bytes");
    // G0..G3, the shift state, and "ESC throughout" (a non-ISO-2022 escape
    // sequence, passed through as ISO-8859-1 up to its final byte)
    let mut g = [CHARSET_ASCII; 4];
    let mut shifted = false;
    let mut esc_throughout = false;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if esc_throughout {
            py_push(out, c as u32);
            i += 1;
            if is_escend(c) {
                esc_throughout = false;
            }
            continue;
        }
        let bypass = |out: &mut String| py_push(out, c as u32);
        match c {
            ESC => {
                let Some(&c2) = b.get(i + 1) else { return Some(Err(err())) };
                if matches!(c2, b'(' | b')' | b'$' | b'.' | b'&') {
                    match iso2022_processesc(&cfg, &mut g, &b[i..]) {
                        Some(len) => i += len,
                        None => return Some(Err(err())),
                    }
                } else if cfg.use_g2 && c2 == b'N' {
                    // SS2
                    let Some(&c3) = b.get(i + 2) else { return Some(Err(err())) };
                    if iso2022_processg2(g[2], c3, out).is_none() {
                        return Some(Err(err()));
                    }
                    i += 3;
                } else {
                    py_push(out, ESC as u32);
                    esc_throughout = true;
                    i += 1;
                }
            }
            SI if !cfg.no_shift => {
                shifted = false;
                i += 1;
            }
            SO if !cfg.no_shift => {
                shifted = true;
                i += 1;
            }
            LF => {
                shifted = false;
                py_push(out, LF as u32);
                i += 1;
            }
            // C0
            _ if c < 0x20 => {
                bypass(out);
                i += 1;
            }
            _ if c >= 0x80 => return Some(Err(err())),
            _ => {
                let charset = if shifted { g[1] } else { g[0] };
                if charset == CHARSET_ASCII {
                    bypass(out);
                    i += 1;
                    continue;
                }
                let Some(&(_, table, width)) = cfg.designations.iter().find(|d| d.0 == charset) else {
                    return Some(Err(err()));
                };
                let Some(data) = b.get(i..i + width) else { return Some(Err(err())) };
                let decoded = if table.is_empty() {
                    None
                } else {
                    match trie(&format!("iso2022_{table}")) {
                        Ok(t) => t.step(data, 0, out),
                        Err(e) => return Some(Err(e)),
                    }
                };
                if decoded != Some(width) {
                    return Some(Err(err()));
                }
                i += width;
            }
        }
    }
    Some(Ok(()))
}

/// `iso2022processesc`: a designation escape sequence at `buf[0]`; its
/// length, or None where Python's decoder fails.
fn iso2022_processesc(cfg: &Iso2022, g: &mut [u8; 4], buf: &[u8]) -> Option<usize> {
    const ESC: u8 = 0x1b;
    const MAX_ESCSEQLEN: usize = 16;
    let inleft = buf.len();
    let mut esclen = 0;
    let mut i = 1;
    while i < MAX_ESCSEQLEN {
        if i >= inleft {
            return None;
        }
        if is_escend(buf[i]) {
            esclen = i + 1;
            break;
        } else if cfg.jisx0208_ext && i + 1 < inleft && buf[i] == b'&' && buf[i + 1] == b'@' {
            i += 2;
        }
        i += 1;
    }
    let (charset, designation) = match esclen {
        // unterminated escape sequence
        0 => return None,
        3 => {
            if buf[1] == b'$' {
                (buf[2] | DBCS, 0)
            } else {
                let designation = match buf[1] {
                    b'(' => 0,
                    b')' => 1,
                    b'.' if cfg.use_g2 => 2,
                    _ => return None,
                };
                (buf[2], designation)
            }
        }
        4 => {
            if buf[1] != b'$' {
                return None;
            }
            let designation = match buf[2] {
                b'(' => 0,
                b')' => 1,
                _ => return None,
            };
            (buf[3] | DBCS, designation)
        }
        // designation with prefix
        6 => {
            if cfg.jisx0208_ext && buf[3] == ESC && buf[4] == b'$' && buf[5] == b'B' {
                (b'B' | DBCS, 0)
            } else {
                return None;
            }
        }
        _ => return None,
    };
    // raise error when the charset is not designated for this encoding
    if charset != CHARSET_ASCII && !cfg.designations.iter().any(|d| d.0 == charset) {
        return None;
    }
    g[designation] = charset;
    Some(esclen)
}

/// `iso2022processg2`: the character after SS2, from G2.
fn iso2022_processg2(g2: u8, c3: u8, out: &mut String) -> Option<()> {
    match g2 {
        // ISO-8859-1
        b'A' => {
            if c3 >= 0x80 {
                return None;
            }
            py_push(out, c3 as u32 + 0x80);
        }
        // ISO-8859-7
        b'F' => {
            let c = (c3 ^ 0x80) as u32;
            let u = if c < 0xa0 {
                c
            } else if c < 0xc0 && (0x288f3bc9u32 & (1 << (c - 0xa0))) != 0 {
                c
            } else if (0xb4..=0xfe).contains(&c) && (c >= 0xd4 || (0xbffffd77u32 & (1 << (c - 0xb4))) != 0) {
                0x02d0 + c
            } else if c == 0xa1 {
                0x2018
            } else if c == 0xa2 {
                0x2019
            } else if c == 0xaf {
                0x2015
            } else {
                return None;
            };
            py_push(out, u);
        }
        CHARSET_ASCII => {
            if c3 & 0x80 != 0 {
                return None;
            }
            py_push(out, c3 as u32);
        }
        _ => return None,
    }
    Some(())
}

// ---------------------------------------------------------------------------
// Punycode and IDNA (encodings/punycode.py, encodings/idna.py).

/// punycode.py's `T`.
fn puny_t(j: i128, bias: i128) -> i128 {
    (36 * (j + 1) - bias).clamp(1, 26)
}

/// punycode.py's `adapt`.
fn puny_adapt(delta: i128, first: bool, numchars: i128) -> i128 {
    let mut delta = if first { delta / 700 } else { delta / 2 };
    delta += delta / numchars;
    let mut divisions = 0;
    while delta > 455 {
        delta /= 35; // base - tmin
        divisions += 36;
    }
    divisions + (36 * delta / (delta + 38))
}

/// `punycode_decode(text, 'strict')`: None where Python raises. Python's
/// integers are unbounded; a number past i128 makes a character past
/// U+10FFFF, which is an error there too.
fn punycode_decode(text: &[u8]) -> Option<Vec<u32>> {
    let (mut base, extended): (Vec<u32>, Vec<u8>) = match text.iter().rposition(|&c| c == b'-') {
        None => (Vec::new(), text.to_ascii_uppercase()),
        Some(pos) => {
            if !text[..pos].is_ascii() {
                return None;
            }
            (text[..pos].iter().map(|&c| c as u32).collect(), text[pos + 1..].to_ascii_uppercase())
        }
    };
    // 3.2 Insertion sort coding
    let mut ch: i128 = 0x80;
    let mut pos: i128 = -1;
    let mut bias: i128 = 72;
    let mut extpos = 0;
    while extpos < extended.len() {
        // 3.3 Generalized variable-length integers
        let (newpos, delta) = {
            let mut result: i128 = 0;
            let mut w: i128 = 1;
            let mut j = 0;
            let mut p = extpos;
            loop {
                let &c = extended.get(p)?;
                p += 1;
                let digit = match c {
                    b'A'..=b'Z' => (c - b'A') as i128,
                    b'0'..=b'9' => (c - 22) as i128,
                    _ => return None,
                };
                let t = puny_t(j, bias);
                result = result.checked_add(digit.checked_mul(w)?)?;
                if digit < t {
                    break (p, result);
                }
                w = w.checked_mul(36 - t)?;
                j += 1;
            }
        };
        pos = pos.checked_add(delta + 1)?;
        let len = base.len() as i128 + 1;
        ch = ch.checked_add(pos.div_euclid(len))?;
        if ch > 0x10FFFF {
            return None;
        }
        pos = pos.rem_euclid(len);
        base.insert(pos as usize, ch as u32);
        bias = puny_adapt(delta, extpos == 0, base.len() as i128);
        extpos = newpos;
    }
    Some(base)
}

/// `punycode_encode(text)`.
fn punycode_encode(text: &[u32]) -> Vec<u8> {
    const DIGITS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    // 3.1 Basic code point segregation
    let base: Vec<u8> = text.iter().filter(|&&c| c < 128).map(|&c| c as u8).collect();
    let mut extended: Vec<u32> = text.iter().copied().filter(|&c| c >= 128).collect();
    extended.sort_unstable();
    extended.dedup();
    // 3.2 Insertion unsort coding
    let mut deltas: Vec<i128> = Vec::new();
    let mut oldchar: i128 = 0x80;
    let mut oldindex: i128 = -1;
    for &c in &extended {
        let (mut index, mut pos): (i128, i128) = (-1, -1);
        let ch = c as i128;
        let curlen = text.iter().filter(|&&x| (x as i128) < ch).count() as i128;
        let mut delta = (curlen + 1) * (ch - oldchar);
        loop {
            // selective_find
            loop {
                pos += 1;
                if pos == text.len() as i128 {
                    index = -1;
                    break;
                }
                let x = text[pos as usize];
                if x == c {
                    index += 1;
                    break;
                } else if x < c {
                    index += 1;
                }
            }
            if index == -1 {
                break;
            }
            delta += index - oldindex;
            deltas.push(delta - 1);
            oldindex = index;
            delta = 0;
        }
        oldchar = ch;
    }
    // 3.4 Bias adaptation
    let mut out: Vec<u8> = Vec::new();
    let mut bias = 72;
    for (points, &delta) in deltas.iter().enumerate() {
        // 3.3 Generalized variable-length integers
        let mut n = delta;
        let mut j = 0;
        loop {
            let t = puny_t(j, bias);
            if n < t {
                out.push(DIGITS[n.rem_euclid(36) as usize]);
                break;
            }
            out.push(DIGITS[(t + (n - t) % (36 - t)) as usize]);
            n = (n - t) / (36 - t);
            j += 1;
        }
        bias = puny_adapt(delta, points == 0, base.len() as i128 + points as i128 + 1);
    }
    if base.is_empty() {
        out
    } else {
        let mut r = base;
        r.push(b'-');
        r.extend(out);
        r
    }
}

/// The `idna` codec's decode: None where Python raises.
fn idna_decode(input: &[u8], out: &mut String) -> Option<()> {
    if input.is_empty() {
        return Some(());
    }
    let has_ace = |b: &[u8]| b.to_ascii_lowercase().windows(4).any(|w| w == b"xn--");
    if !has_ace(input) && input.is_ascii() {
        // Fast path
        input.iter().for_each(|&c| py_push(out, c as u32));
        return Some(());
    }
    let mut labels: Vec<&[u8]> = input.split(|&c| c == b'.').collect();
    let trailing_dot = labels.last().is_some_and(|l| l.is_empty());
    if trailing_dot {
        labels.pop();
    }
    let mut result: Vec<Vec<u32>> = Vec::new();
    for label in labels {
        result.push(idna_to_unicode(label)?);
    }
    for (k, label) in result.iter().enumerate() {
        if k > 0 {
            py_push(out, '.' as u32);
        }
        label.iter().for_each(|&c| py_push(out, c));
    }
    if trailing_dot {
        py_push(out, '.' as u32);
    }
    Some(())
}

/// idna.py's `ToUnicode` for a bytes label.
fn idna_to_unicode(label: &[u8]) -> Option<Vec<u32>> {
    if label.len() > 1024 {
        return None;
    }
    // Step 3: Check for ACE prefix
    if !label.to_ascii_lowercase().starts_with(b"xn--") {
        return label.is_ascii().then(|| label.iter().map(|&c| c as u32).collect());
    }
    // Step 4: Remove ACE prefix; Step 5: Decode using PUNYCODE
    let result = punycode_decode(&label[4..])?;
    // Step 6: Apply ToASCII
    let label2 = idna_to_ascii(&result)?;
    // Step 7: Compare the result of step 6 with the one of step 3
    if !label.is_ascii() || label.to_ascii_lowercase() != label2 {
        return None;
    }
    // Step 8: return the result of step 5
    Some(result)
}

/// idna.py's `ToASCII`.
fn idna_to_ascii(label: &[u32]) -> Option<Vec<u8>> {
    let ascii = |l: &[u32]| -> Option<Option<Vec<u8>>> {
        if l.iter().all(|&c| c < 128) {
            return Some((!l.is_empty() && l.len() < 64).then(|| l.iter().map(|&c| c as u8).collect()));
        }
        None
    };
    // Step 1: try ASCII
    if let Some(r) = ascii(label) {
        return r;
    }
    // Step 2: nameprep
    let label = nameprep(label)?;
    // Step 4: try ASCII
    if let Some(r) = ascii(&label) {
        return r;
    }
    // Step 5: Check ACE prefix
    if py_codepoints(&py_lower(&py_from_codepoints(&label))).starts_with(&[0x78, 0x6e, 0x2d, 0x2d]) {
        return None;
    }
    // Step 6: Encode with PUNYCODE; Step 7: Prepend ACE prefix
    let mut label_ascii = b"xn--".to_vec();
    label_ascii.extend(punycode_encode(&label));
    // Step 8: Check size
    (label_ascii.len() < 64).then_some(label_ascii)
}

/// What nameprep reads: stringprep's tables and Unicode 3.2 NFKC as
/// `unicodedata.ucd_3_2_0` has them (data/i18n/codecs.txt, line `idna`).
#[derive(serde::Deserialize, Default)]
struct IdnaTables {
    b1: Vec<(u32, u32)>,
    b2: Vec<(u32, String)>,
    c: Vec<(u32, u32)>,
    d1: Vec<(u32, u32)>,
    d2: Vec<(u32, u32)>,
    nfkd: Vec<(u32, String)>,
    ccc: Vec<(u32, u32, u8)>,
    comp: Vec<(u32, u32, u32)>,
    #[serde(skip)]
    b2_map: HashMap<u32, Vec<u32>>,
    #[serde(skip)]
    nfkd_map: HashMap<u32, Vec<u32>>,
    #[serde(skip)]
    comp_map: HashMap<(u32, u32), u32>,
    #[serde(skip)]
    comp_first: HashSet<u32>,
}

static IDNA: LazyLock<IdnaTables> = LazyLock::new(|| {
    let mut t: IdnaTables = codec_line("idna").and_then(|l| serde_json::from_str(l).ok()).unwrap_or_default();
    let cps = |s: &str| s.chars().map(|c| c as u32).collect::<Vec<u32>>();
    t.b2_map = t.b2.iter().map(|(c, s)| (*c, cps(s))).collect();
    t.nfkd_map = t.nfkd.iter().map(|(c, s)| (*c, cps(s))).collect();
    t.comp_map = t.comp.iter().map(|&(a, b, c)| ((a, b), c)).collect();
    t.comp_first = t.comp.iter().map(|&(a, _, _)| a).collect();
    t
});

/// The canonical combining class NFKC orders by (the current database's).
fn ccc(u: u32) -> u8 {
    let t = &IDNA.ccc;
    let k = t.partition_point(|&(lo, _, _)| lo <= u);
    match k.checked_sub(1).map(|k| t[k]) {
        Some((_, hi, c)) if u <= hi => c,
        _ => 0,
    }
}

const SBASE: u32 = 0xAC00;
const LBASE: u32 = 0x1100;
const VBASE: u32 = 0x1161;
const TBASE: u32 = 0x11A7;
const LCOUNT: u32 = 19;
const VCOUNT: u32 = 21;
const TCOUNT: u32 = 28;
const NCOUNT: u32 = VCOUNT * TCOUNT;
const SCOUNT: u32 = LCOUNT * NCOUNT;

/// `unicodedata.ucd_3_2_0.normalize('NFKC', s)`, as CPython's `nfc_nfkc`
/// computes it.
fn nfkc_3_2(s: &[u32]) -> Vec<u32> {
    let t = &*IDNA;
    // nfd_nfkd: decompose (Hangul by rule), then order each run of
    // combining characters (a stable sort, as CPython's)
    let mut d: Vec<u32> = Vec::with_capacity(s.len());
    for &code in s {
        if (SBASE..SBASE + SCOUNT).contains(&code) {
            let sindex = code - SBASE;
            d.push(LBASE + sindex / NCOUNT);
            d.push(VBASE + (sindex % NCOUNT) / TCOUNT);
            if sindex % TCOUNT != 0 {
                d.push(TBASE + sindex % TCOUNT);
            }
        } else if let Some(x) = t.nfkd_map.get(&code) {
            d.extend(x);
        } else {
            d.push(code);
        }
    }
    let mut i = 0;
    while i < d.len() {
        if ccc(d[i]) == 0 {
            i += 1;
            continue;
        }
        let start = i;
        while i < d.len() && ccc(d[i]) != 0 {
            i += 1;
        }
        d[start..i].sort_by_key(|&c| ccc(c));
    }
    // nfc_nfkc: compose
    let len = d.len();
    let mut out: Vec<u32> = Vec::with_capacity(len);
    let mut skipped: Vec<usize> = Vec::new();
    let mut i = 0;
    while i < len {
        if let Some(k) = skipped.iter().position(|&x| x == i) {
            // this character is skipped: remove it from the list
            skipped.swap_remove(k);
            i += 1;
            continue;
        }
        let code = d[i];
        // Hangul Composition
        if (LBASE..LBASE + LCOUNT).contains(&code) && i + 1 < len && (VBASE..VBASE + VCOUNT).contains(&d[i + 1]) {
            let mut code = SBASE + ((code - LBASE) * VCOUNT + (d[i + 1] - VBASE)) * TCOUNT;
            i += 2;
            if i < len && TBASE < d[i] && d[i] < TBASE + TCOUNT {
                code += d[i] - TBASE;
                i += 1;
            }
            out.push(code);
            continue;
        }
        if !t.comp_first.contains(&code) {
            out.push(code);
            i += 1;
            continue;
        }
        // Find next unblocked character.
        let mut cur = code;
        let mut i1 = i + 1;
        let mut comb = 0;
        while i1 < len {
            let code1 = d[i1];
            let comb1 = ccc(code1);
            if comb != 0 {
                if comb1 == 0 {
                    break;
                }
                if comb >= comb1 {
                    // Character is blocked.
                    i1 += 1;
                    continue;
                }
            }
            match t.comp_map.get(&(cur, code1)) {
                None => {
                    if comb1 == 0 {
                        break;
                    }
                    comb = comb1;
                    i1 += 1;
                }
                Some(&c) => {
                    // Replace the original character; the second is used
                    cur = c;
                    skipped.push(i1);
                    i1 += 1;
                    if !t.comp_first.contains(&cur) {
                        break;
                    }
                }
            }
        }
        out.push(cur);
        i += 1;
    }
    out
}

/// idna.py's `nameprep` (RFC 3491): None where it raises.
fn nameprep(label: &[u32]) -> Option<Vec<u32>> {
    let t = &*IDNA;
    // Map
    let mut mapped: Vec<u32> = Vec::with_capacity(label.len());
    for &c in label {
        if in_ranges(&t.b1, c) {
            // Map to nothing
            continue;
        }
        match t.b2_map.get(&c) {
            Some(m) => mapped.extend(m),
            None => mapped.push(c),
        }
    }
    // Normalize
    let label = if mapped.is_empty() { mapped } else { nfkc_3_2(&mapped) };
    // Prohibit
    if label.iter().any(|&c| in_ranges(&t.c, c)) {
        return None;
    }
    // Check bidi
    let rand_al: Vec<bool> = label.iter().map(|&c| in_ranges(&t.d1, c)).collect();
    if rand_al.iter().any(|&r| r) {
        // A string with a RandALCat character must have no LCat one, and
        // start and end with RandALCat.
        if label.iter().any(|&c| in_ranges(&t.d2, c)) {
            return None;
        }
        if !rand_al[0] || !rand_al[rand_al.len() - 1] {
            return None;
        }
    }
    Some(label)
}

// ---------------------------------------------------------------------------
// The Unicode name database, as `\N{...}` reads it (unicodedata's getcode):
// Hangul syllables and CJK/Tangut ideographs by rule, the rest by name or
// alias (data/i18n/names.txt: sorted, each line `<shared><rest>;<hex>` with
// <shared> the number of leading characters shared with the line before, as
// one character '0' + n).

static NAMES_DATA: &str = include_str!("../data/i18n/names.txt");

static NAMES: LazyLock<HashMap<String, u32>> = LazyLock::new(|| {
    let mut map = HashMap::new();
    let mut prev = String::new();
    for line in NAMES_DATA.lines() {
        let mut cs = line.chars();
        let Some(k) = cs.next() else { continue };
        let Some((rest, hex)) = cs.as_str().rsplit_once(';') else { continue };
        let k = (k as usize).saturating_sub('0' as usize).min(prev.len());
        let name = format!("{}{rest}", &prev[..k]);
        if let Ok(cp) = u32::from_str_radix(hex, 16) {
            map.insert(name.clone(), cp);
        }
        prev = name;
    }
    map
});

/// The derived names: (first, last, prefix index) as CPython 3.14's
/// unicodename_db.h has them; prefix 0 is Hangul.
const DERIVED_NAME_RANGES: &[(u32, u32, usize)] = &[
    (0x3400, 0x4DBF, 1),
    (0x4E00, 0x9FFF, 1),
    (0xAC00, 0xD7A3, 0),
    (0x17000, 0x187F7, 2),
    (0x18D00, 0x18D08, 2),
    (0x20000, 0x2A6DF, 1),
    (0x2A700, 0x2B739, 1),
    (0x2B740, 0x2B81D, 1),
    (0x2B820, 0x2CEA1, 1),
    (0x2CEB0, 0x2EBE0, 1),
    (0x2EBF0, 0x2EE5D, 1),
    (0x30000, 0x3134A, 1),
    (0x31350, 0x323AF, 1),
];
const DERIVED_NAME_PREFIXES: &[&str] = &["HANGUL SYLLABLE ", "CJK UNIFIED IDEOGRAPH-", "TANGUT IDEOGRAPH-"];

/// The short names of the Hangul jamo: (L, V, T) columns.
const HANGUL_SYLLABLES: [&[&str]; 3] = [
    &["G", "GG", "N", "D", "DD", "R", "M", "B", "BB", "S", "SS", "", "J", "JJ", "C", "K", "T", "P", "H"],
    &["A", "AE", "YA", "YAE", "EO", "E", "YEO", "YE", "O", "WA", "WAE", "OE", "YO", "U", "WEO", "WE", "WI", "YU", "EU", "YI", "I"],
    &["", "G", "GG", "GS", "N", "NJ", "NH", "D", "L", "LG", "LM", "LB", "LS", "LT", "LP", "LH", "M", "B", "BS", "S", "SS", "NG", "J", "C", "K", "T", "P", "H"],
];

/// The code point `\N{name}` names (aliases included, named sequences not),
/// matched without regard to ASCII case.
fn unicode_lookup(name: &[u8]) -> Option<u32> {
    let prefix = DERIVED_NAME_PREFIXES
        .iter()
        .position(|p| name.len() >= p.len() && name[..p.len()].eq_ignore_ascii_case(p.as_bytes()));
    match prefix {
        Some(0) => {
            // Hangul syllables: the longest jamo name at each step
            let mut pos = 16;
            let mut found = [0usize; 3];
            for (col, names) in HANGUL_SYLLABLES.iter().enumerate() {
                let mut best: Option<(usize, usize)> = None;
                for (k, s) in names.iter().enumerate() {
                    if best.is_some_and(|(len, _)| s.len() <= len) {
                        continue;
                    }
                    if name.len() >= pos + s.len() && name[pos..pos + s.len()].eq_ignore_ascii_case(s.as_bytes()) {
                        best = Some((s.len(), k));
                    }
                }
                let (len, k) = best?;
                found[col] = k;
                pos += len;
            }
            (pos == name.len()).then(|| SBASE + (found[0] as u32 * VCOUNT + found[1] as u32) * TCOUNT + found[2] as u32)
        }
        Some(i) => {
            // CJK and Tangut ideographs: 4 to 6 hex digits, no leading zero
            let hex = &name[DERIVED_NAME_PREFIXES[i].len()..];
            if !(4..=6).contains(&hex.len()) || hex[0] == b'0' || !hex.iter().all(|c| c.is_ascii_hexdigit()) {
                return None;
            }
            let v = u32::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?;
            let id = DERIVED_NAME_RANGES.iter().find(|&&(lo, hi, _)| lo <= v && v <= hi).map(|r| r.2);
            (id == Some(i) && v <= 0x10FFFF).then_some(v)
        }
        None => NAMES.get(std::str::from_utf8(&name.to_ascii_uppercase()).ok()?).copied(),
    }
}

impl Translation {
    /// `GNUTranslations.gettext`, following the fallback chain.
    pub fn gettext(&self, message: &str) -> Result<String, String> {
        self.gettext_py(&py_from_str(message)).map(|s| py_lossy(&s))
    }

    /// `gettext` on pystrs.
    fn gettext_py(&self, message: &str) -> Result<String, String> {
        for cat in &self.chain {
            if let Some(t) = cat.catalog.get(&Key::Msg(message.to_string())) {
                return Ok(t.clone());
            }
            let p = cat.plural(&PluralN::Int(BigInt::from(1)), &mut false)?;
            if let Some(t) = cat.get_plural(message, &p) {
                return Ok(t.clone());
            }
        }
        Ok(message.to_string())
    }

    /// `GNUTranslations.ngettext`, following the fallback chain.
    pub fn ngettext(&self, msgid1: &str, msgid2: &str, n: impl Into<PluralN>) -> Result<String, String> {
        self.ngettext_n(msgid1, msgid2, &n.into(), &mut false)
    }

    fn ngettext_n(&self, msgid1: &str, msgid2: &str, n: &PluralN, warned: &mut bool) -> Result<String, String> {
        self.ngettext_py(&py_from_str(msgid1), &py_from_str(msgid2), n, warned).map(|s| py_lossy(&s))
    }

    /// `ngettext` on pystrs; `warned` is set where gettext warns that the
    /// count is not an integer.
    fn ngettext_py(&self, msgid1: &str, msgid2: &str, n: &PluralN, warned: &mut bool) -> Result<String, String> {
        if self.chain.is_empty() {
            // NullTranslations: `n = _as_int2(n)`, which warns for anything
            // that is not an int
            if !matches!(n, PluralN::Int(_)) {
                *warned = true;
            }
        }
        for cat in &self.chain {
            let p = cat.plural(n, warned)?;
            if let Some(t) = cat.get_plural(msgid1, &p) {
                return Ok(t.clone());
            }
        }
        Ok(if n.is_one() { msgid1 } else { msgid2 }.to_string())
    }
}

/// Parsed catalogs, by path: Python's `gettext._translations` cache.
static TRANSLATIONS: LazyLock<Mutex<HashMap<PathBuf, Arc<GnuCatalog>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// `gettext.translation(domain, localedir, languages, fallback=True)`.
pub fn translation(domain: &str, localedir: Option<&str>, languages: Option<Vec<String>>) -> Result<Translation, String> {
    let mofiles = find_paths(domain, localedir, languages);
    let mut result = Translation::default();
    // Avoid opening, reading, and parsing the .mo file after it's been done
    // once.
    for mofile in mofiles {
        let key = abspath(&mofile);
        let cached = TRANSLATIONS.lock().ok().and_then(|m| m.get(&key).cloned());
        let t = match cached {
            Some(t) => t,
            None => {
                let buf = std::fs::read(&mofile).map_err(|e| e.to_string())?;
                let t = Arc::new(GnuCatalog::parse(&buf, &mofile.to_string_lossy())?);
                if let Ok(mut m) = TRANSLATIONS.lock() {
                    m.entry(key).or_insert_with(|| t.clone()).clone()
                } else {
                    t
                }
            }
        };
        result.chain.push(t);
    }
    Ok(result)
}

/// `os.path.abspath`: joined to the working directory, then
/// `os.path.normpath` ('.', '..' and repeated slashes resolved as text).
fn abspath(p: &Path) -> PathBuf {
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(d) => d.join(p),
            Err(_) => p.to_path_buf(),
        }
    };
    PathBuf::from(OsString::from_vec(normpath(joined.as_os_str().as_bytes())))
}

/// `posixpath.normpath`.
fn normpath(path: &[u8]) -> Vec<u8> {
    if path.is_empty() {
        return b".".to_vec();
    }
    // POSIX allows one or two initial slashes, but treats three or more
    // as single slash.
    let initial = if path.starts_with(b"/") {
        if path.starts_with(b"//") && !path.starts_with(b"///") { 2 } else { 1 }
    } else {
        0
    };
    let mut comps: Vec<&[u8]> = Vec::new();
    for comp in path.split(|&c| c == b'/') {
        if comp.is_empty() || comp == b"." {
            continue;
        }
        if comp != b".." || (initial == 0 && comps.is_empty()) || comps.last() == Some(&&b".."[..]) {
            comps.push(comp);
        } else if !comps.is_empty() {
            comps.pop();
        }
    }
    let mut out = vec![b'/'; initial];
    out.extend(comps.join(&b'/'));
    if out.is_empty() {
        b".".to_vec()
    } else {
        out
    }
}

/// `os.environ[name]`, as a pystr: Python decodes the bytes as UTF-8 with
/// `surrogateescape`, each undecodable byte b becoming the lone surrogate
/// U+DC00 + b, so a value that is not UTF-8 is still used.
fn environ(name: &str) -> Option<String> {
    let raw = std::env::var_os(name)?;
    let mut out = String::new();
    for chunk in raw.as_bytes().utf8_chunks() {
        py_push_str(&mut out, chunk.valid());
        for &b in chunk.invalid() {
            py_push(&mut out, 0xDC00 + b as u32);
        }
    }
    Some(out)
}

/// `os.fsencode` of a pystr: UTF-8, a surrogate U+DC80..U+DCFF back to its
/// byte; None where Python raises (any other lone surrogate).
fn fsencode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    for u in py_codepoints(s) {
        if (0xDC80..=0xDCFF).contains(&u) {
            out.push((u - 0xDC00) as u8);
        } else {
            let mut buf = [0; 4];
            out.extend_from_slice(char::from_u32(u)?.encode_utf8(&mut buf).as_bytes());
        }
    }
    Some(out)
}

/// `gettext.find(domain, localedir, languages, all=True)`.
pub fn find(domain: &str, localedir: Option<&str>, languages: Option<Vec<String>>) -> Vec<String> {
    find_paths(domain, localedir, languages).iter().map(|p| p.to_string_lossy().into_owned()).collect()
}

fn find_paths(domain: &str, localedir: Option<&str>, languages: Option<Vec<String>>) -> Vec<PathBuf> {
    // Get some reasonable defaults for arguments that were not supplied
    let localedir = localedir.unwrap_or(default_localedir());
    let languages: Vec<String> = match languages {
        Some(l) => l.iter().map(|s| py_from_str(s)).collect(),
        None => {
            let mut languages: Vec<String> = Vec::new();
            for envar in ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"] {
                if let Some(val) = environ(envar) {
                    if !val.is_empty() {
                        languages = val.split(':').map(String::from).collect();
                        break;
                    }
                }
            }
            if !languages.iter().any(|l| l == "C") {
                languages.push("C".into());
            }
            languages
        }
    };
    // now normalize and expand the languages
    let mut nelangs: Vec<String> = Vec::new();
    for lang in &languages {
        for nelang in expand_lang_py(lang) {
            if !nelangs.contains(&nelang) {
                nelangs.push(nelang);
            }
        }
    }
    // select a language
    let mut result = Vec::new();
    for lang in nelangs {
        if lang == "C" {
            break;
        }
        // os.path.exists is False for a path os.fsencode cannot encode
        let Some(lang) = fsencode(&lang) else { continue };
        let lang = PathBuf::from(OsString::from_vec(lang));
        let mofile = Path::new(localedir).join(&lang).join("LC_MESSAGES").join(format!("{domain}.mo"));
        if mofile.exists() {
            result.push(mofile);
        }
    }
    result
}

/// `gettext._expand_lang`.
pub fn expand_lang(loc: &str) -> Vec<String> {
    expand_lang_py(&py_from_str(loc)).iter().map(|s| py_lossy(s)).collect()
}

/// `_expand_lang` on a pystr.
fn expand_lang_py(loc: &str) -> Vec<String> {
    let loc = normalize_py(loc);
    const COMPONENT_CODESET: u32 = 1 << 0;
    const COMPONENT_TERRITORY: u32 = 1 << 1;
    const COMPONENT_MODIFIER: u32 = 1 << 2;
    // split up the locale into its base components
    let mut mask = 0;
    let mut loc = loc.as_str();
    let modifier = match loc.find('@') {
        Some(pos) => {
            let m = &loc[pos..];
            loc = &loc[..pos];
            mask |= COMPONENT_MODIFIER;
            m
        }
        None => "",
    };
    let codeset = match loc.find('.') {
        Some(pos) => {
            let c = &loc[pos..];
            loc = &loc[..pos];
            mask |= COMPONENT_CODESET;
            c
        }
        None => "",
    };
    let territory = match loc.find('_') {
        Some(pos) => {
            let t = &loc[pos..];
            loc = &loc[..pos];
            mask |= COMPONENT_TERRITORY;
            t
        }
        None => "",
    };
    let language = loc;
    let mut ret = Vec::new();
    for i in 0..=mask {
        if i & !mask == 0 {
            // if all components for this combo exist ...
            let mut val = language.to_string();
            if i & COMPONENT_TERRITORY != 0 {
                val += territory;
            }
            if i & COMPONENT_CODESET != 0 {
                val += codeset;
            }
            if i & COMPONENT_MODIFIER != 0 {
                val += modifier;
            }
            ret.push(val);
        }
    }
    ret.reverse();
    ret
}

static LOCALE_ALIAS_MAP: LazyLock<HashMap<&'static str, &'static str>> =
    LazyLock::new(|| LOCALE_ALIAS.iter().copied().collect());
static LOCALE_ENCODING_ALIAS_MAP: LazyLock<HashMap<&'static str, &'static str>> =
    LazyLock::new(|| LOCALE_ENCODING_ALIAS.iter().copied().collect());
static ENCODING_ALIASES_MAP: LazyLock<HashMap<&'static str, &'static str>> =
    LazyLock::new(|| ENCODING_ALIASES.iter().copied().collect());

fn locale_alias(k: &str) -> Option<String> {
    LOCALE_ALIAS_MAP.get(k).map(|s| s.to_string())
}

/// `locale.normalize`: Returns a normalized locale code for the given locale
/// name. If normalization fails, the original name is returned unchanged.
pub fn normalize(localename: &str) -> String {
    py_lossy(&normalize_py(&py_from_str(localename)))
}

/// `locale.normalize` on a pystr.
fn normalize_py(localename: &str) -> String {
    // Normalize the locale name and extract the encoding and modifier
    let mut code = py_lower(localename);
    if code.contains(':') {
        // ':' is sometimes used as encoding delimiter.
        code = code.replace(':', ".");
    }
    let (code, modifier) = match code.split_once('@') {
        Some((c, m)) => (c.to_string(), m.to_string()),
        None => (code, String::new()),
    };
    let (langname, encoding) = if code.contains('.') {
        let mut it = code.split('.');
        (it.next().unwrap_or("").to_string(), it.next().unwrap_or("").to_string())
    } else {
        (code.clone(), String::new())
    };

    // First lookup: fullname (possibly with encoding and modifier)
    let mut lang_enc = langname.clone();
    if !encoding.is_empty() {
        let norm_encoding = encoding.replace('-', "").replace('_', "");
        lang_enc += ".";
        lang_enc += &norm_encoding;
    }
    let mut lookup_name = lang_enc.clone();
    if !modifier.is_empty() {
        lookup_name += "@";
        lookup_name += &modifier;
    }
    if let Some(code) = locale_alias(&lookup_name) {
        return code;
    }

    if !modifier.is_empty() {
        // Second try: fullname without modifier (possibly with encoding)
        if let Some(code) = locale_alias(&lang_enc) {
            match code.split_once('@') {
                None => return append_modifier(&code, &modifier),
                Some((_, m)) if py_lower(m) == modifier => return code,
                _ => {}
            }
        }
    }

    if !encoding.is_empty() {
        // Third try: langname (without encoding, possibly with modifier)
        let mut lookup_name = langname.clone();
        if !modifier.is_empty() {
            lookup_name += "@";
            lookup_name += &modifier;
        }
        if let Some(code) = locale_alias(&lookup_name) {
            return match code.split_once('@') {
                None => replace_encoding(&code, &encoding),
                Some((c, m)) => format!("{}@{}", replace_encoding(c, &encoding), m),
            };
        }

        if !modifier.is_empty() {
            // Fourth try: langname (without encoding and modifier)
            if let Some(code) = locale_alias(&langname) {
                match code.split_once('@') {
                    None => {
                        let code = replace_encoding(&code, &encoding);
                        return append_modifier(&code, &modifier);
                    }
                    Some((c, defmod)) => {
                        if py_lower(defmod) == modifier {
                            return format!("{}@{}", replace_encoding(c, &encoding), defmod);
                        }
                    }
                }
            }
        }
    }
    localename.to_string()
}

/// `locale._replace_encoding`.
fn replace_encoding(code: &str, encoding: &str) -> String {
    let langname = match code.find('.') {
        Some(i) => &code[..i],
        None => code,
    };
    // Convert the encoding to a C lib compatible encoding string
    let norm_encoding = normalize_encoding(encoding);
    let norm_encoding = ENCODING_ALIASES_MAP
        .get(py_lower(&norm_encoding).as_str())
        .map(|s| s.to_string())
        .unwrap_or(norm_encoding);
    let mut encoding = norm_encoding.clone();
    let norm_encoding = py_lower(&norm_encoding);
    if let Some(e) = LOCALE_ENCODING_ALIAS_MAP.get(norm_encoding.as_str()) {
        encoding = e.to_string();
    } else {
        let norm_encoding = norm_encoding.replace('_', "").replace('-', "");
        if let Some(e) = LOCALE_ENCODING_ALIAS_MAP.get(norm_encoding.as_str()) {
            encoding = e.to_string();
        }
    }
    format!("{langname}.{encoding}")
}

/// `locale._append_modifier`.
fn append_modifier(code: &str, modifier: &str) -> String {
    if modifier == "euro" {
        let Some((_, encoding)) = code.split_once('.') else {
            return format!("{code}.ISO8859-15");
        };
        if encoding == "ISO8859-15" || encoding == "UTF-8" {
            return code.to_string();
        }
        if encoding == "ISO8859-1" {
            return replace_encoding(code, "ISO8859-15");
        }
    }
    format!("{code}@{modifier}")
}

/// `encodings.normalize_encoding`: all non-alphanumeric characters except
/// the dot used for Python package names are collapsed and replaced with a
/// single underscore, e.g. '  -;#' becomes '_'. Leading and trailing
/// underscores are removed.
fn normalize_encoding(encoding: &str) -> String {
    let mut chars = String::new();
    let mut punct = false;
    for c in encoding.chars() {
        if py_isalnum(c) || c == '.' {
            if punct && !chars.is_empty() {
                chars.push('_');
            }
            if c.is_ascii() {
                chars.push(c);
            }
            punct = false;
        } else {
            punct = true;
        }
    }
    chars
}

// ---------------------------------------------------------------------------
// `gettext.c2py`: Plural-Forms expressions. Python turns the C expression
// into Python source text and `exec`s it, so the expression means what that
// Python text means. Since CPython 3.14.8 a negated operand is wrapped in
// parentheses (`!n == 1` becomes `(not n) == 1`, as in C); before, it read
// `not n == 1`, i.e. `!(n == 1)`.
// The port builds the same text, then parses and evaluates it as Python.

/// `str.isalnum()`: the code point ranges where it is true (Unicode 16.0,
/// as CPython 3.14 has it). Unlike `char::is_alphanumeric`, combining marks
/// that are Other_Alphabetic (U+0345, U+093E, ...) are not alphanumeric.
fn py_isalnum(c: char) -> bool {
    in_ranges(PY_ALNUM, c as u32)
}

/// `\w` in a Python `str` pattern: `isalnum()` or '_'.
fn py_word(c: Option<&char>) -> bool {
    c.is_some_and(|&c| c == '_' || py_isalnum(c))
}

/// `gettext._tokenize`: a generator over `_token_pattern`,
///
/// ```text
///     (?P<WHITESPACES>[ \t]+)                    | # spaces and horizontal tabs
///     (?P<NUMBER>[0-9]+\b)                       | # decimal integer
///     (?P<NAME>n\b)                              | # only n is allowed
///     (?P<PARENTHESIS>[()])                      |
///     (?P<OPERATOR>[-*/%+?:]|[><!]=?|==|&&|\|\|) | # !, *, /, %, +, -, <, >,
///                                                  # <=, >=, ==, !=, &&, ||,
///                                                  # ? :
///                                                  # unary and bitwise ops
///                                                  # not allowed
///     (?P<INVALID>\w+|.)                           # invalid token
/// ```
///
/// matched by hand with Python's `\w` and `\b`, and as lazily: an invalid
/// token is an error only once the parser asks for it.
struct Tokens {
    chars: Vec<char>,
    pos: usize,
    /// The final '' has been given out.
    done: bool,
}

impl Tokens {
    fn new(plural: &str) -> Tokens {
        Tokens { chars: plural.chars().collect(), pos: 0, done: false }
    }

    fn next(&mut self) -> Result<String, String> {
        let cs = &self.chars;
        loop {
            let i = self.pos;
            let Some(&c) = cs.get(i) else {
                if self.done {
                    return Err("StopIteration".into());
                }
                self.done = true;
                return Ok(String::new());
            };
            let take = |n: usize| cs[i..i + n].iter().collect::<String>();
            let run = |f: &dyn Fn(char) -> bool| cs[i..].iter().take_while(|&&c| f(c)).count();
            // WHITESPACES
            if c == ' ' || c == '\t' {
                self.pos += run(&|c| c == ' ' || c == '\t');
                continue;
            }
            let (len, invalid) = if c.is_ascii_digit() && !py_word(cs.get(i + run(&|c| c.is_ascii_digit()))) {
                // NUMBER: every shorter run of digits is followed by a digit,
                // so only the whole run can end at a word boundary.
                (run(&|c| c.is_ascii_digit()), false)
            } else if c == 'n' && !py_word(cs.get(i + 1)) {
                (1, false) // NAME
            } else if "()-*/%+?:".contains(c) {
                (1, false) // PARENTHESIS, OPERATOR
            } else if "><!".contains(c) {
                (if cs.get(i + 1) == Some(&'=') { 2 } else { 1 }, false)
            } else if (c == '=' || c == '&' || c == '|') && cs.get(i + 1) == Some(&c) {
                (2, false)
            } else if py_word(Some(&c)) {
                (run(&|c| py_word(Some(&c))), true) // INVALID \w+
            } else {
                (1, true) // INVALID .
            };
            let value = take(len);
            self.pos += len;
            if invalid {
                return Err(format!("invalid token in plural form: {value}"));
            }
            return Ok(value);
        }
    }
}

fn error(value: &str) -> String {
    if !value.is_empty() {
        format!("unexpected token in plural form: {value}")
    } else {
        "unexpected end of plural form".into()
    }
}

fn binary_op(tok: &str) -> Option<i32> {
    Some(match tok {
        "||" => 1,
        "&&" => 2,
        "==" | "!=" => 3,
        "<" | ">" | "<=" | ">=" => 4,
        "+" | "-" => 5,
        "*" | "/" | "%" => 6,
        _ => return None,
    })
}

/// `gettext._parse`: the Python source text for the C expression.
fn parse_c(tokens: &mut Tokens, priority: i32) -> Result<(String, String), String> {
    let mut result = String::new();
    let mut nexttok = tokens.next()?;
    while nexttok == "!" {
        result += "not ";
        nexttok = tokens.next()?;
    }

    if nexttok == "(" {
        let (sub, nt) = parse_c(tokens, -1)?;
        nexttok = nt;
        result = format!("{result}({sub})");
        if nexttok != ")" {
            return Err("unbalanced parenthesis in plural form".into());
        }
    } else if nexttok == "n" {
        result = format!("{result}{nexttok}");
    } else {
        let value: BigInt = if !nexttok.is_empty() && nexttok.bytes().all(|b| b.is_ascii_digit()) {
            nexttok.parse().map_err(|_| error(&nexttok))?
        } else {
            return Err(error(&nexttok));
        };
        result = format!("{result}{value}");
    }
    // Unary '!' binds tighter than binary operators in C, unlike 'not'.
    // (CPython 3.14.8, gh-157451; before it, `!n == 1` meant `!(n == 1)`.)
    if result.starts_with("not ") {
        result = format!("({result})");
    }
    nexttok = tokens.next()?;

    let mut j = 100;
    while let Some(i) = binary_op(&nexttok) {
        if i < priority {
            break;
        }
        // Break chained comparisons
        if (i == 3 || i == 4) && (j == 3 || j == 4) {
            // '==', '!=', '<', '>', '<=', '>='
            result = format!("({result})");
        }
        // Replace some C operators by their Python equivalents
        let op = match nexttok.as_str() {
            "||" => "or",
            "&&" => "and",
            "/" => "//",
            o => o,
        }
        .to_string();
        let (right, nt) = parse_c(tokens, i + 1)?;
        nexttok = nt;
        result = format!("{result} {op} {right}");
        j = i;
    }
    if j == priority && priority == 4 {
        // '<', '>', '<=', '>='
        result = format!("({result})");
    }

    if nexttok == "?" && priority <= 0 {
        let (if_true, nt) = parse_c(tokens, 0)?;
        nexttok = nt;
        if nexttok != ":" {
            return Err(error(&nexttok));
        }
        let (if_false, nt) = parse_c(tokens, -1)?;
        nexttok = nt;
        result = format!("{if_true} if {result} else {if_false}");
        if priority == 0 {
            result = format!("({result})");
        }
    }

    Ok((result, nexttok))
}

/// `gettext.c2py`: Gets a C expression as used in PO files for plural forms
/// and returns a function that implements an equivalent expression.
///
/// Parsing recurses once per `!` or nested operand, up to the 1000
/// characters an expression may have: on a thread with room for that.
fn c2py(plural: &str) -> Result<Expr, String> {
    crate::util::with_stack(crate::util::DEEP_STACK, || c2py_here(plural))
}

fn c2py_here(plural: &str) -> Result<Expr, String> {
    if plural.chars().count() > 1000 {
        return Err("plural form expression is too long".into());
    }
    let mut tokens = Tokens::new(plural);
    let (result, nexttok) = parse_c(&mut tokens, -1)?;
    if !nexttok.is_empty() {
        return Err(error(&nexttok));
    }

    let mut depth = 0;
    for c in result.chars() {
        if c == '(' {
            depth += 1;
            if depth > 20 {
                // Python compiler limit is about 90.
                // The most complex example has 2.
                return Err("plural form expression is too complex".into());
            }
        } else if c == ')' {
            depth -= 1;
        }
    }
    // exec('def func(n): return int(%s)' % result): compile the text as Python.
    py_compile(&result)
}

/// A Python expression over `n`, as `c2py` writes them.
#[derive(Debug, Clone)]
enum Expr {
    N,
    Num(BigInt),
    Not(Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    IfElse(Box<Expr>, Box<Expr>, Box<Expr>),
    /// `a op b op c`: Python chains comparisons.
    Compare(Box<Expr>, Vec<(String, Expr)>),
    Bin(String, Box<Expr>, Box<Expr>),
}

fn py_tokens(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let cs: Vec<char> = src.chars().collect();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if c == ' ' {
            i += 1;
        } else if c.is_ascii_alphanumeric() {
            let s = i;
            while i < cs.len() && cs[i].is_ascii_alphanumeric() {
                i += 1;
            }
            out.push(cs[s..i].iter().collect());
        } else {
            let two: String = cs[i..(i + 2).min(cs.len())].iter().collect();
            if ["==", "!=", "<=", ">=", "//"].contains(&two.as_str()) {
                out.push(two);
                i += 2;
            } else {
                out.push(c.to_string());
                i += 1;
            }
        }
    }
    out
}

struct PyParser {
    toks: Vec<String>,
    pos: usize,
}

const SYNTAX: &str = "invalid syntax";

impl PyParser {
    fn peek(&self) -> &str {
        self.toks.get(self.pos).map(|s| s.as_str()).unwrap_or("")
    }
    fn eat(&mut self, t: &str) -> bool {
        if self.peek() == t {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    // expr := or_test ['if' or_test 'else' expr]
    fn expr(&mut self) -> Result<Expr, String> {
        let body = self.or_test()?;
        if self.eat("if") {
            let cond = self.or_test()?;
            if !self.eat("else") {
                return Err(SYNTAX.into());
            }
            let orelse = self.expr()?;
            return Ok(Expr::IfElse(Box::new(cond), Box::new(body), Box::new(orelse)));
        }
        Ok(body)
    }
    fn or_test(&mut self) -> Result<Expr, String> {
        let mut e = self.and_test()?;
        while self.eat("or") {
            e = Expr::Or(Box::new(e), Box::new(self.and_test()?));
        }
        Ok(e)
    }
    fn and_test(&mut self) -> Result<Expr, String> {
        let mut e = self.not_test()?;
        while self.eat("and") {
            e = Expr::And(Box::new(e), Box::new(self.not_test()?));
        }
        Ok(e)
    }
    fn not_test(&mut self) -> Result<Expr, String> {
        if self.eat("not") {
            return Ok(Expr::Not(Box::new(self.not_test()?)));
        }
        self.comparison()
    }
    fn comparison(&mut self) -> Result<Expr, String> {
        let first = self.arith()?;
        let mut rest = Vec::new();
        while ["==", "!=", "<", ">", "<=", ">="].contains(&self.peek()) {
            let op = self.peek().to_string();
            self.pos += 1;
            rest.push((op, self.arith()?));
        }
        Ok(if rest.is_empty() { first } else { Expr::Compare(Box::new(first), rest) })
    }
    fn arith(&mut self) -> Result<Expr, String> {
        let mut e = self.term()?;
        while ["+", "-"].contains(&self.peek()) {
            let op = self.peek().to_string();
            self.pos += 1;
            e = Expr::Bin(op, Box::new(e), Box::new(self.term()?));
        }
        Ok(e)
    }
    fn term(&mut self) -> Result<Expr, String> {
        let mut e = self.atom()?;
        while ["*", "//", "%"].contains(&self.peek()) {
            let op = self.peek().to_string();
            self.pos += 1;
            e = Expr::Bin(op, Box::new(e), Box::new(self.atom()?));
        }
        Ok(e)
    }
    fn atom(&mut self) -> Result<Expr, String> {
        let t = self.peek().to_string();
        self.pos += 1;
        if t == "(" {
            let e = self.expr()?;
            if !self.eat(")") {
                return Err(SYNTAX.into());
            }
            Ok(e)
        } else if t == "n" {
            Ok(Expr::N)
        } else if !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()) {
            t.parse().map(Expr::Num).map_err(|_| SYNTAX.to_string())
        } else {
            Err(SYNTAX.into())
        }
    }
}

fn py_compile(src: &str) -> Result<Expr, String> {
    let mut p = PyParser { toks: py_tokens(src), pos: 0 };
    let e = p.expr()?;
    if p.pos != p.toks.len() {
        return Err(SYNTAX.into());
    }
    Ok(e)
}

/// A Python number while a plural expression is evaluated: `n` is an int or
/// a float, and so is everything computed from it (a bool is an int).
#[derive(Debug, Clone)]
enum Value {
    Int(BigInt),
    Float(f64),
}

impl Value {
    fn truthy(&self) -> bool {
        match self {
            Value::Int(i) => *i != BigInt::from(0),
            // NaN is true
            Value::Float(f) => *f != 0.0,
        }
    }

    /// `float(x)`.
    fn to_f64(&self) -> Result<f64, String> {
        match self {
            Value::Float(f) => Ok(*f),
            // correctly rounded, as Python converts
            Value::Int(i) => match i.to_string().parse::<f64>() {
                Ok(f) if f.is_finite() => Ok(f),
                _ => Err("int too large to convert to float".into()),
            },
        }
    }
}

/// `int(f)`: truncated, exactly.
fn float_to_int(f: f64) -> Result<BigInt, String> {
    if f.is_nan() {
        return Err("cannot convert float NaN to integer".into());
    }
    if f.is_infinite() {
        return Err("cannot convert float infinity to integer".into());
    }
    format!("{:.0}", f.trunc()).parse::<BigInt>().map_err(|e| e.to_string())
}

/// Python's comparison of two numbers: exact between an int and a float;
/// None where a NaN makes every order false.
fn compare_values(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;
    let int_float = |i: &BigInt, f: f64| -> Option<Ordering> {
        if f.is_nan() {
            return None;
        }
        if f.is_infinite() {
            return Some(if f > 0.0 { Ordering::Less } else { Ordering::Greater });
        }
        let floor = f.floor();
        match i.cmp(&float_to_int(floor).ok()?) {
            Ordering::Equal if f > floor => Some(Ordering::Less),
            o => Some(o),
        }
    };
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Some(x.cmp(y)),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y),
        (Value::Int(x), Value::Float(y)) => int_float(x, *y),
        (Value::Float(x), Value::Int(y)) => int_float(y, *x).map(Ordering::reverse),
    }
}

/// `x // y` and `x % y` on floats, as CPython's `float_floor_div` and
/// `float_rem` compute them.
fn float_divmod(vx: f64, wx: f64) -> (f64, f64) {
    let mut m = vx % wx;
    let mut div = (vx - m) / wx;
    if m != 0.0 {
        // ensure the remainder has the same sign as the denominator
        if (wx < 0.0) != (m < 0.0) {
            m += wx;
            div -= 1.0;
        }
    } else {
        // the remainder is zero: it takes the sign of the denominator
        m = 0.0f64.copysign(wx);
    }
    let floordiv = if div != 0.0 {
        let mut f = div.floor();
        if div - f > 0.5 {
            f += 1.0;
        }
        f
    } else {
        // div is zero: get the correct sign
        0.0f64.copysign(vx / wx)
    };
    (floordiv, m)
}

impl Expr {
    /// The function `c2py` builds:
    ///
    /// ```text
    /// def func(n):
    ///     if not isinstance(n, int):
    ///         n = _as_int(n)
    ///     return int(<expr>)
    /// ```
    ///
    /// `_as_int` raises TypeError for what is not a number and fails where
    /// `round(n)` does (NaN, infinity); otherwise it warns (`warned`) and
    /// keeps the float, which the expression then works on.
    fn call(&self, n: &PluralN, warned: &mut bool) -> Result<BigInt, String> {
        // Evaluation recurses once per level of the expression: a real
        // catalog's few levels here, a pathological one's (`!!!…!n`, up to
        // ~1000) on a thread with room for them.
        if self.depth() > 64 {
            return crate::util::with_stack(crate::util::DEEP_STACK, || self.call_here(n, warned));
        }
        self.call_here(n, warned)
    }

    /// How many levels deep the expression goes.
    fn depth(&self) -> usize {
        let mut deepest = 0;
        let mut todo = vec![(self, 1)];
        while let Some((e, d)) = todo.pop() {
            deepest = deepest.max(d);
            match e {
                Expr::N | Expr::Num(_) => {}
                Expr::Not(a) => todo.push((a, d + 1)),
                Expr::Or(a, b) | Expr::And(a, b) | Expr::Bin(_, a, b) => todo.extend([(&**a, d + 1), (&**b, d + 1)]),
                Expr::IfElse(a, b, c) => todo.extend([(&**a, d + 1), (&**b, d + 1), (&**c, d + 1)]),
                Expr::Compare(a, rest) => {
                    todo.push((a, d + 1));
                    todo.extend(rest.iter().map(|(_, x)| (x, d + 1)));
                }
            }
        }
        deepest
    }

    fn call_here(&self, n: &PluralN, warned: &mut bool) -> Result<BigInt, String> {
        let v = match n {
            PluralN::Int(i) => Value::Int(i.clone()),
            PluralN::Float(f) => {
                float_to_int(*f)?;
                *warned = true;
                Value::Float(*f)
            }
            PluralN::Other(t) => return Err(format!("Plural value must be an integer, got {t}")),
        };
        match self.eval(&v)? {
            Value::Int(i) => Ok(i),
            Value::Float(f) => float_to_int(f),
        }
    }

    /// The expression with Python's semantics: unbounded ints (`//` and `%`
    /// floor), floats where `n` is one, operands given back by `or`/`and`.
    fn eval(&self, n: &Value) -> Result<Value, String> {
        use std::cmp::Ordering;
        let zero = BigInt::from(0);
        let truth = |b: bool| Value::Int(BigInt::from(b as i32));
        Ok(match self {
            Expr::N => n.clone(),
            Expr::Num(v) => Value::Int(v.clone()),
            Expr::Not(e) => truth(!e.eval(n)?.truthy()),
            // `or` / `and` give back an operand, as Python's do
            Expr::Or(a, b) => {
                let x = a.eval(n)?;
                if x.truthy() { x } else { b.eval(n)? }
            }
            Expr::And(a, b) => {
                let x = a.eval(n)?;
                if !x.truthy() { x } else { b.eval(n)? }
            }
            Expr::IfElse(c, t, f) => {
                if c.eval(n)?.truthy() { t.eval(n)? } else { f.eval(n)? }
            }
            Expr::Compare(first, rest) => {
                let mut left = first.eval(n)?;
                for (op, e) in rest {
                    let right = e.eval(n)?;
                    let o = compare_values(&left, &right);
                    let ok = match op.as_str() {
                        "==" => o == Some(Ordering::Equal),
                        "!=" => o != Some(Ordering::Equal),
                        "<" => o == Some(Ordering::Less),
                        ">" => o == Some(Ordering::Greater),
                        "<=" => matches!(o, Some(Ordering::Less | Ordering::Equal)),
                        _ => matches!(o, Some(Ordering::Greater | Ordering::Equal)),
                    };
                    if !ok {
                        return Ok(truth(false));
                    }
                    left = right;
                }
                truth(true)
            }
            Expr::Bin(op, a, b) => match (a.eval(n)?, b.eval(n)?) {
                (Value::Int(x), Value::Int(y)) => Value::Int(match op.as_str() {
                    "+" => x + y,
                    "-" => x - y,
                    "*" => x * y,
                    // floor division and modulo: the quotient rounds down,
                    // the remainder takes the sign of the divisor
                    "//" => {
                        if y == zero {
                            return Err("integer division or modulo by zero".into());
                        }
                        let (q, r) = (&x / &y, &x % &y);
                        if r != zero && ((r < zero) != (y < zero)) { q - 1 } else { q }
                    }
                    _ => {
                        if y == zero {
                            return Err("integer modulo by zero".into());
                        }
                        let r = &x % &y;
                        if r != zero && ((r < zero) != (y < zero)) { r + y } else { r }
                    }
                }),
                (x, y) => {
                    let (x, y) = (x.to_f64()?, y.to_f64()?);
                    Value::Float(match op.as_str() {
                        "+" => x + y,
                        "-" => x - y,
                        "*" => x * y,
                        "//" => {
                            if y == 0.0 {
                                return Err("float floor division by zero".into());
                            }
                            float_divmod(x, y).0
                        }
                        _ => {
                            if y == 0.0 {
                                return Err("float modulo by zero".into());
                            }
                            float_divmod(x, y).1
                        }
                    })
                }
            },
        })
    }
}


/// Python's `locale.locale_alias` (CPython 3.14), verbatim.
const LOCALE_ALIAS: &[(&str, &str)] = &[
    ("a3", "az_AZ.KOI8-C"), ("a3_az", "az_AZ.KOI8-C"), ("a3_az.koic", "az_AZ.KOI8-C"),
    ("aa_dj", "aa_DJ.ISO8859-1"), ("aa_er", "aa_ER.UTF-8"), ("aa_et", "aa_ET.UTF-8"),
    ("af", "af_ZA.ISO8859-1"), ("af_za", "af_ZA.ISO8859-1"), ("agr_pe", "agr_PE.UTF-8"),
    ("ak_gh", "ak_GH.UTF-8"), ("am", "am_ET.UTF-8"), ("am_et", "am_ET.UTF-8"),
    ("american", "en_US.ISO8859-1"), ("an_es", "an_ES.ISO8859-15"), ("anp_in", "anp_IN.UTF-8"),
    ("ar", "ar_AA.ISO8859-6"), ("ar_aa", "ar_AA.ISO8859-6"), ("ar_ae", "ar_AE.ISO8859-6"),
    ("ar_bh", "ar_BH.ISO8859-6"), ("ar_dz", "ar_DZ.ISO8859-6"), ("ar_eg", "ar_EG.ISO8859-6"),
    ("ar_in", "ar_IN.UTF-8"), ("ar_iq", "ar_IQ.ISO8859-6"), ("ar_jo", "ar_JO.ISO8859-6"),
    ("ar_kw", "ar_KW.ISO8859-6"), ("ar_lb", "ar_LB.ISO8859-6"), ("ar_ly", "ar_LY.ISO8859-6"),
    ("ar_ma", "ar_MA.ISO8859-6"), ("ar_om", "ar_OM.ISO8859-6"), ("ar_qa", "ar_QA.ISO8859-6"),
    ("ar_sa", "ar_SA.ISO8859-6"), ("ar_sd", "ar_SD.ISO8859-6"), ("ar_ss", "ar_SS.UTF-8"),
    ("ar_sy", "ar_SY.ISO8859-6"), ("ar_tn", "ar_TN.ISO8859-6"), ("ar_ye", "ar_YE.ISO8859-6"),
    ("arabic", "ar_AA.ISO8859-6"), ("as", "as_IN.UTF-8"), ("as_in", "as_IN.UTF-8"),
    ("ast_es", "ast_ES.ISO8859-15"), ("ayc_pe", "ayc_PE.UTF-8"), ("az", "az_AZ.ISO8859-9E"),
    ("az_az", "az_AZ.ISO8859-9E"), ("az_az.iso88599e", "az_AZ.ISO8859-9E"),
    ("az_ir", "az_IR.UTF-8"), ("be", "be_BY.CP1251"), ("be@latin", "be_BY.UTF-8@latin"),
    ("be_bg.utf8", "bg_BG.UTF-8"), ("be_by", "be_BY.CP1251"), ("be_by@latin", "be_BY.UTF-8@latin"),
    ("bem_zm", "bem_ZM.UTF-8"), ("ber_dz", "ber_DZ.UTF-8"), ("ber_ma", "ber_MA.UTF-8"),
    ("bg", "bg_BG.CP1251"), ("bg_bg", "bg_BG.CP1251"), ("bhb_in.utf8", "bhb_IN.UTF-8"),
    ("bho_in", "bho_IN.UTF-8"), ("bho_np", "bho_NP.UTF-8"), ("bi_vu", "bi_VU.UTF-8"),
    ("bn_bd", "bn_BD.UTF-8"), ("bn_in", "bn_IN.UTF-8"), ("bo_cn", "bo_CN.UTF-8"),
    ("bo_in", "bo_IN.UTF-8"), ("bokmal", "nb_NO.ISO8859-1"), ("bokmål", "nb_NO.ISO8859-1"),
    ("br", "br_FR.ISO8859-1"), ("br_fr", "br_FR.ISO8859-1"), ("brx_in", "brx_IN.UTF-8"),
    ("bs", "bs_BA.ISO8859-2"), ("bs_ba", "bs_BA.ISO8859-2"), ("bulgarian", "bg_BG.CP1251"),
    ("byn_er", "byn_ER.UTF-8"), ("c", "C"), ("c-french", "fr_CA.ISO8859-1"), ("c.ascii", "C"),
    ("c.en", "C"), ("c.iso88591", "en_US.ISO8859-1"), ("c_c", "C"), ("c_c.c", "C"),
    ("ca", "ca_ES.ISO8859-1"), ("ca_ad", "ca_AD.ISO8859-1"), ("ca_es", "ca_ES.ISO8859-1"),
    ("ca_es@valencia", "ca_ES.UTF-8@valencia"), ("ca_fr", "ca_FR.ISO8859-1"),
    ("ca_it", "ca_IT.ISO8859-1"), ("catalan", "ca_ES.ISO8859-1"), ("ce_ru", "ce_RU.UTF-8"),
    ("cextend", "en_US.ISO8859-1"), ("chinese-s", "zh_CN.eucCN"), ("chinese-t", "zh_TW.eucTW"),
    ("chr_us", "chr_US.UTF-8"), ("ckb_iq", "ckb_IQ.UTF-8"), ("cmn_tw", "cmn_TW.UTF-8"),
    ("crh_ru", "crh_RU.UTF-8"), ("crh_ua", "crh_UA.UTF-8"), ("croatian", "hr_HR.ISO8859-2"),
    ("cs", "cs_CZ.ISO8859-2"), ("cs_cs", "cs_CZ.ISO8859-2"), ("cs_cz", "cs_CZ.ISO8859-2"),
    ("csb_pl", "csb_PL.UTF-8"), ("cv_ru", "cv_RU.UTF-8"), ("cy", "cy_GB.ISO8859-1"),
    ("cy_gb", "cy_GB.ISO8859-1"), ("cz", "cs_CZ.ISO8859-2"), ("cz_cz", "cs_CZ.ISO8859-2"),
    ("czech", "cs_CZ.ISO8859-2"), ("da", "da_DK.ISO8859-1"), ("da_dk", "da_DK.ISO8859-1"),
    ("danish", "da_DK.ISO8859-1"), ("dansk", "da_DK.ISO8859-1"), ("de", "de_DE.ISO8859-1"),
    ("de_at", "de_AT.ISO8859-1"), ("de_be", "de_BE.ISO8859-1"), ("de_ch", "de_CH.ISO8859-1"),
    ("de_de", "de_DE.ISO8859-1"), ("de_it", "de_IT.UTF-8"), ("de_li", "de_LI.ISO8859-1"),
    ("de_lu", "de_LU.ISO8859-1"), ("deutsch", "de_DE.ISO8859-1"), ("doi_in", "doi_IN.UTF-8"),
    ("dsb_de", "dsb_DE.UTF-8"), ("dutch", "nl_NL.ISO8859-1"),
    ("dutch.iso88591", "nl_BE.ISO8859-1"), ("dv_mv", "dv_MV.UTF-8"), ("dz_bt", "dz_BT.UTF-8"),
    ("ee", "ee_EE.ISO8859-4"), ("ee_ee", "ee_EE.ISO8859-4"), ("eesti", "et_EE.ISO8859-1"),
    ("el", "el_GR.ISO8859-7"), ("el_cy", "el_CY.ISO8859-7"), ("el_gr", "el_GR.ISO8859-7"),
    ("el_gr@euro", "el_GR.ISO8859-15"), ("en", "en_US.ISO8859-1"), ("en_ag", "en_AG.UTF-8"),
    ("en_au", "en_AU.ISO8859-1"), ("en_be", "en_BE.ISO8859-1"), ("en_bw", "en_BW.ISO8859-1"),
    ("en_ca", "en_CA.ISO8859-1"), ("en_dk", "en_DK.ISO8859-1"), ("en_dl.utf8", "en_DL.UTF-8"),
    ("en_gb", "en_GB.ISO8859-1"), ("en_hk", "en_HK.ISO8859-1"), ("en_ie", "en_IE.ISO8859-1"),
    ("en_il", "en_IL.ISO8859-1"), ("en_in", "en_IN.ISO8859-1"), ("en_ng", "en_NG.UTF-8"),
    ("en_nz", "en_NZ.ISO8859-1"), ("en_ph", "en_PH.ISO8859-1"), ("en_sc.utf8", "en_SC.UTF-8"),
    ("en_sg", "en_SG.ISO8859-1"), ("en_uk", "en_GB.ISO8859-1"), ("en_us", "en_US.ISO8859-1"),
    ("en_us@euro@euro", "en_US.ISO8859-15"), ("en_za", "en_ZA.ISO8859-1"),
    ("en_zm", "en_ZM.UTF-8"), ("en_zw", "en_ZW.ISO8859-1"), ("en_zw.utf8", "en_ZS.UTF-8"),
    ("eng_gb", "en_GB.ISO8859-1"), ("english", "en_EN.ISO8859-1"),
    ("english_uk", "en_GB.ISO8859-1"), ("english_united-states", "en_US.ISO8859-1"),
    ("english_united-states.437", "C"), ("english_us", "en_US.ISO8859-1"),
    ("eo", "eo_XX.ISO8859-3"), ("eo.utf8", "eo.UTF-8"), ("eo_eo", "eo_EO.ISO8859-3"),
    ("eo_us.utf8", "eo_US.UTF-8"), ("eo_xx", "eo_XX.ISO8859-3"), ("es", "es_ES.ISO8859-1"),
    ("es_ar", "es_AR.ISO8859-1"), ("es_bo", "es_BO.ISO8859-1"), ("es_cl", "es_CL.ISO8859-1"),
    ("es_co", "es_CO.ISO8859-1"), ("es_cr", "es_CR.ISO8859-1"), ("es_cu", "es_CU.ISO8859-1"),
    ("es_do", "es_DO.ISO8859-1"), ("es_ec", "es_EC.ISO8859-1"), ("es_es", "es_ES.ISO8859-1"),
    ("es_gt", "es_GT.ISO8859-1"), ("es_hn", "es_HN.ISO8859-1"), ("es_mx", "es_MX.ISO8859-1"),
    ("es_ni", "es_NI.ISO8859-1"), ("es_pa", "es_PA.ISO8859-1"), ("es_pe", "es_PE.ISO8859-1"),
    ("es_pr", "es_PR.ISO8859-1"), ("es_py", "es_PY.ISO8859-1"), ("es_sv", "es_SV.ISO8859-1"),
    ("es_us", "es_US.ISO8859-1"), ("es_uy", "es_UY.ISO8859-1"), ("es_ve", "es_VE.ISO8859-1"),
    ("estonian", "et_EE.ISO8859-1"), ("et", "et_EE.ISO8859-15"), ("et_ee", "et_EE.ISO8859-15"),
    ("eu", "eu_ES.ISO8859-1"), ("eu_es", "eu_ES.ISO8859-1"), ("eu_fr", "eu_FR.ISO8859-1"),
    ("fa", "fa_IR.UTF-8"), ("fa_ir", "fa_IR.UTF-8"), ("fa_ir.isiri3342", "fa_IR.ISIRI-3342"),
    ("ff_sn", "ff_SN.UTF-8"), ("fi", "fi_FI.ISO8859-15"), ("fi_fi", "fi_FI.ISO8859-15"),
    ("fil_ph", "fil_PH.UTF-8"), ("finnish", "fi_FI.ISO8859-1"), ("fo", "fo_FO.ISO8859-1"),
    ("fo_fo", "fo_FO.ISO8859-1"), ("fr", "fr_FR.ISO8859-1"), ("fr_be", "fr_BE.ISO8859-1"),
    ("fr_ca", "fr_CA.ISO8859-1"), ("fr_ch", "fr_CH.ISO8859-1"), ("fr_fr", "fr_FR.ISO8859-1"),
    ("fr_lu", "fr_LU.ISO8859-1"), ("français", "fr_FR.ISO8859-1"), ("fre_fr", "fr_FR.ISO8859-1"),
    ("french", "fr_FR.ISO8859-1"), ("french.iso88591", "fr_CH.ISO8859-1"),
    ("french_france", "fr_FR.ISO8859-1"), ("fur_it", "fur_IT.UTF-8"), ("fy_de", "fy_DE.UTF-8"),
    ("fy_nl", "fy_NL.UTF-8"), ("ga", "ga_IE.ISO8859-1"), ("ga_ie", "ga_IE.ISO8859-1"),
    ("galego", "gl_ES.ISO8859-1"), ("galician", "gl_ES.ISO8859-1"), ("gbm_in", "gbm_IN.UTF-8"),
    ("gd", "gd_GB.ISO8859-1"), ("gd_gb", "gd_GB.ISO8859-1"), ("ger_de", "de_DE.ISO8859-1"),
    ("german", "de_DE.ISO8859-1"), ("german.iso88591", "de_CH.ISO8859-1"),
    ("german_germany", "de_DE.ISO8859-1"), ("gez_er", "gez_ER.UTF-8"), ("gez_et", "gez_ET.UTF-8"),
    ("gl", "gl_ES.ISO8859-1"), ("gl_es", "gl_ES.ISO8859-1"), ("greek", "el_GR.ISO8859-7"),
    ("gu_in", "gu_IN.UTF-8"), ("gv", "gv_GB.ISO8859-1"), ("gv_gb", "gv_GB.ISO8859-1"),
    ("ha_ng", "ha_NG.UTF-8"), ("hak_tw", "hak_TW.UTF-8"), ("he", "he_IL.ISO8859-8"),
    ("he_il", "he_IL.ISO8859-8"), ("hebrew", "he_IL.ISO8859-8"), ("hi", "hi_IN.ISCII-DEV"),
    ("hi_in", "hi_IN.ISCII-DEV"), ("hi_in.isciidev", "hi_IN.ISCII-DEV"),
    ("hif_fj", "hif_FJ.UTF-8"), ("hne", "hne_IN.UTF-8"), ("hne_in", "hne_IN.UTF-8"),
    ("hr", "hr_HR.ISO8859-2"), ("hr_hr", "hr_HR.ISO8859-2"), ("hrvatski", "hr_HR.ISO8859-2"),
    ("hsb_de", "hsb_DE.ISO8859-2"), ("ht_ht", "ht_HT.UTF-8"), ("hu", "hu_HU.ISO8859-2"),
    ("hu_hu", "hu_HU.ISO8859-2"), ("hungarian", "hu_HU.ISO8859-2"), ("hy_am", "hy_AM.UTF-8"),
    ("hy_am.armscii8", "hy_AM.ARMSCII_8"), ("ia", "ia.UTF-8"), ("ia_fr", "ia_FR.UTF-8"),
    ("icelandic", "is_IS.ISO8859-1"), ("id", "id_ID.ISO8859-1"), ("id_id", "id_ID.ISO8859-1"),
    ("ie", "ie.UTF-8"), ("ig_ng", "ig_NG.UTF-8"), ("ik_ca", "ik_CA.UTF-8"),
    ("in", "id_ID.ISO8859-1"), ("in_id", "id_ID.ISO8859-1"), ("is", "is_IS.ISO8859-1"),
    ("is_is", "is_IS.ISO8859-1"), ("iso-8859-1", "en_US.ISO8859-1"),
    ("iso-8859-15", "en_US.ISO8859-15"), ("iso8859-1", "en_US.ISO8859-1"),
    ("iso8859-15", "en_US.ISO8859-15"), ("iso_8859_1", "en_US.ISO8859-1"),
    ("iso_8859_15", "en_US.ISO8859-15"), ("it", "it_IT.ISO8859-1"), ("it_ch", "it_CH.ISO8859-1"),
    ("it_it", "it_IT.ISO8859-1"), ("italian", "it_IT.ISO8859-1"), ("iu", "iu_CA.NUNACOM-8"),
    ("iu_ca", "iu_CA.NUNACOM-8"), ("iu_ca.nunacom8", "iu_CA.NUNACOM-8"), ("iw", "he_IL.ISO8859-8"),
    ("iw_il", "he_IL.ISO8859-8"), ("iw_il.utf8", "iw_IL.UTF-8"), ("ja", "ja_JP.eucJP"),
    ("ja_jp", "ja_JP.eucJP"), ("ja_jp.euc", "ja_JP.eucJP"), ("ja_jp.mscode", "ja_JP.SJIS"),
    ("ja_jp.pck", "ja_JP.SJIS"), ("japan", "ja_JP.eucJP"), ("japanese", "ja_JP.eucJP"),
    ("japanese-euc", "ja_JP.eucJP"), ("japanese.euc", "ja_JP.eucJP"), ("jp_jp", "ja_JP.eucJP"),
    ("ka", "ka_GE.GEORGIAN-ACADEMY"), ("ka_ge", "ka_GE.GEORGIAN-ACADEMY"),
    ("ka_ge.georgianacademy", "ka_GE.GEORGIAN-ACADEMY"), ("ka_ge.georgianps", "ka_GE.GEORGIAN-PS"),
    ("ka_ge.georgianrs", "ka_GE.GEORGIAN-ACADEMY"), ("kab_dz", "kab_DZ.UTF-8"),
    ("kk_kz", "kk_KZ.ptcp154"), ("kl", "kl_GL.ISO8859-1"), ("kl_gl", "kl_GL.ISO8859-1"),
    ("km_kh", "km_KH.UTF-8"), ("kn", "kn_IN.UTF-8"), ("kn_in", "kn_IN.UTF-8"),
    ("ko", "ko_KR.eucKR"), ("ko_kr", "ko_KR.eucKR"), ("ko_kr.euc", "ko_KR.eucKR"),
    ("kok_in", "kok_IN.UTF-8"), ("korean", "ko_KR.eucKR"), ("korean.euc", "ko_KR.eucKR"),
    ("ks", "ks_IN.UTF-8"), ("ks_in", "ks_IN.UTF-8"),
    ("ks_in@devanagari.utf8", "ks_IN.UTF-8@devanagari"), ("ku_tr", "ku_TR.ISO8859-9"),
    ("kv_ru", "kv_RU.UTF-8"), ("kw", "kw_GB.ISO8859-1"), ("kw_gb", "kw_GB.ISO8859-1"),
    ("ky", "ky_KG.UTF-8"), ("ky_kg", "ky_KG.UTF-8"), ("lb_lu", "lb_LU.UTF-8"),
    ("lg_ug", "lg_UG.ISO8859-10"), ("li_be", "li_BE.UTF-8"), ("li_nl", "li_NL.UTF-8"),
    ("lij_it", "lij_IT.UTF-8"), ("lithuanian", "lt_LT.ISO8859-13"), ("ln_cd", "ln_CD.UTF-8"),
    ("lo", "lo_LA.MULELAO-1"), ("lo_la", "lo_LA.MULELAO-1"), ("lo_la.cp1133", "lo_LA.IBM-CP1133"),
    ("lo_la.ibmcp1133", "lo_LA.IBM-CP1133"), ("lo_la.mulelao1", "lo_LA.MULELAO-1"),
    ("lt", "lt_LT.ISO8859-13"), ("lt_lt", "lt_LT.ISO8859-13"), ("ltg_lv.utf8", "ltg_LV.UTF-8"),
    ("lv", "lv_LV.ISO8859-13"), ("lv_lv", "lv_LV.ISO8859-13"), ("lzh_tw", "lzh_TW.UTF-8"),
    ("mag_in", "mag_IN.UTF-8"), ("mai", "mai_IN.UTF-8"), ("mai_in", "mai_IN.UTF-8"),
    ("mai_np", "mai_NP.UTF-8"), ("mdf_ru", "mdf_RU.UTF-8"), ("mfe_mu", "mfe_MU.UTF-8"),
    ("mg_mg", "mg_MG.ISO8859-15"), ("mhr_ru", "mhr_RU.UTF-8"), ("mi", "mi_NZ.ISO8859-1"),
    ("mi_nz", "mi_NZ.ISO8859-1"), ("miq_ni", "miq_NI.UTF-8"), ("mjw_in", "mjw_IN.UTF-8"),
    ("mk", "mk_MK.ISO8859-5"), ("mk_mk", "mk_MK.ISO8859-5"), ("ml", "ml_IN.UTF-8"),
    ("ml_in", "ml_IN.UTF-8"), ("mn_mn", "mn_MN.UTF-8"), ("mni_in", "mni_IN.UTF-8"),
    ("mnw_mm", "mnw_MM.UTF-8"), ("mr", "mr_IN.UTF-8"), ("mr_in", "mr_IN.UTF-8"),
    ("ms", "ms_MY.ISO8859-1"), ("ms_my", "ms_MY.ISO8859-1"), ("mt", "mt_MT.ISO8859-3"),
    ("mt_mt", "mt_MT.ISO8859-3"), ("my_mm", "my_MM.UTF-8"), ("nan_tw", "nan_TW.UTF-8"),
    ("nb", "nb_NO.ISO8859-1"), ("nb_no", "nb_NO.ISO8859-1"), ("nds_de", "nds_DE.UTF-8"),
    ("nds_nl", "nds_NL.UTF-8"), ("ne_np", "ne_NP.UTF-8"), ("nhn_mx", "nhn_MX.UTF-8"),
    ("niu_nu", "niu_NU.UTF-8"), ("niu_nz", "niu_NZ.UTF-8"), ("nl", "nl_NL.ISO8859-1"),
    ("nl_aw", "nl_AW.UTF-8"), ("nl_be", "nl_BE.ISO8859-1"), ("nl_nl", "nl_NL.ISO8859-1"),
    ("nn", "nn_NO.ISO8859-1"), ("nn_no", "nn_NO.ISO8859-1"), ("no", "no_NO.ISO8859-1"),
    ("no@nynorsk", "ny_NO.ISO8859-1"), ("no_no", "no_NO.ISO8859-1"),
    ("no_no.iso88591@bokmal", "no_NO.ISO8859-1"), ("no_no.iso88591@nynorsk", "no_NO.ISO8859-1"),
    ("norwegian", "no_NO.ISO8859-1"), ("nr", "nr_ZA.ISO8859-1"), ("nr_za", "nr_ZA.ISO8859-1"),
    ("nso", "nso_ZA.ISO8859-15"), ("nso_za", "nso_ZA.ISO8859-15"), ("ny", "ny_NO.ISO8859-1"),
    ("ny_no", "ny_NO.ISO8859-1"), ("nynorsk", "nn_NO.ISO8859-1"), ("oc", "oc_FR.ISO8859-1"),
    ("oc_fr", "oc_FR.ISO8859-1"), ("om_et", "om_ET.UTF-8"), ("om_ke", "om_KE.ISO8859-1"),
    ("or", "or_IN.UTF-8"), ("or_in", "or_IN.UTF-8"), ("os_ru", "os_RU.UTF-8"),
    ("pa", "pa_IN.UTF-8"), ("pa_in", "pa_IN.UTF-8"), ("pa_pk", "pa_PK.UTF-8"),
    ("pap_an", "pap_AN.UTF-8"), ("pap_aw", "pap_AW.UTF-8"), ("pap_cw", "pap_CW.UTF-8"),
    ("pd", "pd_US.ISO8859-1"), ("pd_de", "pd_DE.ISO8859-1"), ("pd_us", "pd_US.ISO8859-1"),
    ("ph", "ph_PH.ISO8859-1"), ("ph_ph", "ph_PH.ISO8859-1"), ("pl", "pl_PL.ISO8859-2"),
    ("pl_pl", "pl_PL.ISO8859-2"), ("polish", "pl_PL.ISO8859-2"), ("portuguese", "pt_PT.ISO8859-1"),
    ("portuguese_brazil", "pt_BR.ISO8859-1"), ("posix", "C"), ("posix-utf2", "C"),
    ("pp", "pp_AN.ISO8859-1"), ("pp_an", "pp_AN.ISO8859-1"), ("ps_af", "ps_AF.UTF-8"),
    ("pt", "pt_PT.ISO8859-1"), ("pt_br", "pt_BR.ISO8859-1"), ("pt_pt", "pt_PT.ISO8859-1"),
    ("quz_pe", "quz_PE.UTF-8"), ("raj_in", "raj_IN.UTF-8"), ("rif_ma", "rif_MA.UTF-8"),
    ("ro", "ro_RO.ISO8859-2"), ("ro_ro", "ro_RO.ISO8859-2"), ("romanian", "ro_RO.ISO8859-2"),
    ("ru", "ru_RU.UTF-8"), ("ru_ru", "ru_RU.UTF-8"), ("ru_ua", "ru_UA.KOI8-U"),
    ("rumanian", "ro_RO.ISO8859-2"), ("russian", "ru_RU.ISO8859-5"), ("rw", "rw_RW.ISO8859-1"),
    ("rw_rw", "rw_RW.ISO8859-1"), ("sa_in", "sa_IN.UTF-8"), ("sah_ru", "sah_RU.UTF-8"),
    ("sat_in", "sat_IN.UTF-8"), ("sc_it", "sc_IT.UTF-8"), ("scn_it", "scn_IT.UTF-8"),
    ("sd", "sd_IN.UTF-8"), ("sd_in", "sd_IN.UTF-8"),
    ("sd_in@devanagari.utf8", "sd_IN.UTF-8@devanagari"), ("sd_pk", "sd_PK.UTF-8"),
    ("se_no", "se_NO.UTF-8"), ("serbocroatian", "sr_RS.UTF-8@latin"), ("sgs_lt", "sgs_LT.UTF-8"),
    ("sh", "sr_RS.UTF-8@latin"), ("sh_ba.iso88592@bosnia", "sr_CS.ISO8859-2"),
    ("sh_hr", "sh_HR.ISO8859-2"), ("sh_hr.iso88592", "hr_HR.ISO8859-2"),
    ("sh_sp", "sr_CS.ISO8859-2"), ("sh_yu", "sr_RS.UTF-8@latin"), ("shn_mm", "shn_MM.UTF-8"),
    ("shs_ca", "shs_CA.UTF-8"), ("si", "si_LK.UTF-8"), ("si_lk", "si_LK.UTF-8"),
    ("sid_et", "sid_ET.UTF-8"), ("sinhala", "si_LK.UTF-8"), ("sk", "sk_SK.ISO8859-2"),
    ("sk_sk", "sk_SK.ISO8859-2"), ("sl", "sl_SI.ISO8859-2"), ("sl_cs", "sl_CS.ISO8859-2"),
    ("sl_si", "sl_SI.ISO8859-2"), ("slovak", "sk_SK.ISO8859-2"), ("slovene", "sl_SI.ISO8859-2"),
    ("slovenian", "sl_SI.ISO8859-2"), ("sm_ws", "sm_WS.UTF-8"), ("so_dj", "so_DJ.ISO8859-1"),
    ("so_et", "so_ET.UTF-8"), ("so_ke", "so_KE.ISO8859-1"), ("so_so", "so_SO.ISO8859-1"),
    ("sp", "sr_CS.ISO8859-5"), ("sp_yu", "sr_CS.ISO8859-5"), ("spanish", "es_ES.ISO8859-1"),
    ("spanish_spain", "es_ES.ISO8859-1"), ("sq", "sq_AL.ISO8859-2"), ("sq_al", "sq_AL.ISO8859-2"),
    ("sq_mk", "sq_MK.UTF-8"), ("sr", "sr_RS.UTF-8"), ("sr@cyrillic", "sr_RS.UTF-8"),
    ("sr@latn", "sr_RS.UTF-8@latin"), ("sr_cs", "sr_CS.UTF-8"),
    ("sr_cs.iso88592@latn", "sr_CS.ISO8859-2"), ("sr_cs@latn", "sr_CS.UTF-8@latin"),
    ("sr_me", "sr_ME.UTF-8"), ("sr_rs", "sr_RS.UTF-8"), ("sr_rs@latn", "sr_RS.UTF-8@latin"),
    ("sr_sp", "sr_CS.ISO8859-2"), ("sr_yu", "sr_RS.UTF-8@latin"),
    ("sr_yu.cp1251@cyrillic", "sr_CS.CP1251"), ("sr_yu.iso88592", "sr_CS.ISO8859-2"),
    ("sr_yu.iso88595", "sr_CS.ISO8859-5"), ("sr_yu.iso88595@cyrillic", "sr_CS.ISO8859-5"),
    ("sr_yu.microsoftcp1251@cyrillic", "sr_CS.CP1251"), ("sr_yu.utf8", "sr_RS.UTF-8"),
    ("sr_yu.utf8@cyrillic", "sr_RS.UTF-8"), ("sr_yu@cyrillic", "sr_RS.UTF-8"),
    ("ss", "ss_ZA.ISO8859-1"), ("ss_za", "ss_ZA.ISO8859-1"), ("ssy_er", "ssy_ER.UTF-8"),
    ("st", "st_ZA.ISO8859-1"), ("st_za", "st_ZA.ISO8859-1"), ("su_id", "su_ID.UTF-8"),
    ("sv", "sv_SE.ISO8859-1"), ("sv_fi", "sv_FI.ISO8859-1"), ("sv_se", "sv_SE.ISO8859-1"),
    ("sw_ke", "sw_KE.UTF-8"), ("sw_tz", "sw_TZ.UTF-8"), ("swedish", "sv_SE.ISO8859-1"),
    ("syr", "syr.UTF-8"), ("szl_pl", "szl_PL.UTF-8"), ("ta", "ta_IN.TSCII-0"),
    ("ta_in", "ta_IN.TSCII-0"), ("ta_in.tscii", "ta_IN.TSCII-0"),
    ("ta_in.tscii0", "ta_IN.TSCII-0"), ("ta_lk", "ta_LK.UTF-8"), ("tcy_in.utf8", "tcy_IN.UTF-8"),
    ("te", "te_IN.UTF-8"), ("te_in", "te_IN.UTF-8"), ("tg", "tg_TJ.KOI8-C"),
    ("tg_tj", "tg_TJ.KOI8-C"), ("th", "th_TH.ISO8859-11"), ("th_th", "th_TH.ISO8859-11"),
    ("th_th.tactis", "th_TH.TIS620"), ("th_th.tis620", "th_TH.TIS620"),
    ("thai", "th_TH.ISO8859-11"), ("the_np", "the_NP.UTF-8"), ("ti_er", "ti_ER.UTF-8"),
    ("ti_et", "ti_ET.UTF-8"), ("tig_er", "tig_ER.UTF-8"), ("tk_tm", "tk_TM.UTF-8"),
    ("tl", "tl_PH.ISO8859-1"), ("tl_ph", "tl_PH.ISO8859-1"), ("tn", "tn_ZA.ISO8859-15"),
    ("tn_za", "tn_ZA.ISO8859-15"), ("to_to", "to_TO.UTF-8"), ("tok", "tok.UTF-8"),
    ("tpi_pg", "tpi_PG.UTF-8"), ("tr", "tr_TR.ISO8859-9"), ("tr_cy", "tr_CY.ISO8859-9"),
    ("tr_tr", "tr_TR.ISO8859-9"), ("ts", "ts_ZA.ISO8859-1"), ("ts_za", "ts_ZA.ISO8859-1"),
    ("tt", "tt_RU.TATAR-CYR"), ("tt_ru", "tt_RU.TATAR-CYR"), ("tt_ru.tatarcyr", "tt_RU.TATAR-CYR"),
    ("tt_ru@iqtelif", "tt_RU.UTF-8@iqtelif"), ("turkish", "tr_TR.ISO8859-9"),
    ("ug_cn", "ug_CN.UTF-8"), ("uk", "uk_UA.KOI8-U"), ("uk_ua", "uk_UA.KOI8-U"),
    ("univ.utf8", "en_US.UTF-8"), ("universal.utf8@ucs4", "en_US.UTF-8"),
    ("unm_us", "unm_US.UTF-8"), ("ur", "ur_PK.CP1256"), ("ur_in", "ur_IN.UTF-8"),
    ("ur_pk", "ur_PK.CP1256"), ("uz", "uz_UZ.UTF-8"), ("uz_uz", "uz_UZ.UTF-8"),
    ("uz_uz@cyrillic", "uz_UZ.UTF-8"), ("ve", "ve_ZA.UTF-8"), ("ve_za", "ve_ZA.UTF-8"),
    ("vi", "vi_VN.TCVN"), ("vi_vn", "vi_VN.TCVN"), ("vi_vn.tcvn", "vi_VN.TCVN"),
    ("vi_vn.tcvn5712", "vi_VN.TCVN"), ("vi_vn.viscii", "vi_VN.VISCII"),
    ("vi_vn.viscii111", "vi_VN.VISCII"), ("wa", "wa_BE.ISO8859-1"), ("wa_be", "wa_BE.ISO8859-1"),
    ("wae_ch", "wae_CH.UTF-8"), ("wal_et", "wal_ET.UTF-8"), ("wo_sn", "wo_SN.UTF-8"),
    ("xh", "xh_ZA.ISO8859-1"), ("xh_za", "xh_ZA.ISO8859-1"), ("yi", "yi_US.CP1255"),
    ("yi_us", "yi_US.CP1255"), ("yo_ng", "yo_NG.UTF-8"), ("yue_hk", "yue_HK.UTF-8"),
    ("yuw_pg", "yuw_PG.UTF-8"), ("zgh_ma", "zgh_MA.UTF-8"), ("zh", "zh_CN.eucCN"),
    ("zh_cn", "zh_CN.gb2312"), ("zh_cn.big5", "zh_TW.big5"), ("zh_cn.euc", "zh_CN.eucCN"),
    ("zh_hk", "zh_HK.big5hkscs"), ("zh_hk.big5hk", "zh_HK.big5hkscs"), ("zh_sg", "zh_SG.GB2312"),
    ("zh_sg.gbk", "zh_SG.GBK"), ("zh_tw", "zh_TW.big5"), ("zh_tw.euc", "zh_TW.eucTW"),
    ("zh_tw.euctw", "zh_TW.eucTW"), ("zu", "zu_ZA.ISO8859-1"), ("zu_za", "zu_ZA.ISO8859-1"),
];

/// Python's `locale.locale_encoding_alias` (CPython 3.14), verbatim.
const LOCALE_ENCODING_ALIAS: &[(&str, &str)] = &[
    ("437", "C"), ("88591", "ISO8859-1"), ("885915", "ISO8859-15"), ("88592", "ISO8859-2"),
    ("88595", "ISO8859-5"), ("ajec", "eucJP"), ("ascii", "ISO8859-1"), ("c", "C"),
    ("cp1251", "CP1251"), ("cp1255", "CP1255"), ("cp1256", "CP1256"), ("en", "ISO8859-1"),
    ("euc_jp", "eucJP"), ("euc_kr", "eucKR"), ("eucjp", "eucJP"), ("euckr", "eucKR"),
    ("iso2022_jp", "JIS7"), ("iso2022jp", "JIS7"), ("iso88591", "ISO8859-1"),
    ("iso885910", "ISO8859-10"), ("iso885911", "ISO8859-11"), ("iso885913", "ISO8859-13"),
    ("iso885914", "ISO8859-14"), ("iso885915", "ISO8859-15"), ("iso885916", "ISO8859-16"),
    ("iso88592", "ISO8859-2"), ("iso88593", "ISO8859-3"), ("iso88594", "ISO8859-4"),
    ("iso88595", "ISO8859-5"), ("iso88596", "ISO8859-6"), ("iso88597", "ISO8859-7"),
    ("iso88598", "ISO8859-8"), ("iso88599", "ISO8859-9"), ("iso8859_1", "ISO8859-1"),
    ("iso8859_10", "ISO8859-10"), ("iso8859_11", "ISO8859-11"), ("iso8859_13", "ISO8859-13"),
    ("iso8859_14", "ISO8859-14"), ("iso8859_15", "ISO8859-15"), ("iso8859_16", "ISO8859-16"),
    ("iso8859_2", "ISO8859-2"), ("iso8859_3", "ISO8859-3"), ("iso8859_4", "ISO8859-4"),
    ("iso8859_5", "ISO8859-5"), ("iso8859_6", "ISO8859-6"), ("iso8859_7", "ISO8859-7"),
    ("iso8859_8", "ISO8859-8"), ("iso8859_9", "ISO8859-9"), ("jis", "JIS7"), ("jis7", "JIS7"),
    ("koi8_r", "KOI8-R"), ("koi8_t", "KOI8-T"), ("koi8_u", "KOI8-U"), ("koi8c", "KOI8-C"),
    ("koi8r", "KOI8-R"), ("koi8t", "KOI8-T"), ("koi8u", "KOI8-U"), ("kz1048", "RK1048"),
    ("latin1", "ISO8859-1"), ("latin_1", "ISO8859-1"), ("microsoftcp1251", "CP1251"),
    ("microsoftcp1255", "CP1255"), ("microsoftcp1256", "CP1256"), ("shift_jis", "SJIS"),
    ("shiftjis", "SJIS"), ("tactis", "TACTIS"), ("utf8", "UTF-8"), ("utf_8", "UTF-8"),
];

/// Python's `encodings.aliases.aliases` (CPython 3.14), verbatim.
const ENCODING_ALIASES: &[(&str, &str)] = &[
    ("037", "cp037"), ("1026", "cp1026"), ("1125", "cp1125"), ("1140", "cp1140"),
    ("1250", "cp1250"), ("1251", "cp1251"), ("1252", "cp1252"), ("1253", "cp1253"),
    ("1254", "cp1254"), ("1255", "cp1255"), ("1256", "cp1256"), ("1257", "cp1257"),
    ("1258", "cp1258"), ("273", "cp273"), ("424", "cp424"), ("437", "cp437"), ("500", "cp500"),
    ("646", "ascii"), ("775", "cp775"), ("850", "cp850"), ("852", "cp852"), ("855", "cp855"),
    ("857", "cp857"), ("858", "cp858"), ("860", "cp860"), ("861", "cp861"), ("862", "cp862"),
    ("863", "cp863"), ("864", "cp864"), ("865", "cp865"), ("866", "cp866"), ("869", "cp869"),
    ("874", "cp874"), ("8859", "latin_1"), ("932", "cp932"), ("936", "gbk"), ("949", "cp949"),
    ("950", "cp950"), ("ansi", "mbcs"), ("ansi_x3.4_1968", "ascii"), ("ansi_x3.4_1986", "ascii"),
    ("ansi_x3_4_1968", "ascii"), ("arabic", "iso8859_6"), ("asmo_708", "iso8859_6"),
    ("base64", "base64_codec"), ("base_64", "base64_codec"), ("big5_hkscs", "big5hkscs"),
    ("big5_tw", "big5"), ("bz2", "bz2_codec"), ("chinese", "gb2312"), ("cp00858", "cp858"),
    ("cp01140", "cp1140"), ("cp1051", "hp_roman8"), ("cp1361", "johab"), ("cp154", "ptcp154"),
    ("cp367", "ascii"), ("cp65001", "utf_8"), ("cp819", "latin_1"), ("cp866u", "cp1125"),
    ("cp936", "gbk"), ("cp_gr", "cp869"), ("cp_is", "cp861"), ("csHPRoman8", "hp_roman8"),
    ("csascii", "ascii"), ("csbig5", "big5"), ("cseuckr", "euc_kr"), ("csibm00858", "cp858"),
    ("csibm01140", "cp1140"), ("csibm037", "cp037"), ("csibm1026", "cp1026"),
    ("csibm273", "cp273"), ("csibm424", "cp424"), ("csibm500", "cp500"), ("csibm855", "cp855"),
    ("csibm857", "cp857"), ("csibm858", "cp858"), ("csibm860", "cp860"), ("csibm861", "cp861"),
    ("csibm863", "cp863"), ("csibm864", "cp864"), ("csibm865", "cp865"), ("csibm866", "cp866"),
    ("csibm869", "cp869"), ("csiso2022jp", "iso2022_jp"), ("csiso2022kr", "iso2022_kr"),
    ("csiso58gb231280", "gb2312"), ("csisolatin1", "latin_1"), ("csisolatin2", "iso8859_2"),
    ("csisolatin3", "iso8859_3"), ("csisolatin4", "iso8859_4"), ("csisolatin5", "iso8859_9"),
    ("csisolatin6", "iso8859_10"), ("csisolatinarabic", "iso8859_6"),
    ("csisolatincyrillic", "iso8859_5"), ("csisolatingreek", "iso8859_7"),
    ("csisolatinhebrew", "iso8859_8"), ("cskoi8r", "koi8_r"), ("cspc775baltic", "cp775"),
    ("cspc850multilingual", "cp850"), ("cspc862latinhebrew", "cp862"),
    ("cspc8codepage437", "cp437"), ("cspcp852", "cp852"), ("csptcp154", "ptcp154"),
    ("csshiftjis", "shift_jis"), ("cyrillic", "iso8859_5"), ("cyrillic_asian", "ptcp154"),
    ("dbcs", "mbcs"), ("ebcdic_cp_be", "cp500"), ("ebcdic_cp_ca", "cp037"),
    ("ebcdic_cp_ch", "cp500"), ("ebcdic_cp_he", "cp424"), ("ebcdic_cp_nl", "cp037"),
    ("ebcdic_cp_us", "cp037"), ("ebcdic_cp_wt", "cp037"), ("ebcdic_us_37_euro", "cp1140"),
    ("ecma_114", "iso8859_6"), ("ecma_118", "iso8859_7"), ("elot_928", "iso8859_7"),
    ("euc_cn", "gb2312"), ("euc_jis2004", "euc_jis_2004"), ("euccn", "gb2312"),
    ("eucgb2312_cn", "gb2312"), ("eucjis2004", "euc_jis_2004"), ("eucjisx0213", "euc_jisx0213"),
    ("eucjp", "euc_jp"), ("euckr", "euc_kr"), ("gb18030_2000", "gb18030"),
    ("gb2312_1980", "gb2312"), ("gb2312_80", "gb2312"), ("greek", "iso8859_7"),
    ("greek8", "iso8859_7"), ("hebrew", "iso8859_8"), ("hex", "hex_codec"), ("hkscs", "big5hkscs"),
    ("hz_gb", "hz"), ("hz_gb_2312", "hz"), ("hzgb", "hz"), ("ibm00858", "cp858"),
    ("ibm01140", "cp1140"), ("ibm037", "cp037"), ("ibm039", "cp037"), ("ibm1026", "cp1026"),
    ("ibm1051", "hp_roman8"), ("ibm1125", "cp1125"), ("ibm1140", "cp1140"), ("ibm273", "cp273"),
    ("ibm367", "ascii"), ("ibm424", "cp424"), ("ibm437", "cp437"), ("ibm500", "cp500"),
    ("ibm775", "cp775"), ("ibm819", "latin_1"), ("ibm850", "cp850"), ("ibm852", "cp852"),
    ("ibm855", "cp855"), ("ibm857", "cp857"), ("ibm858", "cp858"), ("ibm860", "cp860"),
    ("ibm861", "cp861"), ("ibm862", "cp862"), ("ibm863", "cp863"), ("ibm864", "cp864"),
    ("ibm865", "cp865"), ("ibm866", "cp866"), ("ibm869", "cp869"), ("iso2022jp", "iso2022_jp"),
    ("iso2022jp_1", "iso2022_jp_1"), ("iso2022jp_2", "iso2022_jp_2"),
    ("iso2022jp_2004", "iso2022_jp_2004"), ("iso2022jp_3", "iso2022_jp_3"),
    ("iso2022jp_ext", "iso2022_jp_ext"), ("iso2022kr", "iso2022_kr"), ("iso646_us", "ascii"),
    ("iso8859", "latin_1"), ("iso8859_1", "latin_1"), ("iso_2022_jp", "iso2022_jp"),
    ("iso_2022_jp_1", "iso2022_jp_1"), ("iso_2022_jp_2", "iso2022_jp_2"),
    ("iso_2022_jp_2004", "iso2022_jp_2004"), ("iso_2022_jp_3", "iso2022_jp_3"),
    ("iso_2022_jp_ext", "iso2022_jp_ext"), ("iso_2022_kr", "iso2022_kr"),
    ("iso_646.irv_1991", "ascii"), ("iso_8859_1", "latin_1"), ("iso_8859_10", "iso8859_10"),
    ("iso_8859_10_1992", "iso8859_10"), ("iso_8859_11", "iso8859_11"),
    ("iso_8859_11_2001", "iso8859_11"), ("iso_8859_13", "iso8859_13"),
    ("iso_8859_14", "iso8859_14"), ("iso_8859_14_1998", "iso8859_14"),
    ("iso_8859_15", "iso8859_15"), ("iso_8859_16", "iso8859_16"),
    ("iso_8859_16_2001", "iso8859_16"), ("iso_8859_1_1987", "latin_1"),
    ("iso_8859_2", "iso8859_2"), ("iso_8859_2_1987", "iso8859_2"), ("iso_8859_3", "iso8859_3"),
    ("iso_8859_3_1988", "iso8859_3"), ("iso_8859_4", "iso8859_4"),
    ("iso_8859_4_1988", "iso8859_4"), ("iso_8859_5", "iso8859_5"),
    ("iso_8859_5_1988", "iso8859_5"), ("iso_8859_6", "iso8859_6"),
    ("iso_8859_6_1987", "iso8859_6"), ("iso_8859_7", "iso8859_7"),
    ("iso_8859_7_1987", "iso8859_7"), ("iso_8859_8", "iso8859_8"),
    ("iso_8859_8_1988", "iso8859_8"), ("iso_8859_8_e", "iso8859_8"), ("iso_8859_8_i", "iso8859_8"),
    ("iso_8859_9", "iso8859_9"), ("iso_8859_9_1989", "iso8859_9"), ("iso_celtic", "iso8859_14"),
    ("iso_ir_100", "latin_1"), ("iso_ir_101", "iso8859_2"), ("iso_ir_109", "iso8859_3"),
    ("iso_ir_110", "iso8859_4"), ("iso_ir_126", "iso8859_7"), ("iso_ir_127", "iso8859_6"),
    ("iso_ir_138", "iso8859_8"), ("iso_ir_144", "iso8859_5"), ("iso_ir_148", "iso8859_9"),
    ("iso_ir_157", "iso8859_10"), ("iso_ir_166", "tis_620"), ("iso_ir_199", "iso8859_14"),
    ("iso_ir_226", "iso8859_16"), ("iso_ir_58", "gb2312"), ("iso_ir_6", "ascii"),
    ("jisx0213", "euc_jis_2004"), ("korean", "euc_kr"), ("ks_c_5601", "euc_kr"),
    ("ks_c_5601_1987", "euc_kr"), ("ks_x_1001", "euc_kr"), ("ksc5601", "euc_kr"),
    ("ksx1001", "euc_kr"), ("kz_1048", "kz1048"), ("l1", "latin_1"), ("l10", "iso8859_16"),
    ("l2", "iso8859_2"), ("l3", "iso8859_3"), ("l4", "iso8859_4"), ("l5", "iso8859_9"),
    ("l6", "iso8859_10"), ("l7", "iso8859_13"), ("l8", "iso8859_14"), ("l9", "iso8859_15"),
    ("latin", "latin_1"), ("latin1", "latin_1"), ("latin10", "iso8859_16"),
    ("latin2", "iso8859_2"), ("latin3", "iso8859_3"), ("latin4", "iso8859_4"),
    ("latin5", "iso8859_9"), ("latin6", "iso8859_10"), ("latin7", "iso8859_13"),
    ("latin8", "iso8859_14"), ("latin9", "iso8859_15"), ("mac_centeuro", "mac_latin2"),
    ("maccentraleurope", "mac_latin2"), ("maccyrillic", "mac_cyrillic"), ("macgreek", "mac_greek"),
    ("maciceland", "mac_iceland"), ("macintosh", "mac_roman"), ("maclatin2", "mac_latin2"),
    ("macroman", "mac_roman"), ("macturkish", "mac_turkish"), ("ms1361", "johab"),
    ("ms874", "cp874"), ("ms932", "cp932"), ("ms936", "gbk"), ("ms949", "cp949"),
    ("ms950", "cp950"), ("ms_kanji", "cp932"), ("mskanji", "cp932"),
    ("pc_multilingual_850_euro", "cp858"), ("pt154", "ptcp154"), ("quopri", "quopri_codec"),
    ("quoted_printable", "quopri_codec"), ("quotedprintable", "quopri_codec"), ("r8", "hp_roman8"),
    ("rk1048", "kz1048"), ("roman8", "hp_roman8"), ("rot13", "rot_13"), ("ruscii", "cp1125"),
    ("s_jis", "shift_jis"), ("s_jis_2004", "shift_jis_2004"), ("s_jisx0213", "shift_jisx0213"),
    ("shiftjis", "shift_jis"), ("shiftjis2004", "shift_jis_2004"),
    ("shiftjisx0213", "shift_jisx0213"), ("sjis", "shift_jis"), ("sjis_2004", "shift_jis_2004"),
    ("sjisx0213", "shift_jisx0213"), ("strk1048_2002", "kz1048"), ("thai", "iso8859_11"),
    ("tis620", "tis_620"), ("tis_620_0", "tis_620"), ("tis_620_2529_0", "tis_620"),
    ("tis_620_2529_1", "tis_620"), ("u16", "utf_16"), ("u32", "utf_32"), ("u7", "utf_7"),
    ("u8", "utf_8"), ("u_jis", "euc_jp"), ("uhc", "cp949"), ("ujis", "euc_jp"),
    ("unicode_1_1_utf_7", "utf_7"), ("unicodebigunmarked", "utf_16_be"),
    ("unicodelittleunmarked", "utf_16_le"), ("us", "ascii"), ("us_ascii", "ascii"),
    ("utf", "utf_8"), ("utf16", "utf_16"), ("utf32", "utf_32"), ("utf7", "utf_7"),
    ("utf8", "utf_8"), ("utf8_ucs2", "utf_8"), ("utf8_ucs4", "utf_8"), ("utf_16be", "utf_16_be"),
    ("utf_16le", "utf_16_le"), ("utf_32be", "utf_32_be"), ("utf_32le", "utf_32_le"),
    ("uu", "uu_codec"), ("windows_1250", "cp1250"), ("windows_1251", "cp1251"),
    ("windows_1252", "cp1252"), ("windows_1253", "cp1253"), ("windows_1254", "cp1254"),
    ("windows_1255", "cp1255"), ("windows_1256", "cp1256"), ("windows_1257", "cp1257"),
    ("windows_1258", "cp1258"), ("windows_31j", "cp932"), ("windows_874", "cp874"),
    ("x_mac_japanese", "shift_jis"), ("x_mac_korean", "euc_kr"), ("x_mac_simp_chinese", "gb2312"),
    ("x_mac_trad_chinese", "big5"), ("zip", "zlib_codec"), ("zlib", "zlib_codec"),
];

/// `str.isalnum()` is true on these code point ranges (CPython 3.14,
/// Unicode 16.0), generated from Python.
const PY_ALNUM: &[(u32, u32)] = &[
    (0x30, 0x39), (0x41, 0x5A), (0x61, 0x7A), (0xAA, 0xAA), (0xB2, 0xB3), (0xB5, 0xB5), (0xB9, 0xBA), (0xBC, 0xBE),
    (0xC0, 0xD6), (0xD8, 0xF6), (0xF8, 0x2C1), (0x2C6, 0x2D1), (0x2E0, 0x2E4), (0x2EC, 0x2EC), (0x2EE, 0x2EE), (0x370, 0x374),
    (0x376, 0x377), (0x37A, 0x37D), (0x37F, 0x37F), (0x386, 0x386), (0x388, 0x38A), (0x38C, 0x38C), (0x38E, 0x3A1), (0x3A3, 0x3F5),
    (0x3F7, 0x481), (0x48A, 0x52F), (0x531, 0x556), (0x559, 0x559), (0x560, 0x588), (0x5D0, 0x5EA), (0x5EF, 0x5F2), (0x620, 0x64A),
    (0x660, 0x669), (0x66E, 0x66F), (0x671, 0x6D3), (0x6D5, 0x6D5), (0x6E5, 0x6E6), (0x6EE, 0x6FC), (0x6FF, 0x6FF), (0x710, 0x710),
    (0x712, 0x72F), (0x74D, 0x7A5), (0x7B1, 0x7B1), (0x7C0, 0x7EA), (0x7F4, 0x7F5), (0x7FA, 0x7FA), (0x800, 0x815), (0x81A, 0x81A),
    (0x824, 0x824), (0x828, 0x828), (0x840, 0x858), (0x860, 0x86A), (0x870, 0x887), (0x889, 0x88E), (0x8A0, 0x8C9), (0x904, 0x939),
    (0x93D, 0x93D), (0x950, 0x950), (0x958, 0x961), (0x966, 0x96F), (0x971, 0x980), (0x985, 0x98C), (0x98F, 0x990), (0x993, 0x9A8),
    (0x9AA, 0x9B0), (0x9B2, 0x9B2), (0x9B6, 0x9B9), (0x9BD, 0x9BD), (0x9CE, 0x9CE), (0x9DC, 0x9DD), (0x9DF, 0x9E1), (0x9E6, 0x9F1),
    (0x9F4, 0x9F9), (0x9FC, 0x9FC), (0xA05, 0xA0A), (0xA0F, 0xA10), (0xA13, 0xA28), (0xA2A, 0xA30), (0xA32, 0xA33), (0xA35, 0xA36),
    (0xA38, 0xA39), (0xA59, 0xA5C), (0xA5E, 0xA5E), (0xA66, 0xA6F), (0xA72, 0xA74), (0xA85, 0xA8D), (0xA8F, 0xA91), (0xA93, 0xAA8),
    (0xAAA, 0xAB0), (0xAB2, 0xAB3), (0xAB5, 0xAB9), (0xABD, 0xABD), (0xAD0, 0xAD0), (0xAE0, 0xAE1), (0xAE6, 0xAEF), (0xAF9, 0xAF9),
    (0xB05, 0xB0C), (0xB0F, 0xB10), (0xB13, 0xB28), (0xB2A, 0xB30), (0xB32, 0xB33), (0xB35, 0xB39), (0xB3D, 0xB3D), (0xB5C, 0xB5D),
    (0xB5F, 0xB61), (0xB66, 0xB6F), (0xB71, 0xB77), (0xB83, 0xB83), (0xB85, 0xB8A), (0xB8E, 0xB90), (0xB92, 0xB95), (0xB99, 0xB9A),
    (0xB9C, 0xB9C), (0xB9E, 0xB9F), (0xBA3, 0xBA4), (0xBA8, 0xBAA), (0xBAE, 0xBB9), (0xBD0, 0xBD0), (0xBE6, 0xBF2), (0xC05, 0xC0C),
    (0xC0E, 0xC10), (0xC12, 0xC28), (0xC2A, 0xC39), (0xC3D, 0xC3D), (0xC58, 0xC5A), (0xC5D, 0xC5D), (0xC60, 0xC61), (0xC66, 0xC6F),
    (0xC78, 0xC7E), (0xC80, 0xC80), (0xC85, 0xC8C), (0xC8E, 0xC90), (0xC92, 0xCA8), (0xCAA, 0xCB3), (0xCB5, 0xCB9), (0xCBD, 0xCBD),
    (0xCDD, 0xCDE), (0xCE0, 0xCE1), (0xCE6, 0xCEF), (0xCF1, 0xCF2), (0xD04, 0xD0C), (0xD0E, 0xD10), (0xD12, 0xD3A), (0xD3D, 0xD3D),
    (0xD4E, 0xD4E), (0xD54, 0xD56), (0xD58, 0xD61), (0xD66, 0xD78), (0xD7A, 0xD7F), (0xD85, 0xD96), (0xD9A, 0xDB1), (0xDB3, 0xDBB),
    (0xDBD, 0xDBD), (0xDC0, 0xDC6), (0xDE6, 0xDEF), (0xE01, 0xE30), (0xE32, 0xE33), (0xE40, 0xE46), (0xE50, 0xE59), (0xE81, 0xE82),
    (0xE84, 0xE84), (0xE86, 0xE8A), (0xE8C, 0xEA3), (0xEA5, 0xEA5), (0xEA7, 0xEB0), (0xEB2, 0xEB3), (0xEBD, 0xEBD), (0xEC0, 0xEC4),
    (0xEC6, 0xEC6), (0xED0, 0xED9), (0xEDC, 0xEDF), (0xF00, 0xF00), (0xF20, 0xF33), (0xF40, 0xF47), (0xF49, 0xF6C), (0xF88, 0xF8C),
    (0x1000, 0x102A), (0x103F, 0x1049), (0x1050, 0x1055), (0x105A, 0x105D), (0x1061, 0x1061), (0x1065, 0x1066), (0x106E, 0x1070), (0x1075, 0x1081),
    (0x108E, 0x108E), (0x1090, 0x1099), (0x10A0, 0x10C5), (0x10C7, 0x10C7), (0x10CD, 0x10CD), (0x10D0, 0x10FA), (0x10FC, 0x1248), (0x124A, 0x124D),
    (0x1250, 0x1256), (0x1258, 0x1258), (0x125A, 0x125D), (0x1260, 0x1288), (0x128A, 0x128D), (0x1290, 0x12B0), (0x12B2, 0x12B5), (0x12B8, 0x12BE),
    (0x12C0, 0x12C0), (0x12C2, 0x12C5), (0x12C8, 0x12D6), (0x12D8, 0x1310), (0x1312, 0x1315), (0x1318, 0x135A), (0x1369, 0x137C), (0x1380, 0x138F),
    (0x13A0, 0x13F5), (0x13F8, 0x13FD), (0x1401, 0x166C), (0x166F, 0x167F), (0x1681, 0x169A), (0x16A0, 0x16EA), (0x16EE, 0x16F8), (0x1700, 0x1711),
    (0x171F, 0x1731), (0x1740, 0x1751), (0x1760, 0x176C), (0x176E, 0x1770), (0x1780, 0x17B3), (0x17D7, 0x17D7), (0x17DC, 0x17DC), (0x17E0, 0x17E9),
    (0x17F0, 0x17F9), (0x1810, 0x1819), (0x1820, 0x1878), (0x1880, 0x1884), (0x1887, 0x18A8), (0x18AA, 0x18AA), (0x18B0, 0x18F5), (0x1900, 0x191E),
    (0x1946, 0x196D), (0x1970, 0x1974), (0x1980, 0x19AB), (0x19B0, 0x19C9), (0x19D0, 0x19DA), (0x1A00, 0x1A16), (0x1A20, 0x1A54), (0x1A80, 0x1A89),
    (0x1A90, 0x1A99), (0x1AA7, 0x1AA7), (0x1B05, 0x1B33), (0x1B45, 0x1B4C), (0x1B50, 0x1B59), (0x1B83, 0x1BA0), (0x1BAE, 0x1BE5), (0x1C00, 0x1C23),
    (0x1C40, 0x1C49), (0x1C4D, 0x1C7D), (0x1C80, 0x1C8A), (0x1C90, 0x1CBA), (0x1CBD, 0x1CBF), (0x1CE9, 0x1CEC), (0x1CEE, 0x1CF3), (0x1CF5, 0x1CF6),
    (0x1CFA, 0x1CFA), (0x1D00, 0x1DBF), (0x1E00, 0x1F15), (0x1F18, 0x1F1D), (0x1F20, 0x1F45), (0x1F48, 0x1F4D), (0x1F50, 0x1F57), (0x1F59, 0x1F59),
    (0x1F5B, 0x1F5B), (0x1F5D, 0x1F5D), (0x1F5F, 0x1F7D), (0x1F80, 0x1FB4), (0x1FB6, 0x1FBC), (0x1FBE, 0x1FBE), (0x1FC2, 0x1FC4), (0x1FC6, 0x1FCC),
    (0x1FD0, 0x1FD3), (0x1FD6, 0x1FDB), (0x1FE0, 0x1FEC), (0x1FF2, 0x1FF4), (0x1FF6, 0x1FFC), (0x2070, 0x2071), (0x2074, 0x2079), (0x207F, 0x2089),
    (0x2090, 0x209C), (0x2102, 0x2102), (0x2107, 0x2107), (0x210A, 0x2113), (0x2115, 0x2115), (0x2119, 0x211D), (0x2124, 0x2124), (0x2126, 0x2126),
    (0x2128, 0x2128), (0x212A, 0x212D), (0x212F, 0x2139), (0x213C, 0x213F), (0x2145, 0x2149), (0x214E, 0x214E), (0x2150, 0x2189), (0x2460, 0x249B),
    (0x24EA, 0x24FF), (0x2776, 0x2793), (0x2C00, 0x2CE4), (0x2CEB, 0x2CEE), (0x2CF2, 0x2CF3), (0x2CFD, 0x2CFD), (0x2D00, 0x2D25), (0x2D27, 0x2D27),
    (0x2D2D, 0x2D2D), (0x2D30, 0x2D67), (0x2D6F, 0x2D6F), (0x2D80, 0x2D96), (0x2DA0, 0x2DA6), (0x2DA8, 0x2DAE), (0x2DB0, 0x2DB6), (0x2DB8, 0x2DBE),
    (0x2DC0, 0x2DC6), (0x2DC8, 0x2DCE), (0x2DD0, 0x2DD6), (0x2DD8, 0x2DDE), (0x2E2F, 0x2E2F), (0x3005, 0x3007), (0x3021, 0x3029), (0x3031, 0x3035),
    (0x3038, 0x303C), (0x3041, 0x3096), (0x309D, 0x309F), (0x30A1, 0x30FA), (0x30FC, 0x30FF), (0x3105, 0x312F), (0x3131, 0x318E), (0x3192, 0x3195),
    (0x31A0, 0x31BF), (0x31F0, 0x31FF), (0x3220, 0x3229), (0x3248, 0x324F), (0x3251, 0x325F), (0x3280, 0x3289), (0x32B1, 0x32BF), (0x3400, 0x4DBF),
    (0x4E00, 0xA48C), (0xA4D0, 0xA4FD), (0xA500, 0xA60C), (0xA610, 0xA62B), (0xA640, 0xA66E), (0xA67F, 0xA69D), (0xA6A0, 0xA6EF), (0xA717, 0xA71F),
    (0xA722, 0xA788), (0xA78B, 0xA7CD), (0xA7D0, 0xA7D1), (0xA7D3, 0xA7D3), (0xA7D5, 0xA7DC), (0xA7F2, 0xA801), (0xA803, 0xA805), (0xA807, 0xA80A),
    (0xA80C, 0xA822), (0xA830, 0xA835), (0xA840, 0xA873), (0xA882, 0xA8B3), (0xA8D0, 0xA8D9), (0xA8F2, 0xA8F7), (0xA8FB, 0xA8FB), (0xA8FD, 0xA8FE),
    (0xA900, 0xA925), (0xA930, 0xA946), (0xA960, 0xA97C), (0xA984, 0xA9B2), (0xA9CF, 0xA9D9), (0xA9E0, 0xA9E4), (0xA9E6, 0xA9FE), (0xAA00, 0xAA28),
    (0xAA40, 0xAA42), (0xAA44, 0xAA4B), (0xAA50, 0xAA59), (0xAA60, 0xAA76), (0xAA7A, 0xAA7A), (0xAA7E, 0xAAAF), (0xAAB1, 0xAAB1), (0xAAB5, 0xAAB6),
    (0xAAB9, 0xAABD), (0xAAC0, 0xAAC0), (0xAAC2, 0xAAC2), (0xAADB, 0xAADD), (0xAAE0, 0xAAEA), (0xAAF2, 0xAAF4), (0xAB01, 0xAB06), (0xAB09, 0xAB0E),
    (0xAB11, 0xAB16), (0xAB20, 0xAB26), (0xAB28, 0xAB2E), (0xAB30, 0xAB5A), (0xAB5C, 0xAB69), (0xAB70, 0xABE2), (0xABF0, 0xABF9), (0xAC00, 0xD7A3),
    (0xD7B0, 0xD7C6), (0xD7CB, 0xD7FB), (0xF900, 0xFA6D), (0xFA70, 0xFAD9), (0xFB00, 0xFB06), (0xFB13, 0xFB17), (0xFB1D, 0xFB1D), (0xFB1F, 0xFB28),
    (0xFB2A, 0xFB36), (0xFB38, 0xFB3C), (0xFB3E, 0xFB3E), (0xFB40, 0xFB41), (0xFB43, 0xFB44), (0xFB46, 0xFBB1), (0xFBD3, 0xFD3D), (0xFD50, 0xFD8F),
    (0xFD92, 0xFDC7), (0xFDF0, 0xFDFB), (0xFE70, 0xFE74), (0xFE76, 0xFEFC), (0xFF10, 0xFF19), (0xFF21, 0xFF3A), (0xFF41, 0xFF5A), (0xFF66, 0xFFBE),
    (0xFFC2, 0xFFC7), (0xFFCA, 0xFFCF), (0xFFD2, 0xFFD7), (0xFFDA, 0xFFDC), (0x10000, 0x1000B), (0x1000D, 0x10026), (0x10028, 0x1003A), (0x1003C, 0x1003D),
    (0x1003F, 0x1004D), (0x10050, 0x1005D), (0x10080, 0x100FA), (0x10107, 0x10133), (0x10140, 0x10178), (0x1018A, 0x1018B), (0x10280, 0x1029C), (0x102A0, 0x102D0),
    (0x102E1, 0x102FB), (0x10300, 0x10323), (0x1032D, 0x1034A), (0x10350, 0x10375), (0x10380, 0x1039D), (0x103A0, 0x103C3), (0x103C8, 0x103CF), (0x103D1, 0x103D5),
    (0x10400, 0x1049D), (0x104A0, 0x104A9), (0x104B0, 0x104D3), (0x104D8, 0x104FB), (0x10500, 0x10527), (0x10530, 0x10563), (0x10570, 0x1057A), (0x1057C, 0x1058A),
    (0x1058C, 0x10592), (0x10594, 0x10595), (0x10597, 0x105A1), (0x105A3, 0x105B1), (0x105B3, 0x105B9), (0x105BB, 0x105BC), (0x105C0, 0x105F3), (0x10600, 0x10736),
    (0x10740, 0x10755), (0x10760, 0x10767), (0x10780, 0x10785), (0x10787, 0x107B0), (0x107B2, 0x107BA), (0x10800, 0x10805), (0x10808, 0x10808), (0x1080A, 0x10835),
    (0x10837, 0x10838), (0x1083C, 0x1083C), (0x1083F, 0x10855), (0x10858, 0x10876), (0x10879, 0x1089E), (0x108A7, 0x108AF), (0x108E0, 0x108F2), (0x108F4, 0x108F5),
    (0x108FB, 0x1091B), (0x10920, 0x10939), (0x10980, 0x109B7), (0x109BC, 0x109CF), (0x109D2, 0x10A00), (0x10A10, 0x10A13), (0x10A15, 0x10A17), (0x10A19, 0x10A35),
    (0x10A40, 0x10A48), (0x10A60, 0x10A7E), (0x10A80, 0x10A9F), (0x10AC0, 0x10AC7), (0x10AC9, 0x10AE4), (0x10AEB, 0x10AEF), (0x10B00, 0x10B35), (0x10B40, 0x10B55),
    (0x10B58, 0x10B72), (0x10B78, 0x10B91), (0x10BA9, 0x10BAF), (0x10C00, 0x10C48), (0x10C80, 0x10CB2), (0x10CC0, 0x10CF2), (0x10CFA, 0x10D23), (0x10D30, 0x10D39),
    (0x10D40, 0x10D65), (0x10D6F, 0x10D85), (0x10E60, 0x10E7E), (0x10E80, 0x10EA9), (0x10EB0, 0x10EB1), (0x10EC2, 0x10EC4), (0x10F00, 0x10F27), (0x10F30, 0x10F45),
    (0x10F51, 0x10F54), (0x10F70, 0x10F81), (0x10FB0, 0x10FCB), (0x10FE0, 0x10FF6), (0x11003, 0x11037), (0x11052, 0x1106F), (0x11071, 0x11072), (0x11075, 0x11075),
    (0x11083, 0x110AF), (0x110D0, 0x110E8), (0x110F0, 0x110F9), (0x11103, 0x11126), (0x11136, 0x1113F), (0x11144, 0x11144), (0x11147, 0x11147), (0x11150, 0x11172),
    (0x11176, 0x11176), (0x11183, 0x111B2), (0x111C1, 0x111C4), (0x111D0, 0x111DA), (0x111DC, 0x111DC), (0x111E1, 0x111F4), (0x11200, 0x11211), (0x11213, 0x1122B),
    (0x1123F, 0x11240), (0x11280, 0x11286), (0x11288, 0x11288), (0x1128A, 0x1128D), (0x1128F, 0x1129D), (0x1129F, 0x112A8), (0x112B0, 0x112DE), (0x112F0, 0x112F9),
    (0x11305, 0x1130C), (0x1130F, 0x11310), (0x11313, 0x11328), (0x1132A, 0x11330), (0x11332, 0x11333), (0x11335, 0x11339), (0x1133D, 0x1133D), (0x11350, 0x11350),
    (0x1135D, 0x11361), (0x11380, 0x11389), (0x1138B, 0x1138B), (0x1138E, 0x1138E), (0x11390, 0x113B5), (0x113B7, 0x113B7), (0x113D1, 0x113D1), (0x113D3, 0x113D3),
    (0x11400, 0x11434), (0x11447, 0x1144A), (0x11450, 0x11459), (0x1145F, 0x11461), (0x11480, 0x114AF), (0x114C4, 0x114C5), (0x114C7, 0x114C7), (0x114D0, 0x114D9),
    (0x11580, 0x115AE), (0x115D8, 0x115DB), (0x11600, 0x1162F), (0x11644, 0x11644), (0x11650, 0x11659), (0x11680, 0x116AA), (0x116B8, 0x116B8), (0x116C0, 0x116C9),
    (0x116D0, 0x116E3), (0x11700, 0x1171A), (0x11730, 0x1173B), (0x11740, 0x11746), (0x11800, 0x1182B), (0x118A0, 0x118F2), (0x118FF, 0x11906), (0x11909, 0x11909),
    (0x1190C, 0x11913), (0x11915, 0x11916), (0x11918, 0x1192F), (0x1193F, 0x1193F), (0x11941, 0x11941), (0x11950, 0x11959), (0x119A0, 0x119A7), (0x119AA, 0x119D0),
    (0x119E1, 0x119E1), (0x119E3, 0x119E3), (0x11A00, 0x11A00), (0x11A0B, 0x11A32), (0x11A3A, 0x11A3A), (0x11A50, 0x11A50), (0x11A5C, 0x11A89), (0x11A9D, 0x11A9D),
    (0x11AB0, 0x11AF8), (0x11BC0, 0x11BE0), (0x11BF0, 0x11BF9), (0x11C00, 0x11C08), (0x11C0A, 0x11C2E), (0x11C40, 0x11C40), (0x11C50, 0x11C6C), (0x11C72, 0x11C8F),
    (0x11D00, 0x11D06), (0x11D08, 0x11D09), (0x11D0B, 0x11D30), (0x11D46, 0x11D46), (0x11D50, 0x11D59), (0x11D60, 0x11D65), (0x11D67, 0x11D68), (0x11D6A, 0x11D89),
    (0x11D98, 0x11D98), (0x11DA0, 0x11DA9), (0x11EE0, 0x11EF2), (0x11F02, 0x11F02), (0x11F04, 0x11F10), (0x11F12, 0x11F33), (0x11F50, 0x11F59), (0x11FB0, 0x11FB0),
    (0x11FC0, 0x11FD4), (0x12000, 0x12399), (0x12400, 0x1246E), (0x12480, 0x12543), (0x12F90, 0x12FF0), (0x13000, 0x1342F), (0x13441, 0x13446), (0x13460, 0x143FA),
    (0x14400, 0x14646), (0x16100, 0x1611D), (0x16130, 0x16139), (0x16800, 0x16A38), (0x16A40, 0x16A5E), (0x16A60, 0x16A69), (0x16A70, 0x16ABE), (0x16AC0, 0x16AC9),
    (0x16AD0, 0x16AED), (0x16B00, 0x16B2F), (0x16B40, 0x16B43), (0x16B50, 0x16B59), (0x16B5B, 0x16B61), (0x16B63, 0x16B77), (0x16B7D, 0x16B8F), (0x16D40, 0x16D6C),
    (0x16D70, 0x16D79), (0x16E40, 0x16E96), (0x16F00, 0x16F4A), (0x16F50, 0x16F50), (0x16F93, 0x16F9F), (0x16FE0, 0x16FE1), (0x16FE3, 0x16FE3), (0x17000, 0x187F7),
    (0x18800, 0x18CD5), (0x18CFF, 0x18D08), (0x1AFF0, 0x1AFF3), (0x1AFF5, 0x1AFFB), (0x1AFFD, 0x1AFFE), (0x1B000, 0x1B122), (0x1B132, 0x1B132), (0x1B150, 0x1B152),
    (0x1B155, 0x1B155), (0x1B164, 0x1B167), (0x1B170, 0x1B2FB), (0x1BC00, 0x1BC6A), (0x1BC70, 0x1BC7C), (0x1BC80, 0x1BC88), (0x1BC90, 0x1BC99), (0x1CCF0, 0x1CCF9),
    (0x1D2C0, 0x1D2D3), (0x1D2E0, 0x1D2F3), (0x1D360, 0x1D378), (0x1D400, 0x1D454), (0x1D456, 0x1D49C), (0x1D49E, 0x1D49F), (0x1D4A2, 0x1D4A2), (0x1D4A5, 0x1D4A6),
    (0x1D4A9, 0x1D4AC), (0x1D4AE, 0x1D4B9), (0x1D4BB, 0x1D4BB), (0x1D4BD, 0x1D4C3), (0x1D4C5, 0x1D505), (0x1D507, 0x1D50A), (0x1D50D, 0x1D514), (0x1D516, 0x1D51C),
    (0x1D51E, 0x1D539), (0x1D53B, 0x1D53E), (0x1D540, 0x1D544), (0x1D546, 0x1D546), (0x1D54A, 0x1D550), (0x1D552, 0x1D6A5), (0x1D6A8, 0x1D6C0), (0x1D6C2, 0x1D6DA),
    (0x1D6DC, 0x1D6FA), (0x1D6FC, 0x1D714), (0x1D716, 0x1D734), (0x1D736, 0x1D74E), (0x1D750, 0x1D76E), (0x1D770, 0x1D788), (0x1D78A, 0x1D7A8), (0x1D7AA, 0x1D7C2),
    (0x1D7C4, 0x1D7CB), (0x1D7CE, 0x1D7FF), (0x1DF00, 0x1DF1E), (0x1DF25, 0x1DF2A), (0x1E030, 0x1E06D), (0x1E100, 0x1E12C), (0x1E137, 0x1E13D), (0x1E140, 0x1E149),
    (0x1E14E, 0x1E14E), (0x1E290, 0x1E2AD), (0x1E2C0, 0x1E2EB), (0x1E2F0, 0x1E2F9), (0x1E4D0, 0x1E4EB), (0x1E4F0, 0x1E4F9), (0x1E5D0, 0x1E5ED), (0x1E5F0, 0x1E5FA),
    (0x1E7E0, 0x1E7E6), (0x1E7E8, 0x1E7EB), (0x1E7ED, 0x1E7EE), (0x1E7F0, 0x1E7FE), (0x1E800, 0x1E8C4), (0x1E8C7, 0x1E8CF), (0x1E900, 0x1E943), (0x1E94B, 0x1E94B),
    (0x1E950, 0x1E959), (0x1EC71, 0x1ECAB), (0x1ECAD, 0x1ECAF), (0x1ECB1, 0x1ECB4), (0x1ED01, 0x1ED2D), (0x1ED2F, 0x1ED3D), (0x1EE00, 0x1EE03), (0x1EE05, 0x1EE1F),
    (0x1EE21, 0x1EE22), (0x1EE24, 0x1EE24), (0x1EE27, 0x1EE27), (0x1EE29, 0x1EE32), (0x1EE34, 0x1EE37), (0x1EE39, 0x1EE39), (0x1EE3B, 0x1EE3B), (0x1EE42, 0x1EE42),
    (0x1EE47, 0x1EE47), (0x1EE49, 0x1EE49), (0x1EE4B, 0x1EE4B), (0x1EE4D, 0x1EE4F), (0x1EE51, 0x1EE52), (0x1EE54, 0x1EE54), (0x1EE57, 0x1EE57), (0x1EE59, 0x1EE59),
    (0x1EE5B, 0x1EE5B), (0x1EE5D, 0x1EE5D), (0x1EE5F, 0x1EE5F), (0x1EE61, 0x1EE62), (0x1EE64, 0x1EE64), (0x1EE67, 0x1EE6A), (0x1EE6C, 0x1EE72), (0x1EE74, 0x1EE77),
    (0x1EE79, 0x1EE7C), (0x1EE7E, 0x1EE7E), (0x1EE80, 0x1EE89), (0x1EE8B, 0x1EE9B), (0x1EEA1, 0x1EEA3), (0x1EEA5, 0x1EEA9), (0x1EEAB, 0x1EEBB), (0x1F100, 0x1F10C),
    (0x1FBF0, 0x1FBF9), (0x20000, 0x2A6DF), (0x2A700, 0x2B739), (0x2B740, 0x2B81D), (0x2B820, 0x2CEA1), (0x2CEB0, 0x2EBE0), (0x2EBF0, 0x2EE5D), (0x2F800, 0x2FA1D),
    (0x30000, 0x3134A), (0x31350, 0x323AF),
];

/// The charmap codecs of Python's `encodings` package (CPython 3.14),
/// generated from Python: each byte's character, U+FFFE where the byte is
/// undefined; a table of 128 is the upper half of a codec that is ASCII below.
const CHARMAPS: &[(&str, &str)] = &[
    ("cp037", "\u{0}\u{1}\u{2}\u{3}\u{9c}\u{9}\u{86}\u{7f}\u{97}\u{8d}\u{8e}\u{b}\u{c}\u{d}\u{e}\u{f}\u{10}\u{11}\u{12}\u{13}\u{9d}\u{85}\u{8}\u{87}\u{18}\u{19}\u{92}\u{8f}\u{1c}\u{1d}\u{1e}\u{1f}\u{80}\u{81}\u{82}\u{83}\u{84}\u{a}\u{17}\u{1b}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{5}\u{6}\u{7}\u{90}\u{91}\u{16}\u{93}\u{94}\u{95}\u{96}\u{4}\u{98}\u{99}\u{9a}\u{9b}\u{14}\u{15}\u{9e}\u{1a} \u{a0}âäàáãåçñ¢.<(+|&éêëèíîïìß!$*);¬-/ÂÄÀÁÃÅÇÑ¦,%_>?øÉÊËÈÍÎÏÌ`:#@'=\u{22}Øabcdefghi«»ðýþ±°jklmnopqrªºæ¸Æ¤µ~stuvwxyz¡¿ÐÝÞ®^£¥·©§¶¼½¾[]¯¨´×{ABCDEFGHI\u{ad}ôöòóõ}JKLMNOPQR¹ûüùúÿ\u{5c}÷STUVWXYZ²ÔÖÒÓÕ0123456789³ÛÜÙÚ\u{9f}"),
    ("cp1006", "\u{80}\u{81}\u{82}\u{83}\u{84}\u{85}\u{86}\u{87}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{8d}\u{8e}\u{8f}\u{90}\u{91}\u{92}\u{93}\u{94}\u{95}\u{96}\u{97}\u{98}\u{99}\u{9a}\u{9b}\u{9c}\u{9d}\u{9e}\u{9f}\u{a0}۰۱۲۳۴۵۶۷۸۹،؛\u{ad}؟ﺁﺍﺎﺎﺏﺑﭖﭘﺓﺕﺗﭦﭨﺙﺛﺝﺟﭺﭼﺡﺣﺥﺧﺩﮄﺫﺭﮌﺯﮊﺱﺳﺵﺷﺹﺻﺽﺿﻁﻅﻉﻊﻋﻌﻍﻎﻏﻐﻑﻓﻕﻗﻙﻛﮒﮔﻝﻟﻠﻡﻣﮞﻥﻧﺅﻭﮦﮨﮩﮪﺀﺉﺊﺋﻱﻲﻳﮰﮮﹼﹽ"),
    ("cp1026", "\u{0}\u{1}\u{2}\u{3}\u{9c}\u{9}\u{86}\u{7f}\u{97}\u{8d}\u{8e}\u{b}\u{c}\u{d}\u{e}\u{f}\u{10}\u{11}\u{12}\u{13}\u{9d}\u{85}\u{8}\u{87}\u{18}\u{19}\u{92}\u{8f}\u{1c}\u{1d}\u{1e}\u{1f}\u{80}\u{81}\u{82}\u{83}\u{84}\u{a}\u{17}\u{1b}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{5}\u{6}\u{7}\u{90}\u{91}\u{16}\u{93}\u{94}\u{95}\u{96}\u{4}\u{98}\u{99}\u{9a}\u{9b}\u{14}\u{15}\u{9e}\u{1a} \u{a0}âäàáãå{ñÇ.<(+!&éêëèíîïìßĞİ*);^-/ÂÄÀÁÃÅ[Ñş,%_>?øÉÊËÈÍÎÏÌı:ÖŞ'=ÜØabcdefghi«»}`¦±°jklmnopqrªºæ¸Æ¤µöstuvwxyz¡¿]$@®¢£¥·©§¶¼½¾¬|¯¨´×çABCDEFGHI\u{ad}ô~òóõğJKLMNOPQR¹û\u{5c}ùúÿü÷STUVWXYZ²Ô#ÒÓÕ0123456789³Û\u{22}ÙÚ\u{9f}"),
    ("cp1125", "АБВГДЕЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯабвгдежзийклмноп░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀рстуфхцчшщъыьэюяЁёҐґЄєІіЇї·√№¤■\u{a0}"),
    ("cp1140", "\u{0}\u{1}\u{2}\u{3}\u{9c}\u{9}\u{86}\u{7f}\u{97}\u{8d}\u{8e}\u{b}\u{c}\u{d}\u{e}\u{f}\u{10}\u{11}\u{12}\u{13}\u{9d}\u{85}\u{8}\u{87}\u{18}\u{19}\u{92}\u{8f}\u{1c}\u{1d}\u{1e}\u{1f}\u{80}\u{81}\u{82}\u{83}\u{84}\u{a}\u{17}\u{1b}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{5}\u{6}\u{7}\u{90}\u{91}\u{16}\u{93}\u{94}\u{95}\u{96}\u{4}\u{98}\u{99}\u{9a}\u{9b}\u{14}\u{15}\u{9e}\u{1a} \u{a0}âäàáãåçñ¢.<(+|&éêëèíîïìß!$*);¬-/ÂÄÀÁÃÅÇÑ¦,%_>?øÉÊËÈÍÎÏÌ`:#@'=\u{22}Øabcdefghi«»ðýþ±°jklmnopqrªºæ¸Æ€µ~stuvwxyz¡¿ÐÝÞ®^£¥·©§¶¼½¾[]¯¨´×{ABCDEFGHI\u{ad}ôöòóõ}JKLMNOPQR¹ûüùúÿ\u{5c}÷STUVWXYZ²ÔÖÒÓÕ0123456789³ÛÜÙÚ\u{9f}"),
    ("cp1250", "€\u{fffe}‚\u{fffe}„…†‡\u{fffe}‰Š‹ŚŤŽŹ\u{fffe}‘’“”•–—\u{fffe}™š›śťžź\u{a0}ˇ˘Ł¤Ą¦§¨©Ş«¬\u{ad}®Ż°±˛ł´µ¶·¸ąş»Ľ˝ľżŔÁÂĂÄĹĆÇČÉĘËĚÍÎĎĐŃŇÓÔŐÖ×ŘŮÚŰÜÝŢßŕáâăäĺćçčéęëěíîďđńňóôőö÷řůúűüýţ˙"),
    ("cp1251", "ЂЃ‚ѓ„…†‡€‰Љ‹ЊЌЋЏђ‘’“”•–—\u{fffe}™љ›њќћџ\u{a0}ЎўЈ¤Ґ¦§Ё©Є«¬\u{ad}®Ї°±Ііґµ¶·ё№є»јЅѕїАБВГДЕЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯабвгдежзийклмнопрстуфхцчшщъыьэюя"),
    ("cp1252", "€\u{fffe}‚ƒ„…†‡ˆ‰Š‹Œ\u{fffe}Ž\u{fffe}\u{fffe}‘’“”•–—˜™š›œ\u{fffe}žŸ\u{a0}¡¢£¤¥¦§¨©ª«¬\u{ad}®¯°±²³´µ¶·¸¹º»¼½¾¿ÀÁÂÃÄÅÆÇÈÉÊËÌÍÎÏÐÑÒÓÔÕÖ×ØÙÚÛÜÝÞßàáâãäåæçèéêëìíîïðñòóôõö÷øùúûüýþÿ"),
    ("cp1253", "€\u{fffe}‚ƒ„…†‡\u{fffe}‰\u{fffe}‹\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}‘’“”•–—\u{fffe}™\u{fffe}›\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{a0}΅Ά£¤¥¦§¨©\u{fffe}«¬\u{ad}®―°±²³΄µ¶·ΈΉΊ»Ό½ΎΏΐΑΒΓΔΕΖΗΘΙΚΛΜΝΞΟΠΡ\u{fffe}ΣΤΥΦΧΨΩΪΫάέήίΰαβγδεζηθικλμνξοπρςστυφχψωϊϋόύώ\u{fffe}"),
    ("cp1254", "€\u{fffe}‚ƒ„…†‡ˆ‰Š‹Œ\u{fffe}\u{fffe}\u{fffe}\u{fffe}‘’“”•–—˜™š›œ\u{fffe}\u{fffe}Ÿ\u{a0}¡¢£¤¥¦§¨©ª«¬\u{ad}®¯°±²³´µ¶·¸¹º»¼½¾¿ÀÁÂÃÄÅÆÇÈÉÊËÌÍÎÏĞÑÒÓÔÕÖ×ØÙÚÛÜİŞßàáâãäåæçèéêëìíîïğñòóôõö÷øùúûüışÿ"),
    ("cp1255", "€\u{fffe}‚ƒ„…†‡ˆ‰\u{fffe}‹\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}‘’“”•–—˜™\u{fffe}›\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{a0}¡¢£₪¥¦§¨©×«¬\u{ad}®¯°±²³´µ¶·¸¹÷»¼½¾¿\u{5b0}\u{5b1}\u{5b2}\u{5b3}\u{5b4}\u{5b5}\u{5b6}\u{5b7}\u{5b8}\u{5b9}\u{fffe}\u{5bb}\u{5bc}\u{5bd}־\u{5bf}׀\u{5c1}\u{5c2}׃װױײ׳״\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}אבגדהוזחטיךכלםמןנסעףפץצקרשת\u{fffe}\u{fffe}\u{200e}\u{200f}\u{fffe}"),
    ("cp1256", "€پ‚ƒ„…†‡ˆ‰ٹ‹Œچژڈگ‘’“”•–—ک™ڑ›œ\u{200c}\u{200d}ں\u{a0}،¢£¤¥¦§¨©ھ«¬\u{ad}®¯°±²³´µ¶·¸¹؛»¼½¾؟ہءآأؤإئابةتثجحخدذرزسشصض×طظعغـفقكàلâمنهوçèéêëىيîï\u{64b}\u{64c}\u{64d}\u{64e}ô\u{64f}\u{650}÷\u{651}ù\u{652}ûü\u{200e}\u{200f}ے"),
    ("cp1257", "€\u{fffe}‚\u{fffe}„…†‡\u{fffe}‰\u{fffe}‹\u{fffe}¨ˇ¸\u{fffe}‘’“”•–—\u{fffe}™\u{fffe}›\u{fffe}¯˛\u{fffe}\u{a0}\u{fffe}¢£¤\u{fffe}¦§Ø©Ŗ«¬\u{ad}®Æ°±²³´µ¶·ø¹ŗ»¼½¾æĄĮĀĆÄÅĘĒČÉŹĖĢĶĪĻŠŃŅÓŌÕÖ×ŲŁŚŪÜŻŽßąįāćäåęēčéźėģķīļšńņóōõö÷ųłśūüżž˙"),
    ("cp1258", "€\u{fffe}‚ƒ„…†‡ˆ‰\u{fffe}‹Œ\u{fffe}\u{fffe}\u{fffe}\u{fffe}‘’“”•–—˜™\u{fffe}›œ\u{fffe}\u{fffe}Ÿ\u{a0}¡¢£¤¥¦§¨©ª«¬\u{ad}®¯°±²³´µ¶·¸¹º»¼½¾¿ÀÁÂĂÄÅÆÇÈÉÊË\u{300}ÍÎÏĐÑ\u{309}ÓÔƠÖ×ØÙÚÛÜƯ\u{303}ßàáâăäåæçèéêë\u{301}íîïđñ\u{323}óôơö÷øùúûüư₫ÿ"),
    ("cp273", "\u{0}\u{1}\u{2}\u{3}\u{9c}\u{9}\u{86}\u{7f}\u{97}\u{8d}\u{8e}\u{b}\u{c}\u{d}\u{e}\u{f}\u{10}\u{11}\u{12}\u{13}\u{9d}\u{85}\u{8}\u{87}\u{18}\u{19}\u{92}\u{8f}\u{1c}\u{1d}\u{1e}\u{1f}\u{80}\u{81}\u{82}\u{83}\u{84}\u{a}\u{17}\u{1b}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{5}\u{6}\u{7}\u{90}\u{91}\u{16}\u{93}\u{94}\u{95}\u{96}\u{4}\u{98}\u{99}\u{9a}\u{9b}\u{14}\u{15}\u{9e}\u{1a} \u{a0}â{àáãåçñÄ.<(+!&éêëèíîïì~Ü$*);^-/Â[ÀÁÃÅÇÑö,%_>?øÉÊËÈÍÎÏÌ`:#§'=\u{22}Øabcdefghi«»ðýþ±°jklmnopqrªºæ¸Æ¤µßstuvwxyz¡¿ÐÝÞ®¢£¥·©@¶¼½¾¬|‾¨´×äABCDEFGHI\u{ad}ô¦òóõüJKLMNOPQR¹û}ùúÿÖ÷STUVWXYZ²Ô\u{5c}ÒÓÕ0123456789³Û]ÙÚ\u{9f}"),
    ("cp424", "\u{0}\u{1}\u{2}\u{3}\u{9c}\u{9}\u{86}\u{7f}\u{97}\u{8d}\u{8e}\u{b}\u{c}\u{d}\u{e}\u{f}\u{10}\u{11}\u{12}\u{13}\u{9d}\u{85}\u{8}\u{87}\u{18}\u{19}\u{92}\u{8f}\u{1c}\u{1d}\u{1e}\u{1f}\u{80}\u{81}\u{82}\u{83}\u{84}\u{a}\u{17}\u{1b}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{5}\u{6}\u{7}\u{90}\u{91}\u{16}\u{93}\u{94}\u{95}\u{96}\u{4}\u{98}\u{99}\u{9a}\u{9b}\u{14}\u{15}\u{9e}\u{1a} אבגדהוזחט¢.<(+|&יךכלםמןנס!$*);¬-/עףפץצקרש¦,%_>?\u{fffe}ת\u{fffe}\u{fffe}\u{a0}\u{fffe}\u{fffe}\u{fffe}‗`:#@'=\u{22}\u{fffe}abcdefghi«»\u{fffe}\u{fffe}\u{fffe}±°jklmnopqr\u{fffe}\u{fffe}\u{fffe}¸\u{fffe}¤µ~stuvwxyz\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}®^£¥·©§¶¼½¾[]¯¨´×{ABCDEFGHI\u{ad}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}}JKLMNOPQR¹\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{5c}÷STUVWXYZ²\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}0123456789³\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{9f}"),
    ("cp437", "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}"),
    ("cp500", "\u{0}\u{1}\u{2}\u{3}\u{9c}\u{9}\u{86}\u{7f}\u{97}\u{8d}\u{8e}\u{b}\u{c}\u{d}\u{e}\u{f}\u{10}\u{11}\u{12}\u{13}\u{9d}\u{85}\u{8}\u{87}\u{18}\u{19}\u{92}\u{8f}\u{1c}\u{1d}\u{1e}\u{1f}\u{80}\u{81}\u{82}\u{83}\u{84}\u{a}\u{17}\u{1b}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{5}\u{6}\u{7}\u{90}\u{91}\u{16}\u{93}\u{94}\u{95}\u{96}\u{4}\u{98}\u{99}\u{9a}\u{9b}\u{14}\u{15}\u{9e}\u{1a} \u{a0}âäàáãåçñ[.<(+!&éêëèíîïìß]$*);^-/ÂÄÀÁÃÅÇÑ¦,%_>?øÉÊËÈÍÎÏÌ`:#@'=\u{22}Øabcdefghi«»ðýþ±°jklmnopqrªºæ¸Æ¤µ~stuvwxyz¡¿ÐÝÞ®¢£¥·©§¶¼½¾¬|¯¨´×{ABCDEFGHI\u{ad}ôöòóõ}JKLMNOPQR¹ûüùúÿ\u{5c}÷STUVWXYZ²ÔÖÒÓÕ0123456789³ÛÜÙÚ\u{9f}"),
    ("cp720", "\u{80}\u{81}éâ\u{84}à\u{86}çêëèïî\u{8d}\u{8e}\u{8f}\u{90}\u{651}\u{652}ô¤ـûùءآأؤ£إئابةتثجحخدذرزسشص«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀ضطظعغفµقكلمنهوىي≡\u{64b}\u{64c}\u{64d}\u{64e}\u{64f}\u{650}≈°∙·√ⁿ²■\u{a0}"),
    ("cp737", "ΑΒΓΔΕΖΗΘΙΚΛΜΝΞΟΠΡΣΤΥΦΧΨΩαβγδεζηθικλμνξοπρσςτυφχψ░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀ωάέήϊίόύϋώΆΈΉΊΌΎΏ±≥≤ΪΫ÷≈°∙·√ⁿ²■\u{a0}"),
    ("cp775", "ĆüéāäģåćłēŖŗīŹÄÅÉæÆōöĢ¢ŚśÖÜø£Ø×¤ĀĪóŻżź”¦©®¬½¼Ł«»░▒▓│┤ĄČĘĖ╣║╗╝ĮŠ┐└┴┬├─┼ŲŪ╚╔╩╦╠═╬Žąčęėįšųūž┘┌█▄▌▐▀ÓßŌŃõÕµńĶķĻļņĒŅ’\u{ad}±“¾¶§÷„°∙·¹³²■\u{a0}"),
    ("cp850", "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜø£Ø×ƒáíóúñÑªº¿®¬½¼¡«»░▒▓│┤ÁÂÀ©╣║╗╝¢¥┐└┴┬├─┼ãÃ╚╔╩╦╠═╬¤ðÐÊËÈıÍÎÏ┘┌█▄¦Ì▀ÓßÔÒõÕµþÞÚÛÙýÝ¯´\u{ad}±‗¾¶§÷¸°¨·¹³²■\u{a0}"),
    ("cp852", "ÇüéâäůćçłëŐőîŹÄĆÉĹĺôöĽľŚśÖÜŤťŁ×čáíóúĄąŽžĘę¬źČş«»░▒▓│┤ÁÂĚŞ╣║╗╝Żż┐└┴┬├─┼Ăă╚╔╩╦╠═╬¤đĐĎËďŇÍÎě┘┌█▄ŢŮ▀ÓßÔŃńňŠšŔÚŕŰýÝţ´\u{ad}˝˛ˇ˘§÷¸°¨˙űŘř■\u{a0}"),
    ("cp855", "ђЂѓЃёЁєЄѕЅіІїЇјЈљЉњЊћЋќЌўЎџЏюЮъЪаАбБцЦдДеЕфФгГ«»░▒▓│┤хХиИ╣║╗╝йЙ┐└┴┬├─┼кК╚╔╩╦╠═╬¤лЛмМнНоОп┘┌█▄Пя▀ЯрРсСтТуУжЖвВьЬ№\u{ad}ыЫзЗшШэЭщЩчЧ§■\u{a0}"),
    ("cp856", "אבגדהוזחטיךכלםמןנסעףפץצקרשת\u{fffe}£\u{fffe}×\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}®¬½¼\u{fffe}«»░▒▓│┤\u{fffe}\u{fffe}\u{fffe}©╣║╗╝¢¥┐└┴┬├─┼\u{fffe}\u{fffe}╚╔╩╦╠═╬¤\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}┘┌█▄¦\u{fffe}▀\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}µ\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}¯´\u{ad}±‗¾¶§÷¸°¨·¹³²■\u{a0}"),
    ("cp857", "ÇüéâäàåçêëèïîıÄÅÉæÆôöòûùİÖÜø£ØŞşáíóúñÑĞğ¿®¬½¼¡«»░▒▓│┤ÁÂÀ©╣║╗╝¢¥┐└┴┬├─┼ãÃ╚╔╩╦╠═╬¤ºªÊËÈ\u{fffe}ÍÎÏ┘┌█▄¦Ì▀ÓßÔÒõÕµ\u{fffe}×ÚÛÙìÿ¯´\u{ad}±\u{fffe}¾¶§÷¸°¨·¹³²■\u{a0}"),
    ("cp858", "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜø£Ø×ƒáíóúñÑªº¿®¬½¼¡«»░▒▓│┤ÁÂÀ©╣║╗╝¢¥┐└┴┬├─┼ãÃ╚╔╩╦╠═╬¤ðÐÊËÈ€ÍÎÏ┘┌█▄¦Ì▀ÓßÔÒõÕµþÞÚÛÙýÝ¯´\u{ad}±‗¾¶§÷¸°¨·¹³²■\u{a0}"),
    ("cp860", "ÇüéâãàÁçêÊèÍÔìÃÂÉÀÈôõòÚùÌÕÜ¢£Ù₧ÓáíóúñÑªº¿Ò¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}"),
    ("cp861", "ÇüéâäàåçêëèÐðÞÄÅÉæÆôöþûÝýÖÜø£Ø₧ƒáíóúÁÍÓÚ¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}"),
    ("cp862", "אבגדהוזחטיךכלםמןנסעףפץצקרשת¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}"),
    ("cp863", "ÇüéâÂà¶çêëèïî‗À§ÉÈÊôËÏûù¤ÔÜ¢£ÙÛƒ¦´óú¨¸³¯Î⌐¬½¼¾«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}"),
    ("cp864", "\u{0}\u{1}\u{2}\u{3}\u{4}\u{5}\u{6}\u{7}\u{8}\u{9}\u{a}\u{b}\u{c}\u{d}\u{e}\u{f}\u{10}\u{11}\u{12}\u{13}\u{14}\u{15}\u{16}\u{17}\u{18}\u{19}\u{1a}\u{1b}\u{1c}\u{1d}\u{1e}\u{1f} !\u{22}#$٪&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\u{5c}]^_`abcdefghijklmnopqrstuvwxyz{|}~\u{7f}°·∙√▒─│┼┤┬├┴┐┌└┘β∞φ±½¼≈«»ﻷﻸ\u{fffe}\u{fffe}ﻻﻼ\u{fffe}\u{a0}\u{ad}ﺂ£¤ﺄ\u{fffe}\u{fffe}ﺎﺏﺕﺙ،ﺝﺡﺥ٠١٢٣٤٥٦٧٨٩ﻑ؛ﺱﺵﺹ؟¢ﺀﺁﺃﺅﻊﺋﺍﺑﺓﺗﺛﺟﺣﺧﺩﺫﺭﺯﺳﺷﺻﺿﻁﻅﻋﻏ¦¬÷×ﻉـﻓﻗﻛﻟﻣﻧﻫﻭﻯﻳﺽﻌﻎﻍﻡﹽ\u{651}ﻥﻩﻬﻰﻲﻐﻕﻵﻶﻝﻙﻱ■\u{fffe}"),
    ("cp865", "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜø£Ø₧ƒáíóúñÑªº¿⌐¬½¼¡«¤░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}"),
    ("cp866", "АБВГДЕЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯабвгдежзийклмноп░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀рстуфхцчшщъыьэюяЁёЄєЇїЎў°∙·√№¤■\u{a0}"),
    ("cp869", "\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}Ά\u{fffe}·¬¦‘’Έ―ΉΊΪΌ\u{fffe}\u{fffe}ΎΫ©Ώ²³ά£έήίϊΐόύΑΒΓΔΕΖΗ½ΘΙ«»░▒▓│┤ΚΛΜΝ╣║╗╝ΞΟ┐└┴┬├─┼ΠΡ╚╔╩╦╠═╬ΣΤΥΦΧΨΩαβγ┘┌█▄δε▀ζηθικλμνξοπρσςτ΄\u{ad}±υφχ§ψ΅°¨ωϋΰώ■\u{a0}"),
    ("cp874", "€\u{fffe}\u{fffe}\u{fffe}\u{fffe}…\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}‘’“”•–—\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{a0}กขฃคฅฆงจฉชซฌญฎฏฐฑฒณดตถทธนบปผฝพฟภมยรฤลฦวศษสหฬอฮฯะ\u{e31}าำ\u{e34}\u{e35}\u{e36}\u{e37}\u{e38}\u{e39}\u{e3a}\u{fffe}\u{fffe}\u{fffe}\u{fffe}฿เแโใไๅๆ\u{e47}\u{e48}\u{e49}\u{e4a}\u{e4b}\u{e4c}\u{e4d}\u{e4e}๏๐๑๒๓๔๕๖๗๘๙๚๛\u{fffe}\u{fffe}\u{fffe}\u{fffe}"),
    ("cp875", "\u{0}\u{1}\u{2}\u{3}\u{9c}\u{9}\u{86}\u{7f}\u{97}\u{8d}\u{8e}\u{b}\u{c}\u{d}\u{e}\u{f}\u{10}\u{11}\u{12}\u{13}\u{9d}\u{85}\u{8}\u{87}\u{18}\u{19}\u{92}\u{8f}\u{1c}\u{1d}\u{1e}\u{1f}\u{80}\u{81}\u{82}\u{83}\u{84}\u{a}\u{17}\u{1b}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{5}\u{6}\u{7}\u{90}\u{91}\u{16}\u{93}\u{94}\u{95}\u{96}\u{4}\u{98}\u{99}\u{9a}\u{9b}\u{14}\u{15}\u{9e}\u{1a} ΑΒΓΔΕΖΗΘΙ[.<(+!&ΚΛΜΝΞΟΠΡΣ]$*);^-/ΤΥΦΧΨΩΪΫ|,%_>?¨ΆΈΉ\u{a0}ΊΌΎΏ`:#@'=\u{22}΅abcdefghiαβγδεζ°jklmnopqrηθικλμ´~stuvwxyzνξοπρσ£άέήϊίόύϋώςτυφχψ{ABCDEFGHI\u{ad}ωΐΰ‘―}JKLMNOPQR±½\u{1a}·’¦\u{5c}\u{1a}STUVWXYZ²§\u{1a}\u{1a}«¬0123456789³©\u{1a}\u{1a}»\u{9f}"),
    ("hp_roman8", "\u{80}\u{81}\u{82}\u{83}\u{84}\u{85}\u{86}\u{87}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{8d}\u{8e}\u{8f}\u{90}\u{91}\u{92}\u{93}\u{94}\u{95}\u{96}\u{97}\u{98}\u{99}\u{9a}\u{9b}\u{9c}\u{9d}\u{9e}\u{9f}\u{a0}ÀÂÈÊËÎÏ´ˋˆ¨˜ÙÛ₤¯Ýý°ÇçÑñ¡¿¤£¥§ƒ¢âêôûáéóúàèòùäëöüÅîØÆåíøæÄìÖÜÉïßÔÁÃãÐðÍÌÓÒÕõŠšÚŸÿÞþ·µ¶¾—¼½ªº«■»±\u{fffe}"),
    ("iso8859_1", "\u{80}\u{81}\u{82}\u{83}\u{84}\u{85}\u{86}\u{87}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{8d}\u{8e}\u{8f}\u{90}\u{91}\u{92}\u{93}\u{94}\u{95}\u{96}\u{97}\u{98}\u{99}\u{9a}\u{9b}\u{9c}\u{9d}\u{9e}\u{9f}\u{a0}¡¢£¤¥¦§¨©ª«¬\u{ad}®¯°±²³´µ¶·¸¹º»¼½¾¿ÀÁÂÃÄÅÆÇÈÉÊËÌÍÎÏÐÑÒÓÔÕÖ×ØÙÚÛÜÝÞßàáâãäåæçèéêëìíîïðñòóôõö÷øùúûüýþÿ"),
    ("iso8859_10", "\u{80}\u{81}\u{82}\u{83}\u{84}\u{85}\u{86}\u{87}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{8d}\u{8e}\u{8f}\u{90}\u{91}\u{92}\u{93}\u{94}\u{95}\u{96}\u{97}\u{98}\u{99}\u{9a}\u{9b}\u{9c}\u{9d}\u{9e}\u{9f}\u{a0}ĄĒĢĪĨĶ§ĻĐŠŦŽ\u{ad}ŪŊ°ąēģīĩķ·ļđšŧž―ūŋĀÁÂÃÄÅÆĮČÉĘËĖÍÎÏÐŅŌÓÔÕÖŨØŲÚÛÜÝÞßāáâãäåæįčéęëėíîïðņōóôõöũøųúûüýþĸ"),
    ("iso8859_11", "\u{80}\u{81}\u{82}\u{83}\u{84}\u{85}\u{86}\u{87}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{8d}\u{8e}\u{8f}\u{90}\u{91}\u{92}\u{93}\u{94}\u{95}\u{96}\u{97}\u{98}\u{99}\u{9a}\u{9b}\u{9c}\u{9d}\u{9e}\u{9f}\u{a0}กขฃคฅฆงจฉชซฌญฎฏฐฑฒณดตถทธนบปผฝพฟภมยรฤลฦวศษสหฬอฮฯะ\u{e31}าำ\u{e34}\u{e35}\u{e36}\u{e37}\u{e38}\u{e39}\u{e3a}\u{fffe}\u{fffe}\u{fffe}\u{fffe}฿เแโใไๅๆ\u{e47}\u{e48}\u{e49}\u{e4a}\u{e4b}\u{e4c}\u{e4d}\u{e4e}๏๐๑๒๓๔๕๖๗๘๙๚๛\u{fffe}\u{fffe}\u{fffe}\u{fffe}"),
    ("iso8859_13", "\u{80}\u{81}\u{82}\u{83}\u{84}\u{85}\u{86}\u{87}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{8d}\u{8e}\u{8f}\u{90}\u{91}\u{92}\u{93}\u{94}\u{95}\u{96}\u{97}\u{98}\u{99}\u{9a}\u{9b}\u{9c}\u{9d}\u{9e}\u{9f}\u{a0}”¢£¤„¦§Ø©Ŗ«¬\u{ad}®Æ°±²³“µ¶·ø¹ŗ»¼½¾æĄĮĀĆÄÅĘĒČÉŹĖĢĶĪĻŠŃŅÓŌÕÖ×ŲŁŚŪÜŻŽßąįāćäåęēčéźėģķīļšńņóōõö÷ųłśūüżž’"),
    ("iso8859_14", "\u{80}\u{81}\u{82}\u{83}\u{84}\u{85}\u{86}\u{87}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{8d}\u{8e}\u{8f}\u{90}\u{91}\u{92}\u{93}\u{94}\u{95}\u{96}\u{97}\u{98}\u{99}\u{9a}\u{9b}\u{9c}\u{9d}\u{9e}\u{9f}\u{a0}Ḃḃ£ĊċḊ§Ẁ©ẂḋỲ\u{ad}®ŸḞḟĠġṀṁ¶ṖẁṗẃṠỳẄẅṡÀÁÂÃÄÅÆÇÈÉÊËÌÍÎÏŴÑÒÓÔÕÖṪØÙÚÛÜÝŶßàáâãäåæçèéêëìíîïŵñòóôõöṫøùúûüýŷÿ"),
    ("iso8859_15", "\u{80}\u{81}\u{82}\u{83}\u{84}\u{85}\u{86}\u{87}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{8d}\u{8e}\u{8f}\u{90}\u{91}\u{92}\u{93}\u{94}\u{95}\u{96}\u{97}\u{98}\u{99}\u{9a}\u{9b}\u{9c}\u{9d}\u{9e}\u{9f}\u{a0}¡¢£€¥Š§š©ª«¬\u{ad}®¯°±²³Žµ¶·ž¹º»ŒœŸ¿ÀÁÂÃÄÅÆÇÈÉÊËÌÍÎÏÐÑÒÓÔÕÖ×ØÙÚÛÜÝÞßàáâãäåæçèéêëìíîïðñòóôõö÷øùúûüýþÿ"),
    ("iso8859_16", "\u{80}\u{81}\u{82}\u{83}\u{84}\u{85}\u{86}\u{87}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{8d}\u{8e}\u{8f}\u{90}\u{91}\u{92}\u{93}\u{94}\u{95}\u{96}\u{97}\u{98}\u{99}\u{9a}\u{9b}\u{9c}\u{9d}\u{9e}\u{9f}\u{a0}ĄąŁ€„Š§š©Ș«Ź\u{ad}źŻ°±ČłŽ”¶·žčș»ŒœŸżÀÁÂĂÄĆÆÇÈÉÊËÌÍÎÏĐŃÒÓÔŐÖŚŰÙÚÛÜĘȚßàáâăäćæçèéêëìíîïđńòóôőöśűùúûüęțÿ"),
    ("iso8859_2", "\u{80}\u{81}\u{82}\u{83}\u{84}\u{85}\u{86}\u{87}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{8d}\u{8e}\u{8f}\u{90}\u{91}\u{92}\u{93}\u{94}\u{95}\u{96}\u{97}\u{98}\u{99}\u{9a}\u{9b}\u{9c}\u{9d}\u{9e}\u{9f}\u{a0}Ą˘Ł¤ĽŚ§¨ŠŞŤŹ\u{ad}ŽŻ°ą˛ł´ľśˇ¸šşťź˝žżŔÁÂĂÄĹĆÇČÉĘËĚÍÎĎĐŃŇÓÔŐÖ×ŘŮÚŰÜÝŢßŕáâăäĺćçčéęëěíîďđńňóôőö÷řůúűüýţ˙"),
    ("iso8859_3", "\u{80}\u{81}\u{82}\u{83}\u{84}\u{85}\u{86}\u{87}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{8d}\u{8e}\u{8f}\u{90}\u{91}\u{92}\u{93}\u{94}\u{95}\u{96}\u{97}\u{98}\u{99}\u{9a}\u{9b}\u{9c}\u{9d}\u{9e}\u{9f}\u{a0}Ħ˘£¤\u{fffe}Ĥ§¨İŞĞĴ\u{ad}\u{fffe}Ż°ħ²³´µĥ·¸ışğĵ½\u{fffe}żÀÁÂ\u{fffe}ÄĊĈÇÈÉÊËÌÍÎÏ\u{fffe}ÑÒÓÔĠÖ×ĜÙÚÛÜŬŜßàáâ\u{fffe}äċĉçèéêëìíîï\u{fffe}ñòóôġö÷ĝùúûüŭŝ˙"),
    ("iso8859_4", "\u{80}\u{81}\u{82}\u{83}\u{84}\u{85}\u{86}\u{87}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{8d}\u{8e}\u{8f}\u{90}\u{91}\u{92}\u{93}\u{94}\u{95}\u{96}\u{97}\u{98}\u{99}\u{9a}\u{9b}\u{9c}\u{9d}\u{9e}\u{9f}\u{a0}ĄĸŖ¤ĨĻ§¨ŠĒĢŦ\u{ad}Ž¯°ą˛ŗ´ĩļˇ¸šēģŧŊžŋĀÁÂÃÄÅÆĮČÉĘËĖÍÎĪĐŅŌĶÔÕÖ×ØŲÚÛÜŨŪßāáâãäåæįčéęëėíîīđņōķôõö÷øųúûüũū˙"),
    ("iso8859_5", "\u{80}\u{81}\u{82}\u{83}\u{84}\u{85}\u{86}\u{87}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{8d}\u{8e}\u{8f}\u{90}\u{91}\u{92}\u{93}\u{94}\u{95}\u{96}\u{97}\u{98}\u{99}\u{9a}\u{9b}\u{9c}\u{9d}\u{9e}\u{9f}\u{a0}ЁЂЃЄЅІЇЈЉЊЋЌ\u{ad}ЎЏАБВГДЕЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯабвгдежзийклмнопрстуфхцчшщъыьэюя№ёђѓєѕіїјљњћќ§ўџ"),
    ("iso8859_6", "\u{80}\u{81}\u{82}\u{83}\u{84}\u{85}\u{86}\u{87}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{8d}\u{8e}\u{8f}\u{90}\u{91}\u{92}\u{93}\u{94}\u{95}\u{96}\u{97}\u{98}\u{99}\u{9a}\u{9b}\u{9c}\u{9d}\u{9e}\u{9f}\u{a0}\u{fffe}\u{fffe}\u{fffe}¤\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}،\u{ad}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}؛\u{fffe}\u{fffe}\u{fffe}؟\u{fffe}ءآأؤإئابةتثجحخدذرزسشصضطظعغ\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}ـفقكلمنهوىي\u{64b}\u{64c}\u{64d}\u{64e}\u{64f}\u{650}\u{651}\u{652}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}"),
    ("iso8859_7", "\u{80}\u{81}\u{82}\u{83}\u{84}\u{85}\u{86}\u{87}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{8d}\u{8e}\u{8f}\u{90}\u{91}\u{92}\u{93}\u{94}\u{95}\u{96}\u{97}\u{98}\u{99}\u{9a}\u{9b}\u{9c}\u{9d}\u{9e}\u{9f}\u{a0}‘’£€₯¦§¨©ͺ«¬\u{ad}\u{fffe}―°±²³΄΅Ά·ΈΉΊ»Ό½ΎΏΐΑΒΓΔΕΖΗΘΙΚΛΜΝΞΟΠΡ\u{fffe}ΣΤΥΦΧΨΩΪΫάέήίΰαβγδεζηθικλμνξοπρςστυφχψωϊϋόύώ\u{fffe}"),
    ("iso8859_8", "\u{80}\u{81}\u{82}\u{83}\u{84}\u{85}\u{86}\u{87}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{8d}\u{8e}\u{8f}\u{90}\u{91}\u{92}\u{93}\u{94}\u{95}\u{96}\u{97}\u{98}\u{99}\u{9a}\u{9b}\u{9c}\u{9d}\u{9e}\u{9f}\u{a0}\u{fffe}¢£¤¥¦§¨©×«¬\u{ad}®¯°±²³´µ¶·¸¹÷»¼½¾\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}‗אבגדהוזחטיךכלםמןנסעףפץצקרשת\u{fffe}\u{fffe}\u{200e}\u{200f}\u{fffe}"),
    ("iso8859_9", "\u{80}\u{81}\u{82}\u{83}\u{84}\u{85}\u{86}\u{87}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{8d}\u{8e}\u{8f}\u{90}\u{91}\u{92}\u{93}\u{94}\u{95}\u{96}\u{97}\u{98}\u{99}\u{9a}\u{9b}\u{9c}\u{9d}\u{9e}\u{9f}\u{a0}¡¢£¤¥¦§¨©ª«¬\u{ad}®¯°±²³´µ¶·¸¹º»¼½¾¿ÀÁÂÃÄÅÆÇÈÉÊËÌÍÎÏĞÑÒÓÔÕÖ×ØÙÚÛÜİŞßàáâãäåæçèéêëìíîïğñòóôõö÷øùúûüışÿ"),
    ("koi8_r", "─│┌┐└┘├┤┬┴┼▀▄█▌▐░▒▓⌠■∙√≈≤≥\u{a0}⌡°²·÷═║╒ё╓╔╕╖╗╘╙╚╛╜╝╞╟╠╡Ё╢╣╤╥╦╧╨╩╪╫╬©юабцдефгхийклмнопярстужвьызшэщчъЮАБЦДЕФГХИЙКЛМНОПЯРСТУЖВЬЫЗШЭЩЧЪ"),
    ("koi8_t", "қғ‚Ғ„…†‡\u{fffe}‰ҳ‹ҲҷҶ\u{fffe}Қ‘’“”•–—\u{fffe}™\u{fffe}›\u{fffe}\u{fffe}\u{fffe}\u{fffe}\u{fffe}ӯӮё¤ӣ¦§\u{fffe}\u{fffe}\u{fffe}«¬\u{ad}®\u{fffe}°±²Ё\u{fffe}Ӣ¶·\u{fffe}№\u{fffe}»\u{fffe}\u{fffe}\u{fffe}©юабцдефгхийклмнопярстужвьызшэщчъЮАБЦДЕФГХИЙКЛМНОПЯРСТУЖВЬЫЗШЭЩЧЪ"),
    ("koi8_u", "─│┌┐└┘├┤┬┴┼▀▄█▌▐░▒▓⌠■∙√≈≤≥\u{a0}⌡°²·÷═║╒ёє╔ії╗╘╙╚╛ґ╝╞╟╠╡ЁЄ╣ІЇ╦╧╨╩╪Ґ╬©юабцдефгхийклмнопярстужвьызшэщчъЮАБЦДЕФГХИЙКЛМНОПЯРСТУЖВЬЫЗШЭЩЧЪ"),
    ("kz1048", "ЂЃ‚ѓ„…†‡€‰Љ‹ЊҚҺЏђ‘’“”•–—\u{fffe}™љ›њқһџ\u{a0}ҰұӘ¤Ө¦§Ё©Ғ«¬\u{ad}®Ү°±Ііөµ¶·ё№ғ»әҢңүАБВГДЕЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯабвгдежзийклмнопрстуфхцчшщъыьэюя"),
    ("mac_arabic", "Ä\u{a0}ÇÉÑÖÜáàâäں«çéèêëí…îïñó»ôö÷úùûü !\u{22}#$٪&'()*+،-./٠١٢٣٤٥٦٧٨٩:؛<=>؟❊ءآأؤإئابةتثجحخدذرزسشصضطظعغ[\u{5c}]^_ـفقكلمنهوىي\u{64b}\u{64c}\u{64d}\u{64e}\u{64f}\u{650}\u{651}\u{652}پٹچەڤگڈڑ{|}ژے"),
    ("mac_croatian", "ÄÅÇÉÑÖÜáàâäãåçéèêëíìîïñóòôöõúùûü†°¢£§•¶ß®Š™´¨≠ŽØ∞±≤≥∆µ∂∑∏š∫ªºΩžø¿¡¬√ƒ≈Ć«Č…\u{a0}ÀÃÕŒœĐ—“”‘’÷◊\u{f8ff}©⁄€‹›Æ»–·‚„‰ÂćÁčÈÍÎÏÌÓÔđÒÚÛÙıˆ˜¯πË˚¸Êæˇ"),
    ("mac_cyrillic", "АБВГДЕЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯ†°Ґ£§•¶І®©™Ђђ≠Ѓѓ∞±≤≥іµґЈЄєЇїЉљЊњјЅ¬√ƒ≈∆«»…\u{a0}ЋћЌќѕ–—“”‘’÷„ЎўЏџ№Ёёяабвгдежзийклмнопрстуфхцчшщъыьэю€"),
    ("mac_farsi", "Ä\u{a0}ÇÉÑÖÜáàâäں«çéèêëí…îïñó»ôö÷úùûü !\u{22}#$٪&'()*+،-./۰۱۲۳۴۵۶۷۸۹:؛<=>؟❊ءآأؤإئابةتثجحخدذرزسشصضطظعغ[\u{5c}]^_ـفقكلمنهوىي\u{64b}\u{64c}\u{64d}\u{64e}\u{64f}\u{650}\u{651}\u{652}پٹچەڤگڈڑ{|}ژے"),
    ("mac_greek", "Ä¹²É³ÖÜ΅àâä΄¨çéèêë£™îï•½‰ôö¦€ùûü†ΓΔΘΛΞΠß®©ΣΪ§≠°·Α±≤≥¥ΒΕΖΗΙΚΜΦΫΨΩάΝ¬ΟΡ≈Τ«»…\u{a0}ΥΧΆΈœ–―“”‘’÷ΉΊΌΎέήίόΏύαβψδεφγηιξκλμνοπώρστθωςχυζϊϋΐΰ\u{ad}"),
    ("mac_iceland", "ÄÅÇÉÑÖÜáàâäãåçéèêëíìîïñóòôöõúùûüÝ°¢£§•¶ß®©™´¨≠ÆØ∞±≤≥¥µ∂∑∏π∫ªºΩæø¿¡¬√ƒ≈∆«»…\u{a0}ÀÃÕŒœ–—“”‘’÷◊ÿŸ⁄€ÐðÞþý·‚„‰ÂÊÁËÈÍÎÏÌÓÔ\u{f8ff}ÒÚÛÙıˆ˜¯˘˙˚¸˝˛ˇ"),
    ("mac_latin2", "ÄĀāÉĄÖÜáąČäčĆćéŹźĎíďĒēĖóėôöõúĚěü†°Ę£§•¶ß®©™ę¨≠ģĮįĪ≤≥īĶ∂∑łĻļĽľĹĺŅņŃ¬√ńŇ∆«»…\u{a0}ňŐÕőŌ–—“”‘’÷◊ōŔŕŘ‹›řŖŗŠ‚„šŚśÁŤťÍŽžŪÓÔūŮÚůŰűŲųÝýķŻŁżĢˇ"),
    ("mac_roman", "ÄÅÇÉÑÖÜáàâäãåçéèêëíìîïñóòôöõúùûü†°¢£§•¶ß®©™´¨≠ÆØ∞±≤≥¥µ∂∑∏π∫ªºΩæø¿¡¬√ƒ≈∆«»…\u{a0}ÀÃÕŒœ–—“”‘’÷◊ÿŸ⁄€‹›ﬁﬂ‡·‚„‰ÂÊÁËÈÍÎÏÌÓÔ\u{f8ff}ÒÚÛÙıˆ˜¯˘˙˚¸˝˛ˇ"),
    ("mac_romanian", "ÄÅÇÉÑÖÜáàâäãåçéèêëíìîïñóòôöõúùûü†°¢£§•¶ß®©™´¨≠ĂȘ∞±≤≥¥µ∂∑∏π∫ªºΩăș¿¡¬√ƒ≈∆«»…\u{a0}ÀÃÕŒœ–—“”‘’÷◊ÿŸ⁄€‹›Țț‡·‚„‰ÂÊÁËÈÍÎÏÌÓÔ\u{f8ff}ÒÚÛÙıˆ˜¯˘˙˚¸˝˛ˇ"),
    ("mac_turkish", "ÄÅÇÉÑÖÜáàâäãåçéèêëíìîïñóòôöõúùûü†°¢£§•¶ß®©™´¨≠ÆØ∞±≤≥¥µ∂∑∏π∫ªºΩæø¿¡¬√ƒ≈∆«»…\u{a0}ÀÃÕŒœ–—“”‘’÷◊ÿŸĞğİıŞş‡·‚„‰ÂÊÁËÈÍÎÏÌÓÔ\u{f8ff}ÒÚÛÙ\u{f8a0}ˆ˜¯˘˙˚¸˝˛ˇ"),
    ("palmos", "€\u{81}‚ƒ„…†‡ˆ‰Š‹Œ♦♣♥♠‘’“”•–—˜™š›œ\u{9d}\u{9e}Ÿ\u{a0}¡¢£¤¥¦§¨©ª«¬\u{ad}®¯°±²³´µ¶·¸¹º»¼½¾¿ÀÁÂÃÄÅÆÇÈÉÊËÌÍÎÏÐÑÒÓÔÕÖ×ØÙÚÛÜÝÞßàáâãäåæçèéêëìíîïðñòóôõö÷øùúûüýþÿ"),
    ("ptcp154", "ҖҒӮғ„…ҶҮҲүҠӢҢҚҺҸҗ‘’“”•–—ҳҷҡӣңқһҹ\u{a0}ЎўЈӨҘҰ§Ё©Ә«¬ӯ®Ҝ°ұІіҙө¶·ё№ә»јҪҫҝАБВГДЕЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯабвгдежзийклмнопрстуфхцчшщъыьэюя"),
    ("tis_620", "\u{80}\u{81}\u{82}\u{83}\u{84}\u{85}\u{86}\u{87}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{8d}\u{8e}\u{8f}\u{90}\u{91}\u{92}\u{93}\u{94}\u{95}\u{96}\u{97}\u{98}\u{99}\u{9a}\u{9b}\u{9c}\u{9d}\u{9e}\u{9f}\u{fffe}กขฃคฅฆงจฉชซฌญฎฏฐฑฒณดตถทธนบปผฝพฟภมยรฤลฦวศษสหฬอฮฯะ\u{e31}าำ\u{e34}\u{e35}\u{e36}\u{e37}\u{e38}\u{e39}\u{e3a}\u{fffe}\u{fffe}\u{fffe}\u{fffe}฿เแโใไๅๆ\u{e47}\u{e48}\u{e49}\u{e4a}\u{e4b}\u{e4c}\u{e4d}\u{e4e}๏๐๑๒๓๔๕๖๗๘๙๚๛\u{fffe}\u{fffe}\u{fffe}\u{fffe}"),
];

/// `str.lower()` for every character it changes (CPython 3.14, Unicode
/// 16.0), generated from Python; 'Σ' is left to the final-sigma rule.
const PY_LOWER: &[(u32, &str)] = &[
    (0x41, "a"),
    (0x42, "b"),
    (0x43, "c"),
    (0x44, "d"),
    (0x45, "e"),
    (0x46, "f"),
    (0x47, "g"),
    (0x48, "h"),
    (0x49, "i"),
    (0x4a, "j"),
    (0x4b, "k"),
    (0x4c, "l"),
    (0x4d, "m"),
    (0x4e, "n"),
    (0x4f, "o"),
    (0x50, "p"),
    (0x51, "q"),
    (0x52, "r"),
    (0x53, "s"),
    (0x54, "t"),
    (0x55, "u"),
    (0x56, "v"),
    (0x57, "w"),
    (0x58, "x"),
    (0x59, "y"),
    (0x5a, "z"),
    (0xc0, "\u{e0}"),
    (0xc1, "\u{e1}"),
    (0xc2, "\u{e2}"),
    (0xc3, "\u{e3}"),
    (0xc4, "\u{e4}"),
    (0xc5, "\u{e5}"),
    (0xc6, "\u{e6}"),
    (0xc7, "\u{e7}"),
    (0xc8, "\u{e8}"),
    (0xc9, "\u{e9}"),
    (0xca, "\u{ea}"),
    (0xcb, "\u{eb}"),
    (0xcc, "\u{ec}"),
    (0xcd, "\u{ed}"),
    (0xce, "\u{ee}"),
    (0xcf, "\u{ef}"),
    (0xd0, "\u{f0}"),
    (0xd1, "\u{f1}"),
    (0xd2, "\u{f2}"),
    (0xd3, "\u{f3}"),
    (0xd4, "\u{f4}"),
    (0xd5, "\u{f5}"),
    (0xd6, "\u{f6}"),
    (0xd8, "\u{f8}"),
    (0xd9, "\u{f9}"),
    (0xda, "\u{fa}"),
    (0xdb, "\u{fb}"),
    (0xdc, "\u{fc}"),
    (0xdd, "\u{fd}"),
    (0xde, "\u{fe}"),
    (0x100, "\u{101}"),
    (0x102, "\u{103}"),
    (0x104, "\u{105}"),
    (0x106, "\u{107}"),
    (0x108, "\u{109}"),
    (0x10a, "\u{10b}"),
    (0x10c, "\u{10d}"),
    (0x10e, "\u{10f}"),
    (0x110, "\u{111}"),
    (0x112, "\u{113}"),
    (0x114, "\u{115}"),
    (0x116, "\u{117}"),
    (0x118, "\u{119}"),
    (0x11a, "\u{11b}"),
    (0x11c, "\u{11d}"),
    (0x11e, "\u{11f}"),
    (0x120, "\u{121}"),
    (0x122, "\u{123}"),
    (0x124, "\u{125}"),
    (0x126, "\u{127}"),
    (0x128, "\u{129}"),
    (0x12a, "\u{12b}"),
    (0x12c, "\u{12d}"),
    (0x12e, "\u{12f}"),
    (0x130, "i\u{307}"),
    (0x132, "\u{133}"),
    (0x134, "\u{135}"),
    (0x136, "\u{137}"),
    (0x139, "\u{13a}"),
    (0x13b, "\u{13c}"),
    (0x13d, "\u{13e}"),
    (0x13f, "\u{140}"),
    (0x141, "\u{142}"),
    (0x143, "\u{144}"),
    (0x145, "\u{146}"),
    (0x147, "\u{148}"),
    (0x14a, "\u{14b}"),
    (0x14c, "\u{14d}"),
    (0x14e, "\u{14f}"),
    (0x150, "\u{151}"),
    (0x152, "\u{153}"),
    (0x154, "\u{155}"),
    (0x156, "\u{157}"),
    (0x158, "\u{159}"),
    (0x15a, "\u{15b}"),
    (0x15c, "\u{15d}"),
    (0x15e, "\u{15f}"),
    (0x160, "\u{161}"),
    (0x162, "\u{163}"),
    (0x164, "\u{165}"),
    (0x166, "\u{167}"),
    (0x168, "\u{169}"),
    (0x16a, "\u{16b}"),
    (0x16c, "\u{16d}"),
    (0x16e, "\u{16f}"),
    (0x170, "\u{171}"),
    (0x172, "\u{173}"),
    (0x174, "\u{175}"),
    (0x176, "\u{177}"),
    (0x178, "\u{ff}"),
    (0x179, "\u{17a}"),
    (0x17b, "\u{17c}"),
    (0x17d, "\u{17e}"),
    (0x181, "\u{253}"),
    (0x182, "\u{183}"),
    (0x184, "\u{185}"),
    (0x186, "\u{254}"),
    (0x187, "\u{188}"),
    (0x189, "\u{256}"),
    (0x18a, "\u{257}"),
    (0x18b, "\u{18c}"),
    (0x18e, "\u{1dd}"),
    (0x18f, "\u{259}"),
    (0x190, "\u{25b}"),
    (0x191, "\u{192}"),
    (0x193, "\u{260}"),
    (0x194, "\u{263}"),
    (0x196, "\u{269}"),
    (0x197, "\u{268}"),
    (0x198, "\u{199}"),
    (0x19c, "\u{26f}"),
    (0x19d, "\u{272}"),
    (0x19f, "\u{275}"),
    (0x1a0, "\u{1a1}"),
    (0x1a2, "\u{1a3}"),
    (0x1a4, "\u{1a5}"),
    (0x1a6, "\u{280}"),
    (0x1a7, "\u{1a8}"),
    (0x1a9, "\u{283}"),
    (0x1ac, "\u{1ad}"),
    (0x1ae, "\u{288}"),
    (0x1af, "\u{1b0}"),
    (0x1b1, "\u{28a}"),
    (0x1b2, "\u{28b}"),
    (0x1b3, "\u{1b4}"),
    (0x1b5, "\u{1b6}"),
    (0x1b7, "\u{292}"),
    (0x1b8, "\u{1b9}"),
    (0x1bc, "\u{1bd}"),
    (0x1c4, "\u{1c6}"),
    (0x1c5, "\u{1c6}"),
    (0x1c7, "\u{1c9}"),
    (0x1c8, "\u{1c9}"),
    (0x1ca, "\u{1cc}"),
    (0x1cb, "\u{1cc}"),
    (0x1cd, "\u{1ce}"),
    (0x1cf, "\u{1d0}"),
    (0x1d1, "\u{1d2}"),
    (0x1d3, "\u{1d4}"),
    (0x1d5, "\u{1d6}"),
    (0x1d7, "\u{1d8}"),
    (0x1d9, "\u{1da}"),
    (0x1db, "\u{1dc}"),
    (0x1de, "\u{1df}"),
    (0x1e0, "\u{1e1}"),
    (0x1e2, "\u{1e3}"),
    (0x1e4, "\u{1e5}"),
    (0x1e6, "\u{1e7}"),
    (0x1e8, "\u{1e9}"),
    (0x1ea, "\u{1eb}"),
    (0x1ec, "\u{1ed}"),
    (0x1ee, "\u{1ef}"),
    (0x1f1, "\u{1f3}"),
    (0x1f2, "\u{1f3}"),
    (0x1f4, "\u{1f5}"),
    (0x1f6, "\u{195}"),
    (0x1f7, "\u{1bf}"),
    (0x1f8, "\u{1f9}"),
    (0x1fa, "\u{1fb}"),
    (0x1fc, "\u{1fd}"),
    (0x1fe, "\u{1ff}"),
    (0x200, "\u{201}"),
    (0x202, "\u{203}"),
    (0x204, "\u{205}"),
    (0x206, "\u{207}"),
    (0x208, "\u{209}"),
    (0x20a, "\u{20b}"),
    (0x20c, "\u{20d}"),
    (0x20e, "\u{20f}"),
    (0x210, "\u{211}"),
    (0x212, "\u{213}"),
    (0x214, "\u{215}"),
    (0x216, "\u{217}"),
    (0x218, "\u{219}"),
    (0x21a, "\u{21b}"),
    (0x21c, "\u{21d}"),
    (0x21e, "\u{21f}"),
    (0x220, "\u{19e}"),
    (0x222, "\u{223}"),
    (0x224, "\u{225}"),
    (0x226, "\u{227}"),
    (0x228, "\u{229}"),
    (0x22a, "\u{22b}"),
    (0x22c, "\u{22d}"),
    (0x22e, "\u{22f}"),
    (0x230, "\u{231}"),
    (0x232, "\u{233}"),
    (0x23a, "\u{2c65}"),
    (0x23b, "\u{23c}"),
    (0x23d, "\u{19a}"),
    (0x23e, "\u{2c66}"),
    (0x241, "\u{242}"),
    (0x243, "\u{180}"),
    (0x244, "\u{289}"),
    (0x245, "\u{28c}"),
    (0x246, "\u{247}"),
    (0x248, "\u{249}"),
    (0x24a, "\u{24b}"),
    (0x24c, "\u{24d}"),
    (0x24e, "\u{24f}"),
    (0x370, "\u{371}"),
    (0x372, "\u{373}"),
    (0x376, "\u{377}"),
    (0x37f, "\u{3f3}"),
    (0x386, "\u{3ac}"),
    (0x388, "\u{3ad}"),
    (0x389, "\u{3ae}"),
    (0x38a, "\u{3af}"),
    (0x38c, "\u{3cc}"),
    (0x38e, "\u{3cd}"),
    (0x38f, "\u{3ce}"),
    (0x391, "\u{3b1}"),
    (0x392, "\u{3b2}"),
    (0x393, "\u{3b3}"),
    (0x394, "\u{3b4}"),
    (0x395, "\u{3b5}"),
    (0x396, "\u{3b6}"),
    (0x397, "\u{3b7}"),
    (0x398, "\u{3b8}"),
    (0x399, "\u{3b9}"),
    (0x39a, "\u{3ba}"),
    (0x39b, "\u{3bb}"),
    (0x39c, "\u{3bc}"),
    (0x39d, "\u{3bd}"),
    (0x39e, "\u{3be}"),
    (0x39f, "\u{3bf}"),
    (0x3a0, "\u{3c0}"),
    (0x3a1, "\u{3c1}"),
    (0x3a4, "\u{3c4}"),
    (0x3a5, "\u{3c5}"),
    (0x3a6, "\u{3c6}"),
    (0x3a7, "\u{3c7}"),
    (0x3a8, "\u{3c8}"),
    (0x3a9, "\u{3c9}"),
    (0x3aa, "\u{3ca}"),
    (0x3ab, "\u{3cb}"),
    (0x3cf, "\u{3d7}"),
    (0x3d8, "\u{3d9}"),
    (0x3da, "\u{3db}"),
    (0x3dc, "\u{3dd}"),
    (0x3de, "\u{3df}"),
    (0x3e0, "\u{3e1}"),
    (0x3e2, "\u{3e3}"),
    (0x3e4, "\u{3e5}"),
    (0x3e6, "\u{3e7}"),
    (0x3e8, "\u{3e9}"),
    (0x3ea, "\u{3eb}"),
    (0x3ec, "\u{3ed}"),
    (0x3ee, "\u{3ef}"),
    (0x3f4, "\u{3b8}"),
    (0x3f7, "\u{3f8}"),
    (0x3f9, "\u{3f2}"),
    (0x3fa, "\u{3fb}"),
    (0x3fd, "\u{37b}"),
    (0x3fe, "\u{37c}"),
    (0x3ff, "\u{37d}"),
    (0x400, "\u{450}"),
    (0x401, "\u{451}"),
    (0x402, "\u{452}"),
    (0x403, "\u{453}"),
    (0x404, "\u{454}"),
    (0x405, "\u{455}"),
    (0x406, "\u{456}"),
    (0x407, "\u{457}"),
    (0x408, "\u{458}"),
    (0x409, "\u{459}"),
    (0x40a, "\u{45a}"),
    (0x40b, "\u{45b}"),
    (0x40c, "\u{45c}"),
    (0x40d, "\u{45d}"),
    (0x40e, "\u{45e}"),
    (0x40f, "\u{45f}"),
    (0x410, "\u{430}"),
    (0x411, "\u{431}"),
    (0x412, "\u{432}"),
    (0x413, "\u{433}"),
    (0x414, "\u{434}"),
    (0x415, "\u{435}"),
    (0x416, "\u{436}"),
    (0x417, "\u{437}"),
    (0x418, "\u{438}"),
    (0x419, "\u{439}"),
    (0x41a, "\u{43a}"),
    (0x41b, "\u{43b}"),
    (0x41c, "\u{43c}"),
    (0x41d, "\u{43d}"),
    (0x41e, "\u{43e}"),
    (0x41f, "\u{43f}"),
    (0x420, "\u{440}"),
    (0x421, "\u{441}"),
    (0x422, "\u{442}"),
    (0x423, "\u{443}"),
    (0x424, "\u{444}"),
    (0x425, "\u{445}"),
    (0x426, "\u{446}"),
    (0x427, "\u{447}"),
    (0x428, "\u{448}"),
    (0x429, "\u{449}"),
    (0x42a, "\u{44a}"),
    (0x42b, "\u{44b}"),
    (0x42c, "\u{44c}"),
    (0x42d, "\u{44d}"),
    (0x42e, "\u{44e}"),
    (0x42f, "\u{44f}"),
    (0x460, "\u{461}"),
    (0x462, "\u{463}"),
    (0x464, "\u{465}"),
    (0x466, "\u{467}"),
    (0x468, "\u{469}"),
    (0x46a, "\u{46b}"),
    (0x46c, "\u{46d}"),
    (0x46e, "\u{46f}"),
    (0x470, "\u{471}"),
    (0x472, "\u{473}"),
    (0x474, "\u{475}"),
    (0x476, "\u{477}"),
    (0x478, "\u{479}"),
    (0x47a, "\u{47b}"),
    (0x47c, "\u{47d}"),
    (0x47e, "\u{47f}"),
    (0x480, "\u{481}"),
    (0x48a, "\u{48b}"),
    (0x48c, "\u{48d}"),
    (0x48e, "\u{48f}"),
    (0x490, "\u{491}"),
    (0x492, "\u{493}"),
    (0x494, "\u{495}"),
    (0x496, "\u{497}"),
    (0x498, "\u{499}"),
    (0x49a, "\u{49b}"),
    (0x49c, "\u{49d}"),
    (0x49e, "\u{49f}"),
    (0x4a0, "\u{4a1}"),
    (0x4a2, "\u{4a3}"),
    (0x4a4, "\u{4a5}"),
    (0x4a6, "\u{4a7}"),
    (0x4a8, "\u{4a9}"),
    (0x4aa, "\u{4ab}"),
    (0x4ac, "\u{4ad}"),
    (0x4ae, "\u{4af}"),
    (0x4b0, "\u{4b1}"),
    (0x4b2, "\u{4b3}"),
    (0x4b4, "\u{4b5}"),
    (0x4b6, "\u{4b7}"),
    (0x4b8, "\u{4b9}"),
    (0x4ba, "\u{4bb}"),
    (0x4bc, "\u{4bd}"),
    (0x4be, "\u{4bf}"),
    (0x4c0, "\u{4cf}"),
    (0x4c1, "\u{4c2}"),
    (0x4c3, "\u{4c4}"),
    (0x4c5, "\u{4c6}"),
    (0x4c7, "\u{4c8}"),
    (0x4c9, "\u{4ca}"),
    (0x4cb, "\u{4cc}"),
    (0x4cd, "\u{4ce}"),
    (0x4d0, "\u{4d1}"),
    (0x4d2, "\u{4d3}"),
    (0x4d4, "\u{4d5}"),
    (0x4d6, "\u{4d7}"),
    (0x4d8, "\u{4d9}"),
    (0x4da, "\u{4db}"),
    (0x4dc, "\u{4dd}"),
    (0x4de, "\u{4df}"),
    (0x4e0, "\u{4e1}"),
    (0x4e2, "\u{4e3}"),
    (0x4e4, "\u{4e5}"),
    (0x4e6, "\u{4e7}"),
    (0x4e8, "\u{4e9}"),
    (0x4ea, "\u{4eb}"),
    (0x4ec, "\u{4ed}"),
    (0x4ee, "\u{4ef}"),
    (0x4f0, "\u{4f1}"),
    (0x4f2, "\u{4f3}"),
    (0x4f4, "\u{4f5}"),
    (0x4f6, "\u{4f7}"),
    (0x4f8, "\u{4f9}"),
    (0x4fa, "\u{4fb}"),
    (0x4fc, "\u{4fd}"),
    (0x4fe, "\u{4ff}"),
    (0x500, "\u{501}"),
    (0x502, "\u{503}"),
    (0x504, "\u{505}"),
    (0x506, "\u{507}"),
    (0x508, "\u{509}"),
    (0x50a, "\u{50b}"),
    (0x50c, "\u{50d}"),
    (0x50e, "\u{50f}"),
    (0x510, "\u{511}"),
    (0x512, "\u{513}"),
    (0x514, "\u{515}"),
    (0x516, "\u{517}"),
    (0x518, "\u{519}"),
    (0x51a, "\u{51b}"),
    (0x51c, "\u{51d}"),
    (0x51e, "\u{51f}"),
    (0x520, "\u{521}"),
    (0x522, "\u{523}"),
    (0x524, "\u{525}"),
    (0x526, "\u{527}"),
    (0x528, "\u{529}"),
    (0x52a, "\u{52b}"),
    (0x52c, "\u{52d}"),
    (0x52e, "\u{52f}"),
    (0x531, "\u{561}"),
    (0x532, "\u{562}"),
    (0x533, "\u{563}"),
    (0x534, "\u{564}"),
    (0x535, "\u{565}"),
    (0x536, "\u{566}"),
    (0x537, "\u{567}"),
    (0x538, "\u{568}"),
    (0x539, "\u{569}"),
    (0x53a, "\u{56a}"),
    (0x53b, "\u{56b}"),
    (0x53c, "\u{56c}"),
    (0x53d, "\u{56d}"),
    (0x53e, "\u{56e}"),
    (0x53f, "\u{56f}"),
    (0x540, "\u{570}"),
    (0x541, "\u{571}"),
    (0x542, "\u{572}"),
    (0x543, "\u{573}"),
    (0x544, "\u{574}"),
    (0x545, "\u{575}"),
    (0x546, "\u{576}"),
    (0x547, "\u{577}"),
    (0x548, "\u{578}"),
    (0x549, "\u{579}"),
    (0x54a, "\u{57a}"),
    (0x54b, "\u{57b}"),
    (0x54c, "\u{57c}"),
    (0x54d, "\u{57d}"),
    (0x54e, "\u{57e}"),
    (0x54f, "\u{57f}"),
    (0x550, "\u{580}"),
    (0x551, "\u{581}"),
    (0x552, "\u{582}"),
    (0x553, "\u{583}"),
    (0x554, "\u{584}"),
    (0x555, "\u{585}"),
    (0x556, "\u{586}"),
    (0x10a0, "\u{2d00}"),
    (0x10a1, "\u{2d01}"),
    (0x10a2, "\u{2d02}"),
    (0x10a3, "\u{2d03}"),
    (0x10a4, "\u{2d04}"),
    (0x10a5, "\u{2d05}"),
    (0x10a6, "\u{2d06}"),
    (0x10a7, "\u{2d07}"),
    (0x10a8, "\u{2d08}"),
    (0x10a9, "\u{2d09}"),
    (0x10aa, "\u{2d0a}"),
    (0x10ab, "\u{2d0b}"),
    (0x10ac, "\u{2d0c}"),
    (0x10ad, "\u{2d0d}"),
    (0x10ae, "\u{2d0e}"),
    (0x10af, "\u{2d0f}"),
    (0x10b0, "\u{2d10}"),
    (0x10b1, "\u{2d11}"),
    (0x10b2, "\u{2d12}"),
    (0x10b3, "\u{2d13}"),
    (0x10b4, "\u{2d14}"),
    (0x10b5, "\u{2d15}"),
    (0x10b6, "\u{2d16}"),
    (0x10b7, "\u{2d17}"),
    (0x10b8, "\u{2d18}"),
    (0x10b9, "\u{2d19}"),
    (0x10ba, "\u{2d1a}"),
    (0x10bb, "\u{2d1b}"),
    (0x10bc, "\u{2d1c}"),
    (0x10bd, "\u{2d1d}"),
    (0x10be, "\u{2d1e}"),
    (0x10bf, "\u{2d1f}"),
    (0x10c0, "\u{2d20}"),
    (0x10c1, "\u{2d21}"),
    (0x10c2, "\u{2d22}"),
    (0x10c3, "\u{2d23}"),
    (0x10c4, "\u{2d24}"),
    (0x10c5, "\u{2d25}"),
    (0x10c7, "\u{2d27}"),
    (0x10cd, "\u{2d2d}"),
    (0x13a0, "\u{ab70}"),
    (0x13a1, "\u{ab71}"),
    (0x13a2, "\u{ab72}"),
    (0x13a3, "\u{ab73}"),
    (0x13a4, "\u{ab74}"),
    (0x13a5, "\u{ab75}"),
    (0x13a6, "\u{ab76}"),
    (0x13a7, "\u{ab77}"),
    (0x13a8, "\u{ab78}"),
    (0x13a9, "\u{ab79}"),
    (0x13aa, "\u{ab7a}"),
    (0x13ab, "\u{ab7b}"),
    (0x13ac, "\u{ab7c}"),
    (0x13ad, "\u{ab7d}"),
    (0x13ae, "\u{ab7e}"),
    (0x13af, "\u{ab7f}"),
    (0x13b0, "\u{ab80}"),
    (0x13b1, "\u{ab81}"),
    (0x13b2, "\u{ab82}"),
    (0x13b3, "\u{ab83}"),
    (0x13b4, "\u{ab84}"),
    (0x13b5, "\u{ab85}"),
    (0x13b6, "\u{ab86}"),
    (0x13b7, "\u{ab87}"),
    (0x13b8, "\u{ab88}"),
    (0x13b9, "\u{ab89}"),
    (0x13ba, "\u{ab8a}"),
    (0x13bb, "\u{ab8b}"),
    (0x13bc, "\u{ab8c}"),
    (0x13bd, "\u{ab8d}"),
    (0x13be, "\u{ab8e}"),
    (0x13bf, "\u{ab8f}"),
    (0x13c0, "\u{ab90}"),
    (0x13c1, "\u{ab91}"),
    (0x13c2, "\u{ab92}"),
    (0x13c3, "\u{ab93}"),
    (0x13c4, "\u{ab94}"),
    (0x13c5, "\u{ab95}"),
    (0x13c6, "\u{ab96}"),
    (0x13c7, "\u{ab97}"),
    (0x13c8, "\u{ab98}"),
    (0x13c9, "\u{ab99}"),
    (0x13ca, "\u{ab9a}"),
    (0x13cb, "\u{ab9b}"),
    (0x13cc, "\u{ab9c}"),
    (0x13cd, "\u{ab9d}"),
    (0x13ce, "\u{ab9e}"),
    (0x13cf, "\u{ab9f}"),
    (0x13d0, "\u{aba0}"),
    (0x13d1, "\u{aba1}"),
    (0x13d2, "\u{aba2}"),
    (0x13d3, "\u{aba3}"),
    (0x13d4, "\u{aba4}"),
    (0x13d5, "\u{aba5}"),
    (0x13d6, "\u{aba6}"),
    (0x13d7, "\u{aba7}"),
    (0x13d8, "\u{aba8}"),
    (0x13d9, "\u{aba9}"),
    (0x13da, "\u{abaa}"),
    (0x13db, "\u{abab}"),
    (0x13dc, "\u{abac}"),
    (0x13dd, "\u{abad}"),
    (0x13de, "\u{abae}"),
    (0x13df, "\u{abaf}"),
    (0x13e0, "\u{abb0}"),
    (0x13e1, "\u{abb1}"),
    (0x13e2, "\u{abb2}"),
    (0x13e3, "\u{abb3}"),
    (0x13e4, "\u{abb4}"),
    (0x13e5, "\u{abb5}"),
    (0x13e6, "\u{abb6}"),
    (0x13e7, "\u{abb7}"),
    (0x13e8, "\u{abb8}"),
    (0x13e9, "\u{abb9}"),
    (0x13ea, "\u{abba}"),
    (0x13eb, "\u{abbb}"),
    (0x13ec, "\u{abbc}"),
    (0x13ed, "\u{abbd}"),
    (0x13ee, "\u{abbe}"),
    (0x13ef, "\u{abbf}"),
    (0x13f0, "\u{13f8}"),
    (0x13f1, "\u{13f9}"),
    (0x13f2, "\u{13fa}"),
    (0x13f3, "\u{13fb}"),
    (0x13f4, "\u{13fc}"),
    (0x13f5, "\u{13fd}"),
    (0x1c89, "\u{1c8a}"),
    (0x1c90, "\u{10d0}"),
    (0x1c91, "\u{10d1}"),
    (0x1c92, "\u{10d2}"),
    (0x1c93, "\u{10d3}"),
    (0x1c94, "\u{10d4}"),
    (0x1c95, "\u{10d5}"),
    (0x1c96, "\u{10d6}"),
    (0x1c97, "\u{10d7}"),
    (0x1c98, "\u{10d8}"),
    (0x1c99, "\u{10d9}"),
    (0x1c9a, "\u{10da}"),
    (0x1c9b, "\u{10db}"),
    (0x1c9c, "\u{10dc}"),
    (0x1c9d, "\u{10dd}"),
    (0x1c9e, "\u{10de}"),
    (0x1c9f, "\u{10df}"),
    (0x1ca0, "\u{10e0}"),
    (0x1ca1, "\u{10e1}"),
    (0x1ca2, "\u{10e2}"),
    (0x1ca3, "\u{10e3}"),
    (0x1ca4, "\u{10e4}"),
    (0x1ca5, "\u{10e5}"),
    (0x1ca6, "\u{10e6}"),
    (0x1ca7, "\u{10e7}"),
    (0x1ca8, "\u{10e8}"),
    (0x1ca9, "\u{10e9}"),
    (0x1caa, "\u{10ea}"),
    (0x1cab, "\u{10eb}"),
    (0x1cac, "\u{10ec}"),
    (0x1cad, "\u{10ed}"),
    (0x1cae, "\u{10ee}"),
    (0x1caf, "\u{10ef}"),
    (0x1cb0, "\u{10f0}"),
    (0x1cb1, "\u{10f1}"),
    (0x1cb2, "\u{10f2}"),
    (0x1cb3, "\u{10f3}"),
    (0x1cb4, "\u{10f4}"),
    (0x1cb5, "\u{10f5}"),
    (0x1cb6, "\u{10f6}"),
    (0x1cb7, "\u{10f7}"),
    (0x1cb8, "\u{10f8}"),
    (0x1cb9, "\u{10f9}"),
    (0x1cba, "\u{10fa}"),
    (0x1cbd, "\u{10fd}"),
    (0x1cbe, "\u{10fe}"),
    (0x1cbf, "\u{10ff}"),
    (0x1e00, "\u{1e01}"),
    (0x1e02, "\u{1e03}"),
    (0x1e04, "\u{1e05}"),
    (0x1e06, "\u{1e07}"),
    (0x1e08, "\u{1e09}"),
    (0x1e0a, "\u{1e0b}"),
    (0x1e0c, "\u{1e0d}"),
    (0x1e0e, "\u{1e0f}"),
    (0x1e10, "\u{1e11}"),
    (0x1e12, "\u{1e13}"),
    (0x1e14, "\u{1e15}"),
    (0x1e16, "\u{1e17}"),
    (0x1e18, "\u{1e19}"),
    (0x1e1a, "\u{1e1b}"),
    (0x1e1c, "\u{1e1d}"),
    (0x1e1e, "\u{1e1f}"),
    (0x1e20, "\u{1e21}"),
    (0x1e22, "\u{1e23}"),
    (0x1e24, "\u{1e25}"),
    (0x1e26, "\u{1e27}"),
    (0x1e28, "\u{1e29}"),
    (0x1e2a, "\u{1e2b}"),
    (0x1e2c, "\u{1e2d}"),
    (0x1e2e, "\u{1e2f}"),
    (0x1e30, "\u{1e31}"),
    (0x1e32, "\u{1e33}"),
    (0x1e34, "\u{1e35}"),
    (0x1e36, "\u{1e37}"),
    (0x1e38, "\u{1e39}"),
    (0x1e3a, "\u{1e3b}"),
    (0x1e3c, "\u{1e3d}"),
    (0x1e3e, "\u{1e3f}"),
    (0x1e40, "\u{1e41}"),
    (0x1e42, "\u{1e43}"),
    (0x1e44, "\u{1e45}"),
    (0x1e46, "\u{1e47}"),
    (0x1e48, "\u{1e49}"),
    (0x1e4a, "\u{1e4b}"),
    (0x1e4c, "\u{1e4d}"),
    (0x1e4e, "\u{1e4f}"),
    (0x1e50, "\u{1e51}"),
    (0x1e52, "\u{1e53}"),
    (0x1e54, "\u{1e55}"),
    (0x1e56, "\u{1e57}"),
    (0x1e58, "\u{1e59}"),
    (0x1e5a, "\u{1e5b}"),
    (0x1e5c, "\u{1e5d}"),
    (0x1e5e, "\u{1e5f}"),
    (0x1e60, "\u{1e61}"),
    (0x1e62, "\u{1e63}"),
    (0x1e64, "\u{1e65}"),
    (0x1e66, "\u{1e67}"),
    (0x1e68, "\u{1e69}"),
    (0x1e6a, "\u{1e6b}"),
    (0x1e6c, "\u{1e6d}"),
    (0x1e6e, "\u{1e6f}"),
    (0x1e70, "\u{1e71}"),
    (0x1e72, "\u{1e73}"),
    (0x1e74, "\u{1e75}"),
    (0x1e76, "\u{1e77}"),
    (0x1e78, "\u{1e79}"),
    (0x1e7a, "\u{1e7b}"),
    (0x1e7c, "\u{1e7d}"),
    (0x1e7e, "\u{1e7f}"),
    (0x1e80, "\u{1e81}"),
    (0x1e82, "\u{1e83}"),
    (0x1e84, "\u{1e85}"),
    (0x1e86, "\u{1e87}"),
    (0x1e88, "\u{1e89}"),
    (0x1e8a, "\u{1e8b}"),
    (0x1e8c, "\u{1e8d}"),
    (0x1e8e, "\u{1e8f}"),
    (0x1e90, "\u{1e91}"),
    (0x1e92, "\u{1e93}"),
    (0x1e94, "\u{1e95}"),
    (0x1e9e, "\u{df}"),
    (0x1ea0, "\u{1ea1}"),
    (0x1ea2, "\u{1ea3}"),
    (0x1ea4, "\u{1ea5}"),
    (0x1ea6, "\u{1ea7}"),
    (0x1ea8, "\u{1ea9}"),
    (0x1eaa, "\u{1eab}"),
    (0x1eac, "\u{1ead}"),
    (0x1eae, "\u{1eaf}"),
    (0x1eb0, "\u{1eb1}"),
    (0x1eb2, "\u{1eb3}"),
    (0x1eb4, "\u{1eb5}"),
    (0x1eb6, "\u{1eb7}"),
    (0x1eb8, "\u{1eb9}"),
    (0x1eba, "\u{1ebb}"),
    (0x1ebc, "\u{1ebd}"),
    (0x1ebe, "\u{1ebf}"),
    (0x1ec0, "\u{1ec1}"),
    (0x1ec2, "\u{1ec3}"),
    (0x1ec4, "\u{1ec5}"),
    (0x1ec6, "\u{1ec7}"),
    (0x1ec8, "\u{1ec9}"),
    (0x1eca, "\u{1ecb}"),
    (0x1ecc, "\u{1ecd}"),
    (0x1ece, "\u{1ecf}"),
    (0x1ed0, "\u{1ed1}"),
    (0x1ed2, "\u{1ed3}"),
    (0x1ed4, "\u{1ed5}"),
    (0x1ed6, "\u{1ed7}"),
    (0x1ed8, "\u{1ed9}"),
    (0x1eda, "\u{1edb}"),
    (0x1edc, "\u{1edd}"),
    (0x1ede, "\u{1edf}"),
    (0x1ee0, "\u{1ee1}"),
    (0x1ee2, "\u{1ee3}"),
    (0x1ee4, "\u{1ee5}"),
    (0x1ee6, "\u{1ee7}"),
    (0x1ee8, "\u{1ee9}"),
    (0x1eea, "\u{1eeb}"),
    (0x1eec, "\u{1eed}"),
    (0x1eee, "\u{1eef}"),
    (0x1ef0, "\u{1ef1}"),
    (0x1ef2, "\u{1ef3}"),
    (0x1ef4, "\u{1ef5}"),
    (0x1ef6, "\u{1ef7}"),
    (0x1ef8, "\u{1ef9}"),
    (0x1efa, "\u{1efb}"),
    (0x1efc, "\u{1efd}"),
    (0x1efe, "\u{1eff}"),
    (0x1f08, "\u{1f00}"),
    (0x1f09, "\u{1f01}"),
    (0x1f0a, "\u{1f02}"),
    (0x1f0b, "\u{1f03}"),
    (0x1f0c, "\u{1f04}"),
    (0x1f0d, "\u{1f05}"),
    (0x1f0e, "\u{1f06}"),
    (0x1f0f, "\u{1f07}"),
    (0x1f18, "\u{1f10}"),
    (0x1f19, "\u{1f11}"),
    (0x1f1a, "\u{1f12}"),
    (0x1f1b, "\u{1f13}"),
    (0x1f1c, "\u{1f14}"),
    (0x1f1d, "\u{1f15}"),
    (0x1f28, "\u{1f20}"),
    (0x1f29, "\u{1f21}"),
    (0x1f2a, "\u{1f22}"),
    (0x1f2b, "\u{1f23}"),
    (0x1f2c, "\u{1f24}"),
    (0x1f2d, "\u{1f25}"),
    (0x1f2e, "\u{1f26}"),
    (0x1f2f, "\u{1f27}"),
    (0x1f38, "\u{1f30}"),
    (0x1f39, "\u{1f31}"),
    (0x1f3a, "\u{1f32}"),
    (0x1f3b, "\u{1f33}"),
    (0x1f3c, "\u{1f34}"),
    (0x1f3d, "\u{1f35}"),
    (0x1f3e, "\u{1f36}"),
    (0x1f3f, "\u{1f37}"),
    (0x1f48, "\u{1f40}"),
    (0x1f49, "\u{1f41}"),
    (0x1f4a, "\u{1f42}"),
    (0x1f4b, "\u{1f43}"),
    (0x1f4c, "\u{1f44}"),
    (0x1f4d, "\u{1f45}"),
    (0x1f59, "\u{1f51}"),
    (0x1f5b, "\u{1f53}"),
    (0x1f5d, "\u{1f55}"),
    (0x1f5f, "\u{1f57}"),
    (0x1f68, "\u{1f60}"),
    (0x1f69, "\u{1f61}"),
    (0x1f6a, "\u{1f62}"),
    (0x1f6b, "\u{1f63}"),
    (0x1f6c, "\u{1f64}"),
    (0x1f6d, "\u{1f65}"),
    (0x1f6e, "\u{1f66}"),
    (0x1f6f, "\u{1f67}"),
    (0x1f88, "\u{1f80}"),
    (0x1f89, "\u{1f81}"),
    (0x1f8a, "\u{1f82}"),
    (0x1f8b, "\u{1f83}"),
    (0x1f8c, "\u{1f84}"),
    (0x1f8d, "\u{1f85}"),
    (0x1f8e, "\u{1f86}"),
    (0x1f8f, "\u{1f87}"),
    (0x1f98, "\u{1f90}"),
    (0x1f99, "\u{1f91}"),
    (0x1f9a, "\u{1f92}"),
    (0x1f9b, "\u{1f93}"),
    (0x1f9c, "\u{1f94}"),
    (0x1f9d, "\u{1f95}"),
    (0x1f9e, "\u{1f96}"),
    (0x1f9f, "\u{1f97}"),
    (0x1fa8, "\u{1fa0}"),
    (0x1fa9, "\u{1fa1}"),
    (0x1faa, "\u{1fa2}"),
    (0x1fab, "\u{1fa3}"),
    (0x1fac, "\u{1fa4}"),
    (0x1fad, "\u{1fa5}"),
    (0x1fae, "\u{1fa6}"),
    (0x1faf, "\u{1fa7}"),
    (0x1fb8, "\u{1fb0}"),
    (0x1fb9, "\u{1fb1}"),
    (0x1fba, "\u{1f70}"),
    (0x1fbb, "\u{1f71}"),
    (0x1fbc, "\u{1fb3}"),
    (0x1fc8, "\u{1f72}"),
    (0x1fc9, "\u{1f73}"),
    (0x1fca, "\u{1f74}"),
    (0x1fcb, "\u{1f75}"),
    (0x1fcc, "\u{1fc3}"),
    (0x1fd8, "\u{1fd0}"),
    (0x1fd9, "\u{1fd1}"),
    (0x1fda, "\u{1f76}"),
    (0x1fdb, "\u{1f77}"),
    (0x1fe8, "\u{1fe0}"),
    (0x1fe9, "\u{1fe1}"),
    (0x1fea, "\u{1f7a}"),
    (0x1feb, "\u{1f7b}"),
    (0x1fec, "\u{1fe5}"),
    (0x1ff8, "\u{1f78}"),
    (0x1ff9, "\u{1f79}"),
    (0x1ffa, "\u{1f7c}"),
    (0x1ffb, "\u{1f7d}"),
    (0x1ffc, "\u{1ff3}"),
    (0x2126, "\u{3c9}"),
    (0x212a, "k"),
    (0x212b, "\u{e5}"),
    (0x2132, "\u{214e}"),
    (0x2160, "\u{2170}"),
    (0x2161, "\u{2171}"),
    (0x2162, "\u{2172}"),
    (0x2163, "\u{2173}"),
    (0x2164, "\u{2174}"),
    (0x2165, "\u{2175}"),
    (0x2166, "\u{2176}"),
    (0x2167, "\u{2177}"),
    (0x2168, "\u{2178}"),
    (0x2169, "\u{2179}"),
    (0x216a, "\u{217a}"),
    (0x216b, "\u{217b}"),
    (0x216c, "\u{217c}"),
    (0x216d, "\u{217d}"),
    (0x216e, "\u{217e}"),
    (0x216f, "\u{217f}"),
    (0x2183, "\u{2184}"),
    (0x24b6, "\u{24d0}"),
    (0x24b7, "\u{24d1}"),
    (0x24b8, "\u{24d2}"),
    (0x24b9, "\u{24d3}"),
    (0x24ba, "\u{24d4}"),
    (0x24bb, "\u{24d5}"),
    (0x24bc, "\u{24d6}"),
    (0x24bd, "\u{24d7}"),
    (0x24be, "\u{24d8}"),
    (0x24bf, "\u{24d9}"),
    (0x24c0, "\u{24da}"),
    (0x24c1, "\u{24db}"),
    (0x24c2, "\u{24dc}"),
    (0x24c3, "\u{24dd}"),
    (0x24c4, "\u{24de}"),
    (0x24c5, "\u{24df}"),
    (0x24c6, "\u{24e0}"),
    (0x24c7, "\u{24e1}"),
    (0x24c8, "\u{24e2}"),
    (0x24c9, "\u{24e3}"),
    (0x24ca, "\u{24e4}"),
    (0x24cb, "\u{24e5}"),
    (0x24cc, "\u{24e6}"),
    (0x24cd, "\u{24e7}"),
    (0x24ce, "\u{24e8}"),
    (0x24cf, "\u{24e9}"),
    (0x2c00, "\u{2c30}"),
    (0x2c01, "\u{2c31}"),
    (0x2c02, "\u{2c32}"),
    (0x2c03, "\u{2c33}"),
    (0x2c04, "\u{2c34}"),
    (0x2c05, "\u{2c35}"),
    (0x2c06, "\u{2c36}"),
    (0x2c07, "\u{2c37}"),
    (0x2c08, "\u{2c38}"),
    (0x2c09, "\u{2c39}"),
    (0x2c0a, "\u{2c3a}"),
    (0x2c0b, "\u{2c3b}"),
    (0x2c0c, "\u{2c3c}"),
    (0x2c0d, "\u{2c3d}"),
    (0x2c0e, "\u{2c3e}"),
    (0x2c0f, "\u{2c3f}"),
    (0x2c10, "\u{2c40}"),
    (0x2c11, "\u{2c41}"),
    (0x2c12, "\u{2c42}"),
    (0x2c13, "\u{2c43}"),
    (0x2c14, "\u{2c44}"),
    (0x2c15, "\u{2c45}"),
    (0x2c16, "\u{2c46}"),
    (0x2c17, "\u{2c47}"),
    (0x2c18, "\u{2c48}"),
    (0x2c19, "\u{2c49}"),
    (0x2c1a, "\u{2c4a}"),
    (0x2c1b, "\u{2c4b}"),
    (0x2c1c, "\u{2c4c}"),
    (0x2c1d, "\u{2c4d}"),
    (0x2c1e, "\u{2c4e}"),
    (0x2c1f, "\u{2c4f}"),
    (0x2c20, "\u{2c50}"),
    (0x2c21, "\u{2c51}"),
    (0x2c22, "\u{2c52}"),
    (0x2c23, "\u{2c53}"),
    (0x2c24, "\u{2c54}"),
    (0x2c25, "\u{2c55}"),
    (0x2c26, "\u{2c56}"),
    (0x2c27, "\u{2c57}"),
    (0x2c28, "\u{2c58}"),
    (0x2c29, "\u{2c59}"),
    (0x2c2a, "\u{2c5a}"),
    (0x2c2b, "\u{2c5b}"),
    (0x2c2c, "\u{2c5c}"),
    (0x2c2d, "\u{2c5d}"),
    (0x2c2e, "\u{2c5e}"),
    (0x2c2f, "\u{2c5f}"),
    (0x2c60, "\u{2c61}"),
    (0x2c62, "\u{26b}"),
    (0x2c63, "\u{1d7d}"),
    (0x2c64, "\u{27d}"),
    (0x2c67, "\u{2c68}"),
    (0x2c69, "\u{2c6a}"),
    (0x2c6b, "\u{2c6c}"),
    (0x2c6d, "\u{251}"),
    (0x2c6e, "\u{271}"),
    (0x2c6f, "\u{250}"),
    (0x2c70, "\u{252}"),
    (0x2c72, "\u{2c73}"),
    (0x2c75, "\u{2c76}"),
    (0x2c7e, "\u{23f}"),
    (0x2c7f, "\u{240}"),
    (0x2c80, "\u{2c81}"),
    (0x2c82, "\u{2c83}"),
    (0x2c84, "\u{2c85}"),
    (0x2c86, "\u{2c87}"),
    (0x2c88, "\u{2c89}"),
    (0x2c8a, "\u{2c8b}"),
    (0x2c8c, "\u{2c8d}"),
    (0x2c8e, "\u{2c8f}"),
    (0x2c90, "\u{2c91}"),
    (0x2c92, "\u{2c93}"),
    (0x2c94, "\u{2c95}"),
    (0x2c96, "\u{2c97}"),
    (0x2c98, "\u{2c99}"),
    (0x2c9a, "\u{2c9b}"),
    (0x2c9c, "\u{2c9d}"),
    (0x2c9e, "\u{2c9f}"),
    (0x2ca0, "\u{2ca1}"),
    (0x2ca2, "\u{2ca3}"),
    (0x2ca4, "\u{2ca5}"),
    (0x2ca6, "\u{2ca7}"),
    (0x2ca8, "\u{2ca9}"),
    (0x2caa, "\u{2cab}"),
    (0x2cac, "\u{2cad}"),
    (0x2cae, "\u{2caf}"),
    (0x2cb0, "\u{2cb1}"),
    (0x2cb2, "\u{2cb3}"),
    (0x2cb4, "\u{2cb5}"),
    (0x2cb6, "\u{2cb7}"),
    (0x2cb8, "\u{2cb9}"),
    (0x2cba, "\u{2cbb}"),
    (0x2cbc, "\u{2cbd}"),
    (0x2cbe, "\u{2cbf}"),
    (0x2cc0, "\u{2cc1}"),
    (0x2cc2, "\u{2cc3}"),
    (0x2cc4, "\u{2cc5}"),
    (0x2cc6, "\u{2cc7}"),
    (0x2cc8, "\u{2cc9}"),
    (0x2cca, "\u{2ccb}"),
    (0x2ccc, "\u{2ccd}"),
    (0x2cce, "\u{2ccf}"),
    (0x2cd0, "\u{2cd1}"),
    (0x2cd2, "\u{2cd3}"),
    (0x2cd4, "\u{2cd5}"),
    (0x2cd6, "\u{2cd7}"),
    (0x2cd8, "\u{2cd9}"),
    (0x2cda, "\u{2cdb}"),
    (0x2cdc, "\u{2cdd}"),
    (0x2cde, "\u{2cdf}"),
    (0x2ce0, "\u{2ce1}"),
    (0x2ce2, "\u{2ce3}"),
    (0x2ceb, "\u{2cec}"),
    (0x2ced, "\u{2cee}"),
    (0x2cf2, "\u{2cf3}"),
    (0xa640, "\u{a641}"),
    (0xa642, "\u{a643}"),
    (0xa644, "\u{a645}"),
    (0xa646, "\u{a647}"),
    (0xa648, "\u{a649}"),
    (0xa64a, "\u{a64b}"),
    (0xa64c, "\u{a64d}"),
    (0xa64e, "\u{a64f}"),
    (0xa650, "\u{a651}"),
    (0xa652, "\u{a653}"),
    (0xa654, "\u{a655}"),
    (0xa656, "\u{a657}"),
    (0xa658, "\u{a659}"),
    (0xa65a, "\u{a65b}"),
    (0xa65c, "\u{a65d}"),
    (0xa65e, "\u{a65f}"),
    (0xa660, "\u{a661}"),
    (0xa662, "\u{a663}"),
    (0xa664, "\u{a665}"),
    (0xa666, "\u{a667}"),
    (0xa668, "\u{a669}"),
    (0xa66a, "\u{a66b}"),
    (0xa66c, "\u{a66d}"),
    (0xa680, "\u{a681}"),
    (0xa682, "\u{a683}"),
    (0xa684, "\u{a685}"),
    (0xa686, "\u{a687}"),
    (0xa688, "\u{a689}"),
    (0xa68a, "\u{a68b}"),
    (0xa68c, "\u{a68d}"),
    (0xa68e, "\u{a68f}"),
    (0xa690, "\u{a691}"),
    (0xa692, "\u{a693}"),
    (0xa694, "\u{a695}"),
    (0xa696, "\u{a697}"),
    (0xa698, "\u{a699}"),
    (0xa69a, "\u{a69b}"),
    (0xa722, "\u{a723}"),
    (0xa724, "\u{a725}"),
    (0xa726, "\u{a727}"),
    (0xa728, "\u{a729}"),
    (0xa72a, "\u{a72b}"),
    (0xa72c, "\u{a72d}"),
    (0xa72e, "\u{a72f}"),
    (0xa732, "\u{a733}"),
    (0xa734, "\u{a735}"),
    (0xa736, "\u{a737}"),
    (0xa738, "\u{a739}"),
    (0xa73a, "\u{a73b}"),
    (0xa73c, "\u{a73d}"),
    (0xa73e, "\u{a73f}"),
    (0xa740, "\u{a741}"),
    (0xa742, "\u{a743}"),
    (0xa744, "\u{a745}"),
    (0xa746, "\u{a747}"),
    (0xa748, "\u{a749}"),
    (0xa74a, "\u{a74b}"),
    (0xa74c, "\u{a74d}"),
    (0xa74e, "\u{a74f}"),
    (0xa750, "\u{a751}"),
    (0xa752, "\u{a753}"),
    (0xa754, "\u{a755}"),
    (0xa756, "\u{a757}"),
    (0xa758, "\u{a759}"),
    (0xa75a, "\u{a75b}"),
    (0xa75c, "\u{a75d}"),
    (0xa75e, "\u{a75f}"),
    (0xa760, "\u{a761}"),
    (0xa762, "\u{a763}"),
    (0xa764, "\u{a765}"),
    (0xa766, "\u{a767}"),
    (0xa768, "\u{a769}"),
    (0xa76a, "\u{a76b}"),
    (0xa76c, "\u{a76d}"),
    (0xa76e, "\u{a76f}"),
    (0xa779, "\u{a77a}"),
    (0xa77b, "\u{a77c}"),
    (0xa77d, "\u{1d79}"),
    (0xa77e, "\u{a77f}"),
    (0xa780, "\u{a781}"),
    (0xa782, "\u{a783}"),
    (0xa784, "\u{a785}"),
    (0xa786, "\u{a787}"),
    (0xa78b, "\u{a78c}"),
    (0xa78d, "\u{265}"),
    (0xa790, "\u{a791}"),
    (0xa792, "\u{a793}"),
    (0xa796, "\u{a797}"),
    (0xa798, "\u{a799}"),
    (0xa79a, "\u{a79b}"),
    (0xa79c, "\u{a79d}"),
    (0xa79e, "\u{a79f}"),
    (0xa7a0, "\u{a7a1}"),
    (0xa7a2, "\u{a7a3}"),
    (0xa7a4, "\u{a7a5}"),
    (0xa7a6, "\u{a7a7}"),
    (0xa7a8, "\u{a7a9}"),
    (0xa7aa, "\u{266}"),
    (0xa7ab, "\u{25c}"),
    (0xa7ac, "\u{261}"),
    (0xa7ad, "\u{26c}"),
    (0xa7ae, "\u{26a}"),
    (0xa7b0, "\u{29e}"),
    (0xa7b1, "\u{287}"),
    (0xa7b2, "\u{29d}"),
    (0xa7b3, "\u{ab53}"),
    (0xa7b4, "\u{a7b5}"),
    (0xa7b6, "\u{a7b7}"),
    (0xa7b8, "\u{a7b9}"),
    (0xa7ba, "\u{a7bb}"),
    (0xa7bc, "\u{a7bd}"),
    (0xa7be, "\u{a7bf}"),
    (0xa7c0, "\u{a7c1}"),
    (0xa7c2, "\u{a7c3}"),
    (0xa7c4, "\u{a794}"),
    (0xa7c5, "\u{282}"),
    (0xa7c6, "\u{1d8e}"),
    (0xa7c7, "\u{a7c8}"),
    (0xa7c9, "\u{a7ca}"),
    (0xa7cb, "\u{264}"),
    (0xa7cc, "\u{a7cd}"),
    (0xa7d0, "\u{a7d1}"),
    (0xa7d6, "\u{a7d7}"),
    (0xa7d8, "\u{a7d9}"),
    (0xa7da, "\u{a7db}"),
    (0xa7dc, "\u{19b}"),
    (0xa7f5, "\u{a7f6}"),
    (0xff21, "\u{ff41}"),
    (0xff22, "\u{ff42}"),
    (0xff23, "\u{ff43}"),
    (0xff24, "\u{ff44}"),
    (0xff25, "\u{ff45}"),
    (0xff26, "\u{ff46}"),
    (0xff27, "\u{ff47}"),
    (0xff28, "\u{ff48}"),
    (0xff29, "\u{ff49}"),
    (0xff2a, "\u{ff4a}"),
    (0xff2b, "\u{ff4b}"),
    (0xff2c, "\u{ff4c}"),
    (0xff2d, "\u{ff4d}"),
    (0xff2e, "\u{ff4e}"),
    (0xff2f, "\u{ff4f}"),
    (0xff30, "\u{ff50}"),
    (0xff31, "\u{ff51}"),
    (0xff32, "\u{ff52}"),
    (0xff33, "\u{ff53}"),
    (0xff34, "\u{ff54}"),
    (0xff35, "\u{ff55}"),
    (0xff36, "\u{ff56}"),
    (0xff37, "\u{ff57}"),
    (0xff38, "\u{ff58}"),
    (0xff39, "\u{ff59}"),
    (0xff3a, "\u{ff5a}"),
    (0x10400, "\u{10428}"),
    (0x10401, "\u{10429}"),
    (0x10402, "\u{1042a}"),
    (0x10403, "\u{1042b}"),
    (0x10404, "\u{1042c}"),
    (0x10405, "\u{1042d}"),
    (0x10406, "\u{1042e}"),
    (0x10407, "\u{1042f}"),
    (0x10408, "\u{10430}"),
    (0x10409, "\u{10431}"),
    (0x1040a, "\u{10432}"),
    (0x1040b, "\u{10433}"),
    (0x1040c, "\u{10434}"),
    (0x1040d, "\u{10435}"),
    (0x1040e, "\u{10436}"),
    (0x1040f, "\u{10437}"),
    (0x10410, "\u{10438}"),
    (0x10411, "\u{10439}"),
    (0x10412, "\u{1043a}"),
    (0x10413, "\u{1043b}"),
    (0x10414, "\u{1043c}"),
    (0x10415, "\u{1043d}"),
    (0x10416, "\u{1043e}"),
    (0x10417, "\u{1043f}"),
    (0x10418, "\u{10440}"),
    (0x10419, "\u{10441}"),
    (0x1041a, "\u{10442}"),
    (0x1041b, "\u{10443}"),
    (0x1041c, "\u{10444}"),
    (0x1041d, "\u{10445}"),
    (0x1041e, "\u{10446}"),
    (0x1041f, "\u{10447}"),
    (0x10420, "\u{10448}"),
    (0x10421, "\u{10449}"),
    (0x10422, "\u{1044a}"),
    (0x10423, "\u{1044b}"),
    (0x10424, "\u{1044c}"),
    (0x10425, "\u{1044d}"),
    (0x10426, "\u{1044e}"),
    (0x10427, "\u{1044f}"),
    (0x104b0, "\u{104d8}"),
    (0x104b1, "\u{104d9}"),
    (0x104b2, "\u{104da}"),
    (0x104b3, "\u{104db}"),
    (0x104b4, "\u{104dc}"),
    (0x104b5, "\u{104dd}"),
    (0x104b6, "\u{104de}"),
    (0x104b7, "\u{104df}"),
    (0x104b8, "\u{104e0}"),
    (0x104b9, "\u{104e1}"),
    (0x104ba, "\u{104e2}"),
    (0x104bb, "\u{104e3}"),
    (0x104bc, "\u{104e4}"),
    (0x104bd, "\u{104e5}"),
    (0x104be, "\u{104e6}"),
    (0x104bf, "\u{104e7}"),
    (0x104c0, "\u{104e8}"),
    (0x104c1, "\u{104e9}"),
    (0x104c2, "\u{104ea}"),
    (0x104c3, "\u{104eb}"),
    (0x104c4, "\u{104ec}"),
    (0x104c5, "\u{104ed}"),
    (0x104c6, "\u{104ee}"),
    (0x104c7, "\u{104ef}"),
    (0x104c8, "\u{104f0}"),
    (0x104c9, "\u{104f1}"),
    (0x104ca, "\u{104f2}"),
    (0x104cb, "\u{104f3}"),
    (0x104cc, "\u{104f4}"),
    (0x104cd, "\u{104f5}"),
    (0x104ce, "\u{104f6}"),
    (0x104cf, "\u{104f7}"),
    (0x104d0, "\u{104f8}"),
    (0x104d1, "\u{104f9}"),
    (0x104d2, "\u{104fa}"),
    (0x104d3, "\u{104fb}"),
    (0x10570, "\u{10597}"),
    (0x10571, "\u{10598}"),
    (0x10572, "\u{10599}"),
    (0x10573, "\u{1059a}"),
    (0x10574, "\u{1059b}"),
    (0x10575, "\u{1059c}"),
    (0x10576, "\u{1059d}"),
    (0x10577, "\u{1059e}"),
    (0x10578, "\u{1059f}"),
    (0x10579, "\u{105a0}"),
    (0x1057a, "\u{105a1}"),
    (0x1057c, "\u{105a3}"),
    (0x1057d, "\u{105a4}"),
    (0x1057e, "\u{105a5}"),
    (0x1057f, "\u{105a6}"),
    (0x10580, "\u{105a7}"),
    (0x10581, "\u{105a8}"),
    (0x10582, "\u{105a9}"),
    (0x10583, "\u{105aa}"),
    (0x10584, "\u{105ab}"),
    (0x10585, "\u{105ac}"),
    (0x10586, "\u{105ad}"),
    (0x10587, "\u{105ae}"),
    (0x10588, "\u{105af}"),
    (0x10589, "\u{105b0}"),
    (0x1058a, "\u{105b1}"),
    (0x1058c, "\u{105b3}"),
    (0x1058d, "\u{105b4}"),
    (0x1058e, "\u{105b5}"),
    (0x1058f, "\u{105b6}"),
    (0x10590, "\u{105b7}"),
    (0x10591, "\u{105b8}"),
    (0x10592, "\u{105b9}"),
    (0x10594, "\u{105bb}"),
    (0x10595, "\u{105bc}"),
    (0x10c80, "\u{10cc0}"),
    (0x10c81, "\u{10cc1}"),
    (0x10c82, "\u{10cc2}"),
    (0x10c83, "\u{10cc3}"),
    (0x10c84, "\u{10cc4}"),
    (0x10c85, "\u{10cc5}"),
    (0x10c86, "\u{10cc6}"),
    (0x10c87, "\u{10cc7}"),
    (0x10c88, "\u{10cc8}"),
    (0x10c89, "\u{10cc9}"),
    (0x10c8a, "\u{10cca}"),
    (0x10c8b, "\u{10ccb}"),
    (0x10c8c, "\u{10ccc}"),
    (0x10c8d, "\u{10ccd}"),
    (0x10c8e, "\u{10cce}"),
    (0x10c8f, "\u{10ccf}"),
    (0x10c90, "\u{10cd0}"),
    (0x10c91, "\u{10cd1}"),
    (0x10c92, "\u{10cd2}"),
    (0x10c93, "\u{10cd3}"),
    (0x10c94, "\u{10cd4}"),
    (0x10c95, "\u{10cd5}"),
    (0x10c96, "\u{10cd6}"),
    (0x10c97, "\u{10cd7}"),
    (0x10c98, "\u{10cd8}"),
    (0x10c99, "\u{10cd9}"),
    (0x10c9a, "\u{10cda}"),
    (0x10c9b, "\u{10cdb}"),
    (0x10c9c, "\u{10cdc}"),
    (0x10c9d, "\u{10cdd}"),
    (0x10c9e, "\u{10cde}"),
    (0x10c9f, "\u{10cdf}"),
    (0x10ca0, "\u{10ce0}"),
    (0x10ca1, "\u{10ce1}"),
    (0x10ca2, "\u{10ce2}"),
    (0x10ca3, "\u{10ce3}"),
    (0x10ca4, "\u{10ce4}"),
    (0x10ca5, "\u{10ce5}"),
    (0x10ca6, "\u{10ce6}"),
    (0x10ca7, "\u{10ce7}"),
    (0x10ca8, "\u{10ce8}"),
    (0x10ca9, "\u{10ce9}"),
    (0x10caa, "\u{10cea}"),
    (0x10cab, "\u{10ceb}"),
    (0x10cac, "\u{10cec}"),
    (0x10cad, "\u{10ced}"),
    (0x10cae, "\u{10cee}"),
    (0x10caf, "\u{10cef}"),
    (0x10cb0, "\u{10cf0}"),
    (0x10cb1, "\u{10cf1}"),
    (0x10cb2, "\u{10cf2}"),
    (0x10d50, "\u{10d70}"),
    (0x10d51, "\u{10d71}"),
    (0x10d52, "\u{10d72}"),
    (0x10d53, "\u{10d73}"),
    (0x10d54, "\u{10d74}"),
    (0x10d55, "\u{10d75}"),
    (0x10d56, "\u{10d76}"),
    (0x10d57, "\u{10d77}"),
    (0x10d58, "\u{10d78}"),
    (0x10d59, "\u{10d79}"),
    (0x10d5a, "\u{10d7a}"),
    (0x10d5b, "\u{10d7b}"),
    (0x10d5c, "\u{10d7c}"),
    (0x10d5d, "\u{10d7d}"),
    (0x10d5e, "\u{10d7e}"),
    (0x10d5f, "\u{10d7f}"),
    (0x10d60, "\u{10d80}"),
    (0x10d61, "\u{10d81}"),
    (0x10d62, "\u{10d82}"),
    (0x10d63, "\u{10d83}"),
    (0x10d64, "\u{10d84}"),
    (0x10d65, "\u{10d85}"),
    (0x118a0, "\u{118c0}"),
    (0x118a1, "\u{118c1}"),
    (0x118a2, "\u{118c2}"),
    (0x118a3, "\u{118c3}"),
    (0x118a4, "\u{118c4}"),
    (0x118a5, "\u{118c5}"),
    (0x118a6, "\u{118c6}"),
    (0x118a7, "\u{118c7}"),
    (0x118a8, "\u{118c8}"),
    (0x118a9, "\u{118c9}"),
    (0x118aa, "\u{118ca}"),
    (0x118ab, "\u{118cb}"),
    (0x118ac, "\u{118cc}"),
    (0x118ad, "\u{118cd}"),
    (0x118ae, "\u{118ce}"),
    (0x118af, "\u{118cf}"),
    (0x118b0, "\u{118d0}"),
    (0x118b1, "\u{118d1}"),
    (0x118b2, "\u{118d2}"),
    (0x118b3, "\u{118d3}"),
    (0x118b4, "\u{118d4}"),
    (0x118b5, "\u{118d5}"),
    (0x118b6, "\u{118d6}"),
    (0x118b7, "\u{118d7}"),
    (0x118b8, "\u{118d8}"),
    (0x118b9, "\u{118d9}"),
    (0x118ba, "\u{118da}"),
    (0x118bb, "\u{118db}"),
    (0x118bc, "\u{118dc}"),
    (0x118bd, "\u{118dd}"),
    (0x118be, "\u{118de}"),
    (0x118bf, "\u{118df}"),
    (0x16e40, "\u{16e60}"),
    (0x16e41, "\u{16e61}"),
    (0x16e42, "\u{16e62}"),
    (0x16e43, "\u{16e63}"),
    (0x16e44, "\u{16e64}"),
    (0x16e45, "\u{16e65}"),
    (0x16e46, "\u{16e66}"),
    (0x16e47, "\u{16e67}"),
    (0x16e48, "\u{16e68}"),
    (0x16e49, "\u{16e69}"),
    (0x16e4a, "\u{16e6a}"),
    (0x16e4b, "\u{16e6b}"),
    (0x16e4c, "\u{16e6c}"),
    (0x16e4d, "\u{16e6d}"),
    (0x16e4e, "\u{16e6e}"),
    (0x16e4f, "\u{16e6f}"),
    (0x16e50, "\u{16e70}"),
    (0x16e51, "\u{16e71}"),
    (0x16e52, "\u{16e72}"),
    (0x16e53, "\u{16e73}"),
    (0x16e54, "\u{16e74}"),
    (0x16e55, "\u{16e75}"),
    (0x16e56, "\u{16e76}"),
    (0x16e57, "\u{16e77}"),
    (0x16e58, "\u{16e78}"),
    (0x16e59, "\u{16e79}"),
    (0x16e5a, "\u{16e7a}"),
    (0x16e5b, "\u{16e7b}"),
    (0x16e5c, "\u{16e7c}"),
    (0x16e5d, "\u{16e7d}"),
    (0x16e5e, "\u{16e7e}"),
    (0x16e5f, "\u{16e7f}"),
    (0x1e900, "\u{1e922}"),
    (0x1e901, "\u{1e923}"),
    (0x1e902, "\u{1e924}"),
    (0x1e903, "\u{1e925}"),
    (0x1e904, "\u{1e926}"),
    (0x1e905, "\u{1e927}"),
    (0x1e906, "\u{1e928}"),
    (0x1e907, "\u{1e929}"),
    (0x1e908, "\u{1e92a}"),
    (0x1e909, "\u{1e92b}"),
    (0x1e90a, "\u{1e92c}"),
    (0x1e90b, "\u{1e92d}"),
    (0x1e90c, "\u{1e92e}"),
    (0x1e90d, "\u{1e92f}"),
    (0x1e90e, "\u{1e930}"),
    (0x1e90f, "\u{1e931}"),
    (0x1e910, "\u{1e932}"),
    (0x1e911, "\u{1e933}"),
    (0x1e912, "\u{1e934}"),
    (0x1e913, "\u{1e935}"),
    (0x1e914, "\u{1e936}"),
    (0x1e915, "\u{1e937}"),
    (0x1e916, "\u{1e938}"),
    (0x1e917, "\u{1e939}"),
    (0x1e918, "\u{1e93a}"),
    (0x1e919, "\u{1e93b}"),
    (0x1e91a, "\u{1e93c}"),
    (0x1e91b, "\u{1e93d}"),
    (0x1e91c, "\u{1e93e}"),
    (0x1e91d, "\u{1e93f}"),
    (0x1e91e, "\u{1e940}"),
    (0x1e91f, "\u{1e941}"),
    (0x1e920, "\u{1e942}"),
    (0x1e921, "\u{1e943}"),
];
/// Characters `str.lower()` treats as Cased and not Case_Ignorable around a final 'Σ'.
const PY_CASED: &[(u32, u32)] = &[
    (0x41, 0x5a),
    (0x61, 0x7a),
    (0xaa, 0xaa),
    (0xb5, 0xb5),
    (0xba, 0xba),
    (0xc0, 0xd6),
    (0xd8, 0xf6),
    (0xf8, 0x1ba),
    (0x1bc, 0x1bf),
    (0x1c4, 0x293),
    (0x295, 0x2af),
    (0x370, 0x373),
    (0x376, 0x377),
    (0x37b, 0x37d),
    (0x37f, 0x37f),
    (0x386, 0x386),
    (0x388, 0x38a),
    (0x38c, 0x38c),
    (0x38e, 0x3a1),
    (0x3a3, 0x3f5),
    (0x3f7, 0x481),
    (0x48a, 0x52f),
    (0x531, 0x556),
    (0x560, 0x588),
    (0x10a0, 0x10c5),
    (0x10c7, 0x10c7),
    (0x10cd, 0x10cd),
    (0x10d0, 0x10fa),
    (0x10fd, 0x10ff),
    (0x13a0, 0x13f5),
    (0x13f8, 0x13fd),
    (0x1c80, 0x1c8a),
    (0x1c90, 0x1cba),
    (0x1cbd, 0x1cbf),
    (0x1d00, 0x1d2b),
    (0x1d6b, 0x1d77),
    (0x1d79, 0x1d9a),
    (0x1e00, 0x1f15),
    (0x1f18, 0x1f1d),
    (0x1f20, 0x1f45),
    (0x1f48, 0x1f4d),
    (0x1f50, 0x1f57),
    (0x1f59, 0x1f59),
    (0x1f5b, 0x1f5b),
    (0x1f5d, 0x1f5d),
    (0x1f5f, 0x1f7d),
    (0x1f80, 0x1fb4),
    (0x1fb6, 0x1fbc),
    (0x1fbe, 0x1fbe),
    (0x1fc2, 0x1fc4),
    (0x1fc6, 0x1fcc),
    (0x1fd0, 0x1fd3),
    (0x1fd6, 0x1fdb),
    (0x1fe0, 0x1fec),
    (0x1ff2, 0x1ff4),
    (0x1ff6, 0x1ffc),
    (0x2102, 0x2102),
    (0x2107, 0x2107),
    (0x210a, 0x2113),
    (0x2115, 0x2115),
    (0x2119, 0x211d),
    (0x2124, 0x2124),
    (0x2126, 0x2126),
    (0x2128, 0x2128),
    (0x212a, 0x212d),
    (0x212f, 0x2134),
    (0x2139, 0x2139),
    (0x213c, 0x213f),
    (0x2145, 0x2149),
    (0x214e, 0x214e),
    (0x2160, 0x217f),
    (0x2183, 0x2184),
    (0x24b6, 0x24e9),
    (0x2c00, 0x2c7b),
    (0x2c7e, 0x2ce4),
    (0x2ceb, 0x2cee),
    (0x2cf2, 0x2cf3),
    (0x2d00, 0x2d25),
    (0x2d27, 0x2d27),
    (0x2d2d, 0x2d2d),
    (0xa640, 0xa66d),
    (0xa680, 0xa69b),
    (0xa722, 0xa76f),
    (0xa771, 0xa787),
    (0xa78b, 0xa78e),
    (0xa790, 0xa7cd),
    (0xa7d0, 0xa7d1),
    (0xa7d3, 0xa7d3),
    (0xa7d5, 0xa7dc),
    (0xa7f5, 0xa7f6),
    (0xa7fa, 0xa7fa),
    (0xab30, 0xab5a),
    (0xab60, 0xab68),
    (0xab70, 0xabbf),
    (0xfb00, 0xfb06),
    (0xfb13, 0xfb17),
    (0xff21, 0xff3a),
    (0xff41, 0xff5a),
    (0x10400, 0x1044f),
    (0x104b0, 0x104d3),
    (0x104d8, 0x104fb),
    (0x10570, 0x1057a),
    (0x1057c, 0x1058a),
    (0x1058c, 0x10592),
    (0x10594, 0x10595),
    (0x10597, 0x105a1),
    (0x105a3, 0x105b1),
    (0x105b3, 0x105b9),
    (0x105bb, 0x105bc),
    (0x10c80, 0x10cb2),
    (0x10cc0, 0x10cf2),
    (0x10d50, 0x10d65),
    (0x10d70, 0x10d85),
    (0x118a0, 0x118df),
    (0x16e40, 0x16e7f),
    (0x1d400, 0x1d454),
    (0x1d456, 0x1d49c),
    (0x1d49e, 0x1d49f),
    (0x1d4a2, 0x1d4a2),
    (0x1d4a5, 0x1d4a6),
    (0x1d4a9, 0x1d4ac),
    (0x1d4ae, 0x1d4b9),
    (0x1d4bb, 0x1d4bb),
    (0x1d4bd, 0x1d4c3),
    (0x1d4c5, 0x1d505),
    (0x1d507, 0x1d50a),
    (0x1d50d, 0x1d514),
    (0x1d516, 0x1d51c),
    (0x1d51e, 0x1d539),
    (0x1d53b, 0x1d53e),
    (0x1d540, 0x1d544),
    (0x1d546, 0x1d546),
    (0x1d54a, 0x1d550),
    (0x1d552, 0x1d6a5),
    (0x1d6a8, 0x1d6c0),
    (0x1d6c2, 0x1d6da),
    (0x1d6dc, 0x1d6fa),
    (0x1d6fc, 0x1d714),
    (0x1d716, 0x1d734),
    (0x1d736, 0x1d74e),
    (0x1d750, 0x1d76e),
    (0x1d770, 0x1d788),
    (0x1d78a, 0x1d7a8),
    (0x1d7aa, 0x1d7c2),
    (0x1d7c4, 0x1d7cb),
    (0x1df00, 0x1df09),
    (0x1df0b, 0x1df1e),
    (0x1df25, 0x1df2a),
    (0x1e900, 0x1e943),
    (0x1f130, 0x1f149),
    (0x1f150, 0x1f169),
    (0x1f170, 0x1f189),
];
/// Characters `str.lower()` treats as Case_Ignorable around a final 'Σ'.
const PY_CASE_IGNORABLE: &[(u32, u32)] = &[
    (0x27, 0x27),
    (0x2e, 0x2e),
    (0x3a, 0x3a),
    (0x5e, 0x5e),
    (0x60, 0x60),
    (0xa8, 0xa8),
    (0xad, 0xad),
    (0xaf, 0xaf),
    (0xb4, 0xb4),
    (0xb7, 0xb8),
    (0x2b0, 0x36f),
    (0x374, 0x375),
    (0x37a, 0x37a),
    (0x384, 0x385),
    (0x387, 0x387),
    (0x483, 0x489),
    (0x559, 0x559),
    (0x55f, 0x55f),
    (0x591, 0x5bd),
    (0x5bf, 0x5bf),
    (0x5c1, 0x5c2),
    (0x5c4, 0x5c5),
    (0x5c7, 0x5c7),
    (0x5f4, 0x5f4),
    (0x600, 0x605),
    (0x610, 0x61a),
    (0x61c, 0x61c),
    (0x640, 0x640),
    (0x64b, 0x65f),
    (0x670, 0x670),
    (0x6d6, 0x6dd),
    (0x6df, 0x6e8),
    (0x6ea, 0x6ed),
    (0x70f, 0x70f),
    (0x711, 0x711),
    (0x730, 0x74a),
    (0x7a6, 0x7b0),
    (0x7eb, 0x7f5),
    (0x7fa, 0x7fa),
    (0x7fd, 0x7fd),
    (0x816, 0x82d),
    (0x859, 0x85b),
    (0x888, 0x888),
    (0x890, 0x891),
    (0x897, 0x89f),
    (0x8c9, 0x902),
    (0x93a, 0x93a),
    (0x93c, 0x93c),
    (0x941, 0x948),
    (0x94d, 0x94d),
    (0x951, 0x957),
    (0x962, 0x963),
    (0x971, 0x971),
    (0x981, 0x981),
    (0x9bc, 0x9bc),
    (0x9c1, 0x9c4),
    (0x9cd, 0x9cd),
    (0x9e2, 0x9e3),
    (0x9fe, 0x9fe),
    (0xa01, 0xa02),
    (0xa3c, 0xa3c),
    (0xa41, 0xa42),
    (0xa47, 0xa48),
    (0xa4b, 0xa4d),
    (0xa51, 0xa51),
    (0xa70, 0xa71),
    (0xa75, 0xa75),
    (0xa81, 0xa82),
    (0xabc, 0xabc),
    (0xac1, 0xac5),
    (0xac7, 0xac8),
    (0xacd, 0xacd),
    (0xae2, 0xae3),
    (0xafa, 0xaff),
    (0xb01, 0xb01),
    (0xb3c, 0xb3c),
    (0xb3f, 0xb3f),
    (0xb41, 0xb44),
    (0xb4d, 0xb4d),
    (0xb55, 0xb56),
    (0xb62, 0xb63),
    (0xb82, 0xb82),
    (0xbc0, 0xbc0),
    (0xbcd, 0xbcd),
    (0xc00, 0xc00),
    (0xc04, 0xc04),
    (0xc3c, 0xc3c),
    (0xc3e, 0xc40),
    (0xc46, 0xc48),
    (0xc4a, 0xc4d),
    (0xc55, 0xc56),
    (0xc62, 0xc63),
    (0xc81, 0xc81),
    (0xcbc, 0xcbc),
    (0xcbf, 0xcbf),
    (0xcc6, 0xcc6),
    (0xccc, 0xccd),
    (0xce2, 0xce3),
    (0xd00, 0xd01),
    (0xd3b, 0xd3c),
    (0xd41, 0xd44),
    (0xd4d, 0xd4d),
    (0xd62, 0xd63),
    (0xd81, 0xd81),
    (0xdca, 0xdca),
    (0xdd2, 0xdd4),
    (0xdd6, 0xdd6),
    (0xe31, 0xe31),
    (0xe34, 0xe3a),
    (0xe46, 0xe4e),
    (0xeb1, 0xeb1),
    (0xeb4, 0xebc),
    (0xec6, 0xec6),
    (0xec8, 0xece),
    (0xf18, 0xf19),
    (0xf35, 0xf35),
    (0xf37, 0xf37),
    (0xf39, 0xf39),
    (0xf71, 0xf7e),
    (0xf80, 0xf84),
    (0xf86, 0xf87),
    (0xf8d, 0xf97),
    (0xf99, 0xfbc),
    (0xfc6, 0xfc6),
    (0x102d, 0x1030),
    (0x1032, 0x1037),
    (0x1039, 0x103a),
    (0x103d, 0x103e),
    (0x1058, 0x1059),
    (0x105e, 0x1060),
    (0x1071, 0x1074),
    (0x1082, 0x1082),
    (0x1085, 0x1086),
    (0x108d, 0x108d),
    (0x109d, 0x109d),
    (0x10fc, 0x10fc),
    (0x135d, 0x135f),
    (0x1712, 0x1714),
    (0x1732, 0x1733),
    (0x1752, 0x1753),
    (0x1772, 0x1773),
    (0x17b4, 0x17b5),
    (0x17b7, 0x17bd),
    (0x17c6, 0x17c6),
    (0x17c9, 0x17d3),
    (0x17d7, 0x17d7),
    (0x17dd, 0x17dd),
    (0x180b, 0x180f),
    (0x1843, 0x1843),
    (0x1885, 0x1886),
    (0x18a9, 0x18a9),
    (0x1920, 0x1922),
    (0x1927, 0x1928),
    (0x1932, 0x1932),
    (0x1939, 0x193b),
    (0x1a17, 0x1a18),
    (0x1a1b, 0x1a1b),
    (0x1a56, 0x1a56),
    (0x1a58, 0x1a5e),
    (0x1a60, 0x1a60),
    (0x1a62, 0x1a62),
    (0x1a65, 0x1a6c),
    (0x1a73, 0x1a7c),
    (0x1a7f, 0x1a7f),
    (0x1aa7, 0x1aa7),
    (0x1ab0, 0x1ace),
    (0x1b00, 0x1b03),
    (0x1b34, 0x1b34),
    (0x1b36, 0x1b3a),
    (0x1b3c, 0x1b3c),
    (0x1b42, 0x1b42),
    (0x1b6b, 0x1b73),
    (0x1b80, 0x1b81),
    (0x1ba2, 0x1ba5),
    (0x1ba8, 0x1ba9),
    (0x1bab, 0x1bad),
    (0x1be6, 0x1be6),
    (0x1be8, 0x1be9),
    (0x1bed, 0x1bed),
    (0x1bef, 0x1bf1),
    (0x1c2c, 0x1c33),
    (0x1c36, 0x1c37),
    (0x1c78, 0x1c7d),
    (0x1cd0, 0x1cd2),
    (0x1cd4, 0x1ce0),
    (0x1ce2, 0x1ce8),
    (0x1ced, 0x1ced),
    (0x1cf4, 0x1cf4),
    (0x1cf8, 0x1cf9),
    (0x1d2c, 0x1d6a),
    (0x1d78, 0x1d78),
    (0x1d9b, 0x1dff),
    (0x1fbd, 0x1fbd),
    (0x1fbf, 0x1fc1),
    (0x1fcd, 0x1fcf),
    (0x1fdd, 0x1fdf),
    (0x1fed, 0x1fef),
    (0x1ffd, 0x1ffe),
    (0x200b, 0x200f),
    (0x2018, 0x2019),
    (0x2024, 0x2024),
    (0x2027, 0x2027),
    (0x202a, 0x202e),
    (0x2060, 0x2064),
    (0x2066, 0x206f),
    (0x2071, 0x2071),
    (0x207f, 0x207f),
    (0x2090, 0x209c),
    (0x20d0, 0x20f0),
    (0x2c7c, 0x2c7d),
    (0x2cef, 0x2cf1),
    (0x2d6f, 0x2d6f),
    (0x2d7f, 0x2d7f),
    (0x2de0, 0x2dff),
    (0x2e2f, 0x2e2f),
    (0x3005, 0x3005),
    (0x302a, 0x302d),
    (0x3031, 0x3035),
    (0x303b, 0x303b),
    (0x3099, 0x309e),
    (0x30fc, 0x30fe),
    (0xa015, 0xa015),
    (0xa4f8, 0xa4fd),
    (0xa60c, 0xa60c),
    (0xa66f, 0xa672),
    (0xa674, 0xa67d),
    (0xa67f, 0xa67f),
    (0xa69c, 0xa69f),
    (0xa6f0, 0xa6f1),
    (0xa700, 0xa721),
    (0xa770, 0xa770),
    (0xa788, 0xa78a),
    (0xa7f2, 0xa7f4),
    (0xa7f8, 0xa7f9),
    (0xa802, 0xa802),
    (0xa806, 0xa806),
    (0xa80b, 0xa80b),
    (0xa825, 0xa826),
    (0xa82c, 0xa82c),
    (0xa8c4, 0xa8c5),
    (0xa8e0, 0xa8f1),
    (0xa8ff, 0xa8ff),
    (0xa926, 0xa92d),
    (0xa947, 0xa951),
    (0xa980, 0xa982),
    (0xa9b3, 0xa9b3),
    (0xa9b6, 0xa9b9),
    (0xa9bc, 0xa9bd),
    (0xa9cf, 0xa9cf),
    (0xa9e5, 0xa9e6),
    (0xaa29, 0xaa2e),
    (0xaa31, 0xaa32),
    (0xaa35, 0xaa36),
    (0xaa43, 0xaa43),
    (0xaa4c, 0xaa4c),
    (0xaa70, 0xaa70),
    (0xaa7c, 0xaa7c),
    (0xaab0, 0xaab0),
    (0xaab2, 0xaab4),
    (0xaab7, 0xaab8),
    (0xaabe, 0xaabf),
    (0xaac1, 0xaac1),
    (0xaadd, 0xaadd),
    (0xaaec, 0xaaed),
    (0xaaf3, 0xaaf4),
    (0xaaf6, 0xaaf6),
    (0xab5b, 0xab5f),
    (0xab69, 0xab6b),
    (0xabe5, 0xabe5),
    (0xabe8, 0xabe8),
    (0xabed, 0xabed),
    (0xfb1e, 0xfb1e),
    (0xfbb2, 0xfbc2),
    (0xfe00, 0xfe0f),
    (0xfe13, 0xfe13),
    (0xfe20, 0xfe2f),
    (0xfe52, 0xfe52),
    (0xfe55, 0xfe55),
    (0xfeff, 0xfeff),
    (0xff07, 0xff07),
    (0xff0e, 0xff0e),
    (0xff1a, 0xff1a),
    (0xff3e, 0xff3e),
    (0xff40, 0xff40),
    (0xff70, 0xff70),
    (0xff9e, 0xff9f),
    (0xffe3, 0xffe3),
    (0xfff9, 0xfffb),
    (0x101fd, 0x101fd),
    (0x102e0, 0x102e0),
    (0x10376, 0x1037a),
    (0x10780, 0x10785),
    (0x10787, 0x107b0),
    (0x107b2, 0x107ba),
    (0x10a01, 0x10a03),
    (0x10a05, 0x10a06),
    (0x10a0c, 0x10a0f),
    (0x10a38, 0x10a3a),
    (0x10a3f, 0x10a3f),
    (0x10ae5, 0x10ae6),
    (0x10d24, 0x10d27),
    (0x10d4e, 0x10d4e),
    (0x10d69, 0x10d6d),
    (0x10d6f, 0x10d6f),
    (0x10eab, 0x10eac),
    (0x10efc, 0x10eff),
    (0x10f46, 0x10f50),
    (0x10f82, 0x10f85),
    (0x11001, 0x11001),
    (0x11038, 0x11046),
    (0x11070, 0x11070),
    (0x11073, 0x11074),
    (0x1107f, 0x11081),
    (0x110b3, 0x110b6),
    (0x110b9, 0x110ba),
    (0x110bd, 0x110bd),
    (0x110c2, 0x110c2),
    (0x110cd, 0x110cd),
    (0x11100, 0x11102),
    (0x11127, 0x1112b),
    (0x1112d, 0x11134),
    (0x11173, 0x11173),
    (0x11180, 0x11181),
    (0x111b6, 0x111be),
    (0x111c9, 0x111cc),
    (0x111cf, 0x111cf),
    (0x1122f, 0x11231),
    (0x11234, 0x11234),
    (0x11236, 0x11237),
    (0x1123e, 0x1123e),
    (0x11241, 0x11241),
    (0x112df, 0x112df),
    (0x112e3, 0x112ea),
    (0x11300, 0x11301),
    (0x1133b, 0x1133c),
    (0x11340, 0x11340),
    (0x11366, 0x1136c),
    (0x11370, 0x11374),
    (0x113bb, 0x113c0),
    (0x113ce, 0x113ce),
    (0x113d0, 0x113d0),
    (0x113d2, 0x113d2),
    (0x113e1, 0x113e2),
    (0x11438, 0x1143f),
    (0x11442, 0x11444),
    (0x11446, 0x11446),
    (0x1145e, 0x1145e),
    (0x114b3, 0x114b8),
    (0x114ba, 0x114ba),
    (0x114bf, 0x114c0),
    (0x114c2, 0x114c3),
    (0x115b2, 0x115b5),
    (0x115bc, 0x115bd),
    (0x115bf, 0x115c0),
    (0x115dc, 0x115dd),
    (0x11633, 0x1163a),
    (0x1163d, 0x1163d),
    (0x1163f, 0x11640),
    (0x116ab, 0x116ab),
    (0x116ad, 0x116ad),
    (0x116b0, 0x116b5),
    (0x116b7, 0x116b7),
    (0x1171d, 0x1171d),
    (0x1171f, 0x1171f),
    (0x11722, 0x11725),
    (0x11727, 0x1172b),
    (0x1182f, 0x11837),
    (0x11839, 0x1183a),
    (0x1193b, 0x1193c),
    (0x1193e, 0x1193e),
    (0x11943, 0x11943),
    (0x119d4, 0x119d7),
    (0x119da, 0x119db),
    (0x119e0, 0x119e0),
    (0x11a01, 0x11a0a),
    (0x11a33, 0x11a38),
    (0x11a3b, 0x11a3e),
    (0x11a47, 0x11a47),
    (0x11a51, 0x11a56),
    (0x11a59, 0x11a5b),
    (0x11a8a, 0x11a96),
    (0x11a98, 0x11a99),
    (0x11c30, 0x11c36),
    (0x11c38, 0x11c3d),
    (0x11c3f, 0x11c3f),
    (0x11c92, 0x11ca7),
    (0x11caa, 0x11cb0),
    (0x11cb2, 0x11cb3),
    (0x11cb5, 0x11cb6),
    (0x11d31, 0x11d36),
    (0x11d3a, 0x11d3a),
    (0x11d3c, 0x11d3d),
    (0x11d3f, 0x11d45),
    (0x11d47, 0x11d47),
    (0x11d90, 0x11d91),
    (0x11d95, 0x11d95),
    (0x11d97, 0x11d97),
    (0x11ef3, 0x11ef4),
    (0x11f00, 0x11f01),
    (0x11f36, 0x11f3a),
    (0x11f40, 0x11f40),
    (0x11f42, 0x11f42),
    (0x11f5a, 0x11f5a),
    (0x13430, 0x13440),
    (0x13447, 0x13455),
    (0x1611e, 0x16129),
    (0x1612d, 0x1612f),
    (0x16af0, 0x16af4),
    (0x16b30, 0x16b36),
    (0x16b40, 0x16b43),
    (0x16d40, 0x16d42),
    (0x16d6b, 0x16d6c),
    (0x16f4f, 0x16f4f),
    (0x16f8f, 0x16f9f),
    (0x16fe0, 0x16fe1),
    (0x16fe3, 0x16fe4),
    (0x1aff0, 0x1aff3),
    (0x1aff5, 0x1affb),
    (0x1affd, 0x1affe),
    (0x1bc9d, 0x1bc9e),
    (0x1bca0, 0x1bca3),
    (0x1cf00, 0x1cf2d),
    (0x1cf30, 0x1cf46),
    (0x1d167, 0x1d169),
    (0x1d173, 0x1d182),
    (0x1d185, 0x1d18b),
    (0x1d1aa, 0x1d1ad),
    (0x1d242, 0x1d244),
    (0x1da00, 0x1da36),
    (0x1da3b, 0x1da6c),
    (0x1da75, 0x1da75),
    (0x1da84, 0x1da84),
    (0x1da9b, 0x1da9f),
    (0x1daa1, 0x1daaf),
    (0x1e000, 0x1e006),
    (0x1e008, 0x1e018),
    (0x1e01b, 0x1e021),
    (0x1e023, 0x1e024),
    (0x1e026, 0x1e02a),
    (0x1e030, 0x1e06d),
    (0x1e08f, 0x1e08f),
    (0x1e130, 0x1e13d),
    (0x1e2ae, 0x1e2ae),
    (0x1e2ec, 0x1e2ef),
    (0x1e4eb, 0x1e4ef),
    (0x1e5ee, 0x1e5ef),
    (0x1e8d0, 0x1e8d6),
    (0x1e944, 0x1e94b),
    (0x1f3fb, 0x1f3ff),
    (0xe0001, 0xe0001),
    (0xe0020, 0xe007f),
    (0xe0100, 0xe01ef),
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn test_a_compiled_translation_is_used() {
        if crate::util::which("msgfmt").is_none() {
            eprintln!("skipped: gettext is not installed");
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let po = d.path().join("de.po");
        std::fs::write(
            &po,
            "msgid \"\"\nmsgstr \"Content-Type: text/plain; charset=UTF-8\\n\"\n\n\
             msgid \"History\"\nmsgstr \"Verlauf\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(d.path().join("de/LC_MESSAGES")).unwrap();
        let st = Command::new("msgfmt")
            .arg("-o")
            .arg(d.path().join("de/LC_MESSAGES/almanac.mo"))
            .arg(&po)
            .status()
            .unwrap();
        assert!(st.success());
        let t = translation(DOMAIN, d.path().to_str(), Some(vec!["de".into()])).unwrap();
        assert_eq!(t.gettext("History").unwrap(), "Verlauf");
    }

    #[test]
    fn stdlib_pieces_match_python() {
        // str.strip, str.isalnum, os.path.normpath, codecs, unbounded ints
        assert_eq!(py_strip("\u{1c} a \u{1f}"), "a");
        assert!(!py_isalnum('\u{93e}') && py_isalnum('a') && py_isalnum('²'));
        assert_eq!(normpath(b"/a/./b/../c//d"), b"/a/c/d");
        assert_eq!(normpath(b"//a/.."), b"//");
        assert_eq!(decode(&[0xbf, 0xb3], "ISO-8859-2").unwrap(), "żł");
        assert!(decode(&[0x81], "cp1252").is_err());
        let f = c2py("n*n*n%7").unwrap();
        assert_eq!(f.call(&PluralN::from(1u64 << 62), &mut false).unwrap(), BigInt::from(1));
        assert!(c2py("n n $").unwrap_err().starts_with("unexpected token"));
    }

    #[test]
    fn test_untranslated_falls_back_to_english() {
        assert_eq!(tr("No string anyone has translated"), "No string anyone has translated");
    }

    #[test]
    fn pystr_keeps_surrogates_and_its_own_plane() {
        let s = "a\u{10F7FF}\u{10F800}\u{10FFFF}é";
        assert_eq!(py_lossy(&py_from_str(s)), s);
        let mut p = String::new();
        py_push(&mut p, 0xD800);
        py_push(&mut p, 0x10F800);
        assert_eq!(py_codepoints(&p), [0xD800, 0x10F800]);
        assert_eq!(py_ascii(&p), "'\\ud800\\U0010f800'");
        assert_eq!(py_lossy(&p), "\u{fffd}\u{10F800}");
        // os.environ's surrogateescape bytes go back to the bytes
        let mut e = String::new();
        py_push(&mut e, 0xDC80);
        assert_eq!(fsencode(&e).unwrap(), [0x80]);
        assert_eq!(fsencode(&py_from_str("\u{10FD80}")).unwrap(), "\u{10FD80}".as_bytes());
    }

    #[test]
    fn str_lower_is_python_s() {
        // '.' is case-ignorable: only the last sigma ends a word
        assert_eq!(py_lossy(&py_lower(&py_from_str("ΣΑΣ.ΣΑΣ"))), "σασ.σας");
        assert_eq!(py_lossy(&py_lower(&py_from_str("İ"))), "i\u{307}");
        // no lower case in Unicode 16.0
        assert_eq!(py_lossy(&py_lower(&py_from_str("\u{a7d2}"))), "\u{a7d2}");
        assert_eq!(normalize("de.a\u{a7d2}a"), "de_DE.a_a");
    }

    #[test]
    fn python_codecs() {
        let d = |b: &[u8], cs: &str| decode(b, cs).map(|s| py_ascii(&s));
        // CJKCodecs' strict decoders, not the Encoding Standard's
        assert!(d(b"\x81\x40", "GB2312").is_err());
        assert_eq!(d(b"\xa1\xa4", "GB2312").unwrap(), "'\\u30fb'");
        assert!(d(b"\x80", "GBK").is_err());
        assert!(d(b"\x87\x40", "Shift_JIS").is_err());
        assert_eq!(d(b"\xa0", "cp932").unwrap(), "'\\uf8f0'");
        assert_eq!(d(b"\x81\x60", "cp932").unwrap(), "'\\uff5e'");
        assert!(d(b"\x81\x41", "EUC-KR").is_err());
        assert_eq!(d(b"\xa4\xd4\xa4\xa1\xa4\xbf\xa4\xd4", "EUC-KR").unwrap(), "'\\uac00'");
        assert!(d(b"\xa3\xe1", "Big5").is_err());
        assert_eq!(d(b"\xa1\xc1", "EUC-JP").unwrap(), "'\\u301c'");
        assert_eq!(d(b"\x84\x31\xa4\x39", "GB18030").unwrap(), "'\\uffff'");
        assert_eq!(d(b"\x90\x30\x81\x30", "GB18030").unwrap(), "'\\U00010000'");
        // the codecs the port did not carry
        assert_eq!(d(b"+AGE-", "utf-7").unwrap(), "'a'");
        assert_eq!(d(b"+2AA-", "utf-7").unwrap(), "'\\ud800'");
        assert_eq!(d(b"~{<:~}", "hz").unwrap(), "'\\u5df1'");
        assert_eq!(d(b"\x88\x61", "johab").unwrap(), "'\\uac00'");
        assert_eq!(d(b"\x1b$)C\x0e\x30\x21\x0f", "iso2022_kr").unwrap(), "'\\uac00'");
        assert_eq!(d(b"\x1b$B\x24\x22\x1b(B", "iso2022_jp_2").unwrap(), "'\\u3042'");
        assert_eq!(d(b"gre-6ka8i", "punycode").unwrap(), "'gr\\xf6\\xdfe'");
        assert_eq!(d(b"xn--mnchen-3ya.de", "idna").unwrap(), "'m\\xfcnchen.de'");
        assert!(d(b"xn--gre-6ka8i", "idna").is_err());
        assert_eq!(d(b"\\N{LATIN SMALL LETTER A}\\N{hangul syllable gag}\\N{NULL}", "unicode_escape").unwrap(), "'a\\uac01\\x00'");
        assert_eq!(d(b"\\N{cjk unified ideograph-4e00}\\N{TANGUT IDEOGRAPH-17000}", "unicode_escape").unwrap(), "'\\u4e00\\U00017000'");
        assert!(d(b"\\N{NOT A NAME}", "unicode_escape").is_err());
        assert_eq!(d(b"\\ud800", "unicode_escape").unwrap(), "'\\ud800'");
    }

    #[test]
    fn ngettext_takes_floats() {
        let t = Translation::default();
        assert_eq!(t.ngettext("one", "many", 1.0).unwrap(), "one");
        assert_eq!(t.ngettext("one", "many", 1.5).unwrap(), "many");
        let f = c2py("n%10==1 && n%100!=11 ? 0 : 1").unwrap();
        assert_eq!(f.call(&PluralN::Float(21.0), &mut false).unwrap(), BigInt::from(0));
        assert_eq!(f.call(&PluralN::Float(21.5), &mut false).unwrap(), BigInt::from(1));
        assert!(f.call(&PluralN::Float(f64::NAN), &mut false).is_err());
        assert!(f.call(&PluralN::Other("str".into()), &mut false).is_err());
        assert_eq!(c2py("n").unwrap().call(&PluralN::Float(2.9), &mut false).unwrap(), BigInt::from(2));
    }

    #[test]
    fn lc_time_has_every_name() {
        let t = lc_time();
        assert_eq!((t.abday.len(), t.day.len(), t.abmon.len(), t.mon.len()), (7, 7, 12, 12));
        assert_eq!(LcTime::c().mon[0], "January");
    }

    #[test]
    fn a_pathological_plural_parses_and_evaluates_on_a_small_stack() {
        // 999 negations: one parse and eval frame each.
        let plural = "!".repeat(999) + "n";
        std::thread::Builder::new()
            .stack_size(128 << 10)
            .spawn(move || {
                let f = c2py(&plural).unwrap();
                assert_eq!(f.call(&PluralN::Int(BigInt::from(0)), &mut false).unwrap(), BigInt::from(1));
                assert_eq!(f.call(&PluralN::Int(BigInt::from(2)), &mut false).unwrap(), BigInt::from(0));
            })
            .unwrap()
            .join()
            .unwrap();
        // gh-157451 (3.14.8): `!` binds tighter than `==` and `+`, as in C.
        let f = c2py("!n == 1").unwrap();
        let at = |n: i64| f.call(&PluralN::Int(BigInt::from(n)), &mut false).unwrap();
        assert_eq!((at(0), at(1), at(2)), (BigInt::from(1), BigInt::from(0), BigInt::from(0)));
        let g = c2py("n+!n").unwrap();
        assert_eq!(g.call(&PluralN::Int(BigInt::from(0)), &mut false).unwrap(), BigInt::from(1));
    }
}
