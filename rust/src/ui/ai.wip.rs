//! UI section `ai` (port of `app.py` lines 4486-4876): the AI page, plus the
//! shared page-shape helpers that live among those lines (`status_page`,
//! `labelled`, `section`) and the icon helpers used by every list.

use std::path::{Path, PathBuf};

use adw::prelude::*;
use gtk::{glib, pango};

use crate::ai::{self, Entry, Group};
use crate::i18n::tr;
use crate::inventory::{self, App};
use crate::tasks;
use crate::ui::shell::{self, Win};

#[derive(Default)]
pub struct State {
    pub ai_page: Option<gtk::Box>,
    pub ai_pending: i64,
}

#[derive(Clone)]
pub struct Section {
    pub root: gtk::Box,
    pub rows: gtk::ListBox,
}

pub const AI_SEVERITY: &[(&str, (&str, &str))] = &[
    ("warn", ("Attention", "pill-warn")),
    ("info", ("Optional", "pill")),
    ("ok", ("", "")),
];

const SET_ICON_SIZE: i32 = 48;

fn extra_str(app: &App, key: &str) -> String {
    app.extra
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}

impl Win {
    // ---- the AI estate ----------------------------------------------------

    /// Harnesses, MCP servers, skills, plugins, marketplaces and hooks.
    ///
    /// Only the harnesses have versions, so only they belong in the package
    /// table. The rest is still software that rots -- an MCP server that
    /// stopped authenticating, a hook whose script was deleted, a marketplace
    /// three weeks stale -- and this is where that shows.
    pub fn build_ai(&self) -> gtk::Widget {
        let page = gtk::Box::new(gtk::Orientation::Vertical, 0);

        let ai_page = gtk::Box::new(gtk::Orientation::Vertical, 12);
        ai_page.set_margin_start(12);
        ai_page.set_margin_end(12);
        ai_page.set_margin_bottom(16);
        let scroller = gtk::ScrolledWindow::new();
        scroller.set_vexpand(true);
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroller.set_child(Some(&ai_page));
        page.append(&scroller);
        self.ai.borrow_mut().ai_page = Some(ai_page);
        page.upcast()
    }

    /// What a screen says when it has nothing to show.
    ///
    /// A blank list is indistinguishable from a broken one. Every empty
    /// screen here names which of the three it is -- nothing to do, nothing
    /// matched, or nothing could be loaded -- and offers the way out.
    pub fn status_page(
        icon: &str,
        title: &str,
        body: &str,
        action: Option<&gtk::Widget>,
    ) -> adw::StatusPage {
        let page = adw::StatusPage::builder()
            .icon_name(icon)
            .title(title)
            .description(body)
            .build();
        page.set_vexpand(true);
        if let Some(action) = action {
            page.set_child(Some(action));
        }
        page
    }

    /// A control with its name above it, for the filter drawer.
    pub fn labelled(label: &str, widget: &impl IsA<gtk::Widget>) -> gtk::Box {
        let bx = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let cap = gtk::Label::new(Some(label));
        cap.set_xalign(0.0);
        cap.add_css_class("caption");
        cap.add_css_class("dim-label");
        bx.append(&cap);
        widget.set_hexpand(true);
        bx.append(widget);
        bx
    }

    pub fn store_reset_filters(&self) {
        let (cat, trust, market, sort, verified, installed) = {
            let s = self.shared.borrow();
            (
                s.store_cat.clone(),
                s.store_trust.clone(),
                s.store_market.clone(),
                s.store_sort.clone(),
                s.store_verified.clone(),
                s.store_installed.clone(),
            )
        };
        for widget in [cat, trust, market].into_iter().flatten() {
            widget.set_selected(0);
        }
        if let Some(sort) = sort {
            sort.set_selected(0);
        }
        if let Some(verified) = verified {
            verified.set_active(false);
        }
        if let Some(installed) = installed {
            installed.set_active(false);
        }
        self.search.set_text("");
        self.store_refresh();
    }

