//! UI section `history` (port of part of `app.py`): maintenance glue and
//! History (`app.py` lines 3675-3695 and 4221-4486).
//!
//! STUBS: every method exists with its real signature; bodies are empty and
//! cannot panic. Builders return an empty widget of the right type.

#![allow(dead_code, unused_variables)]

use gtk::prelude::*;

use crate::history;
use crate::i18n::tr;
use crate::inventory;
use crate::maintenance;
use crate::ui::shell::Win;

/// Widgets and data only the History and Health check pages use.
///
/// `_findings` is not here: Updates, the store header and Today read it too,
/// so it lives on `shell::Shared` as `findings`.
#[derive(Default)]
pub struct State {
    /// `self.maint_list`: the rows box of the one Health check section.
    pub maint_list: Option<gtk::ListBox>,
    /// `self.history_body`: one section per day goes in here.
    pub history_body: Option<gtk::Box>,
}

// ---- history ----------------------------------------------------------

/// `_HISTORY_ICONS`: the icon for each kind of history event.
pub const HISTORY_ICONS: &[(&str, &str)] = &[
    (history::PACKAGES, "package-x-generic-symbolic"),
    (history::SNAPSHOT, "camera-photo-symbolic"),
    (history::RESET, "edit-undo-symbolic"),
    (history::ALMANAC, "object-select-symbolic"),
];

/// `_HISTORY_ICONS.get(kind, "object-select-symbolic")`.
pub fn history_icon(kind: &str) -> &'static str {
    HISTORY_ICONS
        .iter()
        .find(|(k, _)| *k == kind)
        .map(|(_, icon)| *icon)
        .unwrap_or("object-select-symbolic")
}

/// `EMPTY_STATES`: (icon, title, body) for a filter that shows nothing.
/// Read by the packages section when a table comes up empty.
pub fn empty_states(key: &str) -> Option<(&'static str, String, String)> {
    let state = match key {
        "updates" => (
            "object-select-symbolic",
            tr("Everything is up to date"),
            tr("Nothing installed has a newer version waiting."),
        ),
        k if k == inventory::ORPHAN => (
            "object-select-symbolic",
            tr("No orphans"),
            tr("Every installed package is either asked for directly or needed by something that is."),
        ),
        k if k == inventory::FLATPAK => (
            "package-x-generic-symbolic",
            tr("No Flatpaks"),
            tr("Nothing is installed through Flatpak on this machine."),
        ),
        k if k == inventory::APPIMAGE => (
            "application-x-executable-symbolic",
            tr("No AppImages tracked"),
            tr("Install one from the App Store, or add an existing file with ☰ → Track an AppImage."),
        ),
        k if k == inventory::LOCAL => (
            "applications-engineering-symbolic",
            tr("No local builds"),
            tr("Nothing here is running from a git checkout."),
        ),
        k if k == inventory::AUR => (
            "package-x-generic-symbolic",
            tr("No AUR packages"),
            tr("Nothing foreign is installed."),
        ),
        _ => return None,
    };
    Some(state)
}

/// `_SEVERITY_CSS`: (pill label, pill css class) per finding severity.
/// Read by the Today section too.
pub fn severity_css(severity: &str) -> Option<(String, &'static str)> {
    match severity {
        s if s == maintenance::CRIT => Some((tr("Critical"), "pill-crit")),
        s if s == maintenance::WARN => Some((tr("Attention"), "pill-warn")),
        s if s == maintenance::INFO => Some((tr("Optional"), "pill")),
        s if s == maintenance::OK => Some((tr("OK"), "pill-ok")),
        _ => None,
    }
}

/// Finding ids whose fix removes something, so the confirm button is red.
pub const DESTRUCTIVE_FIXES: &[&str] =
    &["trash", "orphans", "dangling-desktop", "broken-symlinks", "rollbacks"];

/// The three cleanups `reclaim_space` bundles.
pub const RECLAIM_WANTED: &[&str] = &["pacman-cache", "yay-cache", "journal"];

impl Win {
    // ---- maintenance ------------------------------------------------------

    /// One page per question the system can answer about its own health.
    pub fn build_maintenance(&self) -> gtk::Box {
        gtk::Box::new(gtk::Orientation::Vertical, 0)
    }

    // ---- history ----------------------------------------------------------

    /// What changed on this machine, by day. See history.py.
    pub fn build_history(&self) -> gtk::Box {
        gtk::Box::new(gtk::Orientation::Vertical, 0)
    }

    pub fn history_reload(&self) {}

    pub fn history_show(&self, days: Vec<history::Day>) {}

    pub fn history_row(&self, e: &history::Event) -> adw::PreferencesRow {
        adw::ActionRow::new().upcast()
    }

    pub fn maint_reload(&self) {}

    pub fn maint_show(&self, findings: Vec<maintenance::Finding>) {}

    pub fn maint_row(&self, f: &maintenance::Finding) -> adw::PreferencesRow {
        adw::ActionRow::new().upcast()
    }

    pub fn maint_fix(&self, f: &maintenance::Finding) {}

    /// After fixes: ask each check again, and say what actually changed.
    pub fn findings_done(&self, fixed: Vec<maintenance::Finding>) -> crate::tasks::OnDone {
        Box::new(|_ok: bool| {})
    }

    pub fn maint_after(&self, ok: bool) {}

    /// The three cleanups that only ever free cached or logged data.
    pub fn reclaim_space(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_icon_falls_back() {
        assert_eq!(history_icon(history::SNAPSHOT), "camera-photo-symbolic");
        assert_eq!(history_icon("nope"), "object-select-symbolic");
    }
}
