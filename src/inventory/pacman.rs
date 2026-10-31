//! Repo and AUR packages, what is pending for them, and orphans: everything
//! read from pacman, checkupdates and yay.

use super::*;

/// What `subprocess.run(cmd, capture_output=True, text=True, timeout=t)`
/// returns or raises.
#[derive(Debug)]
pub(super) enum RunError {
    /// OSError: the program could not be started.
    Os,
    /// subprocess.TimeoutExpired.
    Timeout,
    /// UnicodeDecodeError (a ValueError): the output is not UTF-8.
    Decode(String),
    /// ValueError("embedded null byte"): an argument holds a NUL, which no
    /// argv can carry.
    Nul,
}

pub(super) struct RunResult {
    pub(super) code: Option<i32>,
    pub(super) stdout: String,
}

/// Text mode's decoding: strict UTF-8, then universal newlines ("\r\n" and
/// a lone "\r" both become "\n").
pub(super) fn py_decode(bytes: &[u8]) -> Result<String, RunError> {
    match std::str::from_utf8(bytes) {
        Ok(s) => Ok(s.replace("\r\n", "\n").replace('\r', "\n")),
        Err(e) => {
            let pos = e.valid_up_to();
            let why = if e.error_len().is_none() {
                "unexpected end of data"
            } else if (0x80..0xC2).contains(&bytes[pos]) || bytes[pos] >= 0xF5 {
                "invalid start byte"
            } else {
                "invalid continuation byte"
            };
            Err(RunError::Decode(format!(
                "'utf-8' codec can't decode byte 0x{:02x} in position {pos}: {why}",
                bytes[pos]
            )))
        }
    }
}

/// `subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)`.
///
/// `util::run_bytes`, decoded here: text mode raises on bytes that are not
/// UTF-8 and translates newlines, where `util::run` decodes lossily and
/// keeps "\r". The timeout behaves as Python's does: on expiry the call
/// gives up at once, without waiting for a grandchild that still holds the
/// pipes open; the child's whole process group is killed (Python kills only
/// the child) and the child reaped. stdin is inherited, as subprocess.run
/// leaves it.
pub(super) fn subprocess_run<S: AsRef<std::ffi::OsStr>>(
    cmd: &[S],
    timeout: u64,
) -> Result<RunResult, RunError> {
    subprocess_run_env(cmd, timeout, &[])
}

/// `subprocess_run` with `env` set on top of this process's environment.
pub(super) fn subprocess_run_env<S: AsRef<std::ffi::OsStr>>(
    cmd: &[S],
    timeout: u64,
    env: &[(&str, &str)],
) -> Result<RunResult, RunError> {
    use std::os::unix::ffi::OsStrExt;
    // subprocess checks every argument before it forks.
    if cmd.iter().any(|a| a.as_ref().as_bytes().contains(&0)) {
        return Err(RunError::Nul);
    }
    let out = crate::util::run_bytes(
        cmd,
        &crate::util::RunOpts {
            timeout: Some(std::time::Duration::from_secs(timeout)),
            stdin: crate::util::Stdin::Inherit,
            env: env
                .iter()
                .map(|(k, v)| ((*k).into(), (*v).into()))
                .collect(),
            ..Default::default()
        },
    );
    if out.failed_to_start {
        return Err(RunError::Os);
    }
    if out.timed_out {
        return Err(RunError::Timeout);
    }
    let stdout = py_decode(&out.stdout)?;
    py_decode(&out.stderr)?;
    Ok(RunResult {
        code: out.code,
        stdout,
    })
}

/// `_run`: run a command, returning stdout. Never raises: a missing or failing
/// helper should degrade one section of the list, not take the window down.
/// (Python default timeout: 25.)
///
/// A timeout is `subprocess.TimeoutExpired`, caught like any other failure,
/// so it answers "" rather than whatever was printed before the kill. Output
/// that is not UTF-8 is the UnicodeDecodeError `_run` does not catch: Err,
/// which takes the calling collector down with it, as in the Python.
///
/// A NUL byte in an argument is the ValueError subprocess raises before it
/// starts anything, which `_run` does not catch either: Err.
pub fn run(cmd: &[&str], timeout: u64) -> Result<String, String> {
    run_os(cmd, timeout)
}

/// `run` for arguments that are paths, which need not be UTF-8.
pub(super) fn run_os<S: AsRef<std::ffi::OsStr>>(cmd: &[S], timeout: u64) -> Result<String, String> {
    match subprocess_run(cmd, timeout) {
        Ok(r) => Ok(r.stdout),
        Err(RunError::Os | RunError::Timeout) => Ok(String::new()),
        Err(RunError::Decode(e)) => Err(e),
        Err(RunError::Nul) => Err("embedded null byte".into()),
    }
}

/// `_FIELDS`: pacman -Qi field -> the key we keep it under.
pub const FIELDS: &[(&str, &str)] = &[
    ("Description", "description"),
    ("URL", "url"),
    ("Groups", "groups"),
    ("Installed Size", "size"),
    ("Build Date", "built"),
    ("Install Date", "installed"),
    ("Licenses", "license"),
];

pub(super) fn field_key(key: &str) -> Option<&'static str> {
    FIELDS.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

pub(super) fn argv<'a>(head: &[&'a str], names: &'a [String]) -> Vec<&'a str> {
    let mut v: Vec<&str> = head.to_vec();
    v.extend(names.iter().map(String::as_str));
    v
}

/// `_package_info`: full metadata for every package in ONE pacman call.
///
/// pacman -Qi emits "Field : value" blocks separated by blank lines. Values
/// can wrap onto continuation lines (long dependency lists), which is why a
/// naive line-per-field parse drops data -- continuation lines are appended
/// to the field already in progress.
///
/// Err: pacman's output is not UTF-8 (`_run`'s UnicodeDecodeError).
pub fn package_info(
    names: &[String],
) -> Result<BTreeMap<String, BTreeMap<String, String>>, String> {
    if names.is_empty() {
        return Ok(BTreeMap::new());
    }
    let out = run(&argv(&[PACMAN, "-Qi"], names), 60)?;
    let mut info: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let (mut current, mut field): (Option<String>, Option<&str>) = (None, None);
    for line in py_splitlines(&out) {
        if py_strip(line).is_empty() {
            current = None;
            field = None;
            continue;
        }
        let cur_set = current.as_deref().is_some_and(|c| !c.is_empty());
        if line.starts_with(' ') && cur_set && field.is_some() {
            if let (Some(c), Some(f)) = (&current, field) {
                if let Some(entry) = info.get_mut(c).and_then(|m| m.get_mut(f)) {
                    entry.push(' ');
                    entry.push_str(py_strip(line));
                }
            }
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let (key, value) = (py_strip(key), py_strip(value));
        if key == "Name" {
            current = Some(value.to_string());
            info.insert(value.to_string(), BTreeMap::new());
            field = None;
        } else if cur_set {
            if let Some(f) = field_key(key) {
                field = Some(f);
                if let Some(c) = &current {
                    info.entry(c.clone())
                        .or_default()
                        .insert(f.to_string(), value.to_string());
                }
            }
        }
    }
    Ok(info)
}

/// Where packages put their launchers.
pub(super) const APPLICATIONS: &str = "/usr/share/applications";

/// The .desktop files under `dir`, a few folders down (some packages keep
/// theirs in a subfolder), sorted. Names that are not UTF-8 are left out:
/// no package's launcher is called that.
pub(super) fn desktop_files(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut todo = vec![(dir.to_path_buf(), 0)];
    while let Some((d, depth)) = todo.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            let Some(text) = path.to_str() else { continue };
            match e.file_type() {
                Ok(t) if t.is_dir() && depth < 3 => todo.push((path.clone(), depth + 1)),
                Ok(_) if text.ends_with(".desktop") => out.push(text.to_string()),
                _ => {}
            }
        }
    }
    out.sort();
    out
}

