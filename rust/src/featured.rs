//! Port of `featured.py`.
//!
//! The landing page: what deserves attention right now.
//!
//! A "featured" section is usually an editorial list someone has to keep writing.
//! This one is assembled from what is true on the machine and in the marketplace
//! at the moment it is opened, which means it is never stale and never a lie.
//!
//! Four shelves, in the order a person actually cares about them:
//!
//! 1. **Needs you** — things that are wrong now: security updates, failed
//!    services, sources the marketplace can no longer reach. This is the only
//!    shelf that can be empty and that being empty is the good outcome.
//! 2. **New in the marketplace** — listed in the last few weeks, and filtered to
//!    what can actually be installed and has been reviewed, because "new" plus
//!    "unverified" is the worst combination to put in front of someone.
//! 3. **Trending** — upstream activity in the last week, ranked by stars. Recent
//!    commits on a popular plugin is the honest version of trending; a "views"
//!    counter is not something this app can see or should guess at.
//! 4. **Because you run X** — the tags of what is already installed, matched
//!    against what is not. No model, no profile, no tracking: a set
//!    intersection over the local plugin list.

#![allow(dead_code, unused_variables)]

use serde::{Deserialize, Serialize};

use crate::inventory::App;
use crate::maintenance::Finding;
use crate::store::Plugin;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Shelf {
    pub key: String,
    pub title: String,
    pub subtitle: String,
    pub items: Vec<Item>,
    pub empty: String,
}

impl Default for Shelf {
    fn default() -> Self {
        Shelf {
            key: String::new(),
            title: String::new(),
            subtitle: String::new(),
            items: Vec::new(),
            empty: "Nothing here.".to_string(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Item {
    pub title: String,
    pub detail: String,
    /// crit | warn | "" for neutral
    pub severity: String,
    /// a store.Plugin, when it is a listing
    pub plugin: Option<Plugin>,
    /// an inventory.App, when it is installed
    pub app: Option<App>,
    /// button label
    pub action: String,
    pub kind: String,
}

pub const DAY: i64 = 86400;
/// No single marketplace may own a shelf. Star counts are not comparable
/// across them -- a vendor's flagship app carries 15,000 stars where the
/// busiest Omarchy plugin carries 2,151 -- so an uncapped ranking is really
/// just "whichever catalogue has the biggest projects in it".
pub const PER_MARKET: usize = 3;

fn _diversify(ranked: &[Plugin], limit: usize, per_market: usize) -> Vec<Plugin> {
    todo!("port: featured::_diversify")
}

fn _ts(value: &str) -> f64 {
    crate::util::iso_ts(value)
}

/// Only genuine problems. A long list here would defeat the purpose.
pub fn needs_you(apps: &[App], findings: &[Finding]) -> Shelf {
    todo!("port: featured::needs_you")
}

/// Python defaults: `days=21`, `limit=8`.
pub fn new_listings(plugins: &[Plugin], days: i64, limit: usize) -> Shelf {
    todo!("port: featured::new_listings")
}

/// Python defaults: `days=7`, `limit=8`.
pub fn trending(plugins: &[Plugin], days: i64, limit: usize) -> Shelf {
    todo!("port: featured::trending")
}

/// Match the tags of what is installed against what is not.
///
/// Deliberately not a recommender: it is a set intersection over tags the
/// marketplace already publishes, computed locally, with nothing sent
/// anywhere and nothing remembered between runs.
///
/// Python default: `limit=8`.
pub fn because_you_run(plugins: &[Plugin], limit: usize) -> Shelf {
    todo!("port: featured::because_you_run")
}

/// Every shelf, in display order. Pure; safe to call off the UI thread.
pub fn build(apps: &[App], findings: &[Finding], plugins: &[Plugin]) -> Vec<Shelf> {
    todo!("port: featured::build")
}

/// Parity hook: see ARCHITECTURE.md.
pub fn parity(_func: &str, _args: &serde_json::Value) -> Option<serde_json::Value> {
    None
}
