//! Port of `plugins.py`.
//!
//! Installed Omarchy shell plugins: what state each is in, and acting on it.
//!
//! Counting commits behind upstream is not enough to decide whether a plugin can
//! be updated. Before anything touches one, it is classified -- the same states
//! omaplug (github.com/fross100/omaplug) settled on, which match what Omarchy's
//! own updater will and will not do:
//!
//! * CURRENT        a clean checkout at its upstream's head
//! * UPDATE         a clean checkout behind its upstream: safe to fast-forward
//! * LOCAL_CHANGES  edited, ahead of upstream, diverged from it, or detached --
//!   an update would have to decide what happens to that work
//! * LOCAL          not Omarchy's to update: a plain folder, a checkout with no
//!   origin, or a **symlink**. Omarchy installs a plugin being
//!   developed as a symlink to its author's workspace, and
//!   `omarchy plugin update` would happily fast-forward that
//!   workspace; this never offers to.
//! * ERROR          the state could not be read (fetch failed, git broke)
//!
//! Updates go through `omarchy plugin update <id> --yes`, which validates the
//! result and rolls back a plugin that fails validation -- something a bare
//! `git pull` never did.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::util;

/// The per-path fetch cache `classify` / `installed` read and update:
/// `str(path) -> {"remote": sha, "fetched_at": time}`. In the Python this is a
/// plain dict, usually `cache.setdefault("_plugins", {})` of inventory's cache.
pub type FetchCache = serde_json::Map<String, serde_json::Value>;

pub const CURRENT: &str = "current";
pub const UPDATE: &str = "update";
pub const LOCAL_CHANGES: &str = "local-changes";
pub const LOCAL: &str = "local";
pub const ERROR: &str = "error";
pub const FETCH_EVERY: f64 = 3600.0;

/// Bulk-update scopes, by the marketplace provenance a plugin carries:
/// `(key, label, allowed trust keys)`. `None`: anything, listed or not.
pub const SCOPES: &[(&str, &str, Option<&[&str]>)] = &[
    ("verified", "Verified only",
     Some(&["official", "first-party", "reviewed", "verified"])),
    ("changed", "Verified, and changed since review",
     Some(&["official", "first-party", "reviewed", "verified", "reviewed-stale", "stale"])),
    ("all", "All plugins", None), // None: anything, listed or not
];

pub fn plugins_dir() -> PathBuf {
    py_path(&crate::xdg::config_home().join("omarchy").join("plugins"))
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub id: String,
    pub kind: String,
    /// why: "symlink", "modified", "ahead", "3 commits"…
    pub detail: String,
    pub behind: i64,
    pub url: String,
}

impl State {
    /// `State(id, kind, detail, behind, url)`, as the Python constructs it.
    pub fn new(id: &str, kind: &str, detail: &str, behind: i64, url: &str) -> State {
        State {
            id: id.to_string(),
            kind: kind.to_string(),
            detail: detail.to_string(),
            behind,
            url: url.to_string(),
        }
    }

    pub fn updatable(&self) -> bool {
        self.kind == UPDATE
    }

    /// One line for the row; "" for a kind `try_describe` refuses (every
    /// State this module builds has one of the five).
    pub fn describe(&self) -> String {
        self.try_describe().unwrap_or_default()
    }

    /// `describe()` as the Python has it: the dict is indexed by `kind`, so a
    /// kind it does not know raises KeyError.
    pub fn try_describe(&self) -> Result<String, String> {
        Ok(match self.kind.as_str() {
            CURRENT => "Up to date".to_string(),
            UPDATE => format!("{} commit(s) behind", self.behind),
            LOCAL_CHANGES => match self.detail.as_str() {
                "modified" => "Has local edits",
                "ahead" => "Ahead of upstream — local commits",
                "diverged" => "Diverged from upstream",
                "detached" => "Not on a branch",
                _ => "Local changes",
            }
            .to_string(),
            LOCAL => match self.detail.as_str() {
                "symlink" => "Development plugin (a link to a workspace) — never updated here",
                "not-git" => "Not a git checkout — nothing to update from",
                "no-origin" => "No upstream to update from",
                "no-commits" => "No commits yet",
                _ => "Managed locally",
            }
            .to_string(),
            ERROR => format!("Could not check: {}", self.detail),
            other => return Err(format!("KeyError: {}", py_repr_str(other))),
        })
    }
}

/// `Path(d)` as Python's PurePosixPath spells it: doubled and trailing
/// slashes and `.` parts dropped (a leading "./" too), `..` kept, exactly
/// two leading slashes kept (POSIX leaves `//` implementation-defined), and
/// "." for an empty path.
fn py_path(d: &Path) -> PathBuf {
    use std::ffi::OsStr;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let b = d.as_os_str().as_bytes();
    let root: &[u8] = if b.starts_with(b"//") && !b.starts_with(b"///") {
        b"//"
    } else if b.starts_with(b"/") {
        b"/"
    } else {
        b""
    };
    let parts: Vec<&[u8]> = b.split(|c| *c == b'/').filter(|p| !p.is_empty() && *p != b".").collect();
    let mut out = root.to_vec();
    out.extend(parts.join(&b'/'));
    if out.is_empty() {
        return PathBuf::from(OsStr::new("."));
    }
    PathBuf::from(std::ffi::OsString::from_vec(out))
}

/// `str(path)`.
fn path_str(d: &Path) -> String {
    py_path(d).to_string_lossy().into_owned()
}

/// `Path.name`: the last part, `..` included; "" for "/" and ".".
fn py_name(d: &Path) -> String {
    let p = py_path(d);
    let s = p.to_string_lossy();
    match s.rsplit('/').next() {
        Some(".") | None => String::new(),
        Some(last) => last.to_string(),
    }
}

/// Python's `repr(str)`, enough for the argv in a TimeoutExpired message.
fn py_repr_str(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(q);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == q => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(q);
    out
}

/// An exception the Python raises: one `classify` catches (`OSError`,
/// `SubprocessError`) and reports as an ERROR state, or one it lets through
/// to its caller (TypeError, ValueError, AttributeError...), as `str(exc)`.
#[derive(Debug, Clone)]
enum PyErr {
    Caught(String),
    Raise(String),
}

impl PyErr {
}

/// What `subprocess.run(argv, capture_output=True, text=True, timeout=n)`
/// hands back: the exit status and the decoded output.
struct Ran {
    code: Option<i32>,
    stdout: String,
    #[allow(dead_code)]
    stderr: String,
}

impl Ran {
    /// `returncode == 0`.
    fn ok(&self) -> bool {
        self.code == Some(0)
    }
}

/// What `subprocess.run` raises: OSError when the program cannot be started
/// (`str(exc)`), TimeoutExpired, and UnicodeDecodeError -- a ValueError --
/// for output that is not UTF-8 (`str(exc)`, with its type).
enum RunErr {
    Os(String),
    Timeout,
    Decode(String),
}

/// Python's `str.strip()`: `str.isspace()` is Unicode White_Space plus the
/// separators \x1c-\x1f, which Rust's `trim()` leaves.
fn py_strip(s: &str) -> &str {
    s.trim_matches(py_isspace)
}

fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// `bytes.decode("utf-8")` (strict), then the universal newlines `text=True`
/// applies ("\r\n" and "\r" read as "\n"). `Err` is UnicodeDecodeError's text.
fn py_decode(b: Vec<u8>) -> Result<String, String> {
    let text = match String::from_utf8(b) {
        Ok(t) => t,
        Err(e) => {
            let bytes = e.as_bytes();
            let u = e.utf8_error();
            let start = u.valid_up_to();
            let (end, reason) = match u.error_len() {
                Some(n) => {
                    // A byte that cannot begin a character is "invalid start
                    // byte"; one that begins a sequence cut short, "invalid
                    // continuation byte".
                    let b0 = bytes[start];
                    let starts = matches!(b0, 0xc2..=0xf4);
                    (start + n, if starts { "invalid continuation byte" } else { "invalid start byte" })
                }
                None => (bytes.len(), "unexpected end of data"),
            };
            let what = if end - start <= 1 {
                format!("byte 0x{:02x} in position {start}", bytes[start])
            } else {
                format!("bytes in position {start}-{}", end - 1)
            };
            return Err(format!("UnicodeDecodeError: 'utf-8' codec can't decode {what}: {reason}"));
        }
    };
    Ok(text.replace("\r\n", "\n").replace('\r', "\n"))
}

/// `str(exc)` of the OSError for errno `n`: "[Errno 2] No such file or
/// directory: 'git'".
fn os_error(n: i32, prog: &str) -> String {
    let text = std::io::Error::from_raw_os_error(n).to_string();
    let text = text.strip_suffix(&format!(" (os error {n})")).unwrap_or(&text).to_string();
    format!("[Errno {n}] {text}: {}", py_repr_str(prog))
}

