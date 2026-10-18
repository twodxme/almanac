//! A local change to a checkout, held up against the update.
//!
//! Plugins and other git checkouts get patched in place: a fix that has not
//! been released yet, a setting the author did not expose. `git pull --ff-only`
//! then refuses to update -- correctly, since it will not touch uncommitted
//! work -- and the checkout sits behind forever, which is worse.
//!
//! What the user needs to know is how their change relates to the update, and
//! git can answer that exactly, without touching the working tree:
//!
//! * **absorbed** -- the update already contains the change, line for line. It
//!   can be dropped.
//! * **applies** -- the change still applies on top of the update. It can be
//!   carried forward.
//! * **conflicts** -- the update changed the same lines. The change is set aside
//!   (as a stash, so nothing is lost) and the update installed.
//!
//! Each is decided by applying the change to a scratch index built from the
//! upstream tree (`git apply --cached --check`), forwards and in reverse. The
//! working tree and the real index are never touched until the user chooses.
//!
//! "Absorbed" is deliberately exact. Upstream fixing the same bug a different
//! way is common -- and it lands in "conflicts", because deciding that two
//! different patches mean the same thing is a judgement git cannot make.
//!
//! Port of `patches.py`.

use serde::{Deserialize, Serialize};

pub const CLEAN: &str = "clean";
pub const ABSORBED: &str = "absorbed";
pub const APPLIES: &str = "applies";
pub const CONFLICTS: &str = "conflicts";
pub const UNKNOWN: &str = "unknown";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PatchState {
    pub kind: String,
    pub files: Vec<String>,
    /// touching `files`
    pub upstream_commits: Vec<String>,
    pub untracked: Vec<String>,
}

/// What escapes `_git` in the Python, sorted by whether `analyze` catches it.
enum GitError {
    /// OSError or subprocess.SubprocessError (git could not be started, or it
    /// ran past the timeout): `analyze` catches these and says UNKNOWN.
    Caught,
    /// Anything else -- UnicodeDecodeError, or the ValueError for a NUL in an
    /// argument -- passes straight through the `except` to `analyze`'s caller.
    Raised(String),
}

/// What `subprocess.run(..., capture_output=True, text=True)` hands back.
struct GitOutput {
    code: Option<i32>,
    stdout: String,
}

impl GitOutput {
    fn ok(&self) -> bool {
        self.code == Some(0)
    }
}

/// The codeset Python's `text=True` decodes with:
/// `locale.getpreferredencoding(False)`, as the interpreter would have set it
/// up at startup from this environment.
///
/// * `PYTHONUTF8=1` is UTF-8 mode: always UTF-8.
/// * Unset, UTF-8 mode switches itself on when the LC_CTYPE locale is "C" or
///   "POSIX" -- including a locale that is named but not installed, since
///   `setlocale` then fails and leaves "C" (PEP 540).
/// * `PYTHONUTF8=0` keeps the locale's own codeset, except that a "C" locale
///   is coerced to C.UTF-8 unless LC_ALL is set or `PYTHONCOERCECLOCALE=0`
///   (PEP 538); an uncoerced "C" is ASCII.
///
/// Python asks `nl_langinfo(CODESET)`; that needs `unsafe` here, so the same
/// answer comes from `locale charmap` under the LC_CTYPE locale. Worked out
/// once, as Python does at startup.
fn preferred_encoding() -> &'static str {
    static ENC: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ENC.get_or_init(|| {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let utf8_mode = var("PYTHONUTF8");
        if utf8_mode.as_deref() == Some("1") {
            return "UTF-8".to_string();
        }
        let name = var("LC_ALL").or_else(|| var("LC_CTYPE")).or_else(|| var("LANG"))
            .unwrap_or_else(|| "C".to_string());
        let out = crate::util::run_with(&["locale", "charmap"], &crate::util::RunOpts {
            timeout: Some(std::time::Duration::from_secs(5)),
            env: vec![("LC_ALL".to_string(), name.clone())],
            ..Default::default()
        });
        let charmap = out.stdout.trim().to_string();
        let is_c = name == "C" || name == "POSIX" || !out.ok() || !out.stderr.trim().is_empty()
            || charmap.is_empty() || charmap == "ANSI_X3.4-1968";
        if !is_c {
            return charmap;
        }
        if utf8_mode.as_deref() != Some("0") {
            return "UTF-8".to_string();
        }
        let coerce = var("LC_ALL").is_none() && var("PYTHONCOERCECLOCALE").as_deref() != Some("0");
        if coerce { "UTF-8" } else { "ANSI_X3.4-1968" }.to_string()
    })
}

