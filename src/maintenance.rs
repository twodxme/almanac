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
    reg.parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
        .join("apps")
}
pub const OK: &str = "ok";
pub const INFO: &str = "info";
pub const WARN: &str = "warn";
pub const CRIT: &str = "crit";
/// A check that could not run (a command missing or timed out, the network
/// down, a check that never finished): not a pass, and not a problem found
/// either. The page shows it as "Couldn't check", never among the passes.
pub const UNKNOWN: &str = "unknown";

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
    /// Several commands, run one after another as one fix.
    Steps(Vec<Vec<String>>),
    Func(FixFn),
}

impl Fix {
    /// Build an argv fix from string literals.
    pub fn argv(args: &[&str]) -> Fix {
        Fix::Argv(args.iter().map(|s| s.to_string()).collect())
    }

    /// Build a fix of several commands from string literals.
    pub fn steps(cmds: &[&[&str]]) -> Fix {
        Fix::Steps(
            cmds.iter()
                .map(|c| c.iter().map(|s| s.to_string()).collect())
                .collect(),
        )
    }

    /// The commands this fix runs, one per step; empty for a callable.
    pub fn commands(&self) -> Vec<Vec<String>> {
        match self {
            Fix::Argv(a) => vec![a.clone()],
            Fix::Steps(s) => s.clone(),
            Fix::Func(_) => Vec::new(),
        }
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
            Fix::Steps(s) => f.debug_tuple("Steps").field(s).finish(),
            Fix::Func(_) => f.write_str("Func(<callable>)"),
        }
    }
}

impl PartialEq for Fix {
    fn eq(&self, other: &Fix) -> bool {
        match (self, other) {
            (Fix::Argv(a), Fix::Argv(b)) => a == b,
            (Fix::Steps(a), Fix::Steps(b)) => a == b,
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
            Fix::Steps(c) => c.serialize(s),
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
            serde_json::Value::Array(items)
                if !items.is_empty() && items.iter().all(|i| i.is_array()) =>
            {
                Ok(Fix::Steps(
                    serde_json::from_value(serde_json::Value::Array(items))
                        .map_err(serde::de::Error::custom)?,
                ))
            }
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

/// What a command printed, and whether it exited 0. Some commands exit
/// non-zero as a matter of course (`find` on directories it may not read,
/// `pacman -Qmq` with nothing foreign), so each check decides what a
/// failed exit means.
#[derive(Debug, Clone, PartialEq)]
struct Ran {
    stdout: String,
    ok: bool,
}

impl Ran {
    /// A run that exited 0 with this output (for tests and stand-ins).
    #[cfg(test)]
    fn ok(stdout: &str) -> Option<Ran> {
        Some(Ran {
            stdout: stdout.into(),
            ok: true,
        })
    }
}

fn _run(cmd: &[&str], timeout: u64) -> Result<Option<Ran>, String> {
    // subprocess.run(..., text=True).stdout. Output that is not UTF-8
    // raises UnicodeDecodeError, which the except clause does not catch:
    // the check fails. None where the command could not run at all (not
    // installed, or timed out): the Python read that as "", which made a
    // check that never ran look like one that found nothing.
    match _run_status(cmd, std::time::Duration::from_secs(timeout)) {
        Some((out, err, ok)) => {
            let out = _py_decode(&out)?;
            _py_decode(&err)?;
            Ok(Some(Ran { stdout: out, ok }))
        }
        None => Ok(None),
    }
}

/// The `_run` a check is handed, so tests and parity can stand in for the
/// commands the way test_app.py patches `maintenance._run`. Err is the
/// exception `_run` lets through (a UnicodeDecodeError); Ok(None) a command
/// that could not run.
type RunFn<'a> = &'a dyn Fn(&[&str], u64) -> Result<Option<Ran>, String>;

/// A finding for a check that could not get its answer: shown as "Couldn't
/// check", with nothing offered to run.
fn _unchecked(id: &str, title: &str, detail: String) -> Finding {
    Finding {
        id: id.into(),
        title: title.into(),
        detail,
        severity: UNKNOWN.into(),
        ..Default::default()
    }
}

/// The device a directory is on, or None if it cannot be read. Walks call it
/// on their root, which may be a symlink (a ~/.cache/yay kept on another
/// disk) and is measured where it points, and on real directories inside.
type DevFn<'a> = &'a dyn Fn(&Path) -> Option<u64>;

fn _dev_of(p: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(p).ok().map(|m| m.dev())
}

/// (megabytes, file count). Walks rather than shelling out to du.
fn _du_mb(path: &Path) -> (f64, i64) {
    _du_mb_on(path, &_dev_of)
}

fn _du_mb_on(path: &Path, dev_of: DevFn<'_>) -> (f64, i64) {
    let mut total: u64 = 0;
    let mut count: i64 = 0;
    if !path.exists() {
        return (0.0, 0);
    }
    // os.walk(path, onerror=lambda _e: None): unreadable directories are
    // skipped; symlinks to directories are listed as directories and not
    // followed; every non-directory entry is lstat'ed. Directories on another
    // filesystem (a network or FUSE mount) are not entered.
    fn walk(
        dir: &Path,
        root_dev: Option<u64>,
        dev_of: DevFn<'_>,
        total: &mut u64,
        count: &mut i64,
    ) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            let is_dir = std::fs::metadata(&p).map(|m| m.is_dir()).unwrap_or(false);
            if is_dir {
                let is_link = entry.file_type().map(|t| t.is_symlink()).unwrap_or(false);
                if !is_link && dev_of(&p) == root_dev {
                    walk(&p, root_dev, dev_of, total, count);
                }
                continue;
            }
            if let Ok(m) = std::fs::symlink_metadata(&p) {
                *total += m.len();
                *count += 1;
            }
        }
    }
    walk(path, dev_of(path), dev_of, &mut total, &mut count);
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
            '\n' | '\r'
                | '\x0b'
                | '\x0c'
                | '\x1c'
                | '\x1d'
                | '\x1e'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
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
static NONPRINTABLE_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^[\p{C}\p{Z}]$").unwrap());

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
            let strerror = msg
                .strip_suffix(&format!(" (os error {code})"))
                .unwrap_or(&msg)
                .to_string();
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
    let parts: Vec<&str> = s
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();
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
        let fname = if is_top {
            format!("PosixPath({})", _py_repr_path(&path))
        } else {
            _py_repr_path(&path)
        };
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
                    let flags = rustix::fs::OFlags::RDONLY
                        | rustix::fs::OFlags::NONBLOCK
                        | rustix::fs::OFlags::CLOEXEC;
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
        Ok(entries) => entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| _name_bytes(p).ends_with(suffix.as_bytes()))
            .collect(),
        Err(_) => Vec::new(),
    };
    out.sort_by_key(|p| _sort_key(p));
    out
}