/// `subprocess.run(argv, capture_output=True, text=True, timeout=timeout)`,
/// with `env` added to the inherited environment.
///
/// util::run does not fit: it hands back output already decoded lossily (so
/// a genuine U+FFFD cannot be told from bytes that are not UTF-8), gives the
/// child /dev/null for stdin where Python's child inherits the parent's, and
/// does not keep the errno. The program is found as `_posixsubprocess` finds
/// it: each PATH entry tried in turn, the first error other than ENOENT /
/// ENOTDIR reported if none starts.
///
/// The arguments are OS strings, as Python's are once `os.fsencode`d: a
/// path that is not UTF-8 reaches the child as its own bytes.
fn py_run(argv: &[&OsStr], env: &[(std::ffi::OsString, std::ffi::OsString)], timeout: u64) -> Result<Ran, RunErr> {
    use std::io::Read;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    const ENOENT: i32 = 2;
    const ENOTDIR: i32 = 20;
    let prog = argv[0];
    let prog_s = prog.to_string_lossy();
    let candidates: Vec<PathBuf> = if prog.as_bytes().contains(&b'/') {
        vec![PathBuf::from(prog)]
    } else {
        // os.get_exec_path(): the child's PATH, else os.defpath.
        let path = env.iter().find(|(k, _)| k == "PATH").map(|(_, v)| v.clone())
            .or_else(|| std::env::var_os("PATH"))
            .unwrap_or_else(|| "/bin:/usr/bin".into());
        std::env::split_paths(&path)
            // os.path.join("", prog) is prog itself: relative to the cwd.
            .map(|dir| if dir.as_os_str().is_empty() { Path::new(".").join(prog) } else { dir.join(prog) })
            .collect()
    };
    let mut saved: Option<i32> = None;
    let mut last = ENOENT;
    let mut child = None;
    for exe in candidates {
        let mut cmd = Command::new(&exe);
        cmd.arg0(prog)
            .args(&argv[1..])
            .envs(env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::inherit())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        match cmd.spawn() {
            Ok(c) => {
                child = Some(c);
                break;
            }
            Err(e) => {
                let n = e.raw_os_error().unwrap_or(ENOENT);
                if n != ENOENT && n != ENOTDIR && saved.is_none() {
                    saved = Some(n);
                }
                last = n;
            }
        }
    }
    let Some(mut child) = child else {
        // Report the first exec error, not the last.
        return Err(RunErr::Os(os_error(saved.unwrap_or(last), &prog_s)));
    };
    // Drain both pipes on their own threads, or a chatty child fills one and
    // blocks forever while we wait on it.
    let reader = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            buf
        })
    };
    let out_t = reader(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err_t = reader(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let limit = std::time::Duration::from_secs(timeout);
    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) if start.elapsed() >= limit => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
            Err(_) => break None,
        }
    };
    let stdout = out_t.join().unwrap_or_default();
    let stderr = err_t.join().unwrap_or_default();
    let Some(status) = status else { return Err(RunErr::Timeout) };
    // stdout is decoded first, so its error is the one raised.
    let stdout = py_decode(stdout).map_err(RunErr::Decode)?;
    let stderr = py_decode(stderr).map_err(RunErr::Decode)?;
    Ok(Ran { code: status.code(), stdout, stderr })
}

/// `git -C d *args` with prompts disabled (GIT_TERMINAL_PROMPT=0, batch-mode ssh).
///
/// `Err` is what the Python's `subprocess.run` raises: OSError when git
/// cannot be started and TimeoutExpired (caught by `classify`); ValueError
/// for a NUL in an argument and UnicodeDecodeError for output that is not
/// UTF-8 (not caught).
///
/// The directory goes to git as its bytes (`str(d)` is fsencoded back to
/// them), so a plugin folder whose name is not UTF-8 is reached as it is.
fn _git(d: &Path, args: &[&str], timeout: u64) -> Result<Ran, PyErr> {
    use std::os::unix::ffi::OsStrExt;
    let dir = py_path(d);
    let mut argv: Vec<&OsStr> = vec![OsStr::new("git"), OsStr::new("-C"), dir.as_os_str()];
    argv.extend(args.iter().map(OsStr::new));
    if argv.iter().any(|a| a.as_bytes().contains(&0)) {
        return Err(PyErr::Raise("ValueError: embedded null byte".to_string()));
    }
    let ssh = std::env::var_os("GIT_SSH_COMMAND").unwrap_or_else(|| "ssh -oBatchMode=yes".into());
    let env = vec![("GIT_TERMINAL_PROMPT".into(), "0".into()), ("GIT_SSH_COMMAND".into(), ssh)];
    match py_run(&argv, &env, timeout) {
        Ok(ran) => Ok(ran),
        Err(RunErr::Os(e)) => Err(PyErr::Caught(e)),
        Err(RunErr::Timeout) => {
            let cmd: Vec<String> = argv.iter().map(|a| py_repr_str(&a.to_string_lossy())).collect();
            Err(PyErr::Caught(format!("Command '[{}]' timed out after {timeout} seconds", cmd.join(", "))))
        }
        Err(RunErr::Decode(e)) => Err(PyErr::Raise(e)),
    }
}

/// The state of the plugin in `d`. `cache` holds fetch results by path,
/// so the network is asked at most once an hour per plugin; with `fetch`
/// False it is not asked at all, and an unknown upstream reads as CURRENT
/// with the detail "unchecked".
///
/// Where the Python raises past its `except` (a cache entry of the wrong
/// shape, git output that is not UTF-8), this reads as an ERROR state with
/// the exception's text; `try_classify` returns it instead.
pub fn classify(d: &Path, cache: Option<&mut FetchCache>, fetch: bool) -> State {
    try_classify(d, cache, fetch).unwrap_or_else(|exc| {
        State::new(&py_name(d), ERROR, &exc.chars().take(80).collect::<String>(), 0, "")
    })
}

/// `classify` as the Python has it: `Err` for an exception it does not catch.
pub fn try_classify(d: &Path, cache: Option<&mut FetchCache>, fetch: bool) -> Result<State, String> {
    let d = &py_path(d);
    let pid = py_name(d);
    if d.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false) {
        return Ok(State::new(&pid, LOCAL, "symlink", 0, ""));
    }
    if !d.join(".git").is_dir() {
        return Ok(State::new(&pid, LOCAL, "not-git", 0, ""));
    }
    match classify_git(d, &pid, cache, fetch) {
        Ok(st) => Ok(st),
        Err(PyErr::Caught(exc)) => Ok(State::new(&pid, ERROR, &exc.chars().take(80).collect::<String>(), 0, "")),
        Err(PyErr::Raise(exc)) => Err(exc),
    }
}

/// The `try:` body of `classify`.
fn classify_git(d: &Path, pid: &str, mut cache: Option<&mut FetchCache>, fetch: bool) -> Result<State, PyErr> {
    let url = py_strip(&_git(d, &["remote", "get-url", "origin"], 25)?.stdout).to_string();
    if url.is_empty() {
        return Ok(State::new(pid, LOCAL, "no-origin", 0, ""));
    }
    let head = py_strip(&_git(d, &["rev-parse", "--verify", "HEAD"], 25)?.stdout).to_string();
    if head.is_empty() {
        return Ok(State::new(pid, LOCAL, "no-commits", 0, &url));
    }
    if !_git(d, &["symbolic-ref", "--quiet", "HEAD"], 25)?.ok() {
        return Ok(State::new(pid, LOCAL_CHANGES, "detached", 0, &url));
    }
    if !py_strip(&_git(d, &["status", "--porcelain", "--untracked-files=normal"], 25)?.stdout).is_empty() {
        // Still worth knowing how far behind: patches.py weighs the edit
        // against the update, and needs there to be one.
        let remote = remote_head(d, cache.as_deref_mut(), fetch)?;
        let behind = if remote.is_empty() { 0 } else { count(d, &head, &remote)? };
        return Ok(State::new(pid, LOCAL_CHANGES, "modified", behind, &url));
    }
    let remote = remote_head(d, cache.as_deref_mut(), fetch)?;
    if remote.is_empty() {
        return Ok(if fetch {
            State::new(pid, ERROR, "fetch failed", 0, &url)
        } else {
            State::new(pid, CURRENT, "unchecked", 0, &url)
        });
    }
    if remote == head {
        return Ok(State::new(pid, CURRENT, "", 0, &url));
    }
    if _git(d, &["merge-base", "--is-ancestor", &head, &remote], 25)?.ok() {
        return Ok(State::new(pid, UPDATE, "behind", count(d, &head, &remote)?, &url));
    }
    if _git(d, &["merge-base", "--is-ancestor", &remote, &head], 25)?.ok() {
        return Ok(State::new(pid, LOCAL_CHANGES, "ahead", 0, &url));
    }
    Ok(State::new(pid, LOCAL_CHANGES, "diverged", 0, &url))
}

/// The value of a Unicode decimal digit (general category Nd), as
/// `int()` and `float()` read it; None for anything else. Every Nd run is
/// a complete 0-9 (Unicode's stability policy), so the digit is its offset
/// in the run of Nd characters it sits in, modulo 10.
fn nd_value(c: char) -> Option<u32> {
    if c.is_ascii() {
        return c.to_digit(10);
    }
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"^\p{Nd}$").unwrap());
    let is_nd = |c: char| re.is_match(c.encode_utf8(&mut [0; 4]));
    if !is_nd(c) {
        return None;
    }
    let mut first = c as u32;
    while let Some(p) = first.checked_sub(1).and_then(char::from_u32) {
        if !is_nd(p) {
            break;
        }
        first -= 1;
    }
    Some((c as u32 - first) % 10)
}

/// Python's `float(s)` of a string: as `_PyUnicode_TransformDecimalAndSpaceToASCII`
/// reads it -- Unicode whitespace as a space, any decimal digit (Arabic-Indic,
/// fullwidth...) as its ASCII digit, any other non-ASCII character refused --
/// then ASCII whitespace around it ignored, underscores allowed between
/// digits, inf / infinity / nan in any case.
fn py_float_str(s: &str) -> Option<f64> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"^[+-]?(?:[0-9](?:_?[0-9])*(?:\.(?:[0-9](?:_?[0-9])*)?)?|\.[0-9](?:_?[0-9])*)(?:[eE][+-]?[0-9](?:_?[0-9])*)?$")
            .unwrap()
    });
    let mut ascii = String::with_capacity(s.len());
    for c in s.chars() {
        if (c as u32) < 127 {
            ascii.push(c);
        } else if py_isspace(c) {
            ascii.push(' ');
        } else {
            ascii.push(nd_value(c).and_then(|v| char::from_digit(v, 10))?);
        }
    }
    // Py_ISSPACE: ASCII whitespace only (not \x1c-\x1f).
    let t = ascii.trim_matches(|c: char| matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r'));
    let (sign, body) = match t.strip_prefix('-') {
        Some(rest) => (-1.0, rest),
        None => (1.0, t.strip_prefix('+').unwrap_or(t)),
    };
    match body.to_ascii_lowercase().as_str() {
        "inf" | "infinity" => return Some(sign * f64::INFINITY),
        "nan" => return Some(f64::NAN),
        _ => {}
    }
    if !re.is_match(t) {
        return None;
    }
    t.replace('_', "").parse().ok()
}

