//! Port of `ai.py`.
//!
//! The AI estate: everything on this machine that is part of an agent setup.
//!
//! A coding agent is not one program. It is a harness, the MCP servers it can
//! reach, the skills and plugins that change how it behaves, and the hooks that
//! fire around it. None of that is visible to pacman, and most of it is invisible
//! to the harness's own `--version` too, so it is the part of the system that
//! drifts furthest without anyone noticing.
//!
//! Everything here is read from the tools themselves (`claude mcp list`,
//! `claude plugin list --json`) or from the files they own, never from a list
//! kept in this module. An agent configured after this was written still shows up.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use regex::Regex;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Map, Value};

use crate::util;

/// `HOME`: `Path.home()`, which is `os.path.expanduser("~")`: $HOME without
/// its trailing slashes, "/" when that leaves nothing (an empty $HOME too).
/// With $HOME unset, the passwd entry of the user.
///
/// Python fixes HOME, CLAUDE and CACHE once, at import; these read the
/// environment on each call. The app never changes either variable, and the
/// parity runner relies on being able to point them elsewhere per case.
pub fn home() -> PathBuf {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    match std::env::var_os("HOME") {
        Some(h) => {
            let mut b = h.into_vec();
            while b.last() == Some(&b'/') {
                b.pop();
            }
            if b.is_empty() {
                PathBuf::from("/")
            } else {
                PathBuf::from(std::ffi::OsStr::from_bytes(&b))
            }
        }
        // No $HOME: the passwd entry's directory (as `expanduser` does),
        // and "~" itself when there is none (where `Path.home()` gives up).
        None => match nix::unistd::User::from_uid(nix::unistd::getuid()) {
            Ok(Some(u)) => {
                let mut b = u.dir.into_os_string().into_vec();
                while b.last() == Some(&b'/') {
                    b.pop();
                }
                if b.is_empty() {
                    PathBuf::from("/")
                } else {
                    PathBuf::from(std::ffi::OsString::from_vec(b))
                }
            }
            _ => PathBuf::from("~"),
        },
    }
}

/// `CLAUDE`: `HOME / ".claude"`.
pub fn claude() -> PathBuf {
    home().join(".claude")
}

/// `CACHE`: `$XDG_CACHE_HOME/almanac` (or `~/.cache/almanac`). Like
/// `os.environ.get`, a set but empty variable is used as it is, giving the
/// relative `almanac`.
pub fn cache() -> PathBuf {
    let base = match std::env::var_os("XDG_CACHE_HOME") {
        Some(v) => PathBuf::from(v),
        None => home().join(".cache"),
    };
    base.join("almanac")
}

/// `MCP_CACHE`: `CACHE / "mcp-list.json"`.
pub fn mcp_cache() -> PathBuf {
    cache().join("mcp-list.json")
}

/// `claude mcp list` opens a connection to every configured server, so its cost
/// scales with how many are set up and how far away they are -- 13 to 22
/// seconds here. Short enough to still catch a server going down, long enough
/// that moving between pages does not re-run it.
pub const MCP_TTL: u64 = 300;

pub const HARNESS: &str = "harness";
pub const MCP: &str = "mcp";
pub const SKILL: &str = "skill";
pub const PLUGIN: &str = "plugin";
pub const MARKET: &str = "marketplace";
pub const HOOK: &str = "hook";

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Entry {
    pub kind: String,
    pub name: String,
    pub detail: String,
    /// connected | needs auth | update | ""
    pub status: String,
    /// ok | warn | info
    pub severity: String,
    pub version: String,
    pub update: String,
    pub path: String,
    pub url: String,
    pub harness: String,
    pub action_label: String,
    /// argv list, or callable(log). Every producer in the Python passes an
    /// argv list or None, so the port carries only the argv form.
    pub action: Option<Vec<String>>,
    pub action_note: String,
    pub items: Vec<String>,
    /// Where to get the vendor's own mark. `icon_key` is the cache key, so two
    /// services from one company share a single download.
    pub icon_key: String,
    pub icon_domain: String,
    pub icon_url: String,
    pub fallback_icon: String,
    /// The JSON value Python holds in a field where it is not the string
    /// (or list of strings) the typed field above carries: Python keeps
    /// whatever the CLI or the cache gave it (`name` 5, `detail` None). The
    /// typed field holds the value as `str()` would print it; serialising
    /// the entry writes this value instead.
    #[serde(skip)]
    pub py_values: PyValues,
    /// The bytes behind a field Python holds as a str with surrogate escapes
    /// (a file name that is not UTF-8, decoded with surrogateescape). The
    /// typed field holds it with U+FFFD; `entries_json` writes it exactly.
    #[serde(skip)]
    pub py_bytes: std::collections::BTreeMap<String, Vec<u8>>,
}

/// [`Entry::py_values`]: values straight from a JSON file, so as deeply
/// nested as Python's json allows. Cloned, compared and dropped without
/// recursion, so an entry can go anywhere (the window's main thread
/// included) whatever its values hold.
#[derive(Debug, Default)]
pub struct PyValues(pub Map<String, Value>);

impl std::ops::Deref for PyValues {
    type Target = Map<String, Value>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for PyValues {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Clone for PyValues {
    fn clone(&self) -> Self {
        PyValues(self.0.iter().map(|(k, v)| (k.clone(), util::tree::clone_value(v))).collect())
    }
}

impl PartialEq for PyValues {
    fn eq(&self, other: &Self) -> bool {
        self.0.len() == other.0.len()
            && self.0.iter().all(|(k, v)| other.0.get(k).is_some_and(|w| util::tree::eq_value(v, w)))
    }
}

impl Drop for PyValues {
    fn drop(&mut self) {
        for (_, v) in std::mem::take(&mut self.0) {
            util::tree::drop_value(v);
        }
    }
}

impl From<Map<String, Value>> for PyValues {
    fn from(m: Map<String, Value>) -> Self {
        PyValues(m)
    }
}

impl Serialize for Entry {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = Map::new();
        let mut put = |k: &str, v: Value| {
            m.insert(k.to_string(), v);
        };
        put("kind", self.kind.clone().into());
        put("name", self.name.clone().into());
        put("detail", self.detail.clone().into());
        put("status", self.status.clone().into());
        put("severity", self.severity.clone().into());
        put("version", self.version.clone().into());
        put("update", self.update.clone().into());
        put("path", self.path.clone().into());
        put("url", self.url.clone().into());
        put("harness", self.harness.clone().into());
        put("action_label", self.action_label.clone().into());
        put("action", self.action.clone().map(Value::from).unwrap_or(Value::Null));
        put("action_note", self.action_note.clone().into());
        put("items", self.items.clone().into());
        put("icon_key", self.icon_key.clone().into());
        put("icon_domain", self.icon_domain.clone().into());
        put("icon_url", self.icon_url.clone().into());
        put("fallback_icon", self.fallback_icon.clone().into());
        for (k, v) in self.py_values.iter() {
            m.insert(k.clone(), util::tree::clone_value(v));
        }
        m.serialize(s)
    }
}

impl Entry {
    /// Set the string field `field` to these OS bytes, as Python's str of
    /// them (surrogateescape) would be: lossy text in the field, the bytes
    /// kept in `py_bytes` when they are not UTF-8.
    fn set_os(&mut self, field: &str, bytes: Vec<u8>) {
        match String::from_utf8(bytes) {
            Ok(text) => {
                if let Some(f) = self.str_field(field) {
                    *f = text;
                }
                self.py_bytes.remove(field);
            }
            Err(e) => {
                let bytes = e.into_bytes();
                if let Some(f) = self.str_field(field) {
                    *f = String::from_utf8_lossy(&bytes).into_owned();
                }
                self.py_bytes.insert(field.to_string(), bytes);
            }
        }
    }

    /// The string field named `name`, for `Entry(**row)`.
    fn str_field(&mut self, name: &str) -> Option<&mut String> {
        Some(match name {
            "kind" => &mut self.kind,
            "name" => &mut self.name,
            "detail" => &mut self.detail,
            "status" => &mut self.status,
            "severity" => &mut self.severity,
            "version" => &mut self.version,
            "update" => &mut self.update,
            "path" => &mut self.path,
            "url" => &mut self.url,
            "harness" => &mut self.harness,
            "action_label" => &mut self.action_label,
            "action_note" => &mut self.action_note,
            "icon_key" => &mut self.icon_key,
            "icon_domain" => &mut self.icon_domain,
            "icon_url" => &mut self.icon_url,
            "fallback_icon" => &mut self.fallback_icon,
            _ => return None,
        })
    }
}

impl Default for Entry {
    fn default() -> Self {
        Entry {
            kind: String::new(),
            name: String::new(),
            detail: String::new(),
            status: String::new(),
            severity: String::new(),
            version: String::new(),
            update: String::new(),
            path: String::new(),
            url: String::new(),
            harness: "Claude Code".into(),
            action_label: String::new(),
            action: None,
            action_note: String::new(),
            items: Vec::new(),
            icon_key: String::new(),
            icon_domain: String::new(),
            icon_url: String::new(),
            fallback_icon: "application-x-executable-symbolic".into(),
            py_values: PyValues::default(),
            py_bytes: Default::default(),
        }
    }
}

/// An org's avatar is the brand mark for most repositories.
pub fn github_avatar(repo: &str) -> String {
    let owner = repo.split('/').next().unwrap_or("");
    if owner.is_empty() { String::new() } else { format!("https://github.com/{owner}.png?size=128") }
}

static HOST: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z]+://([^/:]+)").unwrap());

/// The host of a `scheme://host...` target, or "".
pub fn host(target: &str) -> String {
    HOST.captures(target).map(|c| c[1].to_string()).unwrap_or_default()
}

/// stdout + stderr of a command, or "" when it could not run.
/// Python's default timeout is 60 seconds.
///
/// `text=True` decodes strictly and translates \r\n and \r to \n. Output
/// that is not UTF-8 raises UnicodeDecodeError, a ValueError `_run` does not
/// catch: that is the Err here.
pub fn run(cmd: &[&str], timeout: u64) -> Result<String, String> {
    // OSError (could not start) and TimeoutExpired both answer "".
    let Some((stdout, stderr)) = run_bytes(cmd, timeout) else {
        return Ok(String::new());
    };
    Ok(py_text(stdout)? + &py_text(stderr)?)
}

/// `subprocess.run(cmd, capture_output=True, timeout=t)` before decoding:
/// None when it could not start or timed out. (`util::run` decodes lossily,
/// which is not what `text=True` does.)
///
/// Like `subprocess.run`, stdin is inherited, and the timeout covers reading
/// the pipes to their end, not only the child's exit: a grandchild that keeps
/// stdout open past it is a timeout too.
fn run_bytes(cmd: &[&str], timeout: u64) -> Option<(Vec<u8>, Vec<u8>)> {
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    let (prog, args) = cmd.split_first()?;
    let deadline = Instant::now() + Duration::from_secs(timeout);
    let mut child = Command::new(prog)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    // Drain both pipes on their own threads, or a chatty child fills one and
    // blocks forever while we wait on it. A thread still reading when the
    // time is up is left to finish on its own (when the last writer exits).
    let drain = |p: Option<Box<dyn Read + Send>>| {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut v = Vec::new();
            if let Some(mut p) = p {
                let _ = p.read_to_end(&mut v);
            }
            let _ = tx.send(v);
        });
        rx
    };
    let out_rx = drain(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err_rx = drain(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => return None,
        }
    }
    let left = || deadline.saturating_duration_since(Instant::now());
    let stdout = out_rx.recv_timeout(left()).ok()?;
    let stderr = err_rx.recv_timeout(left()).ok()?;
    Some((stdout, stderr))
}

