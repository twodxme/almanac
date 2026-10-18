//! Where things live, and how to reach a terminal, on anyone's desktop.
//!
//! Almanac is published for anyone's machine, so nothing here may assume the one
//! it was written on: base directories follow the XDG Base Directory spec (a
//! user can move any of them), and a terminal is found the way the desktop
//! would find one rather than by trying three favourites.
//!
//! Port of `xdg.py`.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};

use crate::util;

fn base(var: &str, default: &str) -> Result<PathBuf, String> {
    // var_os: Python reads the environment through surrogateescape, so a
    // value that is not UTF-8 is still used as the path it names.
    let value = std::env::var_os(var).unwrap_or_default();
    // The spec: a relative path is invalid and must be ignored.
    if !value.is_empty() && Path::new(&value).is_absolute() {
        Ok(py_path(&value))
    } else {
        Ok(home()?.join(default))
    }
}

/// `Path(value)`: pathlib collapses repeated separators and drops `.`
/// components and a trailing slash, but POSIX keeps exactly two leading
/// slashes as they are.
fn py_path(value: &OsStr) -> PathBuf {
    let p: PathBuf = Path::new(value).components().filter(|c| *c != Component::CurDir).collect();
    let b = value.as_bytes();
    if b.starts_with(b"//") && !b.starts_with(b"///") {
        let mut out = b"/".to_vec();
        out.extend_from_slice(p.as_os_str().as_bytes());
        PathBuf::from(OsString::from_vec(out))
    } else {
        p
    }
}

/// What `Path.home()` raises (as RuntimeError) when there is no home.
const NO_HOME: &str = "RuntimeError: Could not determine home directory.";

/// `Path.home()`, i.e. `Path(os.path.expanduser("~"))`: $HOME when it is set
/// (even empty), else the passwd entry of the real uid; trailing slashes are
/// stripped, an empty result becomes "/", and the result is a normalised Path.
///
/// Err where Python raises RuntimeError: expanduser gave "~" back, either
/// because $HOME is "~" (or "~/", ...) or because there is no $HOME and the
/// uid has no passwd entry.
fn home() -> Result<PathBuf, String> {
    let raw = match std::env::var_os("HOME") {
        Some(h) => h,
        None => match passwd_home() {
            Some(h) => h,
            // expanduser returns "~" unchanged on KeyError.
            None => return Err(NO_HOME.into()),
        },
    };
    let mut b = raw.into_vec();
    while b.last() == Some(&b'/') {
        b.pop();
    }
    if b.is_empty() {
        return Ok(PathBuf::from("/"));
    }
    if b == b"~" {
        return Err(NO_HOME.into());
    }
    Ok(py_path(OsStr::from_bytes(&b)))
}

/// `pwd.getpwuid(os.getuid()).pw_dir`: getpwuid_r, so the lookup follows
/// nsswitch.conf (files, systemd-homed, LDAP, ...) exactly as Python's does.
/// None where Python gets KeyError.
fn passwd_home() -> Option<OsString> {
    match nix::unistd::User::from_uid(nix::unistd::getuid()) {
        Ok(Some(u)) => Some(u.dir.into_os_string()),
        _ => None,
    }
}

/// `shutil.which(name)`: $PATH, or `os.confstr("CS_PATH")` when PATH is unset;
/// nothing at all when PATH is empty; and a match must pass
/// `os.access(X_OK)` for the real uid, not merely have some x bit set.
fn which(name: &OsStr) -> Option<PathBuf> {
    fn access_check(p: &Path) -> bool {
        p.exists() && rustix::fs::access(p, rustix::fs::Access::EXEC_OK).is_ok() && !p.is_dir()
    }
    if name.as_bytes().contains(&b'/') {
        let p = PathBuf::from(name);
        return access_check(&p).then_some(p);
    }
    let path = std::env::var_os("PATH").unwrap_or_else(|| OsString::from("/bin:/usr/bin"));
    if path.is_empty() {
        return None;
    }
    let mut seen: Vec<&[u8]> = Vec::new();
    for dir in path.as_bytes().split(|c| *c == b':') {
        if seen.contains(&dir) {
            continue;
        }
        seen.push(dir);
        let p = Path::new(OsStr::from_bytes(dir)).join(name);
        if access_check(&p) {
            return Some(p);
        }
    }
    None
}

/// Every caller in the app is still infallible: these wrappers log what
/// Python would raise and answer under "/", which an ordinary user cannot
/// write to, so the write that follows fails as the Python call would have.
/// New callers should use the `try_` form and handle the error.
fn report(r: Result<PathBuf, String>) -> PathBuf {
    r.unwrap_or_else(|e| {
        eprintln!("almanac: {e}");
        PathBuf::from("/")
    })
}

