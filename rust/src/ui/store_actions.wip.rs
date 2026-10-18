//! UI section `store_actions` (port of `app.py` lines 3153-3675):
//! install/remove from the store, setup after install, themes, AppImages.
//!
//! STUBS: every method has its real signature and a body that does nothing.

#![allow(dead_code, unused_variables)]

use std::path::{Path, PathBuf};

use crate::plugin_setup::SetupPlan;
use crate::store::Plugin;
use crate::tasks::LogFn;
use crate::ui::shell::Win;

/// This section's own state. Every attribute these methods touch
/// (`store_model`, `_plugins`, `_thumb_pool`, `toasts`, `hero`) belongs to
/// another section or the shell, so there is nothing of its own to keep.
#[derive(Default)]
pub struct State {}

impl Win {
    // ---- marketplace actions ---------------------------------------------
    pub fn on_card_activated(&self, position: u32) {}

    /// Install from the store, without leaving the app.
    pub fn on_plugin_install(&self, p: &Plugin) {}

    // ---- setup after install ----------------------------------------------
    /// Where `omarchy plugin add` put this plugin: named by manifest id.
    pub fn plugin_dir(&self, p: &Plugin) -> Option<PathBuf> {
        None
    }

    /// Read the installed plugin's README and walk through its setup.
    pub fn offer_setup(&self, p: &Plugin) {}

    pub fn show_setup(&self, p: &Plugin, plan: &SetupPlan) {}

    pub fn run_setup(&self, p: &Plugin, plan: &SetupPlan) {}

    /// Remove, with the command that kind of entry actually uses.
    ///
    /// Three catalogues, three removal mechanisms. Dispatching on the kind
    /// rather than assuming `omarchy plugin remove` matters: run against a
    /// theme that command addresses something that does not exist.
    pub fn on_plugin_remove(&self, p: &Plugin) {}

    // ---- store entries that are themes ----
    /// Themes install and apply as two steps, so they are offered as two.
    ///
    /// `omarchy theme install` clones it; `omarchy theme set` switches the
    /// whole desktop over. Doing both on one click would re-skin everything
    /// the moment someone clicked Install out of curiosity.
    pub fn install_theme(&self, p: &Plugin) {}

    pub fn theme_installed(&self, ok: bool, p: &Plugin) {}

    pub fn apply_theme_now(&self, p: &Plugin) {}

    pub fn after_theme(&self, ok: bool, p: &Plugin) {}

    pub fn remove_theme(&self, p: &Plugin) {}

    // ---- store entries that are AppImages, not plugins ----
    pub fn install_store_appimage(&self, p: &Plugin) {}

    /// Download, register and add a launcher entry. Raises on failure.
    ///
    /// Runs on the task's worker thread, so it takes no `&self` (`Win` is
    /// not `Send`).
    pub fn fetch_appimage(log: LogFn<'_>, p: &Plugin, dest: &Path) -> Result<(), String> {
        Ok(())
    }

    pub fn remove_store_appimage(&self, p: &Plugin) {}

    /// The app ran the command, so it knows when to re-read the system.
    pub fn store_after(&self, ok: bool, p: &Plugin, verb: &str) {}

    /// The full listing for one plugin, as the marketplace shows it.
    pub fn show_plugin(&self, p: &Plugin) {}

    /// `@staticmethod _plugin_badges`: (text, css class) pills for a plugin.
    pub fn plugin_badges(p: &Plugin) -> Vec<(String, String)> {
        Vec::new()
    }
}
