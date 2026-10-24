//! Background services of the user session: systemd user units.
//!
//! Only units that can actually be switched -- `enabled` or `disabled` in
//! `systemctl --user list-unit-files`, services and timers. Static, generated,
//! transient and alias units have no switch and are left out.
//!
//! Some of these are the session's plumbing. Switching off PipeWire takes all
//! sound with it; switching off the portal breaks file pickers and screen
//! sharing. Those are marked essential, and the page asks first and says what
//! stops working. Omarchy's own units are marked as such, since a later
//! `omarchy update` may expect them.
//!
//! Port of `services.py`.

use serde::{Deserialize, Serialize};

// Exact unit names -> what stops working without it. Names, not prefixes:
// hyprsunset is a blue-light filter and xdg-desktop-portal-rewrite-launchers
// a helper, and a prefix match called both essential.
const SOUND: &str = "All sound, and screen sharing";
const PORTAL: &str = "File pickers, screen sharing and links from apps";
pub const ESSENTIAL: &[(&str, &str)] = &[
    ("pipewire.service", SOUND),
    ("pipewire.socket", SOUND),
    ("pipewire-pulse.service", SOUND),
    ("pipewire-pulse.socket", SOUND),
    ("wireplumber.service", "All sound"),
    ("xdg-desktop-portal.service", PORTAL),
    ("xdg-desktop-portal-hyprland.service", PORTAL),
    ("xdg-desktop-portal-gtk.service", PORTAL),
    ("dbus.service", "Almost everything in the session"),
    ("dbus-broker.service", "Almost everything in the session"),
    ("gnome-keyring-daemon.service", "Saved passwords and SSH keys"),
    ("at-spi-dbus-bus.service", "Screen readers and accessibility tools"),
    ("gvfs-daemon.service", "Network locations and devices in the file manager"),
    ("hyprpolkitagent.service", "Password prompts for system changes"),
];

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Service {
    pub unit: String,
    pub description: String,
    pub enabled: bool,
    pub active: bool,
    /// what stops working, '' if nothing much
    pub essential: String,
    pub omarchy: bool,
}

pub fn available() -> bool {
    which_systemctl()
}

/// `bool(shutil.which("systemctl"))`, with shutil's PATH rules: PATH unset
/// falls back to `os.confstr("CS_PATH")` (/bin:/usr/bin), PATH="" finds
/// nothing, and an empty entry (PATH=":") means the current directory.
fn which_systemctl() -> bool {
    use std::path::Path;
    let path = match std::env::var_os("PATH") {
        Some(p) => p,
        None => "/bin:/usr/bin".into(),
    };
    if path.is_empty() {
        return false;
    }
    // shutil's _access_check: exists, os.access(F_OK | X_OK), not a directory.
    let check = |p: &Path| {
        p.exists()
            && rustix::fs::access(p, rustix::fs::Access::EXISTS | rustix::fs::Access::EXEC_OK).is_ok()
            && !p.is_dir()
    };
    std::env::split_paths(&path).any(|d| {
        let p = if d.as_os_str().is_empty() { Path::new("systemctl").to_path_buf() } else { d.join("systemctl") };
        check(&p)
    })
}

