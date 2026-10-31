//! Programs that start when you log in: XDG autostart entries.
//!
//! Every desktop that follows the freedesktop autostart spec -- GNOME, KDE,
//! Xfce, and Hyprland sessions run through uwsm or systemd's autostart
//! generator -- starts the `.desktop` files in `$XDG_CONFIG_DIRS/autostart`
//! (usually /etc/xdg/autostart) and `$XDG_CONFIG_HOME/autostart`. A file in the
//! user's directory overrides a system one of the same name.
//!
//! Switching one off follows the spec exactly: a user-directory copy with
//! `Hidden=true`. The system file is never touched, and switching it back on
//! removes only an override Almanac itself wrote (marked `X-Almanac-Override`),
//! so a user's own hand-made entry is edited in place rather than deleted.
//!
//! Port of `autostart.py`.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const MARK: &str = "X-Almanac-Override";
/// Almanac's own: shown elsewhere.
pub const OWN: &[&str] = &["io.github.twodxme.Almanac"];

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Entry {
    /// Shown text. A name that is not UTF-8 is kept byte-exact in `path`
    /// (see `raw_filename`), which is what `set_enabled` writes to.
    pub filename: String,
    pub name: String,
    pub comment: String,
    pub icon: String,
    pub enabled: bool,
    /// the file that decides (user copy if any)
    pub path: PathBuf,
    /// the shipped one, when there is one
    pub system: Option<PathBuf>,
    /// the user copy is an override Almanac wrote
    pub ours: bool,
}

pub fn user_dir() -> PathBuf {
    crate::xdg::config_home().join("autostart")
}

pub fn system_dirs() -> Vec<PathBuf> {
    // var_os: Python reads the environment through surrogateescape, so a
    // value that is not UTF-8 is still used as the paths it names.
    let dirs = std::env::var_os("XDG_CONFIG_DIRS")
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| OsString::from("/etc/xdg"));
    dirs.as_bytes()
        .split(|&b| b == b':')
        .filter(|d| !d.is_empty())
        .map(|d| py_path(d).join("autostart"))
        .collect()
}

/// `pathlib.Path(s)`: repeated slashes and `.` parts collapse, a trailing
/// slash goes, and exactly two leading slashes are kept (POSIX).
fn py_path(s: &[u8]) -> PathBuf {
    let prefix: &[u8] = if s.starts_with(b"//") && !s.starts_with(b"///") {
        b"//"
    } else if s.starts_with(b"/") {
        b"/"
    } else {
        b""
    };
    let parts: Vec<&[u8]> = s
        .split(|&b| b == b'/')
        .filter(|p| !p.is_empty() && *p != b".")
        .collect();
    let mut joined = prefix.to_vec();
    joined.extend_from_slice(&parts.join(&b'/'));
    // Path(".") / "x" is "x": an empty path joins the same way.
    PathBuf::from(OsString::from_vec(joined))
}

/// `pathlib.Path(p)` for a path already built: normalised as `py_path`
/// does, and an empty result is `.` (`Path("")` is `Path(".")`).
fn py_norm(p: &Path) -> PathBuf {
    let n = py_path(p.as_os_str().as_bytes());
    if n.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        n
    }
}

/// The code points of `os.fsdecode(b)`: bytes that are not UTF-8 become
/// U+DC80..U+DCFF (surrogateescape), so names compare and sort as Python's do.
fn py_codepoints(b: &[u8]) -> Vec<u32> {
    let mut out = Vec::with_capacity(b.len());
    for chunk in b.utf8_chunks() {
        out.extend(chunk.valid().chars().map(|c| c as u32));
        out.extend(chunk.invalid().iter().map(|&x| 0xDC00 + x as u32));
    }
    out
}

/// Characters Python's Unicode 16 tables do not have but Rust's Unicode 17
/// ones give case behaviour (a lowercase, or cased / case-ignorable for the
/// final-sigma rule). To Python they are unassigned: unchanged, and neither
/// cased nor case-ignorable. Found by comparing `str.lower()` with
/// `to_lowercase` over every code point (alone and next to a capital sigma).
const PY_UNASSIGNED: &[(u32, u32)] = &[
    (0x1ACF, 0x1ADD),
    (0x1AE0, 0x1AEB),
    (0xA7CE, 0xA7CF),
    (0xA7D2, 0xA7D2),
    (0xA7D4, 0xA7D4),
    (0xA7F1, 0xA7F1),
    (0x10EC5, 0x10EC5),
    (0x10EFA, 0x10EFB),
    (0x11B60, 0x11B60),
    (0x11B62, 0x11B64),
    (0x11B66, 0x11B66),
    (0x11DD9, 0x11DD9),
    (0x16EA0, 0x16EB8),
    (0x16EBB, 0x16ED3),
    (0x16FF2, 0x16FF3),
    (0x1E6E3, 0x1E6E3),
    (0x1E6E6, 0x1E6E6),
    (0x1E6EE, 0x1E6EF),
    (0x1E6F5, 0x1E6F5),
    (0x1E6FF, 0x1E6FF),
];
/// Cased (a lowercase letter) in Unicode 16, no longer cased in 17.
const PY_CASED_ONLY: &[u32] = &[0x0295];

fn py_unassigned(cp: u32) -> bool {
    (0xD800..=0xDFFF).contains(&cp) || PY_UNASSIGNED.iter().any(|&(a, b)| (a..=b).contains(&cp))
}

/// For the final-sigma rule, as Python's tables see it: (case-ignorable,
/// cased and not case-ignorable). Read off Rust's own sigma handling, then
/// corrected by the two tables above.
fn py_sigma_class(cp: u32) -> (bool, bool) {
    if py_unassigned(cp) {
        return (false, false);
    }
    if PY_CASED_ONLY.contains(&cp) {
        return (false, true);
    }
    let Some(c) = char::from_u32(cp) else {
        return (false, false);
    };
    // "A c Σ" ends in ς when c is skipped (ignorable) or is itself cased;
    // "c Σ" ends in ς only when c is cased and not skipped.
    let strong = format!("{c}\u{3a3}").to_lowercase().ends_with('\u{3c2}');
    let either = format!("A{c}\u{3a3}").to_lowercase().ends_with('\u{3c2}');
    (either && !strong, strong)
}

/// Python's `str.lower()` (Unicode 16, with its final-sigma rule) over
/// code points.
fn py_lower_cps(s: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(s.len());
    for (i, &cp) in s.iter().enumerate() {
        if cp == 0x3A3 {
            // \p{cased}\p{case-ignorable}* Σ !(\p{case-ignorable}*\p{cased})
            let before = s[..i]
                .iter()
                .rev()
                .map(|&c| py_sigma_class(c))
                .find(|&(ign, _)| !ign);
            let mut fin = matches!(before, Some((_, true)));
            if fin {
                let after = s[i + 1..]
                    .iter()
                    .map(|&c| py_sigma_class(c))
                    .find(|&(ign, _)| !ign);
                fin = !matches!(after, Some((_, true)));
            }
            out.push(if fin { 0x3C2 } else { 0x3C3 });
        } else if py_unassigned(cp) {
            out.push(cp);
        } else if let Some(c) = char::from_u32(cp) {
            out.extend(c.to_lowercase().map(|x| x as u32));
        } else {
            out.push(cp);
        }
    }
    out
}

/// Python's `str.lower()`.
fn py_lower(s: &str) -> String {
    let cps: Vec<u32> = s.chars().map(|c| c as u32).collect();
    py_lower_cps(&cps)
        .into_iter()
        .filter_map(char::from_u32)
        .collect()
}

/// Python's `str.splitlines()`: every line boundary it knows, no trailing
/// empty line.
fn py_splitlines(s: &str) -> Vec<&str> {
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
            '\n' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}'
            | '\u{2029}' => i + c.len_utf8(),
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

/// Python's `str.strip()` with no argument (`str.isspace` characters).
fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c))
}

/// The encoding `Path.read_text` / `write_text` use: the locale's (Python
/// 3.14 is not in UTF-8 mode by default). `None` is UTF-8 -- also for the C
/// / POSIX locale, where Python switches UTF-8 mode on (PEP 538/540), and
/// under PYTHONUTF8=1. The locale itself is set by `gtk::init`.
fn locale_codeset() -> Option<String> {
    if std::env::var_os("PYTHONUTF8").is_some_and(|v| v == "1") {
        return None;
    }
    let (utf8, cs) = gtk::glib::charset();
    if utf8 {
        return None;
    }
    let cs = cs?.as_str().to_owned();
    match cs.to_ascii_uppercase().as_str() {
        "ANSI_X3.4-1968" | "ASCII" | "US-ASCII" | "646" | "" => None,
        _ => Some(cs),
    }
}

