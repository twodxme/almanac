//! The table's columns and the click slots of their recycled buttons.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::i18n::tr;
use crate::inventory;

use super::helpers::{INSTALL_ICON, age, as_text, fmt_size, lower_cmp, name, source_label, truthy, version_text};
use super::items::AppItem;
use super::window::Win;

impl Win {
    /// `_column`: append one column to the table.
    ///
    /// `fixed` is the STARTING width (0 for none); `resizable` lets the user
    /// drag the header edge from there.
    #[allow(clippy::too_many_arguments)]
    pub fn column(
        &self,
        title: &str,
        setup: impl Fn(&gtk::SignalListItemFactory, &glib::Object) + 'static,
        bind: impl Fn(&gtk::SignalListItemFactory, &glib::Object) + 'static,
        sorter: Option<gtk::Sorter>,
        expand: bool,
        fixed: i32,
        resizable: bool,
    ) -> gtk::ColumnViewColumn {
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(setup);
        factory.connect_bind(bind);
        let col = gtk::ColumnViewColumn::new(Some(title), Some(factory));
        col.set_expand(expand);
        if fixed != 0 {
            // fixed_width is the STARTING width; resizable lets the user drag
            // the header edge from there.
            col.set_fixed_width(fixed);
        }
        col.set_resizable(resizable);
        if let Some(s) = sorter {
            col.set_sorter(Some(&s));
        }
        self.view.append_column(&col);
        col
    }