/// `pacman -Qo` lines ("<path> is owned by <pkg> <version>", in the C
/// locale) as package -> its files, for the packages in `wanted`. Each
/// package's files stay sorted, as `pacman -Ql` lists them.
pub(super) fn owners_from(out: &str, wanted: &HashSet<&str>) -> Vec<(String, Vec<String>)> {
    let mut owns: Vec<(String, Vec<String>)> = Vec::new();
    for line in py_splitlines(out) {
        let Some(at) = line.rfind(" is owned by ") else {
            continue;
        };
        let (path, rest) = (&line[..at], &line[at + " is owned by ".len()..]);
        let Some(pkg) = rest.split_whitespace().next() else {
            continue;
        };
        if !wanted.contains(pkg) {
            continue;
        }
        match owns.iter_mut().find(|(p, _)| p == pkg) {
            Some((_, v)) => v.push(path.to_string()),
            None => owns.push((pkg.to_string(), vec![path.to_string()])),
        }
    }
    for (_, paths) in &mut owns {
        paths.sort();
    }
    owns
}

/// `_desktop_map`: package -> {icon, categories} by reading the .desktop files it owns.
///
/// The launchers under /usr/share/applications, and one `pacman -Qo` for
/// who owns each, then read only those. Listing every file of every package
/// (`pacman -Ql`) to find them read 12 MB, 150 000 lines, for 90 files, and
/// glibc kept most of that heap after each scan: the window grew by 8 MB a
/// rescan.
pub fn desktop_map(names: &[String]) -> Result<BTreeMap<String, DesktopEntry>, String> {
    if names.is_empty() {
        return Ok(BTreeMap::new());
    }
    let files = desktop_files(Path::new(APPLICATIONS));
    if files.is_empty() {
        return Ok(BTreeMap::new());
    }
    let mut cmd: Vec<&str> = vec!["env", "LC_ALL=C", PACMAN, "-Qo"];
    cmd.extend(files.iter().map(String::as_str));
    let wanted: HashSet<&str> = names.iter().map(String::as_str).collect();
    let owns = owners_from(&run(&cmd, 60)?, &wanted);

    let mut result = BTreeMap::new();
    for (pkg, paths) in owns {
        for path in paths {
            let text = match std::fs::read(&path) {
                Ok(b) => String::from_utf8_lossy(&b).into_owned(),
                Err(_) => continue,
            };
            let (mut icon, mut cats) = (String::new(), String::new());
            for line in py_splitlines(&text) {
                if line.starts_with("Icon=") && icon.is_empty() {
                    icon = py_strip(line.split_once('=').map(|x| x.1).unwrap_or("")).to_string();
                } else if line.starts_with("Categories=") && cats.is_empty() {
                    cats = py_strip(line.split_once('=').map(|x| x.1).unwrap_or("")).to_string();
                } else if line.starts_with("NoDisplay=true") {
                    icon.clear(); // helper entry, not a real app
                    cats.clear();
                    break;
                }
            }
            if !icon.is_empty() || !cats.is_empty() {
                result.insert(
                    pkg.clone(),
                    DesktopEntry {
                        icon,
                        categories: cats,
                    },
                );
                break; // first real entry wins
            }
        }
    }
    Ok(result)
}

/// One run at a time of a slow query, its answer handed to everyone who
/// asked while it ran: the collector and Health starting together wait for
/// one pacman instead of starting two. `key` says which answer will do: a
/// caller with another key (the databases changed meanwhile) waits for the
/// run in flight to end, then runs its own. A run that panics answers no
/// one; whoever waited on it runs again.
pub(super) struct Flight<K, T> {
    state: std::sync::Mutex<FlightState<K, T>>,
    done: std::sync::Condvar,
}

struct FlightState<K, T> {
    running: Option<K>,
    /// runs finished so far
    seq: u64,
    /// the last run's key, its number and its answer
    last: Option<(K, u64, T)>,
}

impl<K: PartialEq + Clone, T: Clone> Flight<K, T> {
    pub(super) const fn new() -> Self {
        Flight {
            state: std::sync::Mutex::new(FlightState {
                running: None,
                seq: 0,
                last: None,
            }),
            done: std::sync::Condvar::new(),
        }
    }

    pub(super) fn run(&self, key: K, f: impl FnOnce() -> T) -> T {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        while let Some(running) = &st.running {
            let same = *running == key;
            let seq = st.seq;
            st = self
                .done
                .wait_while(st, |s| s.seq == seq)
                .unwrap_or_else(|e| e.into_inner());
            if same {
                if let Some((k, n, answer)) = &st.last {
                    if *k == key && *n > seq {
                        return answer.clone();
                    }
                }
            }
        }
        st.running = Some(key.clone());
        drop(st);

        /// Lets the waiters go however the run ends, a panic included.
        struct Landed<'a, K, T> {
            flight: &'a Flight<K, T>,
            answer: Option<(K, T)>,
        }
        impl<K, T> Drop for Landed<'_, K, T> {
            fn drop(&mut self) {
                let mut st = self.flight.state.lock().unwrap_or_else(|e| e.into_inner());
                st.running = None;
                st.seq += 1;
                let seq = st.seq;
                st.last = self.answer.take().map(|(k, v)| (k, seq, v));
                self.flight.done.notify_all();
            }
        }
        let mut landed = Landed {
            flight: self,
            answer: None,
        };
        let answer = f();
        landed.answer = Some((key, answer.clone()));
        answer
    }
}

