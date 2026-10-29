//! Port of `verify.py`.
//!
//! Did it work? Checked, not assumed.
//!
//! A command's exit status says the command ran. It does not say the thing the
//! user asked for happened. Every silent failure this app has had was that gap:
//! `yay -S --needed` skipping a package the system database did not know was
//! newer, `journalctl --vacuum-size` deleting nothing without root, `paccache`
//! trimming a cache that had nothing left to trim -- each exited 0, each was
//! reported as done, and the row stayed exactly where it was.
//!
//! So every action that changes something is bracketed by a probe: the same
//! question asked before and after ("which version is installed?", "what does
//! this check find?"). The answer the user sees is the difference between the
//! two, never the exit status.
//!
//! Probes are plain callables returning something comparable. They run off the
//! UI thread and must never raise; a probe that cannot answer returns None, and
//! "could not tell" is reported as that rather than as success or failure.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Outcome {
    /// None: the probe could not tell
    pub changed: Option<bool>,
    pub message: String,
}

impl Outcome {
    pub fn new(changed: Option<bool>, message: impl Into<String>) -> Self {
        Outcome { changed, message: message.into() }
    }

    pub fn ok(&self) -> bool {
        self.changed.unwrap_or(false)
    }
}

/// "Something comparable": what a probe answers. Python probes return a str,
/// a `(size, mtime)` tuple, a set, or whatever an ad-hoc probe in the UI
/// returns (a bool, a dict of plugin HEADs); this enum covers those.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProbeValue {
    Str(String),
    /// `(st_size, int(st_mtime))` from `file_identity`.
    Identity(u64, i64),
    Set(BTreeSet<String>),
    Bool(bool),
    Map(BTreeMap<String, Option<String>>),
}

/// A probe: asked before and after, off the UI thread. `None` is "could not
/// tell".
pub type Probe = Arc<dyn Fn() -> Option<ProbeValue> + Send + Sync>;

/// A probe as the Python really behaves: besides answering or answering None,
/// it can raise (an exception `_out` does not catch, a non-string path, an
/// embedded NUL). `Err` carries `"<ExceptionType>: <message>"`. The `*_try`
/// constructors give these, for a caller that lets the exception end its
/// report the way the Python's does.
pub type TryProbe = Arc<dyn Fn() -> Result<Option<ProbeValue>, String> + Send + Sync>;

/// `str.isspace()`: Rust's whitespace plus the four separators \x1c-\x1f.
fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// `str.strip()`.
fn py_strip(s: &str) -> &str {
    s.trim_matches(py_isspace)
}

/// `str.split()`.
fn py_split(s: &str) -> Vec<&str> {
    s.split(py_isspace).filter(|x| !x.is_empty()).collect()
}

/// `text=True` decoding: strict UTF-8 (a bad byte raises UnicodeDecodeError,
/// worded as Python words it) in universal-newlines mode (`\r\n` and a lone
/// `\r` become `\n`).
fn py_text(bytes: Vec<u8>) -> Result<String, String> {
    let s = String::from_utf8(bytes).map_err(|e| {
        let b = e.as_bytes();
        let at = e.utf8_error().valid_up_to();
        // Rust's error length is the same maximal invalid subpart Python
        // reports as the range.
        let (n, reason) = match e.utf8_error().error_len() {
            Some(n) => {
                let lead = b.get(at).copied().unwrap_or(0);
                let bad_start = (0x80..=0xC1).contains(&lead) || lead >= 0xF5;
                (n, if bad_start { "invalid start byte" } else { "invalid continuation byte" })
            }
            None => (b.len() - at, "unexpected end of data"),
        };
        if n == 1 {
            format!(
                "UnicodeDecodeError: 'utf-8' codec can't decode byte {:#04x} in position {at}: {reason}",
                b.get(at).copied().unwrap_or(0)
            )
        } else {
            format!(
                "UnicodeDecodeError: 'utf-8' codec can't decode bytes in position {at}-{}: {reason}",
                at + n - 1
            )
        }
    })?;
    Ok(if s.contains('\r') { s.replace("\r\n", "\n").replace('\r', "\n") } else { s })
}

/// `subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)`,
/// exactly: stdin inherited, and `communicate()` waits for EOF on both pipes
/// (a grandchild holding stdout open runs into the timeout) before the exit
/// status. `Ok(None)` is OSError or TimeoutExpired; `Err` is what Python
/// raises past `(OSError, SubprocessError)`. A timeout that is not finite
/// (`timeout=None`) is no timeout.
///
/// `util::run_bytes` with stdin inherited, and the decode done here: `util::run`
/// would decode lossily without newline translation.
fn py_subprocess(cmd: &[&str], timeout: f64) -> Result<Option<(Option<i32>, String)>, String> {
    use crate::util::{run_bytes, RunOpts, Stdin};
    use std::time::Duration;

    if cmd.is_empty() {
        return Err("IndexError: list index out of range".into());
    }
    if cmd.iter().any(|a| a.contains('\0')) {
        return Err("ValueError: embedded null byte".into());
    }
    // None: wait as long as it takes (as when the deadline would overflow).
    let timeout = timeout.is_finite().then(|| Duration::from_secs_f64(timeout.clamp(0.0, 1e9)))
        .filter(|t| std::time::Instant::now().checked_add(*t).is_some());
    let out = run_bytes(cmd, &RunOpts { timeout, stdin: Stdin::Inherit, ..Default::default() });
    if out.failed_to_start || out.timed_out {
        return Ok(None); // OSError / TimeoutExpired
    }
    let stdout = py_text(out.stdout)?;
    py_text(out.stderr)?;
    Ok(Some((out.code, stdout)))
}

