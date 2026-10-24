//! How Almanac was installed, and the state of its git checkout.

use super::*;

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
pub(super) fn pacman_owner(_root: &Path) -> Result<Result<Captured, RunError>, String> {
    pacman_owner_of(running_exe().into_os_string())
}

/// `pacman -Qoq <path>`, run the way `pacman_owner` runs it.
pub(super) fn pacman_owner_of(path: OsString) -> Result<Result<Captured, RunError>, String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_nul_raises_like_python() {
        assert_eq!(git_in(&ROOT, &["status\0x"], 5), Err("ValueError: embedded null byte".to_string()));
    }
}
