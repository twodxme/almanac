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
//!   after the user has seen every line, the full contents of the file of
//!   the plugin's that a line names to run (`./install`, `bash x.sh`,
//!   `python3 setup.py`, `bun x.ts`), and those of the build files a build
//!   tool opens in the plugin (`make`'s Makefile, `npm install`'s
//!   package.json, `pip install .`'s pyproject.toml, `cargo build`'s
//!   build.rs, `go run`'s go.mod). A line whose file cannot be shown is not
//!   offered. Before every command the shown files are checked unchanged
//!   (and every path naming one still resolving, through any symlink, to
//!   the file shown), and every build file the tool would have opened but
//!   that was absent is checked still absent; either check failing stops
//!   the run. A name with a control character in it (a newline the
//!   terminal's `$(...)` would drop) is not offered at all, and every
//!   `python -m` runs with `-P`, so the plugin's folder is not where it finds
//!   `logging` or `shutil`. `systemctl --user` only enables,
//!   disables, starts, stops or restarts units, or reloads the daemon.
//!
//!   A line is offered only when bash would read it as plain words, joined
//!   by `&&` at most: no variables, globs, braces, `;`, pipes, redirections,
//!   subshells or comments (`literal_words`), and no `cd`. Each step then
//!   runs as exactly the words read from it, quoted, not as the README's
//!   text, so the shell cannot expand it into anything other than what the
//!   checks reasoned about.
//!
//!   The checks are made as each command starts; nothing watches files while
//!   it runs. Work an earlier step left running in the background (a daemon,
//!   a forked helper, a service `systemctl --user --now` started) can change
//!   a file after its check and before, or while, the next command reads it.
//!   Nothing here can stop that, short of not running the earlier step.
//!
//!   That is all it guarantees. What a shown file goes on to run -- another
//!   script, a module it imports, a Makefile recipe's `./x.sh`, a
//!   postinstall's `node gen.js`, the sources a compiler builds and a build
//!   script or `go generate` runs -- is not followed. Every build step (and
//!   Python or JavaScript program) runs such code, and the confirmation says
//!   so in as many words. A Python script's own folder stays first on its
//!   path, as Python puts it there, so a module beside it (even one named
//!   like the standard library's) is part of what it runs. Runners whose
//!   whole job is to run some program rather than install one -- `uv run`,
//!   `uv tool run`, `pipx run`, and the `exec`/`x`/`dlx` of npm, pnpm,
//!   yarn and bun -- are not offered at all.
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