/// `bytes.decode('utf-8')`, with its exact UnicodeDecodeError message: one
/// byte is "byte 0x.. in position n", a longer bad sequence is "bytes in
/// position a-b".
fn decode_utf8(raw: &[u8]) -> Result<String, String> {
    match std::str::from_utf8(raw) {
        Ok(s) => Ok(s.to_string()),
        Err(e) => {
            let pos = e.valid_up_to();
            let byte = raw[pos];
            let (len, reason) = match e.error_len() {
                None => (raw.len() - pos, "unexpected end of data"),
                Some(n) if n == 1 && ((0x80..0xc2).contains(&byte) || byte >= 0xf5) =>
                    (1, "invalid start byte"),
                Some(n) => (n, "invalid continuation byte"),
            };
            let at = if len == 1 {
                format!("byte {byte:#04x} in position {pos}")
            } else {
                format!("bytes in position {pos}-{}", pos + len - 1)
            };
            Err(format!("UnicodeDecodeError: 'utf-8' codec can't decode {at}: {reason}"))
        }
    }
}

/// `bytes.decode(encoding)` for the codeset `preferred_encoding` names.
fn decode(raw: &[u8], encoding: &str) -> Result<String, String> {
    match encoding.to_ascii_uppercase().as_str() {
        "UTF-8" | "UTF8" => decode_utf8(raw),
        "ANSI_X3.4-1968" | "ASCII" | "US-ASCII" => match raw.iter().position(|b| *b >= 0x80) {
            None => Ok(raw.iter().map(|b| *b as char).collect()),
            Some(pos) => Err(format!("UnicodeDecodeError: 'ascii' codec can't decode byte \
                                      {:#04x} in position {pos}: ordinal not in range(128)",
                                     raw[pos])),
        },
        // Latin-1 maps every byte, so it never raises. (encoding_rs would read
        // this label as windows-1252, which is a different codec.)
        "ISO-8859-1" | "ISO8859-1" | "LATIN1" => Ok(raw.iter().map(|b| *b as char).collect()),
        _ => match encoding_rs::Encoding::for_label(encoding.as_bytes()) {
            Some(enc) => enc.decode_without_bom_handling_and_without_replacement(raw)
                .map(|s| s.into_owned())
                .ok_or_else(|| format!("UnicodeDecodeError: '{}' codec can't decode the \
                                        output", encoding.to_ascii_lowercase())),
            None => decode_utf8(raw),
        },
    }
}

/// Python's text-mode pipe: a strict decode in the locale's encoding (see
/// `preferred_encoding`), then universal newlines (`\r\n` and a lone `\r`
/// both become `\n`). This is not cosmetic: the translated diff is what is
/// fed back to `git apply`, so a CRLF or CR file's patch no longer matches
/// its blobs and lands in "conflicts" -- the Python's behaviour, kept bug for
/// bug.
fn text_mode(raw: &[u8]) -> Result<String, String> {
    let s = decode(raw, preferred_encoding())?;
    Ok(s.replace("\r\n", "\n").replace('\r', "\n"))
}

/// `tempfile.gettempdir()`: the first of $TMPDIR, $TEMP, $TMP, /tmp,
/// /var/tmp, /usr/tmp and the current directory that a file can actually be
/// created and written in. Worked out once and remembered, as Python does.
/// None is Python's FileNotFoundError ("No usable temporary directory").
fn gettempdir() -> Option<&'static std::path::Path> {
    static DIR: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let mut dirs: Vec<std::path::PathBuf> = ["TMPDIR", "TEMP", "TMP"].iter()
            .filter_map(|k| std::env::var_os(k).filter(|v| !v.is_empty()))
            .map(std::path::PathBuf::from)
            .collect();
        dirs.extend(["/tmp", "/var/tmp", "/usr/tmp"].iter().map(std::path::PathBuf::from));
        dirs.push(std::env::current_dir().unwrap_or_else(|_| ".".into()));
        dirs.into_iter().find_map(|d| {
            let d = if d.as_os_str() == "." { d } else { std::path::absolute(&d).ok()? };
            let mut f = tempfile::Builder::new().tempfile_in(&d).ok()?;
            std::io::Write::write_all(&mut f, b"blat").ok()?;
            f.close().ok()?;
            Some(d)
        })
    }).as_deref()
}

