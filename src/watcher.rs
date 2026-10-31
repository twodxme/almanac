//! Port of `watcher.py`.
//!
//! `almanac --guard`: notice settings resets while the window is closed.
//!
//! The window watches for a config file being reset to its defaults, but only
//! while it is open -- and the reset that prompted this happened with it
//! closed. This is the same check as a small background process: no window, no
//! GTK, a few directory monitors on a GLib main loop, idle until a tool writes a
//! `<file>.bak.<epoch>`.
//!
//! It starts with the session through XDG autostart, the one mechanism every
//! desktop honours (systemd-based sessions run it through their autostart
//! generator; others read the directory directly). Turning it on and off is a
//! switch in Preferences, which writes or removes the autostart entry.
//!
//! A reset is announced once. The backups already reported are remembered in
//! the XDG state directory, so a restart does not repeat old news.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use num_bigint::BigInt;

pub const ENTRY_NAME: &str = "io.github.twodxme.Almanac.Guard.desktop";

pub fn autostart_path() -> PathBuf {
    crate::xdg::config_home().join("autostart").join(ENTRY_NAME)
}

fn reported_path() -> PathBuf {
    crate::xdg::state_home()
        .join("almanac")
        .join("guard-reported.json")
}

/// The command Almanac is installed as.
pub fn command_name() -> &'static str {
    "almanac"
}

/// How to start Almanac from an autostart entry, however it is installed.
///
/// `almanac` from PATH when it is installed, else the binary running this.
///
/// The paths are kept as bytes: Python's `shutil.which` gives a
/// surrogateescape'd str, which an argv carries through unchanged and which
/// `enable` fails to write.
pub fn launcher() -> Vec<OsString> {
    if let Some(found) = which(command_name()) {
        return vec![found.into_os_string()];
    }
    match std::env::current_exe() {
        Ok(exe) => vec![exe.into_os_string()],
        Err(_) => vec![OsString::from("almanac")],
    }
}

/// glibc's `confstr(_CS_PATH)`: what `shutil.which` searches when PATH is
/// not set at all.
const CS_PATH: &str = "/bin:/usr/bin";

/// `shutil.which(cmd)` for a bare command name, exactly as Python does it
/// on Linux: PATH (or CS_PATH when unset; nothing when empty), split on
/// `:`, an empty entry meaning the current directory, and the first
/// `dir/cmd` that exists, that *this user* may execute (`os.access(X_OK)`)
/// and that is not a directory. (`util::which` asks for a regular file with
/// any x bit, which is not the same.)
fn which(cmd: &str) -> Option<PathBuf> {
    which_in(cmd, std::env::var_os("PATH"))
}

/// `which` along `path`, the value PATH would have (None: unset).
fn which_in(cmd: &str, path: Option<OsString>) -> Option<PathBuf> {
    let path = path.unwrap_or_else(|| OsString::from(CS_PATH));
    if path.is_empty() {
        return None; // PATH='' doesn't match, whereas PATH=':' looks in the current directory
    }
    for dir in path.as_bytes().split(|&b| b == b':') {
        // os.path.join(dir, cmd)
        let mut name = dir.to_vec();
        if !name.is_empty() && !name.ends_with(b"/") {
            name.push(b'/');
        }
        name.extend_from_slice(cmd.as_bytes());
        let name = PathBuf::from(OsString::from_vec(name));
        if access_check(&name) {
            return Some(name);
        }
    }
    None
}

/// `shutil._access_check(fn, os.F_OK | os.X_OK)`.
fn access_check(p: &Path) -> bool {
    p.exists() && rustix::fs::access(p, rustix::fs::Access::EXEC_OK).is_ok() && !p.is_dir()
}

pub fn enabled() -> bool {
    autostart_path().is_file()
}

/// `subprocess.Popen(argv, start_new_session=True, ...)`: the child calls
/// setsid() before exec, so it leaves our session and controlling terminal
/// (no SIGHUP when the terminal closes) and outlives us. stdin is inherited,
/// as Popen does when not told otherwise; `quiet` sends stdout and stderr to
/// /dev/null.
fn popen_detached(argv: &[OsString], quiet: bool) -> Result<(), String> {
    use gtk::gio;
    if argv.is_empty() {
        return Err("IndexError: list index out of range".to_string());
    }
    let mut flags = gio::SubprocessFlags::STDIN_INHERIT;
    if quiet {
        flags |= gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_SILENCE;
    }
    let launcher = gio::SubprocessLauncher::new(flags);
    launcher.set_child_setup(|| {
        let _ = rustix::process::setsid();
    });
    let args: Vec<&OsStr> = argv.iter().map(OsString::as_os_str).collect();
    launcher
        .spawn(&args)
        .map(|_| ())
        .map_err(|e| format!("OSError: {e}"))
}

/// Writes the autostart entry and, with `start_now`, starts a guard unless
/// one is running. A failed write is an `Err` (the Python raises OSError,
/// which the Preferences switch reports), and so is an unreadable pid (the
/// OverflowError `running` raises).
///
/// Python default: `start_now=True`.
pub fn enable(start_now: bool) -> Result<(), String> {
    enable_with(start_now, &launcher())
}

/// `enable`, with the command that starts Almanac given.
fn enable_with(start_now: bool, launcher: &[OsString]) -> Result<(), String> {
    let path = autostart_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("OSError: {e}"))?;
    }
    let mut argv = launcher.to_vec();
    argv.push("--guard".into());
    let mut exec_line = PyStr::new();
    for (i, a) in argv.iter().enumerate() {
        if i > 0 {
            exec_line.push(u32::from(' '));
        }
        exec_line.extend(quote_py(&pystr_of_path(Path::new(a))));
    }
    let mut text = pystr(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Almanac settings guard\n\
         Comment=Notices when a config file is reset to its defaults\n\
         Exec=",
    );
    text.extend(exec_line);
    text.extend(pystr(
        "\n\
         Icon=io.github.twodxme.Almanac\n\
         NoDisplay=true\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n",
    ));
    match pystr_to_string(&text) {
        Some(text) => {
            crate::util::atomic_write(&path, text).map_err(|e| format!("OSError: {e}"))?
        }
        None => {
            // write_text opens (and so truncates) the file, then fails to
            // encode a surrogate: the entry is left empty.
            std::fs::File::create(&path).map_err(|e| format!("OSError: {e}"))?;
            return Err(utf8_encode_error(&text));
        }
    }
    if start_now && running()? == 0 {
        let mut argv = launcher.to_vec();
        argv.push("--guard".into());
        popen_detached(&argv, true)?;
    }
    Ok(())
}

/// The UnicodeEncodeError of `str.encode("utf-8")` on a str holding lone
/// surrogates: the first run of them is reported.
fn utf8_encode_error(s: &PyStr) -> String {
    let surrogate = |c: &u32| (0xD800..=0xDFFF).contains(c);
    let start = s.iter().position(surrogate).unwrap_or(0);
    let end = start + s[start..].iter().take_while(|c| surrogate(c)).count();
    let what = if end - start == 1 {
        format!("character '\\u{:04x}' in position {start}", s[start])
    } else {
        format!("characters in position {start}-{}", end - 1)
    };
    format!("UnicodeEncodeError: 'utf-8' codec can't encode {what}: surrogates not allowed")
}

