//! The Plugins page: state pills, the filter, each row with its logo, menu
//! and switch, and the plugin cache's sanity checks.

use std::collections::BTreeMap;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::i18n::tr;
use crate::ui::actions::Judge;
use crate::ui::shell::{name, spawn_bg, SwitchGuard, WeakWin, Win};
use crate::verify::{Outcome, ProbeValue, TryProbe};
use crate::{
    history, inventory, plugins as plugins_mod, shortcuts, store as store_mod, tasks, util,
};

use super::pyisms::{py_splitlines, py_strip, run_checked, truthy, uncaught, uncaught_err};
use super::LogoChoice;

/// `_PLUGIN_STATE_PILLS`: a plugin state key -> (pill label, css class).
pub const PLUGIN_STATE_PILLS: &[(&str, (&str, &str))] = &[
    ("update", ("Update waiting", "pill-accent")),
    ("modified", ("Local edits", "pill-warn")),
    ("ahead", ("Ahead of upstream", "pill-warn")),
    ("diverged", ("Diverged", "pill-warn")),
    ("detached", ("Not on a branch", "pill-warn")),
    ("symlink", ("Development link", "pill-quiet")),
    ("error", ("Could not check", "pill-warn")),
];

/// `key in _PLUGIN_STATE_PILLS` / `_PLUGIN_STATE_PILLS[key]`.
pub fn plugin_state_pill(key: &str) -> Option<(&'static str, &'static str)> {
    PLUGIN_STATE_PILLS
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| *v)
}

/// The pill key for a plugin state, as `_plugin_row` picks it: the kind for
/// "update" and "error", otherwise the detail ("modified", "symlink", ...).
pub fn plugin_state_key(st: Option<&plugins_mod::State>) -> String {
    match st {
        Some(st) if st.kind == "update" || st.kind == "error" => st.kind.clone(),
        Some(st) => st.detail.clone(),
        None => String::new(),
    }
}

/// Whether a row matches the search text, as `_plugins_filter` decides it.
pub fn plugin_matches(i: &plugins_mod::Installed, text: &str) -> bool {
    let q = py_strip(text).to_lowercase();
    if q.is_empty() {
        return true;
    }
    let hay = [
        i.id.as_str(),
        i.name.as_str(),
        i.description.as_str(),
        &i.kinds.join(" "),
    ]
    .join(" ")
    .to_lowercase();
    hay.contains(&q)
}

/// The row menu `_plugin_row` builds: (label msgid, action name) in order.
/// Split out so the decision can be tested without a display.
///
/// `Err` where `i.has_panel` raises.
pub fn plugin_menu(
    i: &plugins_mod::Installed,
    shortcuts_available: bool,
) -> Result<Vec<(&'static str, &'static str)>, String> {
    let st = i.state.as_ref();
    let mut out = Vec::new();
    if let Some(st) = st {
        if st.updatable() || (st.detail == "modified" && st.behind != 0) {
            out.push(("Update…", "update"));
        }
    }
    if shortcuts_available && i.try_has_panel()? && i.folder.is_some() {
        out.push(("Keyboard shortcut…", "shortcut"));
    }
    if i.folder.is_some() {
        out.push(("Open folder", "folder"));
    }
    if st.is_some_and(|s| !s.url.is_empty()) {
        out.push(("Open project page", "page"));
    }
    if !i.built_in {
        out.push(("Remove…", "remove"));
    }
    Ok(out)
}

/// `f"{i.name} on" if on else f"{i.name} off"`: the switch's accessible name.
pub fn switch_name(plugin_name: &str, on: bool) -> String {
    if on {
        format!("{plugin_name} on")
    } else {
        format!("{plugin_name} off")
    }
}

/// `shortcuts.set_shortcut(pid, binding)`: `None` removes it.
type SetShortcut<'a> = &'a dyn Fn(&str, Option<&str>) -> Result<String, String>;

/// The `_forget_shortcut` step body with the three `shortcuts` calls passed
/// in -- the seam the Python tests reach with `mock.patch.object`.
fn forget_shortcut_with(
    pid: &str,
    log: tasks::LogFn<'_>,
    available: &dyn Fn() -> bool,
    current: &dyn Fn(&str) -> Result<String, String>,
    set_shortcut: SetShortcut<'_>,
) {
    let run = || -> Result<(), String> {
        if available() && !current(pid)?.is_empty() {
            log(&set_shortcut(pid, None)?);
        }
        Ok(())
    };
    if let Err(exc) = run() {
        log(&format!("its shortcut was left in bindings.lua: {exc}"));
    }
}

/// Whether `classify` reached `_remote` for a plugin it gave this state:
/// every answer but the ones it settles from the checkout alone.
fn reached_remote(st: &plugins_mod::State) -> bool {
    !(st.kind == plugins_mod::LOCAL
        || st.detail == "detached"
        || (st.kind == plugins_mod::ERROR && st.detail != "fetch failed"))
}

