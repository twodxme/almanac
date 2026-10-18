//! Port of `themeart.py`.
//!
//! Drawing what a theme actually looks like.
//!
//! GitHub's social card shows a repository: its name, its description, its owner's
//! avatar. For a theme that is a picture of the wrong thing — it tells you nothing
//! about the only question you have, which is what your desktop will look like.
//!
//! Every Omarchy theme ships `colors.toml`, and raw.githubusercontent.com serves
//! it without touching the API rate limit. So the preview is drawn here from the
//! theme's own palette: a miniature desktop with the theme's bar, window,
//! text and accent in their real colours, over its real background.
//!
//! It is a mock, not a screenshot, and it is honest about that — but it is drawn
//! entirely from values the theme itself publishes, so the colours are exactly
//! the ones you would get.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

/// A palette as Python's dict holds it: keys in the order colors.toml has them.
pub type Palette = indexmap::IndexMap<String, String>;

/// `str(Path(s))`: what pathlib makes of a path string. Repeated slashes
/// collapse, "." parts and a trailing slash go, ".." stays, and POSIX keeps
/// exactly two leading slashes ("//a") as they are. An empty path is ".".
/// Bytes, not text: Python keeps undecodable bytes (surrogateescape), so a
/// non-UTF-8 directory name must come through unchanged.
fn py_path(s: &[u8]) -> Vec<u8> {
    let prefix: &[u8] = if s.starts_with(b"//") && !s.starts_with(b"///") {
        b"//"
    } else if s.starts_with(b"/") {
        b"/"
    } else {
        b""
    };
    let parts: Vec<&[u8]> = s.split(|&c| c == b'/').filter(|p| !p.is_empty() && *p != b".").collect();
    let mut out = prefix.to_vec();
    out.extend_from_slice(&parts.join(&b'/'));
    if out.is_empty() { b".".to_vec() } else { out }
}

/// `a / b` on pathlib paths: "." on the left adds nothing.
fn py_join(a: &[u8], b: &[u8]) -> Vec<u8> {
    if a == b"." {
        return py_path(b);
    }
    let mut s = a.to_vec();
    if !a.ends_with(b"/") {
        s.push(b'/');
    }
    s.extend_from_slice(b);
    py_path(&s)
}

fn to_path(b: Vec<u8>) -> PathBuf {
    PathBuf::from(OsString::from_vec(b))
}

/// `Path.home()`, which is `os.path.expanduser("~")`: $HOME when it is set,
/// even to "", else the passwd entry; trailing slashes go and an empty result
/// is "/". With neither, Python raises RuntimeError and themeart does not
/// import at all; "~" (what expanduser hands back) is the nearest thing.
fn py_home(home: Option<&OsStr>) -> Vec<u8> {
    let home = match home {
        Some(h) => h.as_bytes().to_vec(),
        None => match nix::unistd::User::from_uid(nix::unistd::getuid()) {
            Ok(Some(u)) => u.dir.into_os_string().into_vec(),
            _ => return b"~".to_vec(),
        },
    };
    let n = home.iter().rposition(|&c| c != b'/').map_or(0, |i| i + 1);
    if n == 0 { b"/".to_vec() } else { home[..n].to_vec() }
}

/// `Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")) / "almanac"`.
fn cache_from(xdg_cache_home: Option<&OsStr>, home: Option<&OsStr>) -> Vec<u8> {
    let base = match xdg_cache_home {
        Some(v) => py_path(v.as_bytes()),
        None => py_join(&py_path(&py_home(home)), b".cache"),
    };
    py_join(&base, b"almanac")
}

/// `CACHE`: `$XDG_CACHE_HOME/almanac`, or `~/.cache/almanac`. Python reads
/// the environment once, at import; the first call here stands in for that,
/// and later changes to the variable are not seen, as in Python.
pub fn cache() -> PathBuf {
    static CACHE: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    let c = CACHE.get_or_init(|| {
        cache_from(std::env::var_os("XDG_CACHE_HOME").as_deref(), std::env::var_os("HOME").as_deref())
    });
    to_path(c.clone())
}

/// `ART`: where drawn previews are cached.
pub fn art() -> PathBuf {
    to_path(py_join(cache().as_os_str().as_bytes(), b"themeart"))
}

pub const RAW: &str = "https://raw.githubusercontent.com";
/// `UA`: the User-Agent header sent with every fetch.
pub const UA: &str = "almanac";

pub const W: i32 = 480;
pub const H: i32 = 270;

/// `_HEX` as a pattern string: `#rgb` or `#rrggbb`.
pub const HEX_PATTERN: &str = r"^#(?:[0-9a-fA-F]{3}|[0-9a-fA-F]{6})$";

/// repo is an https://github.com/owner/name URL.
pub fn palette_url(repo: &str) -> String {
    let parts: Vec<&str> = repo.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() < 4 {
        return String::new();
    }
    format!("{RAW}/{}/{}/HEAD/colors.toml", parts[2], parts[3])
}

/// `_HEX` as Python's `re.match` sees it: Python's `$` also matches just
/// before a trailing newline, so the Rust regex spells that out.
fn hex_re() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"^#(?:[0-9a-fA-F]{3}|[0-9a-fA-F]{6})\n?\z").unwrap())
}

type Rgb = (f64, f64, f64);

const GREY: Rgb = (0.5, 0.5, 0.5);

/// `_rgb`. It can raise, as the Python does: "#abc\n" passes `_HEX` (`$`
/// matches before a trailing newline) but leaves h = "abc\n", four
/// characters, so it is not doubled and `int(h[4:6], 16)` sees "".
fn _rgb(value: &str, fallback: Rgb) -> Result<Rgb, String> {
    if value.is_empty() || !hex_re().is_match(value) {
        return Ok(fallback);
    }
    let mut h: String = value.trim_start_matches('#').to_string();
    if h.chars().count() == 3 {
        h = h.chars().flat_map(|c| [c, c]).collect();
    }
    // int(h[i:i+2], 16): h is ASCII here, and int() ignores the surrounding
    // whitespace `$` let through ("c\n").
    let part = |i: usize| -> Result<f64, String> {
        let s = h.get(i.min(h.len())..(i + 2).min(h.len())).unwrap_or("");
        u8::from_str_radix(s.trim(), 16)
            .map(|n| n as f64 / 255.0)
            .map_err(|_| format!("invalid literal for int() with base 16: '{s}'"))
    };
    Ok((part(0)?, part(2)?, part(4)?))
}

/// Python's `str.isspace()`: Rust's whitespace plus U+001C..U+001F, which
/// Python counts and Rust does not.
fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `str.strip()`.
fn py_strip(s: &str) -> &str {
    s.trim_matches(py_isspace)
}

/// Code points whose case properties differ between Python 3.14 (Unicode
/// 16.0) and this Rust (Unicode 17), each with a stand-in Rust treats the way
/// Python treats the original. U+0295 is a cased letter in 16.0 and not in
/// 17; the rest are new in 17, so unassigned to Python: not cased, not
/// case-ignorable, not lowercased. Found by comparing `lower()` of every
/// code point, alone and next to a final sigma, on both sides.
const UNICODE16_STAND_INS: &[(u32, u32, char)] = &[
    (0x295, 0x295, 'a'), (0x1ACF, 0x1ADD, '0'), (0x1AE0, 0x1AEB, '0'), (0xA7CE, 0xA7CF, '0'),
    (0xA7D2, 0xA7D2, '0'), (0xA7D4, 0xA7D4, '0'), (0xA7F1, 0xA7F1, '0'), (0x10EC5, 0x10EC5, '0'),
    (0x10EFA, 0x10EFB, '0'), (0x11B60, 0x11B60, '0'), (0x11B62, 0x11B64, '0'), (0x11B66, 0x11B66, '0'),
    (0x11DD9, 0x11DD9, '0'), (0x16EA0, 0x16EB8, '0'), (0x16EBB, 0x16ED3, '0'), (0x16FF2, 0x16FF3, '0'),
    (0x1E6E3, 0x1E6E3, '0'), (0x1E6E6, 0x1E6E6, '0'), (0x1E6EE, 0x1E6EF, '0'), (0x1E6F5, 0x1E6F5, '0'),
    (0x1E6FF, 0x1E6FF, '0'),
];

fn unicode16_stand_in(c: char) -> Option<char> {
    let cp = c as u32;
    UNICODE16_STAND_INS.iter().find(|(a, b, _)| (*a..=*b).contains(&cp)).map(|(_, _, s)| *s)
}

/// `str.lower()` with Python's Unicode tables. Rust lowercases one character
/// at a time, except that a capital sigma looks at its neighbours to choose
/// between σ and final ς; so the string is lowercased with stand-ins in place
/// (which gets the sigmas right) and each stand-in's own output is then put
/// back as the original character, which Python leaves alone.
fn py_lower(s: &str) -> String {
    let subst: Vec<char> = s.chars().map(|c| unicode16_stand_in(c).unwrap_or(c)).collect();
    let lowered: String = subst.iter().collect::<String>().to_lowercase();
    let mut rest = lowered.chars();
    let mut out = String::with_capacity(lowered.len());
    for (orig, sc) in s.chars().zip(subst) {
        let n = if sc == '\u{3a3}' { 1 } else { sc.to_lowercase().count() };
        let piece: String = rest.by_ref().take(n).collect();
        if orig != sc {
            out.push(orig);
        } else {
            out.push_str(&piece);
        }
    }
    out
}

/// `tomllib.loads`, ported from Python 3.14's `tomllib/_parser.py` so the
/// same documents pass and fail: integers of any size (up to int()'s 4300
/// digit limit), float overflow to inf, dates checked the way `datetime()`
/// checks them (no year 0, no Feb 30), no leap seconds, no \e or \x escapes,
/// no newlines in inline tables, no leading BOM. Only what fetch_palette
/// needs is kept of each value: strings, and the shape of tables and arrays
/// (the duplicate-key rules depend on it).
mod tomllib {
    use std::cell::Cell;
    use std::collections::HashMap;

    pub type Table = indexmap::IndexMap<String, Val>;

    pub enum Val {
        Str(String),
        Table(Table),
        Array(Vec<Val>),
        /// Numbers, booleans, dates and times.
        Other,
    }

    type Key = Vec<String>;
    type R<T> = Result<T, String>;