/// Removes the autostart entry and stops a running guard.
pub fn disable() -> Result<(), String> {
    match std::fs::remove_file(autostart_path()) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {} // missing_ok
        Err(e) => return Err(format!("OSError: {e}")),
    }
    let pid = running()?;
    if pid != 0 {
        if let Some(p) = rustix::process::Pid::from_raw(pid) {
            let _ = rustix::process::kill_process(p, rustix::process::Signal::TERM);
        }
    }
    Ok(())
}

/// Desktop Entry Exec quoting: reserved characters need double quotes.
/// (`_quote` in the Python.)
pub fn quote(arg: &str) -> String {
    // A str of chars has no surrogates, so the result is one too.
    quote_py(&pystr(arg))
        .into_iter()
        .filter_map(char::from_u32)
        .collect()
}

/// `_quote` on a Python str, lone surrogates included.
fn quote_py(arg: &PyStr) -> PyStr {
    const RESERVED: &str = " \t\n\"'\\><~|&;$*?#()`";
    let reserved = |c: u32| char::from_u32(c).is_some_and(|c| RESERVED.contains(c));
    if !arg.iter().any(|&c| reserved(c)) {
        return arg.clone();
    }
    let mut out = vec![u32::from('"')];
    for &c in arg {
        if matches!(char::from_u32(c), Some('\\' | '"' | '`' | '$')) {
            out.push(u32::from('\\'));
        }
        out.push(c);
    }
    out.push(u32::from('"'));
    out
}

fn pid_path() -> PathBuf {
    // `os.environ.get(...) or ...`: any non-empty value, UTF-8 or not.
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| crate::xdg::state_home().join("almanac").into_os_string());
    py_path(&runtime).join("almanac-guard.pid")
}

/// `Path(s)`: repeated and trailing slashes and `.` parts dropped, exactly
/// two leading slashes kept, "" for "." (so that joining onto it gives a
/// relative name, as `Path(".") / "x"` is `x`).
fn py_path(s: &OsStr) -> PathBuf {
    let b = s.as_bytes();
    let root: &[u8] = if b.starts_with(b"//") && !b.starts_with(b"///") {
        b"//"
    } else if b.starts_with(b"/") {
        b"/"
    } else {
        b""
    };
    let parts: Vec<&[u8]> = b
        .split(|&c| c == b'/')
        .filter(|p| !p.is_empty() && *p != b".")
        .collect();
    let mut out = root.to_vec();
    out.extend(parts.join(&b'/'));
    PathBuf::from(OsString::from_vec(out))
}

// ---- Python's str and int, as running() and the state files need them ----

/// `str.isspace()` for one character (Unicode 16, as Python 3.14 has it):
/// Rust's `char::is_whitespace` leaves out U+001C..U+001F.
fn py_isspace(c: char) -> bool {
    matches!(
        c,
        '\t'..='\r'
            | '\u{1c}'..='\u{20}'
            | '\u{85}'
            | '\u{a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
    )
}

/// `str.strip()` with no argument.
fn py_strip(s: &str) -> &str {
    s.trim_matches(py_isspace)
}

/// `sys.get_int_max_str_digits()`: longer decimal strings are a ValueError.
const INT_MAX_STR_DIGITS: usize = 4300;

/// Python's `int(text)` (base 10): surrounding `isspace()` characters, an
/// optional ASCII sign, then decimal digits of any script with single
/// underscores between them. `None` is the ValueError.
fn py_int(text: &str) -> Option<BigInt> {
    let t = py_strip(text);
    let (neg, body) = match t.chars().next() {
        Some('-') => (true, &t[1..]),
        Some('+') => (false, &t[1..]),
        _ => (false, t),
    };
    let mut digits = String::new();
    let mut prev_underscore = true; // no leading underscore
    for c in body.chars() {
        if c == '_' {
            if prev_underscore {
                return None;
            }
            prev_underscore = true;
        } else {
            digits.push(char::from_digit(crate::util::decimal_value(c)?, 10)?);
            prev_underscore = false;
        }
    }
    if digits.is_empty() || prev_underscore || digits.len() > INT_MAX_STR_DIGITS {
        return None;
    }
    let n: BigInt = digits.parse().ok()?;
    Some(if neg { -n } else { n })
}

/// PID of a guard that is running, or 0.
///
/// A pid that does not fit a C int is the OverflowError of `os.kill`, which
/// the Python does not catch: an `Err`.
pub fn running() -> Result<i32, String> {
    running_in(&pid_path(), &guard_alive)
}

/// Whether `pid` is a live `--guard`.
fn guard_alive(pid: i32) -> bool {
    match std::fs::read(format!("/proc/{pid}/cmdline")) {
        Ok(cmdline) => cmdline.windows(7).any(|w| w == b"--guard"),
        Err(_) => false,
    }
}

/// [`running`] with the pid file and the liveness check passed in.
fn running_in(path: &Path, alive: &dyn Fn(i32) -> bool) -> Result<i32, String> {
    // read_text(): an unreadable or non-UTF-8 file is OSError/ValueError.
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(0);
    };
    let Some(pid) = py_int(py_strip(&text)) else {
        return Ok(0);
    };
    let Ok(pid) = i32::try_from(&pid) else {
        return Err("OverflowError: Python int too large to convert to C int".to_string());
    };
    // os.kill(0 or a negative pid) signals a group; there is no
    // /proc/<pid> for those, so the Python ends at 0 just the same.
    let Some(p) = rustix::process::Pid::from_raw(pid.max(0)) else {
        return Ok(0);
    };
    if pid <= 0 || rustix::process::test_kill_process(p).is_err() {
        return Ok(0);
    }
    Ok(if alive(pid) { pid } else { 0 })
}

/// Become the session's one guard: Ok(true) when no other is running, and
/// this one's pid is now in the pid file. The check and the write are one
/// step under a lock beside the pid file: two guards started together
/// (autostart and a login script) both read "none running" otherwise, and
/// both ran.
fn claim(path: &Path, me: i32, alive: &dyn Fn(i32) -> bool) -> Result<bool, String> {
    let mut lock_name = path.as_os_str().to_owned();
    lock_name.push(".lock");
    // Without the lock (a read-only runtime dir), as before: one guard
    // still wins nearly always.
    let _lock = crate::util::lock_file(Path::new(&lock_name)).ok();
    if running_in(path, alive)? != 0 {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_ok() {
            let _ = std::fs::write(path, me.to_string());
        }
    }
    Ok(true)
}

/// A Python `str`, as code points: it may hold lone surrogates (from a
/// `\udXXX` JSON escape, or a non-UTF-8 path decoded with surrogateescape),
/// which a Rust `String` cannot. Ordering a `Vec<u32>` is Python's str order.
type PyStr = Vec<u32>;

fn pystr(s: &str) -> PyStr {
    s.chars().map(u32::from).collect()
}

/// `str(path)`: the bytes decoded as UTF-8 with surrogateescape.
fn pystr_of_path(p: &Path) -> PyStr {
    let mut out = Vec::new();
    for chunk in p.as_os_str().as_bytes().utf8_chunks() {
        out.extend(chunk.valid().chars().map(u32::from));
        out.extend(chunk.invalid().iter().map(|&b| 0xDC00 + u32::from(b)));
    }
    out
}