/// Lines of checkupdates and `yay -Qua`: `name old -> new`, nothing else.
/// yay also prints notes on stdout ("-> Missing AUR Packages: …"), which
/// are not updates.
pub(super) fn pending_from(out: &str) -> BTreeMap<String, String> {
    let mut pending = BTreeMap::new();
    for line in py_splitlines(out) {
        let parts = py_split(line);
        if parts.len() >= 4
            && parts[2] == "->"
            && !parts[0].starts_with("->")
            && !parts[0].ends_with(':')
        {
            pending.insert(parts[0].to_string(), parts[3].to_string());
        }
    }
    pending
}

/// One checkupdates at a time from this process: two at once share its
/// temporary database, and the second fails.
static CHECKUPDATES: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(super) const CHECKUPDATES_BIN: &str = "/usr/bin/checkupdates";
/// Package tools by path: PATH puts user-writable folders (npm and mise
/// bins) first, and a `pacman` or `yay` planted there must not be what runs.
pub const PACMAN: &str = "/usr/bin/pacman";
/// The PATH checkupdates runs with. It is a script that finds pacman and
/// fakeroot through PATH; only folders root alone can write are on it.
pub const SYSTEM_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/bin";
pub const YAY: &str = "/usr/bin/yay";

/// What checkupdates' run says. Its exit status tells the answers apart:
/// 0 lists updates, 2 means there are none, anything else is a failure
/// (offline, a mirror down, another check running at the same time).
pub(super) fn repo_answer(
    ran: Result<RunResult, RunError>,
) -> Result<BTreeMap<String, String>, String> {
    match ran {
        Ok(r) => match r.code {
            Some(0) => Ok(pending_from(&r.stdout)),
            Some(2) => Ok(BTreeMap::new()),
            Some(n) => Err(format!(
                "checkupdates could not check (exit status {n}): offline, or a mirror is down"
            )),
            None => Err("checkupdates was stopped before it finished".into()),
        },
        Err(RunError::Os) => {
            Err("checkupdates could not be started (it comes with pacman-contrib)".into())
        }
        Err(RunError::Timeout) => Err("checkupdates did not answer within a minute".into()),
        Err(RunError::Decode(e)) => Err(e),
        Err(RunError::Nul) => Err("embedded null byte".into()),
    }
}

/// What `yay -Qua` says. yay exits 1 both when nothing is pending and when
/// it fails, so a failure is told apart by what it says on stderr (the AUR
/// unreachable, a rate limit).
pub(super) fn aur_answer(
    code: Option<i32>,
    stdout: &str,
    stderr: &str,
) -> Result<BTreeMap<String, String>, String> {
    let found = pending_from(stdout);
    match code {
        Some(0) => Ok(found),
        Some(1) if found.is_empty() && stderr.trim().is_empty() => Ok(found),
        Some(n) => {
            let why = stderr
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("");
            Err(if why.is_empty() {
                format!("yay could not check the AUR (exit status {n})")
            } else {
                format!("yay could not check the AUR: {why}")
            })
        }
        None => Err("yay was stopped before it finished".into()),
    }
}

/// Pending repository updates from checkupdates (which never touches the
/// system's databases), or why it could not tell.
pub fn check_repo() -> Result<BTreeMap<String, String>, String> {
    let _one = CHECKUPDATES.lock().unwrap_or_else(|e| e.into_inner());
    repo_answer(subprocess_run_env(
        &[CHECKUPDATES_BIN],
        60,
        &[("PATH", SYSTEM_PATH)],
    ))
}

/// Pending AUR updates from `yay -Qua`, or why it could not tell. Without
/// yay there is nothing to ask, and nothing pending.
pub fn check_aur() -> Result<BTreeMap<String, String>, String> {
    if !Path::new(YAY).exists() {
        return Ok(BTreeMap::new());
    }
    let out = crate::util::run_bytes(
        &[YAY, "-Qua"],
        &crate::util::RunOpts {
            timeout: Some(std::time::Duration::from_secs(90)),
            ..Default::default()
        },
    );
    if out.failed_to_start {
        return Err("yay could not be started".into());
    }
    if out.timed_out {
        return Err("yay did not answer within 90 seconds".into());
    }
    let stdout = py_decode(&out.stdout).map_err(|e| match e {
        RunError::Decode(e) => e,
        _ => "yay's output could not be read".into(),
    })?;
    aur_answer(out.code, &stdout, &String::from_utf8_lossy(&out.stderr))
}

/// `_pending_repo`: name -> new version, from checkupdates.
/// Err: it could not tell (see `repo_answer`), never "nothing pending".
pub fn try_pending_repo() -> Result<BTreeMap<String, String>, String> {
    check_repo()
}
/// When pacman's databases last changed: the local database directory
/// (a package installed, upgraded or removed) and the newest sync database
/// (`pacman -Sy`). None where they cannot be read, and then nothing is kept.
pub(super) fn pacman_db_stamp() -> Option<(std::time::SystemTime, std::time::SystemTime)> {
    let root = Path::new("/var/lib/pacman");
    let local = std::fs::metadata(root.join("local"))
        .and_then(|m| m.modified())
        .ok()?;
    let sync = std::fs::read_dir(root.join("sync"))
        .ok()?
        .filter_map(|e| e.ok()?.metadata().ok()?.modified().ok())
        .max()?;
    Some((local, sync))
}

/// How long `pending_both` keeps an answer while the databases stand still:
/// long enough that the scans an action, the log watcher and page changes
/// set off together share one, short enough that a release upstream still
/// shows up on its own.
pub(super) const PENDING_TTL: std::time::Duration = std::time::Duration::from_secs(600);

pub(super) type Pending = (BTreeMap<String, String>, BTreeMap<String, String>);

/// What `pacman_db_stamp` answers: the local and sync databases' mtimes.
pub(super) type DbStamp = (std::time::SystemTime, std::time::SystemTime);

/// The last `pending_both` answer, with the database stamp and time it was
/// taken at.
pub(super) static PENDING: std::sync::Mutex<Option<(DbStamp, std::time::Instant, Pending)>> =
    std::sync::Mutex::new(None);

/// What `pending_both` answers: each half, or why it could not tell.
pub type PendingAnswer = (
    Result<BTreeMap<String, String>, String>,
    Result<BTreeMap<String, String>, String>,
);

