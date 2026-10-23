//! Port of `maintenance.py`.
//!
//! System maintenance checks.
//!
//! Each check answers one question with a number, and offers the one command that
//! acts on it. That shape is deliberate: "your package cache is 4.5 GB across 528
//! files" is actionable, "run paccache occasionally" is not.
//!
//! Three rules the checks follow:
//!
//! * Never report a finding without saying what would be done about it. A warning
//!   with no fix is just anxiety.
//! * Never propose a fix that needs judgement the check has not done. Orphan
//!   removal keeps build tooling; cache trimming keeps the last two versions so a
//!   downgrade is still possible.
//! * Reading is free, acting is not. Every check here is read-only; the fixes run
//!   only when asked, through the same confirm-then-run path as everything else.


use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// `Path.home()`, evaluated once at import in the Python.
pub static HOME: LazyLock<PathBuf> = LazyLock::new(crate::util::home);
pub const PACMAN_CACHE: &str = "/var/cache/pacman/pkg";
/// `xdg.cache_home() / "yay"`, evaluated once at import in the Python.
pub static YAY_CACHE: LazyLock<PathBuf> = LazyLock::new(|| crate::xdg::cache_home().join("yay"));
/// `xdg.data_home() / "Trash"`, evaluated once at import in the Python.
pub static TRASH: LazyLock<PathBuf> = LazyLock::new(|| crate::xdg::data_home().join("Trash"));

/// Where Almanac keeps AppImages -- beside its registry, in XDG state.
fn _appimage_store() -> PathBuf {
    let reg = crate::inventory::registry();
    reg.parent().map(Path::to_path_buf).unwrap_or_default().join("apps")
}
pub const OK: &str = "ok";
pub const INFO: &str = "info";
pub const WARN: &str = "warn";
pub const CRIT: &str = "crit";

/// The log function a callable fix (and a `tasks` step) is handed: one line
/// of output per call.
pub type LogFn<'a> = &'a (dyn Fn(&str) + Send + Sync);

/// A callable fix: takes the log function, returns `Err` where the Python
/// would raise to fail the step.
pub type FixFn = Arc<dyn Fn(LogFn<'_>) -> Result<(), String> + Send + Sync>;

/// `Finding.fix`: argv to run, or a callable taking a log function. (Python
/// stores either in one `object` field; `None` is `Option::None` on the
/// field.)
#[derive(Clone)]
pub enum Fix {
    Argv(Vec<String>),
    Func(FixFn),
}

impl Fix {
    /// Build an argv fix from string literals.
    pub fn argv(args: &[&str]) -> Fix {
        Fix::Argv(args.iter().map(|s| s.to_string()).collect())
    }

    /// Wrap a closure as a callable fix.
    pub fn func<F>(f: F) -> Fix
    where
        F: Fn(LogFn<'_>) -> Result<(), String> + Send + Sync + 'static,
    {
        Fix::Func(Arc::new(f))
    }
}

impl fmt::Debug for Fix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Fix::Argv(a) => f.debug_tuple("Argv").field(a).finish(),
            Fix::Func(_) => f.write_str("Func(<callable>)"),
        }
    }
}

impl PartialEq for Fix {
    fn eq(&self, other: &Fix) -> bool {
        match (self, other) {
            (Fix::Argv(a), Fix::Argv(b)) => a == b,
            (Fix::Func(a), Fix::Func(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

/// An argv serialises as its list; a callable as the string `"<callable>"`.
impl Serialize for Fix {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Fix::Argv(a) => a.serialize(s),
            Fix::Func(_) => s.serialize_str("<callable>"),
        }
    }
}

/// A list deserialises as an argv; anything else (a serialised callable)
/// becomes a callable that does nothing.
impl<'de> Deserialize<'de> for Fix {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Fix, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        match v {
            serde_json::Value::Array(items) => Ok(Fix::Argv(
                items
                    .into_iter()
                    .map(|i| match i {
                        serde_json::Value::String(s) => s,
                        other => other.to_string(),
                    })
                    .collect(),
            )),
            _ => Ok(Fix::func(|_log| Ok(()))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Finding {
    pub id: String,
    pub title: String,
    pub detail: String,
    pub severity: String,
    pub count: i64,
    pub size_mb: f64,
    pub fix_label: String,
    /// argv to run, or a callable taking a log function. Empty means there is
    /// nothing to do but look.
    pub fix: Option<Fix>,
    /// what the fix actually does, for the dialog
    pub fix_note: String,
    pub items: Vec<String>,
    pub url: String,
}

impl Default for Finding {
    fn default() -> Finding {
        Finding {
            id: String::new(),
            title: String::new(),
            detail: String::new(),
            severity: INFO.to_string(),
            count: 0,
            size_mb: 0.0,
            fix_label: String::new(),
            fix: None,
            fix_note: String::new(),
            items: Vec::new(),
            url: String::new(),
        }
    }
}

impl Finding {
    pub fn actionable(&self) -> bool {
        self.fix.is_some() && self.severity != OK
    }
}

fn _run(cmd: &[&str], timeout: u64) -> Result<String, String> {
    // subprocess.run(..., text=True).stdout, or "" on OSError /
    // TimeoutExpired. Output that is not UTF-8 raises UnicodeDecodeError,
    // which the except clause does not catch: the check fails.
    match _run_bytes(cmd, timeout) {
        Some((out, err)) => {
            let out = _py_decode(&out)?;
            _py_decode(&err)?;
            Ok(out)
        }
        None => Ok(String::new()),
    }
}

/// The `_run` a check is handed, so tests and parity can stand in for the
/// commands the way test_app.py patches `maintenance._run`. Err is the
/// exception `_run` lets through (a UnicodeDecodeError).
type RunFn<'a> = &'a dyn Fn(&[&str], u64) -> Result<String, String>;

/// (megabytes, file count). Walks rather than shelling out to du.
fn _du_mb(path: &Path) -> (f64, i64) {
    let mut total: u64 = 0;
    let mut count: i64 = 0;
    if !path.exists() {
        return (0.0, 0);
    }
    // os.walk(path, onerror=lambda _e: None): unreadable directories are
    // skipped; symlinks to directories are listed as directories and not
    // followed; every non-directory entry is lstat'ed.
    fn walk(dir: &Path, total: &mut u64, count: &mut i64) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let p = entry.path();
            let is_dir = std::fs::metadata(&p).map(|m| m.is_dir()).unwrap_or(false);
            if is_dir {
                let is_link = entry.file_type().map(|t| t.is_symlink()).unwrap_or(false);
                if !is_link {
                    walk(&p, total, count);
                }
                continue;
            }
            if let Ok(m) = std::fs::symlink_metadata(&p) {
                *total += m.len();
                *count += 1;
            }
        }
    }
    walk(path, &mut total, &mut count);
    (total as f64 / 1048576.0, count)
}

/// Python's `f"{x:.{prec}f}"`, including its spelling of inf and nan.
fn _ff(x: f64, prec: usize) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    format!("{x:.prec$}")
}

pub fn fmt_size(mb: f64) -> String {
    if mb >= 1024.0 {
        return format!("{} GB", _ff(mb / 1024.0, 1));
    }
    if mb >= 1.0 {
        return format!("{} MB", _ff(mb, 0));
    }
    format!("{} KB", _ff(mb * 1024.0, 0))
}

// ---- Python string/OS helpers -----------------------------------------------

/// `str.isspace()` for one character: Unicode White_Space plus the four
/// information separators Python also counts.
fn _py_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// `str.split()` with no argument.
fn _py_split(s: &str) -> Vec<&str> {
    s.split(_py_space).filter(|p| !p.is_empty()).collect()
}

/// `str.strip()` with no argument.
fn _py_strip(s: &str) -> &str {
    s.trim_matches(_py_space)
}

/// `str.splitlines()`: every line boundary Python knows, no trailing empty.
fn _py_splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        let brk = matches!(
            c,
            '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}'
        );
        if !brk {
            continue;
        }
        out.push(&s[start..i]);
        let mut next = i + c.len_utf8();
        if c == '\r' {
            if let Some(&(j, '\n')) = it.peek() {
                it.next();
                next = j + 1;
            }
        }
        start = next;
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// Characters `str.isprintable()` rejects (other than the ones `repr`
/// spells as `\t`, `\n`, `\r`): categories C* and Z* except the space.
static NONPRINTABLE_RE: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"^[\p{C}\p{Z}]$").unwrap());

/// `repr()` of a Python str holding these bytes as `os.fsdecode` reads them:
/// undecodable bytes become the surrogates `\udcXX`.
fn _py_repr_bytes(b: &[u8]) -> String {
    enum U {
        C(char),
        Byte(u8),
    }
    let mut units: Vec<U> = Vec::new();
    let mut rest = b;
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(s) => {
                units.extend(s.chars().map(U::C));
                break;
            }
            Err(e) => {
                let good = e.valid_up_to();
                if let Ok(s) = std::str::from_utf8(&rest[..good]) {
                    units.extend(s.chars().map(U::C));
                }
                let bad = e.error_len().unwrap_or(rest.len() - good);
                units.extend(rest[good..good + bad].iter().map(|x| U::Byte(*x)));
                rest = &rest[good + bad..];
            }
        }
    }
    let has = |q: char| units.iter().any(|u| matches!(u, U::C(c) if *c == q));
    let quote = if has('\'') && !has('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(quote);
    for u in &units {
        match u {
            U::Byte(x) => out.push_str(&format!("\\udc{x:02x}")),
            U::C(c) => match *c {
                '\\' => out.push_str("\\\\"),
                '\t' => out.push_str("\\t"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                c if c == quote => {
                    out.push('\\');
                    out.push(c);
                }
                ' ' => out.push(' '),
                c if NONPRINTABLE_RE.is_match(c.encode_utf8(&mut [0; 4])) => {
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
            },
        }
    }
    out.push(quote);
    out
}

/// `repr()` of a path's text.
fn _py_repr_path(p: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    _py_repr_bytes(p.as_os_str().as_bytes())
}

/// `str(OSError)` from an errno and the already-`repr`ed filename:
/// `[Errno 13] Permission denied: '/path'`.
fn _oserror_repr(e: &std::io::Error, filename_repr: &str) -> String {
    match e.raw_os_error() {
        Some(code) => {
            let msg = e.to_string();
            let strerror = msg.strip_suffix(&format!(" (os error {code})")).unwrap_or(&msg).to_string();
            format!("[Errno {code}] {strerror}: {filename_repr}")
        }
        None => e.to_string(),
    }
}

/// `str(OSError)` for a pathlib call, whose filename is the path as a str.
fn _oserror(e: &std::io::Error, path: &Path) -> String {
    _oserror_repr(e, &_py_repr_path(path))
}

/// `str(Path(s))`: pathlib drops `.` components, repeated and trailing
/// slashes (a leading `//`, exactly two, is kept); `..` stays.
fn _pathlib_str(s: &str) -> String {
    let root = if s.starts_with("//") && !s.starts_with("///") {
        "//"
    } else if s.starts_with('/') {
        "/"
    } else {
        ""
    };
    let parts: Vec<&str> = s.split('/').filter(|p| !p.is_empty() && *p != ".").collect();
    let joined = format!("{root}{}", parts.join("/"));
    if joined.is_empty() {
        ".".into()
    } else {
        joined
    }
}

/// `shutil.rmtree(path, ignore_errors=...)` as Python 3.14 runs it (the
/// fd-based walk): a symlink -- to a directory or anything else -- is refused
/// rather than removed, directories are emptied before the subdirectories
/// on the stack are entered, and an error carries the path of the directory
/// being worked on (the top one as the `PosixPath` it was given). With
/// `ignore_errors` every failure is skipped and the walk goes on; otherwise
/// the first one is returned as `str(exc)`.
fn _rmtree(top: &Path, ignore_errors: bool) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    enum Op {
        Lstat,
        Rmdir,
    }
    // Some(err) is an OSError from the OS; None is OSError("Cannot call
    // rmtree on a symbolic link"), whose errno and strerror are None.
    type StepErr = Option<std::io::Error>;
    let top = PathBuf::from(_pathlib_str(&top.to_string_lossy()));
    let mut stack: Vec<(Op, PathBuf, bool)> = vec![(Op::Lstat, top, true)];
    while let Some((op, path, is_top)) = stack.pop() {
        let fname = if is_top { format!("PosixPath({})", _py_repr_path(&path)) } else { _py_repr_path(&path) };
        let report = |e: StepErr| -> Result<(), String> {
            if let Some(io) = &e {
                // except FileNotFoundError: only the top level reports it.
                if io.kind() == std::io::ErrorKind::NotFound && !is_top {
                    return Ok(());
                }
            }
            if ignore_errors {
                return Ok(());
            }
            Err(match e {
                Some(io) => _oserror_repr(&io, &fname),
                None => format!("[Errno None] None: {fname}"),
            })
        };
        let mut step = || -> Result<(), StepErr> {
            match op {
                Op::Rmdir => std::fs::remove_dir(&path).map_err(Some),
                Op::Lstat => {
                    let orig = std::fs::symlink_metadata(&path).map_err(Some)?;
                    let flags = rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NONBLOCK | rustix::fs::OFlags::CLOEXEC;
                    let fd = rustix::fs::open(&path, flags, rustix::fs::Mode::empty())
                        .map_err(|e| Some(std::io::Error::from(e)))?;
                    let st = rustix::fs::fstat(&fd).map_err(|e| Some(std::io::Error::from(e)))?;
                    if st.st_dev as u64 != orig.dev() || st.st_ino as u64 != orig.ino() {
                        // Symlinks to directories are forbidden, see GH-46010.
                        return Err(None);
                    }
                    stack.push((Op::Rmdir, path.clone(), is_top));
                    let entries = std::fs::read_dir(&path)
                        .and_then(|it| it.collect::<Result<Vec<_>, _>>())
                        .map_err(Some)?;
                    drop(fd);
                    for entry in entries {
                        let fullname = path.join(entry.file_name());
                        match entry.file_type() {
                            Ok(t) if t.is_dir() => {
                                // Traverse into sub-directory.
                                stack.push((Op::Lstat, fullname, false));
                                continue;
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                            _ => {}
                        }
                        match std::fs::remove_file(&fullname) {
                            Ok(()) => {}
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                            // onexc: raised to the caller, or skipped.
                            Err(e) if !ignore_errors => return Err(Some(e)),
                            Err(_) => {}
                        }
                    }
                    Ok(())
                }
            }
        };
        if let Err(e) = step() {
            report(e)?;
        }
    }
    Ok(())
}

/// A Python str read from these bytes with `os.fsdecode`, as text: bytes
/// that are not UTF-8 are lone surrogates there (surrogateescape), which a
/// Rust String cannot hold, so each is written out as `\udcXX` -- the way
/// `repr()` shows it. Only for showing and logging: anything acted on keeps
/// the bytes (see `_dangling_rows`).
fn _pystr(b: &[u8]) -> String {
    let mut out = String::new();
    for chunk in b.utf8_chunks() {
        out.push_str(chunk.valid());
        for x in chunk.invalid() {
            out.push_str(&format!("\\udc{x:02x}"));
        }
    }
    out
}

/// The code points of `os.fsdecode(b)`, undecodable bytes as the surrogates
/// U+DC80..U+DCFF: what Python compares and sorts names by.
fn _py_codepoints(b: &[u8]) -> Vec<u32> {
    let mut out = Vec::with_capacity(b.len());
    for chunk in b.utf8_chunks() {
        out.extend(chunk.valid().chars().map(u32::from));
        out.extend(chunk.invalid().iter().map(|&x| 0xDC00 + u32::from(x)));
    }
    out
}

/// `p.name` as bytes.
fn _name_bytes(p: &Path) -> &[u8] {
    use std::os::unix::ffi::OsStrExt;
    p.file_name().map(|n| n.as_bytes()).unwrap_or_default()
}

/// `p.name` as text (see `_pystr`).
fn _name(p: &Path) -> String {
    _pystr(_name_bytes(p))
}

/// The key `sorted()` orders paths in one directory by: their names' code
/// points (a surrogate-escaped byte sorts below U+E000, as in Python).
fn _sort_key(p: &Path) -> Vec<u32> {
    _py_codepoints(_name_bytes(p))
}

/// `str(p)` as text (see `_pystr`).
fn _path_str(p: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    _pystr(p.as_os_str().as_bytes())
}

/// `sorted(d.glob(pattern))` for a `*<suffix>` pattern (pathlib matches
/// hidden names too). A missing directory yields nothing.
fn _glob_suffix(d: &Path, suffix: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = match std::fs::read_dir(d) {
        Ok(entries) => entries.flatten().map(|e| e.path()).filter(|p| _name_bytes(p).ends_with(suffix.as_bytes())).collect(),
        Err(_) => Vec::new(),
    };
    out.sort_by_key(|p| _sort_key(p));
    out
}

/// `d.glob("*<suffix>")` in directory order, unsorted.
fn _glob_suffix_unsorted(d: &Path, suffix: &str) -> Vec<PathBuf> {
    match std::fs::read_dir(d) {
        Ok(entries) => entries.flatten().map(|e| e.path()).filter(|p| _name_bytes(p).ends_with(suffix.as_bytes())).collect(),
        Err(_) => Vec::new(),
    }
}

/// `list(d.iterdir())`, raising like the Python when the directory cannot
/// be listed.
fn _iterdir(d: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = std::fs::read_dir(d).map_err(|e| _oserror(&e, d))?;
    Ok(entries.flatten().map(|e| e.path()).collect())
}

/// `Path(root).rglob("*<suffix>")`, in the order Python 3.14 yields them:
/// the root's own matches, then each subdirectory's matches as it is met,
/// subdirectories explored last-in first-out; symlinked directories are not
/// followed.
fn _rglob_suffix(root: &Path, suffix: &str) -> Vec<PathBuf> {
    let mut out = _glob_suffix_unsorted(root, suffix);
    let mut stack = vec![root.to_path_buf()];
    while let Some(p) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&p) else { continue };
        for entry in entries.flatten() {
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                let ep = entry.path();
                out.extend(_glob_suffix_unsorted(&ep, suffix));
                stack.push(ep);
            }
        }
    }
    out
}