/// The str as a Rust String; `None` when it holds a surrogate.
fn pystr_to_string(s: &PyStr) -> Option<String> {
    s.iter().map(|&c| char::from_u32(c)).collect()
}

/// `json.dumps(s)` (ensure_ascii): as `util::py_json_string`, surrogates too.
fn pystr_json(s: &PyStr) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for &c in s {
        match c {
            0x22 => out.push_str("\\\""),
            0x5C => out.push_str("\\\\"),
            0x0A => out.push_str("\\n"),
            0x0D => out.push_str("\\r"),
            0x09 => out.push_str("\\t"),
            0x08 => out.push_str("\\b"),
            0x0C => out.push_str("\\f"),
            0x20..=0x7E => out.push(char::from(c as u8)),
            0x10000.. => {
                let v = c - 0x10000;
                out.push_str(&format!(
                    "\\u{:04x}\\u{:04x}",
                    0xD800 + (v >> 10),
                    0xDC00 + (v & 0x3FF)
                ));
            }
            _ => out.push_str(&format!("\\u{c:04x}")),
        }
    }
    out.push('"');
    out
}

/// A JSON number (or true/false) as Python holds it in a set.
#[derive(Clone, Debug, PartialEq)]
enum Num {
    Bool(bool),
    Int(BigInt),
    Float(f64),
}

impl Num {
    fn int(&self) -> Option<BigInt> {
        match self {
            Num::Bool(b) => Some(BigInt::from(u8::from(*b))),
            Num::Int(i) => Some(i.clone()),
            Num::Float(_) => None,
        }
    }

    /// `type(x).__name__`.
    fn type_name(&self) -> &'static str {
        match self {
            Num::Bool(_) => "bool",
            Num::Int(_) => "int",
            Num::Float(_) => "float",
        }
    }

    fn is_nan(&self) -> bool {
        matches!(self, Num::Float(f) if f.is_nan())
    }

    fn json(&self) -> String {
        match self {
            Num::Bool(b) => b.to_string(),
            Num::Int(i) => i.to_string(),
            Num::Float(f) => crate::util::py_float_repr(*f), // NaN, Infinity included
        }
    }
}

/// Python's comparison of two numbers (bool, int, float compare exactly with
/// each other); `None` when unordered (NaN).
fn num_cmp(a: &Num, b: &Num) -> Option<Ordering> {
    fn int_float(i: &BigInt, f: f64) -> Option<Ordering> {
        if f.is_nan() {
            return None;
        }
        if f.is_infinite() {
            return Some(if f > 0.0 {
                Ordering::Less
            } else {
                Ordering::Greater
            });
        }
        let fl = f.floor();
        let fb: BigInt = format!("{fl:.0}").parse().ok()?;
        Some(match i.cmp(&fb) {
            Ordering::Equal if f > fl => Ordering::Less,
            o => o,
        })
    }
    match (a.int(), b.int(), a, b) {
        (Some(x), Some(y), _, _) => Some(x.cmp(&y)),
        (Some(x), None, _, Num::Float(f)) => int_float(&x, *f),
        (None, Some(y), Num::Float(f), _) => int_float(&y, *f).map(Ordering::reverse),
        (_, _, Num::Float(x), Num::Float(y)) => x.partial_cmp(y),
        _ => None,
    }
}

/// A Python `set` of what `json.loads` can give that is hashable: strs,
/// numbers (bool/int/float, equal across types, the first one kept, as a set
/// does), and None. The remembered-reports file is read into one, and is
/// written back with `json.dumps(sorted(seen))`, which fails when the set
/// mixes kinds that do not compare.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Seen {
    strs: BTreeSet<PyStr>,
    nums: Vec<Num>,
    none: bool,
}

impl Seen {
    pub fn len(&self) -> usize {
        self.strs.len() + self.nums.len() + usize::from(self.none)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn add(&mut self, item: Item) {
        match item {
            Item::Str(s) => {
                self.strs.insert(s);
            }
            Item::None => self.none = true,
            Item::Num(n) => {
                // NaN equals nothing, but json's decoder gives every `NaN`
                // literal as one shared float object (json.decoder.NaN), and
                // a set finds a member by identity first: one NaN at most.
                let dup =
                    |m: &Num| num_cmp(m, &n) == Some(Ordering::Equal) || (m.is_nan() && n.is_nan());
                if !self.nums.iter().any(dup) {
                    self.nums.push(n);
                }
            }
        }
    }

    /// `sorted(seen)`, or the TypeError of comparing a str, a number and
    /// None with one another.
    ///
    /// Which two members the sort happens to compare first follows the set's
    /// iteration order, which for strs is hash-seeded per process: the
    /// message names the kinds present (a number by the type of the first one
    /// added) in a fixed order. Where NaN lands is likewise up to the set's
    /// order, which for NaN hangs on its address (here it goes last).
    fn sorted(&self) -> Result<Vec<Item>, String> {
        let kinds = [!self.strs.is_empty(), !self.nums.is_empty(), self.none];
        if kinds.iter().filter(|&&k| k).count() > 1 {
            let num = self.nums.first().map_or("int", Num::type_name);
            let name = |i: usize| ["str", num, "NoneType"][i];
            let mut present = (0..3).filter(|&i| kinds[i]);
            let (a, b) = (present.next().unwrap_or(0), present.next().unwrap_or(0));
            return Err(format!(
                "TypeError: '<' not supported between instances of '{}' and '{}'",
                name(b),
                name(a)
            ));
        }
        let mut out: Vec<Item> = self.strs.iter().cloned().map(Item::Str).collect();
        let (mut ordered, nans): (Vec<Num>, Vec<Num>) =
            self.nums.iter().cloned().partition(|n| !n.is_nan());
        ordered.sort_by(|a, b| num_cmp(a, b).unwrap_or(Ordering::Equal));
        out.extend(ordered.into_iter().chain(nans).map(Item::Num));
        if self.none {
            out.push(Item::None);
        }
        Ok(out)
    }

    /// `json.dumps(sorted(seen))`.
    fn dumps(&self) -> Result<String, String> {
        let items: Vec<String> = self
            .sorted()?
            .iter()
            .map(|i| match i {
                Item::Str(s) => pystr_json(s),
                Item::Num(n) => n.json(),
                Item::None => "null".to_string(),
            })
            .collect();
        Ok(format!("[{}]", items.join(", ")))
    }
}

/// One hashable value from `json.loads`.
#[derive(Clone, Debug, PartialEq)]
enum Item {
    Str(PyStr),
    Num(Num),
    None,
}

/// How `json.loads` fails: a ValueError (JSONDecodeError, or too many digits
/// for an int), or a RecursionError, which the Python's callers do not catch.
#[derive(Debug, PartialEq)]
enum JsonErr {
    Value,
    Recursion,
}

/// The top level of a document, as `set()` sees it: a list's members (None
/// for a list or object, unhashable), an object's keys, or a lone value.
enum Top {
    List(Vec<Option<Item>>),
    Dict(Vec<PyStr>),
    Scalar(Item),
}

/// How many arrays/objects deep Python 3.14's json decoder gets before its
/// RecursionError: it is bounded by the C stack left, so this is the model
/// guard.rs uses (`PyStack::json_depth`). A worker thread starts on a fresh
/// stack of RLIMIT_STACK bytes (160 bytes a level, 43920 before the first:
/// 52154 levels under 8 MB); the main thread's stack starts below the
/// environment, where ASLR moves it by a few KB, so Python varies there
/// from run to run (52101..52139 levels) and this takes the middle.
fn json_max_depth(thread: bool) -> usize {
    // glibc: a thread's default stack is RLIMIT_STACK (page-aligned), or
    // 2 MB when that is unlimited.
    let size = match rustix::process::getrlimit(rustix::process::Resource::Stack).current {
        Some(n) => (n.max(16384) as usize).div_ceil(4096) * 4096,
        None => 2 * 1024 * 1024,
    };
    let depth = size.saturating_sub(43920) / 160;
    if thread {
        depth
    } else {
        depth.saturating_sub(35)
    }
}

/// `json.loads(text)` as CPython's scanner reads it: whitespace is only
/// space/tab/CR/LF, NaN/Infinity/-Infinity are numbers, `\udXXX` escapes may
/// leave lone surrogates, ints are unbounded (up to 4300 digits), control
/// characters inside strings are refused, a duplicate key keeps the last.
/// Containers below the top level are only checked, not kept: `set()` of
/// the result never looks inside them.
struct JsonParser {
    s: Vec<char>,
    i: usize,
    max_depth: usize,
}

impl JsonParser {
    fn loads(text: &str, max_depth: usize) -> Result<Top, JsonErr> {
        let mut p = JsonParser {
            s: text.chars().collect(),
            i: 0,
            max_depth,
        };
        p.ws();
        let top = match p.peek() {
            Some('[') => {
                p.i += 1;
                let mut items = Vec::new();
                p.ws();
                if p.peek() == Some(']') {
                    p.i += 1;
                } else {
                    loop {
                        items.push(match p.peek() {
                            Some('[' | '{') => {
                                p.skip_value(1)?;
                                None
                            }
                            _ => Some(p.scalar()?),
                        });
                        p.ws();
                        match p.next() {
                            Some(',') => p.ws(),
                            Some(']') => break,
                            _ => return Err(JsonErr::Value),
                        }
                    }
                }
                Top::List(items)
            }
            Some('{') => {
                p.i += 1;
                let mut keys = Vec::new();
                p.ws();
                if p.peek() == Some('}') {
                    p.i += 1;
                } else {
                    loop {
                        keys.push(p.key()?);
                        p.skip_value(1)?;
                        p.ws();
                        match p.next() {
                            Some(',') => p.ws(),
                            Some('}') => break,
                            _ => return Err(JsonErr::Value),
                        }
                    }
                }
                Top::Dict(keys)
            }
            _ => Top::Scalar(p.scalar()?),
        };
        p.ws();
        if p.i != p.s.len() {
            return Err(JsonErr::Value); // Extra data
        }
        Ok(top)
    }

    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }

