//! `Win::new` and the body of `__init__`: the stylesheet and theme providers,
//! the sidebar, the content header and the pages.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::i18n::tr;

use super::helpers::{CSS, lower_cmp, name, spawn_bg, thumb_bg};
use super::shape::PAGE_MARGIN;
use super::items::AppItem;
use super::tables::{COLLAPSED_BY_DEFAULT, GROUPS, GroupItem, sorts_installed};
use super::window::{GroupHeader, Inner, Shared, WeakWin, Win, sidebar_entry};

/// Keep `action` enabled exactly while `widget` is shown.
pub(super) fn enabled_while_visible(action: &gio::SimpleAction, widget: &impl IsA<gtk::Widget>) {
    action.set_enabled(widget.is_visible());
    let action = action.clone();
    widget.connect_visible_notify(move |w| action.set_enabled(w.is_visible()));
}

impl Win {
    /// `AlmanacWindow.__init__`: build the window, its sidebar, header,
    /// table and pages, restore saved state and start the first scan.
    pub fn new(app: &adw::Application) -> Win {
        let window = adw::ApplicationWindow::builder().application(app).build();
        window.set_title(Some(&tr("Almanac")));
        window.set_icon_name(Some("io.github.twodxme.Almanac"));
        window.set_default_size(1320, 800);
        // Nothing here should ever need a horizontal scrollbar, so the window
        // has to be able to shrink to a half-screen tile.
        window.set_size_request(480, 420);

        let shared = Shared::default();
        let selection = gtk::SingleSelection::new(Some(shared.sorted.clone()));
        selection.set_can_unselect(true);
        selection.set_autoselect(false);

        // The widgets every section reaches for, made here so `Inner` can
        // hold them; `init` configures and places them in __init__'s order.
        let sort_btn = gtk::MenuButton::builder().icon_name("view-sort-descending-symbolic").build();
        let busy = gtk::Spinner::builder().valign(gtk::Align::Center).build();
        let stats = gtk::Grid::builder()
            .column_spacing(12)
            .row_spacing(1)
            .column_homogeneous(true)
            .margin_start(12)
            .margin_end(12)
            .margin_top(6)
            .margin_bottom(8)
            .build();
        // On the page margins, like everything under the header: the page
        // below it starts its own PAGE_MARGIN down, so a page with a search
        // bar and one without start their content at the same rhythm.
        let search_bar = gtk::Box::builder()
            .margin_start(PAGE_MARGIN)
            .margin_end(PAGE_MARGIN)
            .margin_top(PAGE_MARGIN)
            .build();
        // Gtk.Button(label=..., icon_name=...): properties are applied in
        // that order, as PyGObject does, so the result is the same button.
        let store_back = gtk::Button::builder()
            .label(tr("Back to the App Store"))
            .icon_name("go-previous-symbolic")
            .halign(gtk::Align::Start)
            .margin_start(12)
            .margin_bottom(6)
            .build();
        let list_empty = Win::status_page("object-select-symbolic", &tr("Nothing here"), "", None);
        // Six workers, as ThreadPoolExecutor(max_workers=6). A pool that
        // cannot be made leaves thumb_bg on spawn_bg.
        let thumb_pool = glib::ThreadPool::shared(Some(6)).ok();

        let inner = Inner {
            window,
            theme_provider: gtk::CssProvider::new(),
            sidebar_toggle: gtk::ToggleButton::builder().icon_name("sidebar-show-symbolic").build(),
            stats,
            title: adw::WindowTitle::new(&tr("Almanac"), ""),
            busy,
            search: gtk::SearchEntry::builder()
                .placeholder_text(tr("Search installed software"))
                .build(),
            search_bar,
            update_btn: gtk::Button::with_label(&tr("Update all")),
            filter_btn: sort_btn.clone(),
            sort_btn,
            store_back,
            view: gtk::ColumnView::new(Some(selection)),
            priority_sorter: gtk::CustomSorter::new(|a, b| {
                match (AppItem::of(a), AppItem::of(b)) {
                    (Some(a), Some(b)) => {
                        a.priority.cmp(&b.priority).then_with(|| lower_cmp(&a.name, &b.name)).into()
                    }
                    _ => gtk::Ordering::Equal,
                }
            }),
            list_stack: gtk::Stack::new(),
            list_empty,
            update_notes: Win::note_list(),
            content_stack: gtk::Stack::new(),
            toasts: adw::ToastOverlay::new(),
            split: adw::OverlaySplitView::new(),
            thumb_pool,
            shared: RefCell::new(shared),
            shell: RefCell::default(),
            packages: RefCell::default(),
            store: RefCell::default(),
            store_actions: RefCell::default(),
            plugins: RefCell::default(),
            switchboard: RefCell::default(),
            history: RefCell::default(),
            ai: RefCell::default(),
            preferences: RefCell::default(),
            today: RefCell::default(),
            actions: RefCell::default(),
        };
        let win = Win(Rc::new(inner));
        win.init();
        win
    }