/// `bytes.decode("utf-8")` with universal newlines; Err is the
/// UnicodeDecodeError message.
fn py_text(raw: Vec<u8>) -> Result<String, String> {
    let text = String::from_utf8(raw).map_err(|e| {
        let bytes = e.as_bytes();
        let err = e.utf8_error();
        let start = err.valid_up_to();
        let b = bytes.get(start).copied().unwrap_or(0);
        let (end, reason) = match err.error_len() {
            None => (bytes.len(), "unexpected end of data"),
            Some(1) if !(0xc2..=0xf4).contains(&b) => (start + 1, "invalid start byte"),
            Some(k) => (start + k, "invalid continuation byte"),
        };
        if end - start == 1 {
            format!("'utf-8' codec can't decode byte 0x{b:02x} in position {start}: {reason}")
        } else {
            format!("'utf-8' codec can't decode bytes in position {start}-{}: {reason}", end - 1)
        }
    })?;
    Ok(if text.contains('\r') { text.replace("\r\n", "\n").replace('\r', "\n") } else { text })
}

/// `shutil.which(name)`: the first `dir/name` on $PATH that exists, is not
/// a directory and that `access(X_OK)` lets this user run. An unset PATH
/// searches `confstr(CS_PATH)` ("/bin:/usr/bin"); an empty one finds
/// nothing, while an empty entry in a longer one is the current directory.
/// (`util::which` takes any execute bit and treats these PATHs otherwise.)
fn which(name: &str) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let ok = |p: &Path| {
        p.exists() && rustix::fs::access(p, rustix::fs::Access::EXISTS | rustix::fs::Access::EXEC_OK).is_ok() && !p.is_dir()
    };
    if let Some((dir, file)) = name.rsplit_once('/') {
        // `os.path.split` keeps "/" for a name directly under the root.
        let dir = if dir.is_empty() { "/" } else { dir };
        let p = Path::new(dir).join(file);
        return ok(&p).then_some(p);
    }
    let path = std::env::var_os("PATH").unwrap_or_else(|| "/bin:/usr/bin".into());
    if path.is_empty() {
        return None;
    }
    let mut seen = std::collections::HashSet::new();
    for dir in path.as_bytes().split(|&c| c == b':') {
        if !seen.insert(dir.to_vec()) {
            continue;
        }
        // `os.path.join("", name)` is the bare name: the current directory.
        let p = Path::new(std::ffi::OsStr::from_bytes(dir)).join(name);
        if ok(&p) {
            return Some(p);
        }
    }
    None
}

// ---- json.loads ------------------------------------------------------------------

/// Marks a float serde_json cannot hold (inf, -inf, nan) in a decoded value.
const PYFLOAT: &str = "\u{0}pyfloat:";

/// Marks a nested value `py_loads` parsed on its own (see `parse_nested`).
const PYSUB: &str = "\u{0}pysub:";

/// How deep Python's json nests before its C decoder runs out of stack and
/// raises RecursionError (measured: 52125 with the default 8 MB stack).
const PY_JSON_DEPTH: usize = 52125;

