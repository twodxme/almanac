//! How stale a package is, and how far behind: AUR health, dates, sizes,
//! version distance.

use super::*;

pub const STALE_DAYS: i64 = 180;

// Things that legitimately stop changing. A font or an icon theme two years
// old is finished, not abandoned, and flagging it would train the user to
// ignore the warning that matters.
pub const FINISHED_PREFIXES: &[&str] = &["ttf-", "otf-", "noto-", "adobe-source-"];
pub const FINISHED_SUFFIXES: &[&str] =
    &["-font", "-fonts", "-icon-theme", "-themes", "-cursor-theme"];

/// What the AUR says about one of its packages (one value of `aur_health`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AurHealth {
    pub out_of_date: bool,
    pub last_modified: f64,
    pub maintainer: String,
}

/// `{icon, categories}` read from a package's .desktop file (one value of
/// `desktop_map`). `Default` is the Python's `{}` for a package with none.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DesktopEntry {
    pub icon: String,
    pub categories: String,
}

/// `_staleness`: 'orphaned' | 'flagged' | 'stale' | '' — strongest signal wins.
///
/// Only the first two are authoritative: a human either abandoned the package
/// or flagged it out of date. 'stale' is a build-date proxy and is deliberately
/// limited to real applications, because libraries and fonts sit untouched for
/// years as a matter of course.
pub fn staleness(
    name: &str,
    source: &str,
    aur: &BTreeMap<String, AurHealth>,
    desk: &DesktopEntry,
    meta: &BTreeMap<String, String>,
) -> String {
    let rec = aur.get(name);
    if source == AUR && rec.is_some_and(|r| r.maintainer.is_empty()) {
        return "orphaned".into();
    }
    if rec.is_some_and(|r| r.out_of_date) {
        return "flagged".into();
    }

    let lower = name.to_lowercase();
    if FINISHED_PREFIXES.iter().any(|p| lower.starts_with(p))
        || FINISHED_SUFFIXES.iter().any(|s| lower.ends_with(s))
    {
        return String::new();
    }
    if desk.icon.is_empty() && desk.categories.is_empty() {
        return String::new(); // library or CLI tool, not an app
    }
    let built = date_ts(meta.get("built").map(String::as_str).unwrap_or(""));
    if built != 0.0 && (util::now() - built) / 86400.0 > STALE_DAYS as f64 {
        return "stale".into();
    }
    String::new()
}

/// Ask the AUR about its own packages: flagged out-of-date, and when the
/// packaging was last touched.
///
/// This is the only *authoritative* staleness signal available -- a human
/// maintainer flags out-of-date, and LastModified is real packaging activity.
/// Build age (below) is only a proxy. One RPC call covers every package.
/// Offline returns an empty map: offline is not a reason to show nothing.
/// Err where the Python raises after the fetch (a reply of an unexpected
/// shape: not an object, `results` not a list of objects, a LastModified
/// that is not a number); `packages` then fails, as in the Python.
pub fn try_aur_health(names: &[String]) -> Result<BTreeMap<String, AurHealth>, String> {
    if names.is_empty() {
        return Ok(BTreeMap::new());
    }
    let query = names
        .iter()
        .map(|n| format!("arg[]={}", py_quote(n)))
        .collect::<Vec<_>>()
        .join("&");
    let url = format!("https://aur.archlinux.org/rpc/v5/info?{query}");
    let data = match http_json(&url, 15) {
        Ok(d) => d,
        Err(_) => return Ok(BTreeMap::new()), // offline is not a reason to show nothing
    };
    let mut out = BTreeMap::new();
    let Value::Object(data) = data else {
        return Err(format!("'{}' object has no attribute 'get'", py_type(&data)));
    };
    for rec in py_iter(data.get("results").unwrap_or(&json!([])))? {
        let Value::Object(rec) = &rec else {
            return Err(format!("'{}' object has no attribute 'get'", py_type(&rec)));
        };
        let name = match rec.get("Name") {
            Some(v @ (Value::Array(_) | Value::Object(_))) => {
                return Err(format!("unhashable type: '{}'", py_type(v)))
            }
            Some(v) => py_str(v),
            None => String::new(),
        };
        out.insert(
            name,
            AurHealth {
                out_of_date: rec.get("OutOfDate").is_some_and(truthy),
                last_modified: float_or_zero(rec, "LastModified")?,
                maintainer: get_str_or(rec, "Maintainer"),
            },
        );
    }
    Ok(out)
}