/// `os.getuid()`.
fn _uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// `os.environ.get("XDG_RUNTIME_DIR") or f"/run/user/{os.getuid()}"`.
fn _runtime_dir() -> String {
    match std::env::var("XDG_RUNTIME_DIR") {
        Ok(v) if !v.is_empty() => v,
        _ => format!("/run/user/{}", _uid()),
    }
}

/// `f"{n:,}"`.
fn _thousands(n: i128) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
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

static ND_RE: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"^\p{Nd}$").unwrap());

/// The value of a Unicode decimal digit (`\d`, category Nd), as Python's
/// `int()` and `float()` read it. Nd digits come in runs of ten from zero.
fn _nd_digit(c: char) -> Option<u32> {
    if c.is_ascii_digit() {
        return c.to_digit(10);
    }
    let is_nd = |c: char| ND_RE.is_match(c.encode_utf8(&mut [0; 4]));
    if !is_nd(c) {
        return None;
    }
    let mut start = c as u32;
    while let Some(prev) = start.checked_sub(1).and_then(char::from_u32) {
        if !is_nd(prev) {
            break;
        }
        start -= 1;
    }
    Some((c as u32 - start) % 10)
}

/// Every Unicode decimal digit as its ASCII digit; nothing else changes.
fn _ascii_digits(s: &str) -> String {
    s.chars().map(|c| _nd_digit(c).and_then(|d| char::from_digit(d, 10)).unwrap_or(c)).collect()
}

/// `float(text)` for what `[\d.]+` matched: Python raises on "." or "1.2.3",
/// and reads any Unicode digits as their ASCII ones.
fn _py_float(text: &str) -> Result<f64, String> {
    _ascii_digits(text)
        .parse::<f64>()
        .map_err(|_| format!("could not convert string to float: {}", _py_repr_bytes(text.as_bytes())))
}

/// `sys.get_int_max_str_digits()`: `int(str)` and `str(int)` refuse longer.
const INT_MAX_STR_DIGITS: usize = 4300;

/// `int(text)` for what `\d+` matched, with no size limit but Python's.
fn _py_int(text: &str) -> Result<num_bigint::BigUint, String> {
    let digits = _ascii_digits(text);
    if digits.len() > INT_MAX_STR_DIGITS {
        return Err(format!(
            "Exceeds the limit ({INT_MAX_STR_DIGITS} digits) for integer string conversion: \
             value has {} digits; use sys.set_int_max_str_digits() to increase the limit",
            digits.len()
        ));
    }
    digits
        .parse::<num_bigint::BigUint>()
        .map_err(|_| format!("invalid literal for int() with base 10: {}", _py_repr_bytes(text.as_bytes())))
}

/// `f"{n:,}"` for an int of any size.
fn _thousands_big(n: &num_bigint::BigUint) -> Result<String, String> {
    let digits = n.to_string();
    if digits.len() > INT_MAX_STR_DIGITS {
        return Err(format!(
            "Exceeds the limit ({INT_MAX_STR_DIGITS} digits) for integer string conversion; \
             use sys.set_int_max_str_digits() to increase the limit"
        ));
    }
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    Ok(out)
}

