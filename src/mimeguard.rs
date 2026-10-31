//! Which files an install quietly took over.
//!
//! An app's launcher entry lists the file types it can open. Installing it is
//! enough, on most desktops, to make it the default for any of those types that
//! had no explicit choice -- which is how a word processor installed to try out
//! becomes what opens every .docx, with no prompt and no record of what opened
//! them before.
//!
//! So installs are bracketed: the default application for every known file type
//! is read before and after, and anything that changed hands is shown with what
//! used to open it, and can be handed back. The reading uses GIO, the same
//! lookup file managers and `xdg-open` use, in a fresh process -- a long-running
//! one caches the desktop database and would not see the change.
//!
//! Handing a type back goes through `xdg-mime default`, the standard way to
//! record a choice, so it lands in the user's mimeapps.list like any choice made
//! in a file manager.
//!
//! Port of `mimeguard.py`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::OnceLock;
use std::time::Duration;

use gtk::gio::prelude::*;

use crate::util;

/// The hidden flag that makes the binary print a snapshot and exit (see
/// `snapshot`).
pub const SNAPSHOT_FLAG: &str = "--mime-snapshot";

/// Every type something can open, with the app that opens it now, as this
/// process's GIO sees it. Only meaningful in a fresh process: GIO's app cache
/// in a long-running one is stale by design (it updates asynchronously from
/// file monitors), which is why `snapshot` runs it in a child.
pub fn snapshot_here() -> BTreeMap<String, String> {
    let mut dirs = vec![std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| {
            util::home()
                .join(".local/share")
                .to_string_lossy()
                .into_owned()
        })];
    dirs.extend(
        std::env::var("XDG_DATA_DIRS")
            .ok()
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| "/usr/local/share:/usr/share".to_string())
            .split(':')
            .map(str::to_string),
    );
    let mut types = BTreeSet::new();
    for d in dirs {
        let Ok(text) =
            std::fs::read_to_string(std::path::Path::new(&d).join("applications/mimeinfo.cache"))
        else {
            continue;
        };
        for line in text.lines() {
            if let Some((t, _)) = line.split_once('=') {
                if !line.starts_with('[') {
                    types.insert(t.trim().to_string());
                }
            }
        }
    }
    let mut out = BTreeMap::new();
    for t in types {
        if let Some(app) = gtk::gio::AppInfo::default_for_type(&t, false) {
            out.insert(t, app.id().map(|i| i.to_string()).unwrap_or_default());
        }
    }
    out
}

/// `--mime-snapshot`: print `snapshot_here` as JSON on stdout.
pub fn print_snapshot() {
    println!(
        "{}",
        serde_json::to_string(&snapshot_here()).unwrap_or_else(|_| "{}".into())
    );
}

/// {mime type: desktop id} for every type something can open.
pub fn snapshot() -> Option<BTreeMap<String, String>> {
    // A fresh process of this same binary, so GIO's app cache is current.
    let exe = std::env::current_exe().ok()?;
    let exe = exe.to_string_lossy().into_owned();
    let (code, stdout, _stderr) = run_bytes(&[&exe, SNAPSHOT_FLAG], 60)?;
    if code != Some(0) {
        return None;
    }
    serde_json::from_slice::<BTreeMap<String, String>>(&stdout).ok()
}

/// `subprocess.run(argv, capture_output=True, timeout=secs)` keeping the raw
/// bytes: (returncode, stdout, stderr), or None on OSError / TimeoutExpired.
///
/// Like `communicate()`, the timeout covers reading both pipes to EOF, not
/// just the child's exit: a descendant that keeps a pipe open past it is a
/// timeout. On a timeout `util::run_bytes` kills the child's whole process
/// group (not only the child, as `subprocess.run` does). stdin is inherited
/// (no `stdin=` argument in the Python).
fn run_bytes(argv: &[&str], timeout_secs: u64) -> Option<(Option<i32>, Vec<u8>, Vec<u8>)> {
    run_bytes_within(argv, Duration::from_secs(timeout_secs))
}

/// `run_bytes` with any timeout (the tests use a fraction of a second).
fn run_bytes_within(argv: &[&str], timeout: Duration) -> Option<(Option<i32>, Vec<u8>, Vec<u8>)> {
    let out = util::run_bytes(
        argv,
        &util::RunOpts {
            timeout: Some(timeout),
            stdin: util::Stdin::Inherit,
            ..Default::default()
        },
    );
    (!out.failed_to_start && !out.timed_out).then_some((out.code, out.stdout, out.stderr))
}

