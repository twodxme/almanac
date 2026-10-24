//! Page chrome: narrow mode, titles, showing a page, the header's primary
//! action, toasts, theming and the status line.

use std::collections::BTreeMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::i18n::tr;
use crate::inventory;
use crate::ui::shell::{Win, name, page_spec, sorts_installed};

use super::PrimaryAction;

impl Win {
    /// Fold or unfold for the window's width.
    pub fn set_narrow(&self, narrow: bool) {
        self.shared.borrow_mut().narrow = narrow;
        // Collapsed, the sidebar is an overlay that should start hidden;
        // expanded, it is furniture and should be back.
        self.split.set_show_sidebar(!narrow);
        self.sidebar_toggle.set_active(!narrow);
        self.sidebar_toggle.set_tooltip_text(Some(&if narrow {
            tr("Show categories")
        } else {
            tr("Hide categories")
        }));
        // One column of cards is the right answer on a narrow window; four
        // would each be 110px wide.
        let grid = self.shared.borrow().store_grid.clone();
        if let Some(grid) = grid {
            grid.set_max_columns(if narrow { 2 } else { 4 });
        }
    }

    pub fn page_title(&self, key: &str, fallback: &str) -> String {
        let row = self.shared.borrow().sidebar_row(key).map(|r| r.row.clone());
        match row {
            Some(row) => row.title().to_string(),
            None => fallback.to_string(),
        }
    }

    /// Apply one page's chrome. The only place that touches it.
    pub fn show_page(&self, key: &str, category: &str) {
        let Some(spec) = page_spec(key).or_else(|| page_spec("packages")) else { return };
        self.shared.borrow_mut().page = Some(key.to_string());
        self.content_stack.set_visible_child_name(spec.view);
        let title = tr(spec.title);
        self.title.set_title(&if !category.is_empty() {
            self.page_title(category, &title)
        } else {
            title
        });
        let drawer = spec.filters;
        self.sort_btn.set_visible(spec.sort || drawer);
        name(
            &self.sort_btn,
            &if drawer { tr("Filter and sort the marketplace") } else { tr("Sort by") },
        );
        if drawer {
            self.sort_btn.set_menu_model(None::<&gio::MenuModel>);
            // Python builds the drawer in __init__, so the page change always
            // runs to the end; if the store section has not built it yet the
            // button simply has no popover until it does.
            let drawer_widget = self.shared.borrow().store_drawer.clone();
            self.sort_btn.set_popover(drawer_widget.as_ref());
        } else {
            self.sort_btn.set_popover(None::<&gtk::Popover>);
            self.rebuild_sort_menu(&sorts_installed());
        }
        self.search_bar.set_visible(spec.search.is_some());
        if let Some(search) = spec.search {
            if !search.is_empty() {
                let text = if spec.search_translated { tr(search) } else { search.to_string() };
                self.search.set_placeholder_text(Some(&text));
            }
        }
        self.store_back.set_visible(false);
        if spec.view == "store" {
            self.pin_market(key);
        }
        self.sync_primary();
    }

    /// Re-read whatever the current page shows. One action, every page.
    pub fn refresh_page(&self) {
        let page = self.shared.borrow().page_or("packages");
        match page.as_str() {
            "featured" => self.feat_reload(),
            "discover" => self.store_reload(true),
            "maintenance" => self.maint_reload(),
            "history" => self.history_reload(),
            "plugins" => self.plugins_reload(),
            "switchboard" => self.switchboard_reload(),
            p if p == inventory::AGENT => self.ai_check(),
            _ => {
                // Asked for: look upstream again rather than reuse the
                // pending updates the last scans shared.
                inventory::forget_pending();
                self.reload()
            }
        }
    }

    /// `getattr(self, "_store_markets", [""])`.
    ///
    /// The attribute always exists here: `Shared` starts it at `[""]`, the
    /// fallback's value, so it is read as it is (an empty list stays empty).
    fn store_market_keys(&self) -> Vec<String> {
        self.shared.borrow().store_markets.clone()
    }