/// `_out`: stdout, stripped, when the command succeeded; "" otherwise
/// (including a command that could not start or timed out). Python default
/// timeout: 30. `Err` is an exception Python lets through.
fn out(cmd: &[&str], timeout: f64) -> Result<String, String> {
    // (OSError, subprocess.SubprocessError) and a non-zero exit are both "".
    Ok(match py_subprocess(cmd, timeout)? {
        Some((Some(0), stdout)) => py_strip(&stdout).to_string(),
        _ => String::new(),
    })
}

/// A value handed to subprocess as an argv item: a string is used as is;
/// anything else makes subprocess raise TypeError.
fn subprocess_arg(v: &Py) -> Result<String, String> {
    match v {
        Py::Str(s) => Ok(s.clone()),
        other => Err(format!(
            "TypeError: expected str, bytes or os.PathLike object, not {}",
            other.type_name()
        )),
    }
}

/// A probe that raises `err` when called.
fn raising(err: String) -> TryProbe {
    Arc::new(move || Err(err.clone()))
}

// ---- probes ------------------------------------------------------------------

/// The installed version of a pacman package, or '' when it is absent;
/// Err where the Python raises.
pub fn installed_package_try(name: &str) -> TryProbe {
    let name = name.to_string();
    Arc::new(move || {
        let o = out(&["pacman", "-Q", &name], 30.0)?;
        let parts = py_split(&o);
        Ok(Some(ProbeValue::Str(if parts.len() == 2 { parts[1].to_string() } else { String::new() })))
    })
}

/// A checkout's HEAD; Err where the Python raises.
pub fn git_head_try(repo: &str) -> TryProbe {
    let repo = repo.to_string();
    Arc::new(move || {
        let o = out(&["git", "-C", &repo, "rev-parse", "HEAD"], 30.0)?;
        Ok(if o.is_empty() { None } else { Some(ProbeValue::Str(o)) })
    })
}

/// The version mise has installed of `tool`, raising what
/// `agents._mise_installed` raises (mise
/// printing JSON that is not an object), as the Python probe does.
pub fn mise_version_try(tool: &str) -> TryProbe {
    let tool = tool.to_string();
    Arc::new(move || {
        let have = crate::agents::try_mise_installed()?;
        // `.get(tool) or None`
        Ok(have
            .into_iter()
            .rev()
            .find(|(k, _)| *k == tool)
            .map(|(_, v)| v)
            .filter(crate::agents::Py::truthy)
            .map(|v| ProbeValue::Str(v.py_str())))
    })
}

/// What `<binary> --version` says, raising what `agents._version_of` lets through: a
/// NUL in the name (ValueError) and output that is not UTF-8
/// (UnicodeDecodeError).
pub fn binary_version_try(binary: &str) -> TryProbe {
    let binary = binary.to_string();
    Arc::new(move || {
        if binary.contains('\0') {
            return Err("ValueError: embedded null byte".into());
        }
        let v = crate::agents::try_version_of(&binary)?;
        Ok(if v.is_empty() { None } else { Some(ProbeValue::Str(v)) })
    })
}

/// `Path(path)`: "" is ".", and empty and "." components are dropped (so a
/// trailing "/" or "/." goes); a leading "//" (exactly two) is kept.
fn py_path(path: &str) -> String {
    let root = if path.starts_with("//") && !path.starts_with("///") {
        "//"
    } else if path.starts_with('/') {
        "/"
    } else {
        ""
    };
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty() && *p != ".").collect();
    if root.is_empty() && parts.is_empty() {
        return ".".to_string();
    }
    format!("{root}{}", parts.join("/"))
}

/// Size and mtime: an AppImage swapped in place changes both. Err where
/// the Python raises.
pub fn file_identity_try(path: &str) -> TryProbe {
    let path = path.to_string();
    Arc::new(move || {
        use std::os::unix::fs::MetadataExt;
        if path.contains('\0') {
            // ValueError, which `except OSError` does not catch.
            return Err("ValueError: stat: embedded null character in path".into());
        }
        let Ok(st) = std::fs::metadata(py_path(&path)) else {
            return Ok(None);
        };
        // st_mtime is the float sec + nsec * 1e-9, and int() truncates it, so
        // a time a nanosecond short of a second can round up to it.
        let mtime = (st.mtime() as f64 + st.mtime_nsec() as f64 * 1e-9).trunc() as i64;
        Ok(Some(ProbeValue::Identity(st.size(), mtime)))
    })
}

/// Every repository and AUR package with an update waiting, or None when
/// either check failed: an empty set from a check that could not run would
/// read as every update installed.
pub fn pending_updates() -> Probe {
    Arc::new(|| {
        let mut names: BTreeSet<String> = crate::inventory::checked_pending_repo()?.into_keys().collect();
        if std::path::Path::new(crate::inventory::YAY).exists() {
            names.extend(crate::inventory::checked_pending_aur()?.into_keys());
        }
        Some(ProbeValue::Set(names))
    })
}