/// A public check where the Python raises: panic with `str(exc)`, which
/// `run_all` (and `verify.recheck`) catch the way the Python catches the
/// exception -- `run_all` reports "Check failed: ...", `recheck` gives None.
fn _raise<T>(r: Result<T, String>) -> T {
    match r {
        Ok(v) => v,
        Err(exc) => {
            // An exception is not a crash: nothing goes to stderr for it.
            _quiet_panics();
            QUIET.with(|q| q.set(true));
            std::panic::panic_any(exc)
        }
    }
}

thread_local! {
    /// Set while a panic on this thread stands for a Python exception that
    /// will be caught, so the panic hook keeps quiet about it.
    static QUIET: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Wrap the panic hook (once) so a panic raised while `QUIET` is set prints
/// nothing -- Python's `except Exception` prints nothing either. Every other
/// panic still reaches the hook that was there before.
fn _quiet_panics() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if !QUIET.with(|q| q.replace(false)) {
                prev(info);
            }
        }));
    });
}

/// `bytes.decode("utf-8")` as `subprocess.run(..., text=True)` does it:
/// strict, so invalid bytes raise UnicodeDecodeError (with Python's message),
/// then universal newlines turn `\r\n` and `\r` into `\n`.
fn _py_decode(b: &[u8]) -> Result<String, String> {
    let s = match std::str::from_utf8(b) {
        Ok(s) => s,
        Err(e) => {
            let pos = e.valid_up_to();
            let (n, reason) = match e.error_len() {
                Some(n) => {
                    let lead = b[pos];
                    let bad_start = (0x80..=0xC1).contains(&lead) || lead >= 0xF5;
                    (n, if bad_start { "invalid start byte" } else { "invalid continuation byte" })
                }
                None => (b.len() - pos, "unexpected end of data"),
            };
            return Err(if n == 1 {
                format!("'utf-8' codec can't decode byte 0x{:02x} in position {pos}: {reason}", b[pos])
            } else {
                format!("'utf-8' codec can't decode bytes in position {pos}-{}: {reason}", pos + n - 1)
            });
        }
    };
    Ok(s.replace("\r\n", "\n").replace('\r', "\n"))
}

/// `subprocess.run(cmd, capture_output=True, timeout=...)` before decoding:
/// (stdout, stderr) bytes, or None on OSError / TimeoutExpired.
/// (`util::run` decodes lossily, where `text=True` raises.)
fn _run_bytes(cmd: &[&str], timeout: u64) -> Option<(Vec<u8>, Vec<u8>)> {
    run_bytes_within(cmd, std::time::Duration::from_secs(timeout))
}

/// `_run_bytes` with any timeout (the tests use a fraction of a second).
fn run_bytes_within(cmd: &[&str], timeout: std::time::Duration) -> Option<(Vec<u8>, Vec<u8>)> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let (prog, args) = cmd.split_first()?;
    // stdin is inherited, as subprocess.run leaves it.
    let mut child =
        crate::util::spawn_group(Command::new(prog).args(args).stdout(Stdio::piped()).stderr(Stdio::piped())).ok()?;
    // Each pipe is read to its end on a thread that reports when done.
    let drain = |p: Option<Box<dyn Read + Send>>| {
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut s = Vec::new();
            if let Some(mut p) = p {
                let _ = p.read_to_end(&mut s);
            }
            let _ = tx.send(s);
        });
        rx
    };
    let out_rx = drain(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err_rx = drain(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    // communicate(timeout=...): the limit covers reading the output too, so a
    // grandchild that keeps a pipe open after the child exits does not hold
    // this up past it. On timeout the child's whole process group is killed
    // and the child reaped, so a grandchild holding a pipe dies with it and
    // the reader threads end.
    let deadline = std::time::Instant::now() + timeout;
    let (mut out, mut err): (Option<Vec<u8>>, Option<Vec<u8>>) = (None, None);
    let mut exited = false;
    loop {
        if out.is_none() {
            out = out_rx.try_recv().ok();
        }
        if err.is_none() {
            err = err_rx.try_recv().ok();
        }
        if !exited {
            match child.try_wait() {
                Ok(Some(_)) => exited = true,
                Ok(None) => {}
                Err(_) => return None,
            }
        }
        if exited && out.is_some() && err.is_some() {
            break;
        }
        if std::time::Instant::now() >= deadline {
            crate::util::kill_group(&mut child);
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    Some((out.unwrap_or_default(), err.unwrap_or_default()))
}

/// `shutil.which(cmd)` for a bare name: PATH (or, when unset,
/// `os.confstr("CS_PATH")`, which is this on glibc), each entry checked with
/// `os.access(X_OK)` -- may *this user* run it -- and not a directory.
fn _which(cmd: &str) -> Option<PathBuf> {
    let path = match std::env::var_os("PATH") {
        Some(p) => p,
        None => std::ffi::OsString::from("/bin:/usr/bin"),
    };
    if path.is_empty() {
        return None;
    }
    use std::os::unix::ffi::OsStrExt;
    let mut seen: Vec<Vec<u8>> = Vec::new();
    for dir in path.as_bytes().split(|b| *b == b':') {
        if seen.iter().any(|s| s == dir) {
            continue;
        }
        seen.push(dir.to_vec());
        let d = Path::new(std::ffi::OsStr::from_bytes(dir));
        // os.path.join("", cmd) is cmd itself, relative to the cwd.
        let name = if dir.is_empty() { PathBuf::from(cmd) } else { d.join(cmd) };
        let ok = name.exists()
            && rustix::fs::access(&name, rustix::fs::Access::EXEC_OK).is_ok()
            && !name.is_dir();
        if ok {
            return Some(name);
        }
    }
    None
}

/// The finding `run_all` builds when a check raises.
fn _failed(name: &str, exc: &str) -> Finding {
    Finding {
        id: name.to_string(),
        title: crate::util::py_title(&name.replace('_', " ")),
        detail: format!("Check failed: {exc}"),
        severity: INFO.to_string(),
        ..Default::default()
    }
}
// ---- checks ------------------------------------------------------------------

/// `_SAVED = re.compile(r"disk space saved:\s*([\d.]+)\s*([KMGT]?)i?B")`
#[allow(dead_code)]
const _SAVED: &str = r"disk space saved:\s*([\d.]+)\s*([KMGT]?)i?B";
/// What `_SAVED` means in Python, for the regex crate: Python's `\s` is
/// `str.isspace()`, which also takes the information separators \x1c-\x1f
/// that Unicode's White_Space (the regex crate's `\s`) leaves out.
const _SAVED_RS: &str = r"disk space saved:[\s\x1c-\x1f]*([\d.]+)[\s\x1c-\x1f]*([KMGT]?)i?B";

static SAVED_RE: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(_SAVED_RS).unwrap());
static JOURNAL_RE: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"([\d.]+)([KMGT])").unwrap());
static TOTAL_FILES_RE: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"(\d+) total files").unwrap());

fn _paccache_reclaimable_with(flags: &[&str], run: RunFn<'_>) -> Result<f64, String> {
    let mut cmd = vec!["paccache", "-d"];
    cmd.extend_from_slice(flags);
    let out = run(&cmd, 60)?;
    let Some(m) = SAVED_RE.captures(&out) else {
        return Ok(0.0); // "no candidate packages found"
    };
    let mult = match &m[2] {
        "" => 1.0 / 1048576.0,
        "K" => 1.0 / 1024.0,
        "M" => 1.0,
        "G" => 1024.0,
        _ => 1048576.0, // "T"
    };
    Ok(_py_float(&m[1])? * mult)
}

pub fn pacman_cache() -> Finding {
    _raise(_pacman_cache())
}

/// `pacman_cache()`, Err where the Python raises.
fn _pacman_cache() -> Result<Finding, String> {
    _pacman_cache_with(_du_mb(Path::new(PACMAN_CACHE)), &_run)
}

fn _pacman_cache_with(du: (f64, i64), run: RunFn<'_>) -> Result<Finding, String> {
    let (mb, n) = du;
    // Judged on what the fix would free, not on the cache's size. A trimmed
    // cache is still gigabytes -- two versions of every installed package --
    // and rating it by size kept it critical after a trim that had nothing
    // left to remove.
    let free = _paccache_reclaimable_with(&["-k2"], run)? + _paccache_reclaimable_with(&["-uk0"], run)?;
    if free < 256.0 {
        return Ok(Finding {
            id: "pacman-cache".into(),
            title: "Package cache".into(),
            detail: format!(
                "{} in {n} downloaded packages, already \
                 trimmed to the two newest versions of each installed \
                 package. That much is what makes a downgrade possible.",
                fmt_size(mb)
            ),
            severity: OK.into(),
            count: n,
            size_mb: mb,
            ..Default::default()
        });
    }
    let sev = if free > 4096.0 {
        CRIT
    } else if free > 1024.0 {
        WARN
    } else {
        INFO
    };
    Ok(Finding {
        id: "pacman-cache".into(),
        title: "Package cache".into(),
        detail: format!(
            "{} in {n} downloaded packages, {} \
             of it older versions and packages no longer installed. \
             Keeping a couple of versions makes a downgrade possible; \
             keeping all of them since install does not.",
            fmt_size(mb),
            fmt_size(free)
        ),
        severity: sev.into(),
        count: n,
        size_mb: free,
        fix_label: "Trim to 2 versions".into(),
        // -rk2 keeps the two newest of each installed package; -ruk0 drops
        // every cached version of packages no longer installed, which can
        // never be downgraded to anyway.
        fix: Some(Fix::argv(&["paccache", "-rk2", "-ruk0"])),
        fix_note: "Keeps the 2 most recent versions of installed packages and \
                   removes cached packages that are no longer installed."
            .into(),
        ..Default::default()
    })
}

pub fn yay_cache() -> Finding {
    _yay_cache_at(&YAY_CACHE)
}

fn _yay_cache_at(cache: &Path) -> Finding {
    let (mb, n) = _du_mb(cache);
    let sev = if mb > 512.0 { WARN } else { OK };
    let at = cache.to_path_buf();
    Finding {
        id: "yay-cache".into(),
        title: "AUR build cache".into(),
        detail: format!(
            "{} of cloned PKGBUILDs and build artefacts in \
             ~/.cache/yay. Rebuilt on demand; nothing installed depends \
             on it.",
            fmt_size(mb)
        ),
        severity: sev.into(),
        count: n,
        size_mb: mb,
        fix_label: "Clear".into(),
        fix: Some(Fix::func(move |log| _clear_yay_cache_at(&at, log))),
        fix_note: "Deletes ~/.cache/yay. The next AUR build re-clones what it \
                   needs."
            .into(),
        ..Default::default()
    }
}

fn _clear_yay_cache_at(cache: &Path, log: LogFn<'_>) -> Result<(), String> {
    if !cache.exists() {
        log("nothing to clear");
        return Ok(());
    }
    let (mb, n) = _du_mb(cache);
    log(&format!("removing {} ({}, {n} files)", cache.display(), fmt_size(mb)));
    // shutil.rmtree(..., ignore_errors=True): a symlinked cache is refused,
    // not unlinked.
    let _ = _rmtree(cache, true);
    log("cleared");
    Ok(())
}

