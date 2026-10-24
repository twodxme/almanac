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

static DECODE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^'[^']+' codec can't decode .*$").unwrap());
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

fn errno_text(n: i64, strerror: &str, path: Option<&str>, to: Option<&str>) -> String {
    let path = path.map(unquote);
    let to = to.map(unquote);
    let with = |text: String| match (&path, &to) {
        (Some(p), Some(t)) => format!("{text}: {p} → {t}"),
        (Some(p), None) => format!("{text}: {p}"),
        _ => text,
    };
    match n {
        2 => with(tr("No such file")),
        13 => with(tr("Permission denied")),
        // The OS's own wording is plain enough; the brackets and quotes go.
        _ if strerror.is_empty() || strerror == "None" => with(tr("The system refused")),
        _ => with(strerror.to_string()),
    }
}

/// The plain text for one recognised error, or None when `text` is not one.
fn known(text: &str) -> Option<String> {
    if let Some(c) = ERRNO.captures(text) {
        let n = c[1].parse().unwrap_or(0);
        return Some(errno_text(n, &c[2], c.get(3).map(|m| m.as_str()), c.get(4).map(|m| m.as_str())));
    }
    if let Some(c) = URLOPEN.captures(text) {
        return Some(human_line(&c[1]));
    }
    if DEEP.is_match(text) {
        return Some(tr("The data is nested too deeply to read"));
    }
    if DECODE.is_match(text) {
        return Some(tr("The file is not valid UTF-8"));
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
        "IndexError" | "KeyError" | "TypeError" | "AttributeError" | "StopIteration" | "AssertionError"
        | "LookupError" => tr("Unexpected data"),
        _ if detail.is_empty() => tr("Something went wrong"),
        // ValueError, OSError, URLError, HTTPError, RuntimeError, ...: what
        // follows the class is already a sentence ("HTTP Error 404: Not
        // Found", "Invalid IPv6 URL").
        _ => detail.to_string(),
    })
}

/// One line: the first place, from the start or after a separator ("X
/// failed: ...", "skipped — ..."), where a recognised error begins; what
/// precedes it is the app's own wording and stays.
fn human_line(line: &str) -> String {
    let mut at = 0;
    loop {
        if let Some(plain) = known(&line[at..]) {
            return format!("{}{plain}", &line[..at]);
        }
        let rest = &line[at..];
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
    text.split('\n').map(human_line).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::human_error as h;

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
}
