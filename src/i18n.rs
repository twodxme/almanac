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
//! Port of `i18n.py`. Python's `_` is `tr` here (ARCHITECTURE.md). The
//! catalog is loaded on the first `tr`/`ngettext`.
//!
//! The Python module leans on the standard library's `gettext` module, which
//! does not use the C library at all: it picks the language from the
//! environment (LANGUAGE, LC_ALL, LC_MESSAGES, LANG), reads the .mo file
//! itself and evaluates its Plural-Forms expression itself. The parts of it
//! this module relies on (`gettext.find`, `_expand_lang`, `locale.normalize`,
//! `GNUTranslations`, `c2py`) are ported below so the Rust app picks the same
//! catalog, and the same plural form, as the Python app would. Python's
//! integers are unbounded, so plural forms are evaluated on `BigInt`s.
//!
//! Catalogs are read as UTF-8 (msgfmt's and po/update.sh's default). Python
//! would decode one in any charset its codecs know; a catalog in another
//! charset, or one that cannot be read at all, is skipped here, and its
//! strings stay English.

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
static TRANSLATION: LazyLock<Translation> = LazyLock::new(|| translation(DOMAIN, localedir().as_deref(), None));

/// `_(text)` that reports, rather than raises, Python's exception: a
/// Plural-Forms expression that fails to evaluate (division by zero).
pub fn try_tr(text: &str) -> Result<String, String> {
    TRANSLATION.gettext(text)
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
    let r = TRANSLATION.ngettext_n(singular, plural, &n, &mut warned);
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

/// A catalog key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Key {
    Msg(String),
    Plural(String, i128),
}

#[derive(Debug, Default)]
struct GnuCatalog {
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

    /// `GNUTranslations._parse`, for a UTF-8 (or ASCII) catalog: any other
    /// charset is an error, as is a string that is not UTF-8.
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
        let text = |b: &[u8]| -> Result<String, String> {
            String::from_utf8(b.to_vec()).map_err(|_| format!("'utf-8' codec can't decode bytes: '{filename}'"))
        };
        let mut cat = GnuCatalog::default();
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
            let mend = moff.saturating_add(mlen);
            let (tlen, toff) = pair(transidx)?;
            let tend = toff.saturating_add(tlen);
            if !(mend < buflen && tend < buflen) {
                return Err(format!("[Errno 0] File is corrupt: '{filename}'"));
            }
            let msg = &buf[moff..mend];
            let tmsg = &buf[toff..tend];
            // See if we're looking at GNU .mo conventions for metadata
            if mlen == 0 {
                // Catalog description
                for b_item in tmsg.split(|&b| b == b'\n') {
                    let item = text(b_item)?;
                    let item = py_strip(&item);
                    if item.is_empty() {
                        continue;
                    }
                    // Skip over comment lines:
                    if item.starts_with("#-#-#-#-#") && item.ends_with("#-#-#-#-#") {
                        continue;
                    }
                    let Some((k, v)) = item.split_once(':') else { continue };
                    let k = py_strip(k).to_lowercase();
                    let v = py_strip(v);
                    if k == "content-type" {
                        let cs = v.split("charset=").nth(1).ok_or("list index out of range")?;
                        if !is_utf8_charset(cs) {
                            return Err(format!("charset {cs} is not UTF-8: '{filename}'"));
                        }
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
            if msg.contains(&0) {
                // Plural forms
                let parts: Vec<&[u8]> = msg.split(|&b| b == 0).collect();
                if parts.len() != 2 {
                    return Err(format!(
                        "too many values to unpack (expected 2, got {})",
                        parts.len()
                    ));
                }
                let msgid1 = text(parts[0])?;
                for (i, x) in tmsg.split(|&b| b == 0).enumerate() {
                    cat.catalog.insert(Key::Plural(msgid1.clone(), i as i128), text(x)?);
                }
            } else {
                cat.catalog.insert(Key::Msg(text(msg)?), text(tmsg)?);
            }
            // advance to next entry in the seek tables
            masteridx += 8;
            transidx += 8;
        }
        Ok(cat)
    }
}

/// Whether a Content-Type charset reads as UTF-8: UTF-8 itself, or ASCII
/// (a subset of it), or none given (Python then reads ASCII).
fn is_utf8_charset(charset: &str) -> bool {
    let name: String = charset.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase();
    matches!(name.as_str(), "" | "utf8" | "ascii" | "usascii" | "646")
}

/// `str.strip()`: Python's whitespace is Unicode White_Space plus the
/// information separators U+001C..U+001F.
fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}

