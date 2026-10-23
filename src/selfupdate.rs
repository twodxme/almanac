//! Port of `selfupdate.py`.
//!
//! Almanac updating Almanac.
//!
//! An app whose whole purpose is telling you what is out of date should not be
//! the one thing on the machine that quietly isn't. It appears in its own Updates
//! list like anything else.
//!
//! How it updates depends on how it was installed, and that is detected rather
//! than assumed:
//!
//! * **git checkout** -- the normal case. `git fetch` then a fast-forward pull.
//!   --ff-only is the point: if local edits or a diverged history would make the
//!   pull a merge, it refuses and says so instead of creating a conflict inside a
//!   running application.
//! * **a distribution package** -- pacman owns it, so this stays out of the way
//!   and lets the package appear in the ordinary update list.
//! * **neither** -- a plain directory someone copied. There is nothing to pull,
//!   so it says that rather than inventing a mechanism.
//!
//! The running process is never swapped underneath itself: new code takes effect
//! on the next launch, and the app offers to restart rather than pretending the
//! update already applied.
//!
//! Where the Python raises past its own `except` clauses (a non-UTF-8 byte in
//! git's output under `text=True`, an encoding ElementTree cannot look up, a
//! description nested deeper than the recursion limit), the `try_*` functions
//! return `Err` with the exception. The plain `check`, `as_app` and
//! `release_notes` keep their old signatures only for the callers in other
//! modules, and are deprecated: they cannot stop where the Python stops.

use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use num_bigint::BigInt;
use serde::{Deserialize, Serialize};

use crate::util;

/// The Almanac checkout this binary was built in, if it runs from one.
///
/// Found at run time: a build in a checkout lives under
/// `<checkout>/target/...`, so the checkout is the nearest ancestor of the
/// running binary that holds both `Cargo.toml` and `.git`. A binary that is
/// not in a checkout (an installed package) falls back to where it was built,
/// which `compiled_check_in` only trusts when the binary lives inside it.
/// Symlinks are resolved.
pub static ROOT: LazyLock<PathBuf> = LazyLock::new(|| {
    let from_exe = std::env::current_exe().ok().map(|e| py_resolve(&e)).and_then(|exe| {
        exe.ancestors()
            .skip(1)
            .find(|d| d.join("Cargo.toml").is_file() && d.join(".git").exists())
            .map(PathBuf::from)
    });
    let root = from_exe.unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    PathBuf::from(py_path(py_resolve(&root).as_os_str()))
});

/// `Path.resolve()` (strict=False): symlinks resolved as far as the path
/// exists, the missing rest appended as written.
fn py_resolve(p: &Path) -> PathBuf {
    if let Ok(real) = std::fs::canonicalize(p) {
        return real;
    }
    match (p.parent(), p.file_name()) {
        (Some(parent), Some(name)) if !parent.as_os_str().is_empty() => py_resolve(parent).join(name),
        _ => p.to_path_buf(),
    }
}

pub const NAME: &str = "Almanac";
/// The release version, from Cargo.toml. git describe supersedes it in a
/// checkout, so this is what a package or tarball install reports. The
/// shipped metainfo's newest release must match it (a test checks), so the
/// "What's New" window always describes the version that is running.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub const GIT: &str = "git";
pub const PACKAGE: &str = "package";
pub const PLAIN: &str = "plain";

pub const METAINFO: &str = "io.github.twodxme.Almanac.metainfo.xml";

/// What an update would consist of (the dict `check` returns).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Info {
    /// One of `GIT`, `PACKAGE`, `PLAIN`.
    pub kind: String,
    pub version: String,
    /// A Python int: whatever `int()` made of git's count, however large.
    #[serde(with = "py_int_json")]
    pub behind: BigInt,
    pub latest: String,
    pub note: String,
    pub can_update: bool,
    pub remote: String,
    pub dirty: bool,
}

/// (version, AppStream description markup) of the newest release.
///
/// Read from the same AppStream file software centres show, so the About
/// window's "What's New" never disagrees with them. A checkout has it under
/// docs/; a package installs it to the system metainfo directory.
///
/// Where the Python raises, this cannot: the exception goes to stderr (as an
/// uncaught one would) and the answer is None. Callers that must stop where
/// the Python stops use `try_release_notes`.
#[deprecated(note = "the Python raises here; use try_release_notes and stop on Err")]
pub fn release_notes() -> Option<(String, String)> {
    try_release_notes().unwrap_or_else(|e| uncaught(e, None))
}

/// An exception the Python lets escape, reaching a caller that cannot take
/// one: printed, as the interpreter prints an uncaught traceback.
fn uncaught<T>(exc: String, fallback: T) -> T {
    eprintln!("{exc}");
    fallback
}

/// `release_notes`, with the exceptions ElementTree lets through as `Err`.
pub fn try_release_notes() -> Result<Option<(String, String)>, String> {
    release_notes_in(&ROOT)
}

/// `release_notes` with `ROOT` given (the parity cases point it at a temp dir).
///
/// On a thread of its own: `tostring` recurses once per level of the
/// description, to Python's recursion limit (about 1000), which a small
/// caller's stack cannot be trusted to hold.
pub fn release_notes_in(root: &Path) -> Result<Option<(String, String)>, String> {
    util::with_stack(util::DEEP_STACK, || release_notes_here(root))
}

fn release_notes_here(root: &Path) -> Result<Option<(String, String)>, String> {
    let paths = [
        py_path_join(root.as_os_str(), &format!("docs/{METAINFO}")),
        OsString::from(format!("/usr/share/metainfo/{METAINFO}")),
    ];
    for path in &paths {
        // except (OSError, ET.ParseError): continue
        let Ok(bytes) = std::fs::read(path) else { continue };
        let doc = match et::parse(&bytes) {
            Ok(doc) => doc,
            Err(et::Error::Parse) => continue,
            Err(et::Error::Raise(e)) => return Err(e),
        };
        let Some(release) = doc.find2("releases", "release") else { continue };
        let Some(desc) = release.find("description") else {
            return Ok(None);
        };
        // libadwaita takes the description's children as AppStream markup.
        let mut body = String::new();
        for child in &desc.children {
            body.push_str(&et::tostring(child)?);
        }
        let version = release.get("version").unwrap_or_default().to_string();
        return Ok(Some((version, py_split(&body).join(" "))));
    }
    Ok(None)
}

/// `_git`: run `git -C ROOT <args>`; (exit code, stdout+stderr stripped).
/// A failure to run git at all is (1, the error message); output that is not
/// UTF-8 is the UnicodeDecodeError `text=True` raises.
pub fn git(args: &[&str], timeout: u64) -> Result<(i32, String), String> {
    git_in(&ROOT, args, timeout)
}

/// `_git` with `ROOT` given.
pub fn git_in(root: &Path, args: &[&str], timeout: u64) -> Result<(i32, String), String> {
    let mut argv: Vec<OsString> = vec!["git".into(), "-C".into(), root.as_os_str().to_owned()];
    argv.extend(args.iter().map(OsString::from));
    match run_capture(&argv, Some(timeout)) {
        // except (OSError, subprocess.SubprocessError) as exc: return 1, str(exc)
        Err(RunError::Os(msg)) => Ok((1, msg)),
        Err(RunError::Raise(e)) => Err(e),
        Err(RunError::Timeout) => {
            let cmd: Vec<String> = argv.iter().map(|a| py_str_repr(&a.to_string_lossy())).collect();
            Ok((1, format!("Command '[{}]' timed out after {} seconds", cmd.join(", "), timeout)))
        }
        Ok(out) => {
            let text = decode_text(&out.stdout)? + &decode_text(&out.stderr)?;
            Ok((out.code, py_strip(&text).to_string()))
        }
    }
}

/// `pacman -Qoq` of the running binary: a package owns Almanac when it owns
/// the program that is running.
fn pacman_owner(_root: &Path) -> Result<Result<Captured, RunError>, String> {
    pacman_owner_of(running_exe().into_os_string())
}

/// `pacman -Qoq <path>`, run the way `pacman_owner` runs it.
fn pacman_owner_of(path: OsString) -> Result<Result<Captured, RunError>, String> {
    let argv: Vec<OsString> = vec!["pacman".into(), "-Qoq".into(), path];
    match run_capture(&argv, Some(20)) {
        Err(RunError::Raise(e)) => Err(e),
        other => Ok(other),
    }
}

/// How this copy was installed: `GIT`, `PACKAGE` or `PLAIN`.
pub fn install_kind() -> Result<&'static str, String> {
    install_kind_in(&ROOT)
}

pub fn install_kind_in(root: &Path) -> Result<&'static str, String> {
    let (code, _) = git_in(root, &["rev-parse", "--is-inside-work-tree"], 60)?;
    if code == 0 {
        return Ok(GIT);
    }
    // except (OSError, subprocess.SubprocessError): pass
    if let Ok(out) = pacman_owner(root)? {
        let stdout = decode_text(&out.stdout)?;
        decode_text(&out.stderr)?;
        if out.code == 0 && !py_strip(&stdout).is_empty() {
            return Ok(PACKAGE);
        }
    }
    Ok(PLAIN)
}

pub fn owning_package_in(root: &Path) -> Result<String, String> {
    match pacman_owner(root)? {
        Ok(out) => {
            let stdout = decode_text(&out.stdout)?;
            decode_text(&out.stderr)?;
            Ok(py_strip(&stdout).to_string())
        }
        Err(_) => Ok(String::new()),
    }
}

/// What is running, preferring what git actually says over the constant.
pub fn version() -> Result<String, String> {
    version_in(&ROOT)
}

pub fn version_in(root: &Path) -> Result<String, String> {
    if install_kind_in(root)? == GIT {
        let (code, described) = git_in(root, &["describe", "--tags", "--always", "--dirty"], 60)?;
        if code == 0 && !described.is_empty() {
            return Ok(described);
        }
    }
    Ok(VERSION.to_string())
}

pub fn remote_url_in(root: &Path) -> Result<String, String> {
    let (code, url) = git_in(root, &["remote", "get-url", "origin"], 60)?;
    Ok(if code == 0 { url } else { String::new() })
}

/// The checked-out branch, or "".
pub fn branch() -> Result<String, String> {
    branch_in(&ROOT)
}

pub fn branch_in(root: &Path) -> Result<String, String> {
    let (code, name) = git_in(root, &["rev-parse", "--abbrev-ref", "HEAD"], 60)?;
    Ok(if code == 0 { name } else { String::new() })
}

/// Whether tracked files have uncommitted changes.
pub fn dirty() -> Result<bool, String> {
    dirty_in(&ROOT)
}

pub fn dirty_in(root: &Path) -> Result<bool, String> {
    let (code, out) = git_in(root, &["status", "--porcelain", "--untracked-files=no"], 60)?;
    Ok(code == 0 && !out.is_empty())
}

/// What an update would consist of, without performing one.
///
/// Returns a dict with: kind, version, behind, latest, note, can_update.
///
/// Where the Python raises, this cannot: the exception goes to stderr and
/// the answer is the default `Info` -- which is not what the Python's callers
/// see (they stop). Callers use `try_check` and stop on Err.
#[deprecated(note = "the Python raises here; use try_check and stop on Err")]
pub fn check(fetch: bool) -> Info {
    try_check(fetch).unwrap_or_else(|e| uncaught(e, Info::default()))
}

/// `check`, with an exception the Python lets through as `Err`. This is a
/// compiled build, so it is `compiled_check_in` for the running binary.
pub fn try_check(fetch: bool) -> Result<Info, String> {
    compiled_check_in(&ROOT, &running_exe(), fetch)
}

/// `check` as the Python does it, against `root`. The app itself goes
/// through `compiled_check_in`; this is the specification it builds on.
pub fn check_in(root: &Path, fetch: bool) -> Result<Info, String> {
    let kind = install_kind_in(root)?;
    let version = version_in(root)?;
    check_as(root, kind, version, || owning_package_in(root), fetch)
}

/// The body of `check` once the kind, the version and how to name the owning
/// package are settled.
fn check_as(
    root: &Path,
    kind: &'static str,
    version: String,
    owner: impl FnOnce() -> Result<String, String>,
    fetch: bool,
) -> Result<Info, String> {
    let mut info = Info { kind: kind.to_string(), version, ..Default::default() };

    if kind == PACKAGE {
        let pkg = owner()?;
        info.note = format!("Installed as the {pkg} package — it updates with the rest of the system.");
        return Ok(info);
    }

    if kind == PLAIN {
        info.note = "Running from a plain directory with no git checkout, so there is nothing to pull from.".to_string();
        return Ok(info);
    }

    info.remote = remote_url_in(root)?;
    info.dirty = dirty_in(root)?;
    if info.remote.is_empty() {
        info.note = "This checkout has no 'origin' remote yet, so there is nowhere to update from.".to_string();
        return Ok(info);
    }

    if fetch {
        let (code, out) = git_in(root, &["fetch", "--quiet", "origin"], 90)?;
        if code != 0 {
            let last = if out.is_empty() { "" } else { py_splitlines(&out).last().copied().unwrap_or("") };
            info.note = format!("Could not reach the remote: {last}");
            return Ok(info);
        }
    }

    let br = branch_in(root)?;
    let range = format!("HEAD...origin/{br}");
    let (code, counts) = git_in(root, &["rev-list", "--left-right", "--count", &range], 60)?;
    if code != 0 {
        info.note = format!("No upstream branch for {br}.");
        return Ok(info);
    }
    // ahead, behind = (int(x) for x in counts.split()); ValueError -> 0, 0
    let parts = py_split(&counts);
    let (ahead, behind) = match parts.as_slice() {
        [a, b] => match (py_int(a), py_int(b)) {
            (Some(a), Some(b)) => (a, b),
            _ => (BigInt::ZERO, BigInt::ZERO),
        },
        _ => (BigInt::ZERO, BigInt::ZERO),
    };

    info.behind = behind.clone();
    if behind != BigInt::ZERO {
        let upstream = format!("origin/{br}");
        let (code, latest) = git_in(root, &["log", "-1", "--format=%h %s", &upstream], 60)?;
        info.latest = if code == 0 { latest } else { String::new() };
    }
    if ahead != BigInt::ZERO && behind != BigInt::ZERO {
        info.note = format!(
            "This checkout has {ahead} local commit(s) the remote does not, so it cannot fast-forward. Rebase or reset it by hand."
        );
        return Ok(info);
    }
    if info.dirty {
        info.note = "There are uncommitted changes here. Commit or stash them before updating, or they would be in the way of a fast-forward.".to_string();
        return Ok(info);
    }
    if behind != BigInt::ZERO {
        info.can_update = true;
        info.note = format!("{behind} commit(s) behind origin/{br}.");
    } else {
        info.note = format!("Up to date with origin/{br}.");
    }
    Ok(info)
}

// ---- A compiled build: the package manager, or a rebuild ----
//
// Not in the Python, deliberately. Python adopts new source on its next
// start, so a fast-forward pull plus a restart *is* an update. A compiled
// Almanac does not: pulling changes the source and nothing that runs, and
// restarting would start the same binary (or the Python launcher). So a
// compiled build never updates itself:
//
// * installed by pacman (the binary itself is owned by a package) -- it
//   defers to pacman exactly as the Python package does;
// * built in a git checkout -- everything `check` reports still applies,
//   but where the Python would offer a pull, this says to pull and rebuild;
// * anything else -- nothing to update from, as for a plain directory.
//
// What is checked for ownership is the running binary, not ROOT/app.py: an
// installed binary has no checkout beside it, and ROOT then falls back to
// the directory it was built in, which may not exist any more (or may be a
// stale makepkg checkout -- which is why git only counts when the binary
// lives inside ROOT).

/// The version of this build: [`VERSION`].
pub const BUILD_VERSION: &str = VERSION;

/// What the note says, after the Python's "N commit(s) behind", in a
/// checkout that would otherwise offer to pull.
pub const REBUILD_NOTE: &str = "This copy is compiled, so pulling alone changes nothing that runs: \
                                pull, then rebuild it with cargo build --release.";

/// The running binary, symlinks resolved.
pub fn running_exe() -> PathBuf {
    let exe = running_program().into_iter().next().unwrap_or_default();
    py_resolve(Path::new(&exe))
}

/// `check` for a compiled build: see above.
pub fn compiled_check_in(root: &Path, exe: &Path, fetch: bool) -> Result<Info, String> {
    let in_checkout = exe.starts_with(root) && git_in(root, &["rev-parse", "--is-inside-work-tree"], 60)?.0 == 0;
    let mut owner: Option<String> = None;
    let kind = if in_checkout {
        GIT
    } else {
        // except (OSError, subprocess.SubprocessError): not a package
        match pacman_owner_of(exe.as_os_str().to_owned())? {
            Ok(out) => {
                let stdout = decode_text(&out.stdout)?;
                decode_text(&out.stderr)?;
                let name = py_strip(&stdout).to_string();
                if out.code == 0 && !name.is_empty() {
                    owner = Some(name);
                    PACKAGE
                } else {
                    PLAIN
                }
            }
            Err(_) => PLAIN,
        }
    };
    let version = if kind == GIT { version_in(root)? } else { BUILD_VERSION.to_string() };
    let mut info = check_as(root, kind, version, || Ok(owner.unwrap_or_default()), fetch)?;
    if info.can_update {
        info.can_update = false;
        info.note = format!("{} {REBUILD_NOTE}", info.note);
    }
    Ok(info)
}