/// `bytes.decode(codeset, errors="replace")`: what cannot be decoded becomes
/// U+FFFD, one per offending byte.
fn decode_with(mut b: &[u8], cs: &str) -> String {
    use gtk::glib::{self, CvtError};
    let lossy = |s: &[u8]| String::from_utf8_lossy(s).into_owned();
    let mut out = String::new();
    while !b.is_empty() {
        match glib::convert(b, "UTF-8", cs) {
            Ok((s, _)) => {
                out.push_str(&lossy(&s));
                break;
            }
            Err(CvtError::IllegalSequence { offset, .. }) if offset < b.len() => {
                if offset > 0 {
                    if let Ok((s, _)) = glib::convert(&b[..offset], "UTF-8", cs) {
                        out.push_str(&lossy(&s));
                    }
                }
                out.push('\u{fffd}');
                b = &b[offset + 1..];
            }
            Err(CvtError::Convert(e))
                if e.kind::<glib::ConvertError>() == Some(glib::ConvertError::PartialInput) =>
            {
                // an unfinished sequence at the end: one replacement for it
                let n = (b.len().saturating_sub(8)..b.len()).rev().find_map(|n| {
                    glib::convert(&b[..n], "UTF-8", cs)
                        .ok()
                        .map(|(s, _)| (n, s))
                });
                if let Some((_, s)) = n {
                    out.push_str(&lossy(&s));
                }
                out.push('\u{fffd}');
                break;
            }
            Err(_) => {
                // a codeset iconv does not know: read it as UTF-8
                out.push_str(&lossy(b));
                break;
            }
        }
    }
    out
}

/// `Path.read_text(errors="replace")`: decoded, then universal newlines
/// (`\r\n` and a lone `\r` read as `\n`).
fn read_text(path: &Path) -> std::io::Result<String> {
    let b = std::fs::read(path)?;
    let text = match locale_codeset() {
        None => String::from_utf8_lossy(&b).into_owned(),
        Some(cs) => decode_with(&b, &cs),
    };
    Ok(if text.contains('\r') {
        text.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        text
    })
}

/// How Python's codec for a locale codeset reports what it cannot encode:
/// (codec name in the error, reason, whether a run of such characters is
/// reported together). Single-byte codecs other than Latin-1 are charmap
/// codecs; the CJK ones report one character at a time.
fn py_codec_error_style(cs: &str) -> (String, &'static str, bool) {
    // encodings.normalize_encoding, then the alias table's spellings
    let mut norm = String::new();
    for c in cs.to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() || c == '.' {
            norm.push(c);
        } else if !norm.ends_with('_') && !norm.is_empty() {
            norm.push('_');
        }
    }
    let norm = norm.trim_end_matches('_');
    let latin1 = [
        "latin_1",
        "latin1",
        "latin",
        "l1",
        "iso_8859_1",
        "iso8859_1",
        "8859",
        "cp819",
        "ibm819",
        "iso_ir_100",
        "iso_8859_1_1987",
        "csisolatin1",
    ];
    if latin1.contains(&norm) {
        return ("latin-1".into(), "ordinal not in range(256)", true);
    }
    let cjk: &[(&[&str], &str)] = &[
        (&["euc_jp", "eucjp", "ujis", "u_jis"], "euc_jp"),
        (
            &[
                "euc_kr",
                "euckr",
                "korean",
                "ksc5601",
                "ks_c_5601",
                "ks_c_5601_1987",
                "ksx1001",
                "ks_x_1001",
            ],
            "euc_kr",
        ),
        (
            &[
                "gb2312",
                "chinese",
                "csiso58gb231280",
                "euc_cn",
                "euccn",
                "eucgb2312_cn",
                "gb2312_1980",
                "gb2312_80",
                "iso_ir_58",
            ],
            "gb2312",
        ),
        (&["gbk", "936", "cp936", "ms936"], "gbk"),
        (&["gb18030", "gb18030_2000"], "gb18030"),
        (&["big5", "big5_tw", "csbig5"], "big5"),
        (&["big5hkscs", "big5_hkscs", "hkscs"], "big5hkscs"),
        (
            &["shift_jis", "csshiftjis", "shiftjis", "sjis", "s_jis"],
            "shift_jis",
        ),
        (&["cp932", "932", "ms932", "mskanji", "ms_kanji"], "cp932"),
        (&["cp949", "949", "ms949", "uhc"], "cp949"),
        (&["cp950", "950", "ms950"], "cp950"),
        (&["euc_jis_2004", "jisx0213", "eucjis2004"], "euc_jis_2004"),
        (&["euc_jisx0213", "eucjisx0213"], "euc_jisx0213"),
        (&["johab", "cp1361", "ms1361"], "johab"),
        (
            &["shift_jis_2004", "shiftjis2004", "sjis_2004", "s_jis_2004"],
            "shift_jis_2004",
        ),
        (
            &["shift_jisx0213", "shiftjisx0213", "sjisx0213", "s_jisx0213"],
            "shift_jisx0213",
        ),
    ];
    for (names, codec) in cjk {
        if names.contains(&norm) {
            return ((*codec).into(), "illegal multibyte sequence", false);
        }
    }
    ("charmap".into(), "character maps to <undefined>", true)
}

/// `str(UnicodeEncodeError)` for the first character of `text` (from
/// character index `pos`) the codeset cannot encode.
fn py_encode_error(cs: &str, chars: &[char], pos: usize) -> String {
    let (codec, reason, run) = py_codec_error_style(cs);
    let encodable = |c: char| {
        if codec == "latin-1" {
            (c as u32) < 0x100
        } else {
            let mut b = [0u8; 4];
            gtk::glib::convert(c.encode_utf8(&mut b).as_bytes(), cs, "UTF-8").is_ok()
        }
    };
    let mut end = pos + 1;
    if run {
        while end < chars.len() && !encodable(chars[end]) {
            end += 1;
        }
    }
    if end == pos + 1 {
        let cp = chars[pos] as u32;
        let shown = if cp <= 0xff {
            format!("\\x{cp:02x}")
        } else if cp <= 0xffff {
            format!("\\u{cp:04x}")
        } else {
            format!("\\U{cp:08x}")
        };
        format!("'{codec}' codec can't encode character '{shown}' in position {pos}: {reason}")
    } else {
        format!(
            "'{codec}' codec can't encode characters in position {pos}-{}: {reason}",
            end - 1
        )
    }
}

/// `text` in the locale's encoding, replacing `path` atomically
/// (`util::atomic_write`): a crash or a text the locale cannot encode leaves
/// the old file as it was. The error is the UnicodeEncodeError's text.
fn write_text(path: &Path, text: &str) -> Result<(), String> {
    use gtk::glib::{self, CvtError};
    let encoded: Vec<u8> = match locale_codeset() {
        None => text.as_bytes().to_vec(),
        Some(cs) => match glib::convert(text.as_bytes(), cs.as_str(), "UTF-8") {
            Ok((s, _)) => s.to_vec(),
            Err(CvtError::IllegalSequence { offset, .. }) => {
                let pos = text.get(..offset).map_or(0, |t| t.chars().count());
                let chars: Vec<char> = text.chars().collect();
                if pos >= chars.len() {
                    return Err(format!("'{cs}' codec can't encode text"));
                }
                return Err(py_encode_error(&cs, &chars, pos));
            }
            Err(e) => return Err(e.to_string()),
        },
    };
    crate::util::atomic_write(path, &encoded).map_err(|x| os_err(path, x))
}

fn get<'a>(d: &'a BTreeMap<String, String>, k: &str, default: &'a str) -> &'a str {
    d.get(k).map(String::as_str).unwrap_or(default)
}

/// The [Desktop Entry] group's keys. Localised keys are skipped.
fn parse(path: &Path) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    let mut group = "";
    let text = match read_text(path) {
        Ok(t) => t,
        Err(_) => return out,
    };
    for line in py_splitlines(&text) {
        let line = py_strip(line);
        if line.starts_with('[') {
            group = line;
        } else if group == "[Desktop Entry]" && line.contains('=') && !line.starts_with('#') {
            let (k, v) = line.split_once('=').unwrap_or((line, ""));
            if !k.contains('[') {
                out.entry(py_strip(k).to_owned())
                    .or_insert_with(|| py_strip(v).to_owned());
            }
        }
    }
    out
}