pub fn journal() -> Finding {
    _raise(_journal())
}

/// `journal()`, Err where the Python raises.
fn _journal() -> Result<Finding, String> {
    _journal_from(&_run(&["journalctl", "--disk-usage"], 60)?)
}

fn _journal_from(out: &str) -> Result<Finding, String> {
    let mut mb = 0.0;
    if let Some(m) = JOURNAL_RE.captures(out) {
        let mult = match &m[2] {
            "K" => 1.0 / 1024.0,
            "M" => 1.0,
            "G" => 1024.0,
            _ => 1048576.0, // "T"
        };
        mb = _py_float(&m[1])? * mult;
    }
    let sev = if mb > 800.0 { WARN } else { OK };
    Ok(Finding {
        id: "journal".into(),
        title: "Systemd journal".into(),
        detail: format!(
            "{} of logs. Useful for diagnosing the recent \
             past; months of it rarely is.",
            fmt_size(mb)
        ),
        // What the vacuum frees, which is what the reclaimable total adds up.
        severity: sev.into(),
        size_mb: f64::max(mb - 500.0, 0.0),
        fix_label: "Vacuum to 500 MB".into(),
        fix: Some(Fix::argv(&["journalctl", "--vacuum-size=500M"])),
        fix_note: "Deletes the oldest journal files until 500 MB remains.".into(),
        ..Default::default()
    })
}

pub fn trash() -> Finding {
    _trash_at(&TRASH)
}

fn _trash_at(trash: &Path) -> Finding {
    let (mb, n) = _du_mb(&trash.join("files"));
    let sev = if mb > 50.0 { INFO } else { OK };
    let at = trash.to_path_buf();
    Finding {
        id: "trash".into(),
        title: "Trash".into(),
        detail: format!("{} in {n} deleted files still taking up space.", fmt_size(mb)),
        severity: sev.into(),
        count: n,
        size_mb: mb,
        fix_label: "Empty trash".into(),
        fix: Some(Fix::func(move |log| _empty_trash_at(&at, log))),
        fix_note: "Permanently deletes everything in the trash. This cannot be \
                   undone."
            .into(),
        ..Default::default()
    }
}

fn _empty_trash_at(trash: &Path, log: LogFn<'_>) -> Result<(), String> {
    for sub in ["files", "info"] {
        let d = trash.join(sub);
        if !d.exists() {
            continue;
        }
        for child in _iterdir(&d)? {
            let is_link = child.is_symlink();
            let res = if child.is_dir() && !is_link {
                _rmtree(&child, false)
            } else {
                std::fs::remove_file(&child).map_err(|exc| _oserror(&exc, &child))
            };
            match res {
                Ok(()) => log(&format!("removed {}", _name(&child))),
                Err(exc) => log(&format!("could not remove {}: {exc}", _name(&child))),
            }
        }
    }
    log("trash emptied");
    Ok(())
}

/// The .previous copies this app keeps after an AppImage update.
pub fn rollbacks() -> Finding {
    _raise(_rollbacks_in(&_appimage_store()))
}

fn _rollbacks_in(apps: &Path) -> Result<Finding, String> {
    let files = if apps.exists() { _glob_suffix(apps, ".previous") } else { Vec::new() };
    let mut mb = 0.0;
    if !files.is_empty() {
        let mut total: u64 = 0;
        for f in &files {
            total += std::fs::metadata(f).map_err(|e| _oserror(&e, f))?.len();
        }
        mb = total as f64 / 1048576.0;
    }
    let at = apps.to_path_buf();
    Ok(Finding {
        id: "rollbacks".into(),
        title: "AppImage rollback copies".into(),
        detail: format!(
            "{} previous build(s) kept after an update, \
             {}. They exist so a bad update can be reverted; \
             once the new version is known good they are dead weight.",
            files.len(),
            fmt_size(mb)
        ),
        severity: if files.is_empty() { OK } else { INFO }.into(),
        count: files.len() as i64,
        size_mb: mb,
        items: files.iter().map(|f| _name(f)).collect(),
        fix_label: "Delete rollback copies".into(),
        fix: if files.is_empty() { None } else { Some(Fix::func(move |log| _delete_rollbacks_in(&at, log))) },
        fix_note: "Deletes the .previous files. The installed AppImages are \
                   untouched, but reverting an update will no longer be \
                   possible without re-downloading."
            .into(),
        ..Default::default()
    })
}

fn _delete_rollbacks_in(apps: &Path, log: LogFn<'_>) -> Result<(), String> {
    for f in _glob_suffix(apps, ".previous") {
        match std::fs::remove_file(&f) {
            Ok(()) => log(&format!("removed {}", _name(&f))),
            Err(exc) => log(&format!("could not remove {}: {}", _name(&f), _oserror(&exc, &f))),
        }
    }
    Ok(())
}

pub fn failed_units() -> Finding {
    _raise(_failed_units_with(&_run))
}

fn _failed_units_with(run: RunFn<'_>) -> Result<Finding, String> {
    let mut rows: Vec<String> = Vec::new();
    for scope in [&[][..], &["--user"][..]] {
        let mut cmd = vec!["systemctl"];
        cmd.extend_from_slice(scope);
        cmd.extend_from_slice(&["--failed", "--no-legend", "--plain", "--no-pager"]);
        let out = run(&cmd, 60)?;
        for line in _py_splitlines(&out) {
            let name = _py_split(line).first().copied().unwrap_or("");
            if !name.is_empty() {
                rows.push(format!("{}{name}", if scope.is_empty() { "system " } else { "user " }));
            }
        }
    }
    let any = !rows.is_empty();
    Ok(Finding {
        id: "failed-units".into(),
        title: "Failed services".into(),
        detail: if any {
            "Units that exited with an error and were not restarted. \
             A failed unit is something that was supposed to be running."
        } else {
            "No failed units."
        }
        .into(),
        severity: if any { WARN } else { OK }.into(),
        count: rows.len() as i64,
        items: rows,
        fix_label: "Clear failed state".into(),
        // reset-failed clears the flag; it does not hide a unit that fails
        // again, so the finding comes straight back if the cause remains.
        fix: if any { Some(Fix::argv(&["systemctl", "--user", "reset-failed"])) } else { None },
        fix_note: "Clears the failed flag for user units. If the cause is still \
                   there, the unit fails again and this reappears."
            .into(),
        ..Default::default()
    })
}

pub fn pacnew() -> Finding {
    _raise(_pacnew_with(&_run))
}

fn _pacnew_with(run: RunFn<'_>) -> Result<Finding, String> {
    let mut files: Vec<String> = Vec::new();
    for pat in ["*.pacnew", "*.pacsave"] {
        let out = run(&["find", "/etc", "-name", pat, "-type", "f"], 40)?;
        files.extend(_py_splitlines(&out).into_iter().filter(|f| !_py_strip(f).is_empty()).map(String::from));
    }
    let any = !files.is_empty();
    Ok(Finding {
        id: "pacnew".into(),
        title: "Unmerged config files".into(),
        detail: if any {
            "Package updates left new default configs beside yours. Until \
             they are merged you are running old defaults, and this is \
             where Arch upgrades usually bite."
        } else {
            "No .pacnew or .pacsave files in /etc."
        }
        .into(),
        severity: if any { WARN } else { OK }.into(),
        count: files.len() as i64,
        items: files,
        // pacdiff is an interactive three-way merge; proposing to run it
        // unattended would be proposing to guess at the user's config.
        fix_label: String::new(),
        fix: None,
        fix_note: "Merge these with `pacdiff` in a terminal — it needs you to \
                   decide, file by file."
            .into(),
        ..Default::default()
    })
}

/// Launcher entries pointing at programs that are no longer there.
pub fn dangling_desktop() -> Finding {
    _raise(_dangling_desktop_in(&crate::xdg::data_home()))
}

fn _dangling_desktop_in(data_home: &Path) -> Result<Finding, String> {
    let bad: Vec<String> = _dangling_rows(data_home)?.iter().map(|r| _pystr(r)).collect();
    let any = !bad.is_empty();
    let at = data_home.to_path_buf();
    Ok(Finding {
        id: "dangling-desktop".into(),
        title: "Broken launcher entries".into(),
        detail: if any {
            ".desktop files whose program no longer exists. They show up \
             in the launcher and do nothing when clicked."
        } else {
            "Every launcher entry points at something real."
        }
        .into(),
        severity: if any { INFO } else { OK }.into(),
        count: bad.len() as i64,
        items: bad,
        fix_label: "Remove broken entries".into(),
        fix: if any { Some(Fix::func(move |log| _remove_dangling_in(&at, log))) } else { None },
        fix_note: "Deletes only the .desktop files listed above, from \
                   ~/.local/share/applications."
            .into(),
        ..Default::default()
    })
}

/// The rows of `dangling_desktop`'s `bad` list as the bytes Python's str
/// stands for: a file name that is not UTF-8 stays exact, so the fix can
/// still name (and delete) the file the row came from.
fn _dangling_rows(data_home: &Path) -> Result<Vec<Vec<u8>>, String> {
    let mut bad: Vec<Vec<u8>> = Vec::new();
    for d in [data_home.join("applications")] {
        if !d.exists() {
            continue;
        }
        for f in _glob_suffix(&d, ".desktop") {
            let mut target = String::new();
            // f.read_text(errors="replace"); OSError skips the file.
            let Ok(bytes) = std::fs::read(&f) else { continue };
            let text = String::from_utf8_lossy(&bytes);
            for line in _py_splitlines(&text) {
                if let Some(rest) = line.strip_prefix("TryExec=") {
                    target = rest.to_string();
                    break;
                }
                if let Some(rest) = line.strip_prefix("Exec=") {
                    if target.is_empty() {
                        // .split()[0] raises IndexError on an empty Exec=.
                        target = _py_split(rest)
                            .first()
                            .ok_or_else(|| "list index out of range".to_string())?
                            .to_string();
                    }
                }
            }
            // Entries routinely quote the path; an unstripped quote would make
            // every single one look broken.
            let target = _py_strip(&target).trim_matches('"').trim_matches('\'').to_string();
            if target.is_empty() {
                continue;
            }
            let found = if !target.contains('/') {
                _which(&target).is_some()
            } else {
                Path::new(&target).exists()
            };
            if !found {
                let mut row = _name_bytes(&f).to_vec();
                row.extend_from_slice(format!(" → {target}").as_bytes());
                bad.push(row);
            }
        }
    }
    Ok(bad)
}