    fn next(&mut self) -> Option<char> {
        let c = self.peek();
        self.i += usize::from(c.is_some());
        c
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) {
            self.i += 1;
        }
    }

    fn starts_with(&self, lit: &str) -> bool {
        let n = lit.chars().count();
        self.s.len() >= self.i + n && lit.chars().eq(self.s[self.i..self.i + n].iter().copied())
    }

    /// `"key"`, whitespace, `:`, whitespace.
    fn key(&mut self) -> Result<PyStr, JsonErr> {
        if self.peek() != Some('"') {
            return Err(JsonErr::Value);
        }
        let k = self.string()?;
        self.ws();
        if self.next() != Some(':') {
            return Err(JsonErr::Value);
        }
        self.ws();
        Ok(k)
    }

    /// Checks one value of any shape without keeping it; iterative, so a
    /// deep document cannot overflow our stack. `depth` is the nesting of
    /// the container this value sits in.
    fn skip_value(&mut self, depth: usize) -> Result<(), JsonErr> {
        let mut stack: Vec<bool> = Vec::new(); // open containers, true = object
        loop {
            // at a value
            match self.peek() {
                Some(open @ ('[' | '{')) => {
                    self.i += 1;
                    if depth + stack.len() + 1 > self.max_depth {
                        return Err(JsonErr::Recursion);
                    }
                    self.ws();
                    let close = if open == '[' { ']' } else { '}' };
                    if self.peek() == Some(close) {
                        self.i += 1;
                    } else {
                        stack.push(open == '{');
                        if open == '{' {
                            self.key()?;
                        }
                        continue;
                    }
                }
                _ => {
                    self.scalar()?;
                }
            }
            // after a complete value: a separator, or closing brackets
            loop {
                let Some(&obj) = stack.last() else {
                    return Ok(());
                };
                self.ws();
                match self.next() {
                    Some(',') => {
                        self.ws();
                        if obj {
                            self.key()?;
                        }
                        break;
                    }
                    Some(']') if !obj => {
                        stack.pop();
                    }
                    Some('}') if obj => {
                        stack.pop();
                    }
                    _ => return Err(JsonErr::Value),
                }
            }
        }
    }

    fn scalar(&mut self) -> Result<Item, JsonErr> {
        let lits: [(&str, Item); 6] = [
            ("null", Item::None),
            ("true", Item::Num(Num::Bool(true))),
            ("false", Item::Num(Num::Bool(false))),
            ("NaN", Item::Num(Num::Float(f64::NAN))),
            ("Infinity", Item::Num(Num::Float(f64::INFINITY))),
            ("-Infinity", Item::Num(Num::Float(f64::NEG_INFINITY))),
        ];
        if self.peek() == Some('"') {
            return Ok(Item::Str(self.string()?));
        }
        for (lit, item) in lits {
            if self.starts_with(lit) {
                self.i += lit.len();
                return Ok(item);
            }
        }
        self.number().map(Item::Num)
    }

    /// `-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][-+]?[0-9]+)?`, ASCII digits only.
    fn number(&mut self) -> Result<Num, JsonErr> {
        let start = self.i;
        let digit = |c: Option<char>| c.is_some_and(|c| c.is_ascii_digit());
        if self.peek() == Some('-') {
            self.i += 1;
        }
        match self.peek() {
            Some('0') => self.i += 1,
            Some('1'..='9') => {
                while digit(self.peek()) {
                    self.i += 1;
                }
            }
            _ => return Err(JsonErr::Value),
        }
        let mut is_float = false;
        if self.peek() == Some('.') && digit(self.s.get(self.i + 1).copied()) {
            self.i += 1;
            while digit(self.peek()) {
                self.i += 1;
            }
            is_float = true;
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            let save = self.i;
            self.i += 1;
            if matches!(self.peek(), Some('+' | '-')) {
                self.i += 1;
            }
            if digit(self.peek()) {
                while digit(self.peek()) {
                    self.i += 1;
                }
                is_float = true;
            } else {
                self.i = save;
            }
        }
        let text: String = self.s[start..self.i].iter().collect();
        if is_float {
            return text
                .parse::<f64>()
                .map(Num::Float)
                .map_err(|_| JsonErr::Value);
        }
        if text.trim_start_matches('-').len() > INT_MAX_STR_DIGITS {
            return Err(JsonErr::Value);
        }
        text.parse::<BigInt>()
            .map(Num::Int)
            .map_err(|_| JsonErr::Value)
    }

    fn hex4(&self, at: usize) -> Option<u32> {
        let h: String = self.s.get(at..at + 4)?.iter().collect();
        if h.chars().all(|c| c.is_ascii_hexdigit()) {
            u32::from_str_radix(&h, 16).ok()
        } else {
            None
        }
    }

    /// A string, at its opening quote.
    fn string(&mut self) -> Result<PyStr, JsonErr> {
        self.i += 1;
        let mut out = Vec::new();
        loop {
            let c = self.next().ok_or(JsonErr::Value)?;
            match c {
                '"' => return Ok(out),
                '\\' => {
                    let e = self.next().ok_or(JsonErr::Value)?;
                    let v = match e {
                        '"' => 0x22,
                        '\\' => 0x5C,
                        '/' => 0x2F,
                        'b' => 0x08,
                        'f' => 0x0C,
                        'n' => 0x0A,
                        'r' => 0x0D,
                        't' => 0x09,
                        'u' => {
                            let mut v = self.hex4(self.i).ok_or(JsonErr::Value)?;
                            self.i += 4;
                            if (0xD800..=0xDBFF).contains(&v)
                                && self.s.len() > self.i + 6
                                && self.s[self.i] == '\\'
                                && self.s[self.i + 1] == 'u'
                            {
                                let low = self.hex4(self.i + 2).ok_or(JsonErr::Value)?;
                                if (0xDC00..=0xDFFF).contains(&low) {
                                    v = 0x10000 + ((v - 0xD800) << 10) + (low - 0xDC00);
                                    self.i += 6;
                                }
                            }
                            v
                        }
                        _ => return Err(JsonErr::Value),
                    };
                    out.push(v);
                }
                c if (c as u32) < 0x20 => return Err(JsonErr::Value), // strict
                c => out.push(u32::from(c)),
            }
        }
    }
}

