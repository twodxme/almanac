//! UI section `today` (port of `app.py` lines 5096-5530): the Today page,
//! its hero and today's picks.

use std::path::{Path, PathBuf};

use adw::prelude::*;
use chrono::Timelike;
use gtk::{gdk, pango};

use crate::featured::{self, Item, Shelf};
use crate::i18n::tr;
use crate::inventory::App;
use crate::maintenance;
use crate::picks::{self as picks_mod, Pick};
use crate::store::{self as store_mod, Plugin};
use crate::theming;
use crate::ui::history::severity_css;
use crate::ui::shell::{name, spawn_bg, AppItem, Win, INSTALL_ICON};

pub const HERO_HEIGHT: i32 = 120;
pub const PICK_HEIGHT: i32 = 176;
pub const PICK_ART: i32 = 196;

/// Where a shelf's link goes: (button label, page, sort on that page).
pub type ShelfLink = (&'static str, &'static str, Option<&'static str>);

// Each shelf is a sample of a bigger list; this says where the rest is.
pub const SHELF_TARGET: &[(&str, ShelfLink)] = &[
    ("attention", ("Open Health check", "maintenance", None)),
    ("new", ("Show all", "discover", Some("new"))),
    ("trending", ("Show all", "discover", Some("trending"))),
    ("affinity", ("Show all", "discover", Some("popular"))),
];

#[derive(Default)]
pub struct State {
    pub feat_page: Option<gtk::Box>,
    pub picks_area: Option<gtk::Box>,
    pub picks_flow: Option<gtk::FlowBox>,
    pub hero: Option<gtk::Overlay>,
    pub hero_pic: Option<gtk::Picture>,
    pub hero_head: Option<gtk::Label>,
    pub hero_sub: Option<gtk::Label>,
}

impl Win {
    // ---- featured ---------------------------------------------------------

    /// The landing page: what deserves attention, assembled from live data.
    pub fn build_featured(&self) -> gtk::Widget {
        let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let feat_page = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_start(12)
            .margin_end(12)
            .margin_top(12)
            .margin_bottom(16)
            .build();
        self.today.borrow_mut().feat_page = Some(feat_page.clone());
        feat_page.append(&self.build_hero());
        feat_page.append(&self.build_picks());
        let scroller = gtk::ScrolledWindow::builder().vexpand(true).build();
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroller.set_child(Some(&feat_page));
        page.append(&scroller);
        page.upcast()
    }

    /// Today's picks: hidden until there is something to show, so a
    /// machine with the network switched off just does not have the shelf.
    pub fn build_picks(&self) -> gtk::Box {
        let picks_area = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .visible(false)
            .build();
        let head = gtk::Label::builder().label(tr("Today's picks")).xalign(0.0).build();
        head.add_css_class("heading");
        let why = tr("Standouts today among what you can install here: stars and \
                      forks in the last 24 hours, downloads, and how much people \
                      are talking about it this week — measured on GitHub, and \
                      each figure says what it counts.");
        head.set_tooltip_text(Some(&why));
        head.update_property(&[gtk::accessible::Property::Description(&why)]);
        picks_area.append(&head);
        // Two across when there is room, stacked when there is not.
        let picks_flow = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .homogeneous(true)
            .min_children_per_line(1)
            .max_children_per_line(2)
            .column_spacing(12)
            .row_spacing(12)
            .build();
        picks_area.append(&picks_flow);
        {
            let mut st = self.today.borrow_mut();
            st.picks_area = Some(picks_area.clone());
            st.picks_flow = Some(picks_flow);
        }
        picks_area
    }

    pub fn picks_reload(&self, plugins: Vec<Plugin>) {
        let w = self.downgrade();
        spawn_bg(
            // The Python's `except Exception: found = []` has nothing to
            // catch here: `picks::today` falls back to an empty list itself.
            move || picks_mod::today(&plugins, None),
            move |found| {
                if let Some(w) = w.upgrade() {
                    w.picks_show(found);
                }
            },
        );
    }

    pub fn picks_show(&self, found: Vec<Pick>) {
        let (flow, area) = {
            let st = self.today.borrow();
            (st.picks_flow.clone(), st.picks_area.clone())
        };
        let (Some(flow), Some(area)) = (flow, area) else { return };
        while let Some(child) = flow.first_child() {
            flow.remove(&child);
        }
        for (i, pick) in found.iter().enumerate() {
            // What a card raises ends `_picks_show` there.
            match self.pick_card(pick, i) {
                Ok(card) => flow.append(&card),
                Err(e) => {
                    eprintln!("almanac: {e}");
                    return;
                }
            }
        }
        area.set_visible(!found.is_empty());
    }

    /// `Err` where a listing property raises.
    pub fn pick_card(&self, pick: &Pick, index: usize) -> Result<gtk::Box, String> {
        let p = &pick.plugin;
        let card = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        card.add_css_class("pick-card");
        card.set_overflow(gtk::Overflow::Hidden);
        card.set_size_request(300, PICK_HEIGHT);
        card.set_cursor_from_name(Some("pointer"));
        let click = gtk::GestureClick::new();
        {
            let w = self.downgrade();
            let pl = p.clone();
            click.connect_released(move |_, _, _, _| {
                if let Some(w) = w.upgrade() {
                    w.show_plugin(&pl);
                }
            });
        }
        card.add_controller(click);

        // Artwork, as large as the card is tall. Same clamp as the store
        // cards: the picture must not vote on the card's width.
        let art = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        art.add_css_class("pick-art");
        art.set_size_request(PICK_ART, -1);
        // Set explicitly, or the picture's hexpand propagates up and every
        // card's art takes a different share of the width.
        art.set_hexpand(false);
        art.set_overflow(gtk::Overflow::Hidden);
        let pic = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Cover)
            .hexpand(true)
            .vexpand(true)
            .build();
        let clamp = gtk::ScrolledWindow::builder().hexpand(true).vexpand(true).build();
        clamp.set_policy(gtk::PolicyType::External, gtk::PolicyType::External);
        clamp.set_propagate_natural_width(false);
        clamp.set_propagate_natural_height(false);
        clamp.set_child(Some(&pic));
        art.append(&clamp);
        card.append(&art);
        self.pick_art(p, &pic, &art);

        let body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .hexpand(true)
            .margin_start(16)
            .margin_end(16)
            .margin_top(12)
            .margin_bottom(12)
            .build();
        let what = if p.install_kind == "appimage" { tr("App") } else { tr("Plugin") };
        let eyebrow = gtk::Label::builder()
            .label(
                if index == 0 { format!("★ {what} of the day") } else { format!("{what} of the day") }
                    .to_uppercase(),
            )
            .xalign(0.0)
            .build();
        eyebrow.add_css_class("pick-eyebrow");
        body.append(&eyebrow);
        let name_l = gtk::Label::builder()
            .label(p.name.as_str())
            .xalign(0.0)
            .ellipsize(pango::EllipsizeMode::End)
            .build();
        name_l.add_css_class("pick-name");
        body.append(&name_l);
        let sub = gtk::Label::builder()
            .label(p.try_subtitle()?)
            .xalign(0.0)
            .ellipsize(pango::EllipsizeMode::End)
            .build();
        sub.add_css_class("caption");
        sub.add_css_class("dim-label");
        body.append(&sub);
        let desc = gtk::Label::builder()
            .label(p.description.as_str())
            .xalign(0.0)
            .wrap(true)
            .lines(2)
            .ellipsize(pango::EllipsizeMode::End)
            .margin_top(4)
            .vexpand(true)
            .valign(gtk::Align::Start)
            .build();
        desc.set_wrap_mode(pango::WrapMode::WordChar);
        desc.set_width_chars(20);
        desc.set_max_width_chars(60);
        body.append(&desc);

        let foot = gtk::Box::new(gtk::Orientation::Horizontal, 16);
        for (number, label) in &pick.reasons {
            let stat = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let n = gtk::Label::builder().label(number.as_str()).xalign(0.0).build();
            n.add_css_class("pick-num");
            let t = gtk::Label::builder()
                .label(label.as_str())
                .xalign(0.0)
                .ellipsize(pango::EllipsizeMode::End)
                .build();
            t.set_max_width_chars(26); // "downloads since yesterday"
            t.add_css_class("caption");
            t.add_css_class("dim-label");
            stat.append(&n);
            stat.append(&t);
            stat.update_property(&[gtk::accessible::Property::Label(&format!("{number} {label}"))]);
            foot.append(&stat);
        }
        foot.append(&gtk::Box::builder().hexpand(true).build());
        let btn = gtk::Button::builder().icon_name(INSTALL_ICON).valign(gtk::Align::End).build();
        btn.add_css_class("suggested-action");
        btn.add_css_class("circular");
        name(&btn, &format!("Install {}", p.name));
        {
            let w = self.downgrade();
            let pl = p.clone();
            btn.connect_clicked(move |_| {
                if let Some(w) = w.upgrade() {
                    w.on_plugin_install(&pl);
                }
            });
        }
        foot.append(&btn);
        body.append(&foot);
        card.append(&body);

        card.update_property(&[gtk::accessible::Property::Label(&format!(
            "{what} of the day: {}. {}",
            p.name,
            pick.reasons
                .iter()
                .map(|(n, l)| format!("{n} {l}"))
                .collect::<Vec<_>>()
                .join(", ")
        ))]);
        card.set_tooltip_text(Some(if p.description.is_empty() {
            p.name.as_str()
        } else {
            p.description.as_str()
        }));
        Ok(card)
    }

    /// The listing's screenshot, at a size worth looking at; the accent
    /// tile with initials when it has none.
    pub fn pick_art(&self, p: &Plugin, pic: &gtk::Picture, art: &gtk::Box) {
        let put = {
            let w = self.downgrade();
            let p = p.clone();
            let pic = pic.clone();
            let art = art.clone();
            move |path: Option<PathBuf>| {
                let Some(w) = w.upgrade() else { return };
                let tex: Option<gdk::Texture> =
                    path.as_deref().and_then(|path| w.texture(path, 640));
                if let Some(tex) = tex {
                    pic.set_paintable(Some(&tex));
                } else {
                    pic.set_visible(false);
                    let tile = gtk::Label::builder()
                        .label(if p.initials.is_empty() {
                            p.name.chars().take(2).collect::<String>().to_uppercase()
                        } else {
                            p.initials.clone()
                        })
                        .hexpand(true)
                        .vexpand(true)
                        .build();
                    tile.add_css_class("store-initials");
                    art.add_css_class(&if store_mod::accent(&p.accent).is_some() {
                        format!("acc-{}", p.accent)
                    } else {
                        "acc-none".to_string()
                    });
                    art.append(&tile);
                }
            }
        };
        let cached = store_mod::thumb_path(p, false);
        if let Some(cached) =
            cached.filter(|c| std::fs::metadata(c).is_ok_and(|m| m.len() > 0))
        {
            put(Some(cached));
        } else if !p.thumb.is_empty() {
            let p = p.clone();
            self.thumb_bg(move || store_mod::fetch_thumb(&p, false), put);
        } else {
            put(None);
        }
    }

    /// The machine's own wallpaper, with the state of the machine on it.
    ///
    /// A hero that is decoration is dead weight on a page about what needs
    /// doing. This one is the image already behind the window — so it costs
    /// no download, carries no stock art, and re-skins with the theme — and
    /// it carries the one line that says whether anything is wrong.
    pub fn build_hero(&self) -> gtk::Overlay {
        let overlay = gtk::Overlay::new();
        overlay.set_size_request(-1, HERO_HEIGHT);
        overlay.add_css_class("hero");
        overlay.set_overflow(gtk::Overflow::Hidden);

        let hero_pic = gtk::Picture::new();
        hero_pic.set_content_fit(gtk::ContentFit::Cover);
        hero_pic.set_hexpand(true);
        hero_pic.set_vexpand(true);
        let clamp = gtk::ScrolledWindow::builder().hexpand(true).vexpand(true).build();
        clamp.set_policy(gtk::PolicyType::External, gtk::PolicyType::External);
        clamp.set_propagate_natural_width(false);
        clamp.set_propagate_natural_height(false);
        clamp.set_child(Some(&hero_pic));
        overlay.set_child(Some(&clamp));

        let scrim = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        scrim.add_css_class("hero-scrim");
        overlay.add_overlay(&scrim);

        let text = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .valign(gtk::Align::End)
            .halign(gtk::Align::Start)
            .margin_start(20)
            .margin_end(20)
            .margin_bottom(16)
            .build();
        let hero_head = gtk::Label::builder().xalign(0.0).wrap(true).build();
        hero_head.add_css_class("hero-head");
        let hero_sub = gtk::Label::builder().xalign(0.0).wrap(true).build();
        hero_sub.add_css_class("hero-sub");
        text.append(&hero_head);
        text.append(&hero_sub);
        overlay.add_overlay(&text);

        {
            let mut st = self.today.borrow_mut();
            st.hero = Some(overlay.clone());
            st.hero_pic = Some(hero_pic);
            st.hero_head = Some(hero_head);
            st.hero_sub = Some(hero_sub);
        }
        self.refresh_hero();
        overlay
    }

    /// Point the hero at the current wallpaper, or hide it if there is none.
    pub fn refresh_hero(&self) {
        let (hero, hero_pic) = {
            let st = self.today.borrow();
            (st.hero.clone(), st.hero_pic.clone())
        };
        let (Some(hero), Some(hero_pic)) = (hero, hero_pic) else { return };
        let Some(bg) = theming::background() else {
            hero.set_visible(false);
            return;
        };
        hero.set_visible(true);
        if let Some(tex) = self.texture_cached(&bg, 1200) {
            hero_pic.set_paintable(Some(&tex));
            return;
        }
        // A wallpaper is a large photo (130-160 ms to decode here), and this
        // runs while the window is built: decode it off the main thread.
        let w = self.downgrade();
        let src = bg.clone();
        self.thumb_bg(
            move || crate::ui::store::decode_texture(&src, 1200),
            move |tex| {
                let (Some(w), Some(tex)) = (w.upgrade(), tex) else { return };
                w.texture_keep(&bg, 1200, &tex);
                // The wallpaper may have changed while this one decoded.
                if theming::background().as_ref() == Some(&bg) {
                    hero_pic.set_paintable(Some(&tex));
                }
            },
        );
    }

    /// `Err` where `theming.theme_name()` raises, before anything is set.
    pub fn hero_text(&self, problems: usize) -> Result<(), String> {
        let hour = chrono::Local::now().hour();
        let part = if hour < 12 {
            "Good morning"
        } else if hour < 18 {
            "Good afternoon"
        } else {
            "Good evening"
        };
        let name = theming::try_theme_name()?;
        let (head, sub) = {
            let st = self.today.borrow();
            (st.hero_head.clone(), st.hero_sub.clone())
        };
        let (Some(head), Some(sub)) = (head, sub) else { return Ok(()) };
        if problems > 0 {
            head.set_text(&format!(
                "{problems} thing{} need you",
                if problems != 1 { "s" } else { "" }
            ));
        } else {
            head.set_text(&format!("{part} — nothing needs you"));
        }
        let mut bits: Vec<String> = Vec::new();
        let (store, findings) = {
            let sh = self.shared.borrow();
            (sh.store.clone(), sh.findings.get())
        };
        let updates = (0..store.n_items())
            .filter(|&i| {
                store.item(i).as_ref().and_then(AppItem::of).is_some_and(|it| it.has_update)
            })
            .count();
        if updates > 0 {
            bits.push(format!(
                "{updates} update{} available",
                if updates != 1 { "s" } else { "" }
            ));
        }
        let free = maintenance::reclaimable_mb(&findings);
        if free != 0.0 {
            bits.push(format!("{} reclaimable", maintenance::fmt_size(free)));
        }
        if !name.is_empty() {
            bits.push(format!("wearing {name}"));
        }
        sub.set_text(&bits.join(" · "));
        Ok(())
    }

    pub fn feat_reload(&self) {
        self.busy(true);
        self.summarize(&tr("Working out what needs you…"), Some("featured"));

        // The worker cannot reach the list store or `shared` (both live on
        // the main thread), so what the Python's `work()` reads from `self`
        // is read here and handed over. `_findings` it assigns itself, as
        // the Python's worker does (`Findings` is shared with workers), so a
        // sidebar refresh before the shelves arrive already sees it;
        // `_plugins` is assigned when the result arrives, before `_feat_show`.
        let (store, findings, plugins) = {
            let sh = self.shared.borrow();
            (sh.store.clone(), sh.findings.clone(), sh.plugins.clone())
        };
        let apps: Vec<App> = (0..store.n_items())
            .filter_map(|i| store.item(i).as_ref().and_then(AppItem::of).map(|it| it.app.clone()))
            .collect();
        let w = self.downgrade();
        spawn_bg(
            move || {
                // The checks and the catalogue do not wait on each other.
                let (found, (plugins, fresh)) = std::thread::scope(|s| {
                    let checks = std::thread::Builder::new().stack_size(crate::util::DEEP_STACK).spawn_scoped(s, || {
                        // `getattr(self, "_findings", None) or
                        // maintenance.run_all()`, where a run the Health page
                        // made a few minutes ago counts.
                        let have = findings.get();
                        let found = if !have.is_empty() {
                            have
                        } else {
                            match maintenance::last_run() {
                                Some((age, found)) if age < maintenance::FINDINGS_FRESH => found,
                                _ => maintenance::run_all(),
                            }
                        };
                        // Assigned as soon as it is known, not after the
                        // catalog: the Python's GIL-bound worker sets
                        // `_findings` long before its slow main thread first
                        // applies the inventory, so the sidebar stats always
                        // show "Free-able"; Rust's main thread gets there
                        // first, and a later assignment came too late.
                        findings.set(found.clone());
                        found
                    });
                    // `if not plugins: try: catalog(); annotate(); self._plugins
                    // = plugins / except Exception: plugins = []`.
                    let catalog = if plugins.is_empty() {
                        match store_mod::try_catalog_annotated(false) {
                            Ok(plugins) => (plugins, true),
                            Err(_) => (Vec::new(), false),
                        }
                    } else {
                        (std::sync::Arc::unwrap_or_clone(plugins), false)
                    };
                    let found = match checks {
                        Ok(h) => h.join().unwrap_or_else(|p| std::panic::resume_unwind(p)),
                        Err(_) => {
                            let found = maintenance::run_all();
                            findings.set(found.clone());
                            found
                        }
                    };
                    (found, catalog)
                });
                let shelves = featured::build(&apps, &found, &plugins);
                (plugins, fresh, shelves)
            },
            move |(plugins, fresh, shelves)| {
                let Some(w) = w.upgrade() else { return };
                if fresh {
                    let mut sh = w.shared.borrow_mut();
                    sh.plugins = std::sync::Arc::new(plugins.clone());
                    sh.plugins_none = false;
                }
                w.feat_show(shelves);
                crate::ui::timings::mark("today shown");
                if !plugins.is_empty() {
                    w.picks_reload(plugins);
                }
            },
        );
    }

    pub fn feat_show(&self, shelves: Vec<Shelf>) {
        self.busy(false);
        self.shared.borrow_mut().shelves = shelves.clone();
        let (feat_page, picks_area) = {
            let st = self.today.borrow();
            (st.feat_page.clone(), st.picks_area.clone())
        };
        let (Some(feat_page), Some(picks_area)) = (feat_page, picks_area) else { return };
        // Keep the hero and the picks; replace everything under them.
        while let Some(child) = picks_area.next_sibling() {
            feat_page.remove(&child);
        }

        let mut problems = 0usize;
        for shelf in &shelves {
            if shelf.key == "attention" && shelf.items.is_empty() {
                // The good outcome takes one line, not a heading, a
                // description and a row.
                let ok = gtk::Box::builder().spacing(8).margin_top(2).build();
                let tick = gtk::Image::from_icon_name("object-select-symbolic");
                tick.add_css_class("store-verified");
                ok.append(&tick);
                let msg = gtk::Label::builder()
                    .label(shelf.empty.as_str())
                    .xalign(0.0)
                    .hexpand(true)
                    .build();
                msg.add_css_class("dim-label");
                ok.append(&msg);
                if let Some(link) = self.shelf_link(shelf) {
                    ok.append(&link);
                }
                feat_page.append(&ok);
                continue;
            }
            let title = if shelf.items.is_empty() {
                shelf.title.clone()
            } else {
                format!("{} ({})", shelf.title, shelf.items.len())
            };
            let link = self.shelf_link(shelf);
            let section = Win::section(&title, &shelf.subtitle, link.as_ref().map(|b| b.upcast_ref::<gtk::Widget>()));
            if shelf.items.is_empty() {
                let row: adw::ActionRow = Win::plain(&shelf.empty, "");
                if shelf.key == "attention" {
                    let tick = gtk::Image::builder()
                        .icon_name("object-select-symbolic")
                        .valign(gtk::Align::Center)
                        .build();
                    tick.add_css_class("store-verified");
                    row.add_prefix(&tick);
                }
                section.rows.append(&row);
            }
            for item in &shelf.items {
                if item.severity == "crit" || item.severity == "warn" {
                    problems += 1;
                }
                // What a row raises ends `_feat_show` there.
                match self.feat_row(item) {
                    Ok(row) => section.rows.append(&row),
                    Err(e) => {
                        eprintln!("almanac: {e}");
                        return;
                    }
                }
            }
            feat_page.append(&section.root);
        }

        self.refresh_hero();
        // What `_hero_text` raises ends `_feat_show` here.
        if let Err(e) = self.hero_text(problems) {
            eprintln!("almanac: {e}");
            return;
        }
        self.summarize(
            &if problems > 0 {
                format!("{problems} thing(s) need you")
            } else {
                tr("Nothing needs you right now")
            },
            Some("featured"),
        );
        let count = self.shared.borrow().sidebar_row("featured").map(|r| r.count.clone());
        if let Some(count) = count {
            // Featured's shelves are Shelf objects; the AI page's are
            // (title, description, entries) tuples. Different shapes.
            let crit = shelves
                .iter()
                .flat_map(|shelf| shelf.items.iter())
                .any(|item| item.severity == "crit");
            Win::badge(&count, problems, if crit { "crit" } else { "warn" });
        }
    }

    pub fn shelf_link(&self, shelf: &Shelf) -> Option<gtk::Button> {
        let target = SHELF_TARGET.iter().find(|(k, _)| *k == shelf.key).map(|(_, t)| *t);
        let (Some((label, page, sort)), false) = (target, shelf.items.is_empty()) else {
            return None;
        };
        let btn = gtk::Button::builder().label(label).valign(gtk::Align::Center).build();
        btn.add_css_class("flat");
        btn.set_tooltip_text(Some(&format!(
            "Open the {}{}",
            if page == "discover" { "App Store" } else { "Health check" },
            sort.map(|s| format!(", sorted by {s}")).unwrap_or_default()
        )));
        let w = self.downgrade();
        btn.connect_clicked(move |_| {
            if let Some(w) = w.upgrade() {
                w.open_shelf(page, sort);
            }
        });
        Some(btn)
    }

    pub fn open_shelf(&self, page: &str, sort: Option<&str>) {
        if let (true, Some(sort)) = (page == "discover", sort.filter(|s| !s.is_empty())) {
            let keys: Vec<&str> = store_mod::SORTS.iter().map(|(k, _l, _f)| *k).collect();
            if let Some(index) = keys.iter().position(|k| *k == sort) {
                let dropdown = self.shared.borrow().store_sort.clone();
                if let Some(dropdown) = dropdown {
                    dropdown.set_selected(index as u32);
                }
            }
        }
        self.select_category(page);
    }

    /// `Err` where a listing property raises.
    pub fn feat_row(&self, item: &Item) -> Result<adw::ActionRow, String> {
        let row: adw::ActionRow = Win::plain(&item.title, &item.detail);
        row.set_subtitle_lines(1);
        row.set_tooltip_text(Some(&crate::util::human_error(&item.detail)));

        if !item.severity.is_empty() {
            let (label, css) = severity_css(if item.severity == "crit" {
                maintenance::CRIT
            } else {
                maintenance::WARN
            })
            .unwrap_or((String::new(), "pill"));
            let pill = gtk::Label::builder().label(label).valign(gtk::Align::Center).build();
            pill.add_css_class(css);
            row.add_prefix(&pill);
        }

        let art = gtk::Image::builder().pixel_size(24).valign(gtk::Align::Center).build();
        if let Some(plugin) = &item.plugin {
            art.set_icon_name(Some("application-x-addon-symbolic"));
            self.plugin_icon(&art, plugin);
        } else if let Some(app) = &item.app {
            self.app_icon(&art, app, 48);
        } else {
            art.set_icon_name(Some("emblem-important-symbolic"));
            art.add_css_class("dim-label");
        }
        row.add_prefix(&art);

        if let Some(plugin) = &item.plugin {
            let chip = gtk::Label::builder()
                .label(plugin.try_market_name_text()?)
                .valign(gtk::Align::Center)
                .build();
            chip.add_css_class("pill");
            row.add_suffix(&chip);
        }

        if !item.action.is_empty() {
            let btn = if item.action == "Install" {
                gtk::Button::builder().icon_name(INSTALL_ICON).valign(gtk::Align::Center).build()
            } else {
                gtk::Button::builder()
                    .label(item.action.as_str())
                    .valign(gtk::Align::Center)
                    .build()
            };
            btn.update_property(&[gtk::accessible::Property::Label(&format!(
                "{} {}",
                item.action, item.title
            ))]);
            if item.severity == "crit" {
                btn.add_css_class("suggested-action");
            }
            btn.set_tooltip_text(Some(&format!("{} {}", item.action, item.title)));
            let w = self.downgrade();
            let it = item.clone();
            btn.connect_clicked(move |_| {
                if let Some(w) = w.upgrade() {
                    w.feat_act(&it);
                }
            });
            row.add_suffix(&btn);
        }

        row.set_activatable(true);
        let w = self.downgrade();
        let it = item.clone();
        row.connect_activated(move |_| {
            if let Some(w) = w.upgrade() {
                w.feat_open(&it);
            }
        });
        Ok(row)
    }

    /// A marketplace entry's own card art, scaled down to a row icon.
    pub fn plugin_icon(&self, image: &gtk::Image, plugin: &Plugin) {
        let cached = store_mod::thumb_path(plugin, false);
        if let Some(cached) =
            cached.filter(|c| std::fs::metadata(c).is_ok_and(|m| m.len() > 0))
        {
            self.set_icon(image, &cached, 64);
            return;
        }

        let done = {
            let w = self.downgrade();
            let image = image.clone();
            move |path: Option<PathBuf>| {
                let Some(w) = w.upgrade() else { return };
                if let Some(path) = path {
                    w.set_icon(&image, Path::new(&path), 64);
                }
            }
        };

        if !plugin.thumb.is_empty() {
            let plugin = plugin.clone();
            self.thumb_bg(move || store_mod::fetch_thumb(&plugin, false), done);
        }
    }

    pub fn feat_act(&self, item: &Item) {
        if let Some(plugin) = &item.plugin {
            self.on_plugin_install(plugin);
        } else if let (true, Some(app)) = (item.kind == "update", &item.app) {
            self.on_update_one(app);
        } else if item.kind == "finding" {
            self.select_category("maintenance");
        }
    }

    pub fn feat_open(&self, item: &Item) {
        if let Some(plugin) = &item.plugin {
            self.show_plugin(plugin);
        } else if item.app.is_some() {
            self.select_category("updates");
        } else {
            self.select_category("maintenance");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/", file!()));
    const CSS: &str = include_str!("../../data/style.css");

    /// This file without its tests, so the inspections below do not find
    /// their own patterns.
    fn code() -> &'static str {
        SRC.split("#[cfg(test)]").next().unwrap_or(SRC)
    }

    #[test]
    fn shelf_target_table() {
        let get = |key: &str| SHELF_TARGET.iter().find(|(k, _)| *k == key).map(|(_, t)| *t);
        assert_eq!(get("attention"), Some(("Open Health check", "maintenance", None)));
        assert_eq!(get("new"), Some(("Show all", "discover", Some("new"))));
        assert_eq!(get("trending"), Some(("Show all", "discover", Some("trending"))));
        assert_eq!(get("affinity"), Some(("Show all", "discover", Some("popular"))));
        assert_eq!(get("other"), None);
    }

    /// Port of `Accessibility.test_every_icon_only_button_is_named`: an
    /// icon-only button must reach `name()` or set the accessible label
    /// within a few lines of being created.
    #[test]
    fn every_icon_only_button_is_named() {
        let lines: Vec<&str> = code().lines().collect();
        let mut unnamed = Vec::new();
        let mut seen = 0;
        for (i, line) in lines.iter().enumerate() {
            if !(line.contains("Button::builder().icon_name(")
                || line.contains("Button::from_icon_name("))
            {
                continue;
            }
            seen += 1;
            // The variable it is bound to: the nearest `let x =` at or above.
            let var = lines[..=i]
                .iter()
                .rev()
                .take(4)
                .find_map(|l| {
                    let l = l.trim_start();
                    l.strip_prefix("let ")
                        .and_then(|r| r.split([' ', ':']).next())
                })
                .unwrap_or("");
            let end = (i + 12).min(lines.len());
            let window = lines[i..end].join("\n");
            if !window.contains(&format!("name(&{var}")) && !window.contains("Property::Label") {
                unnamed.push(format!("today.rs:{} {var}", i + 1));
            }
        }
        // The pick card's and the shelf row's install buttons.
        assert_eq!(seen, 2, "the inspection must see both icon-only buttons");
        assert_eq!(unnamed, Vec::<String>::new());
    }

    /// Port of `VisualSystem.test_spacing_comes_from_the_scale`.
    #[test]
    fn spacing_comes_from_the_scale() {
        const SCALE: [i32; 8] = [2, 4, 6, 8, 12, 16, 20, 24];
        let mut offenders = Vec::new();
        let src = code();
        for prop in ["margin_start(", "margin_end(", "margin_top(", "margin_bottom(", ".spacing(",
                     "set_spacing("] {
            for (at, _) in src.match_indices(prop) {
                let rest = &src[at + prop.len()..];
                let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                if let Ok(v) = digits.parse::<i32>() {
                    if !SCALE.contains(&v) {
                        offenders.push(format!("{prop}{v}"));
                    }
                }
            }
        }
        // `gtk::Box::new(orientation, spacing)`
        for (at, _) in src.match_indices("Box::new(gtk::Orientation::") {
            let rest = &src[at..];
            let Some(close) = rest.find(')') else { continue };
            let args = &rest[..close];
            if let Some(v) = args.rsplit(", ").next().and_then(|v| v.trim().parse::<i32>().ok()) {
                if v != 0 && !SCALE.contains(&v) {
                    offenders.push(format!("Box::new spacing {v}"));
                }
            }
        }
        assert_eq!(offenders, Vec::<String>::new());
    }

    /// Port of `VisualSystem.test_every_pill_class_is_defined_in_the_stylesheet`
    /// for the pills this page applies (its own and `severity_css`'s).
    #[test]
    fn every_pill_class_is_defined_in_the_stylesheet() {
        const PILLS: [&str; 6] = ["pill", "pill-crit", "pill-warn", "pill-ok", "pill-accent", "pill-quiet"];
        let src = code();
        let mut used = Vec::new();
        for (at, _) in src.match_indices("\"pill") {
            let rest = &src[at + 1..];
            if let Some(end) = rest.find('"') {
                let name = &rest[..end];
                if name.chars().all(|c| c.is_ascii_lowercase() || c == '-') {
                    used.push(name.to_string());
                }
            }
        }
        assert!(!used.is_empty(), "no pill classes found at all");
        for name in &used {
            assert!(CSS.contains(&format!(".{name}")), "{name} is applied but never styled");
            assert!(PILLS.contains(&name.as_str()), "unknown pill variant: {name}");
        }
        for sev in [maintenance::CRIT, maintenance::WARN] {
            let (_label, css) = severity_css(sev).unwrap_or((String::new(), "pill"));
            assert!(PILLS.contains(&css));
            assert!(CSS.contains(&format!(".{css}")));
        }
    }

    /// The classes this page applies are all styled.
    #[test]
    fn page_classes_are_in_the_stylesheet() {
        for class in ["pick-card", "pick-art", "pick-eyebrow", "pick-name", "pick-num", "hero",
                      "hero-scrim", "hero-head", "hero-sub", "store-initials", "store-verified",
                      "acc-none"] {
            assert!(CSS.contains(&format!(".{class}")), "{class} is applied but never styled");
        }
        for (accent, _) in store_mod::ACCENTS {
            assert!(CSS.contains(&format!(".acc-{accent}")), "acc-{accent} is never styled");
        }
    }

    /// Port of `IconsExist.test_every_icon_name_is_in_adwaita_or_shipped_here`
    /// for the icon names this page uses.
    #[test]
    fn every_icon_name_is_in_adwaita_or_shipped_here() {
        let adwaita = Path::new("/usr/share/icons/Adwaita");
        if !adwaita.is_dir() {
            eprintln!("Adwaita icon theme not installed; skipped");
            return;
        }
        fn walk(dir: &Path, out: &mut std::collections::HashSet<String>) {
            let Ok(rd) = std::fs::read_dir(dir) else { return };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if matches!(p.extension().and_then(|x| x.to_str()), Some("svg" | "png")) {
                    if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                        out.insert(stem.to_string());
                    }
                }
            }
        }
        let mut have = std::collections::HashSet::new();
        walk(adwaita, &mut have);
        walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("icons"), &mut have);
        let src = code();
        let mut names: Vec<String> = Vec::new();
        for (at, _) in src.match_indices("-symbolic\"") {
            let head = &src[..at];
            if let Some(start) = head.rfind('"') {
                let n = &src[start + 1..at + "-symbolic".len()];
                if n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
                    names.push(n.to_string());
                }
            }
        }
        assert!(names.len() >= 3, "the inspection found no icon names: {names:?}");
        names.push(INSTALL_ICON.to_string());
        let missing: Vec<&String> = names
            .iter()
            .filter(|n| !have.contains(n.as_str()) && !n.starts_with("adw-"))
            .collect();
        assert_eq!(missing, Vec::<&String>::new());
    }
}