/// `float(x or 0)` for a cached `fetched_at`: ValueError for a string that
/// is not a number, TypeError for a list or dict.
fn py_float(v: Option<&Value>) -> Result<f64, String> {
    let Some(v) = v.filter(|v| truthy(Some(v))) else { return Ok(0.0) };
    match v {
        Value::Number(n) => Ok(n.as_f64().unwrap_or(0.0)),
        Value::Bool(b) => Ok(f64::from(u8::from(*b))),
        Value::String(s) => py_float_str(s)
            .ok_or_else(|| format!("ValueError: could not convert string to float: {}", py_repr_str(s))),
        other => Err(format!("TypeError: float() argument must be a string or a real number, not '{}'",
                             py_type(other))),
    }
}


fn remote_head(d: &Path, cache: Option<&mut FetchCache>, fetch: bool) -> Result<String, PyErr> {
    let key = path_str(d);
    // `(cache or {}).get(key) or {}`, then `.get` on it: AttributeError for
    // an entry that is not a dict.
    let empty = serde_json::Map::new();
    let entry = match cache.as_ref().and_then(|c| c.get(&key)).filter(|v| truthy(Some(v))) {
        None => &empty,
        Some(Value::Object(o)) => o,
        Some(other) => {
            return Err(PyErr::Raise(format!("AttributeError: '{}' object has no attribute 'get'", py_type(other))))
        }
    };
    let fresh = util::now() - py_float(entry.get("fetched_at")).map_err(PyErr::Raise)? < FETCH_EVERY;
    let sha = entry.get("remote").cloned().unwrap_or_else(|| Value::String(String::new()));
    if (fresh || !fetch) && truthy(Some(&sha)) {
        // Anything but a string in an argv makes subprocess raise TypeError.
        let Value::String(sha) = sha else {
            return Err(PyErr::Raise(format!("TypeError: expected str, bytes or os.PathLike object, not {}",
                                            py_type(&sha))));
        };
        if _git(d, &["cat-file", "-e", &sha], 25)?.ok() {
            return Ok(sha);
        }
    }
    if !fetch {
        return Ok(String::new());
    }
    if !_git(d, &["fetch", "--quiet", "origin", "HEAD"], 45)?.ok() {
        return Ok(String::new());
    }
    let sha = py_strip(&_git(d, &["rev-parse", "--verify", "FETCH_HEAD"], 25)?.stdout).to_string();
    if let Some(cache) = cache {
        if !sha.is_empty() {
            cache.insert(key, serde_json::json!({"remote": sha, "fetched_at": util::now()}));
        }
    }
    Ok(sha)
}


fn count(d: &Path, head: &str, remote: &str) -> Result<i64, PyErr> {
    let range = format!("{head}..{remote}");
    let out = py_strip(&_git(d, &["rev-list", "--count", &range], 25)?.stdout).to_string();
    // `int(out) if out.isdigit() else 0`: any decimal digits, not only ASCII.
    let digits: Option<String> = out.chars().map(|c| nd_value(c).and_then(|v| char::from_digit(v, 10))).collect();
    Ok(match digits {
        Some(d) if !d.is_empty() => d.parse().unwrap_or(i64::MAX),
        _ => 0,
    })
}

/// One plugin on this system, built in or added, for the Plugins page.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Installed {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub kinds: Vec<String>,
    pub enabled: bool,
    pub can_disable: bool,
    pub built_in: bool,
    pub folder: Option<PathBuf>,
    /// only for plugins the user added
    pub state: Option<State>,
    /// What the Python holds where omarchy's list or a manifest gave a value
    /// that is not a string; serialised in place of the field above.
    #[serde(skip)]
    pub raw: Raw,
}

/// Values that are not strings, kept as the Python keeps them: an `id` of 5
/// or true from omarchy's list, a manifest's `description` or `version` of
/// null or a number, `kinds` items that are not strings. The plain field
/// holds `str()` of the id and of each kind, and "" for the rest. Each is
/// None when the plain field says it all.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Raw {
    pub id: Option<Py>,
    pub description: Option<Py>,
    pub version: Option<Py>,
    pub kinds: Option<Vec<Py>>,
}

impl Default for Installed {
    fn default() -> Self {
        Installed {
            id: String::new(),
            name: String::new(),
            description: String::new(),
            version: String::new(),
            kinds: Vec::new(),
            enabled: false,
            can_disable: true,
            built_in: false,
            folder: None,
            state: None,
            raw: Raw::default(),
        }
    }
}

impl Serialize for Installed {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let or = |raw: &Option<Py>, plain: &str| raw.as_ref().map_or_else(|| Value::String(plain.to_string()), Py::to_value);
        let mut st = s.serialize_struct("Installed", 10)?;
        st.serialize_field("id", &or(&self.raw.id, &self.id))?;
        st.serialize_field("name", &self.name)?;
        st.serialize_field("description", &or(&self.raw.description, &self.description))?;
        st.serialize_field("version", &or(&self.raw.version, &self.version))?;
        match &self.raw.kinds {
            Some(k) => st.serialize_field("kinds", &k.iter().map(Py::to_value).collect::<Vec<_>>())?,
            None => st.serialize_field("kinds", &self.kinds)?,
        }
        st.serialize_field("enabled", &self.enabled)?;
        st.serialize_field("can_disable", &self.can_disable)?;
        st.serialize_field("built_in", &self.built_in)?;
        st.serialize_field("folder", &self.folder.as_deref().map(path_str))?;
        st.serialize_field("state", &self.state)?;
        st.end()
    }
}

impl Installed {
    pub fn has_panel(&self) -> bool {
        self.try_has_panel().unwrap_or(false)
    }

    /// `has_panel` as the Python has it: `set(self.kinds)` raises TypeError
    /// for a kind that is a list or a dict.
    pub fn try_has_panel(&self) -> Result<bool, String> {
        if let Some(k) = self.raw.kinds.as_ref().and_then(|k| k.iter().find(|k| k.is_list() || k.is_dict())) {
            return Err(format!("TypeError: unhashable type: '{}'", k.type_name()));
        }
        Ok(self.kinds.iter().enumerate().any(|(n, k)| {
            // A kind that was not a string is never one of these.
            let was_str = self.raw.kinds.as_ref().is_none_or(|r| r.get(n).is_some_and(Py::is_str));
            was_str && matches!(k.as_str(), "panel" | "bar-widget" | "overlay")
        }))
    }
}

/// Python's `bool(x)` of a JSON value.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// `type(x).__name__` of a JSON value.
fn py_type(v: &Value) -> &'static str {
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

// ---- json.loads, as Python has it ----------------------------------------------
//
// omarchy's list, the manifests and the 1.4.0 scope file are read with
// Python's `json.loads`, which takes more than serde does: NaN, Infinity and
// -Infinity, ints of any size, numbers that overflow to inf, lone surrogate
// escapes, and nesting tens of thousands deep. serde's `Value` can hold none
// of those, so they go through a small reader that follows CPython's C
// scanner instead, into `Py`.

/// A Python str: code points, lone surrogates included.
type PyStr = Vec<u32>;

/// One parsed value. Containers refer to their children by index into
/// `Doc::nodes`, so neither reading, cloning nor dropping a deeply nested
/// document recurses.
#[derive(Debug)]
enum Node {
    Null,
    Bool(bool),
    /// An int, as `str(int)` writes it: any size, `-0` is `0`.
    Int(String),
    Float(f64),
    Str(PyStr),
    List(Vec<usize>),
    /// In first-insertion order, each key once (a dict).
    Dict(Vec<(PyStr, usize)>),
}

#[derive(Debug)]
struct Doc {
    nodes: Vec<Node>,
}

/// A value exactly as Python's `json.loads` returns it (see above): one
/// node of a shared parsed document, so cheap to clone.
#[derive(Debug, Clone)]
pub struct Py {
    doc: std::sync::Arc<Doc>,
    id: usize,
}

/// Equal when `json.dumps` writes them the same (so NaN equals NaN, which
/// change detection wants).
impl PartialEq for Py {
    fn eq(&self, other: &Py) -> bool {
        self.dumps() == other.dumps()
    }
}

impl Serialize for Py {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.to_value().serialize(s)
    }
}

/// What a value is equal to as a dict key -- `5 == 5.0 == True`, `1e22 ==
/// 10**22` -- for the values that can be one. NaN is never equal to another
/// NaN read from JSON (each is its own object), so it has no key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum PyKey {
    Null,
    /// An int, a bool, or a float that is a whole number: its digits.
    Num(String),
    /// Any other float: its repr.
    Float(String),
    Str(PyStr),
}

fn py_str_of(s: &str) -> PyStr {
    s.chars().map(u32::from).collect()
}

/// A Rust string of a Python one: a lone surrogate becomes U+FFFD.
fn lossy(s: &[u32]) -> String {
    s.iter().map(|&c| char::from_u32(c).unwrap_or('\u{fffd}')).collect()
}

/// `str(float)`: "nan", "inf", "-inf", else the shortest repr.
fn py_float_str_repr(f: f64) -> String {
    if f.is_nan() {
        "nan".into()
    } else if f.is_infinite() {
        (if f > 0.0 { "inf" } else { "-inf" }).into()
    } else {
        util::py_float_repr(f)
    }
}

/// How deep `repr` and the serde copy go; past it a container is cut short
/// (Python's own repr would raise RecursionError long before Rust's stack
/// ran out, and serde's `Value` is dropped recursively).
const SHOW_DEPTH: usize = 100;

impl Py {
    fn leaf(node: Node) -> Py {
        Py { doc: std::sync::Arc::new(Doc { nodes: vec![node] }), id: 0 }
    }

    /// A str value.
    fn string(s: &str) -> Py {
        Py::leaf(Node::Str(py_str_of(s)))
    }

    fn node(&self) -> &Node {
        &self.doc.nodes[self.id]
    }

    fn at(&self, id: usize) -> Py {
        Py { doc: self.doc.clone(), id }
    }

    pub fn is_str(&self) -> bool {
        matches!(self.node(), Node::Str(_))
    }

    pub fn is_list(&self) -> bool {
        matches!(self.node(), Node::List(_))
    }

    pub fn is_dict(&self) -> bool {
        matches!(self.node(), Node::Dict(_))
    }