/// The exception the Python's `_remote` raises when the git cache's
/// `"_plugins"` entry is `other`, neither a dict nor null: `found` and
/// `scratch` are what `installed` gave with an empty cache in its place.
/// A truthy value fails at `.get` the first time `_remote` runs; a falsy one
/// reads as `{}` and fails at the first fetch it stores.
fn foreign_cache_error(
    other: &serde_json::Value,
    found: &[plugins_mod::Installed],
    scratch: &plugins_mod::FetchCache,
) -> Option<(&'static str, String)> {
    use serde_json::Value as V;
    if crate::demo::active() {
        return None; // the demo list is returned before the cache is touched
    }
    let (tname, truthy) = match other {
        V::Bool(b) => ("bool", *b),
        V::Number(n) if n.is_i64() || n.is_u64() => ("int", n.as_f64().is_some_and(|f| f != 0.0)),
        V::Number(n) => ("float", n.as_f64().is_some_and(|f| f != 0.0)),
        V::String(t) => ("str", !t.is_empty()),
        V::Array(a) => ("list", !a.is_empty()),
        V::Null | V::Object(_) => return None,
    };
    if truthy {
        let reached = found
            .iter()
            .filter_map(|i| i.state.as_ref())
            .any(reached_remote);
        reached.then(|| {
            (
                "AttributeError",
                format!("'{tname}' object has no attribute 'get'"),
            )
        })
    } else if !scratch.is_empty() {
        let msg = if tname == "list" {
            "list indices must be integers or slices, not str".to_string()
        } else {
            format!("'{tname}' object does not support item assignment")
        };
        Some(("TypeError", msg))
    } else {
        None
    }
}

/// Switch a plugin through Omarchy, then show what actually holds.
///
/// The switch moves only once Omarchy's own list agrees; a refusal puts
/// it back and says why. `i` is the row's own plugin, updated in place.
fn plugin_switch_shared(
    w: &WeakWin,
    switch: &gtk::Switch,
    on: bool,
    guard: &SwitchGuard,
    i: std::rc::Rc<std::cell::RefCell<plugins_mod::Installed>>,
) -> glib::Propagation {
    if w.upgrade().is_some_and(|w| w.demo_refuses()) {
        // Back where it was, once this handler has returned.
        let (sw, g) = (switch.downgrade(), guard.clone());
        glib::idle_add_local_once(move || {
            if let Some(sw) = sw.upgrade() {
                g.set(&sw, !on);
            }
        });
        return glib::Propagation::Stop;
    }
    switch.set_sensitive(false);

    let id = i.borrow().id.clone();
    let work = move || {
        // A command that could not start or timed out still leaves Omarchy's
        // list to read, so the switch can show what really holds.
        let r = run_checked(&plugins_mod::toggle_argv(&id, on), 60);
        let now = match plugins_mod::try_enabled() {
            Ok(on) => Ok(on.get(&id).copied()),
            Err(e) => {
                uncaught_err(&e);
                Err(())
            }
        };
        (r, now)
    };

    let w = w.clone();
    let switch = switch.clone();
    let guard = guard.clone();
    let done = move |(r, now): (Result<util::Output, String>, Result<Option<bool>, ()>)| {
        let (nm, can_disable, was) = {
            let x = i.borrow();
            (x.name.clone(), x.can_disable, x.enabled)
        };
        // Whatever happened, the switch is usable again.
        switch.set_sensitive(can_disable);
        let win = w.upgrade();
        let word = if on { "on" } else { "off" };
        let Ok(now) = now else {
            // `enabled()` raised (logged above): what holds is unknown, so
            // the switch goes back to what it showed before the flip.
            guard.set(&switch, was);
            name(&switch, &switch_name(&nm, was));
            if let (Err(e), Some(w)) = (&r, &win) {
                w.notify(&format!("{nm} was not switched {word}"), e, "normal");
            }
            return;
        };
        let now_b = now.unwrap_or(false);
        // Quietly: snapping back must not run the toggle again.
        guard.set(&switch, now_b);
        i.borrow_mut().enabled = now_b;
        name(&switch, &switch_name(&nm, now_b));
        if now == Some(on) {
            if let Some(w) = &win {
                w.say(&format!("{nm} {word}"), 5);
            }
            if let Err(e) = history::record_checked(
                history::ALMANAC,
                &format!("{nm} switched {word}"),
                "",
                "",
                &[],
            ) {
                return uncaught_err(&e);
            }
        } else {
            let body = match &r {
                Ok(r) => {
                    let text = if r.stderr.is_empty() {
                        &r.stdout
                    } else {
                        &r.stderr
                    };
                    let why: Vec<&str> = py_splitlines(py_strip(text));
                    why.last()
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| tr("Omarchy did not change it"))
                }
                // Could not start, or timed out: the error says which.
                Err(e) => e.clone(),
            };
            if let Some(w) = &win {
                w.notify(&format!("{nm} was not switched {word}"), &body, "normal");
            }
        }
        if let Some(w) = &win {
            // `if getattr(self, "_plugins", None): annotate(self._plugins)`;
            // what annotate raises is logged. Read off the main thread: it
            // runs `omarchy plugin list`.
            let wanted = {
                let shared = w.shared.borrow();
                !shared.plugins_none && !shared.plugins.is_empty()
            };
            if wanted {
                w.annotate_plugins_then(|_| {});
            }
        }
    };
    spawn_bg(work, done);
    glib::Propagation::Stop // the state is set when Omarchy answers
}