/// An `inventory.App` as the Python holds it: every field whatever value it
/// was given (the dataclass annotations are not enforced), so a None or a
/// non-dict `extra` behaves as it does there.
#[derive(Debug, Clone)]
struct PyApp {
    name: Py,
    version: Py,
    source: Py,
    update: Py,
    exec_path: Py,
    extra: Py,
}

impl PyApp {
    fn of(app: &crate::inventory::App) -> PyApp {
        PyApp {
            name: Py::Str(app.name.clone()),
            version: Py::Str(app.version.clone()),
            source: Py::Str(app.source.clone()),
            update: Py::Str(app.update.clone()),
            exec_path: Py::Str(app.exec_path.clone()),
            extra: Py::plain_dict(&app.extra),
        }
    }

}

/// `extra.get(key)`: a non-dict `extra` has no `.get`.
fn extra_get<'a>(extra: &'a Py, key: &str) -> Result<Option<&'a Py>, String> {
    match extra {
        Py::Dict(d) => Ok(d.iter().find(|(k, _)| k == key).map(|(_, v)| v)),
        other => Err(format!("AttributeError: '{}' object has no attribute 'get'", other.type_name())),
    }
}

fn for_app_py(app: &PyApp) -> Result<Option<TryProbe>, String> {
    use crate::inventory;
    // `app.extra or {}`
    let empty = Py::Dict(Vec::new());
    let extra = if app.extra.truthy() { &app.extra } else { &empty };
    let is = |v: &Py, s: &str| matches!(v, Py::Str(x) if x == s);
    if [inventory::REPO, inventory::AUR, inventory::SYSTEM, inventory::ORPHAN]
        .iter()
        .any(|s| is(&app.source, s))
    {
        return Ok(Some(match subprocess_arg(&app.name) {
            Ok(name) => installed_package_try(&name),
            Err(e) => raising(e),
        }));
    }
    if is(&app.source, inventory::LOCAL) {
        if let Some(repo) = extra_get(extra, "repo")?.filter(|r| r.truthy()) {
            return Ok(Some(match subprocess_arg(repo) {
                Ok(repo) => git_head_try(&repo),
                Err(e) => raising(e),
            }));
        }
    }
    if is(&app.source, inventory::APPIMAGE) && app.exec_path.truthy() {
        return Ok(Some(match &app.exec_path {
            Py::Str(p) => file_identity_try(p),
            other => raising(format!(
                "TypeError: argument should be a str or an os.PathLike object where __fspath__ returns a str, not '{}'",
                other.type_name()
            )),
        }));
    }
    if extra_get(extra, "manager")?.is_some_and(|m| is(m, "mise")) {
        // `extra.get("key") or app.name`
        let key = extra_get(extra, "key")?.filter(|k| k.truthy()).unwrap_or(&app.name);
        return Ok(Some(match key {
            Py::Str(k) => {
                mise_version_try(k)
            }
            // `_mise_installed().get(key)`: a list, set or dict is
            // unhashable; any other non-string is simply not one of its keys.
            k @ (Py::List(_) | Py::Set(_) | Py::Dict(_)) => {
                raising(format!("TypeError: unhashable type: '{}'", k.type_name()))
            }
            _ => Arc::new(|| Ok(None)),
        }));
    }
    if let Some(binary) = extra_get(extra, "binary")?.filter(|b| b.truthy()) {
        return Ok(Some(match subprocess_arg(binary) {
            Ok(binary) => binary_version_try(&binary),
            Err(e) => raising(e),
        }));
    }
    Ok(None)
}

/// The probe that answers "did this app's update land?", raising what the
/// Python raises.
pub fn for_app_try(app: &crate::inventory::App) -> Option<TryProbe> {
    for_app_py(&PyApp::of(app)).ok().flatten()
}

// ---- judging -----------------------------------------------------------------

/// What `judge_app` needs of a probe answer: Python `==`, and the text when
/// it is a `str`.
trait Answer {
    fn same(&self, other: &Self) -> bool;
    fn text(&self) -> Option<&str>;
}

impl Answer for ProbeValue {
    fn same(&self, other: &Self) -> bool {
        self == other
    }
    fn text(&self) -> Option<&str> {
        match self {
            ProbeValue::Str(s) => Some(s),
            _ => None,
        }
    }
}

/// Compare an app before and after its update.
pub fn judge_app(
    app: &crate::inventory::App,
    before: Option<&ProbeValue>,
    after: Option<&ProbeValue>,
) -> Outcome {
    // A typed App's update is a str, so judging cannot raise.
    judge_app_any(&PyApp::of(app), before, after)
        .unwrap_or_else(|_| Outcome::new(None, format!("{}: the update ran, but its result could not be checked", app.name)))
}

fn judge_app_any<T: Answer>(app: &PyApp, before: Option<&T>, after: Option<&T>) -> Result<Outcome, String> {
    let name = app.name.py_str();
    let (Some(before), Some(after)) = (before, after) else {
        return Ok(Outcome::new(None, format!("{name}: the update ran, but its result could not be checked")));
    };
    if before.same(after) {
        return Ok(Outcome::new(
            Some(false),
            format!("{name} did not change — the update ran, but {} is still installed", app.version.py_str()),
        ));
    }
    if let Some(after) = after.text() {
        if app.update.truthy() {
            // `after != app.update`: a str is never equal to a non-str, whose
            // `.split` then raises.
            let update = match &app.update {
                Py::Str(u) => u.as_str(),
                other => {
                    return Err(format!("AttributeError: '{}' object has no attribute 'split'", other.type_name()))
                }
            };
            let stem = update.split('-').next().unwrap_or("");
            if after != update && !after.starts_with(stem) {
                return Ok(Outcome::new(Some(true), format!("{name} is now {after} ({update} was expected)")));
            }
        }
        // A git HEAD or a file identity is not something to show a person.
        if after.chars().count() < 24 {
            return Ok(Outcome::new(Some(true), format!("{name} {} → {after}", app.version.py_str())));
        }
    }
    Ok(Outcome::new(Some(true), format!("{name} updated")))
}