    /// One titled, full-width list. The shared shape for secondary pages.
    pub fn section(title: &str, description: &str, trailing: Option<&gtk::Widget>) -> Section {
        let bx = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let head = gtk::Label::new(Some(title));
        head.set_xalign(0.0);
        head.set_hexpand(true);
        head.set_visible(!title.is_empty());
        head.add_css_class("heading");
        if let Some(trailing) = trailing {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            row.append(&head);
            row.append(trailing);
            bx.append(&row);
        } else {
            bx.append(&head);
        }
        if !description.is_empty() {
            // What the section is, a hover away rather than a line per section:
            // on a full page those lines were a fifth of the height.
            head.set_tooltip_text(Some(description));
            head.update_property(&[gtk::accessible::Property::Description(description)]);
        }
        let rows = gtk::ListBox::new();
        rows.set_selection_mode(gtk::SelectionMode::None);
        rows.add_css_class("boxed-list");
        bx.append(&rows);
        Section { root: bx, rows }
    }

    /// Render each group as it arrives rather than waiting for all six.
    ///
    /// Checking MCP health means opening a connection to every configured
    /// server, which takes ten to twenty seconds on a cold cache. The skills
    /// and hooks are read from disk in milliseconds and should not be held
    /// behind it.
    pub fn ai_reload(&self) {
        self.busy(true);
        self.summarize(&tr("Reading the AI setup…"), Some(inventory::AGENT));

        if crate::demo::active() {
            // One shot, no threads: the fixtures are already in memory.
            self.ai_show(&ai::survey(), true);
            self.busy(false);
            return;
        }

        let specs = ai::group_specs();
        let survey: Vec<Group> = specs
            .iter()
            .map(|(title, desc, _fn)| (title.to_string(), desc.to_string(), Vec::new()))
            .collect();
        self.shared.borrow_mut().ai_survey = survey.clone();
        self.ai.borrow_mut().ai_pending = specs.len() as i64;
        self.ai_show(&survey, false);

        for (index, (title, _desc, f)) in specs.into_iter().enumerate() {
            let w = self.clone();
            let t = title.to_string();
            self.thumb_submit(
                move || ai::load_group(f, &t),
                move |entries| {
                    w.ai_group_done(index, title, entries);
                },
            );
        }
    }

    pub fn ai_group_done(&self, index: usize, _title: &str, entries: Vec<Entry>) -> glib::ControlFlow {
        let survey = {
            let mut s = self.shared.borrow_mut();
            if index < s.ai_survey.len() {
                s.ai_survey[index].2 = entries;
            }
            s.ai_survey.clone()
        };
        let pending = {
            let mut st = self.ai.borrow_mut();
            st.ai_pending -= 1;
            st.ai_pending
        };
        if pending <= 0 {
            self.busy(false);
        }
        self.ai_show(&survey, pending <= 0);
        glib::ControlFlow::Break
    }

    pub fn ai_show(&self, groups: &[Group], final_: bool) -> glib::ControlFlow {
        // Keep what was rendered: revisiting the page re-renders from this
        // rather than reloading, and an unrecorded result showed as empty.
        if final_ {
            self.shared.borrow_mut().ai_survey = groups.to_vec();
        }
        let ai_page = self.ai.borrow().ai_page.clone();
        if let Some(ai_page) = &ai_page {
            while let Some(child) = ai_page.first_child() {
                ai_page.remove(&child);
            }
        }

        let loading = !final_;
        for (title, desc, entries) in groups {
            let pending = loading && entries.is_empty();
            let section = Win::section(
                &if pending {
                    format!("{title} …")
                } else {
                    format!("{title} ({})", entries.len())
                },
                desc,
                None,
            );
            if pending {
                let row: adw::ActionRow = Win::plain(&tr("Checking…"), "");
                #[allow(deprecated)]
                let spinner = gtk::Spinner::new();
                spinner.set_valign(gtk::Align::Center);
                #[allow(deprecated)]
                spinner.start();
                row.add_prefix(&spinner);
                section.rows.append(&row);
            } else if entries.is_empty() {
                let row: adw::ActionRow =
                    Win::plain(&tr("None"), &tr("Nothing of this kind is set up."));
                section.rows.append(&row);
            }
            // Trouble first: a broken hook among thirty healthy skills is the
            // whole reason to open this page.
            let order = |sev: &str| match sev {
                "warn" => 0,
                "info" => 1,
                _ => 2,
            };
            let mut sorted: Vec<&Entry> = entries.iter().collect();
            sorted.sort_by_key(|e| (order(&e.severity), e.name.to_lowercase()));
            for e in sorted {
                section.rows.append(&self.ai_row(e));
            }
            if let Some(ai_page) = &ai_page {
                ai_page.append(&section.root);
            }
        }

        self.sync_primary();
        let total: usize = groups.iter().map(|(_t, _d, es)| es.len()).sum();
        let attention = ai::needs_attention(groups);
        if loading {
            self.summarize(&format!("{total} found so far…"), Some(inventory::AGENT));
        } else {
            self.summarize(
                &if attention > 0 {
                    format!("{total} items across {} kinds · {attention} need attention", groups.len())
                } else {
                    format!("{total} items across {} kinds · all healthy", groups.len())
                },
                Some(inventory::AGENT),
            );
        }

        let count = self
            .shared
            .borrow()
            .sidebar_row(inventory::AGENT)
            .map(|r| r.count.clone());
        if let Some(count) = count {
            if final_ {
                Win::badge(
                    &count,
                    if attention > 0 { attention } else { total },
                    if attention > 0 { "warn" } else { "" },
                );
            }
        }
        glib::ControlFlow::Break
    }

