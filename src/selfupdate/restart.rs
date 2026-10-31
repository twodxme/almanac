//! Starting the new code once it is on disk.

use super::*;

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
pub(super) fn restart_argv_or(running: Vec<String>) -> Result<Vec<String>, String> {
    let launcher = py_path_join(
        &py_home()?,
        &format!(".local/bin/{}", crate::watcher::command_name()),
    );
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
pub(super) fn running_program() -> Vec<String> {
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
pub(super) fn spawn_new_session(argv: &[String]) -> Result<(), ()> {
    use nix::spawn::{posix_spawn, PosixSpawnAttr, PosixSpawnFileActions, PosixSpawnFlags};
    use nix::sys::signal::{SigSet, Signal};
    use std::ffi::CString;
    use std::os::fd::AsRawFd;

    let Some(prog) = argv.first() else {
        return Err(());
    };
    let devnull = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")
        .map_err(drop)?;
    let mut actions = PosixSpawnFileActions::init().map_err(drop)?;
    actions.add_dup2(devnull.as_raw_fd(), 1).map_err(drop)?;
    actions.add_dup2(devnull.as_raw_fd(), 2).map_err(drop)?;
    // close_fds=True. glibc ignores a close of a descriptor that is already
    // gone by spawn time (the directory handle of this very listing).
    let mut open_fds: Vec<i32> = std::fs::read_dir("/proc/self/fd")
        .map(|dir| {
            dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
                .collect()
        })
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
    let args: Vec<CString> = argv
        .iter()
        .map(|a| CString::new(a.as_bytes()))
        .collect::<Result<_, _>>()
        .map_err(drop)?;
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
pub(super) fn exit_without_cleanup() -> ! {
    use std::os::unix::process::CommandExt;
    for prog in ["/usr/bin/true", "/bin/true", "true"] {
        let _ = Command::new(prog).env_clear().exec();
    }
    std::process::exit(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_starts_the_child_in_a_new_session() {
        // Fields after "(comm)": state ppid pgrp session ...
        let session = |stat: &str| -> String {
            stat.rsplit_once(") ")
                .map(|(_, r)| r.split_whitespace().nth(3).unwrap_or("").to_string())
                .unwrap_or_default()
        };
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("stat");
        let script = format!(
            "cat /proc/self/stat > '{}.tmp' && mv '{0}.tmp' '{0}'",
            out.display()
        );
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
    fn restart_argv_prefers_the_launcher_else_the_running_program() {
        let argv = restart_argv().unwrap();
        assert_eq!(argv.len(), 1);
        let exe = std::env::current_exe().unwrap();
        assert!(argv[0].ends_with("/.local/bin/almanac") || argv[0] == exe.to_string_lossy());
        assert_eq!(running_program(), vec![exe.to_string_lossy().into_owned()]);
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
}
