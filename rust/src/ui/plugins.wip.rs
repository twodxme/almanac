//! UI section `plugins` (port of `app.py` lines 3695-3993 and 5636-5999):
//! the Plugins page; on/off, shortcuts, bulk updates, the shell — and the
//! row actions, install and remove that sit beside them in `app.py`.
//!
//! STUBS: every method exists with its real signature; bodies do nothing.

#![allow(dead_code, unused_variables, unused_imports)]

use std::collections::BTreeMap;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::inventory;
use crate::plugins as plugins_mod;
use crate::store as store_mod;
use crate::tasks;
use crate::ui::shell::Win;

/// State only the Plugins section reads and writes.
#[derive(Default)]
pub struct State {
    /// `self.plugins_body`: the page's column of sections.
    pub plugins_body: Option<gtk::Box>,
    /// `self._plugin_rows`: every row shown, with the plugin it stands for
    /// (searched by `plugins_filter`, looked up by `plugin_action`).
    pub plugin_rows: Vec<(adw::ActionRow, plugins_mod::Installed)>,
    /// `self._plugin_updates`: how many of the user's plugins have an update.
    pub plugin_updates: usize,
}

// ---- plugins -----------------------------------------------------------

/// `_PLUGIN_STATE_PILLS`: a plugin state key -> (pill label, css class).
pub const PLUGIN_STATE_PILLS: &[(&str, (&str, &str))] = &[
    ("update", ("Update waiting", "pill-accent")),
    ("modified", ("Local edits", "pill-warn")),
    ("ahead", ("Ahead of upstream", "pill-warn")),
    ("diverged", ("Diverged", "pill-warn")),
    ("detached", ("Not on a branch", "pill-warn")),
    ("symlink", ("Development link", "pill-quiet")),
    ("error", ("Could not check", "pill-warn")),
];

/// `key in _PLUGIN_STATE_PILLS` / `_PLUGIN_STATE_PILLS[key]`.
pub fn plugin_state_pill(key: &str) -> Option<(&'static str, &'static str)> {
    PLUGIN_STATE_PILLS.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// The pill key for a plugin state, as `_plugin_row` picks it: the kind for
/// "update" and "error", otherwise the detail ("modified", "symlink", ...).
pub fn plugin_state_key(st: Option<&plugins_mod::State>) -> String {
    match st {
        Some(st) if st.kind == "update" || st.kind == "error" => st.kind.clone(),
        Some(st) => st.detail.clone(),
        None => String::new(),
    }
}

/// Whether a row matches the search text, as `_plugins_filter` decides it.
pub fn plugin_matches(i: &plugins_mod::Installed, text: &str) -> bool {
    let q = text.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    let hay = [i.id.as_str(), i.name.as_str(), i.description.as_str(), &i.kinds.join(" ")]
        .join(" ")
        .to_lowercase();
    hay.contains(&q)
}

impl Win {
    /// Every plugin on the system, with a switch each. See plugins.py.
    pub fn build_plugins_page(&self) -> gtk::Widget {
        gtk::Box::new(gtk::Orientation::Vertical, 0).upcast()
    }

    pub fn plugins_reload(&self) {}

    pub fn plugins_show(
        &self,
        found: Vec<plugins_mod::Installed>,
        trust: BTreeMap<String, store_mod::Plugin>,
        keys: BTreeMap<String, String>,
    ) {
    }

    pub fn plugins_filter(&self, text: &str) {}

    pub fn plugin_row(
        &self,
        i: &plugins_mod::Installed,
        trust: Option<&store_mod::Trust>,
        shortcut: &str,
        listing: Option<&store_mod::Plugin>,
    ) -> adw::ActionRow {
        adw::ActionRow::new()
    }

    /// The mark beside a plugin, best source first (see plugins.py):
    /// the vendor's own logo for a plugin about a service, the marketplace
    /// artwork for a listed one, a system icon for what it does, the
    /// author's GitHub avatar, and a generic plugin icon last.
    ///
    /// The system icon shows at once; a logo replaces it when it arrives,
    /// so rows never jump.
    pub fn plugin_logo(
        &self,
        i: &plugins_mod::Installed,
        listing: Option<&store_mod::Plugin>,
    ) -> gtk::Box {
        gtk::Box::new(gtk::Orientation::Horizontal, 0)
    }

    /// A task step: drop a removed plugin's shortcut. Otherwise the
    /// binding outlives the plugin -- the key stays taken and opens nothing.
    /// Never fails the removal; a binding it cannot remove is reported.
    pub fn forget_shortcut(pid: &str) -> tasks::Step {
        let f: tasks::StepFn = std::sync::Arc::new(|_log: tasks::LogFn<'_>| Ok(()));
        tasks::Step::call(crate::i18n::tr("Removing its keyboard shortcut"), f)
    }

    /// Switch a plugin through Omarchy, then show what actually holds.
    ///
    /// The switch moves only once Omarchy's own list agrees; a refusal puts
    /// it back and says why.
    pub fn plugin_switch(
        &self,
        switch: &gtk::Switch,
        on: bool,
        i: &plugins_mod::Installed,
    ) -> glib::Propagation {
        glib::Propagation::Proceed
    }

    pub fn plugin_action(&self, action: &str, pid: &str) {}

    // ---- plugins: on/off, shortcuts, bulk updates, the shell ----------------

    pub fn row_toggle(&self, name: &str) {}

    /// Switch a plugin on or off through Omarchy, then check it took.
    pub fn toggle_plugin(&self, pid: &str, on: bool, label: &str) {}

    pub fn row_shortcut(&self, name: &str) {}

    /// Record a key combination and bind it to opening the plugin.
    pub fn edit_shortcut(&self, pid: &str, label: &str) {}

    /// Update every plugin with a clean update waiting, within the scope
    /// chosen in Preferences; say which were held back and why.
    pub fn on_update_plugins(&self) {}

    /// `trust` maps plugin id -> trust key (`{p.id: p.trust.key}`).
    pub fn confirm_plugin_updates(
        &self,
        states: Vec<plugins_mod::State>,
        trust: BTreeMap<String, String>,
    ) {
    }

    pub fn on_restart_shell(&self) {}

    pub fn row_aur(&self, name: &str) {}

    pub fn row_remove(&self, name: &str) {}

    /// Hand the URL to the desktop's default browser.
    pub fn open_url(&self, url: &str) {}

    pub fn on_launch(&self, app: &inventory::App) {}

    /// The one remaining escape to a terminal: see on_update_all.
    pub fn in_terminal(&self, script: &str, note: &str) -> bool {
        false
    }

    /// Remove only the orphans judged safe, with pacman asking first.
    ///
    /// Build tooling and debug packages are excluded: the next AUR build pulls
    /// rust straight back, so removing it is churn rather than cleanup.
    pub fn on_clean_orphans(&self) {}

    /// Install one result, through the tool that owns that kind of thing.
    pub fn on_install_item(&self, app: &inventory::App) {}

    /// Search and install, handed to yay in a terminal.
    ///
    /// yay's own interactive picker is better than anything reimplemented
    /// here, and it is the tool that owns the transaction.
    pub fn on_install(&self) {}

    /// Install through yay: snapshot first, then check what changed.
    pub fn install_package(&self, name: &str, argv: Vec<String>) {}

    /// Remove a package, with its orphaned dependencies.
    ///
    /// -Rns is what a user means by "uninstall", but it can take more than
    /// expected, so the terminal shows the full list before confirming --
    /// no --noconfirm here, deliberately.
    pub fn on_remove(&self, app: &inventory::App) {}
}