    pub fn ai_row(&self, e: &Entry) -> adw::ActionRow {
        let (w1, w2) = (self.downgrade(), self.downgrade());
        Win::ai_row_with(
            e,
            |art, e| self.vendor_icon(art, e),
            move |u| {
                if let Some(w) = w1.upgrade() {
                    w.open_url(u);
                }
            },
            move |e| {
                if let Some(w) = w2.upgrade() {
                    w.ai_act(e);
                }
            },
        )
    }

    pub fn ai_row_with(
        e: &Entry,
        vendor_icon: impl FnOnce(&gtk::Image, &Entry),
        open_url: impl Fn(&str) + 'static,
        ai_act: impl Fn(&Entry) + 'static,
    ) -> adw::ActionRow {
        let mut subtitle = e.detail.clone();
        if !e.version.is_empty() {
            subtitle = e.version.clone();
            if !e.update.is_empty() {
                subtitle.push_str(&format!(" → {}", e.update));
            }
            if !e.detail.is_empty() {
                subtitle.push_str(&format!(" · {}", e.detail));
            }
        }
        let row: adw::ActionRow = Win::plain(&e.name, &subtitle);
        row.set_subtitle_lines(1);
        row.set_tooltip_text(Some(&subtitle));
        row.set_subtitle_selectable(true);

        // The vendor's own mark, fetched from the vendor. Falls back to a
        // symbolic icon immediately and swaps in the real one when it arrives,
        // so a slow or missing favicon never holds up the row.
        let art = gtk::Image::new();
        art.set_pixel_size(22);
        art.set_valign(gtk::Align::Center);
        art.set_icon_name(Some(&e.fallback_icon));
        art.add_css_class("store-shot");
        row.add_prefix(&art);
        vendor_icon(&art, e);

        let (label, css) = AI_SEVERITY
            .iter()
            .find(|(k, _)| *k == e.severity)
            .map(|(_, v)| *v)
            .unwrap_or(("", ""));
        if !label.is_empty() {
            let pill = gtk::Label::new(Some(label));
            pill.set_valign(gtk::Align::Center);
            pill.add_css_class(css);
            row.add_prefix(&pill);
        }

        if !e.status.is_empty() {
            // A status can be a whole server error ("Failed to connect — HTTP
            // 429: Error POSTing to endpoint: {…}"). Unbounded, one such
            // label asked for 724px, pushed the page wider than the window
            // and squeezed the sidebar to its minimum. Short, ellipsized,
            // with the full text a hover away.
            let st = gtk::Label::new(Some(&e.status));
            st.set_valign(gtk::Align::Center);
            st.set_ellipsize(pango::EllipsizeMode::End);
            st.set_max_width_chars(28);
            st.set_xalign(1.0);
            st.set_tooltip_text(Some(&e.status));
            st.update_property(&[gtk::accessible::Property::Description(&e.status)]);
            st.add_css_class("dim-label");
            st.add_css_class("caption");
            row.add_suffix(&st);
        }

        if !e.url.is_empty() {
            let link = gtk::Button::from_icon_name("adw-external-link-symbolic");
            link.set_valign(gtk::Align::Center);
            link.add_css_class("flat");
            shell::name(&link, &format!("Open {}", e.url));
            let u = e.url.clone();
            link.connect_clicked(move |_b| open_url(&u));
            row.add_suffix(&link);
        }

        if !e.action_label.is_empty() && e.action.is_some() {
            let btn = gtk::Button::with_label(&e.action_label);
            btn.set_valign(gtk::Align::Center);
            if e.severity == "warn" {
                btn.add_css_class("suggested-action");
            }
            let e = e.clone();
            btn.connect_clicked(move |_b| ai_act(&e));
            row.add_suffix(&btn);
        } else if !e.action_note.is_empty() && e.action.is_none() && !e.action_label.is_empty() {
            // There is something to do, but not something this app can do.
            let hint = gtk::Label::new(Some(&tr("manual")));
            hint.set_valign(gtk::Align::Center);
            hint.add_css_class("dim-label");
            hint.add_css_class("caption");
            hint.set_tooltip_text(Some(&e.action_note));
            row.add_suffix(&hint);
        }
        row
    }