/// Compare the pending-update set around a full upgrade (both sides what
/// `pending_updates` answers); Err where the Python raises (TypeError on an
/// answer that is not a set).
pub fn try_judge_pending(before: Option<&ProbeValue>, after: Option<&ProbeValue>) -> Result<Outcome, String> {
    if let (Some(ProbeValue::Set(before)), Some(ProbeValue::Set(after))) = (before, after) {
        return Ok(judge_pending_sets(before, after));
    }
    judge_pending_py(before.map(Py::of).as_ref(), after.map(Py::of).as_ref())
}

fn judge_pending_sets(before: &BTreeSet<String>, after: &BTreeSet<String>) -> Outcome {
    let done: Vec<&String> = before.difference(after).collect();
    let left: Vec<&String> = before.intersection(after).collect();
    if before.is_empty() {
        return Outcome::new(None, "There was nothing to update");
    }
    if done.is_empty() {
        return Outcome::new(
            Some(false),
            format!("Nothing was updated — all {} update(s) are still waiting", before.len()),
        );
    }
    if !left.is_empty() {
        // BTreeSet order is code-point order, the same as Python's sorted().
        let mut names = left.iter().take(4).map(|s| s.as_str()).collect::<Vec<_>>().join(", ");
        if left.len() > 4 {
            names.push('…');
        }
        return Outcome::new(
            Some(true),
            format!("{} updated · {} still waiting: {}", done.len(), left.len(), names),
        );
    }
    Outcome::new(Some(true), format!("All {} update(s) installed", done.len()))
}

/// A `maintenance.Finding` as the Python holds it: each field whatever value
/// it was given (an int `size_mb` stays an exact int, a None `count` stays
/// None).
#[derive(Debug, Clone)]
struct PyFinding {
    title: Py,
    detail: Py,
    severity: Py,
    count: Py,
    size_mb: Py,
    fix: Py,
}

impl PyFinding {
    fn of(f: &crate::maintenance::Finding) -> PyFinding {
        PyFinding {
            title: Py::Str(f.title.clone()),
            detail: Py::Str(f.detail.clone()),
            severity: Py::Str(f.severity.clone()),
            count: Py::Int(f.count as i128),
            size_mb: Py::Float(f.size_mb),
            // Only whether there is one matters here.
            fix: if f.fix.is_some() { Py::Bool(true) } else { Py::None },
        }
    }

    #[cfg(test)]
    /// `None if d is None else Finding(**d)`.
    fn from_json(v: Option<&serde_json::Value>) -> Result<Option<PyFinding>, String> {
        let d = match v {
            None | Some(serde_json::Value::Null) => return Ok(None),
            Some(serde_json::Value::Object(d)) => d,
            Some(_) => return Err("TypeError: maintenance.Finding() argument after ** must be a mapping".into()),
        };
        const FIELDS: [&str; 11] = [
            "id", "title", "detail", "severity", "count", "size_mb", "fix_label", "fix", "fix_note", "items", "url",
        ];
        if let Some(k) = d.keys().find(|k| !FIELDS.contains(&k.as_str())) {
            return Err(format!("TypeError: Finding.__init__() got an unexpected keyword argument '{k}'"));
        }
        let missing: Vec<&str> = FIELDS[..3].iter().copied().filter(|k| !d.contains_key(*k)).collect();
        if !missing.is_empty() {
            return Err(format!("TypeError: Finding.__init__() missing required argument(s): {}", missing.join(", ")));
        }
        let get = |k: &str, default: Py| d.get(k).map(Py::plain).unwrap_or(default);
        Ok(Some(PyFinding {
            title: get("title", Py::None),
            detail: get("detail", Py::None),
            severity: get("severity", Py::Str(crate::maintenance::INFO.into())),
            count: get("count", Py::Int(0)),
            size_mb: get("size_mb", Py::Float(0.0)),
            fix: get("fix", Py::None),
        }))
    }

    fn actionable(&self) -> bool {
        !matches!(self.fix, Py::None) && !py_eq(&self.severity, &Py::Str(crate::maintenance::OK.into()))
    }
}

/// Compare a health finding before and after its fix ran.
pub fn judge_finding(
    before: &crate::maintenance::Finding,
    after: Option<&crate::maintenance::Finding>,
) -> Outcome {
    // Typed findings hold numbers where numbers belong, so this cannot raise.
    let after = after.map(PyFinding::of);
    judge_finding_any(Some(&PyFinding::of(before)), after.as_ref())
        .unwrap_or_else(|_| Outcome::new(None, format!("{}: done, but the check could not be run again", before.title)))
}