fn _remove_dangling_in(data_home: &Path, log: LogFn<'_>) -> Result<(), String> {
    use std::os::unix::ffi::OsStrExt;
    let found = _dangling_rows(data_home)?;
    let d = data_home.join("applications");
    for row in &found {
        // row.split(" → ")[0], on the bytes the str stands for.
        let sep = " → ".as_bytes();
        let cut = row.windows(sep.len()).position(|w| w == sep).unwrap_or(row.len());
        let raw = &row[..cut];
        let f = d.join(std::ffi::OsStr::from_bytes(raw));
        let name = _pystr(raw);
        match std::fs::remove_file(&f) {
            Ok(()) => log(&format!("removed {name}")),
            Err(exc) => log(&format!("could not remove {name}: {}", _oserror(&exc, &f))),
        }
    }
    Ok(())
}

pub fn broken_symlinks() -> Finding {
    _raise(_broken_symlinks_in(&HOME))
}

fn _broken_symlinks_in(home: &Path) -> Result<Finding, String> {
    let mut bad: Vec<String> = Vec::new();
    for d in [home.join(".local/bin")] {
        if !d.exists() {
            continue;
        }
        for f in _iterdir(&d)? {
            if f.is_symlink() && !f.exists() {
                let link = std::fs::read_link(&f).map_err(|e| _oserror(&e, &f))?;
                // os.readlink gives the target as a str, surrogates and all.
                use std::os::unix::ffi::OsStrExt;
                bad.push(format!("{} → {}", _name(&f), _pystr(link.as_os_str().as_bytes())));
            }
        }
    }
    let any = !bad.is_empty();
    let at = home.to_path_buf();
    Ok(Finding {
        id: "broken-symlinks".into(),
        title: "Broken symlinks on PATH".into(),
        detail: if any {
            "Links in ~/.local/bin whose target is gone. Running one is a \
             command-not-found with a confusing name."
        } else {
            "No broken links in ~/.local/bin."
        }
        .into(),
        severity: if any { INFO } else { OK }.into(),
        count: bad.len() as i64,
        items: bad,
        fix_label: "Remove broken links".into(),
        fix: if any { Some(Fix::func(move |log| _remove_broken_links_in(&at, log))) } else { None },
        fix_note: "Deletes only the dangling symlinks listed above.".into(),
        ..Default::default()
    })
}

fn _remove_broken_links_in(home: &Path, log: LogFn<'_>) -> Result<(), String> {
    let d = home.join(".local/bin");
    for f in _iterdir(&d)? {
        if f.is_symlink() && !f.exists() {
            match std::fs::remove_file(&f) {
                Ok(()) => log(&format!("removed {}", _name(&f))),
                Err(exc) => log(&format!("could not remove {}: {}", _name(&f), _oserror(&exc, &f))),
            }
        }
    }
    Ok(())
}

pub fn orphan_packages() -> Finding {
    _orphan_packages_from(&crate::inventory::orphans())
}

fn _orphan_packages_from(apps: &[crate::inventory::App]) -> Finding {
    let keep = |a: &crate::inventory::App| a.extra.get("advice").and_then(|v| v.as_str()) == Some("keep");
    let safe: Vec<String> = apps.iter().filter(|a| !keep(a)).map(|a| a.name.clone()).collect();
    let kept: Vec<String> = apps.iter().filter(|a| keep(a)).map(|a| a.name.clone()).collect();
    let mut fix_argv = vec!["pacman".to_string(), "-Rns".into(), "--noconfirm".into()];
    fix_argv.extend(safe.iter().cloned());
    Finding {
        id: "orphans".into(),
        title: "Orphaned packages".into(),
        detail: if !apps.is_empty() {
            format!(
                "{} installed as dependencies that nothing needs any \
                 more. {} of them are build tooling or debug symbols \
                 and are kept deliberately — removing rust only means \
                 downloading it again on the next AUR build.",
                apps.len(),
                kept.len()
            )
        } else {
            "Nothing is orphaned.".into()
        },
        severity: if safe.is_empty() { OK } else { INFO }.into(),
        count: safe.len() as i64,
        fix: if safe.is_empty() { None } else { Some(Fix::Argv(fix_argv)) },
        items: safe,
        fix_label: "Review and remove".into(),
        fix_note: "Removes only the packages listed above.".into(),
        ..Default::default()
    }
}

/// Foreign packages the AUR no longer carries, or that are flagged.
pub fn unmaintained_aur() -> Finding {
    _raise(_unmaintained_aur_with(&_run, &crate::inventory::aur_health))
}

fn _unmaintained_aur_with(
    run: RunFn<'_>,
    aur_health: &dyn Fn(&[String]) -> std::collections::BTreeMap<String, crate::inventory::AurHealth>,
) -> Result<Finding, String> {
    let names: Vec<String> = _py_split(&run(&["pacman", "-Qmq"], 60)?).into_iter().map(String::from).collect();
    if names.is_empty() {
        return Ok(Finding {
            id: "aur-health".into(),
            title: "AUR packages".into(),
            detail: "No foreign packages installed.".into(),
            severity: OK.into(),
            ..Default::default()
        });
    }
    let health = aur_health(&names);
    let mut gone: Vec<String> = Vec::new();
    let mut flagged: Vec<String> = Vec::new();
    for n in &names {
        match health.get(n) {
            None => gone.push(n.clone()),
            Some(h) if h.out_of_date => flagged.push(n.clone()),
            Some(h) if h.maintainer.is_empty() => flagged.push(format!("{n} (orphaned upstream)")),
            Some(_) => {}
        }
    }
    let mut rows: Vec<String> = gone.iter().map(|n| format!("{n} — no longer in the AUR")).collect();
    rows.extend(flagged.iter().cloned());
    Ok(Finding {
        id: "aur-health".into(),
        title: "AUR package health".into(),
        detail: if !rows.is_empty() {
            format!(
                "{} package(s) are installed from the AUR but no \
                 longer listed there, and {} are flagged \
                 out-of-date or have no maintainer. Neither will ever \
                 update again on its own.",
                gone.len(),
                flagged.len()
            )
        } else {
            format!(
                "All {} AUR packages are still listed and \
                 maintained.",
                names.len()
            )
        },
        severity: if !gone.is_empty() {
            WARN
        } else if !flagged.is_empty() {
            INFO
        } else {
            OK
        }
        .into(),
        count: rows.len() as i64,
        items: rows,
        fix_label: String::new(),
        fix: None,
        fix_note: "Decide per package: find a replacement, or keep it knowing \
                   it is frozen."
            .into(),
        ..Default::default()
    })
}

/// (free MB, total MB, percent used) or None.
fn _usage(path: &str) -> Option<(f64, f64, f64)> {
    let st = rustix::fs::statvfs(path).ok()?;
    let total = st.f_blocks as f64 * st.f_frsize as f64 / 1048576.0;
    let free = st.f_bavail as f64 * st.f_frsize as f64 / 1048576.0;
    if total <= 0.0 {
        return None;
    }
    Some((free, total, 100.0 * (1.0 - free / total)))
}

/// Files a package owns that are no longer where it put them.
///
/// `pacman -Qk` reports two completely different things in the same breath:
/// a file that is genuinely gone, and a file this process is not allowed to
/// look at. Most of /etc and all of /boot is root-only, so an unprivileged
/// run cannot see a large part of what it is checking -- and counting those
/// as faults reports a healthy system as broken. They are counted and
/// stated separately, never as problems.
pub fn package_integrity() -> Finding {
    _raise(_package_integrity())
}

/// `package_integrity()`, Err where the Python raises.
fn _package_integrity() -> Result<Finding, String> {
    // pacman writes the per-file warnings to stderr and the per-package
    // summary to stdout. Reading only stdout silently reports full coverage
    // of a check that could not open a quarter of what it looked at.
    // (text=True: output that is not UTF-8 raises, uncaught.)
    let out = match _run_bytes(&["pacman", "-Qk"], 240) {
        Some((o, e)) => _py_decode(&o)? + &_py_decode(&e)?,
        None => String::new(),
    };
    _package_integrity_from(&out)
}

fn _package_integrity_from(out: &str) -> Result<Finding, String> {
    if _py_strip(out).is_empty() {
        return Ok(Finding {
            id: "integrity".into(),
            title: "Package files".into(),
            detail: "Could not run the integrity check.".into(),
            severity: OK.into(),
            ..Default::default()
        });
    }

    let mut missing: Vec<String> = Vec::new();
    let mut unreadable = 0;
    for line in _py_splitlines(out) {
        if !line.contains("warning:") || !line.contains('(') {
            continue;
        }
        let tail = line.rsplit_once('(').map(|(_, t)| t).unwrap_or(line);
        let reason = _py_strip(tail.trim_end_matches(')')).to_lowercase();
        let body = _py_strip(line.split_once("warning:").map(|(_, b)| b).unwrap_or(""));
        let path = _py_strip(body.rsplit_once('(').map(|(p, _)| p).unwrap_or(body));
        if reason.contains("permission denied") {
            unreadable += 1;
        } else if reason.contains("no such file") || reason.contains("missing") {
            missing.push(path.to_string());
        } else {
            missing.push(format!("{path} — {reason}"));
        }
    }

    let mut checked = num_bigint::BigUint::default();
    for m in TOTAL_FILES_RE.captures_iter(out) {
        checked += _py_int(&m[1])?;
    }
    let note = if unreadable > 0 {
        format!(" {unreadable} file(s) could not be read without root and were not checked.")
    } else {
        String::new()
    };

    if !missing.is_empty() {
        return Ok(Finding {
            id: "integrity".into(),
            title: "Package files".into(),
            detail: format!(
                "{} file(s) a package owns are missing from \
                 disk, out of {} checked.{note} A package that \
                 has lost files usually needs reinstalling.",
                missing.len(),
                _thousands_big(&checked)?
            ),
            severity: WARN.into(),
            count: missing.len() as i64,
            items: missing.into_iter().take(40).collect(),
            fix_label: String::new(),
            fix: None,
            fix_note: "Reinstall the owning package with \
                       `pacman -S <name>` once you know which it is."
                .into(),
            ..Default::default()
        });
    }

    Ok(Finding {
        id: "integrity".into(),
        title: "Package files".into(),
        detail: format!("All {} readable files are where their packages put them.{note}", _thousands_big(&checked)?),
        severity: OK.into(),
        ..Default::default()
    })
}

/// Every filesystem that matters, not just the root one.
///
/// Checking only / misses the ways a machine actually runs out of room: a
/// full /boot stops a kernel update, and a full XDG runtime directory --
/// 1.6 GB of tmpfs that one runaway log can fill -- breaks logins, Wayland
/// clients and system updates while `df /` still reports hundreds of
/// gigabytes free.
pub fn disk_space() -> Finding {
    _raise(_disk_space_with(&_usage))
}

