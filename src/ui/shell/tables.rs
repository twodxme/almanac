//! The window's tables (AlmanacWindow class attributes).

use crate::inventory;

/// (key, label, icon) of one sidebar destination. Labels are msgids:
/// translate with `i18n::tr` where they are shown.
pub type GroupItem = (&'static str, &'static str, &'static str);

/// Grouped by the question each section answers: what can I get, what
/// wants my attention, what do I have, and where did it come from. Eleven
/// peers in one list made all of them equally weighted, which none are.
pub const GROUPS: &[(&str, Option<&str>, &[GroupItem])] = &[
    // The two primary destinations, ungrouped: getting software and
    // keeping it current are what the app is for, and a header above them
    // would rank them as peers of "Sources".
    ("primary", None, &[
        // "featured" stays the key: saved state and --page featured keep
        // working. Nothing here is featured by anyone; it is what is true
        // today.
        ("featured", "Today", "weather-clear-symbolic"),
        ("discover", "App Store", "system-software-install-symbolic"),
        ("themes", "Themes", "preferences-desktop-wallpaper-symbolic"),
        ("updates", "Updates", "software-update-available-symbolic"),
        (inventory::AGENT, "AI", "applications-science-symbolic"),
    ]),
    ("library", Some("Library"), &[
        ("apps", "Applications", "view-grid-symbolic"),
        ("plugins", "Plugins", "application-x-addon-symbolic"),
        ("switchboard", "Switchboard", "almanac-switchboard-symbolic"),
        ("all", "Everything", "view-list-symbolic"),
    ]),
    ("sources", Some("Sources"), &[
        (inventory::SYSTEM, "System", "preferences-system-symbolic"),
        (inventory::REPO, "Repositories", "package-x-generic-symbolic"),
        (inventory::AUR, "AUR", "package-x-generic-symbolic"),
        (inventory::APPIMAGE, "AppImages", "application-x-executable-symbolic"),
        (inventory::FLATPAK, "Flatpak", "application-x-executable-symbolic"),
        (inventory::LOCAL, "Local builds", "applications-engineering-symbolic"),
        (inventory::TOOL, "Dev tools", "utilities-terminal-symbolic"),
    ]),
    ("maintenance", Some("Maintenance"), &[
        ("maintenance", "Health check", "object-select-symbolic"),
        ("history", "History", "document-open-recent-symbolic"),
        (inventory::ORPHAN, "Orphans", "user-trash-symbolic"),
    ]),
];

/// Sources collapse by default: they answer "where did this come from",
/// which is a question you ask occasionally, not on every launch.
pub const COLLAPSED_BY_DEFAULT: &[&str] = &["sources"];

/// One entry of `PAGES`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageSpec {
    pub key: &'static str,
    /// which stack child to show
    pub view: &'static str,
    /// msgid; translate where shown
    pub title: &'static str,
    /// placeholder, or None when the box does nothing here
    pub search: Option<&'static str>,
    /// whether the Python wraps `search` in `_()` (some placeholders are not)
    pub search_translated: bool,
    /// show the "Update all" button
    pub update: bool,
    /// show the sort menu
    pub sort: bool,
    pub filters: bool,
    pub market: Option<&'static str>,
}