    /// `sys.getrecursionlimit()`.
    const RECURSION_LIMIT: usize = 1000;
    /// Python frames already on the stack when the app's thumbnail worker
    /// calls `tomllib.loads`: threading's _bootstrap, _bootstrap_inner and
    /// run, the pool's _worker, _WorkItem.run and WorkerContext.run, the
    /// app's work(), store.fetch_thumb, thumbnail and fetch_palette.
    const BASE_FRAMES: usize = 10;
    /// `MAX_KEY_PARTS = sys.getrecursionlimit()`.
    const MAX_KEY_PARTS: usize = RECURSION_LIMIT;
    /// `sys.get_int_max_str_digits()`: int() refuses longer decimal strings.
    const INT_MAX_STR_DIGITS: usize = 4300;

    const FROZEN: u8 = 1;
    const EXPLICIT_NEST: u8 = 2;

    /// The parse, plus a count of the Python frames it would have open:
    /// every function below is one Python function, and the parser is
    /// recursive, so a deep enough document raises RecursionError there.
    struct Cx<'s> {
        src: &'s str,
        b: &'s [u8],
        depth: Cell<usize>,
    }

    struct Frame<'a>(&'a Cell<usize>);

    impl Drop for Frame<'_> {
        fn drop(&mut self) {
            self.0.set(self.0.get() - 1);
        }
    }

    impl<'s> Cx<'s> {
        fn frame(&self) -> R<Frame<'_>> {
            self.depth.set(self.depth.get() + 1);
            let f = Frame(&self.depth);
            if self.depth.get() > RECURSION_LIMIT {
                return Err("maximum recursion depth exceeded".into());
            }
            Ok(f)
        }
        fn at(&self, pos: usize) -> Option<u8> {
            self.b.get(pos).copied()
        }
        fn starts(&self, pos: usize, s: &str) -> bool {
            self.b.get(pos..).is_some_and(|r| r.starts_with(s.as_bytes()))
        }
    }

    fn is_ws(c: u8) -> bool {
        c == b' ' || c == b'\t'
    }
    fn is_ws_nl(c: u8) -> bool {
        is_ws(c) || c == b'\n'
    }
    fn is_bare(c: u8) -> bool {
        c.is_ascii_alphanumeric() || c == b'-' || c == b'_'
    }
    fn is_ctrl(c: u8) -> bool {
        c < 32 || c == 127
    }
    /// ILLEGAL_BASIC_STR_CHARS, ILLEGAL_LITERAL_STR_CHARS, ILLEGAL_COMMENT_CHARS.
    fn illegal_basic(c: u8) -> bool {
        is_ctrl(c) && c != b'\t'
    }
    /// ILLEGAL_MULTILINE_BASIC_STR_CHARS, ILLEGAL_MULTILINE_LITERAL_STR_CHARS.
    fn illegal_multiline(c: u8) -> bool {
        is_ctrl(c) && c != b'\t' && c != b'\n'
    }

    #[derive(Default)]
    struct FlagNode {
        flags: u8,
        recursive_flags: u8,
        nested: HashMap<String, FlagNode>,
    }

    /// Flags that map to parsed keys/namespaces.
    #[derive(Default)]
    struct Flags {
        flags: HashMap<String, FlagNode>,
        pending: Vec<(Key, u8)>,
    }

    impl Flags {
        fn add_pending(&mut self, cx: &Cx, key: Key, flag: u8) -> R<()> {
            let _f = cx.frame()?;
            self.pending.push((key, flag));
            Ok(())
        }

        fn finalize_pending(&mut self, cx: &Cx) -> R<()> {
            let _f = cx.frame()?;
            for (key, flag) in std::mem::take(&mut self.pending) {
                self.set(cx, &key, flag, false)?;
            }
            Ok(())
        }

        fn unset_all(&mut self, cx: &Cx, key: &[String]) -> R<()> {
            let _f = cx.frame()?;
            let mut cont = &mut self.flags;
            for k in &key[..key.len() - 1] {
                match cont.get_mut(k) {
                    Some(n) => cont = &mut n.nested,
                    None => return Ok(()),
                }
            }
            cont.remove(&key[key.len() - 1]);
            Ok(())
        }

        fn set(&mut self, cx: &Cx, key: &[String], flag: u8, recursive: bool) -> R<()> {
            let _f = cx.frame()?;
            let mut cont = &mut self.flags;
            for k in &key[..key.len() - 1] {
                cont = &mut cont.entry(k.clone()).or_default().nested;
            }
            let node = cont.entry(key[key.len() - 1].clone()).or_default();
            if recursive {
                node.recursive_flags |= flag;
            } else {
                node.flags |= flag;
            }
            Ok(())
        }

        fn is_(&self, cx: &Cx, key: &[String], flag: u8) -> R<bool> {
            let _f = cx.frame()?;
            if key.is_empty() {
                return Ok(false); // document root has no flags
            }
            let mut cont = &self.flags;
            for k in &key[..key.len() - 1] {
                let Some(inner) = cont.get(k) else { return Ok(false) };
                if inner.recursive_flags & flag != 0 {
                    return Ok(true);
                }
                cont = &inner.nested;
            }
            Ok(cont.get(&key[key.len() - 1]).is_some_and(|n| (n.flags | n.recursive_flags) & flag != 0))
        }
    }

    /// `NestedDict.get_or_create_nest`. KeyError and IndexError (an empty
    /// array) both end the parse, so both are just errors here.
    fn get_or_create_nest<'t>(cx: &Cx, dict: &'t mut Table, key: &[String], access_lists: bool) -> R<&'t mut Table> {
        let _f = cx.frame()?;
        let mut cont = dict;
        for k in key {
            let mut v = cont.entry(k.clone()).or_insert_with(|| Val::Table(Table::new()));
            if access_lists {
                if let Val::Array(a) = v {
                    v = a.last_mut().ok_or("list index out of range")?;
                }
            }
            match v {
                Val::Table(t) => cont = t,
                _ => return Err("There is no nest behind this key".into()),
            }
        }
        Ok(cont)
    }

    fn append_nest_to_list(cx: &Cx, dict: &mut Table, key: &[String]) -> R<()> {
        let _f = cx.frame()?;
        let cont = get_or_create_nest(cx, dict, &key[..key.len() - 1], true)?;
        let last_key = &key[key.len() - 1];
        match cont.get_mut(last_key) {
            Some(Val::Array(list)) => list.push(Val::Table(Table::new())),
            Some(_) => return Err("An object other than list found behind this key".into()),
            None => {
                cont.insert(last_key.clone(), Val::Array(vec![Val::Table(Table::new())]));
            }
        }
        Ok(())
    }

    struct Output {
        data: Table,
        flags: Flags,
    }

    /// Parse TOML from a string.
    ///
    /// Run on a thread of its own with a large stack: Python's recursion
    /// limit allows about 500 nested arrays, more than a 2 MB thread stack
    /// holds in Rust frames, so the limit must be reached before the stack is.
    pub fn loads(s: &str) -> R<Table> {
        std::thread::scope(|scope| {
            std::thread::Builder::new()
                .stack_size(64 << 20)
                .spawn_scoped(scope, || loads_here(s))
                .map_err(|e| e.to_string())?
                .join()
                .map_err(|_| "parser thread panicked".to_string())?
        })
    }

    fn loads_here(s: &str) -> R<Table> {
        // The spec allows converting "\r\n" to "\n", even in string
        // literals. Let's do so to simplify parsing.
        let src = s.replace("\r\n", "\n");
        let cx = Cx { src: &src, b: src.as_bytes(), depth: Cell::new(BASE_FRAMES) };
        let _f = cx.frame()?;
        let mut out = {
            // Output(), which makes a NestedDict() and a Flags()
            let _f = cx.frame()?;
            drop(cx.frame()?);
            drop(cx.frame()?);
            Output { data: Table::new(), flags: Flags::default() }
        };
        let mut header: Key = Vec::new();
        drop(cx.frame()?); // make_safe_parse_float
        let mut pos = 0;

        // Parse one statement at a time
        // (typically means one line in TOML source)
        loop {
            // 1. Skip line leading whitespace
            pos = skip_chars(&cx, pos, is_ws)?;

            // 2. Parse rules. Expect one of the following:
            //    - end of file
            //    - end of line
            //    - comment
            //    - key/value pair
            //    - append dict to list (and move to its namespace)
            //    - create dict (and move to its namespace)
            // Skip trailing whitespace when applicable.
            let Some(char) = cx.at(pos) else { break };
            if char == b'\n' {
                pos += 1;
                continue;
            }
            if is_bare(char) || char == b'"' || char == b'\'' {
                pos = key_value_rule(&cx, pos, &mut out, &header)?;
                pos = skip_chars(&cx, pos, is_ws)?;
            } else if char == b'[' {
                let second_char = cx.at(pos + 1);
                out.flags.finalize_pending(&cx)?;
                (pos, header) = if second_char == Some(b'[') {
                    create_list_rule(&cx, pos, &mut out)?
                } else {
                    create_dict_rule(&cx, pos, &mut out)?
                };
                pos = skip_chars(&cx, pos, is_ws)?;
            } else if char != b'#' {
                return Err("Invalid statement".into());
            }

            // 3. Skip comment
            pos = skip_comment(&cx, pos)?;

            // 4. Expect end of line or end of file
            let Some(char) = cx.at(pos) else { break };
            if char != b'\n' {
                return Err("Expected newline or end of document after a statement".into());
            }
            pos += 1;
        }
        Ok(out.data)
    }

    fn skip_chars(cx: &Cx, mut pos: usize, chars: fn(u8) -> bool) -> R<usize> {
        let _f = cx.frame()?;
        while cx.at(pos).is_some_and(chars) {
            pos += 1;
        }
        Ok(pos)
    }

    fn skip_until(cx: &Cx, pos: usize, expect: &str, error_on: fn(u8) -> bool, error_on_eof: bool) -> R<usize> {
        let _f = cx.frame()?;
        let new_pos = match cx.src[pos..].find(expect) {
            Some(i) => pos + i,
            None if error_on_eof => return Err(format!("Expected {expect:?}")),
            None => cx.b.len(),
        };
        if cx.b[pos..new_pos].iter().any(|&c| error_on(c)) {
            return Err("Found invalid character".into());
        }
        Ok(new_pos)
    }

    fn skip_comment(cx: &Cx, pos: usize) -> R<usize> {
        let _f = cx.frame()?;
        if cx.at(pos) == Some(b'#') {
            return skip_until(cx, pos + 1, "\n", illegal_basic, false);
        }
        Ok(pos)
    }

    fn skip_comments_and_array_ws(cx: &Cx, mut pos: usize) -> R<usize> {
        let _f = cx.frame()?;
        loop {
            let pos_before_skip = pos;
            pos = skip_chars(cx, pos, is_ws_nl)?;
            pos = skip_comment(cx, pos)?;
            if pos == pos_before_skip {
                return Ok(pos);
            }
        }
    }

    fn create_dict_rule(cx: &Cx, mut pos: usize, out: &mut Output) -> R<(usize, Key)> {
        let _f = cx.frame()?;
        pos += 1; // Skip "["
        pos = skip_chars(cx, pos, is_ws)?;
        let (p, key) = parse_key(cx, pos)?;
        pos = p;

        if out.flags.is_(cx, &key, EXPLICIT_NEST)? || out.flags.is_(cx, &key, FROZEN)? {
            return Err(format!("Cannot declare {key:?} twice"));
        }
        out.flags.set(cx, &key, EXPLICIT_NEST, false)?;
        get_or_create_nest(cx, &mut out.data, &key, true).map_err(|_| "Cannot overwrite a value")?;

        if !cx.starts(pos, "]") {
            return Err("Expected ']' at the end of a table declaration".into());
        }
        Ok((pos + 1, key))
    }

    fn create_list_rule(cx: &Cx, mut pos: usize, out: &mut Output) -> R<(usize, Key)> {
        let _f = cx.frame()?;
        pos += 2; // Skip "[["
        pos = skip_chars(cx, pos, is_ws)?;
        let (p, key) = parse_key(cx, pos)?;
        pos = p;

        if out.flags.is_(cx, &key, FROZEN)? {
            return Err(format!("Cannot mutate immutable namespace {key:?}"));
        }
        // Free the namespace now that it points to another empty list item...
        out.flags.unset_all(cx, &key)?;
        // ...but this key precisely is still prohibited from table declaration
        out.flags.set(cx, &key, EXPLICIT_NEST, false)?;
        append_nest_to_list(cx, &mut out.data, &key).map_err(|_| "Cannot overwrite a value")?;

        if !cx.starts(pos, "]]") {
            return Err("Expected ']]' at the end of an array declaration".into());
        }
        Ok((pos + 2, key))
    }

    fn key_value_rule(cx: &Cx, pos: usize, out: &mut Output, header: &[String]) -> R<usize> {
        let _f = cx.frame()?;
        let (pos, key, value) = parse_key_value_pair(cx, pos)?;
        let (key_parent, key_stem) = (&key[..key.len() - 1], &key[key.len() - 1]);
        let abs_key_parent: Key = header.iter().chain(key_parent).cloned().collect();

        for i in 1..key.len() {
            let cont_key: Key = header.iter().chain(&key[..i]).cloned().collect();
            // Check that dotted key syntax does not redefine an existing table
            if out.flags.is_(cx, &cont_key, EXPLICIT_NEST)? {
                return Err(format!("Cannot redefine namespace {cont_key:?}"));
            }
            // Containers in the relative path can't be opened with the table syntax or
            // dotted key/value syntax in following table sections.
            out.flags.add_pending(cx, cont_key, EXPLICIT_NEST)?;
        }

        if out.flags.is_(cx, &abs_key_parent, FROZEN)? {
            return Err(format!("Cannot mutate immutable namespace {abs_key_parent:?}"));
        }

        let nest = get_or_create_nest(cx, &mut out.data, &abs_key_parent, true).map_err(|_| "Cannot overwrite a value")?;
        if nest.contains_key(key_stem) {
            return Err("Cannot overwrite a value".into());
        }
        // Mark inline table and array namespaces recursively immutable
        if matches!(value, Val::Table(_) | Val::Array(_)) {
            let full: Key = header.iter().chain(&key).cloned().collect();
            out.flags.set(cx, &full, FROZEN, true)?;
        }
        nest.insert(key_stem.clone(), value);
        Ok(pos)
    }

    fn parse_key_value_pair(cx: &Cx, pos: usize) -> R<(usize, Key, Val)> {
        let _f = cx.frame()?;
        let (mut pos, key) = parse_key(cx, pos)?;
        if cx.at(pos) != Some(b'=') {
            return Err("Expected '=' after a key in a key/value pair".into());
        }
        pos += 1;
        pos = skip_chars(cx, pos, is_ws)?;
        let (pos, value) = parse_value(cx, pos)?;
        Ok((pos, key, value))
    }

    fn parse_key(cx: &Cx, pos: usize) -> R<(usize, Key)> {
        let _f = cx.frame()?;
        let (mut pos, key_part) = parse_key_part(cx, pos)?;
        let mut key: Key = vec![key_part];
        pos = skip_chars(cx, pos, is_ws)?;
        loop {
            if cx.at(pos) != Some(b'.') {
                return Ok((pos, key));
            }
            pos += 1;
            pos = skip_chars(cx, pos, is_ws)?;
            let (p, key_part) = parse_key_part(cx, pos)?;
            pos = p;
            key.push(key_part);
            if key.len() > MAX_KEY_PARTS {
                return Err(format!("TOML key has more than the allowed {MAX_KEY_PARTS} parts"));
            }
            pos = skip_chars(cx, pos, is_ws)?;
        }
    }

    fn parse_key_part(cx: &Cx, pos: usize) -> R<(usize, String)> {
        let _f = cx.frame()?;
        match cx.at(pos) {
            Some(c) if is_bare(c) => {
                let start_pos = pos;
                let pos = skip_chars(cx, pos, is_bare)?;
                Ok((pos, cx.src[start_pos..pos].to_string()))
            }
            Some(b'\'') => parse_literal_str(cx, pos),
            Some(b'"') => parse_one_line_basic_str(cx, pos),
            _ => Err("Invalid initial character for a key part".into()),
        }
    }

    fn parse_one_line_basic_str(cx: &Cx, pos: usize) -> R<(usize, String)> {
        let _f = cx.frame()?;
        parse_basic_str(cx, pos + 1, false)
    }

    fn parse_array(cx: &Cx, mut pos: usize) -> R<(usize, Val)> {
        let _f = cx.frame()?;
        pos += 1;
        let mut array = Vec::new();

        pos = skip_comments_and_array_ws(cx, pos)?;
        if cx.starts(pos, "]") {
            return Ok((pos + 1, Val::Array(array)));
        }
        loop {
            let (p, val) = parse_value(cx, pos)?;
            array.push(val);
            pos = skip_comments_and_array_ws(cx, p)?;

            match cx.at(pos) {
                Some(b']') => return Ok((pos + 1, Val::Array(array))),
                Some(b',') => {}
                _ => return Err("Unclosed array".into()),
            }
            pos += 1;

            pos = skip_comments_and_array_ws(cx, pos)?;
            if cx.starts(pos, "]") {
                return Ok((pos + 1, Val::Array(array)));
            }
        }
    }

    fn parse_inline_table(cx: &Cx, mut pos: usize) -> R<(usize, Val)> {
        let _f = cx.frame()?;
        pos += 1;
        drop(cx.frame()?); // NestedDict()
        let mut nested_dict = Table::new();
        drop(cx.frame()?); // Flags()
        let mut flags = Flags::default();

        pos = skip_chars(cx, pos, is_ws)?;
        if cx.starts(pos, "}") {
            return Ok((pos + 1, Val::Table(nested_dict)));
        }
        loop {
            let (p, key, value) = parse_key_value_pair(cx, pos)?;
            pos = p;
            let (key_parent, key_stem) = (&key[..key.len() - 1], &key[key.len() - 1]);
            if flags.is_(cx, &key, FROZEN)? {
                return Err(format!("Cannot mutate immutable namespace {key:?}"));
            }
            let nest = get_or_create_nest(cx, &mut nested_dict, key_parent, false).map_err(|_| "Cannot overwrite a value")?;
            if nest.contains_key(key_stem) {
                return Err(format!("Duplicate inline table key {key_stem:?}"));
            }
            let container = matches!(value, Val::Table(_) | Val::Array(_));
            nest.insert(key_stem.clone(), value);
            pos = skip_chars(cx, pos, is_ws)?;
            match cx.at(pos) {
                Some(b'}') => return Ok((pos + 1, Val::Table(nested_dict))),
                Some(b',') => {}
                _ => return Err("Unclosed inline table".into()),
            }
            if container {
                flags.set(cx, &key, FROZEN, true)?;
            }
            pos += 1;
            pos = skip_chars(cx, pos, is_ws)?;
        }
    }

    fn parse_basic_str_escape(cx: &Cx, mut pos: usize, multiline: bool) -> R<(usize, String)> {
        let _f = cx.frame()?;
        let escape_id = &cx.b[pos..(pos + 2).min(cx.b.len())];
        pos += 2;
        if multiline && matches!(escape_id, b"\\ " | b"\\\t" | b"\\\n") {
            // Skip whitespace until next non-whitespace character or end of
            // the doc. Error if non-whitespace is found before newline.
            if escape_id != b"\\\n" {
                pos = skip_chars(cx, pos, is_ws)?;
                match cx.at(pos) {
                    None => return Ok((pos, String::new())),
                    Some(b'\n') => {}
                    Some(_) => return Err("Unescaped '\\' in a string".into()),
                }
                pos += 1;
            }
            pos = skip_chars(cx, pos, is_ws_nl)?;
            return Ok((pos, String::new()));
        }
        let s = match escape_id {
            b"\\u" => return parse_hex_char(cx, pos, 4),
            b"\\U" => return parse_hex_char(cx, pos, 8),
            b"\\b" => "\u{8}",
            b"\\t" => "\t",
            b"\\n" => "\n",
            b"\\f" => "\u{c}",
            b"\\r" => "\r",
            b"\\\"" => "\"",
            b"\\\\" => "\\",
            _ => return Err("Unescaped '\\' in a string".into()),
        };
        Ok((pos, s.to_string()))
    }

    fn parse_basic_str_escape_multiline(cx: &Cx, pos: usize) -> R<(usize, String)> {
        let _f = cx.frame()?;
        parse_basic_str_escape(cx, pos, true)
    }

    fn parse_hex_char(cx: &Cx, pos: usize, hex_len: usize) -> R<(usize, String)> {
        let _f = cx.frame()?;
        let hex = cx.b.get(pos..pos + hex_len).filter(|h| h.iter().all(u8::is_ascii_hexdigit));
        let Some(hex) = hex else { return Err("Invalid hex value".into()) };
        let pos = pos + hex_len;
        let hex_int = hex.iter().fold(0u32, |n, &d| n * 16 + (d as char).to_digit(16).unwrap_or(0));
        drop(cx.frame()?); // is_unicode_scalar_value
        match char::from_u32(hex_int) {
            Some(c) => Ok((pos, c.to_string())),
            None => Err("Escaped character is not a Unicode scalar value".into()),
        }
    }

    fn parse_literal_str(cx: &Cx, mut pos: usize) -> R<(usize, String)> {
        let _f = cx.frame()?;
        pos += 1; // Skip starting apostrophe
        let start_pos = pos;
        pos = skip_until(cx, pos, "'", illegal_basic, true)?;
        Ok((pos + 1, cx.src[start_pos..pos].to_string())) // Skip ending apostrophe
    }

    fn parse_multiline_str(cx: &Cx, mut pos: usize, literal: bool) -> R<(usize, String)> {
        let _f = cx.frame()?;
        pos += 3;
        if cx.starts(pos, "\n") {
            pos += 1;
        }

        let (delim, mut result) = if literal {
            let end_pos = skip_until(cx, pos, "'''", illegal_multiline, true)?;
            let result = cx.src[pos..end_pos].to_string();
            pos = end_pos + 3;
            ('\'', result)
        } else {
            let (p, result) = parse_basic_str(cx, pos, true)?;
            pos = p;
            ('"', result)
        };

        // Add at maximum two extra apostrophes/quotes if the end sequence
        // is 4 or 5 chars long instead of just 3.
        for _ in 0..2 {
            if cx.at(pos) != Some(delim as u8) {
                return Ok((pos, result));
            }
            pos += 1;
            result.push(delim);
        }
        Ok((pos, result))
    }

    fn parse_basic_str(cx: &Cx, mut pos: usize, multiline: bool) -> R<(usize, String)> {
        let _f = cx.frame()?;
        let error_on = if multiline { illegal_multiline } else { illegal_basic };
        let mut result = String::new();
        let mut start_pos = pos;
        loop {
            let Some(char) = cx.at(pos) else { return Err("Unterminated string".into()) };
            if char == b'"' {
                if !multiline {
                    result.push_str(&cx.src[start_pos..pos]);
                    return Ok((pos + 1, result));
                }
                if cx.starts(pos, "\"\"\"") {
                    result.push_str(&cx.src[start_pos..pos]);
                    return Ok((pos + 3, result));
                }
                pos += 1;
                continue;
            }
            if char == b'\\' {
                result.push_str(&cx.src[start_pos..pos]);
                let (p, parsed_escape) = if multiline {
                    parse_basic_str_escape_multiline(cx, pos)?
                } else {
                    parse_basic_str_escape(cx, pos, false)?
                };
                pos = p;
                result.push_str(&parsed_escape);
                start_pos = pos;
                continue;
            }
            if error_on(char) {
                return Err(format!("Illegal character {:?}", char as char));
            }
            pos += 1;
        }
    }

    fn re(pattern: &str) -> regex::Regex {
        regex::Regex::new(pattern).unwrap()
    }

    /// `_TIME_RE_STR`, e.g. 00:32:00.999999 or 00:32:00.
    const TIME_RE: &str = r"([01][0-9]|2[0-3]):([0-5][0-9]):([0-5][0-9])(?:\.([0-9]{1,6})[0-9]*)?";

    fn re_number() -> &'static regex::Regex {
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| {
            re(concat!(
                r"\A(?:0(?:x[0-9A-Fa-f](?:_?[0-9A-Fa-f])*", // hex
                r"|b[01](?:_?[01])*",                        // bin
                r"|o[0-7](?:_?[0-7])*)",                     // oct
                r"|[+-]?(?:0|[1-9](?:_?[0-9])*)",            // dec, integer part
                r"(?P<floatpart>(?:\.[0-9](?:_?[0-9])*)?",   // optional fractional part
                r"(?:[eE][+-]?[0-9](?:_?[0-9])*)?))",        // optional exponent part
            ))
        })
    }

    fn re_localtime() -> &'static regex::Regex {
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| re(&format!(r"\A{TIME_RE}")))
    }

    fn re_datetime() -> &'static regex::Regex {
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| {
            re(&format!(
                // date, e.g. 1988-10-27, then an optional time and offset
                r"\A([0-9]{{4}})-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01])(?:[Tt ]{TIME_RE}(?:([Zz])|([+-])([01][0-9]|2[0-3]):([0-5][0-9]))?)?"
            ))
        })
    }

    /// `match_to_datetime`: `date()` / `datetime()` raise ValueError for a
    /// year 0 or a day the month does not have.
    fn match_to_datetime(cx: &Cx, m: &regex::Captures) -> R<()> {
        let _f = cx.frame()?;
        let num = |i: usize| m.get(i).map_or(0, |g| g.as_str().parse::<u32>().unwrap_or(0));
        let (year, month, day) = (num(1), num(2), num(3));
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let days = match month {
            2 if leap => 29,
            2 => 28,
            4 | 6 | 9 | 11 => 30,
            _ => 31,
        };
        if year < 1 || day > days {
            return Err("Invalid date or datetime".into());
        }
        if m.get(4).is_some() && m.get(9).is_some() {
            drop(cx.frame()?); // cached_tz
        }
        Ok(())
    }

    /// `match_to_number`: float() takes anything RE_NUMBER lets through
    /// (overflowing to inf); int() refuses a decimal string of more than
    /// 4300 digits.
    fn match_to_number(cx: &Cx, m: &regex::Captures) -> R<()> {
        let _f = cx.frame()?;
        let text = m.get(0).map_or("", |g| g.as_str());
        let float = m.name("floatpart").is_some_and(|g| !g.as_str().is_empty());
        let decimal = !text.starts_with("0x") && !text.starts_with("0b") && !text.starts_with("0o");
        if !float && decimal && text.bytes().filter(u8::is_ascii_digit).count() > INT_MAX_STR_DIGITS {
            return Err(format!("Exceeds the limit ({INT_MAX_STR_DIGITS} digits) for integer string conversion"));
        }
        Ok(())
    }

    fn parse_value(cx: &Cx, pos: usize) -> R<(usize, Val)> {
        let _f = cx.frame()?;
        let char = cx.at(pos);

        // IMPORTANT: order conditions based on speed of checking and likelihood

        // Basic strings
        if char == Some(b'"') {
            if cx.starts(pos, "\"\"\"") {
                let (p, s) = parse_multiline_str(cx, pos, false)?;
                return Ok((p, Val::Str(s)));
            }
            let (p, s) = parse_one_line_basic_str(cx, pos)?;
            return Ok((p, Val::Str(s)));
        }

        // Literal strings
        if char == Some(b'\'') {
            if cx.starts(pos, "'''") {
                let (p, s) = parse_multiline_str(cx, pos, true)?;
                return Ok((p, Val::Str(s)));
            }
            let (p, s) = parse_literal_str(cx, pos)?;
            return Ok((p, Val::Str(s)));
        }

        // Booleans
        if char == Some(b't') && cx.starts(pos, "true") {
            return Ok((pos + 4, Val::Other));
        }
        if char == Some(b'f') && cx.starts(pos, "false") {
            return Ok((pos + 5, Val::Other));
        }

        // Arrays
        if char == Some(b'[') {
            return parse_array(cx, pos);
        }

        // Inline tables
        if char == Some(b'{') {
            return parse_inline_table(cx, pos);
        }

        let rest = &cx.src[pos..];
        // Dates and times
        if let Some(m) = re_datetime().captures(rest) {
            match_to_datetime(cx, &m)?;
            return Ok((pos + m.get(0).map_or(0, |g| g.end()), Val::Other));
        }
        if let Some(m) = re_localtime().captures(rest) {
            drop(cx.frame()?); // match_to_localtime
            return Ok((pos + m.get(0).map_or(0, |g| g.end()), Val::Other));
        }

        // Integers and "normal" floats.
        // The regex will greedily match any type starting with a decimal
        // char, so needs to be located after handling of dates and times.
        if let Some(m) = re_number().captures(rest) {
            match_to_number(cx, &m)?;
            return Ok((pos + m.get(0).map_or(0, |g| g.end()), Val::Other));
        }

        // Special floats
        if cx.starts(pos, "inf") || cx.starts(pos, "nan") {
            return Ok((pos + 3, Val::Other));
        }
        if ["-inf", "+inf", "-nan", "+nan"].iter().any(|s| cx.starts(pos, s)) {
            return Ok((pos + 4, Val::Other));
        }

        Err("Invalid value".into())
    }
}