/// `try_aur_health` for callers that need a plain map: where the Python
/// raises, this answers `{}`.
pub fn aur_health(names: &[String]) -> BTreeMap<String, AurHealth> {
    try_aur_health(names).unwrap_or_default()
}

/// '1737.27 KiB' -> 1.70. Sorting needs a number: as text, '9 KiB' sorts
/// above '10 MiB'.
pub fn size_mb(value: &str) -> f64 {
    let parts = py_split(value);
    if parts.len() < 2 {
        return 0.0;
    }
    // The Python builds the whole unit table first, so a bad number is a
    // ValueError whatever the unit.
    let Some(num) = py_float_text(parts[0]) else { return 0.0 };
    match parts[1] {
        "B" => num / 1048576.0,
        "KiB" => num / 1024.0,
        "MiB" => num,
        "GiB" => num * 1024.0,
        _ => 0.0,
    }
}

/// `datetime.strptime(text, fmt)` for the two formats `date_ts` tries, as a
/// naive local datetime.
pub(super) fn strptime(text: &str, with_time: bool) -> Option<chrono::NaiveDateTime> {
    use std::sync::LazyLock;
    // _strptime's regexes for %d %b %Y [%I:%M:%S %p], case-insensitive, with
    // format whitespace as \s+.
    static FULL: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(concat!(
            r"(?i)^(3[01]|[12]\d|0[1-9]|[1-9]| [1-9])\s+",
            r"(jan|feb|mar|apr|may|jun|jul|aug|sep|oct|nov|dec)\s+(\d\d\d\d)\s+",
            r"(1[0-2]|0[1-9]|[1-9]| [1-9]):([0-5]\d|\d):(6[0-1]|[0-5]\d|\d)\s+(am|pm)$"
        ))
        .unwrap()
    });
    static DATE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(concat!(
            r"(?i)^(3[01]|[12]\d|0[1-9]|[1-9]| [1-9])\s+",
            r"(jan|feb|mar|apr|may|jun|jul|aug|sep|oct|nov|dec)\s+(\d\d\d\d)$"
        ))
        .unwrap()
    });
    const MONTHS: [&str; 12] =
        ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    // `\d` is any Unicode decimal digit, in Python's re as in this one, and
    // int() reads them all; the literal ranges ([12], 0[1-9], ...) are ASCII.
    let caps = if with_time { FULL.captures(text)? } else { DATE.captures(text)? };
    let num = |i: usize| -> u32 { ascii_digits(caps[i].trim()).parse().unwrap_or(0) };
    let day = num(1);
    let month = MONTHS.iter().position(|m| *m == caps[2].to_lowercase())? as u32 + 1;
    let year = num(3) as i32;
    if year < 1 {
        return None; // datetime: year 0 is out of range
    }
    let (mut hour, minute, second) = if with_time { (num(4), num(5), num(6)) } else { (0, 0, 0) };
    if with_time {
        let pm = caps[7].eq_ignore_ascii_case("pm");
        if pm && hour != 12 {
            hour += 12;
        } else if !pm && hour == 12 {
            hour = 0;
        }
    }
    chrono::NaiveDate::from_ymd_opt(year, month, day)?.and_hms_opt(hour, minute, second)
}

