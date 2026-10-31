//! Arch news and "restart to finish updating", where they are needed: on
//! Today above "Needs you", above the Updates list while a system update is
//! waiting, and once before Update all.
//!
//! The news is read off the main thread (`news::unread_now`, cached for an
//! hour) and kept here, so the Updates list and Update all can ask without
//! waiting on the network.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::i18n::tr;
use crate::news::{self, NewsItem};
use crate::restart;
use crate::ui::shell::{spawn_bg, AppItem, Win};
use crate::ui::update_all::{route, Route};

/// How many headlines a card shows; the rest are a click away on the site.
const SHOWN: usize = 3;
const NEWS_PAGE: &str = "https://archlinux.org/news/";

thread_local! {
    /// The unread news as last read; None until the first read finishes.
    static UNREAD: RefCell<Option<Vec<NewsItem>>> = const { RefCell::new(None) };
    static LOADING: Cell<bool> = const { Cell::new(false) };
    /// Headlines already shown before an Update all, and updated past: not
    /// asked about again this session.
    static ASKED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    /// Today's slot for the news card and the restart banner.
    static TODAY: RefCell<Option<glib::WeakRef<gtk::Box>>> = const { RefCell::new(None) };
}

fn unread() -> Vec<NewsItem> {
    UNREAD.with(|u| u.borrow().clone().unwrap_or_default())
}

/// The headlines to ask about before Update all: unread ones not already
/// updated past this session.
fn to_ask(items: &[NewsItem], asked: &[String]) -> Vec<NewsItem> {
    items
        .iter()
        .filter(|i| !asked.contains(&i.title))
        .take(SHOWN)
        .cloned()
        .collect()
}

/// "1 more unread post" / "4 more unread posts".
fn more_news_title(more: usize) -> String {
    crate::i18n::ngettext("{n} more unread post", "{n} more unread posts", more as u64)
        .replace("{n}", &more.to_string())
}

/// The line under a headline: its date, then the start of the post.
fn subtitle(item: &NewsItem) -> String {
    match (item.date(), item.summary.is_empty()) {
        (d, true) => d,
        (d, false) if d.is_empty() => item.summary.clone(),
        (d, false) => format!("{d} · {}", item.summary),
    }
}

impl Win {
    /// Read the news (and whether a restart is needed) again, then show both
    /// wherever they are shown.
    pub fn news_reload(&self) {
        if LOADING.with(|l| l.replace(true)) {
            return;
        }
        let w = self.downgrade();
        spawn_bg(
            || (news::unread_now(), restart::reboot_needed()),
            move |(items, reboot)| {
                LOADING.with(|l| l.set(false));
                UNREAD.with(|u| *u.borrow_mut() = Some(items));
                let Some(w) = w.upgrade() else { return };
                w.news_show_today(reboot);
                let (filter_key, query) = {
                    let sh = w.shared.borrow();
                    (sh.filter_key.clone(), sh.query.clone())
                };
                w.refresh_update_notes(&filter_key, &query);
            },
        );
    }

    /// Today: a slot above "Needs you" for the restart banner and the news
    /// card, filled when the read finishes.
    pub fn news_today(&self, page: &gtk::Box) {
        let slot = gtk::Box::new(gtk::Orientation::Vertical, 12);
        slot.set_visible(false);
        page.append(&slot);
        TODAY.with(|t| *t.borrow_mut() = Some(slot.downgrade()));
        self.news_reload();
    }

    fn news_show_today(&self, reboot: Option<String>) {
        let Some(slot) = TODAY.with(|t| t.borrow().as_ref().and_then(|w| w.upgrade())) else {
            return;
        };
        while let Some(child) = slot.first_child() {
            slot.remove(&child);
        }
        if let Some(why) = reboot {
            let list = Win::note_list();
            list.append(&self.reboot_row(&why));
            slot.append(&list);
        }
        let items = unread();
        if !items.is_empty() {
            let more = (items.len() > SHOWN).then(|| self.all_news_button());
            let section = Win::section(
                &format!("{} ({})", tr("Arch news"), items.len()),
                &tr(
                    "Announcements from archlinux.org you have not read. Updates that need manual \
                     steps are announced here and nowhere else.",
                ),
                more.as_ref().map(|b| b.upcast_ref::<gtk::Widget>()),
            );
            for item in items.iter().take(SHOWN) {
                section.rows.append(&self.news_row(item));
            }
            slot.append(&section.root);
        }
        slot.set_visible(slot.first_child().is_some());
    }

    /// "All news": the rest of the headlines, on archlinux.org.
    fn all_news_button(&self) -> gtk::Button {
        let b = gtk::Button::builder()
            .label(tr("All news"))
            .valign(gtk::Align::Center)
            .build();
        b.add_css_class("flat");
        b.set_tooltip_text(Some(NEWS_PAGE));
        let w = self.downgrade();
        b.connect_clicked(move |_| {
            if let Some(w) = w.upgrade() {
                w.open_url(NEWS_PAGE);
            }
        });
        b
    }