// ---- the fetch, the way urllib.request.urlopen(req, timeout=20) does it ----

/// urlopen's `timeout=20`: a socket timeout, so it bounds each connect
/// attempt (per address), each send and each recv, not a whole phase.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// The environment the fetch sees, in os.environ's order. The parity hook
/// overlays variables on it; the app passes the process's own.
#[derive(Clone)]
struct Env(Vec<(OsString, OsString)>);

impl Env {
    fn current() -> Env {
        Env(std::env::vars_os().collect())
    }
    fn get(&self, name: &str) -> Option<&OsStr> {
        self.0.iter().find(|(k, _)| k.as_os_str() == OsStr::new(name)).map(|(_, v)| v.as_os_str())
    }
    fn set(&mut self, name: &str, value: &str) {
        match self.0.iter_mut().find(|(k, _)| k.as_os_str() == OsStr::new(name)) {
            Some(e) => e.1 = value.into(),
            None => self.0.push((name.into(), value.into())),
        }
    }
}

/// `urllib.request.getproxies_environment()`: every `<scheme>_proxy`
/// variable, any case; a lowercase one wins over the others, and an empty
/// lowercase one removes the scheme. HTTP_PROXY is ignored under CGI.
fn getproxies_environment(env: &Env) -> std::collections::HashMap<String, String> {
    let mut proxies = std::collections::HashMap::new();
    let mut environment = Vec::new();
    for (name, value) in &env.0 {
        let name = name.to_string_lossy();
        let chars: Vec<char> = name.chars().collect();
        // fast screen underscore position before more expensive case-folding
        if chars.len() > 5 && chars[chars.len() - 6] == '_'
            && py_lower(&chars[chars.len() - 5..].iter().collect::<String>()) == "proxy"
        {
            let value = value.to_string_lossy().into_owned();
            let proxy_name = py_lower(&chars[..chars.len() - 6].iter().collect::<String>());
            if !value.is_empty() {
                proxies.insert(proxy_name.clone(), value.clone());
            }
            environment.push((name.into_owned(), value, proxy_name));
        }
    }
    // CVE-2016-1000110 - If we are running as CGI script, forget HTTP_PROXY
    // (non-all-lowercase) as it may be set from the web server by a "Proxy:"
    // header from the client
    // If "proxy" is lowercase, it will still be used thanks to the next block
    if env.get("REQUEST_METHOD").is_some() {
        proxies.remove("http");
    }
    for (name, value, proxy_name) in environment {
        // not case-folded, checking here for lower-case env vars only
        if name.ends_with("_proxy") {
            if !value.is_empty() {
                proxies.insert(proxy_name, value);
            } else {
                proxies.remove(&proxy_name);
            }
        }
    }
    proxies
}

