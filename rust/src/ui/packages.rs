//! UI section `packages` (port of `app.py` lines 1454-2481).
//!
//! Data loading and filtering: the Updates, Applications, Everything and
//! Orphans tables, the detail dialog, the sidebar's selection and counts,
//! page chrome, the header's primary action, theming and the status line.
//!
//! STUBS: every method has its real signature; bodies do nothing yet.

#![allow(unused_variables, dead_code, unused_imports)]

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::i18n::tr;
use crate::inventory;
use crate::ui::shell::{AppItem, Win};

/// The header's primary action for one page: (label, enabled, tooltip,
/// handler), as `_primary_actions` returns it.
#[derive(Clone)]
pub struct PrimaryAction {
    pub label: String,
    pub enabled: bool,
    pub tooltip: String,
    pub handler: Rc<dyn Fn(&Win)>,
}

/// Rows `plain` can build: the Python passed the row class (`cls`), which
/// is `Adw.ActionRow` by default and `Adw.ExpanderRow` in a few places.
pub trait PlainRow: IsA<adw::PreferencesRow> {
    fn new_plain() -> Self;
    fn put_subtitle(&self, subtitle: &str);
}

impl PlainRow for adw::ActionRow {
    fn new_plain() -> Self {
        adw::ActionRow::new()
    }
    fn put_subtitle(&self, subtitle: &str) {
        self.set_subtitle(subtitle);
    }
}

impl PlainRow for adw::ExpanderRow {
    fn new_plain() -> Self {
        adw::ExpanderRow::new()
    }
    fn put_subtitle(&self, subtitle: &str) {
        self.set_subtitle(subtitle);
    }
}

/// State only this section uses.
#[derive(Default)]
pub struct State {
    /// Health findings already announced (`_reported`), so a rescan does not
    /// repeat a notification.
    pub reported: BTreeSet<String>,
    /// The App Store / discover results are showing (`_discovering`).
    pub discovering: bool,
    /// Debounce for the discover search (`_discover_timer`).
    pub discover_timer: Option<glib::SourceId>,
    /// Coalesces theme-change bursts into one restyle (`_theme_timer`).
    pub theme_timer: Option<glib::SourceId>,
    /// The window is in its narrow layout (`_narrow`).
    pub narrow: bool,
    /// The current page's primary action (`_primary`).
    pub primary: Option<PrimaryAction>,
    /// Urgency, then name (`_priority_sorter`); built by the shell's init.
    pub priority_sorter: Option<gtk::CustomSorter>,
    /// First-visit flags for pages loaded lazily.
    pub feat_loaded_once: bool,
    pub ai_loaded_once: bool,
    pub maint_loaded_once: bool,
    pub store_loaded_once: bool,
}

impl Win {
    // ---- data ------------------------------------------------------------
    pub fn reload(&self) {
        // checkupdates and yay take seconds; on the UI thread that is a freeze.
    }

    /// Desktop notification, falling back to the status line.
    pub fn notify(&self, title: &str, body: &str, urgency: &str) {}

    /// Tell the user once about things that need a decision, not a click.
    ///
    /// Only new findings are announced: a notification repeated on every
    /// rescan is one the user learns to dismiss without reading.
    pub fn report_health(&self, apps: &[inventory::App]) {}

    pub fn apply(&self, apps: Vec<inventory::App>) {}

    pub fn update_status(&self) {}

    pub fn on_row_activated(&self, view: &gtk::ColumnView, position: u32) {}

    /// A card for one app: everything known about it, and what can be done.
    ///
    /// Details are fetched on open rather than during the scan -- dependency
    /// lists are a pacman call each, which would make the list slow for data
    /// nobody reads until they ask for it.
    pub fn show_details(&self, item: &AppItem) {}

    pub fn detail_badges(&self, item: &AppItem) -> Vec<(String, &'static str)> {
        Vec::new()
    }

    /// A row whose text is taken literally.
    ///
    /// Adw rows parse title and subtitle as Pango markup, and every string
    /// put in one here was written by someone else -- a skill description, a
    /// marketplace listing, a package summary. An unescaped "&" or "<name>"
    /// makes the row render empty.
    ///
    /// The order matters: the text is parsed when it is assigned, so markup
    /// has to be switched off before the title and subtitle go in, not after.
    pub fn plain<R: PlainRow>(title: &str, subtitle: &str) -> R {
        let row = R::new_plain();
        row.set_use_markup(false);
        if !title.is_empty() {
            row.set_title(title);
        }
        if !subtitle.is_empty() {
            row.put_subtitle(subtitle);
        }
        row
    }

    pub fn detail_row(group: &adw::PreferencesGroup, title: &str, value: &str) {
        // A row reading "Reviewed by: (blank)" says less than no row at all,
        // and the provenance group has several fields that only some entries
        // carry.
        if value.is_empty() {
            return;
        }
        let row: adw::ActionRow = Win::plain(title, value);
        row.set_subtitle_selectable(true);
        group.add(&row);
    }