    /// `d.get(key)` on a dict; None for anything else.
    fn get(&self, key: &str) -> Option<Py> {
        let Node::Dict(entries) = self.node() else { return None };
        let key = py_str_of(key);
        entries.iter().find(|(k, _)| *k == key).map(|(_, c)| self.at(*c))
    }

    /// A list's items.
    fn items(&self) -> Vec<Py> {
        match self.node() {
            Node::List(items) => items.iter().map(|c| self.at(*c)).collect(),
            _ => Vec::new(),
        }
    }

    /// `bool(x)`: NaN is true.
    pub fn truthy(&self) -> bool {
        match self.node() {
            Node::Null => false,
            Node::Bool(b) => *b,
            Node::Int(s) => s != "0",
            Node::Float(f) => *f != 0.0,
            Node::Str(s) => !s.is_empty(),
            Node::List(a) => !a.is_empty(),
            Node::Dict(o) => !o.is_empty(),
        }
    }

    /// `type(x).__name__`.
    pub fn type_name(&self) -> &'static str {
        match self.node() {
            Node::Null => "NoneType",
            Node::Bool(_) => "bool",
            Node::Int(_) => "int",
            Node::Float(_) => "float",
            Node::Str(_) => "str",
            Node::List(_) => "list",
            Node::Dict(_) => "dict",
        }
    }

    /// `str(x)`; a lone surrogate in a str becomes U+FFFD.
    pub fn py_str(&self) -> String {
        match self.node() {
            Node::Str(s) => lossy(s),
            _ => self.repr(0),
        }
    }

    /// `repr(x)`.
    fn repr(&self, depth: usize) -> String {
        let deeper = |c: usize| {
            if depth >= SHOW_DEPTH { "...".to_string() } else { self.at(c).repr(depth + 1) }
        };
        match self.node() {
            Node::Null => "None".into(),
            Node::Bool(b) => (if *b { "True" } else { "False" }).into(),
            Node::Int(s) => s.clone(),
            Node::Float(f) => py_float_str_repr(*f),
            Node::Str(s) => repr_pystr(s),
            Node::List(items) => format!("[{}]", items.iter().map(|c| deeper(*c)).collect::<Vec<_>>().join(", ")),
            Node::Dict(entries) => format!(
                "{{{}}}",
                entries.iter().map(|(k, c)| format!("{}: {}", repr_pystr(k), deeper(*c))).collect::<Vec<_>>().join(", ")
            ),
        }
    }

    /// The value as a dict key: `Ok(None)` for NaN (equal to nothing), and
    /// TypeError for a list or dict, which cannot be one.
    fn key(&self) -> Result<Option<PyKey>, String> {
        Ok(Some(match self.node() {
            Node::Null => PyKey::Null,
            Node::Bool(b) => PyKey::Num(u8::from(*b).to_string()),
            Node::Int(s) => PyKey::Num(s.clone()),
            Node::Float(f) if f.is_nan() => return Ok(None),
            Node::Float(f) if *f == 0.0 => PyKey::Num("0".into()),
            // A whole float is equal to the int with its exact digits.
            Node::Float(f) if f.is_finite() && f.fract() == 0.0 => PyKey::Num(format!("{f:.0}")),
            Node::Float(f) => PyKey::Float(py_float_str_repr(*f)),
            Node::Str(s) => PyKey::Str(s.clone()),
            Node::List(_) | Node::Dict(_) => {
                return Err(format!("TypeError: unhashable type: '{}'", self.type_name()))
            }
        }))
    }

    /// As a serde value, for the parity hook and callers that need one: what
    /// serde cannot hold degrades (a lone surrogate to U+FFFD, NaN and the
    /// infinities to null, an int past u64/i64 to the nearest float, nesting
    /// past SHOW_DEPTH to null).
    pub fn to_value(&self) -> Value {
        self.value_at(0)
    }

    fn value_at(&self, depth: usize) -> Value {
        let float = |f: f64| serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number);
        if depth > SHOW_DEPTH {
            return Value::Null;
        }
        match self.node() {
            Node::Null => Value::Null,
            Node::Bool(b) => Value::Bool(*b),
            Node::Int(s) => s.parse::<i64>().map(Value::from)
                .or_else(|_| s.parse::<u64>().map(Value::from))
                .unwrap_or_else(|_| float(s.parse::<f64>().unwrap_or(f64::NAN))),
            Node::Float(f) => float(*f),
            Node::Str(s) => Value::String(lossy(s)),
            Node::List(items) => Value::Array(items.iter().map(|c| self.at(*c).value_at(depth + 1)).collect()),
            Node::Dict(entries) => {
                Value::Object(entries.iter().map(|(k, c)| (lossy(k), self.at(*c).value_at(depth + 1))).collect())
            }
        }
    }

    /// `json.dumps(x)`: default separators, `ensure_ascii`, NaN / Infinity
    /// written as such, lone surrogates as the escapes they were read from.
    pub fn dumps(&self) -> String {
        enum Step<'a> {
            Val(usize),
            Text(&'static str),
            Key(&'a [u32]),
        }
        let mut out = String::new();
        let mut todo = vec![Step::Val(self.id)];
        while let Some(step) = todo.pop() {
            match step {
                Step::Text(t) => out.push_str(t),
                Step::Key(k) => {
                    push_json_str(&mut out, k);
                    out.push_str(": ");
                }
                Step::Val(id) => match &self.doc.nodes[id] {
                    Node::Null => out.push_str("null"),
                    Node::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
                    Node::Int(s) => out.push_str(s),
                    Node::Float(f) => out.push_str(&util::py_float_repr(*f)),
                    Node::Str(s) => push_json_str(&mut out, s),
                    Node::List(items) => {
                        out.push('[');
                        todo.push(Step::Text("]"));
                        for (n, c) in items.iter().enumerate().rev() {
                            todo.push(Step::Val(*c));
                            if n > 0 {
                                todo.push(Step::Text(", "));
                            }
                        }
                    }
                    Node::Dict(entries) => {
                        out.push('{');
                        todo.push(Step::Text("}"));
                        for (n, (k, c)) in entries.iter().enumerate().rev() {
                            todo.push(Step::Val(*c));
                            todo.push(Step::Key(k));
                            if n > 0 {
                                todo.push(Step::Text(", "));
                            }
                        }
                    }
                },
            }
        }
        out
    }
}

/// `repr(str)`, lone surrogates as `\udXXX`.
fn repr_pystr(s: &[u32]) -> String {
    let text = lossy(s);
    let q = if text.contains('\'') && !text.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(q);
    for &c in s {
        match char::from_u32(c) {
            Some('\\') => out.push_str("\\\\"),
            Some('\n') => out.push_str("\\n"),
            Some('\r') => out.push_str("\\r"),
            Some('\t') => out.push_str("\\t"),
            Some(ch) if ch == q => {
                out.push('\\');
                out.push(ch);
            }
            Some(ch) if (ch as u32) < 0x20 || ch as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", ch as u32)),
            Some(ch) => out.push(ch),
            None => out.push_str(&format!("\\u{c:04x}")),
        }
    }
    out.push(q);
    out
}

/// A string the way `json.dumps` writes it (`ensure_ascii=True`).
fn push_json_str(out: &mut String, s: &[u32]) {
    out.push('"');
    for &c in s {
        match c {
            0x22 => out.push_str("\\\""),
            0x5c => out.push_str("\\\\"),
            0x0a => out.push_str("\\n"),
            0x0d => out.push_str("\\r"),
            0x09 => out.push_str("\\t"),
            0x08 => out.push_str("\\b"),
            0x0c => out.push_str("\\f"),
            0x20..=0x7e => out.push(char::from(c as u8)),
            0x10000.. => {
                let v = c - 0x10000;
                out.push_str(&format!("\\u{:04x}\\u{:04x}", 0xd800 | (v >> 10), 0xdc00 | (v & 0x3ff)));
            }
            _ => out.push_str(&format!("\\u{c:04x}")),
        }
    }
    out.push('"');
}

/// Why `json.loads` failed.
enum LoadErr {
    /// A ValueError (JSONDecodeError): every caller here catches it.
    Value,
    /// A RecursionError, which none of them catches: its text.
    Recursion(String),
}

/// Nesting at which CPython 3.14's C json scanner raises RecursionError.
/// Its limit is a C-stack byte budget, not a count (settings.rs measured
/// it): a worker thread decodes 52154 containers and not one more; the main
/// thread's limit moves from run to run, 52098 to 52131, and this is the
/// middle of that range.
const MAX_DEPTH_MAIN: usize = 52118;
const MAX_DEPTH_THREAD: usize = 52154;

fn on_main_thread() -> bool {
    std::thread::current().name() == Some("main")
}

/// CPython's RecursionError text for a container that would nest too deep.
fn recursion_message(is_obj: bool) -> String {
    let used = if on_main_thread() { 8144 } else { 8156 };
    let what = if is_obj { "object" } else { "array" };
    format!("RecursionError: Stack overflow (used {used} kB) while decoding a JSON {what} from a unicode string")
}

/// `int(str)` refuses more digits than this (`sys.get_int_max_str_digits()`),
/// with a ValueError.
const INT_MAX_STR_DIGITS: usize = 4300;