fn _disk_space_with(usage: &dyn Fn(&str) -> Option<(f64, f64, f64)>) -> Result<Finding, String> {
    use std::os::unix::fs::MetadataExt;

    let mut watched: Vec<(String, String)> = vec![("/".into(), "/".into())];
    for extra in ["/boot".to_string(), "/var".into(), "/home".into(), _runtime_dir()] {
        if !extra.is_empty() && Path::new(&extra).is_dir() {
            if let (Ok(a), Ok(b)) = (std::fs::metadata(&extra), std::fs::metadata("/")) {
                if a.dev() != b.dev() {
                    watched.push((extra.clone(), extra));
                }
            }
        }
    }

    let order = [CRIT, WARN, INFO, OK];
    let rank = |s: &str| order.iter().position(|x| *x == s).unwrap_or(order.len());
    let mut rows: Vec<String> = Vec::new();
    let mut worst = OK;
    let mut worst_pct = 0.0_f64;
    let mut seen: Vec<(u64, u64)> = Vec::new();
    for (label, path) in &watched {
        let Some((free, total, pct)) = usage(path) else { continue };
        // btrfs subvolumes report distinct device ids while sharing one pool,
        // so /, /home and /var would otherwise be listed three times with
        // identical figures.
        let fingerprint = (_py_round_int(total)?, _py_round_int(free)?);
        if seen.contains(&fingerprint) {
            continue;
        }
        seen.push(fingerprint);
        rows.push(format!("{label}  {} free of {} ({}% used)", fmt_size(free), fmt_size(total), _ff(pct, 0)));
        // A small tmpfs fills fast and breaks more than it looks like it
        // should, so it is judged on headroom rather than percentage alone.
        let tight = (total < 4096.0 && free < 128.0) || pct > 92.0;
        let level = if tight {
            CRIT
        } else if pct > 85.0 {
            WARN
        } else {
            OK
        };
        if rank(level) < rank(worst) {
            worst = level;
        }
        worst_pct = worst_pct.max(pct);
    }

    let root = usage("/").unwrap_or((0.0, 0.0, 0.0));
    let mut detail = if rows.is_empty() { "Could not read filesystem stats.".to_string() } else { rows.join(" · ") };
    if worst == CRIT {
        detail.push_str("  A filesystem is nearly full; an update will fail before it starts.");
    }
    Ok(Finding {
        id: "disk".into(),
        title: "Disk space".into(),
        detail,
        severity: worst.into(),
        items: if rows.len() > 1 { rows } else { Vec::new() },
        size_mb: root.0,
        ..Default::default()
    })
}

/// `round(x)` as the exact int Python makes of it, as a key: a rounded
/// float is a whole number that f64 holds exactly, so equal ints have equal
/// bits (0.0 and -0.0 made one). NaN and the infinities raise, as `round`
/// does.
fn _py_round_int(x: f64) -> Result<u64, String> {
    if x.is_nan() {
        return Err("cannot convert float NaN to integer".into());
    }
    if x.is_infinite() {
        return Err("cannot convert float infinity to integer".into());
    }
    Ok((crate::util::py_round(x) + 0.0).to_bits())
}

/// Session logs in the runtime directory, which is a small tmpfs.
///
/// Hyprland and Quickshell write per-session logs into XDG_RUNTIME_DIR. A
/// display that flaps produces a hotplug event per cycle and several log
/// lines per event, and the directory it lands in is typically 1.6 GB. The
/// failure mode is not "logs are large" but "the next system update dies
/// with No space left on device".
pub fn runtime_logs() -> Finding {
    _runtime_logs_in(&_runtime_dir())
}

fn _runtime_logs_in(runtime: &str) -> Finding {
    if !Path::new(runtime).is_dir() {
        return Finding {
            id: "runtime-logs".into(),
            title: "Session logs".into(),
            detail: "No runtime directory to check.".into(),
            severity: OK.into(),
            ..Default::default()
        };
    }

    let mut biggest: Vec<String> = Vec::new();
    let mut total_mb = 0.0;
    let mut items: Vec<String> = Vec::new();
    for sub in ["hypr", "quickshell"] {
        let d = Path::new(runtime).join(sub);
        if !d.exists() {
            continue;
        }
        let (mb, _n) = _du_mb(&d);
        total_mb += mb;
        if mb >= 1.0 {
            items.push(format!("{sub}  {}", fmt_size(mb)));
        }
    }
    // Path(runtime).rglob(): the paths it yields are spelled from the
    // normalised Path, not the raw string ("/./" and "//" collapse).
    let root = _pathlib_str(runtime);
    for f in _rglob_suffix(Path::new(&root), ".log") {
        let Ok(m) = std::fs::metadata(&f) else { continue };
        let size = m.len() as f64 / 1048576.0;
        if size > 100.0 {
            let shown = _path_str(&f);
            // Path(".") / name is spelled "name".
            let shown = if root == "." { shown.strip_prefix("./").unwrap_or(&shown).to_string() } else { shown };
            biggest.push(format!("{}  {shown}", fmt_size(size)));
        }
    }

    let pct = _usage(runtime).map(|u| u.2).unwrap_or(0.0);
    let sev = if pct > 90.0 {
        CRIT
    } else if total_mb > 400.0 || !biggest.is_empty() {
        WARN
    } else {
        OK
    };
    let some = total_mb >= 1.0;
    items.extend(biggest);
    let at = PathBuf::from(runtime);
    Finding {
        id: "runtime-logs".into(),
        title: "Session logs".into(),
        detail: if some {
            format!(
                "{} of session logs in {runtime}, which is \
                 {}% full. These are logs from this and previous \
                 desktop sessions; nothing reads them after the session ends.",
                fmt_size(total_mb),
                _ff(pct, 0)
            )
        } else {
            format!("Session logs are negligible ({runtime} is {}% full).", _ff(pct, 0))
        },
        severity: sev.into(),
        size_mb: total_mb,
        items,
        fix_label: if some { "Clear old session logs" } else { "" }.into(),
        fix: if some {
            Some(Fix::func(move |log| {
                _clear_runtime_logs_in(&at, &_hyprctl_instances(), crate::util::now(), log)
            }))
        } else {
            None
        },
        fix_note: "Truncates the running session's log and removes log \
                   directories from sessions that have ended. The desktop is \
                   not restarted and nothing else is touched."
            .into(),
        ..Default::default()
    }
}

/// `hyprctl instances -j` stdout, or "" when it cannot run.
fn _hyprctl_instances() -> String {
    // Undecodable output raises in the Python, inside the `except Exception`
    // that leaves `live` empty -- which "" does here too.
    match _run_bytes(&["hyprctl", "instances", "-j"], 20) {
        Some((out, err)) => match (_py_decode(&out), _py_decode(&err)) {
            (Ok(o), Ok(_)) => o,
            _ => String::new(),
        },
        None => String::new(),
    }
}

fn _clear_runtime_logs(log: LogFn<'_>) -> Result<(), String> {
    let runtime = PathBuf::from(_runtime_dir());
    _clear_runtime_logs_in(&runtime, &_hyprctl_instances(), crate::util::now(), log)
}