/// The run of the two checks in flight, keyed by the database stamp and how
/// many times the answer was forgotten: a caller after `forget_pending` does
/// not take an answer that was asked for before it.
static PENDING_RUN: Flight<(Option<DbStamp>, u64), PendingAnswer> = Flight::new();
static FORGOTTEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Why the last check for updates could not tell, None when it could.
static PENDING_PROBLEM: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Why the last look for pending updates could not tell: the Updates page
/// says so rather than "Everything is up to date". None when it could.
pub fn pending_problem() -> Option<String> {
    PENDING_PROBLEM
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// (`try_pending_repo`, `try_pending_aur`), run side by side: checkupdates
/// (0.9 s) and `yay -Qua` (1.2 s) do not wait on each other. An answer is
/// kept for `PENDING_TTL` while pacman's databases are unchanged; installing,
/// upgrading or syncing anything starts afresh, as does `forget_pending`.
/// Callers that ask while a check runs share its answer. A check that could
/// not tell is an Err for that half, and is never kept: asked again, it runs
/// again.
pub fn pending_both() -> PendingAnswer {
    pending_both_with(&|| {
        std::thread::scope(|s| {
            let aur = s.spawn(try_pending_aur);
            let repo = try_pending_repo();
            (
                repo,
                aur.join()
                    .unwrap_or_else(|_| Err("yay -Qua panicked".into())),
            )
        })
    })
}

/// `pending_both`, asking `check` for the two answers.
pub(super) fn pending_both_with(check: &dyn Fn() -> PendingAnswer) -> PendingAnswer {
    let stamp = pacman_db_stamp();
    if let Some(stamp) = stamp {
        let kept = PENDING.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, when, pending)) = kept.as_ref() {
            if *at == stamp && when.elapsed() < PENDING_TTL {
                return (Ok(pending.0.clone()), Ok(pending.1.clone()));
            }
        }
    }
    let key = (stamp, FORGOTTEN.load(std::sync::atomic::Ordering::SeqCst));
    let answer = PENDING_RUN.run(key, check);
    let problem = match &answer {
        (Ok(repo), Ok(aur)) => {
            if let Some(stamp) = stamp {
                *PENDING.lock().unwrap_or_else(|e| e.into_inner()) = Some((
                    stamp,
                    std::time::Instant::now(),
                    (repo.clone(), aur.clone()),
                ));
            }
            None
        }
        (Err(e), _) | (_, Err(e)) => Some(e.clone()),
    };
    *PENDING_PROBLEM.lock().unwrap_or_else(|e| e.into_inner()) = problem;
    answer
}

/// The last `pacman -Qq`, with the database stamp it was taken at.
pub(super) static INSTALLED: std::sync::Mutex<Option<(DbStamp, std::sync::Arc<HashSet<String>>)>> =
    std::sync::Mutex::new(None);

/// `pacman -Qq` as a set, kept while pacman's databases are unchanged: the
/// Discover page asks on every search.
pub(super) fn installed_names() -> Result<std::sync::Arc<HashSet<String>>, String> {
    let stamp = pacman_db_stamp();
    if let Some(stamp) = stamp {
        if let Some((at, names)) = INSTALLED.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            if *at == stamp {
                return Ok(names.clone());
            }
        }
    }
    let names: std::sync::Arc<HashSet<String>> = std::sync::Arc::new(
        py_split(&run(&[PACMAN, "-Qq"], 25)?)
            .into_iter()
            .map(String::from)
            .collect(),
    );
    if let Some(stamp) = stamp {
        *INSTALLED.lock().unwrap_or_else(|e| e.into_inner()) = Some((stamp, names.clone()));
    }
    Ok(names)
}

/// The last clean `pacman -Qmq`, with the database stamp it was taken at.
pub(super) static FOREIGN: std::sync::Mutex<Option<(DbStamp, std::sync::Arc<Vec<String>>)>> =
    std::sync::Mutex::new(None);

/// `pacman -Qmq` (installed packages no sync database carries), in pacman's
/// order, as `run` gives it: empty when pacman could not be run. It is the
/// slow query of the lot (it reads every sync database), and what it answers
/// moves only when a package comes or goes or the databases are synced,
/// which is exactly what the stamp sees; so an answer pacman gave cleanly is
/// kept while the stamp holds.
pub fn foreign_packages() -> Result<std::sync::Arc<Vec<String>>, String> {
    foreign_state().map(|(names, _)| names)
}

/// Whether `name` is a foreign package, None when pacman could not say.
pub fn is_foreign(name: &str) -> Option<bool> {
    let (names, clean) = foreign_state().ok()?;
    clean.then(|| names.iter().any(|n| n == name))
}

/// `foreign_packages`, with whether pacman answered cleanly (exit 0, or 1
/// with nothing listed: there are none).
pub(super) fn foreign_state() -> Result<(std::sync::Arc<Vec<String>>, bool), String> {
    let stamp = pacman_db_stamp();
    if let Some(stamp) = stamp {
        if let Some((at, names)) = FOREIGN.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            if *at == stamp {
                return Ok((names.clone(), true));
            }
        }
    }
    // The collector and Health ask at the same moment at launch: one
    // pacman answers both.
    FOREIGN_RUN.run(stamp, || {
        let (stdout, clean) = match subprocess_run(&[PACMAN, "-Qmq"], 25) {
            Ok(r) => {
                let clean =
                    r.code == Some(0) || (r.code == Some(1) && py_strip(&r.stdout).is_empty());
                (r.stdout, clean)
            }
            Err(RunError::Os | RunError::Timeout) => (String::new(), false),
            Err(RunError::Decode(e)) => return Err(e),
            Err(RunError::Nul) => return Err("embedded null byte".into()),
        };
        let names: std::sync::Arc<Vec<String>> =
            std::sync::Arc::new(py_split(&stdout).into_iter().map(String::from).collect());
        if let (Some(stamp), true) = (stamp, clean) {
            *FOREIGN.lock().unwrap_or_else(|e| e.into_inner()) = Some((stamp, names.clone()));
        }
        Ok((names, clean))
    })
}

type ForeignAnswer = Result<(std::sync::Arc<Vec<String>>, bool), String>;
static FOREIGN_RUN: Flight<Option<DbStamp>, ForeignAnswer> = Flight::new();

/// The explicit installs, split by where they come from: (`pacman -Qentq`,
/// `pacman -Qemq`), worked out from `pacman -Qetq`, `pacman -Qeq` and the
/// foreign list. -n and -m each read every sync database, which made the two
/// direct queries 0.2 s apiece; -m is needed anyway, and the explicit lists
/// without either take a few milliseconds. pacman's own order is kept: each
/// list is filtered in the order pacman gave it, never re-sorted.
pub(super) fn explicit_split(
    unrequired: &str,
    explicit: &str,
    foreign: &[String],
) -> (Vec<String>, Vec<String>) {
    let foreign: HashSet<&str> = foreign.iter().map(String::as_str).collect();
    let native = py_split(unrequired)
        .into_iter()
        .filter(|n| !foreign.contains(n))
        .map(String::from)
        .collect();
    let aur = py_split(explicit)
        .into_iter()
        .filter(|n| foreign.contains(n))
        .map(String::from)
        .collect();
    (native, aur)
}