fn judge_finding_any(before: Option<&PyFinding>, after: Option<&PyFinding>) -> Result<Outcome, String> {
    use crate::maintenance as m;
    let attr = |name: &str| format!("AttributeError: 'NoneType' object has no attribute '{name}'");
    let Some(after) = after else {
        let before = before.ok_or_else(|| attr("title"))?;
        return Ok(Outcome::new(None, format!("{}: done, but the check could not be run again", before.title.py_str())));
    };
    let before = before.ok_or_else(|| attr("size_mb"))?;
    // Python numbers: an int size stays exact, so 2**53 + 1 - 2**53 is 1.
    let freed = py_sub(&before.size_mb, &after.size_mb)?;
    let title = before.title.py_str();
    if py_eq(&after.severity, &Py::Str(m::OK.into())) || !after.actionable() {
        let tail = if py_ge1(&freed) { format!(" — freed {}", m::fmt_size(freed.as_f64())) } else { String::new() };
        return Ok(Outcome::new(Some(true), format!("{title}: resolved{tail}")));
    }
    if py_ge1(&freed) {
        return Ok(Outcome::new(
            Some(true),
            format!("{title}: freed {}, but it still needs attention", m::fmt_size(freed.as_f64())),
        ));
    }
    if py_eq(&after.severity, &before.severity)
        && py_eq(&after.count, &before.count)
        && py_abs_lt1(&py_sub(&after.size_mb, &before.size_mb)?)
    {
        return Ok(Outcome::new(
            Some(false),
            format!("{title}: the fix ran but nothing changed — {}", after.detail.py_str()),
        ));
    }
    Ok(Outcome::new(Some(true), format!("{title}: changed, but it still needs attention")))
}

/// `recheck` with "the check raised" kept apart from "no such check", so the
/// parity hook can tell a check that is still a stub from a real `None`.
fn recheck_inner(finding_id: &str) -> Result<Option<crate::maintenance::Finding>, ()> {
    let Some(f) = crate::maintenance::check_by_id(finding_id) else {
        return Ok(None);
    };
    // `except Exception`: a Rust check cannot raise, so a panic is the only
    // failure there is to catch.
    std::panic::catch_unwind(f).map(Some).map_err(|_| ())
}

/// Run the one health check that produced `finding_id` again.
pub fn recheck(finding_id: &str) -> Option<crate::maintenance::Finding> {
    recheck_inner(finding_id).ok().flatten()
}

// ---- Python values ProbeValue and the typed structs cannot hold -------------

/// How many containers deep an app's `extra` value is converted. An agent's
/// row can carry a value as deep as Python's json reads (52 000 levels, from
/// `mise outdated --json`), and the conversion and this `Py`'s drop recurse
/// once per level, on whatever thread judges the update (the main one
/// included). The judging reads only an extra value's type, truthiness and
/// text.
const EXTRA_DEPTH: usize = 64;

/// A Python value as the parity cases build it from JSON, for the comparable
/// answers `ProbeValue` has no variant for (a dict of ints, a tuple of
/// floats, a set of numbers) and for dataclass fields holding something other
/// than their annotation. No Rust probe produces these; the judging is still
/// Python's.
#[derive(Debug, Clone)]
enum Py {
    None,
    Bool(bool),
    Int(i128),
    Float(f64),
    Str(String),
    List(Vec<Py>),
    Tuple(Vec<Py>),
    Set(Vec<Py>),
    Dict(Vec<(String, Py)>),
}

impl Py {
    /// A dict of JSON values (an app's `extra`) as `json.loads` gives it,
    /// down to [`EXTRA_DEPTH`] levels.
    fn plain_dict(m: &serde_json::Map<String, serde_json::Value>) -> Py {
        Py::Dict(m.iter().map(|(k, v)| (k.clone(), Py::plain_to(v, EXTRA_DEPTH))).collect())
    }

    /// A JSON value as `json.loads` gives it.
    #[cfg(test)]
    fn plain(v: &serde_json::Value) -> Py {
        Py::plain_to(v, usize::MAX)
    }

    /// A JSON value as `json.loads` gives it, down to `depth` containers: one
    /// deeper keeps its type and whether it is empty (a `None` stands in for
    /// what it held), which is all the judging reads of an `extra` value.
    fn plain_to(v: &serde_json::Value, depth: usize) -> Py {
        use serde_json::Value;
        match v {
            Value::Array(a) if depth == 0 => Py::List(a.iter().take(1).map(|_| Py::None).collect()),
            Value::Object(o) if depth == 0 => Py::Dict(o.keys().take(1).map(|k| (k.clone(), Py::None)).collect()),
            Value::Null => Py::None,
            Value::Bool(b) => Py::Bool(*b),
            Value::Number(n) => match (n.as_i64(), n.as_u64()) {
                (Some(i), _) => Py::Int(i as i128),
                (_, Some(u)) => Py::Int(u as i128),
                _ => {
                    let f = n.as_f64().unwrap_or(f64::NAN);
                    // serde_json reads an integer literal beyond 64 bits as
                    // an f64; a whole number that large is taken to be the
                    // Python int it was written from.
                    if f.fract() == 0.0 && f.abs() >= 18446744073709551616.0 && f.abs() < 1.7e38 {
                        Py::Int(f as i128)
                    } else {
                        Py::Float(f)
                    }
                }
            },
            Value::String(s) => Py::Str(s.clone()),
            Value::Array(a) => Py::List(a.iter().map(|x| Py::plain_to(x, depth - 1)).collect()),
            Value::Object(o) => Py::Dict(o.iter().map(|(k, x)| (k.clone(), Py::plain_to(x, depth - 1))).collect()),
        }
    }