/// `git -C repo <args>` with an optional environment and stdin, as `_git`.
///
/// Run here rather than through `util::run_with` because Python's text mode
/// has to be reproduced exactly (see `text_mode`), and like
/// `subprocess.run(input=None)` the child inherits our stdin when there is
/// no input.
fn git(repo: &str, args: &[&str], env: &[(String, String)], input_text: Option<&str>,
       timeout: u64) -> Result<GitOutput, GitError> {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    // subprocess refuses an argument with a NUL in it before starting
    // anything: ValueError, which `analyze` does not catch.
    if repo.contains('\0') || args.iter().any(|a| a.contains('\0')) {
        return Err(GitError::Raised("ValueError: embedded null byte".to_string()));
    }
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo).args(args)
        .stdin(if input_text.is_some() { Stdio::piped() } else { Stdio::inherit() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().map_err(|_| GitError::Caught)?;
    if let (Some(text), Some(mut stdin)) = (input_text, child.stdin.take()) {
        let text = text.to_owned();
        std::thread::spawn(move || {
            let _ = stdin.write_all(text.as_bytes());
        });
    }
    // Drain both pipes on their own threads, or a chatty child fills one and
    // blocks forever while we wait on it.
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            buf
        })
    };
    let out_t = drain(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err_t = drain(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let start = Instant::now();
    let limit = Duration::from_secs(timeout);
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => {
                if start.elapsed() >= limit {
                    // TimeoutExpired, a SubprocessError.
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(GitError::Caught);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return Err(GitError::Caught),
        }
    };
    let out = out_t.join().unwrap_or_default();
    let err = err_t.join().unwrap_or_default();
    // communicate() decodes stdout, then stderr; either can raise.
    let stdout = text_mode(&out).map_err(GitError::Raised)?;
    text_mode(&err).map_err(GitError::Raised)?;
    Ok(GitOutput { code: status.code(), stdout })
}

/// Python's `str.splitlines()`: every line boundary it knows, not just `\n`.
fn splitlines(s: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                lines.push(std::mem::take(&mut cur));
            }
            '\n' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}'
            | '\u{2029}' => lines.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

/// `[l for l in out.splitlines() if l]`
fn nonempty_lines(s: &str) -> Vec<String> {
    splitlines(s).into_iter().filter(|l| !l.is_empty()).collect()
}

fn state(kind: &str, files: Vec<String>, upstream_commits: Vec<String>, untracked: Vec<String>)
         -> PatchState {
    PatchState { kind: kind.to_string(), files, upstream_commits, untracked }
}

/// `upstream` None means the Python default, `"@{upstream}"`.
///
/// `Err` is what the Python raises to its caller: git output that does not
/// decode in the locale's encoding (a latin-1 file in the diff, a latin-1
/// commit subject, under UTF-8), or a NUL byte in `repo` or `upstream`.
pub fn try_analyze(repo: &str, upstream: Option<&str>) -> Result<PatchState, String> {
    let upstream = upstream.unwrap_or("@{upstream}");
    match analyze_inner(repo, upstream) {
        Ok(st) => Ok(st),
        Err(GitError::Caught) => Ok(state(UNKNOWN, vec![], vec![], vec![])),
        Err(GitError::Raised(msg)) => Err(msg),
    }
}

/// `try_analyze` for callers that need a state either way: an exception the
/// Python would raise comes back as UNKNOWN. (The faithful port is
/// `try_analyze`; in app.py such a raise kills the worker thread and no
/// dialog appears.)
pub fn analyze(repo: &str, upstream: Option<&str>) -> PatchState {
    try_analyze(repo, upstream).unwrap_or_else(|_| state(UNKNOWN, vec![], vec![], vec![]))
}