    /// The best icon available for an inventory row.
    ///
    /// Order: the .desktop icon, then the vendor's own mark where we know
    /// their domain, then a per-source glyph. Agents have no .desktop entry,
    /// so without the middle step Claude Code and Codex rendered as the same
    /// grey circle as an unidentifiable package.
    ///
    /// Returns False when it fell through to the glyph, so the caller can
    /// dim it.
    pub fn app_icon(&self, image: &gtk::Image, app: &App, size: i32) -> bool {
        let icon = extra_str(app, "icon");
        let theme = gtk::IconTheme::for_display(&WidgetExt::display(&self.window));
        if !icon.is_empty() && theme.has_icon(&icon) {
            image.set_icon_name(Some(&icon));
            return true;
        }
        if !icon.is_empty() && Path::new(&icon).is_file() {
            image.set_from_file(Some(&icon));
            return true;
        }

        let (key, domain, url) = Win::icon_source(app);
        if !key.is_empty() {
            if let Some(hit) = crate::icons::cached(&key) {
                self.set_icon(image, &hit, size);
                return true;
            }
            // Not fetched yet: show the glyph now, swap it in when it lands.
            image.set_icon_name(Some(
                shell::fallback_icon(&app.source).unwrap_or("package-x-generic"),
            ));
            let w = self.clone();
            let image = image.clone();
            self.thumb_submit(
                move || crate::icons::fetch(&key, &url, &domain),
                move |path| {
                    w.icon_arrived(&image, size, path);
                },
            );
            return true;
        }

        image.set_icon_name(Some(
            shell::fallback_icon(&app.source).unwrap_or("package-x-generic"),
        ));
        false
    }

    /// (cache key, domain, explicit url) for an entry's vendor mark.
    ///
    /// pacman already records every package's upstream homepage, so most of
    /// the list can show the project's own logo instead of a generic box.
    /// Projects hosted on GitHub are the exception worth special-casing: the
    /// site favicon would put the same octocat on half the list, while the
    /// owner's avatar is the thing people actually recognise.
    pub fn icon_source(app: &App) -> (String, String, String) {
        let domain = extra_str(app, "domain");
        if !domain.is_empty() {
            return (domain.clone(), domain, String::new());
        }

        // Local builds and plugins record their clone URL rather than a
        // homepage; Flatpaks are named after their vendor's domain in
        // reverse. Both are an upstream identity, which is all this needs.
        let mut url = extra_str(app, "url");
        if url.is_empty() {
            url = extra_str(app, "repo_url");
        }
        let appid = extra_str(app, "appid");
        if url.is_empty() && !appid.is_empty() {
            let parts: Vec<&str> = appid.split('.').collect();
            if parts.len() >= 2 {
                let d = format!("{}.{}", parts[1], parts[0]);
                return (d.clone(), d, String::new());
            }
        }
        let host = crate::icons::host_of(&url);
        if host.is_empty() {
            return (String::new(), String::new(), String::new());
        }
        if host.ends_with("github.com") {
            let parts: Vec<&str> = url.split('/').filter(|x| !x.is_empty()).collect();
            let owner = parts.get(2).copied().unwrap_or("");
            if !owner.is_empty() && owner != "sponsors" && owner != "orgs" {
                return (
                    format!("gh-{}", owner.to_lowercase()),
                    String::new(),
                    crate::icons::github_avatar(owner),
                );
            }
            return (String::new(), String::new(), String::new());
        }
        if host.ends_with("gitlab.com") || host.ends_with("codeberg.org") {
            return (crate::icons::registrable(&host), host, String::new());
        }
        (crate::icons::registrable(&host), host, String::new())
    }