/// `systemctl --user <args>`'s stdout, or "" when it could not run.
///
/// This is `subprocess.run(..., capture_output=True, text=True,
/// timeout=timeout)` exactly, through `util::run_bytes` so the decoding is
/// Python's: the child inherits our stdin; the timeout covers reading the
/// pipes to the end as well as the exit, so a child that leaves a
/// descendant holding stdout times out (communicate() waits for the pipes);
/// the output is decoded strictly, in the locale's encoding, so bytes it
/// cannot decode raise UnicodeDecodeError -- a ValueError, which the
/// `except` below does not catch -- and text=True's universal newlines turn
/// \r\n and \r into \n.
///
/// The arguments are Python strings (UTF-16 here, so a lone surrogate
/// survives): subprocess encodes each with os.fsencode, in order, and
/// rejects one holding a NUL with ValueError("embedded null byte") -- also
/// not caught below, so it reaches the caller.
///
/// (One thing it does not copy: close_fds=True, which closes every inherited
/// descriptor above 2 in the child. std's Command has no safe way to do that;
/// everything std and GLib open is close-on-exec anyway, and systemctl's
/// output does not depend on it.)
fn run(args: &[Vec<u16>], timeout: u64) -> Result<String, String> {
    use std::os::unix::ffi::OsStringExt;
    let mut argv: Vec<std::ffi::OsString> = vec!["systemctl".into(), "--user".into()];
    for a in args {
        let b = fsencode(a)?;
        if b.contains(&0) {
            return Err("ValueError: embedded null byte".to_owned());
        }
        argv.push(std::ffi::OsString::from_vec(b));
    }
    let out = crate::util::run_bytes(&argv, &crate::util::RunOpts {
        timeout: Some(std::time::Duration::from_secs(timeout)),
        stdin: crate::util::Stdin::Inherit,
        ..Default::default()
    });
    // OSError, and TimeoutExpired (run() kills and reaps the child) -> "".
    if out.failed_to_start || out.timed_out {
        return Ok(String::new());
    }
    let stdout = py_decode(out.stdout)?;
    py_decode(out.stderr)?;
    Ok(stdout)
}
/// `os.fsencode` (UTF-8 with surrogateescape): a lone surrogate
/// \udc80-\udcff becomes the raw byte it stands for; any other lone
/// surrogate raises UnicodeEncodeError, naming the run of surrogates it is
/// in from the first one that cannot be escaped.
fn fsencode(s: &[u16]) -> Result<Vec<u8>, String> {
    let chars: Vec<Result<char, u16>> = char::decode_utf16(s.iter().copied())
        .map(|c| c.map_err(|e| e.unpaired_surrogate())).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            Ok(c) => {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                i += 1;
            }
            Err(_) => {
                let mut end = i;
                while end < chars.len() && chars[end].is_err() {
                    end += 1;
                }
                for k in i..end {
                    match chars[k] {
                        Err(u) if (0xdc80..=0xdcff).contains(&u) => out.push((u & 0xff) as u8),
                        Err(u) => {
                            return Err(if end - k == 1 {
                                format!("UnicodeEncodeError: 'utf-8' codec can't encode character \
                                         '\\u{u:04x}' in position {k}: surrogates not allowed")
                            } else {
                                format!("UnicodeEncodeError: 'utf-8' codec can't encode characters \
                                         in position {k}-{}: surrogates not allowed", end - 1)
                            });
                        }
                        Ok(_) => {}
                    }
                }
                i = end;
            }
        }
    }
    Ok(out)
}

/// A Python string (UTF-16, lone surrogates and all) as Rust text: a lone
/// surrogate, which a Rust string cannot hold, becomes U+FFFD.
fn lossy(s: &[u16]) -> String {
    String::from_utf16_lossy(s)
}

/// The codecs text=True can end up with here.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Codec {
    Utf8,
    Latin1,
    Ascii,
}

/// `locale.getpreferredencoding(False)`, which text=True decodes with, as
/// Python works it out at startup: UTF-8 mode (PYTHONUTF8=1, or by itself in
/// the C/POSIX locale -- which is also where a locale that is not installed
/// leaves it) means UTF-8; PYTHONUTF8=0 in the C locale still gets UTF-8 by
/// coercing to C.UTF-8 unless LC_ALL is set or PYTHONCOERCECLOCALE=0, and
/// is ASCII then; otherwise the codeset of the LC_CTYPE locale. Of the
/// legacy codesets only ISO-8859-1 is decoded as such; others are read as
/// UTF-8.
fn locale_codec() -> Codec {
    static CODEC: std::sync::OnceLock<Codec> = std::sync::OnceLock::new();
    *CODEC.get_or_init(|| {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let utf8_mode = env("PYTHONUTF8");
        if utf8_mode.as_deref() == Some("1") {
            return Codec::Utf8;
        }
        let name = env("LC_ALL").or_else(|| env("LC_CTYPE")).or_else(|| env("LANG"))
            .unwrap_or_else(|| "C".to_owned());
        let lower = name.to_ascii_lowercase();
        let charmap = if name == "C" || name == "POSIX" {
            "ANSI_X3.4-1968".to_owned()
        } else if lower.contains(".utf-8") || lower.contains(".utf8") {
            "UTF-8".to_owned()
        } else {
            // nl_langinfo(CODESET) after setlocale(LC_CTYPE, ""): what
            // `locale charmap` prints in this environment.
            // A `locale` that cannot start, or hangs, reads as UTF-8.
            let o = crate::util::run(&["locale", "charmap"], 5);
            if o.failed_to_start || o.timed_out {
                "UTF-8".to_owned()
            } else {
                o.stdout.trim().to_owned()
            }
        };
        match charmap.as_str() {
            "ANSI_X3.4-1968" => {
                let coerced = env("LC_ALL").is_none() && env("PYTHONCOERCECLOCALE").as_deref() != Some("0");
                if utf8_mode.as_deref() == Some("0") && !coerced { Codec::Ascii } else { Codec::Utf8 }
            }
            "ISO-8859-1" => Codec::Latin1,
            _ => Codec::Utf8,
        }
    })
}