use crate::i18n::tr;

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
// Commands worth running for the user, by the tool they start with: the
// pattern, and a line it takes (which `step_kind` must classify -- a test
// holds every entry to that, so a tool added here cannot slip past the
// files shown for it).
const TOOLS: &[(&str, &str)] = &[
    ("make", "make install"),
    ("cmake", "cmake -B build"),
    ("meson", "meson setup build"),
    ("ninja", "ninja -C build"),
    ("cargo", "cargo build --release"),
    ("go", "go run ."),
    ("npm", "npm install"),
    ("pnpm", "pnpm install"),
    ("yarn", "yarn"),
    ("bun", "bun x.ts"),
    ("uv", "uv sync"),
    ("pip", "pip install ."),
    ("just", "just setup"),
    ("pipx", "pipx install ."),
    ("python3?", "python3 x.py"),
    ("gcc", "gcc -o x x.c"),
    ("cc", "cc -o x x.c"),
    (r"systemctl\s+--user", "systemctl --user enable --now x.service"),
    (r"bash\s+\S+\.sh", "bash x.sh"),
    (r"sh\s+\S+\.sh", "sh x.sh"),
];
static RUNNABLE: LazyLock<Regex> = LazyLock::new(|| {
    // `(?:...)\b`: every tool ends in a word character; `\./[\w./-]+` may
    // end in either kind, so each case gets its own boundary.
    let tools: Vec<&str> = TOOLS.iter().map(|(pat, _)| *pat).collect();
    Regex::new(&format!(
        "{}(?:{}{}|{}(?:{PY_W}{}|[./-]{PY_W}))",
        py(r"^(?:sudo\s+)?"),
        py(&format!("(?:{})", tools.join("|"))),
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
    /// (path as a step names it, the absolute file it resolved to): `scripts`
    /// and `digests` are keyed by the file symlinks lead to, so a symlink
    /// (`./install`, or a folder on the way, `tools/`) repointed after the
    /// plan at another file of the plugin's would leave every digest true
    /// while the step runs the other file. Both checks resolve each path
    /// again and stop when it lands anywhere else.
    pub links: Vec<(String, String)>,
    /// Build files a step's tool would open but that were not there when the
    /// plan was made (paths relative to the plugin): a step before it (a
    /// postinstall, a build script) could write one -- a GNUmakefile, which
    /// make prefers to the Makefile shown, a `.cargo/config.toml`, an
    /// `.npmrc` -- and so they are checked still absent wherever `digests`
    /// are, and one that appears stops the run as a changed file does.
    pub absent: Vec<String>,
    /// The same for names a tool matches in any ASCII case (`just` takes
    /// `JustFile` as readily as `justfile`): no entry of the folder may be
    /// spelled like one, whatever its case.
    pub absent_any_case: Vec<String>,
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

/// One word of a setup step as bash reads it: its text, and where bash puts
/// `$HOME` for an unquoted tilde (`PREFIX=~/.local`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Word {
    text: String,
    home: Option<usize>,
}

static ASSIGN_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\A[A-Za-z_][A-Za-z0-9_]*=\z").unwrap());

/// The words of one step (a line, or one side of its `&&`) when bash would
/// read it as nothing but plain words -- the words `shlex_split` gives, which
/// is what every check here reasons about -- or None. The planner splits
/// words; the terminal runs bash, which also expands `$x`, `${x}`,
/// backticks, globs and braces, and reads `;`, `|`, `&`, redirections,
/// subshells and comments. A step that leans on any of that could run what
/// was never shown (`bash ${a}x.sh` shows a file named `${a}x.sh` and runs
/// `x.sh`), so it is not offered at all. Allowed: plain characters, single
/// quotes (nothing inside is special to bash either), double quotes with no
/// `$`, backtick or backslash in them, and one tilde bash expands that does
/// not name a file the planner opens: right after the `=` of a word shaped
/// like an assignment (`make install PREFIX=~/.local`), as bash expands it
/// there. A bare `~/x` or `~user` is refused: it could be the path a step
/// runs, and the planner reads paths in the plugin, not the home folder.
fn literal_words(part: &str) -> Option<Vec<Word>> {
    #[derive(PartialEq)]
    enum Q {
        Plain,
        Single,
        Double,
    }
    let mut words: Vec<Word> = Vec::new();
    // The word being read, and whether any of it was quoted so far.
    let mut cur: Option<(Word, bool)> = None;
    let mut state = Q::Plain;
    let chars: Vec<char> = part.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        // Control characters (a `\r` bash keeps in the word, shlex splits
        // at) and spaces other than ' ' and tab would show one thing and
        // run another.
        if (c.is_control() && c != '\t') || (py_isspace(c) && c != ' ' && c != '\t') {
            return None;
        }
        match state {
            Q::Single => match c {
                '\'' => state = Q::Plain,
                _ => cur.as_mut()?.0.text.push(c),
            },
            Q::Double => match c {
                '"' => state = Q::Plain,
                '$' | '`' | '\\' => return None,
                _ => cur.as_mut()?.0.text.push(c),
            },
            Q::Plain => match c {
                ' ' | '\t' => {
                    if let Some((w, _)) = cur.take() {
                        words.push(w);
                    }
                }
                '\'' | '"' => {
                    cur.get_or_insert_with(|| (Word { text: String::new(), home: None }, false)).1 = true;
                    state = if c == '\'' { Q::Single } else { Q::Double };
                }
                '~' => {
                    let (w, quoted) = cur.as_mut()?;
                    // The tilde prefix runs to the first unquoted slash; one
                    // with anything after it (`~user`, `~"/x"`) is not taken.
                    let ends = matches!(chars.get(i + 1), None | Some('/' | ' ' | '\t'));
                    if *quoted || w.home.is_some() || !ends || !ASSIGN_NAME.is_match(&w.text) {
                        return None;
                    }
                    w.home = Some(w.text.len());
                    w.text.push('~');
                }
                '$' | '`' | '*' | '?' | '[' | ']' | '{' | '}' | ';' | '|' | '&' | '<' | '>' | '(' | ')' | '!'
                | '#' | '\\' => return None,
                _ => cur.get_or_insert_with(|| (Word { text: String::new(), home: None }, false)).0.text.push(c),
            },
        }
    }
    if state != Q::Plain {
        return None;
    }
    words.extend(cur.map(|(w, _)| w));
    // Every check reads the line through shlex_split: the two must agree.
    let split = shlex_split(part).ok()?;
    (split.len() == words.len() && split.iter().zip(&words).all(|(s, w)| *s == w.text)).then_some(words)
}

/// A step's words, quoted for bash so that it runs exactly them: each in
/// single quotes (always: an unquoted `a=b` first would be an assignment,
/// an `if` a keyword), the tilde `literal_words` allows as `"$HOME"`, which
/// is what bash would have put there.
fn argv_line(words: &[Word]) -> String {
    let sq = |s: &str| format!("'{}'", s.replace('\'', "'\"'\"'"));
    words
        .iter()
        .map(|w| match w.home {
            Some(at) => format!("{}\"$HOME\"{}", sq(&w.text[..at]), sq(&w.text[at + 1..])),
            None => sq(&w.text),
        })
        .collect::<Vec<_>>()
        .join(" ")
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

/// What a setup line is, by the program it starts with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Runs one file of the plugin's, given by path or handed to a shell
    /// (`./install`, `bash x.sh`): that file is shown in full.
    Script,
    /// A build tool, compiler or interpreter (`make`, `cargo run`, `go run`,
    /// `npm install`, `python3 x.py`, `bun x.ts`): the file it is handed and
    /// the build files it opens are shown, but it goes on to run code of the
    /// plugin's that is not -- recipes, install scripts, build scripts, the
    /// sources it compiles, the modules it imports.
    Build,
    /// `systemctl --user`: starts what is already installed.
    Service,
}

/// The program a line runs and its arguments, `sudo` taken off.
fn program(words: &[String]) -> Option<(&str, &[String])> {
    let w = match words.split_first() {
        Some((s, rest)) if s == "sudo" => rest,
        _ => words,
    };
    w.split_first().map(|(p, a)| (p.as_str(), a))
}

/// Every program `TOOLS` offers, and a path, has its own arm: one with none
/// is not offered at all, rather than run with nothing shown for it.
fn step_kind(part: &str) -> Option<Kind> {
    let words = shlex_split(part).ok()?;
    let (prog, args) = program(&words)?;
    match prog {
        // `systemctl --user enable ./x.service` links a unit file by path:
        // a file of the plugin's, run as a service with nothing shown. Only
        // units already installed, named without a path, are started.
        "systemctl" if args.iter().any(|a| a.contains('/')) => None,
        "systemctl" if !service_line(args) => None,
        p if p.contains('/') => Some(Kind::Script),
        "bash" | "sh" | "zsh" | "dash" => Some(Kind::Script),
        "make" | "cmake" | "meson" | "ninja" | "cargo" | "go" | "npm" | "pnpm" | "yarn" | "bun" | "uv" | "pip"
        | "pipx" | "just" | "gcc" | "cc" => Some(Kind::Build),
        p if is_python(p) => Some(Kind::Build),
        "systemctl" => Some(Kind::Service),
        _ => None,
    }
}

/// Every line of a .python-version(s) file a version request and nothing
/// else: `3.12`, `3.12.1`, `3.13t`, `pypy3.10`, `cpython-3.12`,
/// `cpython@3.12`, blank lines and `#` comments. A path or a bare word (a
/// folder of the plugin's with a bin/python in it) is not.
fn python_versions_only(body: &str) -> bool {
    let request = regex::Regex::new(r"^(?:[A-Za-z]+[-@]?)?[0-9]+(?:\.[0-9]+){0,2}[a-z0-9+]*$").expect("constant regex");
    body.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).all(|l| request.is_match(l))
}

/// A `systemctl --user` line that only turns units already installed on or
/// off: one of a few verbs, unit names, `--user` and `--now`. Any other verb
/// or option could do what nothing shown says -- `edit` opens an editor,
/// `set-environment` changes every later service's environment,
/// `--root=` or `-H` points it at another tree or machine -- so it is not
/// offered: being free of a `/` was not enough.
fn service_line(args: &[String]) -> bool {
    const VERBS: &[&str] = &["enable", "disable", "start", "stop", "restart", "daemon-reload"];
    let (opts, words): (Vec<&String>, Vec<&String>) = args.iter().partition(|a| a.starts_with('-'));
    if !opts.iter().all(|o| *o == "--user" || *o == "--now") || !opts.iter().any(|o| *o == "--user") {
        return false;
    }
    match words.split_first() {
        Some((verb, units)) if *verb == "daemon-reload" => units.is_empty() && !opts.iter().any(|o| *o == "--now"),
        // A quoted glob is plain words, and `stop '*'` matches every unit.
        Some((verb, units)) => {
            VERBS.contains(&verb.as_str()) && !units.is_empty() && units.iter().all(|u| !u.contains(['*', '?', '[', ']']))
        }
        None => false,
    }
}

/// Python's options that take no value, which may come before `-m` or the
/// script.
fn py_flag(a: &str) -> bool {
    matches!(a, "-u" | "-B" | "-E" | "-I" | "-s" | "-S" | "-O" | "-OO" | "-q" | "-b" | "-bb")
}

/// `python3 [flags] -m module args...`: the module and its arguments.
fn py_module(args: &[String]) -> Option<(&str, &[String])> {
    let mut i = 0;
    while let Some(a) = args.get(i) {
        if a == "-m" {
            return Some((args.get(i + 1)?.as_str(), &args[(i + 2).min(args.len())..]));
        }
        if !py_flag(a) {
            return None;
        }
        i += 1;
    }
    None
}

/// What `python3 -m a.b` would load from the working directory, which
/// Python searches first: the packages on the way (their `__init__.py`
/// runs on import) and the module itself, as a file or a package. Even
/// `-m venv` or `-m pip` is the plugin's own if it ships `venv.py`.
fn python_module_files(module: &str) -> Vec<String> {
    let parts: Vec<&str> = module.split('.').collect();
    let mut out = Vec::new();
    for i in 1..parts.len() {
        out.push(format!("{}/__init__.py", parts[..i].join("/")));
    }
    let m = parts.join("/");
    out.extend([format!("{m}.py"), format!("{m}/__init__.py"), format!("{m}/__main__.py")]);
    out
}

/// The file `bun x.ts` or `bun run x.ts` runs, when the word names a file
/// rather than one of bun's own commands or a package.json script.
fn bun_file(args: &[String], plugin_dir: &Path) -> Option<String> {
    const OWN: &[&str] = &[
        "install", "i", "add", "a", "remove", "rm", "update", "upgrade", "x", "build", "test", "init",
        "create", "link", "unlink", "pm", "outdated", "publish", "patch", "exec", "repl", "info", "audit", "why",
    ];
    let mut words = args.iter().filter(|a| !a.starts_with('-'));
    let mut w = words.next()?;
    if w == "run" {
        w = words.next()?;
    } else if OWN.contains(&w.as_str()) {
        return None;
    }
    const EXT: &[&str] = &[".js", ".ts", ".mjs", ".cjs", ".jsx", ".tsx", ".mts", ".cts"];
    (w.contains('/') || EXT.iter().any(|e| w.ends_with(e)) || plugin_dir.join(w).is_file()).then(|| w.clone())
}

/// The files uv reads the Python to use from. Shown and checked like any
/// build file; one that names a path (a `/` in it) rather than a version is
/// refused outright, since the program it names would run unseen.
const PYTHON_VERSION_FILES: [&str; 2] = [".python-version", ".python-versions"];

/// The names `just` looks for, in any ASCII case.
const JUSTFILES: [&str; 2] = ["justfile", ".justfile"];

/// The build files a configure step writes for a later step to read:
/// `meson setup DIR`'s build.ninja, `cmake -B DIR`'s Makefile, `./configure`'s
/// Makefile. A later `ninja -C DIR` or `make` reads one that was not there
/// when the plan was made, and that is no file appearing unseen: it is the
/// output of a step shown, from build files shown. (Normalised paths.)
///
/// `./configure` counts only as the plugin's top-level one, which writes the
/// top-level Makefile: any script that happens to be named `configure`
/// (`./tools/configure`) used to excuse ./Makefile and ./build.ninja, so a
/// file nothing shown writes could appear unchecked before `make`.
fn generated_by(part: &str, files: &[String]) -> Vec<String> {
    if files.iter().any(|f| py_path_str(Path::new(f)) == "configure") {
        return vec!["Makefile".to_string()];
    }
    let dir = (|| {
        let words = shlex_split(part).ok()?;
        let (prog, args) = program(&words)?;
        match prog {
            "meson" => {
                let words: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
                match words.as_slice() {
                    [s, d, ..] if *s == "setup" => Some(d.to_string()),
                    _ => None,
                }
            }
            "cmake" => {
                if args.iter().any(|a| a == "--build" || a == "--install" || a == "-E") {
                    return None;
                }
                let mut b = None;
                scan_args(args, "SBHGTACDUWP", "", &[], |opt, v| {
                    if opt == "B" {
                        b = Some(v.to_string());
                    }
                })
                .ok()?;
                // Without -B, cmake configures into the folder it runs in.
                Some(b.unwrap_or_else(|| ".".into()))
            }
            _ => None,
        }
    })();
    dir.map(|d| ["Makefile", "build.ninja"].map(|n| py_path_str(Path::new(&under(&d, n)))).to_vec())
        .unwrap_or_default()
}

/// The plugin's own file a setup command names to execute, which must be
/// there and shown: a program given by path (`./install`, `sudo ./x.sh`),
/// the script a shell is handed (`bash x.sh`, `sh -e x.sh`), the one Python
/// is (`python3 setup.py`; `python3 -m pkg.tool` loads what
/// `build_files` lists) or bun (`bun x.ts`). Err where that cannot be told
/// for certain -- a line that does not split, `-c` code, an option not known
/// to take no argument, a script read from stdin -- and then the command is
/// not offered at all.
fn executed_files(part: &str, plugin_dir: &Path) -> Result<Vec<String>, ()> {
    let words = shlex_split(part).map_err(|_| ())?;
    let Some((prog, args)) = program(&words) else { return Ok(Vec::new()) };
    if prog.contains('/') {
        return Ok(vec![prog.to_string()]);
    }
    if prog == "bun" {
        return Ok(bun_file(args, plugin_dir).into_iter().collect());
    }
    let shell = matches!(prog, "bash" | "sh" | "zsh" | "dash");
    let python = is_python(prog);
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
            // Python looks in the working directory first: the module files
            // the plugin ships (or could by then) are `build_files`' to list,
            // shown when there and checked still absent when not.
            args.get(i + 1).ok_or(())?;
            return Ok(Vec::new());
        }
        let known = if shell {
            matches!(a.as_str(), "-e" | "-x" | "-u" | "-v" | "-l" | "--login" | "--norc" | "--noprofile")
        } else {
            py_flag(a)
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

fn is_python(prog: &str) -> bool {
    prog == "python" || prog == "python3" || prog.starts_with("python3.")
}

/// `name` inside `dir`, as a path relative to the plugin ("." is the plugin).
fn under(dir: &str, name: &str) -> String {
    if name.starts_with('/') || dir == "." || dir.is_empty() {
        name.to_string()
    } else {
        format!("{}/{name}", dir.trim_end_matches('/'))
    }
}

/// What a Python build reads from a project folder, and runs: setup.py
/// itself, and pyproject.toml / setup.cfg, which name the build backend
/// (possibly one of the project's own) and what it runs.
fn python_project(dir: &str) -> Vec<String> {
    ["setup.py", "pyproject.toml", "setup.cfg"].iter().map(|n| under(dir, n)).collect()
}

/// The words of a command line that are not options, with `on(option,
/// value)` called for each option that takes a value: a short one among
/// `takes` (its value the rest of the cluster or the next word), or a long
/// one in `long` (`--name=value` or `--name value`). Short options among
/// `optional` take the rest of their cluster, if any. Err when an option's
/// value is missing.
fn scan_args<'a>(
    args: &'a [String],
    takes: &str,
    optional: &str,
    long: &[&str],
    mut on: impl FnMut(&str, &'a str),
) -> Result<Vec<&'a str>, ()> {
    let mut words = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--" {
            words.extend(it.map(String::as_str));
            break;
        }
        if let Some(name) = a.strip_prefix("--") {
            match name.split_once('=') {
                Some((n, v)) if long.contains(&n) => on(n, v),
                None if long.contains(&name) => on(name, it.next().ok_or(())?),
                _ => {}
            }
            continue;
        }
        let Some(cluster) = a.strip_prefix('-').filter(|c| !c.is_empty()) else {
            words.push(a.as_str());
            continue;
        };
        for (i, c) in cluster.char_indices() {
            let rest = &cluster[i + c.len_utf8()..];
            if takes.contains(c) {
                let value = if rest.is_empty() { it.next().ok_or(())?.as_str() } else { rest };
                on(c.encode_utf8(&mut [0; 4]), value);
                break;
            }
            if optional.contains(c) {
                break;
            }
        }
    }
    Ok(words)
}