/// Steps for the task runner. Fast-forward only, deliberately.
///
/// Each step is (title, argv).
pub fn update_steps() -> Vec<(String, Vec<String>)> {
    update_steps_in(&ROOT)
}

pub fn update_steps_in(root: &Path) -> Vec<(String, Vec<String>)> {
    let argv = ["git", "-C", &root.to_string_lossy(), "pull", "--ff-only", "--stat"].map(String::from);
    vec![("Updating Almanac".to_string(), argv.to_vec())]
}

/// How to start the new code once it is on disk.
///
/// Err is the RuntimeError `Path.home()` raises when there is no home at all.
pub fn restart_argv() -> Result<Vec<String>, String> {
    restart_argv_or(running_program())
}

/// `restart_argv` with what `[sys.executable, str(ROOT / "app.py")]` stands
/// for given: the program that is running, started again.
///
/// The launcher looked for is `~/.local/bin/almanac`.
fn restart_argv_or(running: Vec<String>) -> Result<Vec<String>, String> {
    let launcher = py_path_join(&py_home()?, &format!(".local/bin/{}", crate::watcher::command_name()));
    if Path::new(&launcher).exists() {
        return Ok(vec![launcher.to_string_lossy().into_owned()]);
    }
    Ok(running)
}

/// `[sys.executable, str(ROOT / "app.py")]` for this process. Python names
/// the interpreter that is actually running plus the script; here the running
/// program is the binary itself, so it is started again on its own. Linux
/// marks a binary replaced since start-up " (deleted)"; the path it was
/// started from is what holds the new build.
fn running_program() -> Vec<String> {
    let exe = std::env::current_exe()
        .map(|p| p.into_os_string())
        .ok()
        .or_else(|| std::env::args_os().next())
        .unwrap_or_default();
    let mut exe = exe.to_string_lossy().into_owned();
    if let Some(stem) = exe.strip_suffix(" (deleted)") {
        if !Path::new(&exe).exists() {
            exe = stem.to_string();
        }
    }
    vec![exe]
}

/// Hand off to a fresh process and let this one go.
///
/// A running Python process cannot adopt new source, so the only honest way
/// to apply an update is to start again.
pub fn restart() {
    // A RuntimeError from Path.home() escapes the Python handler: nothing happens.
    let Ok(argv) = restart_argv() else { return };
    // subprocess.Popen(argv, start_new_session=True, stdout=DEVNULL, stderr=DEVNULL)
    // except OSError: return
    if spawn_new_session(&argv).is_err() {
        return;
    }
    exit_without_cleanup();
}

/// `Popen(argv, start_new_session=True, stdout=DEVNULL, stderr=DEVNULL)`.
///
/// posix_spawn with POSIX_SPAWN_SETSID is how to get setsid() in the child
/// without an unsafe pre_exec hook; SIGPIPE and SIGXFSZ go back to their
/// defaults, as Popen's restore_signals does; and every descriptor above 2 is
/// closed in the child, as Popen's close_fds=True does.
fn spawn_new_session(argv: &[String]) -> Result<(), ()> {
    use nix::spawn::{posix_spawn, PosixSpawnAttr, PosixSpawnFileActions, PosixSpawnFlags};
    use nix::sys::signal::{SigSet, Signal};
    use std::ffi::CString;
    use std::os::fd::AsRawFd;

    let Some(prog) = argv.first() else { return Err(()) };
    let devnull = std::fs::OpenOptions::new().read(true).write(true).open("/dev/null").map_err(drop)?;
    let mut actions = PosixSpawnFileActions::init().map_err(drop)?;
    actions.add_dup2(devnull.as_raw_fd(), 1).map_err(drop)?;
    actions.add_dup2(devnull.as_raw_fd(), 2).map_err(drop)?;
    // close_fds=True. glibc ignores a close of a descriptor that is already
    // gone by spawn time (the directory handle of this very listing).
    let mut open_fds: Vec<i32> = std::fs::read_dir("/proc/self/fd")
        .map(|dir| dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok()).collect())
        .unwrap_or_default();
    open_fds.sort_unstable();
    for fd in open_fds.into_iter().filter(|&fd| fd > 2) {
        actions.add_close(fd).map_err(drop)?;
    }
    let mut attr = PosixSpawnAttr::init().map_err(drop)?;
    let mut sigs = SigSet::empty();
    sigs.add(Signal::SIGPIPE);
    sigs.add(Signal::SIGXFSZ);
    attr.set_sigdefault(&sigs).map_err(drop)?;
    let flags = PosixSpawnFlags::POSIX_SPAWN_SETSIGDEF
        | PosixSpawnFlags::from_bits_retain(libc::c_int::from(libc::POSIX_SPAWN_SETSID));
    attr.set_flags(flags).map_err(drop)?;
    let args: Vec<CString> = argv.iter().map(|a| CString::new(a.as_bytes())).collect::<Result<_, _>>().map_err(drop)?;
    let env: Vec<CString> = std::env::vars_os()
        .map(|(k, v)| {
            let mut kv = k.into_vec();
            kv.push(b'=');
            kv.extend(v.into_vec());
            CString::new(kv)
        })
        .collect::<Result<_, _>>()
        .map_err(drop)?;
    for candidate in exec_candidates(OsStr::new(prog)) {
        let path = CString::new(candidate.into_vec()).map_err(drop)?;
        if posix_spawn(path.as_c_str(), &actions, &attr, &args, &env).is_ok() {
            return Ok(());
        }
    }
    Err(())
}

/// `os._exit(0)`: leave at once, without atexit handlers or library
/// destructors. std has no safe `_exit` (and `unsafe` is off the table), so
/// the process image is replaced by `true`, which exits 0 just the same: exec
/// runs no atexit handler or destructor either. Only if no `true` can start
/// at all does it fall back to exit(0), which does run them.
fn exit_without_cleanup() -> ! {
    use std::os::unix::process::CommandExt;
    for prog in ["/usr/bin/true", "/bin/true", "true"] {
        let _ = Command::new(prog).env_clear().exec();
    }
    std::process::exit(0)
}

/// Almanac as an entry in its own inventory.
///
/// Where the Python raises (and the inventory drops the row), this cannot:
/// the exception goes to stderr and the row is built from a default `Info`.
/// Callers use `try_as_app` and leave the row out on Err.
#[deprecated(note = "the Python raises here; use try_as_app and drop the row on Err")]
pub fn as_app(check_updates: bool) -> crate::inventory::App {
    try_as_app(check_updates).unwrap_or_else(|e| uncaught(e, app_from(&ROOT, Info::default(), String::new())))
}

/// `as_app`, with an exception the Python lets through as `Err`: the row for
/// this compiled build (`compiled_check_in`).
pub fn try_as_app(check_updates: bool) -> Result<crate::inventory::App, String> {
    compiled_as_app_in(&ROOT, &running_exe(), check_updates)
}

/// `as_app` for a compiled build: the same row, from `compiled_check_in`. It
/// never carries an update (can_update is never set), so it stays out of the
/// Updates list: pacman lists the package; a checkout says to rebuild.
pub fn compiled_as_app_in(root: &Path, exe: &Path, check_updates: bool) -> Result<crate::inventory::App, String> {
    let info = compiled_check_in(root, exe, check_updates)?;
    app_with_home(root, info)
}

fn app_with_home(root: &Path, info: Info) -> Result<crate::inventory::App, String> {
    let exec_path = py_path_join(&py_home()?, ".local/bin/almanac").to_string_lossy().into_owned();
    Ok(app_from(root, info, exec_path))
}

fn app_from(root: &Path, info: Info, exec_path: String) -> crate::inventory::App {
    use serde_json::{json, Map, Value};
    let mut update = String::new();
    if info.can_update {
        update = format!("{} commit(s)", info.behind);
    }
    let url = if info.remote.starts_with("http") { info.remote.replace(".git", "") } else { String::new() };
    let mut extra = Map::new();
    extra.insert("key".into(), json!("almanac"));
    extra.insert("self".into(), json!(true));
    extra.insert("kind".into(), json!(info.kind));
    extra.insert("repo".into(), json!(root.to_string_lossy()));
    extra.insert("url".into(), Value::String(url));
    extra.insert("note".into(), json!(info.note));
    extra.insert("latest".into(), json!(info.latest));
    extra.insert("git_behind".into(), py_int_json::to_value(&info.behind));
    extra.insert("git_dirty".into(), json!(info.dirty));
    extra.insert("is_app".into(), json!(true));
    crate::inventory::App {
        name: NAME.to_string(),
        version: info.version.clone(),
        source: crate::inventory::SYSTEM.to_string(),
        description: format!("This application · {}", info.note),
        update,
        exec_path,
        extra: extra.into(),
    }
}

// ---- Python process semantics this module leans on ----

/// What `subprocess.run(..., capture_output=True)` returned.
struct Captured {
    /// `returncode`: the exit status, or -signal for a killed process.
    code: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

enum RunError {
    /// The OSError, as `str(exc)` reads.
    Os(String),
    /// TimeoutExpired.
    Timeout,
    /// An exception no caller here catches (ValueError for an embedded NUL).
    Raise(String),
}

/// `subprocess.run(argv, capture_output=True, timeout=secs)`, still as bytes.
///
/// Like CPython: the program is looked up on PATH by trying each directory in
/// turn and reporting the first error that is not ENOENT/ENOTDIR; stdin is
/// inherited; and the timeout bounds the whole call -- on expiry the child is
/// killed and the call returns, even while a grandchild still holds the pipes.
fn run_capture(argv: &[OsString], timeout: Option<u64>) -> Result<Captured, RunError> {
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::sync::mpsc::{channel, RecvTimeoutError};

    let Some((prog, args)) = argv.split_first() else {
        return Err(RunError::Os("[Errno 2] No such file or directory: ''".to_string()));
    };
    // _fork_exec turns each argument into a C string before anything runs.
    if argv.iter().any(|a| a.as_bytes().contains(&0)) {
        return Err(RunError::Raise("ValueError: embedded null byte".to_string()));
    }
    let mut child: Option<Child> = None;
    let (mut first_err, mut last_err) = (None, None);
    for candidate in exec_candidates(prog) {
        let spawned = crate::util::spawn_group(Command::new(&candidate)
            .arg0(prog)
            .args(args)
            .stdin(Stdio::inherit())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()));
        match spawned {
            Ok(c) => {
                child = Some(c);
                break;
            }
            Err(e) => {
                let n = e.raw_os_error().unwrap_or(libc::ENOENT);
                if n != libc::ENOENT && n != libc::ENOTDIR && first_err.is_none() {
                    first_err = Some(n);
                }
                last_err = Some(n);
            }
        }
    }
    let Some(mut child) = child else {
        let n = first_err.or(last_err).unwrap_or(libc::ENOENT);
        return Err(RunError::Os(format!("[Errno {n}] {}: {}", strerror(n), py_str_repr(&prog.to_string_lossy()))));
    };

    let deadline = timeout.map(|t| Instant::now() + Duration::from_secs(t));
    let timed_out = |mut child: Child| {
        crate::util::kill_group(&mut child);
        Err(RunError::Timeout)
    };
    // Drain both pipes on their own threads, or a chatty child fills one and
    // blocks forever. They are not joined: a grandchild may hold the pipes open
    // long after git is gone, and the timeout must not wait for it.
    let (tx, rx) = channel::<(bool, Vec<u8>)>();
    for (is_err, pipe) in [
        (false, child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>)),
        (true, child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>)),
    ] {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            let _ = tx.send((is_err, buf));
        });
    }
    drop(tx);
    let (mut stdout, mut stderr) = (None, None);
    while stdout.is_none() || stderr.is_none() {
        let got = match deadline {
            Some(d) => match rx.recv_timeout(d.saturating_duration_since(Instant::now())) {
                Ok(got) => got,
                Err(RecvTimeoutError::Timeout) => return timed_out(child),
                Err(RecvTimeoutError::Disconnected) => break,
            },
            None => match rx.recv() {
                Ok(got) => got,
                Err(_) => break,
            },
        };
        if got.0 {
            stderr = Some(got.1);
        } else {
            stdout = Some(got.1);
        }
    }
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => {
                if deadline.is_some_and(|d| Instant::now() >= d) {
                    return timed_out(child);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => return Err(RunError::Os(os_error_str(&e))),
        }
    };
    let code = status.code().unwrap_or_else(|| -status.signal().unwrap_or(0));
    Ok(Captured { code, stdout: stdout.unwrap_or_default(), stderr: stderr.unwrap_or_default() })
}

/// The paths CPython's `_execute_child` tries for `prog`: itself when it has a
/// slash, else `os.path.join(dir, prog)` for each entry of PATH (`os.defpath`
/// when unset). An empty entry means the current directory.
fn exec_candidates(prog: &OsStr) -> Vec<OsString> {
    if prog.as_bytes().contains(&b'/') {
        return vec![prog.to_owned()];
    }
    let path = std::env::var_os("PATH").unwrap_or_else(|| OsString::from("/bin:/usr/bin"));
    path.as_bytes()
        .split(|&c| c == b':')
        .map(|dir| {
            let mut p = if dir.is_empty() { b"./".to_vec() } else { dir.to_vec() };
            if p.last() != Some(&b'/') {
                p.push(b'/');
            }
            p.extend_from_slice(prog.as_bytes());
            OsString::from_vec(p)
        })
        .collect()
}

/// `os.strerror(n)`.
fn strerror(n: i32) -> String {
    let s = std::io::Error::from_raw_os_error(n).to_string();
    match s.rfind(" (os error ") {
        Some(i) => s[..i].to_string(),
        None => s,
    }
}

/// `str(OSError)` for an error with no filename.
fn os_error_str(e: &std::io::Error) -> String {
    match e.raw_os_error() {
        Some(n) => format!("[Errno {n}] {}", strerror(n)),
        None => e.to_string(),
    }
}

/// `text=True`: strict UTF-8, then universal newlines (\r\n and \r become \n).
/// Err is the UnicodeDecodeError, which no caller here catches.
fn decode_text(b: &[u8]) -> Result<String, String> {
    match std::str::from_utf8(b) {
        Ok(s) => Ok(s.replace("\r\n", "\n").replace('\r', "\n")),
        Err(e) => {
            let pos = e.valid_up_to();
            let n = e.error_len().unwrap_or(b.len() - pos);
            let reason = match e.error_len() {
                None => "unexpected end of data",
                Some(_) if !matches!(b[pos], 0xC2..=0xF4) => "invalid start byte",
                Some(_) => "invalid continuation byte",
            };
            let at = if n <= 1 {
                format!("byte 0x{:02x} in position {pos}", b[pos])
            } else {
                format!("bytes in position {pos}-{}", pos + n - 1)
            };
            Err(format!("UnicodeDecodeError: 'utf-8' codec can't decode {at}: {reason}"))
        }
    }
}

/// `Path.home()`: `os.path.expanduser("~")` -- $HOME, else the passwd entry
/// of the real uid -- with trailing slashes dropped, as a normalised Path.
fn py_home() -> Result<OsString, String> {
    let no_home = || "RuntimeError: Could not determine home directory.".to_string();
    let home = match std::env::var_os("HOME") {
        Some(h) => h,
        None => passwd_home().ok_or_else(no_home)?,
    };
    let b = home.as_bytes();
    let end = b.iter().rposition(|&c| c != b'/').map_or(0, |i| i + 1);
    // (userhome + path[i:]) or root
    let home: &[u8] = if end == 0 { b"/" } else { &b[..end] };
    // expanduser hands back "~" unchanged when it found nothing.
    if home == b"~" {
        return Err(no_home());
    }
    Ok(py_path(OsStr::from_bytes(home)))
}

/// `pwd.getpwuid(os.getuid()).pw_dir`: /etc/passwd first (where NSS looks
/// first), then `getent` for the other databases.
fn passwd_home() -> Option<OsString> {
    let uid = rustix::process::getuid().as_raw().to_string();
    let from_line = |line: &[u8]| -> Option<OsString> {
        let fields: Vec<&[u8]> = line.split(|&c| c == b':').collect();
        (fields.len() >= 6 && fields[2] == uid.as_bytes()).then(|| OsString::from_vec(fields[5].to_vec()))
    };
    if let Ok(text) = std::fs::read("/etc/passwd") {
        if let Some(home) = text.split(|&c| c == b'\n').find_map(from_line) {
            return Some(home);
        }
    }
    let argv: Vec<OsString> = vec!["getent".into(), "passwd".into(), uid.clone().into()];
    let out = run_capture(&argv, Some(20)).ok().filter(|o| o.code == 0)?;
    out.stdout.split(|&c| c == b'\n').find_map(from_line)
}

/// `str(PurePosixPath(p))`: empty and "." parts dropped, runs of slashes
/// collapsed -- except a leading exactly-two, which POSIX leaves alone.
fn py_path(p: &OsStr) -> OsString {
    let b = p.as_bytes();
    let (root, rel): (&[u8], &[u8]) = if !b.starts_with(b"/") {
        (b"", b)
    } else if b.get(1) != Some(&b'/') || b.get(2) == Some(&b'/') {
        (b"/", &b[1..])
    } else {
        (b"//", &b[2..])
    };
    let parts: Vec<&[u8]> = rel.split(|&c| c == b'/').filter(|x| !x.is_empty() && *x != b".").collect();
    let mut out = root.to_vec();
    out.extend(parts.join(&b'/'));
    if out.is_empty() {
        out.push(b'.');
    }
    OsString::from_vec(out)
}