/// `urllib.request.proxy_bypass_environment`: is `host` on no_proxy?
fn proxy_bypass_environment(host: &str, proxies: &std::collections::HashMap<String, String>) -> bool {
    // don't bypass, if no_proxy isn't specified
    let Some(no_proxy) = proxies.get("no") else { return false };
    // '*' is special case for always bypass
    if no_proxy == "*" {
        return true;
    }
    let host = py_lower(host);
    // strip port off host (`_splitport`: the last ':' followed only by digits)
    let hostonly = match host.rfind(':') {
        Some(i) if i + 1 < host.len() && host[i + 1..].bytes().all(|c| c.is_ascii_digit()) => &host[..i],
        _ => host.as_str(),
    };
    // check if the host ends with any of the DNS suffixes
    for name in no_proxy.split(',') {
        let name = py_strip(name);
        if !name.is_empty() {
            let name = py_lower(name.trim_start_matches('.')); // ignore leading dots
            if hostonly == name || host == name {
                return true;
            }
            let name = format!(".{name}");
            if hostonly.ends_with(&name) || host.ends_with(&name) {
                return true;
            }
        }
    }
    // otherwise, don't bypass
    false
}

/// `urllib.parse.unquote`: %XX runs decode as UTF-8 with replacement;
/// anything else, non-ASCII text included, is kept as it is.
fn py_unquote(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    // unquote_to_bytes on each ASCII run (a '%' takes exactly two hex
    // digits after it, or stays literal), then decode the run.
    fn run(ascii: &mut Vec<u8>, out: &mut String) {
        let hex = |c: Option<&u8>| c.and_then(|&c| (c as char).to_digit(16));
        let mut bytes = Vec::new();
        let mut i = 0;
        while i < ascii.len() {
            if ascii[i] == b'%' {
                if let (Some(h), Some(l)) = (hex(ascii.get(i + 1)), hex(ascii.get(i + 2))) {
                    bytes.push((h * 16 + l) as u8);
                    i += 3;
                    continue;
                }
            }
            bytes.push(ascii[i]);
            i += 1;
        }
        out.push_str(&String::from_utf8_lossy(&bytes));
        ascii.clear();
    }
    let mut out = String::new();
    let mut ascii = Vec::new();
    for c in s.chars() {
        if c.is_ascii() {
            ascii.push(c as u8);
        } else {
            run(&mut ascii, &mut out);
            out.push(c);
        }
    }
    run(&mut ascii, &mut out);
    out
}