    /// A probe answer as the Python value it stands for.
    fn of(v: &ProbeValue) -> Py {
        match v {
            ProbeValue::Str(s) => Py::Str(s.clone()),
            ProbeValue::Identity(size, mtime) => Py::Tuple(vec![Py::Int(*size as i128), Py::Int(*mtime as i128)]),
            ProbeValue::Set(s) => Py::Set(s.iter().map(|x| Py::Str(x.clone())).collect()),
            ProbeValue::Bool(b) => Py::Bool(*b),
            ProbeValue::Map(m) => Py::Dict(
                m.iter()
                    .map(|(k, v)| (k.clone(), v.as_ref().map(|s| Py::Str(s.clone())).unwrap_or(Py::None)))
                    .collect(),
            ),
        }
    }

    fn type_name(&self) -> &'static str {
        match self {
            Py::None => "NoneType",
            Py::Bool(_) => "bool",
            Py::Int(_) => "int",
            Py::Float(_) => "float",
            Py::Str(_) => "str",
            Py::List(_) => "list",
            Py::Tuple(_) => "tuple",
            Py::Set(_) => "set",
            Py::Dict(_) => "dict",
        }
    }

    /// `bool(v)`.
    fn truthy(&self) -> bool {
        match self {
            Py::None => false,
            Py::Bool(b) => *b,
            Py::Int(i) => *i != 0,
            Py::Float(f) => *f != 0.0,
            Py::Str(s) => !s.is_empty(),
            Py::List(a) | Py::Tuple(a) | Py::Set(a) => !a.is_empty(),
            Py::Dict(d) => !d.is_empty(),
        }
    }

    /// `str(v)`, as an f-string shows it.
    fn py_str(&self) -> String {
        match self {
            Py::Str(s) => s.clone(),
            other => other.py_repr(),
        }
    }

    /// `repr(v)`.
    fn py_repr(&self) -> String {
        let join = |a: &[Py]| a.iter().map(|x| x.py_repr()).collect::<Vec<_>>().join(", ");
        match self {
            Py::None => "None".into(),
            Py::Bool(b) => if *b { "True" } else { "False" }.into(),
            Py::Int(i) => i.to_string(),
            Py::Float(f) if f.is_nan() => "nan".into(),
            Py::Float(f) if f.is_infinite() => if *f > 0.0 { "inf" } else { "-inf" }.into(),
            Py::Float(f) => crate::util::py_float_repr(*f),
            Py::Str(s) => str_repr(s),
            Py::List(a) => format!("[{}]", join(a)),
            Py::Tuple(a) if a.len() == 1 => format!("({},)", a[0].py_repr()),
            Py::Tuple(a) => format!("({})", join(a)),
            Py::Set(a) if a.is_empty() => "set()".into(),
            Py::Set(a) => format!("{{{}}}", join(a)),
            Py::Dict(d) => format!(
                "{{{}}}",
                d.iter().map(|(k, v)| format!("{}: {}", str_repr(k), v.py_repr())).collect::<Vec<_>>().join(", ")
            ),
        }
    }


    fn number(&self) -> Option<Result<i128, f64>> {
        match self {
            Py::Bool(b) => Some(Ok(*b as i128)),
            Py::Int(i) => Some(Ok(*i)),
            Py::Float(f) => Some(Err(*f)),
            _ => None,
        }
    }

    /// `float(v)` of a number (0.0 for anything else, which py_sub never
    /// gives).
    fn as_f64(&self) -> f64 {
        match self.number() {
            Some(Ok(i)) => i as f64,
            Some(Err(f)) => f,
            None => 0.0,
        }
    }
}

/// `repr(str)`: single quotes unless the text has one and no double quote.
fn str_repr(s: &str) -> String {
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
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(q);
    out
}

/// Python `a - b` on numbers: int with int stays an exact int.
fn py_sub(a: &Py, b: &Py) -> Result<Py, String> {
    match (a.number(), b.number()) {
        (Some(Ok(x)), Some(Ok(y))) => Ok(x.checked_sub(y).map(Py::Int).unwrap_or(Py::Float(x as f64 - y as f64))),
        (Some(_), Some(_)) => Ok(Py::Float(a.as_f64() - b.as_f64())),
        _ => Err(format!(
            "TypeError: unsupported operand type(s) for -: '{}' and '{}'",
            a.type_name(),
            b.type_name()
        )),
    }
}

/// `n >= 1` of a number.
fn py_ge1(n: &Py) -> bool {
    match n.number() {
        Some(Ok(i)) => i >= 1,
        Some(Err(f)) => f >= 1.0,
        None => false,
    }
}

/// `abs(n) < 1` of a number.
fn py_abs_lt1(n: &Py) -> bool {
    match n.number() {
        Some(Ok(i)) => i.abs() < 1,
        Some(Err(f)) => f.abs() < 1.0,
        None => false,
    }
}