    /// Point the marketplace filter at whatever this page is about.
    ///
    /// The App Store shows every catalogue; Themes is the same view with one
    /// of them selected. Changing the filter by hand afterwards re-titles the
    /// page, so the header never claims to be showing something it is not.
    pub fn pin_market(&self, page: &str) {
        let has_plugins = !self.shared.borrow().plugins.is_empty();
        let filters_for = self.shared.borrow().filters_for;
        if has_plugins && filters_for != Some(page == "themes") {
            self.store_filters();
        }
        let wanted = page_spec(page).and_then(|s| s.market).unwrap_or("");
        let keys = self.store_market_keys();
        let Some(market) = self.shared.borrow().store_market.clone() else { return };
        // Themes is one catalogue already; the App Store chooses among the rest.
        market.set_visible(page != "themes" && keys.len() > 2);
        let index = keys.iter().position(|k| k == wanted).unwrap_or(0) as u32;
        if market.selected() != index {
            self.shared.borrow_mut().pinning = true;
            market.set_selected(index);
            self.shared.borrow_mut().pinning = false;
        }
    }

    /// Say what the store is showing when the marketplace filter changes.
    ///
    /// This only edits the title. It used to also select the matching
    /// sidebar row, which re-entered the selection handler and rebuilt the
    /// page -- so opening Themes, then letting the catalogue finish loading,
    /// put you back on the App Store. Which page you are on is the sidebar's
    /// business and nothing else's.
    pub fn retitle_store(&self) {
        if self.content_stack.visible_child_name().as_deref() != Some("store") {
            return;
        }
        if self.shared.borrow().pinning {
            return;
        }
        let keys = self.store_market_keys();
        if keys.len() <= 1 {
            return;
        }
        let Some(store_market) = self.shared.borrow().store_market.clone() else { return };
        let i = store_market.selected() as usize;
        let market = keys.get(i).cloned().unwrap_or_default();
        let page = self.shared.borrow().page_or("discover");
        if market == "themes" {
            self.title.set_title(&tr("Themes"));
        } else if page == "themes" && market.is_empty() {
            // Un-pinned from the Themes page: it is the whole store now.
            self.title.set_title(&tr("App Store"));
        } else {
            let title = page_spec(&page).map(|s| tr(s.title)).unwrap_or_else(|| "App Store".into());
            self.title.set_title(&title);
        }
    }

    /// Put the current page's main action in the header button.
    pub fn sync_primary(&self) {
        let page = self.shared.borrow().page_or("packages");
        let action = self.primary_actions().remove(&page);
        self.shared.borrow_mut().primary = action.clone();
        let Some(action) = action else {
            self.update_btn.set_visible(false);
            return;
        };
        // A disabled primary button ("Nothing to reclaim", "Plugins current")
        // takes the header's most prominent place to say there is nothing to
        // do; the subtitle already says that.
        if !action.enabled {
            self.update_btn.set_visible(false);
            return;
        }
        self.update_btn.set_visible(true);
        self.update_btn.set_label(&action.label);
        self.update_btn.set_sensitive(action.enabled);
        self.update_btn.set_tooltip_text(Some(&action.tooltip));
    }