    /// `_build_columns`: Name, Version, Source and the action buttons.
    pub fn build_columns(&self) {
        fn num_sorter(attr: fn(&AppItem) -> i64) -> gtk::Sorter {
            gtk::CustomSorter::new(move |a, b| match (AppItem::of(a), AppItem::of(b)) {
                (Some(a), Some(b)) => attr(&a).cmp(&attr(&b)).into(),
                _ => gtk::Ordering::Equal,
            })
            .upcast()
        }

        fn str_sorter(attr: fn(&AppItem) -> &str) -> gtk::Sorter {
            gtk::CustomSorter::new(move |a, b| {
                let (Some(a), Some(b)) = (AppItem::of(a), AppItem::of(b)) else {
                    return gtk::Ordering::Equal;
                };
                let (x, y) = (attr(&a), attr(&b));
                // Blanks last in either direction: an empty category is missing
                // data, not a value that deserves the top of the list.
                if x.is_empty() && !y.is_empty() {
                    return gtk::Ordering::Larger;
                }
                if !x.is_empty() && y.is_empty() {
                    return gtk::Ordering::Smaller;
                }
                lower_cmp(x, y).into()
            })
            .upcast()
        }

        // --- Name: icon + name + description ---
        let setup_name = |_f: &gtk::SignalListItemFactory, obj: &glib::Object| {
            let Some(item) = obj.downcast_ref::<gtk::ListItem>() else { return };
            let row = gtk::Box::builder().spacing(8).margin_top(4).margin_bottom(4).build();
            let img = gtk::Image::new();
            img.set_pixel_size(32);
            img.set_valign(gtk::Align::Center);
            let text = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .valign(gtk::Align::Center)
                .hexpand(true)
                .build();
            let title_row = gtk::Box::builder().spacing(6).build();
            let name_lbl = gtk::Label::builder().xalign(0.0).hexpand(true).build();
            name_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
            let pill = gtk::Label::builder().hexpand(false).build();
            pill.set_halign(gtk::Align::Start);
            pill.add_css_class("pill");
            pill.set_valign(gtk::Align::Center);
            pill.set_visible(false);
            title_row.append(&name_lbl);
            title_row.append(&pill);
            let desc = gtk::Label::builder().xalign(0.0).build();
            desc.add_css_class("dim-label");
            desc.add_css_class("caption");
            desc.set_ellipsize(gtk::pango::EllipsizeMode::End);
            text.append(&title_row);
            text.append(&desc);
            row.append(&img);
            row.append(&text);
            item.set_child(Some(&row));
        };

        let w = self.downgrade();
        let bind_name = move |_f: &gtk::SignalListItemFactory, obj: &glib::Object| {
            let Some(item) = obj.downcast_ref::<gtk::ListItem>() else { return };
            let Some(it) = item.item().and_then(|o| AppItem::of(&o).map(|i| i.clone())) else {
                return;
            };
            let Some(row) = item.child() else { return };
            let Some(img) = row.first_child().and_downcast::<gtk::Image>() else { return };
            let Some(text) = img.next_sibling() else { return };
            let Some(title_row) = text.first_child() else { return };
            let Some(desc_lbl) = title_row.next_sibling().and_downcast::<gtk::Label>() else {
                return;
            };
            let Some(name_lbl) = title_row.first_child().and_downcast::<gtk::Label>() else {
                return;
            };
            let Some(pill) = name_lbl.next_sibling().and_downcast::<gtk::Label>() else { return };

            name_lbl.set_text(&it.name);
            let installed = if it.installed_ts != 0.0 {
                format!("installed {}", age(it.installed_ts))
            } else {
                String::new()
            };
            let bits: Vec<String> = [it.category.clone(), fmt_size(it.size_mb), installed]
                .into_iter()
                .filter(|b| !b.is_empty())
                .collect();
            let desc: String = it.app.description.chars().take(70).collect();
            let meta = bits.join(" · ");
            desc_lbl.set_text(&if !desc.is_empty() && !meta.is_empty() {
                format!("{desc}  —  {meta}")
            } else if !desc.is_empty() {
                desc
            } else {
                meta
            });
            // Only ever shown on a row that actually has an update pending --
            // labelling an up-to-date browser "security" would be noise.
            for css in ["pill-crit", "pill-warn", "pill-ok"] {
                pill.remove_css_class(css);
            }
            if it.git_dirty {
                pill.set_text("local edits");
                pill.add_css_class("pill-warn");
                pill.set_tooltip_text(Some(&tr(
                    "This checkout has uncommitted changes. Pulling upstream \
                     would overwrite them — commit or stash first.",
                )));
                pill.set_visible(true);
            } else if it.source == inventory::SYSTEM && it.has_update {
                pill.set_text("system");
                pill.add_css_class("pill-warn");
                pill.set_tooltip_text(Some(&tr(
                    "Omarchy itself — update with the whole system (Update all), never on its own",
                )));
                pill.set_visible(true);
            } else if it.source == inventory::ORPHAN {
                let keep = it.advice == "keep";
                pill.set_text(if keep { "keep" } else { "safe to remove" });
                pill.add_css_class("pill-warn");
                pill.set_tooltip_text(Some(&if keep {
                    tr("Build tooling or a debug package — the next AUR build will want it back")
                } else {
                    tr("Nothing requires this any more")
                }));
                pill.set_visible(true);
            } else if !it.stale.is_empty() && it.critical.is_empty() {
                let (label, tip) = match it.stale.as_str() {
                    "orphaned" => (
                        "unmaintained",
                        "No maintainer in the AUR — nobody is packaging this any more".to_string(),
                    ),
                    "flagged" => (
                        "out of date",
                        "The AUR maintainer has flagged this as out of date".to_string(),
                    ),
                    "stale" => (
                        "stale",
                        format!(
                            "No new build in over {} months — upstream may have stopped",
                            inventory::STALE_DAYS.div_euclid(30)
                        ),
                    ),
                    // The Python's dict lookup raises on anything else; an
                    // unknown reason shows no pill rather than a crash.
                    _ => {
                        pill.set_visible(false);
                        return;
                    }
                };
                pill.set_text(label);
                pill.add_css_class("pill-warn");
                pill.set_tooltip_text(Some(&tip));
                pill.set_visible(true);
            } else if it.critical == "security" {
                pill.set_text("security");
                pill.set_tooltip_text(Some(&tr(
                    "Network-facing or credential-handling: a stale version is exposure",
                )));
                pill.set_visible(true);
            } else if it.critical == "core" {
                pill.set_text("core");
                pill.add_css_class("pill-warn");
                pill.set_tooltip_text(Some(&tr(
                    "Touches boot or needs a reboot — update with the full system, not alone",
                )));
                pill.set_visible(true);
            } else {
                pill.set_visible(false);
            }

            img.remove_css_class("dim-label");
            if let Some(w) = w.upgrade() {
                if !w.app_icon(&img, &it.app, 32) {
                    img.add_css_class("dim-label");
                }
            }
        };

        self.column("Name", setup_name, bind_name, Some(str_sorter(|i| &i.name)), true, 0, true);

        // --- plain text columns ---
        fn make_text(
            getter: fn(&AppItem) -> String,
            dim: bool,
            align: f32,
            accent_on_update: bool,
        ) -> (
            impl Fn(&gtk::SignalListItemFactory, &glib::Object) + 'static,
            impl Fn(&gtk::SignalListItemFactory, &glib::Object) + 'static,
        ) {
            let setup = move |_f: &gtk::SignalListItemFactory, obj: &glib::Object| {
                let Some(item) = obj.downcast_ref::<gtk::ListItem>() else { return };
                let lbl = gtk::Label::builder().xalign(align).build();
                lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
                lbl.add_css_class("caption");
                item.set_child(Some(&lbl));
            };
            let bind = move |_f: &gtk::SignalListItemFactory, obj: &glib::Object| {
                let Some(item) = obj.downcast_ref::<gtk::ListItem>() else { return };
                let Some(lbl) = item.child().and_downcast::<gtk::Label>() else { return };
                let Some((text, has_update)) = item
                    .item()
                    .and_then(|o| AppItem::of(&o).map(|it| (getter(&it), it.has_update)))
                else {
                    return;
                };
                lbl.set_text(&text);
                lbl.remove_css_class("accent");
                lbl.remove_css_class("dim-label");
                if accent_on_update && has_update {
                    // The one colour in the table: a pending update is the most
                    // actionable fact in the row.
                    lbl.add_css_class("accent");
                } else if dim {
                    lbl.add_css_class("dim-label");
                }
            };
            (setup, bind)
        }

        let (s, b) = make_text(version_text, true, 1.0, true);
        self.column("Version", s, b, Some(num_sorter(|i| i.behind)), false, 170, true);

        // Rows are recycled as the list scrolls, so a stale handler would
        // fire for whichever item later lands in this widget. Python
        // disconnects and reconnects on every bind; here each button has one
        // handler that runs whatever its current slot holds.
        let slots = ClickSlots::default();

        let setup_source = {
            let slots = slots.clone();
            move |_f: &gtk::SignalListItemFactory, obj: &glib::Object| {
                let Some(item) = obj.downcast_ref::<gtk::ListItem>() else { return };
                // A LinkButton would underline permanently and steal the row's
                // styling; a flat button reads as a label until hovered.
                let btn = gtk::Button::new();
                btn.add_css_class("flat");
                btn.add_css_class("caption");
                btn.set_halign(gtk::Align::Start);
                btn.set_valign(gtk::Align::Center);
                slots.attach(&btn);
                item.set_child(Some(&btn));
            }
        };

        let w = self.downgrade();
        let bind_source = {
            let slots = slots.clone();
            move |_f: &gtk::SignalListItemFactory, obj: &glib::Object| {
                let Some(item) = obj.downcast_ref::<gtk::ListItem>() else { return };
                let Some(btn) = item.child().and_downcast::<gtk::Button>() else { return };
                let Some((source, url)) = item.item().and_then(|o| {
                    AppItem::of(&o).map(|it| (it.source.clone(), as_text(it.app.extra.get("url"))))
                }) else {
                    return;
                };
                btn.set_label(&source_label(&source));
                slots.set(&btn, None);
                if !url.is_empty() {
                    btn.set_tooltip_text(Some(&format!("Open {url}")));
                    btn.set_sensitive(true);
                    btn.add_css_class("link-ish");
                    let w = w.clone();
                    slots.set(
                        &btn,
                        Some(Rc::new(move |_b: &gtk::Button| {
                            if let Some(w) = w.upgrade() {
                                w.open_url(&url);
                            }
                        })),
                    );
                } else {
                    // Local builds and AppImages often have nowhere to point.
                    btn.set_tooltip_text(Some(&tr("No upstream URL recorded")));
                    btn.set_sensitive(false);
                    btn.remove_css_class("link-ish");
                }
            }
        };

        self.column("Source", setup_source, bind_source, Some(str_sorter(|i| &i.source)), false, 74, true);

        // --- launch button ---
        let setup_act = {
            let slots = slots.clone();
            move |_f: &gtk::SignalListItemFactory, obj: &glib::Object| {
                let Some(item) = obj.downcast_ref::<gtk::ListItem>() else { return };
                let row = gtk::Box::builder()
                    .spacing(2)
                    .halign(gtk::Align::End)
                    .valign(gtk::Align::Center)
                    .build();
                let upd = gtk::Button::from_icon_name("software-update-available-symbolic");
                upd.add_css_class("flat");
                upd.add_css_class("circular");
                let play = gtk::Button::from_icon_name("media-playback-start-symbolic");
                play.add_css_class("flat");
                let menu = gtk::MenuButton::builder().icon_name("view-more-symbolic").build();
                menu.add_css_class("flat");
                slots.attach(&upd);
                slots.attach(&play);
                row.append(&upd);
                row.append(&play);
                row.append(&menu);
                item.set_child(Some(&row));
            }
        };

        let w = self.downgrade();
        let bind_act = {
            let slots = slots.clone();
            move |_f: &gtk::SignalListItemFactory, obj: &glib::Object| {
                let Some(item) = obj.downcast_ref::<gtk::ListItem>() else { return };
                let Some(it) = item.item().and_then(|o| AppItem::of(&o).map(|i| i.clone())) else {
                    return;
                };
                let Some(row) = item.child() else { return };
                let Some(upd) = row.first_child().and_downcast::<gtk::Button>() else { return };
                let Some(play) = upd.next_sibling().and_downcast::<gtk::Button>() else { return };
                let Some(menu) = play.next_sibling().and_downcast::<gtk::MenuButton>() else {
                    return;
                };
                let Some(win) = w.upgrade() else { return };
                name(&menu, &format!("More actions for {}", it.name));

                // Rows are recycled as the list scrolls, so a stale handler would
                // fire for whichever item later lands in this widget.
                slots.set(&upd, None);
                slots.set(&play, None);

                let extra = &it.app.extra;
                if it.source == inventory::AVAILABLE || it.source == inventory::PLUGIN {
                    let already = truthy(extra.get("already"));
                    upd.set_visible(!already);
                    play.set_visible(false);
                    menu.set_visible(truthy(extra.get("url")));
                    if !already {
                        upd.remove_css_class("error");
                        upd.add_css_class("accent");
                        upd.set_icon_name(INSTALL_ICON);
                        let suffix = if it.source == inventory::PLUGIN {
                            tr(" (Omarchy plugin)")
                        } else {
                            String::new()
                        };
                        name(&upd, &format!("Install {}{}", it.name, suffix));
                        let (w, app) = (w.clone(), it.app.clone());
                        slots.set(
                            &upd,
                            Some(Rc::new(move |_b: &gtk::Button| {
                                if let Some(w) = w.upgrade() {
                                    w.on_install_item(&app);
                                }
                            })),
                        );
                    }
                    if truthy(extra.get("url")) {
                        let m = gio::Menu::new();
                        m.append(Some("Open repository"), Some(&format!("row.visit::{}", it.name)));
                        menu.set_menu_model(Some(&m));
                        win.shared.borrow_mut().row_actions.insert(it.name.clone(), it.app.clone());
                    }
                    return;
                }

                upd.set_icon_name("software-update-available-symbolic");
                let mut can_update = it.has_update
                    && [inventory::REPO, inventory::AUR, inventory::APPIMAGE, inventory::TOOL]
                        .contains(&it.source.as_str());
                if it.source == inventory::SYSTEM {
                    // Almanac is a SYSTEM entry but is the one thing here that
                    // can genuinely update itself.
                    can_update = truthy(extra.get("self")) && it.has_update;
                } else if it.source == inventory::LOCAL {
                    // Never offer a pull over uncommitted work.
                    can_update = it.git_behind != 0 && !it.git_dirty;
                }
                upd.set_visible(can_update);
                if can_update {
                    name(
                        &upd,
                        &format!(
                            "Update {} alone, in a terminal.\nArch discourages partial upgrades: \
                             prefer Update all unless you need just this one.",
                            it.name
                        ),
                    );
                    // Colour only where it earns attention: a security update is
                    // worth a red glyph, routine ones stay quiet.
                    upd.remove_css_class("error");
                    upd.remove_css_class("accent");
                    upd.add_css_class(if it.critical == "security" { "error" } else { "accent" });
                    let (w, app) = (w.clone(), it.app.clone());
                    let handler: Rc<dyn Fn(&gtk::Button)> = if truthy(extra.get("self")) {
                        name(
                            &upd,
                            &format!("Update Almanac itself — {}", as_text(extra.get("note"))),
                        );
                        Rc::new(move |_b| {
                            if let Some(w) = w.upgrade() {
                                w.on_self_update();
                            }
                        })
                    } else if it.source == inventory::LOCAL {
                        let pid = as_text(extra.get("plugin_id"));
                        name(
                            &upd,
                            &if truthy(extra.get("plugin_id")) {
                                format!("omarchy plugin update {pid}")
                            } else {
                                format!(
                                    "git pull in {}\nLatest upstream: {}",
                                    as_text(extra.get("repo")),
                                    as_text(extra.get("git_subject"))
                                )
                            },
                        );
                        Rc::new(move |_b| {
                            if let Some(w) = w.upgrade() {
                                w.on_git_pull(&app);
                            }
                        })
                    } else if it.source == inventory::APPIMAGE {
                        name(
                            &upd,
                            &format!(
                                "Download {} {} and replace the current build (the old one is \
                                 kept as .previous)",
                                it.name, it.app.update
                            ),
                        );
                        Rc::new(move |b| {
                            if let Some(w) = w.upgrade() {
                                w.on_update_appimage(b, &app);
                            }
                        })
                    } else {
                        Rc::new(move |_b| {
                            if let Some(w) = w.upgrade() {
                                w.on_update_one(&app);
                            }
                        })
                    };
                    slots.set(&upd, Some(handler));
                }

                // extra.get("present", True)
                let present = extra.get("present").map(|v| truthy(Some(v))).unwrap_or(true);
                let missing = it.source == inventory::APPIMAGE && !present;
                let launchable = !it.app.exec_path.is_empty()
                    && (it.source == inventory::APPIMAGE || it.source == inventory::LOCAL);
                play.set_visible(launchable && !missing);
                if play.is_visible() {
                    name(&play, &format!("Launch {}", it.name));
                    let (w, app) = (w.clone(), it.app.clone());
                    slots.set(
                        &play,
                        Some(Rc::new(move |_b: &gtk::Button| {
                            if let Some(w) = w.upgrade() {
                                w.on_launch(&app);
                            }
                        })),
                    );
                }

                // Destructive actions sit behind a menu, never a bare button where
                // the pointer already is.
                let model = gio::Menu::new();
                let url = as_text(extra.get("url"));
                let n = &it.name;
                if !url.is_empty() {
                    model.append(Some("Open project page"), Some(&format!("row.visit::{n}")));
                }
                if it.has_update && url.contains("github.com/") {
                    model.append(Some("What’s new…"), Some(&format!("row.notes::{n}")));
                } else if it.source == inventory::AUR {
                    model.append(Some("View on the AUR"), Some(&format!("row.aur::{n}")));
                }
                if [inventory::REPO, inventory::AUR, inventory::APPIMAGE, inventory::FLATPAK]
                    .contains(&it.source.as_str())
                {
                    model.append(Some("Remove…"), Some(&format!("row.remove::{n}")));
                }

                // A local checkout with uncommitted work gets no pull button,
                // because the pull would be refused. Without these it got no
                // affordance at all and no explanation -- a row 209 commits
                // behind that simply does nothing.
                if it.source == inventory::LOCAL && truthy(extra.get("repo")) {
                    let pid = truthy(extra.get("plugin_id"));
                    let enabled = extra.get("enabled").filter(|v| !v.is_null());
                    if pid && enabled.is_some() {
                        model.append(
                            Some(&if truthy(enabled) { tr("Turn off") } else { tr("Turn on") }),
                            Some(&format!("row.toggle::{n}")),
                        );
                        // What has_panel raises ends this bind: the row
                        // keeps whatever menu it had.
                        let panel = crate::shortcuts::available()
                            && match crate::shortcuts::try_has_panel(std::path::Path::new(&as_text(
                                extra.get("repo"),
                            ))) {
                                Ok(panel) => panel,
                                Err(e) => {
                                    eprintln!("almanac: {e}");
                                    return;
                                }
                            };
                        if panel {
                            model.append(
                                Some(&tr("Keyboard shortcut…")),
                                Some(&format!("row.shortcut::{n}")),
                            );
                        }
                    }
                    model.append(Some("Open repository folder"), Some(&format!("row.folder::{n}")));
                    if it.git_dirty {
                        model.append(Some("Show local changes…"), Some(&format!("row.changes::{n}")));
                        if it.git_behind != 0 {
                            model.append(
                                Some("Stash changes and update…"),
                                Some(&format!("row.stash::{n}")),
                            );
                        }
                    } else if it.git_behind != 0 {
                        let label = if pid { tr("Update") } else { "Update (git pull)".to_string() };
                        model.append(Some(&label), Some(&format!("row.pull::{n}")));
                    }
                }

                let has_items = model.n_items() > 0;
                menu.set_menu_model(if has_items { Some(&model) } else { None });
                menu.set_visible(has_items);
                win.shared.borrow_mut().row_actions.insert(it.name.clone(), it.app.clone());
            }
        };

        self.column("", setup_act, bind_act, None, false, 104, false);
    }
}