/// Python `==` (True == 1 == 1.0; a list is never equal to a tuple).
fn py_eq(a: &Py, b: &Py) -> bool {
    if let (Some(x), Some(y)) = (a.number(), b.number()) {
        return match (x, y) {
            (Ok(x), Ok(y)) => x == y,
            (Ok(i), Err(f)) | (Err(f), Ok(i)) => i as f64 == f,
            (Err(x), Err(y)) => x == y,
        };
    }
    match (a, b) {
        (Py::None, Py::None) => true,
        (Py::Str(x), Py::Str(y)) => x == y,
        (Py::List(x), Py::List(y)) | (Py::Tuple(x), Py::Tuple(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| py_eq(p, q))
        }
        (Py::Set(x), Py::Set(y)) => x.len() == y.len() && x.iter().all(|p| y.iter().any(|q| py_eq(p, q))),
        (Py::Dict(x), Py::Dict(y)) => {
            x.len() == y.len()
                && x.iter().all(|(k, p)| y.iter().any(|(j, q)| k == j && py_eq(p, q)))
        }
        _ => false,
    }
}

impl Answer for Py {
    fn same(&self, other: &Self) -> bool {
        py_eq(self, other)
    }
    fn text(&self) -> Option<&str> {
        match self {
            Py::Str(s) => Some(s),
            _ => None,
        }
    }
}

