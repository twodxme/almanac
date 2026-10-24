//! What Python gave this section: OSError text, detached and checked
//! processes, str/repr of values.

use gtk::{gio, glib};

use crate::{inventory, util};
use crate::verify::ProbeValue;

/// `str(OSError)` as Python prints it: `[Errno 2] No such file or
/// directory: 'x'`.
pub(super) fn py_oserror(e: &std::io::Error, filename: &str) -> String {
    match e.raw_os_error() {
        Some(code) => {
            let text = e.to_string();
            let text = text.strip_suffix(&format!(" (os error {code})")).unwrap_or(&text);
            format!("[Errno {code}] {text}: {}", py_repr(filename))
        }
        None => e.to_string(),
    }
}

/// The OSError subclass Python raises for an errno, as its traceback names it.
pub(super) fn py_oserror_class(e: &std::io::Error) -> &'static str {
    match e.raw_os_error() {
        Some(libc::ENOENT) => "FileNotFoundError",
        Some(libc::EACCES) | Some(libc::EPERM) => "PermissionError",
        Some(libc::ENOTDIR) => "NotADirectoryError",
        Some(libc::EISDIR) => "IsADirectoryError",
        _ => "OSError",
    }
}

/// What Python prints for an exception nothing catches: the traceback,
/// ending in `Class: message`. The frames are the interpreter's; only the
/// header and the last line can be reproduced.
pub(super) fn uncaught(class: &str, message: &str) {
    eprintln!("Traceback (most recent call last):\n{class}: {message}");
}

/// `uncaught` for an error already spelled "Type: message".
pub(super) fn uncaught_err(e: &str) {
    eprintln!("Traceback (most recent call last):\n{e}");
}

/// The `G_SPAWN_ERROR` domain, read back as the errno it was made from.
#[derive(Clone, Copy)]
struct SpawnCode(i32);

impl glib::error::ErrorDomain for SpawnCode {
    fn domain() -> glib::Quark {
        glib::Quark::from_str("g-exec-error-quark")
    }
    fn code(self) -> i32 {
        self.0
    }
    fn from(code: i32) -> Option<Self> {
        Some(SpawnCode(code))
    }
}

fn spawn_errno(e: &glib::Error) -> std::io::Error {
    use glib::ffi as g;
    let Some(SpawnCode(code)) = e.kind::<SpawnCode>() else {
        return std::io::Error::other(e.message().to_string());
    };
    let errno = match code {
        g::G_SPAWN_ERROR_ACCES => libc::EACCES,
        g::G_SPAWN_ERROR_PERM => libc::EPERM,
        g::G_SPAWN_ERROR_TOO_BIG => libc::E2BIG,
        g::G_SPAWN_ERROR_NOEXEC => libc::ENOEXEC,
        g::G_SPAWN_ERROR_NAMETOOLONG => libc::ENAMETOOLONG,
        g::G_SPAWN_ERROR_NOENT => libc::ENOENT,
        g::G_SPAWN_ERROR_NOMEM => libc::ENOMEM,
        g::G_SPAWN_ERROR_NOTDIR => libc::ENOTDIR,
        g::G_SPAWN_ERROR_LOOP => libc::ELOOP,
        g::G_SPAWN_ERROR_TXTBUSY => libc::ETXTBSY,
        g::G_SPAWN_ERROR_IO => libc::EIO,
        g::G_SPAWN_ERROR_NFILE => libc::ENFILE,
        g::G_SPAWN_ERROR_MFILE => libc::EMFILE,
        g::G_SPAWN_ERROR_INVAL => libc::EINVAL,
        g::G_SPAWN_ERROR_ISDIR => libc::EISDIR,
        g::G_SPAWN_ERROR_LIBBAD => libc::ELIBBAD,
        _ => return std::io::Error::other(e.message().to_string()),
    };
    std::io::Error::from_raw_os_error(errno)
}

/// `subprocess.Popen(argv, start_new_session=True)`.
pub(super) fn popen_detached(argv: &[String]) -> std::io::Result<()> {
    if argv.is_empty() {
        return Err(std::io::Error::from_raw_os_error(libc::ENOENT));
    }
    let launcher = gio::SubprocessLauncher::new(gio::SubprocessFlags::STDIN_INHERIT);
    launcher.set_child_setup(|| {
        let _ = rustix::process::setsid();
    });
    let args: Vec<&std::ffi::OsStr> = argv.iter().map(std::ffi::OsStr::new).collect();
    launcher.spawn(&args).map(drop).map_err(|e| spawn_errno(&e))
}