/// text=True's decode: strict, in the locale's encoding, then universal
/// newlines.
fn py_decode(b: Vec<u8>) -> Result<String, String> {
    let text = match locale_codec() {
        Codec::Latin1 => b.iter().map(|&c| c as char).collect(),
        Codec::Ascii => match b.iter().position(|&c| c >= 0x80) {
            Some(pos) => {
                return Err(format!("UnicodeDecodeError: 'ascii' codec can't decode byte {:#04x} \
                                    in position {pos}: ordinal not in range(128)", b[pos]));
            }
            None => String::from_utf8_lossy(&b).into_owned(),
        },
        Codec::Utf8 => match String::from_utf8(b) {
            Ok(s) => s,
            Err(e) => {
                let u = e.utf8_error();
                let pos = u.valid_up_to();
                let bytes = e.as_bytes();
                let byte = bytes[pos];
                let why = match u.error_len() {
                    None => "unexpected end of data",
                    Some(_) if (0x80..=0xbf).contains(&byte) || byte >= 0xf5 || byte == 0xc0 || byte == 0xc1 => "invalid start byte",
                    Some(_) => "invalid continuation byte",
                };
                return Err(format!("UnicodeDecodeError: 'utf-8' codec can't decode byte {byte:#04x} in position {pos}: {why}"));
            }
        },
    };
    Ok(text.replace("\r\n", "\n").replace('\r', "\n"))
}

pub fn essential_for(unit: &str) -> String {
    ESSENTIAL.iter().find(|(u, _)| *u == unit).map(|(_, s)| (*s).to_owned()).unwrap_or_default()
}

/// Python's `str.isspace()`: Rust's White_Space plus the four ASCII
/// separators \x1c-\x1f, which Python also strips.
fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// Python's `str.strip()`.
fn py_strip(s: &str) -> &str {
    s.trim_matches(py_space)
}

/// Python's `str.splitlines()` (no keepends).
fn py_splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        let brk = matches!(c, '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e'
                              | '\u{85}' | '\u{2028}' | '\u{2029}');
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

/// A value from [`py_json_loads`], shaped for what `services()` asks of it.
/// Strings are UTF-16 code units so that a lone surrogate (`"\ud800"`, which
/// Python's json keeps) survives. Containers nested below the third level
/// keep only their type: nothing reads into them, and a shallow tree cannot
/// overflow the stack when it is dropped.
#[derive(Debug, Clone, PartialEq)]
enum J {
    Null,
    Bool,
    Int,
    Float,
    Str(Vec<u16>),
    Arr(Vec<J>),
    /// Keys in order, duplicates kept; lookups take the last (dict semantics).
    Obj(Vec<(Vec<u16>, J)>),
}

