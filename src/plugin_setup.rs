//! Port of `plugin_setup.py`.
//!
//! What a plugin still needs after it is installed, read from its README.
//!
//! The marketplace marks some plugins "requires additional setup" and stops
//! there. Installing them is the same `omarchy plugin add` as any other; the
//! setup is whatever their README asks for next. This reads the README from the
//! installed checkout -- local, nothing fetched -- and sorts what it finds:
//!
//! * **packages**  `pacman -S` / `yay -S` / `paru -S` lines: installed by
//!   Almanac itself, snapshotted and verified like any other install.
//! * **commands**  build steps, setup scripts and `systemctl --user` lines from
//!   the README's code blocks, run in a terminal in the plugin's directory --
//!   after the user has seen every line, and the contents of every file of
//!   the plugin's that a line runs (`./install`, `bash x.sh`, `python3
//!   setup.py`). A line whose file cannot be shown is not offered, and a file
//!   that changes after it was shown stops the run.
//! * **by hand**   what cannot be done for the user: API keys and logins,
//!   config and keybinding edits. Listed, with the README heading to read.
//!
//! Only code blocks are read for commands: prose is not an instruction. A line
//! is taken only when it starts with a known tool, so an example output block
//! or a snippet of QML is never run. `sudo` lines are kept as written -- the
//! terminal shows the password prompt, and nothing is elevated silently.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

// Python's `re` and the regex crate disagree on `\w`, `\s`, `\b` and
// re.I, so the patterns below spell out Python's meaning: `\w` is
// `str.isalnum()` or `_` (exactly \p{L}\p{N}_), `\s` is `str.isspace()`
// (White_Space plus U+001C..U+001F), a `\b` next to a word character is
// "not a word character, or the end", and re.I also matches İ and ı to i.
const PY_W: &str = r"[\p{L}\p{N}_]";
const PY_NW: &str = r"[^\p{L}\p{N}_]";

/// A Python pattern in regex-crate syntax: `\s`, `\S` and `\w` given
/// Python's meaning. (`\b` is written out by hand where it occurs.)
fn py(pat: &str) -> String {
    let mut out = String::new();
    let mut chars = pat.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push_str(r"[\s\x1C-\x1F]"),
            Some('S') => out.push_str(r"[^\s\x1C-\x1F]"),
            Some('w') => out.push_str(PY_W),
            Some(n) => {
                out.push('\\');
                out.push(n);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// re.I for an ASCII-literal pattern: Python's case-insensitive `i` also
/// matches U+0130 and U+0131, which simple case folding does not.
fn py_ci(pat: &str) -> String {
    format!("(?i:{})", pat.replace(['i', 'I'], "[iIİı]"))
}

/// `\b` after a word character: a non-word character next, or the end.
fn end_b() -> String {
    format!("(?:{PY_NW}|$)")
}

static FENCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?ms)^```[^\n]*\n(.*?)^```").unwrap());
static PKG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&py(r"^(?:sudo\s+)?(?:pacman|yay|paru)\s+-S\w*\s+(.+)$")).unwrap()
});
static PKG_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z0-9@][a-z0-9@._+-]*$").unwrap());
// Commands worth running for the user, by the tool they start with.
static RUNNABLE: LazyLock<Regex> = LazyLock::new(|| {
    // `(?:...)\b`: every tool ends in a word character; `\./[\w./-]+` may
    // end in either kind, so each case gets its own boundary.
    Regex::new(&format!(
        "{}(?:{}{}|{}(?:{PY_W}{}|[./-]{PY_W}))",
        py(r"^(?:sudo\s+)?"),
        py(concat!(
            r"(?:make|cmake|meson|ninja|cargo|go|npm|pnpm|yarn|bun|uv|pip|",
            r"pipx|python3?|gcc|cc|systemctl\s+--user|bash\s+\S+\.sh|sh\s+\S+\.sh)"
        )),
        end_b(),
        py(r"\./[\w./-]*"),
        end_b(),
    ))
    .unwrap()
});
// Never run, whatever the README says.
static NEVER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&py(concat!(
        r"(curl|wget)[^|]*\|\s*(sudo\s+)?(ba|z)?sh|rm\s+-rf\s+[/~]|",
        r"omarchy[- ]plugin|git\s+clone|cd\s"
    )))
    .unwrap()
});
// `\b(...)\b`, the leading `\b` checked by `by_hand_starts`.
static BY_HAND: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    let bounded = |body: &str| Regex::new(&format!("({}){}", py_ci(body), end_b())).unwrap();
    vec![
        (bounded(concat!(
            r"(api[ _-]?key|access token|secret|password|log ?in|sign ?in|",
            r"credential|oauth)"
        )), "Credentials or a sign-in"),
        (bounded(concat!(
            r"(keybind|bindings?\.(conf|lua)|hyprland\.(conf|lua)|",
            r"config\.(toml|json|ya?ml))"
        )), "Configuration you edit"),
    ]
});
static HEADING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&py(r"(?m)^(#{1,6})\s+(.+?)\s*#*$")).unwrap());
// Only these parts of a README are instructions for a user installing it.
// Everything else -- development, tests, uninstalling -- is not setup, and a
// README's `cargo clippy` or `uninstall.sh` must never be offered as such.
static SETUP_SECTION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&py_ci(concat!(
        r"install|setup|set up|getting started|quick ?start|",
        r"requirement|dependenc|prerequisite|build|configur|usage"
    )))
    .unwrap()
});
static OTHER_SECTION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        "{}|{}{}",
        py_ci(concat!(
            r"develop|contribut|test|uninstall|remov|troubleshoot|",
            r"faq|changelog|release|licen|hack|debug|lint"
        )),
        py_ci("ci"),
        end_b()
    ))
    .unwrap()
});
// Commands that are development chores even inside an install section.
static CHORE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        "(?:^|{PY_NW})(test|fmt|clippy|lint|check|bench|uninstall){}",
        end_b()
    ))
    .unwrap()
});
static PY_WORD_CHAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"\A{PY_W}\z")).unwrap());