/// `int(s)` for a port or a status code: optional sign, digits with single
/// underscores between them, surrounding whitespace allowed.
fn py_int(s: &str) -> Option<i64> {
    let t = py_strip(s);
    let (neg, digits) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__")
        || !digits.bytes().all(|c| c.is_ascii_digit() || c == b'_')
    {
        return None;
    }
    let n: i64 = digits.replace('_', "").parse().ok()?;
    Some(if neg { -n } else { n })
}

/// Where an https request goes through a proxy: the proxy's host and port,
/// and the Proxy-Authorization value if the proxy URL carries both a user
/// and a password.
#[derive(Debug, Clone, PartialEq)]
struct Tunnel {
    host: String,
    port: u16,
    auth: Option<String>,
}

/// `ProxyHandler.proxy_open` for an https URL on `host`: `_parse_proxy`
/// (which can raise, before no_proxy is even looked at), the no_proxy check,
/// the credentials, then http.client's `_get_hostport` on what is left,
/// whose default port is HTTPSConnection's 443 whatever the proxy's scheme.
/// urllib always tunnels https through CONNECT, so the proxy's own scheme
/// does not matter.
fn https_route(proxies: &std::collections::HashMap<String, String>, host: &str) -> Result<Option<Tunnel>, String> {
    let Some(proxy) = proxies.get("https") else { return Ok(None) };
    // _parse_proxy: _splittype, then the authority
    let r_scheme = match proxy.find([':', '/']) {
        Some(i) if i > 0 && proxy.as_bytes()[i] == b':' => &proxy[i + 1..],
        _ => proxy.as_str(),
    };
    let authority = if !r_scheme.starts_with('/') {
        proxy.as_str()
    } else {
        if !r_scheme.starts_with("//") {
            return Err(format!("proxy URL with no authority: {proxy:?}"));
        }
        let end = match r_scheme.find('@') {
            Some(at) => r_scheme[at..].find('/').map(|j| at + j),
            None => r_scheme[2..].find('/').map(|j| 2 + j),
        };
        &r_scheme[2..end.unwrap_or(r_scheme.len())]
    };
    let (userinfo, hostport) = match authority.rfind('@') {
        Some(i) => (Some(&authority[..i]), &authority[i + 1..]),
        None => (None, authority),
    };
    let (user, password) = match userinfo {
        Some(u) => match u.find(':') {
            Some(i) => (Some(&u[..i]), Some(&u[i + 1..])),
            None => (Some(u), None),
        },
        None => (None, None),
    };

    if !host.is_empty() && proxy_bypass_environment(host, proxies) {
        return Ok(None);
    }

    let auth = match (user, password) {
        (Some(u), Some(p)) if !u.is_empty() && !p.is_empty() => {
            Some(format!("Basic {}", b64(format!("{}:{}", py_unquote(u), py_unquote(p)).as_bytes())))
        }
        _ => None,
    };
    let hostport = py_unquote(hostport);
    // http.client's _get_hostport
    let (i, j) = (hostport.rfind(':'), hostport.rfind(']'));
    let (mut h, port) = match i {
        Some(i) if j.is_none_or(|j| i > j) => {
            let tail = &hostport[i + 1..];
            let port = match py_int(tail) {
                Some(p) => u16::try_from(p).map_err(|_| "port must be 0-65535.".to_string())?,
                None if tail.is_empty() => 443,
                None => return Err(format!("nonnumeric port: '{tail}'")),
            };
            (&hostport[..i], port)
        }
        _ => (hostport.as_str(), 443),
    };
    if h.len() >= 2 && h.starts_with('[') && h.ends_with(']') {
        h = &h[1..h.len() - 1];
    }
    Ok(Some(Tunnel { host: h.to_string(), port, auth }))
}

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The CA certificates OpenSSL's default verify paths give Python's default
/// SSL context: the file `$SSL_CERT_FILE` (default /etc/ssl/cert.pem) and
/// the hashed directories in `$SSL_CERT_DIR` (default /etc/ssl/certs,
/// ':'-separated), whose certificates OpenSSL reads from files named
/// `<subject hash>.<n>`. A file that fails to parse adds nothing, as with
/// OpenSSL; nothing here at all means no certificate can be verified.
fn openssl_default_roots(env: &Env) -> Vec<ureq::tls::Certificate<'static>> {
    fn load(path: &Path, out: &mut Vec<ureq::tls::Certificate<'static>>) {
        let Ok(pem) = std::fs::read(path) else { return };
        let mut certs = Vec::new();
        for item in ureq::tls::parse_pem(&pem) {
            match item {
                Ok(ureq::tls::PemItem::Certificate(c)) => certs.push(c),
                Ok(_) => {}
                Err(_) => return,
            }
        }
        out.extend(certs);
    }
    let mut roots = Vec::new();
    let file = env.get("SSL_CERT_FILE").unwrap_or(OsStr::new("/etc/ssl/cert.pem"));
    load(Path::new(file), &mut roots);
    let dirs = env.get("SSL_CERT_DIR").unwrap_or(OsStr::new("/etc/ssl/certs"));
    let hashed = regex::bytes::Regex::new(r"\A[0-9a-f]{8}\.[0-9]+\z").unwrap();
    for dir in dirs.as_bytes().split(|&c| c == b':').filter(|d| !d.is_empty()) {
        let Ok(entries) = std::fs::read_dir(OsStr::from_bytes(dir)) else { continue };
        let mut names: Vec<PathBuf> = entries
            .flatten()
            .filter(|e| hashed.is_match(e.file_name().as_bytes()))
            .map(|e| e.path())
            .collect();
        names.sort();
        for p in names {
            load(&p, &mut roots);
        }
    }
    roots
}

use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{Buffers, ConnectionDetails, Connector, LazyBuffers, NextTimeout, Transport};

/// A TCP socket with Python's socket timeout: every send and every recv
/// gets its own 20 seconds, whatever phase of the request it belongs to,
/// so slow headers or a slow body that keep arriving never time out.
struct PyTransport {
    stream: std::net::TcpStream,
    buffers: LazyBuffers,
}

impl std::fmt::Debug for PyTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PyTransport").field("addr", &self.stream.peer_addr().ok()).finish()
    }
}

fn io_err(e: std::io::Error, timeout: &NextTimeout) -> ureq::Error {
    match e.kind() {
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => ureq::Error::Timeout(timeout.reason),
        _ => e.into(),
    }
}

impl Transport for PyTransport {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        use std::io::Write;
        self.stream.set_write_timeout(Some(TIMEOUT))?;
        let output = &self.buffers.output()[..amount];
        self.stream.write_all(output).map_err(|e| io_err(e, &timeout))
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        use std::io::Read;
        self.stream.set_read_timeout(Some(TIMEOUT))?;
        let input = self.buffers.input_append_buf();
        let amount = self.stream.read(input).map_err(|e| io_err(e, &timeout))?;
        self.buffers.input_appended(amount);
        Ok(amount > 0)
    }

    fn is_open(&mut self) -> bool {
        // A pooled socket is reusable only if nothing is waiting on it.
        use std::io::Read;
        if self.stream.set_nonblocking(true).is_err() {
            return false;
        }
        let mut buf = [0];
        let open = matches!(self.stream.read(&mut buf), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock);
        open && self.stream.set_nonblocking(false).is_ok()
    }
}