    /// One headline, with Read and Mark as read.
    fn news_row(&self, item: &NewsItem) -> adw::ActionRow {
        let row: adw::ActionRow = Win::plain(&item.title, &subtitle(item));
        row.set_subtitle_lines(2);
        let icon = gtk::Image::builder()
            .icon_name("dialog-information-symbolic")
            .pixel_size(24)
            .valign(gtk::Align::Center)
            .build();
        icon.add_css_class("dim-label");
        icon.set_accessible_role(gtk::AccessibleRole::Presentation);
        row.add_prefix(&icon);

        let read = gtk::Button::builder()
            .label(tr("Read"))
            .valign(gtk::Align::Center)
            .build();
        read.set_tooltip_text(Some(&tr("Open the post on archlinux.org")));
        read.update_property(&[gtk::accessible::Property::Label(&format!(
            "{} {}",
            tr("Read"),
            item.title
        ))]);
        let link = if item.link.is_empty() {
            NEWS_PAGE.to_string()
        } else {
            item.link.clone()
        };
        let w = self.downgrade();
        read.connect_clicked(move |_| {
            if let Some(w) = w.upgrade() {
                w.open_url(&link);
            }
        });
        row.add_suffix(&read);

        let mark = gtk::Button::builder()
            .label(tr("Mark as read"))
            .valign(gtk::Align::Center)
            .build();
        mark.add_css_class("flat");
        mark.set_tooltip_text(Some(&tr(
            "Marks this read here and for the scheduled update, which holds updates while \
             Arch news is unread",
        )));
        mark.update_property(&[gtk::accessible::Property::Label(&format!(
            "{} {}",
            tr("Mark as read"),
            item.title
        ))]);
        let w = self.downgrade();
        let title = item.title.clone();
        mark.connect_clicked(move |_| {
            if let Some(w) = w.upgrade() {
                w.news_mark_read(vec![title.clone()]);
            }
        });
        row.add_suffix(&mark);
        row
    }

    /// Record `titles` as read in the shared seen-file, then show what is
    /// left.
    pub fn news_mark_read(&self, titles: Vec<String>) {
        let w = self.downgrade();
        spawn_bg(
            move || news::mark_read(&titles).map_err(|e| e.to_string()),
            move |r| {
                let Some(w) = w.upgrade() else { return };
                if let Err(e) = r {
                    w.say(&format!("Could not mark the news read: {e}"), 5);
                }
                w.news_reload();
            },
        );
    }

    /// The news rows above the Updates list: only while a system update is
    /// waiting, which is when the news matters. Returns how many were added.
    pub fn news_into(&self, list: &gtk::ListBox) -> usize {
        let system_pending = self
            .store_objects()
            .iter()
            .filter_map(AppItem::of)
            .any(|i| i.has_update && route(&i.app) == Route::System);
        if !system_pending || !news::enabled() {
            return 0;
        }
        if UNREAD.with(|u| u.borrow().is_none()) {
            self.news_reload();
            return 0;
        }
        let items = unread();
        for item in items.iter().take(SHOWN) {
            list.append(&self.news_row(item));
        }
        let mut added = items.len().min(SHOWN);
        // The rest are a click away, as on Today.
        if items.len() > SHOWN {
            list.append(&self.more_news_row(items.len() - SHOWN));
            added += 1;
        }
        added
    }

    /// Below the headlines the Updates list shows: how many more are
    /// unread, and "All news".
    fn more_news_row(&self, more: usize) -> adw::ActionRow {
        let row: adw::ActionRow =
            Win::plain(&more_news_title(more), &tr("Read them on archlinux.org"));
        row.add_suffix(&self.all_news_button());
        row
    }

    /// Before Update all: when there is unread Arch news, ask once whether to
    /// read it first. Returns true when it has taken over, and `then` runs
    /// later if the person chooses to update anyway; false when there is
    /// nothing to ask and the caller goes ahead now.
    pub fn confirm_news_then(&self, then: impl Fn(&Win) + 'static) -> bool {
        self.confirm_news(Rc::new(then))
    }