/// `json.loads`: serde_json, plus what Python's json also accepts -- the
/// NaN, Infinity and -Infinity literals and floats too large for a double
/// (1e400 is inf), which decode to a marked string `special_float` reads,
/// and nesting deeper than serde_json's 128 levels.
///
/// Ok(None) is a ValueError (malformed); Err is the RecursionError Python
/// raises past `PY_JSON_DEPTH`, which is not a ValueError and so escapes
/// every reader's `except`.
fn py_loads(text: &str) -> Result<Option<Value>, String> {
    let (mut depth, mut in_str, mut esc) = (0usize, false, false);
    for c in text.bytes() {
        if in_str {
            match (esc, c) {
                (true, _) => esc = false,
                (false, b'\\') => esc = true,
                (false, b'"') => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'[' | b'{' => {
                depth += 1;
                if depth > PY_JSON_DEPTH {
                    let what = if c == b'[' { "array" } else { "object" };
                    return Err(format!("Stack overflow (used 8148 kB) while decoding a JSON {what} from a unicode string"));
                }
            }
            b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(py_loads_flat(text))
}

/// `py_loads` without the depth check.
fn py_loads_flat(text: &str) -> Option<Value> {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let (mut i, mut last, mut in_str) = (0, 0, false);
    while i < b.len() {
        let c = b[i];
        if in_str {
            match c {
                b'\\' => i += 2,
                b'"' => {
                    in_str = false;
                    i += 1
                }
                _ => i += 1,
            }
            continue;
        }
        let word = |w: &str| text[i..].starts_with(w);
        let (len, special) = if c == b'"' {
            in_str = true;
            i += 1;
            continue;
        } else if word("NaN") {
            (3, "nan")
        } else if word("Infinity") {
            (8, "inf")
        } else if word("-Infinity") {
            (9, "-inf")
        } else if c == b'-' || c.is_ascii_digit() {
            let mut j = i + 1;
            while j < b.len() && matches!(b[j], b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-') {
                j += 1;
            }
            let tok = &text[i..j];
            let big = tok.contains(['.', 'e', 'E']) && tok.parse::<f64>().is_ok_and(f64::is_infinite);
            if !big {
                i = j;
                continue;
            }
            (j - i, if tok.starts_with('-') { "-inf" } else { "inf" })
        } else {
            i += 1;
            continue;
        };
        out.push_str(&text[last..i]);
        out.push_str(&serde_json::to_string(&format!("{PYFLOAT}{special}")).ok()?);
        i += len;
        last = i;
    }
    out.push_str(&text[last..]);
    parse_nested(&out)
}

/// serde_json stops at 128 levels. Every array or object that starts 64
/// levels down is cut out, parsed on its own (recursively, so it may be cut
/// again) and stood in for by a marked string, which is swapped back for
/// the value once the outer text has parsed. Malformed anywhere is None.
fn parse_nested(text: &str) -> Option<Value> {
    const CUT: usize = 64;
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut subs: Vec<Value> = Vec::new();
    let (mut i, mut last, mut depth, mut in_str) = (0, 0, 0usize, false);
    while i < b.len() {
        let c = b[i];
        if in_str {
            match c {
                b'\\' => i += 2,
                b'"' => {
                    in_str = false;
                    i += 1
                }
                _ => i += 1,
            }
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'[' | b'{' if depth == CUT => {
                // Find the bracket that closes this one.
                let (mut j, mut d, mut s) = (i, 0usize, false);
                let end = loop {
                    let Some(&x) = b.get(j) else {
                        return None;
                    };
                    if s {
                        match x {
                            b'\\' => j += 1,
                            b'"' => s = false,
                            _ => {}
                        }
                    } else {
                        match x {
                            b'"' => s = true,
                            b'[' | b'{' => d += 1,
                            b']' | b'}' => {
                                d -= 1;
                                if d == 0 {
                                    break j + 1;
                                }
                            }
                            _ => {}
                        }
                    }
                    j += 1;
                };
                let inner = text.get(i..end)?;
                subs.push(parse_nested(inner)?);
                out.push_str(&text[last..i]);
                out.push_str(&serde_json::to_string(&format!("{PYSUB}{}", subs.len() - 1)).ok()?);
                i = end;
                last = end;
                continue;
            }
            b'[' | b'{' => depth += 1,
            b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
        i += 1;
    }
    if subs.is_empty() {
        return serde_json::from_str(text).ok();
    }
    out.push_str(&text[last..]);
    let mut v: Value = serde_json::from_str(&out).ok()?;
    let mut subs: Vec<Option<Value>> = subs.into_iter().map(Some).collect();
    fn put_back(v: &mut Value, subs: &mut [Option<Value>]) {
        match v {
            Value::String(s) => {
                if let Some(k) = s.strip_prefix(PYSUB).and_then(|n| n.parse::<usize>().ok()) {
                    if let Some(sub) = subs.get_mut(k).and_then(Option::take) {
                        *v = sub;
                    }
                }
            }
            Value::Array(a) => a.iter_mut().for_each(|x| put_back(x, subs)),
            Value::Object(o) => o.values_mut().for_each(|x| put_back(x, subs)),
            _ => {}
        }
    }
    put_back(&mut v, &mut subs);
    Some(v)
}

/// The float a `py_loads` marker stands for.
fn special_float(v: &Value) -> Option<f64> {
    match v.as_str()?.strip_prefix(PYFLOAT)? {
        "inf" => Some(f64::INFINITY),
        "-inf" => Some(f64::NEG_INFINITY),
        _ => Some(f64::NAN),
    }
}

/// The text of a JSON string (not of a `py_loads` float marker).
fn py_s(v: &Value) -> Option<&str> {
    v.as_str().filter(|_| special_float(v).is_none())
}

// ---- Python string semantics the readers below depend on ----------------------

/// `str.isspace()`: Unicode White_Space plus the four ASCII separators
/// (\x1c-\x1f) Python also counts. `re`'s `\s` and `str.strip()` use this set.
fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// `str.strip()`.
fn py_strip(s: &str) -> &str {
    s.trim_matches(is_py_space)
}

/// `str.splitlines()`.
fn py_splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut iter = s.char_indices().peekable();
    while let Some((pos, c)) = iter.next() {
        if matches!(c, '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}') {
            out.push(&s[start..pos]);
            let mut next = pos + c.len_utf8();
            if c == '\r' {
                if let Some(&(_, '\n')) = iter.peek() {
                    iter.next();
                    next += 1;
                }
            }
            start = next;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// `s[:n]` on a str: the first n characters.
fn py_prefix(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// `str(Path(s))`: repeated and trailing slashes and `.` parts dropped, ""
/// becoming ".", and POSIX's special leading `//` kept.
fn py_path_bytes(s: &[u8]) -> Vec<u8> {
    let root: &[u8] = if s.starts_with(b"//") && !s.starts_with(b"///") {
        b"//"
    } else if s.starts_with(b"/") {
        b"/"
    } else {
        b""
    };
    let parts: Vec<&[u8]> = s.split(|&c| c == b'/').filter(|p| !p.is_empty() && *p != b".").collect();
    let mut joined = root.to_vec();
    joined.extend(parts.join(&b'/'));
    if joined.is_empty() { b".".to_vec() } else { joined }
}

/// `str(path)` as text (U+FFFD for what is not UTF-8).
fn path_str(p: &Path) -> String {
    String::from_utf8_lossy(&path_bytes(p)).into_owned()
}

/// `str(path)` as the bytes it stands for.
fn path_bytes(p: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    py_path_bytes(p.as_os_str().as_bytes())
}

/// The final component's bytes (`Path.name`).
fn name_bytes(p: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    p.file_name().map(|n| n.as_bytes().to_vec()).unwrap_or_default()
}

/// A JSON string the way `json.dumps` writes a str decoded from these bytes
/// with surrogateescape: each byte that is not part of valid UTF-8 is the
/// lone surrogate U+DC80+byte, written `\udcXX`.
fn py_json_bytes(b: &[u8]) -> String {
    let mut out = String::from("\"");
    let mut rest = b;
    let text = |s: &str, out: &mut String| {
        let q = util::py_json_string(s);
        out.push_str(&q[1..q.len() - 1]);
    };
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(s) => {
                text(s, &mut out);
                break;
            }
            Err(e) => {
                let good = e.valid_up_to();
                text(std::str::from_utf8(&rest[..good]).unwrap_or(""), &mut out);
                let bad = e.error_len().unwrap_or(rest.len() - good);
                for x in &rest[good..good + bad] {
                    out.push_str(&format!("\\udc{x:02x}"));
                }
                rest = &rest[good + bad..];
            }
        }
    }
    out.push('"');
    out
}

/// `json.dumps([asdict(e) for e in entries])`, exact where serde cannot be:
/// fields held in `py_bytes` come out with their surrogate escapes.
pub fn entries_json(entries: &[Entry]) -> String {
    let mut subs = Vec::new();
    let vals: Vec<Value> = entries
        .iter()
        .map(|e| {
            let mut v = serde_json::to_value(e).unwrap_or(Value::Null);
            for (k, b) in &e.py_bytes {
                if let Some(slot) = v.get_mut(k.as_str()) {
                    *slot = Value::String(format!("\u{0}pybytes:{}", subs.len()));
                    subs.push(py_json_bytes(b));
                }
            }
            v
        })
        .collect();
    let mut text = util::dumps(&Value::Array(vals), None, false);
    for (i, sub) in subs.iter().enumerate() {
        text = text.replacen(&util::py_json_string(&format!("\u{0}pybytes:{i}")), sub, 1);
    }
    text
}

/// Python's `type(v).__name__` for a JSON value.
fn py_type(v: &Value) -> &'static str {
    if special_float(v).is_some() {
        return "float";
    }
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

/// The AttributeError Python raises calling `.get`/`.items` on a non-dict.
fn attr_err(v: &Value, attr: &str) -> String {
    format!("'{}' object has no attribute '{attr}'", py_type(v))
}

/// Python truthiness of a JSON value.
fn truthy(v: &Value) -> bool {
    if special_float(v).is_some() {
        return true; // inf and nan are both true
    }
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `repr(v)` for a JSON value, as Python would print the decoded object.
fn py_repr(v: &Value) -> String {
    if special_float(v).is_some() {
        return py_str(v);
    }
    match v {
        Value::String(s) => {
            let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
            let mut out = String::from(q);
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
                    c if !py_printable(c) => {
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
                }
            }
            out.push(q);
            out
        }
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter().map(|(k, x)| format!("{}: {}", py_repr(&Value::String(k.clone())), py_repr(x))).collect::<Vec<_>>().join(", ")
        ),
        other => py_str(other),
    }
}

/// `str(v)` (what an f-string prints) for a JSON value.
fn py_str(v: &Value) -> String {
    if let Some(f) = special_float(v) {
        return if f.is_nan() { "nan" } else if f > 0.0 { "inf" } else { "-inf" }.into();
    }
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => match n.as_f64().filter(|_| n.is_f64()) {
            Some(f) => util::py_float_repr(f),
            None => n.to_string(),
        },
        Value::String(s) => s.clone(),
        other => py_repr(other),
    }
}

/// `d.get(key) or ""` for a dict decoded from JSON.
fn get_or_empty(d: &serde_json::Map<String, Value>, key: &str) -> String {
    d.get(key).filter(|v| truthy(v)).map(py_str).unwrap_or_default()
}

/// `str.isprintable()` for one character: false in the Unicode categories
/// Cc, Cf, Cs, Co, Cn, Zl, Zp and Zs other than the space. `repr()` escapes
/// these.
fn py_printable(c: char) -> bool {
    let n = c as u32;
    NONPRINTABLE.binary_search_by(|&(lo, hi)| if hi < n { std::cmp::Ordering::Less } else if lo > n { std::cmp::Ordering::Greater } else { std::cmp::Ordering::Equal }).is_err()
}

/// The `[Errno n] strerror: 'path'` text of a Python OSError.
fn os_err(e: &std::io::Error, path: &Path) -> String {
    let text = e.to_string();
    let strerror = text.split(" (os error").next().unwrap_or(&text).to_string();
    match e.raw_os_error() {
        Some(n) => format!("[Errno {n}] {strerror}: '{}'", path_str(path)),
        None => strerror,
    }
}

static FM_KEY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^([A-Za-z_-]+):[\s\x1c-\x1f]*(.*)$").unwrap());

/// name/description from a SKILL.md header, without a YAML dependency.
///
/// Only the two scalar keys are needed, and they may be folded onto several
/// lines with `>`; a real parser would be more than this costs.
///
/// A dict in Python, so the keys keep the order they were first seen in.
pub fn frontmatter(path: &Path) -> Map<String, Value> {
    let mut out = Map::new();
    let Ok(bytes) = std::fs::read(path) else {
        return out;
    };
    // read_text(errors="replace"), with its universal-newline translation.
    let text = String::from_utf8_lossy(&bytes).replace("\r\n", "\n").replace('\r', "\n");
    if !text.starts_with("---") {
        return out;
    }
    let end = text[3..].find("\n---").map(|i| i + 3);
    let block: String = match end {
        Some(e) if e > 0 => text[3..e].to_string(),
        _ => text.chars().skip(3).take(4000 - 3).collect(),
    };

    let mut key: Option<String> = None;
    let mut buf: Vec<String> = Vec::new();
    for line in py_splitlines(&block) {
        if let Some(m) = FM_KEY.captures(line) {
            if let Some(k) = key.take() {
                out.insert(k, py_strip(&buf.join(" ")).into());
            }
            key = Some(m[1].to_lowercase());
            let val = py_strip(&m[2]).to_string();
            buf = if matches!(val.as_str(), ">" | "|" | ">-" | "|-") { Vec::new() } else { vec![val] };
        } else if key.is_some() && !py_strip(line).is_empty() {
            buf.push(py_strip(line).to_string());
        }
    }
    if let Some(k) = key {
        out.insert(k, py_strip(&buf.join(" ")).into());
    }
    out
}

// ---- harnesses ---------------------------------------------------------------

/// `os.path.expanduser("~")`: $HOME without its trailing slash ("/" if that
/// leaves nothing).
fn expand_home() -> String {
    home().to_string_lossy().into_owned()
}

/// A path under the home directory, written the way people say it.
pub fn tilde(path: &str) -> String {
    let home = expand_home();
    if !path.is_empty() && path.starts_with(&format!("{home}/")) {
        format!("~{}", &path[home.len()..])
    } else {
        path.to_string()
    }
}

static EXC_PREFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_.]*(Error|Exception|Exit|Interrupt|Warning): ").unwrap());

/// `str(exc)` from the "ExcType: message" form other modules report.
fn py_exc_message(e: &str) -> String {
    EXC_PREFIX.replace(e, "").into_owned()
}

pub fn harnesses() -> Result<Vec<Entry>, String> {
    let mut out = Vec::new();
    // agents reports an exception as "TypeError: msg"; what escapes here is
    // the exception itself, whose str() is the message.
    let found = crate::agents::try_detect(true).map_err(|e| py_exc_message(&e))?;
    for a in found {
        let ex = &a.extra;
        let (behind, kind) = match ex.get("behind") {
            None => ("0".to_string(), String::new()),
            Some(Value::Array(v)) if v.len() == 2 => (py_str(&v[0]), py_str(&v[1])),
            Some(Value::Array(v)) => {
                return Err(if v.len() < 2 {
                    format!("not enough values to unpack (expected 2, got {})", v.len())
                } else {
                    "too many values to unpack (expected 2)".to_string()
                })
            }
            Some(other) => return Err(format!("cannot unpack non-iterable {} object", py_type(other))),
        };
        let has_update = !a.update.is_empty();
        let update_cmd = ex.get("update_cmd").cloned().unwrap_or(Value::Null);
        let cmd_list: Vec<String> = match &update_cmd {
            Value::Array(v) => v.iter().map(py_str).collect(),
            _ => Vec::new(),
        };
        let domain = ex.get("domain").filter(|v| truthy(v));
        out.push(Entry {
            kind: HARNESS.into(),
            name: a.name.clone(),
            detail: format!(
                "{}-managed · {}",
                ex.get("manager").map(py_str).unwrap_or_else(|| "None".into()),
                tilde(&a.exec_path)
            ),
            version: a.version.clone(),
            update: a.update.clone(),
            status: if has_update { format!("{behind} {kind} behind") } else { "current".into() },
            severity: if has_update { "warn" } else { "ok" }.into(),
            path: a.exec_path.clone(),
            url: ex.get("url").map(|v| if v.is_null() { String::new() } else { py_str(v) }).unwrap_or_default(),
            harness: a.name.clone(),
            action_label: if has_update && truthy(&update_cmd) { "Update".into() } else { String::new() },
            action: if has_update && !update_cmd.is_null() { Some(cmd_list.clone()) } else { None },
            action_note: format!("Runs {}.", if truthy(&update_cmd) { cmd_list.join(" ") } else { String::new() }),
            icon_key: domain.map(py_str).unwrap_or_else(|| {
                ex.get("key").map(|v| if v.is_null() { String::new() } else { py_str(v) }).unwrap_or_default()
            }),
            icon_domain: ex.get("domain").map(|v| if v.is_null() { String::new() } else { py_str(v) }).unwrap_or_default(),
            icon_url: if domain.is_some() {
                String::new()
            } else {
                github_avatar(&ex.get("repo").filter(|v| truthy(v)).map(py_str).unwrap_or_default())
            },
            fallback_icon: "utilities-terminal-symbolic".into(),
            ..Default::default()
        });
        // A release url that is not a string (a hand-edited cache: `5`) is
        // kept as is by the Python, not turned into its str.
        if let (Some(e), Some(v)) = (out.last_mut(), ex.get("url")) {
            if !v.is_string() && !v.is_null() {
                e.py_values.insert("url".into(), v.clone());
            }
        }
    }
    Ok(out)
}

// ---- MCP servers -------------------------------------------------------------

static MCP_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?P<name>.+?):[\s\x1c-\x1f]+(?P<target>[^\s\x1c-\x1f].*?)[\s\x1c-\x1f]+-[\s\x1c-\x1f]+(?P<state>.+)$").unwrap()
});

const ENTRY_FIELDS: &[&str] = &[
    "kind", "name", "detail", "status", "severity", "version", "update", "path", "url", "harness",
    "action_label", "action", "action_note", "items", "icon_key", "icon_domain", "icon_url", "fallback_icon",
];

/// `Entry(**row)`: None where Python raises TypeError (not a mapping, an
/// unknown field, kind or name missing). Python takes a value of any type in
/// any field; one the typed field cannot hold is kept in `py_values`, and the
/// field gets it as `str()` prints it (None as "").
fn entry_from_row(row: &Value) -> Option<Entry> {
    let obj = row.as_object()?;
    if !obj.contains_key("kind") || !obj.contains_key("name") || obj.keys().any(|k| !ENTRY_FIELDS.contains(&k.as_str())) {
        return None;
    }
    let text = |v: &Value| match py_s(v) {
        Some(s) => s.to_string(),
        None if v.is_null() => String::new(),
        None => py_str(v),
    };
    let mut e = Entry::default();
    for (k, v) in obj {
        let typed = match (k.as_str(), v) {
            ("action", Value::Null) => {
                e.action = None;
                true
            }
            ("action", Value::Array(a)) => {
                e.action = Some(a.iter().map(text).collect());
                a.iter().all(|x| py_s(x).is_some())
            }
            ("action", other) => {
                e.action = py_s(other).map(|s| vec![s.to_string()]);
                false
            }
            ("items", Value::Array(a)) => {
                e.items = a.iter().map(text).collect();
                a.iter().all(|x| py_s(x).is_some())
            }
            ("items", _) => {
                e.items = Vec::new();
                false
            }
            (field, v) => {
                let s = text(v);
                if let Some(f) = e.str_field(field) {
                    *f = s;
                }
                py_s(v).is_some()
            }
        };
        if !typed {
            e.py_values.insert(k.clone(), v.clone());
        }
    }
    Some(e)
}

