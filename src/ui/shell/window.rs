//! The window: `Win`, `Inner`, the state sections share, the sidebar rows,
//! the texture cache.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::Deref;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use crate::i18n::tr;
use crate::inventory;

use super::tables::COLLAPSED_BY_DEFAULT;
use super::PrimaryAction;

/// A sidebar destination: the row plus what Python stashed on it
/// (`row._key`, `row._count`, `row._group`).
#[derive(Debug, Clone)]
pub struct SidebarRow {
    pub key: String,
    pub row: adw::ActionRow,
    pub count: gtk::Label,
    pub group: String,
}

/// A group's header row (`header._group`, `header._chevron`).
#[derive(Debug, Clone)]
pub struct GroupHeader {
    pub group: String,
    pub row: gtk::ListBoxRow,
    pub chevron: gtk::Image,
}

/// How much decoded image data `TexCache` keeps: about a hundred 640px
/// screenshots, or every icon on a page many times over.
pub const TEX_BUDGET: usize = 48 << 20;

/// Decoded textures by "path@width", so scrolling back is free.
///
/// Bounded by the bytes the pixels take (`tex_bytes`), not by a count: a
/// 72px icon is 20 KB, a 1200px hero image several MB. When it is full the
/// least recently used go first, one at a time, so what is on screen stays.
pub struct TexCache<T = gdk::Texture> {
    /// value, its size, and when it was last used
    entries: HashMap<String, (T, usize, u64)>,
    /// last use -> key, oldest first
    order: BTreeMap<u64, String>,
    tick: u64,
    bytes: usize,
    budget: usize,
}

impl<T: Clone> TexCache<T> {
    pub fn new(budget: usize) -> Self {
        TexCache {
            entries: HashMap::new(),
            order: BTreeMap::new(),
            tick: 0,
            bytes: 0,
            budget,
        }
    }

    fn touch(&mut self) -> u64 {
        self.tick += 1;
        self.tick
    }

    /// The value for `key`, now the most recently used.
    pub fn get(&mut self, key: &str) -> Option<T> {
        let tick = self.touch();
        let (value, _, used) = self.entries.get_mut(key)?;
        self.order.remove(used);
        *used = tick;
        let value = value.clone();
        self.order.insert(tick, key.to_string());
        Some(value)
    }

    /// Keep `value` (`bytes` big) under `key`, dropping the least recently
    /// used until it fits. One bigger than the whole budget is not kept.
    pub fn insert(&mut self, key: String, value: T, bytes: usize) {
        self.remove(&key);
        if bytes > self.budget {
            return;
        }
        while self.bytes + bytes > self.budget {
            let Some((_, oldest)) = self.order.pop_first() else {
                break;
            };
            if let Some((_, size, _)) = self.entries.remove(&oldest) {
                self.bytes -= size;
            }
        }
        let tick = self.touch();
        self.order.insert(tick, key.clone());
        self.entries.insert(key, (value, bytes, tick));
        self.bytes += bytes;
    }