impl Win {
    /// Every plugin on the system, with a switch each. See plugins.py.
    pub fn build_plugins_page(&self) -> gtk::Widget {
        let (page, plugins_body) = crate::ui::shell::page_body();
        {
            let mut st = self.plugins.borrow_mut();
            st.plugins_body = Some(plugins_body);
            st.plugin_rows = Vec::new();
        }
        self.shared.borrow_mut().plugin_updates = 0;
        page.upcast()
    }

    pub fn plugins_reload(&self) {
        self.summarize(&tr("Reading plugins…"), Some("plugins"));

        // `getattr(self, "_plugins", None)`, read here: the worker cannot
        // reach the window.
        let have = self.shared.borrow().plugins.clone();
        let w = self.downgrade();
        spawn_bg(
            move || {
                let mut cache = inventory::git_cache();
                let entry = cache
                    .entry("_plugins".to_string())
                    .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
                // What `installed` raises ends the worker: the page stays at
                // "Reading plugins…".
                let found = match entry {
                    serde_json::Value::Object(m) => plugins_mod::try_installed(true, Some(m)),
                    serde_json::Value::Null => plugins_mod::try_installed(true, None),
                    other => {
                        // `setdefault` hands back whatever is stored, and
                        // `_remote` then reads it with `.get` (or, when it is
                        // falsy, writes into it): on anything but a dict that
                        // raises, the worker dies, and the page stays at
                        // "Reading plugins…".
                        let mut scratch = plugins_mod::FetchCache::new();
                        let found = plugins_mod::try_installed(true, Some(&mut scratch));
                        if let Some(exc) = found
                            .as_ref()
                            .ok()
                            .and_then(|f| foreign_cache_error(other, f, &scratch))
                        {
                            uncaught(exc.0, &exc.1);
                            return None;
                        }
                        found
                    }
                };
                let found = match found {
                    Ok(found) => found,
                    Err(e) => {
                        uncaught_err(&e);
                        return None;
                    }
                };
                // `except Exception: trust = {}`
                let catalog = if have.is_empty() {
                    store_mod::try_catalog(false)
                } else {
                    Ok(std::sync::Arc::unwrap_or_clone(have))
                };
                let trust: BTreeMap<String, store_mod::Plugin> = match catalog {
                    Ok(catalog) => catalog.into_iter().map(|c| (c.id.clone(), c)).collect(),
                    Err(_) => BTreeMap::new(),
                };
                let raw = if shortcuts::available() {
                    shortcuts::_read().map(|r| r.0).unwrap_or_default()
                } else {
                    String::new()
                };
                let mut keys = BTreeMap::new();
                for i in &found {
                    // `i.has_panel` raising ends the worker.
                    let panel = !raw.is_empty()
                        && match i.try_has_panel() {
                            Ok(panel) => panel,
                            Err(e) => {
                                uncaught_err(&e);
                                return None;
                            }
                        };
                    if panel {
                        keys.insert(
                            i.id.clone(),
                            shortcuts::current(&i.id, Some(&raw)).unwrap_or_default(),
                        );
                    }
                }
                Some((found, trust, keys))
            },
            move |res| {
                let Some((found, trust, keys)) = res else {
                    return;
                };
                if let Some(w) = w.upgrade() {
                    w.plugins_show(found, trust, keys);
                }
            },
        );
    }