/// `naive.timestamp()`: the naive datetime read as local time, by CPython's
/// own algorithm (`datetime._mktime`, fold=0): solve `t = local(u)` for u,
/// taking the earlier offset in a fold and the later instant in a gap.
/// None where CPython raises: `local()` builds a datetime from
/// `localtime(u)`, and a year outside 1..=9999 there is a ValueError
/// (1 Jan 0001 probes the day before it, which is year 0).
pub(super) fn local_timestamp(naive: chrono::NaiveDateTime) -> Option<f64> {
    use chrono::{Datelike, Offset, TimeZone};
    const MAX_FOLD_SECONDS: i64 = 24 * 3600;
    let t = naive.and_utc().timestamp();
    let local = |u: i64| -> Option<i64> {
        let utc = chrono::DateTime::from_timestamp(u, 0)?.naive_utc();
        let off = chrono::Local.offset_from_utc_datetime(&utc).fix().local_minus_utc() as i64;
        let lt = utc + chrono::Duration::seconds(off);
        if !(1..=9999).contains(&lt.year()) {
            return None;
        }
        Some(u + off)
    };
    let a = local(t)? - t;
    let u1 = t - a;
    let t1 = local(u1)?;
    let b = if t1 == t {
        // We found one solution, but it may not be the one we need: look
        // for an earlier one (fold=0).
        let u2 = u1 - MAX_FOLD_SECONDS;
        let b = local(u2)? - u2;
        if a == b {
            return Some(u1 as f64);
        }
        b
    } else {
        t1 - u1
    };
    let u2 = t - b;
    let t2 = local(u2)?;
    if t2 == t {
        return Some(u2 as f64);
    }
    if t1 == t {
        return Some(u1 as f64);
    }
    // Both offsets found, but neither t - a nor t - b is a solution: t is
    // in the gap.
    Some(u1.max(u2) as f64)
}

/// pacman's 'Fri 16 Oct 2026 04:08:05 PM PDT' -> epoch seconds, so the
/// column sorts chronologically instead of by the weekday name.
pub fn date_ts(value: &str) -> f64 {
    if value.is_empty() {
        return 0.0;
    }
    let words = py_split(value);
    for (with_time, count) in [(true, 6usize), (false, 4usize)] {
        let end = count.min(words.len());
        let text = if words.len() > 1 { words[1..end.max(1)].join(" ") } else { String::new() };
        // (A ValueError from .timestamp() is caught like strptime's.)
        if let Some(ts) = strptime(&text, with_time).and_then(local_timestamp) {
            return ts;
        }
    }
    0.0
}

/// How far behind, and at which level.
///
/// Returns (count, kind) where kind is 'major'/'minor'/'patch'/'rel'. Only the
/// first differing component is counted: 1.2.9 -> 3.0.0 is "2 major behind",
/// not "2 major and 2 minor", because once the major moves the lower numbers
/// restart and differencing them is meaningless.
///
/// Serialises as a JSON array `[count, kind]`, like the Python tuple.
///
/// The count is a machine integer for callers that do arithmetic on it; a
/// difference beyond i64 saturates. `versions_behind_big` has the exact
/// (unbounded, like Python's int) count.
pub fn versions_behind(current: &str, latest: &str) -> (i64, String) {
    use num_bigint::ToBigInt;
    let (n, kind) = versions_behind_big(current, latest);
    let max = i64::MAX.to_bigint().unwrap_or_default();
    let min = i64::MIN.to_bigint().unwrap_or_default();
    let clamped = if n > max { i64::MAX } else if n < min { i64::MIN } else { i64::try_from(&n).unwrap_or(0) };
    (clamped, kind)
}