    pub fn update_from_detail(&self, item: &AppItem) {}

    pub fn rebuild_sort_menu<K: AsRef<str>, L: AsRef<str>>(&self, options: &[(K, L)]) {}

    /// Sort by a field that is no longer its own column.
    ///
    /// Selecting one replaces the column sorter; clicking a column header
    /// afterwards takes precedence again, because the MultiSorter still has
    /// the view's sorter first.
    pub fn on_sort(&self, action: &gio::SimpleAction, param: Option<&glib::Variant>) {}

    pub fn rebuild_sorter(&self) {}

    /// Set a sidebar count and how loudly it should read.
    ///
    /// Three levels, and only one of them is red. Every badge used to turn
    /// red as soon as any single package anywhere was security-critical,
    /// which painted Featured, Updates and AI the same alarming colour and
    /// left the actual security item indistinguishable from the rest.
    pub fn badge(label: &gtk::Label, count: usize, level: &str) {
        label.set_text(&if count > 0 {
            count.to_string()
        } else {
            String::new()
        });
        for css in ["is-warn", "is-crit"] {
            label.remove_css_class(css);
        }
        label.set_visible(count > 0);
        if count == 0 {
            return;
        }
        if level == "crit" {
            label.add_css_class("is-crit");
        } else if level == "warn" {
            label.add_css_class("is-warn");
        }
    }

    pub fn stat_row(&self, label: &str, value: &str, tooltip: &str) -> gtk::Box {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let name = gtk::Label::builder()
            .label(label)
            .xalign(0.0)
            .hexpand(true)
            .build();
        name.add_css_class("stat-key");
        let val = gtk::Label::builder().label(value).xalign(1.0).build();
        val.add_css_class("stat-val");
        row.append(&name);
        row.append(&val);
        if !tooltip.is_empty() {
            row.set_tooltip_text(Some(tooltip));
        }
        row
    }

    /// The numbers you would otherwise open a terminal to get.
    pub fn refresh_stats(&self) {
        // Counting packages, not releases: summing release deltas across
        // date-versioned and epoch-versioned packages produced numbers in the
        // tens of thousands, which described nothing.
    }

    pub fn kernel_version() -> String {
        if crate::demo::active() {
            return "6.18.5".to_string(); // a screenshot must not name this machine's
        }
        // os.uname().release
        match std::fs::read_to_string("/proc/sys/kernel/osrelease") {
            Ok(release) => {
                let release = release.trim();
                release.split("-arch").next().unwrap_or("").to_string()
            }
            Err(_) => String::new(),
        }
    }

    pub fn uptime() -> String {
        if crate::demo::active() {
            return "6d 2h".to_string();
        }
        let Ok(text) = std::fs::read_to_string("/proc/uptime") else {
            return String::new();
        };
        let Some(seconds) = text
            .split_whitespace()
            .next()
            .and_then(|s| s.parse::<f64>().ok())
        else {
            return String::new();
        };
        let total = seconds as i64;
        let (days, rem) = (total.div_euclid(86400), total.rem_euclid(86400));
        let (hours, minutes) = ((rem / 60).div_euclid(60), (rem / 60).rem_euclid(60));
        if days != 0 {
            return format!("{days}d {hours}h");
        }
        if hours != 0 {
            return format!("{hours}h {minutes}m");
        }
        format!("{minutes}m")
    }

    /// Per-category totals, so the sidebar answers "what needs me?" at rest.
    pub fn update_sidebar_counts(&self) {
        // An empty category stays visible but recedes, so the layout does
        // not reshuffle as things are installed and removed.
        // Red means "a security update is waiting", nothing else. Pending
        // updates are a number, not an emergency.
    }

