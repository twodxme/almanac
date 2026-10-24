//! Python process semantics this module leans on: `subprocess.run`, PATH
//! search, errno text, the home directory and pathlib's spelling of a path.

use super::*;

// ---- Python process semantics this module leans on ----

/// What `subprocess.run(..., capture_output=True)` returned.
pub(super) struct Captured {
    /// `returncode`: the exit status, or -signal for a killed process.
    pub(super) code: i32,
    pub(super) stdout: Vec<u8>,
    pub(super) stderr: Vec<u8>,
}

pub(super) enum RunError {
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
pub(super) fn run_capture(argv: &[OsString], timeout: Option<u64>) -> Result<Captured, RunError> {
    use crate::util::{run_bytes, RunOpts, Stdin};

    let Some((prog, args)) = argv.split_first() else {
        return Err(RunError::Os("[Errno 2] No such file or directory: ''".to_string()));
    };
    // _fork_exec turns each argument into a C string before anything runs.
    if argv.iter().any(|a| a.as_bytes().contains(&0)) {
        return Err(RunError::Raise("ValueError: embedded null byte".to_string()));
    }
    let opts = RunOpts {
        timeout: timeout.map(Duration::from_secs),
        stdin: Stdin::Inherit,
        arg0: Some(prog),
        ..Default::default()
    };
    let (mut first_err, mut last_err) = (None, None);
    for candidate in exec_candidates(prog) {
        let mut cmd: Vec<&OsStr> = Vec::with_capacity(argv.len());
        cmd.push(&candidate);
        cmd.extend(args.iter().map(OsString::as_os_str));
        // The timeout bounds reading the pipes as well as the exit: on
        // expiry the child's group is killed and the call returns, even
        // while a grandchild still holds the pipes.
        let out = run_bytes(&cmd, &opts);
        if out.timed_out {
            return Err(RunError::Timeout);
        }
        if !out.failed_to_start {
            let code = out.code.unwrap_or_else(|| -out.signal.unwrap_or(0));
            return Ok(Captured { code, stdout: out.stdout, stderr: out.stderr });
        }
        let n = out.start_errno.unwrap_or(libc::ENOENT);
        if n != libc::ENOENT && n != libc::ENOTDIR && first_err.is_none() {
            first_err = Some(n);
        }
        last_err = Some(n);
    }
    let n = first_err.or(last_err).unwrap_or(libc::ENOENT);
    Err(RunError::Os(format!("[Errno {n}] {}: {}", strerror(n), py_str_repr(&prog.to_string_lossy()))))
}

/// The paths CPython's `_execute_child` tries for `prog`: itself when it has a
/// slash, else `os.path.join(dir, prog)` for each entry of PATH (`os.defpath`
/// when unset). An empty entry means the current directory.
pub(super) fn exec_candidates(prog: &OsStr) -> Vec<OsString> {
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
pub(super) fn strerror(n: i32) -> String {
    let s = std::io::Error::from_raw_os_error(n).to_string();
    match s.rfind(" (os error ") {
        Some(i) => s[..i].to_string(),
        None => s,
    }
}

/// `text=True`: strict UTF-8, then universal newlines (\r\n and \r become \n).
/// Err is the UnicodeDecodeError, which no caller here catches.
pub(super) fn decode_text(b: &[u8]) -> Result<String, String> {
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
pub(super) fn py_home() -> Result<OsString, String> {
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
pub(super) fn passwd_home() -> Option<OsString> {
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
pub(super) fn py_path(p: &OsStr) -> OsString {
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
pub(super) fn py_path_join(base: &OsStr, rel: &str) -> OsString {
    let mut b = base.as_bytes().to_vec();
    if b.last() != Some(&b'/') {
        b.push(b'/');
    }
    b.extend_from_slice(rel.as_bytes());
    py_path(OsStr::from_bytes(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Instant;
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
}