impl J {
    fn get(&self, key: &str) -> Option<&J> {
        let key: Vec<u16> = key.encode_utf16().collect();
        match self {
            J::Obj(o) => o.iter().rev().find(|(k, _)| *k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    fn has_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }
    fn is_str(&self, s: &str) -> bool {
        matches!(self, J::Str(v) if v.iter().copied().eq(s.encode_utf16()))
    }
    /// Python's type name, for the TypeError messages.
    fn type_name(&self) -> &'static str {
        match self {
            J::Null => "NoneType",
            J::Bool => "bool",
            J::Int => "int",
            J::Float => "float",
            J::Str(_) => "str",
            J::Arr(_) => "list",
            J::Obj(_) => "dict",
        }
    }
}

/// Containers at this depth or deeper are kept empty (see [`J`]).
const KEEP_DEPTH: usize = 3;

/// Why [`py_json_loads`] gave up: ValueError (malformed JSON, or an int
/// past the digit limit), or RecursionError (nesting too deep for the C
/// scanner's stack), which is not a ValueError.
#[derive(Debug, PartialEq)]
enum JErr {
    Value,
    Recursion(String),
}

impl From<()> for JErr {
    fn from(_: ()) -> Self {
        JErr::Value
    }
}

/// `sys.get_int_max_str_digits()`: PYTHONINTMAXSTRDIGITS (0 for no limit,
/// else at least 640; Python will not start with anything else), by default
/// 4300. An int literal with more digits raises ValueError in json.loads.
fn int_max_str_digits() -> usize {
    match std::env::var("PYTHONINTMAXSTRDIGITS").ok().and_then(|v| v.trim().parse::<usize>().ok()) {
        Some(n) if n == 0 || n >= 640 => n,
        _ => 4300,
    }
}

/// How many arrays/objects deep Python's C scanner gets before it raises
/// RecursionError. Python 3.14 measures the C stack rather than counting
/// frames: each level costs it 160 bytes, and it stops some 43 kB short of
/// the end of the thread's stack, which is RLIMIT_STACK (glibc's 2 MiB when
/// that is unlimited) -- for the switchboard's worker thread, 52 154 levels
/// under the usual 8 MiB.
fn json_max_depth() -> usize {
    let stack = rustix::process::getrlimit(rustix::process::Resource::Stack)
        .current.unwrap_or(2 << 20);
    (stack.saturating_sub(43_900) / 160) as usize
}

/// Python's `json.loads` (the C scanner, strict=True): what it accepts and
/// what it rejects (`Err(JErr::Value)` is its ValueError). Unlike serde_json
/// it takes NaN, Infinity and -Infinity, floats of any size (1e400 is inf),
/// lone surrogate escapes; ints are limited to int_max_str_digits() digits
/// and nesting to json_max_depth() levels (RecursionError). The parser
/// itself is iterative, so the limit is Python's, not our stack's.
fn py_json_loads(s: &str) -> Result<J, JErr> {
    let max_digits = int_max_str_digits();
    let max_depth = json_max_depth();
    let too_deep = |what: &str, depth: usize| JErr::Recursion(format!(
        "RecursionError: Stack overflow (used {} kB) while decoding a JSON {what} from a unicode string",
        (43_900 + depth * 160) / 1024));
    enum Frame {
        Arr(Vec<J>, bool),
        Obj(Vec<(Vec<u16>, J)>, Option<Vec<u16>>, bool),
    }
    let b = s.as_bytes();
    let mut i = 0;
    let ws = |i: &mut usize| {
        while *i < b.len() && matches!(b[*i], b' ' | b'\t' | b'\n' | b'\r') {
            *i += 1;
        }
    };
    let mut stack: Vec<Frame> = Vec::new();
    ws(&mut i);
    loop {
        // Parse one value at i (or open a container).
        let keep = stack.len() < KEEP_DEPTH;
        let mut value: Option<J> = None;
        match b.get(i) {
            Some(b'{') => {
                if stack.len() + 1 > max_depth {
                    return Err(too_deep("object", stack.len() + 1));
                }
                i += 1;
                ws(&mut i);
                if b.get(i) == Some(&b'}') {
                    i += 1;
                    value = Some(J::Obj(vec![]));
                } else {
                    let key = obj_key(s, &mut i)?;
                    ws(&mut i);
                    stack.push(Frame::Obj(vec![], Some(key), keep));
                    continue;
                }
            }
            Some(b'[') => {
                if stack.len() + 1 > max_depth {
                    return Err(too_deep("array", stack.len() + 1));
                }
                i += 1;
                ws(&mut i);
                if b.get(i) == Some(&b']') {
                    i += 1;
                    value = Some(J::Arr(vec![]));
                } else {
                    stack.push(Frame::Arr(vec![], keep));
                    continue;
                }
            }
            Some(b'"') => value = Some(J::Str(py_json_string(s, &mut i)?)),
            _ => {
                let rest = &s[i..];
                for (word, v) in [("null", J::Null), ("true", J::Bool), ("false", J::Bool),
                                  ("NaN", J::Float), ("Infinity", J::Float), ("-Infinity", J::Float)] {
                    if rest.starts_with(word) {
                        i += word.len();
                        value = Some(v);
                        break;
                    }
                }
                if value.is_none() {
                    value = Some(py_json_number(b, &mut i, max_digits)?);
                }
            }
        }
        // Hand the finished value up, closing containers as they end.
        let mut v = value.unwrap_or(J::Null);
        loop {
            ws(&mut i);
            let Some(top) = stack.last_mut() else {
                // "Extra data" unless only whitespace is left.
                return if i == b.len() { Ok(v) } else { Err(JErr::Value) };
            };
            match top {
                Frame::Arr(items, keep) => {
                    if *keep {
                        items.push(v);
                    }
                    match b.get(i) {
                        Some(b',') => {
                            i += 1;
                            ws(&mut i);
                            break;
                        }
                        Some(b']') => {
                            i += 1;
                            let Some(Frame::Arr(items, _)) = stack.pop() else { return Err(JErr::Value) };
                            v = J::Arr(items);
                        }
                        _ => return Err(JErr::Value),
                    }
                }
                Frame::Obj(items, key, keep) => {
                    let k = key.take().unwrap_or_default();
                    if *keep {
                        items.push((k, v));
                    }
                    match b.get(i) {
                        Some(b',') => {
                            i += 1;
                            ws(&mut i);
                            let k = obj_key(s, &mut i)?;
                            *key = Some(k);
                            ws(&mut i);
                            break;
                        }
                        Some(b'}') => {
                            i += 1;
                            let Some(Frame::Obj(items, _, _)) = stack.pop() else { return Err(JErr::Value) };
                            v = J::Obj(items);
                        }
                        _ => return Err(JErr::Value),
                    }
                }
            }
        }
    }
}

/// `"key"` then whitespace and `:`, leaving i after the colon.
fn obj_key(s: &str, i: &mut usize) -> Result<Vec<u16>, ()> {
    let b = s.as_bytes();
    if b.get(*i) != Some(&b'"') {
        return Err(());
    }
    let k = py_json_string(s, i)?;
    while *i < b.len() && matches!(b[*i], b' ' | b'\t' | b'\n' | b'\r') {
        *i += 1;
    }
    if b.get(*i) != Some(&b':') {
        return Err(());
    }
    *i += 1;
    while *i < b.len() && matches!(b[*i], b' ' | b'\t' | b'\n' | b'\r') {
        *i += 1;
    }
    Ok(k)
}

/// A JSON string starting at the opening quote, as Python's scanstring:
/// control characters are errors, a \\uD8xx\\uDCxx pair joins, a lone
/// surrogate stays.
fn py_json_string(s: &str, i: &mut usize) -> Result<Vec<u16>, ()> {
    let b = s.as_bytes();
    let hex4 = |at: usize| -> Result<u16, ()> {
        let h = s.get(at..at + 4).ok_or(())?;
        if !h.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(());
        }
        u16::from_str_radix(h, 16).map_err(|_| ())
    };
    let mut out = Vec::new();
    *i += 1;
    loop {
        let c = s[*i..].chars().next().ok_or(())?;
        match c {
            '"' => {
                *i += 1;
                return Ok(out);
            }
            '\\' => {
                let e = *b.get(*i + 1).ok_or(())?;
                *i += 2;
                let unit = match e {
                    b'"' => b'"' as u16,
                    b'\\' => b'\\' as u16,
                    b'/' => b'/' as u16,
                    b'b' => 8,
                    b'f' => 12,
                    b'n' => 10,
                    b'r' => 13,
                    b't' => 9,
                    b'u' => {
                        let u = hex4(*i)?;
                        *i += 4;
                        if (0xd800..=0xdbff).contains(&u) && b.get(*i) == Some(&b'\\') && b.get(*i + 1) == Some(&b'u') {
                            let lo = hex4(*i + 2)?;
                            if (0xdc00..=0xdfff).contains(&lo) {
                                *i += 6;
                                out.push(u);
                                out.push(lo);
                                continue;
                            }
                        }
                        u
                    }
                    _ => return Err(()),
                };
                out.push(unit);
            }
            c if (c as u32) < 0x20 => return Err(()),
            c => {
                let mut buf = [0u16; 2];
                out.extend_from_slice(c.encode_utf16(&mut buf));
                *i += c.len_utf8();
            }
        }
    }
}