/// Drop what `pending_both` keeps: the user asked for a fresh look.
pub fn forget_pending() {
    FORGOTTEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    *PENDING.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// `try_pending_repo` for callers that need a plain map: where the Python
/// raises, this answers `{}`.
pub fn pending_repo() -> BTreeMap<String, String> {
    try_pending_repo().unwrap_or_default()
}

/// The version pacman's own sync database offers for `name`, or ''.
///
/// checkupdates reads a private, freshly synced copy of the database, so it
/// can see a release the system database has not heard of yet. Installing
/// one package without -y uses the system copy, and against that copy the
/// package is already current -- pacman skips it and reports success.
///
/// Err where the Python raises: output that is not UTF-8, or a "Version"
/// line without a colon (IndexError).
pub fn try_synced_version(name: &str) -> Result<String, String> {
    for line in py_splitlines(&run(&["env", "LC_ALL=C", PACMAN, "-Si", name], 25)?) {
        if line.starts_with("Version") {
            let Some((_, v)) = line.split_once(':') else {
                return Err("list index out of range".into());
            };
            return Ok(py_strip(v).to_string());
        }
    }
    Ok(String::new())
}

/// `try_synced_version` for callers that need a plain answer: where the
/// Python raises, this answers ''.
pub fn synced_version(name: &str) -> String {
    try_synced_version(name).unwrap_or_default()
}

/// `_pending_aur`: name -> new version, from `yay -Qua`.
/// Err: it could not tell (see `aur_answer`), never "nothing pending".
pub fn try_pending_aur() -> Result<BTreeMap<String, String>, String> {
    check_aur()
}

/// Pending repository updates, or None when checkupdates could not tell
/// (offline, a mirror down, a timeout) -- and "nothing pending" read from
/// a failed check is how "Update all" would report every update installed.
pub fn checked_pending_repo() -> Option<BTreeMap<String, String>> {
    check_repo().ok()
}

/// Pending AUR updates, or None when yay could not tell.
pub fn checked_pending_aur() -> Option<BTreeMap<String, String>> {
    check_aur().ok()
}

/// `try_pending_aur` for callers that need a plain map: where the Python
/// raises, this answers `{}`.
pub fn pending_aur() -> BTreeMap<String, String> {
    try_pending_aur().unwrap_or_default()
}

/// Omarchy's own packages: the distro updating itself, which deserves to be
/// visible rather than buried as somebody's dependency.
pub const OMARCHY_PREFIX: &str = "omarchy";

/// `dict(line.split()[:2] for line in out.splitlines() if line.split())`.
/// Err: a one-word line, the ValueError `dict()` raises on a 1-element pair.
pub(super) fn version_map(out: &str) -> Result<HashMap<String, String>, String> {
    let mut versions = HashMap::new();
    let lines = py_splitlines(out)
        .into_iter()
        .map(py_split)
        .filter(|p| !p.is_empty());
    for (i, parts) in lines.enumerate() {
        if parts.len() < 2 {
            return Err(format!(
                "dictionary update sequence element #{i} has length 1; 2 is required"
            ));
        }
        versions.insert(parts[0].to_string(), parts[1].to_string());
    }
    Ok(versions)
}

/// Explicitly-installed packages, plus anything with an update waiting.
///
/// Listing only explicit installs keeps the dependency closure out of the
/// way -- nobody installed libjpeg on purpose. But a dependency with an
/// update pending is still something the user has to act on, and omarchy
/// itself arrives that way, so pending updates are always included.
///
/// Err where the Python raises (output that is not UTF-8, a one-word
/// `pacman -Q` line, an AUR reply of the wrong shape); `collect` then drops
/// the section.
pub fn packages() -> Result<Vec<App>, String> {
    // What pacman says, read side by side: none of these waits on another.
    let (unrequired, explicit_all, versions, pending, foreign_all) = std::thread::scope(|s| {
        let unrequired = s.spawn(|| run(&[PACMAN, "-Qetq"], 25));
        let explicit_all = s.spawn(|| run(&[PACMAN, "-Qeq"], 25));
        let versions = s.spawn(|| run(&[PACMAN, "-Q"], 25));
        let pending = s.spawn(pending_both);
        let foreign = foreign_state();
        let join = |h: std::thread::ScopedJoinHandle<'_, Result<String, String>>| {
            h.join().unwrap_or_else(|_| Err("pacman panicked".into()))
        };
        (
            join(unrequired),
            join(explicit_all),
            join(versions),
            pending.join().unwrap_or_else(|_| {
                let e = || Err("pending updates panicked".to_string());
                (e(), e())
            }),
            foreign,
        )
    });
    let (unrequired, explicit_all) = (unrequired?, explicit_all?);
    let (foreign_all, clean) = foreign_all?;
    let (mut repo_names, mut aur_names) = if clean {
        explicit_split(&unrequired, &explicit_all, &foreign_all)
    } else {
        // `pacman -Qmq` did not answer (a timeout while everything else
        // queries pacman too): ask the two direct questions rather than
        // count every AUR package as a repo one.
        let words =
            |s: String| -> Vec<String> { py_split(&s).into_iter().map(String::from).collect() };
        (
            words(run(&[PACMAN, "-Qentq"], 25)?),
            words(run(&[PACMAN, "-Qemq"], 25)?),
        )
    };
    let versions = version_map(&versions?)?;
    // Pull in any non-explicit package that has an update waiting, so the
    // list can never disagree with "Update all".
    // A check that could not tell lists nothing for its half, and
    // `pending_problem` says why: the rest of the list still stands.
    let (repo_pending, aur_pending) = pending;
    let (up_repo_pre, up_aur_pre) = (
        repo_pending.unwrap_or_default(),
        aur_pending.unwrap_or_default(),
    );
    let known: HashSet<String> = repo_names.iter().chain(aur_names.iter()).cloned().collect();
    let foreign: HashSet<String> = foreign_all.iter().cloned().collect();
    // (The Python walks a set here, in hash order; sorted is one fixed order.)
    let pending_names: BTreeSet<String> = up_repo_pre
        .keys()
        .chain(up_aur_pre.keys())
        .filter(|n| !known.contains(*n))
        .cloned()
        .collect();
    for name in pending_names {
        if foreign.contains(&name) || up_aur_pre.contains_key(&name) {
            aur_names.push(name);
        } else {
            repo_names.push(name);
        }
    }

    let all_names: Vec<String> = repo_names.iter().chain(aur_names.iter()).cloned().collect();
    // Four lookups on the final names, side by side.
    let (info, desktops, aur, vulns) = std::thread::scope(|s| {
        let info = s.spawn(|| package_info(&all_names));
        let desktops = s.spawn(|| desktop_map(&all_names));
        // "Security update" means the update fixes a known vulnerability in
        // the installed version, per the Arch Security Tracker. Only when
        // the tracker cannot be reached does the old guess-by-name stand in.
        let vulns = s.spawn(|| crate::security::vulnerabilities(Some(&versions), None));
        // Every foreign package is asked about, not only the listed ones:
        // the Health page's check asks about them all, and one kept answer
        // then serves both.
        let aur = if aur_names.is_empty() {
            Ok(BTreeMap::new())
        } else {
            try_aur_health(&foreign_all)
        };
        (
            info.join()
                .unwrap_or_else(|_| Err("pacman -Qi panicked".into())),
            desktops
                .join()
                .unwrap_or_else(|_| Err("pacman -Ql panicked".into())),
            aur,
            vulns.join().ok().flatten(),
        )
    });
    let (info, desktops, aur) = (info?, desktops?, aur?);
    let (up_repo, up_aur) = (up_repo_pre, up_aur_pre);
    // The explicit installs: the two lists above as pacman gave them, before
    // the pending names joined them (asking pacman again took 0.4 s).
    let explicit = known;
    let vuln_map = vulns.as_ref().map(|v| crate::security::by_package(v));

    let critical_for = |name: &str, pend: &str| -> String {
        if pend.is_empty() {
            return String::new();
        }
        let Some(vm) = &vuln_map else {
            return criticality(name);
        };
        let none = Vec::new();
        if !crate::security::fixed_by(vm.get(name).unwrap_or(&none), pend).is_empty() {
            return "security".into();
        }
        if criticality(name) == "core" {
            "core".into()
        } else {
            String::new()
        }
    };

    let empty_meta = BTreeMap::new();
    let empty_desk = DesktopEntry::default();
    let mut apps = Vec::new();
    for (names, source, pending) in [(&repo_names, REPO, &up_repo), (&aur_names, AUR, &up_aur)] {
        for n in names {
            let meta = info.get(n).unwrap_or(&empty_meta);
            let desk = desktops.get(n).unwrap_or(&empty_desk);
            let m = |k: &str| meta.get(k).cloned().unwrap_or_default();
            let pend = pending.get(n).cloned().unwrap_or_default();
            let version = versions.get(n).cloned();
            // Omarchy's own packages are the distro updating itself; grouping
            // them under System stops them reading as ordinary apps.
            let row_source = if n.starts_with(OMARCHY_PREFIX) {
                SYSTEM
            } else {
                source
            };
            let vulns_json: Vec<Value> = vuln_map
                .as_ref()
                .and_then(|vm| vm.get(n))
                .map(|vs| {
                    vs.iter()
                        .map(|v| {
                            json!({"group": v.group, "severity": v.severity,
                                        "kind": v.kind, "fixed": v.fixed,
                                        "cves": v.cves})
                        })
                        .collect()
                })
                .unwrap_or_default();
            let behind = if !pend.is_empty() {
                let (n, kind) = versions_behind_big(version.as_deref().unwrap_or(""), &pend);
                (big_json(&n), kind)
            } else {
                (json!(0), String::new())
            };
            let rec = aur.get(n);
            let mut extra = Map::new();
            extra.insert("icon".into(), json!(desk.icon));
            extra.insert("categories".into(), json!(desk.categories));
            extra.insert("size".into(), json!(m("size")));
            extra.insert("installed".into(), json!(m("installed")));
            extra.insert("built".into(), json!(m("built")));
            extra.insert("url".into(), json!(m("url")));
            extra.insert("license".into(), json!(m("license")));
            // A package with a .desktop entry is something the user
            // actually launches, rather than a toolchain or library
            // pulled in explicitly. Worth distinguishing in the list.
            extra.insert(
                "is_app".into(),
                json!(!desk.icon.is_empty() || !desk.categories.is_empty()),
            );
            extra.insert("dependency".into(), json!(!explicit.contains(n)));
            extra.insert("real_source".into(), json!(source));
            // Numeric twins of size/installed, purely so the columns
            // can sort on magnitude rather than on their text form.
            extra.insert("critical".into(), json!(critical_for(n, &pend)));
            extra.insert("vulns".into(), Value::Array(vulns_json));
            extra.insert("behind".into(), json!([behind.0, behind.1]));
            extra.insert(
                "maintainer".into(),
                json!(rec.map(|r| r.maintainer.clone()).unwrap_or_default()),
            );
            extra.insert(
                "orphaned".into(),
                json!(source == AUR && rec.is_some_and(|r| r.maintainer.is_empty())),
            );
            extra.insert("flagged".into(), json!(rec.is_some_and(|r| r.out_of_date)));
            extra.insert(
                "stale".into(),
                json!(staleness(n, source, &aur, desk, meta)),
            );
            extra.insert("size_mb".into(), json!(size_mb(&m("size"))));
            extra.insert("installed_ts".into(), json!(date_ts(&m("installed"))));
            extra.insert("built_ts".into(), json!(date_ts(&m("built"))));
            apps.push(App {
                name: n.clone(),
                version: version.unwrap_or_else(|| "?".into()),
                source: row_source.to_string(),
                description: m("description"),
                update: pend,
                exec_path: n.clone(),
                extra: extra.into(),
            });
        }
    }
    Ok(apps)
}