/// `set(json.loads(path.read_text()))` with the Python's
/// `except (OSError, ValueError, TypeError): return set()`: a list's
/// members, an object's keys, a string's characters; a number, null, or a
/// list holding a list is the TypeError, so nothing. A RecursionError is not
/// caught: it is the `Err`. `thread`: read on a worker thread, whose stack
/// lets the decoder go deeper than on the main thread.
fn json_set(path: &Path, thread: bool) -> Result<Seen, String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(Seen::default()); // OSError, or not UTF-8 (ValueError)
    };
    let top =
        match JsonParser::loads(&text, json_max_depth(thread)) {
            Ok(top) => top,
            Err(JsonErr::Value) => return Ok(Seen::default()),
            Err(JsonErr::Recursion) => return Err(
                "RecursionError: maximum recursion depth exceeded while decoding a JSON document"
                    .into(),
            ),
        };
    let mut seen = Seen::default();
    match top {
        Top::List(items) => {
            if items.iter().any(Option::is_none) {
                return Ok(Seen::default()); // unhashable: TypeError
            }
            items.into_iter().flatten().for_each(|i| seen.add(i));
        }
        Top::Dict(keys) => keys.into_iter().for_each(|k| seen.add(Item::Str(k))),
        Top::Scalar(Item::Str(s)) => s.into_iter().for_each(|c| seen.add(Item::Str(vec![c]))),
        Top::Scalar(_) => {} // not iterable: TypeError
    }
    Ok(seen)
}

/// Read on the main thread, by `main`.
fn load_reported() -> Result<Seen, String> {
    json_set(&reported_path(), false)
}

/// `json.dumps(sorted(seen))` of strs.
fn dumps_set(seen: &BTreeSet<PyStr>) -> String {
    format!(
        "[{}]",
        seen.iter().map(pystr_json).collect::<Vec<_>>().join(", ")
    )
}

/// An OSError is ignored, as in the Python; the TypeError of sorting a set
/// that mixes kinds is not (`Err`).
fn save_reported(seen: &Seen) -> Result<(), String> {
    let path = reported_path();
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return Ok(());
        }
    }
    let text = seen.dumps()?;
    let _ = crate::util::atomic_write(&path, text);
    Ok(())
}

/// Notify about resets not already reported. Returns the updated set.
pub fn announce(found: &[crate::guard::Reset], seen: &Seen) -> Seen {
    let new: Vec<&crate::guard::Reset> = if crate::settings::get_bool("notify_resets") {
        found
            .iter()
            .filter(|r| !seen.strs.contains(&pystr_of_path(&r.backup)))
            .collect()
    } else {
        Vec::new()
    };
    for r in new {
        let n = r.lost.len();
        let lost = if n > 0 {
            format!(
                " {n} of your entries {} removed.",
                if n == 1 { "was" } else { "were" }
            )
        } else {
            String::new()
        };
        let body = format!(
            "{} was replaced with the shipped defaults.{lost} \
             Your version was kept — open Almanac to restore it.",
            r.name()
        );
        notify("Your settings were reset", &body, "health", "critical");
    }
    let mut out = seen.clone();
    out.strs
        .extend(found.iter().map(|r| pystr_of_path(&r.backup)));
    out
}

// ---- plugin updates --------------------------------------------------------

pub const PLUGIN_CHECK_EVERY: u64 = 6 * 3600;

fn plugins_seen_path() -> PathBuf {
    crate::xdg::state_home()
        .join("almanac")
        .join("plugin-updates-notified.json")
}

/// Plugins with a clean update waiting, fetched as Omarchy fetches.
///
/// The names are Python strs (`d.name`): a directory name that is not UTF-8
/// comes out surrogateescape'd, and sorts by code point.
pub fn plugin_updates() -> Vec<PyStr> {
    let pdir = crate::plugins::plugins_dir();
    let mut out = Vec::new();
    if !pdir.is_dir() {
        return out;
    }
    // `pdir.glob("*")`: pathlib's glob matches dot-entries too.
    let mut dirs: Vec<PathBuf> = match std::fs::read_dir(&pdir) {
        Ok(rd) => rd.filter_map(Result::ok).map(|e| e.path()).collect(),
        Err(_) => Vec::new(),
    };
    // `sorted()` of Paths: by their str, code point by code point.
    dirs.sort_by_cached_key(|d| pystr_of_path(d));
    for d in dirs {
        if d.join("manifest.json").is_file() && updatable(&d) {
            out.push(
                d.file_name()
                    .map(|n| pystr_of_path(Path::new(n)))
                    .unwrap_or_default(),
            );
        }
    }
    out
}

/// `plugins.classify(d, {}).updatable`. `classify` hands git the
/// directory's own bytes, so a plugin whose path is not UTF-8 is reached as
/// the Python reaches it.
fn updatable(d: &Path) -> bool {
    crate::plugins::classify(d, Some(&mut crate::plugins::FetchCache::new()), true).updatable()
}

/// Notify only when something new is waiting -- not the same list again,
/// and not when the count merely fell because some were updated.
pub fn announce_plugin_updates(waiting: &[PyStr], last: &BTreeSet<PyStr>) -> BTreeSet<PyStr> {
    let all: BTreeSet<PyStr> = waiting.iter().cloned().collect();
    let new = all.difference(last).next().is_some();
    if new && crate::settings::get_bool("notify_plugin_updates") {
        let mut sorted = waiting.to_vec();
        sorted.sort();
        // An argv carries a surrogate as its byte; notify's text is a
        // Rust str, so it is shown as U+FFFD.
        let lossy = |s: &PyStr| {
            s.iter()
                .map(|&c| char::from_u32(c).unwrap_or('\u{fffd}'))
                .collect::<String>()
        };
        let names = sorted
            .iter()
            .take(4)
            .map(lossy)
            .collect::<Vec<_>>()
            .join(", ")
            + if waiting.len() > 4 { "…" } else { "" };
        notify(
            &format!("{} plugin update(s) available", waiting.len()),
            &names,
            "plugins",
            "normal",
        );
    }
    all
}