/// `str(Path(base) / rel)`.
fn py_path_join(base: &OsStr, rel: &str) -> OsString {
    let mut b = base.as_bytes().to_vec();
    if b.last() != Some(&b'/') {
        b.push(b'/');
    }
    b.extend_from_slice(rel.as_bytes());
    py_path(OsStr::from_bytes(&b))
}

// ---- Python string semantics this module leans on ----

/// Python's `str.isspace()` per character: Unicode White_Space plus the
/// information separators \x1c-\x1f, which `str.split()` also splits on.
fn py_ws(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// `str.split()` with no separator.
fn py_split(s: &str) -> Vec<&str> {
    s.split(py_ws).filter(|p| !p.is_empty()).collect()
}

/// `str.strip()`.
fn py_strip(s: &str) -> &str {
    s.trim_matches(py_ws)
}

/// `str.splitlines()`.
fn py_splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        let brk = matches!(c, '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}');
        if brk {
            out.push(&s[start..i]);
            let mut end = i + c.len_utf8();
            if c == '\r' {
                if let Some(&(j, '\n')) = it.peek() {
                    it.next();
                    end = j + 1;
                }
            }
            start = end;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// `int(x)` for a token `str.split()` produced: an ASCII sign, then decimal
/// digits of any script with single underscores between them, at most 4300 of
/// them (`sys.get_int_max_str_digits()`), and no upper bound on the value.
fn py_int(s: &str) -> Option<BigInt> {
    let (neg, digits) = match s.strip_prefix('-') {
        Some(d) => (true, d),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let mut ascii = String::new();
    let mut after_underscore = true;
    for c in digits.chars() {
        if c == '_' {
            if after_underscore {
                return None;
            }
            after_underscore = true;
            continue;
        }
        ascii.push(char::from_digit(crate::util::decimal_value(c)?, 10)?);
        after_underscore = false;
    }
    if ascii.is_empty() || after_underscore || ascii.len() > 4300 {
        return None;
    }
    let n = BigInt::parse_bytes(ascii.as_bytes(), 10)?;
    Some(if neg { -n } else { n })
}

/// `repr(str)`, as `str(TimeoutExpired)` shows each argv entry.
fn py_str_repr(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::from(q);
    for c in s.chars() {
        let cp = c as u32;
        match c {
            '\\' => out.push_str("\\\\"),
            c if c == q => {
                out.push('\\');
                out.push(c);
            }
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ if cp < 0x20 || cp == 0x7f => out.push_str(&format!("\\x{cp:02x}")),
            c if cp < 0x7f || !py_nonprintable(cp) => out.push(c),
            _ if cp <= 0xff => out.push_str(&format!("\\x{cp:02x}")),
            _ if cp <= 0xffff => out.push_str(&format!("\\u{cp:04x}")),
            _ => out.push_str(&format!("\\U{cp:08x}")),
        }
    }
    out.push(q);
    out
}

/// `not str.isprintable()` for a code point from U+0080 up.
fn py_nonprintable(cp: u32) -> bool {
    let i = NONPRINTABLE.partition_point(|&(_, hi)| hi < cp);
    NONPRINTABLE.get(i).is_some_and(|&(lo, _)| lo <= cp)
}

/// A Python int in JSON: a number, exact while it fits 64 bits.
mod py_int_json {
    use num_bigint::BigInt;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn to_value(v: &BigInt) -> serde_json::Value {
        if let Ok(n) = i64::try_from(v) {
            serde_json::Value::from(n)
        } else if let Ok(n) = u64::try_from(v) {
            serde_json::Value::from(n)
        } else {
            // serde_json cannot hold a wider integer; the nearest float.
            serde_json::Value::from(v.to_string().parse::<f64>().unwrap_or(f64::INFINITY))
        }
    }

    pub fn serialize<S: Serializer>(v: &BigInt, s: S) -> Result<S::Ok, S::Error> {
        to_value(v).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<BigInt, D::Error> {
        let n = serde_json::Number::deserialize(d)?;
        Ok(if let Some(i) = n.as_i64() {
            BigInt::from(i)
        } else if let Some(u) = n.as_u64() {
            BigInt::from(u)
        } else {
            BigInt::parse_bytes(format!("{:.0}", n.as_f64().unwrap_or(0.0)).as_bytes(), 10).unwrap_or_default()
        })
    }
}

/// What pyexpat's unknown-encoding handler makes of an encoding name: a byte
/// map expat accepts, one it refuses (ParseError), or a Python exception.
#[derive(Debug, Clone, Copy)]
enum Codec {
    Map(usize),
    Unusable,
    Raise(&'static str),
}

/// `codecs.lookup(name)` as `encodings.search_function` resolves it, reduced
/// to what `bytes(range(256)).decode(name, "replace")` gives pyexpat.
fn py_codec(name: &str) -> Codec {
    // _Py_normalize_encoding: lower-case, every other run of punctuation but
    // '.' becomes one '_', none at the ends.
    let mut norm = String::new();
    let mut punct = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '.' {
            if punct && !norm.is_empty() {
                norm.push('_');
            }
            norm.push(c.to_ascii_lowercase());
            punct = false;
        } else {
            punct = true;
        }
    }
    let table = |t: &'static [(&'static str, &'static str)], k: &str| -> Option<&'static str> {
        t.binary_search_by(|(a, _)| (*a).cmp(k)).ok().map(|i| t[i].1)
    };
    let alias = table(CODEC_ALIASES, &norm).or_else(|| table(CODEC_ALIASES, &norm.replace('.', "_")));
    for module in alias.into_iter().chain(std::iter::once(norm.as_str())) {
        if module.is_empty() || module.contains('.') {
            continue;
        }
        if let Ok(i) = CODEC_MODULES.binary_search_by(|(m, _)| (*m).cmp(module)) {
            return CODEC_MODULES[i].1;
        }
    }
    Codec::Raise("LookupError")
}

/// The 256 characters of a codec map; U+FFFD is a byte expat calls malformed.
fn codec_map(i: usize) -> Option<Vec<char>> {
    let mut map: Vec<char> = (0u8..0x80).map(char::from).collect();
    for &(m, b) in CODEC_LOW_UNDEFINED {
        if m == i {
            map[usize::from(b)] = '\u{fffd}';
        }
    }
    map.extend(CODEC_MAPS.get(i)?.chars());
    (map.len() == 256).then_some(map)
}

/// Just enough of `xml.etree.ElementTree` for `release_notes`, matching what
/// CPython's C XMLParser gets from expat 2.8 in namespace mode: expat's
/// encoding detection and declarations (an unknown encoding goes through
/// pyexpat's handler, so a Python error can come out of it), its name tables,
/// the internal DTD subset (general entities, attribute defaults and types;
/// parameter entities are never read, so after a reference to one later
/// declarations are skipped unless standalone="yes"), the namespace rules, and
/// `tostring(encoding="unicode")` with its recursion limit.
mod et {
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
}


#[cfg(test)]
mod tests {
    use super::*;

    fn sh_git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@example.invalid")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@example.invalid")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "git {args:?}");
    }

    /// A compiled build never offers to pull: a checkout behind its remote
    /// says to rebuild, and a binary outside the checkout ignores it.
    #[test]
    fn compiled_build_rebuilds_or_defers_to_pacman() {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        std::fs::create_dir(&work).unwrap();
        sh_git(&work, &["init", "-q", "-b", "main"]);
        for (i, text) in ["one", "two"].iter().enumerate() {
            std::fs::write(work.join("app.py"), text).unwrap();
            sh_git(&work, &["add", "app.py"]);
            sh_git(&work, &["commit", "-q", "-m", &format!("c{i}")]);
        }
        let clone = tmp.path().join("clone");
        sh_git(tmp.path(), &["clone", "-q", work.to_str().unwrap(), clone.to_str().unwrap()]);
        sh_git(&clone, &["reset", "-q", "--hard", "HEAD~1"]);

        let exe = clone.join("target/release/almanac");
        let py = check_in(&clone, false).unwrap();
        assert!(py.can_update);
        let info = compiled_check_in(&clone, &exe, false).unwrap();
        assert_eq!(info.kind, GIT);
        assert!(!info.can_update);
        assert_eq!(info.note, format!("{} {REBUILD_NOTE}", py.note));
        assert!(info.note.contains("cargo build --release"));
        let app = compiled_as_app_in(&clone, &exe, false).unwrap();
        assert_eq!(app.update, "");

        // Installed elsewhere (/usr/bin, say): the checkout it was built in
        // is not this copy, and pacman owns nothing under a temp dir.
        let elsewhere = tmp.path().join("usr/bin/almanac");
        let info = compiled_check_in(&clone, &elsewhere, false).unwrap();
        assert_eq!(info.kind, PLAIN);
        assert_eq!(info.version, BUILD_VERSION);
        assert!(!info.can_update);
    }

    // AboutWindow.test_whats_new_comes_from_the_shipped_metainfo
    #[test]
    fn test_whats_new_comes_from_the_shipped_metainfo() {
        let (version, markup) = try_release_notes().unwrap().expect("release notes");
        assert_eq!(version, VERSION);
        assert!(markup.starts_with("<p>"));
    }

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

    #[test]
    fn python_string_helpers() {
        assert_eq!(py_splitlines("a\r\nb\rc\n"), vec!["a", "b", "c"]);
        assert_eq!(py_split(" a\x1c b\u{85}c\u{a0}d"), vec!["a", "b", "c", "d"]);
        assert_eq!(py_int("+3"), Some(BigInt::from(3)));
        assert_eq!(py_int("1_0"), Some(BigInt::from(10)));
        assert_eq!(py_int("٣"), Some(BigInt::from(3)));
        assert_eq!(py_int("x"), None);
        assert_eq!(py_str_repr("it's"), "\"it's\"");
        assert_eq!(py_str_repr("a\u{a0}\u{2028}\u{85}\u{200b}é"), "'a\\xa0\\u2028\\x85\\u200bé'");
        assert_eq!(py_path(OsStr::new("/tmp//a/./b/")), OsString::from("/tmp/a/b"));
        assert_eq!(py_path(OsStr::new("//x")), OsString::from("//x"));
    }

    #[test]
    fn update_steps_fast_forward_only() {
        let steps = update_steps();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].0, "Updating Almanac");
        assert_eq!(&steps[0].1[3..], &["pull", "--ff-only", "--stat"]);
    }

    #[test]
    fn restart_starts_the_child_in_a_new_session() {
        // Fields after "(comm)": state ppid pgrp session ...
        let session = |stat: &str| -> String {
            stat.rsplit_once(") ").map(|(_, r)| r.split_whitespace().nth(3).unwrap_or("").to_string()).unwrap_or_default()
        };
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("stat");
        let script = format!("cat /proc/self/stat > '{}.tmp' && mv '{0}.tmp' '{0}'", out.display());
        spawn_new_session(&["/bin/sh".to_string(), "-c".to_string(), script]).unwrap();
        for _ in 0..500 {
            if out.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let child = std::fs::read_to_string(&out).unwrap();
        let ours = std::fs::read_to_string("/proc/self/stat").unwrap();
        assert_ne!(session(&child), session(&ours));
        assert!(!session(&child).is_empty());
        assert!(spawn_new_session(&["/nonexistent/almanac".to_string()]).is_err());
    }

    #[test]
    fn run_capture_reports_signals_and_errno_like_python() {
        let argv = |a: &[&str]| a.iter().map(OsString::from).collect::<Vec<_>>();
        let out = run_capture(&argv(&["sh", "-c", "kill -9 $$"]), Some(5)).ok().unwrap();
        assert_eq!(out.code, -9);
        let start = Instant::now();
        assert!(matches!(run_capture(&argv(&["sh", "-c", "sleep 5 & sleep 5"]), Some(1)), Err(RunError::Timeout)));
        assert!(start.elapsed() < Duration::from_secs(3));
        match run_capture(&argv(&["/proc"]), Some(1)) {
            Err(RunError::Os(m)) => assert_eq!(m, "[Errno 13] Permission denied: '/proc'"),
            _ => panic!("a directory cannot run"),
        }
        assert!(decode_text(b"caf\xe9").unwrap_err().starts_with("UnicodeDecodeError"));
    }

    #[test]
    fn restart_argv_prefers_the_launcher_else_the_running_program() {
        let argv = restart_argv().unwrap();
        assert_eq!(argv.len(), 1);
        let exe = std::env::current_exe().unwrap();
        assert!(argv[0].ends_with("/.local/bin/almanac") || argv[0] == exe.to_string_lossy());
        assert_eq!(running_program(), vec![exe.to_string_lossy().into_owned()]);
    }

    #[test]
    fn root_is_the_resolved_checkout_of_the_running_binary() {
        assert!(ROOT.join("Cargo.toml").is_file());
        assert_eq!(*ROOT, std::fs::canonicalize(&*ROOT).unwrap());
        let dir = tempfile::tempdir().unwrap();
        let real = std::fs::canonicalize(dir.path()).unwrap();
        std::os::unix::fs::symlink(&real, real.join("link")).unwrap();
        assert_eq!(py_resolve(&real.join("link/missing/x")), real.join("missing/x"));
    }

    #[test]
    fn embedded_nul_raises_like_python() {
        assert_eq!(git_in(&ROOT, &["status\0x"], 5), Err("ValueError: embedded null byte".to_string()));
    }

    #[test]
    fn restart_closes_inherited_descriptors() {
        let dir = tempfile::tempdir().unwrap();
        // std opens with O_CLOEXEC; clear it, so only close_fds keeps it out.
        let keep = std::fs::File::open("/dev/null").unwrap();
        rustix::io::fcntl_setfd(&keep, rustix::io::FdFlags::empty()).unwrap();
        let fd = std::os::fd::AsRawFd::as_raw_fd(&keep);
        let out = dir.path().join("fds");
        let script = format!("[ -e /proc/self/fd/{fd} ] && echo leaked > '{0}.tmp'; echo done >> '{0}.tmp'; mv '{0}.tmp' '{0}'", out.display());
        spawn_new_session(&["/bin/sh".to_string(), "-c".to_string(), script]).unwrap();
        for _ in 0..500 {
            if out.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "done\n");
        drop(keep);
    }

    #[test]
    fn release_notes_nested_past_the_recursion_limit_do_not_overflow_a_small_stack() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("docs")).unwrap();
        let deep = |n: usize| "<p>".repeat(n) + "x" + &"</p>".repeat(n);
        for (n, ok) in [(10, true), (5000, false)] {
            let xml = format!(
                "<component><releases><release version=\"1.0\"><description>{}</description></release></releases></component>",
                deep(n)
            );
            std::fs::write(dir.path().join("docs").join(METAINFO), xml).unwrap();
            let root = dir.path().to_path_buf();
            let got = std::thread::Builder::new()
                .stack_size(128 << 10)
                .spawn(move || release_notes_in(&root))
                .unwrap()
                .join()
                .unwrap();
            assert_eq!(got.is_ok(), ok, "{n}: {got:?}");
        }
    }
}
// ---- Tables generated from CPython 3.14 / expat 2.8 (see the parity notes) ----