/// The fresh rows of the `claude mcp list` cache. Ok(None) is a miss (Python's
/// caught OSError/ValueError/TypeError); Err is what Python lets escape.
fn mcp_cached() -> Result<Option<Vec<Entry>>, String> {
    let Ok(text) = std::fs::read_to_string(mcp_cache()) else {
        return Ok(None);
    };
    let Some(blob) = py_loads(&text)? else {
        return Ok(None);
    };
    let Some(obj) = blob.as_object() else {
        return Err(attr_err(&blob, "get"));
    };
    let at = match obj.get("at") {
        None => 0.0,
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::Bool(b)) => *b as u8 as f64,
        // Infinity (or 1e400) is fresh forever, NaN never.
        Some(v) if special_float(v).is_some() => special_float(v).unwrap_or(0.0),
        Some(_) => return Ok(None),
    };
    if !(util::now() - at < MCP_TTL as f64) {
        return Ok(None);
    }
    Ok(match obj.get("rows") {
        None => Some(Vec::new()),
        Some(Value::Array(rows)) => rows.iter().map(entry_from_row).collect(),
        Some(Value::String(s)) if s.is_empty() => Some(Vec::new()),
        Some(Value::Object(o)) if o.is_empty() => Some(Vec::new()),
        Some(_) => None,
    })
}

/// What `claude mcp list` reports, including its health check.
///
/// Parsed from the CLI rather than from ~/.claude.json because the connectors
/// enabled through claude.ai never appear in that file -- reading the file
/// would report zero servers on a machine with twenty.
pub fn mcp_servers() -> Result<Vec<Entry>, String> {
    if which("claude").is_none() {
        return Ok(Vec::new());
    }
    if let Some(rows) = mcp_cached()? {
        return Ok(rows);
    }

    let mut out = Vec::new();
    for line in py_splitlines(&run(&["claude", "mcp", "list"], 120)?) {
        let line = py_strip(line);
        if line.is_empty() || line.starts_with("Checking") {
            continue;
        }
        let Some(m) = MCP_LINE.captures(line) else {
            continue;
        };
        let state = py_strip(&m["state"]);
        let target = py_strip(&m["target"]);
        let connected = state.contains("Connect") && !state.contains('✘') && !state.contains('!');
        let needs_auth = state.to_lowercase().contains("auth");
        out.push(Entry {
            kind: MCP.into(),
            name: py_strip(&m["name"]).to_string(),
            detail: target.to_string(),
            status: if connected {
                "connected".into()
            } else if needs_auth {
                "needs authentication".into()
            } else {
                state.trim_matches(|c| matches!(c, '✘' | '!' | ' ')).to_string()
            },
            severity: if connected { "ok" } else if needs_auth { "info" } else { "warn" }.into(),
            url: if m["target"].starts_with("http") { target.to_string() } else { String::new() },
            icon_key: host(target),
            icon_domain: host(target),
            fallback_icon: "network-server-symbolic".into(),
            action_label: if needs_auth { "Authenticate".into() } else { String::new() },
            action: None,
            action_note: "Authentication happens inside the agent session: \
                          run the server's authenticate tool there."
                .into(),
            ..Default::default()
        });
    }
    if std::fs::create_dir_all(cache()).is_ok() {
        let rows: Vec<Value> = out.iter().filter_map(|e| serde_json::to_value(e).ok()).collect();
        let blob = serde_json::json!({"at": util::now(), "rows": rows});
        let _ = std::fs::write(mcp_cache(), util::dumps(&blob, None, false));
    }
    Ok(out)
}

// ---- skills ------------------------------------------------------------------

/// Every directory under `dir`, itself included, without following symlinks
/// (how `rglob`'s `**` walks).
fn walk_dirs(dir: &Path, out: &mut Vec<PathBuf>) {
    out.push(dir.to_path_buf());
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            walk_dirs(&e.path(), out);
        }
    }
}

/// `root.rglob("skills/*/SKILL.md")`.
fn plugin_skill_files(root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if root.is_dir() {
        walk_dirs(root, &mut dirs);
    }
    let mut out = Vec::new();
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(d.join("skills")) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                let f = p.join("SKILL.md");
                if std::fs::symlink_metadata(&f).is_ok() {
                    out.push(f);
                }
            }
        }
    }
    out
}

pub fn skills() -> Result<Vec<Entry>, String> {
    let mut out = Vec::new();
    let root = claude().join("skills");
    if root.exists() {
        let rd = std::fs::read_dir(&root).map_err(|e| os_err(&e, &root))?;
        let mut dirs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        dirs.sort();
        for d in dirs {
            let f = d.join("SKILL.md");
            if !f.is_file() {
                continue;
            }
            let fm = frontmatter(&f);
            let mut e = Entry {
                kind: SKILL.into(),
                detail: py_prefix(fm.get("description").and_then(Value::as_str).unwrap_or(""), 300),
                status: "user skill".into(),
                severity: "ok".into(),
                fallback_icon: "text-x-generic-symbolic".into(),
                ..Default::default()
            };
            match fm.get("name").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                Some(n) => e.name = n.to_string(),
                None => e.set_os("name", name_bytes(&d)),
            }
            e.set_os("path", path_bytes(&d));
            out.push(e);
        }
    }

    // Skills that arrive with a plugin are listed separately: they are not
    // yours to edit, and they come and go with the plugin.
    let mut files = plugin_skill_files(&claude().join("plugins"));
    files.sort();
    for f in files {
        let fm = frontmatter(&f);
        let parent = f.parent().map(Path::to_path_buf).unwrap_or_default();
        let plugin = f.ancestors().nth(3).map(name_bytes).unwrap_or_default();
        let mut e = Entry {
            kind: SKILL.into(),
            detail: py_prefix(fm.get("description").and_then(Value::as_str).unwrap_or(""), 300),
            severity: "ok".into(),
            fallback_icon: "text-x-generic-symbolic".into(),
            ..Default::default()
        };
        match fm.get("name").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            Some(n) => e.name = n.to_string(),
            None => e.set_os("name", name_bytes(&parent)),
        }
        e.set_os("status", [b"from ".as_slice(), &plugin].concat());
        e.set_os("path", path_bytes(&parent));
        out.push(e);
    }
    Ok(out)
}

// ---- plugins and marketplaces ------------------------------------------------

pub fn plugins() -> Result<Vec<Entry>, String> {
    if which("claude").is_none() {
        return Ok(Vec::new());
    }
    // A UnicodeDecodeError from `_run` is a ValueError too: caught here.
    let Ok(text) = run(&["claude", "plugin", "list", "--json"], 60) else {
        return Ok(Vec::new());
    };
    let Some(rows) = py_loads(if text.is_empty() { "[]" } else { &text })? else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for r in rows.as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let Some(r) = r.as_object() else {
            return Err(attr_err(r, "get"));
        };
        let name_v = ["name", "id"]
            .iter()
            .find_map(|k| r.get(*k).filter(|v| truthy(v)).cloned())
            .unwrap_or_else(|| "?".into());
        let name = py_str(&name_v);
        let enabled = r.get("enabled").map(truthy).unwrap_or(true);
        let verb = if enabled { "disable" } else { "enable" };
        // Python keeps a field's JSON type (a name of 5 stays 5, in the argv
        // too).
        let mut py_values = Map::new();
        if py_s(&name_v).is_none() {
            py_values.insert("name".into(), name_v.clone());
            py_values.insert("action".into(), serde_json::json!(["claude", "plugin", verb, name_v]));
        }
        for (field, key) in [("detail", "description"), ("version", "version")] {
            if let Some(v) = r.get(key).filter(|v| truthy(v) && py_s(v).is_none()) {
                py_values.insert(field.into(), v.clone());
            }
        }
        out.push(Entry {
            kind: PLUGIN.into(),
            name: name.clone(),
            detail: get_or_empty(r, "description"),
            version: get_or_empty(r, "version"),
            status: if enabled { "enabled" } else { "disabled" }.into(),
            severity: if enabled { "ok" } else { "info" }.into(),
            action_label: if enabled { "Disable" } else { "Enable" }.into(),
            fallback_icon: "application-x-addon-symbolic".into(),
            action: Some(vec!["claude".into(), "plugin".into(), verb.into(), name]),
            py_values: py_values.into(),
            ..Default::default()
        });
    }
    Ok(out)
}

/// The value of a Unicode decimal digit (category Nd), which `int()` reads
/// like its ASCII counterpart.
fn decimal_value(c: char) -> Option<i64> {
    let n = c as u32;
    let i = DECIMAL_ZEROS.partition_point(|&z| z <= n);
    let zero = *DECIMAL_ZEROS.get(i.checked_sub(1)?)?;
    (n - zero < 10).then_some((n - zero) as i64)
}

/// `int(x)` for the forms a date part can take (any Unicode decimal digits,
/// underscores between them, surrounding whitespace); None where Python
/// raises ValueError.
fn py_int(s: &str) -> Option<i64> {
    let t = py_strip(s);
    let (neg, digits) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return None;
    }
    let mut n: i64 = 0;
    for c in digits.chars().filter(|&c| c != '_') {
        n = n.checked_mul(10)?.checked_add(decimal_value(c)?)?;
    }
    Some(if neg { -n } else { n })
}

/// `(date.today() - date(y, m, d)).days > 14` for "Y-M-D"; false where
/// Python's parse or `date()` raises ValueError.
fn is_stale(updated: &str) -> bool {
    let parts: Vec<Option<i64>> = updated.split('-').map(py_int).collect();
    let [Some(y), Some(m), Some(d)] = parts.as_slice() else {
        return false;
    };
    if !(1..=9999).contains(y) || !(1..=12).contains(m) || !(1..=31).contains(d) {
        return false;
    }
    let Some(when) = chrono::NaiveDate::from_ymd_opt(*y as i32, *m as u32, *d as u32) else {
        return false;
    };
    (chrono::Local::now().date_naive() - when).num_days() > 14
}

pub fn marketplaces() -> Result<Vec<Entry>, String> {
    let path = claude().join("plugins/known_marketplaces.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(Vec::new());
    };
    let Some(data) = py_loads(&text)? else {
        return Ok(Vec::new());
    };
    let empty = serde_json::Map::new();
    let data = if truthy(&data) {
        data.as_object().ok_or_else(|| attr_err(&data, "items"))?
    } else {
        &empty
    };
    let mut out = Vec::new();
    for (name, info) in data {
        let Some(info) = info.as_object() else {
            return Err(attr_err(info, "get"));
        };
        let loc_s = match info.get("installLocation").filter(|v| truthy(v)) {
            None => "",
            Some(v) => py_s(v).ok_or_else(|| {
                format!(
                    "argument should be a str or an os.PathLike object where __fspath__ returns a str, not '{}'",
                    py_type(v)
                )
            })?,
        };
        let loc = PathBuf::from(if loc_s.is_empty() { "." } else { loc_s });
        let count = if loc.exists() {
            std::fs::read_dir(loc.join("plugins"))
                .map(|rd| rd.flatten().filter(|e| e.path().is_dir()).count())
                .unwrap_or(0)
        } else {
            0
        };
        let src = match info.get("source") {
            Some(v) if truthy(v) => v.as_object().ok_or_else(|| attr_err(v, "get"))?,
            _ => &empty,
        };
        let repo_v = ["repo", "source"].iter().find_map(|k| src.get(*k).filter(|v| truthy(v)));
        // `(info.get("lastUpdated") or "")[:10]`, then `.split("-")`.
        let updated = match info.get("lastUpdated").filter(|v| truthy(v)) {
            None => String::new(),
            Some(v) => match py_s(v) {
                Some(s) => py_prefix(s, 10),
                None => match v {
                    Value::Array(_) => return Err(attr_err(v, "split")),
                    Value::Object(_) => return Err("slice(None, 10, None)".into()),
                    other => return Err(format!("'{}' object is not subscriptable", py_type(other))),
                },
            },
        };
        // A marketplace that has not been synced in weeks is listing plugin
        // versions that no longer exist, which is the quiet failure mode here.
        let stale = !updated.is_empty() && is_stale(&updated);
        // `repo.split("/")` for the icon raises on anything but a str.
        let repo = match repo_v {
            None => String::new(),
            Some(v) => py_s(v).ok_or_else(|| attr_err(v, "split"))?.to_string(),
        };
        out.push(Entry {
            kind: MARKET.into(),
            name: name.clone(),
            detail: format!("{count} plugins available · {repo}"),
            status: if updated.is_empty() {
                "never synced".into()
            } else if stale {
                format!("synced {updated} — stale")
            } else {
                format!("synced {updated}")
            },
            severity: if stale || updated.is_empty() { "warn" } else { "info" }.into(),
            path: path_str(&loc),
            url: if repo.is_empty() { String::new() } else { format!("https://github.com/{repo}") },
            icon_key: if repo.is_empty() {
                String::new()
            } else {
                format!("gh-{}", repo.split('/').next().unwrap_or(""))
            },
            icon_url: github_avatar(&repo),
            fallback_icon: "system-software-install-symbolic".into(),
            action_label: "Sync".into(),
            action: Some(vec!["claude".into(), "plugin".into(), "marketplace".into(), "update".into(), name.clone()]),
            action_note: "Re-fetches the marketplace catalogue from its source.".into(),
            ..Default::default()
        });
    }
    Ok(out)
}