    pub fn icon_arrived(&self, image: &gtk::Image, size: i32, path: Option<PathBuf>) -> glib::ControlFlow {
        if let Some(path) = path {
            self.set_icon(image, &path, size);
        }
        glib::ControlFlow::Break
    }

    pub fn vendor_icon(&self, image: &gtk::Image, e: &Entry) {
        if !(!e.icon_key.is_empty() && (!e.icon_domain.is_empty() || !e.icon_url.is_empty())) {
            return;
        }
        if let Some(hit) = crate::icons::cached(&e.icon_key) {
            self.set_icon(image, &hit, SET_ICON_SIZE);
            return;
        }

        let w = self.clone();
        let image = image.clone();
        let done = move |path: Option<PathBuf>| {
            if let Some(path) = path {
                w.set_icon(&image, &path, SET_ICON_SIZE);
            }
        };
        let (key, url, domain) = (e.icon_key.clone(), e.icon_url.clone(), e.icon_domain.clone());
        self.thumb_submit(move || crate::icons::fetch(&key, &url, &domain), done);
    }

    pub fn set_icon(&self, image: &gtk::Image, path: &Path, size: i32) {
        if let Some(tex) = self.texture(path, size) {
            image.set_paintable(Some(&tex));
        }
    }

    pub fn ai_act(&self, e: &Entry) {
        let cmd = e.action.clone();
        if let Some(cmd) = cmd.as_ref().filter(|_| {
            e.kind == ai::HARNESS
                && !e.update.is_empty()
                && crate::agents::major_jump(&e.version, &e.update)
        }) {
            // Same treatment as from the Updates list: notes first.
            let spec = crate::agents::KNOWN.iter().find(|k| k.name == e.name);
            let mut app = App {
                name: e.name.clone(),
                version: e.version.clone(),
                source: inventory::AGENT.to_string(),
                update: e.update.clone(),
                ..Default::default()
            };
            app.extra.insert("update_cmd".into(), serde_json::json!(cmd));
            app.extra.insert("manager".into(), serde_json::json!(""));
            app.extra.insert(
                "repo".into(),
                serde_json::json!(spec.map(|s| s.repo.clone()).unwrap_or_default()),
            );
            app.extra.insert(
                "binary".into(),
                serde_json::json!(spec.map(|s| s.id.clone()).unwrap_or_default()),
            );
            let manager = if cmd.first().map(String::as_str) == Some("mise") { "mise" } else { "self" };
            app.extra.insert("manager".into(), serde_json::json!(manager));
            if manager == "mise" {
                if let Some(spec) = spec {
                    app.extra.insert("key".into(), serde_json::json!(spec.id));
                }
            }
            self.update_agent(&app);
            return;
        }
        let mut detail = e.action_note.clone();
        if let Some(cmd) = &cmd {
            detail.push_str(&format!("\n\n$ {}", cmd.join(" ")));
        }
        let cmd = cmd.unwrap_or_default();
        let w = self.downgrade();
        let label = format!("{} {}", e.action_label, e.name);
        tasks::confirm(
            &self.window,
            &format!("{} {}?", e.action_label, e.name),
            &e.detail,
            py_strip(&detail),
            &e.action_label,
            false,
            move || {
                let Some(win) = w.upgrade() else { return };
                let w2 = win.downgrade();
                tasks::run(
                    &win.window,
                    &label,
                    vec![tasks::Step::argv(label.clone(), cmd.clone())],
                    Some(Box::new(move |ok| {
                        if let Some(w) = w2.upgrade() {
                            w.ai_after(ok);
                        }
                    })),
                );
            },
        );
    }