    fn remove(&mut self, key: &str) {
        if let Some((_, size, used)) = self.entries.remove(key) {
            self.order.remove(&used);
            self.bytes -= size;
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The bytes held.
    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

/// What a texture holds decoded: four bytes a pixel.
pub fn tex_bytes(tex: &gdk::Texture) -> usize {
    tex.width().max(0) as usize * tex.height().max(0) as usize * 4
}

/// State read and written by more than one section.
pub struct Shared {
    pub filter_key: String,
    pub query: String,
    /// `self._page`: None until `_show_page` first sets it. The Python
    /// reads it with `getattr(self, "_page", <default>)` and a different
    /// default in different places ("packages", "discover", ""), so readers
    /// go through `page_or` with the default their Python line has.
    pub page: Option<String>,
    /// `self.store`: every row of the package tables, as boxed `AppItem`s.
    pub store: gio::ListStore,
    /// Filter -> sort -> selection. Filtering first means the sorter only
    /// ever walks the rows actually on screen. (`self.filter`; its function
    /// is `Win::match`, set when the window is built.)
    pub filter: gtk::CustomFilter,
    /// `self.sorted`
    pub sorted: gtk::SortListModel,
    /// Decoded textures, so scrolling back is free (see `TexCache`).
    pub tex_cache: TexCache,
    /// `self.sidebar_rows`, in sidebar order.
    pub sidebar_rows: Vec<SidebarRow>,
    /// `self.group_rows`: None for the ungrouped primary group.
    pub group_rows: Vec<(String, Option<GroupHeader>)>,
    pub group_members: BTreeMap<String, Vec<SidebarRow>>,
    /// `self._collapsed`
    pub collapsed: BTreeSet<String>,
    pub primary: Option<PrimaryAction>,
    pub manual_sorter: Option<gtk::Sorter>,
    pub narrow: bool,
    /// Row-menu actions resolve by name at activation time, because the row
    /// widgets themselves are recycled as the list scrolls.
    pub row_actions: HashMap<String, inventory::App>,
    pub discovering: bool,
    pub discover_timer: Option<glib::SourceId>,
    // Marketplace state. Images are fetched on a small pool -- 5,600 cards
    // would otherwise open 5,600 connections on the first scroll -- and
    // decoded textures are kept so scrolling back is free.
    //
    // Shared, not copied: the store page's results and the workers that
    // read the catalogue hold the same list. A change in place
    // (`annotate`) goes through `Arc::make_mut`.
    pub plugins: std::sync::Arc<Vec<crate::store::Plugin>>,
    pub store_cats: Vec<String>,
    pub store_cat_key: String,
    pub store_timer: Option<glib::SourceId>,
    pub store_loaded_once: bool,
    pub pinning: bool,
    pub findings: Findings,
    pub maint_loaded_once: bool,
    pub ai_survey: Vec<crate::ai::Group>,
    pub ai_loaded_once: bool,
    pub shelves: Vec<crate::featured::Shelf>,
    pub feat_loaded_once: bool,
    /// Findings already announced, so a rescan does not re-notify.
    pub reported: BTreeSet<String>,
    /// `self._plugin_updates`: written by the Plugins page, read by the
    /// header's primary action (`getattr(self, "_plugin_updates", 0)`).
    pub plugin_updates: usize,
    /// `self.store_sort`: the App Store's sort dropdown. Built by the store
    /// section, reset by the AI page's filter reset, set by Today's shelf links.
    pub store_sort: Option<gtk::DropDown>,
    /// The rest of the store's filter drawer (`self.store_cat`,
    /// `store_trust`, `store_market`, `store_verified`, `store_installed`):
    /// built by the store section, reset by `store_reset_filters` (ai).
    pub store_cat: Option<gtk::DropDown>,
    pub store_trust: Option<gtk::DropDown>,
    pub store_market: Option<gtk::DropDown>,
    pub store_verified: Option<gtk::ToggleButton>,
    pub store_installed: Option<gtk::ToggleButton>,
    /// `self.store_drawer`: the filter drawer behind `filter_btn`, built by
    /// the store section and attached by `show_page`.
    pub store_drawer: Option<gtk::Popover>,
    /// `self.store_model`: the cards showing, as boxed `PluginItem`s. Filled
    /// by the store section, read by `on_card_activated` (store_actions).
    pub store_model: gio::ListStore,
    /// `self.store_grid`: its column count follows `set_narrow`.
    pub store_grid: Option<gtk::GridView>,
    /// `self._store_markets`: market ids parallel to the marketplace
    /// dropdown ("" = all). Read by `pin_market` / `retitle_store`.
    pub store_markets: Vec<String>,
    /// `self._filters_for`: whether the filter lists were last built for the
    /// Themes page (None: never built). Read by `pin_market`.
    pub filters_for: Option<bool>,
    /// `self._plugins is None`. The Python's `__init__` starts `_plugins` as
    /// `[]`, so this starts false; a Preferences marketplace switch
    /// (`reload_store`) sets it (and empties `plugins`, as None holds
    /// nothing). Whoever assigns `plugins` (`store_loaded`, `feat_reload`'s
    /// fetch) sets it back to false; `plugins` stays a `Vec` because every
    /// other reader treats None as an empty list.
    pub plugins_none: bool,
}

impl Default for Shared {
    fn default() -> Shared {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let filter = gtk::CustomFilter::new(|_| true);
        let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
        let sorted = gtk::SortListModel::new(Some(filtered), None::<gtk::Sorter>);
        Shared {
            filter_key: "all".to_string(),
            query: String::new(),
            page: None,
            store,
            filter,
            sorted,
            tex_cache: TexCache::new(TEX_BUDGET),
            sidebar_rows: Vec::new(),
            group_rows: Vec::new(),
            group_members: BTreeMap::new(),
            collapsed: COLLAPSED_BY_DEFAULT
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            primary: None,
            manual_sorter: None,
            narrow: false,
            row_actions: HashMap::new(),
            discovering: false,
            discover_timer: None,
            plugins: Default::default(),
            store_cats: vec![String::new()],
            store_cat_key: String::new(),
            store_timer: None,
            store_loaded_once: false,
            pinning: false,
            findings: Findings::default(),
            maint_loaded_once: false,
            ai_survey: Vec::new(),
            ai_loaded_once: false,
            shelves: Vec::new(),
            feat_loaded_once: false,
            reported: BTreeSet::new(),
            plugin_updates: 0,
            store_sort: None,
            store_cat: None,
            store_trust: None,
            store_market: None,
            store_verified: None,
            store_installed: None,
            store_drawer: None,
            store_model: gio::ListStore::new::<glib::BoxedAnyObject>(),
            store_grid: None,
            store_markets: vec![String::new()],
            filters_for: None,
            plugins_none: false,
        }
    }
}

/// `self._findings`. The Python assigns it from worker threads as well as
/// the main one (`_feat_reload`'s worker sets it the moment it has run the
/// checks, before its `idle_add`), and what reads it next -- the sidebar
/// stats' "Free-able", the hero -- sees it at once. A handle the workers can
/// hold keeps that: one list, behind a lock, shared by every clone.
#[derive(Clone, Default)]
pub struct Findings(std::sync::Arc<std::sync::Mutex<Vec<crate::maintenance::Finding>>>);

impl Findings {
    /// The list as it is now.
    pub fn get(&self) -> Vec<crate::maintenance::Finding> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// `self._findings = findings`
    pub fn set(&self, findings: Vec<crate::maintenance::Finding>) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = findings;
    }
}

impl Shared {
    /// `getattr(self, "_page", default)`.
    pub fn page_or(&self, default: &str) -> String {
        self.page.clone().unwrap_or_else(|| default.to_string())
    }

    /// `self.sidebar_rows.get(key)`.
    pub fn sidebar_row(&self, key: &str) -> Option<&SidebarRow> {
        self.sidebar_rows.iter().find(|r| r.key == key)
    }

    /// `key in self.group_rows`
    pub fn has_group(&self, key: &str) -> bool {
        self.group_rows.iter().any(|(k, _)| k == key)
    }
}

/// State only this section uses.
#[derive(Default)]
pub struct State {
    /// Kept so the theme watch keeps delivering (a dropped monitor stops).
    pub theme_monitors: Vec<gio::FileMonitor>,
    /// pacman.log watch; None when it could not be set up.
    pub monitor: Option<gio::FileMonitor>,
    pub pending_rescan: Option<glib::SourceId>,
    pub config_monitors: Vec<gio::FileMonitor>,
    pub config_pending: Option<glib::SourceId>,
    pub config_reported: BTreeSet<String>,
    /// (sort column title, descending) restored from window-state.json.
    pub wanted_sort: Option<(Option<String>, bool)>,
}

/// The window. Cheap to clone; callbacks hold a `WeakWin`.
#[derive(Clone)]
pub struct Win(pub(super) Rc<Inner>);

/// A weak handle on the window, for signal handlers.
#[derive(Clone)]
pub struct WeakWin(pub(super) Weak<Inner>);

impl WeakWin {
    pub fn upgrade(&self) -> Option<Win> {
        self.0.upgrade().map(Win)
    }
}

impl Deref for Win {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

/// Everything the window owns: the widgets shared across sections (created
/// once, never replaced), the shared state, and one state per section.
pub struct Inner {
    pub window: adw::ApplicationWindow,
    /// The Omarchy palette, applied over our own stylesheet.
    pub theme_provider: gtk::CssProvider,
    pub sidebar_toggle: gtk::ToggleButton,
    /// The sidebar's stats footer.
    pub stats: gtk::Grid,
    pub title: adw::WindowTitle,
    pub busy: gtk::Spinner,
    pub search: gtk::SearchEntry,
    pub search_bar: gtk::Box,
    pub update_btn: gtk::Button,
    pub sort_btn: gtk::MenuButton,
    /// The same control as `sort_btn`: a sort menu on the package list and a
    /// filter drawer in the store.
    pub filter_btn: gtk::MenuButton,
    pub store_back: gtk::Button,
    pub view: gtk::ColumnView,
    /// `self._priority_sorter`: urgency, then name.
    pub priority_sorter: gtk::CustomSorter,
    pub list_stack: gtk::Stack,
    pub list_empty: adw::StatusPage,
    /// Above the Updates table: why the count is what it is, and whether the
    /// batch needs a restart (`mirror::notes`).
    pub update_notes: gtk::ListBox,
    pub content_stack: gtk::Stack,
    pub toasts: adw::ToastOverlay,
    pub split: adw::OverlaySplitView,
    /// `self._thumb_pool`: six workers for images (see `thumb_bg`). None
    /// only if GLib could not make a pool; `thumb_bg` then uses `spawn_bg`.
    pub thumb_pool: Option<glib::ThreadPool>,

    pub shared: RefCell<Shared>,
    pub shell: RefCell<State>,
    pub packages: RefCell<crate::ui::packages::State>,
    pub store: RefCell<crate::ui::store::State>,
    pub store_actions: RefCell<crate::ui::store_actions::State>,
    pub plugins: RefCell<crate::ui::plugins::State>,
    pub switchboard: RefCell<crate::ui::switchboard::State>,
    pub history: RefCell<crate::ui::history::State>,
    pub ai: RefCell<crate::ui::ai::State>,
    pub preferences: RefCell<crate::ui::preferences::State>,
    pub today: RefCell<crate::ui::today::State>,
    pub actions: RefCell<crate::ui::actions::State>,
}

/// A `GtkLabel` that is centred vertically and hidden: a sidebar count.
fn count_label() -> gtk::Label {
    let count = gtk::Label::builder()
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    count.add_css_class("count-badge");
    count
}

/// One sidebar destination row (the loop body shared by both kinds of group).
pub(super) fn sidebar_entry(key: &str, label: &str, icon: &str, gkey: &str) -> SidebarRow {
    let row = adw::ActionRow::builder().title(tr(label)).build();
    row.set_title_lines(1);
    row.add_prefix(&gtk::Image::from_icon_name(icon));
    let count = count_label();
    row.add_suffix(&count);
    SidebarRow {
        key: key.to_string(),
        row,
        count,
        group: gkey.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_texture_cache_drops_the_least_recently_used_by_size() {
        let mut c: TexCache<u32> = TexCache::new(100);
        c.insert("a".into(), 1, 40);
        c.insert("b".into(), 2, 40);
        assert_eq!(c.get("a"), Some(1)); // a is now newer than b
        c.insert("c".into(), 3, 40); // over budget: b goes, not a
        assert_eq!(
            (c.get("b"), c.get("a"), c.get("c")),
            (None, Some(1), Some(3))
        );
        assert_eq!((c.len(), c.bytes()), (2, 80));
        // replacing a key does not count it twice
        c.insert("c".into(), 4, 50);
        assert_eq!((c.len(), c.bytes()), (2, 90));
        // one big entry evicts as many as it needs
        c.insert("big".into(), 5, 100);
        assert_eq!((c.len(), c.bytes(), c.get("big")), (1, 100, Some(5)));
        // and one bigger than the whole budget is not kept at all
        c.insert("huge".into(), 6, 101);
        assert_eq!((c.get("huge"), c.get("big")), (None, Some(5)));
        // many small ones stay within the budget
        for i in 0..1000 {
            c.insert(format!("i{i}"), i, 7);
        }
        assert!(c.bytes() <= 100 && c.len() == 14);
        assert_eq!(c.get("i999"), Some(999));
    }
}