/// A pip-style requirement that names a folder or file of the plugin's
/// (`.`, `./sub`, `.[extra]`, a folder that exists) rather than a package
/// on an index or a URL.
fn local_spec(spec: &str, plugin_dir: &Path) -> Option<String> {
    let path = spec.split_once('[').map_or(spec, |(p, _)| p);
    if path.is_empty() || path.contains("://") || path.starts_with("git+") {
        return None;
    }
    let pathy = path.starts_with('.') || path.starts_with('/') || path.contains('/');
    (pathy || plugin_dir.join(path).is_dir()).then(|| path.to_string())
}

/// pip / pipx / `uv pip`: for `install` (and `wheel`, `download`), the
/// project files of every local folder installed (`.`, `-e .`), a local
/// archive itself, and every requirements or constraints file named.
fn pip_files(args: &[String], plugin_dir: &Path) -> Result<Vec<String>, ()> {
    let mut local: Vec<String> = Vec::new();
    let mut lists: Vec<String> = Vec::new();
    let long = [
        "editable", "requirement", "constraint", "index-url", "extra-index-url", "find-links",
        "target", "prefix", "root", "src", "python", "platform", "python-version",
        "implementation", "abi", "config-settings", "global-option", "pip-args", "suffix", "spec",
    ];
    let words = scan_args(args, "ercitf", "", &long, |opt, v| match opt {
        "e" | "editable" => local.extend(local_spec(v, plugin_dir)),
        "r" | "requirement" | "c" | "constraint" => lists.push(v.to_string()),
        _ => {}
    })?;
    let Some((sub, specs)) = words.split_first() else { return Ok(Vec::new()) };
    if !matches!(*sub, "install" | "wheel" | "download" | "run" | "inject" | "sync") {
        return Ok(Vec::new());
    }
    local.extend(specs.iter().filter_map(|s| local_spec(s, plugin_dir)));
    let mut out = lists;
    for p in local {
        if plugin_dir.join(&p).is_dir() {
            out.extend(python_project(&p));
        } else {
            out.push(p);
        }
    }
    Ok(out)
}

/// The build files a build step reads from the plugin and runs code from,
/// which the confirmation must show beside the line itself: `make` runs the
/// Makefile, `npm install` the package.json's install scripts, `pip install
/// .` setup.py or the backend pyproject.toml names, `cargo build` build.rs,
/// and so on. Only the files the tool itself opens (paths relative to the
/// plugin, present or not: one that is there is shown and checked
/// unchanged, one that is not is checked still absent, since an earlier
/// step could write it); what those files include or run in turn is not
/// followed. Err where the tool is pointed somewhere this cannot follow
/// (`npm --prefix`, `just -f`, `uv --directory`, `go -C`), and for a program
/// with no arm here: then the step is not offered.
fn build_files(part: &str, plugin_dir: &Path) -> Result<Vec<String>, ()> {
    let words = shlex_split(part).map_err(|_| ())?;
    let Some((mut prog, mut args)) = program(&words) else { return Ok(Vec::new()) };
    let mut out: Vec<String> = Vec::new();
    if is_python(prog) {
        let Some((module, rest)) = py_module(args) else { return Ok(Vec::new()) };
        out.extend(python_module_files(module));
        match module {
            "pip" => {
                prog = "pip";
                args = rest;
            }
            "build" => {
                out.extend(python_project("."));
                return Ok(out);
            }
            _ => return Ok(out),
        }
    }
    // Options that point a tool at another folder or file than the one
    // whose build files are shown.
    let elsewhere = |names: &[&str]| {
        args.iter().any(|a| names.iter().any(|n| a == n || (n.starts_with("--") && a.starts_with(&format!("{n}=")))))
    };
    match prog {
        "make" => {
            // GNU make reads GNUmakefile, makefile or Makefile (or the -f
            // ones), after changing into -C's folder.
            let (mut dir, mut files) = (String::from("."), Vec::new());
            let long = ["directory", "file", "makefile", "include-dir", "old-file", "assume-old", "what-if", "new-file", "assume-new", "eval"];
            scan_args(args, "CfIoW", "jlO", &long, |opt, v| match opt {
                "C" | "directory" => dir = under(&dir, v),
                "f" | "file" | "makefile" => files.push(v.to_string()),
                _ => {}
            })?;
            if files.is_empty() {
                files = ["GNUmakefile", "makefile", "Makefile"].map(String::from).to_vec();
            }
            out.extend(files.iter().map(|f| under(&dir, f)));
        }
        "ninja" => {
            let (mut dir, mut file) = (String::from("."), String::from("build.ninja"));
            scan_args(args, "Cfjkldtw", "", &[], |opt, v| match opt {
                "C" => dir = under(&dir, v),
                "f" => file = v.to_string(),
                _ => {}
            })?;
            out.push(under(&dir, &file));
        }
        "meson" => out.extend(["meson.build", "meson.options", "meson_options.txt"].map(String::from)),
        "cmake" => {
            if args.iter().any(|a| a == "--build" || a == "--install" || a == "-E") {
                return Ok(Vec::new());
            }
            let mut src: Option<String> = None;
            let words = scan_args(args, "SBHGTACDUWP", "", &[], |opt, v| match opt {
                "S" | "H" => src = Some(v.to_string()),
                // -C preloads a script, -P runs one.
                "C" | "P" => out.push(v.to_string()),
                _ => {}
            })?;
            let src = src.or_else(|| words.first().map(|s| s.to_string())).unwrap_or_else(|| ".".into());
            // Presets can set the compiler, or any cache variable, to a
            // program of the plugin's.
            out.extend(["CMakeLists.txt", "CMakePresets.json", "CMakeUserPresets.json"].map(|n| under(&src, n)));
        }
        "cargo" => {
            if elsewhere(&["-C"]) {
                return Err(());
            }
            // .cargo/config.toml can name a rustc wrapper or a runner to run,
            // and rust-toolchain.toml a toolchain by path, whose cargo runs.
            let project = |dir: &str| {
                ["Cargo.toml", "build.rs", ".cargo/config.toml", ".cargo/config", "rust-toolchain.toml", "rust-toolchain"]
                    .map(|n| under(dir, n))
            };
            out.extend(project("."));
            let mut dirs = Vec::new();
            scan_args(args, "pjFZ", "", &["manifest-path", "path"], |opt, v| match opt {
                "path" => dirs.push(v.to_string()),
                _ => {
                    let parent = Path::new(v).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
                    out.push(v.to_string());
                    dirs.push(if parent.is_empty() { ".".into() } else { parent });
                }
            })?;
            for d in dirs {
                out.extend(project(&d));
            }
        }
        "npm" | "pnpm" | "yarn" | "bun" => {
            if elsewhere(&["--prefix", "-C", "--dir", "--cwd", "--filter", "-F", "--workspace", "-w"]) {
                return Err(());
            }
            // Their fetch-and-run commands run a package from the registry --
            // code that is not even in the plugin -- rather than set it up.
            const RUNNERS: &[&str] = &["exec", "x", "dlx"];
            if args.iter().find(|a| !a.starts_with('-')).is_some_and(|a| RUNNERS.contains(&a.as_str())) {
                return Err(());
            }
            // bun's preload and config options name files to run or read.
            if prog == "bun" && elsewhere(&["-r", "--preload", "-c", "--config"]) {
                return Err(());
            }
            out.push("package.json".into());
            // Their own config, which can name a script shell, a hook file
            // or a yarn release to run in place of the tool.
            out.extend(
                match prog {
                    "npm" => &[".npmrc"][..],
                    "pnpm" => &[".npmrc", ".pnpmfile.cjs", "pnpm-workspace.yaml"][..],
                    "yarn" => &[".yarnrc", ".yarnrc.yml"][..],
                    _ => &["bunfig.toml"][..],
                }
                .iter()
                .map(|s| s.to_string()),
            );
        }
        // `pipx run` runs a program rather than installing one: whatever it
        // names (a script of the plugin's, a package's entry point) is not a
        // setup step, and nothing here could show what it runs.
        "pipx" if args.iter().find(|a| !a.starts_with('-')).is_some_and(|a| a == "run") => return Err(()),
        "pip" | "pipx" => out.extend(pip_files(args, plugin_dir)?),
        "uv" => {
            if elsewhere(&["--directory", "--project"]) {
                return Err(());
            }
            // What every uv command reads in the folder it runs in: its
            // config (uv.toml, pyproject's [tool.uv]) and the Python to use
            // (.python-version(s)), which may name a path -- `./fakepy` made
            // uv run a program of the plugin's (see `PYTHON_VERSION_FILES`).
            out.extend(["uv.toml", "pyproject.toml", PYTHON_VERSION_FILES[0], PYTHON_VERSION_FILES[1]].map(String::from));
            let words = scan_args(args, "pw", "", &["python", "with", "index-url", "extra-index-url", "find-links", "from"], |_, _| {})?;
            match words.as_slice() {
                // `uv run` and `uv tool run` run a program -- `uv run python
                // evil.py`, `uv run python -m venv` importing the plugin's own
                // venv.py -- rather than install anything: not offered.
                ["run", ..] | ["tool", "run", ..] => return Err(()),
                ["pip", ..] | ["tool", ..] => {
                    let at = args.iter().position(|a| a == words[0]).map_or(args.len(), |i| i + 1);
                    out.extend(pip_files(&args[at..], plugin_dir)?);
                }
                ["sync" | "build" | "lock" | "install", ..] => out.extend(python_project(".")),
                _ => {}
            }
        }
        "just" => {
            if elsewhere(&["-f", "--justfile", "-d", "--working-directory"]) {
                return Err(());
            }
            // just matches its names in any ASCII case (`JustFile` too), so
            // the folder is read for them; the absent ones are checked in
            // any case as well (`absent_any_case`).
            let mut found: Vec<String> = std::fs::read_dir(plugin_dir)
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| JUSTFILES.iter().any(|j| n.eq_ignore_ascii_case(j)))
                .collect();
            found.sort();
            out.extend(found);
        }
        "go" => {
            if elsewhere(&["-C"]) {
                return Err(());
            }
            // go.mod and go.work name the module, its replacements (folders
            // of the plugin's) and the toolchain; the .go files named on the
            // line are what `go run x.go` compiles first.
            out.extend(["go.mod", "go.work"].map(String::from));
            out.extend(args.iter().filter(|a| a.ends_with(".go") && !a.starts_with('-')).cloned());
        }
        // A compiler opens the sources it is given, which it does not run.
        "gcc" | "cc" => {}
        // systemctl starts what is installed; shells and paths run the one
        // file `executed_files` names, and Python's -m was handled above.
        "systemctl" | "bash" | "sh" | "zsh" | "dash" => {}
        p if p.contains('/') || is_python(p) => {}
        _ => return Err(()),
    }
    Ok(out)
}