    // ---- filtering -------------------------------------------------------
    /// `_match`: the row filter (`match` is a Rust keyword, hence `r#`).
    pub fn r#match(&self, item: &AppItem) -> bool {
        true
    }

    /// Query the repos and the AUR for what the search box says.
    ///
    /// Debounced: every keystroke would otherwise be a pacman -Ss plus an AUR
    /// round trip.
    pub fn run_discover(&self) {}

    pub fn discover_now(&self) -> glib::ControlFlow {
        // Nothing typed: show the marketplace itself. Packages are excluded
        // there -- 15,669 of them is a database dump, not a storefront, and
        // there is no popularity signal to rank them.
        // Plugins lead: on an Omarchy box they are the likelier intent, and
        // the package list is long enough to bury them otherwise.
        glib::ControlFlow::Break
    }

    pub fn show_discover(&self, query: &str, results: Vec<inventory::App>) {}

    pub fn on_search(&self, entry: &gtk::SearchEntry) {}

    /// Header rows fold their group; everything else is a category.
    pub fn on_sidebar_activated(&self, list: &gtk::ListBox, row: &gtk::ListBoxRow) {}

    pub fn toggle_group(&self, gkey: &str) {}

    pub fn apply_group_state(&self, gkey: &str) {
        // ungrouped primaries never fold
        // The selected category stays visible even when its group folds,
        // or the sidebar would show a selection that is not there.
    }

    pub fn apply_all_group_states(&self) {}

    /// Go to a category, whether or not its row is already selected.
    ///
    /// GtkListBox emits nothing when you select the row that is already
    /// selected, so the page never got applied in that case -- which is
    /// exactly what happens when a restored category and `--page` name the
    /// same destination, or when another path has already moved the
    /// selection. Applying it directly makes the call mean what it says.
    pub fn select_category(&self, key: &str) {}

    pub fn on_sidebar_selected(&self, list: &gtk::ListBox, row: Option<&gtk::ListBoxRow>) {
        // selection cleared during a rebuild when row is None.
        // A folded group keeps only the selected row showing; once the
        // selection moves on, that row folds away too. Without this a row
        // kept visible because it was selected stayed out after leaving it.
    }

    /// Fold or unfold for the window's width.
    pub fn set_narrow(&self, narrow: bool) {
        // Collapsed, the sidebar is an overlay that should start hidden;
        // expanded, it is furniture and should be back.
        // One column of cards is the right answer on a narrow window; four
        // would each be 110px wide.
    }

    pub fn select_category_and_close(&self, key: &str) {}

    pub fn page_title(&self, key: &str, fallback: &str) -> String {
        fallback.to_string()
    }

    /// Apply one page's chrome. The only place that touches it.
    pub fn show_page(&self, key: &str, category: &str) {}

    /// Re-read whatever the current page shows. One action, every page.
    pub fn refresh_page(&self) {}

    /// Point the marketplace filter at whatever this page is about.
    ///
    /// The App Store shows every catalogue; Themes is the same view with one
    /// of them selected. Changing the filter by hand afterwards re-titles the
    /// page, so the header never claims to be showing something it is not.
    pub fn pin_market(&self, page: &str) {
        // Themes is one catalogue already; the App Store chooses among the rest.
    }

    /// Say what the store is showing when the marketplace filter changes.
    ///
    /// This only edits the title. It used to also select the matching
    /// sidebar row, which re-entered the selection handler and rebuilt the
    /// page -- so opening Themes, then letting the catalogue finish loading,
    /// put you back on the App Store. Which page you are on is the sidebar's
    /// business and nothing else's.
    pub fn retitle_store(&self) {
        // Un-pinned from the Themes page: it is the whole store now.
    }

    /// Put the current page's main action in the header button.
    pub fn sync_primary(&self) {
        // A disabled primary button ("Nothing to reclaim", "Plugins current")
        // takes the header's most prominent place to say there is nothing to
        // do; the subtitle already says that.
    }

    /// (label, enabled, tooltip, handler) for each page that has one.
    pub fn primary_actions(&self) -> BTreeMap<String, PrimaryAction> {
        BTreeMap::new()
    }

    pub fn on_primary(&self, button: &gtk::Button) {}

    /// A transient result. Replaces four separate dim status labels.
    pub fn say(&self, message: &str, timeout: u32) {}

    /// Restate the active Omarchy palette as libadwaita's named colours.
    ///
    /// Absence is a valid state: with no Omarchy theme the provider is
    /// emptied and GTK's own theme shows through unmodified.
    pub fn apply_theme(&self) {
        // No Omarchy palette: measure the pills against whatever theme GTK
        // is showing, so they read as well there as on an Omarchy one.
    }

    /// The active GTK theme's status fills, as hex. lookup_color is the
    /// only way GTK 4 exposes a named colour; it is deprecated, not gone.
    pub fn theme_fills(&self) -> BTreeMap<String, String> {
        // Translucent colours are flattened onto the window first:
        // libadwaita's dark card is white at 8%, and read as plain white
        // it measured "black text" onto a dark card.
        BTreeMap::new()
    }

    pub fn on_theme_changed(&self) {
        // The theme is swapped as several filesystem operations, so this
        // fires a handful of times; coalesce into one restyle.
    }

    pub fn theme_tick(&self) -> glib::ControlFlow {
        glib::ControlFlow::Break
    }

    /// The one place the window says it is doing something.
    pub fn busy(&self, working: bool) {}

    /// The standing one-line description of what is on screen.
    ///
    /// Scoped to a page because the loaders are asynchronous: the package
    /// scan finishing half a second after you opened the App Store used to
    /// overwrite the store's summary with "129 installed · showing 0".
    pub fn summarize(&self, text: &str, page: Option<&str>) {}

    /// Put the installed inventory back. True if the store was showing.
    pub fn leave_store(&self) -> bool {
        false
    }
}