/// `d.glob("*<suffix>")` in directory order, unsorted.
fn _glob_suffix_unsorted(d: &Path, suffix: &str) -> Vec<PathBuf> {
    match std::fs::read_dir(d) {
        Ok(entries) => entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| _name_bytes(p).ends_with(suffix.as_bytes()))
            .collect(),
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
/// followed. Unlike rglob, it stays on root's filesystem: the runtime
/// directory holds GVFS and portal FUSE mounts, and an SFTP mount there would
/// walk the remote machine from its root (GitHub #4).
fn _rglob_suffix(root: &Path, suffix: &str) -> Vec<PathBuf> {
    _rglob_suffix_on(root, suffix, &_dev_of)
}

fn _rglob_suffix_on(root: &Path, suffix: &str, dev_of: DevFn<'_>) -> Vec<PathBuf> {
    let root_dev = dev_of(root);
    let mut out = _glob_suffix_unsorted(root, suffix);
    let mut stack = vec![root.to_path_buf()];
    while let Some(p) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&p) else {
            continue;
        };
        for entry in entries.flatten() {
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                let ep = entry.path();
                if dev_of(&ep) != root_dev {
                    continue;
                }
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
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
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

/// Every Unicode decimal digit as its ASCII digit; nothing else changes.
fn _ascii_digits(s: &str) -> String {
    s.chars()
        .map(|c| {
            crate::util::decimal_value(c)
                .and_then(|d| char::from_digit(d, 10))
                .unwrap_or(c)
        })
        .collect()
}

/// `float(text)` for what `[\d.]+` matched: Python raises on "." or "1.2.3",
/// and reads any Unicode digits as their ASCII ones.
fn _py_float(text: &str) -> Result<f64, String> {
    _ascii_digits(text).parse::<f64>().map_err(|_| {
        format!(
            "could not convert string to float: {}",
            _py_repr_bytes(text.as_bytes())
        )
    })
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
    digits.parse::<num_bigint::BigUint>().map_err(|_| {
        format!(
            "invalid literal for int() with base 10: {}",
            _py_repr_bytes(text.as_bytes())
        )
    })
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
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
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
                    (
                        n,
                        if bad_start {
                            "invalid start byte"
                        } else {
                            "invalid continuation byte"
                        },
                    )
                }
                None => (b.len() - pos, "unexpected end of data"),
            };
            return Err(if n == 1 {
                format!(
                    "'utf-8' codec can't decode byte 0x{:02x} in position {pos}: {reason}",
                    b[pos]
                )
            } else {
                format!(
                    "'utf-8' codec can't decode bytes in position {pos}-{}: {reason}",
                    pos + n - 1
                )
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
/// stdin is inherited, as subprocess.run leaves it; the limit covers
/// reading the output too, as communicate(timeout=...)'s does.
fn run_bytes_within(cmd: &[&str], timeout: std::time::Duration) -> Option<(Vec<u8>, Vec<u8>)> {
    _run_status(cmd, timeout).map(|(out, err, _ok)| (out, err))
}

/// `run_bytes_within`, with whether the command exited 0.
fn _run_status(cmd: &[&str], timeout: std::time::Duration) -> Option<(Vec<u8>, Vec<u8>, bool)> {
    let out = crate::util::run_bytes(
        cmd,
        &crate::util::RunOpts {
            timeout: Some(timeout),
            stdin: crate::util::Stdin::Inherit,
            ..Default::default()
        },
    );
    let ok = out.ok();
    (!out.failed_to_start && !out.timed_out).then_some((out.stdout, out.stderr, ok))
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
        let name = if dir.is_empty() {
            PathBuf::from(cmd)
        } else {
            d.join(cmd)
        };
        let ok = name.exists()
            && rustix::fs::access(&name, rustix::fs::Access::EXEC_OK).is_ok()
            && !name.is_dir();
        if ok {
            return Some(name);
        }
    }
    None
}

/// The finding `run_all` builds when a check raises. It got no answer, so
/// it is "Couldn't check", not a pass (the Python marked it info).
fn _failed(name: &str, exc: &str) -> Finding {
    Finding {
        id: name.to_string(),
        title: crate::util::py_title(&name.replace('_', " ")),
        detail: format!("Check failed: {exc}"),
        severity: UNKNOWN.to_string(),
        ..Default::default()
    }
}

/// The finding `run_all` builds for a check still running at its deadline.
fn _unfinished(name: &str, after: std::time::Duration) -> Finding {
    Finding {
        id: name.to_string(),
        title: crate::util::py_title(&name.replace('_', " ")),
        detail: format!(
            "This check did not finish within {} seconds, so the others went \
             ahead without it. Something it reads is not answering: often a \
             network share, or a process stuck waiting on one.",
            after.as_secs()
        ),
        severity: UNKNOWN.to_string(),
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
static JOURNAL_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"([\d.]+)([KMGT])").unwrap());
static TOTAL_FILES_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(\d+) total files").unwrap());

/// What a dry run with `flags` would free, in MB; None where paccache could
/// not run (pacman-contrib not installed, or it timed out).
fn _paccache_reclaimable_with(flags: &[&str], run: RunFn<'_>) -> Result<Option<f64>, String> {
    let mut cmd = vec!["paccache", "-d"];
    cmd.extend_from_slice(flags);
    let Some(out) = run(&cmd, 60)? else {
        return Ok(None);
    };
    let Some(m) = SAVED_RE.captures(&out.stdout) else {
        return Ok(Some(0.0)); // "no candidate packages found"
    };
    let mult = match &m[2] {
        "" => 1.0 / 1048576.0,
        "K" => 1.0 / 1024.0,
        "M" => 1.0,
        "G" => 1024.0,
        _ => 1048576.0, // "T"
    };
    Ok(Some(_py_float(&m[1])? * mult))
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
    let (Some(old), Some(gone)) = (
        _paccache_reclaimable_with(&["-k2"], run)?,
        _paccache_reclaimable_with(&["-uk0"], run)?,
    ) else {
        return Ok(Finding {
            size_mb: 0.0,
            count: n,
            .._unchecked(
                "pacman-cache",
                "Package cache",
                format!(
                    "{} in {n} downloaded packages. How much a trim would free \
                     could not be worked out: paccache (from pacman-contrib) \
                     did not run.",
                    fmt_size(mb)
                ),
            )
        });
    };
    let free = old + gone;
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
        fix: Some(paccache_trim_fix()),
        fix_note: "Keeps the 2 most recent versions of installed packages and \
                   removes cached packages that are no longer installed."
            .into(),
        ..Default::default()
    })
}

/// The package cache trimmed to the two newest versions of what is
/// installed: the package cache check's fix, also run after Update all.
pub fn paccache_trim_fix() -> Fix {
    // -rk2 keeps the two newest of each package; then -ruk0 drops every
    // cached version of packages no longer installed, which can never be
    // downgraded to anyway. Two runs: given together, the later -k0 and
    // -u override -k2, and installed packages are never trimmed.
    Fix::steps(&[&["paccache", "-rk2"], &["paccache", "-ruk0"]])
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

/// The AUR build cache's fix for just `names` (each a package base yay
/// keeps a folder for), or None when none of them has one. A name that is
/// not a plain package name is never joined onto the path.
pub fn yay_cache_fix_for(names: &[String]) -> Option<Fix> {
    _yay_cache_fix_in(&YAY_CACHE, names)
}

fn _yay_cache_fix_in(cache: &Path, names: &[String]) -> Option<Fix> {
    let plain = |n: &str| {
        !n.is_empty()
            && !n.starts_with('.')
            && n.chars()
                .all(|c| c.is_ascii_alphanumeric() || "@._+-".contains(c))
    };
    let dirs: Vec<PathBuf> = names
        .iter()
        .filter(|n| plain(n))
        .map(|n| cache.join(n))
        .filter(|d| std::fs::symlink_metadata(d).is_ok_and(|m| m.is_dir()))
        .collect();
    if dirs.is_empty() {
        return None;
    }
    Some(Fix::func(move |log| {
        for d in &dirs {
            _clear_yay_cache_at(d, log)?;
        }
        Ok(())
    }))
}

fn _clear_yay_cache_at(cache: &Path, log: LogFn<'_>) -> Result<(), String> {
    if !cache.exists() {
        log("nothing to clear");
        return Ok(());
    }
    let (mb, n) = _du_mb(cache);
    log(&format!(
        "removing {} ({}, {n} files)",
        cache.display(),
        fmt_size(mb)
    ));
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
    _journal_with(&_run)
}

fn _journal_with(run: RunFn<'_>) -> Result<Finding, String> {
    let out = run(&["journalctl", "--disk-usage"], 60)?;
    match out {
        Some(r) if r.ok || JOURNAL_RE.is_match(&r.stdout) => _journal_from(&r.stdout),
        // Not installed, timed out, or refused: "0 B of logs" would be a guess.
        _ => Ok(_unchecked(
            "journal",
            "Systemd journal",
            "Could not ask journalctl how much the journal holds.".into(),
        )),
    }
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
        detail: format!(
            "{} in {n} deleted files still taking up space.",
            fmt_size(mb)
        ),
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
    let files = if apps.exists() {
        _glob_suffix(apps, ".previous")
    } else {
        Vec::new()
    };
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
        fix: if files.is_empty() {
            None
        } else {
            Some(Fix::func(move |log| _delete_rollbacks_in(&at, log)))
        },
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
            Err(exc) => log(&format!(
                "could not remove {}: {}",
                _name(&f),
                _oserror(&exc, &f)
            )),
        }
    }
    Ok(())
}

pub fn failed_units() -> Finding {
    _raise(_failed_units_with(&_run))
}