/// Whether this desktop would run it at all (OnlyShowIn / NotShowIn).
fn applies(d: &BTreeMap<String, String>) -> bool {
    // var_os: compared as bytes, so a value that is not UTF-8 still counts
    // (it can never equal a key from the file, as with Python's surrogates).
    let current = std::env::var_os("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let here: BTreeSet<&[u8]> = current
        .as_bytes()
        .split(|&b| b == b':')
        .filter(|x| !x.is_empty())
        .collect();
    let list = |k: &str| -> BTreeSet<&[u8]> {
        get(d, k, "")
            .split(';')
            .filter(|x| !x.is_empty())
            .map(str::as_bytes)
            .collect()
    };
    let only = list("OnlyShowIn");
    let never = list("NotShowIn");
    if !only.is_empty() && !here.is_empty() && only.is_disjoint(&here) {
        return false;
    }
    never.is_disjoint(&here)
}

fn on(d: &BTreeMap<String, String>) -> bool {
    py_lower(get(d, "Hidden", "false")) != "true"
        && py_lower(get(d, "X-GNOME-Autostart-enabled", "true")) != "false"
}

/// `sorted(folder.glob("*.desktop")) if folder.is_dir() else []`
fn desktop_files(folder: &Path) -> Vec<PathBuf> {
    if !folder.is_dir() {
        return Vec::new();
    }
    let mut files: Vec<PathBuf> = match std::fs::read_dir(folder) {
        Ok(rd) => rd
            .flatten()
            .filter(|e| e.file_name().as_bytes().ends_with(b".desktop"))
            .map(|e| e.path())
            .collect(),
        Err(_) => Vec::new(),
    };
    files.sort_by_cached_key(|f| py_codepoints(f.as_os_str().as_bytes()));
    files
}

/// The file name exactly as on disk. `filename` is a `String`, so a name that
/// is not UTF-8 is only shown lossily there; the entry's `path` carries the
/// real bytes (it always ends in that name when the entry came from
/// `entries()`).
fn raw_filename(e: &Entry) -> OsString {
    match e.path.file_name() {
        Some(n) if n.to_string_lossy() == e.filename.as_str() => n.to_owned(),
        _ => OsString::from(&e.filename),
    }
}

pub fn entries() -> Vec<Entry> {
    if crate::demo::active() {
        return crate::demo::startup_apps();
    }
    // Keyed by the name's Python code points: dict keys and sorted() order
    // as Python has them, and names that are not UTF-8 stay distinct.
    let mut found: BTreeMap<Vec<u32>, (OsString, PathBuf)> = BTreeMap::new();
    for folder in system_dirs().iter().rev() {
        // earlier dirs take precedence
        for f in desktop_files(folder) {
            let name = f.file_name().unwrap_or_default().to_owned();
            found.insert(py_codepoints(name.as_bytes()), (name, f));
        }
    }
    let systems = found.clone();
    for f in desktop_files(&user_dir()) {
        let name = f.file_name().unwrap_or_default().to_owned();
        found.insert(py_codepoints(name.as_bytes()), (name, f));
    }
    let mut out: Vec<(Vec<u32>, Entry)> = Vec::new();
    for (key, (raw, path)) in &found {
        if OWN.iter().any(|o| raw.as_bytes().starts_with(o.as_bytes())) {
            continue;
        }
        let name = raw.to_string_lossy().into_owned();
        let d = parse(path);
        let system = systems.get(key).map(|(_, p)| p.clone());
        let base = match &system {
            Some(s) => parse(s),
            None => d.clone(),
        };
        if !applies(&base) || get(&d, "Type", "Application") != "Application" {
            continue;
        }
        let pick = |a: Option<&String>| a.filter(|x| !x.is_empty()).cloned();
        let or = |a: Option<&String>, b: &str| pick(a).unwrap_or_else(|| b.to_owned());
        // out.sort(key=lambda e: e.name.lower()), over Python's str: the
        // name falls back to the file name, surrogates and all.
        let (display, sort_key) = match pick(base.get("Name")).or_else(|| pick(d.get("Name"))) {
            Some(n) => {
                let k = py_lower_cps(&n.chars().map(|c| c as u32).collect::<Vec<_>>());
                (n, k)
            }
            None => (name.clone(), py_lower_cps(key)),
        };
        out.push((
            sort_key,
            Entry {
                filename: name,
                name: display,
                comment: or(base.get("Comment"), get(&d, "Comment", "")),
                icon: or(base.get("Icon"), get(&d, "Icon", "")),
                enabled: on(&d),
                path: path.clone(),
                system,
                ours: py_lower(get(&d, MARK, "")) == "true",
            },
        ));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out.into_iter().map(|(_, e)| e).collect()
}

/// Code points Python 3.14's `str.isprintable` (Unicode 16) rejects, as
/// inclusive ranges: categories Cc Cf Cs Co Cn Zl Zp and Zs except space.
/// Generated from CPython itself.
const PY_NOT_PRINTABLE: &[(u32, u32)] = &[
    (0x0, 0x1F),
    (0x7F, 0xA0),
    (0xAD, 0xAD),
    (0x378, 0x379),
    (0x380, 0x383),
    (0x38B, 0x38B),
    (0x38D, 0x38D),
    (0x3A2, 0x3A2),
    (0x530, 0x530),
    (0x557, 0x558),
    (0x58B, 0x58C),
    (0x590, 0x590),
    (0x5C8, 0x5CF),
    (0x5EB, 0x5EE),
    (0x5F5, 0x605),
    (0x61C, 0x61C),
    (0x6DD, 0x6DD),
    (0x70E, 0x70F),
    (0x74B, 0x74C),
    (0x7B2, 0x7BF),
    (0x7FB, 0x7FC),
    (0x82E, 0x82F),
    (0x83F, 0x83F),
    (0x85C, 0x85D),
    (0x85F, 0x85F),
    (0x86B, 0x86F),
    (0x88F, 0x896),
    (0x8E2, 0x8E2),
    (0x984, 0x984),
    (0x98D, 0x98E),
    (0x991, 0x992),
    (0x9A9, 0x9A9),
    (0x9B1, 0x9B1),
    (0x9B3, 0x9B5),
    (0x9BA, 0x9BB),
    (0x9C5, 0x9C6),
    (0x9C9, 0x9CA),
    (0x9CF, 0x9D6),
    (0x9D8, 0x9DB),
    (0x9DE, 0x9DE),
    (0x9E4, 0x9E5),
    (0x9FF, 0xA00),
    (0xA04, 0xA04),
    (0xA0B, 0xA0E),
    (0xA11, 0xA12),
    (0xA29, 0xA29),
    (0xA31, 0xA31),
    (0xA34, 0xA34),
    (0xA37, 0xA37),
    (0xA3A, 0xA3B),
    (0xA3D, 0xA3D),
    (0xA43, 0xA46),
    (0xA49, 0xA4A),
    (0xA4E, 0xA50),
    (0xA52, 0xA58),
    (0xA5D, 0xA5D),
    (0xA5F, 0xA65),
    (0xA77, 0xA80),
    (0xA84, 0xA84),
    (0xA8E, 0xA8E),
    (0xA92, 0xA92),
    (0xAA9, 0xAA9),
    (0xAB1, 0xAB1),
    (0xAB4, 0xAB4),
    (0xABA, 0xABB),
    (0xAC6, 0xAC6),
    (0xACA, 0xACA),
    (0xACE, 0xACF),
    (0xAD1, 0xADF),
    (0xAE4, 0xAE5),
    (0xAF2, 0xAF8),
    (0xB00, 0xB00),
    (0xB04, 0xB04),
    (0xB0D, 0xB0E),
    (0xB11, 0xB12),
    (0xB29, 0xB29),
    (0xB31, 0xB31),
    (0xB34, 0xB34),
    (0xB3A, 0xB3B),
    (0xB45, 0xB46),
    (0xB49, 0xB4A),
    (0xB4E, 0xB54),
    (0xB58, 0xB5B),
    (0xB5E, 0xB5E),
    (0xB64, 0xB65),
    (0xB78, 0xB81),
    (0xB84, 0xB84),
    (0xB8B, 0xB8D),
    (0xB91, 0xB91),
    (0xB96, 0xB98),
    (0xB9B, 0xB9B),
    (0xB9D, 0xB9D),
    (0xBA0, 0xBA2),
    (0xBA5, 0xBA7),
    (0xBAB, 0xBAD),
    (0xBBA, 0xBBD),
    (0xBC3, 0xBC5),
    (0xBC9, 0xBC9),
    (0xBCE, 0xBCF),
    (0xBD1, 0xBD6),
    (0xBD8, 0xBE5),
    (0xBFB, 0xBFF),
    (0xC0D, 0xC0D),
    (0xC11, 0xC11),
    (0xC29, 0xC29),
    (0xC3A, 0xC3B),
    (0xC45, 0xC45),
    (0xC49, 0xC49),
    (0xC4E, 0xC54),
    (0xC57, 0xC57),
    (0xC5B, 0xC5C),
    (0xC5E, 0xC5F),
    (0xC64, 0xC65),
    (0xC70, 0xC76),
    (0xC8D, 0xC8D),
    (0xC91, 0xC91),
    (0xCA9, 0xCA9),
    (0xCB4, 0xCB4),
    (0xCBA, 0xCBB),
    (0xCC5, 0xCC5),
    (0xCC9, 0xCC9),
    (0xCCE, 0xCD4),
    (0xCD7, 0xCDC),
    (0xCDF, 0xCDF),
    (0xCE4, 0xCE5),
    (0xCF0, 0xCF0),
    (0xCF4, 0xCFF),
    (0xD0D, 0xD0D),
    (0xD11, 0xD11),
    (0xD45, 0xD45),
    (0xD49, 0xD49),
    (0xD50, 0xD53),
    (0xD64, 0xD65),
    (0xD80, 0xD80),
    (0xD84, 0xD84),
    (0xD97, 0xD99),
    (0xDB2, 0xDB2),
    (0xDBC, 0xDBC),
    (0xDBE, 0xDBF),
    (0xDC7, 0xDC9),
    (0xDCB, 0xDCE),
    (0xDD5, 0xDD5),
    (0xDD7, 0xDD7),
    (0xDE0, 0xDE5),
    (0xDF0, 0xDF1),
    (0xDF5, 0xE00),
    (0xE3B, 0xE3E),
    (0xE5C, 0xE80),
    (0xE83, 0xE83),
    (0xE85, 0xE85),
    (0xE8B, 0xE8B),
    (0xEA4, 0xEA4),
    (0xEA6, 0xEA6),
    (0xEBE, 0xEBF),
    (0xEC5, 0xEC5),
    (0xEC7, 0xEC7),
    (0xECF, 0xECF),
    (0xEDA, 0xEDB),
    (0xEE0, 0xEFF),
    (0xF48, 0xF48),
    (0xF6D, 0xF70),
    (0xF98, 0xF98),
    (0xFBD, 0xFBD),
    (0xFCD, 0xFCD),
    (0xFDB, 0xFFF),
    (0x10C6, 0x10C6),
    (0x10C8, 0x10CC),
    (0x10CE, 0x10CF),
    (0x1249, 0x1249),
    (0x124E, 0x124F),
    (0x1257, 0x1257),
    (0x1259, 0x1259),
    (0x125E, 0x125F),
    (0x1289, 0x1289),
    (0x128E, 0x128F),
    (0x12B1, 0x12B1),
    (0x12B6, 0x12B7),
    (0x12BF, 0x12BF),
    (0x12C1, 0x12C1),
    (0x12C6, 0x12C7),
    (0x12D7, 0x12D7),
    (0x1311, 0x1311),
    (0x1316, 0x1317),
    (0x135B, 0x135C),
    (0x137D, 0x137F),
    (0x139A, 0x139F),
    (0x13F6, 0x13F7),
    (0x13FE, 0x13FF),
    (0x1680, 0x1680),
    (0x169D, 0x169F),
    (0x16F9, 0x16FF),
    (0x1716, 0x171E),
    (0x1737, 0x173F),
    (0x1754, 0x175F),
    (0x176D, 0x176D),
    (0x1771, 0x1771),
    (0x1774, 0x177F),
    (0x17DE, 0x17DF),
    (0x17EA, 0x17EF),
    (0x17FA, 0x17FF),
    (0x180E, 0x180E),
    (0x181A, 0x181F),
    (0x1879, 0x187F),
    (0x18AB, 0x18AF),
    (0x18F6, 0x18FF),
    (0x191F, 0x191F),
    (0x192C, 0x192F),
    (0x193C, 0x193F),
    (0x1941, 0x1943),
    (0x196E, 0x196F),
    (0x1975, 0x197F),
    (0x19AC, 0x19AF),
    (0x19CA, 0x19CF),
    (0x19DB, 0x19DD),
    (0x1A1C, 0x1A1D),
    (0x1A5F, 0x1A5F),
    (0x1A7D, 0x1A7E),
    (0x1A8A, 0x1A8F),
    (0x1A9A, 0x1A9F),
    (0x1AAE, 0x1AAF),
    (0x1ACF, 0x1AFF),
    (0x1B4D, 0x1B4D),
    (0x1BF4, 0x1BFB),
    (0x1C38, 0x1C3A),
    (0x1C4A, 0x1C4C),
    (0x1C8B, 0x1C8F),
    (0x1CBB, 0x1CBC),
    (0x1CC8, 0x1CCF),
    (0x1CFB, 0x1CFF),
    (0x1F16, 0x1F17),
    (0x1F1E, 0x1F1F),
    (0x1F46, 0x1F47),
    (0x1F4E, 0x1F4F),
    (0x1F58, 0x1F58),
    (0x1F5A, 0x1F5A),
    (0x1F5C, 0x1F5C),
    (0x1F5E, 0x1F5E),
    (0x1F7E, 0x1F7F),
    (0x1FB5, 0x1FB5),
    (0x1FC5, 0x1FC5),
    (0x1FD4, 0x1FD5),
    (0x1FDC, 0x1FDC),
    (0x1FF0, 0x1FF1),
    (0x1FF5, 0x1FF5),
    (0x1FFF, 0x200F),
    (0x2028, 0x202F),
    (0x205F, 0x206F),
    (0x2072, 0x2073),
    (0x208F, 0x208F),
    (0x209D, 0x209F),
    (0x20C1, 0x20CF),
    (0x20F1, 0x20FF),
    (0x218C, 0x218F),
    (0x242A, 0x243F),
    (0x244B, 0x245F),
    (0x2B74, 0x2B75),
    (0x2B96, 0x2B96),
    (0x2CF4, 0x2CF8),
    (0x2D26, 0x2D26),
    (0x2D28, 0x2D2C),
    (0x2D2E, 0x2D2F),
    (0x2D68, 0x2D6E),
    (0x2D71, 0x2D7E),
    (0x2D97, 0x2D9F),
    (0x2DA7, 0x2DA7),
    (0x2DAF, 0x2DAF),
    (0x2DB7, 0x2DB7),
    (0x2DBF, 0x2DBF),
    (0x2DC7, 0x2DC7),
    (0x2DCF, 0x2DCF),
    (0x2DD7, 0x2DD7),
    (0x2DDF, 0x2DDF),
    (0x2E5E, 0x2E7F),
    (0x2E9A, 0x2E9A),
    (0x2EF4, 0x2EFF),
    (0x2FD6, 0x2FEF),
    (0x3000, 0x3000),
    (0x3040, 0x3040),
    (0x3097, 0x3098),
    (0x3100, 0x3104),
    (0x3130, 0x3130),
    (0x318F, 0x318F),
    (0x31E6, 0x31EE),
    (0x321F, 0x321F),
    (0xA48D, 0xA48F),
    (0xA4C7, 0xA4CF),
    (0xA62C, 0xA63F),
    (0xA6F8, 0xA6FF),
    (0xA7CE, 0xA7CF),
    (0xA7D2, 0xA7D2),
    (0xA7D4, 0xA7D4),
    (0xA7DD, 0xA7F1),
    (0xA82D, 0xA82F),
    (0xA83A, 0xA83F),
    (0xA878, 0xA87F),
    (0xA8C6, 0xA8CD),
    (0xA8DA, 0xA8DF),
    (0xA954, 0xA95E),
    (0xA97D, 0xA97F),
    (0xA9CE, 0xA9CE),
    (0xA9DA, 0xA9DD),
    (0xA9FF, 0xA9FF),
    (0xAA37, 0xAA3F),
    (0xAA4E, 0xAA4F),
    (0xAA5A, 0xAA5B),
    (0xAAC3, 0xAADA),
    (0xAAF7, 0xAB00),
    (0xAB07, 0xAB08),
    (0xAB0F, 0xAB10),
    (0xAB17, 0xAB1F),
    (0xAB27, 0xAB27),
    (0xAB2F, 0xAB2F),
    (0xAB6C, 0xAB6F),
    (0xABEE, 0xABEF),
    (0xABFA, 0xABFF),
    (0xD7A4, 0xD7AF),
    (0xD7C7, 0xD7CA),
    (0xD7FC, 0xF8FF),
    (0xFA6E, 0xFA6F),
    (0xFADA, 0xFAFF),
    (0xFB07, 0xFB12),
    (0xFB18, 0xFB1C),
    (0xFB37, 0xFB37),
    (0xFB3D, 0xFB3D),
    (0xFB3F, 0xFB3F),
    (0xFB42, 0xFB42),
    (0xFB45, 0xFB45),
    (0xFBC3, 0xFBD2),
    (0xFD90, 0xFD91),
    (0xFDC8, 0xFDCE),
    (0xFDD0, 0xFDEF),
    (0xFE1A, 0xFE1F),
    (0xFE53, 0xFE53),
    (0xFE67, 0xFE67),
    (0xFE6C, 0xFE6F),
    (0xFE75, 0xFE75),
    (0xFEFD, 0xFF00),
    (0xFFBF, 0xFFC1),
    (0xFFC8, 0xFFC9),
    (0xFFD0, 0xFFD1),
    (0xFFD8, 0xFFD9),
    (0xFFDD, 0xFFDF),
    (0xFFE7, 0xFFE7),
    (0xFFEF, 0xFFFB),
    (0xFFFE, 0xFFFF),
    (0x1000C, 0x1000C),
    (0x10027, 0x10027),
    (0x1003B, 0x1003B),
    (0x1003E, 0x1003E),
    (0x1004E, 0x1004F),
    (0x1005E, 0x1007F),
    (0x100FB, 0x100FF),
    (0x10103, 0x10106),
    (0x10134, 0x10136),
    (0x1018F, 0x1018F),
    (0x1019D, 0x1019F),
    (0x101A1, 0x101CF),
    (0x101FE, 0x1027F),
    (0x1029D, 0x1029F),
    (0x102D1, 0x102DF),
    (0x102FC, 0x102FF),
    (0x10324, 0x1032C),
    (0x1034B, 0x1034F),
    (0x1037B, 0x1037F),
    (0x1039E, 0x1039E),
    (0x103C4, 0x103C7),
    (0x103D6, 0x103FF),
    (0x1049E, 0x1049F),
    (0x104AA, 0x104AF),
    (0x104D4, 0x104D7),
    (0x104FC, 0x104FF),
    (0x10528, 0x1052F),
    (0x10564, 0x1056E),
    (0x1057B, 0x1057B),
    (0x1058B, 0x1058B),
    (0x10593, 0x10593),
    (0x10596, 0x10596),
    (0x105A2, 0x105A2),
    (0x105B2, 0x105B2),
    (0x105BA, 0x105BA),
    (0x105BD, 0x105BF),
    (0x105F4, 0x105FF),
    (0x10737, 0x1073F),
    (0x10756, 0x1075F),
    (0x10768, 0x1077F),
    (0x10786, 0x10786),
    (0x107B1, 0x107B1),
    (0x107BB, 0x107FF),
    (0x10806, 0x10807),
    (0x10809, 0x10809),
    (0x10836, 0x10836),
    (0x10839, 0x1083B),
    (0x1083D, 0x1083E),
    (0x10856, 0x10856),
    (0x1089F, 0x108A6),
    (0x108B0, 0x108DF),
    (0x108F3, 0x108F3),
    (0x108F6, 0x108FA),
    (0x1091C, 0x1091E),
    (0x1093A, 0x1093E),
    (0x10940, 0x1097F),
    (0x109B8, 0x109BB),
    (0x109D0, 0x109D1),
    (0x10A04, 0x10A04),
    (0x10A07, 0x10A0B),
    (0x10A14, 0x10A14),
    (0x10A18, 0x10A18),
    (0x10A36, 0x10A37),
    (0x10A3B, 0x10A3E),
    (0x10A49, 0x10A4F),
    (0x10A59, 0x10A5F),
    (0x10AA0, 0x10ABF),
    (0x10AE7, 0x10AEA),
    (0x10AF7, 0x10AFF),
    (0x10B36, 0x10B38),
    (0x10B56, 0x10B57),
    (0x10B73, 0x10B77),
    (0x10B92, 0x10B98),
    (0x10B9D, 0x10BA8),
    (0x10BB0, 0x10BFF),
    (0x10C49, 0x10C7F),
    (0x10CB3, 0x10CBF),
    (0x10CF3, 0x10CF9),
    (0x10D28, 0x10D2F),
    (0x10D3A, 0x10D3F),
    (0x10D66, 0x10D68),
    (0x10D86, 0x10D8D),
    (0x10D90, 0x10E5F),
    (0x10E7F, 0x10E7F),
    (0x10EAA, 0x10EAA),
    (0x10EAE, 0x10EAF),
    (0x10EB2, 0x10EC1),
    (0x10EC5, 0x10EFB),
    (0x10F28, 0x10F2F),
    (0x10F5A, 0x10F6F),
    (0x10F8A, 0x10FAF),
    (0x10FCC, 0x10FDF),
    (0x10FF7, 0x10FFF),
    (0x1104E, 0x11051),
    (0x11076, 0x1107E),
    (0x110BD, 0x110BD),
    (0x110C3, 0x110CF),
    (0x110E9, 0x110EF),
    (0x110FA, 0x110FF),
    (0x11135, 0x11135),
    (0x11148, 0x1114F),
    (0x11177, 0x1117F),
    (0x111E0, 0x111E0),
    (0x111F5, 0x111FF),
    (0x11212, 0x11212),
    (0x11242, 0x1127F),
    (0x11287, 0x11287),
    (0x11289, 0x11289),
    (0x1128E, 0x1128E),
    (0x1129E, 0x1129E),
    (0x112AA, 0x112AF),
    (0x112EB, 0x112EF),
    (0x112FA, 0x112FF),
    (0x11304, 0x11304),
    (0x1130D, 0x1130E),
    (0x11311, 0x11312),
    (0x11329, 0x11329),
    (0x11331, 0x11331),
    (0x11334, 0x11334),
    (0x1133A, 0x1133A),
    (0x11345, 0x11346),
    (0x11349, 0x1134A),
    (0x1134E, 0x1134F),
    (0x11351, 0x11356),
    (0x11358, 0x1135C),
    (0x11364, 0x11365),
    (0x1136D, 0x1136F),
    (0x11375, 0x1137F),
    (0x1138A, 0x1138A),
    (0x1138C, 0x1138D),
    (0x1138F, 0x1138F),
    (0x113B6, 0x113B6),
    (0x113C1, 0x113C1),
    (0x113C3, 0x113C4),
    (0x113C6, 0x113C6),
    (0x113CB, 0x113CB),
    (0x113D6, 0x113D6),
    (0x113D9, 0x113E0),
    (0x113E3, 0x113FF),
    (0x1145C, 0x1145C),
    (0x11462, 0x1147F),
    (0x114C8, 0x114CF),
    (0x114DA, 0x1157F),
    (0x115B6, 0x115B7),
    (0x115DE, 0x115FF),
    (0x11645, 0x1164F),
    (0x1165A, 0x1165F),
    (0x1166D, 0x1167F),
    (0x116BA, 0x116BF),
    (0x116CA, 0x116CF),
    (0x116E4, 0x116FF),
    (0x1171B, 0x1171C),
    (0x1172C, 0x1172F),
    (0x11747, 0x117FF),
    (0x1183C, 0x1189F),
    (0x118F3, 0x118FE),
    (0x11907, 0x11908),
    (0x1190A, 0x1190B),
    (0x11914, 0x11914),
    (0x11917, 0x11917),
    (0x11936, 0x11936),
    (0x11939, 0x1193A),
    (0x11947, 0x1194F),
    (0x1195A, 0x1199F),
    (0x119A8, 0x119A9),
    (0x119D8, 0x119D9),
    (0x119E5, 0x119FF),
    (0x11A48, 0x11A4F),
    (0x11AA3, 0x11AAF),
    (0x11AF9, 0x11AFF),
    (0x11B0A, 0x11BBF),
    (0x11BE2, 0x11BEF),
    (0x11BFA, 0x11BFF),
    (0x11C09, 0x11C09),
    (0x11C37, 0x11C37),
    (0x11C46, 0x11C4F),
    (0x11C6D, 0x11C6F),
    (0x11C90, 0x11C91),
    (0x11CA8, 0x11CA8),
    (0x11CB7, 0x11CFF),
    (0x11D07, 0x11D07),
    (0x11D0A, 0x11D0A),
    (0x11D37, 0x11D39),
    (0x11D3B, 0x11D3B),
    (0x11D3E, 0x11D3E),
    (0x11D48, 0x11D4F),
    (0x11D5A, 0x11D5F),
    (0x11D66, 0x11D66),
    (0x11D69, 0x11D69),
    (0x11D8F, 0x11D8F),
    (0x11D92, 0x11D92),
    (0x11D99, 0x11D9F),
    (0x11DAA, 0x11EDF),
    (0x11EF9, 0x11EFF),
    (0x11F11, 0x11F11),
    (0x11F3B, 0x11F3D),
    (0x11F5B, 0x11FAF),
    (0x11FB1, 0x11FBF),
    (0x11FF2, 0x11FFE),
    (0x1239A, 0x123FF),
    (0x1246F, 0x1246F),
    (0x12475, 0x1247F),
    (0x12544, 0x12F8F),
    (0x12FF3, 0x12FFF),
    (0x13430, 0x1343F),
    (0x13456, 0x1345F),
    (0x143FB, 0x143FF),
    (0x14647, 0x160FF),
    (0x1613A, 0x167FF),
    (0x16A39, 0x16A3F),
    (0x16A5F, 0x16A5F),
    (0x16A6A, 0x16A6D),
    (0x16ABF, 0x16ABF),
    (0x16ACA, 0x16ACF),
    (0x16AEE, 0x16AEF),
    (0x16AF6, 0x16AFF),
    (0x16B46, 0x16B4F),
    (0x16B5A, 0x16B5A),
    (0x16B62, 0x16B62),
    (0x16B78, 0x16B7C),
    (0x16B90, 0x16D3F),
    (0x16D7A, 0x16E3F),
    (0x16E9B, 0x16EFF),
    (0x16F4B, 0x16F4E),
    (0x16F88, 0x16F8E),
    (0x16FA0, 0x16FDF),
    (0x16FE5, 0x16FEF),
    (0x16FF2, 0x16FFF),
    (0x187F8, 0x187FF),
    (0x18CD6, 0x18CFE),
    (0x18D09, 0x1AFEF),
    (0x1AFF4, 0x1AFF4),
    (0x1AFFC, 0x1AFFC),
    (0x1AFFF, 0x1AFFF),
    (0x1B123, 0x1B131),
    (0x1B133, 0x1B14F),
    (0x1B153, 0x1B154),
    (0x1B156, 0x1B163),
    (0x1B168, 0x1B16F),
    (0x1B2FC, 0x1BBFF),
    (0x1BC6B, 0x1BC6F),
    (0x1BC7D, 0x1BC7F),
    (0x1BC89, 0x1BC8F),
    (0x1BC9A, 0x1BC9B),
    (0x1BCA0, 0x1CBFF),
    (0x1CCFA, 0x1CCFF),
    (0x1CEB4, 0x1CEFF),
    (0x1CF2E, 0x1CF2F),
    (0x1CF47, 0x1CF4F),
    (0x1CFC4, 0x1CFFF),
    (0x1D0F6, 0x1D0FF),
    (0x1D127, 0x1D128),
    (0x1D173, 0x1D17A),
    (0x1D1EB, 0x1D1FF),
    (0x1D246, 0x1D2BF),
    (0x1D2D4, 0x1D2DF),
    (0x1D2F4, 0x1D2FF),
    (0x1D357, 0x1D35F),
    (0x1D379, 0x1D3FF),
    (0x1D455, 0x1D455),
    (0x1D49D, 0x1D49D),
    (0x1D4A0, 0x1D4A1),
    (0x1D4A3, 0x1D4A4),
    (0x1D4A7, 0x1D4A8),
    (0x1D4AD, 0x1D4AD),
    (0x1D4BA, 0x1D4BA),
    (0x1D4BC, 0x1D4BC),
    (0x1D4C4, 0x1D4C4),
    (0x1D506, 0x1D506),
    (0x1D50B, 0x1D50C),
    (0x1D515, 0x1D515),
    (0x1D51D, 0x1D51D),
    (0x1D53A, 0x1D53A),
    (0x1D53F, 0x1D53F),
    (0x1D545, 0x1D545),
    (0x1D547, 0x1D549),
    (0x1D551, 0x1D551),
    (0x1D6A6, 0x1D6A7),
    (0x1D7CC, 0x1D7CD),
    (0x1DA8C, 0x1DA9A),
    (0x1DAA0, 0x1DAA0),
    (0x1DAB0, 0x1DEFF),
    (0x1DF1F, 0x1DF24),
    (0x1DF2B, 0x1DFFF),
    (0x1E007, 0x1E007),
    (0x1E019, 0x1E01A),
    (0x1E022, 0x1E022),
    (0x1E025, 0x1E025),
    (0x1E02B, 0x1E02F),
    (0x1E06E, 0x1E08E),
    (0x1E090, 0x1E0FF),
    (0x1E12D, 0x1E12F),
    (0x1E13E, 0x1E13F),
    (0x1E14A, 0x1E14D),
    (0x1E150, 0x1E28F),
    (0x1E2AF, 0x1E2BF),
    (0x1E2FA, 0x1E2FE),
    (0x1E300, 0x1E4CF),
    (0x1E4FA, 0x1E5CF),
    (0x1E5FB, 0x1E5FE),
    (0x1E600, 0x1E7DF),
    (0x1E7E7, 0x1E7E7),
    (0x1E7EC, 0x1E7EC),
    (0x1E7EF, 0x1E7EF),
    (0x1E7FF, 0x1E7FF),
    (0x1E8C5, 0x1E8C6),
    (0x1E8D7, 0x1E8FF),
    (0x1E94C, 0x1E94F),
    (0x1E95A, 0x1E95D),
    (0x1E960, 0x1EC70),
    (0x1ECB5, 0x1ED00),
    (0x1ED3E, 0x1EDFF),
    (0x1EE04, 0x1EE04),
    (0x1EE20, 0x1EE20),
    (0x1EE23, 0x1EE23),
    (0x1EE25, 0x1EE26),
    (0x1EE28, 0x1EE28),
    (0x1EE33, 0x1EE33),
    (0x1EE38, 0x1EE38),
    (0x1EE3A, 0x1EE3A),
    (0x1EE3C, 0x1EE41),
    (0x1EE43, 0x1EE46),
    (0x1EE48, 0x1EE48),
    (0x1EE4A, 0x1EE4A),
    (0x1EE4C, 0x1EE4C),
    (0x1EE50, 0x1EE50),
    (0x1EE53, 0x1EE53),
    (0x1EE55, 0x1EE56),
    (0x1EE58, 0x1EE58),
    (0x1EE5A, 0x1EE5A),
    (0x1EE5C, 0x1EE5C),
    (0x1EE5E, 0x1EE5E),
    (0x1EE60, 0x1EE60),
    (0x1EE63, 0x1EE63),
    (0x1EE65, 0x1EE66),
    (0x1EE6B, 0x1EE6B),
    (0x1EE73, 0x1EE73),
    (0x1EE78, 0x1EE78),
    (0x1EE7D, 0x1EE7D),
    (0x1EE7F, 0x1EE7F),
    (0x1EE8A, 0x1EE8A),
    (0x1EE9C, 0x1EEA0),
    (0x1EEA4, 0x1EEA4),
    (0x1EEAA, 0x1EEAA),
    (0x1EEBC, 0x1EEEF),
    (0x1EEF2, 0x1EFFF),
    (0x1F02C, 0x1F02F),
    (0x1F094, 0x1F09F),
    (0x1F0AF, 0x1F0B0),
    (0x1F0C0, 0x1F0C0),
    (0x1F0D0, 0x1F0D0),
    (0x1F0F6, 0x1F0FF),
    (0x1F1AE, 0x1F1E5),
    (0x1F203, 0x1F20F),
    (0x1F23C, 0x1F23F),
    (0x1F249, 0x1F24F),
    (0x1F252, 0x1F25F),
    (0x1F266, 0x1F2FF),
    (0x1F6D8, 0x1F6DB),
    (0x1F6ED, 0x1F6EF),
    (0x1F6FD, 0x1F6FF),
    (0x1F777, 0x1F77A),
    (0x1F7DA, 0x1F7DF),
    (0x1F7EC, 0x1F7EF),
    (0x1F7F1, 0x1F7FF),
    (0x1F80C, 0x1F80F),
    (0x1F848, 0x1F84F),
    (0x1F85A, 0x1F85F),
    (0x1F888, 0x1F88F),
    (0x1F8AE, 0x1F8AF),
    (0x1F8BC, 0x1F8BF),
    (0x1F8C2, 0x1F8FF),
    (0x1FA54, 0x1FA5F),
    (0x1FA6E, 0x1FA6F),
    (0x1FA7D, 0x1FA7F),
    (0x1FA8A, 0x1FA8E),
    (0x1FAC7, 0x1FACD),
    (0x1FADD, 0x1FADE),
    (0x1FAEA, 0x1FAEF),
    (0x1FAF9, 0x1FAFF),
    (0x1FB93, 0x1FB93),
    (0x1FBFA, 0x1FFFF),
    (0x2A6E0, 0x2A6FF),
    (0x2B73A, 0x2B73F),
    (0x2B81E, 0x2B81F),
    (0x2CEA2, 0x2CEAF),
    (0x2EBE1, 0x2EBEF),
    (0x2EE5E, 0x2F7FF),
    (0x2FA1E, 0x2FFFF),
    (0x3134B, 0x3134F),
    (0x323B0, 0xE00FF),
    (0xE01F0, 0x10FFFF),
];

/// `str.isprintable` for one code point, by Python's own tables.
fn py_printable(cp: u32) -> bool {
    PY_NOT_PRINTABLE
        .binary_search_by(|&(a, b)| {
            if b < cp {
                std::cmp::Ordering::Less
            } else if a > cp {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_err()
}

/// Python's `repr(str)`, over code points (surrogates show as `\udcXX`).
fn py_repr(cps: &[u32]) -> String {
    let has = |c: char| cps.contains(&(c as u32));
    let q = if has('\'') && !has('"') { '"' } else { '\'' };
    let printable = |cp: u32| char::from_u32(cp).is_some() && py_printable(cp);
    let mut out = String::new();
    out.push(q);
    for &cp in cps {
        match char::from_u32(cp) {
            Some('\\') => out.push_str("\\\\"),
            Some(c) if c == q => {
                out.push('\\');
                out.push(c);
            }
            Some('\t') => out.push_str("\\t"),
            Some('\n') => out.push_str("\\n"),
            Some('\r') => out.push_str("\\r"),
            Some(c) if printable(cp) => out.push(c),
            _ if cp < 0x100 => out.push_str(&format!("\\x{cp:02x}")),
            _ if cp < 0x10000 => out.push_str(&format!("\\u{cp:04x}")),
            _ => out.push_str(&format!("\\U{cp:08x}")),
        }
    }
    out.push(q);
    out
}

fn repr_path(p: &Path) -> String {
    py_repr(&py_codepoints(p.as_os_str().as_bytes()))
}

/// `[Errno 13] Permission denied` -- the start of an OSError's str().
fn errno_text(e: &std::io::Error) -> Option<String> {
    let n = e.raw_os_error()?;
    let s = std::io::Error::from_raw_os_error(n).to_string();
    let msg = s
        .strip_suffix(&format!(" (os error {n})"))
        .unwrap_or(&s)
        .to_owned();
    Some(format!("[Errno {n}] {msg}"))
}

/// `str(OSError)` for a call on one path: `[Errno 2] No such file or
/// directory: '/p'`.
fn os_err(path: &Path, e: std::io::Error) -> String {
    match errno_text(&e) {
        Some(t) => format!("{t}: {}", repr_path(path)),
        None => e.to_string(),
    }
}

/// `Path.mkdir(parents=True, exist_ok=True)`, with the error naming the
/// directory that failed, as Python's does.
fn mkdir_parents(p: &Path) -> Result<(), String> {
    match std::fs::create_dir(p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Path("x").parent is Path("."), which is its own parent
            let parent = match p.parent() {
                Some(x) if x.as_os_str().is_empty() => Path::new("."),
                Some(x) => x,
                None => return Err(os_err(p, e)),
            };
            if parent == p {
                return Err(os_err(p, e));
            }
            mkdir_parents(parent)?;
            match std::fs::create_dir(p) {
                Ok(()) => Ok(()),
                Err(_) if p.is_dir() => Ok(()),
                Err(e) => Err(os_err(p, e)),
            }
        }
        Err(_) if p.is_dir() => Ok(()),
        Err(e) => Err(os_err(p, e)),
    }
}

/// Switch an entry, the way the spec says to. `Err` where the Python raises
/// OSError (or UnicodeEncodeError, a ValueError, under a locale that cannot
/// encode the text).
pub fn set_enabled(e: &Entry, on: bool) -> Result<(), String> {
    // pathlib: `user_dir() / e.filename` is normalised ("" and "." are the
    // directory itself, a trailing slash goes)
    let target = py_norm(&user_dir().join(raw_filename(e)));
    let source = py_norm(e.system.as_ref().unwrap_or(&e.path));
    if on {
        if target.exists() && py_lower(get(&parse(&target), MARK, "")) == "true" {
            // our override: just remove it
            std::fs::remove_file(&target).map_err(|x| os_err(&target, x))?;
        } else if target.exists() {
            // the user's own: edit in place
            return set_keys(
                &target,
                &[("Hidden", "false"), ("X-GNOME-Autostart-enabled", "true")],
            );
        }
        // With no copy of the user's left, the system's file decides; one
        // that is itself switched off needs an override switching it on.
        if !target.exists() && source != target && !self::on(&parse(&source)) {
            let text = read_text(&source).map_err(|x| os_err(&source, x))?;
            mkdir_parents(&user_dir())?;
            let text = with_keys(
                &text,
                &[
                    ("Hidden", "false"),
                    ("X-GNOME-Autostart-enabled", "true"),
                    (MARK, "true"),
                ],
            );
            write_text(&target, &text)?;
        }
        return Ok(());
    }
    if target.exists() {
        return set_keys(&target, &[("Hidden", "true")]);
    }
    mkdir_parents(&user_dir())?;
    let text = read_text(&source).map_err(|x| os_err(&source, x))?;
    // One write: a copy without Hidden would still start, and one without
    // the mark would shadow the system file for good.
    write_text(
        &target,
        &with_keys(&text, &[("Hidden", "true"), (MARK, "true")]),
    )
}

fn set_keys(path: &Path, keys: &[(&str, &str)]) -> Result<(), String> {
    let text = read_text(path).map_err(|x| os_err(path, x))?;
    write_text(path, &with_keys(&text, keys))
}

/// `text` with each key set in its [Desktop Entry] group.
fn with_keys(text: &str, keys: &[(&str, &str)]) -> String {
    keys.iter()
        .fold(text.to_string(), |t, (k, v)| with_key(&t, k, v))
}

fn with_key(text: &str, key: &str, value: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut group = String::new();
    let mut done = false;
    for line in py_splitlines(text) {
        let stripped = py_strip(line);
        if stripped.starts_with('[') {
            if group == "[Desktop Entry]" && !done {
                out.push(format!("{key}={value}"));
                done = true;
            }
            group = stripped.to_owned();
        } else if group == "[Desktop Entry]"
            && py_strip(stripped.split_once('=').map_or(stripped, |(k, _)| k)) == key
        {
            if !done {
                out.push(format!("{key}={value}"));
                done = true;
            }
            continue;
        }
        out.push(line.to_owned());
    }
    if !done {
        out.push(format!("{key}={value}"));
    }
    out.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The StartupApps fixture: a system dir with one entry for every desktop
    /// and one for GNOME only, a user config home, and Hyprland running.
    struct StartupApps {
        _tmp: tempfile::TempDir,
        sysdir: PathBuf,
        saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl StartupApps {
        fn set_up() -> Self {
            let guard = crate::util::TEST_ENV
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().to_path_buf();
            let sysdir = root.join("etc/xdg/autostart");
            std::fs::create_dir_all(&sysdir).unwrap();
            std::fs::write(
                sysdir.join("keys.desktop"),
                "[Desktop Entry]\nType=Application\nName=Keys\nExec=keys\n",
            )
            .unwrap();
            std::fs::write(
                sysdir.join("gnome-only.desktop"),
                "[Desktop Entry]\nType=Application\nName=G\nExec=g\nOnlyShowIn=GNOME;\n",
            )
            .unwrap();
            let vars: [(&'static str, String); 4] = [
                (
                    "XDG_CONFIG_DIRS",
                    root.join("etc/xdg").to_string_lossy().into_owned(),
                ),
                (
                    "XDG_CONFIG_HOME",
                    root.join("home").to_string_lossy().into_owned(),
                ),
                ("XDG_CURRENT_DESKTOP", "Hyprland".into()),
                ("ALMANAC_DEMO", String::new()),
            ];
            let saved = vars
                .iter()
                .map(|(k, _)| (*k, std::env::var_os(k)))
                .collect();
            for (k, v) in &vars {
                std::env::set_var(k, v);
            }
            StartupApps {
                _tmp: tmp,
                sysdir,
                saved,
                _guard: guard,
            }
        }

        fn entry(&self, name: &str) -> Entry {
            entries().into_iter().find(|e| e.filename == name).unwrap()
        }
    }

    impl Drop for StartupApps {
        fn drop(&mut self) {
            for (k, old) in &self.saved {
                match old {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    #[test]
    fn test_entries_that_would_not_run_here_are_left_out() {
        let _t = StartupApps::set_up();
        let names: Vec<String> = entries().into_iter().map(|e| e.filename).collect();
        assert_eq!(names, vec!["keys.desktop".to_string()]);
    }

    #[test]
    fn test_off_is_a_user_override_and_on_removes_only_ours() {
        let t = StartupApps::set_up();
        set_enabled(&t.entry("keys.desktop"), false).unwrap();
        let over = user_dir().join("keys.desktop");
        assert!(std::fs::read_to_string(&over)
            .unwrap()
            .contains("Hidden=true"));
        assert!(!t.entry("keys.desktop").enabled);
        assert!(!std::fs::read_to_string(t.sysdir.join("keys.desktop"))
            .unwrap()
            .contains("Hidden"));
        set_enabled(&t.entry("keys.desktop"), true).unwrap();
        assert!(!over.exists());
        assert!(t.entry("keys.desktop").enabled);
    }

    #[test]
    fn a_system_entry_that_is_itself_off_can_be_switched_on() {
        let t = StartupApps::set_up();
        std::fs::write(
            t.sysdir.join("off.desktop"),
            "[Desktop Entry]\nType=Application\nName=Off\nExec=o\nHidden=true\n",
        )
        .unwrap();
        assert!(!t.entry("off.desktop").enabled);
        set_enabled(&t.entry("off.desktop"), true).unwrap();
        assert!(t.entry("off.desktop").enabled, "switched on, it is on");
        set_enabled(&t.entry("off.desktop"), false).unwrap();
        assert!(!t.entry("off.desktop").enabled);
        set_enabled(&t.entry("off.desktop"), true).unwrap();
        assert!(t.entry("off.desktop").enabled, "and on again after that");
        // The off override is written whole: hidden and marked as Almanac's.
        set_enabled(&t.entry("keys.desktop"), false).unwrap();
        let over = std::fs::read_to_string(user_dir().join("keys.desktop")).unwrap();
        assert!(
            over.contains("Hidden=true") && over.contains(&format!("{MARK}=true")),
            "{over}"
        );
    }

    #[test]
    fn test_a_users_own_entry_is_edited_never_deleted() {
        let t = StartupApps::set_up();
        let mine = user_dir().join("mine.desktop");
        std::fs::create_dir_all(mine.parent().unwrap()).unwrap();
        std::fs::write(
            &mine,
            "[Desktop Entry]\nType=Application\nName=Mine\nExec=m\n",
        )
        .unwrap();
        set_enabled(&t.entry("mine.desktop"), false).unwrap();
        set_enabled(&t.entry("mine.desktop"), true).unwrap();
        assert!(mine.exists());
        assert!(t.entry("mine.desktop").enabled);
    }

    #[test]
    fn splitlines_and_strip_follow_python() {
        assert_eq!(
            py_splitlines("a\r\nb\rc\n\nd\x0ce\u{2028}f\n"),
            vec!["a", "b", "c", "", "d", "e", "f"]
        );
        assert_eq!(py_splitlines(""), Vec::<&str>::new());
        assert_eq!(py_strip("\x1f a \u{3000}"), "a");
    }

    #[test]
    fn lower_uses_pythons_unicode_tables() {
        // Unicode 17 additions are unassigned to Python 3.14: unchanged
        assert_eq!(py_lower("\u{a7d2}\u{16ea0}"), "\u{a7d2}\u{16ea0}");
        assert_eq!(py_lower("ÉMILE İ"), "émile i\u{307}");
        // final sigma, and U+0295 still counts as cased
        assert_eq!(py_lower("ΟΔΟΣ ΣΑ"), "οδος σα");
        assert_eq!(py_lower("\u{295}Σ"), "\u{295}ς");
        assert_eq!(py_lower("\u{16ebb}Σ"), "\u{16ebb}σ");
        // surrogate-escaped bytes sort after every BMP letter below them
        assert_eq!(py_codepoints(b"caf\xe9"), vec![0x63, 0x61, 0x66, 0xdce9]);
    }

    #[test]
    fn os_errors_read_like_pythons() {
        let e = std::io::Error::from_raw_os_error(2);
        assert_eq!(
            os_err(Path::new("/p"), e),
            "[Errno 2] No such file or directory: '/p'"
        );
        let odd = PathBuf::from(OsString::from_vec(b"/it's/caf\xe9\n".to_vec()));
        assert_eq!(repr_path(&odd), "\"/it's/caf\\udce9\\n\"");
    }

    #[test]
    fn repr_escapes_what_python_cannot_print() {
        // unassigned, noncharacter, format and tag characters
        assert_eq!(py_repr(&[0x78, 0x378]), "'x\\u0378'");
        assert_eq!(
            py_repr(&[0xFDD0, 0x890, 0x110BD]),
            "'\\ufdd0\\u0890\\U000110bd'"
        );
        assert_eq!(py_repr(&[0xE0041, 0x1D173]), "'\\U000e0041\\U0001d173'");
        assert_eq!(py_repr(&[0x65E5, 0x20, 0xE9]), "'日 é'");
    }

    #[test]
    fn paths_normalise_like_pathlib() {
        assert_eq!(py_norm(Path::new("/a/b/")), Path::new("/a/b"));
        assert_eq!(py_norm(Path::new("/a/b/.")), Path::new("/a/b"));
        assert_eq!(py_norm(Path::new("")), Path::new("."));
    }

    #[test]
    fn encode_errors_name_pythons_codec() {
        let chars: Vec<char> = "ab日日c".chars().collect();
        assert_eq!(
            py_encode_error("ISO-8859-1", &chars, 2),
            "'latin-1' codec can't encode characters in position 2-3: ordinal not in range(256)"
        );
        let chars: Vec<char> = "abc日".chars().collect();
        assert_eq!(py_encode_error("ISO-8859-1", &chars, 3),
            "'latin-1' codec can't encode character '\\u65e5' in position 3: ordinal not in range(256)");
        let chars: Vec<char> = "a\u{1f600}\u{1f600}".chars().collect();
        assert_eq!(py_encode_error("EUC-JP", &chars, 1),
            "'euc_jp' codec can't encode character '\\U0001f600' in position 1: illegal multibyte sequence");
        let chars: Vec<char> = "ab日日c".chars().collect();
        assert_eq!(py_encode_error("ISO-8859-15", &chars, 2),
            "'charmap' codec can't encode characters in position 2-3: character maps to <undefined>");
    }

    #[test]
    fn a_name_that_is_not_utf8_is_switched_under_that_name() {
        let t = StartupApps::set_up();
        let raw = OsString::from_vec(b"caf\xe9.desktop".to_vec());
        std::fs::write(
            t.sysdir.join(&raw),
            "[Desktop Entry]\nType=Application\nName=Cafe\nExec=c\n",
        )
        .unwrap();
        let e = entries().into_iter().find(|e| e.name == "Cafe").unwrap();
        set_enabled(&e, false).unwrap();
        assert!(user_dir().join(&raw).exists());
        assert!(
            !entries()
                .into_iter()
                .find(|e| e.name == "Cafe")
                .unwrap()
                .enabled
        );
        set_enabled(&e, true).unwrap();
        assert!(!user_dir().join(&raw).exists());
    }
}
