//! UI section `store` (port of `app.py` lines 2481-3153): the App Store and
//! Themes pages -- the card grid, the card widgets, and the marketplace data
//! behind them.

use std::cell::Ref;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gtk::gdk_pixbuf;
use gtk::pango;
use gtk::prelude::*;
use gtk::{gdk, glib};

use crate::i18n::tr;
use crate::store as store_mod;
use crate::store::Plugin;
use crate::ui::shell::{compact, exc_str, page_spec, sorts_discover, spawn_bg, tex_bytes, PluginItem, Win, INSTALL_ICON};

use gtk::gio;

/// `item._parts`
#[derive(Clone)]
pub struct Card {
    pub pic: gtk::Picture,
    pub art: gtk::ScrolledWindow,
    pub initials: gtk::Label,
    pub shot: gtk::Box,
    pub name: gtk::Label,
    pub check: gtk::Image,
    pub installed: gtk::Label,
    pub sub: gtk::Label,
    pub desc: gtk::Label,
    pub cat: gtk::Label,
    pub stars: gtk::Label,
    pub action: gtk::Button,
    pub trust: gtk::Label,
    pub prov: gtk::Label,
    pub prov_row: gtk::Box,
}

/// `item._parts`, `item._handler`, `item._token`
pub struct CardSlot {
    pub parts: Card,
    pub handler: Option<glib::SignalHandlerId>,
    pub token: u64,
}

/// The store's result set: positions in the catalogue it was drawn from,
/// which it holds on to, so a refresh copies no entry (clearing the search
/// lists all 5,900) and only the page being shown is cloned into the model.
#[derive(Default)]
pub struct Results {
    plugins: Arc<Vec<Plugin>>,
    idx: Vec<usize>,
}

impl Results {
    pub fn len(&self) -> usize {
        self.idx.len()
    }

    pub fn is_empty(&self) -> bool {
        self.idx.is_empty()
    }

    /// Let go of the list, if it is `plugins` (so it can be changed in
    /// place); `put_plugins` gives it back. Whether it was let go.
    pub fn take_plugins_if(&mut self, plugins: &Arc<Vec<Plugin>>) -> bool {
        if !Arc::ptr_eq(&self.plugins, plugins) {
            return false;
        }
        self.plugins = Arc::default();
        true
    }

    pub fn put_plugins(&mut self, plugins: Arc<Vec<Plugin>>) {
        self.plugins = plugins;
    }

    /// Results `from..to` (clamped), as copies for the model.
    fn page(&self, from: usize, to: usize) -> Vec<Plugin> {
        let to = to.min(self.idx.len());
        self.idx.get(from..to).unwrap_or_default().iter().map(|&i| self.plugins[i].clone()).collect()
    }
}

/// State used only by the store section.
#[derive(Default)]
pub struct State {
    // ---- filter drawer ---------------------------------------------------
    /// "Search repos and the AUR instead".
    pub store_pkgs: Option<gtk::Button>,

    // ---- grid and its empty states --------------------------------------
    pub store_scroll_inner: Option<gtk::ScrolledWindow>,
    pub store_scroller: Option<gtk::ScrolledWindow>,
    /// The grid and its empty state occupy the same slot.
    pub store_stack: Option<gtk::Stack>,
    pub store_empty: Option<adw::StatusPage>,
    pub store_offline: Option<adw::StatusPage>,

    // ---- data ------------------------------------------------------------
    /// The full, filtered and sorted result set; the model holds a prefix of
    /// it, a page at a time.
    pub store_results: Results,
    /// Trust keys parallel to the provenance dropdown ("" = any).
    pub store_trusts: Vec<String>,

    // ---- cards -----------------------------------------------------------
    /// `item._parts` & co. Python hangs them on the ListItem itself; here
    /// they live in this side table, keyed by the ListItem.
    pub cards: HashMap<gtk::ListItem, CardSlot>,
}

/// `page or getattr(self, "_page", "discover")`
fn pool_page(page: Option<&str>, current: &str) -> String {
    match page {
        Some(p) if !p.is_empty() => p.to_string(),
        _ => current.to_string(),
    }
}

/// `_store_pool` without the window: what a store page draws from.
fn pool_of(plugins: &[Plugin], page: &str) -> Vec<Plugin> {
    pool_refs(plugins, page).into_iter().cloned().collect()
}

/// `pool_of`, borrowed: what the search and the count read on every
/// keystroke, without copying the catalogue to read it.
fn pool_refs<'a>(plugins: &'a [Plugin], page: &str) -> Vec<&'a Plugin> {
    let themes = page == "themes";
    plugins.iter().filter(|p| (p.install_kind == "theme") == themes).collect()
}