fn analyze_inner(repo: &str, upstream: &str) -> Result<PatchState, GitError> {
    let diff = git(repo, &["diff", "HEAD", "--binary"], &[], None, 30)?;
    if !diff.ok() {
        return Ok(state(UNKNOWN, vec![], vec![], vec![]));
    }
    let untracked = nonempty_lines(
        &git(repo, &["ls-files", "--others", "--exclude-standard"], &[], None, 30)?.stdout);
    if diff.stdout.trim().is_empty() {
        return Ok(state(CLEAN, vec![], vec![], untracked));
    }
    let files = nonempty_lines(&git(repo, &["diff", "HEAD", "--name-only"], &[], None, 30)?.stdout);
    let range = format!("HEAD..{upstream}");
    let mut log_args: Vec<&str> = vec!["log", "--format=%h %s", &range, "--"];
    log_args.extend(files.iter().map(String::as_str));
    let commits = nonempty_lines(&git(repo, &log_args, &[], None, 30)?.stdout);

    // `tempfile.TemporaryDirectory()`: in `gettempdir()`, which skips an
    // unusable $TMPDIR; failing that, an OSError, caught as UNKNOWN.
    let tmp = gettempdir()
        .and_then(|d| tempfile::Builder::new().prefix("tmp").tempdir_in(d).ok())
        .ok_or(GitError::Caught)?;
    let env = vec![("GIT_INDEX_FILE".to_string(),
                    tmp.path().join("index").to_string_lossy().into_owned())];
    if !git(repo, &["read-tree", upstream], &env, None, 30)?.ok() {
        return Ok(state(UNKNOWN, files, commits, untracked));
    }

    let check = |flags: &[&str]| -> Result<bool, GitError> {
        let mut args: Vec<&str> = vec!["apply", "--cached", "--check"];
        args.extend_from_slice(flags);
        Ok(git(repo, &args, &env, Some(&diff.stdout), 30)?.ok())
    };

    let kind = if check(&["-R"])? {
        ABSORBED
    } else if check(&[])? {
        APPLIES
    } else {
        CONFLICTS
    };
    Ok(state(kind, files, commits, untracked))
}

/// The task steps that update `repo` for this state. `pull` is the
/// command that fetches the update -- `git pull --ff-only` by default; an
/// Omarchy plugin passes `omarchy plugin update`, which also validates.
/// (None or an empty list means the default, as Python's `pull or [...]`.)
pub fn steps(repo: &str, state: &PatchState, label: &str, pull: Option<&[String]>)
             -> Vec<crate::tasks::Step> {
    use crate::tasks::Step;
    let sv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<String>>();
    let stash = sv(&["git", "-C", repo, "stash", "push", "-m",
                     &format!("almanac: local change before updating {label}")]);
    let pull: Vec<String> = match pull {
        Some(p) if !p.is_empty() => p.to_vec(),
        _ => sv(&["git", "-C", repo, "pull", "--ff-only"]),
    };
    if state.kind == APPLIES {
        return vec![Step::argv("Setting your change aside", stash),
                    Step::argv("Pulling the update", pull),
                    Step::argv("Putting your change back", sv(&["git", "-C", repo, "stash", "pop"]))];
    }
    if state.kind == ABSORBED || state.kind == CONFLICTS {
        // Kept as a stash either way: "absorbed" is exact, but a stash costs
        // nothing and a lost afternoon of work costs a great deal.
        return vec![Step::argv("Setting your change aside", stash),
                    Step::argv("Pulling the update", pull)];
    }
    vec![Step::argv("Pulling the update", pull)]
}