/// `_KEEP_ORPHANS`: orphans that are usually wanted back. Build tooling gets
/// pulled in by the next AUR package and removing it just means downloading
/// it again; a debug package was almost certainly a deliberate choice.
pub const KEEP_ORPHANS: &[&str] = &[
    "rust",
    "cargo",
    "go",
    "python-build",
    "python-installer",
    "python-wheel",
    "python-setuptools",
    "meson",
    "ninja",
    "cmake",
];

/// 'keep' when removing it is likely a false economy, else 'remove'.
pub fn orphan_advice(name: &str) -> String {
    if KEEP_ORPHANS.contains(&name) || name.ends_with("-debug") || name.ends_with("-docs") {
        return "keep".into();
    }
    "remove".into()
}

/// Packages installed as dependencies that nothing requires any more.
///
/// Listed separately rather than mixed into the main inventory: they were
/// never chosen, so they are a cleanup decision, not software the user owns.
///
/// Err where the Python raises: output that is not UTF-8, or a one-word
/// `pacman -Q` line.
pub fn try_orphans() -> Result<Vec<App>, String> {
    // Asked by the collector and by Health, often at once: one answer for
    // both, kept while the databases stand still (orphans change only when
    // a package comes or goes).
    let stamp = pacman_db_stamp();
    if let Some(stamp) = stamp {
        if let Some((at, apps)) = ORPHANS.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            if *at == stamp {
                return Ok(apps.clone());
            }
        }
    }
    ORPHAN_RUN.run(stamp, || {
        let apps = orphans_now()?;
        if let Some(stamp) = stamp {
            *ORPHANS.lock().unwrap_or_else(|e| e.into_inner()) = Some((stamp, apps.clone()));
        }
        Ok(apps)
    })
}