/// `json.loads(text)`, following CPython's C scanner.
fn py_loads(text: &str) -> Result<Py, LoadErr> {
    let s: Vec<char> = text.chars().collect();
    // json.loads refuses a str that starts with a BOM.
    if s.first() == Some(&'\u{feff}') {
        return Err(LoadErr::Value);
    }
    let ws = |mut i: usize| {
        while matches!(s.get(i), Some(' ' | '\t' | '\n' | '\r')) {
            i += 1;
        }
        i
    };
    let mut nodes: Vec<Node> = Vec::new();
    // Open containers: index, and for a dict the key awaiting its value and
    // where each key already is (a repeated key keeps its place, last value).
    struct Open {
        id: usize,
        key: Option<PyStr>,
        seen: std::collections::HashMap<PyStr, usize>,
    }
    let mut stack: Vec<Open> = Vec::new();
    let limit = if on_main_thread() { MAX_DEPTH_MAIN } else { MAX_DEPTH_THREAD };
    let mut i = ws(0);
    // A dict key and its colon, from `"` on; returns the key.
    let key_at = |i: &mut usize| -> Result<PyStr, LoadErr> {
        if s.get(*i) != Some(&'"') {
            return Err(LoadErr::Value);
        }
        let key = parse_string(&s, i)?;
        *i = ws(*i);
        if s.get(*i) != Some(&':') {
            return Err(LoadErr::Value);
        }
        *i = ws(*i + 1);
        Ok(key)
    };
    'value: loop {
        let mut node = match s.get(i) {
            Some(c @ ('{' | '[')) => {
                let is_obj = *c == '{';
                if stack.len() + 1 > limit {
                    return Err(LoadErr::Recursion(recursion_message(is_obj)));
                }
                nodes.push(if is_obj { Node::Dict(Vec::new()) } else { Node::List(Vec::new()) });
                let id = nodes.len() - 1;
                i = ws(i + 1);
                if s.get(i) == Some(if is_obj { &'}' } else { &']' }) {
                    i += 1;
                    id
                } else {
                    let key = if is_obj { Some(key_at(&mut i)?) } else { None };
                    stack.push(Open { id, key, seen: Default::default() });
                    continue 'value;
                }
            }
            Some('"') => {
                let v = parse_string(&s, &mut i)?;
                nodes.push(Node::Str(v));
                nodes.len() - 1
            }
            Some(_) => {
                let v = parse_scalar(&s, &mut i)?;
                nodes.push(v);
                nodes.len() - 1
            }
            None => return Err(LoadErr::Value),
        };
        // A value is complete: hand it to its container, and close every
        // container that ends here.
        loop {
            let Some(top) = stack.last_mut() else {
                // The top level: nothing but whitespace may follow.
                if ws(i) != s.len() {
                    return Err(LoadErr::Value);
                }
                return Ok(Py { doc: std::sync::Arc::new(Doc { nodes }), id: node });
            };
            let is_obj = top.key.is_some();
            match top.key.take() {
                Some(key) => {
                    if let Node::Dict(entries) = &mut nodes[top.id] {
                        match top.seen.get(&key) {
                            Some(&at) => entries[at].1 = node,
                            None => {
                                top.seen.insert(key.clone(), entries.len());
                                entries.push((key, node));
                            }
                        }
                    }
                }
                None => {
                    if let Node::List(items) = &mut nodes[top.id] {
                        items.push(node);
                    }
                }
            }
            i = ws(i);
            match s.get(i) {
                Some(',') => {
                    i = ws(i + 1);
                    if is_obj {
                        top.key = Some(key_at(&mut i)?);
                    }
                    continue 'value;
                }
                Some('}') if is_obj => {}
                Some(']') if !is_obj => {}
                _ => return Err(LoadErr::Value),
            }
            i += 1;
            node = top.id;
            stack.pop();
        }
    }
}

/// A string from its opening quote; `i` ends past the closing one.
fn parse_string(s: &[char], i: &mut usize) -> Result<PyStr, LoadErr> {
    let hex4 = |at: usize| -> Result<u32, LoadErr> {
        let mut v = 0;
        for k in 0..4 {
            let d = s.get(at + k).and_then(|c| c.to_digit(16).filter(|_| c.is_ascii()));
            v = v * 16 + d.ok_or(LoadErr::Value)?;
        }
        Ok(v)
    };
    let mut out = Vec::new();
    let mut j = *i + 1;
    loop {
        let c = *s.get(j).ok_or(LoadErr::Value)?;
        j += 1;
        match c {
            '"' => break,
            '\\' => {
                let e = *s.get(j).ok_or(LoadErr::Value)?;
                j += 1;
                out.push(match e {
                    '"' => 0x22,
                    '\\' => 0x5c,
                    '/' => 0x2f,
                    'b' => 0x08,
                    'f' => 0x0c,
                    'n' => 0x0a,
                    'r' => 0x0d,
                    't' => 0x09,
                    'u' => {
                        let mut v = hex4(j)?;
                        j += 4;
                        // A high surrogate followed by an escaped low one is
                        // one character; anything else stays as it is.
                        if (0xd800..0xdc00).contains(&v) && s.get(j) == Some(&'\\') && s.get(j + 1) == Some(&'u') {
                            let low = hex4(j + 2)?;
                            if (0xdc00..0xe000).contains(&low) {
                                v = 0x10000 + ((v - 0xd800) << 10) + (low - 0xdc00);
                                j += 6;
                            }
                        }
                        v
                    }
                    _ => return Err(LoadErr::Value),
                });
            }
            // strict=True: no raw control characters.
            c if (c as u32) < 0x20 => return Err(LoadErr::Value),
            c => out.push(c as u32),
        }
    }
    *i = j;
    Ok(out)
}

/// null / true / false / NaN / Infinity / -Infinity, or a number.
fn parse_scalar(s: &[char], i: &mut usize) -> Result<Node, LoadErr> {
    let starts = |word: &str| word.chars().enumerate().all(|(k, c)| s.get(*i + k) == Some(&c));
    for (word, node) in [
        ("null", Node::Null),
        ("true", Node::Bool(true)),
        ("false", Node::Bool(false)),
        ("NaN", Node::Float(f64::NAN)),
        ("Infinity", Node::Float(f64::INFINITY)),
        ("-Infinity", Node::Float(f64::NEG_INFINITY)),
    ] {
        if starts(word) {
            *i += word.chars().count();
            return Ok(node);
        }
    }
    // -?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][-+]?[0-9]+)?
    let digit = |k: usize| s.get(k).is_some_and(char::is_ascii_digit);
    let start = *i;
    let mut j = start;
    if s.get(j) == Some(&'-') {
        j += 1;
    }
    match s.get(j) {
        Some('0') => j += 1,
        Some('1'..='9') => {
            while digit(j) {
                j += 1;
            }
        }
        _ => return Err(LoadErr::Value),
    }
    let mut is_float = false;
    if s.get(j) == Some(&'.') && digit(j + 1) {
        j += 1;
        while digit(j) {
            j += 1;
        }
        is_float = true;
    }
    if matches!(s.get(j), Some('e' | 'E')) {
        let mut k = j + 1;
        if matches!(s.get(k), Some('+' | '-')) {
            k += 1;
        }
        if digit(k) {
            while digit(k) {
                k += 1;
            }
            j = k;
            is_float = true;
        }
    }
    let lit: String = s[start..j].iter().collect();
    *i = j;
    if is_float {
        // float(): correctly rounded, and inf past the largest double --
        // as Rust's parser is.
        return lit.parse::<f64>().map(Node::Float).map_err(|_| LoadErr::Value);
    }
    let digits = lit.trim_start_matches('-');
    if digits.len() > INT_MAX_STR_DIGITS {
        return Err(LoadErr::Value);
    }
    // int("-0") is 0.
    Ok(Node::Int(if digits == "0" { "0".to_owned() } else { lit }))
}

/// A Python dict keyed by JSON values: a key keeps the object it was first
/// stored under, as a dict does when an equal key is stored again. A NaN key
/// is equal to nothing, so each is an entry of its own and never found.
struct PyDict<T> {
    index: BTreeMap<PyKey, usize>,
    items: Vec<(Py, T)>,
}

impl<T> PyDict<T> {
    fn new() -> Self {
        PyDict { index: BTreeMap::new(), items: Vec::new() }
    }

    fn get(&self, k: &Py) -> Result<Option<&T>, String> {
        Ok(k.key()?.and_then(|k| self.index.get(&k)).map(|&n| &self.items[n].1))
    }

    /// `d[k] = v`.
    fn set(&mut self, k: Py, v: T) -> Result<(), String> {
        match k.key()? {
            Some(key) => match self.index.get(&key) {
                Some(&n) => self.items[n].1 = v,
                None => {
                    self.index.insert(key, self.items.len());
                    self.items.push((k, v));
                }
            },
            None => self.items.push((k, v)),
        }
        Ok(())
    }

    /// `d.setdefault(k, v)`.
    fn setdefault(&mut self, k: Py, v: T) -> Result<(), String> {
        if self.get(&k)?.is_none() {
            self.set(k, v)?;
        }
        Ok(())
    }
}

/// `tuple(x)` for `kinds`: a list's items, a string's characters, a dict's
/// keys; TypeError for anything else (it is truthy, or it would not be here).
fn kinds_of(v: &Py) -> Result<Vec<Py>, String> {
    Ok(match v.node() {
        Node::List(_) => v.items(),
        Node::Str(s) => s.iter().map(|&c| Py::leaf(Node::Str(vec![c]))).collect(),
        Node::Dict(entries) => entries.iter().map(|(k, _)| Py::leaf(Node::Str(k.clone()))).collect(),
        _ => return Err(format!("TypeError: '{}' object is not iterable", v.type_name())),
    })
}

/// `Path.rglob("manifest.json")`, in the order Python 3.14 walks it: the
/// root's own manifest, then a stack of directories (last found, first
/// walked), each directory's entries in `scandir` order, a subdirectory's
/// manifest yielded as the subdirectory is found. Symlinked directories are
/// not followed.
fn rglob_manifests(root: &Path) -> Vec<PathBuf> {
    let exists = |p: &Path| p.symlink_metadata().is_ok();
    let mut found = Vec::new();
    if exists(&root.join("manifest.json")) {
        found.push(root.join("manifest.json"));
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let path = entry.path();
            if exists(&path.join("manifest.json")) {
                found.push(path.join("manifest.json"));
            }
            stack.push(path);
        }
    }
    found
}

/// `json.loads(path.read_text())` under `except (OSError, ValueError)`:
/// `Ok(None)` where that except is taken (unreadable, not UTF-8, not JSON),
/// `Err` for the RecursionError it does not catch.
fn load_file(path: &Path) -> Result<Option<Py>, String> {
    let Ok(text) = std::fs::read_to_string(path) else { return Ok(None) };
    match py_loads(&text) {
        Ok(v) => Ok(Some(v)),
        Err(LoadErr::Value) => Ok(None),
        Err(LoadErr::Recursion(e)) => Err(e),
    }
}

