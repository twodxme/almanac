//! List items: the rows the list models hold.

use std::cell::Ref;

use adw::prelude::*;
use gtk::glib;

use crate::inventory;

use super::helpers::{as_behind, as_float, as_int, as_text, primary_category, truthy};

/// Wrapper so `inventory::App` can live in a `gio::ListStore` (as a
/// `glib::BoxedAnyObject`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AppItem {
    pub app: inventory::App,
    // Flattened for the sorters, which must be cheap: they run on every
    // comparison during a sort.
    pub name: String,
    pub category: String,
    pub size_mb: f64,
    pub installed_ts: f64,
    pub source: String,
    pub has_update: bool,
    pub critical: String,
    pub stale: String,
    pub advice: String,
    pub behind: i64,
    pub behind_kind: String,
    pub git_behind: i64,
    pub git_dirty: bool,
    pub priority: i32,
    pub votes: i64,
    pub popularity: f64,
    pub added: String,
    pub verified: bool,
    pub haystack: String,
}

impl AppItem {
    pub fn new(app: inventory::App) -> AppItem {
        let e = &app.extra;
        let mut category = primary_category(&as_text(e.get("categories")));
        if category.is_empty() && app.source == inventory::LOCAL {
            category = "Local build".to_string();
        }
        let critical = as_text(e.get("critical"));
        let stale = as_text(e.get("stale"));
        let (mut behind, behind_kind) = as_behind(e);
        let git_behind = as_int(e.get("git_behind"));
        if git_behind != 0 {
            // Sorting treats commits-behind as its own magnitude; mixing it
            // with semver counts would compare unlike things.
            behind = git_behind;
        }
        let has_update = app.has_update();
        // Critical first, then any update, then the rest -- so the default
        // sort already puts the rows that matter at the top.
        let priority = if !critical.is_empty() || !stale.is_empty() {
            0
        } else if has_update && app.source == inventory::SYSTEM {
            1
        } else if has_update {
            2
        } else {
            3
        };
        AppItem {
            name: app.name.clone(),
            size_mb: as_float(e.get("size_mb")),
            installed_ts: as_float(e.get("installed_ts")),
            source: app.source.clone(),
            has_update,
            advice: as_text(e.get("advice")),
            behind,
            behind_kind,
            git_behind,
            git_dirty: truthy(e.get("git_dirty")),
            priority,
            votes: as_int(e.get("votes")),
            popularity: as_float(e.get("popularity")),
            added: as_text(e.get("added")),
            verified: truthy(e.get("verified")),
            haystack: format!("{} {} {}", app.name, app.description, category).to_lowercase(),
            category,
            critical,
            stale,
            app,
        }
    }

    /// Box it for a `gio::ListStore`.
    pub fn boxed(self) -> glib::BoxedAnyObject {
        glib::BoxedAnyObject::new(self)
    }

    /// The `AppItem` inside a list-model row, or None if the row holds
    /// something else.
    pub fn of(obj: &glib::Object) -> Option<Ref<'_, AppItem>> {
        obj.downcast_ref::<glib::BoxedAnyObject>()?.try_borrow::<AppItem>().ok()
    }
}

/// A marketplace entry, boxed so a `gio::ListStore` can hold it.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginItem {
    pub plugin: crate::store::Plugin,
}

impl PluginItem {
    pub fn new(plugin: crate::store::Plugin) -> PluginItem {
        PluginItem { plugin }
    }

    /// Box it for a `gio::ListStore`.
    pub fn boxed(self) -> glib::BoxedAnyObject {
        glib::BoxedAnyObject::new(self)
    }

    /// The `PluginItem` inside a list-model row, or None.
    pub fn of(obj: &glib::Object) -> Option<Ref<'_, PluginItem>> {
        obj.downcast_ref::<glib::BoxedAnyObject>()?.try_borrow::<PluginItem>().ok()
    }
}