    pub fn plugins_show(
        &self,
        found: Vec<plugins_mod::Installed>,
        trust: BTreeMap<String, store_mod::Plugin>,
        keys: BTreeMap<String, String>,
    ) {
        let Some(body) = self.plugins.borrow().plugins_body.clone() else {
            return;
        };
        while let Some(child) = body.first_child() {
            body.remove(&child);
        }
        let old = std::mem::take(&mut self.plugins.borrow_mut().plugin_rows);
        drop(old);
        let mine: Vec<&plugins_mod::Installed> = found.iter().filter(|i| !i.built_in).collect();
        let builtin: Vec<&plugins_mod::Installed> = found.iter().filter(|i| i.built_in).collect();
        for (title, note, group) in [
            (
                tr("Your plugins"),
                tr("Added from the marketplace or by hand. Updated through \
                    omarchy plugin update; a development link never is."),
                &mine,
            ),
            (
                tr("Built in"),
                tr(
                    "Ship with Omarchy and update with it. They can be switched \
                    on and off.",
                ),
                &builtin,
            ),
        ] {
            if group.is_empty() {
                continue;
            }
            let section = Win::section(&format!("{title} ({})", group.len()), &note, None);
            for i in group.iter() {
                let listing = trust.get(&i.id);
                // What raises building a row (`listing.trust`, the logo's
                // icon cache) ends `_plugins_show` there.
                let t = match listing.map(|l| l.try_trust()).transpose() {
                    Ok(t) => t,
                    Err(e) => return uncaught_err(&e),
                };
                let row = match self.plugin_row(
                    i,
                    t.as_ref(),
                    keys.get(&i.id).map(String::as_str).unwrap_or(""),
                    listing,
                ) {
                    Ok(row) => row,
                    Err(e) => return uncaught_err(&e),
                };
                section.rows.append(&row.0);
                self.plugins.borrow_mut().plugin_rows.push(row);
            }
            body.append(&section.root);
        }
        if found.is_empty() {
            body.append(&Win::status_page(
                "application-x-addon-symbolic",
                &tr("No plugins found"),
                &tr("The omarchy command did not list any plugins."),
                None,
            ));
        }
        let updates = mine
            .iter()
            .filter(|i| i.state.as_ref().is_some_and(|s| s.updatable()))
            .count();
        self.shared.borrow_mut().plugin_updates = updates;
        let on = found.iter().filter(|i| i.enabled).count();
        let text = format!("{} plugins · {on} on", found.len())
            + &if updates != 0 {
                format!(" · {updates} with updates")
            } else {
                String::new()
            };
        self.summarize(&text, Some("plugins"));
        self.plugins_filter(&self.search.text());
        self.sync_primary();
    }

    pub fn plugins_filter(&self, text: &str) {
        let rows = self.plugins.borrow().plugin_rows.clone();
        for (row, i) in rows {
            let hit = plugin_matches(&i.borrow(), text);
            row.set_visible(hit);
        }
    }

    pub fn plugin_row(
        &self,
        i: &plugins_mod::Installed,
        trust: Option<&store_mod::Trust>,
        shortcut: &str,
        listing: Option<&store_mod::Plugin>,
    ) -> Result<
        (
            adw::ActionRow,
            std::rc::Rc<std::cell::RefCell<plugins_mod::Installed>>,
        ),
        String,
    > {
        let subtitle = if i.description.is_empty() {
            &i.id
        } else {
            &i.description
        };
        let row: adw::ActionRow = Win::plain(&i.name, subtitle);
        row.set_title_lines(1);
        row.set_subtitle_lines(1);
        row.set_tooltip_text(Some(subtitle));
        row.add_prefix(&self.plugin_logo(i, listing)?);

        let st = i.state.as_ref();
        let key = plugin_state_key(st);
        if let Some((label, css)) = plugin_state_pill(&key) {
            let pill = gtk::Label::builder()
                .label(tr(label))
                .valign(gtk::Align::Center)
                .build();
            pill.add_css_class(css);
            pill.set_tooltip_text(Some(
                &st.map(|s| s.try_describe())
                    .transpose()?
                    .unwrap_or_default(),
            ));
            row.add_suffix(&pill);
        }
        if let Some(trust) = trust {
            if !i.built_in {
                let chip = gtk::Label::builder()
                    .label(&trust.label)
                    .valign(gtk::Align::Center)
                    .build();
                chip.add_css_class(&trust.css);
                chip.set_tooltip_text(Some(&trust.why));
                row.add_suffix(&chip);
            }
        }
        if !shortcut.is_empty() {
            let keys = gtk::Label::builder()
                .label(shortcut.replace(" + ", "+"))
                .valign(gtk::Align::Center)
                .build();
            keys.add_css_class("dim-label");
            keys.add_css_class("caption");
            keys.set_tooltip_text(Some(&tr("Keyboard shortcut")));
            row.add_suffix(&keys);
        }

        // The actions, behind a menu; the switch beside it is the one control
        // that is always one click.
        let menu = gio::Menu::new();
        for (label, act) in plugin_menu(i, shortcuts::available())? {
            menu.append(Some(&tr(label)), Some(&format!("plugin.{act}::{}", i.id)));
        }
        if menu.n_items() != 0 {
            let more = gtk::MenuButton::builder()
                .icon_name("view-more-symbolic")
                .valign(gtk::Align::Center)
                .menu_model(&menu)
                .build();
            more.add_css_class("flat");
            name(&more, &format!("More actions for {}", i.name));
            row.add_suffix(&more);
        }

        let switch = gtk::Switch::builder()
            .active(i.enabled)
            .valign(gtk::Align::Center)
            .sensitive(i.can_disable)
            .build();
        name(&switch, &switch_name(&i.name, i.enabled));
        if !i.can_disable {
            switch.set_tooltip_text(Some(&tr("Omarchy needs this one; it cannot be \
                                              switched off")));
        }
        let shared = std::rc::Rc::new(std::cell::RefCell::new(i.clone()));
        let w = self.downgrade();
        let ic = shared.clone();
        let guard = SwitchGuard::default();
        let g = guard.clone();
        guard.connect(&switch, move |sw, on| {
            plugin_switch_shared(&w, sw, on, &g, ic.clone())
        });
        row.add_suffix(&switch);
        row.set_activatable_widget(Some(&switch));
        Ok((row, shared))
    }