/// `socket.create_connection(address, timeout=20)`: each address in turn,
/// each with the full 20 seconds; the last error if none answers.
fn create_connection(addrs: impl IntoIterator<Item = std::net::SocketAddr>) -> std::io::Result<std::net::TcpStream> {
    let mut err = None;
    for a in addrs {
        match std::net::TcpStream::connect_timeout(&a, TIMEOUT) {
            Ok(s) => {
                // http.client sets TCP_NODELAY, and ignores it failing.
                let _ = s.set_nodelay(true);
                return Ok(s);
            }
            Err(e) => err = Some(e),
        }
    }
    Err(err.unwrap_or_else(|| std::io::Error::other("getaddrinfo returns an empty list")))
}

/// Opens the TCP connection for each request (redirects included) the way
/// urllib would: straight to the host, or for https through the proxy
/// `https_proxy` names, with a CONNECT tunnel as http.client's `_tunnel`
/// sends it. TLS is chained on top by rustls.
///
/// An http:// URL (only reachable by a redirect) goes direct: urllib would
/// send it to `http_proxy` in absolute form, which ureq cannot.
#[derive(Debug)]
struct UrllibConnector {
    proxies: std::sync::Arc<std::collections::HashMap<String, String>>,
}

fn route(proxies: &std::collections::HashMap<String, String>, uri: &ureq::http::Uri) -> Result<Option<Tunnel>, ureq::Error> {
    if uri.scheme_str() != Some("https") {
        return Ok(None);
    }
    https_route(proxies, uri.host().unwrap_or("")).map_err(|e| ureq::Error::Io(std::io::Error::other(e)))
}

impl Connector<()> for UrllibConnector {
    type Out = PyTransport;

    fn connect(&self, details: &ConnectionDetails, _chained: Option<()>) -> Result<Option<PyTransport>, ureq::Error> {
        let buffers = LazyBuffers::new(details.config.input_buffer_size(), details.config.output_buffer_size());
        let Some(tunnel) = route(&self.proxies, details.uri)? else {
            let stream = create_connection(details.addrs.iter().copied())?;
            return Ok(Some(PyTransport { stream, buffers }));
        };
        use std::net::ToSocketAddrs;
        let addrs = (tunnel.host.as_str(), tunnel.port).to_socket_addrs()?;
        let stream = create_connection(addrs)?;
        let mut t = PyTransport { stream, buffers };
        let timeout = details.timeout;

        // http.client's _tunnel: one CONNECT, Proxy-Authorization first (if
        // any) and then Host, in a single send.
        let target_host = details.uri.host().unwrap_or("");
        let target_port = details.uri.port_u16().unwrap_or(443);
        let wrapped = if target_host.contains(':') && !target_host.starts_with('[') {
            format!("[{target_host}]")
        } else {
            target_host.to_string()
        };
        let mut req = format!("CONNECT {wrapped}:{target_port} HTTP/1.1\r\n");
        if let Some(auth) = &tunnel.auth {
            req.push_str(&format!("Proxy-Authorization: {auth}\r\n"));
        }
        req.push_str(&format!("Host: {target_host}:{target_port}\r\n\r\n"));
        {
            use std::io::Write;
            t.stream.set_write_timeout(Some(TIMEOUT))?;
            t.stream.write_all(req.as_bytes()).map_err(|e| io_err(e, &timeout))?;
        }

        // The status line, then header lines until a blank one (or the end).
        let mut lines = 0;
        loop {
            let input = t.buffers.input();
            if let Some(nl) = input.iter().position(|&c| c == b'\n') {
                let line = input[..=nl].to_vec();
                t.buffers.input_consume(nl + 1);
                if line.len() > 65536 {
                    return Err(ureq::Error::Io(std::io::Error::other("got more than 65536 bytes when reading header line")));
                }
                if lines == 0 {
                    // _read_status, and the CONNECT must answer 200
                    let text: String = line.iter().map(|&c| c as char).collect();
                    let mut parts = text.split_whitespace();
                    let version = parts.next().unwrap_or("");
                    let status = parts.next().and_then(py_int);
                    if !version.starts_with("HTTP/") || !status.is_some_and(|s| (100..=999).contains(&s)) {
                        return Err(ureq::Error::Io(std::io::Error::other(format!("BadStatusLine: {text:?}"))));
                    }
                    if status != Some(200) {
                        return Err(ureq::Error::ConnectProxyFailed(format!("Tunnel connection failed: {}", text.trim())));
                    }
                } else if line == b"\r\n" || line == b"\n" {
                    break;
                } else if lines > 100 {
                    return Err(ureq::Error::Io(std::io::Error::other("got more than 100 headers")));
                }
                lines += 1;
                continue;
            }
            if input.len() > 65536 {
                return Err(ureq::Error::Io(std::io::Error::other("header line too long")));
            }
            if !t.await_input(timeout)? {
                if lines == 0 {
                    return Err(ureq::Error::Io(std::io::Error::other(
                        "Remote end closed connection without response",
                    )));
                }
                break;
            }
        }
        Ok(Some(t))
    }
}

/// getaddrinfo for the host, with no timeout of its own, as in Python;
/// skipped when the request goes to a proxy (only the proxy is looked up).
#[derive(Debug)]
struct UrllibResolver {
    proxies: std::sync::Arc<std::collections::HashMap<String, String>>,
}

impl Resolver for UrllibResolver {
    fn resolve(&self, uri: &ureq::http::Uri, config: &ureq::config::Config, timeout: NextTimeout) -> Result<ResolvedSocketAddrs, ureq::Error> {
        if route(&self.proxies, uri)?.is_some() {
            let mut v = self.empty();
            v.push(std::net::SocketAddr::from(([0, 0, 0, 0], 0)));
            return Ok(v);
        }
        DefaultResolver::default().resolve(uri, config, timeout)
    }
}

/// `urllib.request.urlopen(req, timeout=20).read()`.
fn urlopen_read(url: &str, env: &Env) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let proxies = std::sync::Arc::new(getproxies_environment(env));
    let roots = openssl_default_roots(env);
    let tls = ureq::tls::TlsConfig::builder()
        .root_certs(ureq::tls::RootCerts::Specific(std::sync::Arc::new(roots)))
        .build();
    // No ureq timeouts and no ureq proxy: the transport times each socket
    // operation, and the connector does urllib's proxying.
    let config = ureq::Agent::config_builder().proxy(None).tls_config(tls).build();
    let connector = UrllibConnector { proxies: proxies.clone() }
        .chain(ureq::unversioned::transport::RustlsConnector::default());
    let agent = ureq::Agent::with_parts(config, connector, UrllibResolver { proxies });
    let r = agent.get(url).header("User-Agent", UA).call().map_err(|e| e.to_string())?;
    let mut body = Vec::new();
    r.into_body().into_reader().read_to_end(&mut body).map_err(|e| e.to_string())?;
    Ok(body)
}

/// The theme's own colours. Served raw, so it costs no API quota.
pub fn fetch_palette(repo: &str) -> Palette {
    fetch_palette_env(repo, &Env::current())
}

fn fetch_palette_env(repo: &str, env: &Env) -> Palette {
    let url = palette_url(repo);
    if url.is_empty() {
        return Palette::new();
    }
    let raw = match urlopen_read(&url, env).and_then(|body| tomllib::loads(&String::from_utf8_lossy(&body))) {
        Ok(t) => t,
        Err(_) => return Palette::new(),
    };
    let mut out = Palette::new();
    for (key, value) in raw {
        if let tomllib::Val::Str(value) = value {
            let v = py_strip(&value);
            if key == "mode" {
                out.insert("mode".to_string(), py_lower(v));
            } else if hex_re().is_match(v) {
                out.insert(key, v.to_string());
            }
        }
    }
    out
}

pub fn art_path(repo: &str) -> PathBuf {
    use sha1::{Digest, Sha1};
    let digest = hex::encode(Sha1::digest(repo.as_bytes()));
    to_path(py_join(art().as_os_str().as_bytes(), format!("{}.png", &digest[..16]).as_bytes()))
}

fn _rounded(ctx: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    use std::f64::consts::PI;
    ctx.new_sub_path();
    ctx.arc(x + w - r, y + r, r, -PI / 2.0, 0.0);
    ctx.arc(x + w - r, y + h - r, r, 0.0, PI / 2.0);
    ctx.arc(x + r, y + h - r, r, PI / 2.0, PI);
    ctx.arc(x + r, y + r, r, PI, 3.0 * PI / 2.0);
    ctx.close_path();
}

use gtk::cairo;

/// `os.path.splitext` on a file name: leading dots belong to the stem.
fn splitext(name: &[u8]) -> (&[u8], &[u8]) {
    let lead = name.iter().take_while(|&&c| c == b'.').count();
    match name[lead..].iter().rposition(|&c| c == b'.') {
        Some(i) => name.split_at(lead + i),
        None => (name, b""),
    }
}

/// `Path.parent` and `Path.with_suffix(suffix)` of a pathlib-normalised path;
/// with_suffix raises (None here) when the path has no name ("." or "/").
fn parent_and_with_suffix(p: &[u8], suffix: &[u8]) -> (Vec<u8>, Option<Vec<u8>>) {
    let (dir, name) = match p.iter().rposition(|&c| c == b'/') {
        Some(i) => (&p[..i + 1], &p[i + 1..]),
        None => (&b""[..], p),
    };
    let trimmed = &dir[..dir.iter().rposition(|&c| c != b'/').map_or(0, |i| i + 1)];
    let parent = match trimmed {
        b"" if dir.is_empty() => b".".to_vec(),
        b"" => dir.to_vec(),
        d => d.to_vec(),
    };
    if name.is_empty() || name == b"." {
        return (parent, None);
    }
    let (stem, _) = splitext(name);
    (parent, Some([dir, stem, suffix].concat()))
}