/// Click handlers for recycled buttons: one permanent handler per button,
/// running whatever closure the button's current row put in its slot.
type ClickFn = Rc<dyn Fn(&gtk::Button)>;

#[derive(Clone, Default)]
struct ClickSlots(Rc<RefCell<HashMap<usize, Rc<RefCell<Option<ClickFn>>>>>>);

impl ClickSlots {
    fn key(btn: &gtk::Button) -> usize {
        btn.as_ptr() as usize
    }

    /// Give a freshly made button its slot and its one handler.
    fn attach(&self, btn: &gtk::Button) {
        let slot: Rc<RefCell<Option<ClickFn>>> = Rc::default();
        let mine = slot.clone();
        btn.connect_clicked(move |b| {
            let f = mine.borrow().clone();
            if let Some(f) = f {
                f(b);
            }
        });
        self.0.borrow_mut().insert(ClickSlots::key(btn), slot);
        let table = Rc::downgrade(&self.0);
        btn.connect_destroy(move |b| {
            if let Some(t) = table.upgrade() {
                t.borrow_mut().remove(&ClickSlots::key(b));
            }
        });
    }

    /// `btn.disconnect(btn._handler)` / `btn._handler = btn.connect(...)`.
    fn set(&self, btn: &gtk::Button, f: Option<ClickFn>) {
        let slot = self.0.borrow().get(&ClickSlots::key(btn)).cloned();
        if let Some(slot) = slot {
            *slot.borrow_mut() = f;
        }
    }
}
