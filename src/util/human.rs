//! Python's error text, in plain English, where a user reads it.
//!
//! The core modules spell their `Err`s the way the 1.x Python raised them
//! ("TypeError: 'int' object is not subscriptable", "[Errno 2] No such file
//! or directory: '/x'"), so the tests can pin their logic against what the
//! Python did. That spelling is for the code, not for people: the display
//! points (toasts, notifications, rows, the task log) pass what they show
//! through [`human_error`], which rewrites only what it recognises and
//! leaves every other text as it was. The producers stay Python-faithful.

use regex::Regex;
use std::sync::LazyLock;

/// The exception classes the ports spell, as a prefix ("KeyError: 'x'") or
/// alone ("TypeError").
const CLASSES: &str = concat!(
    r"socket\.gaierror|socket\.timeout|json\.decoder\.JSONDecodeError",
    r"|IndexError|KeyError|TypeError|AttributeError|ValueError|LookupError",
    r"|StopIteration|AssertionError|RecursionError|OverflowError|RuntimeError",
    r"|UnicodeDecodeError|UnicodeEncodeError|UnicodeError|JSONDecodeError",
    r"|OSError|FileNotFoundError|PermissionError|IsADirectoryError",
    r"|NotADirectoryError|FileExistsError|TimeoutError|ConnectionError",
    r"|URLError|HTTPError|HTTPException",
);

static CLASS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"^({CLASSES})(?:: (.*))?$")).unwrap());

/// `[Errno N] strerror`, then `: 'path'` and, for a rename or a link,
/// ` -> 'path'` (Python's repr: single quotes, or double where the path
/// holds a single one).
static ERRNO: LazyLock<Regex> = LazyLock::new(|| {
    let path = r#"'(?:[^'\\]|\\.)*'|"(?:[^"\\]|\\.)*""#;
    Regex::new(&format!(r"^\[Errno (-?\d+|None)\] ([^:]*?)(?:: ({path})(?: -> ({path}))?)?$")).unwrap()
});

static URLOPEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^<urlopen error (.*)>$").unwrap());

/// What the Python said about data of the wrong shape: a cache, a state
/// file or an answer from a server holding something the code did not
/// expect. Nothing in it helps a user; that it happened does.
static SHAPE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"^(?:list index out of range|string index out of range|tuple index out of range",
        r"|'[^']*' object is not (?:subscriptable|iterable|callable)",
        r"|'[^']*' object has no attribute '[^']*'",
        r"|'[^']*' object does not support item assignment",
        r"|unhashable type: '[^']*'",
        r"|cannot use '[^']*' as a dict key.*",
        r"|argument of type '[^']*' is not iterable",
        r"|object of type '[^']*' has no len\(\)",
        r"|'[<>=!]+' not supported between instances of .*",
        r"|unsupported operand type\(s\) for .*",
        r"|list indices must be integers or slices, not .*",
        r"|sequence item \d+: expected str instance, .* found",
        r"|can only join an iterable",
        r"|expected str, bytes or os\.PathLike object, not .*",
        r"|expected string or bytes-like object.*",
        r"|argument should be a str or an os\.PathLike object.*",
        r"|int\(\) argument must be .*",
        r"|header value is None",
        r"|(?:not enough|too many) values to unpack.*",
        r"|dictionary update sequence element #\d+ has length \d+; 2 is required",
        r"|invalid literal for int\(\) with base \d+: .*",
        r"|could not convert string to float: .*",
        r"|cannot convert float (?:NaN|infinity) to integer",
        r"|Exceeds the limit \(\d+ digits\) for integer string conversion.*",
        r")$"
    ))
    .unwrap()
});

static DEEP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:maximum recursion depth exceeded.*|Stack overflow \(used \d+ kB\) while .*)$").unwrap()
});

static DECODE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^'([^']+)' codec can't decode .*$").unwrap());
static ENCODE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^'[^']+' codec can't encode .*$").unwrap());
static JSON: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:Expecting .*|Extra data|Unterminated string .*|Invalid .*): line \d+ column \d+ \(char \d+\)$").unwrap());
static NUL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:\w+: )?embedded null (?:byte|character)(?: in path)?$").unwrap());