/// expat's name-start characters above U+007F (its XML 1.0 Appendix B tables).
const NMSTRT_BMP: &[(u16, u16)] = &[
    (0x00C0, 0x00D6), (0x00D8, 0x00F6), (0x00F8, 0x0131), (0x0134, 0x013E), (0x0141, 0x0148), (0x014A, 0x017E),
    (0x0180, 0x01C3), (0x01CD, 0x01F0), (0x01F4, 0x01F5), (0x01FA, 0x0217), (0x0250, 0x02A8), (0x02BB, 0x02C1),
    (0x0386, 0x0386), (0x0388, 0x038A), (0x038C, 0x038C), (0x038E, 0x03A1), (0x03A3, 0x03CE), (0x03D0, 0x03D6),
    (0x03DA, 0x03DA), (0x03DC, 0x03DC), (0x03DE, 0x03DE), (0x03E0, 0x03E0), (0x03E2, 0x03F3), (0x0401, 0x040C),
    (0x040E, 0x044F), (0x0451, 0x045C), (0x045E, 0x0481), (0x0490, 0x04C4), (0x04C7, 0x04C8), (0x04CB, 0x04CC),
    (0x04D0, 0x04EB), (0x04EE, 0x04F5), (0x04F8, 0x04F9), (0x0531, 0x0556), (0x0559, 0x0559), (0x0561, 0x0586),
    (0x05D0, 0x05EA), (0x05F0, 0x05F2), (0x0621, 0x063A), (0x0641, 0x064A), (0x0671, 0x06B7), (0x06BA, 0x06BE),
    (0x06C0, 0x06CE), (0x06D0, 0x06D3), (0x06D5, 0x06D5), (0x06E5, 0x06E6), (0x0905, 0x0939), (0x093D, 0x093D),
    (0x0958, 0x0961), (0x0985, 0x098C), (0x098F, 0x0990), (0x0993, 0x09A8), (0x09AA, 0x09B0), (0x09B2, 0x09B2),
    (0x09B6, 0x09B9), (0x09DC, 0x09DD), (0x09DF, 0x09E1), (0x09F0, 0x09F1), (0x0A05, 0x0A0A), (0x0A0F, 0x0A10),
    (0x0A13, 0x0A28), (0x0A2A, 0x0A30), (0x0A32, 0x0A33), (0x0A35, 0x0A36), (0x0A38, 0x0A39), (0x0A59, 0x0A5C),
    (0x0A5E, 0x0A5E), (0x0A72, 0x0A74), (0x0A85, 0x0A8B), (0x0A8D, 0x0A8D), (0x0A8F, 0x0A91), (0x0A93, 0x0AA8),
    (0x0AAA, 0x0AB0), (0x0AB2, 0x0AB3), (0x0AB5, 0x0AB9), (0x0ABD, 0x0ABD), (0x0AE0, 0x0AE0), (0x0B05, 0x0B0C),
    (0x0B0F, 0x0B10), (0x0B13, 0x0B28), (0x0B2A, 0x0B30), (0x0B32, 0x0B33), (0x0B36, 0x0B39), (0x0B3D, 0x0B3D),
    (0x0B5C, 0x0B5D), (0x0B5F, 0x0B61), (0x0B85, 0x0B8A), (0x0B8E, 0x0B90), (0x0B92, 0x0B95), (0x0B99, 0x0B9A),
    (0x0B9C, 0x0B9C), (0x0B9E, 0x0B9F), (0x0BA3, 0x0BA4), (0x0BA8, 0x0BAA), (0x0BAE, 0x0BB5), (0x0BB7, 0x0BB9),
    (0x0C05, 0x0C0C), (0x0C0E, 0x0C10), (0x0C12, 0x0C28), (0x0C2A, 0x0C33), (0x0C35, 0x0C39), (0x0C60, 0x0C61),
    (0x0C85, 0x0C8C), (0x0C8E, 0x0C90), (0x0C92, 0x0CA8), (0x0CAA, 0x0CB3), (0x0CB5, 0x0CB9), (0x0CDE, 0x0CDE),
    (0x0CE0, 0x0CE1), (0x0D05, 0x0D0C), (0x0D0E, 0x0D10), (0x0D12, 0x0D28), (0x0D2A, 0x0D39), (0x0D60, 0x0D61),
    (0x0E01, 0x0E2E), (0x0E30, 0x0E30), (0x0E32, 0x0E33), (0x0E40, 0x0E45), (0x0E81, 0x0E82), (0x0E84, 0x0E84),
    (0x0E87, 0x0E88), (0x0E8A, 0x0E8A), (0x0E8D, 0x0E8D), (0x0E94, 0x0E97), (0x0E99, 0x0E9F), (0x0EA1, 0x0EA3),
    (0x0EA5, 0x0EA5), (0x0EA7, 0x0EA7), (0x0EAA, 0x0EAB), (0x0EAD, 0x0EAE), (0x0EB0, 0x0EB0), (0x0EB2, 0x0EB3),
    (0x0EBD, 0x0EBD), (0x0EC0, 0x0EC4), (0x0F40, 0x0F47), (0x0F49, 0x0F69), (0x10A0, 0x10C5), (0x10D0, 0x10F6),
    (0x1100, 0x1100), (0x1102, 0x1103), (0x1105, 0x1107), (0x1109, 0x1109), (0x110B, 0x110C), (0x110E, 0x1112),
    (0x113C, 0x113C), (0x113E, 0x113E), (0x1140, 0x1140), (0x114C, 0x114C), (0x114E, 0x114E), (0x1150, 0x1150),
    (0x1154, 0x1155), (0x1159, 0x1159), (0x115F, 0x1161), (0x1163, 0x1163), (0x1165, 0x1165), (0x1167, 0x1167),
    (0x1169, 0x1169), (0x116D, 0x116E), (0x1172, 0x1173), (0x1175, 0x1175), (0x119E, 0x119E), (0x11A8, 0x11A8),
    (0x11AB, 0x11AB), (0x11AE, 0x11AF), (0x11B7, 0x11B8), (0x11BA, 0x11BA), (0x11BC, 0x11C2), (0x11EB, 0x11EB),
    (0x11F0, 0x11F0), (0x11F9, 0x11F9), (0x1E00, 0x1E9B), (0x1EA0, 0x1EF9), (0x1F00, 0x1F15), (0x1F18, 0x1F1D),
    (0x1F20, 0x1F45), (0x1F48, 0x1F4D), (0x1F50, 0x1F57), (0x1F59, 0x1F59), (0x1F5B, 0x1F5B), (0x1F5D, 0x1F5D),
    (0x1F5F, 0x1F7D), (0x1F80, 0x1FB4), (0x1FB6, 0x1FBC), (0x1FBE, 0x1FBE), (0x1FC2, 0x1FC4), (0x1FC6, 0x1FCC),
    (0x1FD0, 0x1FD3), (0x1FD6, 0x1FDB), (0x1FE0, 0x1FEC), (0x1FF2, 0x1FF4), (0x1FF6, 0x1FFC), (0x2126, 0x2126),
    (0x212A, 0x212B), (0x212E, 0x212E), (0x2180, 0x2182), (0x3007, 0x3007), (0x3021, 0x3029), (0x3041, 0x3094),
    (0x30A1, 0x30FA), (0x3105, 0x312C), (0x4E00, 0x9FA5), (0xAC00, 0xD7A3),
];
/// expat's name characters above U+007F (name-start characters included).
const NAME_BMP: &[(u16, u16)] = &[
    (0x00B7, 0x00B7), (0x00C0, 0x00D6), (0x00D8, 0x00F6), (0x00F8, 0x0131), (0x0134, 0x013E), (0x0141, 0x0148),
    (0x014A, 0x017E), (0x0180, 0x01C3), (0x01CD, 0x01F0), (0x01F4, 0x01F5), (0x01FA, 0x0217), (0x0250, 0x02A8),
    (0x02BB, 0x02C1), (0x02D0, 0x02D1), (0x0300, 0x0345), (0x0360, 0x0361), (0x0386, 0x038A), (0x038C, 0x038C),
    (0x038E, 0x03A1), (0x03A3, 0x03CE), (0x03D0, 0x03D6), (0x03DA, 0x03DA), (0x03DC, 0x03DC), (0x03DE, 0x03DE),
    (0x03E0, 0x03E0), (0x03E2, 0x03F3), (0x0401, 0x040C), (0x040E, 0x044F), (0x0451, 0x045C), (0x045E, 0x0481),
    (0x0483, 0x0486), (0x0490, 0x04C4), (0x04C7, 0x04C8), (0x04CB, 0x04CC), (0x04D0, 0x04EB), (0x04EE, 0x04F5),
    (0x04F8, 0x04F9), (0x0531, 0x0556), (0x0559, 0x0559), (0x0561, 0x0586), (0x0591, 0x05A1), (0x05A3, 0x05B9),
    (0x05BB, 0x05BD), (0x05BF, 0x05BF), (0x05C1, 0x05C2), (0x05C4, 0x05C4), (0x05D0, 0x05EA), (0x05F0, 0x05F2),
    (0x0621, 0x063A), (0x0640, 0x0652), (0x0660, 0x0669), (0x0670, 0x06B7), (0x06BA, 0x06BE), (0x06C0, 0x06CE),
    (0x06D0, 0x06D3), (0x06D5, 0x06E8), (0x06EA, 0x06ED), (0x06F0, 0x06F9), (0x0901, 0x0903), (0x0905, 0x0939),
    (0x093C, 0x094D), (0x0951, 0x0954), (0x0958, 0x0963), (0x0966, 0x096F), (0x0981, 0x0983), (0x0985, 0x098C),
    (0x098F, 0x0990), (0x0993, 0x09A8), (0x09AA, 0x09B0), (0x09B2, 0x09B2), (0x09B6, 0x09B9), (0x09BC, 0x09BC),
    (0x09BE, 0x09C4), (0x09C7, 0x09C8), (0x09CB, 0x09CD), (0x09D7, 0x09D7), (0x09DC, 0x09DD), (0x09DF, 0x09E3),
    (0x09E6, 0x09F1), (0x0A02, 0x0A02), (0x0A05, 0x0A0A), (0x0A0F, 0x0A10), (0x0A13, 0x0A28), (0x0A2A, 0x0A30),
    (0x0A32, 0x0A33), (0x0A35, 0x0A36), (0x0A38, 0x0A39), (0x0A3C, 0x0A3C), (0x0A3E, 0x0A42), (0x0A47, 0x0A48),
    (0x0A4B, 0x0A4D), (0x0A59, 0x0A5C), (0x0A5E, 0x0A5E), (0x0A66, 0x0A74), (0x0A81, 0x0A83), (0x0A85, 0x0A8B),
    (0x0A8D, 0x0A8D), (0x0A8F, 0x0A91), (0x0A93, 0x0AA8), (0x0AAA, 0x0AB0), (0x0AB2, 0x0AB3), (0x0AB5, 0x0AB9),
    (0x0ABC, 0x0AC5), (0x0AC7, 0x0AC9), (0x0ACB, 0x0ACD), (0x0AE0, 0x0AE0), (0x0AE6, 0x0AEF), (0x0B01, 0x0B03),
    (0x0B05, 0x0B0C), (0x0B0F, 0x0B10), (0x0B13, 0x0B28), (0x0B2A, 0x0B30), (0x0B32, 0x0B33), (0x0B36, 0x0B39),
    (0x0B3C, 0x0B43), (0x0B47, 0x0B48), (0x0B4B, 0x0B4D), (0x0B56, 0x0B57), (0x0B5C, 0x0B5D), (0x0B5F, 0x0B61),
    (0x0B66, 0x0B6F), (0x0B82, 0x0B83), (0x0B85, 0x0B8A), (0x0B8E, 0x0B90), (0x0B92, 0x0B95), (0x0B99, 0x0B9A),
    (0x0B9C, 0x0B9C), (0x0B9E, 0x0B9F), (0x0BA3, 0x0BA4), (0x0BA8, 0x0BAA), (0x0BAE, 0x0BB5), (0x0BB7, 0x0BB9),
    (0x0BBE, 0x0BC2), (0x0BC6, 0x0BC8), (0x0BCA, 0x0BCD), (0x0BD7, 0x0BD7), (0x0BE7, 0x0BEF), (0x0C01, 0x0C03),
    (0x0C05, 0x0C0C), (0x0C0E, 0x0C10), (0x0C12, 0x0C28), (0x0C2A, 0x0C33), (0x0C35, 0x0C39), (0x0C3E, 0x0C44),
    (0x0C46, 0x0C48), (0x0C4A, 0x0C4D), (0x0C55, 0x0C56), (0x0C60, 0x0C61), (0x0C66, 0x0C6F), (0x0C82, 0x0C83),
    (0x0C85, 0x0C8C), (0x0C8E, 0x0C90), (0x0C92, 0x0CA8), (0x0CAA, 0x0CB3), (0x0CB5, 0x0CB9), (0x0CBE, 0x0CC4),
    (0x0CC6, 0x0CC8), (0x0CCA, 0x0CCD), (0x0CD5, 0x0CD6), (0x0CDE, 0x0CDE), (0x0CE0, 0x0CE1), (0x0CE6, 0x0CEF),
    (0x0D02, 0x0D03), (0x0D05, 0x0D0C), (0x0D0E, 0x0D10), (0x0D12, 0x0D28), (0x0D2A, 0x0D39), (0x0D3E, 0x0D43),
    (0x0D46, 0x0D48), (0x0D4A, 0x0D4D), (0x0D57, 0x0D57), (0x0D60, 0x0D61), (0x0D66, 0x0D6F), (0x0E01, 0x0E2E),
    (0x0E30, 0x0E3A), (0x0E40, 0x0E4E), (0x0E50, 0x0E59), (0x0E81, 0x0E82), (0x0E84, 0x0E84), (0x0E87, 0x0E88),
    (0x0E8A, 0x0E8A), (0x0E8D, 0x0E8D), (0x0E94, 0x0E97), (0x0E99, 0x0E9F), (0x0EA1, 0x0EA3), (0x0EA5, 0x0EA5),
    (0x0EA7, 0x0EA7), (0x0EAA, 0x0EAB), (0x0EAD, 0x0EAE), (0x0EB0, 0x0EB9), (0x0EBB, 0x0EBD), (0x0EC0, 0x0EC4),
    (0x0EC6, 0x0EC6), (0x0EC8, 0x0ECD), (0x0ED0, 0x0ED9), (0x0F18, 0x0F19), (0x0F20, 0x0F29), (0x0F35, 0x0F35),
    (0x0F37, 0x0F37), (0x0F39, 0x0F39), (0x0F3E, 0x0F47), (0x0F49, 0x0F69), (0x0F71, 0x0F84), (0x0F86, 0x0F8B),
    (0x0F90, 0x0F95), (0x0F97, 0x0F97), (0x0F99, 0x0FAD), (0x0FB1, 0x0FB7), (0x0FB9, 0x0FB9), (0x10A0, 0x10C5),
    (0x10D0, 0x10F6), (0x1100, 0x1100), (0x1102, 0x1103), (0x1105, 0x1107), (0x1109, 0x1109), (0x110B, 0x110C),
    (0x110E, 0x1112), (0x113C, 0x113C), (0x113E, 0x113E), (0x1140, 0x1140), (0x114C, 0x114C), (0x114E, 0x114E),
    (0x1150, 0x1150), (0x1154, 0x1155), (0x1159, 0x1159), (0x115F, 0x1161), (0x1163, 0x1163), (0x1165, 0x1165),
    (0x1167, 0x1167), (0x1169, 0x1169), (0x116D, 0x116E), (0x1172, 0x1173), (0x1175, 0x1175), (0x119E, 0x119E),
    (0x11A8, 0x11A8), (0x11AB, 0x11AB), (0x11AE, 0x11AF), (0x11B7, 0x11B8), (0x11BA, 0x11BA), (0x11BC, 0x11C2),
    (0x11EB, 0x11EB), (0x11F0, 0x11F0), (0x11F9, 0x11F9), (0x1E00, 0x1E9B), (0x1EA0, 0x1EF9), (0x1F00, 0x1F15),
    (0x1F18, 0x1F1D), (0x1F20, 0x1F45), (0x1F48, 0x1F4D), (0x1F50, 0x1F57), (0x1F59, 0x1F59), (0x1F5B, 0x1F5B),
    (0x1F5D, 0x1F5D), (0x1F5F, 0x1F7D), (0x1F80, 0x1FB4), (0x1FB6, 0x1FBC), (0x1FBE, 0x1FBE), (0x1FC2, 0x1FC4),
    (0x1FC6, 0x1FCC), (0x1FD0, 0x1FD3), (0x1FD6, 0x1FDB), (0x1FE0, 0x1FEC), (0x1FF2, 0x1FF4), (0x1FF6, 0x1FFC),
    (0x20D0, 0x20DC), (0x20E1, 0x20E1), (0x2126, 0x2126), (0x212A, 0x212B), (0x212E, 0x212E), (0x2180, 0x2182),
    (0x3005, 0x3005), (0x3007, 0x3007), (0x3021, 0x302F), (0x3031, 0x3035), (0x3041, 0x3094), (0x3099, 0x309A),
    (0x309D, 0x309E), (0x30A1, 0x30FA), (0x30FC, 0x30FE), (0x3105, 0x312C), (0x4E00, 0x9FA5), (0xAC00, 0xD7A3),
];
/// Code points from U+0080 that `str.isprintable()` rejects, so `repr()` escapes them.
const NONPRINTABLE: &[(u32, u32)] = &[
    (0x80, 0xA0), (0xAD, 0xAD), (0x378, 0x379), (0x380, 0x383), (0x38B, 0x38B),
    (0x38D, 0x38D), (0x3A2, 0x3A2), (0x530, 0x530), (0x557, 0x558), (0x58B, 0x58C),
    (0x590, 0x590), (0x5C8, 0x5CF), (0x5EB, 0x5EE), (0x5F5, 0x605), (0x61C, 0x61C),
    (0x6DD, 0x6DD), (0x70E, 0x70F), (0x74B, 0x74C), (0x7B2, 0x7BF), (0x7FB, 0x7FC),
    (0x82E, 0x82F), (0x83F, 0x83F), (0x85C, 0x85D), (0x85F, 0x85F), (0x86B, 0x86F),
    (0x88F, 0x896), (0x8E2, 0x8E2), (0x984, 0x984), (0x98D, 0x98E), (0x991, 0x992),
    (0x9A9, 0x9A9), (0x9B1, 0x9B1), (0x9B3, 0x9B5), (0x9BA, 0x9BB), (0x9C5, 0x9C6),
    (0x9C9, 0x9CA), (0x9CF, 0x9D6), (0x9D8, 0x9DB), (0x9DE, 0x9DE), (0x9E4, 0x9E5),
    (0x9FF, 0xA00), (0xA04, 0xA04), (0xA0B, 0xA0E), (0xA11, 0xA12), (0xA29, 0xA29),
    (0xA31, 0xA31), (0xA34, 0xA34), (0xA37, 0xA37), (0xA3A, 0xA3B), (0xA3D, 0xA3D),
    (0xA43, 0xA46), (0xA49, 0xA4A), (0xA4E, 0xA50), (0xA52, 0xA58), (0xA5D, 0xA5D),
    (0xA5F, 0xA65), (0xA77, 0xA80), (0xA84, 0xA84), (0xA8E, 0xA8E), (0xA92, 0xA92),
    (0xAA9, 0xAA9), (0xAB1, 0xAB1), (0xAB4, 0xAB4), (0xABA, 0xABB), (0xAC6, 0xAC6),
    (0xACA, 0xACA), (0xACE, 0xACF), (0xAD1, 0xADF), (0xAE4, 0xAE5), (0xAF2, 0xAF8),
    (0xB00, 0xB00), (0xB04, 0xB04), (0xB0D, 0xB0E), (0xB11, 0xB12), (0xB29, 0xB29),
    (0xB31, 0xB31), (0xB34, 0xB34), (0xB3A, 0xB3B), (0xB45, 0xB46), (0xB49, 0xB4A),
    (0xB4E, 0xB54), (0xB58, 0xB5B), (0xB5E, 0xB5E), (0xB64, 0xB65), (0xB78, 0xB81),
    (0xB84, 0xB84), (0xB8B, 0xB8D), (0xB91, 0xB91), (0xB96, 0xB98), (0xB9B, 0xB9B),
    (0xB9D, 0xB9D), (0xBA0, 0xBA2), (0xBA5, 0xBA7), (0xBAB, 0xBAD), (0xBBA, 0xBBD),
    (0xBC3, 0xBC5), (0xBC9, 0xBC9), (0xBCE, 0xBCF), (0xBD1, 0xBD6), (0xBD8, 0xBE5),
    (0xBFB, 0xBFF), (0xC0D, 0xC0D), (0xC11, 0xC11), (0xC29, 0xC29), (0xC3A, 0xC3B),
    (0xC45, 0xC45), (0xC49, 0xC49), (0xC4E, 0xC54), (0xC57, 0xC57), (0xC5B, 0xC5C),
    (0xC5E, 0xC5F), (0xC64, 0xC65), (0xC70, 0xC76), (0xC8D, 0xC8D), (0xC91, 0xC91),
    (0xCA9, 0xCA9), (0xCB4, 0xCB4), (0xCBA, 0xCBB), (0xCC5, 0xCC5), (0xCC9, 0xCC9),
    (0xCCE, 0xCD4), (0xCD7, 0xCDC), (0xCDF, 0xCDF), (0xCE4, 0xCE5), (0xCF0, 0xCF0),
    (0xCF4, 0xCFF), (0xD0D, 0xD0D), (0xD11, 0xD11), (0xD45, 0xD45), (0xD49, 0xD49),
    (0xD50, 0xD53), (0xD64, 0xD65), (0xD80, 0xD80), (0xD84, 0xD84), (0xD97, 0xD99),
    (0xDB2, 0xDB2), (0xDBC, 0xDBC), (0xDBE, 0xDBF), (0xDC7, 0xDC9), (0xDCB, 0xDCE),
    (0xDD5, 0xDD5), (0xDD7, 0xDD7), (0xDE0, 0xDE5), (0xDF0, 0xDF1), (0xDF5, 0xE00),
    (0xE3B, 0xE3E), (0xE5C, 0xE80), (0xE83, 0xE83), (0xE85, 0xE85), (0xE8B, 0xE8B),
    (0xEA4, 0xEA4), (0xEA6, 0xEA6), (0xEBE, 0xEBF), (0xEC5, 0xEC5), (0xEC7, 0xEC7),
    (0xECF, 0xECF), (0xEDA, 0xEDB), (0xEE0, 0xEFF), (0xF48, 0xF48), (0xF6D, 0xF70),
    (0xF98, 0xF98), (0xFBD, 0xFBD), (0xFCD, 0xFCD), (0xFDB, 0xFFF), (0x10C6, 0x10C6),
    (0x10C8, 0x10CC), (0x10CE, 0x10CF), (0x1249, 0x1249), (0x124E, 0x124F), (0x1257, 0x1257),
    (0x1259, 0x1259), (0x125E, 0x125F), (0x1289, 0x1289), (0x128E, 0x128F), (0x12B1, 0x12B1),
    (0x12B6, 0x12B7), (0x12BF, 0x12BF), (0x12C1, 0x12C1), (0x12C6, 0x12C7), (0x12D7, 0x12D7),
    (0x1311, 0x1311), (0x1316, 0x1317), (0x135B, 0x135C), (0x137D, 0x137F), (0x139A, 0x139F),
    (0x13F6, 0x13F7), (0x13FE, 0x13FF), (0x1680, 0x1680), (0x169D, 0x169F), (0x16F9, 0x16FF),
    (0x1716, 0x171E), (0x1737, 0x173F), (0x1754, 0x175F), (0x176D, 0x176D), (0x1771, 0x1771),
    (0x1774, 0x177F), (0x17DE, 0x17DF), (0x17EA, 0x17EF), (0x17FA, 0x17FF), (0x180E, 0x180E),
    (0x181A, 0x181F), (0x1879, 0x187F), (0x18AB, 0x18AF), (0x18F6, 0x18FF), (0x191F, 0x191F),
    (0x192C, 0x192F), (0x193C, 0x193F), (0x1941, 0x1943), (0x196E, 0x196F), (0x1975, 0x197F),
    (0x19AC, 0x19AF), (0x19CA, 0x19CF), (0x19DB, 0x19DD), (0x1A1C, 0x1A1D), (0x1A5F, 0x1A5F),
    (0x1A7D, 0x1A7E), (0x1A8A, 0x1A8F), (0x1A9A, 0x1A9F), (0x1AAE, 0x1AAF), (0x1ACF, 0x1AFF),
    (0x1B4D, 0x1B4D), (0x1BF4, 0x1BFB), (0x1C38, 0x1C3A), (0x1C4A, 0x1C4C), (0x1C8B, 0x1C8F),
    (0x1CBB, 0x1CBC), (0x1CC8, 0x1CCF), (0x1CFB, 0x1CFF), (0x1F16, 0x1F17), (0x1F1E, 0x1F1F),
    (0x1F46, 0x1F47), (0x1F4E, 0x1F4F), (0x1F58, 0x1F58), (0x1F5A, 0x1F5A), (0x1F5C, 0x1F5C),
    (0x1F5E, 0x1F5E), (0x1F7E, 0x1F7F), (0x1FB5, 0x1FB5), (0x1FC5, 0x1FC5), (0x1FD4, 0x1FD5),
    (0x1FDC, 0x1FDC), (0x1FF0, 0x1FF1), (0x1FF5, 0x1FF5), (0x1FFF, 0x200F), (0x2028, 0x202F),
    (0x205F, 0x206F), (0x2072, 0x2073), (0x208F, 0x208F), (0x209D, 0x209F), (0x20C1, 0x20CF),
    (0x20F1, 0x20FF), (0x218C, 0x218F), (0x242A, 0x243F), (0x244B, 0x245F), (0x2B74, 0x2B75),
    (0x2B96, 0x2B96), (0x2CF4, 0x2CF8), (0x2D26, 0x2D26), (0x2D28, 0x2D2C), (0x2D2E, 0x2D2F),
    (0x2D68, 0x2D6E), (0x2D71, 0x2D7E), (0x2D97, 0x2D9F), (0x2DA7, 0x2DA7), (0x2DAF, 0x2DAF),
    (0x2DB7, 0x2DB7), (0x2DBF, 0x2DBF), (0x2DC7, 0x2DC7), (0x2DCF, 0x2DCF), (0x2DD7, 0x2DD7),
    (0x2DDF, 0x2DDF), (0x2E5E, 0x2E7F), (0x2E9A, 0x2E9A), (0x2EF4, 0x2EFF), (0x2FD6, 0x2FEF),
    (0x3000, 0x3000), (0x3040, 0x3040), (0x3097, 0x3098), (0x3100, 0x3104), (0x3130, 0x3130),
    (0x318F, 0x318F), (0x31E6, 0x31EE), (0x321F, 0x321F), (0xA48D, 0xA48F), (0xA4C7, 0xA4CF),
    (0xA62C, 0xA63F), (0xA6F8, 0xA6FF), (0xA7CE, 0xA7CF), (0xA7D2, 0xA7D2), (0xA7D4, 0xA7D4),
    (0xA7DD, 0xA7F1), (0xA82D, 0xA82F), (0xA83A, 0xA83F), (0xA878, 0xA87F), (0xA8C6, 0xA8CD),
    (0xA8DA, 0xA8DF), (0xA954, 0xA95E), (0xA97D, 0xA97F), (0xA9CE, 0xA9CE), (0xA9DA, 0xA9DD),
    (0xA9FF, 0xA9FF), (0xAA37, 0xAA3F), (0xAA4E, 0xAA4F), (0xAA5A, 0xAA5B), (0xAAC3, 0xAADA),
    (0xAAF7, 0xAB00), (0xAB07, 0xAB08), (0xAB0F, 0xAB10), (0xAB17, 0xAB1F), (0xAB27, 0xAB27),
    (0xAB2F, 0xAB2F), (0xAB6C, 0xAB6F), (0xABEE, 0xABEF), (0xABFA, 0xABFF), (0xD7A4, 0xD7AF),
    (0xD7C7, 0xD7CA), (0xD7FC, 0xF8FF), (0xFA6E, 0xFA6F), (0xFADA, 0xFAFF), (0xFB07, 0xFB12),
    (0xFB18, 0xFB1C), (0xFB37, 0xFB37), (0xFB3D, 0xFB3D), (0xFB3F, 0xFB3F), (0xFB42, 0xFB42),
    (0xFB45, 0xFB45), (0xFBC3, 0xFBD2), (0xFD90, 0xFD91), (0xFDC8, 0xFDCE), (0xFDD0, 0xFDEF),
    (0xFE1A, 0xFE1F), (0xFE53, 0xFE53), (0xFE67, 0xFE67), (0xFE6C, 0xFE6F), (0xFE75, 0xFE75),
    (0xFEFD, 0xFF00), (0xFFBF, 0xFFC1), (0xFFC8, 0xFFC9), (0xFFD0, 0xFFD1), (0xFFD8, 0xFFD9),
    (0xFFDD, 0xFFDF), (0xFFE7, 0xFFE7), (0xFFEF, 0xFFFB), (0xFFFE, 0xFFFF), (0x1000C, 0x1000C),
    (0x10027, 0x10027), (0x1003B, 0x1003B), (0x1003E, 0x1003E), (0x1004E, 0x1004F), (0x1005E, 0x1007F),
    (0x100FB, 0x100FF), (0x10103, 0x10106), (0x10134, 0x10136), (0x1018F, 0x1018F), (0x1019D, 0x1019F),
    (0x101A1, 0x101CF), (0x101FE, 0x1027F), (0x1029D, 0x1029F), (0x102D1, 0x102DF), (0x102FC, 0x102FF),
    (0x10324, 0x1032C), (0x1034B, 0x1034F), (0x1037B, 0x1037F), (0x1039E, 0x1039E), (0x103C4, 0x103C7),
    (0x103D6, 0x103FF), (0x1049E, 0x1049F), (0x104AA, 0x104AF), (0x104D4, 0x104D7), (0x104FC, 0x104FF),
    (0x10528, 0x1052F), (0x10564, 0x1056E), (0x1057B, 0x1057B), (0x1058B, 0x1058B), (0x10593, 0x10593),
    (0x10596, 0x10596), (0x105A2, 0x105A2), (0x105B2, 0x105B2), (0x105BA, 0x105BA), (0x105BD, 0x105BF),
    (0x105F4, 0x105FF), (0x10737, 0x1073F), (0x10756, 0x1075F), (0x10768, 0x1077F), (0x10786, 0x10786),
    (0x107B1, 0x107B1), (0x107BB, 0x107FF), (0x10806, 0x10807), (0x10809, 0x10809), (0x10836, 0x10836),
    (0x10839, 0x1083B), (0x1083D, 0x1083E), (0x10856, 0x10856), (0x1089F, 0x108A6), (0x108B0, 0x108DF),
    (0x108F3, 0x108F3), (0x108F6, 0x108FA), (0x1091C, 0x1091E), (0x1093A, 0x1093E), (0x10940, 0x1097F),
    (0x109B8, 0x109BB), (0x109D0, 0x109D1), (0x10A04, 0x10A04), (0x10A07, 0x10A0B), (0x10A14, 0x10A14),
    (0x10A18, 0x10A18), (0x10A36, 0x10A37), (0x10A3B, 0x10A3E), (0x10A49, 0x10A4F), (0x10A59, 0x10A5F),
    (0x10AA0, 0x10ABF), (0x10AE7, 0x10AEA), (0x10AF7, 0x10AFF), (0x10B36, 0x10B38), (0x10B56, 0x10B57),
    (0x10B73, 0x10B77), (0x10B92, 0x10B98), (0x10B9D, 0x10BA8), (0x10BB0, 0x10BFF), (0x10C49, 0x10C7F),
    (0x10CB3, 0x10CBF), (0x10CF3, 0x10CF9), (0x10D28, 0x10D2F), (0x10D3A, 0x10D3F), (0x10D66, 0x10D68),
    (0x10D86, 0x10D8D), (0x10D90, 0x10E5F), (0x10E7F, 0x10E7F), (0x10EAA, 0x10EAA), (0x10EAE, 0x10EAF),
    (0x10EB2, 0x10EC1), (0x10EC5, 0x10EFB), (0x10F28, 0x10F2F), (0x10F5A, 0x10F6F), (0x10F8A, 0x10FAF),
    (0x10FCC, 0x10FDF), (0x10FF7, 0x10FFF), (0x1104E, 0x11051), (0x11076, 0x1107E), (0x110BD, 0x110BD),
    (0x110C3, 0x110CF), (0x110E9, 0x110EF), (0x110FA, 0x110FF), (0x11135, 0x11135), (0x11148, 0x1114F),
    (0x11177, 0x1117F), (0x111E0, 0x111E0), (0x111F5, 0x111FF), (0x11212, 0x11212), (0x11242, 0x1127F),
    (0x11287, 0x11287), (0x11289, 0x11289), (0x1128E, 0x1128E), (0x1129E, 0x1129E), (0x112AA, 0x112AF),
    (0x112EB, 0x112EF), (0x112FA, 0x112FF), (0x11304, 0x11304), (0x1130D, 0x1130E), (0x11311, 0x11312),
    (0x11329, 0x11329), (0x11331, 0x11331), (0x11334, 0x11334), (0x1133A, 0x1133A), (0x11345, 0x11346),
    (0x11349, 0x1134A), (0x1134E, 0x1134F), (0x11351, 0x11356), (0x11358, 0x1135C), (0x11364, 0x11365),
    (0x1136D, 0x1136F), (0x11375, 0x1137F), (0x1138A, 0x1138A), (0x1138C, 0x1138D), (0x1138F, 0x1138F),
    (0x113B6, 0x113B6), (0x113C1, 0x113C1), (0x113C3, 0x113C4), (0x113C6, 0x113C6), (0x113CB, 0x113CB),
    (0x113D6, 0x113D6), (0x113D9, 0x113E0), (0x113E3, 0x113FF), (0x1145C, 0x1145C), (0x11462, 0x1147F),
    (0x114C8, 0x114CF), (0x114DA, 0x1157F), (0x115B6, 0x115B7), (0x115DE, 0x115FF), (0x11645, 0x1164F),
    (0x1165A, 0x1165F), (0x1166D, 0x1167F), (0x116BA, 0x116BF), (0x116CA, 0x116CF), (0x116E4, 0x116FF),
    (0x1171B, 0x1171C), (0x1172C, 0x1172F), (0x11747, 0x117FF), (0x1183C, 0x1189F), (0x118F3, 0x118FE),
    (0x11907, 0x11908), (0x1190A, 0x1190B), (0x11914, 0x11914), (0x11917, 0x11917), (0x11936, 0x11936),
    (0x11939, 0x1193A), (0x11947, 0x1194F), (0x1195A, 0x1199F), (0x119A8, 0x119A9), (0x119D8, 0x119D9),
    (0x119E5, 0x119FF), (0x11A48, 0x11A4F), (0x11AA3, 0x11AAF), (0x11AF9, 0x11AFF), (0x11B0A, 0x11BBF),
    (0x11BE2, 0x11BEF), (0x11BFA, 0x11BFF), (0x11C09, 0x11C09), (0x11C37, 0x11C37), (0x11C46, 0x11C4F),
    (0x11C6D, 0x11C6F), (0x11C90, 0x11C91), (0x11CA8, 0x11CA8), (0x11CB7, 0x11CFF), (0x11D07, 0x11D07),
    (0x11D0A, 0x11D0A), (0x11D37, 0x11D39), (0x11D3B, 0x11D3B), (0x11D3E, 0x11D3E), (0x11D48, 0x11D4F),
    (0x11D5A, 0x11D5F), (0x11D66, 0x11D66), (0x11D69, 0x11D69), (0x11D8F, 0x11D8F), (0x11D92, 0x11D92),
    (0x11D99, 0x11D9F), (0x11DAA, 0x11EDF), (0x11EF9, 0x11EFF), (0x11F11, 0x11F11), (0x11F3B, 0x11F3D),
    (0x11F5B, 0x11FAF), (0x11FB1, 0x11FBF), (0x11FF2, 0x11FFE), (0x1239A, 0x123FF), (0x1246F, 0x1246F),
    (0x12475, 0x1247F), (0x12544, 0x12F8F), (0x12FF3, 0x12FFF), (0x13430, 0x1343F), (0x13456, 0x1345F),
    (0x143FB, 0x143FF), (0x14647, 0x160FF), (0x1613A, 0x167FF), (0x16A39, 0x16A3F), (0x16A5F, 0x16A5F),
    (0x16A6A, 0x16A6D), (0x16ABF, 0x16ABF), (0x16ACA, 0x16ACF), (0x16AEE, 0x16AEF), (0x16AF6, 0x16AFF),
    (0x16B46, 0x16B4F), (0x16B5A, 0x16B5A), (0x16B62, 0x16B62), (0x16B78, 0x16B7C), (0x16B90, 0x16D3F),
    (0x16D7A, 0x16E3F), (0x16E9B, 0x16EFF), (0x16F4B, 0x16F4E), (0x16F88, 0x16F8E), (0x16FA0, 0x16FDF),
    (0x16FE5, 0x16FEF), (0x16FF2, 0x16FFF), (0x187F8, 0x187FF), (0x18CD6, 0x18CFE), (0x18D09, 0x1AFEF),
    (0x1AFF4, 0x1AFF4), (0x1AFFC, 0x1AFFC), (0x1AFFF, 0x1AFFF), (0x1B123, 0x1B131), (0x1B133, 0x1B14F),
    (0x1B153, 0x1B154), (0x1B156, 0x1B163), (0x1B168, 0x1B16F), (0x1B2FC, 0x1BBFF), (0x1BC6B, 0x1BC6F),
    (0x1BC7D, 0x1BC7F), (0x1BC89, 0x1BC8F), (0x1BC9A, 0x1BC9B), (0x1BCA0, 0x1CBFF), (0x1CCFA, 0x1CCFF),
    (0x1CEB4, 0x1CEFF), (0x1CF2E, 0x1CF2F), (0x1CF47, 0x1CF4F), (0x1CFC4, 0x1CFFF), (0x1D0F6, 0x1D0FF),
    (0x1D127, 0x1D128), (0x1D173, 0x1D17A), (0x1D1EB, 0x1D1FF), (0x1D246, 0x1D2BF), (0x1D2D4, 0x1D2DF),
    (0x1D2F4, 0x1D2FF), (0x1D357, 0x1D35F), (0x1D379, 0x1D3FF), (0x1D455, 0x1D455), (0x1D49D, 0x1D49D),
    (0x1D4A0, 0x1D4A1), (0x1D4A3, 0x1D4A4), (0x1D4A7, 0x1D4A8), (0x1D4AD, 0x1D4AD), (0x1D4BA, 0x1D4BA),
    (0x1D4BC, 0x1D4BC), (0x1D4C4, 0x1D4C4), (0x1D506, 0x1D506), (0x1D50B, 0x1D50C), (0x1D515, 0x1D515),
    (0x1D51D, 0x1D51D), (0x1D53A, 0x1D53A), (0x1D53F, 0x1D53F), (0x1D545, 0x1D545), (0x1D547, 0x1D549),
    (0x1D551, 0x1D551), (0x1D6A6, 0x1D6A7), (0x1D7CC, 0x1D7CD), (0x1DA8C, 0x1DA9A), (0x1DAA0, 0x1DAA0),
    (0x1DAB0, 0x1DEFF), (0x1DF1F, 0x1DF24), (0x1DF2B, 0x1DFFF), (0x1E007, 0x1E007), (0x1E019, 0x1E01A),
    (0x1E022, 0x1E022), (0x1E025, 0x1E025), (0x1E02B, 0x1E02F), (0x1E06E, 0x1E08E), (0x1E090, 0x1E0FF),
    (0x1E12D, 0x1E12F), (0x1E13E, 0x1E13F), (0x1E14A, 0x1E14D), (0x1E150, 0x1E28F), (0x1E2AF, 0x1E2BF),
    (0x1E2FA, 0x1E2FE), (0x1E300, 0x1E4CF), (0x1E4FA, 0x1E5CF), (0x1E5FB, 0x1E5FE), (0x1E600, 0x1E7DF),
    (0x1E7E7, 0x1E7E7), (0x1E7EC, 0x1E7EC), (0x1E7EF, 0x1E7EF), (0x1E7FF, 0x1E7FF), (0x1E8C5, 0x1E8C6),
    (0x1E8D7, 0x1E8FF), (0x1E94C, 0x1E94F), (0x1E95A, 0x1E95D), (0x1E960, 0x1EC70), (0x1ECB5, 0x1ED00),
    (0x1ED3E, 0x1EDFF), (0x1EE04, 0x1EE04), (0x1EE20, 0x1EE20), (0x1EE23, 0x1EE23), (0x1EE25, 0x1EE26),
    (0x1EE28, 0x1EE28), (0x1EE33, 0x1EE33), (0x1EE38, 0x1EE38), (0x1EE3A, 0x1EE3A), (0x1EE3C, 0x1EE41),
    (0x1EE43, 0x1EE46), (0x1EE48, 0x1EE48), (0x1EE4A, 0x1EE4A), (0x1EE4C, 0x1EE4C), (0x1EE50, 0x1EE50),
    (0x1EE53, 0x1EE53), (0x1EE55, 0x1EE56), (0x1EE58, 0x1EE58), (0x1EE5A, 0x1EE5A), (0x1EE5C, 0x1EE5C),
    (0x1EE5E, 0x1EE5E), (0x1EE60, 0x1EE60), (0x1EE63, 0x1EE63), (0x1EE65, 0x1EE66), (0x1EE6B, 0x1EE6B),
    (0x1EE73, 0x1EE73), (0x1EE78, 0x1EE78), (0x1EE7D, 0x1EE7D), (0x1EE7F, 0x1EE7F), (0x1EE8A, 0x1EE8A),
    (0x1EE9C, 0x1EEA0), (0x1EEA4, 0x1EEA4), (0x1EEAA, 0x1EEAA), (0x1EEBC, 0x1EEEF), (0x1EEF2, 0x1EFFF),
    (0x1F02C, 0x1F02F), (0x1F094, 0x1F09F), (0x1F0AF, 0x1F0B0), (0x1F0C0, 0x1F0C0), (0x1F0D0, 0x1F0D0),
    (0x1F0F6, 0x1F0FF), (0x1F1AE, 0x1F1E5), (0x1F203, 0x1F20F), (0x1F23C, 0x1F23F), (0x1F249, 0x1F24F),
    (0x1F252, 0x1F25F), (0x1F266, 0x1F2FF), (0x1F6D8, 0x1F6DB), (0x1F6ED, 0x1F6EF), (0x1F6FD, 0x1F6FF),
    (0x1F777, 0x1F77A), (0x1F7DA, 0x1F7DF), (0x1F7EC, 0x1F7EF), (0x1F7F1, 0x1F7FF), (0x1F80C, 0x1F80F),
    (0x1F848, 0x1F84F), (0x1F85A, 0x1F85F), (0x1F888, 0x1F88F), (0x1F8AE, 0x1F8AF), (0x1F8BC, 0x1F8BF),
    (0x1F8C2, 0x1F8FF), (0x1FA54, 0x1FA5F), (0x1FA6E, 0x1FA6F), (0x1FA7D, 0x1FA7F), (0x1FA8A, 0x1FA8E),
    (0x1FAC7, 0x1FACD), (0x1FADD, 0x1FADE), (0x1FAEA, 0x1FAEF), (0x1FAF9, 0x1FAFF), (0x1FB93, 0x1FB93),
    (0x1FBFA, 0x1FFFF), (0x2A6E0, 0x2A6FF), (0x2B73A, 0x2B73F), (0x2B81E, 0x2B81F), (0x2CEA2, 0x2CEAF),
    (0x2EBE1, 0x2EBEF), (0x2EE5E, 0x2F7FF), (0x2FA1E, 0x2FFFF), (0x3134B, 0x3134F), (0x323B0, 0xE00FF),
    (0xE01F0, 0x10FFFF),
];
/// `encodings.aliases.aliases`: normalised alias -> codec module.
const CODEC_ALIASES: &[(&str, &str)] = &[
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
    ("csibm01140", "cp1140"), ("csibm037", "cp037"), ("csibm1026", "cp1026"), ("csibm273", "cp273"),
    ("csibm424", "cp424"), ("csibm500", "cp500"), ("csibm855", "cp855"), ("csibm857", "cp857"),
    ("csibm858", "cp858"), ("csibm860", "cp860"), ("csibm861", "cp861"), ("csibm863", "cp863"),
    ("csibm864", "cp864"), ("csibm865", "cp865"), ("csibm866", "cp866"), ("csibm869", "cp869"),
    ("csiso2022jp", "iso2022_jp"), ("csiso2022kr", "iso2022_kr"), ("csiso58gb231280", "gb2312"),
    ("csisolatin1", "latin_1"), ("csisolatin2", "iso8859_2"), ("csisolatin3", "iso8859_3"),
    ("csisolatin4", "iso8859_4"), ("csisolatin5", "iso8859_9"), ("csisolatin6", "iso8859_10"),
    ("csisolatinarabic", "iso8859_6"), ("csisolatincyrillic", "iso8859_5"),
    ("csisolatingreek", "iso8859_7"), ("csisolatinhebrew", "iso8859_8"), ("cskoi8r", "koi8_r"),
    ("cspc775baltic", "cp775"), ("cspc850multilingual", "cp850"), ("cspc862latinhebrew", "cp862"),
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
    ("iso_8859_16_2001", "iso8859_16"), ("iso_8859_1_1987", "latin_1"), ("iso_8859_2", "iso8859_2"),
    ("iso_8859_2_1987", "iso8859_2"), ("iso_8859_3", "iso8859_3"), ("iso_8859_3_1988", "iso8859_3"),
    ("iso_8859_4", "iso8859_4"), ("iso_8859_4_1988", "iso8859_4"), ("iso_8859_5", "iso8859_5"),
    ("iso_8859_5_1988", "iso8859_5"), ("iso_8859_6", "iso8859_6"), ("iso_8859_6_1987", "iso8859_6"),
    ("iso_8859_7", "iso8859_7"), ("iso_8859_7_1987", "iso8859_7"), ("iso_8859_8", "iso8859_8"),
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
    ("latin", "latin_1"), ("latin1", "latin_1"), ("latin10", "iso8859_16"), ("latin2", "iso8859_2"),
    ("latin3", "iso8859_3"), ("latin4", "iso8859_4"), ("latin5", "iso8859_9"),
    ("latin6", "iso8859_10"), ("latin7", "iso8859_13"), ("latin8", "iso8859_14"),
    ("latin9", "iso8859_15"), ("mac_centeuro", "mac_latin2"), ("maccentraleurope", "mac_latin2"),
    ("maccyrillic", "mac_cyrillic"), ("macgreek", "mac_greek"), ("maciceland", "mac_iceland"),
    ("macintosh", "mac_roman"), ("maclatin2", "mac_latin2"), ("macroman", "mac_roman"),
    ("macturkish", "mac_turkish"), ("ms1361", "johab"), ("ms874", "cp874"), ("ms932", "cp932"),
    ("ms936", "gbk"), ("ms949", "cp949"), ("ms950", "cp950"), ("ms_kanji", "cp932"),
    ("mskanji", "cp932"), ("pc_multilingual_850_euro", "cp858"), ("pt154", "ptcp154"),
    ("quopri", "quopri_codec"), ("quoted_printable", "quopri_codec"),
    ("quotedprintable", "quopri_codec"), ("r8", "hp_roman8"), ("rk1048", "kz1048"),
    ("roman8", "hp_roman8"), ("rot13", "rot_13"), ("ruscii", "cp1125"), ("s_jis", "shift_jis"),
    ("s_jis_2004", "shift_jis_2004"), ("s_jisx0213", "shift_jisx0213"), ("shiftjis", "shift_jis"),
    ("shiftjis2004", "shift_jis_2004"), ("shiftjisx0213", "shift_jisx0213"), ("sjis", "shift_jis"),
    ("sjis_2004", "shift_jis_2004"), ("sjisx0213", "shift_jisx0213"), ("strk1048_2002", "kz1048"),
    ("thai", "iso8859_11"), ("tis620", "tis_620"), ("tis_620_0", "tis_620"),
    ("tis_620_2529_0", "tis_620"), ("tis_620_2529_1", "tis_620"), ("u16", "utf_16"),
    ("u32", "utf_32"), ("u7", "utf_7"), ("u8", "utf_8"), ("u_jis", "euc_jp"), ("uhc", "cp949"),
    ("ujis", "euc_jp"), ("unicode_1_1_utf_7", "utf_7"), ("unicodebigunmarked", "utf_16_be"),
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
/// Each module of the `encodings` package, and what pyexpat's unknown-encoding
/// handler makes of it: a byte map, an encoding expat refuses, or a Python error.
const CODEC_MODULES: &[(&str, Codec)] = &[
    ("aliases", Codec::Raise("LookupError")), ("ascii", Codec::Map(0)),
    ("base64_codec", Codec::Raise("LookupError")), ("big5", Codec::Raise("ValueError")),
    ("big5hkscs", Codec::Raise("ValueError")), ("bz2_codec", Codec::Raise("LookupError")),
    ("charmap", Codec::Map(1)), ("cp037", Codec::Unusable), ("cp1006", Codec::Map(2)),
    ("cp1026", Codec::Unusable), ("cp1125", Codec::Map(3)), ("cp1140", Codec::Unusable),
    ("cp1250", Codec::Map(4)), ("cp1251", Codec::Map(5)), ("cp1252", Codec::Map(6)),
    ("cp1253", Codec::Map(7)), ("cp1254", Codec::Map(8)), ("cp1255", Codec::Map(9)),
    ("cp1256", Codec::Map(10)), ("cp1257", Codec::Map(11)), ("cp1258", Codec::Map(12)),
    ("cp273", Codec::Unusable), ("cp424", Codec::Unusable), ("cp437", Codec::Map(13)),
    ("cp500", Codec::Unusable), ("cp720", Codec::Map(14)), ("cp737", Codec::Map(15)),
    ("cp775", Codec::Map(16)), ("cp850", Codec::Map(17)), ("cp852", Codec::Map(18)),
    ("cp855", Codec::Map(19)), ("cp856", Codec::Map(20)), ("cp857", Codec::Map(21)),
    ("cp858", Codec::Map(22)), ("cp860", Codec::Map(23)), ("cp861", Codec::Map(24)),
    ("cp862", Codec::Map(25)), ("cp863", Codec::Map(26)), ("cp864", Codec::Unusable),
    ("cp865", Codec::Map(27)), ("cp866", Codec::Map(28)), ("cp869", Codec::Map(29)),
    ("cp874", Codec::Map(30)), ("cp875", Codec::Unusable), ("cp932", Codec::Raise("ValueError")),
    ("cp949", Codec::Raise("ValueError")), ("cp950", Codec::Raise("ValueError")),
    ("euc_jis_2004", Codec::Raise("ValueError")), ("euc_jisx0213", Codec::Raise("ValueError")),
    ("euc_jp", Codec::Raise("ValueError")), ("euc_kr", Codec::Raise("ValueError")),
    ("gb18030", Codec::Raise("ValueError")), ("gb2312", Codec::Raise("ValueError")),
    ("gbk", Codec::Raise("ValueError")), ("hex_codec", Codec::Raise("LookupError")),
    ("hp_roman8", Codec::Map(31)), ("hz", Codec::Map(32)), ("idna", Codec::Raise("UnicodeError")),
    ("iso2022_jp", Codec::Map(0)), ("iso2022_jp_1", Codec::Map(0)), ("iso2022_jp_2", Codec::Map(0)),
    ("iso2022_jp_2004", Codec::Map(0)), ("iso2022_jp_3", Codec::Map(0)),
    ("iso2022_jp_ext", Codec::Map(0)), ("iso2022_kr", Codec::Raise("ValueError")),
    ("iso8859_1", Codec::Map(1)), ("iso8859_10", Codec::Map(33)), ("iso8859_11", Codec::Map(34)),
    ("iso8859_13", Codec::Map(35)), ("iso8859_14", Codec::Map(36)), ("iso8859_15", Codec::Map(37)),
    ("iso8859_16", Codec::Map(38)), ("iso8859_2", Codec::Map(39)), ("iso8859_3", Codec::Map(40)),
    ("iso8859_4", Codec::Map(41)), ("iso8859_5", Codec::Map(42)), ("iso8859_6", Codec::Map(43)),
    ("iso8859_7", Codec::Map(44)), ("iso8859_8", Codec::Map(45)), ("iso8859_9", Codec::Map(46)),
    ("johab", Codec::Raise("ValueError")), ("koi8_r", Codec::Map(47)), ("koi8_t", Codec::Map(48)),
    ("koi8_u", Codec::Map(49)), ("kz1048", Codec::Map(50)), ("latin_1", Codec::Map(1)),
    ("mac_arabic", Codec::Unusable), ("mac_croatian", Codec::Map(51)),
    ("mac_cyrillic", Codec::Map(52)), ("mac_farsi", Codec::Unusable), ("mac_greek", Codec::Map(53)),
    ("mac_iceland", Codec::Map(54)), ("mac_latin2", Codec::Map(55)), ("mac_roman", Codec::Map(56)),
    ("mac_romanian", Codec::Map(57)), ("mac_turkish", Codec::Map(58)),
    ("mbcs", Codec::Raise("LookupError")), ("oem", Codec::Raise("LookupError")),
    ("palmos", Codec::Map(59)), ("ptcp154", Codec::Map(60)),
    ("punycode", Codec::Raise("ValueError")), ("quopri_codec", Codec::Raise("LookupError")),
    ("raw_unicode_escape", Codec::Map(1)), ("rot_13", Codec::Raise("LookupError")),
    ("shift_jis", Codec::Raise("ValueError")), ("shift_jis_2004", Codec::Raise("ValueError")),
    ("shift_jisx0213", Codec::Raise("ValueError")), ("tis_620", Codec::Map(61)),
    ("undefined", Codec::Raise("UnicodeError")), ("unicode_escape", Codec::Map(1)),
    ("utf_16", Codec::Raise("ValueError")), ("utf_16_be", Codec::Raise("ValueError")),
    ("utf_16_le", Codec::Raise("ValueError")), ("utf_32", Codec::Raise("ValueError")),
    ("utf_32_be", Codec::Raise("ValueError")), ("utf_32_le", Codec::Raise("ValueError")),
    ("utf_7", Codec::Raise("ValueError")), ("utf_8", Codec::Map(0)), ("utf_8_sig", Codec::Map(0)),
    ("uu_codec", Codec::Raise("LookupError")), ("zlib_codec", Codec::Raise("LookupError")),
];
/// Bytes 0x80-0xFF of each single-byte codec (U+FFFD: undefined, malformed for expat).
const CODEC_MAPS: &[&str] = &[
    "\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}",
    "\u{80}\u{81}\u{82}\u{83}\u{84}\u{85}\u{86}\u{87}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{8d}\u{8e}\u{8f}\u{90}\u{91}\u{92}\u{93}\u{94}\u{95}\u{96}\u{97}\u{98}\u{99}\u{9a}\u{9b}\u{9c}\u{9d}\u{9e}\u{9f}\u{a0}¡¢£¤¥¦§¨©ª«¬\u{ad}®¯°±²³´µ¶·¸¹º»¼½¾¿ÀÁÂÃÄÅÆÇÈÉÊËÌÍÎÏÐÑÒÓÔÕÖ×ØÙÚÛÜÝÞßàáâãäåæçèéêëìíîïðñòóôõö÷øùúûüýþÿ",
    "\u{80}\u{81}\u{82}\u{83}\u{84}\u{85}\u{86}\u{87}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{8d}\u{8e}\u{8f}\u{90}\u{91}\u{92}\u{93}\u{94}\u{95}\u{96}\u{97}\u{98}\u{99}\u{9a}\u{9b}\u{9c}\u{9d}\u{9e}\u{9f}\u{a0}۰۱۲۳۴۵۶۷۸۹،؛\u{ad}؟ﺁﺍﺎﺎﺏﺑﭖﭘﺓﺕﺗﭦﭨﺙﺛﺝﺟﭺﭼﺡﺣﺥﺧﺩﮄﺫﺭﮌﺯﮊﺱﺳﺵﺷﺹﺻﺽﺿﻁﻅﻉﻊﻋﻌﻍﻎﻏﻐﻑﻓﻕﻗﻙﻛﮒﮔﻝﻟﻠﻡﻣﮞﻥﻧﺅﻭﮦﮨﮩﮪﺀﺉﺊﺋﻱﻲﻳﮰﮮﹼﹽ",
    "АБВГДЕЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯабвгдежзийклмноп░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀рстуфхцчшщъыьэюяЁёҐґЄєІіЇї·√№¤■\u{a0}",
    "€\u{fffd}‚\u{fffd}„…†‡\u{fffd}‰Š‹ŚŤŽŹ\u{fffd}‘’“”•–—\u{fffd}™š›śťžź\u{a0}ˇ˘Ł¤Ą¦§¨©Ş«¬\u{ad}®Ż°±˛ł´µ¶·¸ąş»Ľ˝ľżŔÁÂĂÄĹĆÇČÉĘËĚÍÎĎĐŃŇÓÔŐÖ×ŘŮÚŰÜÝŢßŕáâăäĺćçčéęëěíîďđńňóôőö÷řůúűüýţ˙",
    "ЂЃ‚ѓ„…†‡€‰Љ‹ЊЌЋЏђ‘’“”•–—\u{fffd}™љ›њќћџ\u{a0}ЎўЈ¤Ґ¦§Ё©Є«¬\u{ad}®Ї°±Ііґµ¶·ё№є»јЅѕїАБВГДЕЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯабвгдежзийклмнопрстуфхцчшщъыьэюя",
    "€\u{fffd}‚ƒ„…†‡ˆ‰Š‹Œ\u{fffd}Ž\u{fffd}\u{fffd}‘’“”•–—˜™š›œ\u{fffd}žŸ\u{a0}¡¢£¤¥¦§¨©ª«¬\u{ad}®¯°±²³´µ¶·¸¹º»¼½¾¿ÀÁÂÃÄÅÆÇÈÉÊËÌÍÎÏÐÑÒÓÔÕÖ×ØÙÚÛÜÝÞßàáâãäåæçèéêëìíîïðñòóôõö÷øùúûüýþÿ",
    "€\u{fffd}‚ƒ„…†‡\u{fffd}‰\u{fffd}‹\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}‘’“”•–—\u{fffd}™\u{fffd}›\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{a0}΅Ά£¤¥¦§¨©\u{fffd}«¬\u{ad}®―°±²³΄µ¶·ΈΉΊ»Ό½ΎΏΐΑΒΓΔΕΖΗΘΙΚΛΜΝΞΟΠΡ\u{fffd}ΣΤΥΦΧΨΩΪΫάέήίΰαβγδεζηθικλμνξοπρςστυφχψωϊϋόύώ\u{fffd}",
    "€\u{fffd}‚ƒ„…†‡ˆ‰Š‹Œ\u{fffd}\u{fffd}\u{fffd}\u{fffd}‘’“”•–—˜™š›œ\u{fffd}\u{fffd}Ÿ\u{a0}¡¢£¤¥¦§¨©ª«¬\u{ad}®¯°±²³´µ¶·¸¹º»¼½¾¿ÀÁÂÃÄÅÆÇÈÉÊËÌÍÎÏĞÑÒÓÔÕÖ×ØÙÚÛÜİŞßàáâãäåæçèéêëìíîïğñòóôõö÷øùúûüışÿ",
    "€\u{fffd}‚ƒ„…†‡ˆ‰\u{fffd}‹\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}‘’“”•–—˜™\u{fffd}›\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{a0}¡¢£₪¥¦§¨©×«¬\u{ad}®¯°±²³´µ¶·¸¹÷»¼½¾¿\u{5b0}\u{5b1}\u{5b2}\u{5b3}\u{5b4}\u{5b5}\u{5b6}\u{5b7}\u{5b8}\u{5b9}\u{fffd}\u{5bb}\u{5bc}\u{5bd}־\u{5bf}׀\u{5c1}\u{5c2}׃װױײ׳״\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}אבגדהוזחטיךכלםמןנסעףפץצקרשת\u{fffd}\u{fffd}\u{200e}\u{200f}\u{fffd}",
    "€پ‚ƒ„…†‡ˆ‰ٹ‹Œچژڈگ‘’“”•–—ک™ڑ›œ\u{200c}\u{200d}ں\u{a0}،¢£¤¥¦§¨©ھ«¬\u{ad}®¯°±²³´µ¶·¸¹؛»¼½¾؟ہءآأؤإئابةتثجحخدذرزسشصض×طظعغـفقكàلâمنهوçèéêëىيîï\u{64b}\u{64c}\u{64d}\u{64e}ô\u{64f}\u{650}÷\u{651}ù\u{652}ûü\u{200e}\u{200f}ے",
    "€\u{fffd}‚\u{fffd}„…†‡\u{fffd}‰\u{fffd}‹\u{fffd}¨ˇ¸\u{fffd}‘’“”•–—\u{fffd}™\u{fffd}›\u{fffd}¯˛\u{fffd}\u{a0}\u{fffd}¢£¤\u{fffd}¦§Ø©Ŗ«¬\u{ad}®Æ°±²³´µ¶·ø¹ŗ»¼½¾æĄĮĀĆÄÅĘĒČÉŹĖĢĶĪĻŠŃŅÓŌÕÖ×ŲŁŚŪÜŻŽßąįāćäåęēčéźėģķīļšńņóōõö÷ųłśūüżž˙",
    "€\u{fffd}‚ƒ„…†‡ˆ‰\u{fffd}‹Œ\u{fffd}\u{fffd}\u{fffd}\u{fffd}‘’“”•–—˜™\u{fffd}›œ\u{fffd}\u{fffd}Ÿ\u{a0}¡¢£¤¥¦§¨©ª«¬\u{ad}®¯°±²³´µ¶·¸¹º»¼½¾¿ÀÁÂĂÄÅÆÇÈÉÊË\u{300}ÍÎÏĐÑ\u{309}ÓÔƠÖ×ØÙÚÛÜƯ\u{303}ßàáâăäåæçèéêë\u{301}íîïđñ\u{323}óôơö÷øùúûüư₫ÿ",
    "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}",
    "\u{80}\u{81}éâ\u{84}à\u{86}çêëèïî\u{8d}\u{8e}\u{8f}\u{90}\u{651}\u{652}ô¤ـûùءآأؤ£إئابةتثجحخدذرزسشص«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀ضطظعغفµقكلمنهوىي≡\u{64b}\u{64c}\u{64d}\u{64e}\u{64f}\u{650}≈°∙·√ⁿ²■\u{a0}",
    "ΑΒΓΔΕΖΗΘΙΚΛΜΝΞΟΠΡΣΤΥΦΧΨΩαβγδεζηθικλμνξοπρσςτυφχψ░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀ωάέήϊίόύϋώΆΈΉΊΌΎΏ±≥≤ΪΫ÷≈°∙·√ⁿ²■\u{a0}",
    "ĆüéāäģåćłēŖŗīŹÄÅÉæÆōöĢ¢ŚśÖÜø£Ø×¤ĀĪóŻżź”¦©®¬½¼Ł«»░▒▓│┤ĄČĘĖ╣║╗╝ĮŠ┐└┴┬├─┼ŲŪ╚╔╩╦╠═╬Žąčęėįšųūž┘┌█▄▌▐▀ÓßŌŃõÕµńĶķĻļņĒŅ’\u{ad}±“¾¶§÷„°∙·¹³²■\u{a0}",
    "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜø£Ø×ƒáíóúñÑªº¿®¬½¼¡«»░▒▓│┤ÁÂÀ©╣║╗╝¢¥┐└┴┬├─┼ãÃ╚╔╩╦╠═╬¤ðÐÊËÈıÍÎÏ┘┌█▄¦Ì▀ÓßÔÒõÕµþÞÚÛÙýÝ¯´\u{ad}±‗¾¶§÷¸°¨·¹³²■\u{a0}",
    "ÇüéâäůćçłëŐőîŹÄĆÉĹĺôöĽľŚśÖÜŤťŁ×čáíóúĄąŽžĘę¬źČş«»░▒▓│┤ÁÂĚŞ╣║╗╝Żż┐└┴┬├─┼Ăă╚╔╩╦╠═╬¤đĐĎËďŇÍÎě┘┌█▄ŢŮ▀ÓßÔŃńňŠšŔÚŕŰýÝţ´\u{ad}˝˛ˇ˘§÷¸°¨˙űŘř■\u{a0}",
    "ђЂѓЃёЁєЄѕЅіІїЇјЈљЉњЊћЋќЌўЎџЏюЮъЪаАбБцЦдДеЕфФгГ«»░▒▓│┤хХиИ╣║╗╝йЙ┐└┴┬├─┼кК╚╔╩╦╠═╬¤лЛмМнНоОп┘┌█▄Пя▀ЯрРсСтТуУжЖвВьЬ№\u{ad}ыЫзЗшШэЭщЩчЧ§■\u{a0}",
    "אבגדהוזחטיךכלםמןנסעףפץצקרשת\u{fffd}£\u{fffd}×\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}®¬½¼\u{fffd}«»░▒▓│┤\u{fffd}\u{fffd}\u{fffd}©╣║╗╝¢¥┐└┴┬├─┼\u{fffd}\u{fffd}╚╔╩╦╠═╬¤\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}┘┌█▄¦\u{fffd}▀\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}µ\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}¯´\u{ad}±‗¾¶§÷¸°¨·¹³²■\u{a0}",
    "ÇüéâäàåçêëèïîıÄÅÉæÆôöòûùİÖÜø£ØŞşáíóúñÑĞğ¿®¬½¼¡«»░▒▓│┤ÁÂÀ©╣║╗╝¢¥┐└┴┬├─┼ãÃ╚╔╩╦╠═╬¤ºªÊËÈ\u{fffd}ÍÎÏ┘┌█▄¦Ì▀ÓßÔÒõÕµ\u{fffd}×ÚÛÙìÿ¯´\u{ad}±\u{fffd}¾¶§÷¸°¨·¹³²■\u{a0}",
    "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜø£Ø×ƒáíóúñÑªº¿®¬½¼¡«»░▒▓│┤ÁÂÀ©╣║╗╝¢¥┐└┴┬├─┼ãÃ╚╔╩╦╠═╬¤ðÐÊËÈ€ÍÎÏ┘┌█▄¦Ì▀ÓßÔÒõÕµþÞÚÛÙýÝ¯´\u{ad}±‗¾¶§÷¸°¨·¹³²■\u{a0}",
    "ÇüéâãàÁçêÊèÍÔìÃÂÉÀÈôõòÚùÌÕÜ¢£Ù₧ÓáíóúñÑªº¿Ò¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}",
    "ÇüéâäàåçêëèÐðÞÄÅÉæÆôöþûÝýÖÜø£Ø₧ƒáíóúÁÍÓÚ¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}",
    "אבגדהוזחטיךכלםמןנסעףפץצקרשת¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}",
    "ÇüéâÂà¶çêëèïî‗À§ÉÈÊôËÏûù¤ÔÜ¢£ÙÛƒ¦´óú¨¸³¯Î⌐¬½¼¾«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}",
    "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜø£Ø₧ƒáíóúñÑªº¿⌐¬½¼¡«¤░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}",
    "АБВГДЕЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯабвгдежзийклмноп░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀рстуфхцчшщъыьэюяЁёЄєЇїЎў°∙·√№¤■\u{a0}",
    "\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}Ά\u{fffd}·¬¦‘’Έ―ΉΊΪΌ\u{fffd}\u{fffd}ΎΫ©Ώ²³ά£έήίϊΐόύΑΒΓΔΕΖΗ½ΘΙ«»░▒▓│┤ΚΛΜΝ╣║╗╝ΞΟ┐└┴┬├─┼ΠΡ╚╔╩╦╠═╬ΣΤΥΦΧΨΩαβγ┘┌█▄δε▀ζηθικλμνξοπρσςτ΄\u{ad}±υφχ§ψ΅°¨ωϋΰώ■\u{a0}",
    "€\u{fffd}\u{fffd}\u{fffd}\u{fffd}…\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}‘’“”•–—\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{a0}กขฃคฅฆงจฉชซฌญฎฏฐฑฒณดตถทธนบปผฝพฟภมยรฤลฦวศษสหฬอฮฯะ\u{e31}าำ\u{e34}\u{e35}\u{e36}\u{e37}\u{e38}\u{e39}\u{e3a}\u{fffd}\u{fffd}\u{fffd}\u{fffd}฿เแโใไๅๆ\u{e47}\u{e48}\u{e49}\u{e4a}\u{e4b}\u{e4c}\u{e4d}\u{e4e}๏๐๑๒๓๔๕๖๗๘๙๚๛\u{fffd}\u{fffd}\u{fffd}\u{fffd}",
    "\u{80}\u{81}\u{82}\u{83}\u{84}\u{85}\u{86}\u{87}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{8d}\u{8e}\u{8f}\u{90}\u{91}\u{92}\u{93}\u{94}\u{95}\u{96}\u{97}\u{98}\u{99}\u{9a}\u{9b}\u{9c}\u{9d}\u{9e}\u{9f}\u{a0}ÀÂÈÊËÎÏ´ˋˆ¨˜ÙÛ₤¯Ýý°ÇçÑñ¡¿¤£¥§ƒ¢âêôûáéóúàèòùäëöüÅîØÆåíøæÄìÖÜÉïßÔÁÃãÐðÍÌÓÒÕõŠšÚŸÿÞþ·µ¶¾—¼½ªº«■»±\u{fffd}",
    "\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}",
    "\u{80}\u{81}\u{82}\u{83}\u{84}\u{85}\u{86}\u{87}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{8d}\u{8e}\u{8f}\u{90}\u{91}\u{92}\u{93}\u{94}\u{95}\u{96}\u{97}\u{98}\u{99}\u{9a}\u{9b}\u{9c}\u{9d}\u{9e}\u{9f}\u{a0}ĄĒĢĪĨĶ§ĻĐŠŦŽ\u{ad}ŪŊ°ąēģīĩķ·ļđšŧž―ūŋĀÁÂÃÄÅÆĮČÉĘËĖÍÎÏÐŅŌÓÔÕÖŨØŲÚÛÜÝÞßāáâãäåæįčéęëėíîïðņōóôõöũøųúûüýþĸ",
    "\u{80}\u{81}\u{82}\u{83}\u{84}\u{85}\u{86}\u{87}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{8d}\u{8e}\u{8f}\u{90}\u{91}\u{92}\u{93}\u{94}\u{95}\u{96}\u{97}\u{98}\u{99}\u{9a}\u{9b}\u{9c}\u{9d}\u{9e}\u{9f}\u{a0}กขฃคฅฆงจฉชซฌญฎฏฐฑฒณดตถทธนบปผฝพฟภมยรฤลฦวศษสหฬอฮฯะ\u{e31}าำ\u{e34}\u{e35}\u{e36}\u{e37}\u{e38}\u{e39}\u{e3a}\u{fffd}\u{fffd}\u{fffd}\u{fffd}฿เแโใไๅๆ\u{e47}\u{e48}\u{e49}\u{e4a}\u{e4b}\u{e4c}\u{e4d}\u{e4e}๏๐๑๒๓๔๕๖๗๘๙๚๛\u{fffd}\u{fffd}\u{fffd}\u{fffd}",
    "\u{80}\u{81}\u{82}\u{83}\u{84}\u{85}\u{86}\u{87}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{8d}\u{8e}\u{8f}\u{90}\u{91}\u{92}\u{93}\u{94}\u{95}\u{96}\u{97}\u{98}\u{99}\u{9a}\u{9b}\u{9c}\u{9d}\u{9e}\u{9f}\u{a0}”¢£¤„¦§Ø©Ŗ«¬\u{ad}®Æ°±²³“µ¶·ø¹ŗ»¼½¾æĄĮĀĆÄÅĘĒČÉŹĖĢĶĪĻŠŃŅÓŌÕÖ×ŲŁŚŪÜŻŽßąįāćäåęēčéźėģķīļšńņóōõö÷ųłśūüżž’",
    "\u{80}\u{81}\u{82}\u{83}\u{84}\u{85}\u{86}\u{87}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{8d}\u{8e}\u{8f}\u{90}\u{91}\u{92}\u{93}\u{94}\u{95}\u{96}\u{97}\u{98}\u{99}\u{9a}\u{9b}\u{9c}\u{9d}\u{9e}\u{9f}\u{a0}Ḃḃ£ĊċḊ§Ẁ©ẂḋỲ\u{ad}®ŸḞḟĠġṀṁ¶ṖẁṗẃṠỳẄẅṡÀÁÂÃÄÅÆÇÈÉÊËÌÍÎÏŴÑÒÓÔÕÖṪØÙÚÛÜÝŶßàáâãäåæçèéêëìíîïŵñòóôõöṫøùúûüýŷÿ",
    "\u{80}\u{81}\u{82}\u{83}\u{84}\u{85}\u{86}\u{87}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{8d}\u{8e}\u{8f}\u{90}\u{91}\u{92}\u{93}\u{94}\u{95}\u{96}\u{97}\u{98}\u{99}\u{9a}\u{9b}\u{9c}\u{9d}\u{9e}\u{9f}\u{a0}¡¢£€¥Š§š©ª«¬\u{ad}®¯°±²³Žµ¶·ž¹º»ŒœŸ¿ÀÁÂÃÄÅÆÇÈÉÊËÌÍÎÏÐÑÒÓÔÕÖ×ØÙÚÛÜÝÞßàáâãäåæçèéêëìíîïðñòóôõö÷øùúûüýþÿ",
    "\u{80}\u{81}\u{82}\u{83}\u{84}\u{85}\u{86}\u{87}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{8d}\u{8e}\u{8f}\u{90}\u{91}\u{92}\u{93}\u{94}\u{95}\u{96}\u{97}\u{98}\u{99}\u{9a}\u{9b}\u{9c}\u{9d}\u{9e}\u{9f}\u{a0}ĄąŁ€„Š§š©Ș«Ź\u{ad}źŻ°±ČłŽ”¶·žčș»ŒœŸżÀÁÂĂÄĆÆÇÈÉÊËÌÍÎÏĐŃÒÓÔŐÖŚŰÙÚÛÜĘȚßàáâăäćæçèéêëìíîïđńòóôőöśűùúûüęțÿ",
    "\u{80}\u{81}\u{82}\u{83}\u{84}\u{85}\u{86}\u{87}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{8d}\u{8e}\u{8f}\u{90}\u{91}\u{92}\u{93}\u{94}\u{95}\u{96}\u{97}\u{98}\u{99}\u{9a}\u{9b}\u{9c}\u{9d}\u{9e}\u{9f}\u{a0}Ą˘Ł¤ĽŚ§¨ŠŞŤŹ\u{ad}ŽŻ°ą˛ł´ľśˇ¸šşťź˝žżŔÁÂĂÄĹĆÇČÉĘËĚÍÎĎĐŃŇÓÔŐÖ×ŘŮÚŰÜÝŢßŕáâăäĺćçčéęëěíîďđńňóôőö÷řůúűüýţ˙",
    "\u{80}\u{81}\u{82}\u{83}\u{84}\u{85}\u{86}\u{87}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{8d}\u{8e}\u{8f}\u{90}\u{91}\u{92}\u{93}\u{94}\u{95}\u{96}\u{97}\u{98}\u{99}\u{9a}\u{9b}\u{9c}\u{9d}\u{9e}\u{9f}\u{a0}Ħ˘£¤\u{fffd}Ĥ§¨İŞĞĴ\u{ad}\u{fffd}Ż°ħ²³´µĥ·¸ışğĵ½\u{fffd}żÀÁÂ\u{fffd}ÄĊĈÇÈÉÊËÌÍÎÏ\u{fffd}ÑÒÓÔĠÖ×ĜÙÚÛÜŬŜßàáâ\u{fffd}äċĉçèéêëìíîï\u{fffd}ñòóôġö÷ĝùúûüŭŝ˙",
    "\u{80}\u{81}\u{82}\u{83}\u{84}\u{85}\u{86}\u{87}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{8d}\u{8e}\u{8f}\u{90}\u{91}\u{92}\u{93}\u{94}\u{95}\u{96}\u{97}\u{98}\u{99}\u{9a}\u{9b}\u{9c}\u{9d}\u{9e}\u{9f}\u{a0}ĄĸŖ¤ĨĻ§¨ŠĒĢŦ\u{ad}Ž¯°ą˛ŗ´ĩļˇ¸šēģŧŊžŋĀÁÂÃÄÅÆĮČÉĘËĖÍÎĪĐŅŌĶÔÕÖ×ØŲÚÛÜŨŪßāáâãäåæįčéęëėíîīđņōķôõö÷øųúûüũū˙",
    "\u{80}\u{81}\u{82}\u{83}\u{84}\u{85}\u{86}\u{87}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{8d}\u{8e}\u{8f}\u{90}\u{91}\u{92}\u{93}\u{94}\u{95}\u{96}\u{97}\u{98}\u{99}\u{9a}\u{9b}\u{9c}\u{9d}\u{9e}\u{9f}\u{a0}ЁЂЃЄЅІЇЈЉЊЋЌ\u{ad}ЎЏАБВГДЕЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯабвгдежзийклмнопрстуфхцчшщъыьэюя№ёђѓєѕіїјљњћќ§ўџ",
    "\u{80}\u{81}\u{82}\u{83}\u{84}\u{85}\u{86}\u{87}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{8d}\u{8e}\u{8f}\u{90}\u{91}\u{92}\u{93}\u{94}\u{95}\u{96}\u{97}\u{98}\u{99}\u{9a}\u{9b}\u{9c}\u{9d}\u{9e}\u{9f}\u{a0}\u{fffd}\u{fffd}\u{fffd}¤\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}،\u{ad}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}؛\u{fffd}\u{fffd}\u{fffd}؟\u{fffd}ءآأؤإئابةتثجحخدذرزسشصضطظعغ\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}ـفقكلمنهوىي\u{64b}\u{64c}\u{64d}\u{64e}\u{64f}\u{650}\u{651}\u{652}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}",
    "\u{80}\u{81}\u{82}\u{83}\u{84}\u{85}\u{86}\u{87}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{8d}\u{8e}\u{8f}\u{90}\u{91}\u{92}\u{93}\u{94}\u{95}\u{96}\u{97}\u{98}\u{99}\u{9a}\u{9b}\u{9c}\u{9d}\u{9e}\u{9f}\u{a0}‘’£€₯¦§¨©ͺ«¬\u{ad}\u{fffd}―°±²³΄΅Ά·ΈΉΊ»Ό½ΎΏΐΑΒΓΔΕΖΗΘΙΚΛΜΝΞΟΠΡ\u{fffd}ΣΤΥΦΧΨΩΪΫάέήίΰαβγδεζηθικλμνξοπρςστυφχψωϊϋόύώ\u{fffd}",
    "\u{80}\u{81}\u{82}\u{83}\u{84}\u{85}\u{86}\u{87}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{8d}\u{8e}\u{8f}\u{90}\u{91}\u{92}\u{93}\u{94}\u{95}\u{96}\u{97}\u{98}\u{99}\u{9a}\u{9b}\u{9c}\u{9d}\u{9e}\u{9f}\u{a0}\u{fffd}¢£¤¥¦§¨©×«¬\u{ad}®¯°±²³´µ¶·¸¹÷»¼½¾\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}‗אבגדהוזחטיךכלםמןנסעףפץצקרשת\u{fffd}\u{fffd}\u{200e}\u{200f}\u{fffd}",
    "\u{80}\u{81}\u{82}\u{83}\u{84}\u{85}\u{86}\u{87}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{8d}\u{8e}\u{8f}\u{90}\u{91}\u{92}\u{93}\u{94}\u{95}\u{96}\u{97}\u{98}\u{99}\u{9a}\u{9b}\u{9c}\u{9d}\u{9e}\u{9f}\u{a0}¡¢£¤¥¦§¨©ª«¬\u{ad}®¯°±²³´µ¶·¸¹º»¼½¾¿ÀÁÂÃÄÅÆÇÈÉÊËÌÍÎÏĞÑÒÓÔÕÖ×ØÙÚÛÜİŞßàáâãäåæçèéêëìíîïğñòóôõö÷øùúûüışÿ",
    "─│┌┐└┘├┤┬┴┼▀▄█▌▐░▒▓⌠■∙√≈≤≥\u{a0}⌡°²·÷═║╒ё╓╔╕╖╗╘╙╚╛╜╝╞╟╠╡Ё╢╣╤╥╦╧╨╩╪╫╬©юабцдефгхийклмнопярстужвьызшэщчъЮАБЦДЕФГХИЙКЛМНОПЯРСТУЖВЬЫЗШЭЩЧЪ",
    "қғ‚Ғ„…†‡\u{fffd}‰ҳ‹ҲҷҶ\u{fffd}Қ‘’“”•–—\u{fffd}™\u{fffd}›\u{fffd}\u{fffd}\u{fffd}\u{fffd}\u{fffd}ӯӮё¤ӣ¦§\u{fffd}\u{fffd}\u{fffd}«¬\u{ad}®\u{fffd}°±²Ё\u{fffd}Ӣ¶·\u{fffd}№\u{fffd}»\u{fffd}\u{fffd}\u{fffd}©юабцдефгхийклмнопярстужвьызшэщчъЮАБЦДЕФГХИЙКЛМНОПЯРСТУЖВЬЫЗШЭЩЧЪ",
    "─│┌┐└┘├┤┬┴┼▀▄█▌▐░▒▓⌠■∙√≈≤≥\u{a0}⌡°²·÷═║╒ёє╔ії╗╘╙╚╛ґ╝╞╟╠╡ЁЄ╣ІЇ╦╧╨╩╪Ґ╬©юабцдефгхийклмнопярстужвьызшэщчъЮАБЦДЕФГХИЙКЛМНОПЯРСТУЖВЬЫЗШЭЩЧЪ",
    "ЂЃ‚ѓ„…†‡€‰Љ‹ЊҚҺЏђ‘’“”•–—\u{fffd}™љ›њқһџ\u{a0}ҰұӘ¤Ө¦§Ё©Ғ«¬\u{ad}®Ү°±Ііөµ¶·ё№ғ»әҢңүАБВГДЕЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯабвгдежзийклмнопрстуфхцчшщъыьэюя",
    "ÄÅÇÉÑÖÜáàâäãåçéèêëíìîïñóòôöõúùûü†°¢£§•¶ß®Š™´¨≠ŽØ∞±≤≥∆µ∂∑∏š∫ªºΩžø¿¡¬√ƒ≈Ć«Č…\u{a0}ÀÃÕŒœĐ—“”‘’÷◊\u{f8ff}©⁄€‹›Æ»–·‚„‰ÂćÁčÈÍÎÏÌÓÔđÒÚÛÙıˆ˜¯πË˚¸Êæˇ",
    "АБВГДЕЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯ†°Ґ£§•¶І®©™Ђђ≠Ѓѓ∞±≤≥іµґЈЄєЇїЉљЊњјЅ¬√ƒ≈∆«»…\u{a0}ЋћЌќѕ–—“”‘’÷„ЎўЏџ№Ёёяабвгдежзийклмнопрстуфхцчшщъыьэю€",
    "Ä¹²É³ÖÜ΅àâä΄¨çéèêë£™îï•½‰ôö¦€ùûü†ΓΔΘΛΞΠß®©ΣΪ§≠°·Α±≤≥¥ΒΕΖΗΙΚΜΦΫΨΩάΝ¬ΟΡ≈Τ«»…\u{a0}ΥΧΆΈœ–―“”‘’÷ΉΊΌΎέήίόΏύαβψδεφγηιξκλμνοπώρστθωςχυζϊϋΐΰ\u{ad}",
    "ÄÅÇÉÑÖÜáàâäãåçéèêëíìîïñóòôöõúùûüÝ°¢£§•¶ß®©™´¨≠ÆØ∞±≤≥¥µ∂∑∏π∫ªºΩæø¿¡¬√ƒ≈∆«»…\u{a0}ÀÃÕŒœ–—“”‘’÷◊ÿŸ⁄€ÐðÞþý·‚„‰ÂÊÁËÈÍÎÏÌÓÔ\u{f8ff}ÒÚÛÙıˆ˜¯˘˙˚¸˝˛ˇ",
    "ÄĀāÉĄÖÜáąČäčĆćéŹźĎíďĒēĖóėôöõúĚěü†°Ę£§•¶ß®©™ę¨≠ģĮįĪ≤≥īĶ∂∑łĻļĽľĹĺŅņŃ¬√ńŇ∆«»…\u{a0}ňŐÕőŌ–—“”‘’÷◊ōŔŕŘ‹›řŖŗŠ‚„šŚśÁŤťÍŽžŪÓÔūŮÚůŰűŲųÝýķŻŁżĢˇ",
    "ÄÅÇÉÑÖÜáàâäãåçéèêëíìîïñóòôöõúùûü†°¢£§•¶ß®©™´¨≠ÆØ∞±≤≥¥µ∂∑∏π∫ªºΩæø¿¡¬√ƒ≈∆«»…\u{a0}ÀÃÕŒœ–—“”‘’÷◊ÿŸ⁄€‹›ﬁﬂ‡·‚„‰ÂÊÁËÈÍÎÏÌÓÔ\u{f8ff}ÒÚÛÙıˆ˜¯˘˙˚¸˝˛ˇ",
    "ÄÅÇÉÑÖÜáàâäãåçéèêëíìîïñóòôöõúùûü†°¢£§•¶ß®©™´¨≠ĂȘ∞±≤≥¥µ∂∑∏π∫ªºΩăș¿¡¬√ƒ≈∆«»…\u{a0}ÀÃÕŒœ–—“”‘’÷◊ÿŸ⁄€‹›Țț‡·‚„‰ÂÊÁËÈÍÎÏÌÓÔ\u{f8ff}ÒÚÛÙıˆ˜¯˘˙˚¸˝˛ˇ",
    "ÄÅÇÉÑÖÜáàâäãåçéèêëíìîïñóòôöõúùûü†°¢£§•¶ß®©™´¨≠ÆØ∞±≤≥¥µ∂∑∏π∫ªºΩæø¿¡¬√ƒ≈∆«»…\u{a0}ÀÃÕŒœ–—“”‘’÷◊ÿŸĞğİıŞş‡·‚„‰ÂÊÁËÈÍÎÏÌÓÔ\u{f8ff}ÒÚÛÙ\u{f8a0}ˆ˜¯˘˙˚¸˝˛ˇ",
    "€\u{81}‚ƒ„…†‡ˆ‰Š‹Œ♦♣♥♠‘’“”•–—˜™š›œ\u{9d}\u{9e}Ÿ\u{a0}¡¢£¤¥¦§¨©ª«¬\u{ad}®¯°±²³´µ¶·¸¹º»¼½¾¿ÀÁÂÃÄÅÆÇÈÉÊËÌÍÎÏÐÑÒÓÔÕÖ×ØÙÚÛÜÝÞßàáâãäåæçèéêëìíîïðñòóôõö÷øùúûüýþÿ",
    "ҖҒӮғ„…ҶҮҲүҠӢҢҚҺҸҗ‘’“”•–—ҳҷҡӣңқһҹ\u{a0}ЎўЈӨҘҰ§Ё©Ә«¬ӯ®Ҝ°ұІіҙө¶·ё№ә»јҪҫҝАБВГДЕЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯабвгдежзийклмнопрстуфхцчшщъыьэюя",
    "\u{80}\u{81}\u{82}\u{83}\u{84}\u{85}\u{86}\u{87}\u{88}\u{89}\u{8a}\u{8b}\u{8c}\u{8d}\u{8e}\u{8f}\u{90}\u{91}\u{92}\u{93}\u{94}\u{95}\u{96}\u{97}\u{98}\u{99}\u{9a}\u{9b}\u{9c}\u{9d}\u{9e}\u{9f}\u{fffd}กขฃคฅฆงจฉชซฌญฎฏฐฑฒณดตถทธนบปผฝพฟภมยรฤลฦวศษสหฬอฮฯะ\u{e31}าำ\u{e34}\u{e35}\u{e36}\u{e37}\u{e38}\u{e39}\u{e3a}\u{fffd}\u{fffd}\u{fffd}\u{fffd}฿เแโใไๅๆ\u{e47}\u{e48}\u{e49}\u{e4a}\u{e4b}\u{e4c}\u{e4d}\u{e4e}๏๐๑๒๓๔๕๖๗๘๙๚๛\u{fffd}\u{fffd}\u{fffd}\u{fffd}",
];
/// Bytes below 0x80 that a codec in CODEC_MAPS leaves undefined: (map, byte).
const CODEC_LOW_UNDEFINED: &[(usize, u8)] = &[ (32, 0x7E), ];