fn is_py_word(c: char) -> bool {
    PY_WORD_CHAR.is_match(c.encode_utf8(&mut [0; 4]))
}

/// Starts of `pattern.finditer(text)` for a `BY_HAND` pattern: its regex
/// carries the trailing `\b`; the leading one (the body starts with a word
/// character) is checked here, moving on a character when it fails.
fn by_hand_starts(pattern: &Regex, text: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos <= text.len() {
        let Some(c) = pattern.captures_at(text, pos) else { break };
        let Some(m) = c.get(1) else { break };
        if text[..m.start()].chars().next_back().is_some_and(is_py_word) {
            pos = m.start() + text[m.start()..].chars().next().map_or(1, char::len_utf8);
            continue;
        }
        out.push(m.start());
        pos = m.end();
    }
    out
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SetupPlan {
    pub plugin_dir: PathBuf,
    /// file name, '' when there is none
    pub readme: String,
    pub packages: Vec<String>,
    pub commands: Vec<String>,
    /// path -> contents, shown first. Insertion-ordered like the Python dict;
    /// serialised as a JSON object. The path is kept as the file system's
    /// bytes, as Python's str keeps a name that is not UTF-8 (surrogateescape);
    /// see `esc` for how such a name is shown.
    #[serde(with = "ordered_map")]
    pub scripts: Vec<(OsString, String)>,
    /// path -> SHA-256 of the bytes shown in `scripts`, checked again just
    /// before anything runs (`unchanged`, and the guard `terminal_script`
    /// puts in front of every command).
    #[serde(with = "ordered_map")]
    pub digests: Vec<(OsString, String)>,
    /// (what, README heading)
    pub by_hand: Vec<(String, String)>,
}

impl SetupPlan {
    pub fn automatable(&self) -> bool {
        !self.packages.is_empty() || !self.commands.is_empty()
    }

    pub fn empty(&self) -> bool {
        !(self.automatable() || !self.by_hand.is_empty())
    }
}

/// `Vec<(String, String)>` as a JSON object, keeping order on the way out.
mod ordered_map {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::ffi::OsString;

    pub fn serialize<S: Serializer>(v: &[(OsString, String)], s: S) -> Result<S::Ok, S::Error> {
        s.collect_map(v.iter().map(|(k, x)| (super::esc(k), x)))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<(OsString, String)>, D::Error> {
        // serde_json is built with `preserve_order`, so the Map keeps the
        // order the object was written in.
        let m = serde_json::Map::<String, serde_json::Value>::deserialize(d)?;
        Ok(m.into_iter()
            .map(|(k, v)| (OsString::from(k), v.as_str().unwrap_or_default().to_string()))
            .collect())
    }
}

/// A file name as text. Python keeps bytes that are not UTF-8 as lone
/// surrogates (`'\udcff.sh'`), which a Rust `String` cannot hold; they are
/// spelled `\xff` instead (`backslashreplace`), the same for every byte, so
/// no two names collapse into one as they would with U+FFFD.
fn esc(s: &std::ffi::OsStr) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::new();
    for chunk in s.as_bytes().utf8_chunks() {
        out.push_str(chunk.valid());
        for b in chunk.invalid() {
            out.push_str(&format!("\\x{b:02x}"));
        }
    }
    out
}

fn _readme(plugin_dir: &Path) -> Option<PathBuf> {
    for name in ["README.md", "readme.md", "Readme.md", "README.MD", "README"] {
        if plugin_dir.join(name).is_file() {
            return Some(plugin_dir.join(name));
        }
    }
    None
}

/// Python's `str.isspace()`: Unicode White_Space plus the four ASCII
/// information separators (U+001C..U+001F) that Python also counts.
fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Python's `str.strip()` with no arguments.
fn py_strip(s: &str) -> &str {
    s.trim_matches(py_isspace)
}

/// Python's `str.splitlines()` (no ends kept).
fn py_splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        let sep = matches!(
            c,
            '\n' | '\r' | '\u{0b}' | '\u{0c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}'
                | '\u{2028}' | '\u{2029}'
        );
        if !sep {
            continue;
        }
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
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// `Path.read_text(errors="replace")`: UTF-8 with replacement characters, and
/// text mode's universal newlines (`\r\n` and `\r` read as `\n`).
fn read_text(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&bytes);
    Ok(text.replace("\r\n", "\n").replace('\r', "\n"))
}