/// `-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][-+]?[0-9]+)?`, as the C scanner
/// matches it (a dangling `.` or exponent is left for the caller to reject).
/// An int with more than `max_digits` digits (0: no limit) is a ValueError,
/// as int() raises it.
fn py_json_number(b: &[u8], i: &mut usize, max_digits: usize) -> Result<J, ()> {
    let digits = |i: &mut usize| {
        let s = *i;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
        }
        *i > s
    };
    let mut j = *i;
    if b.get(j) == Some(&b'-') {
        j += 1;
    }
    let first = j;
    match b.get(j) {
        Some(b'0') => j += 1,
        Some(b'1'..=b'9') => {
            digits(&mut j);
        }
        _ => return Err(()),
    }
    let mut float = false;
    if b.get(j) == Some(&b'.') && b.get(j + 1).is_some_and(u8::is_ascii_digit) {
        j += 1;
        digits(&mut j);
        float = true;
    }
    if matches!(b.get(j), Some(b'e' | b'E')) {
        let mut k = j + 1;
        if matches!(b.get(k), Some(b'+' | b'-')) {
            k += 1;
        }
        if digits(&mut k) {
            j = k;
            float = true;
        }
    }
    if !float && max_digits > 0 {
        let int_end = (first..j).find(|&k| !b[k].is_ascii_digit()).unwrap_or(j);
        if int_end - first > max_digits {
            return Err(());
        }
    }
    *i = j;
    Ok(if float { J::Float } else { J::Int })
}