/// Draw a miniature desktop in the theme's colours.
///
/// Err is the exception Python lets out: the base colours are worked out
/// before its `try`, so a bad one raises to the caller; everything inside the
/// `try` (an ANSI chip, drawing, writing) falls back to None.
pub fn render(palette: &Palette, dest: &Path) -> Result<Option<PathBuf>, String> {
    if palette.is_empty() {
        return Ok(None);
    }
    // `dest` is a Path in Python, already normalised by pathlib.
    let dest = py_path(dest.as_os_str().as_bytes());

    let pick = |names: &[&str], default: &str| -> String {
        for n in names {
            if let Some(v) = palette.get(*n) {
                if !v.is_empty() {
                    return v.clone();
                }
            }
        }
        default.to_string()
    };

    let bg      = _rgb(&pick(&["background"], "#1c1e24"), GREY)?;
    let bg_dark = _rgb(&pick(&["dark_background", "background"], "#17191f"), GREY)?;
    let bg_dkr  = _rgb(&pick(&["darker_background", "dark_background"], "#111318"), GREY)?;
    let bg_lit  = _rgb(&pick(&["lighter_background", "background"], "#2d3038"), GREY)?;
    let fg      = _rgb(&pick(&["foreground"], "#f2f3f5"), GREY)?;
    let muted   = _rgb(&pick(&["muted", "dark_foreground", "foreground"], "#adb0b9"), GREY)?;
    let accent  = _rgb(&pick(&["accent", "blue"], "#78b4ff"), GREY)?;

    let rgb = |ctx: &cairo::Context, c: Rgb| ctx.set_source_rgb(c.0, c.1, c.2);
    let (wf, hf) = (W as f64, H as f64);

    let draw = || -> Result<PathBuf, Box<dyn std::error::Error>> {
        let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, W, H)?;
        {
            let ctx = cairo::Context::new(&surface)?;

            // Desktop
            rgb(&ctx, bg);
            ctx.paint()?;

            // Bar across the top, the way Omarchy wears it
            let bar_h = 26.0;
            rgb(&ctx, bg_dkr);
            ctx.rectangle(0.0, 0.0, wf, bar_h);
            ctx.fill()?;
            // Workspace pips: the active one takes the accent
            for i in 0..5 {
                rgb(&ctx, if i == 1 { accent } else { muted });
                let w = if i == 1 { 11.0 } else { 6.0 };
                _rounded(&ctx, 14.0 + i as f64 * 18.0, bar_h / 2.0 - 3.0, w, 6.0, 3.0);
                ctx.fill()?;
            }
            // Right-hand indicators
            for i in 0..3 {
                rgb(&ctx, muted);
                _rounded(&ctx, wf - 30.0 - i as f64 * 26.0, bar_h / 2.0 - 3.0, 16.0, 6.0, 3.0);
                ctx.fill()?;
            }

            // A window, with its own header
            let (wx, wy, ww, wh) = (38.0, 52.0, wf - 76.0, hf - 108.0);
            ctx.set_source_rgba(0.0, 0.0, 0.0, 0.22);
            _rounded(&ctx, wx + 3.0, wy + 4.0, ww, wh, 10.0);
            ctx.fill()?;
            rgb(&ctx, bg_lit);
            _rounded(&ctx, wx, wy, ww, wh, 10.0);
            ctx.fill()?;
            rgb(&ctx, bg_dark);
            ctx.save()?;
            _rounded(&ctx, wx, wy, ww, wh, 10.0);
            ctx.clip();
            ctx.rectangle(wx, wy, ww, 24.0);
            ctx.fill()?;
            ctx.restore()?;
            for i in 0..3 {
                rgb(&ctx, muted);
                ctx.arc(wx + 14.0 + i as f64 * 13.0, wy + 12.0, 3.2, 0.0, 2.0 * std::f64::consts::PI);
                ctx.fill()?;
            }

            // Text lines, long then short, the shape of a paragraph
            for (i, frac) in [0.74, 0.56, 0.66, 0.38].iter().enumerate() {
                rgb(&ctx, if i == 0 { fg } else { muted });
                _rounded(&ctx, wx + 16.0, wy + 42.0 + i as f64 * 15.0, (ww - 32.0) * frac, 7.0, 3.5);
                ctx.fill()?;
            }

            // The accent, as a button
            rgb(&ctx, accent);
            _rounded(&ctx, wx + ww - 92.0, wy + wh - 32.0, 76.0, 20.0, 10.0);
            ctx.fill()?;

            // The ANSI row: the colours a terminal will actually use
            let chips: Vec<String> = [
                pick(&["red"], ""),
                pick(&["yellow", "orange"], ""),
                pick(&["green"], ""),
                pick(&["cyan"], ""),
                pick(&["blue"], ""),
                pick(&["magenta"], ""),
            ]
            .into_iter()
            .filter(|c| !c.is_empty())
            .collect();
            if !chips.is_empty() {
                let cw = 34.0;
                let total = chips.len() as f64 * (cw + 8.0) - 8.0;
                let x0 = (wf - total) / 2.0;
                for (i, colour) in chips.iter().enumerate() {
                    rgb(&ctx, _rgb(colour, GREY)?);
                    _rounded(&ctx, x0 + i as f64 * (cw + 8.0), hf - 36.0, cw, 14.0, 7.0);
                    ctx.fill()?;
                }
            }
        }

        let (parent, tmp) = parent_and_with_suffix(&dest, b".tmp.png");
        std::fs::create_dir_all(to_path(parent))?;
        let tmp = to_path(tmp.ok_or("path has an empty name")?);
        // surface.write_to_png: cairo's own encoder, as pycairo uses it.
        surface.write_to_png(&mut std::fs::File::create(&tmp)?)?;
        let dest = to_path(dest.clone());
        std::fs::rename(&tmp, &dest)?;
        Ok(dest)
    };
    Ok(draw().ok())
}

/// A cached preview for a theme repository, drawing it if needed.
pub fn thumbnail(repo: &str) -> Option<PathBuf> {
    let dest = art_path(repo);
    if std::fs::metadata(&dest).map(|m| m.len() > 0).unwrap_or(false) {
        return Some(dest);
    }
    // Python lets render's exception through here too, but it cannot happen:
    // fetch_palette strips every value, so no "#abc\n" reaches render.
    render(&fetch_palette(repo), &dest).ok().flatten()
}

/// Parity helper: sha1 of a PNG's pixels as cairo holds them (B,G,R,A
/// premultiplied, row by row), so both sides compare what was drawn rather
/// than how each encoder compressed it; plus the file's own sha1, since the
/// encoder is now the same one.
fn png_pixels(path: &Path) -> serde_json::Value {
    use gtk::gdk;
    use gtk::prelude::*;
    use sha1::{Digest, Sha1};
    let Ok(texture) = gdk::Texture::from_filename(path) else {
        return serde_json::Value::Null;
    };
    let stride = texture.width() as usize * 4;
    let mut buf = vec![0u8; stride * texture.height() as usize];
    texture.download(&mut buf, stride);
    serde_json::json!({
        "size": [texture.width(), texture.height()],
        "sha1": hex::encode(Sha1::digest(&buf)),
    })
}

fn path_result(p: Option<PathBuf>) -> serde_json::Value {
    match p {
        Some(p) => serde_json::json!({
            "path": p.to_string_lossy(),
            "pixels": png_pixels(&p),
            "file_sha1": std::fs::read(&p).ok().map(|b| {
                use sha1::{Digest, Sha1};
                hex::encode(Sha1::digest(&b))
            }),
        }),
        None => serde_json::Value::Null,
    }
}

fn palette_json(p: &Palette) -> serde_json::Value {
    serde_json::Value::Object(p.iter().map(|(k, v)| (k.clone(), serde_json::json!(v))).collect())
}