/// Python's `str.isprintable()` for one character: everything but the
/// Unicode categories Other (Cc, Cf, Cs, Co, Cn) and Separator (Zl, Zp, Zs),
/// with the ASCII space let through.
fn py_printable(c: char) -> bool {
    static NON_PRINTABLE: std::sync::OnceLock<Option<regex::Regex>> = std::sync::OnceLock::new();
    if c == ' ' {
        return true;
    }
    match NON_PRINTABLE.get_or_init(|| regex::Regex::new(r"^[\p{C}\p{Z}]$").ok()) {
        Some(re) => !re.is_match(c.encode_utf8(&mut [0; 4])),
        None => !c.is_control(),
    }
}

/// Python's `repr(str)`.
pub(super) fn py_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::from(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if py_printable(c) => out.push(c),
            c if (c as u32) < 0x100 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if (c as u32) < 0x10000 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push_str(&format!("\\U{:08x}", c as u32)),
        }
    }
    out.push(quote);
    out
}

/// Python's `str(value)` of a JSON value as `json.loads` would hand it over.
pub(super) fn py_str_value(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => py_repr_value(other),
    }
}

/// Python's `repr(value)` of a JSON value.
fn py_repr_value(v: &serde_json::Value) -> String {
    use serde_json::Value as V;
    match v {
        V::Null => "None".to_string(),
        V::Bool(true) => "True".to_string(),
        V::Bool(false) => "False".to_string(),
        V::Number(n) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
            (Some(i), _, _) => i.to_string(),
            (None, Some(u), _) => u.to_string(),
            (None, None, Some(f)) => util::py_float_repr(f),
            _ => n.to_string(),
        },
        V::String(s) => py_repr(s),
        V::Array(a) => format!("[{}]", a.iter().map(py_repr_value).collect::<Vec<_>>().join(", ")),
        V::Object(o) => format!(
            "{{{}}}",
            o.iter().map(|(k, v)| format!("{}: {}", py_repr(k), py_repr_value(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// Python's `str.splitlines()`.
pub(super) fn py_splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        let end = match c {
            '\r' => {
                if let Some(&(_, '\n')) = it.peek() {
                    it.next();
                    i + 2
                } else {
                    i + 1
                }
            }
            '\n' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}' => {
                i + c.len_utf8()
            }
            _ => continue,
        };
        out.push(&s[start..i]);
        start = end;
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// Python's `str.strip()`.
pub(super) fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c))
}

/// `subprocess.run(argv, capture_output=True, text=True, timeout=n)`, with
/// the two exceptions it raises as `Err` (TimeoutExpired, OSError).
///
/// Through `util::run`, whose deadline covers the output too: a grandchild
/// left holding the pipe is a timeout, not a hang. A failed start keeps its
/// errno, so the OSError text carries it (`[Errno 13] Permission denied:
/// 'omarchy'`), and it reaches the task log.
pub(super) fn run_checked(argv: &[String], timeout: u64) -> Result<util::Output, String> {
    let Some(prog) = argv.first() else {
        // Popen([]) raises IndexError before anything starts.
        return Err("list index out of range".to_string());
    };
    let args: Vec<&str> = argv.iter().map(String::as_str).collect();
    let r = util::run(&args, timeout);
    if r.failed_to_start {
        return Err(match r.start_errno {
            Some(code) => py_oserror(&std::io::Error::from_raw_os_error(code), prog),
            None => r.start_error,
        });
    }
    if r.timed_out {
        let list: Vec<String> = argv.iter().map(|a| py_repr(a)).collect();
        return Err(format!("Command '[{}]' timed out after {timeout} seconds", list.join(", ")));
    }
    Ok(r)
}

/// Python's `bool(a)` of a probe answer.
pub(super) fn truthy(v: &Option<ProbeValue>) -> bool {
    match v {
        None => false,
        Some(ProbeValue::Bool(b)) => *b,
        Some(ProbeValue::Str(s)) => !s.is_empty(),
        Some(ProbeValue::Set(s)) => !s.is_empty(),
        Some(ProbeValue::Map(m)) => !m.is_empty(),
        Some(ProbeValue::Identity(..)) => true,
    }
}