impl Translation {
    /// `GNUTranslations.gettext`, following the fallback chain.
    pub fn gettext(&self, message: &str) -> Result<String, String> {
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

    /// `ngettext`; `warned` is set where gettext warns that the count is not
    /// an integer.
    fn ngettext_n(&self, msgid1: &str, msgid2: &str, n: &PluralN, warned: &mut bool) -> Result<String, String> {
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

/// `gettext.translation(domain, localedir, languages, fallback=True)`,
/// except that a catalog that cannot be read (corrupt, or not UTF-8) is left
/// out, with a note on stderr, rather than raising: its strings stay English.
pub fn translation(domain: &str, localedir: Option<&str>, languages: Option<Vec<String>>) -> Translation {
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
                let parsed = std::fs::read(&mofile)
                    .map_err(|e| e.to_string())
                    .and_then(|buf| GnuCatalog::parse(&buf, &mofile.to_string_lossy()));
                let t = match parsed {
                    Ok(t) => Arc::new(t),
                    Err(e) => {
                        eprintln!("i18n: {}: {e}; not translated", mofile.display());
                        continue;
                    }
                };
                if let Ok(mut m) = TRANSLATIONS.lock() {
                    m.entry(key).or_insert_with(|| t.clone()).clone()
                } else {
                    t
                }
            }
        };
        result.chain.push(t);
    }
    result
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

/// `gettext.find(domain, localedir, languages, all=True)`. A language
/// variable that is not UTF-8 (Python would keep its bytes) names no
/// catalog: it is read lossily and its path does not exist.
fn find_paths(domain: &str, localedir: Option<&str>, languages: Option<Vec<String>>) -> Vec<PathBuf> {
    // Get some reasonable defaults for arguments that were not supplied
    let localedir = localedir.unwrap_or(default_localedir());
    let languages: Vec<String> = match languages {
        Some(l) => l,
        None => {
            let mut languages: Vec<String> = Vec::new();
            for envar in ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"] {
                if let Some(val) = std::env::var_os(envar) {
                    if !val.is_empty() {
                        languages = val.to_string_lossy().split(':').map(String::from).collect();
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
        for nelang in expand_lang(lang) {
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
        let mofile = Path::new(localedir).join(&lang).join("LC_MESSAGES").join(format!("{domain}.mo"));
        if mofile.exists() {
            result.push(mofile);
        }
    }
    result
}

/// `gettext._expand_lang`.
fn expand_lang(loc: &str) -> Vec<String> {
    let loc = normalize(loc);
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
    // Normalize the locale name and extract the encoding and modifier
    let mut code = localename.to_lowercase();
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
        let norm_encoding = encoding.replace(['-', '_'], "");
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
                Some((_, m)) if m.to_lowercase() == modifier => return code,
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
                        if defmod.to_lowercase() == modifier {
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
        .get(norm_encoding.to_lowercase().as_str())
        .map(|s| s.to_string())
        .unwrap_or(norm_encoding);
    let mut encoding = norm_encoding.clone();
    let norm_encoding = norm_encoding.to_lowercase();
    if let Some(e) = LOCALE_ENCODING_ALIAS_MAP.get(norm_encoding.as_str()) {
        encoding = e.to_string();
    } else {
        let norm_encoding = norm_encoding.replace(['_', '-'], "");
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
        if c.is_alphanumeric() || c == '.' {
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

/// `\w` in a Python `str` pattern: alphanumeric or '_'. (Python's
/// `isalnum()` leaves out a few combining marks Rust counts; in a Plural-Forms
/// expression either is an invalid token.)
fn py_word(c: Option<&char>) -> bool {
    c.is_some_and(|&c| c == '_' || c.is_alphanumeric())
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
            } else if (c == 'n' && !py_word(cs.get(i + 1))) || "()-*/%+?:".contains(c) {
                (1, false) // NAME (only `n`), PARENTHESIS, OPERATOR
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// Compiles `po` (the text of a .po file) into <dir>/<lang>/LC_MESSAGES;
    /// false when gettext is not installed.
    fn msgfmt(dir: &Path, lang: &str, po: &str) -> bool {
        if crate::util::which("msgfmt").is_none() {
            eprintln!("skipped: gettext is not installed");
            return false;
        }
        let src = dir.join(format!("{lang}.po"));
        std::fs::write(&src, po).unwrap();
        std::fs::create_dir_all(dir.join(lang).join("LC_MESSAGES")).unwrap();
        let st = Command::new("msgfmt")
            .arg("-o")
            .arg(dir.join(lang).join("LC_MESSAGES/almanac.mo"))
            .arg(&src)
            .status()
            .unwrap();
        assert!(st.success());
        true
    }

    #[test]
    fn test_a_compiled_translation_is_used() {
        let d = tempfile::tempdir().unwrap();
        let po = "msgid \"\"\nmsgstr \"Content-Type: text/plain; charset=UTF-8\\n\"\n\n\
                  msgid \"History\"\nmsgstr \"Verlauf\"\n";
        if !msgfmt(d.path(), "de", po) {
            return;
        }
        let t = translation(DOMAIN, d.path().to_str(), Some(vec!["de".into()]));
        assert_eq!(t.gettext("History").unwrap(), "Verlauf");
        // de_DE.UTF-8 expands to de as well
        let t = translation(DOMAIN, d.path().to_str(), Some(vec!["de_DE.UTF-8".into()]));
        assert_eq!(t.gettext("History").unwrap(), "Verlauf");
    }

    #[test]
    fn plural_forms_pick_the_catalog_s_form() {
        let d = tempfile::tempdir().unwrap();
        let po = "msgid \"\"\nmsgstr \"\"\n\"Content-Type: text/plain; charset=UTF-8\\n\"\n\
                  \"Plural-Forms: nplurals=3; plural=(n%10==1 && n%100!=11 ? 0 : n%10>=2 && n%10<=4 && (n%100<10 || n%100>=20) ? 1 : 2);\\n\"\n\n\
                  msgid \"%d file\"\nmsgid_plural \"%d files\"\nmsgstr[0] \"%d plik\"\nmsgstr[1] \"%d pliki\"\nmsgstr[2] \"%d plików\"\n";
        if !msgfmt(d.path(), "pl", po) {
            return;
        }
        let t = translation(DOMAIN, d.path().to_str(), Some(vec!["pl".into()]));
        let at = |n: i64| t.ngettext("%d file", "%d files", n).unwrap();
        assert_eq!((at(1), at(3), at(5), at(22)), ("%d plik".into(), "%d pliki".into(), "%d plików".into(), "%d pliki".into()));
        // untranslated plurals fall back to English's two forms
        assert_eq!(t.ngettext("one", "many", 2).unwrap(), "many");
    }

    #[test]
    fn a_catalog_that_is_not_utf8_stays_english() {
        // msgfmt converts to UTF-8, so the catalog is written by hand: a
        // little-endian .mo with its strings sorted, each NUL-terminated.
        fn mo(entries: &[(&[u8], &[u8])]) -> Vec<u8> {
            let n = entries.len() as u32;
            let mut head: Vec<u8> = Vec::new();
            for w in [0x950412de, 0, n, 28, 28 + 8 * n, 0, 28 + 16 * n] {
                head.extend(u32::to_le_bytes(w));
            }
            let (mut orig, mut trans, mut strings) = (Vec::new(), Vec::new(), Vec::new());
            let base = 28 + 16 * n as usize;
            for (side, table) in [(0, &mut orig), (1, &mut trans)] {
                for e in entries {
                    let s = if side == 0 { e.0 } else { e.1 };
                    table.extend(u32::to_le_bytes(s.len() as u32));
                    table.extend(u32::to_le_bytes((base + strings.len()) as u32));
                    strings.extend(s);
                    strings.push(0);
                }
            }
            [head, orig, trans, strings].concat()
        }
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("pl/LC_MESSAGES");
        std::fs::create_dir_all(&dir).unwrap();
        let header: &[u8] = b"Content-Type: text/plain; charset=ISO-8859-2\n";
        std::fs::write(dir.join("almanac.mo"), mo(&[(b"", header), (b"History", b"Historia")])).unwrap();
        let t = translation(DOMAIN, d.path().to_str(), Some(vec!["pl".into()]));
        assert_eq!(t.gettext("History").unwrap(), "History");
        // the same catalog in UTF-8 is read
        let header: &[u8] = b"Content-Type: text/plain; charset=UTF-8\n";
        std::fs::create_dir_all(d.path().join("cs/LC_MESSAGES")).unwrap();
        let cat = mo(&[(b"", header), (b"History", "Historia ż".as_bytes())]);
        std::fs::write(d.path().join("cs/LC_MESSAGES/almanac.mo"), cat).unwrap();
        let t = translation(DOMAIN, d.path().to_str(), Some(vec!["cs".into()]));
        assert_eq!(t.gettext("History").unwrap(), "Historia ż");
        // nor does a file that is not a catalog stop anything
        std::fs::create_dir_all(d.path().join("fr/LC_MESSAGES")).unwrap();
        std::fs::write(d.path().join("fr/LC_MESSAGES/almanac.mo"), b"not a catalog").unwrap();
        let t = translation(DOMAIN, d.path().to_str(), Some(vec!["fr".into()]));
        assert_eq!(t.gettext("History").unwrap(), "History");
    }

    #[test]
    fn utf8_charset_names() {
        for cs in ["UTF-8", "utf8", "ASCII", "US-ASCII", ""] {
            assert!(is_utf8_charset(cs), "{cs}");
        }
        for cs in ["ISO-8859-2", "cp1252", "UTF-16"] {
            assert!(!is_utf8_charset(cs), "{cs}");
        }
    }

    #[test]
    fn stdlib_pieces_match_python() {
        // str.strip, os.path.normpath, unbounded ints
        assert_eq!(py_strip("\u{1c} a \u{1f}"), "a");
        assert_eq!(normpath(b"/a/./b/../c//d"), b"/a/c/d");
        assert_eq!(normpath(b"//a/.."), b"//");
        let f = c2py("n*n*n%7").unwrap();
        assert_eq!(f.call(&PluralN::from(1u64 << 62), &mut false).unwrap(), BigInt::from(1));
        assert!(c2py("n n $").unwrap_err().starts_with("unexpected token"));
    }

    #[test]
    fn locale_names_normalize_as_python_s() {
        assert_eq!(normalize("de"), "de_DE.ISO8859-1");
        assert_eq!(normalize("de_DE.utf8"), "de_DE.UTF-8");
        assert_eq!(normalize("fr_FR@euro"), "fr_FR.ISO8859-15");
        assert_eq!(expand_lang("pt_BR.UTF-8"), ["pt_BR.UTF-8", "pt_BR", "pt.UTF-8", "pt"]);
    }

    #[test]
    fn test_untranslated_falls_back_to_english() {
        assert_eq!(tr("No string anyone has translated"), "No string anyone has translated");
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
