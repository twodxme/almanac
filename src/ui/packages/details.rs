//! The details sheet: everything known about one app, and what can be done.

use adw::prelude::*;
use serde_json::Value;

use crate::i18n::tr;
use crate::inventory;
use crate::ui::shell::{AppItem, Win, name, source_label, spawn_bg};

use super::decisions::{available_label, detail_badges_for, squeeze};
use super::extra::{DetailValue, local_time, py_format_thousands, py_repr, py_repr_str, py_str, timestamp_of, truthy};
use super::PlainRow;

impl Win {
    pub fn on_row_activated(&self, _view: &gtk::ColumnView, position: u32) {
        let sorted = self.shared.borrow().sorted.clone();
        let item = sorted.item(position).and_then(|o| AppItem::of(&o).map(|it| it.clone()));
        if let Some(item) = item {
            self.show_details(&item);
        }
    }

    /// A card for one app: everything known about it, and what can be done.
    ///
    /// Details are fetched on open rather than during the scan -- dependency
    /// lists are a pacman call each, which would make the list slow for data
    /// nobody reads until they ask for it.
    ///
    /// Those pacman calls (`-Qi`, `-Qlq`) run off the main thread; the card
    /// opens when they answer.
    pub fn show_details(&self, item: &AppItem) {
        let w = self.downgrade();
        let item = item.clone();
        let app = item.app.clone();
        spawn_bg(
            move || inventory::try_details(&app),
            move |extra| {
                let Some(w) = w.upgrade() else { return };
                // An entry whose details Python could not read (a missing
                // advisory field, a timestamp that is not a number) raised
                // out of the click handler: the traceback went to stderr
                // and no dialog opened.
                if let Err(e) = extra.and_then(|extra| w.build_details(&item, extra)) {
                    eprintln!("almanac: cannot show details for {}: {e}", item.app.name);
                }
            },
        );
    }