/// The last `try_orphans` answer, with the database stamp it was taken at.
static ORPHANS: std::sync::Mutex<Option<(DbStamp, Vec<App>)>> = std::sync::Mutex::new(None);
static ORPHAN_RUN: Flight<Option<DbStamp>, Result<Vec<App>, String>> = Flight::new();

fn orphans_now() -> Result<Vec<App>, String> {
    let names: Vec<String> = py_split(&run(&[PACMAN, "-Qdtq"], 30)?)
        .into_iter()
        .map(String::from)
        .collect();
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let info = package_info(&names)?;
    let versions = version_map(&run(&argv(&[PACMAN, "-Q"], &names), 25)?)?;
    let empty = BTreeMap::new();
    let mut apps = Vec::new();
    for n in &names {
        let meta = info.get(n).unwrap_or(&empty);
        let m = |k: &str| meta.get(k).cloned().unwrap_or_default();
        let advice = orphan_advice(n);
        let mut extra = Map::new();
        extra.insert("size".into(), json!(m("size")));
        extra.insert("size_mb".into(), json!(size_mb(&m("size"))));
        extra.insert("installed_ts".into(), json!(date_ts(&m("installed"))));
        extra.insert("url".into(), json!(m("url")));
        extra.insert("advice".into(), json!(advice));
        extra.insert("is_app".into(), json!(false));
        apps.push(App {
            name: n.clone(),
            version: versions.get(n).cloned().unwrap_or_else(|| "?".into()),
            source: ORPHAN.into(),
            description: m("description"),
            exec_path: String::new(),
            extra: extra.into(),
            ..Default::default()
        });
    }
    Ok(apps)
}