thread_local! {
    /// When set, `notify` records its arguments here instead of showing
    /// anything: the Python tests' `mock.patch.object(watcher, "_notify")`,
    /// used by the tests and the parity hook.
    static CAPTURE: RefCell<Option<Vec<[String; 4]>>> = const { RefCell::new(None) };
}

/// A notification whose action opens Almanac on the relevant page.
///
/// Python defaults: `page="health"`, `urgency="critical"`.
fn notify(title: &str, body: &str, page: &str, urgency: &str) {
    let captured = CAPTURE.with(|c| {
        c.borrow_mut().as_mut().map(|calls| {
            calls.push([title.into(), body.into(), page.into(), urgency.into()]);
        })
    });
    if captured.is_some() {
        return;
    }
    let Some(tool) = which("notify-send") else {
        return;
    };
    let tool = tool.to_string_lossy().into_owned();
    // What the user reads: a failed check's Python error in plain words.
    let (title, body) = (
        crate::util::human_error(title),
        crate::util::human_error(body),
    );
    let (page, urgency) = (page.to_owned(), urgency.to_owned());
    std::thread::spawn(move || {
        let res = crate::util::run(
            &[
                &tool,
                "-u",
                &urgency,
                "-i",
                "io.github.twodxme.Almanac",
                "-A",
                "open=Open Almanac",
                "--wait",
                &title,
                &body,
            ],
            3600,
        );
        if res.failed_to_start || res.timed_out {
            return;
        }
        if py_strip(&res.stdout) == "open" {
            let mut argv = launcher();
            argv.push("--page".into());
            argv.push(page.into());
            // Uncaught in the Python's thread: it dies with a traceback.
            if let Err(e) = popen_detached(&argv, false) {
                eprintln!("Exception in thread (run):\n{e}");
            }
        }
    });
}