/// `versions_behind` with Python's unbounded int for the count.
pub fn versions_behind_big(current: &str, latest: &str) -> (num_bigint::BigInt, String) {
    use num_bigint::BigInt;
    fn nums(v: &str) -> Vec<BigInt> {
        let v = ascii_digits(v);
        let last = v.rsplit(':').next().unwrap_or("");
        let core = last.split(['-', '+', '~']).next().unwrap_or("");
        let mut out = Vec::new();
        for part in core.split('.') {
            let digits: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
            if digits.is_empty() {
                break;
            }
            out.push(digits.parse::<BigInt>().unwrap_or_default());
        }
        out
    }

    let zero = BigInt::from(0);
    let (a, b) = (nums(current), nums(latest));
    if a.is_empty() || b.is_empty() {
        return (zero, String::new());
    }
    // Calendar versions are not release counters. Both the packed form
    // (20181225 -> 20261002, which differenced to "79777 major behind") and
    // the dotted form (2026.10.16 -> 2026.10.17, which read as "1 patch")
    // describe when a build was cut, not how many releases were missed.
    fn calendar(v: &[BigInt]) -> bool {
        let n = |x: i64| BigInt::from(x);
        if v[0] > n(100000) {
            // 20261002
            return true;
        }
        (n(1980) <= v[0] && v[0] <= n(2100)) // 2026.10.17
            && v.len() >= 2 && (n(1) <= v[1] && v[1] <= n(12))
            && (v.len() < 3 || (n(1) <= v[2] && v[2] <= n(31)))
    }

    if calendar(&a) || calendar(&b) {
        return (zero, String::new());
    }
    let names = ["major", "minor", "patch"];
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).cloned().unwrap_or_default();
        let y = b.get(i).cloned().unwrap_or_default();
        if y != x {
            let kind = if i < 3 { names[i] } else { "point" };
            return (y - x, kind.to_string());
        }
    }
    // Same version numbers: the difference is only the pkgrel (e.g. -1 -> -2),
    // which is a rebuild, not a new upstream release.
    (zero, "rel".into())
}

/// A Python int as JSON: exact up to 64 bits; beyond that the nearest
/// float, the most a `serde_json::Value` (without arbitrary precision) can
/// hold.
pub(super) fn big_json(n: &num_bigint::BigInt) -> Value {
    if let Ok(i) = i64::try_from(n) {
        return json!(i);
    }
    if let Ok(u) = u64::try_from(n) {
        return json!(u);
    }
    n.to_string().parse::<f64>().map(|f| json!(f)).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bh(c: &str, l: &str) -> (i64, String) {
        versions_behind(c, l)
    }

    // ---- VersionsBehind
    #[test]
    fn test_counts_first_differing_level_only() {
        // 1.2.9 -> 3.0.0 is two majors; the lower components restart, so
        // differencing them as well would be meaningless.
        assert_eq!(bh("1.2.9", "3.0.0"), (2, "major".into()));
        assert_eq!(bh("0.6.0", "0.12.0"), (6, "minor".into()));
        assert_eq!(bh("1.2.3", "1.2.9"), (6, "patch".into()));
    }

    #[test]
    fn test_pkgrel_bump_is_a_rebuild_not_a_release() {
        assert_eq!(bh("0.29.0-1", "0.29.0-2"), (0, "rel".into()));
    }

    #[test]
    fn test_epoch_is_ignored() {
        // "1:1.2.96" -- the epoch is packaging metadata, not an upstream version.
        assert_eq!(bh("1:1.2.96.518-2", "1:1.3.4.258-1"), (1, "minor".into()));
    }

    #[test]
    fn test_date_stamped_versions_are_not_counted_as_releases() {
        // 20181225 -> 20261002 differenced as a release count gives 79,777,
        // which is a fact about calendars, not about software.
        assert_eq!(bh("20181225-1", "20261002.r63-3"), (0, "".into()));
        assert_eq!(bh("2026.10.16", "2026.10.17"), (0, "".into()));
    }

    #[test]
    fn test_unparseable_is_silent_rather_than_wrong() {
        assert_eq!(bh("", "1.0"), (0, "".into()));
        assert_eq!(bh("git-abc123", "git-def456"), (0, "".into()));
    }
}