fn tr(text: &str) -> String {
    crate::i18n::try_tr(text).unwrap_or_else(|_| text.to_string())
}

/// A Python repr of a str (`'a\'b'`, `"it's"`) as the text it stands for.
fn unquote(repr: &str) -> String {
    let inner = &repr[1..repr.len() - 1];
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(c @ ('\\' | '\'' | '"')) => out.push(c),
            Some(c) => {
                out.push('\\');
                out.push(c);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// What a failed decode says, by codec: only UTF-8 is worth naming, every
/// other one ("'ascii' codec can't decode ...") is not about UTF-8 at all.
fn decode_text(codec: &str) -> String {
    let norm = codec.to_ascii_lowercase().replace(['-', '_'], "");
    if norm == "utf8" {
        tr("The file is not valid UTF-8")
    } else {
        tr("The text could not be decoded")
    }
}

/// `after` is the text from the start of `strerror` to the end of the error,
/// as written.
fn errno_text(n: i64, strerror: &str, path: Option<&str>, to: Option<&str>, after: &str) -> String {
    let path = path.map(unquote);
    let to = to.map(unquote);
    let with = |text: String| match (&path, &to) {
        (Some(p), Some(t)) => format!("{text}: {p} → {t}"),
        (Some(p), None) => format!("{text}: {p}"),
        _ => text,
    };
    // Only the OS's own words are replaced: with no path to end it, the
    // match runs to the end of the text, and what follows them there
    // ("; continuing without one") is someone else's sentence. A path is
    // the OS's only when it follows those words directly (`No such file or
    // directory: 'p'`); after anything else ("; extra: 'p'") it is part of
    // that sentence and stays where it was, as written -- it used to be
    // moved in front of it ("No such file: p; extra").
    let stock = |own: &str, plain: &str| match strerror.strip_prefix(own) {
        Some("") => with(tr(plain)),
        Some(_) => format!("{}{}", tr(plain), human_line(&after[own.len()..])),
        None => with(tr(plain)),
    };
    match n {
        2 => stock("No such file or directory", "No such file"),
        13 => stock("Permission denied", "Permission denied"),
        // The OS's own wording is plain enough; the brackets and quotes go.
        _ if strerror.is_empty() || strerror == "None" => with(tr("The system refused")),
        _ => with(human_line(strerror)),
    }
}

/// The plain text for one recognised error, or None when `text` is not one.
fn known(text: &str) -> Option<String> {
    if let Some(c) = ERRNO.captures(text) {
        let n = c[1].parse().unwrap_or(0);
        let after = &text[c.get(2).map_or(text.len(), |m| m.start())..];
        return Some(errno_text(n, &c[2], c.get(3).map(|m| m.as_str()), c.get(4).map(|m| m.as_str()), after));
    }
    if let Some(c) = URLOPEN.captures(text) {
        return Some(human_line(&c[1]));
    }
    if DEEP.is_match(text) {
        return Some(tr("The data is nested too deeply to read"));
    }
    if let Some(c) = DECODE.captures(text) {
        return Some(decode_text(&c[1]));
    }
    if ENCODE.is_match(text) {
        return Some(tr("The text has characters that cannot be written out"));
    }
    if JSON.is_match(text) {
        return Some(tr("The data is not valid JSON"));
    }
    if NUL.is_match(text) {
        return Some(tr("A name contains a NUL character"));
    }
    if SHAPE.is_match(text) {
        return Some(tr("Unexpected data"));
    }
    if text == "timed out" || text == "The read operation timed out" {
        return Some(tr("The connection timed out"));
    }
    let c = CLASS.captures(text)?;
    let detail = c.get(2).map_or("", |m| m.as_str()).trim();
    if let Some(plain) = known(detail) {
        return Some(plain);
    }
    let class = c[1].rsplit('.').next().unwrap_or("");
    Some(match class {
        "RecursionError" => tr("The data is nested too deeply to read"),
        "UnicodeDecodeError" if detail.is_empty() => tr("The file is not valid UTF-8"),
        "JSONDecodeError" => tr("The data is not valid JSON"),
        "OverflowError" => tr("A number in the data is out of range"),
        "HTTPException" => tr("Unexpected answer from the server"),
        "TimeoutError" | "timeout" => tr("The connection timed out"),
        "LookupError" if detail.starts_with("unknown encoding: ") => {
            format!("{}: {}", tr("Unknown text encoding"), &detail["unknown encoding: ".len()..])
        }
        // What follows these is Python's own wording (a key's repr, "'x'
        // object is not ..."): it says nothing to a user.
        "IndexError" | "KeyError" | "TypeError" | "AttributeError" | "LookupError" => tr("Unexpected data"),
        // Python raises these bare; a message is the code's own sentence
        // (`assert ok, "x: no loader"`) and stays, as ValueError's does.
        "StopIteration" | "AssertionError" if detail.is_empty() => tr("Unexpected data"),
        _ if detail.is_empty() => tr("Something went wrong"),
        // ValueError, OSError, URLError, HTTPError, RuntimeError, ...: what
        // follows the class is already a sentence ("HTTP Error 404: Not
        // Found", "Invalid IPv6 URL"). It is read as a line of its own, so
        // an error inside it ("one (two: KeyError: 'z') three") is reworded
        // in the same pass, not left for a second one to find.
        _ => human_line(detail),
    })
}

/// Where an error that starts `rest` can end: before the ")" closing a
/// "(" left open in `before` ("no snapshot was taken ([Errno 13] ...);
/// continuing"), else before Almanac's own " — " ("x: TypeError —
/// skipped"), else at the end of the line.
///
/// A quoted key or path the error starts with (`KeyError: ' — '`,
/// `[Errno 2] ...: 'a (b'`) is one unit: neither end is looked for inside
/// it. Cut at the " — " in `KeyError: ' — ' — skipped`, the error was
/// `KeyError: '` and the rest of the key was left behind as a fragment.
fn extent(before: &str, rest: &str) -> usize {
    let from = QUOTED_LEAD.find(rest).map_or(0, |m| m.end());
    let open = before.chars().fold(0usize, |d, c| match c {
        '(' => d + 1,
        ')' => d.saturating_sub(1),
        _ => d,
    });
    if open > 0 {
        let mut depth = 0usize;
        for (i, c) in rest[from..].char_indices() {
            match c {
                '(' => depth += 1,
                ')' if depth == 0 => return from + i,
                ')' => depth -= 1,
                _ => {}
            }
        }
    }
    rest[from..].find(" — ").map_or(rest.len(), |i| from + i)
}

/// The start of an error that ends in a Python repr: a `KeyError`'s key,
/// an `[Errno N]`'s path (and its ` -> ` target), behind any classes.
static QUOTED_LEAD: LazyLock<Regex> = LazyLock::new(|| {
    let repr = r#"(?:'(?:[^'\\]|\\.)*'|"(?:[^"\\]|\\.)*")"#;
    Regex::new(&format!(
        r"^(?:(?:{CLASSES}): )*(?:KeyError: {repr}|\[Errno (?:-?\d+|None)\] [^:]*?: {repr}(?: -> {repr})?)"
    ))
    .unwrap()
});

/// One line: the first place, from the start or after a separator ("X
/// failed: ...", "skipped — ..."), where a recognised error begins; what
/// precedes it is the app's own wording and stays, and so does what
/// follows it (see `extent`), which is read the same way.
fn human_line(line: &str) -> String {
    let mut at = 0;
    loop {
        let rest = &line[at..];
        let end = extent(&line[..at], rest);
        if let Some(plain) = known(&rest[..end]) {
            return format!("{}{plain}{}", &line[..at], human_line(&rest[end..]));
        }
        let next = [": ", " — ", "("]
            .iter()
            .filter_map(|sep| rest.find(sep).map(|i| i + sep.len()))
            .min();
        match next {
            Some(n) => at += n,
            None => return line.to_string(),
        }
    }
}

/// `text` with every Python-style error in it put in plain English (see
/// the module comment). Text with none is returned unchanged.
/// Past this length a line is shown as it is (see `human_error`).
const LONGEST_ERROR_LINE: usize = 4096;

pub fn human_error(text: &str) -> String {
    if !text.contains("Error")
        && !text.contains('\'')
        && !text.contains("Errno")
        && !text.contains("timed out")
        && !text.contains("Stack overflow")
        && !text.contains("StopIteration")
        && !text.contains("HTTPException")
        && !text.contains("embedded null")
        && !text.contains("line ")
        && !text.contains("can only join")
        && !text.contains("header value")
        && !text.contains("values to unpack")
        && !text.contains("index out of range")
        && !text.contains("unknown encoding")
        && !text.contains("socket.")
        && !text.contains("Exceeds the limit")
    {
        return text.to_string();
    }
    // A line longer than any error message is someone's data, not an error to
    // reword -- and reading one nested thousands of brackets deep would recurse
    // that deep.
    let pass = |t: &str| {
        t.split('\n').map(|l| if l.len() > LONGEST_ERROR_LINE { l.to_string() } else { human_line(l) }).collect::<Vec<_>>().join("\n")
    };
    // A display point may pass a text through here twice (a toast of a task
    // log line), and the second pass must change nothing. One pass reads
    // what an error keeps (its sentence, the rest of the line) as well, so
    // it is normally the last; the next ones make sure, a few at most.
    let mut out = pass(text);
    for _ in 0..4 {
        let next = pass(&out);
        if next == out {
            break;
        }
        out = next;
    }
    out
}

#[cfg(test)]
mod tests {
    /// `human_error`, held to idempotence on every input any test gives it:
    /// what it returns, passed through again, comes back the same.
    fn h(s: &str) -> String {
        let once = super::human_error(s);
        assert_eq!(super::human_error(&once), once, "not idempotent for {s:?}");
        once
    }

    /// The third review's probes, with the exact text each now gives.
    #[test]
    fn nested_quoted_and_trailing_paths() {
        // An error inside a kept sentence is reworded in the first pass.
        assert_eq!(h("a: ValueError: one (two: KeyError: 'z') three"), "a: one (two: Unexpected data) three");
        // A key holding Almanac's own separator is one unit.
        assert_eq!(h("x: KeyError: ' — ' — skipped"), "x: Unexpected data — skipped");
        assert_eq!(h("a (KeyError: ')') b"), "a (Unexpected data) b");
        assert_eq!(h("x: [Errno 2] No such file or directory: 'a — b' — skipped"), "x: No such file: a — b — skipped");
        // A path is the OS's only right after its words.
        assert_eq!(h("[Errno 2] No such file or directory; extra: 'p'"), "No such file; extra: 'p'");
        assert_eq!(h("[Errno 13] Permission denied; then: 'q'"), "Permission denied; then: 'q'");
        assert_eq!(h("[Errno 2] No such file or directory: 'p'"), "No such file: p");
        // An errno's own wording is read for errors too.
        assert_eq!(h("[Errno 5] x (TypeError) y"), "x (Unexpected data) y");
    }

    #[test]
    fn errno_with_a_path() {
        assert_eq!(h("[Errno 2] No such file or directory: '/tmp/x'"), "No such file: /tmp/x");
        assert_eq!(h("FileNotFoundError: [Errno 2] No such file or directory: '/a b'"), "No such file: /a b");
        assert_eq!(h("[Errno 13] Permission denied: '/etc/shadow'"), "Permission denied: /etc/shadow");
        assert_eq!(h("PermissionError: [Errno 13] Permission denied: \"it's\""), "Permission denied: it's");
        assert_eq!(h("[Errno 2] No such file or directory"), "No such file");
        assert_eq!(h("OSError: [Errno 18] Invalid cross-device link: 'a' -> 'b'"), "Invalid cross-device link: a → b");
        assert_eq!(h("OSError: [Errno 75] Value too large for defined data type"), "Value too large for defined data type");
        assert_eq!(h("[Errno None] None: 'x'"), "The system refused: x");
        // Inside the app's own sentence, which stays.
        assert_eq!(
            h("Could not start the update: [Errno 2] No such file or directory: 'kitty'"),
            "Could not start the update: No such file: kitty"
        );
        assert_eq!(h("could not start: [Errno 13] Permission denied: 'yay'"), "could not start: Permission denied: yay");
    }

    #[test]
    fn data_of_the_wrong_shape() {
        assert_eq!(h("IndexError: list index out of range"), "Unexpected data");
        assert_eq!(h("list index out of range"), "Unexpected data");
        assert_eq!(h("TypeError: 'int' object is not subscriptable"), "Unexpected data");
        assert_eq!(h("TypeError: '<' not supported between instances of 'int' and 'NoneType'"), "Unexpected data");
        assert_eq!(h("AttributeError: 'str' object has no attribute 'get'"), "Unexpected data");
        assert_eq!(h("KeyError: 'unit_file'"), "Unexpected data");
        assert_eq!(h("TypeError"), "Unexpected data");
        assert_eq!(h("ValueError: invalid literal for int() with base 10: '1.5'"), "Unexpected data");
        assert_eq!(h("ValueError: not enough values to unpack (expected 2, got 1)"), "Unexpected data");
        assert_eq!(h("GridCraft was not installed: TypeError: unhashable type: 'list'"), "GridCraft was not installed: Unexpected data");
        assert_eq!(h("OverflowError: cannot convert float infinity to integer"), "Unexpected data");
        assert_eq!(h("OverflowError: timestamp out of range for platform time_t"), "A number in the data is out of range");
    }

    #[test]
    fn nesting_text_and_json() {
        assert_eq!(h("RecursionError: maximum recursion depth exceeded"), "The data is nested too deeply to read");
        assert_eq!(
            h("Check failed: RecursionError: Stack overflow (used 8156 kB) while decoding a JSON array from a unicode string"),
            "Check failed: The data is nested too deeply to read"
        );
        assert_eq!(h("RecursionError"), "The data is nested too deeply to read");
        assert_eq!(
            h("UnicodeDecodeError: 'utf-8' codec can't decode byte 0xff in position 0: invalid start byte"),
            "The file is not valid UTF-8"
        );
        assert_eq!(h("'utf-8' codec can't decode bytes in position 2-3: unexpected end of data"), "The file is not valid UTF-8");
        // A message of its own is kept.
        assert_eq!(h("UnicodeDecodeError: theme.name is not UTF-8"), "theme.name is not UTF-8");
        assert_eq!(
            h("UnicodeEncodeError: 'utf-8' codec can't encode character '\\udcff' in position 1: surrogates not allowed"),
            "The text has characters that cannot be written out"
        );
        assert_eq!(h("JSONDecodeError: Expecting value: line 1 column 1 (char 0)"), "The data is not valid JSON");
        assert_eq!(h("Expecting ',' delimiter: line 3 column 5 (char 20)"), "The data is not valid JSON");
        assert_eq!(h("LookupError: unknown encoding: klingon"), "Unknown text encoding: klingon");
    }

    #[test]
    fn network_and_the_rest() {
        assert_eq!(h("URLError: <urlopen error timed out>"), "The connection timed out");
        assert_eq!(h("URLError: <urlopen error [Errno -2] Name or service not known>"), "Name or service not known");
        assert_eq!(h("URLError: <urlopen error no host given>"), "no host given");
        assert_eq!(h("HTTPError: HTTP Error 404: Not Found"), "HTTP Error 404: Not Found");
        assert_eq!(h("HTTPException: got more than 100 headers"), "Unexpected answer from the server");
        assert_eq!(h("TimeoutError: timed out"), "The connection timed out");
        assert_eq!(h("socket.gaierror: failed to lookup address information"), "failed to lookup address information");
        assert_eq!(h("ValueError: embedded null byte"), "A name contains a NUL character");
        assert_eq!(h("ValueError: stat: embedded null character in path"), "A name contains a NUL character");
        assert_eq!(h("ValueError: Invalid IPv6 URL"), "Invalid IPv6 URL");
        assert_eq!(h("RuntimeError: can't start new thread"), "can't start new thread");
        assert_eq!(h("OSError"), "Something went wrong");
        assert_eq!(h("pkg: skipped — KeyError: 'x'"), "pkg: skipped — Unexpected data");
    }

    #[test]
    fn what_surrounds_an_error_stays() {
        // snapshots.rs: the error in brackets, the app's sentence after it.
        assert_eq!(
            h("no snapshot was taken ([Errno 13] Permission denied); continuing without one"),
            "no snapshot was taken (Permission denied); continuing without one"
        );
        assert_eq!(
            h("no snapshot was taken ([Errno 2] No such file or directory: 'snapper'); continuing without one"),
            "no snapshot was taken (No such file: snapper); continuing without one"
        );
        let timed_out = "no snapshot was taken (Command '['pkexec', 'snapper']' timed out after 120 seconds); continuing without one";
        assert_eq!(h(timed_out), timed_out);
        assert_eq!(h("pkg 'x' (TypeError: bad) — skipped"), "pkg 'x' (Unexpected data) — skipped");
        assert_eq!(h("x (ValueError: f(1) failed) y"), "x (f(1) failed) y");
        assert_eq!(h("x: KeyError: 'a' — skipped"), "x: Unexpected data — skipped");
        // Unbracketed, only the OS's words go.
        assert_eq!(h("[Errno 13] Permission denied; retrying"), "Permission denied; retrying");
        // Two errors on one line.
        assert_eq!(
            h("a (KeyError: 'k') then b: IndexError: list index out of range"),
            "a (Unexpected data) then b: Unexpected data"
        );
    }

    #[test]
    fn a_message_of_the_codes_own_is_kept() {
        assert_eq!(h("AssertionError: gridcraft: no mapper or loader"), "gridcraft: no mapper or loader");
        assert_eq!(h("StopIteration: the list ran out"), "the list ran out");
        assert_eq!(h("AssertionError"), "Unexpected data");
        assert_eq!(h("StopIteration"), "Unexpected data");
        assert_eq!(h("failed: AssertionError: x is not ready"), "failed: x is not ready");
        // Python's own wording still goes, whichever class carries it.
        assert_eq!(h("AssertionError: list index out of range"), "Unexpected data");
        assert_eq!(h("KeyError: 'k'"), "Unexpected data");
    }

    #[test]
    fn a_decode_names_no_codec_it_was_not() {
        assert_eq!(
            h("'ascii' codec can't decode byte 0xc3 in position 4: ordinal not in range(128)"),
            "The text could not be decoded"
        );
        assert_eq!(
            h("UnicodeDecodeError: 'latin-1' codec can't decode byte 0x81 in position 0: x"),
            "The text could not be decoded"
        );
        assert_eq!(h("'UTF8' codec can't decode byte 0xff in position 0: invalid start byte"), "The file is not valid UTF-8");
    }

    #[test]
    fn idempotent() {
        for s in [
            "[Errno 2] No such file or directory: '/tmp/x'",
            "no snapshot was taken ([Errno 13] Permission denied); continuing without one",
            "pkg 'x' (TypeError: bad) — skipped",
            "AssertionError: gridcraft: no mapper or loader",
            "'ascii' codec can't decode byte 0xc3 in position 4: ordinal not in range(128)",
            "URLError: <urlopen error timed out>",
            "OSError: [Errno 18] Invalid cross-device link: 'a' -> 'b'",
            "Updated 2\nfailed: IndexError: list index out of range",
            "LookupError: unknown encoding: klingon",
        ] {
            let once = h(s);
            assert_eq!(h(&once), once, "{s}");
        }
    }

    #[test]
    fn other_text_is_left_alone() {
        for s in [
            "",
            "Removed GridCraft",
            "Update running in a terminal — the list refreshes when it closes",
            "HTTP 429: rate limited",
            "It's the 'stable' branch",
            "Command '['pacman', '-Rs']' timed out after 60 seconds",
            "MyError: something",
            "line 3 of the file",
            "error: target not found: foo",
        ] {
            assert_eq!(h(s), s);
        }
        // Each line on its own.
        assert_eq!(h("Updated 2\nfailed: IndexError: list index out of range"), "Updated 2\nfailed: Unexpected data");
    }

    #[test]
    fn a_line_longer_than_any_error_is_left_alone() {
        let deep = format!("TypeError: {}{}", "(".repeat(20_000), ")".repeat(20_000));
        let out = std::thread::Builder::new().stack_size(2 << 20).spawn(move || super::human_error(&deep)).unwrap().join().unwrap();
        assert!(out.starts_with("TypeError: ((("));
    }
}