/// `data_home()`; Err where the Python raises.
pub fn try_data_home() -> Result<PathBuf, String> {
    base("XDG_DATA_HOME", ".local/share")
}

/// [`try_data_home`], logged and answered under "/" where the Python raises.
pub fn data_home() -> PathBuf {
    report(try_data_home())
}

/// `config_home()`; Err where the Python raises.
pub fn try_config_home() -> Result<PathBuf, String> {
    base("XDG_CONFIG_HOME", ".config")
}

/// [`try_config_home`], logged and answered under "/" where the Python raises.
pub fn config_home() -> PathBuf {
    report(try_config_home())
}

/// `state_home()`; Err where the Python raises.
pub fn try_state_home() -> Result<PathBuf, String> {
    base("XDG_STATE_HOME", ".local/state")
}

/// [`try_state_home`], logged and answered under "/" where the Python raises.
pub fn state_home() -> PathBuf {
    report(try_state_home())
}

/// `cache_home()`; Err where the Python raises.
pub fn try_cache_home() -> Result<PathBuf, String> {
    base("XDG_CACHE_HOME", ".cache")
}

/// [`try_cache_home`], logged and answered under "/" where the Python raises.
pub fn cache_home() -> PathBuf {
    report(try_cache_home())
}

/// ~/.local/bin: not in the spec's variables, but named by it.
/// Err where the Python raises.
pub fn try_bin_home() -> Result<PathBuf, String> {
    Ok(home()?.join(".local/bin"))
}

/// [`try_bin_home`], logged and answered under "/" where the Python raises.
pub fn bin_home() -> PathBuf {
    report(try_bin_home())
}

/// How each terminal takes a command to run. Most use -e; several do not, and
/// guessing wrong opens an empty shell instead of running anything.
const TERMINALS: &[(&str, &[&str])] = &[
    ("foot", &[]),
    ("alacritty", &["-e"]),
    ("kitty", &[]),
    ("ghostty", &["-e"]),
    ("wezterm", &["start", "--"]),
    ("konsole", &["-e"]),
    ("gnome-terminal", &["--wait", "--"]),
    ("kgx", &["-e"]),
    ("xfce4-terminal", &["--disable-server", "-x"]),
    ("tilix", &["-e"]),
    ("xterm", &["-e"]),
];

/// argv that runs `command` in a terminal window and waits for it.
///
/// `xdg-terminal-exec` first: it is the freedesktop proposal for exactly
/// this, honours the user's chosen terminal, and Omarchy and several other
/// distributions ship it. Then $TERMINAL, then whatever is installed.
///
/// OsString because $TERMINAL is read as bytes (Python's surrogateescape):
/// a terminal at a path that is not UTF-8 is still found and run.
pub fn terminal_argv_os<S: AsRef<str>>(command: &[S]) -> Option<Vec<OsString>> {
    terminal_argv_with(command, &|n| which(n), &std::env::var_os("TERMINAL").unwrap_or_default())
}

/// [`terminal_argv_os`] as Strings, for callers that spawn with `&[String]`.
/// Only a $TERMINAL that is not UTF-8 is affected: it is converted lossily,
/// so starting it fails and the caller reports that; use `terminal_argv_os`
/// to run it.
pub fn terminal_argv<S: AsRef<str>>(command: &[S]) -> Option<Vec<String>> {
    terminal_argv_os(command).map(|a| a.into_iter().map(|s| s.to_string_lossy().into_owned()).collect())
}

/// `terminal_argv` with `shutil.which` and `$TERMINAL` passed in, so tests can
/// stand in for the machine (the Python tests patch `xdg.shutil.which`).
fn terminal_argv_with<S: AsRef<str>>(
    command: &[S],
    which: &dyn Fn(&OsStr) -> Option<PathBuf>,
    chosen: &OsStr,
) -> Option<Vec<OsString>> {
    let command = command.iter().map(|c| OsString::from(c.as_ref()));
    if which(OsStr::new("xdg-terminal-exec")).is_some() {
        return Some(std::iter::once(OsString::from("xdg-terminal-exec")).chain(command).collect());
    }
    if !chosen.is_empty() && which(chosen).is_some() {
        // Path(chosen).name
        let name = Path::new(chosen).file_name().unwrap_or_default();
        let flags: &[&str] =
            TERMINALS.iter().find(|(t, _)| OsStr::new(t) == name).map(|(_, f)| *f).unwrap_or(&["-e"]);
        let mut argv = vec![chosen.to_os_string()];
        argv.extend(flags.iter().map(OsString::from));
        argv.extend(command);
        return Some(argv);
    }
    for (name, flags) in TERMINALS {
        if which(OsStr::new(name)).is_some() {
            let mut argv = vec![OsString::from(name)];
            argv.extend(flags.iter().map(OsString::from));
            argv.extend(command);
            return Some(argv);
        }
    }
    None
}