/// What a type's old id can be: a string, or (from a caller passing a map
/// the Python would accept) any JSON value, carried through unchanged.
pub trait OldId: Clone {
    /// `before.get(t, "")`'s default.
    fn missing() -> Self;
    /// `new == old` for a string `new`.
    fn is(&self, new: &str) -> bool;
}

impl OldId for String {
    fn missing() -> Self {
        String::new()
    }
    fn is(&self, new: &str) -> bool {
        self == new
    }
}

impl OldId for serde_json::Value {
    fn missing() -> Self {
        serde_json::Value::String(String::new())
    }
    fn is(&self, new: &str) -> bool {
        self.as_str() == Some(new)
    }
}

/// (type, old id, new id) for every type whose default moved.
///
/// A type that had no default before and has one now is a takeover too --
/// but only one where something else could already open it is worth asking
/// about, so those carry the old id as ''.
///
/// `after` is walked in its own order (a Python dict's insertion order), so it
/// is any iterable of pairs: a snapshot's map, or an ordered list.
pub fn changes<'a, V, A>(
    before: Option<&BTreeMap<String, V>>,
    after: Option<A>,
) -> Vec<(String, V, String)>
where
    V: OldId,
    A: IntoIterator<Item = (&'a String, &'a String)>,
{
    let (Some(before), Some(after)) = (before, after) else {
        return Vec::new();
    };
    let mut after = after.into_iter().peekable();
    if before.is_empty() || after.peek().is_none() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (t, new) in after {
        let old = before.get(t).cloned().unwrap_or_else(V::missing);
        if !new.is_empty() && !old.is(new) {
            out.push((t.clone(), old, new.clone()));
        }
    }
    out
}

pub fn app_name(desktop_id: &str) -> String {
    if desktop_id.is_empty() {
        return "nothing".to_string();
    }
    // PyGObject rejects an id with a NUL in it (the Python's
    // ValueError/TypeError); fall through to the bare id the same way.
    if !desktop_id.contains('\0') {
        // `Gio.DesktopAppInfo.new(id)` (gio-rs 0.22 keeps it in gio-unix). It
        // skips an entry that cannot load (TryExec missing, not a key file)
        // and uses the next one of the same id further down the data dirs.
        if let Some(info) = gio_unix::DesktopAppInfo::new(desktop_id) {
            let name = info.display_name();
            if !name.is_empty() {
                return name.to_string();
            }
            return desktop_id.to_string();
        }
    }
    desktop_id
        .strip_suffix(".desktop")
        .unwrap_or(desktop_id)
        .to_string()
}

/// A file type as a person names it: '.docx' rather than the MIME string.
///
/// `Err` is what raised out of it: the OSError `mimetypes.init()` raises for
/// a system mime.types file that exists but cannot be read.
pub fn try_label(mime: &str) -> Result<String, String> {
    Ok(guess_extension(mime, false)?.unwrap_or_else(|| mime.to_string()))
}

/// `try_label` for callers that cannot take the error: the MIME string itself
/// when the system's mime.types cannot be read.
pub fn label(mime: &str) -> String {
    try_label(mime).unwrap_or_else(|_| mime.to_string())
}

/// `try_summary` for callers that cannot take the error: types whose label
/// cannot be looked up are shown as their MIME string.
pub fn summary(moved: &[(String, String, String)]) -> Vec<String> {
    summary_with(moved, &mut |t| Ok(label(t))).unwrap_or_default()
}

/// One line per new owner: 'WordCraft now opens .docx, .odt (was LibreOffice)'.
///
/// `Err` is `label`'s OSError, which the Python does not catch.
pub fn try_summary(moved: &[(String, String, String)]) -> Result<Vec<String>, String> {
    summary_with(moved, &mut try_label)
}