    fn confirm_news(&self, then: Rc<dyn Fn(&Win)>) -> bool {
        if !news::enabled() {
            return false;
        }
        let known = UNREAD.with(|u| u.borrow().clone());
        let Some(items) = known else {
            // Not read yet this session: read it (at most a 10 s fetch), then
            // decide.
            self.say(&tr("Checking Arch news…"), 3);
            let w = self.downgrade();
            spawn_bg(news::unread_now, move |items| {
                UNREAD.with(|u| *u.borrow_mut() = Some(items));
                if let Some(w) = w.upgrade() {
                    // Read now: ask, or go straight on when there is nothing
                    // to ask.
                    if !w.confirm_news(then.clone()) {
                        then(&w);
                    }
                }
            });
            return true;
        };
        let ask = ASKED.with(|a| to_ask(&items, &a.borrow()));
        if ask.is_empty() {
            return false;
        }
        let body = tr(
            "Arch announces updates that need manual steps on its news page. Read \
                       these before updating:",
        );
        let detail: Vec<String> = ask
            .iter()
            .map(|i| format!("{} — {}", i.date(), i.title))
            .collect();
        // Unread recent posts: what was read before the last update does not
        // show, and what came out before it but was never read does.
        let dialog = adw::AlertDialog::new(Some(&tr("Unread Arch news")), Some(&body));
        let text = gtk::Label::builder()
            .label(detail.join("\n"))
            .xalign(0.0)
            .wrap(true)
            .selectable(true)
            .build();
        dialog.set_extra_child(Some(&text));
        dialog.add_response("cancel", &tr("Cancel"));
        dialog.add_response("read", &tr("Read news"));
        dialog.add_response("update", &tr("Update anyway"));
        dialog.set_response_appearance("read", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("read"));
        dialog.set_close_response("cancel");
        let w = self.downgrade();
        dialog.connect_response(None, move |_d, resp| {
            let Some(w) = w.upgrade() else { return };
            match resp {
                "read" => {
                    let link = match ask.as_slice() {
                        [one] if !one.link.is_empty() => one.link.clone(),
                        _ => NEWS_PAGE.to_string(),
                    };
                    w.open_url(&link);
                }
                "update" => {
                    ASKED.with(|a| a.borrow_mut().extend(ask.iter().map(|i| i.title.clone())));
                    then(&w);
                }
                _ => {}
            }
        });
        dialog.present(Some(&self.window));
        true
    }

    /// Preferences switched the news on or off.
    pub fn news_setting_changed(&self) {
        UNREAD.with(|u| *u.borrow_mut() = None);
        self.news_reload();
    }

    /// "Restart to finish updating", with a button that asks first.
    fn reboot_row(&self, why: &str) -> adw::ActionRow {
        let row: adw::ActionRow = Win::plain(&tr("Restart to finish updating"), why);
        row.set_subtitle_lines(2);
        let icon = gtk::Image::builder()
            .icon_name("system-reboot-symbolic")
            .pixel_size(24)
            .valign(gtk::Align::Center)
            .build();
        icon.set_accessible_role(gtk::AccessibleRole::Presentation);
        row.add_prefix(&icon);
        let btn = gtk::Button::builder()
            .label(tr("Restart now"))
            .valign(gtk::Align::Center)
            .build();
        btn.set_tooltip_text(Some(&tr("Restart the computer, after asking")));
        let w = self.downgrade();
        let why = why.to_string();
        btn.connect_clicked(move |_| {
            if let Some(w) = w.upgrade() {
                w.confirm_reboot(&why);
            }
        });
        row.add_suffix(&btn);
        row
    }

    fn confirm_reboot(&self, why: &str) {
        let w = self.downgrade();
        crate::tasks::confirm(
            &self.window,
            &tr("Restart now?"),
            &tr("Everything open closes, and unsaved work in any app is lost. Save your work first."),
            why,
            &tr("Restart now"),
            true,
            move || {
                let w = w.clone();
                spawn_bg(restart::reboot_now, move |r| {
                    if let (Err(e), Some(w)) = (r, w.upgrade()) {
                        w.say(&format!("Could not restart: {e}"), 8);
                    }
                });
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(title: &str) -> NewsItem {
        NewsItem {
            title: title.into(),
            published: 1_790_000_000,
            ..Default::default()
        }
    }

    #[test]
    fn update_all_asks_about_a_headline_once_a_session() {
        let items = vec![item("a"), item("b")];
        assert_eq!(to_ask(&items, &[]).len(), 2);
        assert_eq!(to_ask(&items, &["a".into()]), vec![item("b")]);
        assert!(to_ask(&items, &["a".into(), "b".into()]).is_empty());
        let many: Vec<NewsItem> = (0..6).map(|i| item(&i.to_string())).collect();
        assert_eq!(to_ask(&many, &[]).len(), SHOWN);
    }

    #[test]
    fn the_updates_list_says_how_many_more_posts_are_unread() {
        assert_eq!(more_news_title(1), "1 more unread post");
        assert_eq!(more_news_title(4), "4 more unread posts");
    }

    /// The dialog lists unread recent posts, which is not "since your last
    /// update"; and the Updates headlines link to the rest, as Today's do.
    #[test]
    fn the_news_copy_says_what_it_lists() {
        let src = include_str!("news.rs");
        let body = &src[..src.find("#[cfg(test)]").unwrap()];
        assert!(!body.contains("since your last update"));
        let into = &body[body.find("pub fn news_into").unwrap()..];
        let into = &into[..into.find("\n    }\n").unwrap()];
        assert!(into.contains("more_news_row"), "{into}");
    }

    #[test]
    fn a_headline_row_says_when_then_what() {
        let mut i = item("x");
        i.summary = "Remove it first.".into();
        assert!(
            subtitle(&i).ends_with(" · Remove it first."),
            "{}",
            subtitle(&i)
        );
        i.published = i64::MAX;
        assert_eq!(subtitle(&i), "Remove it first.");
    }
}