/// Parity hook: see ARCHITECTURE.md.
pub fn parity(func: &str, args: &serde_json::Value) -> Option<serde_json::Value> {
    use crate::util::arg_str;
    let palette = || -> Palette {
        let mut v: Vec<(String, String)> = crate::util::str_map(args.get("palette").unwrap_or(&serde_json::Value::Null))
            .into_iter()
            .collect();
        v.sort();
        v.into_iter().collect()
    };
    Some(match func {
        "palette_url" => serde_json::json!(palette_url(arg_str(args, "repo"))),
        "fetch_palette" => {
            // "_env": variables the Python side ran with, in a subprocess.
            let mut env = Env::current();
            if let Some(extra) = args.get("_env").and_then(|e| e.as_object()) {
                for (k, v) in extra {
                    env.set(k, v.as_str().unwrap_or_default());
                }
            }
            let p = fetch_palette_env(arg_str(args, "repo"), &env);
            if args.get("_keys").is_some() {
                serde_json::json!(p.keys().collect::<Vec<_>>())
            } else {
                palette_json(&p)
            }
        }
        "art_path" => serde_json::json!(art_path(arg_str(args, "repo")).to_string_lossy()),
        "_rgb" => {
            match _rgb(arg_str(args, "value"), GREY) {
                Ok(c) => serde_json::json!([c.0, c.1, c.2]),
                Err(e) => serde_json::json!({"error": e}),
            }
        }
        "render" => match render(&palette(), Path::new(arg_str(args, "dest"))) {
            Ok(p) => path_result(p),
            Err(e) => serde_json::json!({"error": e}),
        },
        "thumbnail" => path_result(thumbnail(arg_str(args, "repo"))),
        "lower" => serde_json::json!(py_lower(arg_str(args, "s"))),
        "toml_ok" => serde_json::json!(tomllib::loads(arg_str(args, "text")).is_ok()),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_url_needs_owner_and_name() {
        assert_eq!(palette_url("https://github.com/a/b"), "https://raw.githubusercontent.com/a/b/HEAD/colors.toml");
        assert_eq!(palette_url("https://github.com/a"), "");
        assert_eq!(palette_url(""), "");
    }

    #[test]
    fn rgb_parses_short_and_long_hex() {
        assert_eq!(_rgb("#fff", GREY), Ok((1.0, 1.0, 1.0)));
        assert_eq!(_rgb("#000000", GREY), Ok((0.0, 0.0, 0.0)));
        assert_eq!(_rgb("nope", GREY), Ok(GREY));
        assert_eq!(_rgb("", GREY), Ok(GREY));
        assert!(_rgb("#abcdef\n", GREY).is_ok());
        assert_eq!(_rgb("#abc\n", GREY), Err("invalid literal for int() with base 16: ''".to_string()));
    }

    #[test]
    fn render_writes_a_png_and_skips_empty_palettes() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("sub/x.png");
        assert_eq!(render(&Palette::new(), &dest), Ok(None));
        let mut p = Palette::new();
        p.insert("background".to_string(), "#101010".to_string());
        p.insert("red".to_string(), "#ff0000".to_string());
        assert_eq!(render(&p, &dest), Ok(Some(dest.clone())));
        let png = std::fs::read(&dest).unwrap();
        assert!(!png.is_empty());
        assert!(!dest.with_extension("tmp.png").exists());
        // cairo's encoder: an opaque surface is 8-bit RGB (colour type 2)
        // with a bKGD chunk, as pycairo writes it.
        assert_eq!((png[24], png[25]), (8, 2));
        assert!(png.windows(4).any(|w| w == b"bKGD"));
    }

    #[test]
    fn paths_normalise_like_pathlib() {
        for (raw, want) in [("//a//b/", "//a/b"), ("///a", "/a"), ("a/./b/.", "a/b"), ("./a", "a"),
                            (".", "."), ("", "."), ("/", "/"), ("//", "//"), ("a/../b", "a/../b")] {
            assert_eq!(py_path(raw.as_bytes()), want.as_bytes(), "{raw:?}");
        }
        assert_eq!(py_join(b".", b"almanac"), b"almanac");
        assert_eq!(py_join(b"/", b"x"), b"/x");
        assert_eq!(py_join(b"/tmp/\xff", b"almanac"), b"/tmp/\xff/almanac");
        let sfx = |p: &str, s: &str| {
            let (a, b) = parent_and_with_suffix(p.as_bytes(), s.as_bytes());
            (String::from_utf8(a).unwrap(), b.map(|b| String::from_utf8(b).unwrap()))
        };
        for (name, want) in [("x.", "x.tmp.png"), ("a.b.png", "a.b.tmp.png"), (".png", ".png.tmp.png"),
                             ("x", "x.tmp.png"), ("..", "...tmp.png"), ("a..png", "a..tmp.png"),
                             (".a.b", ".a.tmp.png"), ("x.png.", "x.png.tmp.png")] {
            assert_eq!(sfx(name, ".tmp.png"), (".".to_string(), Some(want.to_string())));
        }
        assert_eq!(sfx("/a/b.png", ".t"), ("/a".to_string(), Some("/a/b.t".to_string())));
        assert_eq!(sfx("//b.png", ".t"), ("//".to_string(), Some("//b.t".to_string())));
        assert_eq!(sfx(".", ".t").1, None);
        assert_eq!(sfx("/", ".t").1, None);
    }

    #[test]
    fn cache_dir_follows_pathlib() {
        let os = |s: &[u8]| OsStr::from_bytes(s).to_os_string();
        let c = |x: Option<&[u8]>, h: Option<&[u8]>| cache_from(x.map(os).as_deref(), h.map(os).as_deref());
        // Values from Python 3.14 with the same environment.
        assert_eq!(c(None, Some(b"")), b"/.cache/almanac");
        assert_eq!(c(None, Some(b"//")), b"/.cache/almanac");
        assert_eq!(c(None, Some(b"/home/x//")), b"/home/x/.cache/almanac");
        assert_eq!(c(None, Some(b"rel")), b"rel/.cache/almanac");
        assert_eq!(c(Some(b""), Some(b"/h")), b"almanac");
        assert_eq!(c(Some(b"/tmp/\xff/"), Some(b"/h")), b"/tmp/\xff/almanac");
        if let Ok(Some(u)) = nix::unistd::User::from_uid(nix::unistd::getuid()) {
            let want = [u.dir.as_os_str().as_bytes(), b"/.cache/almanac"].concat();
            assert_eq!(c(None, None), want);
        }
    }

    #[test]
    fn strip_matches_python() {
        assert_eq!(py_strip("#0000ff\u{1f}"), "#0000ff");
        assert_eq!(py_strip("\u{1c} #fff\u{3000}\u{85}"), "#fff");
    }

    #[test]
    fn lower_uses_python_unicode_tables() {
        assert_eq!(py_lower("DARK"), "dark");
        // New in Unicode 17: Python 3.14 leaves them alone.
        assert_eq!(py_lower("\u{A7CE}\u{16EA0}X"), "\u{A7CE}\u{16EA0}x");
        // Final sigma: U+A7CE is not cased to Python, so the sigma after it
        // is not word-final; U+0295 is cased, so the one after it is.
        assert_eq!(py_lower("\u{A7CE}\u{3a3}"), "\u{A7CE}\u{3c3}");
        assert_eq!(py_lower("\u{295}\u{3a3}"), "\u{295}\u{3c2}");
        assert_eq!(py_lower("A\u{3a3}"), "a\u{3c2}");
        assert_eq!(py_lower("\u{130}X"), "i\u{307}x");
    }

    #[test]
    fn toml_is_parsed_as_tomllib_does() {
        let ok = |s: &str| tomllib::loads(s).is_ok();
        assert!(!ok("\u{feff}red=\"#fff\""));
        assert!(!ok("red=\"\\e\"\nblue=\"#00f\""));
        assert!(!ok("x=\"\\x41\""));
        assert!(!ok("t={a=1,\n b=2}\nblue=\"#00f\""));
        assert!(!ok("t={a=1,}\n"));
        assert!(!ok("t=07:32\n"));
        assert!(ok("red=\"#0000ff\\u001f\""));
        // Integers of any size, float overflow to inf.
        assert!(ok("x = 99999999999999999999"));
        assert!(ok("x = -9223372036854775809"));
        assert!(ok("x = 0xffffffffffffffffff"));
        assert!(ok("x = 1e400"));
        assert!(ok(&format!("x = {}", "1".repeat(4300))));
        assert!(!ok(&format!("x = {}", "1".repeat(4301))));
        assert!(!ok(&format!("x = {}1", "1_".repeat(4300))));
        assert!(ok(&format!("x = 0x{}", "f".repeat(5000))));
        // datetime() rules, and no leap seconds.
        assert!(!ok("x = 0000-01-01"));
        assert!(!ok("x = 1979-05-27T07:32:60Z"));
        assert!(!ok("x = 07:32:60"));
        assert!(!ok("x = 2023-02-29"));
        assert!(ok("x = 2024-02-29"));
        assert!(ok("x = 1979-05-27T07:32:00.999999-07:00"));
        // Tables, arrays of tables and the redefinition rules.
        assert!(ok("[a]\nb=1\n[a.c]\n[[d]]\n[[d]]\ne.f=\"x\""));
        assert!(!ok("[a]\n[a]"));
        assert!(!ok("a={b=1}\n[a.c]"));
        assert!(!ok("a.b=1\n[a]"));
        assert!(!ok("a=1\na=2"));
        assert!(!ok("a=[]\n[[a]]"));
        // Python's recursion limit, as reached from the app's worker thread.
        let nest = |n: usize| format!("x = {}{}", "[".repeat(n), "]".repeat(n));
        assert!(ok(&nest(492)));
        assert!(!ok(&nest(493)));
        let t = tomllib::loads("mode = \"Dark\"\naccent = '#fff'\nn = 1\n[t]\nred = \"#f00\"").unwrap();
        assert_eq!(t.keys().collect::<Vec<_>>(), ["mode", "accent", "n", "t"]);
    }

    #[test]
    fn proxies_follow_urllib() {
        let env = |pairs: &[(&str, &str)]| Env(pairs.iter().map(|(k, v)| ((*k).into(), (*v).into())).collect());
        let p = getproxies_environment(&env(&[("http_proxy", "http://h:1"), ("ALL_PROXY", "http://a:1")]));
        assert_eq!(https_route(&p, "raw.githubusercontent.com"), Ok(None));
        let p = getproxies_environment(&env(&[("HTTPS_PROXY", "http://h:1"), ("https_proxy", "")]));
        assert_eq!(p.get("https"), None);
        let p = getproxies_environment(&env(&[("https_proxy", "http://h:1"), ("HTTPS_PROXY", "http://x:2")]));
        assert_eq!(p.get("https").map(String::as_str), Some("http://h:1"));
        let route = |proxy: &str, no: &str| {
            let mut m = std::collections::HashMap::new();
            m.insert("https".to_string(), proxy.to_string());
            if !no.is_empty() {
                m.insert("no".to_string(), no.to_string());
            }
            https_route(&m, "raw.githubusercontent.com")
        };
        let t = |host: &str, port: u16, auth: Option<&str>| Ok(Some(Tunnel { host: host.into(), port, auth: auth.map(String::from) }));
        assert_eq!(route("http://p:3128", ""), t("p", 3128, None));
        assert_eq!(route("p:3128", ""), t("p", 3128, None));
        assert_eq!(route("http://p", ""), t("p", 443, None));
        assert_eq!(route("http://p:", ""), t("p", 443, None));
        assert_eq!(route("socks5://[::1]:1080/x", ""), t("::1", 1080, None));
        assert_eq!(route("http://u:p%40ss@p:8", ""), t("p", 8, Some("Basic dTpwQHNz")));
        assert_eq!(route("http://u@p:8", ""), t("p", 8, None));
        assert_eq!(route("http://p:8", "githubusercontent.com"), Ok(None));
        assert_eq!(route("http://p:8", ".GitHubUserContent.com, x"), Ok(None));
        assert_eq!(route("http://p:8", "*"), Ok(None));
        assert_eq!(route("http://p:8", "hubusercontent.com"), t("p", 8, None));
        assert!(route("http:/p", "*").is_err());
        assert!(route("http://p:x", "").is_err());
        assert!(route("http://p:99999", "").is_err());
    }

    #[test]
    fn unquote_and_base64() {
        assert_eq!(py_unquote("a%40b%zz%4"), "a@b%zz%4");
        assert_eq!(py_unquote("%C3%A9é%ff"), "éé\u{fffd}");
        assert_eq!(b64(b"u:p@ss"), "dTpwQHNz");
        assert_eq!(b64(b"ab"), "YWI=");
        assert_eq!(b64(b"a"), "YQ==");
    }

    #[test]
    fn roots_follow_openssl_paths() {
        let env = |pairs: &[(&str, &str)]| Env(pairs.iter().map(|(k, v)| ((*k).into(), (*v).into())).collect());
        assert!(openssl_default_roots(&env(&[("SSL_CERT_FILE", "/dev/null"), ("SSL_CERT_DIR", "/nonexistent")])).is_empty());
        if Path::new("/etc/ssl/cert.pem").exists() {
            assert!(!openssl_default_roots(&env(&[("SSL_CERT_DIR", "/nonexistent")])).is_empty());
        }
    }

    #[test]
    fn lossy_utf8_matches_python_replace() {
        // Counts of U+FFFD from Python's decode("utf-8", "replace").
        for (bytes, want) in [(&b"\xf0\x80\x80"[..], 3), (b"\xe2\x82", 1), (b"\xed\xa0\x80", 3),
                              (b"\xc0\xaf", 2), (b"\xf4\x90\x80\x80", 4), (b"a\xffb", 1),
                              (b"\xe2\x82\xe2\x82\xac", 1), (b"\xf8\x88\x80\x80\x80", 5), (b"\xf0\x9f\x98", 1)] {
            assert_eq!(String::from_utf8_lossy(bytes).matches('\u{fffd}').count(), want, "{bytes:?}");
        }
    }
}