    fn build_details(&self, item: &AppItem, extra: inventory::Extra) -> Result<(), String> {
        let app = &item.app;
        let has = |key: &str| truthy(extra.get(key));

        let page = adw::PreferencesPage::new();

        // --- header: icon, name, what it is ---
        let hero = adw::PreferencesGroup::new();
        let head = gtk::Box::builder().spacing(12).margin_bottom(4).build();
        let img = gtk::Image::new();
        img.set_pixel_size(64);
        self.app_icon(&img, &item.app, 128);
        head.append(&img);

        let titles = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .valign(gtk::Align::Center)
            .spacing(2)
            .build();
        let t = gtk::Label::builder().xalign(0.0).label(&app.name).build();
        t.add_css_class("title-2");
        titles.append(&t);
        if !app.description.is_empty() {
            let d = gtk::Label::builder().xalign(0.0).label(&app.description).wrap(true).build();
            d.add_css_class("dim-label");
            titles.append(&d);
        }
        let badges = gtk::Box::builder().spacing(6).margin_top(4).build();
        for (text, css) in self.detail_badges(item) {
            let b = gtk::Label::new(Some(&text));
            b.add_css_class(css);
            badges.append(&b);
        }
        if badges.first_child().is_some() {
            titles.append(&badges);
        }
        head.append(&titles);
        hero.add(&head);
        page.add(&hero);

        // --- version ---
        let vg = adw::PreferencesGroup::builder().title(tr("Version")).build();
        Win::detail_row(&vg, &tr("Installed"), &app.version);
        if !app.update.is_empty() {
            Win::detail_row(&vg, &tr("Available"), &available_label(item));
        }
        if has("published") {
            Win::detail_row(&vg, &tr("Released"), &extra["published"]);
        }
        if has("git_subject") {
            Win::detail_row(&vg, &tr("Latest upstream commit"), &extra["git_subject"]);
        }
        if has("plugin_detail") {
            Win::detail_row(&vg, &tr("State"), &extra["plugin_detail"]);
            if extra.get("enabled").is_some_and(|v| !v.is_null()) {
                Win::detail_row(
                    &vg,
                    &tr("Switched"),
                    &if truthy(extra.get("enabled")) { tr("On") } else { tr("Off") },
                );
            }
        }
        page.add(&vg);

        // Advisories from the Arch Security Tracker, each linked to its page.
        if has("vulns") {
            let sg = adw::PreferencesGroup::builder()
                .title(tr("Known vulnerabilities"))
                .description(tr("From the Arch Security Tracker, for the installed version."))
                .build();
            // `for v in extra["vulns"]` with `v["cves"]`, `v["fixed"]` and
            // `v['group']`: anything but a list of dicts, or a dict missing a
            // key, raised before the dialog opened.
            let vulns = match &extra["vulns"] {
                Value::Array(a) => a.clone(),
                other => return Err(format!("TypeError: vulns is not a list: {}", py_repr(other))),
            };
            for v in &vulns {
                let Value::Object(v) = v else {
                    return Err(format!("TypeError: advisory is not a dict: {}", py_repr(v)));
                };
                let get = |k: &str| v.get(k).ok_or_else(|| format!("KeyError: {}", py_repr_str(k)));
                // ", ".join(v["cves"][:4]): a list of strings, or a string,
                // whose characters are then joined.
                // Only the four that are joined have to be strings; len()
                // counts them all.
                let (cve_list, cve_n): (Vec<String>, usize) = match get("cves")? {
                    Value::Array(a) => (
                        a.iter()
                        .take(4)
                        .map(|c| match c {
                            Value::String(s) => Ok(s.clone()),
                            other => Err(format!(
                                "TypeError: sequence item: expected str instance, got {}",
                                py_repr(other)
                            )),
                        })
                        .collect::<Result<_, _>>()?,
                        a.len(),
                    ),
                    Value::String(s) => {
                        (s.chars().take(4).map(String::from).collect(), s.chars().count())
                    }
                    other => return Err(format!("TypeError: cves cannot be sliced: {}", py_repr(other))),
                };
                let cves = cve_list.join(", ") + if cve_n > 4 { "…" } else { "" };
                let fixed = get("fixed")?;
                let fix = if truthy(Some(fixed)) {
                    format!("fixed in {}", py_str(fixed))
                } else {
                    "no fix released yet".to_string()
                };
                let group = py_str(get("group")?);
                let (severity, kind) = (py_str(get("severity")?), py_str(get("kind")?));
                let row: adw::ActionRow = Win::plain(
                    &format!("{group} · {severity} · {kind}"),
                    &format!("{fix} · {cves}"),
                );
                row.set_subtitle_lines(2);
                let link = gtk::Button::builder()
                    .icon_name("adw-external-link-symbolic")
                    .valign(gtk::Align::Center)
                    .tooltip_text(tr("Open the advisory"))
                    .build();
                link.add_css_class("flat");
                link.update_property(&[gtk::accessible::Property::Label(&format!(
                    "Open advisory {group}"
                ))]);
                let w = self.downgrade();
                link.connect_clicked(move |_b| {
                    if let Some(w) = w.upgrade() {
                        w.open_url(&format!("https://security.archlinux.org/{group}"));
                    }
                });
                row.add_suffix(&link);
                sg.add(&row);
            }
            page.add(&sg);
        }

        // --- where it came from ---
        let og = adw::PreferencesGroup::builder().title(tr("Origin")).build();
        Win::detail_row(&og, &tr("Source"), &source_label(&item.source));
        for (key, label) in [
            ("category", "Category"),
            ("size", "Installed size"),
            ("license", "Licence"),
            ("Packager", "Packager"),
            ("Architecture", "Architecture"),
            ("repo", "Checkout"),
        ] {
            let val = if has(key) {
                extra[key].clone()
            } else if key == "category" {
                Value::String(item.category.clone())
            } else {
                Value::String(String::new())
            };
            if truthy(Some(&val)) {
                Win::detail_row(&og, label, &py_str(&val));
            }
        }
        if item.installed_ts != 0.0 {
            Win::detail_row(&og, &tr("Installed"), &local_time(item.installed_ts, "%d %b %Y, %H:%M")?);
        }
        if has("built_ts") {
            Win::detail_row(&og, &tr("Built"), &local_time(timestamp_of(extra.get("built_ts"))?, "%d %b %Y")?);
        }
        if has("files") {
            Win::detail_row(&og, &tr("Files"), &py_format_thousands(&extra["files"])?);
        }
        if has("url") {
            // subtitle=extra["url"]: a GObject string property takes only a str.
            let url = match &extra["url"] {
                Value::String(u) => u.clone(),
                other => return Err(format!("TypeError: url must be a str: {}", py_repr(other))),
            };
            // Plain text: a query string's `&` is not markup.
            let r: adw::ActionRow = Win::plain(&tr("Project page"), &url);
            r.set_subtitle_selectable(true);
            let btn = gtk::Button::builder()
                .icon_name("adw-external-link-symbolic")
                .valign(gtk::Align::Center)
                .build();
            btn.add_css_class("flat");
            name(&btn, &format!("Open {url}"));
            let w = self.downgrade();
            btn.connect_clicked(move |_b| {
                if let Some(w) = w.upgrade() {
                    w.open_url(&url);
                }
            });
            r.add_suffix(&btn);
            og.add(&r);
        }
        page.add(&og);

        // --- relationships ---
        let deps = adw::PreferencesGroup::builder().title(tr("Dependencies")).build();
        let mut added = false;
        for (key, label) in [
            ("Depends On", "Requires"),
            ("Required By", "Required by"),
            ("Optional Deps", "Optional"),
            ("Provides", "Provides"),
            ("Conflicts With", "Conflicts with"),
        ] {
            if has(key) {
                // extra[key].split(): only a str has one.
                let Value::String(text) = &extra[key] else {
                    return Err(format!("AttributeError: {key} has no attribute 'split'"));
                };
                // Plain text: package relations carry version constraints
                // (`glibc>=2.40`), and a `<` or `&` read as markup would
                // blank the row.
                let row: adw::ActionRow = Win::plain(label, &squeeze(text, 400));
                row.set_subtitle_lines(4);
                row.set_subtitle_selectable(true);
                deps.add(&row);
                added = true;
            }
        }
        if added {
            page.add(&deps);
        }

        let dialog = adw::Dialog::new();
        dialog.set_title(&app.name);
        dialog.set_content_width(620);
        dialog.set_content_height(720);

        let tv = adw::ToolbarView::new();
        let hb = adw::HeaderBar::new();

        if !app.update.is_empty() && item.source != inventory::SYSTEM {
            let up = gtk::Button::with_label(&tr("Update"));
            up.add_css_class("suggested-action");
            let (w, d, it) = (self.downgrade(), dialog.downgrade(), item.clone());
            up.connect_clicked(move |_b| {
                if let Some(d) = d.upgrade() {
                    d.close();
                }
                if let Some(w) = w.upgrade() {
                    w.update_from_detail(&it);
                }
            });
            hb.pack_end(&up);
        }
        tv.add_top_bar(&hb);

        let scroll = gtk::ScrolledWindow::builder().vexpand(true).build();
        scroll.set_child(Some(&page));
        tv.set_content(Some(&scroll));
        dialog.set_child(Some(&tv));
        dialog.present(Some(&self.window));
        Ok(())
    }