fn _failed_units_with(run: RunFn<'_>) -> Result<Finding, String> {
    let mut rows: Vec<String> = Vec::new();
    // The scopes systemd did not answer for: not running, its bus not
    // reachable, or too slow. "No failed units" is only said of the ones
    // that answered.
    let mut unread: Vec<&str> = Vec::new();
    for scope in [&[][..], &["--user"][..]] {
        let mut cmd = vec!["systemctl"];
        cmd.extend_from_slice(scope);
        cmd.extend_from_slice(&["--failed", "--no-legend", "--plain", "--no-pager"]);
        let out = match run(&cmd, 60)? {
            Some(r) if r.ok || !_py_strip(&r.stdout).is_empty() => r.stdout,
            _ => {
                unread.push(if scope.is_empty() { "system" } else { "user" });
                continue;
            }
        };
        for line in _py_splitlines(&out) {
            let name = _py_split(line).first().copied().unwrap_or("");
            if !name.is_empty() {
                rows.push(format!(
                    "{}{name}",
                    if scope.is_empty() { "system " } else { "user " }
                ));
            }
        }
    }
    let any = !rows.is_empty();
    let unread_note = match unread.as_slice() {
        [] => "",
        ["system"] => " System services could not be checked: systemd did not answer.",
        [_] => " Your own services could not be checked: systemd did not answer for them.",
        _ => " Neither system nor user services could be checked: systemd did not answer.",
    };
    if !any && !unread.is_empty() {
        let detail = if unread.len() == 2 {
            _py_strip(unread_note).to_string()
        } else {
            format!("No failed units among the ones read.{unread_note}")
        };
        return Ok(_unchecked("failed-units", "Failed services", detail));
    }
    Ok(Finding {
        id: "failed-units".into(),
        title: "Failed services".into(),
        detail: if any {
            format!(
                "Units that exited with an error and were not restarted. \
                 A failed unit is something that was supposed to be running.{unread_note}"
            )
        } else {
            "No failed units.".into()
        },
        severity: if any { WARN } else { OK }.into(),
        count: rows.len() as i64,
        items: rows,
        fix_label: "Clear failed state".into(),
        // reset-failed clears the flag; it does not hide a unit that fails
        // again, so the finding comes straight back if the cause remains.
        fix: if any {
            Some(Fix::argv(&["systemctl", "--user", "reset-failed"]))
        } else {
            None
        },
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
    // One walk of /etc for both. find exits 1 there as a matter of course
    // (root-only directories), so only a find that did not run is no answer.
    let pattern = [
        "find",
        "/etc",
        "(",
        "-name",
        "*.pacnew",
        "-o",
        "-name",
        "*.pacsave",
        ")",
        "-type",
        "f",
    ];
    let Some(out) = run(&pattern, 40)? else {
        return Ok(_unchecked(
            "pacnew",
            "Unmerged config files",
            "Could not look through /etc: find did not finish.".into(),
        ));
    };
    let mut files: Vec<String> = _py_splitlines(&out.stdout)
        .into_iter()
        .filter(|f| !_py_strip(f).is_empty())
        .map(String::from)
        .collect();
    // .pacnew files first, then .pacsave, as the two walks listed them.
    files.sort_by_key(|f| !f.ends_with(".pacnew"));
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
    let bad: Vec<String> = _dangling_rows(data_home)?
        .iter()
        .map(|r| _pystr(r))
        .collect();
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
        fix: if any {
            Some(Fix::func(move |log| _remove_dangling_in(&at, log)))
        } else {
            None
        },
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
            // f.read_text(errors="replace"); OSError skips the file, and so
            // does anything that is not a regular file.
            let Some(bytes) = _read_regular(&f) else {
                continue;
            };
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
            let target = _py_strip(&target)
                .trim_matches('"')
                .trim_matches('\'')
                .to_string();
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

/// The bytes of `p` when it is a regular file (a symlink to one counts:
/// launcher entries often are), None otherwise. A FIFO named like a
/// .desktop file would hold the read, and the whole Health check, forever,
/// and a device may never end or may act on being opened: the type is
/// checked before opening, and again on what was opened, without blocking.
fn _read_regular(p: &Path) -> Option<Vec<u8>> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    if !std::fs::metadata(p).ok()?.is_file() {
        return None;
    }
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32)
        .open(p)
        .ok()?;
    if !f.metadata().ok()?.is_file() {
        return None;
    }
    let mut out = Vec::new();
    f.read_to_end(&mut out).ok()?;
    Some(out)
}

fn _remove_dangling_in(data_home: &Path, log: LogFn<'_>) -> Result<(), String> {
    use std::os::unix::ffi::OsStrExt;
    let found = _dangling_rows(data_home)?;
    let d = data_home.join("applications");
    for row in &found {
        // row.split(" → ")[0], on the bytes the str stands for.
        let sep = " → ".as_bytes();
        let cut = row
            .windows(sep.len())
            .position(|w| w == sep)
            .unwrap_or(row.len());
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
                bad.push(format!(
                    "{} → {}",
                    _name(&f),
                    _pystr(link.as_os_str().as_bytes())
                ));
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
        fix: if any {
            Some(Fix::func(move |log| _remove_broken_links_in(&at, log)))
        } else {
            None
        },
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
                Err(exc) => log(&format!(
                    "could not remove {}: {}",
                    _name(&f),
                    _oserror(&exc, &f)
                )),
            }
        }
    }
    Ok(())
}

pub fn orphan_packages() -> Finding {
    _orphan_packages_from(&crate::inventory::orphans())
}

fn _orphan_packages_from(apps: &[crate::inventory::App]) -> Finding {
    let keep =
        |a: &crate::inventory::App| a.extra.get("advice").and_then(|v| v.as_str()) == Some("keep");
    let safe: Vec<String> = apps
        .iter()
        .filter(|a| !keep(a))
        .map(|a| a.name.clone())
        .collect();
    let kept: Vec<String> = apps
        .iter()
        .filter(|a| keep(a))
        .map(|a| a.name.clone())
        .collect();
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
        fix: if safe.is_empty() {
            None
        } else {
            Some(Fix::Argv(fix_argv))
        },
        items: safe,
        fix_label: "Review and remove".into(),
        fix_note: "Removes only the packages listed above.".into(),
        ..Default::default()
    }
}

/// Foreign packages the AUR no longer carries, or that are flagged.
pub fn unmaintained_aur() -> Finding {
    // The foreign list is the one the inventory keeps while pacman's
    // databases stand still (reading it means reading every sync database),
    // and the AUR's answer is the one the inventory's scan already asked.
    let run = |cmd: &[&str], timeout: u64| -> Result<Option<Ran>, String> {
        if cmd == ["pacman", "-Qmq"] {
            return crate::inventory::foreign_packages().map(|names| {
                Some(Ran {
                    stdout: names.join("\n"),
                    ok: true,
                })
            });
        }
        _run(cmd, timeout)
    };
    _raise(_unmaintained_aur_with(
        &run,
        &crate::inventory::aur_health_known,
    ))
}

/// What the AUR is known to say about some names (see `aur_health_known`).
type AurKnown<'a> = &'a dyn Fn(&[String]) -> Result<Option<crate::inventory::AurAnswer>, String>;