/// id -> (manifest, folder) for every manifest under `root`. The ids are
/// whatever truthy value the manifest holds; `Err` is the TypeError of one
/// that cannot be a dict key (a list or dict), or a RecursionError.
fn _manifests(root: &Path) -> Result<PyDict<(Py, PathBuf)>, String> {
    let root = py_path(root);
    let mut out = PyDict::new();
    let found = if root.is_dir() { rglob_manifests(&root) } else { Vec::new() };
    for m in found {
        let Some(d) = load_file(&m)? else { continue };
        if let Some(id) = d.get("id").filter(Py::truthy) {
            let folder = m.parent().map(Path::to_path_buf).unwrap_or_default();
            out.setdefault(id, (d, folder))?;
        }
    }
    Ok(out)
}

/// `omarchy plugin list --json`, parsed; `Ok(None)` where the Python's
/// `except (OSError, SubprocessError, ValueError)` returns early (omarchy
/// missing, timed out, output that is not UTF-8 or not JSON), `Err` for the
/// RecursionError it lets through.
fn omarchy_list() -> Result<Option<Py>, String> {
    let argv = ["omarchy", "plugin", "list", "--json"].map(OsStr::new);
    let Ok(out) = py_run(&argv, &[], 25) else { return Ok(None) };
    match py_loads(&out.stdout) {
        Ok(v) => Ok(Some(v)),
        Err(LoadErr::Value) => Ok(None),
        Err(LoadErr::Recursion(e)) => Err(e),
    }
}

/// Iterating the parsed list: a list's items, a string's characters or a
/// dict's keys (never dicts, so nothing); TypeError for a number, a bool or
/// null. Only the dicts with a truthy id are kept.
fn listed_dicts(v: &Py) -> Result<Vec<Py>, String> {
    match v.node() {
        Node::List(_) => Ok(v.items().into_iter()
            .filter(|i| i.is_dict() && i.get("id").is_some_and(|x| x.truthy())).collect()),
        Node::Str(_) | Node::Dict(_) => Ok(Vec::new()),
        _ => Err(format!("TypeError: '{}' object is not iterable", v.type_name())),
    }
}

/// A value for a String field: the string itself, or `str()` of it and the
/// value kept in `raw`.
fn split_str(v: &Py, plain: impl Fn(&Py) -> String) -> (String, Option<Py>) {
    match v.node() {
        Node::Str(s) => (lossy(s), None),
        _ => (plain(v), Some(v.clone())),
    }
}

/// `str.lower()` as a sort key: by code point, each lowered as Python lowers
/// it (a lone surrogate stays itself).
fn lower_key(s: &[u32]) -> Vec<u32> {
    s.iter().flat_map(|&c| match char::from_u32(c) {
        Some(ch) => ch.to_lowercase().map(u32::from).collect::<Vec<_>>(),
        None => vec![c],
    }).collect()
}

/// Every plugin Omarchy knows about, with what each one is and its state.
///
/// Omarchy's own list decides what exists and whether it is on; manifests
/// supply the description, and the user's own plugins are classified so the
/// page can say whether each can be updated.
///
/// Where the Python raises (omarchy printing a bare number, a manifest that
/// is a list, a name that is not a string), this is an empty list;
/// `try_installed` returns the error instead.
pub fn installed(fetch: bool, cache: Option<&mut FetchCache>) -> Vec<Installed> {
    try_installed(fetch, cache).unwrap_or_default()
}

/// `installed` as the Python has it: `Err` for an exception it does not catch.
pub fn try_installed(fetch: bool, mut cache: Option<&mut FetchCache>) -> Result<Vec<Installed>, String> {
    if crate::demo::active() {
        return Ok(crate::demo::plugins());
    }
    let Some(parsed) = omarchy_list()? else { return Ok(Vec::new()) };
    let listed = listed_dicts(&parsed)?;
    let omarchy_path = std::env::var_os("OMARCHY_PATH").filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/usr/share/omarchy".into());
    let shipped = _manifests(&PathBuf::from(omarchy_path).join("shell").join("plugins"))?;
    let mut mine: PyDict<(Py, PathBuf)> = PyDict::new();
    let pdir = plugins_dir();
    let mut dirs: Vec<PathBuf> = if pdir.is_dir() {
        std::fs::read_dir(&pdir).map(|r| r.flatten().map(|e| e.path()).collect()).unwrap_or_default()
    } else {
        Vec::new()
    };
    dirs.sort();
    for d in dirs {
        let Some(m) = load_file(&d.join("manifest.json"))? else { continue };
        // `m.get("id") or d.name`: AttributeError for a manifest that is
        // not a dict; the id kept as whatever it is.
        if !m.is_dict() {
            return Err(format!("AttributeError: '{}' object has no attribute 'get'", m.type_name()));
        }
        let key = m.get("id").filter(Py::truthy).unwrap_or_else(|| Py::string(&py_name(&d)));
        mine.set(key, (m, d))?;
    }

    let mut result = Vec::new();
    for item in listed {
        let Some(pid) = item.get("id") else { continue };
        let (manifest, folder) = match mine.get(&pid)? {
            Some((m, f)) => (Some(m), Some(f.clone())),
            None => match shipped.get(&pid)? {
                Some((m, f)) => (Some(m), Some(f.clone())),
                None => (None, None),
            },
        };
        let own = mine.get(&pid)?.is_some();
        let from_manifest = |k: &str| manifest.and_then(|m| m.get(k));
        // `item.get("name") or manifest.get("name") or pid`: a name that
        // is not a string makes the sort below raise.
        let name = [item.get("name"), from_manifest("name")].into_iter().flatten()
            .find(Py::truthy).unwrap_or_else(|| pid.clone());
        let missing = Py::string("");
        let (description, raw_description) =
            split_str(&from_manifest("description").unwrap_or_else(|| missing.clone()), |_| String::new());
        let (version, raw_version) = split_str(&from_manifest("version").unwrap_or(missing), |_| String::new());
        let kinds = match [item.get("kinds"), from_manifest("kinds")].into_iter().flatten().find(Py::truthy) {
            Some(k) => kinds_of(&k)?,
            None => Vec::new(),
        };
        let raw_kinds = (!kinds.iter().all(Py::is_str)).then(|| kinds.clone());
        let state = match (&folder, own) {
            (Some(f), true) => Some(try_classify(f, cache.as_deref_mut(), fetch)?),
            _ => None,
        };
        let (id, raw_id) = split_str(&pid, Py::py_str);
        result.push((name.clone(), Installed {
            id,
            name: name.py_str(),
            description,
            version,
            kinds: kinds.iter().map(Py::py_str).collect(),
            enabled: item.get("enabled").is_some_and(|v| v.truthy()),
            can_disable: item.get("canDisable").is_none_or(|v| v.truthy()),
            built_in: item.get("firstParty").is_some_and(|v| v.truthy()) && !own,
            folder,
            state,
            raw: Raw { id: raw_id, description: raw_description, version: raw_version, kinds: raw_kinds },
        }));
    }
    // `key=lambda i: (i.built_in, i.name.lower())`: every key is computed
    // first, so the first name that is not a string raises.
    let mut keyed = Vec::with_capacity(result.len());
    for (name, inst) in result {
        let Node::Str(s) = name.node() else {
            return Err(format!("AttributeError: '{}' object has no attribute 'lower'", name.type_name()));
        };
        keyed.push(((inst.built_in, lower_key(s)), inst));
    }
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(keyed.into_iter().map(|(_, i)| i).collect())
}

// ---- what to show beside each plugin -------------------------------------------

/// A plugin about a service carries that service's mark, fetched from the
/// vendor's own domain (icons.py) -- matched on the plugin's id and name only,
/// never its description, where "works with Slack" would mislabel it.
pub const VENDORS: &[(&str, &str)] = &[
    ("tailscale", "tailscale.com"), ("slack", "slack.com"),
    ("spotify", "spotify.com"), ("github", "github.com"),
    ("dropbox", "dropbox.com"), ("1password", "1password.com"),
    ("docker", "docker.com"), ("obsidian", "obsidian.md"),
    ("discord", "discord.com"), ("youtube", "youtube.com"),
    ("google", "google.com"), ("todoist", "todoist.com"),
    ("notion", "notion.so"), ("proton", "proton.me"),
    ("steam", "steampowered.com"), ("twitch", "twitch.tv"),
    ("claude", "anthropic.com"), ("anthropic", "anthropic.com"),
    ("openai", "openai.com"), ("chatgpt", "openai.com"),
    ("home assistant", "home-assistant.io"), ("jellyfin", "jellyfin.org"),
];

/// Otherwise, a system icon for what it does. Adwaita names only, each checked
/// to exist, so the row looks right under any icon theme that follows it.
pub const SYMBOLS: &[(&str, &str)] = &[
    ("bluetooth", "bluetooth-symbolic"),
    ("wifi", "network-wireless-symbolic"), ("network", "network-wireless-symbolic"),
    ("tailscale", "network-transmit-receive-symbolic"),
    ("vpn", "network-transmit-receive-symbolic"),
    ("audio", "audio-volume-high-symbolic"), ("volume", "audio-volume-high-symbolic"),
    ("sound", "audio-volume-high-symbolic"),
    ("microphone", "microphone-sensitivity-high-symbolic"),
    ("media", "media-playback-start-symbolic"), ("music", "media-playback-start-symbolic"),
    ("battery", "battery-full-symbolic"), ("power", "system-shutdown-symbolic"),
    ("clock", "preferences-system-time-symbolic"), ("time", "preferences-system-time-symbolic"),
    ("reminder", "alarm-symbolic"), ("alarm", "alarm-symbolic"),
    ("weather", "weather-few-clouds-symbolic"),
    ("clipboard", "edit-paste-symbolic"), ("emoji", "face-smile-symbolic"),
    ("image", "image-x-generic-symbolic"), ("screenshot", "camera-photo-symbolic"),
    ("lock", "system-lock-screen-symbolic"), ("idle", "system-lock-screen-symbolic"),
    ("polkit", "dialog-password-symbolic"), ("password", "dialog-password-symbolic"),
    ("menu", "open-menu-symbolic"), ("background", "preferences-desktop-wallpaper-symbolic"),
    ("wallpaper", "preferences-desktop-wallpaper-symbolic"),
    ("appearance", "preferences-desktop-appearance-symbolic"),
    ("theme", "preferences-desktop-appearance-symbolic"),
    ("monitor", "video-display-symbolic"), ("display", "video-display-symbolic"),
    ("nightlight", "night-light-symbolic"), ("night", "night-light-symbolic"),
    ("notification", "preferences-system-notifications-symbolic"),
    ("osd", "preferences-system-notifications-symbolic"),
    ("keyboard", "preferences-desktop-keyboard-symbolic"),
    ("update", "software-update-available-symbolic"),
    ("disk", "drive-harddisk-symbolic"), ("speedtest", "content-loading-symbolic"),
    ("workspace", "view-grid-symbolic"), ("window", "window-new-symbolic"),
    ("tray", "view-app-grid-symbolic"), ("bar", "view-app-grid-symbolic"),
    ("agent", "utilities-terminal-symbolic"), ("dev", "utilities-terminal-symbolic"),
    ("vitals", "emblem-system-symbolic"), ("system", "emblem-system-symbolic"),
    ("radar", "weather-few-clouds-symbolic"),
];
pub const GENERIC: &str = "application-x-addon-symbolic";

