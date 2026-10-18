//! UI section `switchboard` (port of `app.py` lines 3993-4221).
//!
//! The on/offs with no other home: startup apps, background services and
//! Almanac's own background work.

use std::collections::BTreeMap;

use adw::prelude::*;
use gtk::glib;

use crate::autostart;
use crate::i18n::tr;
use crate::services;
use crate::ui::shell::Win;

/// Switchboard state (`self.switch_body`, `self._switch_rows`).
#[derive(Default)]
pub struct State {
    /// `self.switch_body`: the column the sections are appended to.
    pub switch_body: Option<gtk::Box>,
    /// `self._switch_rows`: every filterable widget with the text it
    /// matches against (`(row, hay)`).
    pub switch_rows: Vec<(gtk::Widget, String)>,
}

/// What `apply(on)` reports: the state now in effect (`None` when it cannot
/// tell), or the error text.
pub type ApplyResult = Result<Option<bool>, String>;

/// The keyword arguments of `_switch_row`.
pub struct SwitchOpts {
    pub prefix: Option<gtk::Widget>,
    /// `(label, css class)` pills shown before the switch.
    pub pills: Vec<(String, String)>,
    /// Asked before switching off something that matters; empty = no asking.
    pub confirm_off: String,
    pub sensitive: bool,
}

impl Default for SwitchOpts {
    fn default() -> Self {
        SwitchOpts { prefix: None, pills: Vec::new(), confirm_off: String::new(), sensitive: true }
    }
}

/// The text a search is matched against for a startup app row.
pub fn autostart_hay(e: &autostart::Entry) -> String {
    format!("{} {} {}", e.name, e.filename, e.comment)
}

/// The text a search is matched against for a service row.
pub fn service_hay(u: &services::Service) -> String {
    format!("{} {}", u.description, u.unit)
}

/// Whether a row with `hay` stays visible for the query `text`.
pub fn matches(text: &str, hay: &str) -> bool {
    let q = text.trim().to_lowercase();
    q.is_empty() || hay.to_lowercase().contains(&q)
}

/// The pills a service row carries: `(label, css class)`.
pub fn service_pills(u: &services::Service) -> Vec<(String, String)> {
    let mut pills = Vec::new();
    if !u.essential.is_empty() {
        pills.push((tr("Essential"), "pill-warn".to_string()));
    }
    if u.omarchy {
        pills.push((tr("Omarchy"), "pill-quiet".to_string()));
    }
    if u.active && u.enabled {
        pills.push((tr("Running"), "pill-ok".to_string()));
    }
    pills
}

/// What a service row asks before switching off (empty: nothing).
pub fn service_warning(u: &services::Service) -> String {
    if !u.essential.is_empty() {
        format!("Without it: {} stops working.", u.essential.to_lowercase())
    } else if u.omarchy {
        tr("Omarchy set this up, and a later omarchy update may expect it to be on.")
    } else {
        String::new()
    }
}

/// The page summary after a load.
pub fn summary(starts: usize, units: usize) -> String {
    let switches = starts + units + 2;
    format!("{switches} switches · {starts} startup apps · {units} services")
}

/// The Plugins row's subtitle: how many of the plugins are on.
pub fn plugins_subtitle(on: &BTreeMap<String, bool>) -> String {
    let n_on = on.values().filter(|v| **v).count();
    format!(
        "{n_on} of {} on — switched on the Plugins page, beside their updates and shortcuts",
        on.len()
    )
}

impl Win {
    /// The on/offs with no other home: startup apps, background services
    /// and Almanac's own background work. Plugins are switched on the
    /// Plugins page, beside their state and actions; here they get one row
    /// that says how many are on and goes there.
    pub fn build_switchboard(&self) -> gtk::Widget {
        gtk::Box::new(gtk::Orientation::Vertical, 0).upcast()
    }

    pub fn switchboard_reload(&self) {}

    pub fn switchboard_show(
        &self,
        on: BTreeMap<String, bool>,
        starts: Vec<autostart::Entry>,
        units: Vec<services::Service>,
    ) -> glib::ControlFlow {
        let _ = (on, starts, units);
        glib::ControlFlow::Break
    }

    pub fn switchboard_filter(&self, text: &str) {
        let _ = text;
    }

    /// One row with a switch that moves only when the change holds.
    ///
    /// `apply(on)` runs off the UI thread and returns the state now in
    /// effect (or None when it cannot tell). `confirm_off` is asked before
    /// switching off something that matters.
    pub fn switch_row<F>(
        &self,
        title: &str,
        subtitle: &str,
        active: bool,
        apply: F,
        opts: SwitchOpts,
    ) -> adw::ActionRow
    where
        F: Fn(bool) -> ApplyResult + Send + Sync + 'static,
    {
        let _ = (title, subtitle, active, apply, opts);
        adw::ActionRow::new()
    }

    pub fn switch_autostart_row(&self, e: &autostart::Entry) -> adw::ActionRow {
        let _ = e;
        adw::ActionRow::new()
    }

    pub fn switch_service_row(&self, u: &services::Service) -> adw::ActionRow {
        let _ = u;
        adw::ActionRow::new()
    }

    pub fn switch_almanac_rows(&self) -> Vec<(adw::ActionRow, String)> {
        Vec::new()
    }
}