/// (heading, body, action label) for the confirmation dialog.
pub fn explain(state: &PatchState, name: &str) -> (String, String, String) {
    let files = state.files.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
        + if state.files.len() > 3 { "…" } else { "" };
    if state.kind == ABSORBED {
        return ("The update already includes your change".to_string(),
                format!("Your edit to {files} is in the new version, line for line. \
                         Updating replaces your copy with it; your copy is kept as a \
                         git stash in case you want it back."),
                "Update".to_string());
    }
    if state.kind == APPLIES {
        return (format!("Update {name} and keep your change?"),
                format!("Your edit to {files} still applies on top of the update. \
                         It is set aside, the update is pulled, and your edit is put \
                         back."),
                "Update and keep it".to_string());
    }
    if state.kind == CONFLICTS {
        let n = state.upstream_commits.len();
        let n = if n == 0 { "Some".to_string() } else { n.to_string() };
        return ("The update changes what you edited".to_string(),
                format!("{n} upstream commit(s) touch {files}, so your edit \
                         cannot be carried forward automatically. Updating sets it \
                         aside as a git stash — nothing is lost — and installs the \
                         new version. It may already do what your edit did."),
                "Update and set mine aside".to_string());
    }
    (format!("Update {name}?"), String::new(), "Update".to_string())
}

/// A step as the Python tuple `(label, argv)`.
fn step_json(s: &crate::tasks::Step) -> serde_json::Value {
    match &s.action {
        crate::tasks::StepAction::Argv(a) => serde_json::json!([s.label, a]),
        crate::tasks::StepAction::Call(_) => serde_json::json!([s.label, "<callable>"]),
    }
}