/// `str(Path(p))`: how pathlib spells a POSIX path -- no empty or `.` parts,
/// no trailing slash, exactly two leading slashes kept, `.` for nothing.
fn py_path_str(p: &Path) -> String {
    let s = p.to_string_lossy();
    let root = if s.starts_with("//") && !s.starts_with("///") {
        "//"
    } else if s.starts_with('/') {
        "/"
    } else {
        ""
    };
    let parts: Vec<&str> = s.split('/').filter(|x| !x.is_empty() && *x != ".").collect();
    let joined = format!("{root}{}", parts.join("/"));
    if joined.is_empty() { ".".to_string() } else { joined }
}

/// `Path.resolve()` (non-strict): `os.path.realpath` as Python 3.14 has it.
/// Symlinks are followed as far as they exist, a missing part is kept as
/// written and `..` applied after it. There is no hop limit: a loop is found
/// by meeting a symlink again before it is resolved, and the path then stays
/// at that symlink.
fn resolve(p: &Path) -> PathBuf {
    use std::collections::HashMap;
    use std::ffi::OsStr;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    enum Part {
        Name(Vec<u8>),
        // A symlink whose target has been resolved; its path follows.
        Done(Vec<u8>),
    }
    let filename = p.as_os_str().as_bytes();
    // The stack of unresolved path parts, reversed.
    let mut rest: Vec<Part> = filename.split(|&b| b == b'/').rev().map(|x| Part::Name(x.to_vec())).collect();
    // The resolved path, absolute throughout.
    let mut path: Vec<u8> = if filename.starts_with(b"/") {
        b"/".to_vec()
    } else {
        std::env::current_dir()
            .map(|d| d.into_os_string().into_vec())
            .unwrap_or_else(|_| b"/".to_vec())
    };
    // Symlink path -> fully resolved target; None while still being resolved,
    // which is how a loop is recognised.
    let mut seen: HashMap<Vec<u8>, Option<Vec<u8>>> = HashMap::new();
    while let Some(part) = rest.pop() {
        let name = match part {
            Part::Done(link) => {
                seen.insert(link, Some(path.clone()));
                continue;
            }
            Part::Name(n) => n,
        };
        if name.is_empty() || name == b"." {
            continue;
        }
        if name == b".." {
            let cut = path.iter().rposition(|&b| b == b'/').unwrap_or(0);
            path.truncate(cut);
            if path.is_empty() {
                path = b"/".to_vec();
            }
            continue;
        }
        let mut newpath = path.clone();
        if newpath != b"/" {
            newpath.push(b'/');
        }
        newpath.extend_from_slice(&name);
        let np = Path::new(OsStr::from_bytes(&newpath));
        let is_link = match std::fs::symlink_metadata(np) {
            Ok(m) => m.file_type().is_symlink(),
            Err(_) => false,
        };
        if !is_link {
            path = newpath;
            continue;
        }
        if let Some(done) = seen.get(&newpath) {
            match done {
                // use the cached value
                Some(v) => path = v.clone(),
                // not resolved yet: a symlink loop
                None => path = newpath,
            }
            continue;
        }
        let target = match std::fs::read_link(np) {
            Ok(t) => t.into_os_string().into_vec(),
            Err(_) => {
                path = newpath;
                continue;
            }
        };
        if target.starts_with(b"/") {
            path = b"/".to_vec();
        }
        seen.insert(newpath.clone(), None);
        rest.push(Part::Done(newpath));
        rest.extend(target.split(|&b| b == b'/').rev().map(|x| Part::Name(x.to_vec())));
    }
    PathBuf::from(std::ffi::OsString::from_vec(path))
}