    /// (label, enabled, tooltip, handler) for each page that has one.
    pub fn primary_actions(&self) -> BTreeMap<String, PrimaryAction> {
        let pending = Win::count_items(&self.store_objects(), |i| i.has_update);
        let (harnesses, free, plugin_updates) = {
            let sh = self.shared.borrow();
            let harnesses = sh
                .ai_survey
                .iter()
                .flat_map(|(_t, _d, es)| es.iter())
                .filter(|e| {
                    e.kind == crate::ai::HARNESS
                        && !e.update.is_empty()
                        && e.action.as_ref().is_some_and(|a| !a.is_empty())
                })
                .count();
            (harnesses, crate::maintenance::reclaimable_mb(&sh.findings.get()), sh.plugin_updates)
        };
        let mut out = BTreeMap::new();
        out.insert(
            "packages".to_string(),
            PrimaryAction {
                label: if pending != 0 {
                    format!("Update all ({pending})")
                } else {
                    "Up to date".to_string()
                },
                enabled: pending != 0,
                tooltip: "Runs `omarchy update` in a terminal, which is where it can ask for \
                          your password, then updates everything else waiting — plugins, AppImages, \
                          agent CLIs, local checkouts"
                    .to_string(),
                handler: Rc::new(|w: &Win| w.on_update_all()),
            },
        );
        out.insert(
            inventory::AGENT.to_string(),
            PrimaryAction {
                label: if harnesses != 0 {
                    format!("Update {harnesses} agent CLI{}", if harnesses != 1 { "s" } else { "" })
                } else {
                    "Agents current".to_string()
                },
                enabled: harnesses != 0,
                tooltip: "Update every agent CLI with a newer release that knows how to update \
                          itself"
                    .to_string(),
                handler: Rc::new(|w: &Win| w.ai_update_all()),
            },
        );
        out.insert(
            "plugins".to_string(),
            PrimaryAction {
                label: if plugin_updates != 0 {
                    format!("Update plugins ({plugin_updates})")
                } else {
                    tr("Plugins current")
                },
                enabled: plugin_updates != 0,
                tooltip: tr(
                    "Update every plugin with a clean update waiting, within the scope set in \
                     Preferences",
                ),
                handler: Rc::new(|w: &Win| w.on_update_plugins()),
            },
        );
        out.insert(
            "maintenance".to_string(),
            PrimaryAction {
                label: if free != 0.0 {
                    format!("Reclaim {}", crate::maintenance::fmt_size(free))
                } else {
                    "Nothing to reclaim".to_string()
                },
                enabled: free > 0.0,
                tooltip: "Trim the package cache, clear the AUR build cache and vacuum the \
                          journal — none of it touches anything you created"
                    .to_string(),
                handler: Rc::new(|w: &Win| w.reclaim_space()),
            },
        );
        out
    }

    pub fn on_primary(&self, _button: &gtk::Button) {
        let primary = self.shared.borrow().primary.clone();
        if let Some(primary) = primary {
            (primary.handler)(self);
        }
    }

    /// A transient result. Replaces four separate dim status labels.
    ///
    /// `timeout` defaulted to 5 in Python (`SAY_TIMEOUT`).
    pub fn say(&self, message: &str, timeout: u32) {
        // Messages carry names and error text from outside the app, so they
        // are shown as written rather than parsed as markup -- but with the
        // Python spelling of an error ("IndexError: ...") put in plain words.
        let message = crate::util::human_error(message);
        let toast = adw::Toast::builder().title(message).use_markup(false).timeout(timeout).build();
        self.toasts.add_toast(toast);
    }

    /// Restate the active Omarchy palette as libadwaita's named colours.
    ///
    /// Absence is a valid state: with no Omarchy theme the provider is
    /// emptied and GTK's own theme shows through unmodified.
    ///
    /// `Err` where the Python raises (out of `theming`), before the
    /// provider is touched.
    pub fn apply_theme(&self) -> Result<(), String> {
        let palette = crate::theming::try_load_palette()?;
        let mut css = crate::theming::try_css_for(&palette)?;
        if palette.is_empty() {
            // No Omarchy palette: measure the pills against whatever theme GTK
            // is showing, so they read as well there as on an Omarchy one.
            let fills = self.theme_fills()?;
            css += &crate::theming::pill_css(&fills, &[]);
            let get = |k: &str| fills.get(k).cloned().unwrap_or_default();
            css += &crate::theming::try_pick_css(&get("card"), &get("fg"), &get("accent"))?;
        }
        // load_from_data raised GLib.Error on a parse failure in older GTK;
        // GTK 4.12+ reports parse errors through a signal and never fails.
        self.theme_provider.load_from_string(&css);
        let dark = crate::theming::is_dark(&palette);
        if let Some(dark) = dark {
            adw::StyleManager::default().set_color_scheme(if dark {
                adw::ColorScheme::ForceDark
            } else {
                adw::ColorScheme::ForceLight
            });
        }
        Ok(())
    }