// ---- hooks and other extensions ----------------------------------------------

static HOOK_PATH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(/[^\s\x1c-\x1f'"]+)"#).unwrap());
static STATUS_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(\$HOME|~)?(/[^\s\x1c-\x1f'"]+)"#).unwrap());

/// `for x in v` over a JSON value: a list's items, a dict's keys, a str's
/// characters; Err where Python raises "not iterable".
fn py_iter(v: &Value) -> Result<Vec<Value>, String> {
    if special_float(v).is_some() {
        return Err("'float' object is not iterable".into());
    }
    match v {
        Value::Array(a) => Ok(a.clone()),
        Value::Object(o) => Ok(o.keys().map(|k| Value::String(k.clone())).collect()),
        Value::String(s) => Ok(s.chars().map(|c| Value::String(c.to_string())).collect()),
        other => Err(format!("'{}' object is not iterable", py_type(other))),
    }
}

pub fn hooks() -> Result<Vec<Entry>, String> {
    let Ok(text) = std::fs::read_to_string(claude().join("settings.json")) else {
        return Ok(Vec::new());
    };
    let Some(settings) = py_loads(&text)? else {
        return Ok(Vec::new());
    };
    let settings = settings.as_object().ok_or_else(|| attr_err(&settings, "get"))?;
    let empty = serde_json::Map::new();
    let hooks = match settings.get("hooks") {
        Some(v) if truthy(v) => v.as_object().ok_or_else(|| attr_err(v, "items"))?,
        _ => &empty,
    };
    let mut out = Vec::new();
    for (event, groups) in hooks {
        let Some(groups) = groups.as_array() else {
            continue;
        };
        for group in groups {
            let group = group.as_object().ok_or_else(|| attr_err(group, "get"))?;
            let hs = match group.get("hooks") {
                Some(v) if truthy(v) => py_iter(v)?,
                _ => Vec::new(),
            };
            for h in &hs {
                let h = h.as_object().ok_or_else(|| attr_err(h, "get"))?;
                let cmd = match h.get("command") {
                    None => String::new(),
                    Some(v) if py_s(v).is_some() => py_s(v).unwrap_or_default().to_string(),
                    Some(other) => {
                        return Err(format!("expected string or bytes-like object, got '{}'", py_type(other)))
                    }
                };
                // A hook whose script has been deleted fails silently on every
                // session start, which is exactly the kind of rot this exists
                // to surface.
                let m = HOOK_PATH.captures(&cmd).map(|c| c[1].to_string());
                let missing = m.as_ref().is_some_and(|p| !Path::new(p).exists());
                out.push(Entry {
                    kind: HOOK.into(),
                    name: event.clone(),
                    detail: cmd.clone(),
                    status: if missing {
                        "script missing".into()
                    } else {
                        format!("matches {}", group.get("matcher").map(py_str).unwrap_or_else(|| "*".into()))
                    },
                    severity: if missing { "warn" } else { "ok" }.into(),
                    fallback_icon: "system-run-symbolic".into(),
                    path: m.unwrap_or_default(),
                    ..Default::default()
                });
            }
        }
    }
    let sl = match settings.get("statusLine") {
        Some(v) if truthy(v) => v.as_object().ok_or_else(|| attr_err(v, "get"))?.get("command").cloned(),
        _ => None,
    };
    if let Some(sl) = sl.filter(truthy) {
        let Some(sl) = py_s(&sl).map(str::to_string) else {
            return Err(format!("expected string or bytes-like object, got '{}'", py_type(&sl)));
        };
        let target = STATUS_PATH.captures(&sl).map(|c| c[2].to_string()).unwrap_or_default();
        let full = if target.is_empty() { String::new() } else { path_str(&home()) + &target };
        let missing = !full.is_empty() && !Path::new(&full).exists();
        out.push(Entry {
            kind: HOOK.into(),
            name: "statusLine".into(),
            detail: sl.clone(),
            status: if missing { "script missing" } else { "configured" }.into(),
            severity: if missing { "warn" } else { "ok" }.into(),
            fallback_icon: "system-run-symbolic".into(),
            path: full,
            ..Default::default()
        });
    }
    Ok(out)
}

/// A group's loader: one of the readers above. `Err` carries the text of the
/// exception the Python reader would raise; `load_group` turns it into an
/// "error" entry.
pub type Loader = fn() -> Result<Vec<Entry>, String>;

/// One group as the AI page shows it: (title, description, entries).
pub type Group = (String, String, Vec<Entry>);

/// (title, kind, loader, description) in display order.
pub const GROUPS: &[(&str, &str, Loader, &str)] = &[
    ("Harnesses", HARNESS, harnesses,
     "The agent CLIs themselves."),
    ("MCP servers", MCP, mcp_servers,
     "Tools and data the agents can reach."),
    ("Skills", SKILL, skills,
     "Instructions loaded on demand to change how an agent works."),
    ("Plugins", PLUGIN, plugins,
     "Bundles of skills, commands, agents and hooks."),
    ("Marketplaces", MARKET, marketplaces,
     "Where plugins are installed from."),
    ("Hooks", HOOK, hooks,
     "Commands that run around an agent session."),
];

/// (title, description, loader) for each group, in display order.
pub fn group_specs() -> Vec<(&'static str, &'static str, Loader)> {
    GROUPS.iter().map(|&(title, _kind, f, desc)| (title, desc, f)).collect()
}

fn error_entry(title: &str, msg: &str) -> Entry {
    Entry {
        kind: "error".into(),
        name: title.to_string(),
        detail: format!("Could not read: {msg}"),
        severity: "warn".into(),
        ..Default::default()
    }
}

/// The message of a caught panic (Python's `str(exc)`).
fn panic_text(p: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic".to_string()
    }
}

fn settle(title: &str, r: std::thread::Result<Result<Vec<Entry>, String>>) -> Vec<Entry> {
    match r {
        Ok(Ok(entries)) => entries,
        Ok(Err(msg)) => vec![error_entry(title, &msg)],
        Err(p) => vec![error_entry(title, &panic_text(p))],
    }
}

/// Run one loader; a failure becomes a single "error" entry naming the group
/// instead of taking the page down.
pub fn load_group(f: Loader, title: &str) -> Vec<Entry> {
    settle(title, on_big_stack(f))
}

/// Stack for a loader's thread. Python's json decodes ~52000 levels deep
/// (`PY_JSON_DEPTH`) on its 8 MB stack; walking and dropping a value that
/// deep takes more here than a default 2 MB thread has. Only touched pages
/// are ever mapped, so the size costs nothing until a file is that deep.
const LOADER_STACK: usize = 256 << 20;

/// Run a loader on a thread of its own with `LOADER_STACK`; a panic is the
/// Err, as `catch_unwind` gives it. If no such thread can be made, it runs
/// here.
fn on_big_stack(f: Loader) -> std::thread::Result<Result<Vec<Entry>, String>> {
    std::thread::scope(|s| match std::thread::Builder::new().stack_size(LOADER_STACK).spawn_scoped(s, f) {
        Ok(h) => h.join(),
        Err(_) => std::panic::catch_unwind(f),
    })
}

/// (title, description, entries) for each group. Read-only.
///
/// `claude mcp list` health-checks 28 servers and takes most of the time
/// here; the file scans should not be queued behind it.
pub fn survey() -> Vec<Group> {
    if crate::demo::active() {
        return crate::demo::ai_groups();
    }
    std::thread::scope(|s| {
        let futures: Vec<_> = GROUPS
            .iter()
            .map(|&(title, _kind, f, desc)| {
                let h = std::thread::Builder::new().stack_size(LOADER_STACK).spawn_scoped(s, f);
                (title, desc, f, h)
            })
            .collect();
        futures
            .into_iter()
            .map(|(title, desc, f, h)| {
                let r = match h {
                    Ok(h) => h.join(),
                    Err(_) => std::panic::catch_unwind(f),
                };
                (title.to_string(), desc.to_string(), settle(title, r))
            })
            .collect()
    })
}

/// {title: number of entries}, in group order (a repeated title keeps its
/// first place and its last count, as a dict does).
pub fn counts(groups: &[Group]) -> Map<String, Value> {
    groups.iter().map(|(title, _d, entries)| (title.clone(), entries.len().into())).collect()
}

pub fn needs_attention(groups: &[Group]) -> usize {
    groups.iter().flat_map(|(_t, _d, entries)| entries).filter(|e| e.severity == "warn").count()
}

// ---- parity ------------------------------------------------------------------

/// The Python name of a loader, for `group_specs` parity.
fn loader_name(f: Loader) -> &'static str {
    const NAMES: &[(&str, Loader)] = &[
        ("harnesses", harnesses),
        ("mcp_servers", mcp_servers),
        ("skills", skills),
        ("plugins", plugins),
        ("marketplaces", marketplaces),
        ("hooks", hooks),
    ];
    NAMES.iter().find(|(_, g)| *g as usize == f as usize).map(|(n, _)| *n).unwrap_or("?")
}

fn loader_by_name(name: &str) -> Option<Loader> {
    Some(match name {
        "harnesses" => harnesses,
        "mcp_servers" => mcp_servers,
        "skills" => skills,
        "plugins" => plugins,
        "marketplaces" => marketplaces,
        "hooks" => hooks,
        "boom" => || Err("boom".to_string()),
        "panics" => || panic!("boom"),
        _ => return None,
    })
}

/// The cache file as JSON, `null` when missing or broken, with a numeric
/// `at` replaced by `"<number>"` (the time it was written cannot match).
fn read_back(path: &Path) -> Value {
    let mut v = std::fs::read_to_string(path).ok().and_then(|t| py_loads(&t).ok().flatten()).unwrap_or(Value::Null);
    if let Some(at) = v.get_mut("at") {
        if at.is_number() || special_float(at).is_some() {
            *at = Value::String("<number>".into());
        }
    }
    v
}

/// Parity hook: see ARCHITECTURE.md.
///
/// Common args: `env` ({VAR: value | null}, applied for the call; null
/// removes it) and `files` ({absolute path: text | null}, written -- or
/// removed -- before the call). A call that reaches a module still `todo!()`
/// is reported as an error rather than taking the whole run down.
pub fn parity(func: &str, args: &serde_json::Value) -> Option<serde_json::Value> {
    let saved: Vec<(String, Option<std::ffi::OsString>)> = args
        .get("env")
        .and_then(|e| e.as_object())
        .map(|m| {
            m.iter()
                .map(|(k, v)| {
                    let old = std::env::var_os(k);
                    match v.as_str() {
                        Some(s) => std::env::set_var(k, s),
                        None => std::env::remove_var(k),
                    }
                    (k.clone(), old)
                })
                .collect()
        })
        .unwrap_or_default();
    if let Some(files) = args.get("files").and_then(|f| f.as_object()) {
        for (p, content) in files {
            let p = Path::new(p);
            match content.as_str() {
                Some(text) => {
                    if let Some(parent) = p.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let _ = std::fs::write(p, text);
                }
                None => {
                    let _ = std::fs::remove_file(p);
                }
            }
        }
    }
    // An entry can hold a value as deep as Python's json decodes; turning
    // the result into serde JSON (serde's serializer recurses) needs room.
    let out = util::with_stack(util::JSON_STACK, || std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| parity_call(func, args))))
        .unwrap_or_else(|_| Some(serde_json::json!({ "error": "blocked: panicked (a module still todo!()?)" })));
    for (k, old) in saved {
        match old {
            Some(v) => std::env::set_var(&k, v),
            None => std::env::remove_var(&k),
        }
    }
    out
}