/// The full-upgrade command this system's own tooling expects.
pub fn system_update_command() -> String {
    system_update_command_with(&|n| which(OsStr::new(n)))
}

fn system_update_command_with(which: &dyn Fn(&str) -> Option<PathBuf>) -> String {
    if which("omarchy").is_some() {
        return "omarchy update".into(); // snapshots, migrations, then pacman
    }
    if which("yay").is_some() {
        return "yay -Syu".into();
    }
    if which("paru").is_some() {
        return "paru -Syu".into();
    }
    "sudo pacman -Syu".into()
}

/// Parity hook: see ARCHITECTURE.md.
///
/// `args.env` (optional) is `{VAR: value | null}` applied for the duration of
/// the call (null removes the variable), mirroring the Python case's
/// `mock.patch.dict(os.environ, ...)`. The parity runner is single-threaded.
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
    let path = |p: Result<PathBuf, String>| match p {
        Ok(p) => serde_json::Value::String(p.to_string_lossy().into_owned()),
        Err(e) => serde_json::json!({ "error": e }),
    };
    let out = match func {
        "data_home" => Some(path(try_data_home())),
        "config_home" => Some(path(try_config_home())),
        "state_home" => Some(path(try_state_home())),
        "cache_home" => Some(path(try_cache_home())),
        "bin_home" => Some(path(try_bin_home())),
        "terminal_argv" => Some(serde_json::to_value(terminal_argv(&util::arg_strs(args, "command"))).unwrap_or_default()),
        "system_update_command" => Some(serde_json::Value::String(system_update_command())),
        _ => None,
    };
    for (k, old) in saved {
        match old {
            Some(v) => std::env::set_var(&k, v),
            None => std::env::remove_var(&k),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_env<T>(vars: &[(&str, &str)], f: impl FnOnce() -> T) -> T {
        // base() reads the environment; these tests are the only ones that
        // touch XDG_DATA_HOME / XDG_CONFIG_HOME, and run under one lock.
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
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

    fn os(v: &[&str]) -> Vec<OsString> {
        v.iter().map(OsString::from).collect()
    }

    #[test]
    fn test_base_directories_follow_xdg() {
        with_env(&[("XDG_DATA_HOME", "/srv/data"), ("XDG_CONFIG_HOME", "relative/ignored")], || {
            assert_eq!(data_home(), PathBuf::from("/srv/data"));
            assert_eq!(config_home(), home().unwrap().join(".config"));
        });
    }

    #[test]
    fn test_a_home_of_tilde_raises_like_path_home() {
        with_env(&[("XDG_CACHE_HOME", ""), ("HOME", "~//")], || {
            assert_eq!(try_cache_home(), Err(NO_HOME.to_string()));
            assert_eq!(try_bin_home(), Err(NO_HOME.to_string()));
        });
    }

    #[test]
    fn test_the_desktops_terminal_launcher_wins() {
        let which = |n: &OsStr| Some(Path::new("/usr/bin").join(n));
        assert_eq!(
            terminal_argv_with(&["true"], &which, &std::env::var_os("TERMINAL").unwrap_or_default()),
            Some(os(&["xdg-terminal-exec", "true"]))
        );
    }

    #[test]
    fn test_each_terminal_gets_its_own_flags() {
        let which = |n: &OsStr| (n == "gnome-terminal").then(|| PathBuf::from(n));
        assert_eq!(terminal_argv_with(&["true"], &which, OsStr::new("")), Some(os(&["gnome-terminal", "--wait", "--", "true"])));
    }

    #[test]
    fn test_a_terminal_named_in_bytes_is_still_chosen() {
        let chosen = OsStr::from_bytes(b"/opt/t\xff/kitty");
        let which = |n: &OsStr| (n == chosen).then(|| PathBuf::from(n));
        let mut want = vec![chosen.to_os_string()];
        want.push(OsString::from("true"));
        assert_eq!(terminal_argv_with(&["true"], &which, chosen), Some(want));
    }

    #[test]
    fn test_no_omarchy_means_the_plain_updater() {
        let which = |n: &str| (n == "yay").then(|| PathBuf::from(n));
        assert_eq!(system_update_command_with(&which), "yay -Syu");
    }
}