    pub fn detail_badges(&self, item: &AppItem) -> Vec<(String, &'static str)> {
        detail_badges_for(item)
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
        Win::plain_with(title, subtitle, |_row: &R| {})
    }

    /// A note above a page's content (the Updates page's, History's last
    /// 24 hours): plain text, wrapped in full, behind a dim 24px icon. One
    /// helper so the pages' notes cannot drift apart.
    pub fn note_row(icon: &str, title: &str, body: &str) -> adw::ActionRow {
        let row: adw::ActionRow = Win::plain(title, body);
        row.set_subtitle_lines(0);
        let image = gtk::Image::builder().icon_name(icon).pixel_size(24).valign(gtk::Align::Center).build();
        image.add_css_class("dim-label");
        image.set_accessible_role(gtk::AccessibleRole::Presentation);
        row.add_prefix(&image);
        row
    }

    /// A boxed list for `note_row`s.
    pub fn note_list() -> gtk::ListBox {
        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        list
    }

    /// `_plain(cls, title, subtitle, **kwargs)`: `kwargs` stands in for the
    /// constructor keywords (`activatable=True`, ...). It runs on the new row
    /// before anything else, as the constructor applied them.
    pub fn plain_with<R: PlainRow>(title: &str, subtitle: &str, kwargs: impl FnOnce(&R)) -> R {
        let row = R::new_plain();
        kwargs(&row);
        row.set_use_markup(false);
        if !title.is_empty() {
            row.set_title(title);
        }
        if !subtitle.is_empty() {
            row.put_subtitle(subtitle);
        }
        row
    }

    pub fn detail_row<V: DetailValue + ?Sized>(group: &adw::PreferencesGroup, title: &str, value: &V) {
        // A row reading "Reviewed by: (blank)" says less than no row at all,
        // and the provenance group has several fields that only some entries
        // carry.
        let Some(value) = value.detail_text() else {
            return;
        };
        let row: adw::ActionRow = Win::plain(title, &value);
        row.set_subtitle_selectable(true);
        group.add(&row);
    }

    pub fn update_from_detail(&self, item: &AppItem) {
        if item.source == inventory::APPIMAGE {
            self.on_update_appimage(&gtk::Button::new(), &item.app);
        } else if item.source == inventory::LOCAL {
            self.on_git_pull(&item.app);
        } else {
            self.on_update_one(&item.app);
        }
    }
}