fn _words(texts: &[&str]) -> String {
    let parts: Vec<String> = texts
        .iter()
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase().replace('.', " ").replace('-', " ").replace('_', " "))
        .collect();
    format!(" {} ", parts.join(" "))
}

/// The plugin's name and the last part of its id. The rest of an id is a
/// reverse-DNS namespace ("io.github.someone"), and the kind ("bar-widget")
/// says where it sits, not what it is -- matching either mislabels it.
fn _own_words(i: &Installed) -> String {
    let last = i.id.rsplit('.').next().unwrap_or_default();
    _words(&[last, &i.name])
}

/// The domain of the service a plugin is about, or ''.
pub fn vendor_for(i: &Installed) -> String {
    let words = _own_words(i);
    for (word, domain) in VENDORS {
        if words.contains(&format!(" {word}")) {
            return domain.to_string();
        }
    }
    String::new()
}

/// A system icon for what the plugin does, from its name and id.
pub fn symbol_for(i: &Installed) -> String {
    let words = _own_words(i);
    for (word, icon) in SYMBOLS {
        if words.contains(&format!(" {word}")) {
            return icon.to_string();
        }
    }
    GENERIC.to_string()
}

/// `re.match(r"(?:https://github\.com/|git@github\.com:)([\w.-]+)/", url)`.
/// Python's `\w` is `str.isalnum()` or "_": letters and numbers by general
/// category, not Rust's `is_alphanumeric`, which also takes combining marks
/// and circled letters.
pub fn github_owner(url: &str) -> String {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"^(?:https://github\.com/|git@github\.com:)([\p{L}\p{N}_.-]+)/").unwrap()
    });
    re.captures(url).and_then(|c| c.get(1)).map(|m| m.as_str().to_string()).unwrap_or_default()
}

/// id -> enabled, from Omarchy's own list. An id that is not a string is
/// keyed by its `str()` ("5", "True").
///
/// Where the Python raises (omarchy printing a bare number), this is empty;
/// `try_enabled` returns the error instead.
pub fn enabled() -> BTreeMap<String, bool> {
    try_enabled().unwrap_or_default()
}

/// `enabled` as the Python has it: `Err` for an exception it does not catch.
pub fn try_enabled() -> Result<BTreeMap<String, bool>, String> {
    if crate::demo::active() {
        return Ok(crate::demo::plugins().into_iter().map(|i| (i.id, i.enabled)).collect());
    }
    let Some(items) = omarchy_list()? else { return Ok(BTreeMap::new()) };
    let mut out: PyDict<bool> = PyDict::new();
    for i in listed_dicts(&items)? {
        let Some(pid) = i.get("id") else { continue };
        out.set(pid, i.get("enabled").is_some_and(|v| v.truthy()))?;
    }
    Ok(out.items.into_iter().map(|(k, on)| (k.py_str(), on)).collect())
}

pub fn toggle_argv(pid: &str, on: bool) -> Vec<String> {
    vec!["omarchy".into(), "plugin".into(),
         (if on { "enable" } else { "disable" }).into(), pid.to_string()]
}

pub fn update_argv(pid: &str) -> Vec<String> {
    vec!["omarchy".into(), "plugin".into(), "update".into(), pid.to_string(), "--yes".into()]
}

/// Whether a plugin with this provenance is covered by a bulk update.
/// `trust_key` is None for a plugin the marketplace does not list.
pub fn in_scope(trust_key: Option<&str>, scope: &str) -> bool {
    let allowed = SCOPES.iter().find(|(k, _l, _s)| *k == scope).and_then(|(_k, _l, s)| *s);
    let Some(allowed) = allowed else { return true };
    trust_key.is_some_and(|t| allowed.contains(&t))
}

/// Restart the shell -- never `omarchy-refresh-shell`, which resets
/// shell.json to its defaults. Clearing Quickshell's compiled QML cache
/// first fixes a plugin acting on stale compiled code. Detached, since the
/// shell being restarted may be what started it.
pub fn restart_argv(clear_cache: bool) -> Vec<String> {
    let cache = path_str(&crate::xdg::cache_home().join("quickshell"));
    let mut script = "omarchy-restart-shell".to_string();
    if clear_cache {
        script = format!("rm -rf \"{cache}/qmlcache\" \"{cache}\"/qtpipelinecache-*; ") + &script;
    }
    vec!["setsid".into(), "-f".into(), "bash".into(), "-c".into(), script]
}

// ---- settings ------------------------------------------------------------------

/// The 1.4.0 scope file, `state_home/almanac/plugins.json`.
pub fn _settings_path() -> PathBuf {
    py_path(&crate::xdg::state_home().join("almanac").join("plugins.json"))
}

/// The bulk-update scope; "all" where `try_scope` raises.
pub fn scope() -> String {
    try_scope().unwrap_or_else(|_| "all".to_string())
}

/// `scope` as the Python has it: `Err` for the TypeError of a carried-over
/// scope that is a list or a dict (`value in {...}` cannot hash it), and
/// the RecursionError of a file nested too deep.
pub fn try_scope() -> Result<String, String> {
    // settings.get, exactly (a str setting may hold a lone surrogate).
    let current = crate::settings::get_exact("plugin_scope")?;
    let mut value = py_loads(&current.to_json()).map_err(|_| "ValueError: settings value".to_string())?;
    let old = _settings_path();
    if old.is_file() {
        // 1.4.0 kept this in its own file: carry it over once.
        // `except (OSError, ValueError, AttributeError): pass` -- unreadable,
        // not JSON, or not a dict.
        let carried = load_file(&old)?
            .filter(Py::is_dict)
            .map(|d| d.get("scope").unwrap_or_else(|| value.clone()));
        if let Some(v) = carried {
            value = v;
            // Handed to settings as JSON text, so what serde cannot hold
            // (NaN, an int of any size, a lone surrogate) is written as the
            // Python writes it.
            match crate::settings::Setting::from_json(&value.dumps())
                .and_then(|s| crate::settings::set("plugin_scope", s))
            {
                Ok(()) => {
                    let _ = std::fs::remove_file(&old);
                }
                // settings raises RecursionError past the except; OSError is caught.
                Err(e) if e.starts_with("RecursionError") => return Err(e),
                Err(_) => {}
            }
        }
    }
    match value.node() {
        Node::Str(s) if SCOPES.iter().any(|(k, _l, _s)| py_str_of(k) == *s) => Ok(lossy(s)),
        Node::List(_) | Node::Dict(_) => Err(format!(
            "TypeError: cannot use '{0}' as a set element (unhashable type: '{0}')", value.type_name())),
        _ => Ok("all".to_string()),
    }
}

/// `Err` when the settings file cannot be written (the Python's OSError).
pub fn set_scope(value: &str) -> Result<(), String> {
    crate::settings::set("plugin_scope", Value::String(value.to_string()))
}