/// `{i.get("instance") for i in json.loads(text)}`, keeping only the str
/// values (nothing else can equal a directory name), as code points; None
/// where the Python raises, which its `except Exception` turns into an empty
/// set. Python's json, not serde's: NaN, Infinity, -Infinity and numbers
/// beyond f64 (1e400 is inf) are accepted, lone surrogate escapes are kept,
/// an int of more digits than `sys.get_int_max_str_digits()` raises, and
/// nesting past the recursion limit raises. Iterating a top-level dict or
/// str gives keys or characters, which have no `.get` -- so only an empty
/// one passes, and gives nothing.
fn _live_instances(text: &str) -> Option<Vec<Vec<u32>>> {
    // Python 3.14 bounds recursion by the C stack; about this many nested
    // containers load before a RecursionError.
    const LOADS_DEPTH: usize = 52_100;
    let int_max_digits = match std::env::var("PYTHONINTMAXSTRDIGITS").ok().and_then(|v| v.trim().parse::<usize>().ok()) {
        Some(0) => usize::MAX,
        Some(n) if n >= 640 => n,
        _ => 4300,
    };
    let b = text.as_bytes();
    let ws = |mut i: usize| {
        while matches!(b.get(i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            i += 1;
        }
        i
    };
    // A str from just after its opening quote: (code points, index after the
    // closing quote). Strict: raw control characters are refused.
    let string = |mut i: usize| -> Option<(Vec<u32>, usize)> {
        let hex4 = |i: usize| -> Option<u32> {
            let h = b.get(i..i + 4)?;
            if !h.iter().all(|c| c.is_ascii_hexdigit()) {
                return None;
            }
            u32::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok()
        };
        let mut out = Vec::new();
        loop {
            let c = *b.get(i)?;
            match c {
                b'"' => return Some((out, i + 1)),
                b'\\' => {
                    let e = *b.get(i + 1)?;
                    i += 2;
                    let unit = match e {
                        b'"' => 0x22,
                        b'\\' => 0x5c,
                        b'/' => 0x2f,
                        b'b' => 0x08,
                        b'f' => 0x0c,
                        b'n' => 0x0a,
                        b'r' => 0x0d,
                        b't' => 0x09,
                        b'u' => {
                            let u = hex4(i)?;
                            i += 4;
                            // A high surrogate escape joins a low one right after it.
                            if (0xd800..0xdc00).contains(&u) && b.get(i) == Some(&b'\\') && b.get(i + 1) == Some(&b'u') {
                                if let Some(u2) = hex4(i + 2).filter(|u2| (0xdc00..0xe000).contains(u2)) {
                                    i += 6;
                                    out.push(0x10000 + ((u - 0xd800) << 10) + (u2 - 0xdc00));
                                    continue;
                                }
                            }
                            u
                        }
                        _ => return None,
                    };
                    out.push(unit);
                }
                c if c < 0x20 => return None,
                _ => {
                    // One whole UTF-8 character (the text is a valid str).
                    let ch = text.get(i..)?.chars().next()?;
                    out.push(u32::from(ch));
                    i += ch.len_utf8();
                }
            }
        }
    };

    /// What a finished value is, as far as the caller cares.
    enum V {
        Str(Vec<u32>),
        /// A dict, with the str it holds under "instance" (last one wins).
        Dict(Option<Vec<u32>>),
        EmptyDict,
        List,
        Other,
    }
    struct Frame {
        dict: bool,
        key: Vec<u32>,
        instance: Option<Vec<u32>>,
    }
    let instance_key: Vec<u32> = "instance".chars().map(u32::from).collect();
    let mut stack: Vec<Frame> = Vec::new();
    // The top-level list's elements, while it is the root.
    let mut elems: Vec<V> = Vec::new();
    let mut i = ws(0);
    loop {
        // Parse one value at i; containers push a frame and go on to their
        // first element (or close at once when empty).
        let mut v = match *b.get(i)? {
            b'"' => {
                let (s, j) = string(i + 1)?;
                i = j;
                V::Str(s)
            }
            c @ (b'{' | b'[') => {
                if stack.len() >= LOADS_DEPTH {
                    return None;
                }
                let dict = c == b'{';
                i = ws(i + 1);
                let close = if dict { b'}' } else { b']' };
                if b.get(i) == Some(&close) {
                    i += 1;
                    if dict { V::EmptyDict } else { V::List }
                } else {
                    let mut key = Vec::new();
                    if dict {
                        if b.get(i) != Some(&b'"') {
                            return None;
                        }
                        let (k, j) = string(i + 1)?;
                        i = ws(j);
                        if b.get(i) != Some(&b':') {
                            return None;
                        }
                        i = ws(i + 1);
                        key = k;
                    }
                    stack.push(Frame { dict, key, instance: None });
                    continue;
                }
            }
            _ if b[i..].starts_with(b"null") => {
                i += 4;
                V::Other
            }
            _ if b[i..].starts_with(b"true") => {
                i += 4;
                V::Other
            }
            _ if b[i..].starts_with(b"false") => {
                i += 5;
                V::Other
            }
            _ if b[i..].starts_with(b"NaN") => {
                i += 3;
                V::Other
            }
            _ if b[i..].starts_with(b"Infinity") => {
                i += 8;
                V::Other
            }
            _ if b[i..].starts_with(b"-Infinity") => {
                i += 9;
                V::Other
            }
            _ => {
                // -?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][-+]?[0-9]+)?, ASCII digits.
                let digits = |mut j: usize| {
                    while b.get(j).is_some_and(u8::is_ascii_digit) {
                        j += 1;
                    }
                    j
                };
                let mut j = i;
                if b.get(j) == Some(&b'-') {
                    j += 1;
                }
                let int_start = j;
                match b.get(j) {
                    Some(b'0') => j += 1,
                    Some(b'1'..=b'9') => j = digits(j),
                    _ => return None,
                }
                let int_end = j;
                let mut is_float = false;
                if b.get(j) == Some(&b'.') && b.get(j + 1).is_some_and(u8::is_ascii_digit) {
                    j = digits(j + 1);
                    is_float = true;
                }
                if matches!(b.get(j), Some(b'e' | b'E')) {
                    let mut k = j + 1;
                    if matches!(b.get(k), Some(b'+' | b'-')) {
                        k += 1;
                    }
                    if b.get(k).is_some_and(u8::is_ascii_digit) {
                        j = digits(k);
                        is_float = true;
                    }
                }
                // int(): ValueError past the digit limit. A float is inf at worst.
                if !is_float && int_end - int_start > int_max_digits {
                    return None;
                }
                i = j;
                V::Other
            }
        };
        // Hand the finished value to the containers it closes.
        loop {
            let Some(top) = stack.last_mut() else {
                // The root: anything after it but whitespace is "Extra data".
                if ws(i) != b.len() {
                    return None;
                }
                return match v {
                    V::List => {
                        let mut live = Vec::new();
                        for e in elems {
                            match e {
                                V::Dict(inst) => live.extend(inst),
                                V::EmptyDict => {}
                                // list/str/int... has no .get: AttributeError.
                                _ => return None,
                            }
                        }
                        Some(live)
                    }
                    // Only an empty dict or str iterates to nothing.
                    V::EmptyDict => Some(Vec::new()),
                    V::Str(s) if s.is_empty() => Some(Vec::new()),
                    _ => None,
                };
            };
            if top.dict {
                if top.key == instance_key {
                    top.instance = match v {
                        V::Str(s) => Some(s),
                        _ => None,
                    };
                }
            } else if stack.len() == 1 {
                elems.push(v);
            }
            let top = stack.last_mut()?;
            i = ws(i);
            match b.get(i) {
                Some(b',') => {
                    i = ws(i + 1);
                    if top.dict {
                        if b.get(i) != Some(&b'"') {
                            return None;
                        }
                        let (k, j) = string(i + 1)?;
                        i = ws(j);
                        if b.get(i) != Some(&b':') {
                            return None;
                        }
                        i = ws(i + 1);
                        top.key = k;
                    }
                    break;
                }
                Some(b'}') if top.dict => {
                    i += 1;
                    let f = stack.pop()?;
                    v = V::Dict(f.instance);
                }
                Some(b']') if !top.dict => {
                    i += 1;
                    stack.pop();
                    v = V::List;
                }
                _ => return None,
            }
        }
    }
}

/// The body of `_clear_runtime_logs`, given what `hyprctl instances -j`
/// printed and the time now.
fn _clear_runtime_logs_in(runtime: &Path, instances: &str, now: f64, log: LogFn<'_>) -> Result<(), String> {
    // live = {i.get("instance") for i in json.loads(out.stdout or "[]")},
    // left empty on any failure.
    let text = if instances.is_empty() { "[]" } else { instances };
    let live: Vec<Vec<u32>> = _live_instances(text).unwrap_or_default();

    let mut dirs: Vec<PathBuf> = match std::fs::read_dir(runtime.join("hypr")) {
        Ok(entries) => entries.flatten().map(|e| e.path()).collect(),
        Err(_) => Vec::new(),
    };
    dirs.sort_by_key(|p| _sort_key(p));
    for d in dirs {
        if !d.is_dir() {
            continue;
        }
        // d.name in live: code points, so a name that is not UTF-8 matches
        // the same lone surrogates in the JSON.
        let is_live = live.contains(&_sort_key(&d));
        for f in _glob_suffix_unsorted(&d, ".log") {
            let size = if f.exists() {
                std::fs::metadata(&f).map_err(|e| _oserror(&e, &f))?.len() as f64 / 1048576.0
            } else {
                0.0
            };
            if is_live {
                // The session is still running: truncate rather than unlink,
                // so the compositor keeps a valid file to write to.
                std::fs::File::create(&f).map_err(|e| _oserror(&e, &f))?;
                log(&format!("truncated {} ({}, live session)", _name(&f), fmt_size(size)));
            } else {
                match std::fs::remove_file(&f) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(_oserror(&e, &f)),
                }
                log(&format!("removed {} ({})", _name(&f), fmt_size(size)));
            }
        }
        if !is_live && _iterdir(&d)?.is_empty() {
            std::fs::remove_dir(&d).map_err(|e| _oserror(&e, &d))?;
        }
    }

    let qs = runtime.join("quickshell/by-id");
    if qs.exists() {
        let cutoff = now - 3.0 * 86400.0;
        let mut ds = _iterdir(&qs)?;
        ds.sort_by_key(|p| _sort_key(p));
        for d in ds {
            if !d.is_dir() {
                continue;
            }
            let Ok(m) = std::fs::metadata(&d) else { continue };
            // st_mtime: seconds + nanoseconds * 1e-9, negative before 1970.
            use std::os::unix::fs::MetadataExt;
            let mtime = m.mtime() as f64 + m.mtime_nsec() as f64 * 1e-9;
            if mtime < cutoff {
                // rmtree(ignore_errors=True): a symlinked session directory
                // is refused, left in place, and still logged as removed.
                let _ = _rmtree(&d, true);
                log(&format!("removed quickshell session {}", _name(&d)));
            }
        }
    }
    log("done");
    Ok(())
}

pub fn config_resets() -> Finding {
    crate::guard::finding()
}

pub fn update_snapshots() -> Finding {
    crate::snapshots::finding()
}

pub fn known_vulnerabilities() -> Finding {
    crate::security::finding()
}

/// A health check: Python's `CHECKS` holds the bare functions and reads
/// `check.__name__` when one fails; here each carries that name beside it.
pub type Check = fn() -> Finding;

pub const CHECKS: &[(&str, Check)] = &[
    ("disk_space", disk_space),
    ("runtime_logs", runtime_logs),
    ("package_integrity", package_integrity),
    ("pacman_cache", pacman_cache),
    ("journal", journal),
    ("yay_cache", yay_cache),
    ("orphan_packages", orphan_packages),
    ("unmaintained_aur", unmaintained_aur),
    ("failed_units", failed_units),
    ("pacnew", pacnew),
    ("dangling_desktop", dangling_desktop),
    ("broken_symlinks", broken_symlinks),
    ("rollbacks", rollbacks),
    ("trash", trash),
    ("config_resets", config_resets),
    ("update_snapshots", update_snapshots),
    ("known_vulnerabilities", known_vulnerabilities),
];

/// Which check produced which finding, so one can be asked again after its fix
/// without re-running all of them (the integrity check alone reads every file
/// pacman installed).
pub const CHECK_BY_ID: &[(&str, Check)] = &[
    ("disk", disk_space),
    ("runtime-logs", runtime_logs),
    ("integrity", package_integrity),
    ("pacman-cache", pacman_cache),
    ("journal", journal),
    ("yay-cache", yay_cache),
    ("orphans", orphan_packages),
    ("aur-health", unmaintained_aur),
    ("failed-units", failed_units),
    ("pacnew", pacnew),
    ("dangling-desktop", dangling_desktop),
    ("broken-symlinks", broken_symlinks),
    ("rollbacks", rollbacks),
    ("trash", trash),
    ("config-reset", config_resets),
    ("snapshot", update_snapshots),
    ("vulnerabilities", known_vulnerabilities),
];

/// `CHECK_BY_ID.get(finding_id)`.
pub fn check_by_id(finding_id: &str) -> Option<Check> {
    CHECK_BY_ID.iter().find(|(id, _)| *id == finding_id).map(|(_, f)| *f)
}

/// Every check. Read-only; safe to run on a timer.
pub fn run_all() -> Vec<Finding> {
    if crate::demo::active() {
        return crate::demo::findings();
    }

    let mut out: Vec<Finding> = Vec::new();
    for (name, check) in CHECKS {
        // `except Exception` around each check: a check that fails (here, one
        // that panics) becomes a finding saying so instead of losing the rest.
        // Quiet: the failure is reported as a finding, not on stderr.
        _quiet_panics();
        QUIET.with(|q| q.set(true));
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(check));
        QUIET.with(|q| q.set(false));
        match caught {
            Ok(f) => out.push(f),
            Err(payload) => {
                let exc = payload
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_default();
                out.push(_failed(name, &exc));
            }
        }
    }
    let rank = |s: &str| match s {
        CRIT => 0,
        WARN => 1,
        INFO => 2,
        _ => 3,
    };
    out.sort_by(|a, b| {
        rank(&a.severity)
            .cmp(&rank(&b.severity))
            .then((-a.size_mb).partial_cmp(&-b.size_mb).unwrap_or(std::cmp::Ordering::Equal))
    });
    out
}