fn summary_with(
    moved: &[(String, String, String)],
    label: &mut dyn FnMut(&str) -> Result<String, String>,
) -> Result<Vec<String>, String> {
    // Insertion-ordered {new: [(type, old)]}, as the Python dict is.
    let mut by_new: Vec<(String, Vec<(String, String)>)> = Vec::new();
    for (t, old, new) in moved {
        match by_new.iter_mut().find(|(n, _)| n == new) {
            Some((_, pairs)) => pairs.push((t.clone(), old.clone())),
            None => by_new.push((new.clone(), vec![(t.clone(), old.clone())])),
        }
    }
    let mut lines = Vec::new();
    for (new, pairs) in &by_new {
        let kinds: BTreeSet<String> = pairs
            .iter()
            .map(|(t, _o)| label(t))
            .collect::<Result<_, _>>()?;
        let kinds = kinds.into_iter().take(8).collect::<Vec<_>>().join(", ");
        let olds: BTreeSet<String> = pairs
            .iter()
            .filter(|(_t, o)| !o.is_empty())
            .map(|(_t, o)| app_name(o))
            .collect();
        let was = if olds.is_empty() {
            String::new()
        } else {
            format!(
                " — before: {}",
                olds.into_iter().collect::<Vec<_>>().join(", ")
            )
        };
        lines.push(format!("{} now opens {kinds}{was}", app_name(new)));
    }
    Ok(lines)
}

/// Give each type back to what opened it before. Returns how many.
/// `log` None is the Python default, `print`.
///
/// `Err` is what raised out of it: subprocess.run's ValueError('embedded null
/// byte') for a NUL in the type or the old id, which the Python does not
/// catch -- the caller must stop there, as the Python's code after the call
/// never runs.
pub fn try_restore(
    moved: &[(String, String, String)],
    log: Option<&mut dyn FnMut(&str)>,
) -> Result<usize, String> {
    restore_with(moved, log, &mut |argv| xdg_mime_ok(argv))
}

/// subprocess.run(..., capture_output=True, timeout=20, check=True): a
/// missing tool, a timeout (pipes included) and a non-zero exit all raise.
fn xdg_mime_ok(argv: &[&str]) -> bool {
    matches!(run_bytes(argv, 20), Some((Some(0), _, _)))
}

/// `restore` with the command runner passed in (the Python tests patch
/// `subprocess.run`; the port's tests pass a fake here).
fn restore_with(
    moved: &[(String, String, String)],
    log: Option<&mut dyn FnMut(&str)>,
    run: &mut dyn FnMut(&[&str]) -> bool,
) -> Result<usize, String> {
    let mut print = |s: &str| println!("{s}");
    let log: &mut dyn FnMut(&str) = match log {
        Some(l) => l,
        None => &mut print,
    };
    let mut n = 0;
    for (t, old, _new) in moved {
        if old.is_empty() {
            continue; // nothing to give it back to
        }
        let argv = ["xdg-mime", "default", old.as_str(), t.as_str()];
        // subprocess refuses a NUL in any argument before it starts anything,
        // with a ValueError that is not one of the exceptions caught here.
        if argv.iter().any(|a| a.contains('\0')) {
            return Err("embedded null byte".to_string());
        }
        if run(&argv) {
            n += 1;
        } else {
            log(&format!("could not restore {t}"));
        }
    }
    Ok(n)
}

// --- `mimetypes.guess_extension(type, strict=False)` ------------------------
//
// The Python standard library's table, as `mimetypes._types_map_default` and
// `_common_types_default` hold it (Python 3.14), followed by the system's
// mime.types files, read in the same order with the same parser.

/// `mimetypes.knownfiles`.
const KNOWNFILES: &[&str] = &[
    "/etc/mime.types",
    "/etc/httpd/mime.types",      // Mac OS X
    "/etc/httpd/conf/mime.types", // Apache
    "/etc/apache/mime.types",     // Apache 1
    "/etc/apache2/mime.types",    // Apache 2
    "/usr/local/etc/httpd/conf/mime.types",
    "/usr/local/lib/netscape/mime.types",
    "/usr/local/etc/httpd/conf/mime.types", // Apache 1.2
    "/usr/local/etc/mime.types",            // Apache 1.3
];

