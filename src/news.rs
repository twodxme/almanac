//! Arch news: where Arch announces the updates that need a person.
//!
//! An update that needs manual intervention -- a config format change, a
//! package split, a step to take before or after upgrading -- is announced on
//! the news feed and nowhere else. pacman does not show it. Reading it before
//! updating is the one habit the Arch wiki asks of everyone.
//!
//! The scheduled update holds itself while the newest headline is unread, so
//! "read" is shared with it through one file, kept where it keeps its state:
//! `${XDG_STATE_HOME:-~/.local/state}/omarchy-auto-update/news-seen`, one
//! headline per line, entities decoded. Older copies of the script wrote
//! headlines as the feed escapes them (`Mkinitcpio &gt;=42`, even `&amp;gt;`),
//! so lines are decoded before they are compared. Marking a headline read
//! here appends it there, which also releases the hold.
//!
//! The feed is fetched at most once an hour and kept, so the news still shows
//! offline; Preferences can switch the fetch off.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::util;

pub const FEED: &str = "https://archlinux.org/feeds/news/";
/// How long a fetched feed is used before asking again.
pub const TTL: f64 = 3600.0;
/// News older than this is not brought up, read or not: a machine set up
/// last week has no use for a manual intervention from last year.
pub const RECENT_DAYS: i64 = 60;
/// The seen-file is kept shorter than this many lines, oldest dropped first.
pub const SEEN_MAX: usize = 200;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NewsItem {
    pub title: String,
    pub link: String,
    /// epoch seconds; 0 when the feed's date could not be read
    pub published: i64,
    /// the first lines of the post, as plain text
    pub summary: String,
}

impl NewsItem {
    /// "21 Oct 2026".
    pub fn date(&self) -> String {
        chrono::DateTime::from_timestamp(self.published, 0)
            .map(|d| d.with_timezone(&chrono::Local).format("%-d %b %Y").to_string())
            .unwrap_or_default()
    }
}

/// Switched on in Preferences (Privacy).
pub fn enabled() -> bool {
    crate::settings::get_bool("net_arch_news")
}

/// The file the scheduled update reads.
pub fn seen_path() -> PathBuf {
    crate::xdg::state_home().join("omarchy-auto-update").join("news-seen")
}

fn cache_path() -> PathBuf {
    crate::xdg::cache_home().join("almanac").join("arch-news.json")
}

// ---- text ---------------------------------------------------------------------

fn entity(name: &str) -> Option<char> {
    if let Some(num) = name.strip_prefix('#') {
        let n = match num.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => num.parse().ok()?,
        };
        return char::from_u32(n).filter(|c| *c != '\0');
    }
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        "ge" => '≥',
        "le" => '≤',
        "ndash" => '–',
        "mdash" => '—',
        "hellip" => '…',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        _ => return None,
    })
}