/// How much space the actionable cleanups would actually free.
pub fn reclaimable_mb(findings: &[Finding]) -> f64 {
    const FREES: [&str; 6] = ["pacman-cache", "yay-cache", "journal", "trash", "rollbacks", "runtime-logs"];
    findings
        .iter()
        .filter(|f| FREES.contains(&f.id.as_str()) && f.actionable())
        .map(|f| f.size_mb)
        .fold(_PySum::default(), _PySum::add)
        .total()
}

/// `sum()` over floats as Python 3.12+ does it: compensated (Neumaier)
/// summation, so values that cancel do not lose what lies between them.
#[derive(Default)]
struct _PySum {
    started: bool,
    f: f64,
    c: f64,
}

impl _PySum {
    fn add(mut self, x: f64) -> Self {
        if !self.started {
            // 0 + x: the int start and the first float.
            self.started = true;
            self.f = 0.0 + x;
            return self;
        }
        let t = self.f + x;
        if self.f.abs() >= x.abs() {
            self.c += (self.f - t) + x;
        } else {
            self.c += (x - t) + self.f;
        }
        self.f = t;
        self
    }

    fn total(self) -> f64 {
        if self.c != 0.0 && self.c.is_finite() { self.f + self.c } else { self.f }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    // ---- DiskChecks ----
    // Checking only / misses how a machine actually runs out of room.
    //
    // A full XDG runtime directory -- 1.6 GB of tmpfs that one runaway
    // compositor log can fill -- stops system updates with "No space left on
    // device" while `df /` still reports hundreds of gigabytes free.

    #[test]
    fn test_disk_check_looks_beyond_the_root_filesystem() {
        let f = disk_space();
        assert!(f.detail.contains('/'));
        let runtime = _runtime_dir();
        if Path::new(&runtime).is_dir() {
            let all = format!("{} {}", f.detail, f.items.join(" "));
            assert!(all.contains(&runtime), "the runtime directory must be watched");
        }
    }

    #[test]
    fn test_a_nearly_full_small_tmpfs_is_critical() {
        // A 1.6 GB tmpfs with 40 MB left is 97% used: judged on headroom,
        // not on percentage alone, because small filesystems fill fast.
        let f = _disk_space_with(&|_p| Some((40.0, 1600.0, 97.5))).unwrap();
        assert_eq!(f.severity, CRIT);
    }

    #[test]
    fn test_runtime_log_cleanup_is_offered_only_when_there_is_something() {
        let f = runtime_logs();
        if f.size_mb < 1.0 {
            assert!(f.fix.is_none(), "nothing to clear, so nothing offered");
        } else {
            assert!(!f.fix_label.is_empty());
        }
    }

    // ---- CleanupsThatCannotHelp ----
    // A finding must clear once its fix has done everything it can.

    #[test]
    fn test_a_trimmed_package_cache_is_not_a_finding() {
        // 4.4 GB, but every byte is the two newest versions of something
        // installed: paccache has no candidates and the button would do nothing.
        let run = |_c: &[&str], _t: u64| Ok("==> no candidate packages found for pruning".to_string());
        let f = _pacman_cache_with((4500.0, 522), &run).unwrap();
        assert_eq!(f.severity, OK);
        assert!(!f.actionable());
    }

    #[test]
    fn test_cache_severity_follows_what_a_trim_would_free() {
        let dry = "==> finished dry run: 26 candidates (disk space saved: 1.23 GiB)";
        let run = |_c: &[&str], _t: u64| Ok(dry.to_string());
        let f = _pacman_cache_with((4500.0, 522), &run).unwrap();
        // Two dry runs (-k2 and -uk0), each 1.23 GiB here.
        assert!((f.size_mb - 2.0 * 1.23 * 1024.0).abs() < 0.5);
        assert_eq!(f.severity, WARN);
        assert!(f.actionable());
    }

    #[test]
    fn test_every_check_can_be_asked_again() {
        let mut by_id: Vec<usize> = CHECK_BY_ID.iter().map(|(_, f)| *f as usize).collect();
        let mut checks: Vec<usize> = CHECKS.iter().map(|(_, f)| *f as usize).collect();
        by_id.sort();
        by_id.dedup();
        checks.sort();
        checks.dedup();
        assert_eq!(by_id, checks);
    }

    #[test]
    fn test_rollback_copies_are_looked_for_where_they_are_kept() {
        let d = tempfile::tempdir().unwrap();
        // inventory.REGISTRY patched to <d>/appimages.json: the store is its
        // sibling "apps".
        let store = d.path().join("apps");
        std::fs::create_dir(&store).unwrap();
        std::fs::write(store.join("tool.previous"), vec![b'x'; 2048]).unwrap();
        let f = _rollbacks_in(&store).unwrap();
        assert_eq!(f.items, vec!["tool.previous".to_string()]);
    }

    #[test]
    fn test_the_store_is_beside_the_registry() {
        assert_eq!(
            _appimage_store(),
            crate::inventory::registry().parent().unwrap().join("apps")
        );
    }

    // ---- PackageIntegrity ----
    // "Permission denied" and "missing" are not the same answer.
    //
    // Most of /etc and all of /boot is root-only, so an unprivileged check
    // cannot see a large share of what it inspects. Counting those as faults
    // reports a healthy system as broken -- which is exactly the conclusion a
    // hand-run check reached on this machine before the distinction existed.

    const DENIED: &str = "warning: omarchy-settings: /etc/sudoers.d/omarchy-dns \
                          (Permission denied)\n\
                          warning: intel-ucode: /boot/intel-ucode.img (Permission denied)\n\
                          omarchy-settings: 643 total files, 4 missing files\n";
    const GONE: &str = "warning: somepkg: /usr/share/somepkg/data.bin \
                        (No such file or directory)\n\
                        somepkg: 12 total files, 1 missing file\n";

    fn run_with(output: &str) -> Finding {
        // stdout = output, stderr = ""
        _package_integrity_from(&format!("{output}{}", "")).unwrap()
    }

    #[test]
    fn test_unreadable_files_are_not_faults() {
        let f = run_with(DENIED);
        assert_eq!(f.severity, OK, "permission denied is not a broken package");
        assert!(f.items.is_empty());
        assert!(f.detail.contains("without root"), "the check must disclose what it could not inspect");
    }

    #[test]
    fn test_genuinely_missing_files_are_reported() {
        let f = run_with(GONE);
        assert_eq!(f.severity, WARN);
        assert_eq!(f.items.len(), 1);
        assert!(f.items[0].contains("data.bin"));
    }

    #[test]
    fn test_both_at_once_counts_them_separately() {
        let f = run_with(&format!("{DENIED}{GONE}"));
        assert_eq!(f.items.len(), 1, "only the real miss is flagged");
        assert!(f.detail.contains("2 file(s) could not be read"));
    }

    #[test]
    fn test_stderr_is_read_too() {
        // pacman writes the per-file warnings to stderr; reading only stdout
        // reports full coverage of a check that saw none of them.
        let stdout = "somepkg: 12 total files, 1 missing file\n";
        let stderr = "warning: somepkg: /usr/share/somepkg/data.bin (No such file or directory)\n";
        let f = _package_integrity_from(&format!("{stdout}{stderr}")).unwrap();
        assert_eq!(f.severity, WARN, "the warning only exists on stderr");
    }

    // ---- helpers ----

    #[test]
    fn test_python_json_and_names() {
        let cp = |s: &str| s.chars().map(u32::from).collect::<Vec<u32>>();
        assert_eq!(_live_instances(r#"[{"instance": "a", "pid": NaN}]"#), Some(vec![cp("a")]));
        assert_eq!(_live_instances(r#"[{"n": 1e400, "instance": "a"}]"#), Some(vec![cp("a")]));
        assert_eq!(_live_instances(r#"[{"instance": "a\udcff"}]"#), Some(vec![vec![97, 0xdcff]]));
        assert_eq!(_live_instances(r#"[{"instance": "a"}, []]"#), None);
        assert_eq!(_live_instances(r#"[{"instance": "a"},]"#), None);
        assert_eq!(_live_instances("{}"), Some(vec![]));
        assert_eq!(_pystr(b"x\xffy"), "x\\udcffy");
        assert!(_py_codepoints(b"\xfe") < _py_codepoints("\u{e000}".as_bytes()));
        let sum = |v: &[f64]| v.iter().copied().fold(_PySum::default(), _PySum::add).total();
        assert_eq!(sum(&[1e16, 1.0, -1e16]), 1.0);
        assert_eq!(sum(&[]), 0.0);
        assert!(_py_round_int(f64::NAN).is_err());
        assert_ne!(_py_round_int(1e30), _py_round_int(1.5e30));
    }

    #[test]
    fn test_run_timeout_covers_a_grandchild_holding_the_pipe() {
        let start = std::time::Instant::now();
        assert_eq!(run_bytes_within(&["sh", "-c", "sleep 4 & echo hi"], std::time::Duration::from_millis(200)), None);
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
        assert_eq!(_run_bytes(&["sh", "-c", "echo hi"], 5).map(|o| o.0), Some(b"hi\n".to_vec()));
    }

    #[test]
    fn test_python_string_helpers() {
        assert_eq!(_py_splitlines("a\r\nb\rc\n\nd\x0be\u{2028}"), vec!["a", "b", "c", "", "d", "e"]);
        assert_eq!(_py_split(" a\x1cb\t c "), vec!["a", "b", "c"]);
        assert_eq!(_thousands(1234567), "1,234,567");
        assert_eq!(_thousands(999), "999");
        assert_eq!(fmt_size(f64::NAN), "nan KB");
        assert_eq!(_py_repr_bytes(b"it's"), "\"it's\"");
        assert_eq!(_py_repr_bytes(&b"a'b\"\n\xff".to_vec()), "'a\\'b\"\\n\\udcff'");
        assert_eq!(_pathlib_str("/tmp/x//rt/./"), "/tmp/x/rt");
        assert_eq!(_pathlib_str("//a"), "//a");
        assert_eq!(_pathlib_str("///a/.."), "/a/..");
        assert_eq!(_py_float("١٢.٥"), Ok(12.5));
        assert_eq!(
            _py_decode(&b"\xe2\x82A".to_vec()).unwrap_err(),
            "'utf-8' codec can't decode bytes in position 0-1: invalid continuation byte"
        );
        assert_eq!(_py_decode(b"a\r\nb\rc").unwrap(), "a\nb\nc");
    }

    #[test]
    fn test_rmtree_refuses_a_symlink() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("real/pkg")).unwrap();
        std::os::unix::fs::symlink("real", d.path().join("link")).unwrap();
        assert!(_rmtree(&d.path().join("link"), false).unwrap_err().starts_with("[Errno None] None: PosixPath("));
        assert!(d.path().join("link").is_symlink() && d.path().join("real/pkg").is_dir());
        _rmtree(&d.path().join("real"), false).unwrap();
        assert!(!d.path().join("real").exists());
    }
}