fn _unmaintained_aur_with(run: RunFn<'_>, aur_health: AurKnown<'_>) -> Result<Finding, String> {
    // pacman -Qmq exits 1 when nothing is foreign: only one that did not run
    // is no answer.
    let Some(listed) = run(&["pacman", "-Qmq"], 60)? else {
        return Ok(_unchecked(
            "aur-health",
            "AUR packages",
            "Could not ask pacman which packages came from the AUR.".into(),
        ));
    };
    let names: Vec<String> = _py_split(&listed.stdout)
        .into_iter()
        .map(String::from)
        .collect();
    if names.is_empty() {
        return Ok(Finding {
            id: "aur-health".into(),
            title: "AUR packages".into(),
            detail: "No foreign packages installed.".into(),
            severity: OK.into(),
            ..Default::default()
        });
    }
    // No answer (offline, rate-limited, a reply that made no sense) is not
    // "every package left the AUR": those names are not checked at all.
    let Some(health) = aur_health(&names).ok().flatten() else {
        return Ok(_unchecked(
            "aur-health",
            "AUR package health",
            format!(
                "The AUR could not be reached, so {} AUR package(s) could not be \
                 checked. They are checked again the next time it can be.",
                names.len()
            ),
        ));
    };
    let mut gone: Vec<String> = Vec::new();
    let mut flagged: Vec<String> = Vec::new();
    let mut unasked: Vec<String> = Vec::new();
    for n in &names {
        match health.results.get(n) {
            // Asked about and not answered for: the AUR no longer has it. A
            // name the answer never asked about is one it cannot speak for.
            None if !health.asked.contains(n) => unasked.push(n.clone()),
            None => gone.push(n.clone()),
            Some(h) if h.out_of_date => flagged.push(n.clone()),
            Some(h) if h.maintainer.is_empty() => flagged.push(format!("{n} (orphaned upstream)")),
            Some(_) => {}
        }
    }
    let mut rows: Vec<String> = gone
        .iter()
        .map(|n| format!("{n} — no longer in the AUR"))
        .collect();
    rows.extend(flagged.iter().cloned());
    let unasked_note = if unasked.is_empty() {
        String::new()
    } else {
        format!(
            " {} could not be checked: the AUR could not be reached.",
            unasked.len()
        )
    };
    if rows.is_empty() && !unasked.is_empty() {
        let detail = format!(
            "The AUR could not be reached, so {} of {} AUR packages could not be \
             checked. The rest are still listed and maintained.",
            unasked.len(),
            names.len()
        );
        return Ok(Finding {
            count: unasked.len() as i64,
            items: unasked
                .iter()
                .map(|n| format!("{n} — not checked"))
                .collect(),
            .._unchecked("aur-health", "AUR package health", detail)
        });
    }
    rows.extend(unasked.iter().map(|n| format!("{n} — not checked")));
    Ok(Finding {
        id: "aur-health".into(),
        title: "AUR package health".into(),
        detail: if !rows.is_empty() {
            format!(
                "{} package(s) are installed from the AUR but no \
                 longer listed there, and {} are flagged \
                 out-of-date or have no maintainer. Neither will ever \
                 update again on its own.{unasked_note}",
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
        count: (rows.len() - unasked.len()) as i64,
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
    // None: pacman did not run or did not finish, which is no answer.
    let out = match _pacman_qk() {
        Some((o, e)) => _py_decode(&o)? + &_py_decode(&e)?,
        None => String::new(),
    };
    _package_integrity_from(&out)
}

/// `pacman -Qk`'s (stdout, stderr), None where it could not run. The check
/// is a stat of every file pacman installed, which one pacman does one at a
/// time; split over the installed packages in their own order, a few run
/// side by side and their outputs joined in that order read exactly as the
/// single run does, in a third of the time.
fn _pacman_qk() -> Option<(Vec<u8>, Vec<u8>)> {
    let listed = _run_bytes(&["pacman", "-Qq"], 60)
        .map(|(o, _)| o)
        .unwrap_or_default();
    let names: Vec<&str> = std::str::from_utf8(&listed)
        .unwrap_or("")
        .split_whitespace()
        .collect();
    let parts = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(4);
    // Anything that is not plainly a package name, too few to be worth it,
    // or one core: the single run.
    if parts < 2 || names.len() < 64 || names.iter().any(|n| n.starts_with('-')) {
        return _run_bytes(&["pacman", "-Qk"], 240);
    }
    let outs: Vec<Option<(Vec<u8>, Vec<u8>)>> = std::thread::scope(|s| {
        let handles: Vec<_> = names
            .chunks(names.len().div_ceil(parts))
            .map(|chunk| {
                s.spawn(move || {
                    let mut argv = vec!["pacman", "-Qk"];
                    argv.extend_from_slice(chunk);
                    _run_bytes(&argv, 240)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().ok().flatten())
            .collect()
    });
    let (mut out, mut err) = (Vec::new(), Vec::new());
    for part in outs {
        let (o, e) = part?;
        out.extend(o);
        err.extend(e);
    }
    Some((out, err))
}

fn _package_integrity_from(out: &str) -> Result<Finding, String> {
    if _py_strip(out).is_empty() {
        // Not a pass: nothing was checked.
        return Ok(_unchecked(
            "integrity",
            "Package files",
            "Could not run the integrity check: pacman did not answer in time.".into(),
        ));
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
        detail: format!(
            "All {} readable files are where their packages put them.{note}",
            _thousands_big(&checked)?
        ),
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

/// `shutil.disk_usage` for a path: (free MB, total MB, percent used).
type DiskUsage<'a> = &'a dyn Fn(&str) -> Option<(f64, f64, f64)>;

fn _disk_space_with(usage: DiskUsage<'_>) -> Result<Finding, String> {
    use std::os::unix::fs::MetadataExt;

    let mut watched: Vec<(String, String)> = vec![("/".into(), "/".into())];
    for extra in [
        "/boot".to_string(),
        "/var".into(),
        "/home".into(),
        _runtime_dir(),
    ] {
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
        let Some((free, total, pct)) = usage(path) else {
            continue;
        };
        // btrfs subvolumes report distinct device ids while sharing one pool,
        // so /, /home and /var would otherwise be listed three times with
        // identical figures.
        let fingerprint = (_py_round_int(total)?, _py_round_int(free)?);
        if seen.contains(&fingerprint) {
            continue;
        }
        seen.push(fingerprint);
        rows.push(format!(
            "{label}  {} free of {} ({}% used)",
            fmt_size(free),
            fmt_size(total),
            _ff(pct, 0)
        ));
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

    if rows.is_empty() {
        return Ok(_unchecked(
            "disk",
            "Disk space",
            "Could not read filesystem stats.".into(),
        ));
    }
    let root = usage("/").unwrap_or((0.0, 0.0, 0.0));
    let mut detail = rows.join(" · ");
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
        let Ok(m) = std::fs::metadata(&f) else {
            continue;
        };
        let size = m.len() as f64 / 1048576.0;
        if size > 100.0 {
            let shown = _path_str(&f);
            // Path(".") / name is spelled "name".
            let shown = if root == "." {
                shown.strip_prefix("./").unwrap_or(&shown).to_string()
            } else {
                shown
            };
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
            format!(
                "Session logs are negligible ({runtime} is {}% full).",
                _ff(pct, 0)
            )
        },
        severity: sev.into(),
        size_mb: total_mb,
        items,
        fix_label: if some { "Clear old session logs" } else { "" }.into(),
        fix: if some {
            Some(Fix::func(move |log| {
                _clear_runtime_logs_in(
                    &at,
                    _hyprctl_instances().as_deref(),
                    _current_instance().as_deref(),
                    crate::util::now(),
                    log,
                )
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

/// `hyprctl instances -j` stdout, or None when it cannot run, fails, or
/// prints what cannot be read. (The Python read every failure as "no
/// instance is running", and so unlinked the running compositor's log.)
fn _hyprctl_instances() -> Option<String> {
    match _run_status(
        &["hyprctl", "instances", "-j"],
        std::time::Duration::from_secs(20),
    ) {
        Some((out, err, true)) => match (_py_decode(&out), _py_decode(&err)) {
            (Ok(o), Ok(_)) => Some(o),
            _ => None,
        },
        _ => None,
    }
}

/// The Hyprland instance this session runs in, which is live whatever
/// hyprctl says.
fn _current_instance() -> Option<std::ffi::OsString> {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").filter(|s| !s.is_empty())
}

fn _clear_runtime_logs(log: LogFn<'_>) -> Result<(), String> {
    let runtime = PathBuf::from(_runtime_dir());
    _clear_runtime_logs_in(
        &runtime,
        _hyprctl_instances().as_deref(),
        _current_instance().as_deref(),
        crate::util::now(),
        log,
    )
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
    let int_max_digits = match std::env::var("PYTHONINTMAXSTRDIGITS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
    {
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
                            if (0xd800..0xdc00).contains(&u)
                                && b.get(i) == Some(&b'\\')
                                && b.get(i + 1) == Some(&b'u')
                            {
                                if let Some(u2) =
                                    hex4(i + 2).filter(|u2| (0xdc00..0xe000).contains(u2))
                                {
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
                    if dict {
                        V::EmptyDict
                    } else {
                        V::List
                    }
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
                    stack.push(Frame {
                        dict,
                        key,
                        instance: None,
                    });
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
/// printed (None: it could not be asked), this session's instance, and the
/// time now.
fn _clear_runtime_logs_in(
    runtime: &Path,
    instances: Option<&str>,
    current: Option<&std::ffi::OsStr>,
    now: f64,
    log: LogFn<'_>,
) -> Result<(), String> {
    // live = {i.get("instance") for i in json.loads(out.stdout or "[]")}.
    // When that cannot be read every instance may be live: a live log is
    // truncated, never unlinked, since unlinking frees nothing while the
    // compositor still writes to it.
    let live: Option<Vec<Vec<u32>>> =
        instances.and_then(|t| _live_instances(if t.is_empty() { "[]" } else { t }));

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
        let is_live = live.as_ref().is_none_or(|l| l.contains(&_sort_key(&d)))
            || current.is_some_and(|c| d.file_name() == Some(c));
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
                let why = if live.is_some() {
                    "live session"
                } else {
                    "its session may still be running"
                };
                log(&format!(
                    "truncated {} ({}, {why})",
                    _name(&f),
                    fmt_size(size)
                ));
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
            if !d.is_dir() || quickshell_running(&d) {
                continue;
            }
            if newest_mtime(&d).is_some_and(|t| t < cutoff) {
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

/// Whether the quickshell instance in `d` is still running: a running one
/// holds a POSIX lock on its instance.lock, which /proc/locks lists without
/// anyone taking it. Unreadable /proc/locks counts as running, so a live
/// shell's socket is never what gets removed.
fn quickshell_running(d: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(m) = std::fs::metadata(d.join("instance.lock")) else {
        return false;
    };
    quickshell_locked(
        m.ino(),
        std::fs::read_to_string("/proc/locks").ok().as_deref(),
    )
}

fn quickshell_locked(ino: u64, locks: Option<&str>) -> bool {
    let Some(locks) = locks else { return true };
    // "37: POSIX  ADVISORY  WRITE 2498825 00:54:9694 0 EOF": maj:min:inode.
    locks.lines().any(|l| {
        l.split_whitespace()
            .any(|w| w.rsplit(':').next() == Some(&ino.to_string()) && w.contains(':'))
    })
}

/// When anything in `d` last changed: the folder's own mtime moves only when
/// entries are added or removed, not while a log in it is written.
fn newest_mtime(d: &Path) -> Option<f64> {
    use std::os::unix::fs::MetadataExt;
    // st_mtime: seconds + nanoseconds * 1e-9, negative before 1970.
    let at = |m: &std::fs::Metadata| m.mtime() as f64 + m.mtime_nsec() as f64 * 1e-9;
    let mut newest = at(&std::fs::metadata(d).ok()?);
    for e in std::fs::read_dir(d).ok()?.flatten() {
        if let Ok(m) = e.path().symlink_metadata() {
            newest = newest.max(at(&m));
        }
    }
    Some(newest)
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

pub fn restart_needed() -> Finding {
    crate::restart::finding()
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
    ("restart_needed", restart_needed),
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
    ("restart-needed", restart_needed),
];

/// `CHECK_BY_ID.get(finding_id)`. None in demo mode, whose findings are
/// fixtures: asking a check again would read the real machine.
pub fn check_by_id(finding_id: &str) -> Option<Check> {
    if crate::demo::active() {
        return None;
    }
    CHECK_BY_ID
        .iter()
        .find(|(id, _)| *id == finding_id)
        .map(|(_, f)| *f)
}

/// The check behind `finding_id` run again the way `run_all` runs it:
/// guarded, and given until its deadline. None when there is no such check
/// (or in demo mode).
pub fn recheck_by_id(finding_id: &str) -> Option<Finding> {
    let check = check_by_id(finding_id)?;
    _run_checks(&[(finding_id, check)]).pop()
}

/// Every check. Read-only; safe to run on a timer.
pub fn run_all() -> Vec<Finding> {
    if crate::demo::active() {
        let mut found = crate::demo::findings();
        _sort_findings(&mut found);
        return found;
    }
    let generation = LAST_RUN_GENERATION.load(std::sync::atomic::Ordering::SeqCst);
    let found = _run_checks(CHECKS);
    let mut kept = LAST_RUN.lock().unwrap_or_else(|e| e.into_inner());
    // A run that began before `forget_last_run` describes the system before
    // whatever changed it: it is still returned to its caller, not kept.
    if LAST_RUN_GENERATION.load(std::sync::atomic::Ordering::SeqCst) == generation {
        *kept = Some((std::time::Instant::now(), found.clone()));
    }
    found
}

/// Bumped by `forget_last_run`.
static LAST_RUN_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The last `run_all` in this process, with when it finished.
static LAST_RUN: std::sync::Mutex<Option<(std::time::Instant, Vec<Finding>)>> =
    std::sync::Mutex::new(None);

/// What the last `run_all` in this process found, and how long ago: Today
/// and Health show the same checks, and the page opened second can show
/// the first one's answer at once rather than ask every check again.
pub fn last_run() -> Option<(std::time::Duration, Vec<Finding>)> {
    LAST_RUN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|(at, found)| (at.elapsed(), found.clone()))
}

/// Drop the kept answer: something just changed the system (an update, a
/// fix), so the next page to ask runs the checks again.
pub fn forget_last_run() {
    let mut kept = LAST_RUN.lock().unwrap_or_else(|e| e.into_inner());
    LAST_RUN_GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    *kept = None;
}

/// How long a `last_run` answer stands in for a new run.
pub const FINDINGS_FRESH: std::time::Duration = std::time::Duration::from_secs(300);

/// "checked just now", "checked 1 min ago", "checked 12 min ago".
pub fn checked_ago(age: std::time::Duration) -> String {
    match age.as_secs() / 60 {
        0 => "checked just now".to_string(),
        m @ 1..60 => format!("checked {m} min ago"),
        m => format!("checked {} h ago", m / 60),
    }
}

/// How long `run_all` waits for a check: its own commands' time limits
/// with room to spare. Past it the check is left running on its own and
/// reported as not finished, so one stuck read (a network share, a FIFO, a
/// process hung on either) cannot hold back every other answer. Takes the
/// check's name or its finding id (`recheck` has only the id).
fn _deadline(name: &str) -> std::time::Duration {
    let secs = match name {
        // pacman -Qq (60 s), then pacman -Qk (240 s) over every installed file.
        "package_integrity" | "integrity" => 330,
        // A walk of the cache, then two paccache dry runs of up to 60 s each.
        "pacman_cache" | "pacman-cache" => 180,
        // Two systemctl calls of up to 60 s each.
        "failed_units" | "failed-units" => 150,
        // pacman, then the network.
        "unmaintained_aur"
        | "aur-health"
        | "known_vulnerabilities"
        | "vulnerabilities"
        | "orphan_packages"
        | "orphans" => 120,
        // One command of up to 60 s, or reading files.
        _ => 90,
    };
    std::time::Duration::from_secs(secs)
}

/// `checks`, each guarded, all at once, each given until its deadline;
/// sorted by severity, then size.
fn _run_checks(checks: &[(&str, Check)]) -> Vec<Finding> {
    _run_checks_within(checks, &_deadline)
}

/// One check, guarded: `except Exception` around it. A check that fails
/// (here, one that panics) becomes a finding saying so instead of losing the
/// rest. Quiet: the failure is reported as a finding, not on stderr. QUIET
/// is per thread, so each check sets it on the thread it runs on.
fn _guarded(name: &str, check: Check) -> Finding {
    _quiet_panics();
    QUIET.with(|q| q.set(true));
    let caught = std::panic::catch_unwind(check);
    QUIET.with(|q| q.set(false));
    match caught {
        Ok(f) => f,
        Err(payload) => {
            let exc = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            _failed(name, &exc)
        }
    }
}

fn _run_checks_within(
    checks: &[(&str, Check)],
    deadline: &dyn Fn(&str) -> std::time::Duration,
) -> Vec<Finding> {
    use std::sync::mpsc::RecvTimeoutError;
    // Every check at once: they only read, and most of the time is spent
    // waiting on pacman, the disk or the network, so the whole run takes as
    // long as its slowest check rather than all of them added up. Gathered
    // in `checks` order, so the sort below sees what a sequential run gave it.
    // Not scoped: a check that does not finish by its deadline has to be
    // left behind, still running, and a scope would wait for it.
    let started = std::time::Instant::now();
    let caller = std::thread::current().name().map(str::to_owned);
    let (tx, rx) = std::sync::mpsc::channel::<(usize, Finding)>();
    let mut got: Vec<Option<Finding>> = vec![None; checks.len()];
    for (i, (name, check)) in checks.iter().enumerate() {
        // The caller's name and the stack the main thread would have had:
        // some checks recurse as deep as the files they read, and their
        // limits depend on which thread they run on (`util::with_stack` does
        // the same).
        let mut b = std::thread::Builder::new().stack_size(8 << 20);
        if let Some(n) = &caller {
            b = b.name(n.clone());
        }
        let (tx, owned, check) = (tx.clone(), name.to_string(), *check);
        if b.spawn(move || {
            // Nobody is listening once the run has moved on without it.
            let _ = tx.send((i, _guarded(&owned, check)));
        })
        .is_err()
        {
            // No thread to be had: run it here.
            got[i] = Some(_guarded(name, check));
        }
    }
    drop(tx);
    loop {
        let waited = started.elapsed();
        let mut next: Option<std::time::Duration> = None;
        for (i, (name, _)) in checks.iter().enumerate() {
            if got[i].is_some() {
                continue;
            }
            let limit = deadline(name);
            if waited >= limit {
                got[i] = Some(_unfinished(name, limit));
            } else {
                next = Some(next.map_or(limit, |n| n.min(limit)));
            }
        }
        let Some(next) = next else { break };
        match rx.recv_timeout(next - waited) {
            Ok((i, f)) => {
                got[i].get_or_insert(f);
            }
            Err(RecvTimeoutError::Timeout) => {}
            // Every thread is gone and these never said: lost, as a check
            // whose thread died was before.
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    let mut out: Vec<Finding> = got
        .into_iter()
        .zip(checks)
        .map(|(f, (name, _))| f.unwrap_or_else(|| _failed(name, "")))
        .collect();
    _sort_findings(&mut out);
    out
}

/// Worst first, then the most space; equal ones keep their order. A check
/// that could not run sits below every finding and above the passes.
fn _sort_findings(out: &mut [Finding]) {
    let rank = |s: &str| match s {
        CRIT => 0,
        WARN => 1,
        INFO => 2,
        UNKNOWN => 3,
        _ => 4,
    };
    out.sort_by(|a, b| {
        rank(&a.severity).cmp(&rank(&b.severity)).then(
            (-a.size_mb)
                .partial_cmp(&-b.size_mb)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
    });
}

/// How much space the actionable cleanups would actually free.
pub fn reclaimable_mb(findings: &[Finding]) -> f64 {
    const FREES: [&str; 6] = [
        "pacman-cache",
        "yay-cache",
        "journal",
        "trash",
        "rollbacks",
        "runtime-logs",
    ];
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
        if self.c != 0.0 && self.c.is_finite() {
            self.f + self.c
        } else {
            self.f
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_running_shell_session_is_never_old() {
        let locks = "36: FLOCK  ADVISORY  WRITE 1505 00:1c:812 0 EOF\n\
                     37: POSIX  ADVISORY  WRITE 2498825 00:54:9694 0 EOF\n";
        assert!(quickshell_locked(9694, Some(locks)));
        assert!(!quickshell_locked(969, Some(locks)), "a different inode");
        assert!(
            !quickshell_locked(54, Some(locks)),
            "a device number is not an inode"
        );
        assert!(
            quickshell_locked(1, None),
            "unreadable /proc/locks keeps it"
        );

        // Days of uptime: the folder's own mtime stays at the session's
        // start while its log is written, so age is the newest file inside.
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("log.log"), "x").unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(10 * 86400);
        std::fs::File::open(d.path())
            .unwrap()
            .set_modified(old)
            .unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        assert!(newest_mtime(d.path()).unwrap() > now - 60.0);
    }

    // ---- Running the checks together ----
    fn slow_ok() -> Finding {
        std::thread::sleep(std::time::Duration::from_millis(300));
        Finding {
            id: "slow".into(),
            severity: OK.into(),
            ..Default::default()
        }
    }
    fn big_warn() -> Finding {
        Finding {
            id: "big".into(),
            severity: WARN.into(),
            size_mb: 900.0,
            ..Default::default()
        }
    }
    fn small_warn() -> Finding {
        Finding {
            id: "small".into(),
            severity: WARN.into(),
            size_mb: 1.0,
            ..Default::default()
        }
    }
    fn tie_a() -> Finding {
        std::thread::sleep(std::time::Duration::from_millis(100));
        Finding {
            id: "tie-a".into(),
            severity: INFO.into(),
            ..Default::default()
        }
    }
    fn tie_b() -> Finding {
        Finding {
            id: "tie-b".into(),
            severity: INFO.into(),
            ..Default::default()
        }
    }
    fn raises() -> Finding {
        _raise(Err("boom".into()))
    }

    #[test]
    fn checks_run_together_but_come_back_in_the_sequential_order() {
        let checks: &[(&str, Check)] = &[
            ("slow_ok", slow_ok),
            ("tie_a", tie_a),
            ("small_warn", small_warn),
            ("raises_here", raises),
            ("tie_b", tie_b),
            ("big_warn", big_warn),
            ("slow_ok", slow_ok),
        ];
        let t = std::time::Instant::now();
        let out = _run_checks(checks);
        assert!(
            t.elapsed() < std::time::Duration::from_millis(550),
            "the two slow checks overlap"
        );
        let ids: Vec<&str> = out.iter().map(|f| f.id.as_str()).collect();
        // Severity, then size; equal ones keep the order of `checks`
        // whichever finished first -- tie_a sleeps, tie_b does not. One that
        // raised could not check, which is below every finding and above
        // the passes.
        assert_eq!(
            ids,
            [
                "big",
                "small",
                "tie-a",
                "tie-b",
                "raises_here",
                "slow",
                "slow"
            ]
        );
        let failed = &out[4];
        assert_eq!(
            failed.detail, "Check failed: boom",
            "a check that raises is a finding, not a lost run"
        );
        assert_eq!(failed.severity, UNKNOWN, "and not a pass");
    }

    #[test]
    fn how_long_ago_findings_were_checked_reads_plainly() {
        let s = std::time::Duration::from_secs;
        assert_eq!(checked_ago(s(20)), "checked just now");
        assert_eq!(checked_ago(s(61)), "checked 1 min ago");
        assert_eq!(checked_ago(s(59 * 60 + 59)), "checked 59 min ago");
        assert_eq!(checked_ago(s(2 * 3600 + 5)), "checked 2 h ago");
    }

    /// `_pacman_qk`, split, reads exactly as one `pacman -Qk` on this
    /// machine. Read-only; opt in with `cargo test -- --ignored`.
    #[test]
    #[ignore = "stats every file pacman installed on this machine"]
    fn the_split_integrity_check_matches_a_single_run_here() {
        let single = _run_bytes(&["pacman", "-Qk"], 240).unwrap();
        assert_eq!(_pacman_qk().unwrap(), single);
    }

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
            assert!(
                all.contains(&runtime),
                "the runtime directory must be watched"
            );
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

    #[test]
    fn test_session_log_scan_stays_off_network_mounts() {
        // GitHub #4: an SFTP mount under $XDG_RUNTIME_DIR/gvfs was walked from
        // the remote root. Another device stands in for the mount.
        let d = tempfile::tempdir().unwrap();
        let mount = d.path().join("gvfs").join("sftp:host=other");
        std::fs::create_dir_all(mount.join("lib")).unwrap();
        std::fs::write(mount.join("lib").join("remote.log"), b"x").unwrap();
        std::fs::create_dir_all(d.path().join("hypr").join("abc")).unwrap();
        std::fs::write(d.path().join("hypr").join("abc").join("hyprland.log"), b"x").unwrap();
        let gvfs = d.path().join("gvfs");
        let dev_of = |p: &Path| Some(if p.starts_with(&gvfs) { 2 } else { 1 });
        let found = _rglob_suffix_on(d.path(), ".log", &dev_of);
        assert_eq!(
            found,
            vec![d.path().join("hypr").join("abc").join("hyprland.log")]
        );
        let (_mb, n) = _du_mb_on(d.path(), &dev_of);
        assert_eq!(n, 1, "only the local log is counted");
        // On one filesystem both still see everything.
        assert_eq!(
            _rglob_suffix_on(d.path(), ".log", &|_p: &Path| Some(1)).len(),
            2
        );
        assert_eq!(_du_mb_on(d.path(), &|_p: &Path| Some(1)).1, 2);
    }

    #[test]
    fn a_symlinked_root_is_measured_where_it_points() {
        // ~/.cache/yay linked to another disk: what is inside is all on that disk.
        // The target on another filesystem than the link, where there is one.
        use std::os::unix::fs::MetadataExt;
        let d = tempfile::tempdir().unwrap();
        let Ok(other) = tempfile::tempdir_in("/dev/shm") else {
            return;
        };
        if std::fs::metadata(other.path()).unwrap().dev()
            == std::fs::metadata(d.path()).unwrap().dev()
        {
            return;
        }
        let target = other.path().join("elsewhere");
        std::fs::create_dir_all(target.join("pkg")).unwrap();
        std::fs::write(target.join("pkg").join("a.log"), b"x").unwrap();
        let link = d.path().join("yay");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert_eq!(_du_mb(&link).1, 1);
        assert_eq!(_rglob_suffix(&link, ".log").len(), 1);
    }

    // ---- CleanupsThatCannotHelp ----
    // A finding must clear once its fix has done everything it can.

    #[test]
    fn test_a_trimmed_package_cache_is_not_a_finding() {
        // 4.4 GB, but every byte is the two newest versions of something
        // installed: paccache has no candidates and the button would do nothing.
        let run = |_c: &[&str], _t: u64| Ok(Ran::ok("==> no candidate packages found for pruning"));
        let f = _pacman_cache_with((4500.0, 522), &run).unwrap();
        assert_eq!(f.severity, OK);
        assert!(!f.actionable());
    }

    #[test]
    fn test_cache_severity_follows_what_a_trim_would_free() {
        let dry = "==> finished dry run: 26 candidates (disk space saved: 1.23 GiB)";
        let run = |_c: &[&str], _t: u64| Ok(Ran::ok(dry));
        let f = _pacman_cache_with((4500.0, 522), &run).unwrap();
        // Two dry runs (-k2 and -uk0), each 1.23 GiB here.
        assert!((f.size_mb - 2.0 * 1.23 * 1024.0).abs() < 0.5);
        assert_eq!(f.severity, WARN);
        assert!(f.actionable());
        // Given together, paccache's later -k0 and -u override -k2 and only
        // uninstalled packages are trimmed, so the fix is two runs.
        assert_eq!(
            f.fix.unwrap().commands(),
            vec![
                vec!["paccache".to_string(), "-rk2".into()],
                vec!["paccache".to_string(), "-ruk0".into()]
            ]
        );
    }

    /// After Update all only what was just built goes: the rest of the
    /// cache is left, and nothing outside it is reachable through a name.
    #[test]
    fn the_build_cache_of_just_the_updated_packages_is_cleared() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("yay");
        for d in ["updated", "other"] {
            std::fs::create_dir_all(cache.join(d).join("src")).unwrap();
            std::fs::write(cache.join(d).join("src/f"), b"x").unwrap();
        }
        std::fs::create_dir_all(tmp.path().join("outside")).unwrap();
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(_yay_cache_fix_in(&cache, &names(&["not-built", "../outside", ".."])).is_none());
        let Some(Fix::Func(f)) =
            _yay_cache_fix_in(&cache, &names(&["updated", "../outside", "linux"]))
        else {
            panic!("a callable fix")
        };
        f(&|_l| {}).unwrap();
        assert!(!cache.join("updated").exists());
        assert!(cache.join("other/src/f").exists());
        assert!(tmp.path().join("outside").exists());
        assert_eq!(paccache_trim_fix().commands().len(), 2);
    }

    #[test]
    fn a_fix_of_several_commands_survives_a_round_trip() {
        let fix = Fix::steps(&[&["paccache", "-rk2"], &["paccache", "-ruk0"]]);
        let back: Fix = serde_json::from_value(serde_json::to_value(&fix).unwrap()).unwrap();
        assert_eq!(back, fix);
        let one = Fix::argv(&["pacman", "-Sc"]);
        assert_eq!(
            serde_json::from_value::<Fix>(serde_json::to_value(&one).unwrap()).unwrap(),
            one
        );
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
        // Both sides read XDG_STATE_HOME, which other tests set under this lock.
        let _g = crate::util::TEST_ENV
            .lock()
            .unwrap_or_else(|e| e.into_inner());
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
        assert!(
            f.detail.contains("without root"),
            "the check must disclose what it could not inspect"
        );
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
        assert_eq!(
            _live_instances(r#"[{"instance": "a", "pid": NaN}]"#),
            Some(vec![cp("a")])
        );
        assert_eq!(
            _live_instances(r#"[{"n": 1e400, "instance": "a"}]"#),
            Some(vec![cp("a")])
        );
        assert_eq!(
            _live_instances(r#"[{"instance": "a\udcff"}]"#),
            Some(vec![vec![97, 0xdcff]])
        );
        assert_eq!(_live_instances(r#"[{"instance": "a"}, []]"#), None);
        assert_eq!(_live_instances(r#"[{"instance": "a"},]"#), None);
        assert_eq!(_live_instances("{}"), Some(vec![]));
        assert_eq!(_pystr(b"x\xffy"), "x\\udcffy");
        assert!(_py_codepoints(b"\xfe") < _py_codepoints("\u{e000}".as_bytes()));
        let sum = |v: &[f64]| {
            v.iter()
                .copied()
                .fold(_PySum::default(), _PySum::add)
                .total()
        };
        assert_eq!(sum(&[1e16, 1.0, -1e16]), 1.0);
        assert_eq!(sum(&[]), 0.0);
        assert!(_py_round_int(f64::NAN).is_err());
        assert_ne!(_py_round_int(1e30), _py_round_int(1.5e30));
    }

    #[test]
    fn test_run_timeout_covers_a_grandchild_holding_the_pipe() {
        let start = std::time::Instant::now();
        assert_eq!(
            run_bytes_within(
                &["sh", "-c", "sleep 4 & echo hi"],
                std::time::Duration::from_millis(200)
            ),
            None
        );
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
        assert_eq!(
            _run_bytes(&["sh", "-c", "echo hi"], 5).map(|o| o.0),
            Some(b"hi\n".to_vec())
        );
    }

    #[test]
    fn test_python_string_helpers() {
        assert_eq!(
            _py_splitlines("a\r\nb\rc\n\nd\x0be\u{2028}"),
            vec!["a", "b", "c", "", "d", "e"]
        );
        assert_eq!(_py_split(" a\x1cb\t c "), vec!["a", "b", "c"]);
        assert_eq!(_thousands(1234567), "1,234,567");
        assert_eq!(_thousands(999), "999");
        assert_eq!(fmt_size(f64::NAN), "nan KB");
        assert_eq!(_py_repr_bytes(b"it's"), "\"it's\"");
        assert_eq!(
            _py_repr_bytes(b"a'b\"\n\xff".as_ref()),
            "'a\\'b\"\\n\\udcff'"
        );
        assert_eq!(_pathlib_str("/tmp/x//rt/./"), "/tmp/x/rt");
        assert_eq!(_pathlib_str("//a"), "//a");
        assert_eq!(_pathlib_str("///a/.."), "/a/..");
        assert_eq!(_py_float("١٢.٥"), Ok(12.5));
        assert_eq!(
            _py_decode(b"\xe2\x82A".as_ref()).unwrap_err(),
            "'utf-8' codec can't decode bytes in position 0-1: invalid continuation byte"
        );
        assert_eq!(_py_decode(b"a\r\nb\rc").unwrap(), "a\nb\nc");
    }

    // ---- A check that cannot finish, or cannot run ----

    fn stuck() -> Finding {
        std::thread::sleep(std::time::Duration::from_secs(30));
        Finding {
            id: "stuck".into(),
            severity: OK.into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_stuck_check_is_left_behind_and_the_rest_arrive() {
        let checks: &[(&str, Check)] =
            &[("stuck", stuck), ("big_warn", big_warn), ("tie_b", tie_b)];
        let t = std::time::Instant::now();
        let deadline =
            |name: &str| std::time::Duration::from_millis(if name == "stuck" { 300 } else { 5000 });
        let out = _run_checks_within(checks, &deadline);
        assert!(
            t.elapsed() < std::time::Duration::from_secs(3),
            "the run does not wait for the stuck check"
        );
        let ids: Vec<&str> = out.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, ["big", "tie-b", "stuck"]);
        assert_eq!(out[2].severity, UNKNOWN);
        assert!(
            out[2].detail.contains("did not finish"),
            "{}",
            out[2].detail
        );
        // Every real check has a deadline, the slow ones longer.
        assert!(_deadline("package_integrity") > _deadline("dangling_desktop"));
    }

    #[test]
    fn a_fifo_named_like_a_launcher_entry_does_not_block_the_check() {
        let d = tempfile::tempdir().unwrap();
        let apps = d.path().join("applications");
        std::fs::create_dir(&apps).unwrap();
        let fifo = apps.join("stuck.desktop");
        rustix::fs::mknodat(
            rustix::fs::CWD,
            &fifo,
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::from_raw_mode(0o600),
            0,
        )
        .unwrap();
        std::os::unix::fs::symlink(&fifo, apps.join("link-to-fifo.desktop")).unwrap();
        std::fs::write(
            apps.join("broken.desktop"),
            "[Desktop Entry]\nExec=/nonexistent/almanac-test\n",
        )
        .unwrap();
        std::fs::write(
            d.path().join("real"),
            "[Desktop Entry]\nExec=/nonexistent/linked\n",
        )
        .unwrap();
        std::os::unix::fs::symlink(d.path().join("real"), apps.join("linked.desktop")).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let home = d.path().to_path_buf();
        std::thread::spawn(move || {
            let _ = tx.send(_dangling_rows(&home));
        });
        let rows = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the check finished")
            .unwrap();
        let rows: Vec<String> = rows.iter().map(|r| _pystr(r)).collect();
        // The FIFOs are skipped; a symlinked entry is still read.
        assert_eq!(
            rows,
            [
                "broken.desktop → /nonexistent/almanac-test",
                "linked.desktop → /nonexistent/linked"
            ]
        );
        assert_eq!(
            _read_regular(Path::new("/dev/null")),
            None,
            "a device is not read"
        );
    }

    #[test]
    fn a_command_that_cannot_run_is_not_one_that_printed_nothing() {
        assert_eq!(_run(&["almanac-no-such-command"], 5), Ok(None));
        assert_eq!(
            _run(&["sh", "-c", "exit 3"], 5),
            Ok(Some(Ran {
                stdout: String::new(),
                ok: false
            }))
        );
        assert_eq!(_run(&["true"], 5), Ok(Ran::ok("")));
    }

    fn not_run(_c: &[&str], _t: u64) -> Result<Option<Ran>, String> {
        Ok(None)
    }

    #[test]
    fn checks_that_could_not_run_are_not_passes() {
        let pending = |f: Finding| {
            assert_eq!(f.severity, UNKNOWN, "{}: {}", f.id, f.detail);
            assert!(f.fix.is_none() && !f.actionable(), "{}", f.id);
        };
        pending(_package_integrity_from("").unwrap());
        pending(_disk_space_with(&|_p| None).unwrap());
        pending(_journal_with(&not_run).unwrap());
        pending(_failed_units_with(&not_run).unwrap());
        pending(_pacnew_with(&not_run).unwrap());
        pending(_pacman_cache_with((4500.0, 522), &not_run).unwrap());
        pending(_unmaintained_aur_with(&not_run, &|_n| unreachable!()).unwrap());
        // systemctl refusing (no bus) prints nothing and exits 1.
        let refused = |_c: &[&str], _t: u64| {
            Ok(Some(Ran {
                stdout: String::new(),
                ok: false,
            }))
        };
        pending(_failed_units_with(&refused).unwrap());
        pending(_journal_with(&refused).unwrap());
        // One scope answering with a failed unit is still a warning, and says
        // what it could not read.
        let half = |c: &[&str], _t: u64| {
            Ok(if c.contains(&"--user") {
                None
            } else {
                Ran::ok("cups.service loaded failed failed CUPS\n")
            })
        };
        let f = _failed_units_with(&half).unwrap();
        assert_eq!(
            (f.severity.as_str(), f.items.clone()),
            (WARN, vec!["system cups.service".to_string()])
        );
        assert!(
            f.detail.contains("Your own services could not be checked"),
            "{}",
            f.detail
        );
        // The passes still pass.
        assert_eq!(
            _failed_units_with(&|_c, _t| Ok(Ran::ok("")))
                .unwrap()
                .severity,
            OK
        );
        assert_eq!(
            _journal_with(&|_c, _t| Ok(Ran::ok(
                "Archived and active journals take up 56.0M in the file system."
            )))
            .unwrap()
            .severity,
            OK
        );
    }

    fn known(asked: &[&str], results: &[&str]) -> crate::inventory::AurAnswer {
        crate::inventory::AurAnswer {
            at: 0.0,
            asked: asked.iter().map(|s| s.to_string()).collect(),
            results: results
                .iter()
                .map(|s| {
                    (
                        s.to_string(),
                        crate::inventory::AurHealth {
                            maintainer: "m".into(),
                            ..Default::default()
                        },
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn an_unreachable_aur_is_not_every_package_gone() {
        let run = |_c: &[&str], _t: u64| Ok(Ran::ok("yay\nfoo\nbar"));
        // Offline with nothing kept, or a reply that made no sense.
        for aur in [
            &(|_n: &[String]| Ok(None)) as AurKnown<'_>,
            &|_n: &[String]| Err("bad reply".into()),
        ] {
            let f = _unmaintained_aur_with(&run, aur).unwrap();
            assert_eq!(f.severity, UNKNOWN, "{}", f.detail);
            assert!(
                f.detail.contains("could not be reached") && !f.detail.contains("no longer listed"),
                "{}",
                f.detail
            );
        }
        // A kept answer that never asked about bar: bar is not checked, the
        // rest are judged.
        let f = _unmaintained_aur_with(&run, &|_n| {
            Ok(Some(known(&["yay", "foo"], &["yay", "foo"])))
        })
        .unwrap();
        assert_eq!(f.severity, UNKNOWN);
        assert_eq!(f.items, ["bar — not checked"]);
        let f =
            _unmaintained_aur_with(&run, &|_n| Ok(Some(known(&["yay", "foo"], &["yay"])))).unwrap();
        assert_eq!(f.severity, WARN, "foo was asked about and is gone");
        assert_eq!(f.items, ["foo — no longer in the AUR", "bar — not checked"]);
        assert_eq!(f.count, 1);
        assert!(f.detail.contains("1 could not be checked"), "{}", f.detail);
        // Everything asked and answered: a pass.
        let all = known(&["yay", "foo", "bar"], &["yay", "foo", "bar"]);
        assert_eq!(
            _unmaintained_aur_with(&run, &|_n| Ok(Some(all.clone())))
                .unwrap()
                .severity,
            OK
        );
    }

    #[test]
    fn the_live_hyprland_log_is_truncated_when_hyprctl_cannot_say() {
        let d = tempfile::tempdir().unwrap();
        let add = |sig: &str| {
            let dir = d.path().join("hypr").join(sig);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("hyprland.log"), b"x").unwrap();
            dir.join("hyprland.log")
        };
        let lines = std::sync::Mutex::new(Vec::new());
        let log = |l: &str| lines.lock().unwrap().push(l.to_string());
        // hyprctl not there (or failing): nothing is unlinked.
        let (a, b) = (add("aaa_1"), add("bbb_2"));
        _clear_runtime_logs_in(d.path(), None, None, 0.0, &log).unwrap();
        assert!(a.exists() && b.exists());
        assert_eq!(std::fs::metadata(&a).unwrap().len(), 0, "truncated instead");
        // Output that cannot be read is no list either.
        _clear_runtime_logs_in(d.path(), Some("not json"), None, 0.0, &log).unwrap();
        assert!(a.exists() && b.exists());
        // This session's own instance is live whatever hyprctl says.
        _clear_runtime_logs_in(
            d.path(),
            Some("[]"),
            Some(std::ffi::OsStr::new("aaa_1")),
            0.0,
            &log,
        )
        .unwrap();
        assert!(a.exists(), "the current instance is kept");
        assert!(!b.exists(), "an ended one goes");
        // And a live list read from hyprctl is still believed.
        let c = add("ccc_3");
        _clear_runtime_logs_in(
            d.path(),
            Some(r#"[{"instance": "ccc_3"}]"#),
            None,
            0.0,
            &log,
        )
        .unwrap();
        assert!(c.exists() && !a.exists());
        assert!(lines
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("its session may still be running")));
    }

    #[test]
    fn pacnew_and_pacsave_are_found_in_one_walk_of_etc() {
        let calls = std::sync::Mutex::new(Vec::new());
        let run = |c: &[&str], _t: u64| {
            calls.lock().unwrap().push(c.join(" "));
            // find exits 1 on the root-only directories it could not enter.
            Ok(Some(Ran {
                stdout: "/etc/a.pacsave\n/etc/b.pacnew\n/etc/c.pacnew\n".into(),
                ok: false,
            }))
        };
        let f = _pacnew_with(&run).unwrap();
        assert_eq!(
            *calls.lock().unwrap(),
            ["find /etc ( -name *.pacnew -o -name *.pacsave ) -type f"]
        );
        assert_eq!(
            f.items,
            ["/etc/b.pacnew", "/etc/c.pacnew", "/etc/a.pacsave"]
        );
        assert_eq!(f.severity, WARN);
    }

    #[test]
    fn demo_findings_come_back_worst_first_and_demo_never_asks_a_real_check() {
        let _g = crate::util::TEST_ENV
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::demo::TEST_DEMO.store(1, std::sync::atomic::Ordering::SeqCst);
        let found = run_all();
        let again = check_by_id("disk");
        crate::demo::TEST_DEMO.store(0, std::sync::atomic::Ordering::SeqCst);
        let mut sorted = found.clone();
        _sort_findings(&mut sorted);
        assert_eq!(found, sorted);
        assert_eq!(
            found[0].severity,
            CRIT,
            "{:?}",
            found.iter().map(|f| &f.severity).collect::<Vec<_>>()
        );
        assert!(again.is_none());
    }

    #[test]
    fn test_rmtree_refuses_a_symlink() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("real/pkg")).unwrap();
        std::os::unix::fs::symlink("real", d.path().join("link")).unwrap();
        assert!(_rmtree(&d.path().join("link"), false)
            .unwrap_err()
            .starts_with("[Errno None] None: PosixPath("));
        assert!(d.path().join("link").is_symlink() && d.path().join("real/pkg").is_dir());
        _rmtree(&d.path().join("real"), false).unwrap();
        assert!(!d.path().join("real").exists());
    }
    #[test]
    fn a_check_asked_again_by_id_gets_the_same_deadline() {
        // verify::recheck runs one check by its finding id; the id must find
        // the deadline its check has in run_all.
        assert_eq!(CHECKS.len(), CHECK_BY_ID.len());
        for ((name, by_name), (id, by_id)) in CHECKS.iter().zip(CHECK_BY_ID) {
            assert_eq!(
                *by_name as usize, *by_id as usize,
                "{name} and {id} are not the same check"
            );
            assert_eq!(_deadline(id), _deadline(name), "{id}");
        }
    }
}