/// `f"{n:,}"`: thousands separated by commas.
fn thousands(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

/// `p.initials or p.name[:2].upper()`
fn initials_of(p: &Plugin) -> String {
    if !p.initials.is_empty() {
        return p.initials.clone();
    }
    p.name.chars().take(2).collect::<String>().to_uppercase()
}

/// `f"acc-{p.accent}" if p.accent in store_mod.ACCENTS else "acc-none"`
fn accent_class(p: &Plugin) -> String {
    if store_mod::accent(&p.accent).is_some() {
        format!("acc-{}", p.accent)
    } else {
        "acc-none".to_string()
    }
}

/// `keys[i] if 0 <= i < len(keys) else ""`
fn pick_key(keys: &[String], i: u32) -> String {
    keys.get(i as usize).cloned().unwrap_or_default()
}

/// `Gdk.MemoryTexture.new(w, h, R8G8B8A8 if alpha else R8G8B8,
/// GLib.Bytes.new(pb.get_pixels()), pb.get_rowstride())`
/// `Win::texture`'s decode, on any thread: a texture is immutable and `Send`,
/// so the decode can run on a worker and only the result cross over.
pub fn decode_texture(path: &Path, max_width: i32) -> Option<gdk::Texture> {
    if max_width != 0 {
        let pb = gdk_pixbuf::Pixbuf::from_file_at_scale(path, max_width, -1, true).ok()?;
        memory_texture(&pb, pb.width(), pb.height())
    } else {
        gdk::Texture::from_filename(path).ok()
    }
}

fn memory_texture(pb: &gdk_pixbuf::Pixbuf, width: i32, height: i32) -> Option<gdk::Texture> {
    let format = if pb.has_alpha() {
        gdk::MemoryFormat::R8g8b8a8
    } else {
        gdk::MemoryFormat::R8g8b8
    };
    let stride = usize::try_from(pb.rowstride()).ok()?;
    Some(gdk::MemoryTexture::new(width, height, format, &pb.read_pixel_bytes(), stride).upcast())
}

/// What Python prints when a signal handler reads a widget that was never
/// built: the handler stops there.
fn missing(attr: &str) {
    eprintln!("AttributeError: 'AlmanacWindow' object has no attribute '{attr}'");
}

impl Win {
    pub const STORE_PAGE: u32 = 100;

    // Scaled for memory, not for layout: 300 cached cards at source
    // resolution would be well over a gigabyte of textures.
    pub const CARD_ART_WIDTH: i32 = 420;

    /// `_square`'s default `side`.
    pub const SQUARE_SIDE: i32 = 72;

    /// `self.<name>` for one of the filter drawer's dropdowns, read at the
    /// point the Python touches it. None (after printing Python's
    /// AttributeError) when the drawer was never built.
    fn store_dropdown(&self, name: &str) -> Option<gtk::DropDown> {
        let widget = {
            let sh = self.shared.borrow();
            match name {
                "store_cat" => sh.store_cat.clone(),
                "store_sort" => sh.store_sort.clone(),
                "store_trust" => sh.store_trust.clone(),
                "store_market" => sh.store_market.clone(),
                _ => None,
            }
        };
        if widget.is_none() {
            missing(name);
        }
        widget
    }

    /// `self.<name>` for one of the filter drawer's toggles; as above.
    fn store_toggle(&self, name: &str) -> Option<gtk::ToggleButton> {
        let widget = {
            let sh = self.shared.borrow();
            match name {
                "store_verified" => sh.store_verified.clone(),
                "store_installed" => sh.store_installed.clone(),
                _ => None,
            }
        };
        if widget.is_none() {
            missing(name);
        }
        widget
    }

    /// `getattr(self, "_page", "discover")`.
    fn store_page(&self) -> String {
        self.shared.borrow().page_or("discover")
    }

    /// `getattr(self, "_plugins", None) or []`
    fn store_plugins(&self) -> Ref<'_, [Plugin]> {
        Ref::map(self.shared.borrow(), |sh| if sh.plugins_none { &[][..] } else { &sh.plugins[..] })
    }

    // ---- the plugin marketplace -------------------------------------------

    /// The App Store page: a card grid over plugins.omarchy.org's catalog.
    ///
    /// A ColumnView was the wrong shape for this. The marketplace's own front
    /// end shows artwork, a description and a star count per entry, and that
    /// is what tells two of 5,600 plugins apart -- none of which survives
    /// being squeezed into table cells.
    pub fn build_store(&self) -> gtk::Box {
        let page = gtk::Box::new(gtk::Orientation::Vertical, 0);

        // Filters live in a drawer behind one header button, not in a bar of
        // eight controls above the results. Every page then has the same
        // chrome in the same places, and the store gets its vertical space
        // back for what it is actually for.
        let bar = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .margin_start(12)
            .margin_end(12)
            .margin_top(8)
            .margin_bottom(12)
            .build();

        let store_cat = gtk::DropDown::from_strings(&["All categories"]);
        store_cat.set_tooltip_text(Some(&tr("Category")));
        let w = self.downgrade();
        store_cat.connect_selected_notify(move |_| {
            if let Some(w) = w.upgrade() {
                w.store_refresh();
            }
        });
        bar.append(&Win::labelled("Category", &store_cat));

        let sort_labels: Vec<&str> = store_mod::SORTS.iter().map(|(_k, label, _f)| *label).collect();
        let store_sort = gtk::DropDown::from_strings(&sort_labels);
        store_sort.set_tooltip_text(Some(&tr("Sort by")));
        let w = self.downgrade();
        store_sort.connect_selected_notify(move |_| {
            if let Some(w) = w.upgrade() {
                w.store_refresh();
            }
        });
        bar.append(&Win::labelled("Sort by", &store_sort));

        let store_trust = gtk::DropDown::from_strings(&["Any provenance"]);
        store_trust.set_tooltip_text(Some(&tr("Filter by what is known about the source")));
        let w = self.downgrade();
        store_trust.connect_selected_notify(move |_| {
            if let Some(w) = w.upgrade() {
                w.store_refresh();
            }
        });
        bar.append(&Win::labelled("Provenance", &store_trust));

        // One marketplace today, so the control would be a dropdown with a
        // single entry. It appears on its own once a second one is registered.
        let store_market = gtk::DropDown::from_strings(&["All marketplaces"]);
        store_market.set_tooltip_text(Some(&tr("Marketplace")));
        // Where `enabled_markets()` raises the Python's window is never
        // built; here it is logged and the control stays hidden.
        store_market.set_visible(match store_mod::try_enabled_markets() {
            Ok(markets) => markets.len() > 1,
            Err(e) => {
                eprintln!("almanac: {e}");
                false
            }
        });
        let w = self.downgrade();
        store_market.connect_selected_notify(move |_| {
            if let Some(w) = w.upgrade() {
                w.retitle_store();
                w.store_refresh();
            }
        });
        bar.append(&Win::labelled("Marketplace", &store_market));

        let toggles = gtk::Box::builder().spacing(8).homogeneous(true).build();
        let store_verified = gtk::ToggleButton::with_label(&tr("Verified"));
        store_verified.set_tooltip_text(Some(&tr(
            "Only entries with a review that still applies to the current upstream commit",
        )));
        let w = self.downgrade();
        store_verified.connect_toggled(move |_| {
            if let Some(w) = w.upgrade() {
                w.store_refresh();
            }
        });
        toggles.append(&store_verified);

        let store_installed = gtk::ToggleButton::with_label(&tr("Installed"));
        store_installed.set_tooltip_text(Some(&tr("Only what is already on this system")));
        let w = self.downgrade();
        store_installed.connect_toggled(move |_| {
            if let Some(w) = w.upgrade() {
                w.store_refresh();
            }
        });
        toggles.append(&store_installed);
        bar.append(&toggles);

        // Packages are not plugins, and mixing 15,000 of them into the
        // marketplace would bury it. The escape hatch stays explicit.
        let reset = gtk::Button::with_label(&tr("Reset filters"));
        reset.add_css_class("flat");
        reset.set_tooltip_text(Some(&tr("Clear every filter and go back to Most popular")));
        let w = self.downgrade();
        reset.connect_clicked(move |_| {
            if let Some(w) = w.upgrade() {
                w.store_reset_filters();
            }
        });

        let store_pkgs = gtk::Button::with_label(&tr("Search repos and the AUR instead"));
        store_pkgs.add_css_class("flat");
        store_pkgs.set_tooltip_text(Some(&tr(
            "Packages are not plugins; 15,000 of them would bury the marketplace, so they are \
             searched separately",
        )));
        let w = self.downgrade();
        store_pkgs.connect_clicked(move |_| {
            if let Some(w) = w.upgrade() {
                w.store_packages();
            }
        });

        bar.append(&reset);
        bar.append(&store_pkgs);

        let store_drawer = gtk::Popover::new();
        store_drawer.set_child(Some(&bar));
        self.filter_btn.set_popover(Some(&store_drawer));

        {
            let mut sh = self.shared.borrow_mut();
            sh.store_cat = Some(store_cat);
            sh.store_sort = Some(store_sort);
            sh.store_trust = Some(store_trust);
            sh.store_market = Some(store_market);
            sh.store_verified = Some(store_verified);
            sh.store_installed = Some(store_installed);
            sh.store_drawer = Some(store_drawer);
        }
        self.store.borrow_mut().store_pkgs = Some(store_pkgs);

        // `Gio.ListStore(item_type=PluginItem)`: PluginItem is not a GObject
        // here, so the rows are boxed PluginItems.
        let store_model = gio::ListStore::new::<glib::BoxedAnyObject>();
        self.shared.borrow_mut().store_model = store_model.clone();
        let sel = gtk::SingleSelection::new(Some(store_model));
        sel.set_can_unselect(true);
        sel.set_autoselect(false);

        let factory = gtk::SignalListItemFactory::new();
        let w = self.downgrade();
        factory.connect_setup(move |_f, obj| {
            let (Some(w), Some(item)) = (w.upgrade(), obj.downcast_ref::<gtk::ListItem>()) else {
                return;
            };
            w.card_setup(item);
        });
        let w = self.downgrade();
        factory.connect_bind(move |_f, obj| {
            let (Some(w), Some(item)) = (w.upgrade(), obj.downcast_ref::<gtk::ListItem>()) else {
                return;
            };
            w.card_bind(item);
        });
        let w = self.downgrade();
        factory.connect_unbind(move |_f, obj| {
            let (Some(w), Some(item)) = (w.upgrade(), obj.downcast_ref::<gtk::ListItem>()) else {
                return;
            };
            w.card_unbind(item);
        });
        // Not in the Python: there `item._parts` dies with the ListItem. The
        // side table holds the ListItem, so it has to let go on teardown or
        // no card would ever be freed.
        let w = self.downgrade();
        factory.connect_teardown(move |_f, obj| {
            let (Some(w), Some(item)) = (w.upgrade(), obj.downcast_ref::<gtk::ListItem>()) else {
                return;
            };
            w.card_teardown(item);
        });

        let store_grid = gtk::GridView::new(Some(sel), Some(factory));
        store_grid.set_max_columns(4);
        store_grid.set_min_columns(1);
        store_grid.set_single_click_activate(true);
        let w = self.downgrade();
        store_grid.connect_activate(move |_grid, position| {
            if let Some(w) = w.upgrade() {
                w.on_card_activated(position);
            }
        });
        store_grid.add_css_class("navigation-sidebar");
        self.shared.borrow_mut().store_grid = Some(store_grid.clone());

        let scroller = gtk::ScrolledWindow::builder().vexpand(true).build();
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroller.set_child(Some(&store_grid));
        scroller.set_margin_start(12);
        scroller.set_margin_end(12);
        scroller.set_margin_top(crate::ui::shell::PAGE_MARGIN);
        scroller.set_margin_bottom(12);
        // Cards arrive a page at a time as you reach the end of the list.
        // GridView recycles the widgets, but building 5,600 PluginItems on
        // every keystroke is what makes the search feel heavy.
        let w = self.downgrade();
        scroller.vadjustment().connect_value_changed(move |adj| {
            if let Some(w) = w.upgrade() {
                w.store_scrolled(adj);
            }
        });
        {
            let mut st = self.store.borrow_mut();
            st.store_scroll_inner = Some(scroller.clone());
            st.store_scroller = Some(scroller.clone());
        }

        // The grid and its empty state occupy the same slot.
        let store_stack = gtk::Stack::new();
        store_stack.set_vexpand(true);
        store_stack.add_named(&scroller, Some("grid"));
        let retry = gtk::Button::builder()
            .label(tr("Try again"))
            .halign(gtk::Align::Center)
            .build();
        retry.add_css_class("suggested-action");
        retry.add_css_class("pill");
        let w = self.downgrade();
        retry.connect_clicked(move |_| {
            if let Some(w) = w.upgrade() {
                w.store_reload(true);
            }
        });
        let store_empty = Win::status_page(
            "system-search-symbolic",
            &tr("Nothing matches"),
            &tr("Try a different search, or clear the filters."),
            None,
        );
        let store_offline = Win::status_page(
            "network-offline-symbolic",
            &tr("The catalogue could not be loaded"),
            &tr("Almanac could not reach the marketplaces. It will use the last copy it has as \
                 soon as one exists."),
            Some(retry.upcast_ref()),
        );
        store_stack.add_named(&store_empty, Some("empty"));
        store_stack.add_named(&store_offline, Some("offline"));
        page.append(&store_stack);
        {
            let mut st = self.store.borrow_mut();
            st.store_stack = Some(store_stack);
            st.store_empty = Some(store_empty);
            st.store_offline = Some(store_offline);
        }
        page
    }

    pub fn store_scrolled(&self, adj: &gtk::Adjustment) {
        // Within two viewports of the end: start the next page before the user
        // arrives at the gap, so scrolling never stops on an empty row.
        if adj.value() + adj.page_size() * 3.0 >= adj.upper() {
            self.store_extend();
        }
    }

    /// Append the next page of results, if there are any left.
    pub fn store_extend(&self) -> bool {
        let model = self.shared.borrow().store_model.clone();
        let have = model.n_items() as usize;
        let next: Vec<Plugin> = {
            let st = self.store.borrow();
            let results = &st.store_results;
            if have >= results.len() {
                return false;
            }
            results.page(have, have + Win::STORE_PAGE as usize)
        };
        for p in next {
            model.append(&PluginItem::new(p).boxed());
        }
        self.store_count();
        // A short result set may not fill the viewport, in which case no
        // scroll event will ever arrive to ask for the rest.
        let w = self.clone();
        glib::idle_add_local(move || w.store_fill());
        true
    }

    pub fn store_fill(&self) -> glib::ControlFlow {
        let scroller = self.store.borrow().store_scroller.clone();
        let Some(scroller) = scroller else {
            missing("store_scroller");
            return glib::ControlFlow::Break;
        };
        let adj = scroller.vadjustment();
        if adj.upper() <= adj.page_size() + 1.0 {
            self.store_extend();
        }
        glib::ControlFlow::Break
    }

    /// Category, provenance and marketplace lists for the page showing.
    ///
    /// Built from what the page draws from, so the App Store's counts do not
    /// include the 345 themes it no longer lists, and rebuilt on switching
    /// between it and Themes.
    pub fn store_filters(&self) {
        let pool = self.store_pool(None);
        let themes = self.store_page() == "themes";
        {
            let mut sh = self.shared.borrow_mut();
            sh.filters_for = Some(themes);
            sh.pinning = true;
        }
        let cats = store_mod::categories(&pool);
        let cat_keys: Vec<String> =
            std::iter::once(String::new()).chain(cats.iter().map(|(c, _n)| c.clone())).collect();
        // Setting a model resets the selection; keep the chosen category.
        let want = {
            let mut sh = self.shared.borrow_mut();
            sh.store_cats = cat_keys.clone();
            sh.store_cat_key.clone()
        };
        let Some(store_cat) = self.store_dropdown("store_cat") else { return };
        let labels: Vec<String> = std::iter::once("All categories".to_string())
            .chain(cats.iter().map(|(c, n)| format!("{c} ({n})")))
            .collect();
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        store_cat.set_model(Some(&gtk::StringList::new(&labels)));
        if let Some(i) = cat_keys.iter().position(|k| *k == want) {
            store_cat.set_selected(i as u32);
        }

        let trusts = match store_mod::try_trust_counts(&pool) {
            Ok(trusts) => trusts,
            Err(e) => {
                eprintln!("almanac: {e}");
                return;
            }
        };
        self.store.borrow_mut().store_trusts =
            std::iter::once(String::new()).chain(trusts.iter().map(|(k, _l, _n)| k.clone())).collect();
        let labels: Vec<String> = std::iter::once("Any provenance".to_string())
            .chain(trusts.iter().map(|(_k, lbl, n)| format!("{lbl} ({n})")))
            .collect();
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        let Some(store_trust) = self.store_dropdown("store_trust") else { return };
        store_trust.set_model(Some(&gtk::StringList::new(&labels)));

        let non_themes: Vec<Plugin> = self
            .store_plugins()
            .iter()
            .filter(|p| p.install_kind != "theme")
            .cloned()
            .collect();
        let markets = store_mod::market_counts(&non_themes);
        self.shared.borrow_mut().store_markets = std::iter::once(String::new())
            .chain(markets.iter().map(|(mid, _nm, _n)| mid.clone()))
            .collect();
        let labels: Vec<String> = std::iter::once("All marketplaces".to_string())
            .chain(markets.iter().map(|(_mid, nm, n)| format!("{nm} ({n})")))
            .collect();
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        let Some(store_market) = self.store_dropdown("store_market") else { return };
        store_market.set_model(Some(&gtk::StringList::new(&labels)));
        self.shared.borrow_mut().pinning = false;
    }

    /// What a store page draws from. Themes have their own page, so the
    /// App Store leaves them out rather than listing them twice.
    pub fn store_pool(&self, page: Option<&str>) -> Vec<Plugin> {
        let page = pool_page(page, &self.store_page());
        pool_of(&self.store_plugins(), &page)
    }

    /// The store's standing summary, worded for whichever page is showing.
    ///
    /// Themes pin the marketplace filter, so "a filter is set" is always true
    /// there and cannot be what decides the wording -- the page has to.
    pub fn store_count(&self) {
        let page = self.store_page();
        let model = self.shared.borrow().store_model.clone();
        let shown = model.n_items() as usize;
        let results = self.store.borrow().store_results.len();
        let noun = if page == "themes" { "themes" } else { "plugins" };

        // A pinned marketplace is the page, not a filter the user applied.
        let pinned = page_spec(&page).and_then(|s| s.market).is_some_and(|m| !m.is_empty());
        let cat_key = self.shared.borrow().store_cat_key.clone();
        // `or` stops at the first truthy operand, so a widget is only read
        // (and only missed) when the ones before it said "not narrowed".
        let narrowed = 'narrowed: {
            if !self.search.text().trim().is_empty() || !cat_key.is_empty() {
                break 'narrowed true;
            }
            let Some(verified) = self.store_toggle("store_verified") else { return };
            if verified.is_active() {
                break 'narrowed true;
            }
            let Some(installed) = self.store_toggle("store_installed") else { return };
            if installed.is_active() {
                break 'narrowed true;
            }
            let Some(trust) = self.store_dropdown("store_trust") else { return };
            if trust.selected() > 0 {
                break 'narrowed true;
            }
            let Some(market) = self.store_dropdown("store_market") else { return };
            market.selected() > 0 && !pinned
        };

        let pool_page = pool_page(Some(&page), &self.store_page());
        let (total, installed) = {
            let plugins = self.store_plugins();
            let pool = pool_refs(&plugins, &pool_page);
            (pool.len(), pool.iter().filter(|p| p.installed).count())
        };

        if shown < results {
            self.summarize(&format!("{shown} of {results} shown"), Some(&page));
        } else if narrowed {
            self.summarize(&format!("{results} of {total} {noun}"), Some(&page));
        } else if page == "themes" {
            self.summarize(&format!("{total} Omarchy themes · {installed} installed"), Some(&page));
        } else {
            let markets = match store_mod::try_enabled_markets() {
                Ok(markets) => markets,
                Err(e) => {
                    eprintln!("almanac: {e}");
                    return;
                }
            };
            let markets = markets
                .iter()
                .map(|m| m.name.clone())
                .collect::<Vec<_>>()
                .join(", ");
            self.summarize(
                &format!("{total} listings from {markets} · {installed} installed here"),
                Some(&page),
            );
        }
    }

    // ---- card widgets ----------------------------------------------------

    pub fn card_setup(&self, item: &gtk::ListItem) {
        let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
        card.add_css_class("store-card");
        card.set_size_request(200, -1); // three across at a laptop's width
        card.set_overflow(gtk::Overflow::Hidden);

        // Artwork: a screenshot when the entry has one, the accent tile when
        // it does not. Both occupy the same box so the grid stays aligned.
        let shot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        shot.add_css_class("store-shot");
        shot.set_size_request(-1, 112);
        shot.set_overflow(gtk::Overflow::Hidden);
        let pic = gtk::Picture::new();
        pic.set_content_fit(gtk::ContentFit::Cover);
        pic.set_hexpand(true);
        pic.set_vexpand(true);
        // The art must not get a vote on how wide the card is. A GtkPicture
        // reports its paintable's size as its natural width, and GridView
        // chooses columns from natural width, so a 16:9 screenshot in a card
        // meant it laid out one column however narrow the card was told to be.
        // A non-propagating ScrolledWindow is the standard way to cut that.
        let art_clamp = gtk::ScrolledWindow::builder().hexpand(true).vexpand(true).build();
        art_clamp.set_policy(gtk::PolicyType::External, gtk::PolicyType::External);
        art_clamp.set_propagate_natural_width(false);
        art_clamp.set_propagate_natural_height(false);
        art_clamp.set_child(Some(&pic));
        let initials = gtk::Label::new(None);
        initials.add_css_class("store-initials");
        initials.set_hexpand(true);
        initials.set_vexpand(true);
        shot.append(&art_clamp);
        shot.append(&initials);
        card.append(&shot);

        let body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .margin_start(12)
            .margin_end(12)
            .margin_top(8)
            .margin_bottom(8)
            .build();

        let top = gtk::Box::builder().spacing(4).build();
        let name = gtk::Label::builder().xalign(0.0).ellipsize(pango::EllipsizeMode::End).build();
        name.set_max_width_chars(22);
        name.add_css_class("store-name");
        name.set_hexpand(true);
        let check = gtk::Image::builder().icon_name("object-select-symbolic").pixel_size(12).build();
        check.add_css_class("store-verified");
        check.set_tooltip_text(Some(&tr("Source reviewed by the marketplace")));
        let spacer = gtk::Box::builder().hexpand(true).build();
        let installed = gtk::Label::builder()
            .label(tr("Installed"))
            .ellipsize(pango::EllipsizeMode::End)
            .build();
        installed.set_max_width_chars(10);
        installed.add_css_class("pill-ok");
        top.append(&name);
        top.append(&check);
        top.append(&spacer);
        top.append(&installed);
        body.append(&top);

        let sub = gtk::Label::builder().xalign(0.0).ellipsize(pango::EllipsizeMode::End).build();
        sub.set_max_width_chars(24);
        sub.add_css_class("caption");
        sub.add_css_class("dim-label");
        body.append(&sub);

        // Provenance, on every card: the repository the code actually comes
        // from. The author line above it is self-declared in the plugin's own
        // manifest and proves nothing by itself.
        let prov = gtk::Box::builder().spacing(4).build();
        let prov_icon =
            gtk::Image::builder().icon_name("folder-remote-symbolic").pixel_size(11).build();
        prov_icon.add_css_class("dim-label");
        let prov_text =
            gtk::Label::builder().xalign(0.0).ellipsize(pango::EllipsizeMode::Middle).build();
        prov_text.set_max_width_chars(26);
        prov_text.add_css_class("caption");
        prov_text.add_css_class("dim-label");
        prov_text.add_css_class("store-prov");
        prov.append(&prov_icon);
        prov.append(&prov_text);
        body.append(&prov);

        let desc = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .lines(2)
            .ellipsize(pango::EllipsizeMode::End)
            .build();
        desc.set_max_width_chars(30);
        desc.set_width_chars(20);
        desc.set_wrap_mode(pango::WrapMode::WordChar);
        desc.add_css_class("caption");
        desc.add_css_class("dim-label");
        desc.set_valign(gtk::Align::Start);
        desc.set_vexpand(true);
        body.append(&desc);

        let bottom = gtk::Box::builder().spacing(6).margin_top(4).build();
        let cat = gtk::Label::builder().ellipsize(pango::EllipsizeMode::End).build();
        cat.set_max_width_chars(11);
        cat.add_css_class("pill");
        let trust = gtk::Label::builder().ellipsize(pango::EllipsizeMode::End).build();
        trust.set_max_width_chars(18);
        let stars = gtk::Label::builder().ellipsize(pango::EllipsizeMode::End).build();
        stars.set_max_width_chars(8);
        stars.add_css_class("store-stars");
        stars.add_css_class("dim-label");
        // Provenance and stars only: a card is 200px wide, and the category
        // is already a filter and in the card's tooltip. Three chips there
        // all truncated ("Gra…", "Fir…", "★ …").
        bottom.append(&trust);
        bottom.append(&stars);
        bottom.append(&gtk::Box::builder().hexpand(true).build());
        let action = gtk::Button::new();
        action.set_can_shrink(true);
        bottom.append(&action);
        body.append(&bottom);

        card.append(&body);
        item.set_child(Some(&card));
        // Stash the leaves: bind runs on every scroll step and walking the
        // widget tree each time would be the expensive part.
        let parts = Card {
            pic,
            art: art_clamp,
            initials,
            shot,
            name,
            check,
            installed,
            sub,
            desc,
            cat,
            stars,
            action,
            trust,
            prov: prov_text,
            prov_row: prov,
        };
        self.store
            .borrow_mut()
            .cards
            .insert(item.clone(), CardSlot { parts, handler: None, token: 0 });
    }

    pub fn card_bind(&self, item: &gtk::ListItem) {
        let Some(row) = item.item() else { return };
        let Some(p) = PluginItem::of(&row).map(|it| it.plugin.clone()) else { return };
        let (w, token) = {
            let mut st = self.store.borrow_mut();
            let Some(slot) = st.cards.get_mut(item) else { return };
            slot.token += 1;
            (slot.parts.clone(), slot.token)
        };

        w.name.set_text(&p.name);
        w.check.set_visible(p.verified);
        w.installed.set_visible(p.installed);
        // Installed and switched off reads "Off": answers "is it running?"
        // without putting a control on a card meant for browsing.
        let off = p.installed && p.install_kind == "plugin" && !p.enabled;
        w.installed.set_text(&if off { tr("Off") } else { tr("Installed") });
        w.installed.remove_css_class(if !off { "pill-quiet" } else { "pill-ok" });
        w.installed.add_css_class(if off { "pill-quiet" } else { "pill-ok" });
        // What a listing property raises ends this bind, logged.
        let subtitle = match p.try_subtitle() {
            Ok(subtitle) => subtitle,
            Err(e) => {
                eprintln!("almanac: {e}");
                return;
            }
        };
        w.sub.set_text(&subtitle);
        w.desc.set_text(if !p.description.is_empty() { &p.description } else { &p.install_note });
        w.cat.set_text(&p.category);
        w.stars.set_text(&if p.stars != 0 { format!("★ {}", compact(p.stars)) } else { String::new() });
        w.stars.set_tooltip_text(Some(&if p.stars != 0 {
            format!("{} stars", thousands(p.stars))
        } else {
            String::new()
        }));
        w.name.set_tooltip_text(Some(&format!("{} · {}", p.name, p.category)));

        // Provenance
        let props = p.try_trust().and_then(|t| Ok((t, p.try_publisher()?, p.try_market_name_text()?)));
        let (t, publisher, market_name) = match props {
            Ok(props) => props,
            Err(e) => {
                eprintln!("almanac: {e}");
                return;
            }
        };
        w.prov.set_text(&publisher);
        w.prov_row.set_tooltip_text(Some(&if !p.repo.is_empty() {
            format!("{}\nListed by {}", publisher, market_name)
        } else {
            format!("Listed by {}", market_name)
        }));
        w.trust.set_text(&t.label);
        for css in ["pill-accent", "pill-ok", "pill-warn", "pill-quiet"] {
            w.trust.remove_css_class(css);
        }
        w.trust.add_css_class(&t.css);
        w.trust.set_tooltip_text(Some(&t.why));
        // The tick duplicates the chip; keep it only where it adds the
        // marketplace's own verified flag on top of a human review.
        w.check.set_visible(p.verified && t.key == "reviewed");

        // Artwork
        for (name, _colors) in store_mod::ACCENTS {
            w.shot.remove_css_class(&format!("acc-{name}"));
        }
        w.shot.remove_css_class("acc-none");
        let cached: Option<PathBuf> = store_mod::thumb_path(&p, false);
        match cached {
            Some(ref c)
                if c.exists() && std::fs::metadata(c).map(|m| m.len() > 0).unwrap_or(false) =>
            {
                self.card_art(item, token, c);
            }
            Some(_) => {
                w.art.set_visible(false);
                w.initials.set_visible(true);
                w.initials.set_text(&initials_of(&p));
                w.shot.add_css_class(&accent_class(&p));
                self.thumb_async(item, token, &p);
            }
            None => {
                w.art.set_visible(false);
                w.initials.set_visible(true);
                w.initials.set_text(&initials_of(&p));
                w.shot.add_css_class(&accent_class(&p));
            }
        }

        // Primary action
        let btn = &w.action;
        let old_handler = self.store.borrow_mut().cards.get_mut(item).and_then(|s| s.handler.take());
        if let Some(id) = old_handler {
            btn.disconnect(id);
        }
        // `.flat` strips the accent background but not the accent foreground,
        // so combining it with `.suggested-action` paints accent_fg_color --
        // near-black under most dark themes -- straight onto the card. That
        // measured 1.41:1 against the card. Filled, the same pair is 8.65:1.
        for css in ["suggested-action", "destructive-action", "flat"] {
            btn.remove_css_class(css);
        }
        // Cards are recycled: a name set for the Install glyph must not stay
        // on the same button once it reads "Remove". Text buttons are named
        // by their label.
        btn.reset_property(gtk::AccessibleProperty::Label);
        let ws = self.downgrade();
        let pl = p.clone();
        let handler = if p.installed && p.install_kind == "theme" {
            // An installed theme's useful action is wearing it; removing one
            // is rare enough to belong in the detail sheet, where Remove sits
            // for every other kind too.
            btn.set_label(&tr("Apply"));
            btn.set_sensitive(true);
            btn.add_css_class("suggested-action"); // filled: the main action
            btn.set_tooltip_text(Some(&format!("Switch the whole desktop to {}", p.name)));
            btn.connect_clicked(move |_b| {
                if let Some(w) = ws.upgrade() {
                    w.apply_theme_now(&pl);
                }
            })
        } else if p.installed {
            btn.set_label(&tr("Remove"));
            btn.set_sensitive(!p.built_in);
            // Low emphasis: removal should never be the loudest thing on a card.
            btn.add_css_class("flat");
            btn.set_tooltip_text(Some(&if p.built_in {
                tr("Ships with Omarchy — cannot be removed")
            } else {
                format!("Remove {}", p.name)
            }));
            btn.connect_clicked(move |_b| {
                if let Some(w) = ws.upgrade() {
                    w.on_plugin_remove(&pl);
                }
            })
        } else if p.can_install() {
            // A glyph rather than a word: cards are narrow, and the word is
            // still there for the pointer (tooltip) and screen readers.
            btn.set_icon_name(INSTALL_ICON);
            btn.set_sensitive(true);
            btn.add_css_class("suggested-action"); // filled: the main action
            let setup = if p.needs_setup { tr(" — setup follows") } else { String::new() };
            let how = if !p.install_cmd.is_empty() {
                format!("\n{}", p.install_cmd)
            } else if !p.asset_name.is_empty() {
                format!("\n{}", p.asset_name)
            } else {
                String::new()
            };
            btn.set_tooltip_text(Some(&format!("Install {}{}{}", p.name, setup, how)));
            btn.update_property(&[gtk::accessible::Property::Label(&format!("Install {}", p.name))]);
            btn.connect_clicked(move |_b| {
                if let Some(w) = ws.upgrade() {
                    w.on_plugin_install(&pl);
                }
            })
        } else {
            // 1,062 entries are listed but not installable through the CLI --
            // suites with their own installers, mostly. Saying so beats an
            // Install button that cannot work.
            // These are listed but have no one-command install -- suites with
            // their own installers, mostly. "Manual setup" is what the
            // marketplace calls it, and it beats an Install that cannot work.
            btn.set_label(&tr("Manual setup"));
            btn.set_sensitive(true);
            btn.add_css_class("flat"); // not an install: quieter
            btn.set_tooltip_text(Some(if !p.install_note.is_empty() {
                &p.install_note
            } else {
                "Not installable from here"
            }));
            btn.connect_clicked(move |_b| {
                if let Some(w) = ws.upgrade() {
                    w.show_plugin(&pl);
                }
            })
        };
        if let Some(slot) = self.store.borrow_mut().cards.get_mut(item) {
            slot.handler = Some(handler);
        }
    }

    pub fn card_unbind(&self, item: &gtk::ListItem) {
        let (action, handler) = {
            let mut st = self.store.borrow_mut();
            let Some(slot) = st.cards.get_mut(item) else { return };
            slot.token += 1; // cancels any in-flight image load
            (slot.parts.action.clone(), slot.handler.take())
        };
        if let Some(id) = handler {
            action.disconnect(id);
        }
    }

    /// Drop a torn-down card from the side table. If something up the stack
    /// holds the table borrowed while GTK disposes list items, finish as soon
    /// as it lets go instead of panicking.
    fn card_teardown(&self, item: &gtk::ListItem) {
        if let Ok(mut st) = self.store.try_borrow_mut() {
            st.cards.remove(item);
            return;
        }
        let w = self.downgrade();
        let item = item.clone();
        glib::idle_add_local_once(move || {
            if let Some(w) = w.upgrade() {
                if let Ok(mut st) = w.store.try_borrow_mut() {
                    st.cards.remove(&item);
                }
            }
        });
    }

    /// Decode once, and not at the source resolution.
    ///
    /// GtkPicture reports its paintable's intrinsic size as its natural size,
    /// and GridView picks its column count from natural width. The marketplace
    /// ships 1600px-wide screenshots, so every card asked for 1600px and
    /// exactly one column fitted however narrow the cards were told to be.
    /// Scaling at decode time bounds the card and keeps 5,600 of them out of
    /// memory at full size.
    ///
    /// `max_width` 0 (the Python's default) decodes at the source size.
    pub fn texture(&self, path: &Path, max_width: i32) -> Option<gdk::Texture> {
        if let Some(tex) = self.texture_cached(path, max_width) {
            return Some(tex);
        }
        let tex = decode_texture(path, max_width)?;
        self.texture_keep(path, max_width, &tex);
        Some(tex)
    }

    /// What `texture` decoded before for this file and width, if anything.
    pub fn texture_cached(&self, path: &Path, max_width: i32) -> Option<gdk::Texture> {
        self.shared.borrow_mut().tex_cache.get(&format!("{}@{}", path.display(), max_width))
    }

    /// Keep a texture decoded elsewhere (see `decode_texture`) for `texture`.
    pub fn texture_keep(&self, path: &Path, max_width: i32, tex: &gdk::Texture) {
        let bytes = tex_bytes(tex);
        let key = format!("{}@{}", path.display(), max_width);
        self.shared.borrow_mut().tex_cache.insert(key, tex.clone(), bytes);
    }

    /// A centred square crop, scaled -- so a 1600px-wide screenshot makes
    /// a tile exactly as big as an icon, instead of the width it asks for.
    ///
    /// The Python's default `side` is `Win::SQUARE_SIDE` (72).
    pub fn square(&self, path: &Path, side: i32) -> Option<gdk::Texture> {
        let key = format!("{}@square{}", path.display(), side);
        let hit = self.shared.borrow_mut().tex_cache.get(&key);
        if let Some(tex) = hit {
            return Some(tex);
        }
        let pb = gdk_pixbuf::Pixbuf::from_file_at_scale(path, side * 4, -1, true).ok()?;
        let edge = pb.width().min(pb.height());
        // new_subpixbuf's preconditions fail (NULL, a panic in the binding)
        // on an empty image; the Python's except returns None for it.
        if edge <= 0 {
            return None;
        }
        let pb = pb.new_subpixbuf((pb.width() - edge) / 2, (pb.height() - edge) / 2, edge, edge);
        let pb = pb.scale_simple(side, side, gdk_pixbuf::InterpType::Bilinear)?;
        let tex = memory_texture(&pb, side, side)?;
        let bytes = tex_bytes(&tex);
        self.shared.borrow_mut().tex_cache.insert(key, tex.clone(), bytes);
        Some(tex)
    }

    /// Put a cached image on a card, decoding at most once per file.
    pub fn card_art(&self, item: &gtk::ListItem, token: u64, path: &Path) {
        let parts = {
            let st = self.store.borrow();
            match st.cards.get(item) {
                Some(slot) if slot.token == token => slot.parts.clone(),
                _ => return,
            }
        };
        let Some(tex) = self.texture(path, Win::CARD_ART_WIDTH) else { return };
        parts.initials.set_visible(false);
        parts.pic.set_paintable(Some(&tex));
        parts.art.set_visible(true);
    }

    pub fn thumb_async(&self, item: &gtk::ListItem, token: u64, plugin: &Plugin) {
        let w = self.downgrade();
        let item = item.clone();
        let plugin = plugin.clone();
        self.thumb_bg(
            move || store_mod::fetch_thumb(&plugin, false),
            move |path| {
                if let Some(path) = path {
                    if let Some(w) = w.upgrade() {
                        w.card_art(&item, token, &path);
                    }
                }
            },
        );
    }

    // ---- marketplace data ------------------------------------------------

    pub fn store_reload(&self, force: bool) {
        self.busy(true);
        self.summarize(&tr("Loading the plugin marketplace…"), Some("discover"));

        // `except Exception as exc: _store_failed(str(exc))`. The catalogue
        // is marked where it is cached, as the Python marks the cached list
        // `catalog()` hands back.
        let work = move || store_mod::try_catalog_annotated(force);
        let w = self.clone();
        spawn_bg(work, move |result| match result {
            Ok(plugins) => w.store_loaded(plugins),
            Err(message) => w.store_failed(&exc_str(&message)),
        });
    }

    pub fn store_failed(&self, message: &str) {
        self.busy(false);
        self.say(&format!("Could not load the catalog: {message}"), 5);
    }

    pub fn store_loaded(&self, plugins: Vec<Plugin>) {
        self.busy(false);
        {
            let mut sh = self.shared.borrow_mut();
            sh.plugins = Arc::new(plugins);
            sh.plugins_none = false;
        }
        self.store_filters();
        let page = self.store_page();
        self.pin_market(&page);
        self.store_refresh();
    }

    pub fn store_refresh(&self) -> glib::ControlFlow {
        if self.store_plugins().is_empty() {
            return glib::ControlFlow::Break;
        }
        let Some(store_cat) = self.store_dropdown("store_cat") else {
            return glib::ControlFlow::Break;
        };
        let idx = store_cat.selected();
        let category = {
            let mut sh = self.shared.borrow_mut();
            let category = pick_key(&sh.store_cats, idx);
            sh.store_cat_key = category.clone();
            category
        };
        let Some(store_sort) = self.store_dropdown("store_sort") else {
            return glib::ControlFlow::Break;
        };
        let sel = store_sort.selected() as usize;
        let sort = if sel < store_mod::SORTS.len() { store_mod::SORTS[sel].0 } else { "popular" };

        // Arguments in the order the Python evaluates them. The pool is
        // borrowed below, once no more widgets are read.
        let page = pool_page(None, &self.store_page());
        let query = self.search.text().to_string();
        let Some(store_installed) = self.store_toggle("store_installed") else {
            return glib::ControlFlow::Break;
        };
        let Some(store_verified) = self.store_toggle("store_verified") else {
            return glib::ControlFlow::Break;
        };
        let Some(store_trust) = self.store_dropdown("store_trust") else {
            return glib::ControlFlow::Break;
        };
        let trust = pick_key(&self.store.borrow().store_trusts, store_trust.selected());
        let Some(store_market) = self.store_dropdown("store_market") else {
            return glib::ControlFlow::Break;
        };
        let market = pick_key(&self.shared.borrow().store_markets, store_market.selected());
        let (installed_only, verified_only) = (store_installed.is_active(), store_verified.is_active());
        let results = {
            let plugins = {
                let sh = self.shared.borrow();
                if sh.plugins_none { Arc::default() } else { sh.plugins.clone() }
            };
            // The pool by position in the catalogue, so the results can
            // name entries rather than copy them.
            let themes = page == "themes";
            let at: Vec<usize> =
                (0..plugins.len()).filter(|&i| (plugins[i].install_kind == "theme") == themes).collect();
            let pool: Vec<&Plugin> = at.iter().map(|&i| &plugins[i]).collect();
            let idx = store_mod::try_browse_refs(&pool, &query, &category, sort, installed_only, verified_only, &market, &trust)
                .map(|idx| idx.into_iter().map(|i| at[i]).collect::<Vec<usize>>());
            drop(pool);
            idx.map(|idx| Results { plugins, idx })
        };
        let results = match results {
            Ok(results) => results,
            Err(e) => {
                eprintln!("almanac: {e}");
                return glib::ControlFlow::Break;
            }
        };

        let none = results.is_empty();
        self.store.borrow_mut().store_results = results;
        let Some(stack) = self.store.borrow().store_stack.clone() else {
            missing("store_stack");
            return glib::ControlFlow::Break;
        };
        let have_plugins = !self.store_plugins().is_empty();
        if !have_plugins {
            stack.set_visible_child_name("offline");
        } else if none {
            let cat_key = self.shared.borrow().store_cat_key.clone();
            let clearable = !self.search.text().trim().is_empty()
                || !cat_key.is_empty()
                || store_verified.is_active()
                || store_installed.is_active()
                || store_trust.selected() > 0;
            let Some(empty) = self.store.borrow().store_empty.clone() else {
                missing("store_empty");
                return glib::ControlFlow::Break;
            };
            empty.set_description(Some(&if clearable {
                tr("Try a different search, or clear the filters.")
            } else {
                tr("This marketplace has nothing listed right now.")
            }));
            stack.set_visible_child_name("empty");
        } else {
            stack.set_visible_child_name("grid");
        }
        let model = self.shared.borrow().store_model.clone();
        model.remove_all();
        // Back to the top: the previous scroll position means nothing once
        // the result set has changed underneath it.
        let Some(scroller) = self.store.borrow().store_scroller.clone() else {
            missing("store_scroller");
            return glib::ControlFlow::Break;
        };
        scroller.vadjustment().set_value(0.0);
        self.store_extend();
        glib::ControlFlow::Break
    }

    /// Debounce the search box: re-sorting 5,600 entries per keystroke.
    pub fn store_queue(&self) {
        let old = self.shared.borrow_mut().store_timer.take();
        if let Some(old) = old {
            // `GLib.source_remove(id)` is g_source_remove: look the id up in
            // the global default context (where timeout_add put it), destroy
            // it, or log a critical if it is gone. SourceId::remove would
            // panic on the latter, so the same steps are spelled out.
            match glib::MainContext::default().find_source_by_id(&old) {
                Some(source) => source.destroy(),
                None => glib::g_critical!(
                    "GLib",
                    "Source ID {} was not found when attempting to remove it",
                    old.as_raw()
                ),
            }
        }
        let w = self.clone();
        let id = glib::timeout_add_local(Duration::from_millis(180), move || w.store_tick());
        self.shared.borrow_mut().store_timer = Some(id);
    }

    pub fn store_tick(&self) -> glib::ControlFlow {
        self.shared.borrow_mut().store_timer = None;
        self.store_refresh();
        glib::ControlFlow::Break
    }

    /// Hand the current query to the repo/AUR search, which is a list.
    pub fn store_packages(&self) {
        let query = self.search.text().trim().to_string();
        if query.chars().count() < 2 {
            self.say(&tr("Type at least two characters to search repos and the AUR"), 5);
            return;
        }
        self.content_stack.set_visible_child_name("list");
        self.title.set_title(&tr("Repositories and AUR"));
        self.update_btn.set_visible(false);
        self.store_back.set_visible(true);
        self.rebuild_sort_menu(&sorts_discover());
        self.sort_btn.set_visible(true);
        self.shared.borrow_mut().manual_sorter = None;
        self.rebuild_sorter();
        self.run_discover();
    }

    pub fn store_return(&self) {
        self.show_page("discover", "");
        self.store_refresh();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn results_name_catalogue_entries_and_copy_only_a_page() {
        let plugins: Vec<Plugin> = (0..5).map(|i| Plugin { id: format!("p{i}"), ..Plugin::default() }).collect();
        let plugins = Arc::new(plugins);
        let r = Results { plugins: plugins.clone(), idx: vec![4, 0, 2] };
        assert_eq!(Arc::strong_count(&plugins), 2, "shared, not copied");
        let ids = |v: Vec<Plugin>| v.into_iter().map(|p| p.id).collect::<Vec<_>>();
        assert_eq!(ids(r.page(0, 2)), ["p4", "p0"]);
        assert_eq!(ids(r.page(2, 99)), ["p2"]);
        assert!(r.page(3, 9).is_empty() && r.page(7, 9).is_empty());
        assert_eq!((r.len(), Results::default().is_empty()), (3, true));
    }

    /// This file's own source, without the tests below.
    fn source() -> String {
        let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/", file!()))
            .unwrap_or_default();
        text.split("#[cfg(test)]").next().unwrap_or_default().to_string()
    }

    fn css() -> &'static str {
        crate::ui::shell::CSS
    }

    fn plugin(id: &str, kind: &str, market: &str) -> Plugin {
        Plugin {
            id: id.into(),
            name: id.to_uppercase(),
            install_kind: kind.into(),
            market: market.into(),
            ..Plugin::default()
        }
    }

    /// StorePages.test_each_page_draws_from_its_own_catalogue: themes have
    /// their own page; the App Store does not list them again.
    #[test]
    fn each_page_draws_from_its_own_catalogue() {
        let theme = plugin("t", "theme", "themes");
        let p = plugin("p", "plugin", "omarchy");
        let all = vec![theme.clone(), p.clone()];
        // `_store_pool(win)` with `win._page = "discover"`, and `"themes"`.
        assert_eq!(pool_of(&all, &pool_page(None, "discover")), vec![p.clone()]);
        assert_eq!(pool_of(&all, &pool_page(Some(""), "discover")), vec![p]);
        assert_eq!(pool_of(&all, &pool_page(Some("themes"), "discover")), vec![theme]);
    }


    #[test]
    fn star_tooltips_group_thousands() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(66175), "66,175");
        assert_eq!(thousands(1234567), "1,234,567");
    }

    #[test]
    fn a_card_without_initials_uses_the_name() {
        let mut p = plugin("x", "plugin", "omarchy");
        p.name = "hyprwhspr".into();
        assert_eq!(initials_of(&p), "HY");
        p.initials = "HW".into();
        assert_eq!(initials_of(&p), "HW");
        p.accent = "violet".into();
        assert_eq!(accent_class(&p), "acc-violet");
        p.accent = "plaid".into();
        assert_eq!(accent_class(&p), "acc-none");
    }

    #[test]
    fn dropdown_positions_out_of_range_mean_all() {
        let keys = vec![String::new(), "Graphics".to_string()];
        assert_eq!(pick_key(&keys, 1), "Graphics");
        assert_eq!(pick_key(&keys, 2), "");
        assert_eq!(pick_key(&keys, gtk::INVALID_LIST_POSITION), "");
    }

    /// VisualSystem.test_spacing_comes_from_the_scale, for this section.
    #[test]
    fn spacing_comes_from_the_scale() {
        let scale = [2, 4, 6, 8, 12, 16, 20, 24];
        let src = source();
        let re = regex::Regex::new(
            r"\.(margin_(?:start|end|top|bottom)|spacing)\((\d+)\)|Box::new\([^,()]+,\s*(\d+)\)",
        )
        .unwrap();
        let mut offenders = Vec::new();
        for c in re.captures_iter(&src) {
            let value: i64 = c.get(2).or(c.get(3)).unwrap().as_str().parse().unwrap();
            if value != 0 && !scale.contains(&value) {
                offenders.push(c.get(0).unwrap().as_str().to_string());
            }
        }
        assert!(offenders.is_empty(), "spacing off the scale: {offenders:?}");
    }

    /// VisualSystem.test_every_pill_class_is_defined_in_the_stylesheet.
    #[test]
    fn every_pill_class_is_defined_in_the_stylesheet() {
        let pills: BTreeSet<&str> =
            ["pill", "pill-crit", "pill-warn", "pill-ok", "pill-accent", "pill-quiet"].into();
        let src = source();
        let re = regex::Regex::new(r#""(pill(?:-[a-z]+)?)""#).unwrap();
        let used: BTreeSet<String> = re.captures_iter(&src).map(|c| c[1].to_string()).collect();
        assert!(!used.is_empty(), "no pill classes found at all");
        for name in &used {
            assert!(css().contains(&format!(".{name}")), "{name} is applied but never styled");
            assert!(pills.contains(name.as_str()), "unknown pill variant: {name}");
        }
    }

    /// VisualSystem.test_badges_use_one_pill_system, for this section.
    #[test]
    fn badges_use_one_pill_system() {
        let retired = [
            "crit-pill", "stale-pill", "crit-core", "store-chip", "store-installed",
            "trust-official", "trust-ok", "trust-warn", "trust-none",
        ];
        let src = source();
        let found: Vec<&str> =
            retired.iter().copied().filter(|n| src.contains(&format!("\"{n}\""))).collect();
        assert!(found.is_empty(), "retired badge classes still referenced: {found:?}");
    }

    /// Every class the cards put on a widget is styled.
    #[test]
    fn every_card_class_is_in_the_stylesheet() {
        for class in [
            "store-card", "store-shot", "store-initials", "store-name", "store-verified",
            "store-prov", "store-stars", "acc-none",
        ] {
            assert!(css().contains(&format!(".{class}")), "{class} is never styled");
        }
        for (name, _) in store_mod::ACCENTS {
            assert!(css().contains(&format!(".acc-{name}")), "acc-{name} is never styled");
        }
    }

    /// Accessibility.test_every_icon_only_button_is_named, for this section:
    /// the card's Install glyph gets its name on bind, next to the icon.
    #[test]
    fn every_icon_only_button_is_named() {
        let src = source();
        let lines: Vec<&str> = src.lines().collect();
        let mut unnamed = Vec::new();
        let mut seen = 0;
        for (i, line) in lines.iter().enumerate() {
            if !(line.contains("Button::from_icon_name(")
                || line.contains("btn.set_icon_name(")
                || line.contains("Button::builder().icon_name("))
            {
                continue;
            }
            seen += 1;
            // Python's window is 12 lines; the tooltip takes eight more here
            // once formatted, so the same statement sits further down.
            let window = lines[i..(i + 20).min(lines.len())].join("\n");
            if !window.contains("name(&") && !window.contains("Property::Label") {
                unnamed.push(format!("store.rs:{} {}", i + 1, line.trim()));
            }
        }
        assert!(seen > 0, "the card's Install glyph was not found");
        assert!(unnamed.is_empty(), "{unnamed:?}");
    }

    /// IconsExist.test_every_icon_name_is_in_adwaita_or_shipped_here.
    #[test]
    fn every_icon_name_is_in_adwaita_or_shipped_here() {
        let adwaita = Path::new("/usr/share/icons/Adwaita");
        if !adwaita.is_dir() {
            return; // Adwaita icon theme not installed
        }
        fn stems(dir: &Path, out: &mut BTreeSet<String>) {
            let Ok(rd) = std::fs::read_dir(dir) else { return };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stems(&p, out);
                } else if let Some(s) = p.file_stem() {
                    out.insert(s.to_string_lossy().into_owned());
                }
            }
        }
        let mut have = BTreeSet::new();
        stems(adwaita, &mut have);
        stems(&Path::new(env!("CARGO_MANIFEST_DIR")).join("icons"), &mut have);
        let re = regex::Regex::new(r#""([a-z0-9-]+-symbolic)""#).unwrap();
        let src = source();
        let missing: Vec<String> = re
            .captures_iter(&src)
            .map(|c| c[1].to_string())
            .filter(|n| !have.contains(n) && !n.starts_with("adw-"))
            .collect();
        assert!(missing.is_empty(), "{missing:?}");
        assert!(have.contains(INSTALL_ICON));
    }
}