    /// The mark beside a plugin, best source first (see plugins.py):
    /// the vendor's own logo for a plugin about a service, the marketplace
    /// artwork for a listed one, a system icon for what it does, the
    /// author's GitHub avatar, and a generic plugin icon last.
    ///
    /// The system icon shows at once; a logo replaces it when it arrives,
    /// so rows never jump.
    pub fn plugin_logo(
        &self,
        i: &plugins_mod::Installed,
        listing: Option<&store_mod::Plugin>,
    ) -> Result<gtk::Box, String> {
        let frame = gtk::Box::builder()
            .width_request(36)
            .height_request(36)
            .hexpand(false)
            .valign(gtk::Align::Center)
            .halign(gtk::Align::Center)
            .build();
        frame.add_css_class("plugin-logo");
        frame.set_overflow(gtk::Overflow::Hidden);
        let symbol = plugins_mod::symbol_for(i);
        let image = gtk::Image::builder()
            .icon_name(&symbol)
            .pixel_size(22)
            .hexpand(true)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .accessible_role(gtk::AccessibleRole::Presentation)
            .build();
        frame.append(&image);

        let show = {
            let w = self.downgrade();
            let frame = frame.clone();
            move |path: Option<std::path::PathBuf>, cover: bool| {
                let Some(path) = path else { return };
                let Some(w) = w.upgrade() else { return };
                let tex = if cover {
                    w.square(&path, 72)
                } else {
                    w.texture(&path, 72)
                };
                let Some(tex) = tex else { return };
                if let Some(first) = frame.first_child() {
                    frame.remove(&first);
                }
                if cover {
                    // Already square and small: an Image at a fixed pixel size
                    // cannot ask for more room than the tile has.
                    let pic = gtk::Image::builder()
                        .paintable(&tex)
                        .pixel_size(36)
                        .accessible_role(gtk::AccessibleRole::Presentation)
                        .build();
                    frame.append(&pic);
                    frame.add_css_class("has-art");
                } else {
                    let logo = gtk::Image::builder()
                        .paintable(&tex)
                        .pixel_size(26)
                        .hexpand(true)
                        .halign(gtk::Align::Center)
                        .valign(gtk::Align::Center)
                        .accessible_role(gtk::AccessibleRole::Presentation)
                        .build();
                    frame.append(&logo);
                }
            }
        };

        // `icons.cached` raising ends `_plugin_logo` (and the row with it);
        // `icons.fetch` raising on the pool is kept by its future, unseen,
        // and nothing is shown.
        let fetch = |key: String, url: String, domain: String| -> Result<(), String> {
            if let Some(hit) = crate::icons::cached_checked(&key)? {
                show(Some(hit), false);
                return Ok(());
            }
            let show = show.clone();
            self.thumb_bg(
                move || crate::icons::fetch_checked(&key, &url, &domain),
                move |path| {
                    if let Ok(path) = path {
                        show(path, false);
                    }
                },
            );
            Ok(())
        };

        match Win::plugin_logo_choice(i, listing) {
            LogoChoice::Vendor { key, domain } => fetch(key, String::new(), domain)?,
            LogoChoice::Avatar { key, url } => fetch(key, url, String::new())?,
            LogoChoice::Symbol { .. } => {}
            LogoChoice::Art { .. } => {
                let Some(listing) = listing else {
                    return Ok(frame);
                };
                let cached = store_mod::thumb_path(listing, false);
                let present = cached.as_ref().is_some_and(|c| {
                    c.exists() && std::fs::metadata(c).map(|m| m.len() != 0).unwrap_or(false)
                });
                if present {
                    show(cached, true);
                } else {
                    let listing = listing.clone();
                    let show = show.clone();
                    self.thumb_bg(
                        move || store_mod::fetch_thumb(&listing, false),
                        move |path| show(path, true),
                    );
                }
            }
        }
        Ok(frame)
    }