pub(super) fn extra_str(app: &inventory::App, key: &str) -> String {
    app.extra.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_string_helpers() {
        assert_eq!(py_splitlines("a\r\nb\rc\n\nd\x0ce\u{2028}f\n"), vec!["a", "b", "c", "", "d", "e", "f"]);
        assert_eq!(py_splitlines(""), Vec::<&str>::new());
        assert_eq!(py_repr("omarchy"), "'omarchy'");
        assert_eq!(py_repr("it's"), "\"it's\"");
        assert_eq!(py_strip(" \x1cx\n"), "x");
        assert_eq!(py_strip("\u{85}\u{a0}x\u{3000}"), "x");
        assert_eq!(py_strip("\u{200b}x"), "\u{200b}x");
        // repr escapes what str.isprintable() rejects, in the width Python uses.
        assert_eq!(py_repr("a\u{85}b\u{a0}c"), "'a\\x85b\\xa0c'");
        assert_eq!(py_repr("\u{2028}\u{feff}é"), "'\\u2028\\ufeffé'");
        assert_eq!(py_repr("\u{e0001}"), "'\\U000e0001'");
        assert_eq!(py_str_value(&serde_json::Value::Null), "None");
        assert_eq!(py_str_value(&serde_json::json!("org.x")), "org.x");
        assert_eq!(py_str_value(&serde_json::json!([1, "a", true])), "[1, 'a', True]");
    }

    #[test]
    fn run_checked_reports_the_real_errno() {
        let dir = std::env::temp_dir().join(format!("almanac-noexec-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let f = dir.join("omarchy");
        std::fs::write(&f, "#!/bin/sh\n").unwrap();
        let prog = f.display().to_string();
        let err = run_checked(&[prog.clone()], 5).unwrap_err();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(err, format!("[Errno 13] Permission denied: '{prog}'"));
        let err = run_checked(&["/nonexistent/almanac-test".to_string()], 5).unwrap_err();
        assert_eq!(err, "[Errno 2] No such file or directory: '/nonexistent/almanac-test'");
        let r = run_checked(&["sh".into(), "-c".into(), "echo out; echo err >&2; exit 3".into()], 5).unwrap();
        assert_eq!((r.stdout.as_str(), r.stderr.as_str(), r.code), ("out\n", "err\n", Some(3)));
        let err = run_checked(&["sleep".into(), "5".into()], 0).unwrap_err();
        assert_eq!(err, "Command '['sleep', '5']' timed out after 0 seconds");
        // A grandchild still holding stdout is a timeout, not a hang.
        let t = std::time::Instant::now();
        let argv: Vec<String> = vec!["sh".into(), "-c".into(), "sleep 30 & echo started".into()];
        let err = run_checked(&argv, 1).unwrap_err();
        assert_eq!(err, "Command '['sh', '-c', 'sleep 30 & echo started']' timed out after 1 seconds");
        assert!(t.elapsed() < std::time::Duration::from_secs(10));
    }

    #[test]
    fn popen_starts_a_new_session() {
        let dir = std::env::temp_dir().join(format!("almanac-popen-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let out = dir.join("sid");
        let script = format!("ps -o sid= -p $$ > {}.tmp && mv {0}.tmp {0}", out.display());
        popen_detached(&["sh".into(), "-c".into(), script]).unwrap();
        let mut text = String::new();
        for _ in 0..200 {
            if let Ok(t) = std::fs::read_to_string(&out) {
                text = t;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = std::fs::remove_dir_all(&dir);
        let mine = rustix::process::getsid(None).unwrap().as_raw_nonzero().get();
        let theirs: i32 = text.trim().parse().unwrap();
        assert_ne!(theirs, mine);
    }

    #[test]
    fn popen_reports_the_os_error_like_python() {
        let e = popen_detached(&["/nonexistent/almanac-test".to_string()]).unwrap_err();
        assert_eq!(py_oserror(&e, "/nonexistent/almanac-test"), "[Errno 2] No such file or directory: '/nonexistent/almanac-test'");
    }

    #[test]
    fn oserror_reads_like_python() {
        let e = std::io::Error::from_raw_os_error(2);
        assert_eq!(py_oserror(&e, "x"), "[Errno 2] No such file or directory: 'x'");
    }
}