fn decode_once(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let decoded = tail[1..]
            .find(';')
            .filter(|&end| end <= 10)
            .and_then(|end| entity(&tail[1..=end]).map(|c| (c, end + 2)));
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = &tail[len..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// HTML entities decoded until none are left: a headline escaped twice
/// (`&amp;gt;`) reads `>` too.
pub fn decode_entities(s: &str) -> String {
    let mut cur = s.to_string();
    for _ in 0..8 {
        let next = decode_once(&cur);
        if next == cur {
            break;
        }
        cur = next;
    }
    cur
}

/// A post's HTML as one paragraph of plain text, cut at about `max`
/// characters on a word.
fn plain_summary(html: &str, max: usize) -> String {
    // A block element breaks the text where it starts or ends; an inline one
    // (<code>, <a>, <em>) runs on, so "<code>42-1</code>," stays "42-1,".
    const BLOCKS: [&str; 14] = ["p", "br", "li", "ul", "ol", "div", "pre", "h1", "h2", "h3", "h4", "tr", "td", "blockquote"];
    let mut text = String::new();
    let mut tag: Option<String> = None;
    for c in html.chars() {
        match (&mut tag, c) {
            (None, '<') => tag = Some(String::new()),
            (Some(t), '>') => {
                let name: String = t
                    .trim_start_matches('/')
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric())
                    .collect::<String>()
                    .to_ascii_lowercase();
                if BLOCKS.contains(&name.as_str()) {
                    text.push(' ');
                }
                tag = None;
            }
            (Some(t), _) => t.push(c),
            (None, _) => text.push(c),
        }
    }
    let text = decode_entities(&text);
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut out = String::new();
    for w in words {
        if out.chars().count() + w.chars().count() + 1 > max {
            out.push('…');
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(w);
    }
    out
}

// ---- the feed -----------------------------------------------------------------

/// The items of an RSS feed, in feed order (newest first on Arch's).
pub fn parse(xml: &[u8]) -> Result<Vec<NewsItem>, String> {
    use crate::selfupdate::et;
    let root = et::parse(xml).map_err(|e| match e {
        et::Error::Parse => "the news feed is not valid XML".to_string(),
        et::Error::Raise(m) => m,
    })?;
    let channel = root.find("channel").ok_or("the news feed has no channel")?;
    let text = |el: &et::Element, tag: &str| el.find(tag).map(|e| e.text.trim().to_string()).unwrap_or_default();
    Ok(channel
        .children
        .iter()
        .filter(|c| c.tag == "item")
        .map(|item| NewsItem {
            title: decode_entities(&text(item, "title")),
            link: text(item, "link"),
            published: chrono::DateTime::parse_from_rfc2822(&text(item, "pubDate")).map_or(0, |d| d.timestamp()),
            summary: plain_summary(&text(item, "description"), 240),
        })
        .filter(|i| !i.title.is_empty())
        .collect())
}

#[derive(Serialize, Deserialize, Default)]
struct Cached {
    at: f64,
    items: Vec<NewsItem>,
}

fn fetch() -> Result<Vec<u8>, String> {
    let t = std::time::Duration::from_secs(10);
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(t))
        .tls_config(ureq::tls::TlsConfig::builder().root_certs(crate::sources::system_roots()).build())
        .build()
        .into();
    let r = agent.get(FEED).header("User-Agent", "almanac").call().map_err(|e| e.to_string())?;
    r.into_body().with_config().limit(4 * 1024 * 1024).read_to_vec().map_err(|e| e.to_string())
}

/// The feed: the copy fetched within the hour, else a fresh one, else the
/// last copy kept however old (offline). None when there is none at all or
/// the fetch is switched off and nothing was kept.
pub fn feed(now: f64) -> Option<Vec<NewsItem>> {
    feed_with(&cache_path(), now, enabled(), &fetch)
}

fn feed_with(cache: &Path, now: f64, online: bool, fetch: &dyn Fn() -> Result<Vec<u8>, String>) -> Option<Vec<NewsItem>> {
    let kept: Option<Cached> =
        std::fs::read(cache).ok().and_then(|b| serde_json::from_slice(&b).ok());
    if let Some(k) = &kept {
        if (0.0..TTL).contains(&(now - k.at)) {
            return Some(k.items.clone());
        }
    }
    if online {
        if let Some(items) = fetch().ok().and_then(|b| parse(&b).ok()) {
            let doc = Cached { at: now, items };
            if let Ok(bytes) = serde_json::to_vec(&doc) {
                if let Some(parent) = cache.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = util::atomic_write(cache, bytes);
            }
            return Some(doc.items);
        }
    }
    kept.map(|k| k.items)
}

// ---- read and unread ------------------------------------------------------------

/// The seen-file's headlines, decoded.
pub fn read_seen(path: &Path) -> Vec<String> {
    std::fs::read(path)
        .map(|b| String::from_utf8_lossy(&b).lines().map(|l| decode_entities(l.trim_end_matches('\r'))).collect())
        .unwrap_or_default()
}

/// What has not been read, newest first: items newer than the newest one
/// marked read, not marked themselves, from the last `RECENT_DAYS`. The
/// scheduled update records only the newest headline it saw, so everything
/// older than a read item counts as read.
pub fn unread(items: &[NewsItem], seen: &[String], now: f64) -> Vec<NewsItem> {
    let is_seen = |i: &NewsItem| {
        let title = decode_entities(&i.title);
        seen.contains(&title)
    };
    let newest_seen = items.iter().filter(|i| is_seen(i)).map(|i| i.published).max();
    let cutoff = now as i64 - RECENT_DAYS * 86400;
    let mut out: Vec<NewsItem> = items
        .iter()
        .filter(|i| !is_seen(i))
        .filter(|i| i.published >= cutoff)
        .filter(|i| newest_seen.is_none_or(|n| i.published > n))
        .cloned()
        .collect();
    out.sort_by_key(|i| std::cmp::Reverse(i.published));
    out
}

/// The unread news now, or nothing when the check is switched off. Reads
/// the network at most hourly; call off the UI thread.
pub fn unread_now() -> Vec<NewsItem> {
    if !enabled() || crate::demo::active() {
        return Vec::new();
    }
    let now = util::now();
    let Some(items) = feed(now) else { return Vec::new() };
    unread(&items, &read_seen(&seen_path()), now)
}

/// Mark `titles` read: append each to the seen-file under its lock, keeping
/// the file under `SEEN_MAX` lines.
pub fn mark_read(titles: &[String]) -> std::io::Result<()> {
    mark_read_at(&seen_path(), titles)
}

fn mark_read_at(path: &Path, titles: &[String]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut lock_name = path.as_os_str().to_owned();
    lock_name.push(".lock");
    let _lock = util::lock_file(Path::new(&lock_name))?;
    let existing = std::fs::read(path).unwrap_or_default();
    let have: Vec<String> = String::from_utf8_lossy(&existing).lines().map(decode_entities).collect();
    let mut add = String::new();
    if !existing.is_empty() && !existing.ends_with(b"\n") {
        add.push('\n');
    }
    for t in titles {
        // One line each: a headline is one line, and a stray newline would
        // write a second "headline" the script then matches.
        let t = decode_entities(t).replace(['\n', '\r'], " ");
        let t = t.trim();
        if t.is_empty() || have.iter().any(|h| h == t) || add.lines().any(|l| l == t) {
            continue;
        }
        add.push_str(t);
        add.push('\n');
    }
    if add.trim().is_empty() {
        return Ok(());
    }
    // One write of whole lines with O_APPEND: a reader never sees half a
    // headline.
    let mut fh = std::fs::OpenOptions::new().append(true).create(true).open(path)?;
    fh.write_all(add.as_bytes())?;
    drop(fh);
    let text = std::fs::read(path)?;
    let text = String::from_utf8_lossy(&text);
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() >= SEEN_MAX {
        let keep = &lines[lines.len() - (SEEN_MAX - 1)..];
        util::atomic_write(path, keep.join("\n") + "\n")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two items from archlinux.org/feeds/news/ as served, plus one with its
    /// title in CDATA and a double-escaped entity, as other RSS writers do.
    const SAMPLE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<rss version="2.0" xmlns:atom="http://www.w3.org/2005/Atom"><channel><title>Arch Linux: Recent news updates</title><link>https://archlinux.org/news/</link><description>The latest and greatest news from the Arch Linux distribution.</description><atom:link href="https://archlinux.org/feeds/news/" rel="self"/><language>en-us</language><lastBuildDate>Wed, 21 Oct 2026 19:29:36 +0000</lastBuildDate><item><title>Update to ca-certificates-mozilla ≥ 3.131.1-1 to prevent man-in-the-middle attacks</title><link>https://archlinux.org/news/update-to-ca-certificates-mozilla-31311-1-to-prevent-man-in-the-middle-attacks/</link><description>&lt;p&gt;On 2026-10-20 it was revealed, that &lt;a href="https://example.org/"&gt;several certificates have been compromised&lt;/a&gt;.&lt;/p&gt;
&lt;p&gt;Without an update to this central certificate bundle, systems are susceptible to man-in-the-middle attacks.&lt;/p&gt;</description><dc:creator xmlns:dc="http://purl.org/dc/elements/1.1/">David Runge</dc:creator><pubDate>Wed, 21 Oct 2026 13:52:37 +0000</pubDate><guid isPermaLink="false">tag:archlinux.org,2026-10-21:/news/update-to-ca-certificates-mozilla-31311-1-to-prevent-man-in-the-middle-attacks/</guid></item><item><title>Mkinitcpio &gt;=42 requires manual intervention for TPM2-based unlocking of LUKS devices</title><link>https://archlinux.org/news/mkinitcpio-42-requires-manual-intervention/</link><description>&lt;p&gt;Users of &lt;code&gt;sd-encrypt&lt;/code&gt; must update &lt;code&gt;/etc/crypttab&lt;/code&gt; &amp;amp; rebuild.&lt;/p&gt;</description><dc:creator xmlns:dc="http://purl.org/dc/elements/1.1/">Morten Linderud</dc:creator><pubDate>Mon, 12 Oct 2026 09:00:00 +0000</pubDate><guid isPermaLink="false">tag:archlinux.org,2026-10-12:/news/mkinitcpio-42/</guid></item><item><title><![CDATA[virtualbox-ext-vnc &amp;gt;= 7.2.12-2 requires manual intervention]]></title><link>https://archlinux.org/news/virtualbox-ext-vnc/</link><description><![CDATA[<p>Remove the package <em>first</em>.</p>]]></description><pubDate>Tue, 01 Sep 2026 10:00:00 +0000</pubDate></item></channel></rss>"#;

    /// 2026-10-26 12:00 UTC.
    const NOW: f64 = 1_793_016_000.0;

    #[test]
    fn a_real_feed_parses_into_headlines_links_and_dates() {
        let items = parse(SAMPLE.as_bytes()).unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(
            items[0].title,
            "Update to ca-certificates-mozilla ≥ 3.131.1-1 to prevent man-in-the-middle attacks"
        );
        assert_eq!(items[1].title, "Mkinitcpio >=42 requires manual intervention for TPM2-based unlocking of LUKS devices");
        assert_eq!(items[1].link, "https://archlinux.org/news/mkinitcpio-42-requires-manual-intervention/");
        assert_eq!(items[1].published, 1_791_795_600);
        assert_eq!(items[1].summary, "Users of sd-encrypt must update /etc/crypttab & rebuild.");
    }

    #[test]
    fn cdata_and_double_escaped_entities_come_out_as_plain_text() {
        let items = parse(SAMPLE.as_bytes()).unwrap();
        assert_eq!(items[2].title, "virtualbox-ext-vnc >= 7.2.12-2 requires manual intervention");
        assert_eq!(items[2].summary, "Remove the package first.");
    }

    #[test]
    fn a_feed_that_is_not_xml_is_an_error_not_an_empty_list() {
        assert!(parse(b"<html><body>502").is_err());
        assert!(parse(b"<rss></rss>").is_err());
    }

    #[test]
    fn entities_are_decoded_until_none_are_left() {
        assert_eq!(decode_entities("Mkinitcpio &gt;=42"), "Mkinitcpio >=42");
        assert_eq!(decode_entities("a &amp;gt;= b"), "a >= b");
        assert_eq!(decode_entities("&#8805; &#x2265;"), "≥ ≥");
        assert_eq!(decode_entities("AT&T & co &unknown;"), "AT&T & co &unknown;");
    }

    #[test]
    fn a_headline_the_old_script_wrote_escaped_still_counts_as_read() {
        let d = tempfile::tempdir().unwrap();
        let seen = d.path().join("news-seen");
        std::fs::write(&seen, "Mkinitcpio &gt;=42 requires manual intervention for TPM2-based unlocking of LUKS devices\n").unwrap();
        let items = parse(SAMPLE.as_bytes()).unwrap();
        let unread = unread(&items, &read_seen(&seen), NOW);
        // The newer one is unread; the one marked and everything older is not.
        assert_eq!(unread.len(), 1);
        assert!(unread[0].title.starts_with("Update to ca-certificates"));
    }

    #[test]
    fn news_older_than_sixty_days_is_never_brought_up() {
        let items = parse(SAMPLE.as_bytes()).unwrap();
        // Forty days on: the October posts are still recent, September's is not.
        let later = NOW + 40.0 * 86400.0;
        let titles: Vec<String> = unread(&items, &[], later).into_iter().map(|i| i.title).collect();
        assert_eq!(titles.len(), 2, "{titles:?}");
        assert!(unread(&items, &[], NOW + 365.0 * 86400.0).is_empty());
    }

    #[test]
    fn marking_read_appends_decoded_lines_once_each() {
        let d = tempfile::tempdir().unwrap();
        let seen = d.path().join("state").join("news-seen");
        mark_read_at(&seen, &["A &gt;= B".to_string()]).unwrap();
        mark_read_at(&seen, &["A >= B".to_string(), "Second\nline".to_string()]).unwrap();
        assert_eq!(std::fs::read_to_string(&seen).unwrap(), "A >= B\nSecond line\n");
        assert!(d.path().join("state").join("news-seen.lock").exists());
    }

    #[test]
    fn marking_read_adds_a_newline_the_file_was_missing() {
        let d = tempfile::tempdir().unwrap();
        let seen = d.path().join("news-seen");
        std::fs::write(&seen, "Old headline").unwrap();
        mark_read_at(&seen, &["New headline".to_string()]).unwrap();
        assert_eq!(std::fs::read_to_string(&seen).unwrap(), "Old headline\nNew headline\n");
    }

    #[test]
    fn the_seen_file_stays_under_two_hundred_lines_dropping_the_oldest() {
        let d = tempfile::tempdir().unwrap();
        let seen = d.path().join("news-seen");
        let old: String = (0..199).map(|i| format!("headline {i}\n")).collect();
        std::fs::write(&seen, old).unwrap();
        mark_read_at(&seen, &["newest".to_string()]).unwrap();
        let text = std::fs::read_to_string(&seen).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), SEEN_MAX - 1);
        assert_eq!(lines.last(), Some(&"newest"));
        assert_eq!(lines.first(), Some(&"headline 1"));
    }

    #[test]
    fn a_fresh_copy_is_used_without_asking_and_a_stale_one_only_offline() {
        let d = tempfile::tempdir().unwrap();
        let cache = d.path().join("arch-news.json");
        let calls = std::cell::Cell::new(0);
        let ok = || -> Result<Vec<u8>, String> {
            calls.set(calls.get() + 1);
            Ok(SAMPLE.as_bytes().to_vec())
        };
        assert_eq!(feed_with(&cache, NOW, true, &ok).unwrap().len(), 3);
        assert_eq!(calls.get(), 1);
        // Within the hour: the kept copy, no request.
        assert_eq!(feed_with(&cache, NOW + 600.0, true, &ok).unwrap().len(), 3);
        assert_eq!(calls.get(), 1);
        // Hours later and offline: still the kept copy.
        let down = || -> Result<Vec<u8>, String> { Err("offline".into()) };
        assert_eq!(feed_with(&cache, NOW + 7200.0, true, &down).unwrap().len(), 3);
        // Switched off: no request, but what was kept still shows.
        let never = || -> Result<Vec<u8>, String> { panic!("fetched while switched off") };
        assert_eq!(feed_with(&cache, NOW + 7200.0, false, &never).unwrap().len(), 3);
        assert_eq!(feed_with(&d.path().join("none.json"), NOW, false, &never), None);
    }
}