#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// The tests that set environment variables run one at a time.

    fn with_env<T>(vars: &[(&str, &str)], f: impl FnOnce() -> T) -> T {
        let _g = crate::util::TEST_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let saved: Vec<_> = vars.iter().map(|(k, _)| (*k, std::env::var_os(k))).collect();
        for (k, v) in vars {
            std::env::set_var(k, v);
        }
        let r = f();
        for (k, old) in saved {
            match old {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
        r
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("almanac-plugins-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn git(cwd: &Path, args: &[&str]) {
        let st = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(st.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&st.stderr));
    }

    /// PluginStates.setUp: an upstream with one commit, and a clone of it.
    struct Repos {
        d: PathBuf,
        up: PathBuf,
        plug: PathBuf,
    }

    impl Repos {
        fn new(tag: &str) -> Repos {
            let d = tmpdir(tag);
            let up = d.join("up");
            git(&d, &["init", "-q", "-b", "main", up.to_str().unwrap()]);
            std::fs::write(up.join("manifest.json"), "{}").unwrap();
            git(&up, &["add", "."]);
            git(&up, &["commit", "-qm", "one"]);
            let plug = d.join("plugins").join("a.plugin");
            git(&d, &["clone", "-q", up.to_str().unwrap(), plug.to_str().unwrap()]);
            Repos { d, up, plug }
        }

        fn advance_upstream(&self) {
            std::fs::write(self.up.join("f"), "x").unwrap();
            git(&self.up, &["add", "."]);
            git(&self.up, &["commit", "-qm", "two"]);
        }

        fn state(&self, d: Option<&Path>) -> State {
            classify(d.unwrap_or(&self.plug), Some(&mut FetchCache::new()), true)
        }
    }

    impl Drop for Repos {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.d);
        }
    }

    #[test]
    fn test_current_and_behind() {
        let r = Repos::new("current");
        assert_eq!(r.state(None).kind, CURRENT);
        r.advance_upstream();
        let st = r.state(None);
        assert_eq!((st.kind.as_str(), st.behind), (UPDATE, 1));
    }

    #[test]
    fn test_a_symlinked_development_plugin_is_never_updatable() {
        let r = Repos::new("symlink");
        r.advance_upstream();
        let link = r.d.join("plugins").join("dev.plugin");
        std::os::unix::fs::symlink(&r.plug, &link).unwrap();
        let st = r.state(Some(&link));
        assert_eq!((st.kind.as_str(), st.detail.as_str()), (LOCAL, "symlink"));
        assert!(!st.updatable());
    }

    #[test]
    fn a_symlink_named_with_a_trailing_slash_is_still_a_symlink() {
        // Path("link/") is Path("link"): the slash must not follow the link.
        let r = Repos::new("symlink-slash");
        r.advance_upstream();
        let link = r.d.join("plugins").join("dev.plugin");
        std::os::unix::fs::symlink(&r.plug, &link).unwrap();
        let st = r.state(Some(Path::new(&format!("{}/", link.display()))));
        assert_eq!((st.kind.as_str(), st.detail.as_str(), st.id.as_str()), (LOCAL, "symlink", "dev.plugin"));
    }

    #[test]
    fn paths_are_spelled_as_python_spells_them() {
        for (raw, want) in [("./a//b/./", "a/b"), ("", "."), (".", "."), ("//x", "//x"), ("///x", "/x"),
                            ("a/..", "a/.."), ("/", "/")] {
            assert_eq!(path_str(Path::new(raw)), want, "{raw:?}");
        }
        assert_eq!(py_name(Path::new("a/sub/..")), "..");
        assert_eq!(py_name(Path::new("/")), "");
    }

    #[test]
    fn test_edits_ahead_and_diverged_are_local_changes() {
        let r = Repos::new("edits");
        std::fs::write(r.plug.join("manifest.json"), r#"{"x": 1}"#).unwrap();
        assert_eq!(r.state(None).detail, "modified");
        git(&r.plug, &["commit", "-qam", "mine"]);
        assert_eq!(r.state(None).detail, "ahead");
        r.advance_upstream();
        let st = r.state(None);
        assert_eq!((st.kind.as_str(), st.detail.as_str()), (LOCAL_CHANGES, "diverged"));
    }

    #[test]
    fn test_plain_folders_are_local() {
        let r = Repos::new("plain");
        let plain = r.d.join("plugins").join("mine");
        std::fs::create_dir(&plain).unwrap();
        assert_eq!(r.state(Some(&plain)).detail, "not-git");
        assert!(!r.state(Some(&plain)).updatable());
    }

    #[test]
    fn test_bulk_update_scope() {
        assert!(in_scope(Some("verified"), "verified"));
        assert!(!in_scope(Some("reviewed-stale"), "verified"));
        assert!(in_scope(Some("reviewed-stale"), "changed"));
        assert!(!in_scope(None, "changed")); // unlisted
        assert!(in_scope(None, "all"));
    }

    #[test]
    fn test_restarting_never_refreshes() {
        let argv = restart_argv(true).join(" ");
        assert!(argv.contains("omarchy-restart-shell"));
        assert!(!argv.contains("refresh"));
    }

    #[test]
    fn test_plugins_are_synthetic() {
        // DemoModeHidesTheMachine: in demo mode the fixtures answer, not omarchy.
        with_env(&[("ALMANAC_DEMO", "1")], || {
            assert!(!installed(true, None).is_empty());
            assert!(!enabled().is_empty());
        });
    }

    #[test]
    fn test_every_plugin_is_listed_with_what_it_is() {
        let listed = serde_json::json!([
            {"id": "omarchy.clock", "name": "Clock", "kinds": ["bar-widget"],
             "enabled": true, "canDisable": true, "firstParty": true},
            {"id": "omarchy.bar", "name": "Bar", "kinds": ["bar"],
             "enabled": true, "canDisable": false, "firstParty": true},
            {"id": "me.thing", "name": "Thing", "kinds": ["panel"],
             "enabled": false, "canDisable": true, "firstParty": false},
        ]);
        let d = tmpdir("page");
        let shipped = d.join("omarchy/shell/plugins/clock");
        std::fs::create_dir_all(&shipped).unwrap();
        std::fs::write(shipped.join("manifest.json"),
                       serde_json::json!({"id": "omarchy.clock", "description": "The time"}).to_string()).unwrap();
        let mine = d.join("config/omarchy/plugins/me.thing");
        std::fs::create_dir_all(&mine).unwrap();
        std::fs::write(mine.join("manifest.json"),
                       serde_json::json!({"id": "me.thing", "version": "1.0"}).to_string()).unwrap();
        // The Python mocks subprocess.run; here a fake `omarchy` prints the list.
        let bin = d.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(d.join("list.json"), listed.to_string()).unwrap();
        let fake = bin.join("omarchy");
        std::fs::write(&fake, format!("#!/bin/sh\ncat '{}'\n", d.join("list.json").display())).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
        let omarchy_path = d.join("omarchy");
        let config = d.join("config");
        let found = with_env(&[("OMARCHY_PATH", omarchy_path.to_str().unwrap()),
                               ("XDG_CONFIG_HOME", config.to_str().unwrap()),
                               ("PATH", &path)], || installed(false, None));
        let _ = std::fs::remove_dir_all(&d);
        let by_id: BTreeMap<&str, &Installed> = found.iter().map(|i| (i.id.as_str(), i)).collect();
        assert_eq!(found[0].id, "me.thing"); // yours first
        assert!(!by_id["me.thing"].built_in);
        assert_eq!(by_id["me.thing"].state.as_ref().unwrap().kind, LOCAL); // no .git
        assert!(by_id["omarchy.clock"].built_in);
        assert_eq!(by_id["omarchy.clock"].description, "The time");
        assert!(!by_id["omarchy.bar"].can_disable);
        assert!(by_id["me.thing"].has_panel());
    }

    fn plugin(pid: &str, name: &str) -> Installed {
        Installed { id: pid.into(), name: name.into(), kinds: vec!["bar-widget".into()], ..Default::default() }
    }

    #[test]
    fn test_a_service_plugin_gets_its_vendor() {
        assert_eq!(vendor_for(&plugin("tim.tailscale", "My Tailscale")), "tailscale.com");
    }

    #[test]
    fn test_a_namespace_is_not_a_vendor() {
        let p = plugin("io.github.someone.lock-explorer", "Lock Screen Explorer");
        assert_eq!(vendor_for(&p), "");
        assert_eq!(symbol_for(&p), "system-lock-screen-symbolic");
    }

    #[test]
    fn test_the_kind_does_not_decide_the_icon() {
        // "bar-widget" must not make every bar plugin look like the bar.
        assert_eq!(symbol_for(&plugin("x.usage", "AI Usage")), GENERIC);
    }

    #[test]
    fn test_github_owner() {
        assert_eq!(github_owner("https://github.com/lxp/vitals.git"), "lxp");
        assert_eq!(github_owner("git@github.com:lxp/vitals.git"), "lxp");
        assert_eq!(github_owner("https://gitlab.com/a/b"), "");
    }

    #[test]
    fn json_reads_what_python_reads() {
        let v = py_loads(r#"{"a": 1e400, "b": "\ud800", "c": NaN, "d": 123456789012345678901234567890, "a": -0}"#)
            .ok().unwrap();
        assert_eq!(v.get("a").unwrap().dumps(), "0"); // a repeated key: the last value
        assert_eq!(v.get("b").unwrap().dumps(), r#""\ud800""#);
        assert!(v.get("c").unwrap().truthy()); // NaN is true
        assert_eq!(v.get("d").unwrap().py_str(), "123456789012345678901234567890");
        assert_eq!(v.dumps(), r#"{"a": 0, "b": "\ud800", "c": NaN, "d": 123456789012345678901234567890}"#);
        assert_eq!(py_loads("[1e400]").ok().unwrap().items()[0].py_str(), "inf");
        assert!(py_loads("\u{feff}{}").is_err());
        assert!(py_loads("[1,]").is_err());
        let deep = format!("{}{}", "[".repeat(60000), "]".repeat(60000));
        assert!(matches!(py_loads(&deep), Err(LoadErr::Recursion(_))));
    }

    #[test]
    fn strings_and_numbers_read_as_python_reads_them() {
        assert_eq!(py_strip("\x1c up\x1f \u{3000}"), "up");
        assert_eq!(py_float_str("\u{669}\u{660}"), Some(90.0)); // Arabic-Indic
        assert_eq!(py_float_str("\u{a0}9e1\u{3000}"), Some(90.0));
        assert_eq!(py_float_str("\x1c9"), None);
        assert_eq!(py_float_str("\u{ff19}\u{ff45}\u{ff11}"), None); // fullwidth e is not a digit
        assert_eq!(nd_value('\u{1d7d9}'), Some(1)); // double-struck 1, after the bold digits
        assert_eq!(py_decode(b"a\r\nb\rc".to_vec()).ok().unwrap(), "a\nb\nc");
        assert_eq!(py_decode("\u{fffd}".as_bytes().to_vec()).ok().unwrap(), "\u{fffd}");
        assert_eq!(py_decode(b"x\xff".to_vec()).unwrap_err(),
                   "UnicodeDecodeError: 'utf-8' codec can't decode byte 0xff in position 1: invalid start byte");
        assert_eq!(py_decode(b"\xe2\x82".to_vec()).unwrap_err(),
                   "UnicodeDecodeError: 'utf-8' codec can't decode bytes in position 0-1: unexpected end of data");
        assert_eq!(os_error(8, "git"), "[Errno 8] Exec format error: 'git'");
    }

    #[test]
    fn test_the_old_plugin_scope_file_is_carried_over() {
        let d = tmpdir("scope");
        with_env(&[("XDG_STATE_HOME", d.to_str().unwrap())], || {
            let old = _settings_path();
            std::fs::create_dir_all(old.parent().unwrap()).unwrap();
            std::fs::write(&old, r#"{"scope": "verified"}"#).unwrap();
            assert_eq!(scope(), "verified");
            assert!(!old.exists());
            assert_eq!(scope(), "verified");
        });
        let _ = std::fs::remove_dir_all(&d);
    }
}