/// `judge_pending` over any Python values: sets of any hashable values, and
/// the TypeError `before - after` / `len()` raise on anything else.
fn judge_pending_py(before: Option<&Py>, after: Option<&Py>) -> Result<Outcome, String> {
    let (Some(before), Some(after)) = (before, after) else {
        return Ok(Outcome::new(None, "The update finished; what it installed could not be checked"));
    };
    let (before, after) = match (before, after) {
        (Py::Set(b), Py::Set(a)) => (b, a),
        (b, a) => {
            let unsupported = |op: &str| {
                format!("TypeError: unsupported operand type(s) for {op}: '{}' and '{}'", b.type_name(), a.type_name())
            };
            // Only numbers subtract; only ints (and bools) also `&`.
            match (b.number(), a.number()) {
                (Some(Ok(_)), Some(Ok(_))) => {}
                (Some(_), Some(_)) => return Err(unsupported("&")),
                _ => return Err(unsupported("-")),
            }
            if !b.truthy() {
                return Ok(Outcome::new(None, "There was nothing to update"));
            }
            // Every remaining branch takes len() or sorted() of a number.
            return Err(format!("TypeError: object of type '{}' has no len()", b.type_name()));
        }
    };
    let done: Vec<&Py> = before.iter().filter(|x| !after.iter().any(|y| py_eq(x, y))).collect();
    let mut left: Vec<&Py> = before.iter().filter(|x| after.iter().any(|y| py_eq(x, y))).collect();
    if before.is_empty() {
        return Ok(Outcome::new(None, "There was nothing to update"));
    }
    if done.is_empty() {
        return Ok(Outcome::new(
            Some(false),
            format!("Nothing was updated — all {} update(s) are still waiting", before.len()),
        ));
    }
    if !left.is_empty() {
        // sorted(left): only str with str, or numbers with numbers.
        if left.iter().all(|x| matches!(x, Py::Str(_))) {
            left.sort_by(|a, b| a.text().cmp(&b.text()));
        } else if left.iter().all(|x| x.number().is_some()) {
            left.sort_by(|a, b| a.as_f64().partial_cmp(&b.as_f64()).unwrap_or(std::cmp::Ordering::Equal));
        } else if left.len() > 1 {
            return Err("TypeError: '<' not supported between these instances".into());
        }
        let mut names = Vec::new();
        for (i, x) in left.iter().take(4).enumerate() {
            match x {
                Py::Str(s) => names.push(s.as_str()),
                other => {
                    return Err(format!(
                        "TypeError: sequence item {i}: expected str instance, {} found",
                        other.type_name()
                    ))
                }
            }
        }
        let mut names = names.join(", ");
        if left.len() > 4 {
            names.push('…');
        }
        return Ok(Outcome::new(
            Some(true),
            format!("{} updated · {} still waiting: {}", done.len(), left.len(), names),
        ));
    }
    Ok(Outcome::new(Some(true), format!("All {} update(s) installed", done.len())))
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::inventory;
    use crate::maintenance as m;

    fn s(v: &str) -> ProbeValue {
        ProbeValue::Str(v.to_string())
    }

    fn set(v: &[&str]) -> ProbeValue {
        ProbeValue::Set(v.iter().map(|x| x.to_string()).collect())
    }

    #[test]
    fn deep_extra_values_are_judged_on_a_small_stack() {
        // An agent row's extra can hold a value as deep as Python's json
        // reads; judging it reads only the top, on whatever thread it runs.
        let mut deep = serde_json::Value::Null;
        for _ in 0..52_000 {
            deep = serde_json::Value::Array(vec![deep]);
        }
        let mut app = inventory::App { update: "2.0".into(), ..inventory::App::new("tool", "1.0", "mise") };
        app.extra.insert("binary".into(), deep);
        std::thread::Builder::new()
            .stack_size(2 << 20)
            .spawn(move || {
                let probe = for_app_try(&app).expect("a probe");
                assert_eq!(
                    probe().err().as_deref(),
                    Some("TypeError: expected str, bytes or os.PathLike object, not list")
                );
                let judged = judge_app(&app, Some(&s("1.0")), Some(&s("2.0")));
                assert_eq!(judged.changed, Some(true));
            })
            .unwrap()
            .join()
            .unwrap();
    }

    // class ActionsAreVerified: a command that exits 0 and changes nothing is
    // reported as that.

    #[test]
    fn test_unchanged_version_is_not_success() {
        let app = inventory::App { update: "2.0".into(), ..inventory::App::new("yaru", "1.0", inventory::REPO) };
        let out = judge_app(&app, Some(&s("1.0")), Some(&s("1.0")));
        assert_eq!(out.changed, Some(false));
        assert!(out.message.contains("did not change"));
    }

    #[test]
    fn test_changed_version_is_success() {
        let app = inventory::App { update: "2.0".into(), ..inventory::App::new("yaru", "1.0", inventory::REPO) };
        assert!(judge_app(&app, Some(&s("1.0")), Some(&s("2.0"))).ok());
    }

    #[test]
    fn test_an_unanswerable_probe_is_not_called_success() {
        let app = inventory::App::new("x", "1", inventory::REPO);
        assert_eq!(judge_app(&app, None, Some(&s("2"))).changed, None);
    }

    #[test]
    fn test_full_upgrade_reports_what_is_still_waiting() {
        let out = try_judge_pending(Some(&set(&["a", "b", "c"])), Some(&set(&["c"]))).unwrap();
        assert!(out.ok());
        assert!(out.message.contains('c'));
        assert_eq!(try_judge_pending(Some(&set(&["a"])), Some(&set(&["a"]))).unwrap().changed, Some(false));
    }

    #[test]
    fn test_a_fix_that_freed_nothing_says_so() {
        let mk = |detail: &str| m::Finding {
            id: "journal".into(),
            title: "Journal".into(),
            detail: detail.into(),
            severity: m::WARN.into(),
            size_mb: 500.0,
            fix: Some(m::Fix::argv(&["true"])),
            ..Default::default()
        };
        let before = mk("1 GB");
        let after = mk("1 GB");
        assert_eq!(judge_finding(&before, Some(&after)).changed, Some(false));
        let fixed = m::Finding {
            id: "journal".into(),
            title: "Journal".into(),
            detail: "ok".into(),
            severity: m::OK.into(),
            ..Default::default()
        };
        assert!(judge_finding(&before, Some(&fixed)).ok());
    }

    #[test]
    fn test_an_empty_path_is_the_working_directory() {
        // Path("") is "." in Python.
        assert_eq!(file_identity_try("")(), file_identity_try(".")());
        assert!(file_identity_try("")().unwrap().is_some());
    }
    #[test]
    fn test_out_is_python_text_mode() {
        assert_eq!(out(&["printf", "a\\r\\nb\\rc"], 30.0), Ok("a\nb\nc".to_string()));
        assert_eq!(out(&["printf", "\\037hi\\034"], 30.0), Ok("hi".to_string()));
        assert!(out(&["printf", "\\377ok"], 30.0).is_err());
        assert!(out(&[], 30.0).is_err());
        assert_eq!(out(&["/nonexistent/binary"], 30.0), Ok(String::new()));
    }

    #[test]
    fn test_out_waits_for_eof_within_the_timeout() {
        let t = std::time::Instant::now();
        assert_eq!(out(&["sh", "-c", "echo hi; sleep 3 &"], 1.0), Ok(String::new()));
        assert!(t.elapsed() < std::time::Duration::from_millis(2500));
    }

    #[test]
    fn test_path_is_normalised_like_pathlib() {
        assert_eq!(py_path(""), ".");
        assert_eq!(py_path("a/b/"), "a/b");
        assert_eq!(py_path("a/./b/."), "a/b");
        assert_eq!(py_path("//a//b"), "//a/b");
        assert_eq!(py_path("///a"), "/a");
        assert_eq!(py_path("a/../b"), "a/../b");
        assert!(file_identity_try("a\0b")().is_err());
    }

    #[test]
    fn test_decode_errors_are_worded_like_python() {
        let e = |b: &[u8]| py_text(b.to_vec()).unwrap_err();
        assert!(e(b"\xffok").ends_with("can't decode byte 0xff in position 0: invalid start byte"));
        assert!(e(b"a\xc3").ends_with("can't decode byte 0xc3 in position 1: unexpected end of data"));
        assert!(e(b"a\xe2\x82").ends_with("can't decode bytes in position 1-2: unexpected end of data"));
        assert!(e(b"\xe2\x28\xa1").ends_with("can't decode byte 0xe2 in position 0: invalid continuation byte"));
        assert!(e(b"\xf0\x90\x28").ends_with("can't decode bytes in position 0-1: invalid continuation byte"));
    }

    #[test]
    fn test_finding_sizes_are_python_numbers() {
        let f = |v: serde_json::Value| PyFinding::from_json(Some(&v)).unwrap();
        let big = 9007199254740993_i64;
        let b = f(serde_json::json!({"id": "j", "title": "Journal", "detail": "", "severity": "warn",
                                     "size_mb": big, "fix": ["true"]}));
        let a = f(serde_json::json!({"id": "j", "title": "Journal", "detail": "", "severity": "ok",
                                     "size_mb": big - 1}));
        let out = judge_finding_any(b.as_ref(), a.as_ref()).unwrap();
        assert_eq!(out.message, "Journal: resolved — freed 1 MB");
        assert!(judge_finding_any(None, None).is_err());
    }

    #[test]
    fn test_binary_version_raises_on_nul() {
        assert!(binary_version_try("a\0b")().is_err());
    }
}
