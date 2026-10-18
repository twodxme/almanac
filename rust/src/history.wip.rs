//! Port of `history.py`.
//!
//! What changed on this machine, and when: one timeline.
//!
//! "It worked last week" is the start of most troubleshooting, and the answer to
//! "what changed since?" is scattered: pacman.log for packages, the journal for
//! snapshots, `.bak` files for settings a tool reset, and nowhere at all for
//! what Almanac itself did. This puts them on one timeline, newest first,
//! grouped by day -- and marks the days that have a snapshot to go back to.
//!
//! Almanac's own actions (an update that did not change anything, settings
//! restored, file types handed back) are appended to a small JSON-lines file in
//! the XDG state directory. Everything else is read from where it already is;
//! nothing is collected that the system was not already keeping.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// recorded events kept; older ones drop off
pub const KEEP: usize = 2000;
pub const DAYS: i64 = 30;

pub const PACKAGES: &str = "packages";
pub const SNAPSHOT: &str = "snapshot";
pub const RESET: &str = "reset";
pub const ALMANAC: &str = "almanac";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Event {
    pub ts: f64,
    pub kind: String,
    pub title: String,
    pub detail: String,
    pub items: Vec<String>,
    /// "warn" for things that went wrong
    pub severity: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Day {
    /// Serialised as ISO `YYYY-MM-DD`, like Python's `date.isoformat()`.
    #[serde(with = "iso_date")]
    pub day: NaiveDate,
    pub events: Vec<Event>,
    /// a snapshot was taken this day
    pub snapshot: bool,
}

impl Day {
    /// "Today", "Yesterday", or e.g. "Monday 12 October".
    pub fn label(&self) -> String {
        todo!("port: history::Day::label")
    }
}

/// chrono is built without its serde feature; dates go through ISO strings.
mod iso_date {
    use chrono::NaiveDate;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(d: &NaiveDate, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&d.format("%Y-%m-%d").to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<NaiveDate, D::Error> {
        let text = String::deserialize(d)?;
        NaiveDate::parse_from_str(&text, "%Y-%m-%d").map_err(serde::de::Error::custom)
    }
}

// ---- what Almanac did --------------------------------------------------------

/// Append one of Almanac's own actions. Never raises.
///
/// Python's `items=None` is an empty slice here.
pub fn record(kind: &str, title: &str, detail: &str, severity: &str, items: &[String]) {
    let _ = (kind, title, detail, severity, items);
    todo!("port: history::record")
}

/// Almanac's own recorded actions since `since` (epoch seconds).
pub fn recorded(since: f64) -> Vec<Event> {
    let _ = since;
    todo!("port: history::recorded")
}

// ---- what the system recorded ------------------------------------------------

/// One event per pacman transaction that changed something.
///
/// `log_text` None reads the tail of `snapshots::PACMAN_LOG`.
pub fn package_events(since: f64, log_text: Option<&str>) -> Vec<Event> {
    let _ = (since, log_text);
    todo!("port: history::package_events")
}

/// `snapper create` runs recorded by sudo or pkexec in the journal.
///
/// `journal_text` None runs `journalctl -o json`.
pub fn snapshot_events(since: f64, journal_text: Option<&str>) -> Vec<Event> {
    let _ = (since, journal_text);
    todo!("port: history::snapshot_events")
}

/// Config files a tool replaced, from the backups it left behind.
pub fn reset_events(since: f64) -> Vec<Event> {
    let _ = since;
    todo!("port: history::reset_events")
}

// ---- the timeline ------------------------------------------------------------

/// Every event of the last `days` days, newest first, grouped by day.
pub fn timeline(days: i64) -> Vec<Day> {
    let _ = days;
    todo!("port: history::timeline")
}

/// Parity hook: see ARCHITECTURE.md.
pub fn parity(_func: &str, _args: &serde_json::Value) -> Option<serde_json::Value> {
    None
}