/// `shlex.quote`.
fn shlex_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    let safe = s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c));
    if safe {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

/// `shlex.split(s, posix=True)`: the same state machine as Python's
/// `shlex.shlex` with `whitespace_split=True` and no comment characters.
/// Errs with Python's message on an open quote or a trailing backslash.
fn shlex_split(s: &str) -> Result<Vec<String>, String> {
    const WS: &str = " \t\r\n";
    let mut out = Vec::new();
    let mut token = String::new();
    let mut quoted = false;
    // ' ' = between words, 'a' = in a word, '\'' / '"' = in quotes, '\\' = escape
    let mut state = ' ';
    let mut escaped_state = 'a';
    let mut chars = s.chars();
    loop {
        let next = chars.next();
        match state {
            ' ' => match next {
                None => break,
                Some(c) if WS.contains(c) => {
                    if !token.is_empty() || quoted {
                        out.push(std::mem::take(&mut token));
                        quoted = false;
                    }
                }
                Some('\\') => {
                    escaped_state = 'a';
                    state = '\\';
                }
                Some(c) if c == '\'' || c == '"' => {
                    quoted = true;
                    state = c;
                }
                Some(c) => {
                    token.push(c);
                    state = 'a';
                }
            },
            '\'' | '"' => match next {
                None => return Err("No closing quotation".to_string()),
                Some(c) if c == state => state = 'a',
                Some('\\') if state == '"' => {
                    escaped_state = state;
                    state = '\\';
                }
                Some(c) => token.push(c),
            },
            '\\' => match next {
                None => return Err("No escaped character".to_string()),
                Some(c) => {
                    if (escaped_state == '\'' || escaped_state == '"')
                        && c != '\\'
                        && c != escaped_state
                    {
                        token.push('\\');
                    }
                    token.push(c);
                    state = escaped_state;
                }
            },
            _ => match next {
                None => break,
                Some(c) if WS.contains(c) => {
                    state = ' ';
                    if !token.is_empty() || quoted {
                        out.push(std::mem::take(&mut token));
                        quoted = false;
                    }
                }
                Some(c) if c == '\'' || c == '"' => {
                    quoted = true;
                    state = c;
                }
                Some('\\') => {
                    escaped_state = 'a';
                    state = '\\';
                }
                Some(c) => token.push(c),
            },
        }
    }
    if !token.is_empty() || quoted {
        out.push(token);
    }
    Ok(out)
}

/// Code-block lines, with backslash continuations joined and prompts cut.
fn _logical_lines(block: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    for raw in py_splitlines(block) {
        let mut line = py_strip(raw);
        if let Some(rest) = line.strip_prefix("$ ") {
            line = rest;
        }
        if let Some(head) = line.strip_suffix('\\') {
            buf.push_str(head);
            buf.push(' ');
            continue;
        }
        let joined = format!("{buf}{line}");
        let line = py_strip(&joined).to_string();
        buf.clear();
        if !line.is_empty() && !line.starts_with('#') {
            out.push(line);
        }
    }
    out
}

/// (start, heading path) for every heading, outermost first.
fn _sections(text: &str) -> Vec<(usize, Vec<String>)> {
    let mut path: Vec<(usize, String)> = Vec::new();
    let mut out = Vec::new();
    for m in HEADING.captures_iter(text) {
        let level = m[1].len();
        path.retain(|(lv, _)| *lv < level);
        path.push((level, m[2].to_string()));
        // Byte offsets, not Python's character offsets: they are only ever
        // compared with other offsets into the same text.
        out.push((m.get(0).map_or(0, |x| x.start()), path.iter().map(|(_, h)| h.clone()).collect()));
    }
    out
}

/// A section is setup when its heading path says so and nothing in it
/// says it is about something else.
fn _is_setup(path: &[String]) -> bool {
    let joined = path.join(" / ");
    SETUP_SECTION.is_match(&joined) && !OTHER_SECTION.is_match(&joined)
}

static AND_AND: LazyLock<Regex> = LazyLock::new(|| Regex::new(&py(r"\s*&&\s*")).unwrap());

/// The plugin's own files a setup command would execute, as written:
/// a program given by path (`./install`, `sudo ./x.sh`), the script a shell
/// is handed (`bash x.sh`, `sh -e x.sh`), or the one Python is
/// (`python3 setup.py`, or `python3 -m pkg.tool` when `pkg/tool.py` is the
/// plugin's). Err where that cannot be told for certain -- a line that does
/// not split, `-c` code, an option not known to take no argument, a script
/// read from stdin -- and then the command is not offered at all.
fn executed_files(part: &str, plugin_dir: &Path) -> Result<Vec<String>, ()> {
    let words = shlex_split(part).map_err(|_| ())?;
    let mut w: &[String] = &words;
    if w.first().is_some_and(|x| x == "sudo") {
        w = &w[1..];
    }
    let Some((prog, args)) = w.split_first() else { return Ok(Vec::new()) };
    if prog.contains('/') {
        return Ok(vec![prog.clone()]);
    }
    let shell = matches!(prog.as_str(), "bash" | "sh" | "zsh" | "dash");
    let python = prog == "python" || prog == "python3" || prog.starts_with("python3.");
    if !shell && !python {
        return Ok(Vec::new());
    }
    let mut i = 0;
    while let Some(a) = args.get(i) {
        if a == "--" {
            i += 1;
            break;
        }
        if !a.starts_with('-') || a == "-" {
            break;
        }
        if python && a == "-m" {
            let module = args.get(i + 1).ok_or(())?.replace('.', "/");
            // Python looks in the working directory first: a module of the
            // plugin's own is a file it runs. One it does not ship is not.
            for cand in [format!("{module}.py"), format!("{module}/__main__.py")] {
                if plugin_dir.join(&cand).exists() {
                    return Ok(vec![cand]);
                }
            }
            return Ok(Vec::new());
        }
        let known = if shell {
            matches!(a.as_str(), "-e" | "-x" | "-u" | "-v" | "-l" | "--login" | "--norc" | "--noprofile")
        } else {
            matches!(a.as_str(), "-u" | "-B" | "-E" | "-I" | "-s" | "-S" | "-O" | "-OO" | "-q" | "-b" | "-bb")
        };
        if !known {
            return Err(());
        }
        i += 1;
    }
    match args.get(i) {
        Some(f) if f != "-" => Ok(vec![f.clone()]),
        _ => Err(()),
    }
}

/// One file a command runs, as the confirmation will show it: its path in
/// the plugin, its text and the SHA-256 of its bytes. None for a file that
/// is not the plugin's own (missing, a folder, outside it once symlinks are
/// followed), cannot be read, or is not text (a NUL byte: a binary cannot be
/// shown, so it is not run either).
fn shown_file(plugin_dir: &Path, rel: &str) -> Option<(OsString, String, String)> {
    use sha2::{Digest, Sha256};
    let target = resolve(&plugin_dir.join(rel));
    let base = resolve(plugin_dir);
    if !(target.is_file() && target != base && target.starts_with(&base)) {
        return None;
    }
    let bytes = std::fs::read(&target).ok()?;
    if bytes.contains(&0) {
        return None;
    }
    let key = target.strip_prefix(&base).unwrap_or(&target).as_os_str().to_os_string();
    // As `read_text` reads it: all of it, since it runs in full (see
    // `describe` for the one limit on showing it).
    let body = String::from_utf8_lossy(&bytes).replace("\r\n", "\n").replace('\r', "\n");
    Some((key, body, hex::encode(Sha256::digest(&bytes))))
}

/// The SHA-256 of a file of the plugin's, as `shown_file` takes it; None
/// where it no longer resolves inside the plugin or cannot be read.
fn digest_now(plugin_dir: &Path, name: &std::ffi::OsStr) -> Option<String> {
    use sha2::{Digest, Sha256};
    let target = resolve(&plugin_dir.join(name));
    let base = resolve(plugin_dir);
    if !(target.is_file() && target != base && target.starts_with(&base)) {
        return None;
    }
    Some(hex::encode(Sha256::digest(std::fs::read(&target).ok()?)))
}

/// Err naming the first file the commands run that is no longer what the
/// confirmation showed: edited, replaced, moved or gone since the plan was
/// made. Checked again in the terminal before every command.
pub fn unchanged(p: &SetupPlan) -> Result<(), String> {
    for (name, _) in &p.scripts {
        let want = p.digests.iter().find(|(k, _)| k == name).map(|(_, d)| d.as_str());
        if want.is_none() || digest_now(&p.plugin_dir, name).as_deref() != want {
            return Err(esc(name));
        }
    }
    Ok(())
}

pub fn plan(plugin_dir: &Path) -> Result<SetupPlan, String> {
    let mut out = SetupPlan { plugin_dir: PathBuf::from(py_path_str(plugin_dir)), ..Default::default() };
    let Some(path) = _readme(plugin_dir) else {
        return Ok(out);
    };
    out.readme = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let text = read_text(&path)?;
    let sections = _sections(&text);

    let heading_at = |pos: usize| -> Vec<String> {
        sections
            .iter()
            .filter(|(start, _)| *start <= pos)
            .next_back()
            .map(|(_, p)| p.clone())
            .unwrap_or_default()
    };

    for fence in FENCE.captures_iter(&text) {
        let start = fence.get(0).map_or(0, |m| m.start());
        if !_is_setup(&heading_at(start)) {
            continue;
        }
        let block = fence.get(1).map_or("", |m| m.as_str());
        for line in _logical_lines(block) {
            for part in AND_AND.split(&line).map(py_strip) {
                if let Some(m) = PKG.captures(part) {
                    let toks = if !part.contains('"') { shlex_split(&m[1])? } else { Vec::new() };
                    for tok in toks {
                        if tok.starts_with('-') {
                            continue;
                        }
                        if PKG_NAME.is_match(&tok) && !out.packages.contains(&tok) {
                            out.packages.push(tok);
                        }
                    }
                    continue;
                }
                if NEVER.is_match(part) || CHORE.is_match(part) || !RUNNABLE.is_match(part) {
                    continue;
                }
                // Every file of the plugin's the command runs is shown in
                // full, or the command is not offered: only files the plugin
                // itself ships, never outside it, and never one unseen.
                let Ok(files) = executed_files(part, plugin_dir) else { continue };
                let Some(shown) = files.iter().map(|f| shown_file(plugin_dir, f)).collect::<Option<Vec<_>>>() else {
                    continue;
                };
                if !out.commands.iter().any(|c| c == part) {
                    out.commands.push(part.to_string());
                }
                for (key, body, digest) in shown {
                    if let Some(slot) = out.scripts.iter_mut().find(|(k, _)| *k == key) {
                        slot.1 = body;
                    } else {
                        out.scripts.push((key.clone(), body));
                    }
                    if let Some(slot) = out.digests.iter_mut().find(|(k, _)| *k == key) {
                        slot.1 = digest;
                    } else {
                        out.digests.push((key, digest));
                    }
                }
            }
        }
    }

    // Things only a person can do, found in setup sections' prose, with the
    // heading to read under. (Fences blanked byte for byte, so offsets still
    // line up with the headings'.)
    let prose = FENCE.replace_all(&text, |c: &regex::Captures| " ".repeat(c[0].len()));
    for (pattern, label) in BY_HAND.iter() {
        for start in by_hand_starts(pattern, &prose) {
            let place = heading_at(start);
            if _is_setup(&place) {
                out.by_hand.push((label.to_string(), place.last().cloned().unwrap_or_default()));
                break;
            }
        }
    }
    Ok(out)
}

/// The commands, as one shell script that stops at the first failure. Each
/// command is preceded by a check that every file shown is still the one
/// shown: a step before it (a Makefile, a build script) could rewrite one.
pub fn terminal_script(p: &SetupPlan) -> String {
    let mut lines = vec![format!("cd {}", shlex_quote(&py_path_str(&p.plugin_dir))), "set -e".to_string()];
    for cmd in &p.commands {
        for (name, digest) in &p.digests {
            let shown = esc(name);
            lines.push(format!(
                "test \"$(sha256sum < {})\" = {} || {{ echo; echo {}; exit 1; }}",
                shlex_quote(&shown),
                shlex_quote(&format!("{digest}  -")),
                shlex_quote(&format!("{shown} changed after it was shown -- setup stopped.")),
            ));
        }
        lines.push(format!("echo; echo '$ '{}", shlex_quote(cmd)));
        lines.push(cmd.clone());
    }
    lines.push("echo; echo 'Setup finished.'".to_string());
    lines.join("\n")
}

/// How much of one script the confirmation shows. A setup script is read in
/// full before it runs, so it is shown in full; only past this many
/// characters (far beyond any real one) is it cut, and then the cut says so,
/// how long the script is, and where to read the rest.
pub const SCRIPT_SHOWN_MAX: usize = 200_000;

/// The scripts `describe` does not show in full, as paths to open.
pub fn cut_scripts(p: &SetupPlan) -> Vec<PathBuf> {
    p.scripts
        .iter()
        .filter(|(_, body)| body.chars().count() > SCRIPT_SHOWN_MAX)
        .map(|(name, _)| p.plugin_dir.join(name))
        .collect()
}

/// One script as the confirmation shows it: every line, indented, or
/// (past `SCRIPT_SHOWN_MAX`) the lines that fit and a plain note of the cut.
fn shown_script(dir: &Path, name: &OsString, body: &str) -> String {
    let lines = py_splitlines(body);
    let mut out: Vec<String> = Vec::new();
    let mut used = 0;
    for (i, l) in lines.iter().enumerate() {
        used += l.chars().count() + 1;
        if used > SCRIPT_SHOWN_MAX {
            out.push(format!(
                "  … cut here: only {i} of its {} lines ({} KB) are shown, and all of it runs. \
                 Read the rest in {} before you run setup.",
                lines.len(),
                body.len().div_ceil(1024),
                esc(dir.join(name).as_os_str()),
            ));
            break;
        }
        out.push(format!("  {l}"));
    }
    out.join("\n")
}

/// Everything the plan would do, for the confirmation dialog.
pub fn describe(p: &SetupPlan) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !p.packages.is_empty() {
        parts.push(format!("Packages, installed by Almanac\n  {}", p.packages.join(" ")));
    }
    if !p.commands.is_empty() {
        parts.push(format!(
            "Run in a terminal, in the plugin's folder\n{}",
            p.commands.iter().map(|c| format!("  $ {c}")).collect::<Vec<_>>().join("\n")
        ));
    }
    for (name, body) in &p.scripts {
        parts.push(format!("Contents of {}\n{}", esc(name), shown_script(&p.plugin_dir, name, body)));
    }
    if !p.by_hand.is_empty() {
        parts.push(format!(
            "Left for you\n{}",
            p.by_hand
                .iter()
                .map(|(what, h)| {
                    let see = if h.is_empty() { String::new() } else { format!(" — see “{h}” in the README") };
                    format!("  • {what}{see}")
                })
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    let joined = parts.join("\n\n");
    if joined.is_empty() { "The README asks for nothing beyond the install.".to_string() } else { joined }
}


#[cfg(test)]
mod tests {
    use super::*;

    const README: &str = r#"# Plugin
## Requirements
You need an API key from the service.
```bash
sudo pacman -S --needed jq socat
yay -S some-helper-bin
```
## Installation
```bash
omarchy plugin add https://github.com/a/x --enable
make install PREFIX=~/.local
./scripts/setup.sh
curl -fsSL https://example.com/i.sh | sh
```
## Development
```bash
cargo test
make build
```
## Uninstall
```bash
./uninstall.sh
```
"#;

    fn plugin(d: &Path) -> PathBuf {
        let root = d.join("x");
        std::fs::create_dir_all(root.join("scripts")).unwrap();
        std::fs::write(root.join("README.md"), README).unwrap();
        std::fs::write(root.join("scripts/setup.sh"), "echo configuring\n").unwrap();
        std::fs::write(root.join("uninstall.sh"), "rm -rf everything\n").unwrap();
        root
    }

    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let d = std::env::temp_dir().join(format!(
            "almanac-plugin-setup-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn test_setup_is_read_only_from_setup_sections() {
        let d = tempdir();
        let plan = plan(&plugin(&d)).unwrap();
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(plan.packages, vec!["jq", "socat", "some-helper-bin"]);
        assert_eq!(plan.commands, vec!["make install PREFIX=~/.local", "./scripts/setup.sh"]);
        assert!(plan.scripts.iter().any(|(k, _)| k == "scripts/setup.sh"));
        assert_eq!(
            plan.by_hand,
            vec![("Credentials or a sign-in".to_string(), "Requirements".to_string())]
        );
    }

    #[test]
    fn test_resolve_follows_long_chains_and_stops_at_loops() {
        use std::os::unix::fs::symlink;
        let d = tempdir();
        // 45 links, more than the kernel's 40: realpath has no limit.
        std::fs::write(d.join("real.sh"), "").unwrap();
        for i in 0..45 {
            let to = if i < 44 { format!("l{}.sh", i + 1) } else { "real.sh".to_string() };
            symlink(to, d.join(format!("l{i}.sh"))).unwrap();
        }
        assert_eq!(resolve(&d.join("l0.sh")), resolve(&d).join("real.sh"));
        // A loop across folders stops where `a` is met again, so `..` lands
        // back in the top folder.
        std::fs::create_dir_all(d.join("d1")).unwrap();
        std::fs::create_dir_all(d.join("d2")).unwrap();
        symlink("d1/b", d.join("a")).unwrap();
        symlink("../d2/c", d.join("d1/b")).unwrap();
        symlink("../a", d.join("d2/c")).unwrap();
        let got = resolve(&d.join("a/../x.sh"));
        let base = resolve(&d);
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(got, base.join("x.sh"));
    }

    #[test]
    fn test_a_script_name_that_is_not_utf8_keeps_its_bytes() {
        use std::os::unix::ffi::OsStrExt;
        let d = tempdir();
        let root = d.join("p");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("README.md"), "# Install\n```\n./a.sh\n```\n").unwrap();
        let bad = std::ffi::OsStr::from_bytes(b"\xff.sh");
        std::fs::write(root.join(bad), "echo nu\n").unwrap();
        std::os::unix::fs::symlink(bad, root.join("a.sh")).unwrap();
        let p = plan(&root).unwrap();
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(p.scripts, vec![(bad.to_os_string(), "echo nu\n".to_string())]);
        assert!(describe(&p).contains("Contents of \\xff.sh"));
    }

    /// The whole script runs, so the whole script is shown: it used to be
    /// cut at 60 lines (or 6000 characters) without a word.
    #[test]
    fn test_the_whole_script_is_shown() {
        let d = tempdir();
        let root = d.join("p");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("README.md"), "# Install\n```\n./a.sh\n./big.sh\n```\n").unwrap();
        let script: String = (1..=500).map(|i| format!("echo step {i}\n")).collect();
        std::fs::write(root.join("a.sh"), &script).unwrap();
        let big: String = (1..=20_000).map(|i| format!("echo a rather long line, number {i}\n")).collect();
        std::fs::write(root.join("big.sh"), &big).unwrap();
        let p = plan(&root).unwrap();
        let shown = describe(&p);
        let _ = std::fs::remove_dir_all(&d);
        assert!(shown.contains("  echo step 1\n") && shown.contains("  echo step 500"), "a.sh shown in full");
        // Past the limit the cut is said, with the length and where to read it.
        assert!(shown.contains("echo a rather long line, number 1\n"));
        assert!(!shown.contains("number 20000"));
        assert!(shown.contains("of its 20000 lines"), "{}", &shown[shown.len() - 300..]);
        assert!(shown.contains("all of it runs"));
        assert!(shown.contains("big.sh before you run setup"));
        assert_eq!(cut_scripts(&p), vec![p.plugin_dir.join("big.sh")]);
    }

    #[test]
    fn test_never_offered() {
        let d = tempdir();
        let cmds = plan(&plugin(&d)).unwrap().commands.join(" ");
        let _ = std::fs::remove_dir_all(&d);
        for bad in ["curl", "uninstall", "cargo test", "make build", "omarchy plugin add"] {
            assert!(!cmds.contains(bad), "{bad} offered in {cmds}");
        }
    }

    /// A step runs only files the confirmation shows, whatever their name.
    #[test]
    fn test_every_file_a_step_runs_is_shown_or_the_step_is_not_offered() {
        let d = tempdir();
        let root = d.join("p");
        std::fs::create_dir_all(root.join("tools")).unwrap();
        std::fs::write(
            root.join("README.md"),
            concat!(
                "# Install\n```\n",
                "./install\n",
                "python3 setup.py install\n",
                "bash tools/prep.sh\n",
                "sh tools/go.sh\n",
                "python3 -m tools.helper\n",
                "python3 -m venv .venv\n",
                "./missing\n",
                "./bin/tool\n",
                "bash ../outside.sh\n",
                "bash -c 'echo hidden'\n",
                "python3 -W ignore sneaky.py\n",
                "make install\n",
                "```\n"
            ),
        )
        .unwrap();
        std::fs::write(root.join("install"), "echo installing\n").unwrap();
        std::fs::write(root.join("setup.py"), "print('setup')\n").unwrap();
        std::fs::write(root.join("tools/prep.sh"), "echo prep\n").unwrap();
        std::fs::write(root.join("tools/go.sh"), "echo go\n").unwrap();
        std::fs::write(root.join("tools/helper.py"), "print('helper')\n").unwrap();
        std::fs::write(root.join("sneaky.py"), "print('sneaky')\n").unwrap();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(root.join("bin/tool"), b"\x7fELF\x00\x00binary").unwrap();
        std::fs::write(d.join("outside.sh"), "echo outside\n").unwrap();
        let p = plan(&root).unwrap();
        let shown = describe(&p);
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(
            p.commands,
            [
                "./install",
                "python3 setup.py install",
                "bash tools/prep.sh",
                "sh tools/go.sh",
                "python3 -m tools.helper",
                "python3 -m venv .venv",
                "make install",
            ]
        );
        let names: Vec<String> = p.scripts.iter().map(|(k, _)| esc(k)).collect();
        assert_eq!(names, ["install", "setup.py", "tools/prep.sh", "tools/go.sh", "tools/helper.py"]);
        assert_eq!(p.digests.iter().map(|(k, _)| esc(k)).collect::<Vec<_>>(), names);
        for body in ["echo installing", "print('setup')", "echo prep", "echo go", "print('helper')"] {
            assert!(shown.contains(&format!("  {body}")), "{body} not shown");
        }
        for never in ["missing", "bin/tool", "outside", "hidden", "sneaky"] {
            assert!(!p.commands.iter().any(|c| c.contains(never)), "{never} offered");
        }
    }

    /// The plan is checked against the files again when it runs: one edited
    /// after the confirmation was shown is refused, by Almanac and by the
    /// terminal script before every command.
    #[test]
    fn test_a_file_changed_after_it_was_shown_is_not_run() {
        use sha2::{Digest, Sha256};
        let d = tempdir();
        let root = d.join("p");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("README.md"), "# Install\n```\nmake\n./install\n```\n").unwrap();
        std::fs::write(root.join("install"), "echo fine\n").unwrap();
        let p = plan(&root).unwrap();
        assert_eq!(unchanged(&p), Ok(()));
        let script = terminal_script(&p);
        let want = hex::encode(Sha256::digest(b"echo fine\n"));
        let guard = format!("test \"$(sha256sum < install)\" = '{want}  -' || {{");
        // Before each command, the make that could rewrite it included.
        assert_eq!(script.matches(&guard).count(), 2, "{script}");
        assert!(script.find(&guard) < script.find("make\n"));
        std::fs::write(root.join("install"), "echo evil\n").unwrap();
        assert_eq!(unchanged(&p), Err("install".to_string()));
        std::fs::remove_file(root.join("install")).unwrap();
        assert_eq!(unchanged(&p), Err("install".to_string()));
        // A plan whose files carry no digest is not trusted either.
        let bare = SetupPlan { digests: Vec::new(), ..p.clone() };
        let _ = std::fs::remove_dir_all(&d);
        assert!(unchanged(&bare).is_err());
    }

    #[test]
    fn test_the_terminal_script_stops_at_the_first_failure() {
        let d = tempdir();
        let script = terminal_script(&plan(&plugin(&d)).unwrap());
        let _ = std::fs::remove_dir_all(&d);
        let lines: Vec<&str> = script.lines().collect();
        assert!(lines[0].starts_with("cd "));
        assert_eq!(lines[1], "set -e");
    }
}