/// Every page, described once. Before this the header's buttons were shown
/// and hidden from seventeen places across six methods, which is how a page
/// ends up with another page's toolbar.
pub const PAGES: &[PageSpec] = &[
    PageSpec { key: "featured", view: "featured", title: "Today", search: None,
               search_translated: false, update: false, sort: false, filters: false,
               market: None },
    PageSpec { key: "discover", view: "store", title: "App Store",
               search: Some("Search plugins and apps…"), search_translated: false,
               update: false, sort: false, filters: true, market: None },
    // The same view, pinned to one marketplace. Without this themes were
    // reachable only through the filter drawer, and the first one did not
    // appear until the thirtieth card of the default ordering.
    PageSpec { key: "themes", view: "store", title: "Themes",
               search: Some("Search Omarchy themes…"), search_translated: false,
               update: false, sort: false, filters: true, market: Some("themes") },
    PageSpec { key: "packages", view: "list", title: "Software",
               search: Some("Search installed software"), search_translated: false,
               update: true, sort: true, filters: false, market: None },
    PageSpec { key: "maintenance", view: "maintenance", title: "Health check", search: None,
               search_translated: false, update: false, sort: false, filters: false,
               market: None },
    PageSpec { key: "plugins", view: "plugins", title: "Plugins",
               search: Some("Search plugins"), search_translated: true,
               update: false, sort: false, filters: false, market: None },
    PageSpec { key: "switchboard", view: "switchboard", title: "Switchboard",
               search: Some("Search the switchboard"), search_translated: true,
               update: false, sort: false, filters: false, market: None },
    PageSpec { key: "history", view: "history", title: "History", search: None,
               search_translated: false, update: false, sort: false, filters: false,
               market: None },
    PageSpec { key: inventory::AGENT, view: "ai", title: "AI", search: None,
               search_translated: false, update: false, sort: false, filters: false,
               market: None },
];

/// `PAGES.get(key)`.
pub fn page_spec(key: &str) -> Option<&'static PageSpec> {
    PAGES.iter().find(|p| p.key == key)
}

/// Ctrl+1 … Ctrl+0, in sidebar order. At most ten: there is no Ctrl+10, and
/// an invalid accelerator makes the window fail to open at all.
pub const PAGE_KEYS: &[&str] = &[
    "featured", "discover", "themes", "updates", inventory::AGENT, "apps", "plugins",
    "switchboard", "maintenance", "history",
];

/// The accelerator of the `n`th page key (1-based): `f"<Control>{n % 10}"`.
pub fn page_accel(n: usize) -> String {
    format!("<Control>{}", n % 10)
}

/// `self.SORTS_INSTALLED`: (key, label) of the package list's sort menu.
pub fn sorts_installed() -> Vec<(&'static str, String)> {
    use crate::i18n::tr;
    vec![
        ("default", tr("Priority (default)")),
        ("name", tr("Name A → Z")),
        ("name_desc", tr("Name Z → A")),
        ("size", tr("Size")),
        ("installed", tr("Install date")),
        ("category", tr("Category")),
        ("behind", tr("Versions behind")),
    ]
}

/// `self.SORTS_DISCOVER`. Only sorts the registry data can actually support:
/// there is no category for 5,576 of the 5,578 plugins, so offering one would
/// be a menu item that silently does nothing.
pub fn sorts_discover() -> Vec<(&'static str, String)> {
    use crate::i18n::tr;
    vec![
        ("default", tr("Best match (default)")),
        ("popular", tr("Most popular")),
        ("trending", tr("Trending")),
        ("newest", tr("Recently added")),
        ("name", tr("Name A → Z")),
        ("name_desc", tr("Name Z → A")),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtk::gdk;

    /// test_every_page_shortcut_is_a_valid_accelerator: Ctrl+10 does not
    /// exist; an invalid accelerator stops the window from being constructed
    /// at all.
    #[test]
    fn every_page_shortcut_is_a_valid_accelerator() {
        assert!(PAGE_KEYS.len() <= 10);
        // `Gtk.ShortcutTrigger.parse_string` needs GTK initialised on the
        // main thread, which a test thread is not; the accelerator is
        // checked the way GTK reads it instead: a modifier GTK knows, then a
        // key name GDK resolves (gdk_keyval_from_name needs no GTK).
        let mut seen = std::collections::BTreeSet::new();
        for i in 1..=PAGE_KEYS.len() {
            let accel = page_accel(i);
            let key = accel.strip_prefix("<Control>").expect("a <Control> accelerator");
            assert!(gdk::Key::from_name(key).is_some(), "{accel}: no such key");
            assert!(seen.insert(accel.clone()), "{accel} used twice");
        }
        // The check would catch the mistake it is for.
        assert!(gdk::Key::from_name("10").is_none());
    }
}