    /// The mark `plugin_logo` puts beside a plugin, decided without a
    /// window: the vendor's logo, the marketplace art, the author's avatar
    /// (only where no system icon says more), or the system icon alone.
    pub fn plugin_logo_choice(
        i: &plugins_mod::Installed,
        listing: Option<&store_mod::Plugin>,
    ) -> LogoChoice {
        let symbol = plugins_mod::symbol_for(i);
        let domain = plugins_mod::vendor_for(i);
        let owner =
            plugins_mod::github_owner(i.state.as_ref().map(|s| s.url.as_str()).unwrap_or(""));
        if !domain.is_empty() {
            LogoChoice::Vendor {
                key: domain.clone(),
                domain,
            }
        } else if let Some(listing) = listing.filter(|l| !l.thumb.is_empty()) {
            LogoChoice::Art {
                thumb: store_mod::thumb_path(listing, false)
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            }
        } else if symbol == plugins_mod::GENERIC && !owner.is_empty() {
            LogoChoice::Avatar {
                key: format!("github-{owner}"),
                url: crate::icons::github_avatar(&owner),
            }
        } else {
            LogoChoice::Symbol { name: symbol }
        }
    }

    /// A task step: drop a removed plugin's shortcut. Otherwise the
    /// binding outlives the plugin -- the key stays taken and opens nothing.
    /// Never fails the removal; a binding it cannot remove is reported.
    pub fn forget_shortcut(pid: &str) -> tasks::Step {
        let pid = pid.to_string();
        let f: tasks::StepFn = std::sync::Arc::new(move |log: tasks::LogFn<'_>| {
            forget_shortcut_with(
                &pid,
                log,
                &shortcuts::available,
                &|p| shortcuts::current(p, None),
                &shortcuts::set_shortcut,
            );
            Ok(())
        });
        tasks::Step::call(tr("Removing its keyboard shortcut"), f)
    }

    /// Switch a plugin through Omarchy, then show what actually holds.
    ///
    /// The switch moves only once Omarchy's own list agrees; a refusal puts
    /// it back and says why.
    pub fn plugin_switch(
        &self,
        switch: &gtk::Switch,
        on: bool,
        guard: &SwitchGuard,
        i: std::rc::Rc<std::cell::RefCell<plugins_mod::Installed>>,
    ) -> glib::Propagation {
        // `i` is the caller's own object (the store dialog's `info`), shared,
        // so what `done` writes to it the caller sees on the next toggle.
        // `guard` is the one `switch` was connected through.
        plugin_switch_shared(&self.downgrade(), switch, on, guard, i)
    }

    pub fn plugin_action(&self, action: &str, pid: &str) {
        let found = self
            .plugins
            .borrow()
            .plugin_rows
            .iter()
            .find(|(_r, x)| x.borrow().id == pid)
            .map(|(_r, x)| x.borrow().clone());
        let Some(i) = found else { return };
        let st = i.state.clone();
        if action == "shortcut" {
            self.edit_shortcut(&i.id, &i.name);
        } else if action == "folder" && i.folder.is_some() {
            if let Some(folder) = &i.folder {
                self.open_path(folder);
            }
        } else if action == "page" && st.as_ref().is_some_and(|s| !s.url.is_empty()) {
            let url = st.as_ref().map(|s| s.url.clone()).unwrap_or_default();
            let page = inventory::github_page(&url);
            self.open_url(if page.is_empty() { &url } else { &page });
        } else if action == "update" && i.folder.is_some() {
            let mut extra = serde_json::Map::new();
            let folder = i
                .folder
                .as_ref()
                .map(|f| f.display().to_string())
                .unwrap_or_default();
            extra.insert("repo".into(), folder.into());
            extra.insert("plugin_id".into(), i.id.clone().into());
            extra.insert(
                "plugin_state".into(),
                st.as_ref()
                    .map(|s| s.kind.clone())
                    .unwrap_or_default()
                    .into(),
            );
            let detail = match st.as_ref().map(|s| s.try_describe()).transpose() {
                Ok(detail) => detail.unwrap_or_default(),
                Err(e) => return uncaught_err(&e),
            };
            extra.insert("plugin_detail".into(), detail.into());
            let app = inventory::App {
                update: st
                    .as_ref()
                    .map(|s| format!("{} commits", s.behind))
                    .unwrap_or_default(),
                extra: extra.into(),
                ..inventory::App::new(&i.id, &i.version, inventory::LOCAL)
            };
            self.on_git_pull(&app);
        } else if action == "remove" {
            let argv: Vec<String> = ["omarchy", "plugin", "remove", &i.id, "--yes"]
                .iter()
                .map(|s| s.to_string())
                .collect();
            let folder = i
                .folder
                .as_ref()
                .map(|f| f.display().to_string())
                .unwrap_or_else(|| "None".into());
            let w = self.downgrade();
            tasks::confirm(
                &self.window,
                &format!("Remove {}?", i.name),
                &tr("Deletes the plugin's folder and takes it off the bar. Its \
                     settings in shell.json are removed with it."),
                &format!("{folder}\n\n$ {}", argv.join(" ")),
                &tr("Remove"),
                true,
                move || {
                    let Some(w) = w.upgrade() else { return };
                    let id = i.id.clone();
                    let probe: TryProbe = std::sync::Arc::new(move || {
                        Ok(Some(ProbeValue::Bool(
                            plugins_mod::try_enabled()?.contains_key(&id),
                        )))
                    });
                    let nm = i.name.clone();
                    let judge: Judge = Box::new(move |_b, a| {
                        let a = truthy(&a);
                        Outcome::new(
                            Some(!a),
                            if !a {
                                format!("{nm} removed")
                            } else {
                                format!("{nm} is still installed")
                            },
                        )
                    });
                    let wt = w.downgrade();
                    let then: tasks::OnDone = Box::new(move |ok| {
                        if let Some(w) = wt.upgrade() {
                            w.plugins_reload();
                            w.after_pacman(ok);
                        }
                    });
                    let wr = w.downgrade();
                    let steps = vec![
                        tasks::Step::argv(format!("Removing {}", i.name), argv.clone()),
                        Win::forget_shortcut(&i.id),
                    ];
                    let title = format!("Removing {}", i.name);
                    w.checked(
                        Some(probe),
                        judge,
                        Some(then),
                        uncaught_err,
                        move |on_done| {
                            if let Some(w) = wr.upgrade() {
                                w.run_task(&title, steps, Some(on_done));
                            }
                        },
                    );
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn plugin_logo_choice_vendor_art_avatar_symbol() {
        let state = |url: &str| {
            Some(plugins_mod::State::new(
                "p",
                plugins_mod::CURRENT,
                "",
                0,
                url,
            ))
        };
        let mut i = plugins_mod::Installed {
            id: "tim.tailscale".into(),
            name: "My Tailscale".into(),
            ..Default::default()
        };
        assert_eq!(
            Win::plugin_logo_choice(&i, None),
            LogoChoice::Vendor {
                key: "tailscale.com".into(),
                domain: "tailscale.com".into()
            }
        );
        i = plugins_mod::Installed {
            id: "a.thing".into(),
            name: "Thing".into(),
            state: state("https://github.com/someone/thing"),
            ..Default::default()
        };
        assert_eq!(
            Win::plugin_logo_choice(&i, None),
            LogoChoice::Avatar {
                key: "github-someone".into(),
                url: "https://github.com/someone.png?size=128".into()
            }
        );
        let listing = store_mod::Plugin {
            id: "a.thing".into(),
            thumb: "img/t.webp".into(),
            ..Default::default()
        };
        assert!(matches!(
            Win::plugin_logo_choice(&i, Some(&listing)),
            LogoChoice::Art { .. }
        ));
        // A system icon that says what it does wins over the author's face.
        i = plugins_mod::Installed {
            id: "lxp.vitals".into(),
            name: "Vitals".into(),
            state: state("https://github.com/lxp/vitals"),
            ..Default::default()
        };
        assert_eq!(
            Win::plugin_logo_choice(&i, None),
            LogoChoice::Symbol {
                name: "emblem-system-symbolic".into()
            }
        );
    }

    fn log_into(lines: &Mutex<Vec<String>>) -> impl Fn(&str) + Send + Sync + '_ {
        move |l: &str| lines.lock().unwrap().push(l.to_string())
    }

    // PluginRemovalAndRecheck.test_removing_a_plugin_removes_its_shortcut
    #[test]
    fn removing_a_plugin_removes_its_shortcut() {
        let lines = Mutex::new(Vec::new());
        let calls = Mutex::new(Vec::new());
        let log = log_into(&lines);
        forget_shortcut_with(
            "lxp.vitals",
            &log,
            &|| true,
            &|_p| Ok("SUPER + ALT + V".to_string()),
            &|p, c| {
                calls
                    .lock()
                    .unwrap()
                    .push((p.to_string(), c.map(str::to_string)));
                Ok("removed".to_string())
            },
        );
        assert_eq!(
            *calls.lock().unwrap(),
            vec![("lxp.vitals".to_string(), None)]
        );
    }

    // PluginRemovalAndRecheck.test_a_shortcut_that_cannot_be_removed_does_not_fail_the_removal
    #[test]
    fn a_shortcut_that_cannot_be_removed_does_not_fail_the_removal() {
        let lines = Mutex::new(Vec::new());
        let log = log_into(&lines);
        forget_shortcut_with(
            "x",
            &log,
            &|| true,
            &|_p| Err("edited by hand".to_string()),
            &|_p, _c| panic!("must not be called"),
        );
        let lines = lines.lock().unwrap();
        assert!(lines.last().unwrap().contains("left in bindings.lua"));
    }

    #[test]
    fn forget_shortcut_step_label() {
        assert_eq!(
            Win::forget_shortcut("x").label,
            "Removing its keyboard shortcut"
        );
    }

    #[test]
    fn a_plugins_cache_that_is_not_a_dict_fails_like_python() {
        // Demo mode answers before the cache is looked at: held so a test
        // switching it on meanwhile cannot change the answer.
        let _g = crate::util::TEST_ENV
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let reached = plugins_mod::Installed {
            state: Some(plugins_mod::State::new("a", "update", "behind", 1, "u")),
            ..Default::default()
        };
        let local = plugins_mod::Installed {
            state: Some(plugins_mod::State::new("a", "local", "symlink", 0, "")),
            ..Default::default()
        };
        let empty = plugins_mod::FetchCache::new();
        let mut wrote = plugins_mod::FetchCache::new();
        wrote.insert("/p".into(), serde_json::json!({}));
        assert_eq!(
            foreign_cache_error(
                &serde_json::json!([1]),
                std::slice::from_ref(&reached),
                &empty
            ),
            Some((
                "AttributeError",
                "'list' object has no attribute 'get'".to_string()
            ))
        );
        assert_eq!(
            foreign_cache_error(
                &serde_json::json!("x"),
                std::slice::from_ref(&local),
                &empty
            ),
            None
        );
        assert_eq!(
            foreign_cache_error(
                &serde_json::json!([]),
                std::slice::from_ref(&reached),
                &empty
            ),
            None
        );
        assert_eq!(
            foreign_cache_error(&serde_json::json!(0), &[], &wrote),
            Some((
                "TypeError",
                "'int' object does not support item assignment".to_string()
            ))
        );
        assert_eq!(
            foreign_cache_error(&serde_json::json!([]), &[], &wrote),
            Some((
                "TypeError",
                "list indices must be integers or slices, not str".to_string()
            ))
        );
    }

    #[test]
    fn state_pills() {
        assert_eq!(
            plugin_state_pill("update"),
            Some(("Update waiting", "pill-accent"))
        );
        assert_eq!(
            plugin_state_pill("symlink"),
            Some(("Development link", "pill-quiet"))
        );
        assert_eq!(plugin_state_pill("current"), None);
        let st = plugins_mod::State::new("a", "update", "3 commits", 3, "");
        assert_eq!(plugin_state_key(Some(&st)), "update");
        let st = plugins_mod::State::new("a", "local-changes", "modified", 0, "");
        assert_eq!(plugin_state_key(Some(&st)), "modified");
        assert_eq!(plugin_state_key(None), "");
    }

    #[test]
    fn filter_matches_id_name_description_and_kinds() {
        let i = plugins_mod::Installed {
            id: "lxp.vitals".into(),
            name: "Vitals".into(),
            description: "CPU and memory".into(),
            kinds: vec!["bar-widget".into()],
            ..Default::default()
        };
        assert!(plugin_matches(&i, ""));
        assert!(plugin_matches(&i, "  VITALS "));
        assert!(plugin_matches(&i, "memory"));
        assert!(plugin_matches(&i, "bar-widget"));
        assert!(!plugin_matches(&i, "weather"));
    }

    #[test]
    fn row_menu_matches_the_python() {
        let mut i = plugins_mod::Installed {
            id: "x".into(),
            kinds: vec!["panel".into()],
            folder: Some("/tmp/x".into()),
            state: Some(plugins_mod::State::new(
                "x",
                "update",
                "",
                2,
                "https://github.com/a/x",
            )),
            ..Default::default()
        };
        let acts: Vec<&str> = plugin_menu(&i, true)
            .expect("menu")
            .iter()
            .map(|(_l, a)| *a)
            .collect();
        assert_eq!(acts, ["update", "shortcut", "folder", "page", "remove"]);
        i.built_in = true;
        i.folder = None;
        i.state = None;
        assert!(plugin_menu(&i, true).expect("menu").is_empty());
        // Local edits with upstream commits waiting can still be updated.
        i.state = Some(plugins_mod::State::new(
            "x",
            "local-changes",
            "modified",
            1,
            "",
        ));
        let acts: Vec<&str> = plugin_menu(&i, false)
            .expect("menu")
            .iter()
            .map(|(_l, a)| *a)
            .collect();
        assert_eq!(acts, ["update"]);
    }

    #[test]
    fn switch_names() {
        assert_eq!(switch_name("Vitals", true), "Vitals on");
        assert_eq!(switch_name("Vitals", false), "Vitals off");
    }
}
