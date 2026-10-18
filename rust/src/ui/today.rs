//! UI section `today` (port of `app.py` lines 5096-5530): the Today page,
//! its hero and today's picks.
//!
//! STUBS: every method has its real signature and a harmless body; the
//! behaviour is ported in a later step.

#![allow(dead_code, unused_variables)]

use adw::prelude::*;

use crate::featured::{Item, Shelf};
use crate::picks::Pick;
use crate::store::Plugin;
use crate::ui::shell::Win;

/// Height of the hero banner (Python `HERO_HEIGHT`).
pub const HERO_HEIGHT: i32 = 120;
/// Height of a pick card (Python `PICK_HEIGHT`).
pub const PICK_HEIGHT: i32 = 176;
/// Width of a pick card's artwork (Python `PICK_ART`).
pub const PICK_ART: i32 = 196;

/// Each shelf is a sample of a bigger list; this says where the rest is.
/// (shelf key, (button label, page, sort))
pub const SHELF_TARGET: &[(&str, (&str, &str, Option<&str>))] = &[
    ("attention", ("Open Health check", "maintenance", None)),
    ("new", ("Show all", "discover", Some("new"))),
    ("trending", ("Show all", "discover", Some("trending"))),
    ("affinity", ("Show all", "discover", Some("popular"))),
];

/// `_SHELF_TARGET.get(key)`.
pub fn shelf_target(key: &str) -> Option<(&'static str, &'static str, Option<&'static str>)> {
    SHELF_TARGET
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, target)| *target)
}

/// The Today page's own widgets. Shared data (`_findings`, `_plugins`,
/// `_shelves`) lives on `shell::Shared`.
#[derive(Default)]
pub struct State {
    /// `self.feat_page`: the column holding hero, picks and shelves.
    pub feat_page: Option<gtk::Box>,
    /// `self.picks_area`: today's picks, hidden until there is something.
    pub picks_area: Option<gtk::Box>,
    /// `self.picks_flow`: the pick cards, two across when there is room.
    pub picks_flow: Option<gtk::FlowBox>,
    /// `self.hero`: the wallpaper banner.
    pub hero: Option<gtk::Overlay>,
    /// `self.hero_pic`: the wallpaper itself.
    pub hero_pic: Option<gtk::Picture>,
    /// `self.hero_head`: the one line that says whether anything is wrong.
    pub hero_head: Option<gtk::Label>,
    /// `self.hero_sub`: updates, reclaimable space, theme.
    pub hero_sub: Option<gtk::Label>,
}

impl Win {
    // ---- featured ---------------------------------------------------------

    /// The landing page: what deserves attention, assembled from live data.
    pub fn build_featured(&self) -> gtk::Widget {
        gtk::Box::new(gtk::Orientation::Vertical, 0).upcast()
    }

    /// Today's picks: hidden until there is something to show, so a
    /// machine with the network switched off just does not have the shelf.
    pub fn build_picks(&self) -> gtk::Box {
        gtk::Box::new(gtk::Orientation::Vertical, 4)
    }

    /// Ranks today's picks in the background, then shows them.
    pub fn picks_reload(&self, plugins: Vec<Plugin>) {}

    /// Replaces the pick cards with `found`; hides the shelf when empty.
    pub fn picks_show(&self, found: Vec<Pick>) {}

    /// One pick card: artwork, eyebrow, name, subtitle, description, the
    /// reasons it was picked and an install button.
    pub fn pick_card(&self, pick: &Pick, index: usize) -> gtk::Box {
        gtk::Box::new(gtk::Orientation::Horizontal, 0)
    }

    /// The listing's screenshot, at a size worth looking at; the accent
    /// tile with initials when it has none.
    pub fn pick_art(&self, p: &Plugin, pic: &gtk::Picture, art: &gtk::Box) {}

    /// The machine's own wallpaper, with the state of the machine on it.
    ///
    /// A hero that is decoration is dead weight on a page about what needs
    /// doing. This one is the image already behind the window — so it costs
    /// no download, carries no stock art, and re-skins with the theme — and
    /// it carries the one line that says whether anything is wrong.
    pub fn build_hero(&self) -> gtk::Overlay {
        gtk::Overlay::new()
    }

    /// Point the hero at the current wallpaper, or hide it if there is none.
    pub fn refresh_hero(&self) {}

    /// Sets the hero's headline from `problems` and its subline from
    /// updates, reclaimable space and the theme name.
    pub fn hero_text(&self, problems: usize) {}

    /// Rebuilds the shelves in the background (findings, catalog,
    /// `featured::build`), then shows them and reloads the picks.
    pub fn feat_reload(&self) {}

    /// Shows `shelves` under the hero and picks, updates the hero text, the
    /// summary and the sidebar badge.
    pub fn feat_show(&self, shelves: Vec<Shelf>) {}

    /// The "Show all" / "Open Health check" button for a shelf, or `None`
    /// when the shelf has no target or no items.
    pub fn shelf_link(&self, shelf: &Shelf) -> Option<gtk::Button> {
        None
    }

    /// Opens the page a shelf samples, with the store sorted by `sort`.
    pub fn open_shelf(&self, page: &str, sort: Option<&str>) {}

    /// One shelf row: severity pill, art, market chip and action button.
    pub fn feat_row(&self, item: &Item) -> adw::ActionRow {
        adw::ActionRow::new()
    }

    /// A marketplace entry's own card art, scaled down to a row icon.
    pub fn plugin_icon(&self, image: &gtk::Image, plugin: &Plugin) {}

    /// A row's action button: install, update, or open the Health check.
    pub fn feat_act(&self, item: &Item) {}

    /// A row activated: the plugin's detail, Updates, or the Health check.
    pub fn feat_open(&self, item: &Item) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shelf_target_table() {
        assert_eq!(
            shelf_target("attention"),
            Some(("Open Health check", "maintenance", None))
        );
        assert_eq!(shelf_target("new"), Some(("Show all", "discover", Some("new"))));
        assert_eq!(shelf_target("affinity"), Some(("Show all", "discover", Some("popular"))));
        assert_eq!(shelf_target("other"), None);
    }
}