/// `almanac --guard`. Returns the process exit status.
pub fn main() -> i32 {
    use gtk::gio;
    use gtk::gio::prelude::*;
    use gtk::glib;
    use std::rc::Rc;

    // An exception the Python does not catch ends `almanac --guard` with
    // a traceback and status 1.
    let uncaught = |e: String| {
        eprintln!("almanac --guard: {e}");
        1
    };
    let me = i32::try_from(std::process::id()).unwrap_or(i32::MAX);
    match claim(&pid_path(), me, &guard_alive) {
        Ok(true) => {}
        Ok(false) => return 0, // one per session
        Err(e) => return uncaught(e),
    }

    let found = match crate::guard::try_resets_at(None, crate::guard::PyStack::WATCHER) {
        Ok(found) => found,
        Err(e) => return uncaught(e),
    };
    let seen = match load_reported() {
        Ok(seen) => announce(&found, &seen),
        Err(e) => return uncaught(e),
    };
    if let Err(e) = save_reported(&seen) {
        return uncaught(e);
    }

    let Some(root) = crate::guard::defaults_root() else {
        return 0; // nothing ships defaults; nothing to guard
    };

    let main_loop = glib::MainLoop::new(None, false);
    struct St {
        seen: Seen,
        pending: Option<glib::SourceId>,
    }
    let state = Rc::new(RefCell::new(St {
        seen,
        pending: None,
    }));

    fn check(state: &Rc<RefCell<St>>) -> glib::ControlFlow {
        // The source is dispatching now; returning Break removes it.
        let seen = {
            let mut st = state.borrow_mut();
            st.pending = None;
            st.seen.clone()
        };
        // `check` runs from the loop: one Python frame more than `main`.
        let at = crate::guard::PyStack {
            frames: crate::guard::PyStack::WATCHER.frames + 1,
            thread: false,
        };
        let found = match crate::guard::try_resets_at(None, at) {
            Ok(found) => found,
            Err(e) => {
                // Raised in a GLib callback: printed, the source removed,
                // the set left as it was.
                eprintln!("almanac --guard: {e}");
                return glib::ControlFlow::Break;
            }
        };
        let seen = announce(&found, &seen);
        state.borrow_mut().seen = seen.clone();
        // Raised in a GLib callback, the Python's TypeError is printed and
        // the source removed; the loop carries on.
        if let Err(e) = save_reported(&seen) {
            eprintln!("almanac --guard: {e}");
        }
        glib::ControlFlow::Break
    }

    let mut monitors: Vec<gio::FileMonitor> = Vec::new();
    // `sorted(root.iterdir())`: the Python raises on an unreadable root.
    let mut subs: Vec<PathBuf> = match std::fs::read_dir(&root) {
        Ok(rd) => rd.filter_map(Result::ok).map(|e| e.path()).collect(),
        Err(e) => {
            eprintln!("almanac --guard: {}: {e}", root.display());
            return 1;
        }
    };
    subs.sort();
    for sub in subs {
        let Some(name) = sub.file_name() else {
            continue;
        };
        let target = crate::guard::config_home().join(name);
        if !target.is_dir() {
            continue;
        }
        let mon = match gio::File::for_path(&target)
            .monitor_directory(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
        {
            Ok(m) => m,
            Err(_) => continue,
        };
        let st = state.clone();
        mon.connect_changed(move |_mon, gfile, _other, event| {
            if event != gio::FileMonitorEvent::Created {
                return;
            }
            let base = gfile
                .basename()
                .map(|b| b.to_string_lossy().into_owned())
                .unwrap_or_default();
            if !crate::guard::BAK.is_match(&base) {
                return;
            }
            if let Some(old) = st.borrow_mut().pending.take() {
                old.remove();
            }
            let st2 = st.clone();
            let id = glib::timeout_add_seconds_local(3, move || check(&st2));
            st.borrow_mut().pending = Some(id);
        });
        monitors.push(mon); // a collected monitor stops delivering
    }

    // Plugin updates: shortly after login, then every few hours, off the
    // main loop -- a fetch per plugin must not delay noticing a reset.
    fn load_seen() -> Result<Seen, String> {
        json_set(&plugins_seen_path(), true) // on `work`'s thread
    }

    fn check_plugins() -> glib::ControlFlow {
        std::thread::spawn(|| {
            let waiting = plugin_updates();
            // Only the strs of the old set can meet `set(waiting)`.
            let last = match load_seen() {
                Ok(last) => last.strs,
                Err(e) => {
                    eprintln!("Exception in thread (work):\n{e}"); // the thread dies
                    return;
                }
            };
            let seen = announce_plugin_updates(&waiting, &last);
            let p = plugins_seen_path();
            if let Some(parent) = p.parent() {
                if std::fs::create_dir_all(parent).is_err() {
                    return;
                }
            }
            let _ = crate::util::atomic_write(&p, dumps_set(&seen));
        });
        glib::ControlFlow::Continue // keep the timer
    }
    glib::timeout_add_seconds_local(120, || {
        check_plugins();
        glib::ControlFlow::Break
    });
    glib::timeout_add_seconds_local(PLUGIN_CHECK_EVERY as u32, check_plugins);

    for sig in [15, 2] {
        let l = main_loop.clone();
        // The Python's callback is loop.quit, which returns None: GLib
        // removes the source after its first delivery.
        glib_unix::unix_signal_add_local(sig, move || {
            l.quit();
            glib::ControlFlow::Break
        });
    }
    main_loop.run();
    drop(monitors);
    let _ = std::fs::remove_file(pid_path());
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    // Tests share the process environment; one at a time.

    #[test]
    fn command_name_is_almanac() {
        assert_eq!(command_name(), "almanac");
    }

    /// `mock.patch.dict("os.environ", {key: value})`.
    struct EnvVar(&'static str, Option<std::ffi::OsString>);
    impl EnvVar {
        fn set(key: &'static str, value: &Path) -> EnvVar {
            let old = std::env::var_os(key);
            std::env::set_var(key, value);
            EnvVar(key, old)
        }
    }
    impl Drop for EnvVar {
        fn drop(&mut self) {
            match self.1.take() {
                Some(v) => std::env::set_var(self.0, v),
                None => std::env::remove_var(self.0),
            }
        }
    }

    /// `mock.patch.object(watcher, "_notify") as notify`: run `f`, return
    /// the calls (title, body, page, urgency).
    fn with_notify_mock(f: impl FnOnce()) -> Vec<[String; 4]> {
        CAPTURE.with(|c| *c.borrow_mut() = Some(Vec::new()));
        f();
        CAPTURE.with(|c| c.borrow_mut().take()).unwrap_or_default()
    }

    // BackgroundGuard

    #[test]
    fn test_enabling_writes_a_standard_autostart_entry() {
        let _l = crate::util::TEST_ENV
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        let entry;
        {
            let _e = EnvVar::set("XDG_CONFIG_HOME", d.path());
            // Not in the Python test: without it, disable() would stop the
            // real guard of this session, if one is running.
            let _r = EnvVar::set("XDG_RUNTIME_DIR", d.path());
            enable(false).unwrap();
            entry = std::fs::read_to_string(d.path().join("autostart").join(ENTRY_NAME)).unwrap();
            assert!(enabled());
            disable().unwrap();
            assert!(!enabled());
        }
        assert!(entry.contains("--guard"));
        assert!(entry.contains("NoDisplay=true"));
    }

    #[test]
    fn test_exec_arguments_are_quoted_per_the_spec() {
        assert_eq!(quote("/usr/bin/almanac"), "/usr/bin/almanac");
        assert_eq!(
            quote("/path with space/app.py"),
            "\"/path with space/app.py\""
        );
    }

    #[test]
    fn test_a_reset_is_announced_once() {
        let _l = crate::util::TEST_ENV
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let r = crate::guard::Reset::new(
            PathBuf::from("/c/shell.json"),
            PathBuf::from("/c/shell.json.bak.1"),
            1.0,
            vec!["x".into()],
        );
        let calls = with_notify_mock(|| {
            let seen = announce(std::slice::from_ref(&r), &Seen::default());
            announce(std::slice::from_ref(&r), &seen);
        });
        assert_eq!(calls.len(), 1);
        assert!(calls.last().unwrap()[1].contains("1 of your entries was removed"));
    }

    // PluginUpdateNotices

    #[test]
    fn test_only_something_new_is_announced() {
        let _l = crate::util::TEST_ENV
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let v = |xs: &[&str]| xs.iter().map(|s| pystr(s)).collect::<Vec<_>>();
        let calls = with_notify_mock(|| {
            let seen = announce_plugin_updates(&v(&["a", "b"]), &BTreeSet::new());
            let seen = announce_plugin_updates(&v(&["a", "b"]), &seen); // same
            let seen = announce_plugin_updates(&v(&["a"]), &seen); // fewer
            announce_plugin_updates(&v(&["a", "c"]), &seen); // new one
        });
        assert_eq!(calls.len(), 2);
        assert_eq!(calls.last().unwrap()[2], "plugins");
    }

    #[test]
    fn test_switched_off_in_preferences() {
        let _l = crate::util::TEST_ENV
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        let _e = EnvVar::set("XDG_STATE_HOME", d.path());
        let calls = with_notify_mock(|| {
            crate::settings::set("notify_plugin_updates", Value::Bool(false)).unwrap();
            announce_plugin_updates(&[pystr("a")], &BTreeSet::new());
        });
        assert!(calls.is_empty());
    }

    #[test]
    fn py_int_matches_python() {
        let int = |s: &str| py_int(s).map(|n| n.to_string());
        assert_eq!(int(" 12\n").as_deref(), Some("12"));
        assert_eq!(int("+1_2").as_deref(), Some("12"));
        assert_eq!(int("-3").as_deref(), Some("-3"));
        assert_eq!(int("007").as_deref(), Some("7"));
        assert_eq!(int("1__2"), None);
        assert_eq!(int("_1"), None);
        assert_eq!(int("1_"), None);
        assert_eq!(int(""), None);
        assert_eq!(int("12a"), None);
        assert_eq!(int("- 1"), None);
        assert_eq!(int("1 2"), None);
        // any script's decimal digits, every isspace() character
        assert_eq!(int("\u{ff11}\u{ff12}").as_deref(), Some("12"));
        assert_eq!(int("\u{663}").as_deref(), Some("3"));
        assert_eq!(int("\u{1c}12\u{1f}").as_deref(), Some("12"));
        assert_eq!(int("\u{3000}12\u{85}").as_deref(), Some("12"));
        assert_eq!(int("\u{b2}"), None); // superscript two: a digit, not decimal
        assert_eq!(
            int("99999999999999999999").as_deref(),
            Some("99999999999999999999")
        );
        assert_eq!(int(&"1".repeat(4301)), None);
    }

    /// Two guards started together: one runs. The pid file is read and
    /// written under a lock, so the second sees the first.
    #[test]
    fn two_guards_started_together_run_once() {
        let d = tempfile::tempdir().unwrap();
        let pid = d.path().join("almanac-guard.pid");
        // A stale pid that is alive as a process (this one) but not a guard;
        // checking it takes a while, as /proc can.
        std::fs::write(&pid, std::process::id().to_string()).unwrap();
        let guards = std::sync::Arc::new(std::sync::Mutex::new(Vec::<i32>::new()));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        // Two live processes stand in for the two guards' pids.
        let mut kids: Vec<std::process::Child> = (0..2)
            .map(|_| {
                std::process::Command::new("sleep")
                    .arg("30")
                    .spawn()
                    .unwrap()
            })
            .collect();
        let pids: Vec<i32> = kids.iter().map(|k| k.id() as i32).collect();
        let handles: Vec<_> = pids
            .into_iter()
            .map(|me| {
                let (pid, guards, barrier) = (pid.clone(), guards.clone(), barrier.clone());
                std::thread::spawn(move || {
                    let alive = |p: i32| {
                        std::thread::sleep(std::time::Duration::from_millis(200));
                        guards.lock().unwrap().contains(&p)
                    };
                    barrier.wait();
                    let won = claim(&pid, me, &alive).unwrap();
                    if won {
                        guards.lock().unwrap().push(me);
                    }
                    won
                })
            })
            .collect();
        let won: Vec<bool> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        for k in &mut kids {
            let _ = k.kill();
            let _ = k.wait();
        }
        assert_eq!(won.iter().filter(|w| **w).count(), 1, "{won:?}");
    }

    #[test]
    fn running_overflow_is_an_error() {
        let _l = crate::util::TEST_ENV
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        let _r = EnvVar::set("XDG_RUNTIME_DIR", d.path());
        let pid = d.path().join("almanac-guard.pid");
        for text in ["2147483648", "-2147483649", "99999999999999999999"] {
            std::fs::write(&pid, text).unwrap();
            assert!(running().is_err(), "{text}");
        }
        for text in ["2147483647", "-2147483648", "0", "x", ""] {
            std::fs::write(&pid, text).unwrap();
            assert_eq!(running(), Ok(0), "{text}");
        }
    }

    fn set_of(text: &str) -> Result<Seen, String> {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s.json");
        std::fs::write(&p, text).unwrap();
        json_set(&p, false)
    }

    #[test]
    fn json_set_matches_python() {
        let dumped = |t: &str| set_of(t).and_then(|s| s.dumps());
        assert_eq!(dumped("[1, 2]").unwrap(), "[1, 2]");
        assert_eq!(dumped("[true, 1, 1.0]").unwrap(), "[true]");
        assert_eq!(dumped("[1.0, true]").unwrap(), "[1.0]");
        assert_eq!(dumped("[null]").unwrap(), "[null]");
        assert_eq!(dumped("[2, 1.5, -0.0, 0]").unwrap(), "[-0.0, 1.5, 2]");
        assert_eq!(
            dumped("[NaN, -Infinity, 1e400]").unwrap(),
            "[-Infinity, Infinity, NaN]"
        );
        assert!(dumped(r#"["a", 1]"#).is_err());
        assert!(dumped("[null, 1]").is_err());
        assert_eq!(dumped(r#"["\ud800"]"#).unwrap(), r#"["\ud800"]"#);
        assert_eq!(
            dumped(r#"{"\ud83d\ude00": 1, "b": [1]}"#).unwrap(),
            r#"["b", "\ud83d\ude00"]"#
        ); // code point order
        assert_eq!(dumped(r#""a\ud800""#).unwrap(), r#"["a", "\ud800"]"#);
        assert_eq!(
            dumped("[123456789012345678901234567890]").unwrap(),
            "[123456789012345678901234567890]"
        );
        for bad in [
            "[1,]",
            "[01]",
            "[1.]",
            "[1e]",
            "nan",
            "[\"\t\"]",
            "\u{feff}[]",
            "[\"\\x\"]",
            "{\"a\" 1}",
            "[[1]]",
            "1",
            "null",
        ] {
            assert!(set_of(bad).unwrap().is_empty(), "{bad}");
        }
        assert!(set_of(&format!("[{}]", "1".repeat(4301)))
            .unwrap()
            .is_empty());
        // How deep Python gets before its stack overflows depends on the
        // machine's stack size, so only "does not crash" is checked past it.
        let _ = set_of(&format!("[{}{}]", "[".repeat(60_000), "]".repeat(60_000)));
        assert!(
            set_of(&format!("[{}{}]", "[".repeat(1000), "]".repeat(1000)))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn json_nan_is_one_member() {
        let dumped = |t: &str| set_of(t).and_then(|s| s.dumps()).unwrap();
        assert_eq!(dumped("[NaN, NaN]"), "[NaN]");
        assert_eq!(dumped("[NaN, 1, NaN]"), "[1, NaN]");
        assert_eq!(
            set_of("[Infinity, Infinity, NaN, NaN, -Infinity]")
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn json_depth_is_pythons() {
        // Python 3.14 (8 MB stack): the main thread reads ~52101..52139
        // levels, a worker thread 52154.
        let deep = |d: usize| format!("{}{}", "[".repeat(d), "]".repeat(d));
        assert!(set_of(&deep(52_090)).unwrap().is_empty());
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s.json");
        std::fs::write(&p, deep(52_150)).unwrap();
        assert!(json_set(&p, true).is_ok() || json_max_depth(true) < 52_150);
        // Past that the outcome follows the machine's stack size (as Python's
        // does); it must not crash either way.
        std::fs::write(&p, deep(52_200)).unwrap();
        let _ = json_set(&p, false);
    }

    #[test]
    fn type_error_names_the_number_type() {
        let e = set_of(r#"["a", 1.5]"#).unwrap().dumps().unwrap_err();
        assert!(e.contains("'float'"), "{e}");
        let e = set_of("[true, null]").unwrap().dumps().unwrap_err();
        assert!(e.contains("'bool'"), "{e}");
    }

    #[test]
    fn pid_path_is_normalised_as_path_does() {
        let _l = crate::util::TEST_ENV
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for (rt, want) in [
            ("/run//x/", "/run/x/almanac-guard.pid"),
            (".", "almanac-guard.pid"),
            ("/run/./x", "/run/x/almanac-guard.pid"),
            ("///run", "/run/almanac-guard.pid"),
            ("//run", "//run/almanac-guard.pid"),
            ("rel/dir/", "rel/dir/almanac-guard.pid"),
        ] {
            let _r = EnvVar::set("XDG_RUNTIME_DIR", Path::new(rt));
            assert_eq!(pid_path(), PathBuf::from(want), "{rt}");
        }
    }

    #[test]
    fn enable_with_a_non_utf8_launcher_fails_as_python_does() {
        use std::os::unix::fs::PermissionsExt;
        let _l = crate::util::TEST_ENV
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        let bin = d.path().join(OsStr::from_bytes(b"b\xffin"));
        std::fs::create_dir(&bin).unwrap();
        std::fs::write(bin.join("almanac"), "").unwrap();
        std::fs::set_permissions(bin.join("almanac"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        // As launcher() finds it on PATH, without swapping PATH for every
        // other test that spawns a program meanwhile.
        let found = which_in("almanac", Some(bin.clone().into_os_string())).unwrap();
        let _c = EnvVar::set("XDG_CONFIG_HOME", d.path());
        let e = enable_with(false, &[found.into_os_string()]).unwrap_err();
        assert!(
            e.starts_with(
                "UnicodeEncodeError: 'utf-8' codec can't encode character '\\udcff' in position "
            ),
            "{e}"
        );
        // write_text truncated the file before failing
        assert_eq!(std::fs::read_to_string(autostart_path()).unwrap(), "");
        assert_eq!(
            quote_py(&vec![0x61, 0xDCFF, 0x20]),
            vec![0x22, 0x61, 0xDCFF, 0x20, 0x22]
        );
    }

    #[test]
    fn which_is_shutil_which() {
        use std::os::unix::fs::PermissionsExt;
        // Root may execute a file with only the "others" execute bit, so the
        // 0o601 file below counts as executable for root -- for Python's
        // shutil.which too. The expectation is a normal user's.
        if nix::unistd::geteuid().is_root() {
            return;
        }
        let _l = crate::util::TEST_ENV
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (d.path().join("a"), d.path().join("b"));
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        std::fs::write(a.join("t"), "").unwrap();
        std::fs::set_permissions(a.join("t"), std::fs::Permissions::from_mode(0o601)).unwrap();
        std::fs::write(b.join("t"), "").unwrap();
        std::fs::set_permissions(b.join("t"), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = std::env::join_paths([&a, &b]).unwrap();
        assert_eq!(which_in("t", Some(path)), Some(b.join("t")));
        assert_eq!(which_in("sh", Some(OsString::new())), None);
    }
}