/// Parity hook: see ARCHITECTURE.md.
pub fn parity(func: &str, args: &serde_json::Value) -> Option<serde_json::Value> {
    use crate::util::arg_str;
    let st = || -> PatchState {
        serde_json::from_value(args.get("state").cloned().unwrap_or_default()).unwrap_or_default()
    };
    Some(match func {
        "analyze" => {
            let up = args.get("upstream").and_then(|v| v.as_str());
            match try_analyze(arg_str(args, "repo"), up) {
                Ok(st) => serde_json::to_value(st).ok()?,
                Err(e) => serde_json::json!({"error": e}),
            }
        }
        "steps" => {
            let pull: Option<Vec<String>> = args.get("pull")
                .and_then(|v| serde_json::from_value(v.clone()).ok());
            let out = steps(arg_str(args, "repo"), &st(), arg_str(args, "label"), pull.as_deref());
            serde_json::Value::Array(out.iter().map(step_json).collect())
        }
        "explain" => serde_json::to_value(explain(&st(), arg_str(args, "name"))).ok()?,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    //! A patched checkout, weighed against the update without touching it.
    use super::*;
    use std::path::{Path, PathBuf};

    fn git(args: &[&str], cwd: &Path) {
        let ok = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("git")
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    /// A tiny upstream with one file, and a clone of it.
    fn git_repo(d: &Path) -> (PathBuf, PathBuf) {
        let up = d.join("up");
        git(&["init", "-q", "-b", "main", up.to_str().unwrap()], d);
        std::fs::write(up.join("f.txt"), "one\ntwo\nthree\nfour\nfive\nsix\n").unwrap();
        git(&["add", "."], &up);
        git(&["commit", "-q", "-m", "first"], &up);
        let clone = d.join("up-clone");
        git(&["clone", "-q", up.to_str().unwrap(), clone.to_str().unwrap()], d);
        (up, clone)
    }

    fn argvs(steps: &[crate::tasks::Step]) -> Vec<String> {
        steps.iter().map(|s| match &s.action {
            crate::tasks::StepAction::Argv(a) => a.join(" "),
            crate::tasks::StepAction::Call(_) => String::new(),
        }).collect()
    }

    #[test]
    fn test_a_change_the_update_already_has_is_absorbed() {
        let d = tempfile::tempdir().unwrap();
        let (up, clone) = git_repo(d.path());
        std::fs::write(up.join("f.txt"), "one\nTWO\nthree\nfour\nfive\nsix\n").unwrap();
        git(&["commit", "-qam", "fix"], &up);
        git(&["fetch", "-q"], &clone);
        std::fs::write(clone.join("f.txt"), "one\nTWO\nthree\nfour\nfive\nsix\n").unwrap();
        assert_eq!(analyze(clone.to_str().unwrap(), None).kind, ABSORBED);
    }

    #[test]
    fn test_an_unrelated_change_still_applies() {
        let d = tempfile::tempdir().unwrap();
        let (up, clone) = git_repo(d.path());
        std::fs::write(up.join("f.txt"), "ONE\ntwo\nthree\nfour\nfive\nsix\n").unwrap();
        git(&["commit", "-qam", "upstream"], &up);
        git(&["fetch", "-q"], &clone);
        std::fs::write(clone.join("f.txt"), "one\ntwo\nthree\nfour\nfive\nSIX\n").unwrap();
        let c = clone.to_str().unwrap();
        let state = analyze(c, None);
        assert_eq!(state.kind, APPLIES);
        let labels: Vec<String> = steps(c, &state, "x", None).into_iter().map(|s| s.label).collect();
        assert_eq!(labels.last().unwrap(), "Putting your change back");
        // Analysing must not have touched the working tree.
        assert!(std::fs::read_to_string(clone.join("f.txt")).unwrap().contains("SIX"));
    }

    #[test]
    fn test_the_same_lines_changed_differently_conflict() {
        let d = tempfile::tempdir().unwrap();
        let (up, clone) = git_repo(d.path());
        std::fs::write(up.join("f.txt"), "one\nUPSTREAM\nthree\nfour\nfive\nsix\n").unwrap();
        git(&["commit", "-qam", "upstream"], &up);
        git(&["fetch", "-q"], &clone);
        std::fs::write(clone.join("f.txt"), "one\nMINE\nthree\nfour\nfive\nsix\n").unwrap();
        let c = clone.to_str().unwrap();
        let state = analyze(c, None);
        assert_eq!(state.kind, CONFLICTS);
        assert_eq!(state.upstream_commits.len(), 1);
        // Set aside, never discarded.
        let cmds = argvs(&steps(c, &state, "x", None));
        assert!(cmds.iter().any(|c| c.contains("stash push")));
        assert!(!cmds.iter().any(|c| c.contains("checkout") || c.contains("reset")));
    }

    #[test]
    fn test_a_clean_checkout_is_a_plain_pull() {
        let d = tempfile::tempdir().unwrap();
        let (_up, clone) = git_repo(d.path());
        let c = clone.to_str().unwrap();
        let state = analyze(c, None);
        assert_eq!(state.kind, CLEAN);
        assert_eq!(steps(c, &state, "x", None).len(), 1);
    }

    /// UnicodeDecodeError messages, word for word as Python 3.14 writes them.
    #[test]
    fn decode_errors_read_as_python_writes_them() {
        let cases: &[(&[u8], &str)] = &[
            (b"ab\xe2\x82A", "bytes in position 2-3: invalid continuation byte"),
            (b"ab\xe2\x82", "bytes in position 2-3: unexpected end of data"),
            (b"ab\xf0\x9f\x98", "bytes in position 2-4: unexpected end of data"),
            (b"\xe0\x80x", "byte 0xe0 in position 0: invalid continuation byte"),
            (b"\xc0", "byte 0xc0 in position 0: invalid start byte"),
            (b"\x80", "byte 0x80 in position 0: invalid start byte"),
            (b"\xff", "byte 0xff in position 0: invalid start byte"),
            (b"\xe2A", "byte 0xe2 in position 0: invalid continuation byte"),
            (b"\xf4\x90\x80\x80", "byte 0xf4 in position 0: invalid continuation byte"),
            (b"\xed\xa0\x80", "byte 0xed in position 0: invalid continuation byte"),
            (b"a\xe2\x82b", "bytes in position 1-2: invalid continuation byte"),
            (b"\xf0\x9f", "bytes in position 0-1: unexpected end of data"),
            (b"\xe2", "byte 0xe2 in position 0: unexpected end of data"),
        ];
        for (raw, want) in cases {
            assert_eq!(decode(raw, "UTF-8").unwrap_err(),
                       format!("UnicodeDecodeError: 'utf-8' codec can't decode {want}"));
        }
        assert_eq!(decode(b"caf\xe9", "ANSI_X3.4-1968").unwrap_err(),
                   "UnicodeDecodeError: 'ascii' codec can't decode byte 0xe9 in position 3: \
                    ordinal not in range(128)");
        assert_eq!(decode(b"caf\xe9", "ISO-8859-1").unwrap(), "caf\u{e9}");
    }

    #[test]
    fn a_nul_byte_in_an_argument_raises() {
        assert_eq!(try_analyze("/tmp/a\0b", None).unwrap_err(), "ValueError: embedded null byte");
    }
}