/// `try_orphans` for callers that need a plain list: where the Python
/// raises, this answers `[]`.
pub fn orphans() -> Vec<App> {
    try_orphans().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #[test]
    fn checkupdates_finds_its_tools_only_where_root_alone_writes() {
        use std::os::unix::fs::MetadataExt;
        for dir in std::env::split_paths(SYSTEM_PATH) {
            let Ok(m) = std::fs::metadata(&dir) else {
                continue;
            };
            assert_eq!(m.uid(), 0, "{} is not root's", dir.display());
            assert_eq!(
                m.mode() & 0o022,
                0,
                "{} is writable by others",
                dir.display()
            );
        }
        let here = include_str!("pacman.rs");
        let here = here.split("#[cfg(test)]").next().unwrap_or(here);
        let flat: String = here.split_whitespace().collect();
        assert!(flat.contains("&[CHECKUPDATES_BIN],60,&[(\"PATH\",SYSTEM_PATH)]"));
        let autorun = include_str!("../bin/almanac-autorun.rs");
        assert!(
            autorun.contains("SYSTEM_PATH"),
            "the scheduled run's checkupdates"
        );
    }

    use super::*;

    // ---- pending updates

    /// yay prints notes on stdout too; only `name old -> new` is an update.
    #[test]
    fn only_real_update_lines_are_updates() {
        let out = " -> Missing AUR Packages:  gone-pkg other\n\
                   :: Searching AUR for updates...\n\
                   foo 1.0-1 -> 1.1-1\n\
                   warning: bar: local (2.0-1) is newer than extra (1.9-1)\n\
                   baz 3-1 -> 4-1 [ignored]\n";
        let got = pending_from(out);
        assert_eq!(
            got.into_iter().collect::<Vec<_>>(),
            vec![
                ("baz".to_string(), "4-1".to_string()),
                ("foo".to_string(), "1.1-1".to_string()),
            ]
        );
    }

    fn ran(code: i32, stdout: &str) -> Result<RunResult, RunError> {
        Ok(RunResult {
            code: Some(code),
            stdout: stdout.to_string(),
        })
    }

    /// checkupdates: 0 lists updates, 2 means none, anything else (1 when
    /// offline, with nothing on stdout) is a check that could not run, and
    /// never "no updates".
    #[test]
    fn a_failed_checkupdates_is_an_error_not_no_updates() {
        assert_eq!(repo_answer(ran(0, "a 1-1 -> 2-1\n")).unwrap().len(), 1);
        assert_eq!(repo_answer(ran(2, "")), Ok(BTreeMap::new()));
        assert!(repo_answer(ran(1, "")).is_err());
        assert!(repo_answer(Err(RunError::Timeout)).is_err());
        assert!(repo_answer(Err(RunError::Os)).is_err());
    }

    #[test]
    fn yay_fails_when_it_says_so_on_stderr() {
        assert_eq!(aur_answer(Some(1), "", ""), Ok(BTreeMap::new()));
        assert_eq!(
            aur_answer(Some(1), " -> Missing AUR Packages: x\n", ""),
            Ok(BTreeMap::new())
        );
        assert_eq!(aur_answer(Some(0), "x 1-1 -> 2-1\n", "").unwrap().len(), 1);
        let e = aur_answer(
            Some(1),
            "",
            "error: Get \"https://aur.archlinux.org\": dial tcp: lookup failed\n",
        );
        assert!(e.unwrap_err().contains("dial tcp"));
    }

    /// Keeps the tests that read and write `pending_both`'s statics apart.
    static PENDING_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A check that could not tell is never kept: the next look asks again,
    /// and `pending_problem` says why meanwhile.
    #[test]
    fn a_failed_check_is_not_kept_and_is_told() {
        let _g = PENDING_TEST.lock().unwrap_or_else(|e| e.into_inner());
        forget_pending();
        let (repo, aur) = pending_both_with(&|| -> PendingAnswer {
            (Err("offline".into()), Ok(BTreeMap::new()))
        });
        assert!(repo.is_err() && aur.is_ok());
        assert_eq!(pending_problem().as_deref(), Some("offline"));
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let ok = || -> PendingAnswer {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            (
                Ok(BTreeMap::from([("a".to_string(), "2".to_string())])),
                Ok(BTreeMap::new()),
            )
        };
        assert_eq!(pending_both_with(&ok).0.unwrap().len(), 1);
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the failure was not served again"
        );
        assert_eq!(pending_problem(), None);
        let _ = pending_both_with(&ok);
        if pacman_db_stamp().is_some() {
            assert_eq!(
                calls.load(std::sync::atomic::Ordering::SeqCst),
                1,
                "a good answer is kept"
            );
        }
        forget_pending();
    }

    /// Callers that ask while a run is in flight share its answer; a caller
    /// with another key waits, then runs its own; a run that panics leaves
    /// its waiters to run again.
    #[test]
    fn overlapping_callers_share_one_run() {
        static F: Flight<u32, usize> = Flight::new();
        let runs = std::sync::atomic::AtomicUsize::new(0);
        let start = std::sync::Barrier::new(4);
        let answers: Vec<usize> = std::thread::scope(|s| {
            let hs: Vec<_> = (0..4)
                .map(|_| {
                    s.spawn(|| {
                        start.wait();
                        F.run(1, || {
                            std::thread::sleep(std::time::Duration::from_millis(300));
                            runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1
                        })
                    })
                })
                .collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(answers, vec![1, 1, 1, 1]);
        // Another key: a run of its own.
        assert_eq!(F.run(2, || 7), 7);
        // A panic answers no one, and does not wedge the next caller.
        let r = std::panic::catch_unwind(|| F.run(3, || panic!("boom")));
        assert!(r.is_err());
        assert_eq!(F.run(3, || 9), 9);
    }

    // ---- launchers

    #[test]
    fn launchers_are_found_and_put_to_their_package() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("kde4")).unwrap();
        for f in ["b.desktop", "a.desktop", "kde4/c.desktop", "mimeinfo.cache"] {
            std::fs::write(dir.path().join(f), "").unwrap();
        }
        let files = desktop_files(dir.path());
        let base = dir.path().to_str().unwrap();
        assert_eq!(
            files,
            vec![
                format!("{base}/a.desktop"),
                format!("{base}/b.desktop"),
                format!("{base}/kde4/c.desktop")
            ]
        );

        let out = "/usr/share/applications/zed.desktop is owned by zed 0.1-1\n\
                   /usr/share/applications/gcr-viewer.desktop is owned by gcr 3.41.2-2\n\
                   /usr/share/applications/gcr-prompter.desktop is owned by gcr 3.41.2-2\n\
                   error: No package owns /usr/share/applications/mine.desktop\n";
        let wanted: HashSet<&str> = ["gcr"].into_iter().collect();
        assert_eq!(
            owners_from(out, &wanted),
            vec![(
                "gcr".to_string(),
                vec![
                    "/usr/share/applications/gcr-prompter.desktop".to_string(),
                    "/usr/share/applications/gcr-viewer.desktop".to_string(),
                ],
            )]
        );
    }

    /// On this machine: the launchers found through /usr/share/applications
    /// are the ones `pacman -Ql` lists. Read-only; opt in with
    /// `cargo test -- --ignored`.
    #[test]
    #[ignore = "reads this machine's pacman database"]
    fn the_launchers_match_pacmans_file_lists_here() {
        let names: Vec<String> = py_split(&run(&["pacman", "-Qeq"], 60).unwrap())
            .into_iter()
            .map(String::from)
            .collect();
        let map = desktop_map(&names).unwrap();
        let wanted: HashSet<&str> = names.iter().map(String::as_str).collect();
        let mut listed: BTreeSet<String> = BTreeSet::new();
        for line in py_splitlines(&run(&argv(&["pacman", "-Ql"], &names), 60).unwrap()) {
            let (pkg, path) = line.split_once(' ').unwrap_or((line, ""));
            if path.ends_with(".desktop")
                && path.starts_with("/usr/share/applications/")
                && wanted.contains(pkg)
            {
                listed.insert(pkg.to_string());
            }
        }
        assert!(map.keys().all(|k| listed.contains(k)), "{map:?}");
        assert!(
            map.len() * 10 >= listed.len() * 7,
            "{} of {}",
            map.len(),
            listed.len()
        );
        // Heap after many scans: it must not grow scan after scan.
        let rss = || -> u64 {
            let s = std::fs::read_to_string("/proc/self/status").unwrap();
            s.lines()
                .find(|l| l.starts_with("VmRSS:"))
                .unwrap()
                .split_whitespace()
                .nth(1)
                .unwrap()
                .parse()
                .unwrap()
        };
        let first = rss();
        for _ in 0..20 {
            desktop_map(&names).unwrap();
        }
        eprintln!("VmRSS {first} kB -> {} kB", rss());
        assert!(rss() < first + 8 * 1024);
    }

    // ---- OrphanAdvice
    #[test]
    fn test_build_tooling_is_kept() {
        // Removing rust just means downloading 300MB again on the next AUR build.
        for name in ["rust", "python-build", "python-installer", "cmake"] {
            assert_eq!(orphan_advice(name), "keep", "{name}");
        }
    }

    #[test]
    fn test_debug_and_docs_are_kept() {
        assert_eq!(orphan_advice("logiops-debug"), "keep");
        assert_eq!(orphan_advice("foo-docs"), "keep");
    }

    #[test]
    fn test_ordinary_orphan_is_removable() {
        assert_eq!(orphan_advice("libfoo"), "remove");
    }

    #[test]
    fn test_nul_in_argv_is_an_error() {
        assert!(run(&["true", "a\0b"], 5).is_err());
    }

    #[test]
    fn the_explicit_lists_split_by_origin_keep_pacmans_order() {
        // -Qetq: explicit and required by nothing; -Qeq: every explicit one.
        let unrequired = "zed\nobs-studio\nyay\nobsidian\n";
        let explicit = "zed\nlibfoo\nobs-studio\nyay\nobsidian\nfoo-bin\n";
        let foreign = [
            "foo-bin".to_string(),
            "yay".to_string(),
            "dep-from-aur".to_string(),
        ];
        let (native, aur) = explicit_split(unrequired, explicit, &foreign);
        // Native: explicit and unrequired, as -Qentq has it (libfoo is
        // required by something, so it is not listed).
        assert_eq!(native, ["zed", "obs-studio", "obsidian"]);
        // Foreign: every explicit one, required or not, as -Qemq has it, in
        // -Qeq's order, not sorted.
        assert_eq!(aur, ["yay", "foo-bin"]);
    }

    #[test]
    fn nothing_foreign_leaves_every_explicit_package_native() {
        let (native, aur) = explicit_split("a\nb\n", "a\nb\nc\n", &[]);
        assert_eq!(native, ["a", "b"]);
        assert!(aur.is_empty());
    }

    /// On this machine: the split matches pacman's own -Qentq and -Qemq,
    /// and `is_foreign` matches `pacman -Qmq <name>`. Read-only; opt in with
    /// `cargo test -- --ignored`.
    #[test]
    #[ignore = "reads this machine's pacman database"]
    fn the_split_matches_pacmans_own_queries_here() {
        let q = |a: &str| run(&["pacman", a], 60).unwrap();
        let words =
            |s: String| -> Vec<String> { py_split(&s).into_iter().map(String::from).collect() };
        let foreign = words(q("-Qmq"));
        let (native, aur) = explicit_split(&q("-Qetq"), &q("-Qeq"), &foreign);
        assert_eq!(native, words(q("-Qentq")));
        assert_eq!(aur, words(q("-Qemq")));
        assert_eq!(*foreign_packages().unwrap(), foreign);
        for name in words(q("-Qeq")).iter().take(40).chain(foreign.iter()) {
            let direct = subprocess_run(&["pacman", "-Qmq", name], 20).unwrap();
            assert_eq!(is_foreign(name), Some(direct.code == Some(0)), "{name}");
        }
    }
}