    // `self._thumb_pool.submit(lambda: GLib.idle_add(done, work()))`.
    fn thumb_submit<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
        done: impl FnOnce(T) + 'static,
    ) {
        let done = glib::thread_guard::ThreadGuard::new(done);
        let job = move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));
            glib::idle_add_once(move || {
                let done = done.into_inner();
                if let Ok(value) = result {
                    done(value);
                }
            });
        };
        let slot = std::sync::Arc::new(std::sync::Mutex::new(Some(job)));
        let theirs = slot.clone();
        let pushed = self.thumb_pool.as_ref().is_some_and(|pool| {
            pool.push(move || {
                if let Some(job) = theirs.lock().ok().and_then(|mut j| j.take()) {
                    job();
                }
            })
            .is_ok()
        });
        if !pushed {
            if let Some(job) = slot.lock().ok().and_then(|mut j| j.take()) {
                std::thread::spawn(job);
            }
        }
    }

    pub fn ai_after(&self, ok: bool) -> glib::ControlFlow {
        self.ai_reload();
        if ok {
            self.reload();
        }
        glib::ControlFlow::Break
    }

    pub fn ai_update_all(&self) {
        let survey = self.shared.borrow().ai_survey.clone();
        let upd: Vec<Entry> = survey
            .into_iter()
            .flat_map(|(_t, _d, es)| es)
            .filter(|e| {
                e.kind == ai::HARNESS
                    && !e.update.is_empty()
                    && e.action.as_ref().is_some_and(|a| !a.is_empty())
            })
            .collect();
        // Major releases are updated one at a time, with their notes shown.
        let (held, upd): (Vec<Entry>, Vec<Entry>) = upd
            .into_iter()
            .partition(|e| crate::agents::hold_major(&e.version, &e.update));
        if !held.is_empty() {
            self.say(
                &(tr("Held back (major release, update it on its own): ")
                    + &held
                        .iter()
                        .map(|e| format!("{} {}", e.name, e.update))
                        .collect::<Vec<_>>()
                        .join(", ")),
                8,
            );
        }
        if upd.is_empty() {
            return;
        }
        let steps: Vec<tasks::Step> = upd
            .iter()
            .map(|e| {
                tasks::Step::argv(format!("Updating {}", e.name), e.action.clone().unwrap_or_default())
            })
            .collect();
        let detail = upd
            .iter()
            .map(|e| {
                format!(
                    "{:22} {} → {}\n  $ {}",
                    e.name,
                    e.version,
                    e.update,
                    e.action.clone().unwrap_or_default().join(" ")
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let w = self.downgrade();
        tasks::confirm(
            &self.window,
            &format!("Update {} agent CLI(s)?", upd.len()),
            &tr("Each replaces its own binary. None of this needs root, and none of it touches your agent configuration."),
            &detail,
            &tr("Update"),
            false,
            move || {
                let Some(win) = w.upgrade() else { return };
                let w2 = win.downgrade();
                tasks::run(
                    &win.window,
                    "Updating agent CLIs",
                    steps.clone(),
                    Some(Box::new(move |ok| {
                        if let Some(w) = w2.upgrade() {
                            w.ai_after(ok);
                        }
                    })),
                );
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[cfg(feature = "wip-ui-ai")]
    const SRC: &str = include_str!("ai.wip.rs");
    #[cfg(not(feature = "wip-ui-ai"))]
    const SRC: &str = include_str!("ai.rs");
    const CSS: &str = include_str!("../../data/style.css");

    /// This file without its tests, so the inspections see only real code.
    fn code() -> &'static str {
        SRC.split("#[cfg(test)]").next().unwrap_or(SRC)
    }

    fn ints_after(src: &str, needle: &str) -> Vec<i64> {
        let mut out = Vec::new();
        for (i, _) in src.match_indices(needle) {
            let rest = &src[i + needle.len()..];
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(n) = digits.parse() {
                out.push(n);
            }
        }
        out
    }

    /// test_spacing_comes_from_the_scale, for this section.
    #[test]
    fn spacing_comes_from_the_scale() {
        let scale: BTreeSet<i64> = [0, 2, 4, 6, 8, 12, 16, 20, 24].into_iter().collect();
        let src = code();
        let mut found = Vec::new();
        for needle in [
            "set_margin_start(", "set_margin_end(", "set_margin_top(", "set_margin_bottom(",
            "set_spacing(", "Orientation::Vertical, ", "Orientation::Horizontal, ",
        ] {
            found.extend(ints_after(src, needle));
        }
        assert!(!found.is_empty());
        let off: Vec<i64> = found.into_iter().filter(|n| !scale.contains(n)).collect();
        assert!(off.is_empty(), "spacing off the scale: {off:?}");
    }

    /// test_every_pill_class_is_defined_in_the_stylesheet /
    /// test_badges_use_one_pill_system, for the severities shown here.
    #[test]
    fn every_pill_class_is_defined_in_the_stylesheet() {
        let pills: BTreeSet<&str> =
            ["pill", "pill-crit", "pill-warn", "pill-ok", "pill-accent", "pill-quiet"].into();
        for (_sev, (_label, css)) in AI_SEVERITY {
            if css.is_empty() {
                continue;
            }
            assert!(pills.contains(css), "unknown pill variant {css}");
            assert!(CSS.contains(&format!(".{css}")), "{css} is applied but never styled");
        }
        for retired in ["crit-pill", "stale-pill", "store-chip", "trust-ok", "trust-warn"] {
            assert!(!code().contains(&format!("\"{retired}\"")));
        }
    }

    /// test_every_icon_name_is_in_adwaita_or_shipped_here, for this file.
    #[test]
    fn every_icon_name_is_in_adwaita_or_shipped_here() {
        let adwaita = Path::new("/usr/share/icons/Adwaita");
        if !adwaita.is_dir() {
            return; // Adwaita icon theme not installed
        }
        fn walk(dir: &Path, have: &mut BTreeSet<String>) {
            let Ok(rd) = std::fs::read_dir(dir) else { return };
            for ent in rd.flatten() {
                let p = ent.path();
                if p.is_dir() {
                    walk(&p, have);
                } else if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    have.insert(stem.to_string());
                }
            }
        }
        let mut have = BTreeSet::new();
        walk(adwaita, &mut have);
        walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../icons"), &mut have);
        let src = code();
        let mut missing = Vec::new();
        for part in src.split('"').skip(1).step_by(2) {
            let is_name = part.ends_with("-symbolic")
                && part.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
            // adw-* icons ship inside libadwaita itself.
            if is_name && !part.starts_with("adw-") && !have.contains(part) {
                missing.push(part.to_string());
            }
        }
        assert!(missing.is_empty(), "missing icons: {missing:?}");
    }

    /// test_every_icon_only_button_is_named, for this file.
    #[test]
    fn every_icon_only_button_is_named() {
        let lines: Vec<&str> = code().lines().collect();
        let mut unnamed = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            let Some(pos) = line.find("= gtk::Button::from_icon_name(") else { continue };
            let var = line[..pos].trim().trim_start_matches("let ").trim();
            let window = lines[i..(i + 12).min(lines.len())].join("\n");
            if !window.contains(&format!("shell::name(&{var}"))
                && !window.contains("Property::Label")
            {
                unnamed.push(format!("ai.rs:{} {var}", i + 1));
            }
        }
        assert!(unnamed.is_empty(), "{unnamed:?}");
    }

    /// One unbounded status label asked for 724px and squeezed the
    /// sidebar to its minimum whenever an MCP server was failing.
    #[test]
    fn a_long_server_error_does_not_widen_the_ai_page() {
        // A package build runs check() as another user, with no display
        // to reach; building a widget there crashes rather than fails.
        // (GTK also refuses a second initialising thread; skip then too.)
        if !matches!(std::panic::catch_unwind(adw::init), Ok(Ok(()))) {
            return; // no display
        }
        let e = Entry {
            kind: ai::MCP.into(),
            name: "A server".into(),
            status: format!(
                "Failed to connect — HTTP 429: Error POSTing to endpoint: {}",
                "{\"code\":\"rate_limited\"} ".repeat(12)
            ),
            severity: "warn".into(),
            url: "https://example.org".into(),
            ..Default::default()
        };
        let row = Win::ai_row_with(&e, |_art, _e| {}, |_u| {}, |_e| {});
        let (minimum, ..) = row.measure(gtk::Orientation::Horizontal, -1);
        assert!(minimum < 480, "the window's own minimum width ({minimum})");
    }

    #[test]
    fn icon_source_prefers_domain_then_reverse_appid() {
        let mut app = App::new("x", "1", "pacman");
        app.extra.insert("appid".into(), serde_json::json!("org.gnome.Foo"));
        assert_eq!(Win::icon_source(&app), ("gnome.org".into(), "gnome.org".into(), String::new()));
        app.extra.insert("domain".into(), serde_json::json!("example.com"));
        assert_eq!(Win::icon_source(&app).0, "example.com");
        let empty = App::new("y", "1", "pacman");
        assert_eq!(Win::icon_source(&empty), (String::new(), String::new(), String::new()));
    }
}