const TYPES_MAP_DEFAULT: &[(&str, &str)] = &[
    (".js", "text/javascript"),
    (".mjs", "text/javascript"),
    (".epub", "application/epub+zip"),
    (".gz", "application/gzip"),
    (".json", "application/json"),
    (".webmanifest", "application/manifest+json"),
    (".doc", "application/msword"),
    (".dot", "application/msword"),
    (".wiz", "application/msword"),
    (".nq", "application/n-quads"),
    (".nt", "application/n-triples"),
    (".bin", "application/octet-stream"),
    (".a", "application/octet-stream"),
    (".dll", "application/octet-stream"),
    (".exe", "application/octet-stream"),
    (".o", "application/octet-stream"),
    (".obj", "application/octet-stream"),
    (".so", "application/octet-stream"),
    (".oda", "application/oda"),
    (".ogx", "application/ogg"),
    (".pdf", "application/pdf"),
    (".p7c", "application/pkcs7-mime"),
    (".ps", "application/postscript"),
    (".ai", "application/postscript"),
    (".eps", "application/postscript"),
    (".trig", "application/trig"),
    (".m3u", "application/vnd.apple.mpegurl"),
    (".m3u8", "application/vnd.apple.mpegurl"),
    (".xls", "application/vnd.ms-excel"),
    (".xlb", "application/vnd.ms-excel"),
    (".eot", "application/vnd.ms-fontobject"),
    (".ppt", "application/vnd.ms-powerpoint"),
    (".pot", "application/vnd.ms-powerpoint"),
    (".ppa", "application/vnd.ms-powerpoint"),
    (".pps", "application/vnd.ms-powerpoint"),
    (".pwz", "application/vnd.ms-powerpoint"),
    (".odg", "application/vnd.oasis.opendocument.graphics"),
    (".odp", "application/vnd.oasis.opendocument.presentation"),
    (".ods", "application/vnd.oasis.opendocument.spreadsheet"),
    (".odt", "application/vnd.oasis.opendocument.text"),
    (
        ".pptx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    ),
    (
        ".xlsx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    ),
    (
        ".docx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    ),
    (".rar", "application/vnd.rar"),
    (".wasm", "application/wasm"),
    (".7z", "application/x-7z-compressed"),
    (".bcpio", "application/x-bcpio"),
    (".cpio", "application/x-cpio"),
    (".csh", "application/x-csh"),
    (".deb", "application/x-debian-package"),
    (".dvi", "application/x-dvi"),
    (".gtar", "application/x-gtar"),
    (".hdf", "application/x-hdf"),
    (".h5", "application/x-hdf5"),
    (".latex", "application/x-latex"),
    (".mif", "application/x-mif"),
    (".cdf", "application/x-netcdf"),
    (".nc", "application/x-netcdf"),
    (".p12", "application/x-pkcs12"),
    (".php", "application/x-httpd-php"),
    (".pfx", "application/x-pkcs12"),
    (".ram", "application/x-pn-realaudio"),
    (".pyc", "application/x-python-code"),
    (".pyo", "application/x-python-code"),
    (".rpm", "application/x-rpm"),
    (".sh", "application/x-sh"),
    (".shar", "application/x-shar"),
    (".swf", "application/x-shockwave-flash"),
    (".sv4cpio", "application/x-sv4cpio"),
    (".sv4crc", "application/x-sv4crc"),
    (".tar", "application/x-tar"),
    (".tcl", "application/x-tcl"),
    (".tex", "application/x-tex"),
    (".texi", "application/x-texinfo"),
    (".texinfo", "application/x-texinfo"),
    (".roff", "application/x-troff"),
    (".t", "application/x-troff"),
    (".tr", "application/x-troff"),
    (".man", "application/x-troff-man"),
    (".me", "application/x-troff-me"),
    (".ms", "application/x-troff-ms"),
    (".ustar", "application/x-ustar"),
    (".src", "application/x-wais-source"),
    (".xsl", "application/xml"),
    (".rdf", "application/xml"),
    (".wsdl", "application/xml"),
    (".xpdl", "application/xml"),
    (".yaml", "application/yaml"),
    (".yml", "application/yaml"),
    (".zip", "application/zip"),
    (".3gp", "audio/3gpp"),
    (".3gpp", "audio/3gpp"),
    (".3g2", "audio/3gpp2"),
    (".3gpp2", "audio/3gpp2"),
    (".aac", "audio/aac"),
    (".adts", "audio/aac"),
    (".loas", "audio/aac"),
    (".ass", "audio/aac"),
    (".au", "audio/basic"),
    (".snd", "audio/basic"),
    (".flac", "audio/flac"),
    (".mka", "audio/matroska"),
    (".m4a", "audio/mp4"),
    (".mp3", "audio/mpeg"),
    (".mp2", "audio/mpeg"),
    (".ogg", "audio/ogg"),
    (".opus", "audio/opus"),
    (".aif", "audio/x-aiff"),
    (".aifc", "audio/x-aiff"),
    (".aiff", "audio/x-aiff"),
    (".ra", "audio/x-pn-realaudio"),
    (".wav", "audio/vnd.wave"),
    (".otf", "font/otf"),
    (".ttf", "font/ttf"),
    (".weba", "audio/webm"),
    (".woff", "font/woff"),
    (".woff2", "font/woff2"),
    (".avif", "image/avif"),
    (".bmp", "image/bmp"),
    (".emf", "image/emf"),
    (".fits", "image/fits"),
    (".g3", "image/g3fax"),
    (".gif", "image/gif"),
    (".ief", "image/ief"),
    (".jp2", "image/jp2"),
    (".jpg", "image/jpeg"),
    (".jpe", "image/jpeg"),
    (".jpeg", "image/jpeg"),
    (".jpm", "image/jpm"),
    (".jpx", "image/jpx"),
    (".heic", "image/heic"),
    (".heif", "image/heif"),
    (".png", "image/png"),
    (".svg", "image/svg+xml"),
    (".t38", "image/t38"),
    (".tiff", "image/tiff"),
    (".tif", "image/tiff"),
    (".tfx", "image/tiff-fx"),
    (".ico", "image/vnd.microsoft.icon"),
    (".webp", "image/webp"),
    (".wmf", "image/wmf"),
    (".ras", "image/x-cmu-raster"),
    (".pnm", "image/x-portable-anymap"),
    (".pbm", "image/x-portable-bitmap"),
    (".pgm", "image/x-portable-graymap"),
    (".ppm", "image/x-portable-pixmap"),
    (".rgb", "image/x-rgb"),
    (".xbm", "image/x-xbitmap"),
    (".xpm", "image/x-xpixmap"),
    (".xwd", "image/x-xwindowdump"),
    (".eml", "message/rfc822"),
    (".mht", "message/rfc822"),
    (".mhtml", "message/rfc822"),
    (".nws", "message/rfc822"),
    (".gltf", "model/gltf+json"),
    (".glb", "model/gltf-binary"),
    (".stl", "model/stl"),
    (".css", "text/css"),
    (".csv", "text/csv"),
    (".html", "text/html"),
    (".htm", "text/html"),
    (".md", "text/markdown"),
    (".markdown", "text/markdown"),
    (".n3", "text/n3"),
    (".txt", "text/plain"),
    (".bat", "text/plain"),
    (".c", "text/plain"),
    (".h", "text/plain"),
    (".ksh", "text/plain"),
    (".pl", "text/plain"),
    (".srt", "text/plain"),
    (".rtx", "text/richtext"),
    (".rtf", "text/rtf"),
    (".tsv", "text/tab-separated-values"),
    (".vtt", "text/vtt"),
    (".py", "text/x-python"),
    (".rst", "text/x-rst"),
    (".etx", "text/x-setext"),
    (".sgm", "text/x-sgml"),
    (".sgml", "text/x-sgml"),
    (".vcf", "text/x-vcard"),
    (".xml", "text/xml"),
    (".mkv", "video/matroska"),
    (".mk3d", "video/matroska-3d"),
    (".mp4", "video/mp4"),
    (".mpeg", "video/mpeg"),
    (".m1v", "video/mpeg"),
    (".mpa", "video/mpeg"),
    (".mpe", "video/mpeg"),
    (".mpg", "video/mpeg"),
    (".ogv", "video/ogg"),
    (".mov", "video/quicktime"),
    (".qt", "video/quicktime"),
    (".webm", "video/webm"),
    (".avi", "video/vnd.avi"),
    (".m4v", "video/x-m4v"),
    (".wmv", "video/x-ms-wmv"),
    (".movie", "video/x-sgi-movie"),
];
const COMMON_TYPES_DEFAULT: &[(&str, &str)] = &[
    (".rtf", "application/rtf"),
    (".apk", "application/vnd.android.package-archive"),
    (".midi", "audio/midi"),
    (".mid", "audio/midi"),
    (".jpg", "image/jpg"),
    (".pict", "image/pict"),
    (".pct", "image/pict"),
    (".pic", "image/pict"),
    (".xul", "text/xul"),
];

/// `MimeTypes.types_map_inv`: (strict, non-strict) {type: [ext, ...]} in
/// insertion order.
#[derive(Default)]
struct MimeDb {
    inv: [HashMap<String, Vec<String>>; 2],
}

impl MimeDb {
    fn add_type(&mut self, type_: &str, ext: &str, strict: bool) {
        if type_.is_empty() {
            return;
        }
        let exts = self.inv[strict as usize]
            .entry(type_.to_string())
            .or_default();
        if !exts.iter().any(|e| e == ext) {
            exts.push(ext.to_string());
        }
    }

    /// `MimeTypes.readfp`, over `read`'s text: universal newlines (`\n`,
    /// `\r\n` and a bare `\r` all end a line), words split as `str.split()`
    /// does.
    fn readfp(&mut self, text: &str, strict: bool) {
        for line in text.split("\r\n").flat_map(|l| l.split(['\r', '\n'])) {
            let mut words: Vec<&str> = line.split(py_isspace).filter(|w| !w.is_empty()).collect();
            if let Some(i) = words.iter().position(|w| w.starts_with('#')) {
                words.truncate(i);
            }
            let Some((type_, suffixes)) = words.split_first() else {
                continue;
            };
            // A type holding an undecodable byte (a lone surrogate in Python)
            // can never equal a type asked about; nothing to record.
            if type_.chars().any(is_escaped_byte) {
                continue;
            }
            for suff in suffixes {
                self.add_type(type_, &format!(".{suff}"), strict);
            }
        }
    }
}

/// `str.isspace()` for one character: Rust's White_Space plus the
/// information separators U+001C..U+001F, which Python also splits on.
fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `read` opens the file with `errors='surrogateescape'`: an undecodable byte
/// becomes a lone surrogate rather than an error. Rust strings cannot hold
/// those, so each such byte b is kept as U+10FF00 + b (private use, never
/// whitespace, unique per byte) and shown as U+FFFD when returned.
const ESCAPE_BASE: u32 = 0x10FF00;

fn is_escaped_byte(c: char) -> bool {
    (ESCAPE_BASE + 0x80..=ESCAPE_BASE + 0xFF).contains(&(c as u32))
}

fn decode_surrogateescape(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for chunk in bytes.utf8_chunks() {
        out.push_str(chunk.valid());
        for &b in chunk.invalid() {
            out.push(char::from_u32(ESCAPE_BASE + u32::from(b)).unwrap_or('\u{fffd}'));
        }
    }
    out
}

/// `mimetypes.init()`'s database. A file in `knownfiles` that exists but
/// cannot be read raises out of `init` (an OSError), leaving `_db` unset, so
/// the next lookup tries again: only a database that loaded is kept.
fn mime_db() -> Result<&'static MimeDb, String> {
    static DB: OnceLock<MimeDb> = OnceLock::new();
    if let Some(db) = DB.get() {
        return Ok(db);
    }
    let mut db = MimeDb::default();
    for (ext, type_) in TYPES_MAP_DEFAULT {
        db.add_type(type_, ext, true);
    }
    for (ext, type_) in COMMON_TYPES_DEFAULT {
        db.add_type(type_, ext, false);
    }
    for file in KNOWNFILES {
        let p = std::path::Path::new(file);
        if p.is_file() {
            let bytes = std::fs::read(p).map_err(|e| os_error(&e, file))?;
            db.readfp(&decode_surrogateescape(&bytes), true);
        }
    }
    Ok(DB.get_or_init(|| db))
}

/// `str(OSError)` for a failed `open(filename)`: "[Errno 13] Permission
/// denied: '/etc/mime.types'".
fn os_error(e: &std::io::Error, filename: &str) -> String {
    match e.raw_os_error() {
        Some(code) => {
            let msg = std::io::Error::from_raw_os_error(code).to_string();
            let msg = msg
                .strip_suffix(&format!(" (os error {code})"))
                .unwrap_or(&msg)
                .to_string();
            format!("[Errno {code}] {msg}: '{filename}'")
        }
        None => e.to_string(),
    }
}

/// `mimetypes.guess_extension`; `Err` is the OSError `init` raised.
fn guess_extension(type_: &str, strict: bool) -> Result<Option<String>, String> {
    let db = mime_db()?;
    let type_ = type_.to_lowercase();
    let mut extensions: Vec<String> = db.inv[1].get(&type_).cloned().unwrap_or_default();
    if !strict {
        for ext in db.inv[0].get(&type_).into_iter().flatten() {
            if !extensions.contains(ext) {
                extensions.push(ext.clone());
            }
        }
    }
    Ok(extensions.into_iter().next().map(|e| {
        e.chars()
            .map(|c| if is_escaped_byte(c) { '\u{fffd}' } else { c })
            .collect()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn m(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn row(t: &str, o: &str, n: &str) -> (String, String, String) {
        (t.to_string(), o.to_string(), n.to_string())
    }

    #[test]
    fn test_a_takeover_is_listed_with_what_it_replaced() {
        let before = m(&[
            ("application/vnd.oasis.opendocument.text", "writer.desktop"),
            ("text/plain", "nvim.desktop"),
        ]);
        let after = m(&[
            (
                "application/vnd.oasis.opendocument.text",
                "wordcraft.desktop",
            ),
            ("text/plain", "nvim.desktop"),
        ]);
        assert_eq!(
            changes(Some(&before), Some(&after)),
            vec![row(
                "application/vnd.oasis.opendocument.text",
                "writer.desktop",
                "wordcraft.desktop"
            )]
        );
    }

    #[test]
    fn readfp_reads_like_the_stdlib() {
        // Universal newlines, str.split() whitespace, surrogateescape.
        let mut db = MimeDb::default();
        let text =
            decode_surrogateescape(b"a/b x\rc/d y\r\ne/f p\x1cq # no\ng/\xff z\nh/i \xffj\n");
        db.readfp(&text, true);
        let get = |t: &str| db.inv[1].get(t).cloned().unwrap_or_default();
        assert_eq!(get("a/b"), vec![".x"]);
        assert_eq!(get("c/d"), vec![".y"]);
        assert_eq!(get("e/f"), vec![".p", ".q"]);
        assert_eq!(db.inv[1].len(), 4);
        assert!(get("h/i")[0].chars().any(is_escaped_byte));
    }

    #[test]
    fn changes_walks_after_in_its_own_order() {
        let before = m(&[("a/a", "o"), ("z/z", "o")]);
        let after: Vec<(String, String)> = [("z/z", "n"), ("m/m", "n"), ("a/a", "n")]
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        let got = changes(Some(&before), Some(after.iter().map(|(a, b)| (a, b))));
        assert_eq!(
            got.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(),
            ["z/z", "m/m", "a/a"]
        );
    }

    #[test]
    fn restore_raises_on_a_nul() {
        let mut log = |_: &str| {};
        let r = restore_with(
            &[row("text/x", "a\0b.desktop", "wc.desktop")],
            Some(&mut log),
            &mut |_| true,
        );
        assert_eq!(r, Err("embedded null byte".to_string()));
    }

    #[test]
    fn changes_carries_a_none_old_id_through() {
        let before: BTreeMap<String, serde_json::Value> =
            [("a/b".to_string(), serde_json::Value::Null)]
                .into_iter()
                .collect();
        let after = m(&[("a/b", "x")]);
        assert_eq!(
            changes(Some(&before), Some(&after)),
            vec![("a/b".to_string(), serde_json::Value::Null, "x".to_string())]
        );
    }

    #[test]
    fn run_bytes_times_out_on_a_pipe_held_by_a_descendant() {
        // The child exits at once; its background sleep keeps stdout open.
        let t = Instant::now();
        let r = run_bytes_within(
            &["/bin/sh", "-c", "/usr/bin/sleep 3 & exit 0"],
            Duration::from_millis(200),
        );
        assert_eq!(r, None);
        assert!(t.elapsed() < Duration::from_millis(1500));
        assert_eq!(
            run_bytes(&["/bin/sh", "-c", "echo hi; exit 3"], 5),
            Some((Some(3), b"hi\n".to_vec(), Vec::new()))
        );
    }

    #[test]
    fn test_no_snapshot_means_no_claim() {
        assert_eq!(
            changes(None::<&BTreeMap<String, String>>, Some(&m(&[("a/b", "x")]))),
            vec![]
        );
    }

    #[test]
    fn test_restoring_uses_the_standard_tool() {
        let mut calls: Vec<Vec<String>> = Vec::new();
        let mut log = |_: &str| {};
        let n = restore_with(
            &[
                row("text/rtf", "writer.desktop", "wc.desktop"),
                row("text/x-new", "", "wc.desktop"),
            ],
            Some(&mut log),
            &mut |argv| {
                calls.push(argv.iter().map(|s| s.to_string()).collect());
                true
            },
        )
        .unwrap();
        assert_eq!(n, 1);
        assert_eq!(
            calls.last().unwrap(),
            &vec!["xdg-mime", "default", "writer.desktop", "text/rtf"]
        );
    }
}
