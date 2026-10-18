//! UI section `preferences` (port of `app.py` lines 4876-5096): the
//! Preferences dialog and "Run the update now".
//!
//! STUBS: every method exists with its real signature; bodies do nothing yet.

#![allow(dead_code, unused_imports, unused_variables)]

use adw::prelude::*;
use gtk::glib;

use crate::ui::shell::Win;

/// Per-section state. The Preferences dialog keeps nothing of its own on the
/// window between openings: every widget is a local of `on_preferences`, and
/// the attributes it touches (`_plugins`, `content_stack`) are shared with
/// other sections.
#[derive(Default)]
pub struct State {}

// ---- settings ---------------------------------------------------------
impl Win {
    /// Almanac's preferences: General, Updates, Privacy, Background.
    ///
    /// Scheduled-update changes are applied when the dialog closes (one
    /// daemon-reload, not one per switch); everything else on change.
    pub fn on_preferences(&self) {}

    /// Runs `~/.local/bin/omarchy-auto-update` through `tasks::run` as
    /// "Scheduled update", then `after_pacman`.
    pub fn run_scheduled_now(&self) {}
}