fn parity_call(func: &str, args: &Value) -> Option<Value> {
    use serde_json::{json, to_value};
    let s = |k: &str| util::arg_str(args, k).to_string();
    let result = |r: Result<Vec<Entry>, String>| -> Option<Value> {
        match r {
            Ok(v) => to_value(v).ok(),
            Err(e) => Some(json!({ "error": e })),
        }
    };
    // The readers run as load_group runs them (a deep file needs the stack).
    let big = |f: Loader| on_big_stack(f).unwrap_or_else(|p| std::panic::resume_unwind(p));
    let groups = || -> Vec<Group> { args.get("groups").cloned().and_then(|g| serde_json::from_value(g).ok()).unwrap_or_default() };
    Some(match func {
        "constants" => json!({
            "HOME": path_str(&home()), "CLAUDE": path_str(&claude()), "CACHE": path_str(&cache()),
            "MCP_CACHE": path_str(&mcp_cache()), "MCP_TTL": MCP_TTL,
            "kinds": [HARNESS, MCP, SKILL, PLUGIN, MARKET, HOOK],
        }),
        "Entry" => to_value(serde_json::from_value::<Entry>(args.get("fields").cloned().unwrap_or(json!({}))).ok()?).ok()?,
        "github_avatar" => json!(github_avatar(&s("repo"))),
        "host" => json!(host(&s("target"))),
        "run" => {
            let cmd = util::arg_strs(args, "cmd");
            let argv: Vec<&str> = cmd.iter().map(String::as_str).collect();
            let timeout = args.get("timeout").and_then(Value::as_u64).unwrap_or(60);
            match run(&argv, timeout) {
                Ok(text) => json!(text),
                Err(e) => json!({ "error": e }),
            }
        }
        "frontmatter" => to_value(frontmatter(Path::new(&s("path")))).ok()?,
        "tilde" => json!(tilde(&s("path"))),
        "harnesses" => result(big(harnesses))?,
        "mcp_servers" => {
            let r = result(big(mcp_servers))?;
            json!([r, read_back(&mcp_cache())])
        }
        "skills" => result(big(skills))?,
        // `json.dumps` of the entries: exact where a file name is not UTF-8.
        "skills_json" => match skills() {
            Ok(v) => json!(entries_json(&v)),
            Err(e) => json!({ "error": e }),
        },
        "plugins" => result(big(plugins))?,
        "marketplaces" => result(big(marketplaces))?,
        "hooks" => result(big(hooks))?,
        "group_specs" => json!(group_specs().iter().map(|(t, d, f)| json!([t, d, loader_name(*f)])).collect::<Vec<_>>()),
        "load_group" => to_value(load_group(loader_by_name(&s("loader"))?, &s("title"))).ok()?,
        "survey" => to_value(survey()).ok()?,
        "counts" => to_value(counts(&groups())).ok()?,
        "needs_attention" => json!(needs_attention(&groups())),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_nests_as_deep_as_pythons() {
        let n = PY_JSON_DEPTH;
        let text = format!("{{\"a\": {}{}}}", "[".repeat(n - 1), "]".repeat(n - 1));
        // A loader's stack, as in survey() and load_group().
        let r = std::thread::Builder::new()
            .stack_size(LOADER_STACK)
            .spawn(move || py_loads(&text).map(|v| v.is_some()))
            .unwrap()
            .join()
            .unwrap();
        assert_eq!(r, Ok(true));
        let over = format!("{}{}", "[".repeat(n + 1), "]".repeat(n + 1));
        assert!(py_loads(&over).is_err());
        assert_eq!(py_loads(&format!("{}{}", "[".repeat(300), "]".repeat(299))), Ok(None));
        let v = py_loads(&format!("{}1{}", "[".repeat(300), "]".repeat(300))).unwrap().unwrap();
        let mut d = 0;
        let mut cur = &v;
        while let Value::Array(a) = cur {
            d += 1;
            cur = &a[0];
        }
        assert_eq!((d, cur.clone()), (300, Value::from(1)));
    }

    #[test]
    fn ai_survey_returns_triples() {
        // the AI page unpacks these as (title, desc, loader)
        let specs = group_specs();
        assert_eq!(specs.len(), GROUPS.len());
        for (title, desc, _f) in specs {
            assert!(!title.is_empty());
            assert!(!desc.is_empty());
        }
    }

    #[test]
    fn paths_from_home_read_as_tilde() {
        let home = util::home().to_string_lossy().trim_end_matches('/').to_string();
        assert_eq!(tilde(&format!("{home}/.local/bin/claude")), "~/.local/bin/claude");
        assert_eq!(tilde("/usr/bin/x"), "/usr/bin/x");
    }

    #[test]
    fn frontmatter_folds_and_lowercases() {
        let dir = std::env::temp_dir().join(format!("almanac-ai-fm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("SKILL.md");
        std::fs::write(&f, "---\nName: x\ndescription: >\n  one\n  two\n---\nbody\n").unwrap();
        let fm = frontmatter(&f);
        assert_eq!(fm.get("name").and_then(Value::as_str), Some("x"));
        assert_eq!(fm.get("description").and_then(Value::as_str), Some("one two"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn host_of_targets() {
        assert_eq!(host("https://mcp.example.org:8080/x"), "mcp.example.org");
        assert_eq!(host("npx foo"), "");
    }

    #[test]
    fn entries_holding_a_deep_value_clone_compare_and_drop_on_a_small_stack() {
        let n = 100_000;
        let deep: Value = {
            let mut v = Value::from(5);
            for _ in 0..n {
                v = Value::Array(vec![v]);
            }
            v
        };
        let mut e = Entry::default();
        e.py_values.insert("url".into(), deep);
        std::thread::Builder::new()
            .stack_size(256 << 10)
            .spawn(move || {
                let f = e.clone();
                assert!(e == f);
                let mut g = f.clone();
                if let Some(old) = g.py_values.insert("url".into(), Value::from(5)) {
                    util::tree::drop_value(old);
                }
                assert!(e != g);
            })
            .unwrap()
            .join()
            .unwrap();
    }
}

/// Code points `str.isprintable()` rejects (Python 3.14, Unicode 16.0.0), as
/// inclusive ranges, generated from `unicodedata`.
#[rustfmt::skip]
const NONPRINTABLE: &[(u32, u32)] = &[
    (0x0, 0x1f), (0x7f, 0xa0), (0xad, 0xad), (0x378, 0x379), (0x380, 0x383), (0x38b, 0x38b),
    (0x38d, 0x38d), (0x3a2, 0x3a2), (0x530, 0x530), (0x557, 0x558), (0x58b, 0x58c), (0x590, 0x590),
    (0x5c8, 0x5cf), (0x5eb, 0x5ee), (0x5f5, 0x605), (0x61c, 0x61c), (0x6dd, 0x6dd), (0x70e, 0x70f),
    (0x74b, 0x74c), (0x7b2, 0x7bf), (0x7fb, 0x7fc), (0x82e, 0x82f), (0x83f, 0x83f), (0x85c, 0x85d),
    (0x85f, 0x85f), (0x86b, 0x86f), (0x88f, 0x896), (0x8e2, 0x8e2), (0x984, 0x984), (0x98d, 0x98e),
    (0x991, 0x992), (0x9a9, 0x9a9), (0x9b1, 0x9b1), (0x9b3, 0x9b5), (0x9ba, 0x9bb), (0x9c5, 0x9c6),
    (0x9c9, 0x9ca), (0x9cf, 0x9d6), (0x9d8, 0x9db), (0x9de, 0x9de), (0x9e4, 0x9e5), (0x9ff, 0xa00),
    (0xa04, 0xa04), (0xa0b, 0xa0e), (0xa11, 0xa12), (0xa29, 0xa29), (0xa31, 0xa31), (0xa34, 0xa34),
    (0xa37, 0xa37), (0xa3a, 0xa3b), (0xa3d, 0xa3d), (0xa43, 0xa46), (0xa49, 0xa4a), (0xa4e, 0xa50),
    (0xa52, 0xa58), (0xa5d, 0xa5d), (0xa5f, 0xa65), (0xa77, 0xa80), (0xa84, 0xa84), (0xa8e, 0xa8e),
    (0xa92, 0xa92), (0xaa9, 0xaa9), (0xab1, 0xab1), (0xab4, 0xab4), (0xaba, 0xabb), (0xac6, 0xac6),
    (0xaca, 0xaca), (0xace, 0xacf), (0xad1, 0xadf), (0xae4, 0xae5), (0xaf2, 0xaf8), (0xb00, 0xb00),
    (0xb04, 0xb04), (0xb0d, 0xb0e), (0xb11, 0xb12), (0xb29, 0xb29), (0xb31, 0xb31), (0xb34, 0xb34),
    (0xb3a, 0xb3b), (0xb45, 0xb46), (0xb49, 0xb4a), (0xb4e, 0xb54), (0xb58, 0xb5b), (0xb5e, 0xb5e),
    (0xb64, 0xb65), (0xb78, 0xb81), (0xb84, 0xb84), (0xb8b, 0xb8d), (0xb91, 0xb91), (0xb96, 0xb98),
    (0xb9b, 0xb9b), (0xb9d, 0xb9d), (0xba0, 0xba2), (0xba5, 0xba7), (0xbab, 0xbad), (0xbba, 0xbbd),
    (0xbc3, 0xbc5), (0xbc9, 0xbc9), (0xbce, 0xbcf), (0xbd1, 0xbd6), (0xbd8, 0xbe5), (0xbfb, 0xbff),
    (0xc0d, 0xc0d), (0xc11, 0xc11), (0xc29, 0xc29), (0xc3a, 0xc3b), (0xc45, 0xc45), (0xc49, 0xc49),
    (0xc4e, 0xc54), (0xc57, 0xc57), (0xc5b, 0xc5c), (0xc5e, 0xc5f), (0xc64, 0xc65), (0xc70, 0xc76),
    (0xc8d, 0xc8d), (0xc91, 0xc91), (0xca9, 0xca9), (0xcb4, 0xcb4), (0xcba, 0xcbb), (0xcc5, 0xcc5),
    (0xcc9, 0xcc9), (0xcce, 0xcd4), (0xcd7, 0xcdc), (0xcdf, 0xcdf), (0xce4, 0xce5), (0xcf0, 0xcf0),
    (0xcf4, 0xcff), (0xd0d, 0xd0d), (0xd11, 0xd11), (0xd45, 0xd45), (0xd49, 0xd49), (0xd50, 0xd53),
    (0xd64, 0xd65), (0xd80, 0xd80), (0xd84, 0xd84), (0xd97, 0xd99), (0xdb2, 0xdb2), (0xdbc, 0xdbc),
    (0xdbe, 0xdbf), (0xdc7, 0xdc9), (0xdcb, 0xdce), (0xdd5, 0xdd5), (0xdd7, 0xdd7), (0xde0, 0xde5),
    (0xdf0, 0xdf1), (0xdf5, 0xe00), (0xe3b, 0xe3e), (0xe5c, 0xe80), (0xe83, 0xe83), (0xe85, 0xe85),
    (0xe8b, 0xe8b), (0xea4, 0xea4), (0xea6, 0xea6), (0xebe, 0xebf), (0xec5, 0xec5), (0xec7, 0xec7),
    (0xecf, 0xecf), (0xeda, 0xedb), (0xee0, 0xeff), (0xf48, 0xf48), (0xf6d, 0xf70), (0xf98, 0xf98),
    (0xfbd, 0xfbd), (0xfcd, 0xfcd), (0xfdb, 0xfff), (0x10c6, 0x10c6), (0x10c8, 0x10cc), (0x10ce, 0x10cf),
    (0x1249, 0x1249), (0x124e, 0x124f), (0x1257, 0x1257), (0x1259, 0x1259), (0x125e, 0x125f), (0x1289, 0x1289),
    (0x128e, 0x128f), (0x12b1, 0x12b1), (0x12b6, 0x12b7), (0x12bf, 0x12bf), (0x12c1, 0x12c1), (0x12c6, 0x12c7),
    (0x12d7, 0x12d7), (0x1311, 0x1311), (0x1316, 0x1317), (0x135b, 0x135c), (0x137d, 0x137f), (0x139a, 0x139f),
    (0x13f6, 0x13f7), (0x13fe, 0x13ff), (0x1680, 0x1680), (0x169d, 0x169f), (0x16f9, 0x16ff), (0x1716, 0x171e),
    (0x1737, 0x173f), (0x1754, 0x175f), (0x176d, 0x176d), (0x1771, 0x1771), (0x1774, 0x177f), (0x17de, 0x17df),
    (0x17ea, 0x17ef), (0x17fa, 0x17ff), (0x180e, 0x180e), (0x181a, 0x181f), (0x1879, 0x187f), (0x18ab, 0x18af),
    (0x18f6, 0x18ff), (0x191f, 0x191f), (0x192c, 0x192f), (0x193c, 0x193f), (0x1941, 0x1943), (0x196e, 0x196f),
    (0x1975, 0x197f), (0x19ac, 0x19af), (0x19ca, 0x19cf), (0x19db, 0x19dd), (0x1a1c, 0x1a1d), (0x1a5f, 0x1a5f),
    (0x1a7d, 0x1a7e), (0x1a8a, 0x1a8f), (0x1a9a, 0x1a9f), (0x1aae, 0x1aaf), (0x1acf, 0x1aff), (0x1b4d, 0x1b4d),
    (0x1bf4, 0x1bfb), (0x1c38, 0x1c3a), (0x1c4a, 0x1c4c), (0x1c8b, 0x1c8f), (0x1cbb, 0x1cbc), (0x1cc8, 0x1ccf),
    (0x1cfb, 0x1cff), (0x1f16, 0x1f17), (0x1f1e, 0x1f1f), (0x1f46, 0x1f47), (0x1f4e, 0x1f4f), (0x1f58, 0x1f58),
    (0x1f5a, 0x1f5a), (0x1f5c, 0x1f5c), (0x1f5e, 0x1f5e), (0x1f7e, 0x1f7f), (0x1fb5, 0x1fb5), (0x1fc5, 0x1fc5),
    (0x1fd4, 0x1fd5), (0x1fdc, 0x1fdc), (0x1ff0, 0x1ff1), (0x1ff5, 0x1ff5), (0x1fff, 0x200f), (0x2028, 0x202f),
    (0x205f, 0x206f), (0x2072, 0x2073), (0x208f, 0x208f), (0x209d, 0x209f), (0x20c1, 0x20cf), (0x20f1, 0x20ff),
    (0x218c, 0x218f), (0x242a, 0x243f), (0x244b, 0x245f), (0x2b74, 0x2b75), (0x2b96, 0x2b96), (0x2cf4, 0x2cf8),
    (0x2d26, 0x2d26), (0x2d28, 0x2d2c), (0x2d2e, 0x2d2f), (0x2d68, 0x2d6e), (0x2d71, 0x2d7e), (0x2d97, 0x2d9f),
    (0x2da7, 0x2da7), (0x2daf, 0x2daf), (0x2db7, 0x2db7), (0x2dbf, 0x2dbf), (0x2dc7, 0x2dc7), (0x2dcf, 0x2dcf),
    (0x2dd7, 0x2dd7), (0x2ddf, 0x2ddf), (0x2e5e, 0x2e7f), (0x2e9a, 0x2e9a), (0x2ef4, 0x2eff), (0x2fd6, 0x2fef),
    (0x3000, 0x3000), (0x3040, 0x3040), (0x3097, 0x3098), (0x3100, 0x3104), (0x3130, 0x3130), (0x318f, 0x318f),
    (0x31e6, 0x31ee), (0x321f, 0x321f), (0xa48d, 0xa48f), (0xa4c7, 0xa4cf), (0xa62c, 0xa63f), (0xa6f8, 0xa6ff),
    (0xa7ce, 0xa7cf), (0xa7d2, 0xa7d2), (0xa7d4, 0xa7d4), (0xa7dd, 0xa7f1), (0xa82d, 0xa82f), (0xa83a, 0xa83f),
    (0xa878, 0xa87f), (0xa8c6, 0xa8cd), (0xa8da, 0xa8df), (0xa954, 0xa95e), (0xa97d, 0xa97f), (0xa9ce, 0xa9ce),
    (0xa9da, 0xa9dd), (0xa9ff, 0xa9ff), (0xaa37, 0xaa3f), (0xaa4e, 0xaa4f), (0xaa5a, 0xaa5b), (0xaac3, 0xaada),
    (0xaaf7, 0xab00), (0xab07, 0xab08), (0xab0f, 0xab10), (0xab17, 0xab1f), (0xab27, 0xab27), (0xab2f, 0xab2f),
    (0xab6c, 0xab6f), (0xabee, 0xabef), (0xabfa, 0xabff), (0xd7a4, 0xd7af), (0xd7c7, 0xd7ca), (0xd7fc, 0xf8ff),
    (0xfa6e, 0xfa6f), (0xfada, 0xfaff), (0xfb07, 0xfb12), (0xfb18, 0xfb1c), (0xfb37, 0xfb37), (0xfb3d, 0xfb3d),
    (0xfb3f, 0xfb3f), (0xfb42, 0xfb42), (0xfb45, 0xfb45), (0xfbc3, 0xfbd2), (0xfd90, 0xfd91), (0xfdc8, 0xfdce),
    (0xfdd0, 0xfdef), (0xfe1a, 0xfe1f), (0xfe53, 0xfe53), (0xfe67, 0xfe67), (0xfe6c, 0xfe6f), (0xfe75, 0xfe75),
    (0xfefd, 0xff00), (0xffbf, 0xffc1), (0xffc8, 0xffc9), (0xffd0, 0xffd1), (0xffd8, 0xffd9), (0xffdd, 0xffdf),
    (0xffe7, 0xffe7), (0xffef, 0xfffb), (0xfffe, 0xffff), (0x1000c, 0x1000c), (0x10027, 0x10027), (0x1003b, 0x1003b),
    (0x1003e, 0x1003e), (0x1004e, 0x1004f), (0x1005e, 0x1007f), (0x100fb, 0x100ff), (0x10103, 0x10106), (0x10134, 0x10136),
    (0x1018f, 0x1018f), (0x1019d, 0x1019f), (0x101a1, 0x101cf), (0x101fe, 0x1027f), (0x1029d, 0x1029f), (0x102d1, 0x102df),
    (0x102fc, 0x102ff), (0x10324, 0x1032c), (0x1034b, 0x1034f), (0x1037b, 0x1037f), (0x1039e, 0x1039e), (0x103c4, 0x103c7),
    (0x103d6, 0x103ff), (0x1049e, 0x1049f), (0x104aa, 0x104af), (0x104d4, 0x104d7), (0x104fc, 0x104ff), (0x10528, 0x1052f),
    (0x10564, 0x1056e), (0x1057b, 0x1057b), (0x1058b, 0x1058b), (0x10593, 0x10593), (0x10596, 0x10596), (0x105a2, 0x105a2),
    (0x105b2, 0x105b2), (0x105ba, 0x105ba), (0x105bd, 0x105bf), (0x105f4, 0x105ff), (0x10737, 0x1073f), (0x10756, 0x1075f),
    (0x10768, 0x1077f), (0x10786, 0x10786), (0x107b1, 0x107b1), (0x107bb, 0x107ff), (0x10806, 0x10807), (0x10809, 0x10809),
    (0x10836, 0x10836), (0x10839, 0x1083b), (0x1083d, 0x1083e), (0x10856, 0x10856), (0x1089f, 0x108a6), (0x108b0, 0x108df),
    (0x108f3, 0x108f3), (0x108f6, 0x108fa), (0x1091c, 0x1091e), (0x1093a, 0x1093e), (0x10940, 0x1097f), (0x109b8, 0x109bb),
    (0x109d0, 0x109d1), (0x10a04, 0x10a04), (0x10a07, 0x10a0b), (0x10a14, 0x10a14), (0x10a18, 0x10a18), (0x10a36, 0x10a37),
    (0x10a3b, 0x10a3e), (0x10a49, 0x10a4f), (0x10a59, 0x10a5f), (0x10aa0, 0x10abf), (0x10ae7, 0x10aea), (0x10af7, 0x10aff),
    (0x10b36, 0x10b38), (0x10b56, 0x10b57), (0x10b73, 0x10b77), (0x10b92, 0x10b98), (0x10b9d, 0x10ba8), (0x10bb0, 0x10bff),
    (0x10c49, 0x10c7f), (0x10cb3, 0x10cbf), (0x10cf3, 0x10cf9), (0x10d28, 0x10d2f), (0x10d3a, 0x10d3f), (0x10d66, 0x10d68),
    (0x10d86, 0x10d8d), (0x10d90, 0x10e5f), (0x10e7f, 0x10e7f), (0x10eaa, 0x10eaa), (0x10eae, 0x10eaf), (0x10eb2, 0x10ec1),
    (0x10ec5, 0x10efb), (0x10f28, 0x10f2f), (0x10f5a, 0x10f6f), (0x10f8a, 0x10faf), (0x10fcc, 0x10fdf), (0x10ff7, 0x10fff),
    (0x1104e, 0x11051), (0x11076, 0x1107e), (0x110bd, 0x110bd), (0x110c3, 0x110cf), (0x110e9, 0x110ef), (0x110fa, 0x110ff),
    (0x11135, 0x11135), (0x11148, 0x1114f), (0x11177, 0x1117f), (0x111e0, 0x111e0), (0x111f5, 0x111ff), (0x11212, 0x11212),
    (0x11242, 0x1127f), (0x11287, 0x11287), (0x11289, 0x11289), (0x1128e, 0x1128e), (0x1129e, 0x1129e), (0x112aa, 0x112af),
    (0x112eb, 0x112ef), (0x112fa, 0x112ff), (0x11304, 0x11304), (0x1130d, 0x1130e), (0x11311, 0x11312), (0x11329, 0x11329),
    (0x11331, 0x11331), (0x11334, 0x11334), (0x1133a, 0x1133a), (0x11345, 0x11346), (0x11349, 0x1134a), (0x1134e, 0x1134f),
    (0x11351, 0x11356), (0x11358, 0x1135c), (0x11364, 0x11365), (0x1136d, 0x1136f), (0x11375, 0x1137f), (0x1138a, 0x1138a),
    (0x1138c, 0x1138d), (0x1138f, 0x1138f), (0x113b6, 0x113b6), (0x113c1, 0x113c1), (0x113c3, 0x113c4), (0x113c6, 0x113c6),
    (0x113cb, 0x113cb), (0x113d6, 0x113d6), (0x113d9, 0x113e0), (0x113e3, 0x113ff), (0x1145c, 0x1145c), (0x11462, 0x1147f),
    (0x114c8, 0x114cf), (0x114da, 0x1157f), (0x115b6, 0x115b7), (0x115de, 0x115ff), (0x11645, 0x1164f), (0x1165a, 0x1165f),
    (0x1166d, 0x1167f), (0x116ba, 0x116bf), (0x116ca, 0x116cf), (0x116e4, 0x116ff), (0x1171b, 0x1171c), (0x1172c, 0x1172f),
    (0x11747, 0x117ff), (0x1183c, 0x1189f), (0x118f3, 0x118fe), (0x11907, 0x11908), (0x1190a, 0x1190b), (0x11914, 0x11914),
    (0x11917, 0x11917), (0x11936, 0x11936), (0x11939, 0x1193a), (0x11947, 0x1194f), (0x1195a, 0x1199f), (0x119a8, 0x119a9),
    (0x119d8, 0x119d9), (0x119e5, 0x119ff), (0x11a48, 0x11a4f), (0x11aa3, 0x11aaf), (0x11af9, 0x11aff), (0x11b0a, 0x11bbf),
    (0x11be2, 0x11bef), (0x11bfa, 0x11bff), (0x11c09, 0x11c09), (0x11c37, 0x11c37), (0x11c46, 0x11c4f), (0x11c6d, 0x11c6f),
    (0x11c90, 0x11c91), (0x11ca8, 0x11ca8), (0x11cb7, 0x11cff), (0x11d07, 0x11d07), (0x11d0a, 0x11d0a), (0x11d37, 0x11d39),
    (0x11d3b, 0x11d3b), (0x11d3e, 0x11d3e), (0x11d48, 0x11d4f), (0x11d5a, 0x11d5f), (0x11d66, 0x11d66), (0x11d69, 0x11d69),
    (0x11d8f, 0x11d8f), (0x11d92, 0x11d92), (0x11d99, 0x11d9f), (0x11daa, 0x11edf), (0x11ef9, 0x11eff), (0x11f11, 0x11f11),
    (0x11f3b, 0x11f3d), (0x11f5b, 0x11faf), (0x11fb1, 0x11fbf), (0x11ff2, 0x11ffe), (0x1239a, 0x123ff), (0x1246f, 0x1246f),
    (0x12475, 0x1247f), (0x12544, 0x12f8f), (0x12ff3, 0x12fff), (0x13430, 0x1343f), (0x13456, 0x1345f), (0x143fb, 0x143ff),
    (0x14647, 0x160ff), (0x1613a, 0x167ff), (0x16a39, 0x16a3f), (0x16a5f, 0x16a5f), (0x16a6a, 0x16a6d), (0x16abf, 0x16abf),
    (0x16aca, 0x16acf), (0x16aee, 0x16aef), (0x16af6, 0x16aff), (0x16b46, 0x16b4f), (0x16b5a, 0x16b5a), (0x16b62, 0x16b62),
    (0x16b78, 0x16b7c), (0x16b90, 0x16d3f), (0x16d7a, 0x16e3f), (0x16e9b, 0x16eff), (0x16f4b, 0x16f4e), (0x16f88, 0x16f8e),
    (0x16fa0, 0x16fdf), (0x16fe5, 0x16fef), (0x16ff2, 0x16fff), (0x187f8, 0x187ff), (0x18cd6, 0x18cfe), (0x18d09, 0x1afef),
    (0x1aff4, 0x1aff4), (0x1affc, 0x1affc), (0x1afff, 0x1afff), (0x1b123, 0x1b131), (0x1b133, 0x1b14f), (0x1b153, 0x1b154),
    (0x1b156, 0x1b163), (0x1b168, 0x1b16f), (0x1b2fc, 0x1bbff), (0x1bc6b, 0x1bc6f), (0x1bc7d, 0x1bc7f), (0x1bc89, 0x1bc8f),
    (0x1bc9a, 0x1bc9b), (0x1bca0, 0x1cbff), (0x1ccfa, 0x1ccff), (0x1ceb4, 0x1ceff), (0x1cf2e, 0x1cf2f), (0x1cf47, 0x1cf4f),
    (0x1cfc4, 0x1cfff), (0x1d0f6, 0x1d0ff), (0x1d127, 0x1d128), (0x1d173, 0x1d17a), (0x1d1eb, 0x1d1ff), (0x1d246, 0x1d2bf),
    (0x1d2d4, 0x1d2df), (0x1d2f4, 0x1d2ff), (0x1d357, 0x1d35f), (0x1d379, 0x1d3ff), (0x1d455, 0x1d455), (0x1d49d, 0x1d49d),
    (0x1d4a0, 0x1d4a1), (0x1d4a3, 0x1d4a4), (0x1d4a7, 0x1d4a8), (0x1d4ad, 0x1d4ad), (0x1d4ba, 0x1d4ba), (0x1d4bc, 0x1d4bc),
    (0x1d4c4, 0x1d4c4), (0x1d506, 0x1d506), (0x1d50b, 0x1d50c), (0x1d515, 0x1d515), (0x1d51d, 0x1d51d), (0x1d53a, 0x1d53a),
    (0x1d53f, 0x1d53f), (0x1d545, 0x1d545), (0x1d547, 0x1d549), (0x1d551, 0x1d551), (0x1d6a6, 0x1d6a7), (0x1d7cc, 0x1d7cd),
    (0x1da8c, 0x1da9a), (0x1daa0, 0x1daa0), (0x1dab0, 0x1deff), (0x1df1f, 0x1df24), (0x1df2b, 0x1dfff), (0x1e007, 0x1e007),
    (0x1e019, 0x1e01a), (0x1e022, 0x1e022), (0x1e025, 0x1e025), (0x1e02b, 0x1e02f), (0x1e06e, 0x1e08e), (0x1e090, 0x1e0ff),
    (0x1e12d, 0x1e12f), (0x1e13e, 0x1e13f), (0x1e14a, 0x1e14d), (0x1e150, 0x1e28f), (0x1e2af, 0x1e2bf), (0x1e2fa, 0x1e2fe),
    (0x1e300, 0x1e4cf), (0x1e4fa, 0x1e5cf), (0x1e5fb, 0x1e5fe), (0x1e600, 0x1e7df), (0x1e7e7, 0x1e7e7), (0x1e7ec, 0x1e7ec),
    (0x1e7ef, 0x1e7ef), (0x1e7ff, 0x1e7ff), (0x1e8c5, 0x1e8c6), (0x1e8d7, 0x1e8ff), (0x1e94c, 0x1e94f), (0x1e95a, 0x1e95d),
    (0x1e960, 0x1ec70), (0x1ecb5, 0x1ed00), (0x1ed3e, 0x1edff), (0x1ee04, 0x1ee04), (0x1ee20, 0x1ee20), (0x1ee23, 0x1ee23),
    (0x1ee25, 0x1ee26), (0x1ee28, 0x1ee28), (0x1ee33, 0x1ee33), (0x1ee38, 0x1ee38), (0x1ee3a, 0x1ee3a), (0x1ee3c, 0x1ee41),
    (0x1ee43, 0x1ee46), (0x1ee48, 0x1ee48), (0x1ee4a, 0x1ee4a), (0x1ee4c, 0x1ee4c), (0x1ee50, 0x1ee50), (0x1ee53, 0x1ee53),
    (0x1ee55, 0x1ee56), (0x1ee58, 0x1ee58), (0x1ee5a, 0x1ee5a), (0x1ee5c, 0x1ee5c), (0x1ee5e, 0x1ee5e), (0x1ee60, 0x1ee60),
    (0x1ee63, 0x1ee63), (0x1ee65, 0x1ee66), (0x1ee6b, 0x1ee6b), (0x1ee73, 0x1ee73), (0x1ee78, 0x1ee78), (0x1ee7d, 0x1ee7d),
    (0x1ee7f, 0x1ee7f), (0x1ee8a, 0x1ee8a), (0x1ee9c, 0x1eea0), (0x1eea4, 0x1eea4), (0x1eeaa, 0x1eeaa), (0x1eebc, 0x1eeef),
    (0x1eef2, 0x1efff), (0x1f02c, 0x1f02f), (0x1f094, 0x1f09f), (0x1f0af, 0x1f0b0), (0x1f0c0, 0x1f0c0), (0x1f0d0, 0x1f0d0),
    (0x1f0f6, 0x1f0ff), (0x1f1ae, 0x1f1e5), (0x1f203, 0x1f20f), (0x1f23c, 0x1f23f), (0x1f249, 0x1f24f), (0x1f252, 0x1f25f),
    (0x1f266, 0x1f2ff), (0x1f6d8, 0x1f6db), (0x1f6ed, 0x1f6ef), (0x1f6fd, 0x1f6ff), (0x1f777, 0x1f77a), (0x1f7da, 0x1f7df),
    (0x1f7ec, 0x1f7ef), (0x1f7f1, 0x1f7ff), (0x1f80c, 0x1f80f), (0x1f848, 0x1f84f), (0x1f85a, 0x1f85f), (0x1f888, 0x1f88f),
    (0x1f8ae, 0x1f8af), (0x1f8bc, 0x1f8bf), (0x1f8c2, 0x1f8ff), (0x1fa54, 0x1fa5f), (0x1fa6e, 0x1fa6f), (0x1fa7d, 0x1fa7f),
    (0x1fa8a, 0x1fa8e), (0x1fac7, 0x1facd), (0x1fadd, 0x1fade), (0x1faea, 0x1faef), (0x1faf9, 0x1faff), (0x1fb93, 0x1fb93),
    (0x1fbfa, 0x1ffff), (0x2a6e0, 0x2a6ff), (0x2b73a, 0x2b73f), (0x2b81e, 0x2b81f), (0x2cea2, 0x2ceaf), (0x2ebe1, 0x2ebef),
    (0x2ee5e, 0x2f7ff), (0x2fa1e, 0x2ffff), (0x3134b, 0x3134f), (0x323b0, 0xe00ff), (0xe01f0, 0x10ffff),
];

/// The zero of every run of Unicode decimal digits (category Nd), which
/// `int()` accepts as 0-9; generated from `unicodedata`.
#[rustfmt::skip]
const DECIMAL_ZEROS: &[u32] = &[
    0x30, 0x660, 0x6f0, 0x7c0, 0x966, 0x9e6, 0xa66, 0xae6, 0xb66, 0xbe6,
    0xc66, 0xce6, 0xd66, 0xde6, 0xe50, 0xed0, 0xf20, 0x1040, 0x1090, 0x17e0,
    0x1810, 0x1946, 0x19d0, 0x1a80, 0x1a90, 0x1b50, 0x1bb0, 0x1c40, 0x1c50, 0xa620,
    0xa8d0, 0xa900, 0xa9d0, 0xa9f0, 0xaa50, 0xabf0, 0xff10, 0x104a0, 0x10d30, 0x10d40,
    0x11066, 0x110f0, 0x11136, 0x111d0, 0x112f0, 0x11450, 0x114d0, 0x11650, 0x116c0, 0x116d0,
    0x116da, 0x11730, 0x118e0, 0x11950, 0x11bf0, 0x11c50, 0x11d50, 0x11da0, 0x11f50, 0x16130,
    0x16a60, 0x16ac0, 0x16b50, 0x16d70, 0x1ccf0, 0x1d7ce, 0x1d7d8, 0x1d7e2, 0x1d7ec, 0x1d7f6,
    0x1e140, 0x1e2f0, 0x1e4f0, 0x1e5f1, 0x1e950, 0x1fbf0,
];