/// One file a command runs, as the confirmation will show it: its path in
/// the plugin, its text, the SHA-256 of its bytes, and the absolute path it
/// resolved to (`SetupPlan::links`). None for a file that is not the
/// plugin's own (missing, a folder, outside it once symlinks are followed),
/// cannot be read, or is not text (a NUL byte: a binary cannot be shown, so
/// it is not run either).
fn shown_file(plugin_dir: &Path, rel: &str) -> Option<(OsString, String, String, String)> {
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
    Some((key, body, hex::encode(Sha256::digest(&bytes)), esc(target.as_os_str())))
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

/// Whether the folder `name` is in (relative to the plugin) has an entry
/// spelled like `name`'s last part in any ASCII case. True where the folder
/// cannot be read: then it cannot be shown to have none.
fn any_case_present(plugin_dir: &Path, name: &str) -> bool {
    let rel = Path::new(name);
    let dir = plugin_dir.join(rel.parent().unwrap_or(Path::new("")));
    let Some(want) = rel.file_name().and_then(|n| n.to_str()) else { return true };
    match std::fs::read_dir(&dir) {
        Ok(entries) => entries.flatten().any(|e| e.file_name().to_str().is_some_and(|n| n.eq_ignore_ascii_case(want))),
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
    }
}

/// Err naming the first file the commands run that is no longer what the
/// confirmation showed: edited, replaced, moved or gone since the plan was
/// made -- or a build file that was absent and is now there. Checked again
/// in the terminal before every command.
pub fn unchanged(p: &SetupPlan) -> Result<(), String> {
    // Each path a step names still leads to the file that was shown.
    for (written, target) in &p.links {
        if esc(resolve(&p.plugin_dir.join(written)).as_os_str()) != *target {
            return Err(written.clone());
        }
    }
    for (name, _) in &p.scripts {
        let want = p.digests.iter().find(|(k, _)| k == name).map(|(_, d)| d.as_str());
        if want.is_none() || digest_now(&p.plugin_dir, name).as_deref() != want {
            return Err(esc(name));
        }
    }
    // A dangling symlink is there too: what it points at could be written.
    if let Some(name) = p.absent.iter().find(|n| p.plugin_dir.join(n).symlink_metadata().is_ok()) {
        return Err(name.clone());
    }
    if let Some(name) = p.absent_any_case.iter().find(|n| any_case_present(&p.plugin_dir, n)) {
        return Err(name.clone());
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
            .iter().rfind(|(start, _)| *start <= pos)
            .map(|(_, p)| p.clone())
            .unwrap_or_default()
    };

    // Build files a configure step already offered will write.
    let mut generated: Vec<String> = Vec::new();
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
                // A program with no case of its own is not offered: it would
                // run with nothing shown for it.
                if step_kind(part).is_none() {
                    continue;
                }
                // Only a step bash reads as plain words, which the terminal
                // then runs as exactly those words (`literal_words`).
                let Some(words) = literal_words(part) else { continue };
                // A `cd` (bare, `pushd`, `popd`, anywhere) would move every
                // later check and command out of the plugin's folder.
                if words.iter().any(|w| matches!(w.text.as_str(), "cd" | "pushd" | "popd")) {
                    continue;
                }
                // The file of the plugin's the command names to run is shown
                // in full, or the command is not offered: only a file the
                // plugin itself ships, never outside it.
                let Ok(files) = executed_files(part, plugin_dir) else { continue };
                // And the build files the step's tool opens (a Makefile,
                // package.json's scripts, setup.py, build.rs, go.mod): one
                // that is there is shown and checked like a script; one that
                // is not is recorded, and checked still absent, since a step
                // before could write it -- unless an earlier configure step
                // shown here writes it (`generated_by`).
                let Ok(read) = build_files(part, plugin_dir) else { continue };
                let base = resolve(plugin_dir);
                let mut present = Vec::new();
                let mut missing = Vec::new();
                let mut inside = true;
                for f in read {
                    if plugin_dir.join(&f).symlink_metadata().is_ok() {
                        present.push(f);
                    } else if generated.contains(&py_path_str(Path::new(&f))) {
                        // written by a configure step shown before this one
                    } else if resolve(&plugin_dir.join(&f)).starts_with(&base) {
                        missing.push(f);
                    } else {
                        // `make -C ..`: what the tool would read is not the plugin's.
                        inside = false;
                    }
                }
                if !inside {
                    continue;
                }
                let any_case: Vec<&str> = match shlex_split(part).ok().as_deref().and_then(program) {
                    Some(("just", _)) => JUSTFILES.iter().copied().filter(|j| !any_case_present(plugin_dir, j)).collect(),
                    _ => Vec::new(),
                };
                let Some(shown) = files
                    .iter()
                    .chain(&present)
                    .map(|f| shown_file(plugin_dir, f).map(|s| (f, s)))
                    .collect::<Option<Vec<_>>>()
                else {
                    continue;
                };
                // A name with a control character in it cannot be checked
                // in the terminal as surely as here (`$(...)` drops trailing
                // newlines; see `exact_output`), nor shown as it is: the
                // step is not offered. The path a step names, the file it
                // leads to, and every build file held absent.
                let ctl = |s: &str| s.chars().any(char::is_control);
                if shown.iter().any(|(f, (key, _, _, target))| ctl(f) || ctl(&esc(key)) || ctl(target))
                    || missing.iter().any(|f| ctl(f))
                {
                    continue;
                }
                // A .python-version that names anything but a version: uv
                // runs a path (`./fakepy`) or a folder's bin/python (`d`) as
                // its Python.
                if shown.iter().any(|(f, (key, body, _, _))| {
                    let name = Path::new(key).file_name().and_then(|n| n.to_str()).unwrap_or_default();
                    (PYTHON_VERSION_FILES.contains(&name) || PYTHON_VERSION_FILES.iter().any(|v| f.ends_with(v)))
                        && !python_versions_only(body)
                }) {
                    continue;
                }
                if !out.commands.iter().any(|c| c == part) {
                    out.commands.push(part.to_string());
                }
                generated.extend(generated_by(part, &files));
                for f in missing {
                    if !out.absent.contains(&f) {
                        out.absent.push(f);
                    }
                }
                for f in any_case {
                    if !out.absent_any_case.iter().any(|a| a == f) {
                        out.absent_any_case.push(f.to_string());
                    }
                }
                for (written, (key, body, digest, target)) in shown {
                    if !out.links.iter().any(|(w, _)| w == written) {
                        out.links.push((written.clone(), target));
                    }
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

/// `name` as a bash glob that matches it in any ASCII case.
fn any_case_glob(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphabetic() {
                format!("[{}{}]", c.to_ascii_lowercase(), c.to_ascii_uppercase())
            } else {
                shlex_quote(&c.to_string())
            }
        })
        .collect()
}

/// A check that `cmd` printed exactly `want` and the one newline it ends its
/// output with, else `fail` is said and the script stops. `$(...)` drops
/// every trailing newline, so `"$(realpath -e -- ./install)"` read a link
/// repointed at a file named `real.sh\n` as `real.sh`, the file shown: a `x`
/// printed after the output keeps them, and only the one newline is taken
/// off. A command that fails prints no `x`, and nothing then matches.
fn exact_output(cmd: &str, want: &str, fail: &str) -> String {
    format!(
        "got=$({cmd} && printf x) && got=${{got%x}} && got=${{got%$'\\n'}} && test \"$got\" = {} || {{ echo; echo {fail}; exit 1; }}",
        shlex_quote(want)
    )
}

/// `python -m X` puts the folder it runs in first on sys.path, so a
/// plugin's own logging.py or shutil.py would be what venv or pip imports,
/// unseen. `-P` (3.11+) leaves it off for that interpreter only: a flag, not
/// PYTHONSAFEPATH in the environment, because the build backends pip starts
/// would inherit the variable, and a setup.py importing the module beside
/// it (its own version file, say) would then fail to build. A script run by
/// path (`python3 setup.py`) is shown, and keeps its own folder.
fn python_m_safe(words: Vec<Word>) -> Vec<Word> {
    let at = usize::from(words.first().is_some_and(|w| w.text == "sudo"));
    let is_m = words.get(at).is_some_and(|w| is_python(&w.text))
        && words[at + 1..].iter().take_while(|w| w.text.starts_with('-')).any(|w| w.text == "-m");
    if !is_m {
        return words;
    }
    let mut out = words;
    out.insert(at + 1, Word { text: "-P".to_string(), home: None });
    out
}

/// The commands, as one shell script that stops at the first failure. Each
/// command is preceded by a check that every file shown is still the one
/// shown, and that every build file absent then is still absent: a step
/// before it (a Makefile, a build script, a postinstall) could write one.
///
/// A command runs as the words `literal_words` read from it, each quoted, not
/// as the README's text: bash then has nothing left to expand, so what runs
/// is the argv the checks reasoned about. A command that no longer reads as
/// plain words stops the script there.
pub fn terminal_script(p: &SetupPlan) -> String {
    let mut lines = vec![
        format!("cd {}", shlex_quote(&py_path_str(&p.plugin_dir))),
        "set -e".to_string(),
    ];
    let stop = |shown: &str| shlex_quote(&format!("{shown} changed after it was shown -- setup stopped."));
    for cmd in &p.commands {
        for (written, target) in &p.links {
            lines.push(exact_output(&format!("realpath -e -- {}", shlex_quote(written)), target, &stop(written)));
        }
        for (name, digest) in &p.digests {
            let shown = esc(name);
            lines.push(exact_output(&format!("sha256sum < {}", shlex_quote(&shown)), &format!("{digest}  -"), &stop(&shown)));
        }
        for name in &p.absent {
            let q = shlex_quote(name);
            lines.push(format!("test ! -e {q} && test ! -L {q} || {{ echo; echo {}; exit 1; }}", stop(name)));
        }
        // A glob that matches nothing stays as written, which is not there.
        for name in &p.absent_any_case {
            lines.push(format!(
                "for f in {}; do test ! -e \"$f\" && test ! -L \"$f\" || {{ echo; echo {}; exit 1; }}; done",
                any_case_glob(name),
                stop(name),
            ));
        }
        lines.push(format!("echo; echo '$ '{}", shlex_quote(cmd)));
        match literal_words(cmd) {
            Some(words) if !words.is_empty() => lines.push(argv_line(&python_m_safe(words))),
            _ => {
                let msg = shlex_quote(&format!("{cmd} is not plain words -- setup stopped."));
                lines.push(format!("echo; echo {msg}; exit 1"));
            }
        }
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

/// A package.json's scripts, set out before the file itself: they are what
/// npm, pnpm, yarn and bun run -- by name, and the install ones (preinstall,
/// install, postinstall, prepare) on every install without being asked --
/// and in a long file they are easy to miss. None for any other file, or
/// one with no scripts (or that is not JSON: it is still shown in full).
fn package_scripts(name: &std::ffi::OsStr, body: &str) -> Option<String> {
    if Path::new(name).file_name()? != "package.json" {
        return None;
    }
    // serde_json stops at 128 levels, so a hostile file cannot nest deeper.
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    let scripts = json.get("scripts")?.as_object()?;
    let lines: Vec<String> = scripts
        .iter()
        .filter_map(|(k, v)| Some(format!("  {k}: {}", v.as_str()?)))
        .collect();
    if lines.is_empty() {
        return None;
    }
    Some(format!(
        "Scripts in {} (install, preinstall, postinstall and prepare run on every install)\n{}",
        esc(name),
        lines.join("\n")
    ))
}

/// Everything the plan would do, for the confirmation dialog.
pub fn describe(p: &SetupPlan) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !p.packages.is_empty() {
        parts.push(format!("Packages, installed by Almanac\n  {}", p.packages.join(" ")));
    }
    if !p.commands.is_empty() {
        let mut run = format!(
            "Run in a terminal, in the plugin's folder\n{}",
            p.commands.iter().map(|c| format!("  $ {c}")).collect::<Vec<_>>().join("\n")
        );
        // What is shown below is what a build reads first, not all it runs:
        // say so rather than let the list of files suggest otherwise.
        if p.commands.iter().any(|c| step_kind(c) == Some(Kind::Build)) {
            run.push('\n');
            run.push_str(&tr(
                "A build, or a Python or JavaScript program, runs code from the repository beyond the files shown here.",
            ));
        }
        parts.push(run);
    }
    for (name, body) in &p.scripts {
        if let Some(scripts) = package_scripts(name, body) {
            parts.push(scripts);
        }
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
        let guard = format!("got=$(sha256sum < install && printf x) && got=${{got%x}} && got=${{got%$'\\n'}} && test \"$got\" = '{want}  -' || {{");
        // Before each command, the make that could rewrite it included.
        assert_eq!(script.matches(&guard).count(), 2, "{script}");
        assert!(script.find(&guard) < script.find("\n'make'\n"));
        std::fs::write(root.join("install"), "echo evil\n").unwrap();
        assert_eq!(unchanged(&p), Err("install".to_string()));
        std::fs::remove_file(root.join("install")).unwrap();
        assert_eq!(unchanged(&p), Err("install".to_string()));
        // A plan whose files carry no digest is not trusted either.
        let bare = SetupPlan { digests: Vec::new(), ..p.clone() };
        let _ = std::fs::remove_dir_all(&d);
        assert!(unchanged(&bare).is_err());
    }

    /// A build step runs the plugin's build files, so the ones present are
    /// shown and checked like scripts; absent ones are held absent.
    #[test]
    fn test_a_build_step_shows_the_build_files_it_runs() {
        let d = tempdir();
        let root = d.join("p");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(
            root.join("README.md"),
            concat!(
                "# Install\n```\n",
                "make install\n",
                "npm install && npm run build\n",
                "pip install -e .\n",
                "cargo build --release\n",
                "just setup\n",
                "meson setup build && ninja -C build\n",
                "make -C sub\n",
                "npm --prefix elsewhere install\n",
                "```\n"
            ),
        )
        .unwrap();
        let files = [
            ("Makefile", "install:\n\techo make\n"),
            ("package.json", "{\"name\": \"p\", \"scripts\": {\"postinstall\": \"node evil.js\", \"build\": \"tsc\"}}\n"),
            ("pyproject.toml", "[build-system]\nbackend-path = [\".\"]\n"),
            ("Cargo.toml", "[package]\nname = \"p\"\n"),
            ("build.rs", "fn main() {}\n"),
            ("justfile", "setup:\n  echo just\n"),
            ("meson.build", "project('p')\n"),
            ("sub/GNUmakefile", "all:\n\techo sub\n"),
        ];
        for (name, body) in files {
            std::fs::write(root.join(name), body).unwrap();
        }
        let p = plan(&root).unwrap();
        let shown = describe(&p);
        let script = terminal_script(&p);
        assert_eq!(unchanged(&p), Ok(()));
        std::fs::write(root.join("build.rs"), "fn main() { evil() }\n").unwrap();
        let after = unchanged(&p);
        let _ = std::fs::remove_dir_all(&d);

        assert!(p.commands.contains(&"make install".to_string()) && p.commands.contains(&"make -C sub".to_string()));
        assert!(!p.commands.iter().any(|c| c.contains("--prefix")), "a step pointed elsewhere is not offered");
        let mut names: Vec<String> = p.scripts.iter().map(|(k, _)| esc(k)).collect();
        names.sort();
        // setup.py, setup.cfg, .npmrc ... are absent: not shown, but held absent (build.ninja meson writes)
        assert_eq!(
            names,
            ["Cargo.toml", "Makefile", "build.rs", "justfile", "meson.build", "package.json", "pyproject.toml", "sub/GNUmakefile"]
        );
        assert_eq!(p.digests.len(), p.scripts.len());
        assert!(p.absent.contains(&"setup.py".to_string()) && p.absent.contains(&".npmrc".to_string()));
        assert!(!p.absent.contains(&"build/build.ninja".to_string()));
        for body in ["echo make", "backend-path", "fn main() {}", "echo just", "project('p')", "echo sub"] {
            assert!(shown.contains(body), "{body} not shown");
        }
        // package.json's scripts are set out on their own, before the file.
        let scripts = shown.find("Scripts in package.json").expect("scripts listed");
        assert!(scripts < shown.find("Contents of package.json").unwrap());
        assert!(shown.contains("  postinstall: node evil.js") && shown.contains("  build: tsc"));
        // Checked before each command, as a script is.
        assert!(script.contains("sha256sum < build.rs"));
        assert_eq!(after, Err("build.rs".to_string()));
    }

    /// A build file that is there but cannot be shown (outside the plugin,
    /// binary) keeps the step from being offered.
    #[test]
    fn test_a_build_file_that_cannot_be_shown_stops_the_step() {
        let d = tempdir();
        let root = d.join("p");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("README.md"), "# Install\n```\nmake\ncargo build\npip install .\n```\n").unwrap();
        std::fs::write(d.join("outside.mk"), "all:\n").unwrap();
        std::os::unix::fs::symlink(d.join("outside.mk"), root.join("Makefile")).unwrap();
        std::fs::write(root.join("Cargo.toml"), b"\x00binary").unwrap();
        let p = plan(&root).unwrap();
        let _ = std::fs::remove_dir_all(&d);
        // pip finds no setup.py, pyproject.toml or setup.cfg: nothing to show
        assert_eq!(p.commands, ["pip install ."]);
        assert!(p.scripts.is_empty());
    }

    #[test]
    fn test_build_files_by_tool() {
        let d = tempdir();
        std::fs::create_dir_all(d.join("pkg")).unwrap();
        let files = |line: &str| build_files(line, &d);
        assert_eq!(files("make -j4 -f build.mk").unwrap(), ["build.mk"]);
        assert_eq!(files("make -C a -C b").unwrap(), ["a/b/GNUmakefile", "a/b/makefile", "a/b/Makefile"]);
        assert_eq!(files("ninja -C build").unwrap(), ["build/build.ninja"]);
        assert_eq!(files("cmake -S src -B build").unwrap(), ["src/CMakeLists.txt", "src/CMakePresets.json", "src/CMakeUserPresets.json"]);
        assert!(files("cmake --build build").unwrap().is_empty());
        assert!(files("cargo install --path tool").unwrap().contains(&"tool/build.rs".to_string()));
        assert!(files("cargo build").unwrap().contains(&".cargo/config.toml".to_string()));
        // `-m pip` is the plugin's own if it ships a pip.py (the folder comes first).
        assert_eq!(
            files("python3 -m pip install ./pkg[cli]").unwrap(),
            ["pip.py", "pip/__init__.py", "pip/__main__.py", "./pkg/setup.py", "./pkg/pyproject.toml", "./pkg/setup.cfg"]
        );
        assert_eq!(files("python3 -u -m a.b").unwrap(), ["a/__init__.py", "a/b.py", "a/b/__init__.py", "a/b/__main__.py"]);
        assert!(files("cargo run").unwrap().contains(&"rust-toolchain.toml".to_string()));
        assert_eq!(files("go generate ./...").unwrap(), ["go.mod", "go.work"]);
        assert_eq!(files("go run main.go").unwrap(), ["go.mod", "go.work", "main.go"]);
        assert!(files("gcc -o x x.c").unwrap().is_empty());
        assert_eq!(files("pip install pkg").unwrap(), ["pkg/setup.py", "pkg/pyproject.toml", "pkg/setup.cfg"]);
        assert_eq!(files("pip install -r requirements.txt requests").unwrap(), ["requirements.txt"]);
        assert!(files("pip install requests git+https://x/y").unwrap().is_empty());
        assert_eq!(
            files("uv tool install .").unwrap(),
            ["uv.toml", "pyproject.toml", ".python-version", ".python-versions", "setup.py", "pyproject.toml", "setup.cfg"]
        );
        assert_eq!(files("pipx install --editable .").unwrap(), ["setup.py", "pyproject.toml", "setup.cfg"]);
        assert!(files("yarn").unwrap().contains(&".yarnrc.yml".to_string()));
        assert!(files("pnpm install").unwrap().contains(&".pnpmfile.cjs".to_string()));
        for elsewhere in [
            "npm --prefix x install", "yarn --cwd=x", "just -f other.just", "uv --directory x sync", "cargo -C x build",
            "go -C x build", "bun --preload ./x.ts run build",
        ] {
            assert!(files(elsewhere).is_err(), "{elsewhere}");
        }
        assert!(files("systemctl --user enable x").unwrap().is_empty());
        // A program with no case of its own is not run with nothing shown.
        assert!(files("node x.js").is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Every tool RUNNABLE offers has its own case: one added to `TOOLS`
    /// without one fails here rather than run with nothing shown for it.
    #[test]
    fn test_every_runnable_tool_has_a_kind() {
        let d = tempdir();
        let lines = TOOLS.iter().map(|(_, line)| *line).chain(["./install", "python x.py", "python3.12 x.py", "sudo make"]);
        for line in lines {
            assert!(RUNNABLE.is_match(line), "{line} not runnable");
            assert!(step_kind(line).is_some(), "{line} has no kind");
            assert!(build_files(line, &d).is_ok(), "{line} has no build files case");
            assert!(executed_files(line, &d).is_ok(), "{line} has no executed files case");
        }
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(step_kind("./install"), Some(Kind::Script));
        assert_eq!(step_kind("bash x.sh"), Some(Kind::Script));
        assert_eq!(step_kind("go run ."), Some(Kind::Build));
        assert_eq!(step_kind("python3 x.py"), Some(Kind::Build));
        assert_eq!(step_kind("systemctl --user enable x"), Some(Kind::Service));
        assert_eq!(step_kind("node x.js"), None);
    }

    /// `go run`, `go generate` and `cargo run` used to be offered with
    /// nothing shown; `bun x.ts` without x.ts. Now go.mod (and Cargo.toml)
    /// are shown, the file bun runs is shown and checked, and a build is
    /// said to run more than that.
    #[test]
    fn test_runners_show_what_they_run() {
        let d = tempdir();
        let root = d.join("p");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("README.md"),
            "# Install\n```\ngo generate ./...\ngo run .\ncargo run --release\nbun x.ts\nbun missing.ts\nbun run build\n```\n",
        )
        .unwrap();
        std::fs::write(root.join("go.mod"), "module p\n").unwrap();
        std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"p\"\n").unwrap();
        std::fs::write(root.join("x.ts"), "console.log('x')\n").unwrap();
        std::fs::write(root.join("package.json"), "{}\n").unwrap();
        let p = plan(&root).unwrap();
        let shown = describe(&p);
        std::fs::write(root.join("x.ts"), "evil()\n").unwrap();
        let after = unchanged(&p);
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(p.commands, ["go generate ./...", "go run .", "cargo run --release", "bun x.ts", "bun run build"]);
        let names: Vec<String> = p.scripts.iter().map(|(k, _)| esc(k)).collect();
        assert_eq!(names, ["go.mod", "Cargo.toml", "x.ts", "package.json"]);
        assert!(shown.contains("  console.log('x')") && shown.contains("  module p"));
        assert!(shown.contains("runs code from the repository beyond the files shown here"));
        assert!(p.absent.contains(&"go.work".to_string()) && p.absent.contains(&"bunfig.toml".to_string()));
        assert_eq!(after, Err("x.ts".to_string()));
    }

    /// A build file a tool would read that was not there when the plan was
    /// made is checked still absent before every command, by Almanac and
    /// by the terminal: `npm install`'s postinstall must not slip a
    /// GNUmakefile (which make prefers) in front of the Makefile shown.
    #[test]
    fn test_a_build_file_that_appears_stops_the_run() {
        let d = tempdir();
        let root = d.join("p");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("README.md"), "# Install\n```\nnpm install && make\njust setup\n```\n").unwrap();
        std::fs::write(root.join("package.json"), "{\"scripts\": {\"postinstall\": \"node gen.js\"}}\n").unwrap();
        std::fs::write(root.join("Makefile"), "all:\n\techo shown\n").unwrap();
        std::fs::write(root.join("JustFile"), "setup:\n  echo just\n").unwrap();
        let p = plan(&root).unwrap();
        assert_eq!(p.commands, ["npm install", "make", "just setup"]);
        // just matches its name in any case: the JustFile is shown.
        assert!(p.scripts.iter().any(|(k, _)| k == "JustFile"));
        for f in ["GNUmakefile", "makefile", ".npmrc"] {
            assert!(p.absent.contains(&f.to_string()), "{f} not recorded absent: {:?}", p.absent);
        }
        assert_eq!(p.absent_any_case, [".justfile"]);
        assert_eq!(unchanged(&p), Ok(()));

        // The terminal's own check, run for real on a stand-in command.
        let probe = SetupPlan { commands: vec!["echo ran".into()], ..p.clone() };
        let run = || {
            let out = std::process::Command::new("bash").arg("-c").arg(terminal_script(&probe)).output().unwrap();
            (out.status.success(), String::from_utf8_lossy(&out.stdout).into_owned())
        };
        let (ok, out) = run();
        assert!(ok && out.contains("ran"), "the check passes while nothing appeared: {out}");
        std::fs::write(root.join("GNUmakefile"), "all:\n\techo unseen\n").unwrap();
        let (ok, out) = run();
        assert!(!ok && !out.contains("ran") && out.contains("GNUmakefile changed after it was shown"), "{out}");
        assert_eq!(unchanged(&p), Err("GNUmakefile".to_string()));
        // A dangling symlink counts as there.
        std::fs::remove_file(root.join("GNUmakefile")).unwrap();
        std::os::unix::fs::symlink("nowhere", root.join("makefile")).unwrap();
        assert_eq!(unchanged(&p), Err("makefile".to_string()));
        assert!(!run().0);
        // And a justfile in another case.
        std::fs::remove_file(root.join("makefile")).unwrap();
        assert!(run().0);
        std::fs::write(root.join(".JUSTFILE"), "setup:\n").unwrap();
        let (ok, out) = run();
        let after = unchanged(&p);
        let _ = std::fs::remove_dir_all(&d);
        assert!(!ok && out.contains(".justfile changed after it was shown"), "{out}");
        assert_eq!(after, Err(".justfile".to_string()));
    }

    /// What a configure step shown before writes (meson's build.ninja,
    /// cmake's and configure's Makefile) is no file appearing unseen; and a
    /// build pointed outside the plugin is not offered.
    #[test]
    fn test_what_a_configure_step_writes_is_not_refused() {
        let d = tempdir();
        let root = d.join("p");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("README.md"),
            concat!(
                "# Install\n```\n",
                "meson setup build && ninja -C build\n",
                "cmake -B out && make -C out\n",
                "./configure && make\n",
                "make -C ..\n",
                "python3 -m venv .venv\n",
                "```\n"
            ),
        )
        .unwrap();
        std::fs::write(root.join("meson.build"), "project('p')\n").unwrap();
        std::fs::write(root.join("CMakeLists.txt"), "project(p)\n").unwrap();
        std::fs::write(root.join("configure"), "echo configuring\n").unwrap();
        let p = plan(&root).unwrap();
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(
            p.commands,
            ["meson setup build", "ninja -C build", "cmake -B out", "make -C out", "./configure", "make", "python3 -m venv .venv"]
        );
        for written in ["build/build.ninja", "out/Makefile", "Makefile"] {
            assert!(!p.absent.contains(&written.to_string()), "{written}: {:?}", p.absent);
        }
        // What they do not write is still held absent.
        for held in ["out/GNUmakefile", "GNUmakefile", "makefile", "meson.options", "venv.py", "venv/__init__.py"] {
            assert!(p.absent.contains(&held.to_string()), "{held}: {:?}", p.absent);
        }
    }

    /// The confirmation says a build runs more than the files it shows; a
    /// plan of scripts alone does not claim it.
    #[test]
    fn test_the_confirmation_says_a_build_runs_more_than_is_shown() {
        let line = "A build, or a Python or JavaScript program, runs code from the repository beyond the files shown here.";
        let build = SetupPlan { commands: vec!["make install".into(), "./install".into()], ..Default::default() };
        assert!(describe(&build).contains(&format!("  $ ./install\n{line}")), "{}", describe(&build));
        let script = SetupPlan { commands: vec!["./install".into(), "systemctl --user enable x".into()], ..Default::default() };
        assert!(!describe(&script).contains(line));
    }

    /// A plugin folder with this README and these files.
    fn plugin_with(d: &Path, readme: &str, files: &[(&str, &str)]) -> PathBuf {
        let root = d.join("p");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("README.md"), readme).unwrap();
        for (name, body) in files {
            if let Some(parent) = Path::new(name).parent() {
                std::fs::create_dir_all(root.join(parent)).unwrap();
            }
            std::fs::write(root.join(name), body).unwrap();
        }
        root
    }

    /// The terminal script with `commands` in place of the plan's, run for
    /// real: (succeeded, what it printed).
    fn run_script(p: &SetupPlan, home: &Path) -> (bool, String) {
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(terminal_script(p))
            .env("HOME", home)
            .output()
            .unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// The reviewer's probes: the planner split words while bash expanded
    /// them, so `bash ${a}x.sh` showed a harmless file literally named
    /// `${a}x.sh` and ran the unshown `x.sh`. Every line bash reads as more
    /// than plain words is now refused, `cd` in any form with it.
    #[test]
    fn test_a_line_bash_would_expand_is_not_offered() {
        let d = tempdir();
        let probes = [
            "bash ${a}x.sh",
            "python3 ${a}x.py",
            "bash $HOME/x.sh",
            "bash *.sh",
            "bash x?.sh",
            "bash [x].sh",
            "bash {x,y}.sh",
            "bash ~/x.sh",
            "make install; ./evil.sh",
            "make x `./evil.sh`",
            "make x $(./evil.sh)",
            "make all; cd",
            "make all;cd",
            "make | sh",
            "make & ./evil.sh",
            "make > Makefile.new",
            "make < in",
            "(make)",
            "make # comment",
            "make \"$(./evil.sh)\"",
            "make \"`./evil.sh`\"",
            "make \"\\$x\"",
            "make a\\ b",
            "make 'open",
            "make cd",
            "sudo make PREFIX=~user/x",
            "make PREFIX=a:~/b",
            "make ~/x",
        ];
        let readme = format!("# Install\n```\n{}\n```\n", probes.join("\n"));
        let files: Vec<(&str, &str)> = vec![
            ("${a}x.sh", "echo harmless\n"),
            ("${a}x.py", "print('harmless')\n"),
            ("x.sh", "echo evil\n"),
            ("x.py", "print('evil')\n"),
            ("evil.sh", "echo evil\n"),
            ("Makefile", "all:\n\techo make\n"),
        ];
        let root = plugin_with(&d, &readme, &files);
        let p = plan(&root).unwrap();
        let _ = std::fs::remove_dir_all(&d);
        assert!(p.commands.is_empty(), "offered: {:?}", p.commands);
        assert!(p.scripts.is_empty(), "shown: {:?}", p.scripts);
        // And each is refused on its own terms, not by some other rule.
        // (`\r` cannot reach a step through a README, which splits lines at
        // it, but bash would keep it in a word that shlex splits.)
        for probe in probes.iter().filter(|l| !l.contains("cd")).chain(&["make install\rnow"]) {
            assert!(literal_words(probe).is_none(), "{probe} read as plain words");
        }
    }

    /// What legitimate setup lines look like still reads as plain words, and
    /// runs as exactly those words: quotes as bash takes them, the one tilde
    /// bash expands as `$HOME`.
    #[test]
    fn test_a_step_runs_as_exactly_the_words_shown() {
        let d = tempdir();
        let readme = concat!(
            "# Install\n```\n",
            "make && sudo make install\n",
            "npm install\n",
            "pip install -e .\n",
            "make install PREFIX=~/.local\n",
            "./show.sh 'a b' \"c d\" e'f'g PREFIX=~ X=~/y\n",
            "```\n"
        );
        let show = "for a in \"$@\"; do printf '[%s]' \"$a\"; done; echo\n";
        let root = plugin_with(
            &d,
            readme,
            &[("Makefile", "all:\n"), ("package.json", "{}\n"), ("pyproject.toml", "[project]\n"), ("show.sh", show)],
        );
        std::fs::set_permissions(root.join("show.sh"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let p = plan(&root).unwrap();
        assert_eq!(
            p.commands,
            [
                "make",
                "sudo make install",
                "npm install",
                "pip install -e .",
                "make install PREFIX=~/.local",
                "./show.sh 'a b' \"c d\" e'f'g PREFIX=~ X=~/y",
            ]
        );
        let script = terminal_script(&p);
        assert!(script.contains("\n'sudo' 'make' 'install'\n"), "{script}");
        assert!(script.contains("\n'make' 'install' 'PREFIX='\"$HOME\"'/.local'\n"), "{script}");
        // Run for real, with only the stand-in script as a command.
        let home = d.join("home");
        let probe = SetupPlan { commands: vec![p.commands[5].clone()], ..p.clone() };
        let (ok, out) = run_script(&probe, &home);
        let _ = std::fs::remove_dir_all(&d);
        let h = home.display();
        assert!(ok, "{out}");
        assert!(out.contains(&format!("[a b][c d][efg][PREFIX={h}][X={h}/y]\n")), "{out}");
    }

    /// Shown files were kept under the file a symlink leads to, and both
    /// checks hashed only that: repointing `./install` (or a folder on the
    /// way) at another file of the plugin's after the plan passed. Each path
    /// is now resolved again, by Almanac and by the terminal.
    #[test]
    fn test_a_symlink_repointed_after_the_plan_stops_the_run() {
        use std::os::unix::fs::symlink;
        let d = tempdir();
        let root = plugin_with(
            &d,
            "# Install\n```\n./install\nbash tools/x.sh\n```\n",
            &[("real.sh", "echo real\n"), ("evil.sh", "echo evil\n"), ("t1/x.sh", "echo one\n"), ("t2/x.sh", "echo two\n")],
        );
        symlink("real.sh", root.join("install")).unwrap();
        symlink("t1", root.join("tools")).unwrap();
        std::fs::set_permissions(root.join("real.sh"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let p = plan(&root).unwrap();
        assert_eq!(p.commands, ["./install", "bash tools/x.sh"]);
        let names: Vec<String> = p.scripts.iter().map(|(k, _)| esc(k)).collect();
        assert_eq!(names, ["real.sh", "t1/x.sh"]);
        let base = resolve(&root);
        assert_eq!(
            p.links,
            [
                ("./install".to_string(), esc(base.join("real.sh").as_os_str())),
                ("tools/x.sh".to_string(), esc(base.join("t1/x.sh").as_os_str())),
            ]
        );
        let probe = SetupPlan { commands: vec!["./install".into()], ..p.clone() };
        assert_eq!(unchanged(&p), Ok(()));
        let (ok, out) = run_script(&probe, &d);
        assert!(ok && out.contains("\nreal\n"), "{out}");

        // The file a symlink names, repointed.
        std::fs::remove_file(root.join("install")).unwrap();
        symlink("evil.sh", root.join("install")).unwrap();
        assert_eq!(unchanged(&p), Err("./install".to_string()));
        let (ok, out) = run_script(&probe, &d);
        assert!(!ok && !out.contains("evil") && out.contains("./install changed after it was shown"), "{out}");
        std::fs::remove_file(root.join("install")).unwrap();
        symlink("real.sh", root.join("install")).unwrap();
        assert_eq!(unchanged(&p), Ok(()));

        // A folder on the way, repointed.
        std::fs::remove_file(root.join("tools")).unwrap();
        symlink("t2", root.join("tools")).unwrap();
        let after = unchanged(&p);
        let (ok, out) = run_script(&probe, &d);
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(after, Err("tools/x.sh".to_string()));
        assert!(!ok && out.contains("tools/x.sh changed after it was shown"), "{out}");
    }

    /// `systemctl --user enable --now ./x.service` links and starts a unit
    /// file of the plugin's by path, with nothing shown: not offered.
    #[test]
    fn test_a_unit_given_by_path_is_not_offered() {
        let d = tempdir();
        let root = plugin_with(
            &d,
            concat!(
                "# Install\n```\n",
                "systemctl --user enable --now ./x.service\n",
                "systemctl --user link /tmp/x.service\n",
                "systemctl --user enable --now x.service\n",
                "```\n"
            ),
            &[("x.service", "[Service]\nExecStart=/bin/true\n")],
        );
        let p = plan(&root).unwrap();
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(p.commands, ["systemctl --user enable --now x.service"]);
        assert_eq!(step_kind("systemctl --user enable ./x.service"), None);
    }

    /// Any script named `configure` (`./tools/configure`) used to excuse
    /// ./Makefile and ./build.ninja from being held absent. Only the
    /// plugin's top-level configure, which writes ./Makefile, does now.
    #[test]
    fn test_only_the_top_level_configure_excuses_its_makefile() {
        let d = tempdir();
        let root = plugin_with(
            &d,
            "# Install\n```\n./tools/configure && make\nninja\n```\n",
            &[("tools/configure", "echo configuring\n")],
        );
        let p = plan(&root).unwrap();
        assert_eq!(p.commands, ["./tools/configure", "make", "ninja"]);
        for held in ["Makefile", "build.ninja"] {
            assert!(p.absent.contains(&held.to_string()), "{held}: {:?}", p.absent);
        }
        std::fs::write(root.join("Makefile"), "all:\n\techo unseen\n").unwrap();
        let after = unchanged(&p);
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(after, Err("Makefile".to_string()));
        assert_eq!(generated_by("./configure", &["./configure".into()]), ["Makefile"]);
        assert!(generated_by("./tools/configure", &["./tools/configure".into()]).is_empty());
    }

    /// The third review's probe: `$(realpath -e -- ./install)` drops trailing
    /// newlines, so `./install` repointed (by an earlier step) at a file
    /// named `real.sh\n` read as `real.sh`, the digest checked the real.sh
    /// left alone, and the next command ran the file never shown. The
    /// terminal's check now keeps every byte; a name with a control
    /// character is not offered in the first place.
    #[test]
    fn test_a_link_repointed_at_a_name_ending_in_a_newline_stops_the_run() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let d = tempdir();
        let root = plugin_with(&d, "# Install\n```\n./install\n```\n", &[("real.sh", "echo real\n"), ("real.sh\n", "echo unshown\n")]);
        for f in ["real.sh", "real.sh\n"] {
            std::fs::set_permissions(root.join(f), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        symlink("real.sh", root.join("install")).unwrap();
        let p = plan(&root).unwrap();
        assert_eq!(p.commands, ["./install"]);
        let (ok, out) = run_script(&p, &d);
        assert!(ok && out.contains("\nreal\n"), "{out}");

        std::fs::remove_file(root.join("install")).unwrap();
        symlink("real.sh\n", root.join("install")).unwrap();
        // The check as it was is fooled: the positive control.
        let old = format!("cd {} && test \"$(realpath -e -- ./install)\" = {}", shlex_quote(&esc(root.as_os_str())), shlex_quote(&p.links[0].1));
        assert!(std::process::Command::new("bash").arg("-c").arg(&old).status().unwrap().success());
        // Almanac's own check, and the terminal's, are not.
        assert_eq!(unchanged(&p), Err("./install".to_string()));
        let (ok, out) = run_script(&p, &d);
        assert!(!ok && !out.contains("unshown") && out.contains("./install changed after it was shown"), "{out}");

        // A digest is held to every byte the same way.
        std::fs::remove_file(root.join("install")).unwrap();
        symlink("real.sh", root.join("install")).unwrap();
        assert!(run_script(&p, &d).0);
        std::fs::write(root.join("real.sh"), "echo edited\n").unwrap();
        let (ok, out) = run_script(&p, &d);
        assert!(!ok && !out.contains("edited") && out.contains("real.sh changed after it was shown"), "{out}");

        // Pointing there when the plan is made, it is not offered at all.
        std::fs::remove_file(root.join("install")).unwrap();
        symlink("real.sh\n", root.join("install")).unwrap();
        let refused = plan(&root).unwrap();
        let _ = std::fs::remove_dir_all(&d);
        assert!(refused.commands.is_empty() && refused.scripts.is_empty(), "{refused:?}");
    }

    /// Python puts the folder first on sys.path: `python3 -m venv .venv`
    /// imported a plugin's own logging.py, unseen. Every `-m` step now runs
    /// with `-P`, through sudo too; a script run by path keeps its folder.
    #[test]
    fn test_python_steps_do_not_import_the_plugins_files() {
        let d = tempdir();
        let root = plugin_with(
            &d,
            "# Install\n```\npython3 -m venv --help\nsudo python3 -m pip install .\n```\n",
            &[("logging.py", "print('planted')\nraise SystemExit(3)\n"), ("pyproject.toml", "[project]\n")],
        );
        let p = plan(&root).unwrap();
        assert_eq!(p.commands, ["python3 -m venv --help", "sudo python3 -m pip install ."]);
        let script = terminal_script(&p);
        assert!(script.contains("\n'python3' '-P' '-m' 'venv' '--help'\n"), "{script}");
        assert!(script.contains("\n'sudo' 'python3' '-P' '-m' 'pip' 'install' '.'\n"), "{script}");
        assert!(!script.contains("PYTHONSAFEPATH"), "{script}");
        let texts = |line: &str| python_m_safe(literal_words(line).unwrap()).into_iter().map(|w| w.text).collect::<Vec<_>>();
        assert_eq!(texts("sudo make"), ["sudo", "make"]);
        assert_eq!(texts("python3 setup.py install"), ["python3", "setup.py", "install"]);
        assert_eq!(texts("python3 -u -m build"), ["python3", "-P", "-u", "-m", "build"]);
        // Run for real where there is a Python to run.
        let python = |safe: bool| {
            let mut c = std::process::Command::new("python3");
            c.args(["-m", "venv", "--help"]).current_dir(&root).env_remove("PYTHONSAFEPATH");
            if safe {
                c.env("PYTHONSAFEPATH", "1");
            }
            c.output().ok().map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        };
        if let Some(unsafe_out) = python(false) {
            // The positive control: without it, the planted file runs.
            assert!(unsafe_out.contains("planted"), "{unsafe_out}");
            let probe = SetupPlan { commands: vec![p.commands[0].clone()], ..p.clone() };
            let (ok, out) = run_script(&probe, &d);
            assert!(ok && !out.contains("planted") && out.contains("usage"), "{out}");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The fourth review's probes: runners through uv and pipx, a
    /// .python-version naming a folder, and a quoted glob of units.
    #[test]
    fn test_runners_folders_and_unit_globs_are_not_offered() {
        let d = tempdir();
        let readme = "# Install\n```\nuv run python evil.py\nuv run python -m venv --help\nuv tool run --from . x\npipx run ./x.py\nsystemctl --user stop '*'\nsystemctl --user stop 'x?'\nnpm exec cowsay\nnpm x cowsay\npnpm dlx cowsay\nyarn dlx cowsay\nbun x cowsay\n```\n";
        let root = plugin_with(&d, readme, &[("pyproject.toml", "[project]\n"), ("evil.py", "print('PWNED')\n"), ("venv.py", "print('PWNED')\n"), ("x.py", "\n")]);
        let p = plan(&root).unwrap();
        assert!(p.commands.is_empty(), "{:?}", p.commands);

        let readme = "# Install\n```\nuv sync\n```\n";
        let root = plugin_with(&d, readme, &[("pyproject.toml", "[project]\n"), (".python-version", "d\n"), ("d/bin/python", "#!/bin/sh\necho PWNED\n")]);
        assert!(plan(&root).unwrap().commands.is_empty());
        for good in ["3.12\n", "3.12.1\n", "3.13t\n", "pypy3.10\n", "cpython-3.12\n", "cpython@3.12\n", "# pinned\n\n3.12\n"] {
            assert!(python_versions_only(good), "{good:?}");
        }
        for bad in ["d\n", "./fakepy\n", "/usr/bin/python3\n", "3.12 d\n", "python\n", "~/py\n"] {
            assert!(!python_versions_only(bad), "{bad:?}");
        }
        std::fs::write(root.join(".python-version"), "3.12\n").unwrap();
        assert_eq!(plan(&root).unwrap().commands, ["uv sync"]);

        assert!(service_line(&["--user".into(), "stop".into(), "x.service".into()]));
        assert!(!service_line(&["--user".into(), "stop".into(), "*".into()]));
        assert!(!service_line(&["--user".into(), "restart".into(), "x[12].service".into()]));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// `uv sync` with a .python-version of `./fakepy` ran a program of the
    /// plugin's as Python. uv's config and version files are now shown, and
    /// one naming a path refuses the step.
    #[test]
    fn test_uv_shows_its_python_version_and_refuses_a_path() {
        let d = tempdir();
        let readme = "# Install\n```\nuv sync\n```\n";
        let root = plugin_with(&d, readme, &[("pyproject.toml", "[project]\n"), (".python-version", "./fakepy\n"), ("fakepy", "#!/bin/sh\n")]);
        assert!(plan(&root).unwrap().commands.is_empty());
        std::fs::write(root.join(".python-version"), "3.12\n").unwrap();
        let p = plan(&root).unwrap();
        assert_eq!(p.commands, ["uv sync"]);
        let names: Vec<String> = p.scripts.iter().map(|(k, _)| esc(k)).collect();
        assert_eq!(names, ["pyproject.toml", ".python-version"]);
        for absent in ["uv.toml", ".python-versions", "setup.py"] {
            assert!(p.absent.contains(&absent.to_string()), "{absent}: {:?}", p.absent);
        }
        // Repointed at a path after the plan: a changed file.
        std::fs::write(root.join(".python-version"), "./fakepy\n").unwrap();
        let after = unchanged(&p);
        std::fs::write(root.join("uv.toml"), "python-preference = \"only-system\"\n").unwrap();
        std::fs::write(root.join(".python-version"), "3.12\n").unwrap();
        let appeared = unchanged(&p);
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(after, Err(".python-version".to_string()));
        assert_eq!(appeared, Err("uv.toml".to_string()));
    }

    /// `systemctl --user` took any verb or option without a `/`: `edit`,
    /// `set-environment`, `--root=`. Only turning installed units on and off.
    #[test]
    fn test_systemctl_takes_only_a_few_verbs() {
        for ok in [
            "systemctl --user enable --now x.service",
            "systemctl --user disable x.service",
            "systemctl --user start x",
            "systemctl --user stop x y",
            "systemctl --user restart x.timer",
            "systemctl --user daemon-reload",
            "systemctl --user --now enable x",
        ] {
            assert_eq!(step_kind(ok), Some(Kind::Service), "{ok}");
        }
        for bad in [
            "systemctl --user edit x",
            "systemctl --user set-environment PATH=x",
            "systemctl --user import-environment",
            "systemctl --user --root=x enable y",
            "systemctl --user -H host start x",
            "systemctl --user enable",
            "systemctl --user daemon-reload x",
            "systemctl --user mask x",
            "systemctl --user enable -- x",
            "systemctl --user link x.service",
            "systemctl --user start --no-block x",
        ] {
            assert_eq!(step_kind(bad), None, "{bad}");
        }
        let d = tempdir();
        let root = plugin_with(&d, "# Install\n```\nsystemctl --user edit x\nsystemctl --user start x\n```\n", &[]);
        let p = plan(&root).unwrap();
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(p.commands, ["systemctl --user start x"]);
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