/// `services()` as the Python has it, raising (`Err`) where it raises:
/// list-unit-files JSON that is not iterable (null, a number, true) or
/// nested too deep (RecursionError), a unit entry without a string
/// unit_file, a name subprocess will not take (a surrogate it cannot encode,
/// a NUL), output that does not decode.
pub fn try_services() -> Result<Vec<Service>, String> {
    if crate::demo::active() {
        return Ok(crate::demo::user_services());
    }
    services_with(available(), &|args: &[Vec<u16>]| run(args, 20))
}

/// A `&str` as the UTF-16 a Python string is held in here.
fn u16s(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// The body of `services()` with `available()` and `_run` passed in, so the
/// tests can stand in for systemctl the way the Python tests mock them.
/// `_run` gets its arguments as Python strings (UTF-16, lone surrogates and
/// all); encoding them for the command line is its business, as in Python.
fn services_with(available: bool, run: &dyn Fn(&[Vec<u16>]) -> Result<String, String>)
    -> Result<Vec<Service>, String>
{
    use std::collections::HashMap;
    if !available {
        return Ok(vec![]);
    }
    let raw = run(&["list-unit-files", "--type=service,timer",
                    "--state=enabled,disabled", "--no-legend", "--output=json"].map(u16s))?;
    // json.loads(raw) if raw.strip() else []; ValueError -> [] (RecursionError
    // is not one, and raises). Iterating a dict (its keys) or a string (its
    // characters) yields no dicts, so those come out empty as well; null, a
    // number or a bool cannot be iterated.
    let parsed = if py_strip(&raw).is_empty() {
        J::Arr(vec![])
    } else {
        match py_json_loads(&raw) {
            Ok(j) => j,
            Err(JErr::Value) => J::Arr(vec![]),
            Err(JErr::Recursion(e)) => return Err(e),
        }
    };
    let units: Vec<J> = match parsed {
        J::Arr(a) => a,
        J::Obj(_) | J::Str(_) => vec![],
        other => return Err(format!("TypeError: '{}' object is not iterable", other.type_name())),
    };
    // names = [u["unit_file"] for u in units
    //          if isinstance(u, dict) and "@" not in u.get("unit_file", "")]
    let mut names: Vec<&J> = Vec::new();
    for u in units.iter().filter(|u| matches!(u, J::Obj(_))) {
        let has_at = match u.get("unit_file") {
            None => false,
            Some(J::Str(s)) => s.contains(&(b'@' as u16)),
            Some(J::Arr(items)) => items.iter().any(|x| x.is_str("@")),
            Some(d @ J::Obj(_)) => d.has_key("@"),
            Some(other) => return Err(format!("TypeError: argument of type '{}' is not iterable", other.type_name())),
        };
        if !has_at {
            names.push(u.get("unit_file").ok_or_else(|| "KeyError: 'unit_file'".to_owned())?);
        }
    }
    if names.is_empty() {
        return Ok(vec![]);
    }
    // states = {u["unit_file"]: u.get("state") for u in units if isinstance(u, dict)}
    // (every dict has a unit_file by now; a list or dict one is unhashable)
    let mut states: HashMap<&[u16], bool> = HashMap::new();
    for u in units.iter().filter(|u| matches!(u, J::Obj(_))) {
        match u.get("unit_file") {
            Some(J::Str(n)) => {
                states.insert(n, u.get("state").is_some_and(|s| s.is_str("enabled")));
            }
            Some(other) => return Err(format!("TypeError: unhashable type: '{}'", other.type_name())),
            None => return Err("KeyError: 'unit_file'".to_owned()),
        }
    }
    // Every name is a string now, kept as Python has it: one with a lone
    // surrogate goes to `show` as is (subprocess's os.fsencode passes
    // \udc80-\udcff through as raw bytes and raises on the rest).
    let mut names16: Vec<&[u16]> = Vec::new();
    for n in &names {
        let J::Str(units16) = n else { return Err("TypeError: name is not a string".to_owned()) };
        names16.push(units16);
    }
    let mut argv: Vec<Vec<u16>> = vec![u16s("show"), u16s("--property=Id,Description,ActiveState")];
    argv.extend(names16.iter().map(|n| n.to_vec()));
    let shown = run(&argv)?;
    let mut info: HashMap<String, HashMap<String, String>> = HashMap::new();
    for block in py_strip(&shown).split("\n\n") {
        let d: HashMap<String, String> = py_splitlines(block).into_iter()
            .filter_map(|line| line.split_once('='))
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        if let Some(id) = d.get("Id").filter(|id| !id.is_empty()).cloned() {
            info.insert(id, d);
        }
    }
    let empty = HashMap::new();
    // (sort key: the name's code points, as Python compares strings)
    let mut out: Vec<(Vec<u32>, Service)> = Vec::new();
    for key in &names16 {
        // Shown text is decoded, so it never holds a lone surrogate: a name
        // with one has no info.
        let name = lossy(key);
        let d = if String::from_utf16(key).is_ok() { info.get(&name).unwrap_or(&empty) } else { &empty };
        let enabled = states.get(*key).copied().unwrap_or(false);
        let active = d.get("ActiveState").map(String::as_str) == Some("active");
        // Disabled yet running: something else starts it on demand (a socket,
        // D-Bus, another unit), so a switch would show "off" for a service
        // that is plainly on, and flipping it would not mean what it says.
        if active && !enabled {
            continue;
        }
        let description = d.get("Description").filter(|s| !s.is_empty())
            .cloned().unwrap_or_else(|| name.clone());
        let points: Vec<u32> = char::decode_utf16(key.iter().copied())
            .map(|c| c.map(u32::from).unwrap_or_else(|e| u32::from(e.unpaired_surrogate())))
            .collect();
        out.push((points, Service {
            essential: essential_for(&name),
            omarchy: name.starts_with("omarchy"),
            unit: name,
            description,
            enabled,
            active,
        }));
    }
    out.sort_by(|(ka, a), (kb, b)| (!a.essential.is_empty(), ka).cmp(&(!b.essential.is_empty(), kb)));
    Ok(out.into_iter().map(|(_, s)| s).collect())
}

/// Enable and start, or disable and stop: the switch means both.
pub fn toggle_argv(unit: &str, on: bool) -> Vec<String> {
    vec!["systemctl".into(), "--user".into(),
         if on { "enable" } else { "disable" }.into(), "--now".into(), unit.into()]
}

/// `is_enabled()` as the Python has it: UnicodeDecodeError and
/// ValueError("embedded null byte") reach the caller.
pub fn try_is_enabled(unit: &str) -> Result<bool, String> {
    Ok(py_strip(&run(&[u16s("is-enabled"), u16s(unit)], 20)?) == "enabled")
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_essential_is_by_exact_name() {
        assert!(!essential_for("wireplumber.service").is_empty());
        assert_eq!(essential_for("hyprsunset.service"), "");
        assert_eq!(essential_for("xdg-desktop-portal-rewrite-launchers.service"), "");
    }

    #[test]
    fn test_on_demand_units_are_not_offered() {
        let files = serde_json::json!([
            {"unit_file": "mine.service", "state": "enabled"},
            {"unit_file": "socketed.service", "state": "disabled"}]).to_string();
        let shown = "Id=mine.service\nDescription=Mine\nActiveState=active\n\n\
                     Id=socketed.service\nDescription=S\nActiveState=active\n";
        let fake = |a: &[Vec<u16>]| -> Result<String, String> {
            Ok(if a[0] == u16s("list-unit-files") { files.clone() } else { shown.to_owned() })
        };
        let names: Vec<String> = services_with(true, &fake).unwrap().into_iter().map(|s| s.unit).collect();
        assert_eq!(names, vec!["mine.service"]);
    }

    #[test]
    fn test_the_switch_means_now_and_at_login() {
        assert_eq!(toggle_argv("x.service", false),
                   vec!["systemctl", "--user", "disable", "--now", "x.service"]);
    }

    #[test]
    fn json_loads_takes_what_python_takes() {
        assert!(py_json_loads("[NaN, -Infinity, 1e400, 01]").is_err());
        assert!(py_json_loads("[NaN, -Infinity, 1e400, 0]").is_ok());
        let deep = |n: usize| format!("{}{}", "[".repeat(n), "]".repeat(n));
        assert!(py_json_loads(&deep(json_max_depth())).is_ok());
        assert!(matches!(py_json_loads(&deep(json_max_depth() + 1)), Err(JErr::Recursion(_))));
        assert!(py_json_loads(&"1".repeat(4300)).is_ok());
        assert_eq!(py_json_loads(&"1".repeat(4301)), Err(JErr::Value));
        assert!(py_json_loads(&format!("{}.5", "1".repeat(5000))).is_ok());
        assert_eq!(py_json_loads(r#""\ud800""#), Ok(J::Str(vec![0xd800])));
        assert_eq!(py_json_loads(r#"{"a": 1, "a": "x"}"#).unwrap().get("a"), Some(&J::Str(vec![b'x' as u16])));
        assert!(py_json_loads("[1,]").is_err());
        assert!(py_json_loads("\u{feff}[]").is_err());
    }

    #[test]
    fn fsencode_is_surrogateescape() {
        assert_eq!(fsencode(&[0xdcff, b'a' as u16]), Ok(vec![0xff, b'a']));
        assert!(fsencode(&[0xd800]).unwrap_err().contains("character '\\ud800' in position 0"));
        assert!(fsencode(&[0xdcff, 0xdc7f]).unwrap_err().contains("character '\\udc7f' in position 1"));
        assert!(fsencode(&[0xd800, 0xd801]).unwrap_err().contains("characters in position 0-1"));
    }

    #[test]
    fn nul_in_argv_raises() {
        assert_eq!(try_is_enabled("a\0b.service"), Err("ValueError: embedded null byte".to_owned()));
    }
}