    /// The body of `__init__` once the shared widgets exist.
    fn init(&self) {
        let display = WidgetExt::display(&self.window);
        let provider = gtk::CssProvider::new();
        provider.load_from_string(CSS);
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );

        // The Omarchy palette is applied over our own stylesheet, because it
        // redefines the named colours our stylesheet is written against.
        gtk::style_context_add_provider_for_display(
            &display,
            &self.theme_provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
        );
        // Where this raises the Python's window is never built; here it is
        // logged and GTK's own theme shows.
        if let Err(e) = self.apply_theme() {
            eprintln!("almanac: {e}");
        }
        let w = self.downgrade();
        let monitors = crate::theming::watch(move || {
            if let Some(w) = w.upgrade() {
                w.on_theme_changed();
            }
        });
        self.shell.borrow_mut().theme_monitors = monitors;
        let w = self.downgrade();
        adw::StyleManager::default().connect_dark_notify(move |_| {
            let w = w.clone();
            glib::idle_add_local_once(move || {
                if let Some(w) = w.upgrade() {
                    if let Err(e) = w.apply_theme() {
                        eprintln!("almanac: {e}");
                    }
                }
            });
        });

        // filter_key = "all", query = "", the store, the image cache and pool
        // and the filter -> sort -> selection chain are made with `Shared`
        // and `Inner`. The filter's function is the packages section's.
        let filter = self.shared.borrow().filter.clone();
        let w = self.downgrade();
        filter.set_filter_func(move |obj| {
            let Some(w) = w.upgrade() else { return true };
            // Read in place: copying every row on every keystroke was most
            // of the filter's cost.
            match AppItem::of(obj) {
                Some(item) => w.r#match(&item),
                None => false,
            }
        });