    /// The active GTK theme's status fills, as hex. lookup_color is the
    /// only way GTK 4 exposes a named colour; it is deprecated, not gone.
    #[allow(deprecated)]
    pub fn theme_fills(&self) -> Result<BTreeMap<String, String>, String> {
        let mut out = BTreeMap::new();
        let ctx = self.window.style_context();
        // Translucent colours are flattened onto the window first:
        // libadwaita's dark card is white at 8%, and read as plain white
        // it measured "black text" onto a dark card.
        let base = ctx
            .lookup_color("window_bg_color")
            .map(|u| (f64::from(u.red()), f64::from(u.green()), f64::from(u.blue())));
        for (role, color) in [
            ("error", "error_bg_color"),
            ("warning", "warning_bg_color"),
            ("success", "success_bg_color"),
            ("accent", "accent_bg_color"),
            ("card", "card_bg_color"),
            ("fg", "window_fg_color"),
        ] {
            if let Some(rgba) = ctx.lookup_color(color) {
                out.insert(
                    role.to_string(),
                    crate::theming::try_flatten(
                        (
                            f64::from(rgba.red()),
                            f64::from(rgba.green()),
                            f64::from(rgba.blue()),
                            f64::from(rgba.alpha()),
                        ),
                        base,
                    )?,
                );
            }
        }
        Ok(out)
    }

    pub fn on_theme_changed(&self) {
        // The theme is swapped as several filesystem operations, so this
        // fires a handful of times; coalesce into one restyle.
        let old = self.packages.borrow_mut().theme_timer.take();
        if let Some(old) = old {
            old.remove();
        }
        let w = self.downgrade();
        let id = glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
            match w.upgrade() {
                Some(w) => w.theme_tick(),
                None => glib::ControlFlow::Break,
            }
        });
        self.packages.borrow_mut().theme_timer = Some(id);
    }

    pub fn theme_tick(&self) -> glib::ControlFlow {
        self.packages.borrow_mut().theme_timer = None;
        // What raises in here ends the Python's timeout callback, logged.
        if let Err(e) = self.apply_theme() {
            eprintln!("almanac: {e}");
            return glib::ControlFlow::Break;
        }
        let has_hero = self.today.borrow().hero.is_some();
        if has_hero {
            self.refresh_hero();
        }
        match crate::theming::try_describe() {
            Ok(text) => self.say(&text, 3),
            Err(e) => eprintln!("almanac: {e}"),
        }
        glib::ControlFlow::Break
    }

    /// The one place the window says it is doing something.
    pub fn busy(&self, working: bool) {
        self.busy.set_visible(working);
        if working {
            self.busy.start();
        } else {
            self.busy.stop();
        }
    }

    /// The standing one-line description of what is on screen.
    ///
    /// Scoped to a page because the loaders are asynchronous: the package
    /// scan finishing half a second after you opened the App Store used to
    /// overwrite the store's summary with "129 installed · showing 0".
    pub fn summarize(&self, text: &str, page: Option<&str>) {
        if let Some(page) = page {
            if page != self.shared.borrow().page_or("packages") {
                return;
            }
        }
        self.title.set_subtitle(text);
    }

    /// Put the installed inventory back. True if the store was showing.
    pub fn leave_store(&self) -> bool {
        if !self.shared.borrow().discovering {
            return false;
        }
        self.shared.borrow_mut().discovering = false;
        self.show_page("packages", "");
        self.rebuild_sort_menu(&sorts_installed());
        self.shared.borrow_mut().manual_sorter = None;
        self.rebuild_sorter();
        // The store showed its search results in the list: put back what
        // the last scan found rather than scanning again (seven ways out of
        // the store each started a full scan). A scan still running puts
        // its own answer there when it lands.
        let (last, scanning) = {
            let st = self.packages.borrow();
            (st.last_scan.clone(), st.scanning)
        };
        match last {
            _ if scanning => {}
            Some(apps) => self.apply(apps),
            None => self.reload(),
        }
        true
    }
}