        // --- sidebar: categories with live counts -------------------------
        // Ten filter chips and five header buttons was most of the window's
        // chrome. A sidebar holds the same navigation with the counts visible
        // at rest, which is the question being asked ("what needs me?").
        let sidebar_list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::Single).build();
        sidebar_list.add_css_class("navigation-sidebar");
        sidebar_list.add_css_class("almanac-sidebar");
        let w = self.downgrade();
        sidebar_list.connect_row_selected(move |list, row| {
            if let Some(w) = w.upgrade() {
                w.on_sidebar_selected(list, row);
            }
        });

        let mut sidebar_rows = Vec::new();
        let mut group_rows = Vec::new();
        let mut group_members = BTreeMap::new();
        for (gkey, glabel, items) in GROUPS {
            let Some(glabel) = glabel else {
                let mut members = Vec::new();
                for (key, label, icon) in items.iter() {
                    let entry = sidebar_entry(key, label, icon, gkey);
                    sidebar_list.append(&entry.row);
                    sidebar_rows.push(entry.clone());
                    members.push(entry);
                }
                group_rows.push(((*gkey).to_string(), None));
                group_members.insert((*gkey).to_string(), members);
                continue;
            };

            let header = gtk::ListBoxRow::builder().selectable(false).activatable(true).build();
            let hb = gtk::Box::builder()
                .spacing(6)
                .margin_start(6)
                .margin_top(6)
                .margin_bottom(2)
                .margin_end(6)
                .build();
            let chevron = gtk::Image::from_icon_name("pan-down-symbolic");
            chevron.add_css_class("dim-label");
            let title = gtk::Label::builder()
                .label(tr(glabel).to_uppercase())
                .xalign(0.0)
                .hexpand(true)
                .build();
            title.add_css_class("caption");
            title.add_css_class("dim-label");
            hb.append(&title);
            hb.append(&chevron);
            header.set_child(Some(&hb));
            // A single-item group has nothing to fold away; its header is a
            // label, not a control.
            header.set_activatable(items.len() > 1);
            sidebar_list.append(&header);
            group_rows.push((
                (*gkey).to_string(),
                Some(GroupHeader { group: (*gkey).to_string(), row: header, chevron }),
            ));

            let mut members = Vec::new();
            for (key, label, icon) in items.iter() {
                let entry = sidebar_entry(key, label, icon, gkey);
                sidebar_list.append(&entry.row);
                sidebar_rows.push(entry.clone());
                members.push(entry);
            }
            group_members.insert((*gkey).to_string(), members);
        }
        {
            let mut sh = self.shared.borrow_mut();
            sh.sidebar_rows = sidebar_rows;
            sh.group_rows = group_rows;
            sh.group_members = group_members;
        }

        let w = self.downgrade();
        sidebar_list.connect_row_activated(move |list, row| {
            if let Some(w) = w.upgrade() {
                w.on_sidebar_activated(list, row);
            }
        });
        self.shared.borrow_mut().collapsed =
            COLLAPSED_BY_DEFAULT.iter().map(|s| (*s).to_string()).collect();

        let sidebar_scroll = gtk::ScrolledWindow::builder().vexpand(true).build();
        sidebar_scroll.set_child(Some(&sidebar_list));
        sidebar_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);

        // A stats footer. The sidebar's bottom half is empty on any normal
        // window height, and these are the numbers you would otherwise open a
        // terminal for.
        // Two columns: six numbers in three lines, not six. Each keeps its
        // explanation as a tooltip.
        self.stats.add_css_class("stats");
        sidebar_scroll.set_vexpand(true);

        let sidebar_page = adw::ToolbarView::new();
        let sidebar_header = adw::HeaderBar::new();
        // The app's own mark, where someone actually looks at it. It was
        // registered with the desktop file and the window manager and so
        // appeared only in a launcher and an alt-tab switcher.
        let brand = gtk::Box::builder().spacing(8).halign(gtk::Align::Center).build();
        let mark = gtk::Image::builder()
            .icon_name("io.github.twodxme.Almanac")
            .pixel_size(18)
            .build();
        let wordmark = gtk::Label::new(Some(&tr("Almanac")));
        wordmark.add_css_class("heading");
        brand.append(&mark);
        brand.append(&wordmark);
        // Clicking an app's own mark opens its About, as it does in most
        // desktop apps; the menu entry stays for keyboard users.
        let brand_btn = gtk::Button::builder()
            .child(&brand)
            .tooltip_text(tr("About Almanac"))
            .build();
        brand_btn.add_css_class("flat");
        brand_btn.update_property(&[gtk::accessible::Property::Label("About Almanac")]);
        let w = self.downgrade();
        brand_btn.connect_clicked(move |_| {
            if let Some(w) = w.upgrade() {
                w.on_about();
            }
        });
        sidebar_header.set_title_widget(Some(&brand_btn));
        sidebar_header.set_show_end_title_buttons(false);
        sidebar_page.add_top_bar(&sidebar_header);
        let sidebar_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        sidebar_box.append(&sidebar_scroll);
        sidebar_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        sidebar_box.append(&self.stats);
        sidebar_page.set_content(Some(&sidebar_box));

        // --- content header: one primary action, everything else in a menu --
        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();

        self.sidebar_toggle.set_active(true);
        name(&self.sidebar_toggle, &tr("Show categories"));
        let w = self.downgrade();
        self.sidebar_toggle.connect_toggled(move |b| {
            if let Some(w) = w.upgrade() {
                w.split.set_show_sidebar(b.is_active());
            }
        });
        header.pack_start(&self.sidebar_toggle);

        // The header names the page and summarises it. Previously the search
        // entry was the title widget, so no page had a name and the four
        // status labels were the only thing describing what you were looking
        // at -- each in a different place on the page.
        let title_box = gtk::Box::builder().spacing(8).halign(gtk::Align::Center).build();
        self.busy.set_visible(false);
        title_box.append(&self.busy);
        title_box.append(&self.title);
        header.set_title_widget(Some(&title_box));

        self.search.set_hexpand(true);
        let w = self.downgrade();
        self.search.connect_search_changed(move |entry| {
            if let Some(w) = w.upgrade() {
                w.on_search(entry);
            }
        });

        // One primary action, always in the same place. Each page says what
        // it is; before this, "Update all" lived in the header while the AI
        // and Maintenance pages had their own button in their own toolbar, in
        // a different position with different alignment.
        self.update_btn.add_css_class("suggested-action");
        let w = self.downgrade();
        self.update_btn.connect_clicked(move |b| {
            if let Some(w) = w.upgrade() {
                w.on_primary(b);
            }
        });
        header.pack_end(&self.update_btn);
        self.shared.borrow_mut().primary = None;

        let menu = gio::Menu::new();
        let actions = gio::Menu::new();
        actions.append(Some(&tr("Install what’s in the search box…")), Some("win.install"));
        actions.append(Some(&tr("Install from a link…")), Some("win.fromlink"));
        actions.append(Some(&tr("Track an AppImage file…")), Some("win.track"));
        menu.append_section(None, &actions);
        let maintenance = gio::Menu::new();
        maintenance.append(Some(&tr("Review orphaned packages…")), Some("win.clean"));
        maintenance.append(Some(&tr("Update plugins…")), Some("win.updateplugins"));
        if crate::util::which("omarchy-restart-shell").is_some() {
            maintenance.append(Some(&tr("Restart the shell…")), Some("win.restartshell"));
        }
        maintenance.append(Some(&tr("Refresh this page")), Some("win.rescan"));
        menu.append_section(None, &maintenance);
        let settings_menu = gio::Menu::new();
        settings_menu.append(Some(&tr("Preferences…")), Some("win.prefs"));
        menu.append_section(None, &settings_menu);
        let about_section = gio::Menu::new();
        about_section.append(Some(&tr("Update Almanac…")), Some("win.selfupdate"));
        about_section.append(Some(&tr("About Almanac")), Some("win.about"));
        menu.append_section(None, &about_section);

        // SORTS_INSTALLED / SORTS_DISCOVER: see sorts_installed().
        self.rebuild_sort_menu(&sorts_installed());
        name(&self.sort_btn, &tr("Sort by"));
        header.pack_end(&self.sort_btn);

        // The store has no second button: the same control in the same place
        // opens a sort menu on the package list and a filter drawer in the
        // store. Two adjacent buttons that both mean "options" is the thing
        // this is avoiding. (`filter_btn` is `sort_btn`; see `Win::new`.)

        let sort_action = gio::SimpleAction::new_stateful(
            "sort",
            Some(glib::VariantTy::STRING),
            &"default".to_variant(),
        );
        let w = self.downgrade();
        sort_action.connect_activate(move |action, param| {
            if let Some(w) = w.upgrade() {
                w.on_sort(action, param);
            }
        });
        self.window.add_action(&sort_action);

        let menu_btn = gtk::MenuButton::builder().icon_name("open-menu-symbolic").build();
        menu_btn.set_menu_model(Some(&menu));
        name(&menu_btn, &tr("More actions"));
        header.pack_end(&menu_btn);

        type Handler = fn(&Win);
        let handlers: [(&str, Handler); 10] = [
            ("install", |w| w.on_install()),
            ("track", |w| w.on_add_appimage()),
            // on_install_link(None): no link yet, so it asks for one.
            ("fromlink", |w| w.on_install_link("")),
            ("updateplugins", |w| w.on_update_plugins()),
            ("restartshell", |w| w.on_restart_shell()),
            ("clean", |w| w.on_clean_orphans()),
            ("prefs", |w| w.on_preferences()),
            ("selfupdate", |w| w.on_self_update()),
            ("about", |w| w.on_about()),
            ("rescan", |w| w.refresh_page()),
        ];
        for (act_name, f) in handlers {
            let act = gio::SimpleAction::new(act_name, None);
            let w = self.downgrade();
            act.connect_activate(move |_, _| {
                if let Some(w) = w.upgrade() {
                    f(&w);
                }
            });
            self.window.add_action(&act);
        }
        // "Install what's in the search box…" means the box on this page:
        // where there is none (Today, Health check, History, AI) the entry
        // is hidden, and installing from it would take whatever another page
        // left in it. The action follows the search bar's visibility, which
        // the page chrome sets.
        if let Some(install) = self.window.lookup_action("install").and_downcast::<gio::SimpleAction>() {
            enabled_while_visible(&install, &self.search_bar);
        }

        toolbar.add_top_bar(&header);

        // One search box for the whole window, in one place on every page,
        // rather than a title-bar widget that silently does nothing on two of
        // the four pages.
        self.search_bar.append(&self.search);
        toolbar.add_top_bar(&self.search_bar);

        // The table's page, on the same top margin as every page_body.
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.set_margin_top(PAGE_MARGIN);

        // The way back from the repo/AUR search, which the sidebar cannot
        // offer: App Store is still the selected row, so re-selecting it
        // emits nothing.
        self.store_back.add_css_class("flat");
        self.store_back.set_tooltip_text(Some(&tr("Return to the plugin marketplace")));
        self.store_back.set_visible(false);
        let w = self.downgrade();
        self.store_back.connect_clicked(move |_| {
            if let Some(w) = w.upgrade() {
                w.store_return();
            }
        });
        content.append(&self.store_back);

        self.view.set_single_click_activate(true);
        let w = self.downgrade();
        self.view.connect_activate(move |view, position| {
            if let Some(w) = w.upgrade() {
                w.on_row_activated(view, position);
            }
        });
        self.view.set_show_row_separators(true);
        self.view.set_show_column_separators(false);
        self.build_columns();
        // Clicking a column header drives the sort.
        // A MultiSorter rather than swapping sorters: with no column selected
        // the column sorter reports every pair equal, so the priority sorter
        // decides; once a header is clicked it takes precedence and priority
        // becomes the tiebreak. Swapping on "changed" instead fired during
        // construction and silently replaced the default ordering.
        // (`priority_sorter` is made in `Win::new`.)
        self.shared.borrow_mut().manual_sorter = None;
        self.rebuild_sorter();

        let scroller = gtk::ScrolledWindow::builder().vexpand(true).build();
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroller.set_child(Some(&self.view));
        // The table sits on a card like every other page's lists -- same
        // background, corners and outline -- rather than on the darker view
        // background, edge to edge. One look for the whole app.
        self.view.add_css_class("almanac-table");
        scroller.add_css_class("card");
        scroller.set_overflow(gtk::Overflow::Hidden);
        // As tall as its rows, like a boxed list -- not a card running to the
        // bottom of the window under five updates. Past the window's height
        // it scrolls as before.
        scroller.set_propagate_natural_height(true);
        scroller.set_valign(gtk::Align::Start);
        scroller.set_margin_start(12);
        scroller.set_margin_end(12);
        scroller.set_margin_bottom(12);

        // A boxed list on the same 12px margins as the table's card, shown
        // only on the Updates page and only when it has something to say.
        self.update_notes.set_margin_start(12);
        self.update_notes.set_margin_end(12);
        self.update_notes.set_margin_bottom(12);
        self.update_notes.set_visible(false);
        content.append(&self.update_notes);

        self.list_stack.set_vexpand(true);
        self.list_stack.add_named(&scroller, Some("list"));
        self.list_stack.add_named(&self.list_empty, Some("empty"));
        content.append(&self.list_stack);

        self.content_stack.set_hhomogeneous(false);
        self.content_stack.set_vhomogeneous(false);
        self.content_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        self.content_stack.add_named(&content, Some("list"));
        self.content_stack.add_named(&self.build_store(), Some("store"));
        self.content_stack.add_named(&self.build_maintenance(), Some("maintenance"));
        self.content_stack.add_named(&self.build_history(), Some("history"));
        self.content_stack.add_named(&self.build_plugins_page(), Some("plugins"));
        self.content_stack.add_named(&self.build_switchboard(), Some("switchboard"));
        self.content_stack.add_named(&self.build_ai(), Some("ai"));
        self.content_stack.add_named(&self.build_featured(), Some("featured"));
        self.content_stack.set_visible_child_name("list");

        // Results and failures appear as toasts. They used to go to whichever
        // of four dim status labels the page happened to own, which is where
        // "Could not remove X" went to die.
        self.toasts.set_child(Some(&self.content_stack));
        toolbar.set_content(Some(&self.toasts));

        self.split.set_sidebar(Some(&sidebar_page));
        self.split.set_content(Some(&toolbar));
        self.split.set_sidebar_width_fraction(0.24);
        self.split.set_min_sidebar_width(205.0);
        self.split.set_max_sidebar_width(260.0);
        self.window.set_content(Some(&self.split));

        // Below ~760px the sidebar costs more than it gives: 205 of 480 pixels
        // is most of a half-screen tile. It folds into an overlay instead, and
        // the toggle in the header becomes the way back to it.
        //
        // A window applies one breakpoint at a time (the last that matches),
        // so the phone-narrow one repeats the folding and adds its own: the
        // table's Source column goes, and Name keeps the room.
        let source_col = (0..self.view.columns().n_items())
            .filter_map(|i| self.view.columns().item(i).and_downcast::<gtk::ColumnViewColumn>())
            .find(|c| c.title().is_some_and(|t| t == "Source"));
        for (width, phone) in [(760, false), (super::columns::SOURCE_HIDES_BELOW, true)] {
            let Ok(condition) = adw::BreakpointCondition::parse(&format!("max-width: {width}px")) else {
                continue;
            };
            let breakpoint = adw::Breakpoint::new(condition);
            breakpoint.add_setter(&self.split, "collapsed", Some(&true.to_value()));
            if let (true, Some(col)) = (phone, &source_col) {
                breakpoint.add_setter(col, "visible", Some(&false.to_value()));
            }
            let w = self.downgrade();
            breakpoint.connect_apply(move |_| {
                if let Some(w) = w.upgrade() {
                    w.set_narrow(true);
                }
            });
            let w = self.downgrade();
            breakpoint.connect_unapply(move |_| {
                if let Some(w) = w.upgrade() {
                    w.set_narrow(false);
                }
            });
            self.window.add_breakpoint(breakpoint);
        }
        self.shared.borrow_mut().narrow = false;

        // Row-menu actions resolve by name at activation time, because the
        // row widgets themselves are recycled as the list scrolls. The
        // marketplace, findings, survey and shelves start empty (`Shared`).
        let group = gio::SimpleActionGroup::new();
        type RowHandler = fn(&Win, &str);
        let row_handlers: [(&str, RowHandler); 10] = [
            ("visit", |w, n| w.row_visit(n)),
            ("remove", |w, n| w.row_remove(n)),
            ("notes", |w, n| w.row_notes(n)),
            ("aur", |w, n| w.row_aur(n)),
            ("folder", |w, n| w.row_folder(n)),
            ("changes", |w, n| w.row_changes(n)),
            ("pull", |w, n| w.row_pull(n)),
            ("stash", |w, n| w.row_stash(n)),
            ("toggle", |w, n| w.row_toggle(n)),
            ("shortcut", |w, n| w.row_shortcut(n)),
        ];
        for (act_name, handler) in row_handlers {
            let a = gio::SimpleAction::new(act_name, Some(glib::VariantTy::STRING));
            let w = self.downgrade();
            a.connect_activate(move |_, param| {
                let arg = param.and_then(|p| p.str().map(str::to_string)).unwrap_or_default();
                if let Some(w) = w.upgrade() {
                    handler(&w, &arg);
                }
            });
            group.add_action(&a);
        }
        self.window.insert_action_group("row", Some(&group));
        let pgroup = gio::SimpleActionGroup::new();
        for act_name in ["update", "shortcut", "folder", "page", "remove"] {
            let a = gio::SimpleAction::new(act_name, Some(glib::VariantTy::STRING));
            let w = self.downgrade();
            a.connect_activate(move |_, param| {
                let arg = param.and_then(|p| p.str().map(str::to_string)).unwrap_or_default();
                if let Some(w) = w.upgrade() {
                    w.plugin_action(act_name, &arg);
                }
            });
            pgroup.add_action(&a);
        }
        self.window.insert_action_group("plugin", Some(&pgroup));

        self.apply_all_group_states();
        self.install_shortcuts();
        self.restore_state();
        // Land on a category, always: an unselected sidebar shows an empty
        // list that reads as a bug.
        let (rows, filter_key) = {
            let sh = self.shared.borrow();
            let rows: Vec<adw::ActionRow> = sh.sidebar_rows.iter().map(|r| r.row.clone()).collect();
            (rows, sh.filter_key.clone())
        };
        if !rows.is_empty() && !rows.iter().any(|r| r.is_selected()) {
            let key = if filter_key == "all" { "featured".to_string() } else { filter_key };
            self.select_category(&key);
        }
        // Work that panics off the main thread cannot hand its page a
        // result; the page is told it failed instead of spinning forever.
        let w = self.downgrade();
        super::helpers::on_bg_failure(move |msg| {
            if let Some(w) = w.upgrade() {
                w.busy(false);
                w.say(&format!("{}: {msg}", tr("Something went wrong")), 8);
            }
        });
        self.watch_pacman();
        self.watch_configs();
        self.reload();
    }

    pub fn downgrade(&self) -> WeakWin {
        WeakWin(Rc::downgrade(&self.0))
    }

    /// `CATEGORIES`: every sidebar destination, in order. Labels are msgids.
    pub fn categories(&self) -> Vec<GroupItem> {
        GROUPS.iter().flat_map(|(_k, _l, items)| items.iter().copied()).collect()
    }

    /// `self._thumb_pool.submit(...)`: see `thumb_bg`.
    pub fn thumb_bg<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
        done: impl FnOnce(T) + 'static,
    ) {
        match &self.thumb_pool {
            Some(pool) => thumb_bg(pool, work, done),
            None => spawn_bg(work, done),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Install-from-the-search-box is on only where the page shows the box.
    #[test]
    fn install_follows_the_search_bar() {
        let set = |var| std::env::var_os(var).is_some_and(|v| !v.is_empty());
        let broadway = std::env::var("GDK_BACKEND").is_ok_and(|b| b.contains("broadway"));
        if !(set("WAYLAND_DISPLAY") || set("DISPLAY") || broadway) {
            return;
        }
        if !matches!(std::panic::catch_unwind(adw::init), Ok(Ok(()))) || gtk::gdk::Display::default().is_none() {
            return;
        }
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        bar.set_visible(false);
        let install = gio::SimpleAction::new("install", None);
        enabled_while_visible(&install, &bar);
        assert!(!install.is_enabled(), "a page without a search box");
        bar.set_visible(true);
        assert!(install.is_enabled(), "a page with one");
        bar.set_visible(false);
        assert!(!install.is_enabled());
        // And the window wires it to the search bar, not to something else.
        let src = include_str!("build.rs");
        assert!(src.contains("enabled_while_visible(&install, &self.search_bar)"));
        let guard = include_str!("../plugins/removal.rs");
        let on_install = guard.split("pub fn on_install(").nth(1).unwrap_or("");
        let head: String = on_install.chars().take(400).collect();
        assert!(head.contains("if !self.search_bar.is_visible()"), "on_install reads a hidden box");
    }
}
